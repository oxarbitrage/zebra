//! Hooks for the criterion benchmarks in `benches/`, compiled only with the `bench` feature.
//!
//! Benches are external crates, so they can only reach verifier internals through public items.
//! Nothing here is supported API, and it must never be enabled in a release build.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

/// See [`set_proof_cache_bypass`].
static BYPASS_PROOF_CACHE: AtomicBool = AtomicBool::new(false);

/// Items in each flushed Halo2 batch, across all Halo2 verifiers, in flush order.
static HALO2_BATCH_SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// When `bypass` is set, the shielded proof caches neither hit nor record, so every bundle reaches
/// its batch verifier, even one verified on a previous benchmark iteration.
pub fn set_proof_cache_bypass(bypass: bool) {
    BYPASS_PROOF_CACHE.store(bypass, Ordering::Relaxed);
}

/// Returns true if the proof caches are bypassed.
pub(crate) fn proof_cache_bypassed() -> bool {
    BYPASS_PROOF_CACHE.load(Ordering::Relaxed)
}

/// Returns and clears the item counts of the Halo2 batches flushed since the last call.
pub fn take_halo2_batch_sizes() -> Vec<usize> {
    std::mem::take(&mut *HALO2_BATCH_SIZES.lock().expect("unpoisoned"))
}

/// Records a flushed Halo2 batch of `items` items.
pub(crate) fn record_halo2_batch(items: usize) {
    HALO2_BATCH_SIZES.lock().expect("unpoisoned").push(items);
}
