+++
id = "ADR-0001"
title = "Extraction from peregrin — entity plane becomes hypha"
status = "done"
created = "2026-11-09"
priority = "medium"
+++

## Context

Peregrin's entity plane (ADR-0091 in the peregrin ledger) grew a
generic substrate — entities `{id,kind,indexDoc}`, namespaced ops,
opaque snapshots, locks, projections, outbox, node host, hub gateway —
inside an orchestration repo. The plane was de-chat-shaped (it never
named a session type) and every consumer (sepia's `sepia-session`
adapter, campaign kinds, future integrations) needed it without
peregrin's campaign machinery. Org plan PL-005 split it into a
dedicated substrate repo, `hypha`, named for the fungal strand
connecting a mycelial network.

## Decision

Extract the entity plane into `hypha` as a ports-and-adapters Rust
workspace:

- **Crate map** — `hypha-node-api` (EntityApi contract + DTOs +
  HttpEntityApi/FakeEntityApi), `hypha-http` (substrate axum surface:
  `/api/entities*`, `/api/entity-index`, `/api/pending-ops`,
  `/api/events`, node/pairing/auth/push), `hypha-node` + `hypha` bin
  (`hypha serve`), `hypha-hub` + `hypha-hub` bin (merged index, sync
  engine wiring, SSE/passthrough bridges, credential store),
  `hypha-sync`/`hypha-outbox`/`hypha-control`/`hypha-push`, and the
  `hypha-secret`/`hypha-service` port families.
- **Dependency direction** — orchestration → hypha → ports. Hypha
  never names an integration type; integrations register
  `EntityPlane`s (`with_kind`) and `KindPolicies` at composition.
- **Kind-policy injection** — the hub boots with an empty
  `KindPolicies` registry (`default_kind_policies`); integrations
  supply theirs via `hub_state_with`. `entity.*` is the substrate's
  own reserved op namespace.
- **Composition seams** — `HyphaState::with_kind`/`with_entity_host`/
  `with_agent_lister`/`with_node_capabilities`/`with_health_probe`/
  `with_auth_cookie` let an integration wrap host state without
  forking the router; `app_routes` returns `Router<HyphaState>` so a
  composer merges routers under one listener and installs a single
  fallback.

## Consequences

- **Env renames** — `PEREGRIN_*` → `HYPHA_*` throughout
  (`HYPHA_HOST/PORT/TOKEN/HOME/NODE/NAME/ORIGINS/SSE_KEEPALIVE_MS/
  PAIR_LIMIT_*/PUSH*`, hub-side `HYPHA_HUB_*`/`HYPHA_NODE_*`/
  `HYPHA_NODES`/`HYPHA_CREDENTIAL_STORE`).
- **Keyring service attrs** — `hypha-keyring` stores under service
  `"hypha"` (was `"peregrin"`); hub node credentials under
  `"hypha-hub"` (was `"peregrin-hub"`). No automatic migration —
  re-pair or re-store after upgrade.
- **Deliberate breaks** — the wire aliases
  (`/api/entities/{id}/export`, `/api/entities/import`, `/api/index`,
  `/api/outbox`) are removed; `HttpEntityApi` speaks only canonical
  paths. `/api/config*` is not a substrate route (integration-owned);
  `EntityApi::get_config`/`set_config` are doc-hidden. Auth cookie
  defaults to `hypha_token` (integrations keep their historical name
  via `with_auth_cookie`).
- **P5 cutover pending** — peregrin's node/hub composition onto hypha
  crates is tracked in org plan PL-005; until then both stacks exist.
