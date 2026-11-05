//! Tests for known-hash list chunks and the verifying loader.
//!
//! The fixtures are built from the continuous Mainnet block test vectors, using
//! small chunks so that a few blocks span several chunk files.

use std::{path::Path, sync::Arc};

use tempfile::TempDir;

use crate::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use super::*;

/// The chunk size used by the fixtures: 11 blocks become chunks of 4, 4 and 3.
const TEST_CHUNK_BLOCKS: u32 = 4;

/// Returns the `(hash, size hint)` of each continuous Mainnet test block,
/// starting at genesis, after checking that the blocks form a chain.
fn real_chain() -> Vec<(block::Hash, u8)> {
    let mut chain: Vec<(block::Hash, u8)> = Vec::new();

    for (height, bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("test block is valid");
        assert_eq!(block.coinbase_height(), Some(Height(*height)));
        if let Some((parent, _)) = chain.last() {
            assert_eq!(block.header.previous_block_hash, *parent);
        }

        let hint = size_hint(bytes.len() as u64).expect("test block size is valid");
        chain.push((block.hash(), hint));
    }

    assert!(chain.len() >= 11, "the fixtures need blocks 0..=10");
    chain.truncate(11);
    chain
}

/// A checkpoint list with checkpoints at heights 0, 5 and 10 of `chain`.
fn checkpoints(chain: &[(block::Hash, u8)]) -> Arc<CheckpointList> {
    Arc::new(
        CheckpointList::from_list([0, 5, 10].map(|h| (Height(h), chain[h as usize].0)))
            .expect("valid checkpoint list"),
    )
}

/// Writes `chain` as chunks of `chunk_blocks` into `dir`, returning each
/// chunk's SHA-256 hex.
fn write_chunks(dir: &Path, chain: &[(block::Hash, u8)], chunk_blocks: u32) -> Vec<String> {
    chain
        .chunks(chunk_blocks as usize)
        .enumerate()
        .map(|(index, blocks)| {
            let start = Height(index as u32 * chunk_blocks);
            let bytes = Chunk::encode(start, blocks);
            std::fs::write(dir.join(chunk_file_name("test-list", index)), &bytes)
                .expect("test dir is writable");
            chunk_sha256_hex(&bytes)
        })
        .collect()
}

/// A spec for a list of `len` blocks in [`TEST_CHUNK_BLOCKS`] chunks.
fn spec<'a>(len: usize, chunk_hashes: &'a [&'a str]) -> KnownHashListSpec<'a> {
    KnownHashListSpec {
        max_height: Height(len as u32 - 1),
        chunk_blocks: TEST_CHUNK_BLOCKS,
        file_prefix: "test-list",
        chunk_hashes,
    }
}

/// Writes `chain` into a new temporary directory and opens it against the
/// fixture checkpoints of `checkpoint_chain`.
///
/// Panics on test setup failures, and returns the loader's result.
#[allow(clippy::unwrap_in_result)]
fn write_and_open(
    chain: &[(block::Hash, u8)],
    checkpoint_chain: &[(block::Hash, u8)],
) -> Result<(), KnownHashError> {
    let dir = TempDir::new().expect("temp dir");
    let hashes = write_chunks(dir.path(), chain, TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();

    KnownHashList::open(
        spec(chain.len(), &hashes),
        checkpoints(checkpoint_chain),
        dir.path(),
    )
    .map(|_| ())
}

#[test]
fn size_hints_are_upper_bounds() {
    let _init_guard = zebra_test::init();

    assert_eq!(size_hint(1), Some(1));
    assert_eq!(size_hint(SIZE_HINT_UNIT.into()), Some(1));
    assert_eq!(size_hint(u64::from(SIZE_HINT_UNIT) + 1), Some(2));
    assert_eq!(size_hint(MAX_BLOCK_BYTES), Some(255));

    assert_eq!(size_hint(0), None);
    assert_eq!(size_hint(MAX_BLOCK_BYTES + 1), None);

    for size in [1, 1_687, 7_844, 7_845, 100_000, 1_999_999, MAX_BLOCK_BYTES] {
        let hint = size_hint(size).expect("valid size");
        assert!(size_hint_upper_bound(hint) >= size);
        assert!(size_hint_upper_bound(hint - 1) < size);
    }
}

#[test]
fn chunk_round_trip() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let bytes = Chunk::encode(Height(4), &chain[4..8]);
    assert_eq!(bytes.len(), chunk::HEADER_LEN + 4 * 33);

    let chunk = Chunk::parse(bytes.clone()).expect("encoded chunk parses");
    assert_eq!(chunk.start_height(), Height(4));
    assert_eq!(chunk.block_count(), 4);
    assert_eq!(chunk.as_bytes(), bytes.as_slice());

    for (rel, expected) in chain[4..8].iter().enumerate() {
        assert_eq!(chunk.get(Height(4 + rel as u32)), Some(*expected));
    }
    assert_eq!(chunk.get(Height(3)), None);
    assert_eq!(chunk.get(Height(8)), None);

    let heights: Vec<Height> = chunk.hashes().map(|(height, _)| height).collect();
    assert_eq!(heights, [4, 5, 6, 7].map(Height));
}

#[test]
fn chunk_parse_rejects_malformed_bytes() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let good = Chunk::encode(Height(0), &chain[0..3]);

    let modified = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = good.clone();
        f(&mut bytes);
        Chunk::parse(bytes).expect_err("malformed chunk is rejected")
    };

    assert_eq!(
        Chunk::parse(good[..10].to_vec()),
        Err(ChunkFormatError::TooShort(10))
    );
    assert_eq!(modified(&|b| b[0] = b'X'), ChunkFormatError::BadMagic);
    assert_eq!(
        modified(&|b| b[4] = 2),
        ChunkFormatError::UnsupportedVersion(2)
    );
    assert_eq!(modified(&|b| b[5] = 1), ChunkFormatError::NonZeroReserved);
    assert_eq!(
        modified(&|b| b[12..16].copy_from_slice(&0u32.to_le_bytes())),
        ChunkFormatError::BadBlockCount(0)
    );
    assert_eq!(
        modified(&|b| b[12..16].copy_from_slice(&(HASHES_PER_CHUNK + 1).to_le_bytes())),
        ChunkFormatError::BadBlockCount(HASHES_PER_CHUNK + 1)
    );
    assert!(matches!(
        modified(&|b| b[8..12].copy_from_slice(&Height::MAX.0.to_le_bytes())),
        ChunkFormatError::HeightOverflow { .. }
    ));
    assert!(matches!(
        modified(&|b| {
            b.pop();
        }),
        ChunkFormatError::BadLength { .. }
    ));
    assert!(matches!(
        modified(&|b| b.push(1)),
        ChunkFormatError::BadLength { .. }
    ));
    assert_eq!(
        modified(&|b| b[16 + 32..16 + 64].fill(0)),
        ChunkFormatError::NullHash(Height(1))
    );
    let last_hint = good.len() - 1;
    assert_eq!(
        modified(&|b| b[last_hint] = 0),
        ChunkFormatError::ZeroSizeHint(Height(2))
    );
}

#[test]
fn open_verifies_and_serves_real_chain() -> color_eyre::Result<()> {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let dir = TempDir::new()?;
    let hashes = write_chunks(dir.path(), &chain, TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    assert_eq!(hashes.len(), 3);

    let mut list =
        KnownHashList::open(spec(chain.len(), &hashes), checkpoints(&chain), dir.path())?;

    assert_eq!(list.max_height(), Height(10));
    assert_eq!(list.resident_chunks(), 0, "open keeps no chunks in memory");

    assert_eq!(list.hash(Height(0))?, Some(Network::Mainnet.genesis_hash()));
    for (height, (hash, hint)) in chain.iter().enumerate() {
        let height = Height(height as u32);
        assert_eq!(list.get(height)?, Some((*hash, *hint)));
        assert_eq!(list.size_hint(height)?, Some(*hint));
        assert!(list.resident_chunks() <= KnownHashList::MAX_RESIDENT_CHUNKS);
    }
    assert_eq!(list.get(Height(11))?, None);

    // The sequential walk leaves the last two chunks in memory.
    assert_eq!(list.resident_chunks(), 2);
    list.release_below(Height(8));
    assert_eq!(list.resident_chunks(), 1);

    Ok(())
}

#[test]
fn open_with_real_mainnet_checkpoints() -> color_eyre::Result<()> {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let checkpoints = Network::Mainnet.checkpoint_list();
    let dir = TempDir::new()?;

    // A genesis-only list ends at the genesis checkpoint.
    let hashes = write_chunks(dir.path(), &chain[..1], TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    KnownHashList::open(spec(1, &hashes), checkpoints.clone(), dir.path())?;

    // Height 10 isn't a Mainnet checkpoint, so a list ending there is rejected.
    let hashes = write_chunks(dir.path(), &chain, TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let error = KnownHashList::open(spec(chain.len(), &hashes), checkpoints, dir.path())
        .expect_err("list must end at a checkpoint");
    assert!(matches!(
        error,
        KnownHashError::NotAnchored {
            max_height: Height(10)
        }
    ));

    Ok(())
}

#[test]
fn tampered_chunk_is_rejected() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let dir = TempDir::new().expect("temp dir");
    let hashes = write_chunks(dir.path(), &chain, TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let spec = spec(chain.len(), &hashes);

    // Change one size hint in chunk 1: the chunk is still well formed and
    // agrees with every checkpoint, so only its pinned SHA-256 catches it.
    let path = dir.path().join(spec.chunk_file_name(1));
    let mut bytes = std::fs::read(&path).expect("chunk exists");
    *bytes.last_mut().expect("chunk isn't empty") ^= 1;
    std::fs::write(&path, &bytes).expect("test dir is writable");

    let error = KnownHashList::open(spec, checkpoints(&chain), dir.path())
        .expect_err("tampered chunk is rejected");
    assert!(
        matches!(error, KnownHashError::ChunkHashMismatch { index: 1, .. }),
        "{error:?}"
    );

    // A missing chunk is also an error.
    std::fs::remove_file(&path).expect("chunk exists");
    let error = KnownHashList::open(spec, checkpoints(&chain), dir.path())
        .expect_err("missing chunk is rejected");
    assert!(
        matches!(error, KnownHashError::ChunkRead { .. }),
        "{error:?}"
    );
}

#[test]
fn chunk_tampered_after_open_is_rejected() -> color_eyre::Result<()> {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let dir = TempDir::new()?;
    let hashes = write_chunks(dir.path(), &chain, TEST_CHUNK_BLOCKS);
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let spec = spec(chain.len(), &hashes);

    let mut list = KnownHashList::open(spec, checkpoints(&chain), dir.path())?;
    assert!(list.hash(Height(0))?.is_some());

    // Lookups re-verify each chunk they load.
    let path = dir.path().join(spec.chunk_file_name(2));
    let mut bytes = std::fs::read(&path)?;
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, &bytes)?;

    let error = list
        .hash(Height(9))
        .expect_err("tampered chunk is rejected on load");
    assert!(
        matches!(error, KnownHashError::ChunkHashMismatch { index: 2, .. }),
        "{error:?}"
    );

    Ok(())
}

#[test]
fn checkpoint_contradicting_chunk_is_rejected() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();

    // These lists have correctly pinned SHA-256s, as if a bad list had passed
    // review: the checkpoint list must still reject them.
    for height in [0, 5, 10] {
        let mut bad = chain.clone();
        bad[height] = chain[if height == 0 { 1 } else { height - 1 }];

        let error = write_and_open(&bad, &chain).expect_err("contradicting list is rejected");
        assert!(
            matches!(
                error,
                KnownHashError::CheckpointMismatch { height: h, .. } if h == Height(height as u32)
            ),
            "{error:?}"
        );
    }

    // A wrong hash between checkpoints can't be detected without the blocks:
    // it is the pinned SHA-256 and review that cover it.
    let mut between = chain.clone();
    between[3] = between[2];
    write_and_open(&between, &chain).expect("hashes between checkpoints aren't checked");
}

#[test]
fn list_must_end_at_a_checkpoint() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let error = write_and_open(&chain[..10], &chain).expect_err("height 9 isn't a checkpoint");
    assert!(
        matches!(
            error,
            KnownHashError::NotAnchored {
                max_height: Height(9)
            }
        ),
        "{error:?}"
    );
}

#[test]
fn misplaced_chunk_is_rejected() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let dir = TempDir::new().expect("temp dir");
    let mut hashes = write_chunks(dir.path(), &chain, TEST_CHUNK_BLOCKS);

    // Chunk 1 holds the right blocks but claims the wrong start height, and
    // the spec pins those exact bytes.
    let bytes = Chunk::encode(Height(5), &chain[4..8]);
    std::fs::write(dir.path().join(chunk_file_name("test-list", 1)), &bytes)
        .expect("test dir is writable");
    hashes[1] = chunk_sha256_hex(&bytes);

    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let error = KnownHashList::open(spec(chain.len(), &hashes), checkpoints(&chain), dir.path())
        .expect_err("misplaced chunk is rejected");
    assert!(
        matches!(error, KnownHashError::ChunkSpan { index: 1, .. }),
        "{error:?}"
    );
}

#[test]
fn malformed_spec_is_rejected() {
    let _init_guard = zebra_test::init();

    let chain = real_chain();
    let checkpoints = checkpoints(&chain);
    let hash = "00".repeat(32);

    let too_few = [hash.as_str(); 2];
    assert!(matches!(
        spec(11, &too_few).validate(&checkpoints),
        Err(KnownHashError::InvalidSpec(_))
    ));

    let upper = "AB".repeat(32);
    let bad_hex = [hash.as_str(), hash.as_str(), upper.as_str()];
    assert!(matches!(
        spec(11, &bad_hex).validate(&checkpoints),
        Err(KnownHashError::InvalidSpec(_))
    ));

    let three = [hash.as_str(); 3];
    let mut zero_chunks = spec(11, &three);
    zero_chunks.chunk_blocks = 0;
    assert!(matches!(
        zero_chunks.validate(&checkpoints),
        Err(KnownHashError::InvalidSpec(_))
    ));

    assert!(spec(11, &three).validate(&checkpoints).is_ok());
    assert!(matches!(
        spec(11, &three).verify_chunk(3, Vec::new(), &checkpoints),
        Err(KnownHashError::ChunkIndex { index: 3 })
    ));
}

#[test]
fn spec_chunk_geometry() {
    let _init_guard = zebra_test::init();

    let hash = "00".repeat(32);
    let hashes = [hash.as_str(); 23];
    let spec = KnownHashListSpec {
        max_height: Height(3_373_206),
        chunk_blocks: HASHES_PER_CHUNK,
        file_prefix: "main-known-hashes",
        chunk_hashes: &hashes,
    };

    assert_eq!(spec.len(), 3_373_207);
    assert_eq!(spec.chunk_len(0), HASHES_PER_CHUNK);
    assert_eq!(spec.chunk_len(22), 3_373_207 - 22 * HASHES_PER_CHUNK);
    assert_eq!(spec.chunk_len(23), 0);
    assert_eq!(spec.chunk_start(22), Height(22 * HASHES_PER_CHUNK));
    assert_eq!(spec.chunk_index(Height(149_999)), Some(0));
    assert_eq!(spec.chunk_index(Height(150_000)), Some(1));
    assert_eq!(spec.chunk_index(Height(3_373_206)), Some(22));
    assert_eq!(spec.chunk_index(Height(3_373_207)), None);
    assert_eq!(spec.chunk_file_name(7), "main-known-hashes-07.bin");
}
