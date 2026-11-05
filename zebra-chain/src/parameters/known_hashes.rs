//! Every-block known-hash lists.
//!
//! A known-hash list pins the hash of every block from genesis up to a
//! checkpoint height, plus an upper bound on each block's serialized size.
//! A future initial sync can use it to request blocks by hash from many peers
//! at once, instead of discovering them a few hundred at a time.
//!
//! The list is far too large to compile into the binary (about 110 MB on
//! Mainnet), so it is split into chunk files (see [`chunk`]). A
//! [`KnownHashListSpec`] pins each chunk's SHA-256, and the loader also checks
//! every chunk against the network's checkpoint list. The checkpoint list stays
//! the root of trust: a chunk that disagrees with any checkpoint it covers is
//! rejected even if its SHA-256 matches the spec, and a list must end exactly at
//! a checkpoint height.
//!
//! Nothing in Zebra reads a known-hash list yet.

use std::{fs, path::PathBuf, sync::Arc};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    block::{self, MAX_BLOCK_BYTES},
    parameters::checkpoint::list::CheckpointList,
};

pub mod chunk;

#[cfg(test)]
mod tests;

pub use chunk::{Chunk, ChunkFormatError};

/// The number of bytes in a block hash.
const HASH_BYTES: usize = 32;

/// The maximum number of blocks in a chunk, and the number of blocks in every
/// chunk except the last one in the lists Zebra publishes.
///
/// A full chunk is 16 + 150,000 × 33 bytes, about 4.95 MB.
pub const HASHES_PER_CHUNK: u32 = 150_000;

/// The size of one size-hint unit, in bytes: `MAX_BLOCK_BYTES / 255`, rounded up.
///
/// A size hint `w` means the block's serialized size is at most
/// `w × SIZE_HINT_UNIT` bytes, so the maximum hint, 255, covers any valid block.
pub const SIZE_HINT_UNIT: u32 = MAX_BLOCK_BYTES.div_ceil(255) as u32;

const _: () = assert!(SIZE_HINT_UNIT == 7_844);
const _: () = assert!(255 * SIZE_HINT_UNIT as u64 >= MAX_BLOCK_BYTES);

/// Returns the size hint for a block of `serialized_size` bytes: the size in
/// [`SIZE_HINT_UNIT`]s, rounded up, so the hint is always an upper bound.
///
/// Returns `None` if `serialized_size` is zero or above [`MAX_BLOCK_BYTES`].
pub fn size_hint(serialized_size: u64) -> Option<u8> {
    if serialized_size == 0 || serialized_size > MAX_BLOCK_BYTES {
        return None;
    }

    u8::try_from(serialized_size.div_ceil(u64::from(SIZE_HINT_UNIT))).ok()
}

/// Returns the maximum serialized size, in bytes, of a block with size `hint`.
pub fn size_hint_upper_bound(hint: u8) -> u64 {
    u64::from(hint) * u64::from(SIZE_HINT_UNIT)
}

/// Returns the SHA-256 of `chunk_bytes` as lowercase hex, the form pinned in
/// [`KnownHashListSpec::chunk_hashes`].
pub fn chunk_sha256_hex(chunk_bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(chunk_bytes))
}

/// Returns the file name of chunk `index` for `file_prefix`, for example
/// `main-known-hashes-00.bin`.
pub fn chunk_file_name(file_prefix: &str, index: usize) -> String {
    format!("{file_prefix}-{index:02}.bin")
}

/// A description of a known-hash list's chunks, reviewed like a checkpoint
/// list.
///
/// `max_height` is pinned rather than derived from the files, so a truncated
/// or extended set of chunks is a hard error.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KnownHashListSpec<'a> {
    /// The highest height in the list. Must be a checkpoint height.
    pub max_height: block::Height,

    /// The number of blocks in every chunk except the last one, in
    /// `1..=HASHES_PER_CHUNK`. Published lists use [`HASHES_PER_CHUNK`].
    pub chunk_blocks: u32,

    /// The chunk file name prefix, see [`chunk_file_name`].
    pub file_prefix: &'a str,

    /// The SHA-256 of each chunk file, as lowercase hex, in height order.
    pub chunk_hashes: &'a [&'a str],
}

impl KnownHashListSpec<'_> {
    /// The number of blocks in the list.
    pub fn len(&self) -> u64 {
        u64::from(self.max_height.0) + 1
    }

    /// Always `false`: a list holds at least the genesis block.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The first height in chunk `index`.
    pub fn chunk_start(&self, index: usize) -> block::Height {
        let start = index as u64 * u64::from(self.chunk_blocks);
        block::Height(u32::try_from(start).unwrap_or(u32::MAX))
    }

    /// The number of blocks in chunk `index`, or zero if it is past the end of
    /// the list.
    pub fn chunk_len(&self, index: usize) -> u32 {
        let start = index as u64 * u64::from(self.chunk_blocks);
        let len = self
            .len()
            .saturating_sub(start)
            .min(u64::from(self.chunk_blocks));
        u32::try_from(len).expect("chunk_blocks is a u32")
    }

    /// The index of the chunk holding `height`, or `None` past the end of the
    /// list.
    pub fn chunk_index(&self, height: block::Height) -> Option<usize> {
        (height <= self.max_height).then(|| (height.0 / self.chunk_blocks) as usize)
    }

    /// The file name of chunk `index`.
    pub fn chunk_file_name(&self, index: usize) -> String {
        chunk_file_name(self.file_prefix, index)
    }

    /// Checks that the spec is well formed and ends at a checkpoint height in
    /// `checkpoints`.
    pub fn validate(&self, checkpoints: &CheckpointList) -> Result<(), KnownHashError> {
        if self.chunk_blocks == 0 || self.chunk_blocks > HASHES_PER_CHUNK {
            return Err(KnownHashError::InvalidSpec(format!(
                "chunk_blocks is {}, expected 1..={HASHES_PER_CHUNK}",
                self.chunk_blocks
            )));
        }

        let expected_chunks = self.len().div_ceil(u64::from(self.chunk_blocks));
        if self.chunk_hashes.len() as u64 != expected_chunks {
            return Err(KnownHashError::InvalidSpec(format!(
                "{} chunk hashes, but {} blocks in chunks of {} need {expected_chunks}",
                self.chunk_hashes.len(),
                self.len(),
                self.chunk_blocks,
            )));
        }

        if let Some(bad) = self.chunk_hashes.iter().find(|hash| {
            hash.len() != 2 * HASH_BYTES
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(KnownHashError::InvalidSpec(format!(
                "chunk hash {bad:?} is not 64 lowercase hex characters"
            )));
        }

        if !checkpoints.contains(self.max_height) {
            return Err(KnownHashError::NotAnchored {
                max_height: self.max_height,
            });
        }

        Ok(())
    }

    /// Verifies that `bytes` are chunk `index` of this list, and returns the
    /// parsed chunk.
    ///
    /// Checks, in order: the spec itself, the chunk's SHA-256 against the
    /// pinned constant, the chunk's structure, that it covers exactly the
    /// heights the spec assigns to `index`, and that it agrees with every
    /// checkpoint in that range.
    pub fn verify_chunk(
        &self,
        index: usize,
        bytes: Vec<u8>,
        checkpoints: &CheckpointList,
    ) -> Result<Chunk, KnownHashError> {
        self.validate(checkpoints)?;

        let expected = *self
            .chunk_hashes
            .get(index)
            .ok_or(KnownHashError::ChunkIndex { index })?;
        let actual = chunk_sha256_hex(&bytes);
        if actual != expected {
            return Err(KnownHashError::ChunkHashMismatch {
                index,
                expected: expected.to_owned(),
                actual,
            });
        }

        let chunk =
            Chunk::parse(bytes).map_err(|error| KnownHashError::ChunkFormat { index, error })?;

        let expected_start = self.chunk_start(index);
        let expected_count = self.chunk_len(index);
        if chunk.start_height() != expected_start || chunk.block_count() != expected_count {
            return Err(KnownHashError::ChunkSpan {
                index,
                expected_start,
                expected_count,
                actual_start: chunk.start_height(),
                actual_count: chunk.block_count(),
            });
        }

        let end = block::Height(expected_start.0 + (expected_count - 1));
        for (height, checkpoint) in checkpoints
            .iter_cloned()
            .skip_while(|(height, _)| *height < expected_start)
            .take_while(|(height, _)| *height <= end)
        {
            let (list, _hint) = chunk.get(height).expect("height is inside the chunk");
            if list != checkpoint {
                return Err(KnownHashError::CheckpointMismatch {
                    height,
                    list,
                    checkpoint,
                });
            }
        }

        Ok(chunk)
    }
}

/// Errors loading or verifying a known-hash list.
#[derive(Error, Debug)]
pub enum KnownHashError {
    /// The spec is malformed.
    #[error("invalid known-hash list spec: {0}")]
    InvalidSpec(String),

    /// The list doesn't end at a checkpoint, so its last hashes aren't
    /// anchored to the checkpoint list.
    #[error("known-hash list max height {max_height:?} is not a checkpoint height")]
    NotAnchored {
        /// The spec's max height.
        max_height: block::Height,
    },

    /// The requested chunk index is past the end of the list.
    #[error("known-hash chunk {index} is past the end of the list")]
    ChunkIndex {
        /// The requested chunk index.
        index: usize,
    },

    /// A chunk file couldn't be read.
    #[error("could not read known-hash chunk {path}: {error}")]
    ChunkRead {
        /// The chunk file path.
        path: PathBuf,
        /// The I/O error.
        error: std::io::Error,
    },

    /// A chunk's SHA-256 doesn't match the spec.
    #[error(
        "known-hash chunk {index} failed its integrity check: expected SHA-256 \
         {expected}, got {actual}; the file is corrupt, tampered with, or from \
         another release"
    )]
    ChunkHashMismatch {
        /// The chunk index.
        index: usize,
        /// The SHA-256 pinned in the spec.
        expected: String,
        /// The SHA-256 of the chunk bytes.
        actual: String,
    },

    /// A chunk's bytes are malformed.
    #[error("known-hash chunk {index} is malformed: {error}")]
    ChunkFormat {
        /// The chunk index.
        index: usize,
        /// The structural error.
        #[source]
        error: ChunkFormatError,
    },

    /// A chunk covers different heights from the ones the spec assigns to it.
    #[error(
        "known-hash chunk {index} covers {actual_count} blocks from {actual_start:?}, \
         expected {expected_count} blocks from {expected_start:?}"
    )]
    ChunkSpan {
        /// The chunk index.
        index: usize,
        /// The first height the spec assigns to the chunk.
        expected_start: block::Height,
        /// The number of blocks the spec assigns to the chunk.
        expected_count: u32,
        /// The first height in the chunk header.
        actual_start: block::Height,
        /// The number of blocks in the chunk header.
        actual_count: u32,
    },

    /// A chunk contradicts the checkpoint list.
    #[error(
        "known-hash list has {list} at height {height:?}, but the checkpoint \
         list has {checkpoint}"
    )]
    CheckpointMismatch {
        /// The checkpoint height.
        height: block::Height,
        /// The hash in the known-hash list.
        list: block::Hash,
        /// The hash in the checkpoint list.
        checkpoint: block::Hash,
    },
}

/// A verified known-hash list in a local directory.
///
/// [`KnownHashList::open`] verifies every chunk, then drops them. Lookups load
/// chunks on demand, re-verifying each one, and keep at most two in memory
/// (about 10 MB for published lists).
pub struct KnownHashList<'a> {
    spec: KnownHashListSpec<'a>,
    checkpoints: Arc<CheckpointList>,
    dir: PathBuf,
    /// Loaded chunks with their indexes, least recently used first.
    resident: Vec<(usize, Chunk)>,
}

impl<'a> KnownHashList<'a> {
    /// The maximum number of chunks kept in memory.
    pub const MAX_RESIDENT_CHUNKS: usize = 2;

    /// Opens the list described by `spec` from the chunk files in `dir`,
    /// verifying every chunk against `spec` and `checkpoints`.
    ///
    /// Reads every chunk once, but keeps none of them in memory.
    pub fn open(
        spec: KnownHashListSpec<'a>,
        checkpoints: Arc<CheckpointList>,
        dir: impl Into<PathBuf>,
    ) -> Result<Self, KnownHashError> {
        spec.validate(&checkpoints)?;

        let list = Self {
            spec,
            checkpoints,
            dir: dir.into(),
            resident: Vec::new(),
        };

        for index in 0..spec.chunk_hashes.len() {
            list.load_chunk(index)?;
        }

        Ok(list)
    }

    /// The spec this list was opened with.
    pub fn spec(&self) -> &KnownHashListSpec<'a> {
        &self.spec
    }

    /// The highest height in the list.
    pub fn max_height(&self) -> block::Height {
        self.spec.max_height
    }

    /// Returns the hash and size hint of the block at `height`, or `None` past
    /// the end of the list.
    pub fn get(
        &mut self,
        height: block::Height,
    ) -> Result<Option<(block::Hash, u8)>, KnownHashError> {
        let Some(index) = self.spec.chunk_index(height) else {
            return Ok(None);
        };

        Ok(self.resident_chunk(index)?.get(height))
    }

    /// Returns the hash of the block at `height`, or `None` past the end of
    /// the list.
    pub fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, KnownHashError> {
        Ok(self.get(height)?.map(|(hash, _)| hash))
    }

    /// Returns the size hint of the block at `height`, or `None` past the end
    /// of the list. See [`size_hint_upper_bound`].
    pub fn size_hint(&mut self, height: block::Height) -> Result<Option<u8>, KnownHashError> {
        Ok(self.get(height)?.map(|(_, hint)| hint))
    }

    /// Drops loaded chunks that only cover heights below `height`.
    pub fn release_below(&mut self, height: block::Height) {
        let keep_from = (height.0 / self.spec.chunk_blocks) as usize;
        self.resident.retain(|(index, _)| *index >= keep_from);
    }

    /// The number of chunks currently in memory.
    pub fn resident_chunks(&self) -> usize {
        self.resident.len()
    }

    /// Returns chunk `index`, loading it if needed and evicting the least
    /// recently used chunk beyond [`Self::MAX_RESIDENT_CHUNKS`].
    fn resident_chunk(&mut self, index: usize) -> Result<&Chunk, KnownHashError> {
        if let Some(pos) = self.resident.iter().position(|(i, _)| *i == index) {
            let entry = self.resident.remove(pos);
            self.resident.push(entry);
        } else {
            let chunk = self.load_chunk(index)?;
            self.resident.push((index, chunk));
            if self.resident.len() > Self::MAX_RESIDENT_CHUNKS {
                self.resident.remove(0);
            }
        }

        Ok(&self
            .resident
            .last()
            .expect("the chunk was just pushed or moved to the end")
            .1)
    }

    /// Reads and verifies chunk `index`.
    fn load_chunk(&self, index: usize) -> Result<Chunk, KnownHashError> {
        let path = self.dir.join(self.spec.chunk_file_name(index));
        let bytes = fs::read(&path).map_err(|error| KnownHashError::ChunkRead { path, error })?;

        self.spec.verify_chunk(index, bytes, &self.checkpoints)
    }
}

impl std::fmt::Debug for KnownHashList<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownHashList")
            .field("max_height", &self.spec.max_height)
            .field("dir", &self.dir)
            .field(
                "resident_chunks",
                &self.resident.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            )
            .finish()
    }
}
