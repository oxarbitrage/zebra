//! Benchmarks for block transaction verification latency through the batch verifiers.
//!
//! Measures the wall time from submitting a set of shielded transactions to the real
//! [`BlockTxVerifier`] until every one of them has verified. The proofs go through the global
//! Sapling and Halo2 batch verifiers, so the result includes the time a proof waits in a pending
//! batch, not just the cost of verifying it. The other benches in this directory measure that cost
//! directly and cannot see batching delays.
//!
//! # Benchmark group
//!
//! `batch_latency`, with one function per arrival pattern:
//!
//! - `sapling_burst/16`, `orchard_burst/16`: every transaction submitted at once, as when a block
//!   arrives. A batch that waits for more input than will come adds that wait to the block.
//! - `orchard_gaps/2ms`, `orchard_gaps/8ms`: one transaction every 2 or 8 ms, as when transactions
//!   trickle in from the network.
//! - `sapling_stream/128`: a burst larger than one batch, where throughput matters more than
//!   latency.
//!
//! Each iteration is one such "block". Iterations are separated by an untimed pause, so pending
//! batch timers and rayon work from the previous iteration do not leak into the next.
//!
//! # Test vectors
//!
//! Only shielded-only transactions are used (no transparent inputs, no Sprout), because the
//! benchmark has no state to look up spent outputs. The Mainnet vectors contain a single
//! Orchard-only transaction, so the Orchard cases submit 16 copies of it.
//!
//! # Proof caches
//!
//! The verifiers skip proofs they have already verified. This benchmark verifies the same
//! transactions on every iteration, so it bypasses the caches with the hidden `bench` feature
//! hook; otherwise every iteration after the first would measure cache hits. The same hook
//! reports Halo2 batch sizes, printed once per Orchard case.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use futures::{stream::FuturesUnordered, StreamExt};
use tokio::runtime::Runtime;
use tower::{service_fn, Service};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
};
use zebra_consensus::{
    bench,
    transaction::{BlockRequest, BlockTxVerifier},
    BoxError,
};

/// A transaction with the height and block time it was mined at.
type Tx = (Arc<Transaction>, Height, DateTime<Utc>);

/// The pause between iterations, so each one starts with empty batches.
const BETWEEN_BLOCKS: Duration = Duration::from_millis(20);

/// Returns up to `max` shielded-only Mainnet vector transactions matching `pick`.
fn shielded_only(pick: impl Fn(&Transaction) -> bool, max: usize) -> Vec<Tx> {
    let mut txs = Vec::new();

    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            if tx.is_coinbase()
                || !tx.inputs().is_empty()
                || tx.has_sprout_joinsplit_data()
                || !pick(tx)
            {
                continue;
            }

            txs.push((tx.clone(), Height(*height), block.header.time));
            if txs.len() == max {
                return txs;
            }
        }
    }

    txs
}

/// Submits every transaction at its offset from the start, and returns the time until all of
/// them have verified.
async fn verify_block<V>(verifier: &mut V, txs: &[Tx], offsets: &[Duration]) -> Duration
where
    V: Service<BlockRequest>,
    V::Error: std::fmt::Debug,
{
    let start = Instant::now();

    // The verifier is always ready, and its futures do all the work, so requests are created
    // up front and only polled once their offset has passed.
    let mut pending: FuturesUnordered<_> = txs
        .iter()
        .zip(offsets)
        .map(|((tx, height, time), offset)| {
            let verify = verifier.call(BlockRequest {
                transaction_hash: tx.hash(),
                transaction: tx.clone(),
                known_utxos: Arc::new(HashMap::new()),
                height: *height,
                time: *time,
            });
            let offset = *offset;

            async move {
                if !offset.is_zero() {
                    tokio::time::sleep_until((start + offset).into()).await;
                }
                verify.await
            }
        })
        .collect();

    while let Some(result) = pending.next().await {
        result.expect("vector transaction must verify");
    }

    start.elapsed()
}

// Prints the Halo2 batch sizes to stderr, as context for the timings in the bench log.
#[allow(clippy::print_stderr)]
fn bench_batch_latency(c: &mut Criterion) {
    bench::set_proof_cache_bypass(true);

    // The global batch verifiers spawn their workers on the runtime that first uses them,
    // so every case shares one runtime.
    let rt = Runtime::new().expect("runtime builds");

    // Shielded-only transactions never reach the state, so any state request is a bug.
    let state = service_fn(|_: zebra_state::Request| async {
        Err::<zebra_state::Response, BoxError>("unexpected state request".into())
    });
    let mut verifier = BlockTxVerifier::new(&Network::Mainnet, state);

    let sapling = shielded_only(
        |tx| tx.has_sapling_shielded_data() && !tx.has_orchard_shielded_data(),
        16,
    );
    let orchard: Vec<Tx> = shielded_only(
        |tx| tx.has_orchard_shielded_data() && !tx.has_sapling_shielded_data(),
        16,
    )
    .into_iter()
    .cycle()
    .take(16)
    .collect();
    let sapling_stream: Vec<Tx> = sapling.iter().cycle().take(128).cloned().collect();

    assert_eq!(sapling.len(), 16, "Sapling-only vectors");
    assert_eq!(orchard.len(), 16, "Orchard-only vectors");

    let burst = |n: usize| vec![Duration::ZERO; n];
    let gaps = |n: u32, gap: Duration| (0..n).map(|i| gap * i).collect::<Vec<_>>();

    let cases = [
        (("sapling_burst", "16"), &sapling, burst(16)),
        (("orchard_burst", "16"), &orchard, burst(16)),
        (
            ("orchard_gaps", "2ms"),
            &orchard,
            gaps(16, Duration::from_millis(2)),
        ),
        (
            ("orchard_gaps", "8ms"),
            &orchard,
            gaps(16, Duration::from_millis(8)),
        ),
        (("sapling_stream", "128"), &sapling_stream, burst(128)),
    ];

    let mut group = c.benchmark_group("batch_latency");
    // Each iteration takes tens to hundreds of milliseconds.
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(20)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));

    for ((function, parameter), txs, offsets) in cases {
        let id = BenchmarkId::new(function, parameter);
        // Load the proving parameters and start the batch workers, then report the batch shape.
        rt.block_on(async {
            verify_block(&mut verifier, txs, &offsets).await;
            tokio::time::sleep(BETWEEN_BLOCKS).await;
            bench::take_halo2_batch_sizes();
            verify_block(&mut verifier, txs, &offsets).await;
        });
        let halo2 = bench::take_halo2_batch_sizes();
        if !halo2.is_empty() {
            eprintln!("batch_latency/{function}/{parameter}: halo2 batch sizes {halo2:?}");
        }

        group.bench_function(id, |b| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        tokio::time::sleep(BETWEEN_BLOCKS).await;
                        total += verify_block(&mut verifier, txs, &offsets).await;
                    }
                    total
                })
            })
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().noise_threshold(0.1);
    targets = bench_batch_latency
}
criterion_main!(benches);
