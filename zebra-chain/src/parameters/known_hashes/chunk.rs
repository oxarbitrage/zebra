//! The byte layout of a known-hash list chunk.
//!
//! A chunk holds the block hash and size hint of every block in a contiguous
//! span of at most [`HASHES_PER_CHUNK`](super::HASHES_PER_CHUNK) heights:
//!
//! ```text
//! header (16 bytes):
//!   magic         b"ZKHL"                      4 bytes
//!   version       u8 = 1                       1 byte
//!   flags         u8 = 0 (reserved)            1 byte
//!   reserved      u16 = 0                      2 bytes
//!   start_height  u32, little-endian           4 bytes
//!   block_count   u32 = n, little-endian       4 bytes
//! block hashes:   n × 32 bytes, Zebra's internal byte order
//! size hints:     n × 1 byte, each in 1..=255
//! ```
//!
//! The layout is a deterministic function of the chain, so any synced node
//! regenerates byte-identical chunks, and their SHA-256 matches the constants
//! pinned in a [`KnownHashListSpec`](super::KnownHashListSpec).
//!
//! [`Chunk::parse`] only checks the structure. Whether a chunk is the *right*
//! chunk is decided by its pinned SHA-256 and the checkpoint list, in
//! [`KnownHashListSpec::verify_chunk`](super::KnownHashListSpec::verify_chunk).

use thiserror::Error;

use crate::block;

use super::{HASHES_PER_CHUNK, HASH_BYTES};

/// The magic bytes at the start of every chunk: "Zebra Known-Hash List".
pub const MAGIC: [u8; 4] = *b"ZKHL";

/// The chunk format version this module reads and writes.
pub const FORMAT_VERSION: u8 = 1;

/// The length of the fixed chunk header, in bytes.
pub const HEADER_LEN: usize = 16;

/// Structural errors in a chunk's bytes.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ChunkFormatError {
    #[error("chunk is {0} bytes, shorter than the {HEADER_LEN} byte header")]
    TooShort(usize),

    #[error("chunk does not start with the known-hash list magic bytes")]
    BadMagic,

    #[error("chunk format version {0} is not supported, expected {FORMAT_VERSION}")]
    UnsupportedVersion(u8),

    #[error("chunk header has non-zero reserved bytes")]
    NonZeroReserved,

    #[error("chunk declares {0} blocks, expected 1..={HASHES_PER_CHUNK}")]
    BadBlockCount(u32),

    #[error("chunk span starting at {start_height} with {block_count} blocks overflows the block height range")]
    HeightOverflow { start_height: u32, block_count: u32 },

    #[error("chunk is {actual} bytes, but its header implies {expected} bytes")]
    BadLength { expected: usize, actual: usize },

    #[error("chunk has a null block hash at height {0:?}")]
    NullHash(block::Height),

    #[error("chunk has an invalid size hint 0 at height {0:?}")]
    ZeroSizeHint(block::Height),
}

/// A structurally valid chunk, indexed in place over its raw bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Chunk {
    bytes: Vec<u8>,
    start_height: block::Height,
    block_count: u32,
}

impl Chunk {
    /// Parses and structurally validates `bytes` as a chunk.
    ///
    /// This doesn't check that the hashes belong to any chain: callers must
    /// also check the chunk's pinned SHA-256 and the checkpoint list.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, ChunkFormatError> {
        let header: &[u8; HEADER_LEN] = bytes
            .get(..HEADER_LEN)
            .and_then(|header| header.try_into().ok())
            .ok_or(ChunkFormatError::TooShort(bytes.len()))?;

        if header[0..4] != MAGIC {
            return Err(ChunkFormatError::BadMagic);
        }
        if header[4] != FORMAT_VERSION {
            return Err(ChunkFormatError::UnsupportedVersion(header[4]));
        }
        if header[5..8] != [0; 3] {
            return Err(ChunkFormatError::NonZeroReserved);
        }

        let start_height = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let block_count = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);

        if block_count == 0 || block_count > HASHES_PER_CHUNK {
            return Err(ChunkFormatError::BadBlockCount(block_count));
        }

        if start_height
            .checked_add(block_count - 1)
            .is_none_or(|end| end > block::Height::MAX.0)
        {
            return Err(ChunkFormatError::HeightOverflow {
                start_height,
                block_count,
            });
        }

        // block_count is at most HASHES_PER_CHUNK, so this can't overflow.
        let expected = HEADER_LEN + block_count as usize * (HASH_BYTES + 1);
        if bytes.len() != expected {
            return Err(ChunkFormatError::BadLength {
                expected,
                actual: bytes.len(),
            });
        }

        let chunk = Self {
            bytes,
            start_height: block::Height(start_height),
            block_count,
        };

        for rel in 0..block_count {
            let height = block::Height(start_height + rel);
            if chunk.hash_at(rel) == [0; HASH_BYTES] {
                return Err(ChunkFormatError::NullHash(height));
            }
            if chunk.hint_at(rel) == 0 {
                return Err(ChunkFormatError::ZeroSizeHint(height));
            }
        }

        Ok(chunk)
    }

    /// Encodes the chunk for `start_height` holding one `(hash, size hint)`
    /// pair per block.
    ///
    /// # Panics
    ///
    /// If `blocks` is empty or longer than [`HASHES_PER_CHUNK`]. These are
    /// emitter bugs: [`Chunk::parse`] validates untrusted bytes instead.
    pub fn encode(start_height: block::Height, blocks: &[(block::Hash, u8)]) -> Vec<u8> {
        assert!(
            !blocks.is_empty() && blocks.len() <= HASHES_PER_CHUNK as usize,
            "a chunk holds 1..={HASHES_PER_CHUNK} blocks, got {}",
            blocks.len(),
        );

        let mut out = Vec::with_capacity(HEADER_LEN + blocks.len() * (HASH_BYTES + 1));
        out.extend_from_slice(&MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&start_height.0.to_le_bytes());
        // The assert above bounds the length by HASHES_PER_CHUNK, a u32.
        out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());

        out.extend(blocks.iter().flat_map(|(hash, _)| hash.0));
        out.extend(blocks.iter().map(|(_, hint)| *hint));

        out
    }

    /// The height of the first block in the chunk.
    pub fn start_height(&self) -> block::Height {
        self.start_height
    }

    /// The number of blocks in the chunk.
    pub fn block_count(&self) -> u32 {
        self.block_count
    }

    /// The raw chunk bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the `(hash, size hint)` of the block at `height`, or `None`
    /// if `height` is outside the chunk.
    pub fn get(&self, height: block::Height) -> Option<(block::Hash, u8)> {
        let rel = height.0.checked_sub(self.start_height.0)?;
        (rel < self.block_count).then(|| (block::Hash(self.hash_at(rel)), self.hint_at(rel)))
    }

    /// Returns an iterator over the `(height, hash)` of every block in the
    /// chunk, in height order.
    pub fn hashes(&self) -> impl Iterator<Item = (block::Height, block::Hash)> + '_ {
        (0..self.block_count).map(|rel| {
            (
                block::Height(self.start_height.0 + rel),
                block::Hash(self.hash_at(rel)),
            )
        })
    }

    /// The hash at offset `rel`, which must be below `block_count`.
    fn hash_at(&self, rel: u32) -> [u8; HASH_BYTES] {
        let offset = HEADER_LEN + rel as usize * HASH_BYTES;
        self.bytes[offset..offset + HASH_BYTES]
            .try_into()
            .expect("the slice is HASH_BYTES long")
    }

    /// The size hint at offset `rel`, which must be below `block_count`.
    fn hint_at(&self, rel: u32) -> u8 {
        self.bytes[HEADER_LEN + self.block_count as usize * HASH_BYTES + rel as usize]
    }
}

impl std::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunk")
            .field("start_height", &self.start_height)
            .field("block_count", &self.block_count)
            .finish_non_exhaustive()
    }
}
