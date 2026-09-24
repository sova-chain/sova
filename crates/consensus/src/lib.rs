//! Burn-to-mine consensus client: Zcash follower, burn parser, sealer, gossip.
//!
//! This crate hosts the components that watch the Zcash chain for burn
//! transactions, parse and validate them, seal Sova blocks, and gossip them
//! across the network.
//!
//! Layout (growing as workstream C lands):
//! - [`sip1`] — the SIP-1 burn transaction format: payload codec, eater
//!   script, and the total, deterministic burn-recognition rule.
//! - [`epoch`] — per-address aggregation, sealer ranking (weight desc,
//!   min-txid asc; timing is liveness-only), and exact-conservation
//!   reward shares in settlement order.
//! - [`follower`] — the reorg-aware epoch scanner over any [`follower::ZcashView`].
//! - [`zebrad`] — the production `ZcashView` over zebrad JSON-RPC.
//! - [`sealer`] — the pure production-eligibility ladder and the
//!   rank-then-hash candidate preference (timing is liveness-only).

pub mod epoch;
pub mod follower;
pub mod pools;
pub mod schedule;
pub mod sealer;
pub mod sip1;
pub mod zebrad;
