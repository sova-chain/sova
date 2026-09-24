//! Chain profiles (board m1-a): which chainspec `bin/sova` boots.
//!
//! Selected with `SOVA_CHAIN`:
//!
//! - `dev` (default, and what an unset `SOVA_CHAIN` means): reth's built-in
//!   `DEV` spec, untouched — chain ID 1337 and 20 prefunded accounts whose
//!   keys come from the public "test test … junk" mnemonic. The box, the
//!   sims and nightly CI all run on this; it is only for local use.
//! - `sova-testnet`: the public-testnet spec. Same hardfork schedule, gas
//!   limit, base-fee params and Paris-at-genesis as `dev` (so the EVM
//!   rules are identical), but an **empty genesis alloc** — no account
//!   holds a single wei at block 0, so every SOVA in existence traces to
//!   burned ZEC — its own chain ID ([`SOVA_TESTNET_CHAIN_ID`]), and a
//!   Sova-specific genesis header (so a unique genesis hash and fork ID).
//!
//! Bootnodes: reth resolves them as `--bootnodes` → reth config file →
//! `ChainSpec::bootnodes()` → **Ethereum mainnet's**. A custom chainspec
//! returns `None` there, so a `sova-testnet` node left to reth's defaults
//! would dial mainnet bootnodes the moment discovery is on. The profile
//! therefore always sets the list explicitly: `SOVA_BOOTNODES`
//! (comma-separated enode/enr records) if given, else
//! [`SOVA_TESTNET_BOOTNODES`] (empty until m1-b ships public propagation).
//! Whether discovery runs at all is [`crate::discovery`]'s call (on for
//! non-dev p2p nodes, which refuse to start with an unpinned list).

use std::sync::Arc;

use alloy_primitives::Bytes;
use reth_ethereum::{
    chainspec::{Chain, ChainSpec, DEV, make_genesis_header},
    node::core::args::NetworkArgs,
    primitives::SealedHeader,
};

/// Chain ID of the Sova public testnet: 82330, the testnet half of the
/// 8233 / 82330 pair (Rob, 2026-09-22) — 8233 is Zcash's own mainnet P2P
/// port. Both were unassigned on chainlist (`chainid.network/chains.json`,
/// 2,764 chains, 2026-09-22). Deliberately *not* either of the old Sova
/// network's registered IDs (100021 "Sova", 120893 "Sova Sepolia Testnet")
/// — a fresh chain must not be replay-compatible with the old one.
pub(crate) const SOVA_TESTNET_CHAIN_ID: u64 = 82_330;

/// Chain ID reserved for Sova mainnet (the other half of the pair); no
/// mainnet chainspec exists yet.
#[allow(dead_code)]
pub(crate) const SOVA_MAINNET_CHAIN_ID: u64 = 8_233;

/// Genesis `extraData` for the testnet (≤ 32 bytes). The chain ID is not
/// part of the genesis header, and with an empty alloc the state root is
/// the empty-trie root — so without Sova-specific header fields the
/// genesis hash, and with it the EIP-2124 fork ID peers compare in their
/// ENR/Status handshake, would be shared by any empty-alloc chain built
/// from reth's dev parameters. Bump the `v0` if the genesis ever changes.
pub(crate) const SOVA_TESTNET_GENESIS_EXTRA_DATA: &[u8] = b"sova-testnet-v0";

/// Genesis timestamp for the testnet: 2026-09-01T00:00:00Z. Every
/// timestamp-activated fork in the (dev) schedule is at `Timestamp(0)`, so
/// this changes no EVM rule, only the genesis hash.
pub(crate) const SOVA_TESTNET_GENESIS_TIMESTAMP: u64 = 1_788_220_800;

/// Default testnet bootnodes (enode/enr strings). Empty until m1-b: an
/// explicit empty list is what keeps reth from falling back to Ethereum
/// mainnet's bootnodes. `SOVA_BOOTNODES` overrides it.
pub(crate) const SOVA_TESTNET_BOOTNODES: &[&str] = &[];

/// A chain profile, parsed from `SOVA_CHAIN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainProfile {
    /// reth's `DEV` spec (local only: publicly-keyed prefunded accounts).
    Dev,
    /// The public testnet: empty alloc, [`SOVA_TESTNET_CHAIN_ID`].
    SovaTestnet,
}

impl ChainProfile {
    /// Parse a `SOVA_CHAIN` value; `None` (unset) means [`Self::Dev`].
    pub(crate) fn parse(raw: Option<&str>) -> eyre::Result<Self> {
        match raw {
            None | Some("dev") => Ok(Self::Dev),
            Some("sova-testnet") => Ok(Self::SovaTestnet),
            Some(other) => Err(eyre::eyre!(
                "SOVA_CHAIN must be \"dev\" or \"sova-testnet\", got {other:?}"
            )),
        }
    }

    /// Read the profile from the `SOVA_CHAIN` environment variable.
    pub(crate) fn from_env() -> eyre::Result<Self> {
        Self::parse(std::env::var("SOVA_CHAIN").ok().as_deref())
    }

    /// The name this profile is selected by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::SovaTestnet => "sova-testnet",
        }
    }

    /// Pin this profile's bootnode list in `network`, from `SOVA_BOOTNODES`
    /// (`raw_override`) if set, else the profile default. `Dev` with no
    /// override is left exactly as reth builds it.
    pub(crate) fn apply_bootnodes(
        self,
        network: &mut NetworkArgs,
        raw_override: Option<&str>,
    ) -> eyre::Result<()> {
        let records: Vec<&str> = match (raw_override, self) {
            (Some(raw), _) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect(),
            (None, Self::SovaTestnet) => SOVA_TESTNET_BOOTNODES.to_vec(),
            (None, Self::Dev) => return Ok(()),
        };
        let parsed = records
            .into_iter()
            .map(|r| {
                r.parse()
                    .map_err(|e| eyre::eyre!("bad SOVA_BOOTNODES entry {r:?}: {e}"))
            })
            .collect::<eyre::Result<Vec<_>>>()?;
        network.bootnodes = Some(parsed);
        Ok(())
    }

    /// The chainspec this profile boots.
    pub(crate) fn chain_spec(self) -> Arc<ChainSpec> {
        match self {
            Self::Dev => dev_chain_spec(),
            Self::SovaTestnet => sova_testnet_chain_spec(),
        }
    }

    /// The chainspec, with SIP-7's `ZcashBlocks` predeploy when `sip7`
    /// (every node of a chain must agree: it changes the genesis hash).
    pub(crate) fn chain_spec_with(self, sip7: bool) -> Arc<ChainSpec> {
        let spec = self.chain_spec();
        if sip7 { with_zcash_blocks(&spec) } else { spec }
    }

    /// The genesis hash a node of this profile boots with (`sova
    /// genesis-hash`). Only SIP-7 moves it; SIP-6 changes no genesis field.
    pub(crate) fn genesis_hash(self, sip7: bool) -> alloy_primitives::B256 {
        self.chain_spec_with(sip7).genesis_hash()
    }
}

/// SIP-7 §4.1: the `ZcashBlocks` runtime bytecode (`forge inspect
/// ZcashBlocks deployedBytecode` in `contracts/`; regenerate with
/// `contracts/script/export-zcash-blocks.sh`, which also checks drift).
const ZCASH_BLOCKS_RUNTIME_HEX: &str = include_str!("zcash_blocks.runtime.hex");

/// SIP-7's system-contract address (`evm::zcash::ZCASH_BLOCKS`).
const ZCASH_BLOCKS_ADDRESS: alloy_primitives::Address =
    alloy_primitives::address!("0x0000000000000000000000000000000000005A01");

/// `spec` with the `ZcashBlocks` predeploy in its genesis alloc: the
/// runtime code at `0x…5A01`, nonce 1, balance 0, empty storage (the
/// contract has no constructor or immutables), and the genesis header
/// rebuilt for the new state root.
pub(crate) fn with_zcash_blocks(spec: &ChainSpec) -> Arc<ChainSpec> {
    let mut spec = spec.clone();
    let code = alloy_primitives::hex::decode(ZCASH_BLOCKS_RUNTIME_HEX.trim()).unwrap_or_default();
    spec.genesis.alloc.insert(
        ZCASH_BLOCKS_ADDRESS,
        alloy_genesis::GenesisAccount {
            nonce: Some(1),
            balance: alloy_primitives::U256::ZERO,
            code: Some(Bytes::from(code)),
            storage: None,
            private_key: None,
        },
    );
    spec.genesis_header =
        SealedHeader::seal_slow(make_genesis_header(&spec.genesis, &spec.hardforks));
    Arc::new(spec)
}

/// reth's built-in dev chain spec (20 pre-funded dev accounts).
pub(crate) fn dev_chain_spec() -> Arc<ChainSpec> {
    DEV.clone()
}

/// The Sova public-testnet chain spec: reth's `DEV` spec with the chain ID
/// replaced, the genesis alloc emptied, Sova's own genesis extraData and
/// timestamp, and the genesis header recomputed to match. Every other
/// field — hardforks, base-fee params, blob params, Paris-at-genesis, gas
/// limit — is `DEV`'s, so the EVM rules are exactly the ones the box and
/// sims exercise.
pub(crate) fn sova_testnet_chain_spec() -> Arc<ChainSpec> {
    let mut spec = ChainSpec::clone(&DEV);
    spec.chain = Chain::from_id(SOVA_TESTNET_CHAIN_ID);
    spec.genesis.alloc.clear();
    spec.genesis.config.chain_id = SOVA_TESTNET_CHAIN_ID;
    spec.genesis.extra_data = Bytes::from_static(SOVA_TESTNET_GENESIS_EXTRA_DATA);
    spec.genesis.timestamp = SOVA_TESTNET_GENESIS_TIMESTAMP;
    // The header carries the state root (now the empty-trie root),
    // extraData and timestamp, so it must be rebuilt from the edited
    // genesis.
    spec.genesis_header =
        SealedHeader::seal_slow(make_genesis_header(&spec.genesis, &spec.hardforks));
    Arc::new(spec)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use reth_ethereum::chainspec::{EthChainSpec, MAINNET};

    /// reth v2.6.0's `DEV` genesis hash (what the box has always booted).
    const DEV_GENESIS_HASH: B256 = alloy_primitives::b256!(
        "0x683713729fcb72be6f3d8b88c8cda3e10569d73b9640d3bf6f5184d94bd97616"
    );

    /// The `sova-testnet` genesis hash: extraData `sova-testnet-v0`,
    /// timestamp 2026-09-01T00:00:00Z. (The chain ID is not in the header,
    /// so it does not move this.)
    const SOVA_TESTNET_GENESIS_HASH: B256 = alloy_primitives::b256!(
        "0x8b04e8fc22b07ffb31eaac7af0b3c49131bac558679827bd09c65decb36db130"
    );

    /// The `sova-testnet` genesis hash with SIP-7's `ZcashBlocks` predeploy
    /// (`SOVA_SIP7=1`): what the public testnet boots, and what `sova
    /// genesis-hash` prints for it.
    const SOVA_TESTNET_SIP7_GENESIS_HASH: B256 = alloy_primitives::b256!(
        "0xb7391a4a83644e1dce95c95348a005febedeaa12fa46eb30ac0dfb5f36f00b71"
    );

    /// keccak256(rlp([])) — the state root of an empty account trie.
    const EMPTY_ROOT: B256 = alloy_primitives::b256!(
        "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
    );

    #[test]
    fn testnet_genesis_alloc_is_empty() {
        let spec = sova_testnet_chain_spec();
        assert!(spec.genesis.alloc.is_empty());
        assert_eq!(spec.genesis_header.state_root, EMPTY_ROOT);
    }

    #[test]
    fn testnet_uses_the_chosen_chain_id() {
        let spec = sova_testnet_chain_spec();
        assert_eq!(SOVA_TESTNET_CHAIN_ID, 82_330);
        assert_eq!(SOVA_MAINNET_CHAIN_ID, 8_233);
        assert_eq!(spec.chain().id(), SOVA_TESTNET_CHAIN_ID);
        assert_eq!(spec.genesis.config.chain_id, SOVA_TESTNET_CHAIN_ID);
        // Never the old Sova network's IDs, nor reth dev's.
        for taken in [100_021, 120_893, 1337] {
            assert_ne!(SOVA_TESTNET_CHAIN_ID, taken);
        }
    }

    #[test]
    fn testnet_differs_from_dev_only_in_chain_id_alloc_and_genesis() {
        let testnet = sova_testnet_chain_spec();
        // Same EVM rules: identical hardfork schedule and fee params.
        assert_eq!(testnet.hardforks, DEV.hardforks);
        assert_eq!(testnet.base_fee_params, DEV.base_fee_params);
        assert_eq!(testnet.blob_params, DEV.blob_params);
        assert_eq!(
            testnet.paris_block_and_final_difficulty,
            DEV.paris_block_and_final_difficulty
        );
        assert_eq!(testnet.genesis.gas_limit, DEV.genesis.gas_limit);
        // Put back the three things we changed: what remains is DEV.
        let mut restored = ChainSpec::clone(&testnet);
        restored.chain = DEV.chain;
        restored.genesis = DEV.genesis.clone();
        restored.genesis_header = DEV.genesis_header.clone();
        assert_eq!(restored, **DEV);
    }

    /// The testnet genesis hash is Sova's own (not dev's, not a generic
    /// empty-alloc chain's), and pinned: any change to the genesis — and
    /// so to the fork ID peers match on — must be a deliberate edit here.
    #[test]
    fn testnet_genesis_hash_is_unique_and_pinned() {
        let testnet = sova_testnet_chain_spec();
        assert_ne!(testnet.genesis_hash(), DEV.genesis_hash());
        assert_eq!(
            testnet.genesis_header.extra_data.as_ref(),
            SOVA_TESTNET_GENESIS_EXTRA_DATA
        );
        assert!(SOVA_TESTNET_GENESIS_EXTRA_DATA.len() <= 32);
        assert_eq!(
            testnet.genesis_header.timestamp,
            SOVA_TESTNET_GENESIS_TIMESTAMP
        );
        // The same spec with reth-dev's default header fields would hash
        // like any other empty-alloc dev-derived chain.
        let mut generic = ChainSpec::clone(&testnet);
        generic.genesis.extra_data = DEV.genesis.extra_data.clone();
        generic.genesis.timestamp = DEV.genesis.timestamp;
        let generic_hash =
            SealedHeader::seal_slow(make_genesis_header(&generic.genesis, &generic.hardforks))
                .hash();
        assert_ne!(testnet.genesis_hash(), generic_hash);
        // Hence a fork ID (CRC32 over genesis hash + fork schedule) of our
        // own, though the fork schedule is identical to dev's.
        assert_ne!(testnet.latest_fork_id(), DEV.latest_fork_id());
        assert_eq!(testnet.genesis_hash(), SOVA_TESTNET_GENESIS_HASH);
    }

    #[test]
    fn testnet_bootnodes_are_explicit_and_never_mainnet() {
        let mainnet = MAINNET.bootnodes().expect("reth ships mainnet bootnodes");
        let mut network = NetworkArgs::default();
        assert!(
            network.resolved_bootnodes().is_none(),
            "reth's default: falls through to chainspec, then mainnet"
        );
        ChainProfile::SovaTestnet
            .apply_bootnodes(&mut network, None)
            .unwrap();
        // `Some` means reth's chainspec→mainnet fallback never runs.
        let resolved = network.resolved_bootnodes().expect("pinned list");
        assert_eq!(resolved.len(), SOVA_TESTNET_BOOTNODES.len());
        assert!(resolved.iter().all(|n| !mainnet.contains(n)));
        // A custom chainspec has no bootnodes of its own — the reason the
        // explicit list is needed.
        assert!(sova_testnet_chain_spec().bootnodes().is_none());
        // Bootnode pinning leaves discovery alone (on/off is
        // `discovery::apply`'s call).
        assert_eq!(
            network.discovery.disable_discovery,
            NetworkArgs::default().discovery.disable_discovery
        );
    }

    #[test]
    fn bootnode_override_and_dev_default() {
        let enode = "enode://6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0@10.3.58.6:30303";
        let mut network = NetworkArgs::default();
        ChainProfile::SovaTestnet
            .apply_bootnodes(&mut network, Some(enode))
            .unwrap();
        assert_eq!(network.bootnodes.as_ref().map(Vec::len), Some(1));
        assert!(
            ChainProfile::SovaTestnet
                .apply_bootnodes(&mut NetworkArgs::default(), Some("not-a-node"))
                .is_err()
        );
        // Dev with no override: reth's NetworkArgs untouched.
        let mut network = NetworkArgs::default();
        ChainProfile::Dev
            .apply_bootnodes(&mut network, None)
            .unwrap();
        assert_eq!(network, NetworkArgs::default());
    }

    #[test]
    fn dev_spec_is_reths_dev_unchanged() {
        let spec = dev_chain_spec();
        assert!(Arc::ptr_eq(&spec, &DEV));
        assert_eq!(spec.chain().id(), 1337);
        assert_eq!(spec.genesis.alloc.len(), 20);
        assert_eq!(spec.genesis_hash(), DEV_GENESIS_HASH);
    }

    #[test]
    fn profile_parsing() {
        assert_eq!(ChainProfile::parse(None).unwrap(), ChainProfile::Dev);
        assert_eq!(ChainProfile::parse(Some("dev")).unwrap(), ChainProfile::Dev);
        assert_eq!(
            ChainProfile::parse(Some("sova-testnet")).unwrap(),
            ChainProfile::SovaTestnet
        );
        assert!(ChainProfile::parse(Some("mainnet")).is_err());
        assert!(ChainProfile::parse(Some("")).is_err());
        assert!(Arc::ptr_eq(&ChainProfile::Dev.chain_spec(), &DEV));
    }

    /// SIP-7: the predeploy puts the ZcashBlocks runtime at 0x…5A01 with
    /// nonce 1 and no storage, and changes the genesis hash; nothing else.
    #[test]
    fn sip7_adds_the_zcash_blocks_predeploy() {
        for profile in [ChainProfile::Dev, ChainProfile::SovaTestnet] {
            let plain = profile.chain_spec_with(false);
            let sip7 = profile.chain_spec_with(true);
            let account = sip7
                .genesis
                .alloc
                .get(&ZCASH_BLOCKS_ADDRESS)
                .expect("predeployed");
            assert_eq!(account.nonce, Some(1));
            assert!(account.storage.is_none());
            let code = account.code.as_ref().expect("code");
            assert!(code.len() > 1_000 && code[0] == 0x60, "runtime bytecode");
            assert_eq!(sip7.genesis.alloc.len(), plain.genesis.alloc.len() + 1);
            assert_ne!(sip7.genesis_hash(), plain.genesis_hash());
            assert_eq!(sip7.chain, plain.chain);
        }
    }

    /// The `sova-testnet` genesis hash with SIP-7 on (`SOVA_SIP7=1`, the
    /// public testnet's setting): [`SOVA_TESTNET_GENESIS_HASH`]'s header
    /// plus the `ZcashBlocks` predeploy. Pinned like the plain one: a change
    /// to the predeploy's bytecode, nonce or address moves it, and must be
    /// a deliberate edit here (and a new announced genesis).
    #[test]
    fn testnet_sip7_genesis_hash_is_pinned() {
        let hash = ChainProfile::SovaTestnet.genesis_hash(true);
        assert_eq!(hash, SOVA_TESTNET_SIP7_GENESIS_HASH);
        assert_ne!(hash, SOVA_TESTNET_GENESIS_HASH);
        assert_eq!(
            ChainProfile::SovaTestnet.genesis_hash(false),
            SOVA_TESTNET_GENESIS_HASH
        );
        assert_eq!(ChainProfile::Dev.genesis_hash(false), DEV_GENESIS_HASH);
        // Same fork schedule, different genesis: a fork ID of its own, so a
        // node with SIP-7 off is filtered out at the ENR / Status check.
        assert_ne!(
            ChainProfile::SovaTestnet
                .chain_spec_with(true)
                .latest_fork_id(),
            sova_testnet_chain_spec().latest_fork_id()
        );
    }
}
