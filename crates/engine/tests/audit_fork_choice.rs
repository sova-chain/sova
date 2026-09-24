//! Audit demonstrations (docs/audits/2026-09-23-reorg-and-fork-choice.md).
//!
//! These tests do not test a feature; they pin down, against the real
//! code, the properties the audit relies on:
//!
//! 1. Validity (C5 + the SIP-4 anchor) is a function of `(height,
//!    anchor, withdrawals)` only. Two blocks at one height with different
//!    parents and different transactions are both valid.
//! 2. Preference (`candidates`) is a function of `(rank, hash)` only. It
//!    never looks at the parent, so a block whose ancestry diverges from
//!    the canonical chain competes at the tip exactly like a sibling.
//! 3. The arbiter adopts any preferred candidate at `height >= head`, and
//!    the adoption is a plain `forkchoiceUpdated` to the candidate's hash;
//!    reth then reorganizes to that block's ancestry, however deep.
//! 4. After a restart the tracker is empty, so the first candidate seen
//!    at the tip height is adopted whatever its rank.
//! 5. One `Announce` with an absurd height wedges late-join catch-up:
//!    `request_sync` keeps only the highest target and the driver waits
//!    for a scan that never arrives.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::B256;
use alloy_rpc_types::Withdrawal;
use consensus::epoch::EpochBurn;
use consensus::follower::EpochData;
use consensus::sealer::Candidate;
use consensus::sip1::Burn;
use engine::candidates::{BestCandidate, CandidateTracker, Observation, SyncTarget, run_arbiter};
use engine::driver::{DRAFT_EPOCH_REWARD_GWEI, epoch_attribute, identify_sealer, ranked_miners};
use engine::expectations::{AnchorVerdict, ExpectedSettlements, HeightRecord, RankedVerdict};
use engine::settlements_to_withdrawals;
use reth_ethereum::primitives::{Header, SealedBlock};
use reth_ethereum::{Block, BlockBody};

const FLAT: consensus::schedule::Schedule = consensus::schedule::Schedule::Flat {
    reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
};
const HEIGHT: u64 = 100;
const ANCHOR: [u8; 32] = [0x5A; 32];

fn burn(txid: u8, addr: u8, zat: u64) -> EpochBurn {
    EpochBurn {
        txid: [txid; 32],
        burn: Burn {
            evm_address: [addr; 20],
            signal_bits: 0,
            value_zat: zat,
        },
        reference: None,
    }
}

/// An epoch with two burners: 0xAA (rank 0) and 0xBB (rank 1).
fn epoch() -> EpochData {
    EpochData {
        height: 1_000,
        hash: ANCHOR,
        burns: vec![burn(1, 0xAA, 600_000), burn(2, 0xBB, 400_000)],
        time: 0,
        txs: Vec::new(),
        pools: None,
    }
}

fn withdrawals_for_rank(rank: usize) -> Vec<Withdrawal> {
    let e = epoch();
    let ranked = ranked_miners(&e.burns);
    let attr = epoch_attribute(
        &e,
        &ranked,
        ranked[rank].evm_address,
        DRAFT_EPOCH_REWARD_GWEI,
    )
    .unwrap_or_else(|err| panic!("{err}"));
    settlements_to_withdrawals(&attr).unwrap_or_else(|err| panic!("{err}"))
}

fn expectations() -> ExpectedSettlements {
    let e = ExpectedSettlements::default();
    let ep = epoch();
    let ranked = ranked_miners(&ep.burns);
    e.insert(
        HEIGHT,
        HeightRecord {
            withdrawals: withdrawals_for_rank(0),
            epoch: ep,
            ranked,
        },
    );
    e
}

/// A block at `HEIGHT` on `parent`, carrying `withdrawals`, with a
/// distinct transactions root standing in for a different transaction set.
fn block(parent: B256, tx_root: B256, withdrawals: &[Withdrawal]) -> SealedBlock<Block> {
    let header = Header {
        number: HEIGHT,
        parent_hash: parent,
        transactions_root: tx_root,
        parent_beacon_block_root: Some(B256::from(ANCHOR)),
        ..Default::default()
    };
    let body = BlockBody {
        withdrawals: Some(withdrawals.to_vec().into()),
        ..Default::default()
    };
    SealedBlock::seal_slow(Block::new(header, body))
}

/// What `SovaConsensus::check_settlements` and
/// `SovaEngineValidator::convert_payload_to_block` compute for a block:
/// the anchor verdict, then the ranked verdict. Both call exactly these two
/// functions (crates/engine/src/consensus.rs:140-174, validator.rs:86-123).
fn verdict(e: &ExpectedSettlements, b: &SealedBlock<Block>) -> (AnchorVerdict, RankedVerdict) {
    let anchor = e.check_anchor(b.number, b.header().parent_beacon_block_root);
    let ranked = e.check_ranked(
        b.number,
        b.body().withdrawals.as_deref().map(|w| w.as_slice()),
        FLAT,
    );
    (anchor, ranked)
}

/// Finding 1: two blocks at the same height, on different parents, with
/// different transactions, are both valid as long as they carry a rank's
/// withdrawals and the epoch's anchor. Nothing in validity mentions the
/// parent or the transactions.
#[test]
fn validity_ignores_parent_and_transactions() {
    let e = expectations();
    let honest = block(
        B256::repeat_byte(0x01),
        B256::repeat_byte(0x11),
        &withdrawals_for_rank(0),
    );
    let alternate = block(
        B256::repeat_byte(0x02),  // a different ancestry
        B256::repeat_byte(0x22),  // different transactions
        &withdrawals_for_rank(0), // the same mint, copied
    );
    assert_ne!(honest.hash(), alternate.hash());
    assert_ne!(honest.parent_hash, alternate.parent_hash);
    assert_eq!(
        verdict(&e, &honest),
        (AnchorVerdict::Match, RankedVerdict::Valid { rank: 0 })
    );
    assert_eq!(
        verdict(&e, &alternate),
        (AnchorVerdict::Match, RankedVerdict::Valid { rank: 0 })
    );
    // And a block at any rank is equally valid, so an alternate history
    // needs only *some* burner's derivation at each height.
    let at_rank1 = block(
        B256::repeat_byte(0x03),
        B256::repeat_byte(0x33),
        &withdrawals_for_rank(1),
    );
    assert_eq!(
        verdict(&e, &at_rank1),
        (AnchorVerdict::Match, RankedVerdict::Valid { rank: 1 })
    );
    let ep = epoch();
    let ranked = ranked_miners(&ep.burns);
    assert_eq!(
        identify_sealer(
            &ep,
            &ranked,
            DRAFT_EPOCH_REWARD_GWEI,
            &withdrawals_for_rank(1)
        ),
        Some(1)
    );
}

/// Finding 1, fixed by the sibling rule: preference is `(rank, hash)` but
/// only among candidates that extend our chain. A block from a divergent
/// history is not a candidate, whatever its rank or hash; a sibling of our
/// tip still wins by rank (the late win).
#[test]
fn preference_requires_our_parent() {
    let canonical_parent = B256::repeat_byte(0x01);
    let t = CandidateTracker::with_canonical_reader(Box::new(move |h| {
        (h == HEIGHT - 1).then_some(canonical_parent.0)
    }));
    let honest = block(
        canonical_parent,
        B256::repeat_byte(0x11),
        &withdrawals_for_rank(1),
    );
    let alternate = block(
        B256::repeat_byte(0x02),
        B256::repeat_byte(0x22),
        &withdrawals_for_rank(0),
    );
    assert_eq!(
        t.observe(
            HEIGHT,
            Candidate {
                sealer_rank: 1,
                block_hash: honest.hash().0
            },
            honest.parent_hash.0
        ),
        Observation::NewBest
    );
    assert_eq!(
        t.observe(
            HEIGHT,
            Candidate {
                sealer_rank: 0,
                block_hash: alternate.hash().0
            },
            alternate.parent_hash.0
        ),
        Observation::NotBetter,
        "a better rank on a foreign parent is not a candidate"
    );
    assert_eq!(t.best(HEIGHT).map(|c| c.block_hash), Some(honest.hash().0));
    // A rank-0 sibling on our parent still displaces the rank-1 tip.
    let sibling = block(
        canonical_parent,
        B256::repeat_byte(0x33),
        &withdrawals_for_rank(0),
    );
    assert_eq!(
        t.observe(
            HEIGHT,
            Candidate {
                sealer_rank: 0,
                block_hash: sibling.hash().0
            },
            sibling.parent_hash.0
        ),
        Observation::NewBest
    );
}

/// Finding 1, the adoption: the arbiter forwards any best candidate at
/// `height >= head` to `forkchoiceUpdated` with only the hash; reth
/// reorganizes to whatever ancestry the hash has. Since the sibling rule
/// the arbiter only receives candidates the tracker judged attached
/// (`preference_requires_our_parent`), so this test documents the
/// arbiter's trust in its input, not an open hole.
#[tokio::test]
async fn arbiter_adopts_a_same_height_candidate_whatever_its_ancestry() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let alternate_tip = [0xA7; 32];
    tx.send(BestCandidate {
        sova_height: HEIGHT,
        block_hash: alternate_tip,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    drop(tx);
    let adopted = Arc::new(Mutex::new(Vec::new()));
    let sink = adopted.clone();
    run_arbiter(
        rx,
        || HEIGHT, // our head is at the same height: the honest block
        || Some(HEIGHT),
        move |best: BestCandidate| {
            let sink = sink.clone();
            async move {
                sink.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(best.block_hash);
                Ok(())
            }
        },
    )
    .await;
    assert_eq!(
        adopted.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        &[alternate_tip],
        "adopted with no knowledge of the candidate's parent"
    );
}

/// Finding 2: after a restart the tracker is empty, so the first
/// candidate observed at the tip height is `NewBest` even at a worse
/// rank than the block we already hold there. A peer can move a freshly
/// restarted node onto any valid history by offering one tip block.
#[test]
fn after_restart_any_rank_is_adopted_first() {
    let t = CandidateTracker::default(); // what a restart leaves behind
    assert_eq!(
        t.observe(
            HEIGHT,
            Candidate {
                sealer_rank: 7,
                block_hash: [0xEE; 32]
            },
            [0; 32],
        ),
        Observation::NewBest
    );
}

/// Finding 3 (fixed): one announcement with an absurd height used to wedge
/// catch-up.
/// `request_sync` keeps only the highest target (candidates.rs
/// `keep_higher`), and the driver waits for our scan to reach it before
/// any forkchoice update, so every later, legitimate target is ignored.
#[tokio::test(start_paused = true)]
async fn one_bogus_announce_no_longer_wedges_the_sync_driver() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let head = Arc::new(AtomicU64::new(0));
    let scanned = Arc::new(AtomicU64::new(500));
    let fcus = Arc::new(Mutex::new(Vec::<u64>::new()));
    let rx = engine::candidates::install_sync().unwrap_or_else(|| panic!("first install"));
    let (h, s, f) = (head.clone(), scanned.clone(), fcus.clone());
    let driver = tokio::spawn(engine::candidates::run_sync_driver(
        rx,
        move || h.load(Ordering::SeqCst),
        move || Some(s.load(Ordering::SeqCst)),
        move |t: SyncTarget| {
            let f = f.clone();
            async move {
                f.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(t.sova_height);
                Ok(())
            }
        },
    ));
    // The attacker: `Announce { height: 10^12, hash: random }` (sova/1
    // `on_announce` forwards any height beyond p2p range to request_sync).
    engine::candidates::request_sync(SyncTarget {
        sova_height: 1_000_000_000_000,
        block_hash: [9; 32],
    });
    // The honest network: a real tip we could sync to right now.
    engine::candidates::request_sync(SyncTarget {
        sova_height: 400,
        block_hash: [1; 32],
    });
    tokio::time::sleep(Duration::from_secs(60)).await;
    let seen = fcus.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(
        seen.contains(&400),
        "fixed (F3): the driver catches up to the honest tip despite the bogus target: {seen:?}"
    );
    assert!(
        !seen.contains(&1_000_000_000_000),
        "the unsatisfiable target is never acted on"
    );
    driver.abort();
}
