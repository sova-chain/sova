//! `sova/1` wire messages and their codec.
//!
//! Every frame is `[message id: u8] ++ rlp(payload)`, the same shape as the
//! eth wire protocol (the id is capability-local; RLPx multiplexing adds
//! the capability offset). Three messages, none of which can move a head:
//!
//! | id   | message                  | payload                          |
//! |------|--------------------------|----------------------------------|
//! | 0x00 | [`SovaMessage::Announce`] | `rlp([height, hash])`            |
//! | 0x01 | [`SovaMessage::GetBlock`] | `rlp([hash])`                    |
//! | 0x02 | [`SovaMessage::Block`]    | `rlp(block)` (the block's own RLP) |
//!
//! Decoding is strict: unknown ids, trailing bytes, and oversized frames
//! are errors (the connection is closed on any of them — see
//! [`super::protocol`]). The block payload is carried as its raw RLP and
//! only decoded into a block by the service, after the frame has passed the
//! size bound here.

use alloy_primitives::{B256, Bytes};
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use reth_ethereum::network::eth_wire::{Capability, protocol::Protocol};

/// Capability name negotiated in the RLPx hello.
pub const CAPABILITY_NAME: &str = "sova";
/// Capability version.
pub const CAPABILITY_VERSION: usize = 1;
/// Number of message ids `sova/1` reserves.
pub const MESSAGE_COUNT: u8 = 3;

/// Largest block RLP a peer may send us (and that we will serve).
///
/// Sova blocks are dev-spec gas-limited (tens of Mgas); a calldata-stuffed
/// 60 Mgas block is under 4 MiB. 8 MiB leaves headroom while staying well
/// below RLPx's own 16 MiB frame ceiling.
pub const MAX_BLOCK_BYTES: usize = 8 * 1024 * 1024;
/// Largest inbound frame (id byte + payload): one block plus RLP framing.
pub const MAX_FRAME_BYTES: usize = MAX_BLOCK_BYTES + 64;

/// The `sova/1` capability.
#[must_use]
pub fn capability() -> Capability {
    Capability::new_static(CAPABILITY_NAME, CAPABILITY_VERSION)
}

/// The `sova/1` protocol (capability + reserved message count).
#[must_use]
pub fn protocol() -> Protocol {
    Protocol::new(capability(), MESSAGE_COUNT)
}

/// Message ids.
mod id {
    pub(super) const ANNOUNCE: u8 = 0x00;
    pub(super) const GET_BLOCK: u8 = 0x01;
    pub(super) const BLOCK: u8 = 0x02;
}

/// "I have accepted this block" — sent to every peer when a node produces
/// a block or accepts one (`VALID`/`ACCEPTED`) from a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Announce {
    /// The block's height.
    pub height: u64,
    /// The block's hash.
    pub hash: B256,
}

/// Request one block body by hash (the pull half of propagation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GetBlock {
    /// Hash of the requested block.
    pub hash: B256,
}

/// A `sova/1` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SovaMessage {
    /// A block the sender has accepted.
    Announce(Announce),
    /// A request for a block by hash.
    GetBlock(GetBlock),
    /// A block, as its raw RLP — only ever sent in answer to a
    /// [`SovaMessage::GetBlock`]; unsolicited ones are dropped.
    Block(Bytes),
}

/// Why a frame failed to decode. Any of these closes the connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// Empty frame (no message id).
    #[error("empty frame")]
    Empty,
    /// Frame larger than [`MAX_FRAME_BYTES`].
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_BYTES}-byte limit")]
    TooLarge(usize),
    /// Message id outside `0..MESSAGE_COUNT`.
    #[error("unknown message id {0:#04x}")]
    UnknownId(u8),
    /// Payload is not valid RLP for the message.
    #[error("bad rlp: {0}")]
    Rlp(String),
    /// Payload decoded but bytes were left over.
    #[error("{0} trailing byte(s) after payload")]
    Trailing(usize),
}

impl SovaMessage {
    /// Encodes `[id] ++ rlp(payload)`.
    #[must_use]
    pub fn encode(&self) -> alloy_primitives::bytes::BytesMut {
        let mut buf = alloy_primitives::bytes::BytesMut::new();
        match self {
            Self::Announce(a) => {
                buf.extend_from_slice(&[id::ANNOUNCE]);
                a.encode(&mut buf);
            }
            Self::GetBlock(g) => {
                buf.extend_from_slice(&[id::GET_BLOCK]);
                g.encode(&mut buf);
            }
            Self::Block(rlp) => {
                buf.extend_from_slice(&[id::BLOCK]);
                buf.extend_from_slice(rlp);
            }
        }
        buf
    }

    /// Decodes one frame, strictly (see [`CodecError`]).
    pub fn decode(frame: &[u8]) -> Result<Self, CodecError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(CodecError::TooLarge(frame.len()));
        }
        let (&msg_id, mut payload) = frame.split_first().ok_or(CodecError::Empty)?;
        let msg = match msg_id {
            id::ANNOUNCE => Self::Announce(Announce::decode(&mut payload).map_err(rlp_err)?),
            id::GET_BLOCK => Self::GetBlock(GetBlock::decode(&mut payload).map_err(rlp_err)?),
            id::BLOCK => {
                // Structural check only (one well-formed RLP list spanning
                // the whole payload); the service decodes the block itself.
                let whole = payload;
                let header = alloy_rlp::Header::decode(&mut payload).map_err(rlp_err)?;
                if !header.list {
                    return Err(CodecError::Rlp("block payload is not an rlp list".into()));
                }
                let body_len = header.payload_length;
                if payload.len() < body_len {
                    return Err(CodecError::Rlp("block payload truncated".into()));
                }
                payload = &payload[body_len..];
                let used = whole.len() - payload.len();
                if !payload.is_empty() {
                    return Err(CodecError::Trailing(payload.len()));
                }
                return Ok(Self::Block(Bytes::copy_from_slice(&whole[..used])));
            }
            other => return Err(CodecError::UnknownId(other)),
        };
        if !payload.is_empty() {
            return Err(CodecError::Trailing(payload.len()));
        }
        Ok(msg)
    }
}

fn rlp_err(e: alloy_rlp::Error) -> CodecError {
    CodecError::Rlp(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(msg: &SovaMessage) {
        let encoded = msg.encode();
        let decoded = SovaMessage::decode(&encoded).unwrap_or_else(|e| panic!("decode: {e}"));
        assert_eq!(&decoded, msg);
    }

    #[test]
    fn announce_round_trips() {
        round_trip(&SovaMessage::Announce(Announce {
            height: 0,
            hash: B256::ZERO,
        }));
        round_trip(&SovaMessage::Announce(Announce {
            height: u64::MAX,
            hash: B256::repeat_byte(0xab),
        }));
    }

    #[test]
    fn get_block_round_trips() {
        round_trip(&SovaMessage::GetBlock(GetBlock {
            hash: B256::repeat_byte(0x11),
        }));
    }

    #[test]
    fn block_round_trips_a_real_block() {
        let block = reth_ethereum::Block::default();
        let mut rlp = Vec::new();
        block.encode(&mut rlp);
        let msg = SovaMessage::Block(Bytes::from(rlp.clone()));
        round_trip(&msg);
        let SovaMessage::Block(bytes) =
            SovaMessage::decode(&msg.encode()).unwrap_or_else(|e| panic!("{e}"))
        else {
            panic!("not a block");
        };
        let decoded = reth_ethereum::Block::decode(&mut bytes.as_ref())
            .unwrap_or_else(|e| panic!("block decode: {e}"));
        assert_eq!(decoded, block);
    }

    #[test]
    fn ids_are_stable() {
        let a = SovaMessage::Announce(Announce {
            height: 1,
            hash: B256::ZERO,
        });
        let g = SovaMessage::GetBlock(GetBlock { hash: B256::ZERO });
        assert_eq!(a.encode()[0], 0x00);
        assert_eq!(g.encode()[0], 0x01);
        assert_eq!(
            SovaMessage::Block(Bytes::from_static(&[0xc0])).encode()[0],
            0x02
        );
        assert_eq!(protocol().messages(), 3);
    }

    #[test]
    fn rejects_malformed_frames() {
        assert_eq!(SovaMessage::decode(&[]), Err(CodecError::Empty));
        assert_eq!(SovaMessage::decode(&[0x03]), Err(CodecError::UnknownId(3)));
        assert!(matches!(
            SovaMessage::decode(&[0x00, 0x01]),
            Err(CodecError::Rlp(_))
        ));
        // Trailing garbage after a valid announce.
        let mut frame = SovaMessage::GetBlock(GetBlock { hash: B256::ZERO })
            .encode()
            .to_vec();
        frame.push(0x00);
        assert_eq!(SovaMessage::decode(&frame), Err(CodecError::Trailing(1)));
        // A block payload that isn't a list, and one with trailing bytes.
        assert!(matches!(
            SovaMessage::decode(&[0x02, 0x80]),
            Err(CodecError::Rlp(_))
        ));
        assert_eq!(
            SovaMessage::decode(&[0x02, 0xc0, 0x00]),
            Err(CodecError::Trailing(1))
        );
        // Truncated block list.
        assert!(matches!(
            SovaMessage::decode(&[0x02, 0xc3, 0x01]),
            Err(CodecError::Rlp(_))
        ));
    }

    #[test]
    fn rejects_oversized_frames() {
        let frame = vec![0x02; MAX_FRAME_BYTES + 1];
        assert_eq!(
            SovaMessage::decode(&frame),
            Err(CodecError::TooLarge(MAX_FRAME_BYTES + 1))
        );
    }
}
