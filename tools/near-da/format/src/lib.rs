//! `sovada`: the SOVADA1 batch format, Sova blocks as posted to NEAR.
//!
//! Spec: `docs/design/near-da.md` §4. One batch is one NEAR function-call
//! argument blob (and one `.sovada` file on disk):
//!
//! ```text
//! file   := magic "SOVADA1\0" (8 bytes) | chain_id u64 LE | first_height u64 LE
//!           | count u32 LE | block*count
//! block  := height u64 LE | hash [32] | len u32 LE | raw [len]
//! ```
//!
//! `raw` is the block exactly as `debug_getRawBlock` returns it (the RLP
//! list `[header, transactions, ommers, withdrawals, ...]`), and `hash` is
//! the Sova block hash, keccak-256 of the header's RLP. Heights are
//! contiguous inside a batch and batches are contiguous across the archive.
//! Integers are little-endian; there is no padding and nothing may follow
//! the last block.
//!
//! The crate has no dependencies, so the NEAR contract (wasm32), the poster,
//! the fetcher and the rebuild tool all share one parser. Hashing is passed
//! in ([`Keccak`]): the contract uses NEAR's host function, tools use a
//! library.

#![forbid(unsafe_code)]

use core::fmt;

/// Magic of an uncompressed v1 batch.
pub const MAGIC_V1: [u8; 8] = *b"SOVADA1\0";
/// Reserved for a compressed v1 batch (not produced or accepted yet; see
/// the design doc, "Compression").
pub const MAGIC_V1_COMPRESSED: [u8; 8] = *b"SOVADA1Z";
/// Bytes before the first block entry.
pub const HEADER_LEN: usize = 8 + 8 + 8 + 4;
/// Bytes of a block entry before its raw RLP.
pub const ENTRY_HEADER_LEN: usize = 8 + 32 + 4;
/// Most blocks one batch may carry (the contract enforces the same bound).
pub const MAX_BLOCKS_PER_BATCH: u32 = 10_000;

/// A keccak-256 implementation, supplied by the caller.
pub type Keccak<'a> = &'a dyn Fn(&[u8]) -> [u8; 32];

/// Why bytes are not a valid batch (or a block in one is wrong).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the fixed header, or a block entry runs past the end.
    Truncated {
        /// What was being read.
        what: &'static str,
    },
    /// The first 8 bytes are not a known magic.
    BadMagic([u8; 8]),
    /// `SOVADA1Z`: reserved, not supported by this version.
    Compressed,
    /// `count` is 0 or above [`MAX_BLOCKS_PER_BATCH`].
    BadCount(u32),
    /// Bytes left after the last block.
    TrailingBytes(usize),
    /// A block entry's height is not the next one.
    NotContiguous {
        /// The height that should come next.
        expected: u64,
        /// The height the entry carries.
        got: u64,
    },
    /// A height overflowed `u64`.
    HeightOverflow,
    /// A different chain than expected.
    WrongChain {
        /// Expected chain id.
        expected: u64,
        /// The batch's chain id.
        got: u64,
    },
    /// The batch does not start where it should.
    WrongStart {
        /// Expected first height.
        expected: u64,
        /// The batch's first height.
        got: u64,
    },
    /// A raw block is not the RLP shape of a block.
    BadRlp {
        /// The block's height.
        height: u64,
        /// What is wrong.
        what: &'static str,
    },
    /// keccak-256 of the block's header is not the hash its entry claims.
    HashMismatch {
        /// The block's height.
        height: u64,
    },
    /// The header's `number` field is not the entry's height.
    NumberMismatch {
        /// The entry's height.
        height: u64,
        /// The header's number.
        number: u64,
    },
    /// The block's `parentHash` is not the previous block's hash.
    ParentMismatch {
        /// The block's height.
        height: u64,
    },
    /// A raw block longer than `u32::MAX` bytes (encode only).
    BlockTooLarge {
        /// The block's height.
        height: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { what } => write!(f, "truncated batch ({what})"),
            Self::BadMagic(m) => write!(f, "not a SOVADA batch (magic {m:02x?})"),
            Self::Compressed => write!(f, "SOVADA1Z (compressed) is reserved and not supported"),
            Self::BadCount(c) => write!(f, "block count {c} outside 1..={MAX_BLOCKS_PER_BATCH}"),
            Self::TrailingBytes(n) => write!(f, "{n} trailing byte(s) after the last block"),
            Self::NotContiguous { expected, got } => {
                write!(f, "heights not contiguous: expected {expected}, got {got}")
            }
            Self::HeightOverflow => write!(f, "height overflows u64"),
            Self::WrongChain { expected, got } => {
                write!(f, "chain id {got}, expected {expected}")
            }
            Self::WrongStart { expected, got } => {
                write!(f, "batch starts at {got}, expected {expected}")
            }
            Self::BadRlp { height, what } => write!(f, "block {height}: bad RLP ({what})"),
            Self::HashMismatch { height } => {
                write!(f, "block {height}: keccak(header) is not the claimed hash")
            }
            Self::NumberMismatch { height, number } => {
                write!(f, "block {height}: header number is {number}")
            }
            Self::ParentMismatch { height } => {
                write!(
                    f,
                    "block {height}: parentHash is not the previous block's hash"
                )
            }
            Self::BlockTooLarge { height } => write!(f, "block {height}: raw block over 4 GiB"),
        }
    }
}

impl std::error::Error for Error {}

/// The fixed 28-byte batch header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// EVM chain id of the Sova network.
    pub chain_id: u64,
    /// Height of the first block.
    pub first_height: u64,
    /// Number of blocks (1..=[`MAX_BLOCKS_PER_BATCH`]).
    pub count: u32,
}

impl Header {
    /// Height of the last block.
    pub fn last_height(&self) -> u64 {
        self.first_height + u64::from(self.count) - 1
    }
}

/// One block entry, borrowing the batch bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block<'a> {
    /// Sova block height.
    pub height: u64,
    /// Sova block hash (keccak-256 of the header RLP), as the batch claims.
    pub hash: [u8; 32],
    /// The block's RLP (`debug_getRawBlock`).
    pub raw: &'a [u8],
}

/// A parsed batch, borrowing its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch<'a> {
    /// The fixed header.
    pub header: Header,
    /// Exactly `header.count` blocks, heights `first_height..`.
    pub blocks: Vec<Block<'a>>,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated { what })?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(Error::Truncated { what })?;
        self.pos = end;
        Ok(s)
    }
    fn u64(&mut self, what: &'static str) -> Result<u64, Error> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8, what)?);
        Ok(u64::from_le_bytes(b))
    }
    fn u32(&mut self, what: &'static str) -> Result<u32, Error> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4, what)?);
        Ok(u32::from_le_bytes(b))
    }
}

/// Parse only the fixed header (magic, chain id, first height, count).
pub fn parse_header(bytes: &[u8]) -> Result<Header, Error> {
    let mut r = Reader { buf: bytes, pos: 0 };
    let mut magic = [0u8; 8];
    magic.copy_from_slice(r.take(8, "magic")?);
    if magic == MAGIC_V1_COMPRESSED {
        return Err(Error::Compressed);
    }
    if magic != MAGIC_V1 {
        return Err(Error::BadMagic(magic));
    }
    let chain_id = r.u64("chain_id")?;
    let first_height = r.u64("first_height")?;
    let count = r.u32("count")?;
    if count == 0 || count > MAX_BLOCKS_PER_BATCH {
        return Err(Error::BadCount(count));
    }
    first_height
        .checked_add(u64::from(count) - 1)
        .ok_or(Error::HeightOverflow)?;
    Ok(Header {
        chain_id,
        first_height,
        count,
    })
}

/// Parse a whole batch: the header, every entry, contiguous heights from
/// `first_height`, and no trailing bytes. Does not hash anything; see
/// [`verify_blocks`].
pub fn parse(bytes: &[u8]) -> Result<Batch<'_>, Error> {
    let header = parse_header(bytes)?;
    let mut r = Reader {
        buf: bytes,
        pos: HEADER_LEN,
    };
    let mut blocks = Vec::with_capacity(header.count as usize);
    for i in 0..u64::from(header.count) {
        let expected = header.first_height + i;
        let height = r.u64("block height")?;
        if height != expected {
            return Err(Error::NotContiguous {
                expected,
                got: height,
            });
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(r.take(32, "block hash")?);
        let len = r.u32("block length")?;
        let raw = r.take(len as usize, "block body")?;
        blocks.push(Block { height, hash, raw });
    }
    if r.pos != bytes.len() {
        return Err(Error::TrailingBytes(bytes.len() - r.pos));
    }
    Ok(Batch { header, blocks })
}

/// Encode a batch. `blocks` must be non-empty, at most
/// [`MAX_BLOCKS_PER_BATCH`], with contiguous heights; the first block's
/// height is the batch's `first_height`.
pub fn encode(chain_id: u64, blocks: &[Block<'_>]) -> Result<Vec<u8>, Error> {
    let count = u32::try_from(blocks.len()).map_err(|_| Error::BadCount(u32::MAX))?;
    if count == 0 || count > MAX_BLOCKS_PER_BATCH {
        return Err(Error::BadCount(count));
    }
    let first_height = blocks[0].height;
    let body: usize = blocks.iter().map(|b| ENTRY_HEADER_LEN + b.raw.len()).sum();
    let mut out = Vec::with_capacity(HEADER_LEN + body);
    out.extend_from_slice(&MAGIC_V1);
    out.extend_from_slice(&chain_id.to_le_bytes());
    out.extend_from_slice(&first_height.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    let mut expected = first_height;
    for b in blocks {
        if b.height != expected {
            return Err(Error::NotContiguous {
                expected,
                got: b.height,
            });
        }
        let len =
            u32::try_from(b.raw.len()).map_err(|_| Error::BlockTooLarge { height: b.height })?;
        out.extend_from_slice(&b.height.to_le_bytes());
        out.extend_from_slice(&b.hash);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(b.raw);
        expected = expected.checked_add(1).ok_or(Error::HeightOverflow)?;
    }
    Ok(out)
}

/// Check every block of a parsed batch against its claims: the header is
/// the first item of the block's RLP list, keccak-256 of the header is the
/// entry's hash, the header's `number` is the entry's height, and each
/// block's `parentHash` is the previous block's hash (the first block's is
/// checked against `prev_hash` when given: the last block of the previous
/// batch). Returns the last block's hash.
pub fn verify_blocks(
    batch: &Batch<'_>,
    keccak: Keccak<'_>,
    prev_hash: Option<[u8; 32]>,
) -> Result<[u8; 32], Error> {
    let mut prev = prev_hash;
    for b in &batch.blocks {
        let header = rlp::block_header(b.raw).map_err(|what| Error::BadRlp {
            height: b.height,
            what,
        })?;
        if keccak(header) != b.hash {
            return Err(Error::HashMismatch { height: b.height });
        }
        let fields = rlp::header_fields(header).map_err(|what| Error::BadRlp {
            height: b.height,
            what,
        })?;
        if fields.number != b.height {
            return Err(Error::NumberMismatch {
                height: b.height,
                number: fields.number,
            });
        }
        if let Some(p) = prev
            && fields.parent_hash != p
        {
            return Err(Error::ParentMismatch { height: b.height });
        }
        prev = Some(b.hash);
    }
    // A parsed batch has at least one block.
    Ok(prev.unwrap_or([0u8; 32]))
}

/// The minimum RLP needed to find a block's header, its parent and number.
pub mod rlp {
    /// One decoded item prefix.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Item {
        /// A list (else a byte string).
        pub is_list: bool,
        /// Prefix bytes before the payload.
        pub header_len: usize,
        /// Payload bytes.
        pub payload_len: usize,
    }

    impl Item {
        /// Prefix plus payload.
        pub fn total_len(&self) -> usize {
            self.header_len + self.payload_len
        }
    }

    /// Decode the prefix of the item at the start of `buf`, checking that
    /// the whole item fits in `buf`.
    pub fn item(buf: &[u8]) -> Result<Item, &'static str> {
        let b = *buf.first().ok_or("empty item")?;
        let (is_list, header_len, payload_len) = match b {
            0x00..=0x7f => (false, 0, 1),
            0x80..=0xb7 => (false, 1, usize::from(b - 0x80)),
            0xb8..=0xbf => {
                let n = usize::from(b - 0xb7);
                (false, 1 + n, long_len(buf, n)?)
            }
            0xc0..=0xf7 => (true, 1, usize::from(b - 0xc0)),
            0xf8..=0xff => {
                let n = usize::from(b - 0xf7);
                (true, 1 + n, long_len(buf, n)?)
            }
        };
        let it = Item {
            is_list,
            header_len,
            payload_len,
        };
        if it
            .header_len
            .checked_add(it.payload_len)
            .ok_or("length overflow")?
            > buf.len()
        {
            return Err("item runs past the end");
        }
        Ok(it)
    }

    fn long_len(buf: &[u8], n: usize) -> Result<usize, &'static str> {
        let bytes = buf.get(1..1 + n).ok_or("truncated length")?;
        if bytes.first() == Some(&0) {
            return Err("length with a leading zero");
        }
        if n > 4 {
            return Err("length over 4 GiB");
        }
        let mut v = 0usize;
        for &x in bytes {
            v = (v << 8) | usize::from(x);
        }
        if v < 56 {
            return Err("long form for a short length");
        }
        Ok(v)
    }

    /// The header's full RLP (prefix included): the first item of the
    /// block's list. The block must be exactly one list item.
    pub fn block_header(raw: &[u8]) -> Result<&[u8], &'static str> {
        let outer = item(raw)?;
        if !outer.is_list {
            return Err("block is not a list");
        }
        if outer.total_len() != raw.len() {
            return Err("bytes after the block's list");
        }
        let payload = &raw[outer.header_len..];
        let h = item(payload)?;
        if !h.is_list {
            return Err("header is not a list");
        }
        Ok(&payload[..h.total_len()])
    }

    /// The header fields the archive checks.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HeaderFields {
        /// `parentHash` (field 0).
        pub parent_hash: [u8; 32],
        /// `number` (field 8).
        pub number: u64,
    }

    /// Read `parentHash` and `number` from a header's RLP.
    pub fn header_fields(header: &[u8]) -> Result<HeaderFields, &'static str> {
        let h = item(header)?;
        if !h.is_list || h.total_len() != header.len() {
            return Err("header is not one list");
        }
        let mut rest = &header[h.header_len..];
        let mut parent_hash = None;
        let mut number = None;
        for i in 0..9 {
            let it = item(rest)?;
            if it.is_list {
                return Err("header field is a list");
            }
            let payload = &rest[it.header_len..it.total_len()];
            match i {
                0 => {
                    if it.header_len != 1 || payload.len() != 32 {
                        return Err("parentHash is not 32 bytes");
                    }
                    let mut p = [0u8; 32];
                    p.copy_from_slice(payload);
                    parent_hash = Some(p);
                }
                8 => number = Some(uint(payload, it)?),
                _ => {}
            }
            rest = &rest[it.total_len()..];
        }
        match (parent_hash, number) {
            (Some(parent_hash), Some(number)) => Ok(HeaderFields {
                parent_hash,
                number,
            }),
            _ => Err("header too short"),
        }
    }

    fn uint(payload: &[u8], it: Item) -> Result<u64, &'static str> {
        if payload.len() > 8 {
            return Err("number over 64 bits");
        }
        if payload.first() == Some(&0) {
            return Err("number with a leading zero");
        }
        // A single byte below 0x80 is its own encoding (header_len 0).
        if it.header_len == 1 && payload.len() == 1 && payload[0] < 0x80 {
            return Err("non-canonical single byte");
        }
        Ok(payload.iter().fold(0u64, |v, &x| (v << 8) | u64::from(x)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A stand-in hash for tests: not keccak, but deterministic and
    /// injective enough for these vectors.
    fn fake_keccak(data: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
        for (i, &b) in data.iter().enumerate() {
            acc ^= u64::from(b);
            acc = acc.wrapping_mul(0x0100_0000_01b3);
            out[i % 32] ^= (acc >> 24) as u8;
        }
        out[..8].copy_from_slice(&acc.to_le_bytes());
        out
    }

    fn rlp_bytes(payload: &[u8]) -> Vec<u8> {
        if payload.len() == 1 && payload[0] < 0x80 {
            return payload.to_vec();
        }
        let mut v = rlp_prefix(0x80, payload.len());
        v.extend_from_slice(payload);
        v
    }

    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let payload: Vec<u8> = items.concat();
        let mut v = rlp_prefix(0xc0, payload.len());
        v.extend_from_slice(&payload);
        v
    }

    fn rlp_prefix(base: u8, len: usize) -> Vec<u8> {
        if len < 56 {
            vec![base + len as u8]
        } else {
            let be = (len as u64).to_be_bytes();
            let skip = be.iter().take_while(|&&b| b == 0).count();
            let mut v = vec![base + 55 + (8 - skip) as u8];
            v.extend_from_slice(&be[skip..]);
            v
        }
    }

    fn rlp_uint(n: u64) -> Vec<u8> {
        let be = n.to_be_bytes();
        let skip = be.iter().take_while(|&&b| b == 0).count();
        rlp_bytes(&be[skip..])
    }

    /// A block shaped like a post-merge Ethereum block: a 17-field header
    /// (with a 256-byte bloom, so the header uses a long-form list prefix),
    /// then empty transactions/ommers/withdrawals lists.
    fn make_block(parent: [u8; 32], number: u64) -> (Vec<u8>, [u8; 32]) {
        let mut fields = vec![
            rlp_bytes(&parent),
            rlp_bytes(&[0x1d; 32]),
            rlp_bytes(&[0xaa; 20]),
            rlp_bytes(&[0x01; 32]),
            rlp_bytes(&[0x02; 32]),
            rlp_bytes(&[0x03; 32]),
            rlp_bytes(&[0u8; 256]),
            rlp_uint(0),
            rlp_uint(number),
            rlp_uint(30_000_000),
            rlp_uint(0),
            rlp_uint(1_790_000_000 + number),
            rlp_bytes(&[]),
        ];
        fields.extend((0..4).map(|i| rlp_bytes(&[i as u8 + 4; 32])));
        let header = rlp_list(&fields);
        let hash = fake_keccak(&header);
        let block = rlp_list(&[header, rlp_list(&[]), rlp_list(&[]), rlp_list(&[])]);
        (block, hash)
    }

    fn chain(first: u64, n: u64, parent: [u8; 32]) -> Vec<(u64, [u8; 32], Vec<u8>)> {
        let mut out = Vec::new();
        let mut p = parent;
        for h in first..first + n {
            let (raw, hash) = make_block(p, h);
            out.push((h, hash, raw));
            p = hash;
        }
        out
    }

    fn blocks(c: &[(u64, [u8; 32], Vec<u8>)]) -> Vec<Block<'_>> {
        c.iter()
            .map(|(height, hash, raw)| Block {
                height: *height,
                hash: *hash,
                raw,
            })
            .collect()
    }

    #[test]
    fn round_trip_and_layout() {
        let c = chain(7, 3, [9u8; 32]);
        let bytes = encode(82330, &blocks(&c)).unwrap();
        assert_eq!(&bytes[..8], b"SOVADA1\0");
        assert_eq!(&bytes[8..16], &82330u64.to_le_bytes());
        assert_eq!(&bytes[16..24], &7u64.to_le_bytes());
        assert_eq!(&bytes[24..28], &3u32.to_le_bytes());
        assert_eq!(&bytes[28..36], &7u64.to_le_bytes());
        assert_eq!(&bytes[36..68], &c[0].1);
        assert_eq!(&bytes[68..72], &(c[0].2.len() as u32).to_le_bytes());
        assert_eq!(&bytes[72..72 + c[0].2.len()], &c[0].2[..]);
        let want_len: usize = HEADER_LEN
            + c.iter()
                .map(|b| ENTRY_HEADER_LEN + b.2.len())
                .sum::<usize>();
        assert_eq!(bytes.len(), want_len);

        let batch = parse(&bytes).unwrap();
        assert_eq!(
            batch.header,
            Header {
                chain_id: 82330,
                first_height: 7,
                count: 3
            }
        );
        assert_eq!(batch.header.last_height(), 9);
        assert_eq!(batch.blocks, blocks(&c));
        assert_eq!(encode(82330, &batch.blocks).unwrap(), bytes);
        let last = verify_blocks(&batch, &fake_keccak, Some([9u8; 32])).unwrap();
        assert_eq!(last, c[2].1);
    }

    #[test]
    fn rejects_structural_damage() {
        let c = chain(0, 2, [0u8; 32]);
        let good = encode(1, &blocks(&c)).unwrap();

        assert!(matches!(parse(&good[..20]), Err(Error::Truncated { .. })));
        assert!(matches!(
            parse(&good[..good.len() - 1]),
            Err(Error::Truncated { .. })
        ));
        let mut extra = good.clone();
        extra.push(0);
        assert_eq!(parse(&extra), Err(Error::TrailingBytes(1)));

        let mut magic = good.clone();
        magic[6] = b'2';
        assert!(matches!(parse(&magic), Err(Error::BadMagic(_))));
        let mut z = good.clone();
        z[7] = b'Z';
        assert_eq!(parse(&z), Err(Error::Compressed));

        let mut zero = good.clone();
        zero[24..28].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse(&zero), Err(Error::BadCount(0)));
        let mut big = good.clone();
        big[24..28].copy_from_slice(&(MAX_BLOCKS_PER_BATCH + 1).to_le_bytes());
        assert_eq!(parse(&big), Err(Error::BadCount(MAX_BLOCKS_PER_BATCH + 1)));

        // The second entry's height skips one.
        let off = HEADER_LEN + ENTRY_HEADER_LEN + c[0].2.len();
        let mut gap = good.clone();
        gap[off..off + 8].copy_from_slice(&2u64.to_le_bytes());
        assert_eq!(
            parse(&gap),
            Err(Error::NotContiguous {
                expected: 1,
                got: 2
            })
        );
        // The header says first_height 5 but the entries start at 0.
        let mut start = good.clone();
        start[16..24].copy_from_slice(&5u64.to_le_bytes());
        assert_eq!(
            parse(&start),
            Err(Error::NotContiguous {
                expected: 5,
                got: 0
            })
        );
        let mut ovf = good.clone();
        ovf[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(parse(&ovf), Err(Error::HeightOverflow));
    }

    #[test]
    fn encode_refuses_bad_input() {
        let c = chain(3, 3, [0u8; 32]);
        let mut b = blocks(&c);
        assert_eq!(encode(1, &[]), Err(Error::BadCount(0)));
        b.swap(1, 2);
        assert_eq!(
            encode(1, &b),
            Err(Error::NotContiguous {
                expected: 4,
                got: 5
            })
        );
    }

    #[test]
    fn verify_catches_wrong_claims() {
        let c = chain(10, 3, [5u8; 32]);
        // Wrong claimed hash.
        let mut bad = c.clone();
        bad[1].1[0] ^= 1;
        let bytes = encode(1, &blocks(&bad)).unwrap();
        assert_eq!(
            verify_blocks(&parse(&bytes).unwrap(), &fake_keccak, None),
            Err(Error::HashMismatch { height: 11 })
        );
        // Wrong link to the previous batch.
        let bytes = encode(1, &blocks(&c)).unwrap();
        assert_eq!(
            verify_blocks(&parse(&bytes).unwrap(), &fake_keccak, Some([6u8; 32])),
            Err(Error::ParentMismatch { height: 10 })
        );
        // A block from another branch in the middle: its hash is right but
        // its parent is not the previous block.
        let mut forked = c.clone();
        let (raw, hash) = make_block([0xee; 32], 11);
        forked[1] = (11, hash, raw);
        let bytes = encode(1, &blocks(&forked)).unwrap();
        assert_eq!(
            verify_blocks(&parse(&bytes).unwrap(), &fake_keccak, None),
            Err(Error::ParentMismatch { height: 11 })
        );
        // A block whose header number is not its entry height.
        let (raw, hash) = make_block(c[0].1, 99);
        let mut renumbered = c.clone();
        renumbered[1] = (11, hash, raw);
        let bytes = encode(1, &blocks(&renumbered)).unwrap();
        assert_eq!(
            verify_blocks(&parse(&bytes).unwrap(), &fake_keccak, None),
            Err(Error::NumberMismatch {
                height: 11,
                number: 99
            })
        );
        // Not RLP at all.
        let junk = [(10u64, [0u8; 32], vec![0x01u8, 0x02])];
        let bytes = encode(1, &blocks(&junk)).unwrap();
        assert!(matches!(
            verify_blocks(&parse(&bytes).unwrap(), &fake_keccak, None),
            Err(Error::BadRlp { height: 10, .. })
        ));
    }

    #[test]
    fn rlp_header_extraction() {
        let (raw, hash) = make_block([3u8; 32], 300);
        let header = rlp::block_header(&raw).unwrap();
        assert_eq!(fake_keccak(header), hash);
        let f = rlp::header_fields(header).unwrap();
        assert_eq!(f.parent_hash, [3u8; 32]);
        assert_eq!(f.number, 300);
        // Height 0 encodes as the empty string 0x80.
        let (raw0, _) = make_block([0u8; 32], 0);
        assert_eq!(
            rlp::header_fields(rlp::block_header(&raw0).unwrap())
                .unwrap()
                .number,
            0
        );
        // Trailing junk after the block list.
        let mut t = raw.clone();
        t.push(0x80);
        assert!(rlp::block_header(&t).is_err());
        // A truncated block.
        assert!(rlp::block_header(&raw[..raw.len() - 1]).is_err());
        assert!(rlp::block_header(&[]).is_err());
    }
}
