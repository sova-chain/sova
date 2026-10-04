//! `sova-rebuild export`: a node's blocks into batch files, so a sim (or
//! anyone) can make an archive without NEAR.
//!
//! Raw blocks come from `debug_getRawBlock` when the node serves it
//! (bin/sova's local RPC profile serves only `eth`/`net`/`web3` unless
//! `SOVA_RPC_DEBUG=1`), otherwise they are rebuilt from the standard `eth`
//! namespace: `eth_getBlockByNumber` for the header and withdrawals and
//! `eth_getRawTransactionByBlockHashAndIndex` for each transaction's
//! EIP-2718 bytes, re-encoded as the block's RLP. Either way every block
//! goes through the archive checks before it is written (deep: the
//! re-encoded header must hash to the node's block hash and the body must
//! match the header's roots, so a reconstruction that is not byte-exact
//! cannot be written), and the chain must link from `--from` to `--to`.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::B256;
use serde_json::{Value, json};

use crate::{
    archive::{CheckOptions, Checked, Checker},
    batch::{self, ArchivedBlock, BatchHeader, Entry},
    rpc::{Endpoint, SHORT, hex_bytes},
};

/// Where raw blocks come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawSource {
    /// `debug_getRawBlock` if the node serves it, else reconstruct.
    Auto,
    /// `debug_getRawBlock` only.
    Debug,
    /// Reconstruct from the `eth` namespace only.
    Reconstruct,
}

/// What to export.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// First height (≥ 1; genesis is not archived).
    pub from: u64,
    /// Last height (default: the node's head).
    pub to: Option<u64>,
    /// Blocks per batch file.
    pub batch_size: u32,
    /// Output directory (created).
    pub out: PathBuf,
    /// Raw block source.
    pub raw: RawSource,
}

/// What an export did.
#[derive(Debug, Clone)]
pub struct ExportReport {
    /// The node's chain ID (written into every batch).
    pub chain_id: u64,
    /// First height exported.
    pub from: u64,
    /// Last height exported.
    pub to: u64,
    /// Files written.
    pub files: Vec<PathBuf>,
    /// Whether raw blocks came from `debug_getRawBlock`.
    pub used_debug: bool,
    /// Last block's hash.
    pub last_hash: B256,
    /// Wall time, seconds.
    pub secs: f64,
}

/// Exports `opts.from..=opts.to` from the node at `rpc`.
pub fn export(rpc: &Endpoint, opts: &ExportOptions) -> eyre::Result<ExportReport> {
    let started = Instant::now();
    if opts.from == 0 {
        return Err(eyre::eyre!(
            "--from must be ≥ 1 (genesis is the chain spec's, not archived)"
        ));
    }
    if opts.batch_size == 0 {
        return Err(eyre::eyre!("--batch-size must be ≥ 1"));
    }
    let chain_id = rpc.chain_id()?;
    let to = match opts.to {
        Some(t) => t,
        None => rpc.block_number()?,
    };
    if to < opts.from {
        return Err(eyre::eyre!(
            "nothing to export: --to {to} < --from {}",
            opts.from
        ));
    }
    std::fs::create_dir_all(&opts.out).map_err(|e| eyre::eyre!("{}: {e}", opts.out.display()))?;
    let used_debug = match opts.raw {
        RawSource::Debug => true,
        RawSource::Reconstruct => false,
        RawSource::Auto => match raw_via_debug(rpc, B256::ZERO) {
            Err(e) if e.is_method_not_found() => {
                eprintln!(
                    "export: node does not serve debug_getRawBlock; rebuilding raw blocks from eth_getBlockByNumber + eth_getRawTransactionByBlockHashAndIndex"
                );
                false
            }
            _ => true,
        },
    };
    // The block below `from` anchors the first parent link.
    let parent_of_first = rpc
        .block_at(opts.from - 1)?
        .map(|b| b.hash)
        .ok_or_else(|| eyre::eyre!("node has no block {}", opts.from - 1))?;
    let mut checker = Checker::new(CheckOptions {
        chain_id: Some(chain_id),
        parent_of_first: Some(parent_of_first),
        deep: true,
        sip6: false,
    });

    let mut files = Vec::new();
    let mut pending: Vec<ArchivedBlock> = Vec::new();
    let mut last_hash = B256::ZERO;
    let mut last_report = Instant::now();
    for height in opts.from..=to {
        let (hash, raw) = if used_debug {
            let hash = rpc
                .block_at(height)?
                .map(|b| b.hash)
                .ok_or_else(|| eyre::eyre!("node has no block {height}"))?;
            (hash, raw_via_debug(rpc, hash)?)
        } else {
            raw_via_eth(rpc, height)?
        };
        let block = ArchivedBlock { height, hash, raw };
        let entry = Entry {
            batch: BatchHeader {
                chain_id,
                first_height: height,
                count: 1,
            },
            index: 0,
            block,
        };
        match checker.check(&entry.batch, &entry) {
            Ok(Checked::Block(_)) => {}
            Ok(Checked::Duplicate { .. }) => {
                return Err(eyre::eyre!("export: height {height} read twice"));
            }
            Err(e) => {
                return Err(eyre::eyre!(
                    "export: block {height} from the node fails the archive checks ({e}); \
                     did the node reorg during the export?"
                ));
            }
        }
        last_hash = hash;
        pending.push(entry.block);
        if pending.len() as u32 == opts.batch_size || height == to {
            files.push(write_file(&opts.out, chain_id, &pending)?);
            pending.clear();
        }
        if last_report.elapsed().as_secs() >= 5 {
            eprintln!("export: through #{height} of {to}");
            last_report = Instant::now();
        }
    }
    Ok(ExportReport {
        chain_id,
        from: opts.from,
        to,
        files,
        used_debug,
        last_hash,
        secs: started.elapsed().as_secs_f64(),
    })
}

/// Batch file name: chain, first and last height, zero-padded so names
/// sort in chain order.
pub fn file_name(chain_id: u64, first: u64, last: u64) -> String {
    format!("sova-{chain_id}-{first:010}-{last:010}.sovada")
}

fn write_file(dir: &Path, chain_id: u64, blocks: &[ArchivedBlock]) -> eyre::Result<PathBuf> {
    let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
        return Err(eyre::eyre!("empty batch"));
    };
    let path = dir.join(file_name(chain_id, first.height, last.height));
    let tmp = dir.join(format!(
        ".{}.tmp",
        file_name(chain_id, first.height, last.height)
    ));
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        batch::write_batch(&mut w, chain_id, first.height, blocks)?;
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// `debug_getRawBlock(hash)`: by hash, so the bytes are the block we
/// named even if the canonical chain moves meanwhile.
fn raw_via_debug(rpc: &Endpoint, hash: B256) -> Result<Vec<u8>, crate::rpc::RpcError> {
    let v = rpc.call("debug_getRawBlock", json!([hash]), SHORT)?;
    hex_bytes(&v).ok_or_else(|| {
        crate::rpc::RpcError::Transport(format!("debug_getRawBlock: bad result {v:.100}"))
    })
}

/// The block's RLP rebuilt from the `eth` namespace.
fn raw_via_eth(rpc: &Endpoint, height: u64) -> eyre::Result<(B256, Vec<u8>)> {
    let v: Value = rpc.call(
        "eth_getBlockByNumber",
        json!([format!("0x{height:x}"), false]),
        SHORT,
    )?;
    if v.is_null() {
        return Err(eyre::eyre!("node has no block {height}"));
    }
    let blk: alloy_rpc_types_eth::Block = serde_json::from_value(v)
        .map_err(|e| eyre::eyre!("block {height}: unexpected eth_getBlockByNumber shape: {e}"))?;
    if !blk.uncles.is_empty() {
        return Err(eyre::eyre!(
            "block {height} has ommers; reconstruction can't fetch them (use debug_getRawBlock)"
        ));
    }
    let hash = blk.header.hash;
    let mut transactions = Vec::with_capacity(blk.transactions.len());
    for i in 0..blk.transactions.len() {
        let raw = rpc.call(
            "eth_getRawTransactionByBlockHashAndIndex",
            json!([hash, format!("0x{i:x}")]),
            SHORT,
        )?;
        let bytes = hex_bytes(&raw)
            .ok_or_else(|| eyre::eyre!("block {height} tx {i}: bad raw transaction {raw:.80}"))?;
        let tx = reth_ethereum::TransactionSigned::decode_2718(&mut bytes.as_slice())
            .map_err(|e| eyre::eyre!("block {height} tx {i}: {e}"))?;
        transactions.push(tx);
    }
    let block = reth_ethereum::Block {
        header: blk.header.inner,
        body: reth_ethereum::BlockBody {
            transactions,
            ommers: Vec::new(),
            withdrawals: blk.withdrawals,
        },
    };
    Ok((hash, alloy_rlp::encode(&block)))
}
