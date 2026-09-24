//! SIP-8 §6 on the miner side: whether a burn may carry a reference to a
//! Sova block (a vote) at all, and which block it references.
//!
//! **Dormant by default.** A burn carries a reference only when all of
//! these hold, and otherwise it is the SIP-1 version-1 burn `mine` has
//! always sent:
//!
//! 1. `mine --sova-rpc <url>` names a Sova node. Without it nothing here
//!    runs and burning never waits on Sova.
//! 2. The network's SIP-8 activation height is known ([`Sip8Gate`]) and the
//!    burn's Zcash block will be at or above it. A version-2 burn mined
//!    below the activation height is not a burn: its ZEC is destroyed and
//!    nothing is minted. No network has an activation height yet
//!    ([`SIP8_ACTIVATION_MAINNET`] and the others are `None`), so today
//!    only `--sip8-from` on regtest turns this on.
//! 3. The node's head is fresh: the Zcash block it anchors is on this
//!    miner's own zebrad's best chain, at most [`MAX_ANCHOR_LAG`] blocks
//!    below the tip.
//!
//! **The anchor epoch comes from the head itself, not from `B`.** SIP-8 §6
//! states the freshness check as `number + B − 1` (the head's anchor epoch)
//! against the Zcash tip. Every Sova block commits to the hash of the Zcash
//! block it anchors, `E_N = N + B − 1`, in `parent_beacon_block_root`
//! (SIP-4 §1; validators reject a block whose anchor is wrong). So the
//! miner reads that hash from the head and asks its own zebrad for the
//! block's height: that height *is* `N + B − 1`, with no epoch base to
//! configure or get wrong. It also checks something `B` alone could not:
//! that the head anchors a block on the miner's own Zcash chain. A head
//! anchored on another Zcash branch is never voted for (its vote would
//! count for nothing, SIP-8 §2.3).
//!
//! **Trust.** A vote is only as good as the node it came from: the head is
//! whatever that node's fork choice says. Pointing `--sova-rpc` at someone
//! else's node hands them this miner's vote.

use std::thread;
use std::time::{Duration, Instant};

use burn_wallet::Network;
use burn_wallet::rpc::{RpcClient, RpcError};
use consensus::sip1::SovaRef;
use serde_json::{Value, json};

/// SIP-8 activation on Zcash mainnet (Sova mainnet), as the first Zcash
/// height at which version-2 burns are recognized. `None`: not activated.
pub(crate) const SIP8_ACTIVATION_MAINNET: Option<u64> = None;
/// SIP-8 activation on Zcash testnet (the Sova testnet). `None` until the
/// testnet chain profile (`bin/sova/src/chain.rs`) fixes an activation
/// epoch and a release compiles it in here too.
pub(crate) const SIP8_ACTIVATION_TESTNET: Option<u64> = None;
/// SIP-8 activation on regtest (the box and the sims). `None`: a regtest
/// Sova node recognizes no v2 burns unless it is told to, so a miner must be
/// told too (`--sip8-from`).
pub(crate) const SIP8_ACTIVATION_REGTEST: Option<u64> = None;

/// SIP-8 §6: a head whose anchor epoch is more than this many Zcash blocks
/// below the tip is behind or stuck, and is not voted for.
pub(crate) const MAX_ANCHOR_LAG: u64 = 2;

/// Per-request timeout for the Sova node, so a hung node costs a burn at
/// most a few seconds on top of `--vote-wait`.
pub(crate) const SOVA_RPC_TIMEOUT: Duration = Duration::from_secs(3);

/// How often `--vote-wait` re-reads the Sova head.
const VOTE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The network's built-in SIP-8 activation height.
#[must_use]
pub(crate) const fn network_activation(network: Network) -> Option<u64> {
    match network {
        Network::Main => SIP8_ACTIVATION_MAINNET,
        Network::Test => SIP8_ACTIVATION_TESTNET,
        Network::Regtest => SIP8_ACTIVATION_REGTEST,
    }
}

/// This run's SIP-8 activation height: the network's built-in value, or
/// `--sip8-from` (`override_from`), which is for regtest testing only. On
/// testnet and mainnet the activation height comes with the release, never
/// from a flag: a wrong one destroys ZEC.
///
/// # Errors
///
/// A human-readable reason when `--sip8-from` is given for a network other
/// than regtest, or contradicts a built-in value.
pub(crate) fn resolve_activation(
    network: Network,
    override_from: Option<u64>,
) -> Result<Option<u64>, String> {
    let builtin = network_activation(network);
    match (network, override_from) {
        (_, None) => Ok(builtin),
        (Network::Regtest, Some(from)) => match builtin {
            Some(b) if b != from => Err(format!(
                "--sip8-from {from} contradicts regtest's built-in SIP-8 activation height {b}"
            )),
            _ => Ok(Some(from)),
        },
        (other, Some(_)) => Err(format!(
            "--sip8-from is for regtest testing only; {other:?}'s SIP-8 activation height \
             comes with the release, never from a flag (a v2 burn before activation \
             destroys its ZEC and mints nothing)"
        )),
    }
}

/// The one gate a version-2 burn passes through (SIP-8 §6 "Activation
/// guard").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sip8Gate {
    /// The first Zcash height at which v2 burns are recognized, if known.
    activation: Option<u64>,
}

impl Sip8Gate {
    /// A gate for activation height `activation` (`None`: not activated).
    #[must_use]
    pub(crate) const fn new(activation: Option<u64>) -> Self {
        Self { activation }
    }

    /// The activation height, if known.
    #[must_use]
    pub(crate) const fn activation(self) -> Option<u64> {
        self.activation
    }

    /// Whether a burn that can be mined no lower than Zcash height
    /// `target_height` would be recognized as a v2 burn.
    #[must_use]
    pub(crate) fn active_at(self, target_height: u64) -> bool {
        self.activation.is_some_and(|from| target_height >= from)
    }

    /// `Some(reference)` only if a burn targeting `target_height` may carry
    /// it; `None` means send a v1 burn.
    #[must_use]
    pub(crate) fn admit(self, target_height: u64, reference: SovaRef) -> Option<SovaRef> {
        self.active_at(target_height).then_some(reference)
    }
}

/// A Sova node's head, from `eth_getBlockByNumber("latest", false)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SovaHead {
    /// Block number.
    pub number: u64,
    /// Block hash, as the RPC prints it.
    pub hash: [u8; 32],
    /// `parentBeaconBlockRoot`: the hash of the Zcash block this block
    /// anchors, in zebrad's display order.
    pub anchor: [u8; 32],
}

/// Reads a Sova node's head.
pub(crate) trait SovaNode {
    /// The node's current head (its fork-choice head: "latest").
    fn head(&self) -> Result<SovaHead, String>;
}

/// Asks this miner's own zebrad where a Zcash block is.
pub(crate) trait ZcashChain {
    /// The best-chain height of the Zcash block with display-order hash
    /// `hash`, or why it can't be placed there (unknown to this node, or on
    /// a side branch).
    fn best_chain_height(&self, hash: [u8; 32]) -> Result<u64, String>;
}

/// Parses an Ethereum JSON-RPC quantity (`"0x1a"`).
fn quantity(v: &Value, field: &str) -> Result<u64, String> {
    let s = v
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("head has no {field}"))?;
    let digits = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("head {field} {s:?} is not a 0x quantity"))?;
    u64::from_str_radix(digits, 16).map_err(|_| format!("head {field} {s:?} is not a quantity"))
}

/// Parses a 0x-prefixed 32-byte hash, keeping the RPC's byte order.
fn hash32(v: &Value, field: &str) -> Result<[u8; 32], String> {
    let s = v
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("head has no {field}"))?;
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|_| format!("head {field} {s:?} is not hex"))?;
    bytes
        .try_into()
        .map_err(|_| format!("head {field} {s:?} is not 32 bytes"))
}

/// Parses an `eth_getBlockByNumber` result into a [`SovaHead`].
fn parse_head(block: &Value) -> Result<SovaHead, String> {
    if block.is_null() {
        return Err("the node returned no latest block".to_string());
    }
    Ok(SovaHead {
        number: quantity(block, "number")?,
        hash: hash32(block, "hash")?,
        // A Sova block always carries its Zcash anchor here (SIP-4 §1); a
        // block without one is not a Sova block.
        anchor: hash32(block, "parentBeaconBlockRoot")
            .map_err(|e| format!("{e} (is --sova-rpc a Sova node?)"))?,
    })
}

impl SovaNode for RpcClient {
    fn head(&self) -> Result<SovaHead, String> {
        let block = self
            .call_method("eth_getBlockByNumber", json!(["latest", false]))
            .map_err(|e| e.to_string())?;
        parse_head(&block)
    }
}

impl ZcashChain for RpcClient {
    fn best_chain_height(&self, hash: [u8; 32]) -> Result<u64, String> {
        let hex_hash = hex::encode(hash);
        let block = self.get_block_summary(&hex_hash).map_err(|e| match e {
            RpcError::RpcFailure { message, .. } => {
                format!("this miner's zebrad doesn't have it ({message})")
            }
            other => other.to_string(),
        })?;
        let height = block
            .get("height")
            .and_then(Value::as_u64)
            .ok_or_else(|| "zebrad's getblock answer has no height".to_string())?;
        // On the best chain iff the best chain's block at that height is
        // this one (zebrad can also serve a side-chain block by hash).
        let at_height = self.get_block_hash(height).map_err(|e| e.to_string())?;
        // Both in display order: compare the hex.
        if !at_height.eq_ignore_ascii_case(&hex_hash) {
            return Err(format!(
                "it is not on this miner's zebrad's best chain (height {height} there is {at_height})"
            ));
        }
        Ok(height)
    }
}

/// A reference chosen for a burn, with what the freshness check saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Vote {
    /// The Sova block referenced.
    pub reference: SovaRef,
    /// The Zcash height that block anchors (`number + B − 1`).
    pub anchor_height: u64,
}

/// SIP-8 §6: read the Sova head, waiting up to `vote_wait` for the block
/// that anchors `zcash_tip` (the "next Sova block": block `N` once Zcash
/// block `E_N` is out), then check the head is fresh and return it as a
/// reference. Stops waiting as soon as the head anchors `zcash_tip` or
/// later, is already too far behind to be voted for, or the node can't be
/// read; with a zero `vote_wait` it reads the head once.
///
/// # Errors
///
/// Why this burn should carry no vote (sent as v1 instead): the node can't
/// be read, its head anchors a Zcash block this miner's zebrad doesn't have
/// on its best chain, the head is more than [`MAX_ANCHOR_LAG`] epochs
/// behind, or the head is genesis (never a vote).
pub(crate) fn pick_reference(
    sova: &impl SovaNode,
    zcash: &impl ZcashChain,
    zcash_tip: u64,
    vote_wait: Duration,
) -> Result<Vote, String> {
    pick_reference_polling(sova, zcash, zcash_tip, vote_wait, VOTE_POLL_INTERVAL)
}

fn pick_reference_polling(
    sova: &impl SovaNode,
    zcash: &impl ZcashChain,
    zcash_tip: u64,
    vote_wait: Duration,
    poll: Duration,
) -> Result<Vote, String> {
    let deadline = Instant::now() + vote_wait;
    // The newest head whose anchor this miner's zebrad places, and the
    // last reason one couldn't be used.
    let mut best: Option<(SovaHead, u64)> = None;
    let mut last_seen: Option<[u8; 32]> = None;
    let mut why_not = String::from("no head read");
    loop {
        match sova.head() {
            Ok(head) if last_seen != Some(head.hash) => {
                last_seen = Some(head.hash);
                match zcash.best_chain_height(head.anchor) {
                    Ok(e) => best = Some((head, e)),
                    Err(e) => {
                        why_not = format!(
                            "Sova head {} anchors Zcash block {}, but {e}",
                            head.number,
                            hex::encode(head.anchor)
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                // An unreachable node never holds up a burn: go with what
                // was read so far, if anything.
                why_not = format!("cannot read the Sova head: {e}");
                break;
            }
        }
        // Stop waiting once the head anchors the tip (nothing newer to
        // wait for) or is too far behind to be voted for: a stuck node
        // never holds up a burn either.
        if best.is_some_and(|(_, e)| e >= zcash_tip || zcash_tip - e > MAX_ANCHOR_LAG) {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep(poll.min(deadline - now));
    }
    let (head, anchor_height) = best.ok_or(why_not)?;
    if zcash_tip.saturating_sub(anchor_height) > MAX_ANCHOR_LAG {
        return Err(format!(
            "the Sova node is behind: its head {} anchors Zcash height {anchor_height}, more than {MAX_ANCHOR_LAG} below the tip {zcash_tip}",
            head.number
        ));
    }
    if head.number == 0 {
        return Err("the Sova head is genesis, which is never a vote".to_string());
    }
    let height = u32::try_from(head.number)
        .map_err(|_| format!("Sova head {} does not fit a v2 reference", head.number))?;
    Ok(Vote {
        reference: SovaRef {
            height,
            hash: head.hash,
        },
        anchor_height,
    })
}

/// A run's anchored-burn setup (`mine --sova-rpc`), deciding each epoch's
/// reference.
pub(crate) struct Anchoring<S> {
    /// The Sova node votes come from.
    pub sova: S,
    /// Its URL, for the log.
    pub url: String,
    /// The activation gate.
    pub gate: Sip8Gate,
    /// `--vote-wait`.
    pub vote_wait: Duration,
    /// Whether "not active yet at this height" was already said.
    said_before_activation: bool,
}

impl<S: SovaNode> Anchoring<S> {
    /// Anchored burns from node `sova` (at `url`) behind `gate`.
    pub(crate) fn new(sova: S, url: String, gate: Sip8Gate, vote_wait: Duration) -> Self {
        Self {
            sova,
            url,
            gate,
            vote_wait,
            said_before_activation: false,
        }
    }

    /// The one line `mine` prints at startup about anchored burns: what it
    /// will do, and if it will send only v1, why.
    pub(crate) fn startup_line(&self, network: Network) -> String {
        match self.gate.activation() {
            None => format!(
                "anchored burns (SIP-8): off. {network:?} has no SIP-8 activation height yet, so every burn is a SIP-1 v1 burn and --sova-rpc {} is not used (a v2 burn before activation is not a burn: its ZEC is destroyed and nothing is minted)",
                self.url
            ),
            Some(from) => format!(
                "anchored burns (SIP-8): on from Zcash height {from}; each burn votes for the head of the Sova node at {} (--vote-wait {}s). A vote is only as good as the node it came from.",
                self.url,
                self.vote_wait.as_secs()
            ),
        }
    }

    /// The reference for a burn that can be mined no lower than
    /// `target_height`, with the Zcash tip now at `zcash_tip`; `None` means
    /// a v1 burn. Never queries the Sova node unless SIP-8 is active at
    /// `target_height`. Logs why a burn goes out without a vote.
    pub(crate) fn reference_for(
        &mut self,
        zcash: &impl ZcashChain,
        target_height: u64,
        zcash_tip: u64,
    ) -> Option<SovaRef> {
        let from = self.gate.activation()?;
        if !self.gate.active_at(target_height) {
            if !self.said_before_activation {
                self.said_before_activation = true;
                println!(
                    "anchored burns (SIP-8): not active before Zcash height {from}; until a burn can't be mined below it (this one targets {target_height}), burns are sent as v1"
                );
            }
            return None;
        }
        match pick_reference(&self.sova, zcash, zcash_tip, self.vote_wait) {
            Ok(vote) => {
                let admitted = self.gate.admit(target_height, vote.reference);
                if admitted.is_some() {
                    println!(
                        "vote: Sova block {} (0x{}) anchoring Zcash {} (tip {zcash_tip}, epoch base {})",
                        vote.reference.height,
                        hex::encode(vote.reference.hash),
                        vote.anchor_height,
                        (vote.anchor_height + 1).saturating_sub(u64::from(vote.reference.height)),
                    );
                }
                admitted
            }
            Err(why) => {
                eprintln!("warning: sending this burn as v1, with no vote: {why}");
                None
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    const R: SovaRef = SovaRef {
        height: 7,
        hash: [0x77; 32],
    };

    #[test]
    fn no_network_has_an_activation_height_yet() {
        for network in [Network::Main, Network::Test, Network::Regtest] {
            assert_eq!(network_activation(network), None, "{network:?}");
            assert_eq!(resolve_activation(network, None), Ok(None));
        }
    }

    #[test]
    fn sip8_from_is_regtest_only() {
        assert_eq!(
            resolve_activation(Network::Regtest, Some(150)),
            Ok(Some(150))
        );
        for network in [Network::Main, Network::Test] {
            let err = resolve_activation(network, Some(150)).unwrap_err();
            assert!(err.contains("regtest testing only"), "{err}");
        }
    }

    /// The activation guard: no v2 burn without a known activation height,
    /// and none that could be mined below it.
    #[test]
    fn v2_before_activation_is_refused() {
        let off = Sip8Gate::new(None);
        for target in [0, 1, 150, u64::MAX] {
            assert_eq!(off.admit(target, R), None);
        }
        let on = Sip8Gate::new(Some(150));
        assert_eq!(on.admit(0, R), None);
        assert_eq!(on.admit(149, R), None);
        assert_eq!(on.admit(150, R), Some(R));
        assert_eq!(on.admit(151, R), Some(R));
    }

    /// A Sova node whose head is `heads[i]` on its i-th read (the last one
    /// repeats), counting reads.
    struct FakeSova {
        heads: Vec<Result<SovaHead, String>>,
        reads: Cell<usize>,
    }

    impl FakeSova {
        fn new(heads: Vec<Result<SovaHead, String>>) -> Self {
            Self {
                heads,
                reads: Cell::new(0),
            }
        }
    }

    impl SovaNode for FakeSova {
        fn head(&self) -> Result<SovaHead, String> {
            let i = self.reads.get();
            self.reads.set(i + 1);
            self.heads[i.min(self.heads.len() - 1)].clone()
        }
    }

    /// A zebrad with a best chain where Zcash block `h` has hash `[h; 32]`,
    /// up to `tip`, with `B = 101` (Sova block `N` anchors `N + 100`).
    struct FakeZcash {
        tip: u64,
        asked: RefCell<Vec<[u8; 32]>>,
    }

    impl ZcashChain for FakeZcash {
        fn best_chain_height(&self, hash: [u8; 32]) -> Result<u64, String> {
            self.asked.borrow_mut().push(hash);
            let h = u64::from(hash[0]);
            (hash == [hash[0]; 32] && h <= self.tip)
                .then_some(h)
                .ok_or_else(|| "not on this chain".to_string())
        }
    }

    fn zcash(tip: u64) -> FakeZcash {
        FakeZcash {
            tip,
            asked: RefCell::new(Vec::new()),
        }
    }

    /// Sova block `n`, anchoring Zcash `n + 100` (B = 101).
    fn head(n: u64) -> SovaHead {
        SovaHead {
            number: n,
            hash: [0xa0 + u8::try_from(n).unwrap(); 32],
            anchor: [u8::try_from(n + 100).unwrap(); 32],
        }
    }

    fn pick(sova: &FakeSova, z: &FakeZcash, tip: u64, wait_ms: u64) -> Result<Vote, String> {
        pick_reference_polling(
            sova,
            z,
            tip,
            Duration::from_millis(wait_ms),
            Duration::from_millis(1),
        )
    }

    #[test]
    fn a_head_anchoring_the_tip_is_voted_for_without_waiting() {
        let sova = FakeSova::new(vec![Ok(head(20))]);
        let vote = pick(&sova, &zcash(120), 120, 10_000).unwrap();
        assert_eq!(vote.reference.height, 20);
        assert_eq!(vote.reference.hash, head(20).hash);
        assert_eq!(vote.anchor_height, 120);
        assert_eq!(sova.reads.get(), 1);
    }

    /// `--vote-wait`: the head lags the tip by one; the miner waits for the
    /// next block and references it.
    #[test]
    fn vote_wait_waits_for_the_next_sova_block() {
        let sova = FakeSova::new(vec![Ok(head(19)), Ok(head(19)), Ok(head(20))]);
        let vote = pick(&sova, &zcash(120), 120, 10_000).unwrap();
        assert_eq!(vote.reference.height, 20);
        assert_eq!(sova.reads.get(), 3);
    }

    /// The next block doesn't come in time: reference whatever the head is
    /// (lag 1 is fresh).
    #[test]
    fn vote_wait_times_out_to_the_current_head() {
        let sova = FakeSova::new(vec![Ok(head(19))]);
        let vote = pick(&sova, &zcash(120), 120, 20).unwrap();
        assert_eq!(vote.reference.height, 19);
        assert_eq!(vote.anchor_height, 119);
        // Zero wait: one read.
        let sova = FakeSova::new(vec![Ok(head(19))]);
        assert_eq!(
            pick(&sova, &zcash(120), 120, 0).unwrap().reference.height,
            19
        );
        assert_eq!(sova.reads.get(), 1);
    }

    /// Neither a stuck node nor an unreachable one holds up a burn for
    /// the whole `--vote-wait`.
    #[test]
    fn stale_or_unreachable_nodes_are_not_waited_for() {
        let sova = FakeSova::new(vec![Ok(head(10))]);
        assert!(pick(&sova, &zcash(120), 120, 10_000).is_err());
        assert_eq!(sova.reads.get(), 1);
        let sova = FakeSova::new(vec![Err("connection refused".to_string())]);
        assert!(pick(&sova, &zcash(120), 120, 10_000).is_err());
        assert_eq!(sova.reads.get(), 1);
        // A read failing mid-wait keeps the head already read.
        let sova = FakeSova::new(vec![Ok(head(19)), Err("timeout".to_string())]);
        assert_eq!(
            pick(&sova, &zcash(120), 120, 10_000)
                .unwrap()
                .reference
                .height,
            19
        );
    }

    #[test]
    fn freshness_allows_a_lag_of_two_not_three() {
        let sova = FakeSova::new(vec![Ok(head(18))]);
        assert!(pick(&sova, &zcash(120), 120, 0).is_ok());
        let sova = FakeSova::new(vec![Ok(head(17))]);
        let err = pick(&sova, &zcash(120), 120, 0).unwrap_err();
        assert!(err.contains("behind"), "{err}");
    }

    #[test]
    fn a_head_anchored_off_this_zcash_chain_is_not_voted_for() {
        let mut off = head(20);
        off.anchor = [0xee; 32];
        let sova = FakeSova::new(vec![Ok(off)]);
        let err = pick(&sova, &zcash(120), 120, 0).unwrap_err();
        assert!(err.contains("not on this chain"), "{err}");
    }

    #[test]
    fn genesis_and_unreachable_nodes_are_not_votes() {
        let sova = FakeSova::new(vec![Ok(SovaHead {
            number: 0,
            hash: [1; 32],
            anchor: [100; 32],
        })]);
        let err = pick(&sova, &zcash(100), 100, 0).unwrap_err();
        assert!(err.contains("genesis"), "{err}");
        let sova = FakeSova::new(vec![Err("connection refused".to_string())]);
        let err = pick(&sova, &zcash(100), 100, 0).unwrap_err();
        assert!(err.contains("connection refused"), "{err}");
    }

    /// With no activation height the Sova node is never asked, whatever
    /// the target: an inactive `--sova-rpc` never delays a burn.
    #[test]
    fn inactive_anchoring_never_queries_the_node() {
        let mut a = Anchoring::new(
            FakeSova::new(vec![Ok(head(20))]),
            "http://sova".into(),
            Sip8Gate::new(None),
            Duration::from_secs(10),
        );
        assert!(a.startup_line(Network::Regtest).contains("off"));
        let z = zcash(120);
        assert_eq!(a.reference_for(&z, 121, 120), None);
        assert_eq!(a.sova.reads.get(), 0);
        assert!(z.asked.borrow().is_empty());

        // Active from 200: below it, still no query.
        let mut a = Anchoring::new(
            FakeSova::new(vec![Ok(head(20))]),
            "http://sova".into(),
            Sip8Gate::new(Some(200)),
            Duration::from_secs(10),
        );
        assert_eq!(a.reference_for(&z, 121, 120), None);
        assert_eq!(a.sova.reads.get(), 0);
    }

    #[test]
    fn active_anchoring_references_a_fresh_head() {
        let mut a = Anchoring::new(
            FakeSova::new(vec![Ok(head(20))]),
            "http://sova".into(),
            Sip8Gate::new(Some(121)),
            Duration::from_secs(10),
        );
        assert_eq!(
            a.reference_for(&zcash(120), 121, 120),
            Some(SovaRef {
                height: 20,
                hash: head(20).hash
            })
        );
        // A stale head: v1.
        let mut a = Anchoring::new(
            FakeSova::new(vec![Ok(head(10))]),
            "http://sova".into(),
            Sip8Gate::new(Some(121)),
            Duration::ZERO,
        );
        assert_eq!(a.reference_for(&zcash(120), 121, 120), None);
    }

    /// The exact `eth_getBlockByNumber` shape reth serves for a Sova block.
    #[test]
    fn parses_a_sova_head() {
        let block = json!({
            "number": "0x1f",
            "hash": format!("0x{}", "ab".repeat(32)),
            "parentBeaconBlockRoot": format!("0x{}", "0c".repeat(32)),
            "parentHash": format!("0x{}", "00".repeat(32)),
        });
        assert_eq!(
            parse_head(&block),
            Ok(SovaHead {
                number: 31,
                hash: [0xab; 32],
                anchor: [0x0c; 32],
            })
        );
        let mut plain = block.clone();
        plain
            .as_object_mut()
            .unwrap()
            .remove("parentBeaconBlockRoot");
        assert!(parse_head(&plain).unwrap_err().contains("Sova node"));
        assert!(parse_head(&Value::Null).is_err());
    }
}
