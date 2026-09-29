//! The consensus branch ID a transaction is signed for, and its default
//! expiry delta.
//!
//! A v5 transaction's ZIP 244 sighash commits to its consensus branch ID,
//! and zebrad only admits a transaction whose ID is the one for the block
//! it is building next. So the ID must be exact. Until NU7 this crate took
//! it from the compile-time activation table (`BranchId::for_height`), but
//! NU7's heights are only set the day before it activates on testnet
//! (ZIP 259), so no build made before then can know them. Instead every
//! transaction is signed for the branch **zebrad reports for its next
//! block** (`getblockchaininfo` -> `consensus.nextblock`, see
//! [`crate::rpc::NextBlockConsensus`]): upgrading zebrad is enough to follow
//! a network upgrade.
//!
//! What this module adds on top, so a wrong ID is never signed silently:
//!
//! - an ID this build of librustzcash doesn't know is refused
//!   ([`BranchError::Unknown`]), never mapped to "the nearest" branch;
//! - a branch that doesn't accept v5 transactions (Sprout, which is also
//!   what zebrad's "no branch" `00000000` decodes to, and everything before
//!   NU5) is refused ([`BranchError::NoV5`]);
//! - the reported branch is cross-checked against the compiled activation
//!   table wherever that table knows the branch's height
//!   ([`BranchError::TableMismatch`]): a node on another chain, or one that
//!   missed an upgrade we do know, is caught. Where the table doesn't know
//!   the upgrade (NU7 on testnet and mainnet today), zebrad's answer is
//!   taken.
//!
//! See `docs/design/nu7-readiness.md`, rows B1 and B2.

use zcash_protocol::consensus::{BlockHeight, BranchId, Parameters};

use zcash_primitives::transaction::TxVersion;

use crate::network::Network;

/// NU7's consensus branch ID (ZIP 259). The retired `0x77190ad8` and the
/// old `zcash_unstable` placeholder `0xffffffff` are not NU7.
pub const NU7_BRANCH_ID: u32 = 0x7719_0ad9;

/// Default expiry delta, in blocks past the target height, before NU7:
/// librustzcash's (and zcashd's) long-standing `DEFAULT_TX_EXPIRY_DELTA`.
pub const DEFAULT_TX_EXPIRY_DELTA: u32 = 40;

/// Default expiry delta once the transaction targets NU7 or later. ZIP 218
/// (25-second blocks): the default expiry delta "SHOULD change to ... 120
/// blocks after activation", keeping roughly the same wall-clock window.
pub const NU7_TX_EXPIRY_DELTA: u32 = 120;

/// The smallest expiry delta accepted. zcashd refuses transactions that
/// expire within `TX_EXPIRING_SOON_THRESHOLD` (3) blocks; anything shorter
/// would be dropped from a mempool before it could be mined.
pub const MIN_TX_EXPIRY_DELTA: u32 = 4;

/// Why a reported consensus branch ID can't be signed for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BranchError {
    /// The ID is not one this build's librustzcash knows. zebrad has
    /// activated (or is configured for) a network upgrade this binary
    /// predates.
    #[error(
        "refusing to sign: zebrad reports consensus branch ID {id:08x} for block {height}, which this build does not know (it knows up to NU7, {NU7_BRANCH_ID:08x}). zebrad is on a network upgrade newer than this binary: upgrade sova-miner / sova-faucet. Signing with any other ID would only be rejected (the ZIP 244 sighash commits to it)."
    )]
    Unknown {
        /// The ID zebrad reported.
        id: u32,
        /// The height it was reported for.
        height: u32,
    },
    /// The branch is known but doesn't accept v5 transactions (Sprout, or
    /// anything before NU5). `00000000` is also what zebrad reports when it
    /// has no branch for the height.
    #[error(
        "refusing to sign: zebrad reports consensus branch {id:08x} ({branch:?}) for block {height}, which does not accept v5 transactions"
    )]
    NoV5 {
        /// The ID zebrad reported.
        id: u32,
        /// What it decodes to.
        branch: BranchId,
        /// The height it was reported for.
        height: u32,
    },
    /// The compiled activation table knows the reported branch's
    /// activation height, and it puts `height` in a different branch: the
    /// node is not on the chain this build expects, or missed an upgrade.
    #[error(
        "refusing to sign: zebrad reports consensus branch {reported:08x} ({reported_branch:?}) for block {height}, but this build's {network:?} activation table puts that block in {expected:08x} ({expected_branch:?}). Is zebrad on the right network and up to date?"
    )]
    TableMismatch {
        /// The network the transaction is built for.
        network: Network,
        /// The height checked.
        height: u32,
        /// What zebrad reported.
        reported: u32,
        /// What it decodes to.
        reported_branch: BranchId,
        /// What the table says.
        expected: u32,
        /// What that decodes to.
        expected_branch: BranchId,
    },
}

/// Parses a consensus branch ID as zebrad's RPC writes it: exactly eight
/// hex digits, big-endian (display order), e.g. `"37a5165b"` for NU6.3 or
/// `"77190ad9"` for NU7. Case-insensitive. `None` for anything else.
#[must_use]
pub fn parse_branch_id_hex(s: &str) -> Option<u32> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// The branch to sign a transaction for, given that zebrad reports `id` as
/// the consensus branch ID of block `height` (its next block).
///
/// # Errors
///
/// [`BranchError::Unknown`] for an ID this build doesn't know,
/// [`BranchError::NoV5`] for a branch without v5 transactions, and
/// [`BranchError::TableMismatch`] when the compiled table knows the
/// reported branch's activation height and disagrees (see the module docs).
pub fn resolve(network: Network, height: u32, id: u32) -> Result<BranchId, BranchError> {
    let branch = BranchId::try_from(id).map_err(|_| BranchError::Unknown { id, height })?;
    if !TxVersion::V5.valid_in_branch(branch) {
        return Err(BranchError::NoV5 { id, branch, height });
    }
    let expected = BranchId::for_height(&network, BlockHeight::from_u32(height));
    let table_knows_branch = branch
        .network_upgrade()
        .and_then(|nu| network.activation_height(nu))
        .is_some();
    if expected != branch && table_knows_branch {
        return Err(BranchError::TableMismatch {
            network,
            height,
            reported: id,
            reported_branch: branch,
            expected: u32::from(expected),
            expected_branch: expected,
        });
    }
    Ok(branch)
}

/// Whether `branch` is NU7 or a later upgrade.
#[must_use]
pub fn is_nu7_or_later(branch: BranchId) -> bool {
    match branch {
        BranchId::Sprout
        | BranchId::Overwinter
        | BranchId::Sapling
        | BranchId::Blossom
        | BranchId::Heartwood
        | BranchId::Canopy
        | BranchId::Nu5
        | BranchId::Nu6
        | BranchId::Nu6_1
        | BranchId::Nu6_2
        | BranchId::Nu6_3 => false,
        // NU7, and anything after it a later librustzcash adds.
        _ => true,
    }
}

/// The default expiry delta for a transaction signed for `branch`:
/// [`NU7_TX_EXPIRY_DELTA`] from NU7 on (ZIP 218), else
/// [`DEFAULT_TX_EXPIRY_DELTA`].
#[must_use]
pub fn default_expiry_delta(branch: BranchId) -> u32 {
    if is_nu7_or_later(branch) {
        NU7_TX_EXPIRY_DELTA
    } else {
        DEFAULT_TX_EXPIRY_DELTA
    }
}

/// `branch` for the log: its ID in zebrad's hex form and its name, e.g.
/// `77190ad9 (Nu7)`.
#[must_use]
pub fn describe(branch: BranchId) -> String {
    format!("{:08x} ({branch:?})", u32::from(branch))
}

#[cfg(test)]
// Test code: an unexpected `Err`/`None` here is a test failure.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Every branch ID the Zcash testnet has used, as zebrad 6.3 reports
    /// them in `getblockchaininfo.upgrades` (verbatim from the laptop's
    /// testnet node, 2026-09-29), plus NU7's from ZIP 259.
    const TESTNET_IDS: [(&str, BranchId); 11] = [
        ("5ba81b19", BranchId::Overwinter),
        ("76b809bb", BranchId::Sapling),
        ("2bb40e60", BranchId::Blossom),
        ("f5b9230b", BranchId::Heartwood),
        ("e9ff75a6", BranchId::Canopy),
        ("c2d6d0b4", BranchId::Nu5),
        ("c8e71055", BranchId::Nu6),
        ("4dec4df0", BranchId::Nu6_1),
        ("5437f330", BranchId::Nu6_2),
        ("37a5165b", BranchId::Nu6_3),
        ("77190ad9", BranchId::Nu7),
    ];

    #[test]
    fn zebrad_hex_ids_map_to_their_branches() {
        for (hex, branch) in TESTNET_IDS {
            let id = parse_branch_id_hex(hex).unwrap_or_else(|| panic!("{hex} parses"));
            assert_eq!(BranchId::try_from(id), Ok(branch), "{hex}");
            assert_eq!(u32::from(branch), id, "{hex} round-trips");
            assert_eq!(describe(branch), format!("{hex} ({branch:?})"));
        }
        assert_eq!(parse_branch_id_hex("77190AD9"), Some(NU7_BRANCH_ID));
    }

    #[test]
    fn malformed_hex_is_not_a_branch_id() {
        for bad in [
            "",
            "7719ad9",
            "077190ad9",
            "0x7719ad",
            "77190adz",
            " 77190ad9",
            "+7190ad9",
        ] {
            assert_eq!(parse_branch_id_hex(bad), None, "{bad:?}");
        }
    }

    /// NU7 (`77190ad9`) as zebrad's next block: signed for NU7 on testnet
    /// and mainnet, whose compiled tables don't know NU7's height -- the
    /// case this module exists for -- and on regtest.
    #[test]
    fn nu7_next_block_resolves_to_nu7() {
        for (network, height) in [
            (Network::Test, 4_500_000),
            (Network::Main, 3_500_000),
            (Network::Regtest, 50),
        ] {
            assert_eq!(
                resolve(network, height, NU7_BRANCH_ID),
                Ok(BranchId::Nu7),
                "{network:?}"
            );
        }
    }

    /// Today's testnet: zebrad and the table agree on NU6.3.
    #[test]
    fn current_testnet_branch_resolves() {
        assert_eq!(
            resolve(Network::Test, 4_415_199, 0x37a5_165b),
            Ok(BranchId::Nu6_3)
        );
        assert_eq!(
            resolve(Network::Regtest, 102, 0xc2d6_d0b4),
            Ok(BranchId::Nu5)
        );
    }

    /// The refusal: an ID this build doesn't know is an error naming it,
    /// never a fallback. Includes the retired NU7 IDs.
    #[test]
    fn unknown_ids_are_refused() {
        for id in [0x7719_0ad8, 0xffff_ffff, 0xffff_fffe, 0x1234_5678] {
            let err = resolve(Network::Test, 4_500_000, id).unwrap_err();
            assert_eq!(
                err,
                BranchError::Unknown {
                    id,
                    height: 4_500_000
                }
            );
            let message = err.to_string();
            assert!(message.starts_with("refusing to sign"), "{message}");
            assert!(message.contains(&format!("{id:08x}")), "{message}");
        }
    }

    /// zebrad's "no branch" (`00000000`, which decodes to Sprout) and
    /// pre-NU5 branches can't carry a v5 transaction.
    #[test]
    fn branches_without_v5_are_refused() {
        assert!(matches!(
            resolve(Network::Regtest, 1, 0),
            Err(BranchError::NoV5 {
                branch: BranchId::Sprout,
                ..
            })
        ));
        assert!(matches!(
            resolve(Network::Test, 1_500_000, 0xe9ff_75a6),
            Err(BranchError::NoV5 {
                branch: BranchId::Canopy,
                ..
            })
        ));
    }

    /// The table knows NU6.3's testnet height, so a zebrad reporting it
    /// where the table has NU6.2 (another chain), or reporting NU6.2 past
    /// NU6.3's height (a node that missed NU6.3), is refused.
    #[test]
    fn disagreement_with_a_known_height_is_refused() {
        assert!(matches!(
            resolve(Network::Test, 4_100_000, 0x37a5_165b),
            Err(BranchError::TableMismatch {
                reported_branch: BranchId::Nu6_3,
                expected_branch: BranchId::Nu6_2,
                ..
            })
        ));
        assert!(matches!(
            resolve(Network::Test, 4_200_000, 0x5437_f330),
            Err(BranchError::TableMismatch {
                reported_branch: BranchId::Nu6_2,
                expected_branch: BranchId::Nu6_3,
                ..
            })
        ));
        // Regtest's table only knows NU5 (at 1): a regtest zebrad
        // configured with later upgrades is taken at its word.
        assert_eq!(
            resolve(Network::Regtest, 5, 0x37a5_165b),
            Ok(BranchId::Nu6_3)
        );
    }

    #[test]
    fn expiry_delta_is_120_from_nu7() {
        assert_eq!(default_expiry_delta(BranchId::Nu5), DEFAULT_TX_EXPIRY_DELTA);
        assert_eq!(default_expiry_delta(BranchId::Nu6_3), 40);
        assert_eq!(default_expiry_delta(BranchId::Nu7), NU7_TX_EXPIRY_DELTA);
        assert_eq!(NU7_TX_EXPIRY_DELTA, 120);
    }
}
