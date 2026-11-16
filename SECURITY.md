# Security Policy

Report vulnerabilities privately by emailing
luisfuturist+9aia@gmail.com with "SECURITY" in the subject — do
not open public issues, do not use GitHub's advisory or
private-reporting features, and do not disclose details publicly;
contact us first and we will coordinate disclosure privately.

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
