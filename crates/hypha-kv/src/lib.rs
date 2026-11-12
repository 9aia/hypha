//! hypha-kv — the node-local key/value store port (org proposal
//! P-006). Generic `{ns}/{key}` addressing over arbitrary JSON
//! values: the substrate half of what used to be sepia-meta's
//! `config`/`projects` sections and peregrin's `/api/config`,
//! `/api/projects`, `/api/client/keypair` routes.
//!
//! Values are opaque [`serde_json::Value`]s — the store never
//! interprets them. What a *namespace* means (`config`, `project`,
//! `ui`, …) is the integration's business; the port only carries the
//! `{ns}/{key}` shape plus a per-namespace [`Visibility`] the HTTP
//! surface enforces (internal namespaces never leave the node over
//! `/api/kv*`).
//!
//! Secrets policy (org convention): the KV stores secret *names* and
//! references, never secret values — anything sensitive belongs to
//! cuora. Visibility is a wire filter, not an access control: callers
//! holding the [`KvStore`] directly read internal namespaces freely.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether a namespace is exposed on the node's HTTP surface.
///
/// `Public` namespaces answer `/api/kv*` reads and accept writes;
/// `Internal` ones are store-API-only (the HTTP layer hides reads and
/// rejects writes) — the generic replacement for peregrin's
/// `INTERNAL_CONFIG` denylist (`vapid`, `pushSubscriptions`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Served over `/api/kv*`.
    #[default]
    Public,
    /// Never exposed over HTTP — node-local only.
    Internal,
}

/// One namespace as reported by [`KvStore::namespaces`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    pub name: String,
    pub visibility: Visibility,
}

/// What a write did — the payload integrations turn into feed events
/// (peregrin's `InstrumentedMeta` emitted `"project"` events on
/// project writes and nothing on config writes; the generic `"kv"`
/// event plus an optional hook covers both).
#[derive(Clone, Debug, PartialEq)]
pub enum KvOp {
    /// `set` — `value` is the stored JSON verbatim.
    Set { value: Value },
    /// `delete` — the key is gone (or was already absent).
    Delete,
}

/// The key/value store port — synchronous like the file-backed
/// reference impl (a mutex + atomic rewrite per mutation); callers on
/// async runtimes treat it like any other quick critical section.
///
/// Addressing is `{ns}/{key}`: `ns` and `key` are separate segments —
/// neither may contain `/` (the HTTP route shape enforces this; the
/// port itself doesn't police it). `ns` names a namespace
/// (`"config"`, `"project"`, …); undeclared namespaces behave as
/// [`Visibility::Public`] until [`KvStore::declare`] says otherwise.
pub trait KvStore: Send + Sync {
    /// The value at `{ns}/{key}`, or `None` when absent — including
    /// for internal namespaces (visibility is a wire concern, not a
    /// store one).
    fn get(&self, ns: &str, key: &str) -> Option<Value>;

    /// Upsert `{ns}/{key}` → `value`, creating the namespace as
    /// public when undeclared. Flushes to durable storage.
    fn set(&self, ns: &str, key: &str, value: Value);

    /// Remove `{ns}/{key}`; `true` when a value was actually removed.
    fn delete(&self, ns: &str, key: &str) -> bool;

    /// Every entry of `ns` (empty map when the namespace doesn't
    /// exist), keyed by `key`.
    fn list(&self, ns: &str) -> BTreeMap<String, Value>;

    /// Every namespace the store knows — declared or holding
    /// entries — with its visibility.
    fn namespaces(&self) -> Vec<NamespaceInfo>;

    /// The namespace's visibility (`Public` when undeclared).
    fn visibility(&self, ns: &str) -> Visibility;

    /// Declare (or re-declare) a namespace's visibility. Declaring an
    /// internal namespace with no entries still registers it, so a
    /// later `set` can't silently publish it as public.
    fn declare(&self, ns: &str, visibility: Visibility);

    /// Bulk-import `entries` into `ns` under `visibility` — the
    /// migration half (meta.json `projects`/`config` sections → KV).
    /// Existing keys are overwritten; other keys in the namespace
    /// survive.
    fn import(&self, ns: &str, entries: BTreeMap<String, Value>, visibility: Visibility);
}

/// The `{ns}/{key}` wire/storage address for one entry.
#[must_use]
pub fn address(ns: &str, key: &str) -> String {
    format!("{ns}/{key}")
}
