//! `fetch` (NEAR → a `.sovada` file set) and `verify` (a file set, offline,
//! optionally byte-compared against a Sova node).
//!
//! Output layout, consumed by the rebuild tool:
//!
//! ```text
//! <dir>/000000000000-000000000119.sovada   one file per batch, the exact
//! <dir>/000000000120-000000000239.sovada   bytes posted (sha256 = index)
//! <dir>/index.json                         contract, chain, every batch
//! ```
//!
//! File names are `<first>-<last>.sovada` with 12-digit zero-padded heights,
//! so a lexical sort is height order.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::archive::{self, Entry, Info, Sources};
use crate::near::Rpc;
use crate::poster::{keccak, write_atomic};
use crate::sova::Sova;

/// The file name for a batch.
pub fn file_name(first: u64, last: u64) -> String {
    format!("{first:012}-{last:012}.sovada")
}

#[derive(Serialize)]
struct Manifest<'a> {
    contract: &'a str,
    info: &'a Info,
    batches: Vec<ManifestBatch>,
}

#[derive(Serialize)]
struct ManifestBatch {
    file: String,
    #[serde(flatten)]
    entry: Entry,
}

/// What `fetch` did.
pub struct FetchReport {
    /// Batches checked.
    pub batches: usize,
    /// Blocks checked.
    pub blocks: u64,
    /// Batches downloaded (the rest were already on disk and matched).
    pub downloaded: usize,
    /// First and last archived height.
    pub range: Option<(u64, u64)>,
}

/// Download and check every batch of `contract` into `dir`.
pub fn fetch(
    index_rpc: &Rpc,
    sources: &Sources,
    contract: &str,
    dir: &Path,
    expect_chain_id: Option<u64>,
    verbose: bool,
) -> Result<FetchReport, String> {
    let info = archive::info(index_rpc, contract)?;
    if info.format != "SOVADA1" {
        return Err(format!("contract format is {}, not SOVADA1", info.format));
    }
    if let Some(c) = expect_chain_id
        && c != info.chain_id
    {
        return Err(format!(
            "contract archives chain {}, expected {c}",
            info.chain_id
        ));
    }
    let entries = archive::list(index_rpc, contract, 0)?;
    if entries.len() as u64 != info.batch_count {
        return Err(format!(
            "index lists {} batches, info says {}",
            entries.len(),
            info.batch_count
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut expected = info.start_height;
    let mut prev: Option<[u8; 32]> = if info.start_height == 0 {
        Some([0u8; 32])
    } else {
        None
    };
    let mut report = FetchReport {
        batches: 0,
        blocks: 0,
        downloaded: 0,
        range: None,
    };
    let mut manifest = Vec::new();
    for e in &entries {
        if e.first_height != expected {
            return Err(format!(
                "index batch {} starts at {}, expected {expected}",
                e.index, e.first_height
            ));
        }
        let path = dir.join(file_name(e.first_height, e.last_height));
        let on_disk = std::fs::read(&path)
            .ok()
            .filter(|b| archive::sha256_hex(b) == e.sha256);
        let bytes = match on_disk {
            Some(b) => b,
            None => {
                let f = archive::fetch(sources, contract, &info.owner, e)?;
                if verbose {
                    eprintln!(
                        "batch #{} heights {}..={} ({} bytes) from {} tx {}",
                        e.index,
                        e.first_height,
                        e.last_height,
                        f.bytes.len(),
                        f.source,
                        f.tx_hash
                    );
                }
                report.downloaded += 1;
                f.bytes
            }
        };
        let last = check_batch(&bytes, info.chain_id, e.first_height, prev)
            .map_err(|err| format!("batch #{} ({}): {err}", e.index, path.display()))?;
        let (count, last_height) = {
            let h = sovada::parse_header(&bytes).map_err(|err| err.to_string())?;
            (h.count, h.last_height())
        };
        if count != e.count || last_height != e.last_height {
            return Err(format!(
                "batch #{}: bytes disagree with the index's range",
                e.index
            ));
        }
        if format!("0x{}", hex::encode(last)) != e.last_hash {
            return Err(format!(
                "batch #{}: last block hash disagrees with the index",
                e.index
            ));
        }
        if !path.exists() {
            write_atomic(&path, &bytes)?;
        }
        prev = Some(last);
        expected = e.last_height + 1;
        report.batches += 1;
        report.blocks += u64::from(e.count);
        report.range = Some((report.range.map_or(e.first_height, |r| r.0), e.last_height));
        manifest.push(ManifestBatch {
            file: file_name(e.first_height, e.last_height),
            entry: e.clone(),
        });
    }
    if expected != info.next_height {
        return Err(format!(
            "batches end at {}, but the index's next height is {}",
            expected, info.next_height
        ));
    }
    if prev.is_some() && info.batch_count > 0 {
        let want = info.last_hash_bytes()?;
        if want != prev {
            return Err("last block hash disagrees with the index's info".to_owned());
        }
    }
    let m = Manifest {
        contract,
        info: &info,
        batches: manifest,
    };
    let data = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("index.json"), &data)?;
    Ok(report)
}

/// Parse and check one batch: format, chain, start height, every block's
/// hash/number/parent. Returns the last block's hash.
pub fn check_batch(
    bytes: &[u8],
    chain_id: u64,
    first_height: u64,
    prev: Option<[u8; 32]>,
) -> Result<[u8; 32], String> {
    let batch = sovada::parse(bytes).map_err(|e| e.to_string())?;
    if batch.header.chain_id != chain_id {
        return Err(sovada::Error::WrongChain {
            expected: chain_id,
            got: batch.header.chain_id,
        }
        .to_string());
    }
    if batch.header.first_height != first_height {
        return Err(sovada::Error::WrongStart {
            expected: first_height,
            got: batch.header.first_height,
        }
        .to_string());
    }
    sovada::verify_blocks(&batch, &keccak, prev).map_err(|e| e.to_string())
}

/// What `verify` found.
pub struct VerifyReport {
    /// Files checked.
    pub files: usize,
    /// Blocks checked.
    pub blocks: u64,
    /// First and last height.
    pub range: (u64, u64),
    /// Chain id.
    pub chain_id: u64,
    /// The last block's hash.
    pub last_hash: [u8; 32],
    /// Blocks byte-compared against a node.
    pub compared: u64,
}

/// The `.sovada` files of `dir`, in height order.
pub fn list_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sovada"))
        .collect();
    files.sort();
    Ok(files)
}

/// Check a `.sovada` file set offline: contiguous, one chain, hash-linked,
/// starting at `start` if given. With `sova`, also byte-compare every block
/// with the node's `debug_getRawBlock` and its hash with
/// `eth_getBlockByNumber`.
pub fn verify(
    dir: &Path,
    expect_chain_id: Option<u64>,
    start: Option<u64>,
    sova: Option<&Sova>,
) -> Result<VerifyReport, String> {
    let files = list_files(dir)?;
    if files.is_empty() {
        return Err(format!("no .sovada files in {}", dir.display()));
    }
    let mut chain_id = expect_chain_id;
    let mut expected = start;
    let mut prev: Option<[u8; 32]> = None;
    let mut first_height = None;
    let mut blocks = 0u64;
    let mut compared = 0u64;
    for path in &files {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let header =
            sovada::parse_header(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let want_name = file_name(header.first_height, header.last_height());
        if path.file_name().and_then(|n| n.to_str()) != Some(want_name.as_str()) {
            return Err(format!("{}: holds heights {want_name}", path.display()));
        }
        let cid = *chain_id.get_or_insert(header.chain_id);
        let first = *expected.get_or_insert(header.first_height);
        if prev.is_none() && first == 0 {
            prev = Some([0u8; 32]);
        }
        first_height.get_or_insert(first);
        let last = check_batch(&bytes, cid, first, prev)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(node) = sova {
            let batch = sovada::parse(&bytes).map_err(|e| e.to_string())?;
            for b in &batch.blocks {
                let (hash, raw) = node.raw_block(b.height)?;
                if hash != b.hash {
                    return Err(format!("block {}: hash differs from the node's", b.height));
                }
                if raw != b.raw {
                    return Err(format!(
                        "block {}: bytes differ from the node's ({})",
                        b.height,
                        node.raw_source().map(|s| s.describe()).unwrap_or("?")
                    ));
                }
                compared += 1;
            }
        }
        blocks += u64::from(header.count);
        prev = Some(last);
        expected = Some(header.last_height() + 1);
    }
    let first = first_height.unwrap_or(0);
    Ok(VerifyReport {
        files: files.len(),
        blocks,
        range: (first, expected.unwrap_or(1) - 1),
        chain_id: chain_id.unwrap_or(0),
        last_hash: prev.unwrap_or([0u8; 32]),
        compared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_by_height() {
        let mut v = [
            file_name(1000, 1099),
            file_name(0, 999),
            file_name(1100, 1100),
        ];
        v.sort();
        assert_eq!(v[0], "000000000000-000000000999.sovada");
        assert_eq!(v[2], "000000001100-000000001100.sovada");
    }
}
