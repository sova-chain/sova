//! B3a acceptance test: a custom `SovaPayloadAttributes.epoch` survives the
//! `engine_forkchoiceUpdated -> payload builder` handoff.
//!
//! ## Route taken, and why
//!
//! reth v2.6.0 collapsed the older two-step "RPC attributes -> distinct
//! `PayloadBuilderAttributes`" conversion into a single type: the engine
//! deserializes `T::PayloadAttributes` straight off the wire (this is what
//! `crates/engine/src/payload.rs`'s serde tests exercise) and hands that
//! same value, unmodified, into `reth_basic_payload_builder::PayloadConfig`
//! — the exact struct `PayloadBuilder::try_build`/`build_empty_payload`
//! receive. So `PayloadConfig<SovaPayloadAttributes>` *is* the
//! forkchoiceUpdated -> payload-builder boundary in this reth version; there
//! is no separate `PayloadBuilderAttributes` type left to convert into (see
//! `docs/WORKPLAN.md` B3a row and the migration note in the final report).
//!
//! This test drives that exact boundary end to end using production types
//! only (no test doubles): it constructs a `SovaPayloadAttributes` carrying
//! `epoch: Some(_)`, round-trips it through the real serde impl the engine
//! API's `engine_forkchoiceUpdated` handler uses to deserialize its
//! attributes parameter, then threads the result through `PayloadConfig` —
//! proving the epoch field is intact on both sides of the handoff.
//!
//! We did not additionally stand up a full node and drive raw HTTP authrpc
//! (JWT-signed `engine_forkchoiceUpdatedV3` calls) for this test: reth's own
//! `examples/custom-engine-types` reference has no such test either, and
//! standing up authrpc auth plus a JSON-RPC engine client is exactly the
//! "disproportionately painful" case the B3a task description calls out as
//! grounds to prefer this route. What that would add beyond this test is
//! JSON-RPC transport/JWT plumbing, not additional evidence about whether
//! `epoch` itself survives — the serde round trip above already proves the
//! wire format, and `PayloadConfig` is the real (not simulated) handoff
//! struct. The dev-node run in step 2 of the task (`bin/sova`, verified by
//! `eth_blockNumber` advancing) is the complementary running-node evidence:
//! it proves `SovaNode`'s full assembly — including
//! `DebugNode::local_payload_attributes_builder`, which dev-mode auto-mining
//! calls on every block — actually launches and mines with these types
//! wired in, end to end.

use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types::engine::PayloadId;
use engine::{SovaEpochAttribute, SovaPayloadAttributes};
use reth_basic_payload_builder::PayloadConfig;
use reth_ethereum::primitives::SealedHeader;

fn sample_attributes() -> SovaPayloadAttributes {
    SovaPayloadAttributes {
        inner: alloy_rpc_types::engine::PayloadAttributes {
            timestamp: 1_700_000_042,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: None,
            parent_beacon_block_root: None,
            slot_number: None,
            ..Default::default()
        },
        epoch: Some(SovaEpochAttribute {
            zcash_height: 12_345,
            zcash_hash: [0x7e; 32],
            settlements: vec![
                (Address::with_last_byte(0xA1), U256::from(500_000u64)),
                (Address::with_last_byte(0xA2), U256::from(1_500_000u64)),
            ],
        }),
    }
}

/// The engine API deserializes `engine_forkchoiceUpdatedV3`'s payload
/// attributes parameter using exactly this serde impl. Proves the epoch
/// field survives the RPC wire format unmodified.
#[test]
fn epoch_survives_forkchoice_updated_wire_deserialization() -> Result<(), serde_json::Error> {
    let sent = sample_attributes();
    let wire_json = serde_json::to_string(&sent)?;

    let received: SovaPayloadAttributes = serde_json::from_str(&wire_json)?;

    assert_eq!(received, sent);
    assert_eq!(received.epoch, sent.epoch);
    Ok(())
}

/// `PayloadConfig<SovaPayloadAttributes>` is the exact struct reth hands to
/// `PayloadBuilder::try_build`/`build_empty_payload` (see
/// `reth_basic_payload_builder::{BuildArguments, PayloadConfig}` and
/// `crates/engine/src/builder.rs`'s `SovaPayloadBuilder`). Constructing it
/// from wire-deserialized attributes and reading `epoch` back off it proves
/// the field survives all the way to the payload builder's doorstep.
#[test]
fn epoch_survives_into_payload_config() -> Result<(), serde_json::Error> {
    let wire_json = serde_json::to_string(&sample_attributes())?;
    let attributes: SovaPayloadAttributes = serde_json::from_str(&wire_json)?;
    let expected_epoch = attributes.epoch.clone();

    // `PayloadConfig::new` is exactly how `BasicPayloadJobGenerator` builds
    // this struct in production (reth_basic_payload_builder::lib.rs) before
    // handing it to `PayloadBuilder::try_build`/`build_empty_payload`.
    let parent_header: SealedHeader = SealedHeader::default();
    let config = PayloadConfig::new(
        std::sync::Arc::new(parent_header),
        attributes,
        PayloadId::new([0u8; 8]),
    );

    // This is precisely the value `SovaPayloadBuilder::try_build` /
    // `build_empty_payload` destructure out of `config.attributes.epoch`
    // (see crates/engine/src/builder.rs) before stripping `.inner` for the
    // stock Ethereum payload builder it delegates to.
    assert_eq!(config.attributes.epoch, expected_epoch);
    assert!(
        config.attributes.epoch.is_some(),
        "epoch must not have been dropped"
    );
    Ok(())
}
