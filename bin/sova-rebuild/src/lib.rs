//! `sova-rebuild`: rebuild a Sova node's full history from a block archive
//! (the NEAR data-availability batches, or files in the same format), with
//! no Sova peers, every block verified by the node's own consensus against
//! its own zebrad.
//!
//! - [`batch`]: the batch format (v1), read and write.
//! - [`archive`]: batches as one ordered block sequence, and the
//!   structural checks (`--verify-only`).
//! - [`rebuild`]: feed a fresh node through its authrpc, block by block.
//! - [`export`]: a node's blocks into batch files (sims, tests).
//! - [`rpc`]: the JSON-RPC clients.

pub mod archive;
pub mod batch;
pub mod export;
pub mod rebuild;
pub mod rpc;
