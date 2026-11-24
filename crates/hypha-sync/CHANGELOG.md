# Changelog

## [0.2.0] - 2026-11-24

### Added

- add hypha-sync and hypha-push crates (`d96247e`)
- release infrastructure — v0.1.0 publish metadata, dist + tag-triggered release.yml (`c4d492a`)
- **sync:** ephemeral-node awareness — lifecycle status + reap (`9e45a87`)
- persist node capability labels for fleet routing (`73074f6`)

### Fixed

- **sync:** bound direct write delivery — fail fast into outbox on hung nodes (T-060) (`fb64f08`)
- **sync:** read entity envelope columns from the wire wrap, not indexDoc (T-061) (`6b3d4d1`)
