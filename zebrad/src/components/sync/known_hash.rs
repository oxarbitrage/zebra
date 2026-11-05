//! PROTOTYPE: known-hash initial block download.
//!
//! Below the max height of a local known-hash list, this replaces tip
//! discovery (`FindBlocks` / `FindHeaders` round-trips) with downloads by known
//! hash: a wide window of pending heights is fetched with batched
//! `BlocksByHash` requests spread across the peer set, and every block is fed
//! to the normal block verifier router (so the checkpoint verifier and the
//! normal state commit) in height order. Above the list's max height, the
//! regular syncer takes over.
//!
//! This is a throwaway prototype for A/B benchmarking, not for merging.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::eyre::{eyre, Report, WrapErr};
use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::ChainTip,
    parameters::{
        known_hashes::{size_hint_upper_bound, KnownHashList, KnownHashListSpec},
        Network,
    },
};
use zebra_network as zn;

use crate::BoxError;

use super::Config;

/// The spec file name inside `sync.known_hash_dir`.
pub const SPEC_FILE_NAME: &str = "known-hashes.spec";

/// An outer timeout on each batched fetch, including time waiting for a ready
/// peer. The network layer also applies its own per-request timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(45);

/// The number of heights above the submit frontier that are hedged when slow.
const HEDGE_SPAN: u32 = 64;

/// The maximum retry backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// The minimum number of outstanding verifier requests: the checkpoint
/// verifier only makes progress once a whole checkpoint range is queued.
const MIN_VERIFY_AHEAD: usize = 2 * zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP;

/// How often the engine logs its progress.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// Loads the spec file in `dir` and opens the known-hash list it describes,
/// verifying every chunk against its pinned SHA-256 and the checkpoint list.
///
/// The spec strings are leaked: the list lives for the whole process.
pub fn load_list(network: &Network, dir: &Path) -> Result<KnownHashList<'static>, Report> {
    let spec_path = dir.join(SPEC_FILE_NAME);
    let text = fs::read_to_string(&spec_path)
        .wrap_err_with(|| format!("reading {}", spec_path.display()))?;

    let mut max_height = None;
    let mut chunk_blocks = None;
    let mut file_prefix = None;
    let mut chunk_hashes = Vec::new();

    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| eyre!("bad spec line {line:?}"))?;
        let (key, value) = (key.trim(), value.trim());
        match key {
            "max_height" => max_height = Some(Height(value.parse()?)),
            "chunk_blocks" => chunk_blocks = Some(value.parse()?),
            "file_prefix" => file_prefix = Some(&*Box::leak(value.to_owned().into_boxed_str())),
            "chunk" => chunk_hashes.push(&*Box::leak(value.to_owned().into_boxed_str())),
            _ => return Err(eyre!("unknown spec key {key:?}")),
        }
    }

    let spec = KnownHashListSpec {
        max_height: max_height.ok_or_else(|| eyre!("spec has no max_height"))?,
        chunk_blocks: chunk_blocks.ok_or_else(|| eyre!("spec has no chunk_blocks"))?,
        file_prefix: file_prefix.ok_or_else(|| eyre!("spec has no file_prefix"))?,
        chunk_hashes: Box::leak(chunk_hashes.into_boxed_slice()),
    };

    let started = Instant::now();
    let list = KnownHashList::open(spec, network.checkpoint_list(), dir)?;
    info!(
        max_height = ?list.max_height(),
        dir = %dir.display(),
        elapsed = ?started.elapsed(),
        "known-hash IBD: loaded and verified known-hash list"
    );

    Ok(list)
}

/// The download state of one height in the window.
#[derive(Debug)]
enum State {
    /// The block is not here yet. It is in the pending set when no request
    /// for it is in flight.
    Need,
    /// The block arrived and waits for the submit frontier.
    Fetched(Arc<Block>),
    /// The block was sent to the verifier.
    Submitted,
}

/// One height in the window.
#[derive(Debug)]
struct Slot {
    hash: block::Hash,
    hint_bytes: u64,
    state: State,
    /// Requests in flight that include this height.
    in_flight: u8,
    /// When the oldest in-flight request for this height was sent.
    since: Option<Instant>,
    hedged: bool,
    attempts: u32,
    not_before: Option<Instant>,
}

/// A completed staged future.
enum Event {
    Fetch {
        heights: Vec<Height>,
        hedge: bool,
        started: Instant,
        result: Result<zn::Response, BoxError>,
    },
    Verify {
        height: Height,
        started: Instant,
        result: Result<block::Hash, BoxError>,
    },
}

/// Engine settings, from [`Config`].
#[derive(Clone, Debug)]
struct Limits {
    window_blocks: usize,
    window_bytes: u64,
    batch_blocks: usize,
    batch_bytes: u64,
    max_batches: usize,
    verify_ahead: usize,
    hedge_after: Duration,
}

impl Limits {
    fn new(config: &Config) -> Self {
        Self {
            window_blocks: config.known_hash_window_blocks.max(1),
            window_bytes: config.known_hash_window_bytes.max(4_000_000),
            batch_blocks: config.known_hash_batch_blocks.clamp(1, 128),
            batch_bytes: config.known_hash_batch_bytes.max(1),
            max_batches: config.known_hash_max_batches.max(1),
            verify_ahead: config.known_hash_verify_ahead.max(MIN_VERIFY_AHEAD),
            hedge_after: Duration::from_millis(config.known_hash_hedge_after_ms.max(100)),
        }
    }
}

/// The known-hash IBD engine.
pub struct KnownHashIbd<ZN, ZV, ZSTip> {
    list: KnownHashList<'static>,
    limits: Limits,
    network: ZN,
    verifier: ZV,
    latest_chain_tip: ZSTip,

    /// The lowest uncommitted height, the first slot in `window`.
    base: Height,
    window: VecDeque<Slot>,
    window_bytes: u64,
    /// Heights in `Need` with no request in flight.
    pending: BTreeSet<u32>,
    /// The next height to send to the verifier.
    next_submit: Height,
    verify_outstanding: usize,
    batches_in_flight: usize,
    futures: FuturesUnordered<BoxFuture<'static, Event>>,
}

impl<ZN, ZV, ZSTip> KnownHashIbd<ZN, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send + 'static,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send + 'static,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Creates an engine over `list`.
    pub fn new(
        config: &Config,
        list: KnownHashList<'static>,
        network: ZN,
        verifier: ZV,
        latest_chain_tip: ZSTip,
    ) -> Self {
        Self {
            list,
            limits: Limits::new(config),
            network,
            verifier,
            latest_chain_tip,
            base: Height(0),
            window: VecDeque::new(),
            window_bytes: 0,
            pending: BTreeSet::new(),
            next_submit: Height(0),
            verify_outstanding: 0,
            batches_in_flight: 0,
            futures: FuturesUnordered::new(),
        }
    }

    fn committed_tip(&self) -> Option<Height> {
        self.latest_chain_tip.best_tip_height()
    }

    fn end(&self) -> Height {
        Height(self.base.0 + self.window.len() as u32)
    }

    fn slot(&self, height: Height) -> Option<&Slot> {
        height
            .0
            .checked_sub(self.base.0)
            .and_then(|i| self.window.get(i as usize))
    }

    fn slot_mut(&mut self, height: Height) -> Option<&mut Slot> {
        height
            .0
            .checked_sub(self.base.0)
            .and_then(|i| self.window.get_mut(i as usize))
    }

    /// Runs the engine until the state tip reaches the list's max height.
    pub async fn run(mut self) -> Result<(), Report> {
        let max_height = self.list.max_height();
        metrics::gauge!("sync.known_hash.list.max_height").set(f64::from(max_height.0));

        let start = self
            .committed_tip()
            .map_or(Height(0), |tip| Height(tip.0 + 1));
        if start > max_height {
            info!(
                ?start,
                ?max_height,
                "known-hash IBD: state is past the list, skipping"
            );
            return Ok(());
        }

        info!(?start, ?max_height, limits = ?self.limits, "known-hash IBD: starting");
        self.base = start;
        self.next_submit = start;

        let started = Instant::now();
        let mut last_progress = Instant::now();
        let mut last_progress_height = start;

        loop {
            self.advance_base();
            if self.base > max_height {
                break;
            }

            self.extend_window(max_height)?;
            self.submit_ready();
            self.issue_hedges();
            self.issue_batches();
            self.update_gauges();

            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                let rate = f64::from(self.base.0 - last_progress_height.0)
                    / last_progress.elapsed().as_secs_f64();
                info!(
                    frontier = ?self.base,
                    next_submit = ?self.next_submit,
                    window = self.window.len(),
                    window_mb = self.window_bytes / 1_000_000,
                    pending = self.pending.len(),
                    batches = self.batches_in_flight,
                    verify_outstanding = self.verify_outstanding,
                    blocks_per_sec = format!("{rate:.0}"),
                    "known-hash IBD progress"
                );
                last_progress = Instant::now();
                last_progress_height = self.base;
            }

            // Classify what the commit frontier is waiting on, for the A/B.
            let wait_class = self.wait_class();
            let wait_start = Instant::now();

            // Wake up periodically for backoff expiry, hedges and tip changes.
            let event = tokio::select! {
                event = self.futures.next() => event,
                _ = tokio::time::sleep(Duration::from_millis(100)) => None,
            };

            let waited = wait_start.elapsed().as_millis() as u64;
            metrics::counter!("sync.known_hash.frontier_wait_ms", "on" => wait_class)
                .increment(waited);

            if let Some(event) = event {
                self.handle(event);
                // Drain everything else that is already ready.
                while let Some(Some(event)) = self.futures.next().now_or_never() {
                    self.handle(event);
                }
            }
        }

        info!(
            tip = ?self.committed_tip(),
            elapsed = ?started.elapsed(),
            "known-hash IBD: reached the end of the known-hash list, handing off to the syncer"
        );

        // Let outstanding commits finish; drop fetches.
        Ok(())
    }

    /// What the commit frontier is waiting on right now.
    fn wait_class(&self) -> &'static str {
        if self.verify_outstanding >= self.limits.verify_ahead {
            return "verify_full";
        }
        match self.slot(self.next_submit).map(|slot| &slot.state) {
            Some(State::Need) => "download",
            Some(_) => "other",
            // Everything in the window was submitted: either the window is
            // full and waits for commits, or the list is exhausted.
            None if self.window.len() >= self.limits.window_blocks
                || self.window_bytes >= self.limits.window_bytes =>
            {
                "window_full"
            }
            None => "commit",
        }
    }

    /// Pops committed heights off the front of the window.
    fn advance_base(&mut self) {
        let Some(tip) = self.committed_tip() else {
            return;
        };
        while self.base <= tip {
            if let Some(slot) = self.window.pop_front() {
                self.window_bytes -= slot.hint_bytes;
            }
            self.pending.remove(&self.base.0);
            self.base = Height(self.base.0 + 1);
        }
        if self.next_submit < self.base {
            self.next_submit = self.base;
        }
    }

    /// Adds heights to the end of the window while it is under its limits.
    fn extend_window(&mut self, max_height: Height) -> Result<(), Report> {
        while self.window.len() < self.limits.window_blocks && self.end() <= max_height {
            let height = self.end();
            let (hash, hint) = self
                .list
                .get(height)?
                .ok_or_else(|| eyre!("known-hash list has no entry for {height:?}"))?;
            let hint_bytes = size_hint_upper_bound(hint);

            // Always admit at least one block, so a huge block can't stall.
            if !self.window.is_empty() && self.window_bytes + hint_bytes > self.limits.window_bytes
            {
                break;
            }

            self.window.push_back(Slot {
                hash,
                hint_bytes,
                state: State::Need,
                in_flight: 0,
                since: None,
                hedged: false,
                attempts: 0,
                not_before: None,
            });
            self.window_bytes += hint_bytes;
            self.pending.insert(height.0);
        }
        Ok(())
    }

    /// Sends fetched blocks to the verifier in height order.
    fn submit_ready(&mut self) {
        while self.verify_outstanding < self.limits.verify_ahead {
            let height = self.next_submit;
            let Some(slot) = self.slot_mut(height) else {
                break;
            };
            match std::mem::replace(&mut slot.state, State::Submitted) {
                State::Fetched(block) => {
                    let verifier = self.verifier.clone();
                    self.verify_outstanding += 1;
                    self.futures.push(
                        async move {
                            let started = Instant::now();
                            let result = verifier
                                .oneshot(zebra_consensus::Request::Commit(block))
                                .await;
                            Event::Verify {
                                height,
                                started,
                                result,
                            }
                        }
                        .boxed(),
                    );
                }
                State::Submitted => {}
                State::Need => {
                    slot.state = State::Need;
                    break;
                }
            }
            self.next_submit = Height(height.0 + 1);
        }
    }

    /// Issues batched requests for pending heights.
    fn issue_batches(&mut self) {
        let now = Instant::now();
        while self.batches_in_flight < self.limits.max_batches {
            let mut heights = Vec::new();
            let mut bytes = 0;
            for &h in self.pending.iter() {
                let slot = self
                    .slot(Height(h))
                    .expect("pending heights are in the window");
                if slot.not_before.is_some_and(|t| t > now) {
                    continue;
                }
                if !heights.is_empty() && bytes + slot.hint_bytes > self.limits.batch_bytes {
                    break;
                }
                heights.push(Height(h));
                bytes += slot.hint_bytes;
                if heights.len() >= self.limits.batch_blocks {
                    break;
                }
            }
            if heights.is_empty() {
                break;
            }
            for h in &heights {
                self.pending.remove(&h.0);
            }
            self.send(heights, false);
        }
    }

    /// Re-requests slow heights just above the submit frontier.
    fn issue_hedges(&mut self) {
        let now = Instant::now();
        let mut heights = Vec::new();
        for h in self.next_submit.0..self.next_submit.0.saturating_add(HEDGE_SPAN) {
            let Some(slot) = self.slot(Height(h)) else {
                break;
            };
            if matches!(slot.state, State::Need)
                && slot.in_flight > 0
                && !slot.hedged
                && slot
                    .since
                    .is_some_and(|since| now - since >= self.limits.hedge_after)
            {
                heights.push(Height(h));
            }
        }
        for batch in heights.chunks(self.limits.batch_blocks) {
            for h in batch {
                self.slot_mut(*h).expect("in window").hedged = true;
            }
            metrics::counter!("sync.known_hash.hedge.blocks").increment(batch.len() as u64);
            self.send(batch.to_vec(), true);
        }
    }

    fn send(&mut self, heights: Vec<Height>, hedge: bool) {
        let now = Instant::now();
        let mut hashes = std::collections::HashSet::new();
        for h in &heights {
            let slot = self.slot_mut(*h).expect("in window");
            slot.in_flight += 1;
            slot.since.get_or_insert(now);
            hashes.insert(slot.hash);
        }

        self.batches_in_flight += 1;
        metrics::counter!("sync.known_hash.requests", "kind" => if hedge { "hedge" } else { "batch" })
            .increment(1);
        metrics::counter!("sync.known_hash.requested.blocks").increment(heights.len() as u64);

        let network = self.network.clone();
        self.futures.push(
            async move {
                let started = Instant::now();
                let result = match tokio::time::timeout(
                    FETCH_TIMEOUT,
                    network.oneshot(zn::Request::BlocksByHash(hashes)),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("known-hash fetch timed out".into()),
                };
                Event::Fetch {
                    heights,
                    hedge,
                    started,
                    result,
                }
            }
            .boxed(),
        );
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Fetch {
                heights,
                hedge,
                started,
                result,
            } => {
                self.batches_in_flight -= 1;
                metrics::histogram!("sync.known_hash.fetch.seconds")
                    .record(started.elapsed().as_secs_f64());
                self.handle_fetch(heights, hedge, result);
            }
            Event::Verify {
                height,
                started,
                result,
            } => {
                self.verify_outstanding -= 1;
                metrics::histogram!("sync.known_hash.verify.seconds")
                    .record(started.elapsed().as_secs_f64());
                if let Err(error) = result {
                    if self.committed_tip().is_some_and(|tip| tip >= height) {
                        return;
                    }
                    metrics::counter!("sync.known_hash.verify.errors").increment(1);
                    warn!(
                        ?height,
                        ?error,
                        "known-hash IBD: block failed verification, refetching"
                    );
                    if let Some(slot) = self.slot_mut(height) {
                        if matches!(slot.state, State::Submitted) {
                            slot.state = State::Need;
                            slot.attempts += 1;
                            slot.hedged = false;
                            slot.since = None;
                            slot.not_before = Some(Instant::now() + backoff(slot.attempts));
                            if slot.in_flight == 0 {
                                self.pending.insert(height.0);
                            }
                        }
                    }
                    if height < self.next_submit {
                        self.next_submit = height;
                    }
                }
            }
        }
    }

    fn handle_fetch(
        &mut self,
        heights: Vec<Height>,
        hedge: bool,
        result: Result<zn::Response, BoxError>,
    ) {
        let kind = if hedge { "hedge" } else { "batch" };
        let mut received: HashMap<block::Hash, (Arc<Block>, Option<zn::PeerSocketAddr>)> =
            HashMap::new();
        let mut missing = 0;
        let mut peer = None;

        match result {
            Ok(zn::Response::Blocks(items)) => {
                for item in items {
                    match item {
                        zn::InventoryResponse::Available((block, addr)) => {
                            peer = peer.or(addr);
                            received.insert(block.hash(), (block, addr));
                        }
                        zn::InventoryResponse::Missing(_) => missing += 1,
                    }
                }
            }
            Ok(other) => {
                warn!(?other, "known-hash IBD: unexpected network response");
            }
            Err(error) => {
                let class = if format!("{error:?}").contains("timed out")
                    || format!("{error:?}").contains("Timeout")
                {
                    "timeout"
                } else {
                    "error"
                };
                metrics::counter!("sync.known_hash.request.failures", "class" => class, "kind" => kind)
                    .increment(1);
                debug!(?error, "known-hash IBD: batch failed");
            }
        }

        let peer_label = peer
            .map(|addr| addr.remove_socket_addr_privacy().ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        if peer.is_some() {
            metrics::counter!("sync.known_hash.peer.blocks", "peer" => peer_label.clone())
                .increment(received.len() as u64);
            metrics::counter!("sync.known_hash.peer.responses", "peer" => peer_label.clone())
                .increment(1);
        }
        if missing > 0 {
            metrics::counter!("sync.known_hash.notfound.blocks").increment(missing);
            metrics::counter!("sync.known_hash.peer.notfound", "peer" => peer_label.clone())
                .increment(missing);
        }

        let now = Instant::now();
        let mut retried = 0;
        for height in heights {
            let Some(slot) = self.slot_mut(height) else {
                // Already committed.
                continue;
            };
            slot.in_flight -= 1;

            if let Some((block, _addr)) = received.remove(&slot.hash) {
                match slot.state {
                    State::Need => {
                        // `received` is keyed by the computed header hash, so
                        // this block's hash equals the known hash.
                        slot.state = State::Fetched(block);
                        if hedge {
                            metrics::counter!("sync.known_hash.hedge.wins").increment(1);
                        }
                        metrics::counter!("sync.known_hash.received.blocks").increment(1);
                    }
                    _ => {
                        metrics::counter!("sync.known_hash.duplicate.blocks").increment(1);
                    }
                }
            }

            if matches!(slot.state, State::Need) && slot.in_flight == 0 {
                slot.attempts += 1;
                slot.hedged = false;
                slot.since = None;
                slot.not_before = Some(now + backoff(slot.attempts));
                retried += 1;
                self.pending.insert(height.0);
            }
        }

        // Unrequested blocks (or blocks with the wrong hash) are dropped.
        if !received.is_empty() {
            metrics::counter!("sync.known_hash.unexpected.blocks").increment(received.len() as u64);
        }
        if retried > 0 {
            metrics::counter!("sync.known_hash.retries", "kind" => kind).increment(retried);
            metrics::counter!("sync.known_hash.peer.retries", "peer" => peer_label)
                .increment(retried);
        }
    }

    fn update_gauges(&self) {
        let mut in_flight = 0;
        let mut fetched = 0;
        for slot in &self.window {
            match slot.state {
                State::Need if slot.in_flight > 0 => in_flight += 1,
                State::Fetched(_) => fetched += 1,
                _ => {}
            }
        }
        metrics::gauge!("sync.known_hash.frontier.height").set(f64::from(self.base.0));
        metrics::gauge!("sync.known_hash.submit.height").set(f64::from(self.next_submit.0));
        metrics::gauge!("sync.known_hash.window.blocks").set(self.window.len() as f64);
        metrics::gauge!("sync.known_hash.window.bytes").set(self.window_bytes as f64);
        metrics::gauge!("sync.known_hash.window.fill").set(
            (self.window.len() as f64 / self.limits.window_blocks as f64)
                .max(self.window_bytes as f64 / self.limits.window_bytes as f64),
        );
        metrics::gauge!("sync.known_hash.pending.blocks").set(self.pending.len() as f64);
        metrics::gauge!("sync.known_hash.inflight.blocks").set(f64::from(in_flight));
        metrics::gauge!("sync.known_hash.inflight.batches").set(self.batches_in_flight as f64);
        metrics::gauge!("sync.known_hash.fetched.waiting").set(f64::from(fetched));
        metrics::gauge!("sync.known_hash.verify.outstanding").set(self.verify_outstanding as f64);
    }
}

fn backoff(attempts: u32) -> Duration {
    Duration::from_millis(250u64.saturating_mul(1 << attempts.min(8))).min(MAX_BACKOFF)
}
