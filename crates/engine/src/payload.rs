//! Sova's custom payload attributes: [`EthPayloadAttributes`] plus the
//! optional Zcash epoch data a sealer's `engine_forkchoiceUpdated` call
//! carries for the next block.
//!
//! This is the seam B3b (settlement mint) fills in: once a `SovaEpochAttribute`
//! reaches the payload builder / block executor, its `settlements` become the
//! synthetic mint credits applied at block top. B3a only proves the field
//! survives the round trip from RPC deserialization through to the payload
//! builder job — see `crates/engine/tests/payload_roundtrip.rs`.

use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types::{
    Withdrawal,
    engine::{PayloadAttributes as EthPayloadAttributes, PayloadId},
};
use reth_ethereum::node::api::PayloadAttributes;
use serde::{Deserialize, Serialize};

/// Sova's custom payload attributes type: the standard Ethereum payload
/// attributes plus an optional Zcash epoch settlement to apply to the block
/// being built.
///
/// `epoch: None` is vanilla behavior — the node builds a normal Ethereum
/// payload, identical to today. `epoch: Some(_)` is how a sealer tells the
/// payload builder "this block also settles this Zcash epoch's burns."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SovaPayloadAttributes {
    /// The standard Ethereum payload attributes (timestamp, prev_randao,
    /// suggested fee recipient, withdrawals, beacon root, ...).
    #[serde(flatten)]
    pub inner: EthPayloadAttributes,
    /// The Zcash epoch this payload settles, if any. `None` preserves
    /// vanilla (non-Sova) engine behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<SovaEpochAttribute>,
}

/// A recognized Zcash epoch's settlement data, attached to a payload build
/// request so the block being built can mint the epoch's burn rewards.
///
/// `settlements` is the exact seam B3b's mint logic reads: each entry credits
/// `wei` to `address` as a synthetic settlement at block top.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SovaEpochAttribute {
    /// The Zcash block height at which this epoch closed.
    pub zcash_height: u64,
    /// The Zcash block hash at which this epoch closed, encoded as `0x`-hex.
    #[serde(with = "zcash_hash_hex")]
    pub zcash_hash: [u8; 32],
    /// The credited address and wei amount for each recognized burn in this
    /// epoch, in the order `epoch_rewards` produced them.
    pub settlements: Vec<(Address, U256)>,
    /// The Zcash block's header time (unix seconds): SIP-6 pins a null
    /// block's timestamp to it and bounds a sealed block's. 0 = unknown.
    #[serde(default)]
    pub zcash_time: u64,
    /// SIP-6: build this epoch's **null block** — no transactions, no mint,
    /// zero beneficiary, empty extra data, unsigned.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub null: bool,
}

/// Serializes/deserializes a raw 32-byte hash as a `0x`-prefixed hex string
/// (matching how the rest of the engine API encodes hashes), rather than
/// serde's default JSON array-of-numbers representation for `[u8; 32]`.
mod zcash_hash_hex {
    use alloy_primitives::B256;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        bytes: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        B256::from(*bytes).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        B256::deserialize(deserializer).map(|hash| hash.0)
    }
}

/// Errors mapping an epoch's settlements to block withdrawals.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettlementMapError {
    /// A settlement amount is not gwei-aligned (withdrawals are u64 gwei).
    #[error("settlement for {address} is not gwei-aligned: {wei} wei")]
    NotGweiAligned {
        /// Credited address.
        address: Address,
        /// Offending amount in wei.
        wei: U256,
    },
    /// A settlement amount does not fit a u64 gwei withdrawal.
    #[error("settlement for {address} overflows u64 gwei: {wei} wei")]
    Overflow {
        /// Credited address.
        address: Address,
        /// Offending amount in wei.
        wei: U256,
    },
    /// A settlement amount is zero, which mints nothing and is not allowed.
    #[error("settlement for {address} is zero")]
    Zero {
        /// Credited address.
        address: Address,
    },
}

/// One gwei in wei.
const GWEI: u64 = 1_000_000_000;

/// Reserved `validator_index` marking a withdrawal as a Sova epoch
/// settlement (Sova has no beacon validators; the field is repurposed).
pub const SETTLEMENT_VALIDATOR_INDEX: u64 = 0;

/// Deterministically map an epoch's settlements to the block's withdrawals.
///
/// This is SIP-2's core rule: a block settling epoch `e` must carry exactly
/// these withdrawals, in this order — `index` is the position within the
/// block, `validator_index` is [`SETTLEMENT_VALIDATOR_INDEX`], `amount` is
/// the settlement in gwei. Ethereum's withdrawal processing (inherited
/// unchanged from reth) then credits the balances; the mint needs no custom
/// execution code. Amounts must be gwei-aligned, non-zero, and fit u64 gwei
/// — the reward schedule is denominated to guarantee this.
pub fn settlements_to_withdrawals(
    epoch: &SovaEpochAttribute,
) -> Result<Vec<Withdrawal>, SettlementMapError> {
    let mut out = Vec::with_capacity(epoch.settlements.len());
    for (i, &(address, wei)) in epoch.settlements.iter().enumerate() {
        if wei.is_zero() {
            return Err(SettlementMapError::Zero { address });
        }
        let gwei_u256 = wei / U256::from(GWEI);
        if gwei_u256 * U256::from(GWEI) != wei {
            return Err(SettlementMapError::NotGweiAligned { address, wei });
        }
        let amount: u64 = gwei_u256
            .try_into()
            .map_err(|_| SettlementMapError::Overflow { address, wei })?;
        out.push(Withdrawal {
            index: i as u64,
            validator_index: SETTLEMENT_VALIDATOR_INDEX,
            address,
            amount,
        });
    }
    Ok(out)
}

impl PayloadAttributes for SovaPayloadAttributes {
    fn payload_id(&self, parent_hash: &B256) -> PayloadId {
        self.inner.payload_id(parent_hash)
    }

    fn timestamp(&self) -> u64 {
        self.inner.timestamp()
    }

    fn withdrawals(&self) -> Option<&Vec<Withdrawal>> {
        self.inner.withdrawals()
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.inner.parent_beacon_block_root()
    }

    fn slot_number(&self) -> Option<u64> {
        self.inner.slot_number()
    }

    fn target_gas_limit(&self) -> Option<u64> {
        self.inner.target_gas_limit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_epoch() -> SovaEpochAttribute {
        SovaEpochAttribute {
            zcash_height: 42,
            zcash_hash: [0xab; 32],
            settlements: vec![
                (Address::with_last_byte(1), U256::from(100_000u64)),
                (Address::with_last_byte(2), U256::from(250_000u64)),
            ],
            zcash_time: 0,
            null: false,
        }
    }

    fn sample_inner() -> EthPayloadAttributes {
        EthPayloadAttributes {
            timestamp: 1_700_000_000,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: None,
            parent_beacon_block_root: None,
            slot_number: None,
            ..Default::default()
        }
    }

    /// `epoch: None` round-trips through the exact serde impl the engine API
    /// uses to deserialize `engine_forkchoiceUpdated`'s payload attributes
    /// parameter — this is the "vanilla behavior" AC.
    #[test]
    fn round_trips_with_no_epoch() -> Result<(), serde_json::Error> {
        let attrs = SovaPayloadAttributes {
            inner: sample_inner(),
            epoch: None,
        };
        let json = serde_json::to_string(&attrs)?;
        assert!(
            !json.contains("\"epoch\""),
            "epoch must be omitted, not null, when absent"
        );
        let round_tripped: SovaPayloadAttributes = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, attrs);
        assert_eq!(round_tripped.epoch, None);
        Ok(())
    }

    /// A well-formed `epoch: Some(_)` round-trips too, including the 0x-hex
    /// encoding of `zcash_hash` and the `(Address, U256)` settlement pairs.
    #[test]
    fn round_trips_with_epoch() -> Result<(), serde_json::Error> {
        let attrs = SovaPayloadAttributes {
            inner: sample_inner(),
            epoch: Some(sample_epoch()),
        };
        let json = serde_json::to_string(&attrs)?;
        let round_tripped: SovaPayloadAttributes = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, attrs);
        assert_eq!(round_tripped.epoch, Some(sample_epoch()));
        Ok(())
    }

    /// `zcash_hash` is carried as `0x`-hex on the wire, not a JSON number
    /// array — this is what the field docs promise.
    #[test]
    fn zcash_hash_is_hex_encoded_on_the_wire() -> Result<(), serde_json::Error> {
        let epoch = sample_epoch();
        let value = serde_json::to_value(&epoch)?;
        let hash_field = value.get("zcashHash");
        match hash_field {
            Some(serde_json::Value::String(s)) => assert!(s.starts_with("0x")),
            other => panic!("expected zcash_hash to serialize as a 0x-hex string, got {other:?}"),
        }
        Ok(())
    }
}
