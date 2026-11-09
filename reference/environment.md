---
url: /hypha/reference/environment.md
---
# Environment

Hypha-native variables (landed crates):

| Variable | Crate | Meaning |
| --- | --- | --- |
| `HYPHA_OTEL_ENDPOINT` | `hypha-telemetry` | OTLP base endpoint; `/v1/traces` appended when no path present |
| `HYPHA_OTEL_SERVICE_NAME` | `hypha-telemetry` | service.name (default `"hypha"`, `OTEL_SERVICE_NAME` also honored) |
| `OTEL_EXPORTER_OTLP_*` | `hypha-telemetry` | standard OTLP vars also enable export |

Standard OTEL vars (`OTEL_*`) are honored per OpenTelemetry semantic
conventions. Node/hub variables (`PEREGRIN_*` today — `HYPHA_*`
post-cutover) land with the binaries in P3; see the org ledger for
the rename mapping.
