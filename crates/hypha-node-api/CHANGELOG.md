# Changelog

## [0.2.0] - 2026-11-24

### Added

- **node-api:** submit_op_result — surface op response bodies (`f5f4e4e`)
- add hypha-hub gateway and hypha-node host with hypha/hypha-hub binaries (`e65c4f5`)
- release infrastructure — v0.1.0 publish metadata, dist + tag-triggered release.yml (`c4d492a`)
- **sync:** ephemeral-node awareness — lifecycle status + reap (`9e45a87`)
- persist node capability labels for fleet routing (`73074f6`)
- **hub:** durable entity creates via entity.create outbox ops (T-054) (`232e408`)

### Fixed

- **http,hub,node-api:** canonical wire paths — drop alias routes (`71cbaf8`)
- **node-api:** percent-encode entity ids in HttpEntityApi paths (`069ea20`)
