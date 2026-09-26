//! Drops one reth warning that never applies to Sova.
//!
//! reth checks every 5 minutes that a consensus-layer (beacon) client has
//! sent it a forkchoice update, and warns "Post-merge network, but never
//! seen beacon client. Please launch one to follow the chain!" when none
//! ever has (`reth_node_events::node`, `ConsensusLayerHealthEvent::
//! NeverSeen`). A Sova node has no beacon client by design: blocks come
//! from `sova/1` and the sealer, checked against zebrad. On a node that
//! hasn't received a block yet (a new follow-only node without peers), the
//! warning reads as if the node were missing a program, so strangers go
//! looking for one (stranger test 2026-09-25, F11).
//!
//! reth has no switch for it short of dev mode or `--debug.tip`, and a
//! target-level directive would also hide the module's other warnings
//! ("Encountered invalid block", …). So this layer vetoes exactly that
//! event: level WARN, target `reth_node_events::node`, message containing
//! "never seen beacon client". Every other event passes untouched,
//! including reth's "Beacon client online, but no consensus updates
//! received for a while" (which on Sova means blocks stopped arriving).
//!
//! The layer sits behind its own per-layer filter that selects only WARN
//! events from that target ([`layer`]). That matters: `RethTracer`'s output
//! layer uses a per-layer filter, so the registry creates a span or event
//! only if some layer's filter wants it. A bare layer here would "want"
//! everything, so every debug/trace span in reth would be created (costly),
//! and one of them (in reth's state-overlay thread) is then cloned after it
//! closed, which panics the thread on every block (seen in dev mode).

use std::fmt;

use reth_tracing::{
    tracing::{
        Event, Level, Metadata, Subscriber,
        field::{Field, Visit},
    },
    tracing_subscriber::{
        Registry,
        filter::filter_fn,
        layer::{Context, Layer},
    },
};

/// The layer to add to `RethTracer`'s layers: [`QuietBeaconWarning`] behind
/// a per-layer filter that only ever selects WARN events from [`TARGET`], so
/// it never enables a span or event that the output layer's filter wouldn't.
pub(crate) fn layer() -> impl Layer<Registry> + Send + Sync {
    QuietBeaconWarning.with_filter(filter_fn(selects))
}

/// The per-layer filter: the callsites [`QuietBeaconWarning`] looks at.
fn selects(meta: &Metadata<'_>) -> bool {
    meta.is_event() && *meta.level() == Level::WARN && meta.target() == TARGET
}

/// The module reth's node-event warnings come from.
const TARGET: &str = "reth_node_events::node";
/// The part of the message that identifies the beacon-client warning.
const NEVER_SEEN: &str = "never seen beacon client";

/// Vetoes reth's "never seen beacon client" warning for every output layer.
/// Use it through [`layer`], never bare (see the module doc).
pub(crate) struct QuietBeaconWarning;

impl<S: Subscriber> Layer<S> for QuietBeaconWarning {
    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        let meta = event.metadata();
        if *meta.level() != Level::WARN || meta.target() != TARGET {
            return true;
        }
        let mut found = MessageContains(false);
        event.record(&mut found);
        !found.0
    }
}

/// Visits an event's `message` field and records whether it names the
/// beacon-client warning.
struct MessageContains(bool);

impl Visit for MessageContains {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" && value.contains(NEVER_SEEN) {
            self.0 = true;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && format!("{value:?}").contains(NEVER_SEEN) {
            self.0 = true;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use reth_tracing::{
        tracing::{self, level_filters::LevelFilter},
        tracing_subscriber::{self, layer::SubscriberExt},
    };
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The same shape as `RethTracer` builds: a registry with a vector of
    /// layers, the output layer carrying a per-layer level filter.
    #[test]
    fn drops_only_the_beacon_warning() {
        let buf = Buf::default();
        let writer = buf.clone();
        let fmt = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .with_filter(LevelFilter::INFO);
        let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> =
            vec![Box::new(layer()), Box::new(fmt)];
        let subscriber = tracing_subscriber::registry().with(layers);
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..3 {
                tracing::warn!(
                    target: "reth_node_events::node",
                    "Post-merge network, but never seen beacon client. Please launch one to follow the chain!"
                );
            }
            tracing::warn!(target: "reth_node_events::node", number = 7, "Encountered invalid block");
            tracing::warn!(
                target: "reth_node_events::node",
                "Beacon client online, but no consensus updates received for a while."
            );
            // Same words from another target, and at another level: kept.
            tracing::warn!(target: "sova::other", "never seen beacon client");
            tracing::info!(target: "reth_node_events::node", "never seen beacon client (info)");
            tracing::info!(target: "reth::cli", "Status connected_peers=0");
            // The layer enables nothing the output filter doesn't: no
            // debug/trace spans get created. (A bare, unfiltered layer
            // did, and one of reth's then panicked a worker thread on
            // every block.) Checked last: `enabled!` leaves per-layer
            // filter state behind for the next event on this thread.
            assert!(tracing::span!(Level::DEBUG, "state_overlay").is_disabled());
            assert!(
                tracing::span!(target: "reth_node_events::node", Level::TRACE, "x").is_disabled()
            );
            assert!(!tracing::enabled!(target: "engine::tree", Level::TRACE));
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("Please launch one"), "{out}");
        for kept in [
            "Encountered invalid block",
            "no consensus updates received",
            "sova::other",
            "never seen beacon client (info)",
            "connected_peers=0",
        ] {
            assert!(out.contains(kept), "missing {kept:?} in:\n{out}");
        }
    }
}
