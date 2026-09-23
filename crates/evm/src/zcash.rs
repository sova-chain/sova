//! SIP-4 Zcash state precompile at [`ZCASH_QUERY`] (`0x…5A00`).
//!
//! Contracts read **transparent Zcash chain state as of the Sova block's
//! anchored Zcash height** `E_N = N + B − 1` (SIP-4 §1), where `N` is the
//! executing block's number (read from the EVM's block env on **every**
//! call) and `B` the network's epoch base. Every answer is a pure function
//! of the anchored chain segment `Z[B ..= E_N]`, so two nodes that accept
//! the same Sova block compute the same answers whatever their own zebrad's
//! tip is (SIP-4 §2).
//!
//! The node-side store ([`ZcashSource`], implemented by the engine's
//! `ZcashIndex`) is deliberately dumb: blocks by height, transactions by
//! txid. **All consensus semantics live here** — the horizon filter, the
//! status codes, confirmations, gas — so they are tested in one place.
//!
//! Consensus-relevant properties (each has a test):
//!
//! - Registered with [`DynPrecompile::new_stateful`], so
//!   `supports_caching() == false` and reth's cross-block precompile cache
//!   (`map_cacheable_precompiles`) never wraps it.
//! - **Coverage before answers.** Every query first requires the source to
//!   hold every Zcash block from `B` through `E_N` contiguously
//!   ([`ZcashSource::indexed_through`]). Otherwise the call returns
//!   [`PrecompileError::Fatal`]: the block is refused (never executed with a
//!   guessed answer, never cached invalid) and retried once the index
//!   catches up. With coverage established, "not found" is a consensus
//!   fact, identical on every honest node. Fatal is reserved for this one
//!   condition: a missing txid or output is a status code, so no
//!   transaction can poison block building on demand.
//! - **The horizon filter.** The index runs ahead of `E_N`; a record above
//!   `E_N` (or below `B`) is answered exactly as if it didn't exist.
//! - A *direct* call carrying value reverts (SIP-4 §3). `DELEGATECALL` and
//!   `CALLCODE` only see an *apparent* value, so they are not rejected.
//! - Malformed calldata reverts; insufficient gas halts out-of-gas. Both
//!   are decided before the index is consulted, except `txOutput`'s
//!   per-byte charge, which depends only on anchored (consensus) data.

use std::fmt;
use std::sync::{Arc, OnceLock};

use reth_ethereum::evm::{
    EthEvm,
    primitives::{
        Database, Evm, EvmEnv, EvmFactory,
        eth::{EthEvmContext, EthEvmFactory},
        precompiles::{DynPrecompile, PrecompileInput, PrecompilesMap},
    },
    revm::{
        context::{BlockEnv, DBErrorMarker, TxEnv},
        context_interface::result::{EVMError, HaltReason},
        inspector::{Inspector, NoOpInspector},
        precompile::{
            PrecompileError, PrecompileHalt, PrecompileId, PrecompileOutput, PrecompileResult,
        },
        primitives::{Address, Bytes, U256, address, hardfork::SpecId},
    },
};

/// The Zcash query precompile address (SIP-4 §3, provisional).
pub const ZCASH_QUERY: Address = address!("0x0000000000000000000000000000000000005A00");

/// `bytes4(keccak256("anchor()"))`.
pub const ANCHOR_SELECTOR: [u8; 4] = [0xd3, 0xfb, 0x73, 0xb4];
/// `bytes4(keccak256("blockAt(uint64)"))`.
pub const BLOCK_AT_SELECTOR: [u8; 4] = [0x6f, 0x8e, 0xa1, 0x5d];
/// `bytes4(keccak256("txInfo(bytes32)"))`.
pub const TX_INFO_SELECTOR: [u8; 4] = [0x0a, 0xc6, 0x92, 0x3d];
/// `bytes4(keccak256("txOutput(bytes32,uint32)"))`.
pub const TX_OUTPUT_SELECTOR: [u8; 4] = [0x2d, 0x45, 0x82, 0x8e];
/// `bytes4(keccak256("burnInfo(bytes32)"))`.
pub const BURN_INFO_SELECTOR: [u8; 4] = [0x7c, 0xe1, 0x6a, 0x38];

/// Gas for `anchor()` (SIP-4 §4 draft table).
pub const ANCHOR_GAS: u64 = 200;
/// Gas for `blockAt`.
pub const BLOCK_AT_GAS: u64 = 2_600;
/// Gas for `txInfo` and `burnInfo`, and the base of `txOutput`.
pub const TX_GAS: u64 = 4_000;
/// `txOutput`'s charge per script byte returned.
pub const SCRIPT_BYTE_GAS: u64 = 8;

/// Status codes (SIP-4 §3). "Not found" is a result, not a revert.
pub mod status {
    /// Answered.
    pub const OK: u8 = 0;
    /// Not in `Z[B ..= E_N]` on the anchored chain.
    pub const NOT_FOUND: u8 = 1;
    /// `blockAt` height above `E_N`.
    pub const NOT_YET: u8 = 2;
    /// `blockAt` height below the epoch base.
    pub const OUT_OF_RANGE: u8 = 3;
    /// `txOutput`: the tx has no transparent output at that index.
    pub const NO_SUCH_OUTPUT: u8 = 4;
    /// `burnInfo`: the tx exists but is not a SIP-1 burn.
    pub const NOT_A_BURN: u8 = 5;
}

/// A Zcash transaction as the index stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTx {
    /// Height of the block that mined it.
    pub height: u64,
    /// Position in that block (coinbase = 0).
    pub index: u32,
    /// Transaction format version.
    pub version: u32,
    /// Transparent outputs in order: `(value_zat, script)`.
    pub outputs: Vec<(u64, Vec<u8>)>,
    /// `sip1::extract_burn(outputs)`, computed once in [`IndexedTx::new`]:
    /// the parse walks every output, so doing it per `burnInfo` call would
    /// make a flat-priced query linear in the tx's output count.
    burn: Option<consensus::sip1::Burn>,
}

impl IndexedTx {
    /// A record for the index. Parses the SIP-1 burn once, with the
    /// consensus parser, over exactly these outputs.
    #[must_use]
    pub fn new(height: u64, index: u32, version: u32, outputs: Vec<(u64, Vec<u8>)>) -> Self {
        let burn = consensus::sip1::extract_burn(outputs.iter().map(|(value_zat, script)| {
            consensus::sip1::TxOutRef {
                value_zat: *value_zat,
                script: script.as_slice(),
            }
        }));
        Self {
            height,
            index,
            version,
            outputs,
            burn,
        }
    }

    /// The SIP-1 burn these outputs carry, if any.
    #[must_use]
    pub const fn burn(&self) -> Option<consensus::sip1::Burn> {
        self.burn
    }
}

/// Where the precompile's answers come from: a local, hash-checked index
/// fed by the same follower as the C5 mint check — never a live zebrad RPC.
pub trait ZcashSource: Send + Sync + fmt::Debug + 'static {
    /// The network's epoch base `B` (`SOVA_EPOCH_BASE`).
    fn epoch_base(&self) -> u64;
    /// Highest height `h` such that every block in `B ..= h` is indexed,
    /// or `None` if the index is empty.
    fn indexed_through(&self) -> Option<u64>;
    /// Hash (display order) and header time of the indexed block at
    /// `zcash_height`.
    fn block(&self, zcash_height: u64) -> Option<([u8; 32], u32)>;
    /// The indexed tx with this txid (display order), at any height; the
    /// precompile applies the horizon. Shared, not cloned: a flat-priced
    /// query must not pay for copying every output of a large tx.
    fn tx(&self, txid: &[u8; 32]) -> Option<Arc<IndexedTx>>;
    /// Bumped **before** every reorg unwind of the index. An EVM captures
    /// it at creation and every call re-checks it after reading: if a Zcash
    /// reorg touched the index while a block was executing, the call is
    /// fatal (block refused and retried) rather than answered from the new
    /// branch on this node only — a node-local state root would be a
    /// permanent fork.
    fn generation(&self) -> u64;
}

static ZCASH_SOURCE: OnceLock<Arc<dyn ZcashSource>> = OnceLock::new();

/// Install the process-wide [`ZcashSource`] (the `expectations::global()`
/// pattern: reth's component builders are constructed statically). Call
/// before the node launches; returns `false` if one was already installed.
pub fn set_zcash_source(source: Arc<dyn ZcashSource>) -> bool {
    ZCASH_SOURCE.set(source).is_ok()
}

/// The process-wide [`ZcashSource`], if installed.
#[must_use]
pub fn zcash_source() -> Option<Arc<dyn ZcashSource>> {
    ZCASH_SOURCE.get().cloned()
}

/// Anchored Zcash height for Sova block `n`: `E_N = N + B − 1`. `None` on
/// overflow — the caller treats that as fatal.
#[must_use]
pub fn anchored_height(block_number: U256, epoch_base: u64) -> Option<u64> {
    let n: u64 = block_number.try_into().ok()?;
    n.checked_add(epoch_base)?.checked_sub(1)
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_address(a: [u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(&a);
    w
}

fn encode_words(words: &[[u8; 32]]) -> Bytes {
    let mut out = Vec::with_capacity(words.len() * 32);
    for w in words {
        out.extend_from_slice(w);
    }
    Bytes::from(out)
}

/// ABI-encode `anchor()`'s return value `(uint64 height, bytes32 hash)`.
#[must_use]
pub fn encode_anchor(height: u64, hash: [u8; 32]) -> Bytes {
    encode_words(&[word_u64(height), hash])
}

/// ABI-encode `(uint8 status, uint64 valueZat, bytes script)`.
fn encode_tx_output(status: u8, value_zat: u64, script: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(128 + script.len().next_multiple_of(32));
    out.extend_from_slice(&word_u64(u64::from(status)));
    out.extend_from_slice(&word_u64(value_zat));
    out.extend_from_slice(&word_u64(0x60));
    out.extend_from_slice(&word_u64(script.len() as u64));
    out.extend_from_slice(script);
    out.resize(out.len().next_multiple_of(32), 0);
    Bytes::from(out)
}

/// A decoded call.
enum Query {
    Anchor,
    BlockAt(u64),
    TxInfo([u8; 32]),
    TxOutput([u8; 32], u32),
    BurnInfo([u8; 32]),
}

/// Strict ABI decoding: exact length, and integer words with clean high
/// bytes (as Solidity itself would reject). `None` = malformed → revert.
fn decode(data: &[u8]) -> Option<Query> {
    let (sel, args) = data.split_first_chunk::<4>()?;
    let word = |i: usize| -> Option<[u8; 32]> { args.get(i * 32..(i + 1) * 32)?.try_into().ok() };
    let uint = |w: [u8; 32], bytes: usize| -> Option<u64> {
        w[..32 - bytes].iter().all(|b| *b == 0).then(|| {
            let mut v = [0u8; 8];
            v[8 - bytes..].copy_from_slice(&w[32 - bytes..]);
            u64::from_be_bytes(v)
        })
    };
    let words = args.len() / 32;
    if args.len() % 32 != 0 {
        return None;
    }
    match (*sel, words) {
        (ANCHOR_SELECTOR, 0) => Some(Query::Anchor),
        (BLOCK_AT_SELECTOR, 1) => Some(Query::BlockAt(uint(word(0)?, 8)?)),
        (TX_INFO_SELECTOR, 1) => Some(Query::TxInfo(word(0)?)),
        (TX_OUTPUT_SELECTOR, 2) => Some(Query::TxOutput(
            word(0)?,
            u32::try_from(uint(word(1)?, 4)?).ok()?,
        )),
        (BURN_INFO_SELECTOR, 1) => Some(Query::BurnInfo(word(0)?)),
        _ => None,
    }
}

const fn base_gas(q: &Query) -> u64 {
    match q {
        Query::Anchor => ANCHOR_GAS,
        Query::BlockAt(_) => BLOCK_AT_GAS,
        Query::TxInfo(_) | Query::TxOutput(..) | Query::BurnInfo(_) => TX_GAS,
    }
}

/// The precompile body. `source` is `None` when the node has no index at
/// all (no zebrad): every well-formed, paid-for call is then fatal.
#[cfg(test)]
fn call_zcash_query(
    source: Option<&dyn ZcashSource>,
    input: &PrecompileInput<'_>,
) -> PrecompileResult {
    call_zcash_query_at(source, None, input)
}

/// [`call_zcash_query`], additionally requiring the index generation to
/// still equal `generation` (captured when the EVM was created) once the
/// answer has been read.
fn call_zcash_query_at(
    source: Option<&dyn ZcashSource>,
    generation: Option<u64>,
    input: &PrecompileInput<'_>,
) -> PrecompileResult {
    let result = answer_zcash_query(source, input);
    if let (Some(source), Some(g0), Ok(_)) = (source, generation, &result)
        && source.generation() != g0
    {
        return Err(PrecompileError::Fatal(format!(
            "sova zcash precompile: zcash index reorged during execution (sova block {})",
            input.internals().block_number()
        )));
    }
    result
}

fn answer_zcash_query(
    source: Option<&dyn ZcashSource>,
    input: &PrecompileInput<'_>,
) -> PrecompileResult {
    let reservoir = input.reservoir;

    // Read-only: a direct call must not strand value at 0x5A00.
    if input.is_direct_call() && !input.value.is_zero() {
        return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir));
    }
    let Some(query) = decode(input.data) else {
        return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir));
    };
    let gas = base_gas(&query);
    if input.gas < gas {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }

    let block_number = input.internals().block_number();
    let fatal = |why: &str| {
        Err(PrecompileError::Fatal(format!(
            "sova zcash precompile: {why} (sova block {block_number})"
        )))
    };
    let Some(source) = source else {
        return fatal("no zcash index installed");
    };
    let base = source.epoch_base();
    let Some(e) = anchored_height(block_number, base) else {
        return fatal("anchored height out of range");
    };
    if source.indexed_through().is_none_or(|t| t < e) {
        return fatal(&format!("zcash index does not cover anchored height {e}"));
    }
    // The horizon: only Z[B ..= e] exists.
    let visible = |txid: &[u8; 32]| {
        source
            .tx(txid)
            .filter(|t| t.height >= base && t.height <= e)
    };
    let ok = PrecompileOutput::new;

    let out = match query {
        Query::Anchor => {
            let Some((hash, _)) = source.block(e) else {
                return fatal(&format!("zcash index has no block at anchored height {e}"));
            };
            ok(gas, encode_anchor(e, hash), reservoir)
        }
        Query::BlockAt(h) => {
            let answer = if h > e {
                encode_words(&[word_u64(u64::from(status::NOT_YET)), [0; 32], [0; 32]])
            } else if h < base {
                encode_words(&[word_u64(u64::from(status::OUT_OF_RANGE)), [0; 32], [0; 32]])
            } else {
                let Some((hash, time)) = source.block(h) else {
                    return fatal(&format!("zcash index hole at height {h}"));
                };
                encode_words(&[
                    word_u64(u64::from(status::OK)),
                    hash,
                    word_u64(u64::from(time)),
                ])
            };
            ok(gas, answer, reservoir)
        }
        Query::TxInfo(txid) => {
            let answer = visible(&txid).map_or_else(
                || {
                    encode_words(&[
                        word_u64(u64::from(status::NOT_FOUND)),
                        [0; 32],
                        [0; 32],
                        [0; 32],
                        [0; 32],
                        [0; 32],
                    ])
                },
                |t| {
                    encode_words(&[
                        word_u64(u64::from(status::OK)),
                        word_u64(t.height),
                        word_u64(u64::from(t.index)),
                        word_u64(e - t.height + 1),
                        word_u64(t.outputs.len() as u64),
                        word_u64(u64::from(t.version)),
                    ])
                },
            );
            ok(gas, answer, reservoir)
        }
        Query::TxOutput(txid, vout) => match visible(&txid) {
            None => ok(gas, encode_tx_output(status::NOT_FOUND, 0, &[]), reservoir),
            Some(t) => match t.outputs.get(vout as usize) {
                None => ok(
                    gas,
                    encode_tx_output(status::NO_SUCH_OUTPUT, 0, &[]),
                    reservoir,
                ),
                Some((value, script)) => {
                    let total = (script.len() as u64)
                        .checked_mul(SCRIPT_BYTE_GAS)
                        .and_then(|g| g.checked_add(gas));
                    match total {
                        Some(total) if total <= input.gas => ok(
                            total,
                            encode_tx_output(status::OK, *value, script),
                            reservoir,
                        ),
                        _ => PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir),
                    }
                }
            },
        },
        Query::BurnInfo(txid) => {
            let answer = match visible(&txid) {
                None => encode_words(&[
                    word_u64(u64::from(status::NOT_FOUND)),
                    [0; 32],
                    [0; 32],
                    [0; 32],
                ]),
                Some(t) => match t.burn() {
                    None => encode_words(&[
                        word_u64(u64::from(status::NOT_A_BURN)),
                        [0; 32],
                        [0; 32],
                        [0; 32],
                    ]),
                    Some(b) => encode_words(&[
                        word_u64(u64::from(status::OK)),
                        word_address(b.evm_address),
                        word_u64(u64::from(b.signal_bits)),
                        word_u64(b.value_zat),
                    ]),
                },
            };
            ok(gas, answer, reservoir)
        }
    };
    Ok(out)
}

/// Build the (stateful, non-cacheable) precompile for `source`.
#[must_use]
pub fn zcash_query_precompile(source: Option<Arc<dyn ZcashSource>>) -> DynPrecompile {
    // Captured once per EVM (one per block, or per RPC call).
    let generation = source.as_deref().map(ZcashSource::generation);
    DynPrecompile::new_stateful(
        PrecompileId::custom("sova_zcash_query"),
        move |input: PrecompileInput<'_>| {
            call_zcash_query_at(source.as_deref(), generation, &input)
        },
    )
}

/// Install the Zcash query precompile into an EVM's precompile map.
pub fn install_zcash_query(map: &mut PrecompilesMap, source: Option<Arc<dyn ZcashSource>>) {
    map.extend_precompiles([(ZCASH_QUERY, zcash_query_precompile(source))]);
}

/// [`EvmFactory`] for Sova: reth's Ethereum EVM plus the Zcash query
/// precompile at [`ZCASH_QUERY`].
///
/// The source is resolved once per `create_evm*` call: an explicitly
/// injected source (tests) wins, otherwise the process global
/// ([`zcash_source`]).
#[derive(Clone, Default)]
pub struct SovaEvmFactory {
    source: Option<Arc<dyn ZcashSource>>,
}

impl SovaEvmFactory {
    /// A factory that answers from `source` instead of the global.
    #[must_use]
    pub fn with_zcash_source(source: Arc<dyn ZcashSource>) -> Self {
        Self {
            source: Some(source),
        }
    }

    fn resolve_source(&self) -> Option<Arc<dyn ZcashSource>> {
        self.source.clone().or_else(zcash_source)
    }
}

impl fmt::Debug for SovaEvmFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SovaEvmFactory")
            .field("source", &self.source)
            .finish()
    }
}

impl EvmFactory for SovaEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I, PrecompilesMap>;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Tx = TxEnv;
    type Error<DBError: DBErrorMarker> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let mut evm = EthEvmFactory::default().create_evm(db, input);
        install_zcash_query(evm.precompiles_mut(), self.resolve_source());
        evm
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let mut evm = EthEvmFactory::default().create_evm_with_inspector(db, input, inspector);
        install_zcash_query(evm.precompiles_mut(), self.resolve_source());
        evm
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use reth_ethereum::evm::{
        primitives::{
            EvmInternals,
            block::{BlockExecutionError, InternalBlockExecutionError},
            precompiles::Precompile,
        },
        revm::{
            bytecode::Bytecode,
            context::CfgEnv,
            context_interface::result::ExecutionResult,
            db::{CacheDB, EmptyDB},
            primitives::{B256, TxKind, keccak256},
            state::AccountInfo,
        },
    };

    use super::*;

    const BASE: u64 = 4_383_000;
    const CALLER: Address = address!("0x00000000000000000000000000000000000c0ffe");

    #[derive(Debug, Default)]
    struct MapSource {
        blocks: BTreeMap<u64, ([u8; 32], u32)>,
        txs: BTreeMap<[u8; 32], Arc<IndexedTx>>,
        /// When set, every `tx()` read bumps the generation: simulates a
        /// Zcash reorg landing mid-execution.
        reorg_on_read: bool,
        generation: std::sync::atomic::AtomicU64,
    }

    impl ZcashSource for MapSource {
        fn epoch_base(&self) -> u64 {
            BASE
        }
        fn indexed_through(&self) -> Option<u64> {
            let mut through = None;
            for (h, _) in self.blocks.range(BASE..) {
                if *h != through.map_or(BASE, |t: u64| t + 1) {
                    break;
                }
                through = Some(*h);
            }
            through
        }
        fn block(&self, zcash_height: u64) -> Option<([u8; 32], u32)> {
            self.blocks.get(&zcash_height).copied()
        }
        fn tx(&self, txid: &[u8; 32]) -> Option<Arc<IndexedTx>> {
            if self.reorg_on_read {
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.txs.get(txid).cloned()
        }
        fn generation(&self) -> u64 {
            self.generation.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn hash(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    /// Blocks B ..= B+10 indexed; Sova blocks 10 and 11 anchor to B+9
    /// (hash 0xaa) and B+10 (hash 0xbb).
    fn map_source() -> MapSource {
        let mut m = MapSource::default();
        for i in 0..=10u64 {
            let tag = match i {
                9 => 0xaa,
                10 => 0xbb,
                _ => i as u8,
            };
            m.blocks
                .insert(BASE + i, (hash(tag), 1_700_000_000 + i as u32));
        }
        m
    }

    fn source() -> Arc<dyn ZcashSource> {
        Arc::new(map_source())
    }

    fn env(block_number: u64) -> EvmEnv {
        let block = BlockEnv {
            number: U256::from(block_number),
            ..Default::default()
        };
        EvmEnv::new(CfgEnv::new_with_spec(SpecId::PRAGUE), block)
    }

    fn funded_db() -> CacheDB<EmptyDB> {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            CALLER,
            AccountInfo::from_balance(U256::from(1_000_000_000_000u64)),
        );
        db
    }

    fn tx(to: Address, value: u64, data: &[u8]) -> TxEnv {
        TxEnv {
            caller: CALLER,
            kind: TxKind::Call(to),
            value: U256::from(value),
            data: Bytes::copy_from_slice(data),
            gas_limit: 1_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn selector_is_keccak_of_signature() {
        assert_eq!(keccak256("anchor()")[..4], ANCHOR_SELECTOR);
    }

    #[test]
    fn anchored_height_formula() {
        assert_eq!(anchored_height(U256::from(1), BASE), Some(BASE));
        assert_eq!(anchored_height(U256::from(10), BASE), Some(BASE + 9));
        assert_eq!(anchored_height(U256::ZERO, 0), None);
        assert_eq!(anchored_height(U256::MAX, BASE), None);
    }

    /// (a) Through a real EVM, the answer tracks the block env's number:
    /// the same calldata gives different bytes at blocks 10 and 11 — on a
    /// fresh EVM per block (how reth runs blocks) and on one EVM whose
    /// block env is swapped (nothing memoizes inside the precompile).
    #[test]
    fn anchor_answer_follows_block_number() {
        let factory = SovaEvmFactory::with_zcash_source(source());
        let want10 = encode_anchor(BASE + 9, hash(0xaa));
        let want11 = encode_anchor(BASE + 10, hash(0xbb));

        for (n, want) in [(10, &want10), (11, &want11)] {
            let mut evm = factory.create_evm(funded_db(), env(n));
            let res = evm
                .transact(tx(ZCASH_QUERY, 0, &ANCHOR_SELECTOR))
                .expect("anchor call executes");
            assert!(res.result.is_success(), "{:?}", res.result);
            assert_eq!(res.result.output(), Some(want));
        }

        let mut evm = factory.create_evm(funded_db(), env(10));
        let first = evm
            .transact(tx(ZCASH_QUERY, 0, &ANCHOR_SELECTOR))
            .expect("block 10");
        evm.ctx_mut().block.number = U256::from(11);
        let second = evm
            .transact(tx(ZCASH_QUERY, 0, &ANCHOR_SELECTOR))
            .expect("block 11");
        assert_eq!(first.result.output(), Some(&want10));
        assert_eq!(second.result.output(), Some(&want11));
        assert_ne!(want10, want11);
    }

    /// (b) The registered precompile is non-cacheable, and the exact call
    /// reth's engine makes (`map_cacheable_precompiles`) skips it while
    /// still visiting the stock precompiles.
    #[test]
    fn precompile_is_not_cacheable() {
        let mut evm = SovaEvmFactory::with_zcash_source(source()).create_evm(funded_db(), env(10));
        let cacheable = evm
            .precompiles()
            .get(&ZCASH_QUERY)
            .expect("registered at 0x5A00")
            .supports_caching();
        assert!(!cacheable);
        assert!(!zcash_query_precompile(None).supports_caching());

        let mut wrapped = Vec::new();
        evm.precompiles_mut().map_cacheable_precompiles(|addr, p| {
            wrapped.push(*addr);
            p
        });
        assert!(!wrapped.contains(&ZCASH_QUERY));
        assert!(wrapped.contains(&address!("0x0000000000000000000000000000000000000001")));
    }

    /// (c) A miss is fatal: revm returns `EVMError::Custom`, not a revert,
    /// and alloy-evm's block executor classifies it as
    /// `BlockExecutionError::Internal` (engine: processing error, block not
    /// marked invalid). Covers both "no index" and "height not indexed".
    #[test]
    fn missing_anchor_is_fatal() {
        let cases = [
            SovaEvmFactory::with_zcash_source(source()), // block 12 not indexed
            SovaEvmFactory::with_zcash_source(Arc::new(MapSource::default())),
        ];
        for factory in cases {
            let mut evm = factory.create_evm(funded_db(), env(12));
            let err = evm
                .transact(tx(ZCASH_QUERY, 0, &ANCHOR_SELECTOR))
                .expect_err("miss must abort, not revert");
            let EVMError::Custom(msg) = &err else {
                panic!("expected EVMError::Custom, got {err:?}");
            };
            assert!(msg.starts_with("fatal: sova zcash precompile"), "{msg}");
            assert!(matches!(
                BlockExecutionError::evm(err, B256::ZERO),
                BlockExecutionError::Internal(InternalBlockExecutionError::EVM { .. })
            ));
        }

        // No source at all (a node with no zcash index).
        let input_fatal =
            call_zcash_query(None, &dummy_input(&ANCHOR_SELECTOR, U256::ZERO, 10_000));
        assert!(matches!(input_fatal, Err(PrecompileError::Fatal(_))));
    }

    /// (d) value > 0 on a direct call reverts (and the value stays with
    /// the caller); malformed calldata reverts; low gas halts OOG. None of
    /// these consult the index, so they are identical on every node.
    #[test]
    fn value_malformed_and_oog_do_not_touch_the_index() {
        // Empty source: any index lookup would be fatal.
        let factory = SovaEvmFactory::with_zcash_source(Arc::new(MapSource::default()));

        let mut evm = factory.create_evm(funded_db(), env(10));
        let res = evm
            .transact(tx(ZCASH_QUERY, 1, &ANCHOR_SELECTOR))
            .expect("executes");
        assert!(
            matches!(res.result, ExecutionResult::Revert { .. }),
            "{:?}",
            res.result
        );
        assert!(
            !res.state
                .get(&ZCASH_QUERY)
                .is_some_and(|a| !a.info.balance.is_zero())
        );

        for bad in [
            &[][..],
            &[0xd3, 0xfb, 0x73][..],
            &[0, 0, 0, 0][..],
            &[0xd3, 0xfb, 0x73, 0xb4, 0][..],
        ] {
            let res = evm.transact(tx(ZCASH_QUERY, 0, bad)).expect("executes");
            assert!(
                matches!(res.result, ExecutionResult::Revert { .. }),
                "{bad:?}: {:?}",
                res.result
            );
        }

        let oog = call_zcash_query(
            None,
            &dummy_input(&ANCHOR_SELECTOR, U256::ZERO, ANCHOR_GAS - 1),
        )
        .expect("oog is not fatal");
        assert!(oog.is_halt());
    }

    // --- Contract-level tests: CALL / STATICCALL / DELEGATECALL + warm set ---

    const CALLER_CONTRACT: Address = address!("0x00000000000000000000000000000000000ca11e");

    /// Bytecode: mstore(0, anchor selector); ok := <op>(gas, target, [value],
    /// 0, 4, 0, 0x40); mstore(0x40, ok); return(0, 0x60).
    fn caller_code(op: u8, target: u16, call_value: u8) -> Bytecode {
        let mut c = vec![0x63];
        c.extend_from_slice(&ANCHOR_SELECTOR); // PUSH4 selector
        c.extend_from_slice(&[0x60, 0xe0, 0x1b, 0x60, 0x00, 0x52]); // <<224, MSTORE at 0
        c.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0x60, 0x04, 0x60, 0x00]); // ret/args
        if op == 0xf1 {
            c.extend_from_slice(&[0x60, call_value]); // value (CALL only)
        }
        c.push(0x61); // PUSH2 target
        c.extend_from_slice(&target.to_be_bytes());
        c.extend_from_slice(&[0x5a, op]); // GAS, <op>
        c.extend_from_slice(&[0x60, 0x40, 0x52]); // MSTORE(0x40, ok)
        c.extend_from_slice(&[0x60, 0x60, 0x60, 0x00, 0xf3]); // RETURN(0, 0x60)
        Bytecode::new_raw(Bytes::from(c))
    }

    fn run_contract(code: Bytecode, tx_value: u64) -> ExecutionResult {
        let mut db = funded_db();
        db.insert_account_info(CALLER_CONTRACT, AccountInfo::from_bytecode(code));
        let mut evm = SovaEvmFactory::with_zcash_source(source()).create_evm(db, env(10));
        evm.transact(tx(CALLER_CONTRACT, tx_value, &[]))
            .expect("executes")
            .result
    }

    fn inner_ok(res: &ExecutionResult) -> bool {
        let out = res.output().expect("output");
        out[64..96] != [0u8; 32]
    }

    const STATICCALL: u8 = 0xfa;
    const DELEGATECALL: u8 = 0xf4;
    const CALL: u8 = 0xf1;

    #[test]
    fn staticcall_and_delegatecall_work_value_call_reverts() {
        let want = encode_anchor(BASE + 9, hash(0xaa));

        let res = run_contract(caller_code(STATICCALL, 0x5a00, 0), 0);
        assert!(inner_ok(&res));
        assert_eq!(&res.output().expect("output")[..64], &want[..]);

        // DELEGATECALL from a frame that received value: apparent value 1,
        // not a transfer — must still succeed.
        let res = run_contract(caller_code(DELEGATECALL, 0x5a00, 0), 1);
        assert!(inner_ok(&res));
        assert_eq!(&res.output().expect("output")[..64], &want[..]);

        // CALL with value 1 (a real transfer) is rejected.
        let res = run_contract(caller_code(CALL, 0x5a00, 1), 1);
        assert!(res.is_success());
        assert!(!inner_ok(&res));
    }

    /// Q4: 0x5A00 is in the EIP-2929 warm set at tx start. Calling it costs
    /// 100 (warm) + 200 (anchor) = 300; calling an empty non-precompile
    /// address costs 2,600 (cold) — so the delta is exactly −2,300. If the
    /// precompile were cold the delta would be +200.
    #[test]
    fn precompile_address_is_warm() {
        let to_precompile = run_contract(caller_code(STATICCALL, 0x5a00, 0), 0);
        let to_empty = run_contract(caller_code(STATICCALL, 0x5b00, 0), 0);
        assert!(inner_ok(&to_precompile));
        let delta = to_precompile.tx_gas_used() as i64 - to_empty.tx_gas_used() as i64;
        assert_eq!(delta, 100 + ANCHOR_GAS as i64 - 2_600);
    }

    /// A `PrecompileInput` for calling the body directly (no EVM).
    fn dummy_input(data: &'static [u8], value: U256, gas: u64) -> PrecompileInput<'static> {
        let ctx: &'static mut EthEvmContext<CacheDB<EmptyDB>> = Box::leak(Box::new(
            EthEvmContext::new(CacheDB::new(EmptyDB::default()), SpecId::PRAGUE),
        ));
        PrecompileInput {
            data,
            gas,
            reservoir: 0,
            caller: CALLER,
            value,
            target_address: ZCASH_QUERY,
            is_static: false,
            bytecode_address: ZCASH_QUERY,
            internals: EvmInternals::from_context(ctx),
        }
    }

    // --- v1 queries: blockAt / txInfo / txOutput / burnInfo -------------

    fn call(src: &MapSource, sova_block: u64, data: &[u8]) -> Bytes {
        let factory = SovaEvmFactory::with_zcash_source(Arc::new(MapSource {
            blocks: src.blocks.clone(),
            txs: src.txs.clone(),
            ..MapSource::default()
        }));
        let mut evm = factory.create_evm(funded_db(), env(sova_block));
        let res = evm.transact(tx(ZCASH_QUERY, 0, data)).expect("executes");
        assert!(res.result.is_success(), "{:?}", res.result);
        res.result.output().expect("output").clone()
    }

    fn word(out: &[u8], i: usize) -> &[u8] {
        &out[i * 32..(i + 1) * 32]
    }

    fn as_u64(w: &[u8]) -> u64 {
        assert!(w[..24].iter().all(|b| *b == 0), "dirty high bytes");
        u64::from_be_bytes(w[24..].try_into().expect("8 bytes"))
    }

    fn calldata(sel: [u8; 4], words: &[[u8; 32]]) -> Vec<u8> {
        let mut d = sel.to_vec();
        for w in words {
            d.extend_from_slice(w);
        }
        d
    }

    const PAY_TXID: [u8; 32] = [0x11; 32];
    const BURN_TXID: [u8; 32] = [0x22; 32];
    const LATE_TXID: [u8; 32] = [0x33; 32];

    /// A payment at B+3, a SIP-1 burn at B+5, and a tx the index holds
    /// at B+10 (above the anchor of Sova block 10).
    fn with_txs() -> MapSource {
        let mut m = map_source();
        m.txs.insert(
            PAY_TXID,
            Arc::new(IndexedTx::new(
                BASE + 3,
                1,
                5,
                vec![(150_000, vec![0x76, 0xa9]), (7, vec![0x51; 40])],
            )),
        );
        let payload = consensus::sip1::BurnPayload {
            evm_address: [0xab; 20],
            signal_bits: 3,
        };
        m.txs.insert(
            BURN_TXID,
            Arc::new(IndexedTx::new(
                BASE + 5,
                2,
                6,
                vec![
                    (0, payload.to_script().to_vec()),
                    (90_000, consensus::sip1::burn_lock_script().to_vec()),
                ],
            )),
        );
        m.txs.insert(
            LATE_TXID,
            Arc::new(IndexedTx::new(BASE + 10, 0, 5, vec![(1, vec![0x51])])),
        );
        m
    }

    #[test]
    fn selectors_are_keccak_of_signatures() {
        for (sig, sel) in [
            ("blockAt(uint64)", BLOCK_AT_SELECTOR),
            ("txInfo(bytes32)", TX_INFO_SELECTOR),
            ("txOutput(bytes32,uint32)", TX_OUTPUT_SELECTOR),
            ("burnInfo(bytes32)", BURN_INFO_SELECTOR),
        ] {
            assert_eq!(keccak256(sig)[..4], sel, "{sig}");
        }
    }

    #[test]
    fn block_at_statuses_and_horizon() {
        let m = with_txs();
        // Sova block 10 anchors to E = B+9.
        let at = |h: u64| call(&m, 10, &calldata(BLOCK_AT_SELECTOR, &[word_u64(h)]));
        let out = at(BASE + 9);
        assert_eq!(as_u64(word(&out, 0)), u64::from(status::OK));
        assert_eq!(word(&out, 1), &hash(0xaa));
        assert_eq!(as_u64(word(&out, 2)), 1_700_000_009);
        // B+10 is indexed but above the anchor: NOT_YET, not its hash.
        assert_eq!(as_u64(word(&at(BASE + 10), 0)), u64::from(status::NOT_YET));
        assert_eq!(
            as_u64(word(&at(BASE - 1), 0)),
            u64::from(status::OUT_OF_RANGE)
        );
        assert_eq!(as_u64(word(&at(BASE), 0)), u64::from(status::OK));
    }

    #[test]
    fn tx_info_confirmations_move_with_the_block_and_the_horizon_holds() {
        let m = with_txs();
        let q = calldata(TX_INFO_SELECTOR, &[PAY_TXID]);
        let at10 = call(&m, 10, &q); // E = B+9
        let at11 = call(&m, 11, &q); // E = B+10
        assert_eq!(as_u64(word(&at10, 0)), u64::from(status::OK));
        assert_eq!(as_u64(word(&at10, 1)), BASE + 3);
        assert_eq!(as_u64(word(&at10, 2)), 1);
        assert_eq!(as_u64(word(&at10, 3)), 7, "confirmations = E - h + 1");
        assert_eq!(as_u64(word(&at11, 3)), 8);
        assert_eq!(as_u64(word(&at10, 4)), 2, "nOut");
        assert_eq!(as_u64(word(&at10, 5)), 5, "version");
        // Indexed at B+10 but invisible from Sova block 10 (E = B+9).
        let late = calldata(TX_INFO_SELECTOR, &[LATE_TXID]);
        assert_eq!(
            as_u64(word(&call(&m, 10, &late), 0)),
            u64::from(status::NOT_FOUND)
        );
        assert_eq!(as_u64(word(&call(&m, 11, &late), 0)), u64::from(status::OK));
        // Unknown: NOT_FOUND with every other word zero.
        let none = call(&m, 10, &calldata(TX_INFO_SELECTOR, &[[0x99; 32]]));
        assert_eq!(none.len(), 6 * 32);
        assert_eq!(as_u64(word(&none, 0)), u64::from(status::NOT_FOUND));
        assert!(none[32..].iter().all(|b| *b == 0));
    }

    #[test]
    fn tx_output_abi_statuses_and_per_byte_gas() {
        let m = with_txs();
        let q = |vout: u64| calldata(TX_OUTPUT_SELECTOR, &[PAY_TXID, word_u64(vout)]);
        let out = call(&m, 10, &q(1));
        assert_eq!(as_u64(word(&out, 0)), u64::from(status::OK));
        assert_eq!(as_u64(word(&out, 1)), 7);
        assert_eq!(as_u64(word(&out, 2)), 0x60, "dynamic offset");
        assert_eq!(as_u64(word(&out, 3)), 40, "script length");
        assert_eq!(&out[128..168], &[0x51; 40]);
        assert_eq!(out.len(), 128 + 64, "padded to a word boundary");
        let missing = call(&m, 10, &q(2));
        assert_eq!(as_u64(word(&missing, 0)), u64::from(status::NO_SUCH_OUTPUT));
        assert_eq!(as_u64(word(&missing, 2)), 0x60);
        assert_eq!(as_u64(word(&missing, 3)), 0);
        let unknown = call(
            &m,
            10,
            &calldata(TX_OUTPUT_SELECTOR, &[[0x99; 32], word_u64(0)]),
        );
        assert_eq!(as_u64(word(&unknown, 0)), u64::from(status::NOT_FOUND));

        // Gas: 4,000 + 8 * 40 for the 40-byte script, halting OOG below it.
        let input = |gas| {
            let data: &'static [u8] = Box::leak(q(1).into_boxed_slice());
            dummy_input_at(data, gas, 10)
        };
        let src = with_txs();
        let paid = call_zcash_query(Some(&src), &input(TX_GAS + 320)).expect("not fatal");
        assert_eq!(paid.gas_used, TX_GAS + 320);
        let short = call_zcash_query(Some(&src), &input(TX_GAS + 319)).expect("not fatal");
        assert!(short.is_halt());
    }

    #[test]
    fn burn_info_uses_the_consensus_parser() {
        let m = with_txs();
        let out = call(&m, 10, &calldata(BURN_INFO_SELECTOR, &[BURN_TXID]));
        assert_eq!(as_u64(word(&out, 0)), u64::from(status::OK));
        assert_eq!(&word(&out, 1)[12..], &[0xab; 20]);
        assert_eq!(as_u64(word(&out, 2)), 3, "signal bits");
        assert_eq!(as_u64(word(&out, 3)), 90_000, "zatoshis destroyed");
        let not_burn = call(&m, 10, &calldata(BURN_INFO_SELECTOR, &[PAY_TXID]));
        assert_eq!(as_u64(word(&not_burn, 0)), u64::from(status::NOT_A_BURN));
        let unknown = call(&m, 10, &calldata(BURN_INFO_SELECTOR, &[[0x99; 32]]));
        assert_eq!(as_u64(word(&unknown, 0)), u64::from(status::NOT_FOUND));
    }

    /// The burn cached by `IndexedTx::new` is exactly the consensus
    /// parser's answer over the record's outputs (burn, non-burn, and a
    /// tx with two payloads, which the parser rejects as ambiguous).
    #[test]
    fn cached_burn_equals_the_parser() {
        let parse = |t: &IndexedTx| {
            consensus::sip1::extract_burn(t.outputs.iter().map(|(value_zat, script)| {
                consensus::sip1::TxOutRef {
                    value_zat: *value_zat,
                    script: script.as_slice(),
                }
            }))
        };
        let m = with_txs();
        let burn = m.txs.get(&BURN_TXID).expect("burn fixture");
        let mut ambiguous = burn.outputs.clone();
        ambiguous.push(burn.outputs[0].clone());
        let ambiguous = IndexedTx::new(burn.height, 3, 5, ambiguous);
        assert!(burn.burn().is_some());
        assert!(ambiguous.burn().is_none());
        for t in m.txs.values().map(|t| &**t).chain([&ambiguous]) {
            assert_eq!(t.burn(), parse(t));
        }
    }

    /// A hole in the index below the anchor must be fatal, never a
    /// node-specific NOT_FOUND.
    #[test]
    fn a_hole_below_the_anchor_is_fatal_not_not_found() {
        let mut m = with_txs();
        m.blocks.remove(&(BASE + 4));
        let src: Arc<dyn ZcashSource> = Arc::new(m);
        let mut evm = SovaEvmFactory::with_zcash_source(src).create_evm(funded_db(), env(10));
        let err = evm
            .transact(tx(
                ZCASH_QUERY,
                0,
                &calldata(TX_INFO_SELECTOR, &[[0x99; 32]]),
            ))
            .expect_err("hole must be fatal");
        assert!(matches!(err, EVMError::Custom(_)), "{err:?}");
    }

    #[test]
    fn malformed_arguments_revert() {
        let m = with_txs();
        let factory = SovaEvmFactory::with_zcash_source(Arc::new(m));
        let mut dirty_u64 = word_u64(BASE);
        dirty_u64[0] = 1;
        let mut dirty_u32 = word_u64(0);
        dirty_u32[27] = 1; // bit above u32
        for bad in [
            calldata(BLOCK_AT_SELECTOR, &[dirty_u64]),
            calldata(TX_OUTPUT_SELECTOR, &[PAY_TXID, dirty_u32]),
            calldata(TX_INFO_SELECTOR, &[]),
            calldata(TX_INFO_SELECTOR, &[PAY_TXID, PAY_TXID]),
            [calldata(TX_INFO_SELECTOR, &[PAY_TXID]), vec![0]].concat(),
        ] {
            let mut evm = factory.create_evm(funded_db(), env(10));
            let res = evm.transact(tx(ZCASH_QUERY, 0, &bad)).expect("executes");
            assert!(
                matches!(res.result, ExecutionResult::Revert { .. }),
                "{bad:?}"
            );
        }
    }

    fn dummy_input_at(data: &'static [u8], gas: u64, block: u64) -> PrecompileInput<'static> {
        let mut ctx = EthEvmContext::new(CacheDB::new(EmptyDB::default()), SpecId::PRAGUE);
        ctx.block.number = U256::from(block);
        let ctx: &'static mut EthEvmContext<CacheDB<EmptyDB>> = Box::leak(Box::new(ctx));
        PrecompileInput {
            data,
            gas,
            reservoir: 0,
            caller: CALLER,
            value: U256::ZERO,
            target_address: ZCASH_QUERY,
            is_static: false,
            bytecode_address: ZCASH_QUERY,
            internals: EvmInternals::from_context(ctx),
        }
    }

    /// A Zcash reorg that touches the index while a block executes must
    /// refuse the block (fatal), never answer from the new branch on this
    /// node only.
    #[test]
    fn a_reorg_during_execution_is_fatal() {
        let mut m = with_txs();
        m.reorg_on_read = true;
        let mut evm =
            SovaEvmFactory::with_zcash_source(Arc::new(m)).create_evm(funded_db(), env(10));
        let err = evm
            .transact(tx(ZCASH_QUERY, 0, &calldata(TX_INFO_SELECTOR, &[PAY_TXID])))
            .expect_err("mid-execution reorg must be fatal");
        let EVMError::Custom(msg) = &err else {
            panic!("expected a fatal error, got {err:?}");
        };
        assert!(msg.contains("reorged during execution"), "{msg}");
    }
}
