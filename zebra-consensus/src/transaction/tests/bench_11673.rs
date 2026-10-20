//! Latency harness for PR #11673 (tower-batch-control idle flush).
//!
//! Drives real Mainnet shielded-only transactions through [`BlockTxVerifier`] and the real
//! global Sapling / Halo2 batch workers. The proof caches are bypassed, so every item
//! (including duplicates) reaches the batch workers.
//!
//! Run: `cargo test --release -p zebra-consensus --lib bench_11673 -- --ignored --nocapture`

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use futures::{stream::FuturesUnordered, StreamExt};
use tower::Service;

use zebra_chain::{
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
};
use zebra_test::mock_service::MockService;

use super::{CacheTestState, HALO2_CACHE_VERIFIER, SAPLING_CACHE_VERIFIER};
use crate::transaction::{BlockRequest, BlockTxVerifier};

type Tx = (Arc<Transaction>, Height, DateTime<Utc>);

fn shielded_only(pick: impl Fn(&Transaction) -> bool, max: usize) -> Vec<Tx> {
    let mut out = Vec::new();
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
            out.push((tx.clone(), Height(*height), block.header.time));
            if out.len() == max {
                return out;
            }
        }
    }
    out
}

fn sapling_txs() -> Vec<Tx> {
    shielded_only(
        |tx| tx.has_sapling_shielded_data() && !tx.has_orchard_shielded_data(),
        16,
    )
}

fn orchard_txs() -> Vec<Tx> {
    let only = shielded_only(
        |tx| tx.has_orchard_shielded_data() && !tx.has_sapling_shielded_data(),
        16,
    );
    let any = shielded_only(|tx| tx.has_orchard_shielded_data(), 64);
    println!(
        "RESULT orchard candidates orchard_only={} orchard_any_shielded_only={}",
        only.len(),
        any.len()
    );
    // The vectors hold one Orchard-only shielded tx; the cache bypass lets 16 copies all verify.
    only.iter().cycle().take(16).cloned().collect()
}

/// One "block": submits every tx (after its offset) concurrently, returns wall time to completion.
async fn run_once(
    verifier: &mut BlockTxVerifier<CacheTestState>,
    txs: &[Tx],
    offsets: &[Duration],
) -> Duration {
    let sapling = crate::primitives::sapling::VERIFIER.bench_uncached();
    let halo2 = crate::primitives::halo2::cached_orchard_v5_verifier_for(NetworkUpgrade::Nu5)
        .bench_uncached();

    let futs: Vec<_> = txs
        .iter()
        .map(|(tx, height, time)| {
            verifier.call(BlockRequest {
                transaction_hash: tx.hash(),
                transaction: tx.clone(),
                known_utxos: Arc::new(HashMap::new()),
                height: *height,
                time: *time,
            })
        })
        .collect();

    SAPLING_CACHE_VERIFIER
        .scope(
            sapling,
            HALO2_CACHE_VERIFIER.scope(halo2, async move {
                let start = Instant::now();
                let mut pending: FuturesUnordered<_> = futs
                    .into_iter()
                    .zip(offsets.iter().copied())
                    .map(|(fut, offset)| async move {
                        if !offset.is_zero() {
                            tokio::time::sleep_until((start + offset).into()).await;
                        }
                        fut.await
                    })
                    .collect();
                while let Some(result) = pending.next().await {
                    result.expect("vector transaction must verify");
                }
                start.elapsed()
            }),
        )
        .await
}

fn drain_batches() -> (Vec<usize>, Vec<String>) {
    let sapling: Vec<usize> = std::mem::take(
        &mut *crate::primitives::sapling::BENCH_BATCHES
            .lock()
            .expect("unpoisoned"),
    )
    .into_iter()
    .map(|(_, n)| n)
    .collect();
    let halo2: Vec<String> = std::mem::take(
        &mut *crate::primitives::halo2::BENCH_BATCHES
            .lock()
            .expect("unpoisoned"),
    )
    .into_iter()
    .map(|(_, items, actions)| format!("{items}tx/{actions}act"))
    .collect();
    (sapling, halo2)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

async fn case(
    name: &str,
    verifier: &mut BlockTxVerifier<CacheTestState>,
    txs: &[Tx],
    offsets: &[Duration],
    iters: usize,
) {
    let _ = drain_batches();
    let cold = run_once(verifier, txs, offsets).await;
    let (s, h) = drain_batches();
    println!(
        "RESULT {name} cold_ms={:.1} sapling_batches={s:?} halo2_batches={h:?}",
        cold.as_secs_f64() * 1e3
    );

    let mut samples = Vec::new();
    let mut batch_shapes: HashMap<String, usize> = HashMap::new();
    for _ in 0..iters {
        // Let the previous batch timers and rayon work drain, as between two blocks.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let elapsed = run_once(verifier, txs, offsets).await;
        samples.push(elapsed.as_secs_f64() * 1e3);
        let (s, h) = drain_batches();
        *batch_shapes
            .entry(format!("sapling={s:?} halo2={h:?}"))
            .or_default() += 1;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    println!(
        "RESULT {name} warm n={iters} p10={:.1} median={:.1} p90={:.1} min={:.1} max={:.1}",
        pct(&samples, 0.1),
        pct(&samples, 0.5),
        pct(&samples, 0.9),
        samples[0],
        samples[samples.len() - 1],
    );
    let mut shapes: Vec<_> = batch_shapes.into_iter().collect();
    shapes.sort_by(|a, b| b.1.cmp(&a.1));
    for (shape, count) in shapes {
        println!("RESULT {name} warm_batches {count}x {shape}");
    }
}

#[test]
#[ignore]
fn bench_11673_batch_idle_latency() {
    let _init_guard = zebra_test::init();
    let iters: usize = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);

    println!(
        "RESULT config max_batch_idle={:?} cores={}",
        crate::primitives::MAX_BATCH_IDLE,
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );

    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let state: CacheTestState = MockService::build().for_prop_tests();
        let mut verifier = BlockTxVerifier::new(&Network::Mainnet, state);

        let sapling = sapling_txs();
        let orchard = orchard_txs();
        for (_, h, _) in &orchard {
            assert_eq!(
                NetworkUpgrade::current(&Network::Mainnet, *h),
                NetworkUpgrade::Nu5,
                "the harness isolates only the pre-NU6.2 Halo2 cache"
            );
        }
        let actions: usize = orchard
            .iter()
            .map(|(tx, ..)| tx.orchard_actions().count())
            .sum();
        println!(
            "RESULT txs sapling={} orchard={} orchard_actions={actions}",
            sapling.len(),
            orchard.len()
        );

        let burst = |n: usize| vec![Duration::ZERO; n];
        case(
            "sapling_burst",
            &mut verifier,
            &sapling,
            &burst(sapling.len()),
            iters,
        )
        .await;
        case(
            "orchard_burst",
            &mut verifier,
            &orchard,
            &burst(orchard.len()),
            iters,
        )
        .await;

        // Gapped arrivals: every tx `gap` after the previous one.
        for gap_ms in [2u64, 8] {
            let offsets: Vec<_> = (0..orchard.len() as u32)
                .map(|i| Duration::from_millis(gap_ms) * i)
                .collect();
            case(
                &format!("orchard_gap_{gap_ms}ms"),
                &mut verifier,
                &orchard,
                &offsets,
                iters / 2,
            )
            .await;
        }

        // Stream: 128 Sapling txs (the 16 cycled 8x) in one burst, for throughput.
        let stream: Vec<Tx> = sapling.iter().cycle().take(128).cloned().collect();
        case(
            "sapling_stream_128",
            &mut verifier,
            &stream,
            &burst(128),
            iters / 2,
        )
        .await;
    });
}
