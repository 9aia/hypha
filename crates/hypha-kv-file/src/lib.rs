//! hypha-kv-file — the JSON-file [`KvStore`] impl, porting
//! `sepia_meta::MetaStore`'s persistence mechanics: one document,
//! rewritten atomically (tmp + rename) after every mutation, and a
//! corrupt or half-written file degrades to empty rather than
//! failing.
//!
//! On-disk shape:
//!
//! ```json
//! {
//!   "namespaces": {
//!     "config": { "visibility": "public", "entries": { "theme": "dark" } },
//!     "push":   { "visibility": "internal", "entries": { "vapid": {} } }
//!   }
//! }
//! ```
//!
//! Reads are tolerant in both directions: a top-level object without
//! a `namespaces` key is read as the flat `{ns: {key: value}}` shape
//! (every top-level object value becomes a public namespace's
//! entries), which is the shape a migrated `meta.json` `config`/
//! `projects` section already has. Writes always emit the canonical
//! `namespaces` form.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use hypha_kv::{KvStore, NamespaceInfo, Visibility};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn lock_inner(mutex: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One namespace on disk: its wire visibility plus the entry map.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Namespace {
    #[serde(default)]
    visibility: Visibility,
    #[serde(default)]
    entries: BTreeMap<String, Value>,
}

/// The canonical document — `namespaces` keyed by namespace name.
#[derive(Default, Serialize)]
struct KvFile {
    namespaces: BTreeMap<String, Namespace>,
}

struct Inner {
    path: PathBuf,
    namespaces: BTreeMap<String, Namespace>,
}

/// A [`KvStore`] over one JSON file. `Clone` shares the same
/// in-memory map and path.
#[derive(Clone)]
pub struct JsonKvStore {
    inner: Arc<Mutex<Inner>>,
}

impl JsonKvStore {
    /// Open the store at `path` (created on first write). A missing,
    /// unreadable, or corrupt file degrades to empty.
    #[must_use]
    pub fn open(path: &Path) -> Self {
        let namespaces = read_namespaces(path);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                path: path.to_path_buf(),
                namespaces,
            })),
        }
    }

    /// The file this store flushes to.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .path
            .clone()
    }

    fn flush(inner: &Inner) {
        if let Some(dir) = inner.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = inner
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let doc = KvFile {
            namespaces: inner.namespaces.clone(),
        };
        let json = serde_json::to_vec(&doc).unwrap_or_default();
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &inner.path);
        }
    }

    /// Mutable access to one namespace's entries, creating it public
    /// when undeclared — callers flush after mutating.
    fn entries_mut<'a>(
        namespaces: &'a mut BTreeMap<String, Namespace>,
        ns: &str,
    ) -> &'a mut BTreeMap<String, Value> {
        &mut namespaces.entry(ns.to_string()).or_default().entries
    }
}

/// Parse the file into the namespace map — canonical `namespaces`
/// shape first, flat `{ns: {entries}}` as the tolerant fallback.
fn read_namespaces(path: &Path) -> BTreeMap<String, Namespace> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(record)) = serde_json::from_str::<Value>(&raw) else {
        return BTreeMap::new();
    };
    if let Some(Value::Object(ns_map)) = record.get("namespaces") {
        return ns_map
            .iter()
            .filter_map(|(name, value)| {
                let ns: Namespace = serde_json::from_value(value.clone()).ok()?;
                Some((name.clone(), ns))
            })
            .collect();
    }
    // Flat shape: every top-level object value is one namespace's
    // entry map (public by default).
    record
        .iter()
        .filter_map(|(name, value)| {
            value.as_object().map(|entries| {
                (
                    name.clone(),
                    Namespace {
                        visibility: Visibility::Public,
                        entries: entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    },
                )
            })
        })
        .collect()
}

impl KvStore for JsonKvStore {
    fn get(&self, ns: &str, key: &str) -> Option<Value> {
        lock_inner(&self.inner)
            .namespaces
            .get(ns)
            .and_then(|n| n.entries.get(key))
            .cloned()
    }

    fn set(&self, ns: &str, key: &str, value: Value) {
        let mut inner = lock_inner(&self.inner);
        Self::entries_mut(&mut inner.namespaces, ns).insert(key.to_string(), value);
        Self::flush(&inner);
    }

    fn delete(&self, ns: &str, key: &str) -> bool {
        let mut inner = lock_inner(&self.inner);
        let removed = inner
            .namespaces
            .get_mut(ns)
            .is_some_and(|n| n.entries.remove(key).is_some());
        if removed {
            Self::flush(&inner);
        }
        removed
    }

    fn list(&self, ns: &str) -> BTreeMap<String, Value> {
        lock_inner(&self.inner)
            .namespaces
            .get(ns)
            .map(|n| n.entries.clone())
            .unwrap_or_default()
    }

    fn namespaces(&self) -> Vec<NamespaceInfo> {
        lock_inner(&self.inner)
            .namespaces
            .iter()
            .map(|(name, ns)| NamespaceInfo {
                name: name.clone(),
                visibility: ns.visibility,
            })
            .collect()
    }

    fn visibility(&self, ns: &str) -> Visibility {
        lock_inner(&self.inner)
            .namespaces
            .get(ns)
            .map_or(Visibility::Public, |ns| ns.visibility)
    }

    fn declare(&self, ns: &str, visibility: Visibility) {
        let mut inner = lock_inner(&self.inner);
        inner
            .namespaces
            .entry(ns.to_string())
            .or_default()
            .visibility = visibility;
        Self::flush(&inner);
    }

    fn import(&self, ns: &str, entries: BTreeMap<String, Value>, visibility: Visibility) {
        let mut inner = lock_inner(&self.inner);
        let namespace = inner.namespaces.entry(ns.to_string()).or_default();
        namespace.visibility = visibility;
        namespace.entries.extend(entries);
        Self::flush(&inner);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::assert_is_empty)]
    use super::*;

    fn temp_store() -> (tempfile::TempDir, JsonKvStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonKvStore::open(&dir.path().join("kv.json"));
        (dir, store)
    }

    #[test]
    fn crud_roundtrip() {
        let (_dir, store) = temp_store();
        assert_eq!(store.get("config", "theme"), None);
        store.set("config", "theme", Value::String("dark".into()));
        store.set("config", "scale", Value::from(2));
        assert_eq!(
            store.get("config", "theme"),
            Some(Value::String("dark".into()))
        );
        assert_eq!(store.list("config").len(), 2);
        assert!(store.delete("config", "theme"));
        assert!(!store.delete("config", "theme"));
        assert_eq!(store.get("config", "theme"), None);
        assert_eq!(store.list("missing").len(), 0);
    }

    #[test]
    fn namespaces_are_independent() {
        let (_dir, store) = temp_store();
        store.set("config", "k", Value::from(1));
        store.set("project", "p1", serde_json::json!({ "name": "Web" }));
        assert_eq!(store.list("config").len(), 1);
        assert_eq!(store.list("project").len(), 1);
        let names: Vec<String> = store.namespaces().iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, vec!["config".to_string(), "project".to_string()]);
        assert!(
            store
                .namespaces()
                .iter()
                .all(|n| n.visibility == Visibility::Public)
        );
    }

    #[test]
    fn visibility_declared_and_preserved() {
        let (_dir, store) = temp_store();
        store.declare("push", Visibility::Internal);
        assert_eq!(store.visibility("push"), Visibility::Internal);
        assert_eq!(store.visibility("undeclared"), Visibility::Public);
        // Store API ignores visibility — internal entries still read.
        store.set("push", "vapid", serde_json::json!({ "k": "v" }));
        assert!(store.get("push", "vapid").is_some());
        // Deleting the last entry keeps the namespace + visibility.
        assert!(store.delete("push", "vapid"));
        assert_eq!(store.visibility("push"), Visibility::Internal);
        let info = store
            .namespaces()
            .into_iter()
            .find(|n| n.name == "push")
            .unwrap();
        assert_eq!(info.visibility, Visibility::Internal);
    }

    #[test]
    fn import_merges_entries_and_sets_visibility() {
        let (_dir, store) = temp_store();
        store.set("config", "keep", Value::from(true));
        let mut entries = BTreeMap::new();
        entries.insert("theme".to_string(), Value::String("dark".into()));
        store.import("config", entries, Visibility::Internal);
        assert_eq!(store.get("config", "keep"), Some(Value::from(true)));
        assert_eq!(store.get("config", "theme"), Some("dark".into()));
        assert_eq!(store.visibility("config"), Visibility::Internal);
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.json");
        {
            let store = JsonKvStore::open(&path);
            store.set("config", "theme", Value::String("dark".into()));
            store.declare("push", Visibility::Internal);
            store.set("push", "vapid", serde_json::json!({ "pub": "x" }));
        }
        let store = JsonKvStore::open(&path);
        assert_eq!(
            store.get("config", "theme"),
            Some(Value::String("dark".into()))
        );
        assert_eq!(store.visibility("push"), Visibility::Internal);
        assert!(store.get("push", "vapid").is_some());
    }

    #[test]
    fn corrupt_file_degrades_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = JsonKvStore::open(&path);
        assert!(store.namespaces().is_empty());
        // And the store still works — first write rewrites the file.
        store.set("config", "k", Value::Null);
        assert!(path.exists());
    }

    #[test]
    fn flat_shape_reads_as_public_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::write(
            &path,
            r#"{"config": {"theme": "dark"}, "projects": {"p1": {"name": "Web"}}}"#,
        )
        .unwrap();
        let store = JsonKvStore::open(&path);
        assert_eq!(
            store.get("config", "theme"),
            Some(Value::String("dark".into()))
        );
        assert_eq!(
            store.get("projects", "p1"),
            Some(serde_json::json!({ "name": "Web" }))
        );
        assert!(
            store
                .namespaces()
                .iter()
                .all(|n| n.visibility == Visibility::Public)
        );
    }

    #[test]
    fn address_helper() {
        assert_eq!(hypha_kv::address("config", "theme"), "config/theme");
    }
}
