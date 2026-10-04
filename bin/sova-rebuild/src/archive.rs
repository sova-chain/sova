//! An archive (a set of batch files, or a stream of batches) read as one
//! ordered sequence of blocks, and the structural checks every block must
//! pass before anything is done with it.
//!
//! The checks are about the *archive*, not about consensus: the batches
//! agree on one chain ID, block heights are contiguous, each block's RLP
//! decodes, `keccak(rlp(header))` is the hash the archive recorded and its
//! `parent_hash` is the previous block's hash, every signed transaction
//! carries the batch's chain ID. With `deep`, the body must also match the
//! header's transactions/withdrawals/ommers roots; with `sip6`, every
//! header must be a null block or carry a valid SIP-6 seal under the chain
//! ID. A block that passes all of this can still be invalid (a wrong mint,
//! a wrong state root, a Zcash anchor our zebrad doesn't have): only the
//! node decides that.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use alloy_consensus::Transaction as _;
use alloy_primitives::{Address, B256};
use alloy_rlp::Decodable;
use reth_ethereum::primitives::BlockBody as _;

use crate::batch::{self, BatchError, BatchHeader, BatchReader, Entry};

/// Where batches come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A batch file (one or more batches back to back).
    File(PathBuf),
    /// Standard input (batches back to back).
    Stdin,
}

impl Source {
    fn label(&self) -> String {
        match self {
            Self::File(p) => p.display().to_string(),
            Self::Stdin => "<stdin>".to_owned(),
        }
    }
}

/// Expands command-line inputs into sources: `-` is stdin (only as the
/// last input), a directory contributes every regular, non-hidden file in
/// it. Files are ordered by their first batch's `first_height` (then by
/// path), so a directory of batches is read in chain order whatever the
/// file names are.
pub fn expand(inputs: &[String]) -> eyre::Result<Vec<Source>> {
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    let mut stdin = false;
    for (i, input) in inputs.iter().enumerate() {
        if input == "-" {
            if i + 1 != inputs.len() {
                return Err(eyre::eyre!("`-` (stdin) must be the last archive input"));
            }
            stdin = true;
            continue;
        }
        let path = PathBuf::from(input);
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)
                .map_err(|e| eyre::eyre!("{}: {e}", path.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                // A directory's batches are its `*.sovada` files: `sova-near-da
                // fetch` also writes an `index.json` there, and neither tool
                // names a batch any other way (explicit file arguments are
                // read whatever their name).
                .filter(|p| {
                    p.is_file()
                        && p.extension().is_some_and(|e| e == "sovada")
                        && !p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with('.'))
                })
                .collect();
            entries.sort();
            for p in entries {
                files.push((first_height_of(&p)?, p));
            }
        } else if path.is_file() {
            files.push((first_height_of(&path)?, path));
        } else {
            return Err(eyre::eyre!("{}: no such file or directory", path.display()));
        }
    }
    files.sort();
    let mut out: Vec<Source> = files.into_iter().map(|(_, p)| Source::File(p)).collect();
    if stdin {
        out.push(Source::Stdin);
    }
    if out.is_empty() {
        return Err(eyre::eyre!(
            "no archive input (give batch files, directories, or `-`)"
        ));
    }
    Ok(out)
}

/// The first batch header of a file (`u64::MAX` for an empty file, which
/// then reads as no blocks).
fn first_height_of(path: &Path) -> eyre::Result<u64> {
    let mut f = File::open(path).map_err(|e| eyre::eyre!("{}: {e}", path.display()))?;
    match batch::read_header(&mut f) {
        Ok(Some(h)) => Ok(h.first_height),
        Ok(None) => Ok(u64::MAX),
        Err(e) => Err(eyre::eyre!("{}: {e}", path.display())),
    }
}

/// Every block of every source, in order, labelled with its source.
pub struct ArchiveReader {
    sources: std::vec::IntoIter<Source>,
    current: Option<(String, BatchReader<Box<dyn Read>>)>,
}

impl std::fmt::Debug for ArchiveReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveReader")
            .field("current", &self.current.as_ref().map(|(l, _)| l))
            .finish_non_exhaustive()
    }
}

impl ArchiveReader {
    /// A reader over `sources`, in the given order.
    pub fn new(sources: Vec<Source>) -> Self {
        Self {
            sources: sources.into_iter(),
            current: None,
        }
    }
}

/// A read failure, with the source it happened in.
#[derive(Debug, thiserror::Error)]
#[error("{source_label}: {error}")]
pub struct ReadError {
    /// The file (or stdin) being read.
    pub source_label: String,
    /// What went wrong.
    pub error: BatchError,
}

impl Iterator for ArchiveReader {
    type Item = Result<(String, Entry), ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((label, reader)) = &mut self.current {
                match reader.next() {
                    Some(Ok(e)) => return Some(Ok((label.clone(), e))),
                    Some(Err(error)) => {
                        let source_label = label.clone();
                        self.current = None;
                        self.sources = Vec::new().into_iter();
                        return Some(Err(ReadError {
                            source_label,
                            error,
                        }));
                    }
                    None => self.current = None,
                }
            }
            let source = self.sources.next()?;
            let label = source.label();
            let reader: Box<dyn Read> = match &source {
                Source::File(p) => match File::open(p) {
                    Ok(f) => Box::new(BufReader::new(f)),
                    Err(e) => {
                        return Some(Err(ReadError {
                            source_label: label,
                            error: BatchError::Io(e),
                        }));
                    }
                },
                Source::Stdin => Box::new(BufReader::new(std::io::stdin())),
            };
            self.current = Some((label, BatchReader::new(reader)));
        }
    }
}

/// What the structural checks require beyond the archive's own content.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOptions {
    /// The chain ID every batch must carry (else: the first batch's).
    pub chain_id: Option<u64>,
    /// The hash the first block's `parent_hash` must be (the genesis
    /// hash, or the node's block below the archive's first height).
    pub parent_of_first: Option<B256>,
    /// Also check each body against its header's roots.
    pub deep: bool,
    /// Also check each header's SIP-6 seal (or null block) under the
    /// chain ID.
    pub sip6: bool,
}

/// A block that passed the structural checks.
#[derive(Debug, Clone)]
pub struct CheckedBlock {
    /// Its height.
    pub height: u64,
    /// Its hash (recorded = computed).
    pub hash: B256,
    /// The decoded block.
    pub block: reth_ethereum::Block,
    /// With `sip6`: the sealer (`None` for a null block).
    pub sealer: Option<Address>,
}

/// The outcome of checking one archive entry.
#[derive(Debug, Clone)]
pub enum Checked {
    /// The next block of the chain.
    Block(Box<CheckedBlock>),
    /// An exact repeat (same height and hash) of a block already seen:
    /// overlapping batches. Skipped.
    Duplicate {
        /// Its height.
        height: u64,
    },
}

/// Why an archive entry failed the structural checks.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// A batch for another chain.
    #[error("batch chain id {got} (first block {first_height}), want {want}")]
    ChainId {
        /// Expected chain ID.
        want: u64,
        /// The batch's chain ID.
        got: u64,
        /// The batch's first height.
        first_height: u64,
    },
    /// A block's recorded height disagrees with its position in the batch.
    #[error(
        "block {index} of the batch starting at {first_height} records height {got}, want {want}"
    )]
    Position {
        /// Position in the batch.
        index: u32,
        /// The batch's first height.
        first_height: u64,
        /// The height position implies.
        want: u64,
        /// The recorded height.
        got: u64,
    },
    /// Heights skip.
    #[error("gap: expected height {want}, archive continues at {got}")]
    Gap {
        /// The next height expected.
        want: u64,
        /// The height found.
        got: u64,
    },
    /// Two different blocks recorded for one height.
    #[error("conflict at height {height}: archive holds {first} and later {second}")]
    Conflict {
        /// The height.
        height: u64,
        /// The block seen first.
        first: B256,
        /// The block seen later.
        second: B256,
    },
    /// A repeat of a height too far back to compare.
    #[error("height {height} repeats a block too far back to compare (overlap > window)")]
    OldRepeat {
        /// The height.
        height: u64,
    },
    /// The RLP does not decode as a block.
    #[error("height {height}: raw block does not decode: {error}")]
    Decode {
        /// The height.
        height: u64,
        /// The decoder's error.
        error: String,
    },
    /// The header's number is not the recorded height.
    #[error("height {height}: header number is {number}")]
    Number {
        /// The recorded height.
        height: u64,
        /// The header's number.
        number: u64,
    },
    /// `keccak(rlp(header))` is not the recorded hash.
    #[error("height {height}: header hashes to {computed}, archive records {recorded}")]
    Hash {
        /// The height.
        height: u64,
        /// The recorded hash.
        recorded: B256,
        /// keccak(rlp(header)).
        computed: B256,
    },
    /// The block doesn't build on the previous one.
    #[error("height {height}: parent_hash {got}, previous block is {want}")]
    Parent {
        /// The height.
        height: u64,
        /// The previous block's hash.
        want: B256,
        /// The block's parent_hash.
        got: B256,
    },
    /// A signed transaction for another chain.
    #[error("height {height}: transaction {index} is signed for chain {got}, want {want}")]
    TxChainId {
        /// The height.
        height: u64,
        /// The transaction's index.
        index: usize,
        /// Expected chain ID.
        want: u64,
        /// The transaction's chain ID.
        got: u64,
    },
    /// A body root disagrees with the header (deep check).
    #[error("height {height}: {what} root of the body is {computed}, header says {header}")]
    BodyRoot {
        /// The height.
        height: u64,
        /// Which root.
        what: &'static str,
        /// The header's value.
        header: String,
        /// The body's value.
        computed: String,
    },
    /// The SIP-6 seal check failed.
    #[error("height {height}: {error}")]
    Seal {
        /// The height.
        height: u64,
        /// The seal check's error.
        error: String,
    },
}

/// Running totals of a check.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Blocks accepted (excluding duplicates).
    pub blocks: u64,
    /// Exact repeats skipped.
    pub duplicates: u64,
    /// Transactions in accepted blocks.
    pub txs: u64,
    /// Blocks with withdrawals (mints).
    pub with_withdrawals: u64,
    /// With `sip6`: sealed blocks.
    pub sealed: u64,
    /// With `sip6`: null blocks.
    pub null_blocks: u64,
    /// First accepted block (height, hash).
    pub first: Option<(u64, B256)>,
    /// Last accepted block (height, hash, state root).
    pub last: Option<(u64, B256, B256)>,
}

/// How many recent heights' hashes are remembered to recognise exact
/// repeats in overlapping batches.
pub const REPEAT_WINDOW: usize = 4096;

/// Checks archive entries in order.
#[derive(Debug)]
pub struct Checker {
    opts: CheckOptions,
    chain_id: Option<u64>,
    recent: BTreeMap<u64, B256>,
    /// Running totals.
    pub stats: Stats,
}

impl Checker {
    /// A checker for one pass over an archive.
    pub const fn new(opts: CheckOptions) -> Self {
        Self {
            opts,
            chain_id: opts.chain_id,
            recent: BTreeMap::new(),
            stats: Stats {
                blocks: 0,
                duplicates: 0,
                txs: 0,
                with_withdrawals: 0,
                sealed: 0,
                null_blocks: 0,
                first: None,
                last: None,
            },
        }
    }

    /// The chain ID in force (given, or the first batch's).
    pub const fn chain_id(&self) -> Option<u64> {
        self.chain_id
    }

    /// Checks the next entry.
    pub fn check(&mut self, batch: &BatchHeader, entry: &Entry) -> Result<Checked, VerifyError> {
        let b = &entry.block;
        let height = b.height;
        let chain_id = *self.chain_id.get_or_insert(batch.chain_id);
        if batch.chain_id != chain_id {
            return Err(VerifyError::ChainId {
                want: chain_id,
                got: batch.chain_id,
                first_height: batch.first_height,
            });
        }
        let want_pos = batch.first_height.saturating_add(u64::from(entry.index));
        if height != want_pos {
            return Err(VerifyError::Position {
                index: entry.index,
                first_height: batch.first_height,
                want: want_pos,
                got: height,
            });
        }
        if let Some((last, _, _)) = self.stats.last {
            if height <= last {
                return match self.recent.get(&height) {
                    Some(h) if *h == b.hash => {
                        self.stats.duplicates += 1;
                        Ok(Checked::Duplicate { height })
                    }
                    Some(h) => Err(VerifyError::Conflict {
                        height,
                        first: *h,
                        second: b.hash,
                    }),
                    None => Err(VerifyError::OldRepeat { height }),
                };
            }
            if height != last + 1 {
                return Err(VerifyError::Gap {
                    want: last + 1,
                    got: height,
                });
            }
        }

        let mut raw = b.raw.as_slice();
        let block = reth_ethereum::Block::decode(&mut raw).map_err(|e| VerifyError::Decode {
            height,
            error: e.to_string(),
        })?;
        if !raw.is_empty() {
            return Err(VerifyError::Decode {
                height,
                error: format!("{} trailing byte(s) after the block", raw.len()),
            });
        }
        let header = &block.header;
        if header.number != height {
            return Err(VerifyError::Number {
                height,
                number: header.number,
            });
        }
        let computed = header.hash_slow();
        if computed != b.hash {
            return Err(VerifyError::Hash {
                height,
                recorded: b.hash,
                computed,
            });
        }
        let want_parent = match self.stats.last {
            Some((_, h, _)) => Some(h),
            None => self.opts.parent_of_first,
        };
        if let Some(want) = want_parent
            && header.parent_hash != want
        {
            return Err(VerifyError::Parent {
                height,
                want,
                got: header.parent_hash,
            });
        }
        for (index, tx) in block.body.transactions.iter().enumerate() {
            if let Some(got) = tx.chain_id()
                && got != chain_id
            {
                return Err(VerifyError::TxChainId {
                    height,
                    index,
                    want: chain_id,
                    got,
                });
            }
        }
        if self.opts.deep {
            check_body_roots(height, &block)?;
        }
        let sealer = if self.opts.sip6 {
            let s = engine::seal::check_header(header, Some(chain_id)).map_err(|e| {
                VerifyError::Seal {
                    height,
                    error: e.to_string(),
                }
            })?;
            if s.is_some() {
                self.stats.sealed += 1;
            } else {
                self.stats.null_blocks += 1;
            }
            s
        } else {
            None
        };

        self.stats.blocks += 1;
        self.stats.txs += block.body.transactions.len() as u64;
        if block
            .body
            .withdrawals
            .as_ref()
            .is_some_and(|w| !w.is_empty())
        {
            self.stats.with_withdrawals += 1;
        }
        self.stats.first.get_or_insert((height, b.hash));
        self.stats.last = Some((height, b.hash, header.state_root));
        self.recent.insert(height, b.hash);
        while self.recent.len() > REPEAT_WINDOW {
            self.recent.pop_first();
        }
        Ok(Checked::Block(Box::new(CheckedBlock {
            height,
            hash: b.hash,
            block,
            sealer,
        })))
    }
}

fn check_body_roots(height: u64, block: &reth_ethereum::Block) -> Result<(), VerifyError> {
    let header = &block.header;
    let tx_root = block.body.calculate_tx_root();
    if tx_root != header.transactions_root {
        return Err(VerifyError::BodyRoot {
            height,
            what: "transactions",
            header: header.transactions_root.to_string(),
            computed: tx_root.to_string(),
        });
    }
    let wd_root = block.body.calculate_withdrawals_root();
    if wd_root != header.withdrawals_root {
        return Err(VerifyError::BodyRoot {
            height,
            what: "withdrawals",
            header: format!("{:?}", header.withdrawals_root),
            computed: format!("{wd_root:?}"),
        });
    }
    let ommers = block.body.calculate_ommers_root();
    if ommers != header.ommers_hash {
        return Err(VerifyError::BodyRoot {
            height,
            what: "ommers",
            header: header.ommers_hash.to_string(),
            computed: ommers.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use alloy_consensus::{Header, constants::EMPTY_OMMER_ROOT_HASH};
    use alloy_primitives::Bytes;

    use super::*;
    use crate::batch::ArchivedBlock;

    /// A minimal chain of empty blocks (null-block shaped) over `genesis`.
    pub(crate) fn chain(genesis: B256, n: u64) -> Vec<ArchivedBlock> {
        let mut parent = genesis;
        (1..=n)
            .map(|h| {
                let header = Header {
                    parent_hash: parent,
                    number: h,
                    ommers_hash: EMPTY_OMMER_ROOT_HASH,
                    transactions_root: engine::seal::EMPTY_ROOT,
                    withdrawals_root: Some(engine::seal::EMPTY_ROOT),
                    receipts_root: engine::seal::EMPTY_ROOT,
                    timestamp: 1_000 + h,
                    gas_limit: 30_000_000,
                    base_fee_per_gas: Some(7),
                    blob_gas_used: Some(0),
                    excess_blob_gas: Some(0),
                    parent_beacon_block_root: Some(B256::repeat_byte(h as u8)),
                    mix_hash: engine::seal::pinned_randao(B256::repeat_byte(h as u8)),
                    requests_hash: Some(B256::ZERO),
                    extra_data: Bytes::new(),
                    ..Default::default()
                };
                let block = reth_ethereum::Block {
                    header,
                    body: reth_ethereum::BlockBody {
                        transactions: vec![],
                        ommers: vec![],
                        withdrawals: Some(Default::default()),
                    },
                };
                let hash = block.header.hash_slow();
                parent = hash;
                ArchivedBlock {
                    height: h,
                    hash,
                    raw: alloy_rlp::encode(&block),
                }
            })
            .collect()
    }

    fn run(
        opts: CheckOptions,
        batches: &[(u64, u64, Vec<ArchivedBlock>)],
    ) -> Result<Stats, VerifyError> {
        let mut buf = Vec::new();
        for (cid, first, blocks) in batches {
            batch::write_batch(&mut buf, *cid, *first, blocks).unwrap();
        }
        let mut c = Checker::new(opts);
        for e in BatchReader::new(&buf[..]) {
            let e = e.unwrap();
            c.check(&e.batch, &e)?;
        }
        Ok(c.stats)
    }

    #[test]
    fn a_good_chain_passes_deep_and_sip6() {
        let g = B256::repeat_byte(0xee);
        let c = chain(g, 6);
        let opts = CheckOptions {
            chain_id: Some(9),
            parent_of_first: Some(g),
            deep: true,
            sip6: true,
        };
        let s = run(opts, &[(9, 1, c[..3].to_vec()), (9, 4, c[3..].to_vec())]).unwrap();
        assert_eq!(s.blocks, 6);
        assert_eq!(s.null_blocks, 6);
        assert_eq!(s.last.unwrap().0, 6);
    }

    #[test]
    fn overlap_is_deduplicated_and_conflict_refused() {
        let g = B256::repeat_byte(0xee);
        let c = chain(g, 5);
        let opts = CheckOptions::default();
        let s = run(opts, &[(9, 1, c[..4].to_vec()), (9, 3, c[2..].to_vec())]).unwrap();
        assert_eq!((s.blocks, s.duplicates), (5, 2));
        let other = chain(B256::repeat_byte(1), 5);
        let err = run(
            opts,
            &[(9, 1, c[..4].to_vec()), (9, 3, other[2..].to_vec())],
        )
        .unwrap_err();
        assert!(
            matches!(err, VerifyError::Conflict { height: 3, .. }),
            "{err}"
        );
    }

    #[test]
    fn gap_chain_id_parent_hash_and_position_are_refused() {
        let g = B256::repeat_byte(0xee);
        let c = chain(g, 6);
        let opts = CheckOptions::default();
        let err = run(opts, &[(9, 1, c[..2].to_vec()), (9, 4, c[3..].to_vec())]).unwrap_err();
        assert!(matches!(err, VerifyError::Gap { want: 3, got: 4 }), "{err}");

        let err = run(opts, &[(9, 1, c[..2].to_vec()), (8, 3, c[2..].to_vec())]).unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::ChainId {
                    want: 9,
                    got: 8,
                    ..
                }
            ),
            "{err}"
        );

        let with_genesis = CheckOptions {
            parent_of_first: Some(B256::repeat_byte(1)),
            ..opts
        };
        let err = run(with_genesis, &[(9, 1, c.clone())]).unwrap_err();
        assert!(
            matches!(err, VerifyError::Parent { height: 1, .. }),
            "{err}"
        );

        let mut wrong_hash = c.clone();
        wrong_hash[2].hash = B256::repeat_byte(7);
        let err = run(opts, &[(9, 1, wrong_hash)]).unwrap_err();
        assert!(matches!(err, VerifyError::Hash { height: 3, .. }), "{err}");

        // Batch says it starts at 2 but holds block 1.
        let err = run(opts, &[(9, 2, c[..1].to_vec())]).unwrap_err();
        assert!(matches!(err, VerifyError::Position { .. }), "{err}");

        // A chain with a missing middle block: heights contiguous in the
        // frames would be a lie, so the frame skips and the gap shows.
        let mut skip = c.clone();
        skip.remove(3);
        let err = run(opts, &[(9, 1, skip)]).unwrap_err();
        assert!(matches!(err, VerifyError::Position { .. }), "{err}");
    }

    #[test]
    fn a_flipped_body_byte_fails_deep_but_not_shallow() {
        let g = B256::repeat_byte(0xee);
        let mut c = chain(g, 3);
        // Re-encode block 2 with a different withdrawals list: header
        // unchanged, so the hash still matches; only the body root differs.
        let mut raw = c[1].raw.as_slice();
        let mut b = reth_ethereum::Block::decode(&mut raw).unwrap();
        b.body.withdrawals = Some(
            vec![alloy_eips::eip4895::Withdrawal {
                index: 0,
                validator_index: 0,
                address: Address::repeat_byte(1),
                amount: 1,
            }]
            .into(),
        );
        c[1].raw = alloy_rlp::encode(&b);
        assert!(run(CheckOptions::default(), &[(9, 1, c.clone())]).is_ok());
        let deep = CheckOptions {
            deep: true,
            ..Default::default()
        };
        let err = run(deep, &[(9, 1, c)]).unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::BodyRoot {
                    height: 2,
                    what: "withdrawals",
                    ..
                }
            ),
            "{err}"
        );
    }
}
