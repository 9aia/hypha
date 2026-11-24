# Changelog

## [0.2.0] - 2026-11-24

### Added

- **http:** composition seams — app_routes, health probe, pair prefix (`c659086`)
- release infrastructure — v0.1.0 publish metadata, dist + tag-triggered release.yml (`c4d492a`)
- **http,hub:** integration seams for the peregrin composition (P4b) (`884730f`)
- add hypha-http — substrate HTTP surface extracted from peregrin-http (`2a74c77`)
- **hub:** durable entity creates via entity.create outbox ops (T-054) (`232e408`)
- **kv:** generic node-local KV store + /api/kv* routes (P-006 phase 1) (`024c20f`)

### Changed

- replace hypha-telemetry with lampy git dep (P-042/PL-023) (`d0b7021`)

### Fixed

- make Pairing::with_token_prefix work on shared instances (`98694d1`)
- **http,hub,node-api:** canonical wire paths — drop alias routes (`71cbaf8`)
