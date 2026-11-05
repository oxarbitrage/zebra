//! Tests for the known-hashes tool.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use color_eyre::eyre::{eyre, Result};
use hex::FromHex;
use tempfile::tempdir;

use zebra_chain::{
    block::{self, Height},
    parameters::{
        checkpoint::list::CheckpointList,
        known_hashes::{chunk_sha256_hex, Chunk, KnownHashListSpec},
        Network,
    },
};

use crate::{
    emit::{emit_chunks, spec_constant_block, verify_emitted},
    source::BlockSource,
    sweep::{run_sweep, SweepFiles},
};

/// A [`BlockSource`] over a fixed in-memory chain, counting every fetch.
#[derive(Clone)]
struct MockSource {
    /// The mock chain: height to block hash and serialized size.
    blocks: Arc<BTreeMap<u32, (block::Hash, u32)>>,

    /// The total number of hash and size fetches.
    calls: Arc<AtomicUsize>,
}

impl MockSource {
    fn new(blocks: BTreeMap<u32, (block::Hash, u32)>) -> Self {
        Self {
            blocks: Arc::new(blocks),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl BlockSource for MockSource {
    async fn tip_height(&self) -> Result<u32> {
        self.blocks
            .keys()
            .next_back()
            .copied()
            .ok_or_else(|| eyre!("the mock chain is empty"))
    }

    async fn block_hash(&self, height: u32) -> Result<block::Hash> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(&height)
            .map(|(hash, _size)| *hash)
            .ok_or_else(|| eyre!("no mock block at height {height}"))
    }

    async fn block_size(&self, height: u32) -> Result<u32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blocks
            .get(&height)
            .map(|(_hash, size)| *size)
            .ok_or_else(|| eyre!("no mock block at height {height}"))
    }
}

/// A 5-block mock mainnet chain whose genesis block matches the real mainnet
/// genesis hash, so the height 0 checkpoint verifies.
fn mock_mainnet_chain() -> BTreeMap<u32, (block::Hash, u32)> {
    let mut blocks = BTreeMap::new();
    blocks.insert(0, (Network::Mainnet.genesis_hash(), 1_687));
    for height in 1..=4u32 {
        let byte = u8::try_from(height).expect("test heights are below 256");
        blocks.insert(height, (block::Hash([byte; 32]), 1_000 + height));
    }
    blocks
}

/// Five `(hash, hint)` blocks, and a checkpoint list over heights 0 and 4.
fn synthetic_list() -> (Vec<(block::Hash, u8)>, Arc<CheckpointList>) {
    let blocks: Vec<(block::Hash, u8)> = (1..=5u8).map(|i| (block::Hash([i; 32]), i)).collect();
    let checkpoints =
        CheckpointList::from_list([(Height(0), blocks[0].0), (Height(4), blocks[4].0)])
            .expect("valid checkpoints");

    (blocks, Arc::new(checkpoints))
}

#[test]
fn display_hex_parses_to_internal_byte_order() {
    let display = "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08";

    let hash: block::Hash = display.parse().expect("valid block hash hex");

    // RPC responses are display-order hex; the internal order is its reverse.
    let mut expected = <[u8; 32]>::from_hex(display).expect("valid hex");
    expected.reverse();

    assert_eq!(hash.0, expected);
    assert_eq!(hash, Network::Mainnet.genesis_hash());
    assert_eq!(hash.to_string(), display);
}

#[test]
fn emitted_chunks_split_at_chunk_boundaries_and_verify() -> Result<()> {
    let dir = tempdir()?;
    let (blocks, checkpoints) = synthetic_list();

    let chunk_hashes = emit_chunks(dir.path(), "test-list", &blocks, 2)?;
    assert_eq!(chunk_hashes.len(), 3, "5 blocks in chunks of 2");

    let last = std::fs::read(dir.path().join("test-list-02.bin"))?;
    assert_eq!(chunk_hashes[2], chunk_sha256_hex(&last));
    let last = Chunk::parse(last)?;
    assert_eq!(last.start_height(), Height(4));
    assert_eq!(last.block_count(), 1);
    assert!(!dir.path().join("test-list-03.bin").exists());

    let chunk_hashes: Vec<&str> = chunk_hashes.iter().map(String::as_str).collect();
    let spec = KnownHashListSpec {
        max_height: Height(4),
        chunk_blocks: 2,
        file_prefix: "test-list",
        chunk_hashes: &chunk_hashes,
    };
    verify_emitted(dir.path(), spec, checkpoints)?;

    Ok(())
}

#[test]
fn emitted_chunks_that_contradict_a_checkpoint_fail_verification() -> Result<()> {
    let dir = tempdir()?;
    let (mut blocks, checkpoints) = synthetic_list();
    blocks[4].0 = block::Hash([0xee; 32]);

    let chunk_hashes = emit_chunks(dir.path(), "test-list", &blocks, 2)?;
    let chunk_hashes: Vec<&str> = chunk_hashes.iter().map(String::as_str).collect();
    let spec = KnownHashListSpec {
        max_height: Height(4),
        chunk_blocks: 2,
        file_prefix: "test-list",
        chunk_hashes: &chunk_hashes,
    };

    let error = verify_emitted(dir.path(), spec, checkpoints).unwrap_err();
    assert!(format!("{error:?}").contains("checkpoint"), "{error:?}");

    Ok(())
}

#[test]
fn emit_chunks_rejects_empty_input() -> Result<()> {
    let dir = tempdir()?;
    assert!(emit_chunks(dir.path(), "test-list", &[], 2).is_err());
    Ok(())
}

#[test]
fn spec_constant_block_matches_spec() {
    let hashes = ["aa".repeat(32), "bb".repeat(32)];
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    let spec = KnownHashListSpec {
        max_height: Height(150_001),
        chunk_blocks: 150_000,
        file_prefix: "test-known-hashes",
        chunk_hashes: &hashes,
    };

    let constant_block = spec_constant_block("TESTNET", &spec);

    for expected in [
        "pub const TESTNET_KNOWN_HASHES: KnownHashListSpec<'static> = KnownHashListSpec {",
        "max_height: block::Height(150001),",
        "chunk_blocks: 150000,",
        "file_prefix: \"test-known-hashes\",",
        &format!("\"{}\",", "aa".repeat(32)),
        &format!("\"{}\",", "bb".repeat(32)),
    ] {
        assert!(constant_block.contains(expected), "{constant_block}");
    }
}

#[test]
fn sweep_files_persist_sparse_entries() -> Result<()> {
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    {
        let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
        assert!(files.hash(0).is_none());
        assert!(files.size(0).is_none());

        files.set_hash(3, block::Hash([3; 32]))?;
        files.set_size(3, 1_234)?;
        files.flush()?;
    }

    // Reopening sees the entry, and the sparse gap below it stays unfetched.
    let files = SweepFiles::open(&hash_path, &sizes_path)?;
    assert_eq!(files.hash(3), Some(block::Hash([3; 32])));
    assert_eq!(files.size(3), Some(1_234));
    assert!(files.hash(2).is_none());
    assert!(files.size(2).is_none());
    assert!(files.hash(4).is_none());

    // Completeness checks name the first missing height.
    let err = files.complete_hashes(3).unwrap_err();
    assert!(err.to_string().contains("height 0"), "{err}");
    let err = files.complete_sizes(3).unwrap_err();
    assert!(err.to_string().contains("height 0"), "{err}");

    Ok(())
}

#[tokio::test]
async fn sweep_fetches_verifies_and_resumes() -> Result<()> {
    let network = Network::Mainnet;
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    let chain = mock_mainnet_chain();
    let source = MockSource::new(chain.clone());

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    run_sweep(&source, &network, &mut files, 0, 4, 8).await?;

    // Each of the 5 heights needs one hash and one size fetch.
    assert_eq!(source.calls(), 10);
    for (height, (hash, size)) in &chain {
        assert_eq!(files.hash(*height), Some(*hash));
        assert_eq!(files.size(*height), Some(*size));
    }

    assert_eq!(files.complete_hashes(4)?.len(), 5);
    assert_eq!(files.complete_sizes(4)?.len(), 5);
    drop(files);

    // A resumed sweep finds nothing missing and makes no fetches.
    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    run_sweep(&source, &network, &mut files, 0, 4, 8).await?;
    assert_eq!(source.calls(), 10);

    Ok(())
}

#[tokio::test]
async fn sweep_fails_on_checkpoint_mismatch() -> Result<()> {
    let network = Network::Mainnet;
    let dir = tempdir()?;
    let hash_path = dir.path().join("hashes.bin");
    let sizes_path = dir.path().join("sizes.u32le");

    // Height 0 is in the mainnet checkpoint list, and this hash is wrong.
    let mut chain = mock_mainnet_chain();
    chain.insert(0, (block::Hash([0xff; 32]), 1_687));
    let source = MockSource::new(chain);

    let mut files = SweepFiles::open(&hash_path, &sizes_path)?;
    let err = run_sweep(&source, &network, &mut files, 0, 4, 8)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("anchor mismatch at height 0"),
        "{err}"
    );

    Ok(())
}
