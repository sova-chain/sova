//! SIP-8 §2.4 fork choice and §2.6 margins, as pure functions over a block
//! tree and a vote table. Dormant: nothing calls them yet (stage 3 of the
//! SIP-8 build; the tracker and arbiter wiring is stage 4).
//!
//! Inputs are the node's own view: the validated blocks it holds, rooted at
//! its newest checkpoint (or genesis), and the summed vote weight per
//! reference `(h, H)` from its own Zcash scan (`engine::votes`). Nothing
//! here depends on the order blocks or votes arrived in: that is the point
//! (SIP-8 §2.4, "no first-seen input anywhere in the rule").
//!
//! Tie-breaks between children with equal weight, in order:
//! 1. the cumulative sealer-rank comparison of their preferred chains
//!    ([`crate::sealer::prefer_branch`], audit F2 measure C);
//! 2. the child's own sealer rank, lower first (SIP-2/SIP-6);
//! 3. the child already on the node's canonical chain (the incumbent);
//! 4. the lower block hash.
//!
//! SIP-8's draft lists "SIP-2/SIP-6 preference of `c` itself (rank
//! ascending, then hash ascending)" before the incumbent; since hashes never
//! tie, that would make the incumbent rule unreachable, while §2.4 also says
//! "tie-break 3 keeps the incumbent". This module takes the reachable
//! reading (rank, then incumbent, then hash); the SIP text needs the same
//! edit.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::sealer::prefer_branch;
use crate::sip1::SovaRef;

/// One validated block the node holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Block hash.
    pub hash: [u8; 32],
    /// Parent hash.
    pub parent: [u8; 32],
    /// Block number.
    pub height: u64,
    /// Sealer trust rank (`crate::sealer::NULL_RANK` for a null block).
    pub rank: usize,
}

/// The node's validated blocks, rooted at `root` (checkpoint or genesis).
#[derive(Debug, Clone)]
pub struct Tree {
    root: [u8; 32],
    blocks: HashMap<[u8; 32], Block>,
    /// Children by parent hash, sorted by hash (deterministic).
    children: HashMap<[u8; 32], Vec<[u8; 32]>>,
}

impl Tree {
    /// A tree rooted at `root` from `blocks` (in any order). Blocks that do
    /// not descend from `root` through blocks in the set are ignored.
    #[must_use]
    pub fn new(root: Block, blocks: impl IntoIterator<Item = Block>) -> Self {
        let mut all: HashMap<[u8; 32], Block> = blocks.into_iter().map(|b| (b.hash, b)).collect();
        all.insert(root.hash, root);
        let mut children: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
        for b in all.values() {
            if b.hash != root.hash {
                children.entry(b.parent).or_default().push(b.hash);
            }
        }
        // Keep only what hangs off the root.
        let mut blocks = HashMap::new();
        let mut stack = vec![root.hash];
        while let Some(h) = stack.pop() {
            if let Some(b) = all.get(&h) {
                blocks.insert(h, *b);
            }
            if let Some(kids) = children.get(&h) {
                stack.extend(kids.iter().copied());
            }
        }
        children.retain(|parent, _| blocks.contains_key(parent));
        for kids in children.values_mut() {
            kids.retain(|k| blocks.contains_key(k));
            kids.sort_unstable();
        }
        Self {
            root: root.hash,
            blocks,
            children,
        }
    }

    /// The root's hash.
    #[must_use]
    pub const fn root(&self) -> [u8; 32] {
        self.root
    }

    /// Whether the tree holds `hash`.
    #[must_use]
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.blocks.contains_key(hash)
    }

    fn kids(&self, hash: &[u8; 32]) -> &[[u8; 32]] {
        self.children.get(hash).map_or(&[], Vec::as_slice)
    }
}

/// Vote weights resolved against a tree: `W` per block (its own votes plus
/// its descendants') and the unattributed votes by reference height.
#[derive(Debug, Clone, Default)]
pub struct Weights {
    subtree: HashMap<[u8; 32], u128>,
    /// `(reference height, weight)` of votes for blocks not in the tree.
    unattributed: Vec<(u64, u128)>,
}

impl Weights {
    /// Resolve `votes` (summed weight per reference) against `tree`. A vote
    /// counts for block `H` only if the tree holds `H` at height `h`;
    /// otherwise it is unattributed (SIP-8 §2.5: never dropped).
    #[must_use]
    pub fn resolve(tree: &Tree, votes: &HashMap<SovaRef, u128>) -> Self {
        let mut own: HashMap<[u8; 32], u128> = HashMap::new();
        let mut unattributed = Vec::new();
        for (r, &w) in votes {
            match tree.blocks.get(&r.hash) {
                Some(b) if b.height == u64::from(r.height) => {
                    *own.entry(r.hash).or_default() += w;
                }
                _ => unattributed.push((u64::from(r.height), w)),
            }
        }
        unattributed.sort_unstable();
        let mut subtree = HashMap::new();
        subtree_sum(tree, tree.root, &own, &mut subtree);
        Self {
            subtree,
            unattributed,
        }
    }

    /// `W(X)`.
    #[must_use]
    pub fn w(&self, hash: &[u8; 32]) -> u128 {
        self.subtree.get(hash).copied().unwrap_or(0)
    }

    /// `U(h)`: unattributed weight whose reference height is at least `h`.
    #[must_use]
    pub fn u(&self, height: u64) -> u128 {
        self.unattributed
            .iter()
            .filter(|(h, _)| *h >= height)
            .map(|(_, w)| w)
            .sum()
    }
}

fn subtree_sum(
    tree: &Tree,
    at: [u8; 32],
    own: &HashMap<[u8; 32], u128>,
    out: &mut HashMap<[u8; 32], u128>,
) -> u128 {
    // Iterative post-order: trees can be thousands of blocks deep.
    let mut stack = vec![(at, false)];
    while let Some((h, expanded)) = stack.pop() {
        if expanded {
            let sum = own.get(&h).copied().unwrap_or(0)
                + tree
                    .kids(&h)
                    .iter()
                    .map(|k| out.get(k).copied().unwrap_or(0))
                    .sum::<u128>();
            out.insert(h, sum);
        } else {
            stack.push((h, true));
            stack.extend(tree.kids(&h).iter().map(|k| (*k, false)));
        }
    }
    out.get(&at).copied().unwrap_or(0)
}

/// SIP-8 §2.4: the head. Start at the root; at each block move to the child
/// with the greatest `W`, breaking ties as the module doc lists; stop at a
/// block with no children. `incumbent` is the node's current canonical
/// chain (may be empty for a node with none, e.g. while joining).
#[must_use]
pub fn head(tree: &Tree, weights: &Weights, incumbent: &HashSet<[u8; 32]>) -> [u8; 32] {
    let mut at = tree.root;
    let mut memo = HashMap::new();
    while let Some(next) = best_child(tree, weights, incumbent, &at, &mut memo) {
        at = next;
    }
    at
}

/// The preferred chain below `from` (exclusive), as sealer ranks in height
/// order: the path `head` would walk from there.
fn preferred_ranks(
    tree: &Tree,
    weights: &Weights,
    incumbent: &HashSet<[u8; 32]>,
    from: [u8; 32],
    memo: &mut HashMap<[u8; 32], Vec<usize>>,
) -> Vec<usize> {
    if let Some(r) = memo.get(&from) {
        return r.clone();
    }
    // Walk down first (the tie-break of each step may itself need a deeper
    // comparison, which recursion through best_child handles).
    let mut path = Vec::new();
    let mut at = from;
    while let Some(next) = best_child(tree, weights, incumbent, &at, memo) {
        path.push(next);
        at = next;
    }
    let ranks: Vec<usize> = path
        .iter()
        .filter_map(|h| tree.blocks.get(h).map(|b| b.rank))
        .collect();
    memo.insert(from, ranks.clone());
    ranks
}

fn best_child(
    tree: &Tree,
    weights: &Weights,
    incumbent: &HashSet<[u8; 32]>,
    at: &[u8; 32],
    memo: &mut HashMap<[u8; 32], Vec<usize>>,
) -> Option<[u8; 32]> {
    let kids = tree.kids(at);
    let mut best: Option<[u8; 32]> = None;
    for &c in kids {
        best = Some(match best {
            None => c,
            Some(b) => {
                if compare_children(tree, weights, incumbent, &c, &b, memo) == Ordering::Less {
                    c
                } else {
                    b
                }
            }
        });
    }
    best
}

/// `Less` means `a` is preferred over `b`.
fn compare_children(
    tree: &Tree,
    weights: &Weights,
    incumbent: &HashSet<[u8; 32]>,
    a: &[u8; 32],
    b: &[u8; 32],
    memo: &mut HashMap<[u8; 32], Vec<usize>>,
) -> Ordering {
    // Greatest W first.
    let by_weight = weights.w(b).cmp(&weights.w(a));
    if by_weight != Ordering::Equal {
        return by_weight;
    }
    let (ba, bb) = (tree.blocks[a], tree.blocks[b]);
    // 1. Cumulative rank over each child's preferred chain, the child itself
    //    included.
    let chain = |h: &[u8; 32], rank: usize, memo: &mut HashMap<[u8; 32], Vec<usize>>| {
        let mut v = vec![rank];
        v.extend(preferred_ranks(tree, weights, incumbent, *h, memo));
        v
    };
    let (ca, cb) = (chain(a, ba.rank, memo), chain(b, bb.rank, memo));
    prefer_branch(&ca, &cb)
        // 2. The child's own rank.
        .then_with(|| ba.rank.cmp(&bb.rank))
        // 3. The incumbent.
        .then_with(|| incumbent.contains(b).cmp(&incumbent.contains(a)))
        // 4. The lower hash.
        .then_with(|| a.cmp(b))
}

/// SIP-8 §2.6: the margin of block `x` on the head chain, `M(x)`, the least
/// `m(Y) = W(Y) − Σ W(siblings of Y) − U(height(Y))` over the path from the
/// root (exclusive) to `x` (inclusive). `None` if `x` is not in the tree or
/// is the root. Positive: no set of votes cast so far, known or not, can
/// make another block win at `x`'s height or below.
#[must_use]
pub fn margin(tree: &Tree, weights: &Weights, x: &[u8; 32]) -> Option<i128> {
    if *x == tree.root || !tree.contains(x) {
        return None;
    }
    let mut min: Option<i128> = None;
    let mut at = *x;
    while at != tree.root {
        let b = tree.blocks.get(&at)?;
        let siblings: u128 = tree
            .kids(&b.parent)
            .iter()
            .filter(|s| **s != at)
            .map(|s| weights.w(s))
            .sum();
        let m = signed(weights.w(&at)) - signed(siblings) - signed(weights.u(b.height));
        min = Some(min.map_or(m, |cur| cur.min(m)));
        at = b.parent;
    }
    min
}

fn signed(v: u128) -> i128 {
    i128::try_from(v).unwrap_or(i128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sealer::NULL_RANK;

    fn h(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    fn blk(tag: u8, parent: u8, height: u64, rank: usize) -> Block {
        Block {
            hash: h(tag),
            parent: h(parent),
            height,
            rank,
        }
    }

    const GENESIS: u8 = 0;

    fn root() -> Block {
        blk(GENESIS, 0xff, 0, NULL_RANK)
    }

    fn vote(tag: u8, height: u32, w: u128) -> (SovaRef, u128) {
        (
            SovaRef {
                height,
                hash: h(tag),
            },
            w,
        )
    }

    /// root ─ 1 ─ 2 ─ 3            (the "honest" chain, all rank 0)
    ///         └─ 12 ─ 13 ─ 14      (a fork at height 2, rank 1 sealers)
    fn forked() -> Vec<Block> {
        vec![
            blk(1, GENESIS, 1, 0),
            blk(2, 1, 2, 0),
            blk(3, 2, 3, 0),
            blk(12, 1, 2, 1),
            blk(13, 12, 3, 1),
            blk(14, 13, 4, 1),
        ]
    }

    fn head_of(blocks: Vec<Block>, votes: &[(SovaRef, u128)], incumbent: &[u8]) -> [u8; 32] {
        let tree = Tree::new(root(), blocks);
        let weights = Weights::resolve(&tree, &votes.iter().copied().collect());
        let inc: HashSet<[u8; 32]> = incumbent.iter().map(|t| h(*t)).collect();
        head(&tree, &weights, &inc)
    }

    #[test]
    fn without_votes_cumulative_rank_decides() {
        // The longer fork has worse ranks over the common range: the honest
        // chain wins even though it is shorter.
        assert_eq!(head_of(forked(), &[], &[]), h(3));
    }

    #[test]
    fn votes_beat_rank_below_the_tip() {
        // 10 zat of votes for the fork's 13 outweigh the honest chain's 0.
        assert_eq!(head_of(forked(), &[vote(13, 3, 10)], &[1, 2, 3]), h(14));
        // More weight on the honest side wins back.
        assert_eq!(
            head_of(forked(), &[vote(13, 3, 10), vote(2, 2, 11)], &[]),
            h(3)
        );
        // A vote for 1 counts on both sides equally: rank decides again.
        assert_eq!(head_of(forked(), &[vote(1, 1, 1_000)], &[]), h(3));
    }

    #[test]
    fn a_vote_naming_the_wrong_height_is_unattributed() {
        // (height 9, hash 13) matches no block: it cannot pull the fork.
        let tree = Tree::new(root(), forked());
        let w = Weights::resolve(&tree, &[vote(13, 9, 50)].into_iter().collect());
        assert_eq!(w.w(&h(13)), 0);
        assert_eq!(w.u(9), 50);
        assert_eq!(w.u(10), 0);
        assert_eq!(head(&tree, &w, &HashSet::new()), h(3));
    }

    #[test]
    fn equal_everything_keeps_the_incumbent_then_the_lower_hash() {
        // Two same-rank single-block siblings, no votes.
        let blocks = vec![blk(1, GENESIS, 1, 0), blk(2, GENESIS, 1, 0)];
        assert_eq!(head_of(blocks.clone(), &[], &[2]), h(2));
        assert_eq!(head_of(blocks.clone(), &[], &[1]), h(1));
        assert_eq!(head_of(blocks, &[], &[]), h(1));
    }

    #[test]
    fn at_the_tip_a_late_better_rank_still_wins() {
        // SIP-8 §2.4: the newest block has no votes, so rank decides as
        // SIP-2 does: a rank-0 sibling displaces an on-time rank-1 one.
        let blocks = vec![blk(1, GENESIS, 1, 0), blk(2, 1, 2, 1), blk(3, 1, 2, 0)];
        assert_eq!(head_of(blocks, &[vote(1, 1, 5)], &[1, 2]), h(3));
    }

    /// SIP-8 §10: the result does not depend on insertion order.
    #[test]
    fn head_is_independent_of_insertion_order() {
        let mut blocks = forked();
        blocks.push(blk(22, 2, 3, 0));
        blocks.push(blk(23, 22, 4, 2));
        let votes = [vote(13, 3, 7), vote(22, 3, 7), vote(99, 4, 3)];
        let expected = head_of(blocks.clone(), &votes, &[]);
        // Every rotation, and every rotation reversed.
        for i in 0..blocks.len() {
            let mut rot = blocks.clone();
            rot.rotate_left(i);
            assert_eq!(head_of(rot.clone(), &votes, &[]), expected, "rotation {i}");
            rot.reverse();
            assert_eq!(head_of(rot, &votes, &[]), expected, "reversed rotation {i}");
        }
    }

    #[test]
    fn blocks_off_the_root_are_ignored() {
        let mut blocks = forked();
        blocks.push(blk(50, 0x77, 5, 0)); // parent unknown
        let tree = Tree::new(root(), blocks);
        assert!(!tree.contains(&h(50)));
        assert!(tree.contains(&h(14)));
    }

    #[test]
    fn margins_follow_section_2_6() {
        let tree = Tree::new(root(), forked());
        let votes: HashMap<SovaRef, u128> = [vote(3, 3, 30), vote(13, 3, 10), vote(77, 2, 4)]
            .into_iter()
            .collect();
        let w = Weights::resolve(&tree, &votes);
        assert_eq!(head(&tree, &w, &HashSet::new()), h(3));
        // Path root→1→2→3. U(1) = U(2) = 4 (the (2, 77) vote), U(3) = 0.
        // m(1) = 40 − 0 − 4 = 36; m(2) = 30 − 10 − 4 = 16; m(3) = 30 − 0 − 0.
        assert_eq!(margin(&tree, &w, &h(1)), Some(36));
        assert_eq!(margin(&tree, &w, &h(2)), Some(16));
        assert_eq!(margin(&tree, &w, &h(3)), Some(16));
        // The losing branch has a negative margin.
        assert_eq!(margin(&tree, &w, &h(13)), Some(-24));
        assert_eq!(margin(&tree, &w, &h(GENESIS)), None);
        assert_eq!(margin(&tree, &w, &h(99)), None);
    }

    #[test]
    fn deep_trees_do_not_overflow_the_stack() {
        let mut blocks = Vec::new();
        let mut parent = [0u8; 32];
        for i in 1..=20_000u32 {
            let mut hash = [0u8; 32];
            hash[..4].copy_from_slice(&i.to_be_bytes());
            hash[31] = 1;
            blocks.push(Block {
                hash,
                parent,
                height: u64::from(i),
                rank: 0,
            });
            parent = hash;
        }
        let tree = Tree::new(root(), blocks);
        let w = Weights::resolve(&tree, &HashMap::new());
        assert_eq!(head(&tree, &w, &HashSet::new()), parent);
    }
}
