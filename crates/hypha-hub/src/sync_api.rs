//! [`SyncEntityApi`] — the hub's [`EntityApi`] backed by `hypha-sync`:
//! the entity list reads the local projection (merged across every
//! paired node), detail + detail-stream traffic proxies to the owning
//! node live, and ops go through `SyncEngine::submit_op` so they queue
//! in the durable outbox when the node is down.
//!
//! Kind-specific behavior — op gating (`sepia.*` vs `*.meta*`),
//! create validation (`"session"` alias), outbox op classification,
//! external lock holders (`indexDoc.lockHolderPid`), and push rules —
//! resolves through the [`KindPolicies`] registry
//! ([`crate::kind_policies`]); this file names no kind itself. When
//! the integration-backed `/api/entities` surface lands (PL-004
//! phase 4), the registrations move with their integrations.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use hypha_control::{Actor, Arbitration, Decision, LockRegistry, NodeId};
use hypha_sync::client::NodeClient;
use hypha_sync::kind::KindPolicies;
use hypha_sync::{
    IndexedEntity, NodeRef, NodeTarget, NodeWrite, OpKind, OutboxEntry, OutboxStatus, SyncError,
    SyncHandle,
};

use crate::credentials::NodeCredentials;
use hypha_node_api::api::EntityApi;
use hypha_node_api::dto::{
    CreateResultDto, EntityDto, EntityIndexDto, EntityOp, NodeInfoDto, NodeStatusDto, PendingOpDto,
    PushSubscriptionDto,
};
use serde_json::Value;

/// Node registry entry — url + optional bearer token.
#[derive(Clone, Default)]
pub struct HubNode {
    pub id: String,
    pub url: String,
    pub token: Option<String>,
}

/// `HYPHA_NODES` format: `id=url[@token];id2=url2[@token2]` — the `@`
/// is unambiguous: a URL never carries one in this context.
pub fn parse_nodes(raw: &str) -> Vec<HubNode> {
    raw.split(';')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (id, rest) = part.split_once('=')?;
            let (url, token) = match rest.split_once('@') {
                Some((u, t)) => (u, if t.is_empty() { None } else { Some(t) }),
                None => (rest, None),
            };
            Some(HubNode {
                id: id.trim().to_string(),
                url: url.trim().trim_end_matches('/').to_string(),
                token: token.map(str::to_string),
            })
        })
        .collect()
}

/// Boot-time token resolution — the [`crate::credentials`] doc has the
/// model. An env `@token`/`HYPHA_NODE_TOKEN` is explicit config: it
/// wins for this boot and seeds the store when the node has no stored
/// credential yet — the migration path that lets the plaintext leave
/// the env (drop `@token` after one boot; the token keeps working from
/// the store). A stored credential is never overwritten by env —
/// rotation goes through re-pairing (`adopt_credential` writes the
/// minted token through). Tokenless nodes resolve a stored credential
/// into `node.token`.
///
/// Returns `(resolved, seeded)` for the boot log. Store errors warn
/// and degrade — a broken backend must not keep the hub from booting.
pub fn resolve_node_tokens(nodes: &mut [HubNode], creds: &dyn NodeCredentials) -> (usize, usize) {
    let (mut resolved, mut seeded) = (0usize, 0usize);
    for node in nodes {
        match (node.token.as_deref(), creds.get(&node.id)) {
            (None, Ok(Some(token))) => {
                node.token = Some(token);
                resolved += 1;
            }
            (Some(env_token), Ok(None)) => {
                // Seed an empty slot once — once stored, later boots
                // read Some and skip the write entirely.
                match creds.set(&node.id, env_token) {
                    Ok(()) => seeded += 1,
                    Err(e) => {
                        tracing::warn!(node = %node.id, "credential store write failed: {e}");
                    }
                }
            }
            // env token + stored credential: env wins at runtime, the
            // stored credential (a pair-minted token) stays untouched.
            (Some(_), Ok(Some(_))) | (None, Ok(None)) => {}
            // A failed read can't tell empty from populated — never
            // write blind: an env token must not clobber an unseen
            // stored credential.
            (_, Err(e)) => {
                tracing::warn!(node = %node.id, "credential store lookup failed: {e}");
            }
        }
    }
    (resolved, seeded)
}

/// The node an entity belongs to — projection lookup; entity ids are
/// unique per node but may collide across nodes (`kind` narrows).
fn owner<'a>(
    projection: &hypha_sync::ProjectionStore,
    id: &str,
    kind: Option<&str>,
    nodes: &'a HashMap<String, HubNode>,
) -> Result<(String, &'a HubNode), String> {
    // Routing policy lives in `hypha-control` — `kind` narrows when
    // ids collide; otherwise the first match wins.
    let rows: Vec<hypha_control::OwnershipRow> = projection
        .entities(None)
        .map_err(proj_err)?
        .into_iter()
        .map(|row| hypha_control::OwnershipRow {
            entity_id: row.entity_id,
            node_id: row.node_id,
            kind: row.kind,
        })
        .collect();
    let node_id = hypha_control::pick_owner(rows.iter(), id, kind)?.0;
    nodes
        .get(&node_id)
        .map(|n| (node_id.clone(), n))
        .ok_or_else(|| format!("node {node_id} not registered"))
}

/// `owner` + the entity row it picked — callers that need the row skip
/// a second lookup whose `None` arm could never fire.
fn owner_row<'a>(
    projection: &hypha_sync::ProjectionStore,
    id: &str,
    kind: Option<&str>,
    nodes: &'a HashMap<String, HubNode>,
) -> Result<(String, &'a HubNode, IndexedEntity), String> {
    let rows = projection.entities(None).map_err(proj_err)?;
    let mut found: Option<IndexedEntity> = None;
    for row in rows {
        if row.entity_id != id {
            continue;
        }
        if let Some(want) = kind {
            if row.kind == want {
                found = Some(row);
                break;
            }
        } else if found.is_none() {
            found = Some(row);
        }
    }
    let Some(row) = found else {
        return Err(format!("Entity not found: {id}"));
    };
    let node_id = row.node_id.clone();
    nodes
        .get(&node_id)
        .map(|n| (node_id.clone(), n, row))
        .ok_or_else(|| format!("node {node_id} not registered"))
}

/// The mutable node registry — `pair` rewrites a node's token + client
/// after minting a credential, so reads take the lock.
#[derive(Default)]
struct Registry {
    nodes: HashMap<String, HubNode>,
    clients: HashMap<String, Arc<NodeClient>>,
}

/// The actor every hub-brokered write runs as. One identity covers the
/// hub itself — foreign holders register separately (pid-keyed when
/// the node's lock probe names one) so arbitration can tell "our own
/// hold" from "another actor's".
const HUB_ACTOR: &str = "hub";

/// Lock-registry key — `(node, entity)`: entity ids are unique per
/// node but collide across nodes, so the node scopes every grant.
fn lock_key(node_id: &str, entity: &str) -> String {
    format!("{node_id}:{entity}")
}

fn hub_actor(node_id: &str) -> Actor {
    Actor {
        id: HUB_ACTOR.to_string(),
        node: NodeId(node_id.to_string()),
    }
}

/// `EntityApi` over the sync projection + per-node clients.
pub struct SyncEntityApi {
    engine: SyncHandle,
    registry: RwLock<Registry>,
    /// Who holds which entity — the hub is the lock authority; the
    /// projection's `locked` reports plus this hub's own op outcomes
    /// populate it.
    locks: RwLock<LockRegistry>,
    /// The takeover/contention policy rendered at write call sites.
    /// `hypha-control` decides; the node executes.
    arbitration: Arbitration,
    /// Where `pair` persists minted tokens (keyring or file — picked at
    /// boot by [`crate::credentials::open`]).
    creds: Arc<dyn NodeCredentials>,
    /// First configured node — the canonical target for registry
    /// writes (config/push) that have no entity to route by.
    primary: Option<String>,
    /// The entity-kind policy registry — op gating, create
    /// validation, outbox classification, and external-actor
    /// extraction all resolve through it.
    policies: KindPolicies,
}

impl SyncEntityApi {
    pub fn new(engine: SyncHandle, nodes: Vec<HubNode>, creds: Arc<dyn NodeCredentials>) -> Self {
        let clients = nodes
            .iter()
            .filter_map(|n| {
                NodeClient::new(&n.url, n.token.clone())
                    .ok()
                    .map(|c| (n.id.clone(), Arc::new(c)))
            })
            .collect();
        Self {
            engine,
            primary: nodes.first().map(|n| n.id.clone()),
            registry: RwLock::new(Registry {
                nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
                clients,
            }),
            locks: RwLock::new(LockRegistry::default()),
            arbitration: Arbitration::default(),
            creds,
            policies: crate::kind_policies::default_kind_policies(),
        }
    }

    /// Override the kind-policy registry — tests and hosts fronting
    /// different integrations.
    #[must_use]
    pub fn with_policies(mut self, policies: KindPolicies) -> Self {
        self.policies = policies;
        self
    }

    /// Override the arbitration policy — tests and future hub config.
    #[must_use]
    pub fn with_arbitration(mut self, arbitration: Arbitration) -> Self {
        self.arbitration = arbitration;
        self
    }

    fn registry(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn registry_mut(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn locks_mut(&self) -> std::sync::RwLockWriteGuard<'_, LockRegistry> {
        self.locks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Fold the projection's lock report for `row` into the registry:
    /// a reported `locked` registers an external holder the hub never
    /// granted (or refreshes a known one), an unlocked report drops
    /// any recorded hold. The book only ever trails the node's probe.
    fn reconcile_locks(
        locks: &mut LockRegistry,
        key: &str,
        node_id: &str,
        row: &IndexedEntity,
        policies: &KindPolicies,
    ) {
        if row.locked {
            if let Some(held) = locks.holder(key) {
                let holder = held.holder.id.clone();
                locks.refresh(key, &holder);
            } else {
                // The kind's policy names the foreign holder when the
                // index doc reports one; unreported reads as
                // `"external"`.
                let actor = policies
                    .for_entity_kind(&row.kind)
                    .and_then(|p| p.external_actor_id(row))
                    .unwrap_or_else(|| "external".to_string());
                locks.grant(
                    key,
                    Actor {
                        id: actor,
                        node: NodeId(node_id.to_string()),
                    },
                );
            }
        } else if let Some(holder) = locks.holder(key).map(|h| h.holder.id.clone()) {
            locks.release(key, &holder);
        }
    }

    /// Reconcile the lock book against `row` and ask the arbitration
    /// policy what the hub may do with `entity` — the single decision
    /// point every gated write consults. Returns the registry key
    /// alongside so callers can record the outcome.
    fn decide(
        &self,
        node_id: &str,
        entity: &str,
        row: &IndexedEntity,
        takeover: bool,
    ) -> (String, Decision) {
        let key = lock_key(node_id, entity);
        let mut locks = self.locks_mut();
        Self::reconcile_locks(&mut locks, &key, node_id, row, &self.policies);
        let decision = self
            .arbitration
            .attach(&locks, &key, &hub_actor(node_id), takeover);
        (key, decision)
    }

    /// The arbitration gate for entity writes — only `Allow` lets a
    /// gated op through; a foreign hold rejects with the holder named
    /// rather than queueing an op the node would refuse on replay.
    fn ensure_writable(
        &self,
        node_id: &str,
        entity: &str,
        row: &IndexedEntity,
    ) -> Result<(), String> {
        let (_key, decision) = self.decide(node_id, entity, row, false);
        match decision {
            Decision::Allow => Ok(()),
            Decision::AttachReadOnly { holder }
            | Decision::Preempt {
                stale_holder: holder,
            } => Err(format!(
                "entity {entity} is held by {holder} on {node_id} — attach with takeover to write"
            )),
            Decision::Queue { node } => Err(format!(
                "entity {entity} is held on {} — attach with takeover to write",
                node.0
            )),
        }
    }

    fn projection(&self) -> hypha_sync::ProjectionStore {
        self.engine.projection()
    }

    fn client(&self, node_id: &str) -> Result<Arc<NodeClient>, String> {
        self.registry()
            .clients
            .get(node_id)
            .cloned()
            .ok_or_else(|| format!("node {node_id} not registered"))
    }

    /// The client for `node` when given, else the primary's.
    fn scoped_client(&self, node: Option<&str>) -> Result<(String, Arc<NodeClient>), String> {
        let id = match node {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => self
                .primary
                .clone()
                .ok_or_else(|| "no nodes registered".to_string())?,
        };
        let client = self.client(&id)?;
        Ok((id, client))
    }

    /// The upstream `(url, token)` for an entity — the proxy uses it
    /// to route entity-scoped requests to the owning node.
    pub fn node_url_for(&self, id: &str, kind: Option<&str>) -> Option<(String, Option<String>)> {
        let registry = self.registry();
        let (_node_id, node) = owner(&self.projection(), id, kind, &registry.nodes).ok()?;
        Some((node.url.clone(), node.token.clone()))
    }

    /// `(url, token, upstream path)` for the entity's live event
    /// stream — the kind policy's `stream_path` (id already
    /// percent-encoded) plus its `call_params` query, resolved against
    /// the owning node. `None` when the entity isn't projected, its
    /// kind is unregistered, or the kind has no stream — the hub's
    /// `/api/entities/{id}/stream` route answers 404 on `None`.
    pub fn stream_upstream(&self, id: &str) -> Option<(String, Option<String>, String)> {
        let registry = self.registry();
        let row = self
            .projection()
            .entities(None)
            .ok()?
            .into_iter()
            .find(|r| r.entity_id == id)?;
        let policy = self.policies.for_entity_kind(&row.kind)?;
        let mut path = policy.stream_path(&hypha_sync::client::encode_segment(id))?;
        let params = policy.call_params(Some(&row.kind));
        if !params.is_empty() {
            let query = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params)
                .finish();
            path.push('?');
            path.push_str(&query);
        }
        let (_node_id, node) =
            owner(&self.projection(), id, Some(&row.kind), &registry.nodes).ok()?;
        Some((node.url.clone(), node.token.clone(), path))
    }

    fn resolve(&self, id: &str, kind: Option<&str>) -> Result<(String, Arc<NodeClient>), String> {
        let node_id = {
            let registry = self.registry();
            owner(&self.projection(), id, kind, &registry.nodes)?.0
        };
        Ok((node_id.clone(), self.client(&node_id)?))
    }

    /// `/api/entities/{id}[/{suffix}]`.
    fn entity_path(id: &str, suffix: &str) -> String {
        let mut path = format!("/api/entities/{}", hypha_sync::client::encode_segment(id));
        if !suffix.is_empty() {
            path.push('/');
            path.push_str(suffix);
        }
        path
    }

    /// An entity op through the engine — posts direct when the node is
    /// up, enqueues (per-entity FIFO) when down. Gated ops consult the
    /// lock book first.
    async fn submit(
        &self,
        id: &str,
        kind: Option<&str>,
        op: &str,
        policy: OpKind,
        payload: Value,
    ) -> Result<(), String> {
        let (node_id, row) = {
            let registry = self.registry();
            let (node_id, _node, row) = owner_row(&self.projection(), id, kind, &registry.nodes)?;
            (node_id, row)
        };
        // `entity.delete` is substrate-gated; every other gated op is
        // the owning kind policy's call.
        if op == "entity.delete" || self.policies.for_op(op).is_some_and(|p| p.op_gated(op)) {
            self.ensure_writable(&node_id, id, &row)?;
        }
        self.engine
            .submit_op(NodeWrite {
                node_id: node_id.clone(),
                entity_id: id.to_string(),
                entity_kind: (!row.kind.is_empty()).then(|| row.kind.clone()),
                op: op.to_string(),
                policy,
                payload,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                ttl_seconds: None,
            })
            .await
            .map_err(err_string)?;
        if op == "entity.delete" {
            // The entity is gone (or queued to be) — drop any recorded
            // hold so a re-created id starts clean.
            let key = lock_key(&node_id, id);
            let mut locks = self.locks_mut();
            if let Some(holder) = locks.holder(&key).map(|h| h.holder.id.clone()) {
                locks.release(&key, &holder);
            }
        }
        Ok(())
    }

    /// Persist a minted credential and start using it: store → registry
    /// (token + rebuilt client) → engine retarget (re-add restarts the
    /// node's sync loop under the new credential). Failures degrade to
    /// in-memory use until restart — the token is still returned to
    /// the caller, so nothing minted is lost.
    async fn adopt_credential(&self, node_id: &str, token: &str) {
        // Blocking store I/O (a dbus round-trip on the keyring backend)
        // stays off the executor.
        let creds = Arc::clone(&self.creds);
        let id = node_id.to_string();
        let tok = token.to_string();
        cred_store_warn(
            tokio::task::spawn_blocking(move || creds.set(&id, &tok)).await,
            node_id,
        );
        let target = {
            let mut registry = self.registry_mut();
            let (node_ref, token_opt) = {
                let Some(entry) = registry.nodes.get_mut(node_id) else {
                    return;
                };
                entry.token = Some(token.to_string());
                (
                    NodeRef {
                        id: entry.id.clone(),
                        url: entry.url.clone(),
                        label: entry.id.clone(),
                    },
                    entry.token.clone(),
                )
            };
            match NodeClient::new(&node_ref.url, token_opt) {
                Ok(client) => {
                    registry
                        .clients
                        .insert(node_id.to_string(), Arc::new(client));
                }
                // The rebuild can't fail for a URL that parsed at boot.
                Err(e) => {
                    tracing::warn!(node = %node_id, "rebuild node client failed: {e}");
                }
            }
            NodeTarget::with_token(node_ref, token)
        };
        retarget_warn(self.engine.add_node(target).await, node_id);
    }

    /// `NodeError` → user-facing text: the node's `{error}` payload
    /// when the body is JSON, else the raw status line.
    fn err_message(e: hypha_sync::client::NodeError) -> String {
        match e {
            hypha_sync::client::NodeError::Http { status, body } => {
                serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| format!("http {status}"))
            }
            other => other.to_string(),
        }
    }
}

/// `IndexedEntity` → the DTO — `index_doc` comes out of the raw doc's
/// `indexDoc` slot when the wire wrapped it, else the row itself.
/// `node_status` is the owning node's projection reachability
/// (`"up"`/`"down"`/`"unknown"`) so merged lists can mark a dead
/// node's stale entries; `None` leaves the field empty.
fn entity_dto(row: &IndexedEntity, node_status: Option<&str>) -> EntityDto {
    EntityDto {
        id: row.entity_id.clone(),
        kind: row.kind.clone(),
        node: Some(row.node_id.clone()),
        node_status: node_status.map(str::to_string),
        index_doc: row
            .raw
            .get("indexDoc")
            .cloned()
            .unwrap_or_else(|| row.raw.clone()),
        locked: row.locked,
        live: row.live,
        updated_at: row.updated_at.clone(),
    }
}

/// `OutboxEntry` → the wire row — pending and in-flight entries read
/// as `queued`; dead-letters surface as `failed` with their last error.
fn pending_dto(e: &OutboxEntry) -> PendingOpDto {
    PendingOpDto {
        id: e.id.clone(),
        node_id: e.node_id.clone(),
        entity_id: e.entity_id.clone(),
        kind: e.op.clone(),
        status: if e.status == OutboxStatus::Dead {
            "failed"
        } else {
            "queued"
        }
        .to_string(),
        enqueued_at: e.created_at.clone(),
        attempts: e.attempts as u32,
        last_error: e.last_error.clone(),
    }
}

#[async_trait::async_trait]
impl EntityApi for SyncEntityApi {
    async fn list_entities(&self, kind: Option<&str>) -> Result<Vec<EntityDto>, String> {
        let projection = self.projection();
        let rows: Vec<IndexedEntity> = projection.entities(None).map_err(proj_err)?;
        // One nodes() read joins each row's owning-node reachability —
        // a dead runner's stale entries read `"down"` inline.
        let statuses: HashMap<String, String> = projection
            .nodes()
            .map_err(proj_err)?
            .into_iter()
            .map(|n| (n.id, n.status.as_str().to_string()))
            .collect();
        Ok(rows
            .iter()
            .filter(|row| kind.is_none_or(|k| row.kind == k))
            .map(|row| entity_dto(row, statuses.get(&row.node_id).map(String::as_str)))
            .collect())
    }

    async fn entity(&self, id: &str) -> Result<EntityDto, String> {
        let registry = self.registry();
        let (node_id, _node, row) = owner_row(&self.projection(), id, None, &registry.nodes)?;
        let status = self
            .projection()
            .node(&node_id)
            .ok()
            .flatten()
            .map(|n| n.status.as_str().to_string());
        Ok(entity_dto(&row, status.as_deref()))
    }

    async fn create_entity(
        &self,
        kind: &str,
        init: &Value,
        node: Option<&str>,
    ) -> Result<CreateResultDto, String> {
        // The kind's registered policy decides whether a create is
        // served — aliases (`"session"`) resolve to the canonical
        // kind, and the policy's `validate_create` hook vets the init
        // doc before anything posts.
        let Some((policy, _canonical)) = self.policies.for_create(kind) else {
            return Err(format!("entity kind {kind:?} not registered"));
        };
        policy.validate_create(init)?;
        let (_node_id, client) = self.scoped_client(node)?;
        let body = serde_json::json!({"kind": kind, "indexDoc": init});
        let res =
            tokio::task::spawn_blocking(move || client.send_json("POST", "/api/entities", &body))
                .await
                .map_err(join_err)?
                .map_err(Self::err_message)?;
        let id = res
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "create returned no entity id".to_string())?;
        Ok(CreateResultDto {
            id: id.to_string(),
            entity: None,
        })
    }

    async fn submit_op(&self, id: &str, op: &EntityOp) -> Result<(), String> {
        // Turn-shaped ops dead-letter on replay failure; the owning
        // kind policy classifies (meta-shaped ones retry —
        // last-writer-wins is safe for overlay state). Unregistered
        // ops are turns.
        let policy = self
            .policies
            .for_op(&op.kind)
            .map_or(OpKind::Turn, |p| p.op_kind(&op.kind));
        self.submit(id, None, &op.kind, policy, op.payload.clone())
            .await
    }

    async fn delete_entity(&self, id: &str) -> Result<(), String> {
        self.submit(
            id,
            None,
            "entity.delete",
            OpKind::Turn,
            serde_json::json!({}),
        )
        .await
    }

    async fn snapshot_export(&self, id: &str) -> Result<Value, String> {
        let (_node_id, client) = self.resolve(id, None)?;
        let path = Self::entity_path(id, "snapshot");
        tokio::task::spawn_blocking(move || client.get_json(&path))
            .await
            .map_err(join_err)?
            .map_err(Self::err_message)
    }

    async fn snapshot_import(&self, body: &Value, node: Option<&str>) -> Result<EntityDto, String> {
        let (node_id, client) = self.scoped_client(node)?;
        let body = body.clone();
        let res = tokio::task::spawn_blocking(move || {
            client.send_json("POST", "/api/entities/snapshot", &body)
        })
        .await
        .map_err(join_err)?
        .map_err(Self::err_message)?;
        let id = res
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "import returned no entity id".to_string())?;
        Ok(EntityDto {
            id: id.to_string(),
            kind: res.get("kind").and_then(Value::as_str).map_or_else(
                || {
                    self.policies
                        .default_entity_kind()
                        .unwrap_or("entity")
                        .to_string()
                },
                str::to_string,
            ),
            node: Some(node_id),
            index_doc: res,
            ..EntityDto::default()
        })
    }

    async fn entity_index(&self) -> Result<Vec<EntityIndexDto>, String> {
        let projection = self.projection();
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for node in projection.nodes().map_err(proj_err)? {
                let status = node.status.as_str().to_string();
                let entities = projection
                    .entities(Some(&node.id))
                    .map_err(proj_err)?
                    .iter()
                    .map(|row| entity_dto(row, Some(&status)))
                    .collect();
                out.push(EntityIndexDto {
                    node: node.id,
                    entities,
                });
            }
            Ok(out)
        })
        .await
        .map_err(join_err)?
    }

    async fn pending_ops(&self) -> Result<Vec<PendingOpDto>, String> {
        // Pending entries are addressable per node; dead-letters are a
        // single list. Entries queued for a since-removed node have no
        // reader — the outbox has no scan-all for `pending`.
        let outbox = self.engine.outbox();
        let mut node_ids: Vec<String> = self.registry().nodes.keys().cloned().collect();
        node_ids.sort_unstable();
        tokio::task::spawn_blocking(move || -> Result<Vec<PendingOpDto>, String> {
            let mut out = Vec::new();
            for id in &node_ids {
                for entry in &outbox.pending_for_node(id).map_err(err_string)? {
                    out.push(pending_dto(entry));
                }
            }
            for entry in &outbox.dead_letters().map_err(err_string)? {
                out.push(pending_dto(entry));
            }
            Ok(out)
        })
        .await
        .map_err(join_err)?
    }

    async fn node_info(&self) -> Result<NodeInfoDto, String> {
        let (_, client) = self.scoped_client(None)?;
        tokio::task::spawn_blocking(move || client.get_json("/api/node"))
            .await
            .map_err(join_err)?
            .map_err(err_string)
            .and_then(|v| serde_json::from_value(v).map_err(decode_err))
    }

    async fn rename_node(&self, name: &str, node: Option<&str>) -> Result<NodeInfoDto, String> {
        // Direct post — a rename on a down node can't queue meaningfully
        // (the identity it edits is now, not later), same as pair.
        let (_, client) = self.scoped_client(node)?;
        let body = serde_json::json!({ "name": name });
        tokio::task::spawn_blocking(move || client.send_json("PATCH", "/api/node", &body))
            .await
            .map_err(join_err)?
            .map_err(err_string)
            .and_then(|v| serde_json::from_value(v).map_err(decode_err))
    }

    async fn node_status(&self) -> Result<Vec<NodeStatusDto>, String> {
        let projection = self.projection();
        let registry = self.registry().nodes.clone();
        tokio::task::spawn_blocking(move || {
            let mut rows: Vec<NodeStatusDto> = projection
                .nodes()
                .map_err(proj_err)?
                .into_iter()
                .map(|n| NodeStatusDto {
                    id: n.id,
                    url: n.url,
                    label: n.label,
                    status: n.status.as_str().to_string(),
                    last_seen_at: n.last_seen_at,
                    down_since: n.down_since,
                    ephemeral: n.ephemeral,
                })
                .collect();
            // Registered-but-never-synced nodes still get a row.
            for (id, node) in &registry {
                if !rows.iter().any(|r| &r.id == id) {
                    rows.push(NodeStatusDto {
                        id: id.clone(),
                        url: node.url.clone(),
                        label: id.clone(),
                        status: "unknown".into(),
                        last_seen_at: None,
                        down_since: None,
                        ephemeral: false,
                    });
                }
            }
            rows.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(rows)
        })
        .await
        .map_err(join_err)?
    }

    async fn pair(&self, code: &str, node: Option<&str>) -> Result<String, String> {
        // Pairing redeems a code on one specific node — `node` picks
        // it, else the primary. Goes direct: the code is single-use
        // and queueing it in the outbox makes no sense.
        let (node_id, client) = self.scoped_client(node)?;
        let body = serde_json::json!({ "code": code });
        let res = tokio::task::spawn_blocking(move || client.send_json("POST", "/api/pair", &body))
            .await
            .map_err(join_err)?
            .map_err(|e| match e {
                // Surface the node's `{error}` payload, not the raw body.
                hypha_sync::client::NodeError::Http { body, .. } => {
                    serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| format!("pairing failed: {body}"))
                }
                e => e.to_string(),
            })?;
        let token = res
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "node returned no token".to_string())?
            .to_string();
        self.adopt_credential(&node_id, &token).await;
        Ok(token)
    }

    // Substrate gap — `hypha-http` serves no `/api/config`; this only
    // answers on integration nodes that mount their own config
    // surface (see the `EntityApi::get_config` doc note).
    async fn get_config(&self) -> Result<BTreeMap<String, Value>, String> {
        let (_, client) = self.scoped_client(None)?;
        tokio::task::spawn_blocking(move || {
            let body = client.get_json("/api/config").map_err(err_string)?;
            let map = body
                .get("config")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            Ok(map.into_iter().collect())
        })
        .await
        .map_err(join_err)?
    }

    // Same gap as `get_config` — no `/api/config/{key}` on the
    // substrate node.
    async fn set_config(&self, key: &str, value: &Value) -> Result<(), String> {
        let (_, client) = self.scoped_client(None)?;
        let path = format!("/api/config/{}", hypha_sync::client::encode_segment(key));
        let body = serde_json::json!({ "value": value });
        tokio::task::spawn_blocking(move || client.send_json("PATCH", &path, &body))
            .await
            .map_err(join_err)?
            .map(|_| ())
            .map_err(err_string)
    }

    async fn fs_dirs(&self, path: &str, node: Option<String>) -> Result<Vec<String>, String> {
        // Read-only and node-local — goes straight to the named node
        // (or the primary) rather than the projection or outbox.
        let (_, client) = self.scoped_client(node.as_deref())?;
        let api_path = format!("/api/fs?path={}", hypha_sync::client::encode_segment(path));
        tokio::task::spawn_blocking(move || client.get_json(&api_path))
            .await
            .map_err(join_err)?
            .map_err(err_string)
            .map(|v| {
                v.get("dirs")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|r| r.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            })
    }

    async fn push_vapid(&self) -> Result<String, String> {
        let (_, client) = self.scoped_client(None)?;
        tokio::task::spawn_blocking(move || client.get_json("/api/push/vapid"))
            .await
            .map_err(join_err)?
            .map_err(err_string)
            .and_then(|v| {
                v.get("publicKey")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "vapid: missing publicKey".to_string())
            })
    }

    async fn push_subscribe(&self, subscription: &PushSubscriptionDto) -> Result<(), String> {
        let (_, client) = self.scoped_client(None)?;
        let body = serde_json::to_value(subscription).map_err(err_string)?;
        tokio::task::spawn_blocking(move || client.send_json("POST", "/api/push/subscribe", &body))
            .await
            .map_err(join_err)?
            .map(|_| ())
            .map_err(err_string)
    }

    async fn push_unsubscribe(&self, endpoint: &str) -> Result<(), String> {
        let (_, client) = self.scoped_client(None)?;
        let body = serde_json::json!({ "endpoint": endpoint });
        tokio::task::spawn_blocking(move || {
            client.send_json("DELETE", "/api/push/subscribe", &body)
        })
        .await
        .map_err(join_err)?
        .map(|_| ())
        .map_err(err_string)
    }

    fn default_entity_kind(&self) -> Option<&'static str> {
        self.policies.default_entity_kind()
    }
}

/// `e.to_string()` as a fn item — one body shared by every `map_err`
/// tail (the closure per site would need an error per site to score).
fn err_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `format!("decode: {e}")` — see [`err_string`].
fn decode_err(e: impl std::fmt::Display) -> String {
    format!("decode: {e}")
}

/// `format!("projection: {e}")` — see [`err_string`].
fn proj_err(e: impl std::fmt::Display) -> String {
    format!("projection: {e}")
}

/// The `JoinError` arm of `spawn_blocking(…).await` — a panic inside
/// a blocking task is unreachable in practice; a named body tests can
/// call instead of racing a panic.
#[allow(clippy::needless_pass_by_value)]
fn join_err(e: tokio::task::JoinError) -> String {
    e.to_string()
}

/// `engine.add_node` outcome → warn log — split out like
/// `cred_store_warn` so the Shutdown arm is directly testable (an
/// `add_node` failure needs a dead engine, awkward to stage inside a
/// live api).
fn retarget_warn(r: Result<(), SyncError>, node_id: &str) {
    if let Err(e) = r {
        tracing::warn!(node = %node_id, "engine retarget failed: {e}");
    }
}

/// The `adopt_credential` store-write outcome → warn log — split out so
/// the JoinError arm (a panic inside the blocking task) is testable.
fn cred_store_warn(r: Result<Result<(), String>, tokio::task::JoinError>, node_id: &str) {
    let msg = match r {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(e) => Some(e.to_string()),
    };
    if let Some(e) = msg {
        tracing::warn!(node = %node_id, "credential store write failed: {e}");
    }
}

#[cfg(test)]
mod cov_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn shared_error_fns() {
        let e = std::io::Error::other("x");
        assert_eq!(err_string(&e), "x");
        assert_eq!(decode_err(&e), "decode: x");
        assert_eq!(proj_err(&e), "projection: x");
        // A real JoinError via abort.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let h = tokio::spawn(async { std::future::pending::<()>().await });
            h.abort();
            let err = h.await.unwrap_err();
            assert!(join_err(err).contains("cancelled"));
        });
    }

    #[tokio::test]
    async fn cred_store_warn_arms() {
        cred_store_warn(Ok(Ok(())), "n1");
        cred_store_warn(Ok(Err("x".into())), "n1");
        let h = tokio::spawn(async { std::future::pending::<Result<(), String>>().await });
        h.abort();
        cred_store_warn(h.await, "n1");
    }

    #[tokio::test]
    async fn adopt_credential_arms() {
        // `node_id` missing from the registry → the early return.
        let (api, _p, _o) = api_for(&[("n1", "http://127.0.0.1:1")]);
        api.adopt_credential("ghost", "tok").await;
        // Re-adopting on an already-registered node → `add_node`
        // retargets cleanly (no warn).
        let (api, _p, _o) = api_for(&[("n1", "http://127.0.0.1:1")]);
        api.adopt_credential("n1", "tok").await;
        // A URL `NodeClient::new` rejects → the rebuild warn arm —
        // the node entered the registry with no client.
        let (api, _p, _o) = api_for(&[("n1", "not a url")]);
        api.adopt_credential("n1", "tok").await;
        // A dead engine → `add_node` Shutdown → the warn arm.
        retarget_warn(Err(SyncError::Shutdown), "n1");
    }

    /// A stand-in integration policy — exercises the registry machinery
    /// without naming a real kind's rules (those live with the
    /// integration in P4).
    struct TestPolicy;

    impl hypha_sync::KindPolicy for TestPolicy {
        fn entity_kinds(&self) -> &'static [&'static str] {
            &["test-entity"]
        }
        fn owns_op(&self, op: &str) -> bool {
            op.starts_with("test.")
        }
        fn op_gated(&self, op: &str) -> bool {
            op == "test.write"
        }
        fn create_kind(&self, requested: &str) -> Option<&'static str> {
            match requested {
                "test-entity" | "test" => Some("test-entity"),
                _ => None,
            }
        }
    }

    fn test_policies() -> hypha_sync::KindPolicies {
        let mut policies = hypha_sync::KindPolicies::new();
        policies.register(std::sync::Arc::new(TestPolicy));
        policies
    }

    #[test]
    fn gated_ops_resolve_through_the_kind_policies() {
        // The bare registry gates only the substrate op.
        let policies = crate::kind_policies::default_kind_policies();
        let gated = |policies: &hypha_sync::KindPolicies, op: &str| {
            op == "entity.delete" || policies.for_op(op).is_some_and(|p| p.op_gated(op))
        };
        assert!(gated(&policies, "entity.delete"));
        assert!(!gated(&policies, "test.write"));
        assert!(!gated(&policies, "bogus.op"));
        // A registered policy gates its own ops; `entity.delete` stays
        // substrate-gated regardless.
        let policies = test_policies();
        assert!(gated(&policies, "test.write"));
        assert!(!gated(&policies, "test.read"));
        assert!(gated(&policies, "entity.delete"));
    }

    #[test]
    fn creates_resolve_through_kind_policies() {
        // Empty registry — no kind is creatable.
        let policies = crate::kind_policies::default_kind_policies();
        assert!(policies.for_create("test-entity").is_none());
        assert!(policies.default_entity_kind().is_none());
        // Registered — the canonical kind and the alias both resolve.
        let policies = test_policies();
        assert_eq!(
            policies.for_create("test-entity").map(|(_, k)| k),
            Some("test-entity")
        );
        assert_eq!(
            policies.for_create("test").map(|(_, k)| k),
            Some("test-entity")
        );
        assert!(policies.for_create("k8s-pod").is_none());
        assert_eq!(policies.default_entity_kind(), Some("test-entity"));
    }

    fn api_for(
        nodes: &[(&str, &str)],
    ) -> (
        SyncEntityApi,
        hypha_sync::ProjectionStore,
        hypha_outbox::Outbox,
    ) {
        let projection = hypha_sync::ProjectionStore::in_memory().unwrap();
        let outbox = hypha_outbox::Outbox::in_memory().unwrap();
        let nodes: Vec<HubNode> = nodes
            .iter()
            .map(|(id, url)| HubNode {
                id: (*id).to_string(),
                url: (*url).to_string(),
                token: None,
            })
            .collect();
        let engine = hypha_sync::SyncEngine::spawn(
            projection.clone(),
            outbox.clone(),
            nodes.iter().map(|n| {
                NodeTarget::new(hypha_sync::NodeRef {
                    id: n.id.clone(),
                    url: n.url.clone(),
                    label: n.id.clone(),
                })
            }),
            hypha_sync::engine::SyncOptions::default(),
        );
        let dir = tempfile::tempdir().unwrap();
        let creds = Arc::new(crate::credentials::FileCredentials::new(
            dir.keep().join("credentials.json"),
        ));
        (SyncEntityApi::new(engine, nodes, creds), projection, outbox)
    }
}
