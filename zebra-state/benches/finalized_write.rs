//! Benchmarks the finalized state's RocksDB write path under sustained load.
//!
//! The benchmark opens an ephemeral state database and writes a long sequence of blocks,
//! one write batch per block, the way the finalized state writer does during sync. It writes
//! enough data to fill many memtables, so the measured time includes the flushes and
//! compactions RocksDB runs in the background, and any write stalls they cause.
//!
//! It exercises the storage layer, not block validation: each block writes the real raw
//! header and transaction bytes of a mainnet test vector into the block and transaction
//! column families, with synthetic heights and unique pseudo-random hashes. Note commitment
//! trees, UTXOs and nullifiers are not written.
//!
//! Run it with:
//!
//! ```sh
//! cargo bench -p zebra-state --features proptest-impl --bench finalized_write
//! ```

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::{
    io::Cursor,
    time::{Duration, Instant},
};

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction,
};
use zebra_state::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    Config, DiskDb, DiskWriteBatch, FromDisk, IntoDisk, RawBytes, TransactionLocation,
    TypedColumnFamily, STATE_COLUMN_FAMILIES_IN_CODE,
};
use zebra_test::vectors::MAINNET_BLOCKS;

/// The number of blocks written in each measured run.
///
/// The mainnet test vectors average about 10 KiB per block, so each run writes about 1 GiB.
const BLOCK_COUNT: u32 = 100_000;

/// The raw bytes of one test vector block, split the way the finalized state stores them.
struct BlockBytes {
    header: Vec<u8>,
    transactions: Vec<Vec<u8>>,
}

/// Returns every mainnet test vector block, split into header and transaction bytes.
fn mainnet_block_bytes() -> Vec<BlockBytes> {
    MAINNET_BLOCKS
        .values()
        .map(|bytes| {
            let block = Block::zcash_deserialize(Cursor::new(bytes)).expect("test vector is valid");

            BlockBytes {
                header: block
                    .header
                    .zcash_serialize_to_vec()
                    .expect("header serializes"),
                transactions: block
                    .transactions
                    .iter()
                    .map(|tx| tx.zcash_serialize_to_vec().expect("transaction serializes"))
                    .collect(),
            }
        })
        .collect()
}

/// Returns 32 pseudo-random bytes derived from `seed`, so hash-keyed column families get
/// unique keys spread across the key space, like real block and transaction hashes.
fn pseudo_random_hash(seed: u64) -> [u8; 32] {
    let mut state = seed;
    let mut hash = [0; 32];

    for chunk in hash.chunks_exact_mut(8) {
        // splitmix64
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }

    hash
}

/// Adds `key` and `value` to `cf_name` in `batch`.
fn insert<Key, Value>(
    db: &DiskDb,
    batch: &mut DiskWriteBatch,
    cf_name: &str,
    key: &Key,
    value: &Value,
) where
    Key: IntoDisk + FromDisk + std::fmt::Debug,
    Value: IntoDisk + FromDisk,
{
    // The typed writer only borrows the batch, so dropping it keeps the insert.
    let _ = TypedColumnFamily::<Key, Value>::new(db, cf_name)
        .expect("column family exists")
        .with_batch_for_writing(batch)
        .zs_insert(key, value);
}

/// Writes `block` to `db` at `height`, as one write batch.
fn write_block(db: &DiskDb, height: u32, block: &BlockBytes) {
    let height = Height(height);
    let block_hash = block::Hash(pseudo_random_hash(u64::from(height.0) << 16));

    let mut batch = DiskWriteBatch::new();

    insert::<Height, block::Hash>(db, &mut batch, "hash_by_height", &height, &block_hash);
    insert::<block::Hash, Height>(db, &mut batch, "height_by_hash", &block_hash, &height);
    insert::<Height, RawBytes>(
        db,
        &mut batch,
        "block_header_by_height",
        &height,
        &RawBytes::new_raw_bytes(block.header.clone()),
    );

    for (index, tx_bytes) in block.transactions.iter().enumerate() {
        let location = TransactionLocation::from_usize(height, index);
        let tx_hash = transaction::Hash(pseudo_random_hash(
            (u64::from(height.0) << 16) + index as u64 + 1,
        ));

        insert::<TransactionLocation, RawBytes>(
            db,
            &mut batch,
            "tx_by_loc",
            &location,
            &RawBytes::new_raw_bytes(tx_bytes.clone()),
        );
        insert::<TransactionLocation, transaction::Hash>(
            db,
            &mut batch,
            "hash_by_tx_loc",
            &location,
            &tx_hash,
        );
        insert::<transaction::Hash, TransactionLocation>(
            db,
            &mut batch,
            "tx_loc_by_hash",
            &tx_hash,
            &location,
        );
    }

    TypedColumnFamily::<Height, block::Hash>::new(db, "hash_by_height")
        .expect("column family exists")
        .take_batch_for_writing(batch)
        .write_batch()
        .expect("writing to an ephemeral database succeeds");
}

fn finalized_write(c: &mut Criterion) {
    let blocks = mainnet_block_bytes();

    let mut group = c.benchmark_group("finalized_write");

    // Each run takes seconds, so take the minimum number of samples, with one run each.
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_secs(1))
        .throughput(Throughput::Elements(BLOCK_COUNT.into()));

    group.bench_function(BenchmarkId::new("blocks", BLOCK_COUNT), |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;

            for _ in 0..iterations {
                // Open the database without the finalized state's format checks, which
                // would reject the synthetic chain and add reads to the measurement.
                let db = DiskDb::new(
                    &Config::ephemeral(),
                    STATE_DATABASE_KIND,
                    &state_database_format_version_in_code(),
                    &Network::Mainnet,
                    STATE_COLUMN_FAMILIES_IN_CODE
                        .iter()
                        .map(ToString::to_string),
                    false,
                )
                .expect("opening an ephemeral database succeeds");

                let start = Instant::now();
                for height in 0..BLOCK_COUNT {
                    write_block(&db, height, &blocks[height as usize % blocks.len()]);
                }
                elapsed += start.elapsed();

                drop(db);
            }

            elapsed
        })
    });

    group.finish();
}

criterion_group!(benches, finalized_write);
criterion_main!(benches);
