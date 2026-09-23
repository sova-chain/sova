//! Sova's custom Engine API payload/node types.
//!
//! This crate is the B3a scaffold: it defines [`SovaPayloadAttributes`] (the
//! standard Ethereum payload attributes plus an optional Zcash epoch
//! settlement) and wires them through reth's Engine API type system —
//! [`SovaEngineTypes`] (`PayloadTypes`/`EngineTypes`), [`SovaEngineValidator`]
//! (`PayloadValidator`/`EngineApiValidator`), [`SovaPayloadBuilder`] (the
//! payload-building seam), and [`SovaNode`] (the `NodeTypes`/`Node` preset
//! that assembles all of the above plus [`evm::SovaExecutorBuilder`]) —
//! following reth's `examples/custom-engine-types` at tag v2.6.0.
//!
//! It is a separate crate from `evm` (rather than folding this into
//! `crates/evm`) because it is solving a different problem: `evm` is Sova's
//! EVM *execution* extensions (settlement transactions, the Zcash query
//! precompile — inside a block); this crate is Sova's Engine API / node
//! *assembly* surface (payload attributes, validators, the node preset —
//! how a block gets requested and built in the first place). `crates/engine`
//! depends on `evm` for the executor seam, not the other way around, and
//! `bin/sova` only needs to depend on `crates/engine` (which re-exports
//! [`evm::SovaExecutorBuilder`]'s wiring point).
//!
//! No mint logic lives here — `epoch` only needs to survive the round trip
//! from `engine_forkchoiceUpdated` through to the payload builder for B3a;
//! B3b (settlement mint) is where the executor actually reads
//! `epoch.settlements` and credits balances.

mod builder;
mod consensus;
mod local;
mod node;
mod payload;
mod types;
mod validator;

pub mod candidates;
pub mod driver;
pub mod expectations;
pub mod miner;
pub mod p2p;
pub mod relay;
pub mod seal;
pub mod signer;
pub mod zcash_index;

pub use builder::{SovaPayloadBuilder, SovaPayloadBuilderBuilder};
pub use consensus::{SettlementError, SovaConsensus, SovaConsensusBuilder};
pub use local::{PendingEpoch, SovaLocalPayloadAttributesBuilder};
pub use node::SovaNode;
pub use payload::{
    SETTLEMENT_VALIDATOR_INDEX, SettlementMapError, SovaEpochAttribute, SovaPayloadAttributes,
    settlements_to_withdrawals,
};
pub use types::SovaEngineTypes;
pub use validator::{SovaEngineValidator, SovaEngineValidatorBuilder, SovaNodeAddOns};
