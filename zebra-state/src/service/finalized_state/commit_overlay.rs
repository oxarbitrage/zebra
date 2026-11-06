//! An in-memory, write-through overlay of recently written transparent outputs and address
//! balances, used by checkpoint commits to skip database reads.
//!
//! See [`CheckpointCommitOverlay`] for details.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    mem::size_of,
};

use zebra_chain::{block::Height, transparent};

use crate::service::finalized_state::disk_format::transparent::{
    AddressBalanceLocation, OutputLocation,
};

/// The default byte budget of a [`CheckpointCommitOverlay`], in MiB.
pub const DEFAULT_CHECKPOINT_COMMIT_CACHE_MIB: usize = 256;

/// An approximation of the `HashMap` memory used per entry on top of the entry itself: one
/// control byte, plus the unused slots kept by its maximum load factor of 7/8.
fn hash_map_entry_bytes<K, V>() -> usize {
    size_of::<(K, V)>() * 8 / 7 + 1
}

/// A bounded, write-through, in-memory copy of the transparent outputs and the address balances
/// that recent checkpoint commits wrote to the database.
///
/// Checkpoint commits read every spent output and every changed address balance from the
/// database. Most spends and most balance changes are for outputs and addresses written a few
/// blocks earlier, so checkpoint commits look them up here first.
///
/// # Correctness
///
/// The database stays authoritative:
/// - entries are only added from the data that a successful block write has just written to the
///   database, and spent outputs are removed in the same step, so every entry is equal to its
///   database value,
/// - a miss falls back to the database read, and
/// - the overlay doesn't change what is written to the database.
///
/// The overlay is only valid while nothing else writes these column families, so it is carried
/// in the [`CheckpointTipCache`](super::CheckpointTipCache), and has the same lifetime: it is
/// dropped when the checkpoint commit loop exits or a commit fails, and isn't created while a
/// database format upgrade is running.
///
/// Debug builds check every hit against the database.
///
/// # Memory
///
/// The overlay keeps its approximate size under a byte budget by evicting the entries written at
/// the lowest height first. Outputs are also removed when they are spent. An address balance
/// written again moves to the new height.
#[derive(Clone, Debug)]
pub struct CheckpointCommitOverlay {
    /// The maximum approximate size of the overlay, in bytes.
    budget_bytes: usize,

    /// The approximate size of the overlay, in bytes, including stale eviction queue entries.
    bytes: usize,

    /// Unspent outputs written by recent commits, and their locations.
    outputs: HashMap<transparent::OutPoint, (OutputLocation, transparent::Utxo)>,

    /// The outputs in `outputs`, in the order they were written, and the height that wrote them.
    ///
    /// Entries for spent outputs stay in the queue until they are evicted.
    output_queue: VecDeque<(Height, transparent::OutPoint)>,

    /// Address balances written by recent commits, and the last height that wrote them.
    balances: HashMap<transparent::Address, (AddressBalanceLocation, Height)>,

    /// The writes to `balances`, in order.
    ///
    /// Entries for balances written again at a later height stay in the queue until they are
    /// evicted, but don't evict the newer balance.
    balance_queue: VecDeque<(Height, transparent::Address)>,

    /// The lookups since the overlay was created.
    lookups: OverlayLookups,
}

/// Overlay lookup counts for one block, recorded as metrics once per block.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct OverlayLookups {
    /// Spent outputs found in the overlay.
    pub output_hits: u64,
    /// Spent outputs read from the database or the block.
    pub output_misses: u64,
    /// The spent outputs in `output_misses` that were created by the same block.
    pub output_same_block: u64,
    /// Address balances found in the overlay.
    pub balance_hits: u64,
    /// Address balances read from the database, or new addresses.
    pub balance_misses: u64,
}

impl OverlayLookups {
    /// Adds these lookup counts to the overlay metrics.
    fn record_metrics(&self) {
        metrics::counter!("state.checkpoint.overlay.output.hit").increment(self.output_hits);
        metrics::counter!("state.checkpoint.overlay.output.miss").increment(self.output_misses);
        metrics::counter!("state.checkpoint.overlay.output.same_block")
            .increment(self.output_same_block);
        metrics::counter!("state.checkpoint.overlay.balance.hit").increment(self.balance_hits);
        metrics::counter!("state.checkpoint.overlay.balance.miss").increment(self.balance_misses);
    }
}

impl CheckpointCommitOverlay {
    /// Returns a new empty overlay with a budget of `budget_bytes`, or `None` if the budget is
    /// zero.
    pub fn new(budget_bytes: usize) -> Option<Self> {
        (budget_bytes > 0).then(|| Self {
            budget_bytes,
            bytes: 0,
            outputs: HashMap::new(),
            output_queue: VecDeque::new(),
            balances: HashMap::new(),
            balance_queue: VecDeque::new(),
            lookups: OverlayLookups::default(),
        })
    }

    /// Returns the location and the output of `outpoint`, if it is in the overlay.
    pub fn utxo(
        &self,
        outpoint: &transparent::OutPoint,
    ) -> Option<(OutputLocation, transparent::Utxo)> {
        self.outputs.get(outpoint).cloned()
    }

    /// Returns the balance and location of `address`, if it is in the overlay.
    pub fn address_balance_location(
        &self,
        address: &transparent::Address,
    ) -> Option<AddressBalanceLocation> {
        self.balances
            .get(address)
            .map(|(balance, _height)| *balance)
    }

    /// Adds a block's lookup counts to the overlay's totals and metrics.
    pub fn add_lookups(&mut self, lookups: OverlayLookups) {
        lookups.record_metrics();

        self.lookups.output_hits += lookups.output_hits;
        self.lookups.output_misses += lookups.output_misses;
        self.lookups.output_same_block += lookups.output_same_block;
        self.lookups.balance_hits += lookups.balance_hits;
        self.lookups.balance_misses += lookups.balance_misses;
    }

    /// Returns the lookups since the overlay was created.
    #[cfg(test)]
    pub fn lookups(&self) -> OverlayLookups {
        self.lookups
    }

    /// Returns the budget of the overlay, in bytes.
    #[cfg(test)]
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Returns the approximate size of the overlay, in bytes.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Returns the number of outputs in the overlay.
    #[cfg(test)]
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }

    /// Returns the number of address balances in the overlay.
    #[cfg(test)]
    pub fn balance_count(&self) -> usize {
        self.balances.len()
    }

    /// Returns an iterator over every output in the overlay.
    #[cfg(test)]
    pub fn outputs(
        &self,
    ) -> impl Iterator<Item = (&transparent::OutPoint, &(OutputLocation, transparent::Utxo))> {
        self.outputs.iter()
    }

    /// Returns an iterator over every address balance in the overlay.
    #[cfg(test)]
    pub fn balances(
        &self,
    ) -> impl Iterator<Item = (&transparent::Address, &AddressBalanceLocation)> {
        self.balances
            .iter()
            .map(|(address, (balance, _height))| (address, balance))
    }

    /// Updates the overlay with the data that the block at `height` has just written to the
    /// database, then evicts the oldest entries until the overlay is under budget.
    ///
    /// Must only be called after the block's batch was successfully written, with:
    /// - `spent_outputs`: every output the block spent,
    /// - `new_outputs`: every output the block created, and their locations, and
    /// - `balances`: every address balance the block wrote, as written.
    pub fn apply_block(
        &mut self,
        height: Height,
        spent_outputs: impl IntoIterator<Item = transparent::OutPoint>,
        new_outputs: impl IntoIterator<
            Item = (transparent::OutPoint, OutputLocation, transparent::Utxo),
        >,
        balances: impl IntoIterator<Item = (transparent::Address, AddressBalanceLocation)>,
    ) {
        let spent_outputs: HashSet<transparent::OutPoint> = spent_outputs.into_iter().collect();

        for outpoint in &spent_outputs {
            if let Some((_, utxo)) = self.outputs.remove(outpoint) {
                self.bytes -= Self::output_entry_bytes(&utxo);
            }
        }

        for (outpoint, location, utxo) in new_outputs {
            // Outputs spent by the same block are deleted from the database by the same batch.
            if spent_outputs.contains(&outpoint) {
                continue;
            }

            let entry_bytes = Self::output_entry_bytes(&utxo);
            if let Some((_, old_utxo)) = self.outputs.insert(outpoint, (location, utxo)) {
                self.bytes -= Self::output_entry_bytes(&old_utxo);
            }
            self.bytes += entry_bytes;

            self.output_queue.push_back((height, outpoint));
            self.bytes += size_of::<(Height, transparent::OutPoint)>();
        }

        for (address, balance) in balances {
            if self.balances.insert(address, (balance, height)).is_none() {
                self.bytes += Self::balance_entry_bytes();
            }

            self.balance_queue.push_back((height, address));
            self.bytes += size_of::<(Height, transparent::Address)>();
        }

        self.evict_to_budget();
        self.record_metrics();
    }

    /// Evicts the entries written at the lowest heights until the overlay is under its budget.
    fn evict_to_budget(&mut self) {
        let mut evicted = 0;

        while self.bytes > self.budget_bytes {
            let oldest_output = self.output_queue.front().map(|(height, _)| *height);
            let oldest_balance = self.balance_queue.front().map(|(height, _)| *height);

            let evict_output = match (oldest_output, oldest_balance) {
                (Some(output_height), Some(balance_height)) => output_height <= balance_height,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };

            if evict_output {
                let (height, outpoint) = self
                    .output_queue
                    .pop_front()
                    .expect("just checked the queue is not empty");
                self.bytes -= size_of::<(Height, transparent::OutPoint)>();

                // An output that was spent, or written again by a later queue entry, has already
                // been accounted for.
                if self
                    .outputs
                    .get(&outpoint)
                    .is_some_and(|(location, _)| location.height() == height)
                {
                    let (_, utxo) = self
                        .outputs
                        .remove(&outpoint)
                        .expect("just checked the output is present");
                    self.bytes -= Self::output_entry_bytes(&utxo);
                    evicted += 1;
                }
            } else {
                let (height, address) = self
                    .balance_queue
                    .pop_front()
                    .expect("just checked the queue is not empty");
                self.bytes -= size_of::<(Height, transparent::Address)>();

                // Only evict the balance if no later block wrote it again.
                if self
                    .balances
                    .get(&address)
                    .is_some_and(|(_, last_height)| *last_height == height)
                {
                    self.balances.remove(&address);
                    self.bytes -= Self::balance_entry_bytes();
                    evicted += 1;
                }
            }
        }

        metrics::counter!("state.checkpoint.overlay.evicted").increment(evicted);
    }

    /// Records the overlay's size metrics.
    fn record_metrics(&self) {
        metrics::gauge!("state.checkpoint.overlay.bytes").set(self.bytes as f64);
        metrics::gauge!("state.checkpoint.overlay.outputs").set(self.outputs.len() as f64);
        metrics::gauge!("state.checkpoint.overlay.balances").set(self.balances.len() as f64);
    }

    /// Returns the approximate memory used by an output entry, excluding its queue entry.
    fn output_entry_bytes(utxo: &transparent::Utxo) -> usize {
        hash_map_entry_bytes::<transparent::OutPoint, (OutputLocation, transparent::Utxo)>()
            + utxo.output.lock_script.as_raw_bytes().len()
    }

    /// Returns the approximate memory used by an address balance entry, excluding its queue
    /// entries.
    fn balance_entry_bytes() -> usize {
        hash_map_entry_bytes::<transparent::Address, (AddressBalanceLocation, Height)>()
    }
}
