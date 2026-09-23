//! v2 producer: reth's `LocalMiner`, rebased onto the canonical chain.
//!
//! Stock `LocalMiner` (reth `crates/engine/local/src/miner.rs`) assumes
//! it is the chain's only producer: it remembers the blocks *it* built
//! (`last_block_hashes`) and re-asserts that lineage as forkchoice every
//! second. Under v2 preference that is exactly wrong — when the arbiter
//! ([`crate::candidates::run_arbiter`]) adopts a better-ranked imported
//! candidate, stock `LocalMiner` would immediately FCU the head back to
//! its own block and build the next epoch on the losing lineage.
//!
//! [`SovaMiner`] is the same loop with one principled difference: **the
//! canonical head always comes from the provider (the engine tree),
//! never from an internal ledger.** An arbiter adoption therefore
//! becomes the parent of the next build, and the periodic FCU re-asserts
//! whatever is canonical — agreeing with the arbiter instead of fighting
//! it. One v2 restraint is added on top: after building a block, we skip
//! promoting it to head if a strictly-preferred candidate for its height
//! is already known (the block is still inserted and relayed-about via
//! observation; preference just doesn't move backwards).
//!
//! Builds are **height-addressed**: the sealer sends the Sova height it
//! wants built, and the block is built on the canonical block at
//! `height − 1` — not on whatever the head happens to be when the trigger
//! is served. That is what makes a late better-ranked seal possible (a
//! sibling of the tip, built on the tip's parent) and what stops a stale
//! trigger from stacking a ghost block on top of an imported one.
//!
//! Finality is depth-lagged exactly like stock `LocalMiner` (64 blocks,
//! halved for safe) so recent tips stay reorgable — on Sova that depth
//! is an epoch-count lag, mirroring the reality that only Zcash depth
//! finalizes anything.

use std::time::Duration;

use alloy_rpc_types::engine::ForkchoiceState;
use eyre::OptionExt;
use reth_ethereum::{
    node::api::{
        BuiltPayload, ConsensusEngineHandle, PayloadAttributesBuilder, PayloadKind, PayloadTypes,
    },
    primitives::{AlloyBlockHeader, HeaderTy},
    storage::BlockReader,
};
use reth_payload_builder::PayloadBuilderHandle;

/// A build request from the sealer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildTarget {
    /// The Sova height to build (on the canonical block at `sova_height − 1`).
    pub sova_height: u64,
    /// A late win: deliberately build a sibling of the current tip. Only
    /// these may build at an already-covered height — an ordinary target
    /// that is covered by the time it is served (a retrigger racing a slow
    /// first build) is a no-op, never a second block.
    pub sibling: bool,
}

/// Blocks behind head marked finalized (and half of it marked safe) —
/// the same default depth as stock `LocalMiner`.
const FINALITY_DEPTH: u64 = 64;

/// A local block producer that treats the provider's canonical chain as
/// the only source of truth for what to build on and what to re-assert.
#[derive(Debug)]
pub struct SovaMiner<T: PayloadTypes, B, P> {
    provider: P,
    payload_attributes_builder: B,
    to_engine: ConsensusEngineHandle<T>,
    /// Sova heights to build, from the sealer.
    targets: tokio::sync::mpsc::Receiver<BuildTarget>,
    payload_builder: PayloadBuilderHandle<T>,
}

impl<T, B, P> SovaMiner<T, B, P>
where
    T: PayloadTypes,
    B: PayloadAttributesBuilder<
            T::PayloadAttributes,
            HeaderTy<<T::BuiltPayload as BuiltPayload>::Primitives>,
        >,
    P: BlockReader<Header = HeaderTy<<T::BuiltPayload as BuiltPayload>::Primitives>>,
{
    /// Same construction shape as `LocalMiner::new`, minus the internal
    /// block ledger it would have seeded.
    pub const fn new(
        provider: P,
        payload_attributes_builder: B,
        to_engine: ConsensusEngineHandle<T>,
        targets: tokio::sync::mpsc::Receiver<BuildTarget>,
        payload_builder: PayloadBuilderHandle<T>,
    ) -> Self {
        Self {
            provider,
            payload_attributes_builder,
            to_engine,
            targets,
            payload_builder,
        }
    }

    /// Runs the miner: build each requested height, re-assert canonical
    /// forkchoice once a second (stock `LocalMiner`'s cadence). Ends when
    /// the sealer drops its sender.
    pub async fn run(mut self) {
        let mut fcu_interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                target = self.targets.recv() => {
                    let Some(target) = target else {
                        tracing::warn!("sova miner: sealer gone; stopping");
                        return;
                    };
                    if let Err(err) = self.build_at(target).await {
                        tracing::warn!(target = target.sova_height, %err, "sova miner: build skipped");
                    }
                }
                _ = fcu_interval.tick() => {
                    if let Err(err) = self.reassert_canonical().await {
                        tracing::error!(%err, "sova miner: error re-asserting canonical forkchoice");
                    }
                }
            }
        }
    }

    /// Forkchoice with an explicit head (used right after building, when
    /// our new block isn't canonical *yet*); safe/finalized still come
    /// from the canonical chain, which holds the head's ancestors.
    fn forkchoice_for(
        &self,
        head_height: u64,
        head_hash: alloy_primitives::B256,
    ) -> eyre::Result<ForkchoiceState> {
        Ok(ForkchoiceState {
            head_block_hash: head_hash,
            safe_block_hash: self.canonical_hash(head_height.saturating_sub(FINALITY_DEPTH / 2))?,
            finalized_block_hash: self
                .canonical_hash(head_height.saturating_sub(FINALITY_DEPTH))?,
        })
    }

    fn canonical_hash(&self, height: u64) -> eyre::Result<alloy_primitives::B256> {
        self.provider
            .block_hash(height)?
            .ok_or_eyre("no canonical hash at height")
    }

    /// Re-assert forkchoice at the tip — toward the *preferred* candidate.
    ///
    /// Re-asserting the provider's canonical head raced the arbiter: a tick
    /// that read the head just before an arbiter adoption would land after
    /// it and put the losing block back (seen on CI: a late rank-0 win
    /// reverted 56 µs after it was committed). The tracker's best only ever
    /// improves, so pointing the head at it can never undo a preference —
    /// and a lost race self-heals on the next tick.
    async fn reassert_canonical(&self) -> eyre::Result<()> {
        let best = self.provider.best_block_number()?;
        let canonical = self.canonical_hash(best)?;
        let preferred = crate::candidates::global()
            .best(best)
            .map(|c| alloy_primitives::B256::from(c.block_hash));
        let head = reassert_head(canonical, preferred);
        if head != canonical {
            tracing::info!(height = best, %head, "re-assert: moving head to the preferred candidate");
        }
        let state = self.forkchoice_for(best, head)?;
        let res = self.to_engine.fork_choice_updated(state, None).await?;
        if !res.is_valid() {
            eyre::bail!("invalid canonical forkchoice update {state:?}: {res:?}");
        }
        Ok(())
    }

    /// One build round for Sova height `target`: attributes on the
    /// canonical block at `target − 1` (a sibling of the tip when `target`
    /// is already covered — a late better-ranked seal), resolve, insert via
    /// newPayload, then promote our block to head unless a
    /// strictly-preferred candidate for its height is already known.
    async fn build_at(&mut self, request: BuildTarget) -> eyre::Result<()> {
        let best = self.provider.best_block_number()?;
        let target = request.sova_height;
        eyre::ensure!(target >= 1, "cannot build genesis");
        if request.sibling {
            eyre::ensure!(
                target == best,
                "late-win height {target} is no longer the tip ({best})"
            );
        } else if target <= best {
            // Built already — unless the canonical block there is anchored to
            // a Zcash block that reorged away (SIP-4 §7): then re-seal it on
            // its canonical parent; adopting ours reorgs the stale tail out.
            let anchor = self
                .provider
                .sealed_header(target)?
                .and_then(|h| h.parent_beacon_block_root())
                .map(|r| r.0);
            if !crate::expectations::global().is_stale(target, anchor) {
                tracing::debug!(
                    target,
                    best,
                    "sova miner: height already built; nothing to do"
                );
                return Ok(());
            }
            tracing::warn!(
                target,
                best,
                "sova miner: canonical block anchored to an orphaned zcash block; re-sealing"
            );
        } else {
            eyre::ensure!(
                target == best.saturating_add(1),
                "parent {} not canonical yet (head {best})",
                target - 1
            );
        }
        let parent_height = target - 1;
        let parent = self
            .provider
            .sealed_header(parent_height)?
            .ok_or_eyre("no canonical header at the target's parent height")?;

        // Head = the parent: reth builds on a canonical ancestor without
        // touching the canonical chain (engine tree `apply_chain_update`);
        // safe/finalized lag from the parent so they are its ancestors.
        let res = self
            .to_engine
            .fork_choice_updated(
                self.forkchoice_for(parent_height, parent.hash())?,
                Some(self.payload_attributes_builder.build(&parent)),
            )
            .await?;
        if !res.is_valid() {
            eyre::bail!("invalid payload-building forkchoice update: {res:?}");
        }
        let payload_id = res.payload_id.ok_or_eyre("no payload id")?;

        let Some(Ok(payload)) = self
            .payload_builder
            .resolve_kind(payload_id, PayloadKind::WaitForPending)
            .await
        else {
            eyre::bail!("no payload");
        };

        let header = payload.block().sealed_header().clone();
        let res = self.to_engine.new_payload(payload.into()).await?;
        if !res.is_valid() {
            eyre::bail!("built payload rejected: {res:?}");
        }

        let ours = header.hash();
        let height = header.number();
        match crate::candidates::global().best(height) {
            Some(preferred) if preferred.block_hash != ours.0 => {
                tracing::debug!(
                    height,
                    "built block loses preference to an already-known candidate; not promoting"
                );
            }
            _ => {
                let state = self.forkchoice_for(height, ours)?;
                let res = self.to_engine.fork_choice_updated(state, None).await?;
                if !res.is_valid() {
                    eyre::bail!("invalid own-block forkchoice update: {res:?}");
                }
            }
        }
        Ok(())
    }
}

/// The head a periodic re-assert should name: the tracker's preferred
/// candidate at the tip height when it knows one, else the canonical head.
fn reassert_head(
    canonical: alloy_primitives::B256,
    preferred: Option<alloy_primitives::B256>,
) -> alloy_primitives::B256 {
    preferred.unwrap_or(canonical)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;

    use super::reassert_head;

    #[test]
    fn reassert_names_the_preferred_block_not_a_stale_canonical_read() {
        let loser = B256::repeat_byte(0x6f);
        let winner = B256::repeat_byte(0x61);
        // Canonical read raced an adoption: the preferred block wins.
        assert_eq!(reassert_head(loser, Some(winner)), winner);
        // Nothing tracked (e.g. just restarted): keep the canonical head.
        assert_eq!(reassert_head(loser, None), loser);
        // Agreement is a no-op.
        assert_eq!(reassert_head(winner, Some(winner)), winner);
    }
}
