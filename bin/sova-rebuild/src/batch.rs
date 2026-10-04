//! Batch format v1: the unit Sova blocks are archived in (NEAR DA,
//! `docs/design/near-da.md` owns the final spec; this module reads and
//! writes the shape it describes).
//!
//! ```text
//! file   := magic "SOVADA1\0" (8 bytes) | chain_id u64 LE | first_height u64 LE
//!           | count u32 LE | block * count
//! block  := height u64 LE | hash [32] (the Sova block hash) | len u32 LE
//!           | raw [len] (the block's RLP, what debug_getRawBlock returns)
//! ```
//!
//! A reader takes any number of batches back to back (a file may be the
//! concatenation of several, and a stream of files is read the same way).
//! The compressed variant (`SOVADA1Z`) is recognised and refused with a
//! clear error: its codec is not specified yet.
//!
//! This module only parses. Whether the blocks are contiguous, hash to
//! what they claim, link to their parents and belong to the right chain is
//! [`crate::archive`]'s job; whether they are *valid* is the node's.

use std::io::{self, Read, Write};

use alloy_primitives::B256;

/// Magic of an uncompressed v1 batch.
pub const MAGIC: [u8; 8] = *b"SOVADA1\0";
/// Magic of the (future) compressed v1 batch: recognised, not decoded.
pub const MAGIC_COMPRESSED: [u8; 8] = *b"SOVADA1Z";
/// Bytes before the first block: magic, chain id, first height, count.
pub const HEADER_LEN: usize = 8 + 8 + 8 + 4;
/// Bytes of a block's framing before its RLP: height, hash, length.
pub const BLOCK_FRAME_LEN: usize = 8 + 32 + 4;
/// Largest raw block accepted. Far above any Sova block (the gas limit
/// bounds a block to a few MB); it only stops a corrupt length from
/// allocating gigabytes.
pub const MAX_RAW_BLOCK: u32 = 64 * 1024 * 1024;

/// A batch's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchHeader {
    /// The Sova chain ID the batch claims.
    pub chain_id: u64,
    /// Height of the batch's first block.
    pub first_height: u64,
    /// Number of blocks in the batch.
    pub count: u32,
}

/// One archived block, as framed in a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedBlock {
    /// The height the archive records.
    pub height: u64,
    /// The Sova block hash the archive records.
    pub hash: B256,
    /// The block's RLP (header, transactions, ommers, withdrawals).
    pub raw: Vec<u8>,
}

/// Why a batch could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    /// The bytes do not start with a known magic.
    #[error("not a Sova DA batch: magic {0:02x?}")]
    BadMagic([u8; 8]),
    /// A compressed (`SOVADA1Z`) batch: its codec is not specified yet.
    #[error("compressed batch (SOVADA1Z) is not supported by this build")]
    Compressed,
    /// The input ended inside a batch.
    #[error("truncated batch: {0}")]
    Truncated(&'static str),
    /// A block's length field is absurd.
    #[error("block at index {index} claims {len} bytes (max {MAX_RAW_BLOCK})")]
    TooLarge {
        /// Position in the batch.
        index: u32,
        /// The claimed length.
        len: u32,
    },
    /// Any other read error.
    #[error("read error: {0}")]
    Io(#[from] io::Error),
}

/// Writes one batch. `blocks` are written in the order given.
pub fn write_batch<W: Write>(
    w: &mut W,
    chain_id: u64,
    first_height: u64,
    blocks: &[ArchivedBlock],
) -> io::Result<()> {
    let count = u32::try_from(blocks.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many blocks in one batch"))?;
    w.write_all(&MAGIC)?;
    w.write_all(&chain_id.to_le_bytes())?;
    w.write_all(&first_height.to_le_bytes())?;
    w.write_all(&count.to_le_bytes())?;
    for b in blocks {
        let len = u32::try_from(b.raw.len())
            .ok()
            .filter(|l| *l <= MAX_RAW_BLOCK)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "block too large"))?;
        w.write_all(&b.height.to_le_bytes())?;
        w.write_all(b.hash.as_slice())?;
        w.write_all(&len.to_le_bytes())?;
        w.write_all(&b.raw)?;
    }
    Ok(())
}

/// Reads the header of the batch at the reader's position. `Ok(None)` at a
/// clean end of input (no byte read).
pub fn read_header<R: Read>(r: &mut R) -> Result<Option<BatchHeader>, BatchError> {
    let mut magic = [0u8; 8];
    match read_full(r, &mut magic)? {
        0 => return Ok(None),
        8 => {}
        _ => return Err(BatchError::Truncated("magic")),
    }
    if magic == MAGIC_COMPRESSED {
        return Err(BatchError::Compressed);
    }
    if magic != MAGIC {
        return Err(BatchError::BadMagic(magic));
    }
    let mut rest = [0u8; HEADER_LEN - 8];
    if read_full(r, &mut rest)? != rest.len() {
        return Err(BatchError::Truncated("batch header"));
    }
    Ok(Some(BatchHeader {
        chain_id: u64_le(&rest[0..8]),
        first_height: u64_le(&rest[8..16]),
        count: u32_le(&rest[16..20]),
    }))
}

/// Reads one block frame. `index` is its position in the batch (errors).
pub fn read_block<R: Read>(r: &mut R, index: u32) -> Result<ArchivedBlock, BatchError> {
    let mut frame = [0u8; BLOCK_FRAME_LEN];
    if read_full(r, &mut frame)? != frame.len() {
        return Err(BatchError::Truncated("block frame"));
    }
    let height = u64_le(&frame[0..8]);
    let hash = B256::from_slice(&frame[8..40]);
    let len = u32_le(&frame[40..44]);
    if len > MAX_RAW_BLOCK {
        return Err(BatchError::TooLarge { index, len });
    }
    let mut raw = vec![0u8; len as usize];
    if read_full(r, &mut raw)? != raw.len() {
        return Err(BatchError::Truncated("block body"));
    }
    Ok(ArchivedBlock { height, hash, raw })
}

/// One block read from a stream of batches, with the header of the batch
/// it came from and its position in it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The enclosing batch's header.
    pub batch: BatchHeader,
    /// Position within the batch (0-based).
    pub index: u32,
    /// The block.
    pub block: ArchivedBlock,
}

/// Iterates every block of every batch in a byte stream, in order.
#[derive(Debug)]
pub struct BatchReader<R> {
    reader: R,
    current: Option<(BatchHeader, u32)>,
    failed: bool,
}

impl<R: Read> BatchReader<R> {
    /// A reader over back-to-back batches.
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            current: None,
            failed: false,
        }
    }
}

impl<R: Read> Iterator for BatchReader<R> {
    type Item = Result<Entry, BatchError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            match self.current {
                Some((batch, index)) if index < batch.count => {
                    let out = read_block(&mut self.reader, index).map(|block| Entry {
                        batch,
                        index,
                        block,
                    });
                    self.current = Some((batch, index + 1));
                    if out.is_err() {
                        self.failed = true;
                    }
                    return Some(out);
                }
                _ => match read_header(&mut self.reader) {
                    Ok(Some(h)) => self.current = Some((h, 0)),
                    Ok(None) => return None,
                    Err(e) => {
                        self.failed = true;
                        return Some(Err(e));
                    }
                },
            }
        }
    }
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn u64_le(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    u64::from_le_bytes(a)
}

fn u32_le(b: &[u8]) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(b);
    u32::from_le_bytes(a)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn blk(h: u64, len: usize) -> ArchivedBlock {
        ArchivedBlock {
            height: h,
            hash: B256::repeat_byte(h as u8),
            raw: vec![h as u8; len],
        }
    }

    #[test]
    fn round_trip_concatenated_batches() {
        let mut buf = Vec::new();
        write_batch(&mut buf, 7, 1, &[blk(1, 3), blk(2, 0)]).unwrap();
        write_batch(&mut buf, 7, 3, &[blk(3, 5)]).unwrap();
        // The documented layout, byte for byte.
        assert_eq!(&buf[..8], b"SOVADA1\0");
        assert_eq!(&buf[8..16], &7u64.to_le_bytes());
        assert_eq!(&buf[16..24], &1u64.to_le_bytes());
        assert_eq!(&buf[24..28], &2u32.to_le_bytes());
        assert_eq!(&buf[28..36], &1u64.to_le_bytes());
        assert_eq!(&buf[36..68], B256::repeat_byte(1).as_slice());
        assert_eq!(&buf[68..72], &3u32.to_le_bytes());
        let got: Vec<_> = BatchReader::new(&buf[..])
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].block, blk(2, 0));
        assert_eq!(got[2].batch.first_height, 3);
        assert_eq!(got[2].index, 0);
    }

    #[test]
    fn empty_input_is_no_blocks() {
        assert_eq!(BatchReader::new(&[][..]).count(), 0);
    }

    #[test]
    fn truncation_magic_and_compressed_are_errors() {
        let mut buf = Vec::new();
        write_batch(&mut buf, 7, 1, &[blk(1, 10)]).unwrap();
        for cut in [3, HEADER_LEN - 1, HEADER_LEN + 5, buf.len() - 1] {
            let r: Vec<_> = BatchReader::new(&buf[..cut]).collect();
            assert!(r.last().unwrap().is_err(), "cut at {cut}");
        }
        let mut bad = buf.clone();
        bad[0] = b'X';
        assert!(matches!(
            BatchReader::new(&bad[..]).next(),
            Some(Err(BatchError::BadMagic(_)))
        ));
        let mut z = buf.clone();
        z[7] = b'Z';
        assert!(matches!(
            BatchReader::new(&z[..]).next(),
            Some(Err(BatchError::Compressed))
        ));
    }

    #[test]
    fn absurd_length_is_refused_before_allocating() {
        let mut buf = Vec::new();
        write_batch(&mut buf, 7, 1, &[blk(1, 1)]).unwrap();
        buf[HEADER_LEN + 40..HEADER_LEN + 44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            BatchReader::new(&buf[..]).next(),
            Some(Err(BatchError::TooLarge { .. }))
        ));
    }
}
