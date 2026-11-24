# Changelog

## [0.2.0] - 2026-11-24

### Added

- **node-api:** submit_op_result — surface op response bodies (`f5f4e4e`)
- add hypha-hub gateway and hypha-node host with hypha/hypha-hub binaries (`e65c4f5`)
- **hypha-hub:** HYPHA_EPHEMERAL_DEAD_AFTER env knob for the ephemeral reap window (`e2dc1b7`)
- release infrastructure — v0.1.0 publish metadata, dist + tag-triggered release.yml (`c4d492a`)
- **sync:** ephemeral-node awareness — lifecycle status + reap (`9e45a87`)
- **http,hub:** integration seams for the peregrin composition (P4b) (`884730f`)
- **hub:** durable entity creates via entity.create outbox ops (T-054) (`232e408`)

### Changed

- replace hypha-telemetry with lampy git dep (P-042/PL-023) (`d0b7021`)
- **hypha-hub:** drop dead entity_router — all /api/entities* paths are hub-owned (`0e6eddc`)

### Fixed

- **sync:** bound direct write delivery — fail fast into outbox on hung nodes (T-060) (`fb64f08`)
- project entity docs on create so ops route before next sync (`a4d90b6`)
- **http,hub,node-api:** canonical wire paths — drop alias routes (`71cbaf8`)
- satisfy clippy in hypha-hub (consume policies, let-else signal) (`121a747`)
