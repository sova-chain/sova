//! Rebuild a fresh node from an archive.
//!
//! **The archive delivers, the node decides.** Each block goes to the
//! node's own authrpc as `engine_newPayloadV4` — the call, wire shape and
//! JWT client gossip v1's relay uses to push a peer's block
//! (`engine::relay`) — and the node validates it through its full import
//! path: SIP-6 seal, the C5 settlement check against its own zebrad, the
//! SIP-4 anchor, execution (SIP-4/SIP-7 precompile answers from its own
//! Zcash index) and the state root. The tool sends no forkchoice update:
//! the node's own arbiter adopts each accepted block as head, exactly as
//! for a relayed or `sova/1` block, and the tool waits for that before
//! sending the next one (a child sent before its parent is canonical is
//! not attached until the arbiter's next retry). Nothing here writes the
//! node's database or skips a check.
//!
//! A block whose Zcash epoch the node's follower has not scanned yet is
//! *held* (SIP-4 "hold, don't accept": `INVALID` with the `sova-hold`
//! marker, never cached as invalid). A rebuild outruns the node's scan by
//! construction, so a hold is waited out and the block resent; only a
//! non-hold `INVALID` is a rejection, and the tool stops on the first one.
//!
//! **Resuming** (the node already has some of the archive's blocks) does
//! not ask the node about every present block. The archive is hash-linked:
//! the checker recomputes every block's hash from its header and checks
//! each `parent_hash` against the block before (and the first one against
//! the node's block below it). A block hash commits to its parent's hash,
//! so if the node's block at height H has the archive's hash, the node's
//! chain below H *is* the archive's chain below H, block for block. The
//! tool therefore looks up only the first present block (fails fast on
//! another chain), one every [`PREFIX_SAMPLE`] blocks (fails fast on a fork
//! part-way), and the last present one (the node's head, or the archive's
//! last block if it ends below it), which proves the whole overlap. A
//! 60k-block overlap costs a handful of RPCs instead of 60k.

use std::time::{Duration, Instant};

use alloy_primitives::B256;
use engine::SovaEngineTypes;
use reth_ethereum::{node::api::PayloadTypes, primitives::SealedBlock};
use serde_json::Value;

use crate::{
    archive::{ArchiveReader, CheckOptions, Checked, Checker, Source, Stats, VerifyError},
    rpc::{BlockRef, Endpoint, RpcError},
};

/// Text in every consensus hold's validation error
/// (`engine::consensus::HOLD_MARKER`; the relay and `sova/1` match on it
/// the same way).
pub const HOLD_MARKER: &str = "sova-hold";

/// While skipping blocks the node already has, look one up every this many
/// heights (plus the first and the last; see the module docs).
pub const PREFIX_SAMPLE: u64 = 10_000;

/// A rebuild's settings.
#[derive(Debug, Clone)]
pub struct RebuildOptions {
    /// The node's authrpc.
    pub node: Endpoint,
    /// The chain ID the archive must carry (default: the node's).
    pub chain_id: Option<u64>,
    /// Check SIP-6 seals in the archive (the node checks them regardless).
    pub sip6: bool,
    /// The head the rebuild must end at.
    pub expect: Option<Expect>,
    /// An RPC (e.g. a public one) whose block at the rebuilt height must
    /// match.
    pub expect_rpc: Option<Endpoint>,
    /// Longest wait on one held block.
    pub hold_timeout: Duration,
    /// Longest wait for an accepted block to become head.
    pub adopt_timeout: Duration,
    /// Longest a block may keep answering `SYNCING`.
    pub syncing_timeout: Duration,
    /// Longest the node may keep failing calls (transport/RPC errors).
    pub error_timeout: Duration,
    /// HTTP timeout of one `engine_newPayloadV4`.
    pub payload_timeout: Duration,
    /// How long to wait for the node's authrpc to come up.
    pub startup_timeout: Duration,
}

/// An expected head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    /// Height.
    pub number: u64,
    /// Block hash.
    pub hash: B256,
    /// State root, if known.
    pub state_root: Option<B256>,
}

impl std::str::FromStr for Expect {
    type Err = eyre::Report;

    /// `NUMBER:HASH[:STATEROOT]`.
    fn from_str(s: &str) -> eyre::Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        let bad = || eyre::eyre!("--expect wants NUMBER:HASH[:STATEROOT], got {s:?}");
        match parts.as_slice() {
            [n, h] | [n, h, _] => Ok(Self {
                number: n.parse().map_err(|_| bad())?,
                hash: h.parse().map_err(|_| bad())?,
                state_root: match parts.get(2) {
                    Some(r) => Some(r.parse().map_err(|_| bad())?),
                    None => None,
                },
            }),
            _ => Err(bad()),
        }
    }
}

/// Why a rebuild stopped.
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// The archive itself is broken (nothing about the node).
    #[error("archive: {0}")]
    Archive(String),
    /// The node rejected a block.
    #[error("REJECTED: the node rejected block #{height} {hash}: {reason}")]
    Rejected {
        /// Height.
        height: u64,
        /// Hash.
        hash: B256,
        /// The node's validation error (status + reason).
        reason: String,
    },
    /// A block stayed held, unadopted or syncing past its timeout, or the
    /// node kept failing.
    #[error("STUCK at block #{height} {hash}: {what}")]
    Stuck {
        /// Height.
        height: u64,
        /// Hash.
        hash: B256,
        /// What the node kept saying.
        what: String,
    },
    /// The node can't take this archive (another chain, another history,
    /// nothing to build on).
    #[error("node: {0}")]
    Node(String),
    /// The rebuilt head is not what it should be.
    #[error("HEAD MISMATCH: {0}")]
    HeadMismatch(String),
}

impl RebuildError {
    /// Process exit code: 2 rejected, 3 archive, 4 head mismatch, 5 stuck,
    /// 6 node unusable.
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Rejected { .. } => 2,
            Self::Archive(_) => 3,
            Self::HeadMismatch(_) => 4,
            Self::Stuck { .. } => 5,
            Self::Node(_) => 6,
        }
    }
}

/// What a rebuild did.
#[derive(Debug, Clone)]
pub struct RebuildReport {
    /// The node's head before.
    pub start: BlockRef,
    /// The node's head after.
    pub head: BlockRef,
    /// Blocks pushed and adopted.
    pub imported: u64,
    /// Archive blocks the node already had (same hash).
    pub already_had: u64,
    /// Times a block was held.
    pub holds: u64,
    /// Time spent waiting on holds.
    pub hold_secs: f64,
    /// Seconds spent pushing (first push to last adoption).
    pub secs: f64,
    /// The archive's check totals.
    pub archive: Stats,
    /// Lines describing each head comparison that passed.
    pub matched: Vec<String>,
}

/// What the node did with one `engine_newPayloadV4`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// `VALID` or `ACCEPTED`.
    Accepted(String),
    /// `SYNCING`: parent unknown (or the node is backfilling).
    Syncing,
    /// A consensus hold (`INVALID` with the hold marker).
    Held(String),
    /// Any other answer: a rejection.
    Rejected(String),
}

/// Classifies a `PayloadStatus` result.
pub fn classify(result: &Value) -> Status {
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let why = result
        .get("validationError")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = if why.is_empty() {
        status.to_owned()
    } else {
        format!("{status}: {why}")
    };
    match status {
        "VALID" | "ACCEPTED" => Status::Accepted(status.to_owned()),
        "SYNCING" => Status::Syncing,
        _ if why.contains(HOLD_MARKER) => Status::Held(text),
        _ => Status::Rejected(text),
    }
}

/// Rebuilds the node at `opts.node` from `sources`.
pub fn rebuild(sources: Vec<Source>, opts: &RebuildOptions) -> Result<RebuildReport, RebuildError> {
    let node = &opts.node;
    let node_chain = wait_for_node(node, opts.startup_timeout)?;
    if let Some(want) = opts.chain_id
        && want != node_chain
    {
        return Err(RebuildError::Node(format!(
            "node's chain id is {node_chain}, --chain-id says {want}"
        )));
    }
    let start = node
        .head_at("latest")
        .map_err(node_err)?
        .ok_or_else(|| RebuildError::Node("node has no head block".into()))?;
    eprintln!("rebuild: node chain id {node_chain}, head {start}");

    let mut reader = ArchiveReader::new(sources).peekable();
    // The first block's parent must be the node's block below it.
    let first_height = match reader.peek() {
        Some(Ok((_, e))) => e.block.height,
        Some(Err(_)) => 0,
        None => return Err(RebuildError::Archive("archive holds no blocks".into())),
    };
    let parent_of_first = if first_height == 0 {
        None
    } else if first_height - 1 > start.number {
        return Err(RebuildError::Node(format!(
            "archive starts at #{first_height} but the node's head is #{}: nothing to build on",
            start.number
        )));
    } else {
        node.block_at(first_height - 1)
            .map_err(node_err)?
            .map(|b| b.hash)
    };
    let mut checker = Checker::new(CheckOptions {
        chain_id: Some(node_chain),
        parent_of_first,
        deep: false,
        sip6: opts.sip6,
    });

    let mut imported = 0u64;
    let mut already_had = 0u64;
    // The last archive block at or below the node's head, and whether the
    // node was asked about it.
    let mut last_present: Option<(u64, B256, bool)> = None;
    let mut prefix_lookups = 0u64;
    let t_prefix = Instant::now();
    let mut holds = 0u64;
    let mut hold_secs = 0f64;
    let mut t_first: Option<Instant> = None;
    let mut last_report = Instant::now();
    let mut since_report = 0u64;
    for item in reader {
        let (label, entry) = item.map_err(|e| RebuildError::Archive(e.to_string()))?;
        let checked = checker
            .check(&entry.batch, &entry)
            .map_err(|e: VerifyError| RebuildError::Archive(format!("{label}: {e}")))?;
        let Checked::Block(b) = checked else { continue };
        if b.height <= start.number {
            // Present on the node (by height): see the module docs for why
            // the first, a sample and the last lookup prove the overlap.
            let ask = already_had == 0 || b.height == start.number || b.height % PREFIX_SAMPLE == 0;
            if ask {
                check_present(node, b.height, b.hash)?;
                prefix_lookups += 1;
            }
            already_had += 1;
            last_present = Some((b.height, b.hash, ask));
            continue;
        }
        // The first block above the node's head: the overlap ended at
        // start.number, which was looked up.
        if last_present.take().is_some() {
            report_prefix(already_had, prefix_lookups, t_prefix);
        }
        let t0 = *t_first.get_or_insert_with(Instant::now);
        let pushed = push_block(node, opts, b.height, b.hash, b.block)?;
        holds += pushed.holds;
        hold_secs += pushed.hold_secs;
        imported += 1;
        since_report += 1;
        if last_report.elapsed() >= Duration::from_secs(5) {
            let el = last_report.elapsed().as_secs_f64();
            eprintln!(
                "rebuild: #{} {} — {imported} imported, {:.1} blocks/s (last {el:.0} s), {:.1} blocks/s overall; {holds} hold(s), {hold_secs:.1} s held",
                b.height,
                short(b.hash),
                since_report as f64 / el,
                imported as f64 / t0.elapsed().as_secs_f64().max(1e-9),
            );
            last_report = Instant::now();
            since_report = 0;
        }
    }
    // The archive ended at or below the node's head: its last block must
    // be the node's block at that height.
    if let Some((h, hash, asked)) = last_present {
        if !asked {
            check_present(node, h, hash)?;
            prefix_lookups += 1;
        }
        report_prefix(already_had, prefix_lookups, t_prefix);
    }
    let secs = t_first.map_or(0.0, |t| t.elapsed().as_secs_f64());
    let archive = checker.stats;

    // The node's head now, against the archive and any expectation.
    let head = node
        .head_at("latest")
        .map_err(node_err)?
        .ok_or_else(|| RebuildError::Node("node has no head block".into()))?;
    let mut matched = Vec::new();
    let mut mismatches = Vec::new();
    if let Some((n, h, root)) = archive.last {
        let want = BlockRef {
            number: n,
            hash: h,
            state_root: root,
        };
        if head == want {
            matched.push(format!("archive's last block {want}"));
        } else {
            mismatches.push(format!("node head {head} != archive's last block {want}"));
        }
    }
    if let Some(e) = &opts.expect {
        let ok = head.number == e.number
            && head.hash == e.hash
            && e.state_root.is_none_or(|r| r == head.state_root);
        if ok {
            matched.push(format!("--expect #{} {}", e.number, e.hash));
        } else {
            mismatches.push(format!(
                "node head {head} != --expect #{} {} {:?}",
                e.number, e.hash, e.state_root
            ));
        }
    }
    if let Some(rpc) = &opts.expect_rpc {
        match rpc.block_at(head.number) {
            Ok(Some(r)) if r == head => matched.push(format!("--expect-rpc block {r}")),
            Ok(Some(r)) => mismatches.push(format!("node head {head} != --expect-rpc's {r}")),
            Ok(None) => mismatches.push(format!(
                "--expect-rpc has no block #{} (node head {head})",
                head.number
            )),
            Err(e) => mismatches.push(format!("--expect-rpc unreadable: {e}")),
        }
    }
    if !mismatches.is_empty() {
        return Err(RebuildError::HeadMismatch(mismatches.join("; ")));
    }
    Ok(RebuildReport {
        start,
        head,
        imported,
        already_had,
        holds,
        hold_secs,
        secs,
        archive,
        matched,
    })
}

struct Pushed {
    holds: u64,
    hold_secs: f64,
}

/// Sends one block until the node accepts it (waiting out holds), then
/// waits for the node's arbiter to make it head.
fn push_block(
    node: &Endpoint,
    opts: &RebuildOptions,
    height: u64,
    hash: B256,
    block: reth_ethereum::Block,
) -> Result<Pushed, RebuildError> {
    let data = SovaEngineTypes::block_to_payload(SealedBlock::new_unchecked(block, hash), None);
    let params =
        engine::relay::new_payload_v4_params(data).map_err(|e| RebuildError::Rejected {
            height,
            hash,
            reason: format!("not sendable as engine_newPayloadV4: {e}"),
        })?;
    let stuck = |what: String| RebuildError::Stuck { height, hash, what };

    let mut holds = 0u64;
    let mut hold_since: Option<Instant> = None;
    let mut syncing_since: Option<Instant> = None;
    let mut error_since: Option<Instant> = None;
    let mut backoff = Duration::from_millis(100);
    let mut last_note = Instant::now();
    loop {
        let outcome = node.call("engine_newPayloadV4", params.clone(), opts.payload_timeout);
        match outcome.as_ref().map(classify) {
            Ok(Status::Accepted(_)) => break,
            Ok(Status::Rejected(reason)) => {
                return Err(RebuildError::Rejected {
                    height,
                    hash,
                    reason,
                });
            }
            Ok(Status::Held(reason)) => {
                error_since = None;
                syncing_since = None;
                holds += 1;
                let since = *hold_since.get_or_insert_with(Instant::now);
                if since.elapsed() > opts.hold_timeout {
                    return Err(stuck(format!(
                        "held for {:.0} s (limit {:.0} s); last: {reason}",
                        since.elapsed().as_secs_f64(),
                        opts.hold_timeout.as_secs_f64()
                    )));
                }
                if holds == 1 || last_note.elapsed() >= Duration::from_secs(10) {
                    eprintln!(
                        "rebuild: #{height} held by the node ({reason}); waiting for its Zcash scan"
                    );
                    last_note = Instant::now();
                }
            }
            Ok(Status::Syncing) => {
                error_since = None;
                let since = *syncing_since.get_or_insert_with(Instant::now);
                if since.elapsed() > opts.syncing_timeout {
                    return Err(stuck(format!(
                        "node kept answering SYNCING for {:.0} s (parent unknown?)",
                        since.elapsed().as_secs_f64()
                    )));
                }
            }
            Err(e) => {
                let since = *error_since.get_or_insert_with(Instant::now);
                if since.elapsed() > opts.error_timeout {
                    return Err(stuck(format!(
                        "engine_newPayloadV4 kept failing for {:.0} s; last: {e}",
                        since.elapsed().as_secs_f64()
                    )));
                }
                if last_note.elapsed() >= Duration::from_secs(10) {
                    eprintln!("rebuild: #{height}: engine_newPayloadV4 failed ({e}); retrying");
                    last_note = Instant::now();
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
    let hold_secs = hold_since.map_or(0.0, |t| t.elapsed().as_secs_f64());

    // Accepted: the node's arbiter moves its head; wait for it.
    let t = Instant::now();
    let mut poll = Duration::from_millis(2);
    loop {
        match node.block_at(height) {
            Ok(Some(b)) if b.hash == hash => break,
            Ok(_) | Err(_) if t.elapsed() <= opts.adopt_timeout => {}
            Ok(other) => {
                let head = node.head_at("latest").ok().flatten();
                return Err(stuck(format!(
                    "accepted but not adopted as head within {:.0} s (node's block there: {:?}, head: {})",
                    opts.adopt_timeout.as_secs_f64(),
                    other.map(|b| b.hash),
                    head.map_or_else(|| "?".to_owned(), |h| h.to_string())
                )));
            }
            Err(e) => {
                return Err(stuck(format!(
                    "accepted, then the node stopped answering: {e}"
                )));
            }
        }
        std::thread::sleep(poll);
        poll = (poll * 2).min(Duration::from_millis(50));
    }
    Ok(Pushed { holds, hold_secs })
}

/// The node's block at `height` must be the archive's `hash`.
fn check_present(node: &Endpoint, height: u64, hash: B256) -> Result<(), RebuildError> {
    match node.block_at(height).map_err(node_err)? {
        Some(have) if have.hash == hash => Ok(()),
        have => Err(RebuildError::Node(format!(
            "node follows another history: its block #{height} is {:?}, the archive's is {hash}; \
             rebuild into an empty datadir",
            have.map(|h| h.hash),
        ))),
    }
}

fn report_prefix(present: u64, lookups: u64, since: Instant) {
    eprintln!(
        "rebuild: node already has {present} of the archive's blocks: {lookups} looked up \
         (first, every {PREFIX_SAMPLE}th, last; the archive's hash links cover the rest), \
         all match; {:.2} s",
        since.elapsed().as_secs_f64()
    );
}

/// Waits for the node's authrpc; returns its chain id.
fn wait_for_node(node: &Endpoint, timeout: Duration) -> Result<u64, RebuildError> {
    let t = Instant::now();
    loop {
        match node.chain_id() {
            Ok(id) => return Ok(id),
            Err(e) if t.elapsed() > timeout => {
                return Err(RebuildError::Node(format!(
                    "authrpc not answering after {:.0} s: {e}",
                    timeout.as_secs_f64()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

fn node_err(e: RpcError) -> RebuildError {
    RebuildError::Node(e.to_string())
}

/// `0x1234abcd…`.
pub fn short(h: B256) -> String {
    let s = h.to_string();
    format!("{}…", &s[..10.min(s.len())])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::rpc::test_server::{FakeNode, Mode};

    #[test]
    fn statuses_classify() {
        assert_eq!(
            classify(&json!({"status": "VALID"})),
            Status::Accepted("VALID".into())
        );
        assert_eq!(
            classify(&json!({"status": "ACCEPTED"})),
            Status::Accepted("ACCEPTED".into())
        );
        assert_eq!(classify(&json!({"status": "SYNCING"})), Status::Syncing);
        assert!(matches!(
            classify(&json!({"status": "INVALID", "validationError": "sova-hold: zcash epoch not scanned yet at height 5 (scanned through 3)"})),
            Status::Held(s) if s.contains("scanned through 3")
        ));
        assert!(matches!(
            classify(&json!({"status": "INVALID", "validationError": "block hash mismatch"})),
            Status::Rejected(s) if s == "INVALID: block hash mismatch"
        ));
        assert!(matches!(classify(&json!(null)), Status::Rejected(_)));
    }

    fn resume_opts(url: String) -> RebuildOptions {
        RebuildOptions {
            node: Endpoint::http(url),
            chain_id: Some(9),
            sip6: false,
            expect: None,
            expect_rpc: None,
            hold_timeout: Duration::from_secs(5),
            adopt_timeout: Duration::from_secs(5),
            syncing_timeout: Duration::from_secs(5),
            error_timeout: Duration::from_secs(5),
            payload_timeout: Duration::from_secs(5),
            startup_timeout: Duration::from_secs(5),
        }
    }

    /// An archive of `n` empty blocks over `g` in one batch file, and the
    /// node blocks (genesis + all of them) a fake node that has them serves.
    fn archive(dir: &tempfile::TempDir, g: B256, n: u64) -> (Vec<Source>, Vec<(u64, B256, B256)>) {
        let c = crate::archive::tests::chain(g, n);
        let path = dir.path().join("1.sovada");
        let mut f = std::fs::File::create(&path).unwrap();
        crate::batch::write_batch(&mut f, 9, 1, &c).unwrap();
        // The test chain's headers keep alloy's default state root.
        let root = engine::seal::EMPTY_ROOT;
        let mut blocks = vec![(0, g, root)];
        blocks.extend(c.iter().map(|b| (b.height, b.hash, root)));
        (vec![Source::File(path)], blocks)
    }

    /// Resuming against a node that has every archive block asks it about
    /// the first and the last one (plus its head and the genesis parent),
    /// not each block, over one connection.
    #[test]
    fn a_full_resume_looks_up_first_and_last_only() {
        let dir = tempfile::tempdir().unwrap();
        let g = B256::repeat_byte(0xee);
        let (sources, blocks) = archive(&dir, g, 25);
        let srv = FakeNode::with_blocks(9, 25, Mode::KeepAlive, &blocks);
        let r = rebuild(sources, &resume_opts(srv.url())).unwrap();
        assert_eq!((r.imported, r.already_had, r.head.number), (0, 25, 25));
        // head ("latest"), #0 (the first block's parent), #1, #25, head again.
        assert_eq!(srv.lookups(), 5);
        assert_eq!(srv.connections(), 1);
    }

    /// The last present block decides: a node whose head differs from the
    /// archive's block at that height follows another history, even if its
    /// first blocks match.
    #[test]
    fn a_resume_refuses_a_node_on_another_history() {
        let dir = tempfile::tempdir().unwrap();
        let g = B256::repeat_byte(0xee);
        let (sources, mut blocks) = archive(&dir, g, 25);
        blocks[25].1 = B256::repeat_byte(0x42);
        let srv = FakeNode::with_blocks(9, 25, Mode::KeepAlive, &blocks);
        let err = rebuild(sources, &resume_opts(srv.url())).unwrap_err();
        assert!(
            matches!(&err, RebuildError::Node(m) if m.contains("another history") && m.contains("#25")),
            "{err}"
        );

        // And on the first block: refused before reading further.
        let (sources, mut blocks) = archive(&dir, g, 25);
        blocks[1].1 = B256::repeat_byte(0x42);
        let srv = FakeNode::with_blocks(9, 25, Mode::KeepAlive, &blocks);
        let err = rebuild(sources, &resume_opts(srv.url())).unwrap_err();
        assert!(
            matches!(&err, RebuildError::Node(m) if m.contains("#1 ")),
            "{err}"
        );
        assert_eq!(srv.lookups(), 3, "head, #0, #1");
    }

    /// Every PREFIX_SAMPLE-th present block is looked up too.
    #[test]
    fn a_long_resume_samples() {
        let dir = tempfile::tempdir().unwrap();
        let g = B256::repeat_byte(0xee);
        let n = PREFIX_SAMPLE * 2 + 5;
        let (sources, blocks) = archive(&dir, g, n);
        let srv = FakeNode::with_blocks(9, n, Mode::KeepAlive, &blocks);
        let r = rebuild(sources, &resume_opts(srv.url())).unwrap();
        assert_eq!((r.imported, r.already_had), (0, n));
        // head, #0, #1, #10000, #20000, #n, head again.
        assert_eq!(srv.lookups(), 7);
        assert_eq!(srv.connections(), 1);
    }

    #[test]
    fn expect_parses() {
        let h = format!("0x{}", "ab".repeat(32));
        let e: Expect = format!("12:{h}").parse().unwrap();
        assert_eq!((e.number, e.state_root), (12, None));
        let e: Expect = format!("12:{h}:{h}").parse().unwrap();
        assert!(e.state_root.is_some());
        assert!("12".parse::<Expect>().is_err());
        assert!("x:0x00".parse::<Expect>().is_err());
    }
}
