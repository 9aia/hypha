# Security Policy

Report vulnerabilities privately to the maintainers via GitHub:
https://github.com/9aia/hypha/security/advisories/new

Do not open public issues for security reports.

## Scope

- Hypha moves entity snapshots and ops across nodes — snapshot bytes
  are opaque but transported verbatim; integrations own payload
  confidentiality.
- Node↔hub auth is token/pairing based; ops are namespaced and
  validated only for shape — authorization policy lives in
  `hypha-control` and the integration, not the wire.
- Secrets resolve via `hypha-secret` ports (env → OS keyring →
  dotenv file); values never appear in config or index docs.

We respond to reports as quickly as we can; expect acknowledgement
within a few days.
