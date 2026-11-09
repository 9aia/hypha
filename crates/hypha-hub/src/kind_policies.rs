//! The hub's entity-kind policy registry — the injection seam for
//! integrations.
//!
//! [`KindPolicies`] carries everything kind-specific the sync layer
//! needs (diff events, op gating, stream paths, create validation,
//! push notices). hypha itself ships **no** integration policies: a
//! bare `hypha-hub` gateways the substrate contract only — generic
//! entity reads, ungated ops, `entity.delete` — and every kind gets
//! the inert defaults. Integrations register their policies at
//! composition through [`crate::hub_state_with`] /
//! [`crate::hub_state_from_with`].

use hypha_sync::KindPolicies;

/// The registry a bare hub boots with — empty. `entity.*` substrate
/// behavior needs no policy: `entity.delete` is gated in
/// [`crate::sync_api::SyncEntityApi`] itself and unregistered kinds
/// get the generic fallbacks everywhere.
///
/// Integrations replace this with their own registry (e.g. a session
/// policy plus a campaign policy) — registration order decides the
/// default kind for kind-less creates.
pub fn default_kind_policies() -> KindPolicies {
    KindPolicies::new()
}
