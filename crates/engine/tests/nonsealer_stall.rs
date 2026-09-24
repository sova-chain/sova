//! Liveness under burners that never seal (sim/nonsealer-stall).
//!
//! Drives the real [`SealerCore`] against a mock Zcash chain that grows on a
//! fixed cadence, with instant builds (every trigger lands at once), and
//! measures how far the Sova head falls behind the Zcash tip when burners
//! with no sealing node win epochs.
//!
//! What it pinned (found 2026-09-24, fixed in the same week): an epoch's
//! ladder clock started only when the epoch reached the front of the
//! in-order queue, so every epoch paid its full wait after the previous
//! block, and a backlog past the queue cap (256) dropped the front epoch and
//! halted the sealer for good. Now the clock runs from when the sealer
//! first saw the epoch, the null-block wait is also bounded by epoch E+1's
//! arrival plus one rank_step (SIP-6), and no epoch at or above the head is
//! ever dropped. These tests assert the bounded behaviour.

use std::time::{Duration, Instant};

use consensus::follower::{BlockView, TxOut, TxView, ViewError, ZcashView};
use consensus::schedule::Schedule;
use consensus::sip1::{BurnPayload, burn_lock_script};
use engine::PendingEpoch;
use engine::driver::{DRAFT_EPOCH_REWARD_GWEI, SealerConfig, SealerCore, SealerOutcome};

const STEP: Duration = Duration::from_secs(15);

/// A Zcash chain that the test extends block by block.
#[derive(Default)]
struct GrowingChain {
    blocks: Vec<BlockView>,
}

impl ZcashView for GrowingChain {
    fn tip_height(&self) -> Result<u64, ViewError> {
        Ok(self.blocks.len() as u64)
    }
    fn block_at(&self, height: u64) -> Result<Option<BlockView>, ViewError> {
        if height == 0 {
            return Ok(None);
        }
        Ok(usize::try_from(height - 1)
            .ok()
            .and_then(|i| self.blocks.get(i))
            .cloned())
    }
}

fn hash(height: u64) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&height.to_be_bytes());
    h[31] = 0x5A;
    h
}

fn burn_tx(txid: u8, height: u64, who: [u8; 20], value_zat: u64) -> TxView {
    let mut id = hash(height);
    id[30] = txid;
    TxView {
        txid: id,
        outputs: vec![
            TxOut {
                value_zat: 0,
                script: BurnPayload {
                    evm_address: who,
                    signal_bits: 0,
                }
                .to_script()
                .to_vec(),
            },
            TxOut {
                value_zat,
                script: burn_lock_script().to_vec(),
            },
        ],
        version: 5,
        shielded: Default::default(),
    }
}

impl GrowingChain {
    fn push(&mut self, burners: &[([u8; 20], u64)]) {
        let height = self.blocks.len() as u64 + 1;
        self.blocks.push(BlockView {
            height,
            hash: hash(height),
            prev_hash: if height == 1 {
                [0; 32]
            } else {
                hash(height - 1)
            },
            time: 0,
            txs: burners
                .iter()
                .enumerate()
                .map(|(i, &(who, zat))| burn_tx(u8::try_from(i).unwrap_or(0), height, who, zat))
                .collect(),
            pools: None,
        });
    }
}

struct Run {
    /// Largest `expected(zcash tip) − sova head` seen, in epochs.
    max_lag: u64,
    /// The lag when the run ended.
    final_lag: u64,
    /// Longest stretch the Sova head did not move, in seconds.
    longest_stall_secs: u64,
    /// Heads that moved only after a wait of more than 5 s.
    slow_epochs: u64,
}

/// Simulate `run` seconds of a Zcash chain with a block every `interval`
/// (block 1 at t = 0), polling the sealer once a second. `burners_at(h)`
/// lists the burns in block h. Our node seals as `our` (SIP-6 on); nobody
/// else seals anything.
fn simulate(
    our: [u8; 20],
    interval: Duration,
    run: Duration,
    burners_at: impl Fn(u64) -> Vec<([u8; 20], u64)>,
) -> Run {
    let mut chain = GrowingChain::default();
    let pending = PendingEpoch::default();
    let mut core = SealerCore::new(
        SealerConfig {
            our_address: our,
            schedule: Schedule::Flat {
                reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
            },
            rank_step: STEP,
            sip6: true,
        },
        1,
        100,
    );
    let t0 = Instant::now();
    let total = run.as_secs();
    let (mut head, mut max_lag, mut slow_epochs) = (0u64, 0u64, 0u64);
    let (mut last_move, mut longest_stall) = (0u64, 0u64);
    for sec in 0..=total {
        if sec % interval.as_secs() == 0 {
            let h = chain.blocks.len() as u64 + 1;
            chain.push(&burners_at(h));
        }
        let out = core
            .process(
                &chain,
                head,
                &pending,
                t0 + Duration::from_secs(sec),
                |_| None, // nobody else's candidate ever arrives
            )
            .unwrap_or_else(|e| panic!("{e}"));
        for o in out {
            if let SealerOutcome::Trigger { sova_height, .. } = o
                && sova_height == head + 1
            {
                let _ = pending.take_for(sova_height);
                head = sova_height;
                longest_stall = longest_stall.max(sec - last_move);
                if sec - last_move > 5 {
                    slow_epochs += 1;
                }
                last_move = sec;
            }
        }
        let tip = chain.blocks.len() as u64; // base 1: expected == tip
        max_lag = max_lag.max(tip.saturating_sub(head));
    }
    let tip = chain.blocks.len() as u64;
    Run {
        max_lag,
        final_lag: tip.saturating_sub(head),
        longest_stall_secs: longest_stall,
        slow_epochs,
    }
}

const OURS: [u8; 20] = [0xAA; 20];
const STRANGER: [u8; 20] = [0xBB; 20];

/// The box observation, fixed: on 3 s Zcash blocks a non-sealing burner
/// that wins every other epoch alone (our miner burns the others) used to
/// cost ~45 s per such epoch, serially, and the lag grew to 150 Zcash blocks.
/// With the ladder clock running from when each epoch was seen and the null
/// block due one rank_step after epoch E+1 arrives, each abandoned epoch
/// costs about one rank_step and the waits overlap.
#[test]
fn box_cadence_one_nonsealer_stays_within_a_few_blocks() {
    let run = simulate(
        OURS,
        Duration::from_secs(3),
        Duration::from_secs(900),
        |h| {
            if !(10..30).contains(&h) {
                vec![]
            } else if h % 2 == 0 {
                vec![(STRANGER, 200_000)] // stranger alone: abandoned epoch
            } else {
                vec![(OURS, 100_000)] // ours: sealed at once
            }
        },
    );
    println!(
        "box cadence, stranger alone: max lag {} Zcash blocks, longest head stall {} s, \
         slow epochs {}, final lag {}",
        run.max_lag, run.longest_stall_secs, run.slow_epochs, run.final_lag
    );
    // E+1 arrives 3 s later, then one 15 s rank_step (+ poll granularity).
    assert!(
        run.longest_stall_secs <= 25,
        "stall {} s",
        run.longest_stall_secs
    );
    assert!(run.max_lag <= 10, "lag {}", run.max_lag);
    assert!(run.final_lag <= 1);
}

/// Same, with our miner also burning in the stranger's epochs (we are rank
/// 1): one 15 s rung from when the epoch was seen, overlapping, so the head
/// stays a few Zcash blocks behind instead of 80+.
#[test]
fn box_cadence_ranked_fallback_stays_within_a_few_blocks() {
    let run = simulate(
        OURS,
        Duration::from_secs(3),
        Duration::from_secs(900),
        |h| {
            if (10..30).contains(&h) {
                vec![(STRANGER, 200_000), (OURS, 100_000)]
            } else {
                vec![]
            }
        },
    );
    println!(
        "box cadence, we are rank 1: max lag {} Zcash blocks, longest head stall {} s, \
         slow epochs {}, final lag {}",
        run.max_lag, run.longest_stall_secs, run.slow_epochs, run.final_lag
    );
    assert!(
        run.longest_stall_secs <= 20,
        "stall {} s",
        run.longest_stall_secs
    );
    assert!(run.max_lag <= 10, "lag {}", run.max_lag);
    assert!(run.final_lag <= 1);
}

/// The liveness gap at testnet cadence (75 s Zcash blocks, SIP-6 on).
/// Four non-sealing strangers burn in every epoch and no sealer is ranked:
/// each epoch's null block waits `(4 + 2) × 15 s = 90 s` counted from the
/// moment the previous block landed, never from when the epoch was seen,
/// so the chain loses 15 s per epoch and never catches up (three strangers:
/// 75 s, break-even at best, losing ground on every slow Zcash block).
///
/// Expected: an epoch seen while an earlier one was still pending has
/// been waiting too; once its predecessor lands it should need only what is
/// left of its own wait, so the head stays within about one wait (~1.2
/// epochs) of the tip.
///
/// Failed before the fix (the ladder clock started at the queue front).
#[test]
fn testnet_cadence_nonsealing_burners_must_not_grow_the_lag() {
    let strangers: [[u8; 20]; 4] = [[0xB1; 20], [0xB2; 20], [0xB3; 20], [0xB4; 20]];
    let run = simulate(
        OURS,
        Duration::from_secs(75),
        Duration::from_secs(75 * 60),
        |_| strangers.iter().map(|&s| (s, 100_000)).collect(),
    );
    println!(
        "testnet cadence, 4 non-sealers every epoch: max lag {} epochs, final lag {}",
        run.max_lag, run.final_lag
    );
    assert!(
        run.final_lag <= 2,
        "head fell {} epochs behind the Zcash tip after 60 epochs",
        run.final_lag
    );
}

/// The cliff behind the box's permanent halt. Once the head is more than
/// `QUEUE_RETAIN` (256) epochs behind the Zcash tip, `SealerCore::process`
/// drops the queue's oldest entries (driver.rs `pop_first`), which include
/// the very epoch at the front. The front is then a gap that the "missing
/// height's epoch hasn't been observed" branch waits on forever. The
/// follower already scanned that epoch, so it never comes back, and nothing
/// is produced again even after the burns stop.
///
/// Live on the box: a stranger burning every other block for 180 s left
/// the head at 260 with the tip at 505 (08:06:28); the next abandoned epoch
/// was due 45 s later, by which time the queue held more than 256 epochs,
/// and the head stayed at 260 for good (tip 573 and rising).
///
/// Failed before the fix.
#[test]
fn backlog_beyond_queue_retain_must_not_halt_the_sealer() {
    let run = simulate(
        OURS,
        Duration::from_secs(3),
        Duration::from_secs(5000),
        |h| {
            if (10..80).contains(&h) && h % 2 == 0 {
                vec![(STRANGER, 200_000)]
            } else {
                vec![]
            }
        },
    );
    println!(
        "box cadence, 35 stranger epochs: max lag {} Zcash blocks, final lag {}",
        run.max_lag, run.final_lag
    );
    // 35 × ~46 s ≈ 1600 s of waits against a 5000 s run: the backlog
    // drains long before the end if the sealer keeps going.
    assert!(
        run.final_lag <= 1,
        "sealer halted {} epochs behind the Zcash tip",
        run.final_lag
    );
}

/// Same cliff without any burns: a mine-mode node whose head is more than
/// 256 epochs behind the Zcash tip when its sealer starts (a restart after
/// a long outage of every sealer, or a fresh node) prunes its own front
/// epoch on the first poll and never produces. A network-wide outage of
/// 256 epochs (~5.3 h at 75 s) would then need a code change to restart.
///
/// Failed before the fix.
#[test]
fn sealer_started_far_behind_the_tip_must_produce() {
    let mut chain = GrowingChain::default();
    for _ in 0..300 {
        chain.push(&[]);
    }
    let pending = PendingEpoch::default();
    let mut core = SealerCore::new(
        SealerConfig {
            our_address: OURS,
            schedule: Schedule::Flat {
                reward_gwei: DRAFT_EPOCH_REWARD_GWEI,
            },
            rank_step: STEP,
            sip6: true,
        },
        1,
        100,
    );
    let out = core
        .process(&chain, 0, &pending, Instant::now(), |_| None)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.iter()
            .any(|o| matches!(o, SealerOutcome::Trigger { sova_height: 1, .. })),
        "burn-less epoch 1 must be produced; got {} outcomes",
        out.len()
    );
}

/// SIP-6's other null-block trigger bounds the dust-burn grief: twenty
/// addresses burning dust in every epoch, none of them sealing, used to
/// make the null block wait `(20 + 2) × 15 s = 330 s` per 75 s epoch. Epoch
/// E+1's arrival plus one rank_step now caps it at about 90 s, overlapped,
/// so the head stays within a couple of epochs.
#[test]
fn testnet_cadence_dust_from_many_addresses_does_not_grow_the_lag() {
    let dust: Vec<[u8; 20]> = (0u8..20)
        .map(|i| {
            [
                0xC0 | (i & 0x0F),
                i,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
            ]
        })
        .collect();
    let run = simulate(
        OURS,
        Duration::from_secs(75),
        Duration::from_secs(75 * 60),
        |_| dust.iter().map(|&s| (s, 1_000)).collect(),
    );
    println!(
        "testnet cadence, 20 dust burners every epoch: max lag {} epochs, final lag {}",
        run.max_lag, run.final_lag
    );
    assert!(run.final_lag <= 2, "final lag {}", run.final_lag);
    assert!(run.max_lag <= 3, "max lag {}", run.max_lag);
}
