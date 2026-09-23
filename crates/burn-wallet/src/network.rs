//! Network parameters for transaction building.
//!
//! `zcash_protocol::consensus::Network` (the enum librustzcash ships) only
//! has `MainNetwork`/`TestNetwork` variants -- there is no `Regtest`
//! variant, because mainnet/testnet activation heights are protocol
//! constants but regtest's are whatever the operator's node is configured
//! with. librustzcash's answer to this is
//! [`zcash_protocol::local_consensus::LocalNetwork`] (behind the
//! `local-consensus` feature): a plain struct of `Option<BlockHeight>`
//! fields that the caller fills in to match their node.
//!
//! [`Network::Regtest`] fills that in to match this repo's
//! `box/regtest/zebrad.toml`: `[network.testnet_parameters.activation_heights]
//! NU5 = 1`, which (per Zebra's `ConfiguredActivationHeights::for_regtest`)
//! also defaults Overwinter/Sapling/Blossom/Heartwood/Canopy to height 1.
//! NU6 and later are left unset (never active) there, so they are `None`
//! here too. If the harness config ever adds e.g. `NU6 = 1`, this must be
//! updated to match, or `BranchId::for_height` (used to pick the
//! `consensus_branch_id` for signing) will compute the wrong branch and
//! Zebra will reject the resulting transaction.

use zcash_protocol::consensus::{
    BlockHeight, MAIN_NETWORK, NetworkType, NetworkUpgrade, Parameters, TEST_NETWORK,
};
use zcash_protocol::local_consensus::LocalNetwork;

/// The Zcash network a transaction is being built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    /// Zcash Mainnet.
    Main,
    /// Zcash Testnet.
    Test,
    /// Regtest, configured to match `box/regtest/zebrad.toml`.
    Regtest,
}

impl Network {
    /// Whether this network's consensus lets a transaction with
    /// transparent outputs spend a transparent coinbase output.
    ///
    /// Zcash consensus (mainnet and the public testnet) requires a
    /// transparent coinbase output to be spent in a transaction that has
    /// **only shielded outputs**: zebrad rejects anything else with
    /// `unshielded transparent coinbase spend ... must be spent in a
    /// transaction which only has shielded outputs`. Every transaction
    /// this crate builds has transparent outputs (the SIP-1 burn, change,
    /// a faucet drip), so on those networks coinbase can never fund one
    /// directly: it must be shielded first, and come back to a t-addr as
    /// an ordinary (non-coinbase) output.
    ///
    /// Zebra's regtest instead defaults
    /// `should_allow_unshielded_coinbase_spends = true`, and
    /// `box/regtest/zebrad.toml` doesn't override it, which is what lets
    /// the box fund its miners straight from `generatetoaddress`.
    pub fn allows_unshielded_coinbase_spends(self) -> bool {
        matches!(self, Network::Regtest)
    }

    /// The regtest activation-height configuration matching
    /// `box/regtest/zebrad.toml`: NU5 (and everything it defaults) active
    /// from height 1; NU6 and later never active.
    fn regtest_local() -> LocalNetwork {
        let genesis = Some(BlockHeight::from_u32(1));
        LocalNetwork {
            overwinter: genesis,
            sapling: genesis,
            blossom: genesis,
            heartwood: genesis,
            canopy: genesis,
            nu5: genesis,
            nu6: None,
            nu6_1: None,
            nu6_2: None,
            nu6_3: None,
        }
    }
}

impl Parameters for Network {
    fn network_type(&self) -> NetworkType {
        match self {
            Network::Main => NetworkType::Main,
            Network::Test => NetworkType::Test,
            Network::Regtest => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Network::Main => MAIN_NETWORK.activation_height(nu),
            Network::Test => TEST_NETWORK.activation_height(nu),
            Network::Regtest => Self::regtest_local().activation_height(nu),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_protocol::consensus::BranchId;

    #[test]
    fn regtest_is_nu5_from_height_one() {
        let net = Network::Regtest;
        assert_eq!(
            BranchId::for_height(&net, BlockHeight::from_u32(1)),
            BranchId::Nu5
        );
        assert_eq!(
            BranchId::for_height(&net, BlockHeight::from_u32(1_000)),
            BranchId::Nu5
        );
    }

    #[test]
    fn only_regtest_allows_unshielded_coinbase_spends() {
        assert!(Network::Regtest.allows_unshielded_coinbase_spends());
        assert!(!Network::Test.allows_unshielded_coinbase_spends());
        assert!(!Network::Main.allows_unshielded_coinbase_spends());
    }

    #[test]
    fn regtest_network_type_is_regtest() {
        assert_eq!(Network::Regtest.network_type(), NetworkType::Regtest);
        assert_eq!(Network::Main.network_type(), NetworkType::Main);
        assert_eq!(Network::Test.network_type(), NetworkType::Test);
    }
}
