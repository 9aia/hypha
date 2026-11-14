//! The sync engine — one supervised loop per node, `JoinSet`-managed,
//! `CancellationToken`-cancellable.
//!
//! Per node the cycle is: `list_entities` → upsert + reconcile the
//! projection → drain the outbox FIFO → tail `/api/events` and apply
//! index diffs → on stream end/error, mark the node down and reconnect
//! with capped exponential backoff + jitter. A periodic full-list
//! reconcile catches anything the feed missed (tombstones are only
//! visible in listings).

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hypha_outbox::{Outbox, Status as OutboxStatus};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::client::{ClientConfig, FeedEvent, NodeClient, NodeError};
use crate::kind::{DiffTombstone, KindPolicies};
use crate::{NodeRef, NodeStatus, NodeWrite, PatchStatus, ProjectionStore, SyncError};

/// Sink for every feed frame the engine applies — the hub's push
/// fan-out rides it. Called per event before projection apply.
pub type FeedHook = Arc<dyn Fn(&str, &FeedEvent) + Send + Sync>;

/// Tuning knobs — production defaults; tests shrink the sleeps.
#[derive(Clone)]
pub struct SyncOptions {
    /// TCP connect budget for node calls.
    pub connect_timeout: Duration,
    /// End-to-end budget for REST calls (list, post, refetch).
    pub request_timeout: Duration,
    /// End-to-end budget for the *direct-delivery* attempt in
    /// [`SyncHandle::submit_op`] — deliberately below
    /// `request_timeout`: when the node is unreachable but the
    /// projection still reads `up` (the sync loop hasn't observed the
    /// failure yet — feed silence alone takes `sse_idle_timeout` to
    /// trip), the write must fail fast into the durable outbox rather
    /// than hold the caller for the full REST budget (T-060). An
    /// elapse is treated exactly like a transport error: the node is
    /// marked `down` and the op queues for replay. The orphaned
    /// in-flight post may still land node-side — the same ambiguity
    /// every transport timeout already carries.
    pub submit_timeout: Duration,
    /// Max feed silence before the SSE subscription is declared dropped
    /// — reconnected (and the index re-listed) when it lapses. The
    /// node's heartbeats keep a healthy stream open indefinitely; this
    /// also bounds how long a dead-silent SSE read can outlive engine
    /// shutdown.
    pub sse_idle_timeout: Duration,
    /// First reconnect delay.
    pub backoff_min: Duration,
    /// Reconnect-delay cap.
    pub backoff_max: Duration,
    /// Periodic full `list_entities` reconcile while subscribed.
    pub resync_interval: Duration,
    /// How long a self-declared *ephemeral* node (its `/api/node`
    /// `capabilities` carry `"ephemeral"` — CI runners) may stay
    /// continuously `down` before the engine reaps it: projected
    /// entities are dropped (a resurrected node resyncs itself) and
    /// its queued writes dead-letter. Ordinary nodes are never
    /// reaped — they keep retrying at `backoff_max` forever.
    pub ephemeral_dead_after: Duration,
    /// Capacity of the per-node SSE event channel.
    pub feed_channel: usize,
    /// Optional per-event hook (node id + frame). Inert `None`.
    pub on_event: Option<FeedHook>,
    /// Per-kind policy registry — diff-event projection, tombstone
    /// semantics, op arbitration, and entity-scoped call params
    /// resolve through it. Empty = fully generic behavior.
    pub policies: KindPolicies,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(15),
            // One connect budget plus slack — a node that can't accept
            // and ack a small op POST within this window is treated as
            // unreachable and the write queues.
            submit_timeout: Duration::from_secs(8),
            sse_idle_timeout: Duration::from_secs(60),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            resync_interval: Duration::from_secs(300),
            ephemeral_dead_after: Duration::from_secs(300),
            feed_channel: 256,
            on_event: None,
            policies: KindPolicies::new(),
        }
    }
}

impl SyncOptions {
    /// The node client for this engine — config plus the shared
    /// kind-policy registry.
    fn client(&self, url: &str, token: Option<String>) -> Result<NodeClient, NodeError> {
        NodeClient::with_config(url, token, self.client_config())
            .map(|c| c.with_policies(self.policies.clone()))
    }
}

impl SyncOptions {
    fn client_config(&self) -> ClientConfig {
        ClientConfig {
            connect_timeout: self.connect_timeout,
            request_timeout: self.request_timeout,
            sse_idle_timeout: self.sse_idle_timeout,
        }
    }
}

/// A node registration — the [`NodeRef`] plus its bearer credential.
#[derive(Clone, Debug)]
pub struct NodeTarget {
    /// The node.
    pub node: NodeRef,
    /// Bearer token minted by `POST /api/pair` on that node.
    pub token: Option<String>,
}

impl NodeTarget {
    pub fn new(node: NodeRef) -> Self {
        Self { node, token: None }
    }

    pub fn with_token(node: NodeRef, token: impl Into<String>) -> Self {
        Self {
            node,
            token: Some(token.into()),
        }
    }
}

/// What [`SyncHandle::submit_op`] did with a write.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// The node was up and accepted the write — the response body.
    Applied(Value),
    /// The node is down (or the write hit a transport error) — queued
    /// for replay on reconnect. `Box` keeps the enum small.
    Queued(Box<hypha_outbox::OutboxEntry>),
}

/// Shared state between the supervisor, the per-node loops, and the
/// public handle.
struct Shared {
    projection: ProjectionStore,
    outbox: Outbox,
    options: SyncOptions,
    /// Registered nodes — also the source of per-node bearer tokens for
    /// `submit_op`'s direct path.
    targets: Mutex<HashMap<String, NodeTarget>>,
    /// Live SSE pump threads — exposed via `SyncHandle::active_pumps`
    /// so shutdown/teardown can be observed, never blocked on.
    active_pumps: Arc<AtomicUsize>,
}

/// Messages the SSE pump thread sends back to the async node loop.
enum PumpMsg {
    /// `GET /api/events` returned 2xx — the node has registered the
    /// broadcast subscription. Always the first message on success so
    /// the loop can order "subscribe → list" without a diff gap.
    Subscribed,
    Event(FeedEvent),
    /// EOF or read error — the connection is over either way; the
    /// payload is `None` for a clean close.
    Closed(Option<NodeError>),
}

/// The entry point: spawns the supervisor and returns a [`SyncHandle`].
pub struct SyncEngine;

impl SyncEngine {
    /// Start the engine — one task per node plus a supervisor that owns
    /// the [`JoinSet`]. Additional nodes can join/leave later via
    /// [`SyncHandle::add_node`]/[`SyncHandle::remove_node`].
    pub fn spawn(
        projection: ProjectionStore,
        outbox: Outbox,
        nodes: impl IntoIterator<Item = NodeTarget>,
        options: SyncOptions,
    ) -> SyncHandle {
        // One registry drives both the wire calls and the projection's
        // column extraction — attach it here so callers can't drift.
        let projection = projection.with_kind_policies(options.policies.clone());
        let cancel = CancellationToken::new();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(64);
        let shared = Arc::new(Shared {
            projection,
            outbox,
            options,
            targets: Mutex::new(HashMap::new()),
            active_pumps: Arc::new(AtomicUsize::new(0)),
        });
        {
            let mut targets = shared
                .targets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for target in nodes {
                targets.insert(target.node.id.clone(), target);
            }
        }
        let supervisor = {
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            tokio::spawn(async move { supervise(shared, cmd_rx, cancel).await })
        };
        SyncHandle {
            shared,
            cmds: cmd_tx,
            cancel,
            supervisor,
        }
    }
}

enum Cmd {
    Add(NodeTarget),
    Remove(String),
}

/// Handle to a running engine — cloneable; `shutdown` consumes it.
pub struct SyncHandle {
    shared: Arc<Shared>,
    cmds: mpsc::Sender<Cmd>,
    cancel: CancellationToken,
    supervisor: tokio::task::JoinHandle<()>,
}

impl SyncHandle {
    /// The projection the engine maintains — safe to read from anywhere.
    pub fn projection(&self) -> ProjectionStore {
        self.shared.projection.clone()
    }

    /// The outbox — inspect `dead_letters`/`pending_for_node` directly.
    pub fn outbox(&self) -> Outbox {
        self.shared.outbox.clone()
    }

    /// Live SSE pump threads — `0` means every node loop is idle or
    /// offline (used by tests to assert clean teardown).
    pub fn active_pumps(&self) -> usize {
        self.shared.active_pumps.load(Ordering::Relaxed)
    }

    /// The direct-delivery budget [`Self::submit_op`] enforces — the
    /// hub's synchronous fast paths (entity create) share it so a
    /// node that stops answering fails fast everywhere, not just on
    /// op submits (T-060).
    pub fn submit_timeout(&self) -> Duration {
        self.shared.options.submit_timeout
    }

    /// The shared pump counter — cloneable so callers can keep
    /// observing teardown after `shutdown()` consumes the handle.
    pub fn pump_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.shared.active_pumps)
    }

    /// Register a node and start syncing it. Re-registering an existing
    /// id retargets the node — the running loop is aborted and respawned
    /// with the new target (e.g. a credential minted after boot); the
    /// projection's node row + session index survive the swap.
    ///
    /// # Errors
    /// [`SyncError::Shutdown`] if the engine is gone; sqlite failure
    /// writing the node row.
    pub async fn add_node(&self, target: NodeTarget) -> Result<(), SyncError> {
        self.shared.projection.upsert_node(&target.node)?;
        self.cmds
            .send(Cmd::Add(target))
            .await
            .map_err(|_| SyncError::Shutdown)
    }

    /// Stop syncing a node and drop its projected state.
    ///
    /// # Errors
    /// [`SyncError::Shutdown`] if the engine is gone; sqlite failure.
    pub async fn remove_node(&self, id: &str) -> Result<(), SyncError> {
        self.shared.projection.remove_node(id)?;
        self.cmds
            .send(Cmd::Remove(id.to_string()))
            .await
            .map_err(|_| SyncError::Shutdown)
    }

    /// A write to a node: posted immediately when the node is up;
    /// appended to the durable outbox (per-entity FIFO) when it's down
    /// or the direct post fails on transport. HTTP rejections surface as
    /// errors — they were delivered and refused, not lost.
    ///
    /// # Errors
    /// [`SyncError::UnknownNode`] for unregistered ids,
    /// [`SyncError::Node`] when the node rejects the write, and
    /// outbox/sqlite failures.
    #[tracing::instrument(
        name = "sync_submit_op",
        skip_all,
        fields(node.id = %write.node_id, entity = %write.entity_id, op = %write.op)
    )]
    pub async fn submit_op(&self, write: NodeWrite) -> Result<SubmitOutcome, SyncError> {
        let target = {
            self.shared
                .targets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&write.node_id)
                .cloned()
        };
        let Some(target) = target else {
            return Err(SyncError::UnknownNode(write.node_id.clone()));
        };

        let up = self
            .shared
            .projection
            .node(&write.node_id)?
            .is_some_and(|row| row.status == NodeStatus::Up);

        if up {
            let client = self
                .shared
                .options
                .client(&target.node.url, target.token.clone())?;
            let entity_id = write.entity_id.clone();
            let kind = write.entity_kind.clone();
            let op = write.op.clone();
            let payload = write.payload.clone();
            // Bounded by `submit_timeout`, not `request_timeout` —
            // the direct-delivery probe must fail fast into the
            // outbox when the node is gone but not yet marked `down`
            // (T-060). `timeout` drops the await, not the blocking
            // post: the orphaned call still finishes on its own
            // `request_timeout` budget and its result is discarded.
            let result = tokio::time::timeout(
                self.shared.options.submit_timeout,
                blocking(client.clone(), move |c| {
                    c.post_op(&entity_id, kind.as_deref(), &op, &payload)
                }),
            )
            .await;
            match result {
                Ok(Ok(value)) => return Ok(SubmitOutcome::Applied(value)),
                Ok(Err(e)) if e.is_transport() => {
                    // Fresh evidence the node is gone — reflect it and
                    // queue; the loop's reconnect drains in order.
                    let _ = self
                        .shared
                        .projection
                        .set_node_status(&write.node_id, NodeStatus::Down);
                }
                Ok(Err(e)) => return Err(SyncError::Node(e)),
                // The submit budget lapsed — same ruling as a
                // transport failure: mark down, queue, replay later.
                Err(_) => {
                    let _ = self
                        .shared
                        .projection
                        .set_node_status(&write.node_id, NodeStatus::Down);
                }
            }
        }

        let entry = self.shared.outbox.enqueue(
            &write.node_id,
            &write.entity_id,
            &write.op,
            write.policy,
            write.payload,
            &write.idempotency_key,
            write.ttl_seconds,
        )?;
        // A confirmed-dead ephemeral node's writes can never replay —
        // dead-letter them now rather than leaving them pending until
        // the sync loop's next reap pass.
        if ephemeral_dead(
            &self.shared.projection,
            &write.node_id,
            self.shared.options.ephemeral_dead_after,
        ) {
            let _ = self
                .shared
                .outbox
                .dead_letter_node(&write.node_id, "ephemeral node down past reap threshold");
        }
        Ok(SubmitOutcome::Queued(Box::new(entry)))
    }

    /// Stop every loop and await the supervisor. The SSE pump threads
    /// exit at the next feed frame or the `sse_idle_timeout` read
    /// deadline — [`Self::active_pumps`] reports stragglers.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.supervisor.await;
    }
}

/// The supervisor — owns the `JoinSet` of per-node loops and the
/// add/remove command channel.
async fn supervise(shared: Arc<Shared>, mut rx: mpsc::Receiver<Cmd>, cancel: CancellationToken) {
    let mut set: JoinSet<()> = JoinSet::new();
    let mut tasks: HashMap<String, tokio::task::AbortHandle> = HashMap::new();
    {
        let targets: Vec<NodeTarget> = shared
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for target in targets {
            if shared.projection.upsert_node(&target.node).is_err() {
                tracing::warn!(node = %target.node.id, "failed to register node in projection");
            }
            let abort = set.spawn(node_loop(
                target.clone(),
                Arc::clone(&shared),
                cancel.clone(),
            ));
            tasks.insert(target.node.id.clone(), abort);
        }
    }
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::Add(target)) => {
                    shared
                        .targets
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(target.node.id.clone(), target.clone());
                    // A re-add retargets the node — abort the old loop
                    // (its SSE pump exits at the next frame or the
                    // `sse_idle_timeout` deadline) and respawn with the
                    // new url/token. The projection keeps its rows.
                    if let Some(prev) = tasks.remove(&target.node.id) {
                        prev.abort();
                    }
                    let abort = set.spawn(node_loop(
                        target.clone(),
                        Arc::clone(&shared),
                        cancel.clone(),
                    ));
                    tasks.insert(target.node.id.clone(), abort);
                }
                Some(Cmd::Remove(id)) => {
                    shared
                        .targets
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id);
                    if let Some(abort) = tasks.remove(&id) {
                        abort.abort();
                    }
                }
                None => break,
            },
            Some(Err(e)) = set.join_next() => report_join_error(&e),
            () = cancel.cancelled() => break,
        }
    }
    // `shutdown` aborts whatever didn't observe the token yet; the
    // loops' own `cancelled` arms exit promptly regardless.
    set.shutdown().await;
}

/// A node-loop task died — panics are the only noteworthy cause.
/// Named so the arm is one region tests can drive directly.
fn report_join_error(e: &tokio::task::JoinError) {
    if e.is_panic() {
        tracing::error!("node sync loop panicked: {e}");
    }
}

/* ---- per-node loop --------------------------------------------------*/

/// Why one connect-cycle ended.
enum CycleEnd {
    /// Engine shutdown.
    Cancelled,
    /// `list_entities` failed — the node is down; exponential backoff.
    PreConnect(NodeError),
    /// We were connected and the SSE stream ended — quick reconnect.
    Disconnected(NodeError),
}

#[tracing::instrument(
    name = "sync_node_loop",
    skip_all,
    fields(node.id = %target.node.id, node.url = %target.node.url)
)]
async fn node_loop(target: NodeTarget, shared: Arc<Shared>, cancel: CancellationToken) {
    let node_id = target.node.id.clone();
    let client = match shared
        .options
        .client(&target.node.url, target.token.clone())
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(node = %node_id, "bad node config: {e}");
            return;
        }
    };
    let mut delay = shared.options.backoff_min;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match connect_once(&target.node, &client, &shared, &cancel).await {
            CycleEnd::Cancelled => break,
            CycleEnd::PreConnect(e) => {
                tracing::debug!(node = %node_id, "connect failed: {e}");
            }
            CycleEnd::Disconnected(e) => {
                tracing::info!(node = %node_id, "connection lost: {e}");
                delay = shared.options.backoff_min;
            }
        }
        if shared
            .projection
            .set_node_status(&node_id, NodeStatus::Down)
            .is_err()
        {
            tracing::warn!(node = %node_id, "failed to mark node down");
        }
        // Self-declared ephemeral nodes get reaped once they've been
        // down past `ephemeral_dead_after`. The check is idempotent,
        // so it runs on every failed cycle — cheap, and it also
        // catches writes enqueued between reaps.
        if ephemeral_dead(
            &shared.projection,
            &node_id,
            shared.options.ephemeral_dead_after,
        ) {
            reap_ephemeral_node(&shared, &node_id);
        }
        // A previously-healthy stream that dropped retries fast;
        // repeated connect failures back off exponentially.
        let wait = jittered(delay);
        delay = delay.saturating_mul(2).min(shared.options.backoff_max);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = cancel.cancelled() => break,
        }
    }
}

/// One connect cycle: subscribe → list → reconcile → drain → apply.
/// Returns only when the subscription ends or the engine is cancelled.
///
/// Ordering: the SSE subscription opens *before* the listing so a diff
/// emitted while the list is in flight lands on the stream — anything
/// earlier is covered by the listing itself. The outbox drains last so
/// the feed events a replayed write triggers are observed.
#[tracing::instrument(name = "sync_connect", skip_all, fields(node.id = %node.id))]
async fn connect_once(
    node: &NodeRef,
    client: &NodeClient,
    shared: &Arc<Shared>,
    cancel: &CancellationToken,
) -> CycleEnd {
    // The pump is a dedicated blocking thread feeding an mpsc; the
    // async side stays cancel-instant.
    let (tx, mut rx) = mpsc::channel::<PumpMsg>(shared.options.feed_channel);
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let client = client.clone();
        let stop = Arc::clone(&stop);
        let shared = Arc::clone(shared);
        std::thread::spawn(move || sse_pump(&client, &tx, &stop, &shared))
    };

    // Wait for the subscription handshake (or connect failure).
    let subscribed = tokio::select! {
        () = cancel.cancelled() => {
            stop.store(true, Ordering::Relaxed);
            // The pump may be parked in `blocking_send` on a full
            // channel — dropping `rx` fails that send so the join
            // below can't hang.
            drop(rx);
            let _ = tokio::task::spawn_blocking(move || pump.join()).await;
            return CycleEnd::Cancelled;
        }
        msg = rx.recv() => match msg {
            Some(PumpMsg::Subscribed) => true,
            Some(PumpMsg::Closed(e)) => {
                // Pre-subscribe closes always carry the connect error —
                // `unwrap_or` keeps the fallback a plain evaluated arg
                // instead of a dead closure.
                return CycleEnd::PreConnect(e.unwrap_or(NodeError::Malformed(
                    "event stream closed before subscribe".into(),
                )));
            }
            Some(PumpMsg::Event(_)) | None => {
                return CycleEnd::Disconnected(NodeError::Malformed(
                    "pump ended before subscribe".into(),
                ));
            }
        },
    };
    debug_assert!(subscribed);

    // Listing — a success means the node is reachable *and* the
    // projection is freshly reconciled (tombstones included).
    let docs = match blocking(client.clone(), move |c| c.list_entities(None, true)).await {
        Ok(s) => s,
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            drop(rx);
            let _ = tokio::task::spawn_blocking(move || pump.join()).await;
            return CycleEnd::Disconnected(e);
        }
    };
    if let Err(e) = shared.projection.apply_listing(&node.id, &docs) {
        tracing::warn!(node = %node.id, "projection apply_listing failed: {e}");
    }
    // Self-reported lifecycle + fleet labels: `/api/node`'s
    // `capabilities` carry `"ephemeral"` for disposable nodes (CI
    // runners) plus integration-defined tags (`"runner:linux"`, …)
    // schedulers match `runs_on` requirements against. Best-effort —
    // a node that doesn't answer keeps its previous values.
    if let Ok(info) = blocking(client.clone(), |c| c.get_json("/api/node")).await {
        let capabilities: Vec<String> = info
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|caps| {
                caps.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let ephemeral = capabilities.iter().any(|c| c == "ephemeral");
        if let Err(e) = shared.projection.set_node_ephemeral(&node.id, ephemeral) {
            tracing::warn!(node = %node.id, "failed to record node capabilities: {e}");
        }
        if let Err(e) = shared
            .projection
            .set_node_capabilities(&node.id, &capabilities)
        {
            tracing::warn!(node = %node.id, "failed to record node capabilities: {e}");
        }
    }
    if let Err(e) = shared.projection.set_node_status(&node.id, NodeStatus::Up) {
        tracing::warn!(node = %node.id, "failed to mark node up: {e}");
    }
    drain_outbox(node, client, shared).await;

    let mut resync = tokio::time::interval_at(
        tokio::time::Instant::now() + shared.options.resync_interval,
        shared.options.resync_interval,
    );
    // The stream's silence bound — re-armed by every wire frame (the
    // node's heartbeats keep a quiet feed alive). Needed because the
    // ureq body budget can't express a per-read idle timeout (see
    // `open_stream`), so this deadline is enforced engine-side.
    let mut silence = tokio::time::Instant::now() + shared.options.sse_idle_timeout;
    let end = loop {
        tokio::select! {
            () = cancel.cancelled() => break CycleEnd::Cancelled,
            () = tokio::time::sleep_until(silence) => {
                break CycleEnd::Disconnected(NodeError::Malformed(
                    "event stream silent".into(),
                ));
            }
            msg = rx.recv() => match msg {
                None => break CycleEnd::Disconnected(NodeError::Malformed("pump ended".into())),
                // The pump sends Subscribed exactly once, before any
                // frames — a duplicate is inert.
                Some(PumpMsg::Subscribed) => {}
                Some(PumpMsg::Closed(e)) => {
                    break CycleEnd::Disconnected(e.unwrap_or_else(|| {
                        NodeError::Malformed("event stream ended".into())
                    }));
                }
                Some(PumpMsg::Event(event)) => {
                    silence = tokio::time::Instant::now() + shared.options.sse_idle_timeout;
                    if let Err(e) = apply_event(node, client, shared, event).await {
                        tracing::warn!(node = %node.id, "apply event failed: {e}");
                    }
                }
            },
            _ = resync.tick() => {
                match blocking(client.clone(), |c| c.list_entities(None, true)).await {
                    Ok(docs) => {
                        if let Err(e) = shared.projection.apply_listing(&node.id, &docs) {
                            tracing::warn!(node = %node.id, "resync failed: {e}");
                        }
                    }
                    Err(e) => break CycleEnd::Disconnected(e),
                }
            }
        }
    };

    stop.store(true, Ordering::Relaxed);
    // `rx` is dropped first — a pump parked in `blocking_send` on a
    // full channel fails immediately instead of hanging the join.
    // Otherwise bounded by `sse_idle_timeout` (the read deadline).
    drop(rx);
    let _ = tokio::task::spawn_blocking(move || pump.join()).await;
    end
}

/// Blocking SSE pump — one std thread per live connection. Reads
/// frames, forwards them to the async loop, checks `stop` between
/// frames (the node's heartbeat cadence and the `sse_idle_timeout`
/// silence bound cap the wakeup latency).
fn sse_pump(
    client: &NodeClient,
    tx: &mpsc::Sender<PumpMsg>,
    stop: &Arc<AtomicBool>,
    shared: &Arc<Shared>,
) {
    shared.active_pumps.fetch_add(1, Ordering::Relaxed);
    let finish = |msg: PumpMsg| {
        let _ = tx.blocking_send(msg);
        shared.active_pumps.fetch_sub(1, Ordering::Relaxed);
    };
    match client.events() {
        Err(e) => finish(PumpMsg::Closed(Some(e))),
        Ok(mut stream) => {
            // Acknowledge the subscription before frames — the async
            // side lists only after this, closing the diff gap.
            if tx.blocking_send(PumpMsg::Subscribed).is_err() {
                shared.active_pumps.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            loop {
                if stop.load(Ordering::Relaxed) {
                    shared.active_pumps.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                match stream.next_event() {
                    Ok(Some(event)) => {
                        if tx.blocking_send(PumpMsg::Event(event)).is_err() {
                            shared.active_pumps.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }
                    }
                    Ok(None) => {
                        finish(PumpMsg::Closed(None));
                        return;
                    }
                    Err(e) => {
                        finish(PumpMsg::Closed(Some(e)));
                        return;
                    }
                }
            }
        }
    }
}

/// Drain queued writes oldest-first. Per-entity FIFO is preserved by
/// skipping the rest of an entity's entries once one fails back to
/// `pending` (it stays head-of-queue for the next drain).
#[tracing::instrument(name = "sync_outbox_drain", skip_all, fields(node.id = %node.id))]
async fn drain_outbox(node: &NodeRef, client: &NodeClient, shared: &Arc<Shared>) {
    let pending = match shared.outbox.pending_for_node(&node.id) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(node = %node.id, "outbox read failed: {e}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    tracing::info!(node = %node.id, "draining {} queued write(s)", pending.len());
    let mut blocked: HashSet<String> = HashSet::new();
    for entry in pending {
        if blocked.contains(&entry.entity_id) {
            continue;
        }
        if let Err(e) = shared.outbox.mark_in_flight(&entry.id) {
            tracing::warn!(node = %node.id, "mark_in_flight failed: {e}");
            continue;
        }
        // `?kind` scope comes from the projection — the outbox only
        // stores the entity id.
        let kind = shared
            .projection
            .entity(&node.id, &entry.entity_id)
            .ok()
            .flatten()
            .map(|e| e.kind)
            .filter(|k| !k.is_empty());
        let entity_id = entry.entity_id.clone();
        let op = entry.op.clone();
        let payload = entry.payload.clone();
        let result = blocking(client.clone(), move |c| {
            c.post_op(&entity_id, kind.as_deref(), &op, &payload)
        })
        .await;
        match result {
            Ok(_) => {
                if let Err(e) = shared.outbox.mark_done(&entry.id) {
                    tracing::warn!(node = %node.id, "mark_done failed: {e}");
                }
            }
            Err(e) => {
                tracing::warn!(node = %node.id, op = %entry.op, "replay failed: {e}");
                match shared.outbox.mark_failed(&entry.id, &e.to_string()) {
                    Ok(OutboxStatus::Pending) => {
                        blocked.insert(entry.entity_id.clone());
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(node = %node.id, "mark_failed failed: {err}"),
                }
            }
        }
    }
}

/// `true` when `node_id`'s row is a self-declared ephemeral node that
/// has been continuously `down` longer than `after` — the reap
/// predicate. `down_since` is an RFC 3339 stamp persisted in the
/// projection, so the verdict survives hub restarts.
fn ephemeral_dead(projection: &ProjectionStore, node_id: &str, after: Duration) -> bool {
    let Ok(Some(row)) = projection.node(node_id) else {
        return false;
    };
    if !row.ephemeral {
        return false;
    }
    let Some(since) = row.down_since.as_deref() else {
        return false;
    };
    let Ok(since) =
        time::OffsetDateTime::parse(since, &time::format_description::well_known::Rfc3339)
    else {
        return false;
    };
    let threshold = time::Duration::seconds(i64::try_from(after.as_secs()).unwrap_or(i64::MAX));
    // A future/garbage stamp reads as not-yet-dead — the subtraction
    // goes negative and the comparison fails.
    time::OffsetDateTime::now_utc() - since > threshold
}

/// Reap a confirmed-dead ephemeral node: drop its projected entities
/// and resync cursor (a resurrected node resyncs itself) and
/// dead-letter its queued writes — they can never replay. The `nodes`
/// row stays so node status keeps reporting `down` + `ephemeral`.
fn reap_ephemeral_node(shared: &Arc<Shared>, node_id: &str) {
    if let Err(e) = shared.projection.drop_node_entities(node_id) {
        tracing::warn!(node = %node_id, "ephemeral reap: entity drop failed: {e}");
    }
    match shared
        .outbox
        .dead_letter_node(node_id, "ephemeral node down past reap threshold")
    {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!(node = %node_id, "ephemeral reap: dead-lettered {n} queued write(s)");
        }
        Err(e) => tracing::warn!(node = %node_id, "ephemeral reap: dead-letter failed: {e}"),
    }
}

/// Apply one feed frame to the projection.
#[tracing::instrument(name = "sync_apply_event", skip_all, fields(node.id = %node.id))]
async fn apply_event(
    node: &NodeRef,
    client: &NodeClient,
    shared: &Arc<Shared>,
    event: FeedEvent,
) -> Result<(), SyncError> {
    if let Some(hook) = &shared.options.on_event {
        hook(&node.id, &event);
    }
    match event {
        // Any diff a registered kind policy claims projects; unknown
        // event names stay inert (entity-detail frames never reach
        // here as `Diff` unless they carry an `id`).
        FeedEvent::Diff {
            kind, id, patch, ..
        } => {
            let Some((_, tombstone)) = shared.options.policies.diff_policy(&kind) else {
                return Ok(());
            };
            if patch.get("deleted") == Some(&Value::Bool(true)) {
                match tombstone {
                    DiffTombstone::DeleteEntity => {
                        shared.projection.delete_entity(&node.id, &id)?;
                        Ok(())
                    }
                    // Overlay-style tombstone — the row survives;
                    // refetch reconciles it.
                    DiffTombstone::Refetch => refetch(node, client, shared, &id).await,
                }
            } else {
                patch_or_refetch(node, client, shared, &id, &patch).await
            }
        }
        FeedEvent::Lagged => {
            let docs = blocking(client.clone(), |c| c.list_entities(None, true)).await?;
            shared.projection.apply_listing(&node.id, &docs)?;
            Ok(())
        }
        // Heartbeats, entity-stream frames, and unknown frames are
        // inert here.
        _ => Ok(()),
    }
}

/// Merge a diff, fetching the full entity doc when the row isn't
/// indexed yet (a `meta` patch can precede the entity's first list).
async fn patch_or_refetch(
    node: &NodeRef,
    client: &NodeClient,
    shared: &Arc<Shared>,
    id: &str,
    patch: &Value,
) -> Result<(), SyncError> {
    match shared.projection.apply_patch(&node.id, id, patch)? {
        PatchStatus::Applied => Ok(()),
        PatchStatus::Missing => refetch(node, client, shared, id).await,
    }
}

async fn refetch(
    node: &NodeRef,
    client: &NodeClient,
    shared: &Arc<Shared>,
    id: &str,
) -> Result<(), SyncError> {
    let entity_id = id.to_string();
    let result = blocking(client.clone(), move |c| c.get_entity(&entity_id, None)).await;
    match result {
        Ok(doc) => {
            shared.projection.upsert_entity(&node.id, &doc)?;
        }
        Err(NodeError::Http { status: 404, .. }) => {
            shared.projection.delete_entity(&node.id, id)?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// `spawn_blocking` wrapper — `JoinError` becomes [`NodeError::Task`].
async fn blocking<T, F>(client: NodeClient, f: F) -> Result<T, NodeError>
where
    T: Send + 'static,
    F: FnOnce(&NodeClient) -> Result<T, NodeError> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || f(&client)).await {
        Ok(result) => result,
        Err(e) => Err(NodeError::Task(e.to_string())),
    }
}

/// Capped exponential backoff with ±25% jitter — `RandomState` provides
/// the entropy so no `rand` dep is needed.
fn jittered(base: Duration) -> Duration {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let pct = 75 + u32::try_from(hasher.finish() % 51).unwrap_or(25);
    base.mul_f64(f64::from(pct) / 100.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::io::{Read, Write};

    fn options() -> SyncOptions {
        SyncOptions {
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_secs(2),
            submit_timeout: Duration::from_millis(200),
            sse_idle_timeout: Duration::from_secs(5),
            backoff_min: Duration::from_millis(30),
            backoff_max: Duration::from_millis(200),
            resync_interval: Duration::from_secs(3600),
            ephemeral_dead_after: Duration::from_secs(3600),
            feed_channel: 8,
            on_event: None,
            policies: KindPolicies::new(),
        }
    }

    fn shared() -> Arc<Shared> {
        shared_with(options())
    }

    fn shared_with(options: SyncOptions) -> Arc<Shared> {
        Arc::new(Shared {
            projection: ProjectionStore::in_memory().unwrap(),
            outbox: Outbox::in_memory().unwrap(),
            options,
            targets: Mutex::new(HashMap::new()),
            active_pumps: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn node(url: String) -> NodeRef {
        NodeRef {
            id: "n".into(),
            url,
            label: "n".into(),
        }
    }

    /// Tiny canned node: `/api/events` → a held-open SSE stream with a
    /// heartbeat cadence; anything else → `status` + `body`.
    fn canned_node(status: &'static str, body: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // One thread per connection — a held-open `/api/events`
            // stream must not block REST calls queued behind it.
            while let Ok((mut conn, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.contains("/api/events") {
                        let _ = conn.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                        );
                        for _ in 0..100 {
                            if conn.write_all(b"event: heartbeat\ndata: {}\n\n").is_err() {
                                break;
                            }
                            let _ = conn.flush();
                            std::thread::sleep(Duration::from_millis(30));
                        }
                    } else {
                        let _ = conn.write_all(
                            format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        );
                    }
                });
            }
        });
        port
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_join_error_maps_to_task() {
        let shared = shared();
        let client =
            NodeClient::with_config("http://127.0.0.1:1", None, shared.options.client_config())
                .unwrap();
        // A panicking closure → JoinError → `NodeError::Task`.
        let result: Result<(), NodeError> = blocking(client, |_| panic!("boom")).await;
        assert!(matches!(result, Err(NodeError::Task(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn report_join_error_both_arms() {
        let panicked = tokio::spawn(async { panic!("loop died") });
        report_join_error(&panicked.await.unwrap_err());
        let cancelled = tokio::spawn(async { std::future::pending::<()>().await });
        cancelled.abort();
        report_join_error(&cancelled.await.unwrap_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_once_pre_connect_close() {
        // Connection refused → pump reports `Closed` before `Subscribed`.
        let shared = shared();
        let node = node("http://127.0.0.1:1".into());
        let client =
            NodeClient::with_config(&node.url, None, shared.options.client_config()).unwrap();
        let end = connect_once(&node, &client, &shared, &CancellationToken::new()).await;
        assert!(matches!(end, CycleEnd::PreConnect(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_once_cancelled_during_handshake() {
        // Pre-cancelled → the handshake `select` takes the cancel arm and
        // drops `rx`; when the pump's `events()` call lands, its
        // `Subscribed` send fails and the pump self-decrements.
        let port = canned_node("200 OK", "[]");
        let shared = shared();
        let node = node(format!("http://127.0.0.1:{port}"));
        let client =
            NodeClient::with_config(&node.url, None, shared.options.client_config()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let end = connect_once(&node, &client, &shared, &cancel).await;
        assert!(matches!(end, CycleEnd::Cancelled));
        // Give the pump thread room to finish `events()` and hit the
        // failed `Subscribed` send (rx is gone).
        tokio::time::sleep(Duration::from_millis(800)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_once_full_cycle_then_stop() {
        // A live canned node: subscribe → list → drain → tail. Cancelling
        // mid-run makes connect_once set `stop`; the pump observes it at
        // its loop top between heartbeat frames.
        let port = canned_node("200 OK", "{\"entities\":[]}");
        let shared = shared();
        let node = node(format!("http://127.0.0.1:{port}"));
        let client =
            NodeClient::with_config(&node.url, None, shared.options.client_config()).unwrap();
        let cancel = CancellationToken::new();
        let task = {
            let node = node.clone();
            let client = client.clone();
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            tokio::spawn(async move { connect_once(&node, &client, &shared, &cancel).await })
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel.cancel();
        let end = task.await.unwrap();
        assert!(matches!(end, CycleEnd::Cancelled));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn node_loop_cancels_during_backoff() {
        // A dead node: connect fails fast → the loop parks in the
        // backoff `select!` — cancelling there takes the cancel arm.
        let shared = shared();
        let target = NodeTarget::new(node("http://127.0.0.1:1".into()));
        let cancel = CancellationToken::new();
        let task = {
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            tokio::spawn(async move { node_loop(target, shared, cancel).await })
        };
        // backoff_min is 30ms — at 200ms the loop is almost surely
        // parked in `sleep(wait)`; the cancel arm is what we're after.
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sse_pump_observes_stop_between_frames() {
        // Drive `sse_pump` directly: `stop` set while the pump is at its
        // loop top → the `stop.load` arm decrements and returns.
        let port = canned_node("200 OK", "{\"entities\":[]}");
        let shared = shared();
        let client = NodeClient::with_config(
            &format!("http://127.0.0.1:{port}"),
            None,
            shared.options.client_config(),
        )
        .unwrap();
        let (tx, mut rx) = mpsc::channel::<PumpMsg>(16);
        let stop = Arc::new(AtomicBool::new(false));
        let pump = {
            let client = client.clone();
            let stop = Arc::clone(&stop);
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || sse_pump(&client, &tx, &stop, &shared))
        };
        // Subscribed lands first; then heartbeat events. After one
        // event is drained, raising `stop` makes the next loop-top
        // check take the `stop` arm (bounded by the 30ms cadence).
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await,
            Ok(Some(PumpMsg::Subscribed))
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await,
            Ok(Some(PumpMsg::Event(_)))
        ));
        stop.store(true, Ordering::Relaxed);
        // Drain until the sender side drops (pump exited).
        while tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .is_ok_and(|m| m.is_some())
        {}
        pump.join().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refetch_non_404_error_propagates() {
        // 500 → neither the Ok nor the 404 arm — `Err(e)` returns.
        let port = canned_node("500 Internal Server Error", "{\"error\":\"down\"}");
        let shared = shared();
        let node = node(format!("http://127.0.0.1:{port}"));
        let client =
            NodeClient::with_config(&node.url, None, shared.options.client_config()).unwrap();
        assert!(refetch(&node, &client, &shared, "s1").await.is_err());
    }

    /// A TCP endpoint that accepts and then never answers — the
    /// blackhole node: connects succeed, every request hangs until a
    /// client-side timeout fires.
    fn blackhole_node() -> String {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // Hold every accepted socket open without reading or
            // writing — requests stall until their own timeout.
            while let Ok((conn, _)) = listener.accept() {
                std::mem::forget(conn);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// T-060 — a node that stops answering while the projection still
    /// reads `up` (the sync loop hasn't observed the failure yet) must
    /// not hold `submit_op` for the full `request_timeout`: the direct
    /// attempt is bounded by `submit_timeout`, then the write lands in
    /// the outbox and the node flips `down`. The *next* submit skips
    /// the probe entirely.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_op_fails_fast_into_outbox_when_node_hangs() {
        let mut opts = options();
        // A REST budget comfortably past the test's own runtime keeps
        // the node loop's probes from marking the node down before
        // the submit under test runs — the window being exercised is
        // "status still up, node dead". (It also bounds how long the
        // orphaned post lingers on a blocking thread.)
        opts.request_timeout = Duration::from_secs(5);
        let shared = shared_with(opts);
        let url = blackhole_node();
        let n = node(url);
        shared.projection.upsert_node(&n).unwrap();
        let engine = SyncEngine::spawn(
            shared.projection.clone(),
            shared.outbox.clone(),
            [NodeTarget::new(n.clone())],
            shared.options.clone(),
        );
        // Fresh evidence window: the loop hasn't run a cycle yet, so
        // the row is `unknown` — pretend the last connect succeeded.
        shared
            .projection
            .set_node_status("n", NodeStatus::Up)
            .unwrap();

        let write = |key: &str| NodeWrite {
            node_id: "n".into(),
            entity_id: "e1".into(),
            entity_kind: None,
            op: "op".into(),
            policy: hypha_outbox::OpKind::Turn,
            payload: serde_json::json!({}),
            idempotency_key: key.to_string(),
            ttl_seconds: None,
        };

        let start = std::time::Instant::now();
        let outcome = engine.submit_op(write("k1")).await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, SubmitOutcome::Queued(_)),
            "hung node must queue, got {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "submit blocked {elapsed:?} — the submit_timeout bound didn't hold"
        );
        assert_eq!(
            shared.projection.node("n").unwrap().unwrap().status,
            NodeStatus::Down,
            "the lapsed probe must mark the node down"
        );
        assert_eq!(shared.outbox.pending_for_node("n").unwrap().len(), 1);

        // Known-down now: the next submit skips the direct attempt
        // outright — no probe, no wait.
        let start = std::time::Instant::now();
        let outcome = engine.submit_op(write("k2")).await.unwrap();
        assert!(matches!(outcome, SubmitOutcome::Queued(_)));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "known-down submit must not probe the node"
        );
        assert_eq!(shared.outbox.pending_for_node("n").unwrap().len(), 2);

        engine.shutdown().await;
    }

    /// Canned node routing on the request path: `/api/events` → the
    /// held-open heartbeat stream, `/api/node` → `node_info`,
    /// everything else → `rest` (the entity listing).
    fn routed_node(node_info: &'static str, rest: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut conn, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.contains("/api/events") {
                        let _ = conn.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                        );
                        for _ in 0..100 {
                            if conn.write_all(b"event: heartbeat\ndata: {}\n\n").is_err() {
                                break;
                            }
                            let _ = conn.flush();
                            std::thread::sleep(Duration::from_millis(30));
                        }
                    } else {
                        let body = if req.contains("/api/node") {
                            node_info
                        } else {
                            rest
                        };
                        let _ = conn.write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        );
                    }
                });
            }
        });
        port
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_once_records_self_reported_ephemeral() {
        // `/api/node` capabilities carry `"ephemeral"` → the node row
        // records it; without the tag the flag stays false. The full
        // capability list persists alongside for fleet routing.
        for (caps, want) in [
            (r#"["entities","ephemeral","runner:linux"]"#, true),
            (r#"["entities","events"]"#, false),
        ] {
            let info = format!(r#"{{"id":"n","capabilities":{caps}}}"#);
            // Leak the per-iteration body — the canned thread needs
            // 'static; tests are short-lived.
            let info: &'static str = Box::leak(info.into_boxed_str());
            let port = routed_node(info, "{\"entities\":[]}");
            let shared = shared();
            let n = node(format!("http://127.0.0.1:{port}"));
            shared.projection.upsert_node(&n).unwrap();
            let client =
                NodeClient::with_config(&n.url, None, shared.options.client_config()).unwrap();
            let cancel = CancellationToken::new();
            let task = {
                let n = n.clone();
                let client = client.clone();
                let shared = Arc::clone(&shared);
                let cancel = cancel.clone();
                tokio::spawn(async move { connect_once(&n, &client, &shared, &cancel).await })
            };
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancel.cancel();
            let _ = task.await.unwrap();
            let row = shared.projection.node("n").unwrap().unwrap();
            assert_eq!(row.ephemeral, want, "capabilities {caps}");
            assert_eq!(row.status, NodeStatus::Up);
            let want_caps: Vec<String> = serde_json::from_str::<Vec<String>>(caps)
                .unwrap()
                .into_iter()
                .collect();
            assert_eq!(row.capabilities, want_caps, "capabilities {caps}");
        }
    }

    /// A dead ephemeral node past `ephemeral_dead_after`: projected
    /// entities drop, queued writes dead-letter, the node row stays
    /// `down` + `ephemeral`. A non-ephemeral node keeps everything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn node_loop_reaps_only_dead_ephemeral_nodes() {
        for ephemeral in [true, false] {
            let mut opts = options();
            // Sub-second thresholds collapse to 0s — any observed down
            // duration reaps immediately.
            opts.ephemeral_dead_after = Duration::from_millis(1);
            let shared = shared_with(opts);
            let n = node("http://127.0.0.1:1".into());
            shared.projection.upsert_node(&n).unwrap();
            shared
                .projection
                .set_node_ephemeral("n", ephemeral)
                .unwrap();
            shared
                .projection
                .upsert_entity("n", &serde_json::json!({"id":"e1","kind":"k"}))
                .unwrap();
            shared
                .outbox
                .enqueue(
                    "n",
                    "e1",
                    "op",
                    hypha_outbox::OpKind::Turn,
                    serde_json::json!({}),
                    "k1",
                    None,
                )
                .unwrap();
            let target = NodeTarget::new(n);
            let cancel = CancellationToken::new();
            let task = {
                let shared = Arc::clone(&shared);
                let cancel = cancel.clone();
                tokio::spawn(async move { node_loop(target, shared, cancel).await })
            };
            tokio::time::sleep(Duration::from_millis(400)).await;
            cancel.cancel();
            task.await.unwrap();

            let row = shared.projection.node("n").unwrap().unwrap();
            assert_eq!(row.status, NodeStatus::Down);
            assert!(row.down_since.is_some());
            if ephemeral {
                assert_eq!(shared.projection.entities(Some("n")).unwrap().len(), 0);
                assert_eq!(shared.outbox.pending_for_node("n").unwrap().len(), 0);
                assert_eq!(shared.outbox.dead_letters().unwrap().len(), 1);
            } else {
                assert_eq!(shared.projection.entities(Some("n")).unwrap().len(), 1);
                assert_eq!(shared.outbox.pending_for_node("n").unwrap().len(), 1);
                assert_eq!(shared.outbox.dead_letters().unwrap().len(), 0);
            }
        }
    }

    #[test]
    fn ephemeral_dead_predicate_arms() {
        // Missing row / not ephemeral / no down_since / fresh down →
        // all false; a stamped past down_since on an ephemeral node →
        // true. `down_since` is only writable via `set_node_status`,
        // so the "past" arm rides a zero-ish threshold instead.
        let shared = shared();
        let proj = &shared.projection;
        proj.upsert_node(&node("http://127.0.0.1:1".into()))
            .unwrap();
        assert!(!ephemeral_dead(proj, "n", Duration::ZERO));
        proj.set_node_ephemeral("n", true).unwrap();
        // Up → no down_since.
        proj.set_node_status("n", NodeStatus::Up).unwrap();
        assert!(!ephemeral_dead(proj, "n", Duration::ZERO));
        // Fresh down with a real threshold → false.
        proj.set_node_status("n", NodeStatus::Down).unwrap();
        assert!(!ephemeral_dead(proj, "n", Duration::from_secs(3600)));
        // Zero threshold → any elapsed down counts.
        assert!(ephemeral_dead(proj, "n", Duration::ZERO));
        // Unknown node id → false.
        assert!(!ephemeral_dead(proj, "ghost", Duration::ZERO));
        // Non-ephemeral with an old-enough down → still false.
        proj.set_node_ephemeral("n", false).unwrap();
        assert!(!ephemeral_dead(proj, "n", Duration::ZERO));
    }
}
