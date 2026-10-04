//! SIP-7 §4.1: the `ZcashBlocks` pre-block system call.
//!
//! [`SovaEvmConfig`] is reth's Ethereum EVM config over
//! [`crate::zcash::SovaEvmFactory`] with one change: its block executor,
//! after the standard pre-execution system calls (EIP-4788, EIP-2935) and
//! before the first transaction, records Zcash block `E_N`'s summary in the
//! `ZcashBlocks` contract at [`ZCASH_BLOCKS`] — EIP-4788's pattern: caller
//! [`SYSTEM_ADDRESS`], value 0, gas not counted against the block, state
//! committed. Only when SIP-7 is active and never at genesis.
//!
//! The calldata comes from the same index record the precompile serves
//! (`encode_zcash_blocks_record`), under the same guards: a missing record
//! or a failed call refuses the block as an execution error — not cached as
//! invalid, retried — like the precompile's fatal (SIP-4 §5). An honest
//! node's record for an anchored, scanned epoch is always present (the
//! follower holds instead of skipping), so a refusal means a local fault.
//!
//! The payload builder, the importer and the RPC all execute through this
//! one config, so the sealer and every validator compute the same write.

use std::sync::Arc;

use reth_ethereum::{
    Block, EthPrimitives, Receipt, TransactionSigned, TxType,
    chainspec::ChainSpec,
    evm::{
        EthBlockAssembler, EthEvm, EthEvmConfig, RethReceiptBuilder,
        primitives::{
            Evm, EvmEnv, EvmEnvFor, EvmFactory, ExecutionCtxFor, InspectorFor,
            NextBlockEnvAttributes,
            block::{BlockExecutorFactory, ExecutableTx, GasOutput, StateDB},
            eth::{EthBlockExecutionCtx, EthBlockExecutor, EthTxResult},
            execute::{BlockExecutionError, BlockExecutor, InternalBlockExecutionError},
            precompiles::PrecompilesMap,
        },
        revm::{
            DatabaseCommit,
            context::{Block as _, TxEnv},
            primitives::hardfork::SpecId,
        },
    },
    node::api::{ConfigureEngineEvm, ConfigureEvm, ExecutableTxIterator},
    primitives::{Header, SealedBlock, SealedHeader},
    provider::BlockExecutionResult,
    rpc::types::engine::ExecutionData,
};

use crate::zcash::{
    SYSTEM_ADDRESS, SovaEvmFactory, ZCASH_BLOCKS, anchored_height, encode_zcash_blocks_record,
    sip7_active, zcash_source,
};

/// Reth's Ethereum EVM config over [`SovaEvmFactory`], with the SIP-7
/// `ZcashBlocks` pre-block call in its block executor.
#[derive(Debug, Clone)]
pub struct SovaEvmConfig {
    inner: EthEvmConfig<ChainSpec, SovaEvmFactory>,
}

impl SovaEvmConfig {
    /// Wrap reth's config.
    #[must_use]
    pub const fn new(inner: EthEvmConfig<ChainSpec, SovaEvmFactory>) -> Self {
        Self { inner }
    }
}

impl BlockExecutorFactory for SovaEvmConfig {
    type EvmFactory = SovaEvmFactory;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type TxExecutionResult = EthTxResult<<SovaEvmFactory as EvmFactory>::HaltReason, TxType>;
    type Executor<'a, DB: StateDB, I: InspectorFor<Self, DB>> =
        SovaBlockExecutor<'a, EthEvm<DB, I, PrecompilesMap>>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EthEvm<DB, I, PrecompilesMap>,
        ctx: EthBlockExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: InspectorFor<Self, DB>,
    {
        SovaBlockExecutor {
            inner: EthBlockExecutor::new(
                evm,
                ctx,
                self.inner.chain_spec(),
                self.inner.executor_factory.receipt_builder(),
            ),
        }
    }
}

impl ConfigureEvm for SovaEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <EthEvmConfig<ChainSpec, SovaEvmFactory> as ConfigureEvm>::Error;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = Self;
    type BlockAssembler = EthBlockAssembler<ChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<Block>,
    ) -> Result<EthBlockExecutionCtx<'a>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<EthBlockExecutionCtx<'_>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

impl ConfigureEngineEvm<ExecutionData> for SovaEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}

/// reth's Ethereum block executor plus the SIP-7 pre-block record.
pub struct SovaBlockExecutor<'a, E> {
    inner: EthBlockExecutor<'a, E, &'a Arc<ChainSpec>, &'a RethReceiptBuilder>,
}

impl<E> BlockExecutor for SovaBlockExecutor<'_, E>
where
    E: Evm<DB: StateDB, Spec: Into<SpecId> + Clone, Tx = TxEnv>,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()?;
        if sip7_active() {
            // The block's anchor: the Zcash hash it commits to (the sealer
            // sets it from its own follower's epoch; engine::local).
            let anchor = self.inner.ctx.parent_beacon_block_root;
            let parent = self.inner.ctx.parent_hash;
            record_zcash_block(self.inner.evm_mut(), anchor, parent)?;
        }
        Ok(())
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }
}

fn refuse(why: String) -> BlockExecutionError {
    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
        format!("sova zcash-blocks: {why}").into(),
    ))
}

/// The record must describe the Zcash block the Sova block anchors to.
///
/// The anchor (`parent_beacon_block_root`) and the record come from two
/// different followers: the sealer's, which picked the epoch, and the
/// expectations follower's index, which holds the summary. During a Zcash
/// reorg they can briefly sit on different branches. On 2026-10-02 the
/// keeper sealed Sova #47,667 anchored to Zcash 4,436,166's canonical hash
/// while recording the summary of the block that reorg had replaced; its
/// header's state root committed to that stale record. Every validator
/// recording from its own (canonical) index computed another root: rpc-1
/// rejected the block and stalled at #47,666, and a block-by-block replay
/// of history still stops there (2026-10-04, the rebuild from NEAR).
///
/// So the indexed hash at the anchored height must equal the anchor, or
/// the block is refused as an execution error: retried, not cached as
/// invalid, like a missing record. A sealer then never builds on a stale
/// index, and a validator whose index is on another branch holds until it
/// isn't. `None` (no anchor field) only happens before Cancun, which no
/// Sova chain has; it is not checked.
fn check_record_anchor(
    zcash_height: u64,
    indexed: [u8; 32],
    anchor: Option<reth_ethereum::evm::revm::primitives::B256>,
) -> Result<(), String> {
    match anchor {
        Some(a) if a.0 != indexed => Err(format!(
            "indexed zcash block {zcash_height} is 0x{}, but the block anchors 0x{} \
             (our index is on another Zcash branch; retry once it follows the anchor)",
            hex_lower(&indexed),
            hex_lower(&a.0)
        )),
        _ => Ok(()),
    }
}

/// Recorded history that differs from the canonical Zcash data, kept so a
/// block-by-block replay reproduces the chain every node follows (the
/// pattern of Bitcoin's BIP30 exceptions): `(chain id, Sova block, its
/// parent's hash, the Zcash hash that block recorded)`.
///
/// Sova testnet #47,667 (2026-10-02, the keeper sealing alone during the
/// seed freeze) anchors Zcash 4,436,166's canonical block `0x0000051e…`
/// but recorded the hash of the block a Zcash reorg replaced at that
/// height; every other field of that record equals the canonical one. Its
/// header's state root, and every Sova header until that ring entry was
/// overwritten (#55,858), commit to it. The guard that now prevents this
/// is [`check_record_anchor`]; this entry only replays what happened.
const RECORD_EXCEPTIONS: &[(u64, u64, [u8; 32], [u8; 32])] = &[(
    82330,
    47_667,
    hex32("123a5a1b92bddd7835f1573627c7e1c33589903e469867146d968cc52288e3b8"),
    hex32("000004623f12a91d20c4556b1762692870ccd7ed2c09698caf292b6fd03c01dc"),
)];

/// The hash a historical record carries, if `(chain, number, parent)` is a
/// [`RECORD_EXCEPTIONS`] entry.
fn recorded_hash_exception(chain_id: u64, number: u64, parent: [u8; 32]) -> Option<[u8; 32]> {
    RECORD_EXCEPTIONS
        .iter()
        .find(|(c, n, p, _)| *c == chain_id && *n == number && *p == parent)
        .map(|(_, _, _, h)| *h)
}

/// A 32-byte value from 64 hex digits, at compile time.
const fn hex32(s: &str) -> [u8; 32] {
    const fn nib(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("hex32: not a lowercase hex digit"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len() == 64, "hex32: need 64 hex digits");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nib(b[2 * i]) << 4) | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}

fn hex_lower(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// The SIP-7 §4.1 system call for the block `evm` is executing.
fn record_zcash_block<E>(
    evm: &mut E,
    anchor: Option<reth_ethereum::evm::revm::primitives::B256>,
    parent: reth_ethereum::evm::revm::primitives::B256,
) -> Result<(), BlockExecutionError>
where
    E: Evm<DB: StateDB, Tx = TxEnv>,
{
    let number = evm.block().number();
    if number.is_zero() {
        return Ok(());
    }
    let Some(source) = zcash_source() else {
        return Err(refuse(format!(
            "no zcash index installed (sova block {number})"
        )));
    };
    let Some(e) = anchored_height(number, source.epoch_base()) else {
        return Err(refuse(format!(
            "anchored height out of range (sova block {number})"
        )));
    };
    let (Some((hash, time)), Some(summary)) = (source.block(e), source.block_summary(e)) else {
        return Err(refuse(format!(
            "no indexed record for zcash block {e} (sova block {number})"
        )));
    };
    check_record_anchor(e, hash, anchor).map_err(refuse)?;
    let n: u64 = number.try_into().unwrap_or(u64::MAX);
    let hash = recorded_hash_exception(evm.chain_id(), n, parent.0).unwrap_or(hash);
    let calldata = encode_zcash_blocks_record(e, hash, time, &summary);
    let beneficiary = evm.block().beneficiary();
    let res = evm
        .transact_system_call(SYSTEM_ADDRESS, ZCASH_BLOCKS, calldata)
        .map_err(|err| refuse(format!("system call failed: {err}")))?;
    if !res.result.is_success() {
        return Err(refuse(format!(
            "record for zcash block {e} reverted (sova block {number}): {:?}",
            res.result
        )));
    }
    let mut state = res.state;
    state.remove(&SYSTEM_ADDRESS);
    state.remove(&beneficiary);
    evm.db_mut().commit(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_ethereum::evm::revm::primitives::B256;

    #[test]
    fn a_record_must_describe_the_anchored_zcash_block() {
        let canonical = [0x05u8; 32];
        let stale = [0xafu8; 32];
        // The index follows the anchor: recorded.
        assert!(check_record_anchor(4_436_166, canonical, Some(B256::from(canonical))).is_ok());
        // 2026-10-02, Sova #47,667: anchored to the canonical block while the
        // index still held the replaced one. Refused, naming both hashes.
        let err = check_record_anchor(4_436_166, stale, Some(B256::from(canonical)))
            .err()
            .unwrap_or_default();
        assert!(err.contains("4436166"), "{err}");
        assert!(err.contains(&"af".repeat(32)), "{err}");
        assert!(err.contains(&"05".repeat(32)), "{err}");
        // No anchor field (pre-Cancun): nothing to compare against.
        assert!(check_record_anchor(1, stale, None).is_ok());
    }

    #[test]
    fn testnet_47667_replays_its_recorded_hash_and_nothing_else_does() {
        let parent = hex32("123a5a1b92bddd7835f1573627c7e1c33589903e469867146d968cc52288e3b8");
        let recorded = hex32("000004623f12a91d20c4556b1762692870ccd7ed2c09698caf292b6fd03c01dc");
        assert_eq!(
            recorded_hash_exception(82330, 47_667, parent),
            Some(recorded)
        );
        // Any other chain, height or parent: the canonical record.
        assert_eq!(recorded_hash_exception(1, 47_667, parent), None);
        assert_eq!(recorded_hash_exception(82330, 47_668, parent), None);
        assert_eq!(recorded_hash_exception(82330, 47_667, [0u8; 32]), None);
    }
}
