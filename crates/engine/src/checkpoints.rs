//! Client checkpoints (audit 2026-09-23 F2, measure B;
//! `docs/design/f2-join-and-restart.md` §B).
//!
//! A list of `(height, hash)` pairs. The node accepts no block at a
//! checkpoint height other than the listed one, so it follows no history
//! that contradicts the list. This is weak subjectivity: a node trusts
//! whoever chose the list to have named the history the network followed
//! (Sova Labs for the list built into a release, the operator for entries
//! added with `SOVA_CHECKPOINTS`). It does not trust them for validity:
//! every block is still checked against the node's own zebrad (C5, the
//! SIP-4 anchor, the SIP-6 seal), so a wrong checkpoint can put a node on a
//! different valid history or stop it, but never create or change a mint.
//!
//! Enforced in three places: `SovaConsensus::validate_header` (every
//! import path; a permanent rejection), the sync driver's target choice
//! (`candidates::actionable_target`), and bin/sova's startup check of the
//! stored chain.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use alloy_primitives::hex;

/// Checkpoints by Sova height.
pub type Checkpoints = BTreeMap<u64, [u8; 32]>;

static CHECKPOINTS: OnceLock<Checkpoints> = OnceLock::new();

/// Install the process's checkpoints. Call once at startup, before the
/// node launches (headers are checked from the first import on). Returns
/// `false` if already installed.
pub fn install(checkpoints: Checkpoints) -> bool {
    CHECKPOINTS.set(checkpoints).is_ok()
}

/// The installed checkpoints (empty if none were installed).
#[must_use]
pub fn installed() -> &'static Checkpoints {
    static EMPTY: Checkpoints = BTreeMap::new();
    CHECKPOINTS.get().unwrap_or(&EMPTY)
}

/// The checkpointed hash at `height`, if there is one.
#[must_use]
pub fn at(height: u64) -> Option<[u8; 32]> {
    installed().get(&height).copied()
}

/// Whether a block `hash` at `height` agrees with the checkpoints: true
/// unless `height` is checkpointed with another hash.
#[must_use]
pub fn allows(height: u64, hash: &[u8; 32]) -> bool {
    at(height).is_none_or(|expected| &expected == hash)
}

/// A block at a checkpoint height with another hash. Permanent: no block
/// at that height with another hash is ever acceptable to this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointMismatch {
    /// The checkpointed Sova height.
    pub height: u64,
    /// The checkpointed hash.
    pub expected: [u8; 32],
    /// The offered block's hash.
    pub got: [u8; 32],
}

impl std::fmt::Display for CheckpointMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "checkpoint mismatch at height {}: block is 0x{}, checkpoint is 0x{}",
            self.height,
            hex::encode(self.got),
            hex::encode(self.expected)
        )
    }
}

impl std::error::Error for CheckpointMismatch {}

/// Check one block against `checkpoints`.
pub fn check_in(
    checkpoints: &Checkpoints,
    height: u64,
    hash: [u8; 32],
) -> Result<(), CheckpointMismatch> {
    match checkpoints.get(&height) {
        Some(&expected) if expected != hash => Err(CheckpointMismatch {
            height,
            expected,
            got: hash,
        }),
        _ => Ok(()),
    }
}

/// Check one block against the installed checkpoints.
pub fn check(height: u64, hash: [u8; 32]) -> Result<(), CheckpointMismatch> {
    check_in(installed(), height, hash)
}

/// Merge the built-in list with operator entries from `SOVA_CHECKPOINTS`
/// (`raw`: comma-separated `height:0xhash`). An operator entry that
/// contradicts a built-in one at the same height is an error, never a
/// silent override; so is a malformed entry, or two operator entries
/// that disagree.
pub fn merge(builtin: &[(u64, [u8; 32])], raw: Option<&str>) -> Result<Checkpoints, String> {
    let mut out: Checkpoints = builtin.iter().copied().collect();
    let Some(raw) = raw else {
        return Ok(out);
    };
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (height, hash) = parse_entry(entry)?;
        if let Some(existing) = out.insert(height, hash)
            && existing != hash
        {
            return Err(format!(
                "SOVA_CHECKPOINTS entry {entry:?} contradicts the checkpoint at height {height} \
                 (0x{}); refusing to start",
                hex::encode(existing)
            ));
        }
    }
    Ok(out)
}

fn parse_entry(entry: &str) -> Result<(u64, [u8; 32]), String> {
    let bad =
        |why: &str| format!("bad SOVA_CHECKPOINTS entry {entry:?}: {why} (want height:0xhash)");
    let (h, x) = entry.split_once(':').ok_or_else(|| bad("no ':'"))?;
    let height: u64 = h
        .trim()
        .parse()
        .map_err(|_| bad("height is not a number"))?;
    let x = x.trim();
    let x = x.strip_prefix("0x").unwrap_or(x);
    let bytes = hex::decode(x).map_err(|_| bad("hash is not hex"))?;
    let hash: [u8; 32] = bytes.try_into().map_err(|_| bad("hash is not 32 bytes"))?;
    Ok((height, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [0xaa; 32];
    const B: [u8; 32] = [0xbb; 32];

    fn hx(b: [u8; 32]) -> String {
        format!("0x{}", hex::encode(b))
    }

    #[test]
    fn merge_adds_operator_entries() {
        let raw = format!("200:{}, 300:{}", hx(A), hex::encode(B));
        let c = merge(&[(100, A)], Some(&raw)).unwrap_or_default();
        assert_eq!(c.len(), 3);
        assert_eq!(c[&200], A);
        assert_eq!(c[&300], B);
        // Repeating a built-in entry exactly is fine.
        assert!(merge(&[(100, A)], Some(&format!("100:{}", hx(A)))).is_ok());
        assert_eq!(merge(&[(100, A)], None).map(|c| c.len()), Ok(1));
        assert_eq!(merge(&[], Some(" , ")).map(|c| c.len()), Ok(0));
    }

    #[test]
    fn merge_refuses_contradictions_and_garbage() {
        let err = merge(&[(100, A)], Some(&format!("100:{}", hx(B))))
            .err()
            .unwrap_or_default();
        assert!(err.contains("contradicts"), "{err}");
        let err = merge(&[], Some(&format!("5:{},5:{}", hx(A), hx(B))))
            .err()
            .unwrap_or_default();
        assert!(err.contains("contradicts"), "{err}");
        for bad in ["100", "x:0xaa", "100:0xzz", "100:0xaaaa"] {
            assert!(merge(&[], Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn check_rejects_only_a_different_hash_at_a_checkpoint() {
        let c: Checkpoints = [(100, A)].into_iter().collect();
        assert_eq!(check_in(&c, 100, A), Ok(()));
        assert_eq!(check_in(&c, 101, B), Ok(()));
        let err = check_in(&c, 100, B).err();
        assert_eq!(
            err,
            Some(CheckpointMismatch {
                height: 100,
                expected: A,
                got: B
            })
        );
        assert!(err.is_some_and(|e| e.to_string().contains("checkpoint mismatch at height 100")));
    }
}
