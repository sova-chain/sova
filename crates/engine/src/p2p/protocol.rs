//! reth plumbing for `sova/1`: the [`ProtocolHandler`] registered with the
//! network after launch, and the per-connection stream that bridges one
//! RLPx sub-protocol connection to the gossip service.
//!
//! Each connection is deliberately dumb: it decodes inbound frames and
//! forwards them to the service, and writes whatever the service queues for
//! it. All policy (dedup, fetch, submission, reputation) lives in
//! [`super::service`].
//!
//! Inbound limits, in layers:
//! - reth enforces [`ProtocolIngressLimits`] per connection before a frame
//!   reaches us: frames above [`MAX_FRAME_BYTES`] and more than
//!   [`MAX_BUFFERED_MESSAGES`] / [`MAX_BUFFERED_BYTES`] of unpolled
//!   backlog are refused by the multiplexer.
//! - A frame that fails [`SovaMessage::decode`] closes the connection
//!   (no reputation change — see the service docs for why only an
//!   `INVALID` block costs reputation).
//! - Decoded messages go to the service over a bounded channel with
//!   `try_send`: if the service is saturated, the message is dropped, not
//!   buffered without bound.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use alloy_primitives::bytes::BytesMut;
use futures::{Stream, StreamExt};
use reth_ethereum::network::{
    api::{Direction, PeerId},
    eth_wire::{
        capability::SharedCapabilities,
        multiplex::ProtocolConnection,
        protocol::{Protocol, ProtocolIngressLimits},
    },
    protocol::{ConnectionHandler, OnNotSupported, ProtocolHandler},
};
use tokio::sync::mpsc;

use super::codec::{MAX_FRAME_BYTES, SovaMessage, protocol};

/// Max unpolled inbound frames reth buffers per connection for us.
pub const MAX_BUFFERED_MESSAGES: usize = 256;
/// Max unpolled inbound bytes reth buffers per connection for us.
pub const MAX_BUFFERED_BYTES: usize = 2 * MAX_FRAME_BYTES;
/// Outbound queue depth per connection (service → peer).
pub const OUTBOUND_QUEUE: usize = 256;
/// Inbound message queue depth (all connections → service).
pub const INBOUND_QUEUE: usize = 1024;

/// Identifies one connection (a peer may reconnect; events from a stale
/// connection must not tear down its successor).
pub type ConnectionId = u64;

/// What connections tell the gossip service.
#[derive(Debug)]
pub enum PeerEvent {
    /// A `sova/1` connection was established.
    Connected {
        /// The remote peer.
        peer_id: PeerId,
        /// This connection's id.
        conn_id: ConnectionId,
        /// Queue for messages to send to this peer.
        outbound: mpsc::Sender<SovaMessage>,
    },
    /// A connection closed (either side, or a decode failure).
    Disconnected {
        /// The remote peer.
        peer_id: PeerId,
        /// The connection that closed.
        conn_id: ConnectionId,
    },
    /// A decoded inbound message.
    Message {
        /// The sender.
        peer_id: PeerId,
        /// The message.
        msg: SovaMessage,
    },
}

/// Channels from connections to the service: lifecycle events are
/// unbounded (at most two per connection, and losing one would orphan a
/// peer's queue), messages are bounded.
#[derive(Debug, Clone)]
pub struct ServiceChannels {
    lifecycle: mpsc::UnboundedSender<PeerEvent>,
    messages: mpsc::Sender<PeerEvent>,
    next_conn_id: Arc<AtomicU64>,
}

/// Receiving ends of [`ServiceChannels`], owned by the service.
#[derive(Debug)]
pub struct ServiceReceivers {
    /// Connect/disconnect events.
    pub lifecycle: mpsc::UnboundedReceiver<PeerEvent>,
    /// Decoded messages.
    pub messages: mpsc::Receiver<PeerEvent>,
}

/// Creates the connection→service channels.
#[must_use]
pub fn service_channels() -> (ServiceChannels, ServiceReceivers) {
    let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
    let (messages_tx, messages_rx) = mpsc::channel(INBOUND_QUEUE);
    (
        ServiceChannels {
            lifecycle: lifecycle_tx,
            messages: messages_tx,
            next_conn_id: Arc::new(AtomicU64::new(0)),
        },
        ServiceReceivers {
            lifecycle: lifecycle_rx,
            messages: messages_rx,
        },
    )
}

/// The `sova/1` protocol handler: offered on every incoming and outgoing
/// RLPx connection once registered via `add_rlpx_sub_protocol`.
#[derive(Debug, Clone)]
pub struct SovaProtocolHandler {
    channels: ServiceChannels,
}

impl SovaProtocolHandler {
    /// A handler feeding the given service channels.
    #[must_use]
    pub const fn new(channels: ServiceChannels) -> Self {
        Self { channels }
    }
}

impl ProtocolHandler for SovaProtocolHandler {
    type ConnectionHandler = SovaConnectionHandler;

    fn on_incoming(&self, _socket_addr: std::net::SocketAddr) -> Option<Self::ConnectionHandler> {
        Some(SovaConnectionHandler {
            channels: self.channels.clone(),
        })
    }

    fn on_outgoing(
        &self,
        _socket_addr: std::net::SocketAddr,
        _peer_id: PeerId,
    ) -> Option<Self::ConnectionHandler> {
        Some(SovaConnectionHandler {
            channels: self.channels.clone(),
        })
    }
}

/// Negotiates `sova/1` on one RLPx connection.
#[derive(Debug)]
pub struct SovaConnectionHandler {
    channels: ServiceChannels,
}

impl ConnectionHandler for SovaConnectionHandler {
    type Connection = SovaConnection;

    fn protocol(&self) -> Protocol {
        protocol()
    }

    fn inbound_limits(&self) -> ProtocolIngressLimits {
        ProtocolIngressLimits::new(MAX_FRAME_BYTES)
            .with_max_buffered_bytes(MAX_BUFFERED_BYTES)
            .with_max_buffered_messages(MAX_BUFFERED_MESSAGES)
    }

    fn on_unsupported_by_peer(
        self,
        _supported: &SharedCapabilities,
        _direction: Direction,
        peer_id: PeerId,
    ) -> OnNotSupported {
        // Keep the session: the eth protocol still serves headers/bodies
        // (reth's own catch-up path), and discovery-era peers may run a
        // client without sova/1.
        tracing::debug!(%peer_id, "sova/1: peer does not support the protocol");
        OnNotSupported::KeepAlive
    }

    fn into_connection(
        self,
        direction: Direction,
        peer_id: PeerId,
        conn: ProtocolConnection,
    ) -> Self::Connection {
        let conn_id = self.channels.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE);
        tracing::info!(%peer_id, ?direction, conn_id, "sova/1: connection established");
        let _ = self.channels.lifecycle.send(PeerEvent::Connected {
            peer_id,
            conn_id,
            outbound: outbound_tx,
        });
        SovaConnection {
            conn,
            outbound: outbound_rx,
            channels: self.channels,
            peer_id,
            conn_id,
        }
    }
}

/// One live `sova/1` connection, polled by reth's session task: yields the
/// frames to send, and forwards decoded inbound frames to the service.
/// Resolving (`None`) closes the connection.
#[derive(Debug)]
pub struct SovaConnection {
    conn: ProtocolConnection,
    outbound: mpsc::Receiver<SovaMessage>,
    channels: ServiceChannels,
    peer_id: PeerId,
    conn_id: ConnectionId,
}

impl Stream for SovaConnection {
    type Item = BytesMut;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.outbound.poll_recv(cx) {
                Poll::Ready(Some(msg)) => return Poll::Ready(Some(msg.encode())),
                // The service dropped our queue (shutdown / replaced).
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => {}
            }

            let Some(frame) = std::task::ready!(this.conn.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };
            match SovaMessage::decode(&frame) {
                Ok(msg) => {
                    let event = PeerEvent::Message {
                        peer_id: this.peer_id,
                        msg,
                    };
                    if this.channels.messages.try_send(event).is_err() {
                        tracing::debug!(
                            peer_id = %this.peer_id,
                            "sova/1: service saturated; inbound message dropped"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        peer_id = %this.peer_id,
                        %err,
                        "sova/1: malformed frame; closing connection"
                    );
                    return Poll::Ready(None);
                }
            }
        }
    }
}

impl Drop for SovaConnection {
    fn drop(&mut self) {
        let _ = self.channels.lifecycle.send(PeerEvent::Disconnected {
            peer_id: self.peer_id,
            conn_id: self.conn_id,
        });
    }
}
