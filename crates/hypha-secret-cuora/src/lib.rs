//! Cuora impl of [`hypha_secret::Secrets`] — the org's shared
//! secrets-broker adapter (ex-`peregrin-secret-cuora`,
//! `bupha-secret-cuora`; `mimus-cuora` keeps its own dialect).
//!
//! Each secret NAME resolves to a fixed cuora ref under the
//! consumer's scope: `CuoraSecrets::new("bupha")` maps
//! `GITHUB_TOKEN` → `cuora://bupha/GITHUB_TOKEN`. Values are fetched
//! through cuora so audit events, guardrails, and vault policy apply
//! exactly as for every other consumer; the host never stores a
//! value itself.
//!
//! Two transports, selected per store via [`CuoraTransport`]:
//!
//! - `cli` (default) — shell `cuora get`/`put` per call. Secret
//!   resolution happens a handful of times per run/verb, so a
//!   per-call process is cheap and needs no shared mutable state.
//! - `serve` — a persistent `cuora serve` child speaking the
//!   ndjson JSON-RPC wire (`cuora-serve/1`) on stdio — see
//!   [`serve`]/[`CuoraServe`]. Spawned lazily on first use, reused
//!   for the client's lifetime, respawned on death. This is the
//!   transport `cuora serve` exists for (orchestrator embedding).
//!
//! A missing `cuora` binary, a denied op, or an absent ref all
//! answer `None` — "not here" — so the caller's chain falls through
//! to the legacy stores (OS keyring, env file) exactly like a
//! keyring miss on a headless box, on either transport.
//!
//! ## Env-var convention
//!
//! The shared crate never reads the environment itself — consumers
//! pass their knob *names* to [`CuoraSecrets::from_env`] /
//! [`CuoraTransport::from_env`]. The convention is
//! `<PRODUCT>_CUORA_BIN` for the binary override (tests stub it)
//! and `<PRODUCT>_CUORA_TRANSPORT` for `cli`/`serve` — e.g.
//! `PEREGRIN_CUORA_BIN` + `PEREGRIN_CUORA_TRANSPORT`. `CUORA_*` and
//! `--config`/`--env` resolution stay cuora's own — children inherit
//! the environment, so `CUORA_ENV` picks the vault env with no
//! consumer-side plumbing (the serve child resolves env at spawn;
//! per-request `env` overrides are not used).
//!
//! ## Process-wide default
//!
//! Worth sharing under the serve transport: `CuoraSecrets` is cheap
//! to construct but the serve child's whole point is persistence —
//! a fresh store per lookup would spawn and reap `cuora serve` on
//! every secret read, worse than the CLI. Consumers that want a
//! shared default keep their own `OnceLock` (the scope/env names
//! are theirs); under `cli` sharing is a no-op behaviorally.

mod serve;

pub use serve::{CuoraServe, CuoraTransport};

use hypha_secret::Secrets;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// `Secrets` over cuora — CLI or `cuora serve` transport per
/// [`CuoraTransport`]. Generic over the ref `scope` (the
/// `cuora://<scope>/…` namespace) and the audit `actor` (defaults to
/// the scope — every org consumer's actor name equals its scope).
///
/// The serve leg holds one shared child per `CuoraSecrets` clone
/// group ([`Arc`]); clones share the process.
#[derive(Clone, Debug)]
pub struct CuoraSecrets {
    /// The `cuora` binary — the consumer's `*_CUORA_BIN` override
    /// when set, else `cuora` resolved on PATH.
    bin: PathBuf,
    /// Ref namespace — every secret name lives under
    /// `cuora://<scope>/…` so `cuora list` shows the whole surface.
    scope: String,
    /// Audit actor — `--actor` on the CLI leg, `actor` on the wire
    /// for serve (a claimed label, like every local actor claim).
    actor: String,
    /// CLI vs persistent serve transport.
    transport: CuoraTransport,
    /// The serve client — constructed eagerly but spawns lazily on
    /// first request, so the CLI transport never pays for it.
    serve: Arc<CuoraServe>,
}

impl CuoraSecrets {
    /// A store over scope `scope` with the `cuora` binary on PATH,
    /// actor = scope, and the CLI transport. Use
    /// [`Self::from_env`] to honor the consumer's env knobs, or the
    /// `with_*` builders for explicit values (tests).
    #[must_use]
    pub fn new(scope: &str) -> Self {
        Self {
            bin: PathBuf::from("cuora"),
            scope: scope.to_string(),
            actor: scope.to_string(),
            transport: CuoraTransport::Cli,
            serve: Arc::new(CuoraServe::new(PathBuf::from("cuora"), scope)),
        }
    }

    /// The env-selected store — `bin_env` names the variable holding
    /// the `cuora` binary override (unset → `cuora` on PATH),
    /// `transport_env` the `<PRODUCT>_CUORA_TRANSPORT` knob. See the
    /// crate docs for the convention.
    #[must_use]
    pub fn from_env(scope: &str, bin_env: &str, transport_env: &str) -> Self {
        let bin = std::env::var_os(bin_env).map_or_else(|| PathBuf::from("cuora"), PathBuf::from);
        Self::new(scope)
            .with_bin(bin)
            .with_transport(CuoraTransport::from_env(transport_env))
    }

    /// A store over `bin` — the test seam and the "I know what I'm
    /// doing" constructor. Transport stays CLI unless
    /// [`Self::with_transport`] says otherwise.
    #[must_use]
    pub fn with_bin(mut self, bin: PathBuf) -> Self {
        self.bin.clone_from(&bin);
        self.serve = Arc::new(CuoraServe::new(bin, self.actor.clone()));
        self
    }

    /// An explicit transport — the test seam and the "I know what
    /// I'm doing" constructor.
    #[must_use]
    pub fn with_transport(mut self, transport: CuoraTransport) -> Self {
        self.transport = transport;
        self
    }

    /// Override the audit actor (defaults to the scope).
    #[must_use]
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self.serve = Arc::new(CuoraServe::new(self.bin.clone(), self.actor.clone()));
        self
    }

    /// The `cuora` binary this store shells/spawns.
    #[must_use]
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// The ref namespace (`cuora://<scope>/…`).
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The selected transport.
    #[must_use]
    pub fn transport(&self) -> CuoraTransport {
        self.transport
    }

    /// The cuora ref one secret name resolves to —
    /// `cuora://<scope>/<name>`.
    ///
    /// Names are env-var-shaped (`[A-Z0-9_]`) which cuora's ref
    /// grammar accepts verbatim — no slugging, the name IS the ref
    /// tail, so `cuora put cuora://<scope>/<NAME>` is the migration
    /// path with zero translation.
    #[must_use]
    pub fn rref(&self, name: &str) -> String {
        rref(&self.scope, name)
    }
}

/// Build the ref for a secret name under `scope`. A free fn so
/// tests (and consumers with sub-namespaces like
/// `peregrin/push`) pin the mapping without constructing a store.
#[must_use]
pub fn rref(scope: &str, name: &str) -> String {
    format!("cuora://{scope}/{name}")
}

/// `get` attempts on the CLI transport — transient flakes (T-059:
/// ~6 empty outputs in ~800 calls) get three tries; the backoff
/// stays small because the flake clears in milliseconds and the
/// common path is a single fast local/HTTP read.
const GET_ATTEMPTS: u32 = 3;
const GET_RETRY_BACKOFF: Duration = Duration::from_millis(50);

/// The outcome of one `cuora get` invocation, classified for the
/// retry loop: a definitive answer ends it, anything ambiguous
/// retries.
enum GetOnce {
    /// A value came back — done.
    Hit(String),
    /// A definitive "not here" — the engine's not-stored message,
    /// a guardrail denial, or a usage error. No retry: these
    /// can't be transient and the fallthrough chain is waiting.
    Miss,
    /// Ambiguous — spawn/io failure, empty stdout on a successful
    /// exit (impossible for a real `put`, which refuses empty
    /// values), or a nonzero exit that isn't a definitive miss
    /// (vault transport errors all land here).
    Retry,
}

/// Does a failed `cuora get` carry a *definitive* answer — one a
/// retry can't change? The engine prints `cuora: <msg>` (or
/// `{"error": …}` under `--json`, which we don't pass) and exits
/// nonzero for everything, so the message text is the only
/// signal. The not-stored string is cuora-cli's own miss wording
/// (`{rref} is not stored in env {env}`); "denied by guardrails"
/// is its advisory-deny wording; clap usage errors exit 2. Any
/// drift in those strings degrades to "retry a permanent miss" —
/// bounded and harmless — never to "retry forever" or a wrong
/// `Some`.
fn is_definitive_miss(code: Option<i32>, stderr: &str) -> bool {
    if code == Some(2) {
        return true;
    }
    stderr.contains("is not stored in env") || stderr.contains("denied by guardrails")
}

impl Secrets for CuoraSecrets {
    fn get(&self, name: &str) -> Option<String> {
        match self.transport {
            CuoraTransport::Cli => self.get_cli(name),
            CuoraTransport::Serve => self.serve.get(&self.rref(name)),
        }
    }

    /// Deploy-time migration: `cuora put` reads the value on stdin
    /// (never argv — argv lands in shell history and `/proc`).
    /// Under serve the value rides the request's `value` field —
    /// the wire's designed channel, still never argv.
    fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        match self.transport {
            CuoraTransport::Cli => self.set_cli(name, value),
            CuoraTransport::Serve => self.serve.put(&self.rref(name), value),
        }
    }
}

impl CuoraSecrets {
    /// The `cli` transport's `get` — shell `cuora get` per lookup,
    /// with a bounded retry for transient failures (T-059).
    ///
    /// `cuora get` answers a *definitive* miss on stderr +
    /// nonzero exit (`… is not stored in env …`) and every other
    /// failure — vault transport flakes, spawn errors, a wedged
    /// config load — the same way: empty stdout, nonzero exit.
    /// Collapsing all of that to `None` in one shot turned a ~1%
    /// transient flake into silent secret misses, so ambiguous
    /// outcomes get [`GET_ATTEMPTS`] tries with a short backoff.
    /// Answers that can't be transient — the not-stored message,
    /// a guardrail denial, a clap usage error (exit 2) — return
    /// `None` on the first try so the fallthrough chain doesn't
    /// pay for them.
    fn get_cli(&self, name: &str) -> Option<String> {
        for attempt in 0..GET_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(GET_RETRY_BACKOFF * attempt);
            }
            match self.get_cli_once(name) {
                GetOnce::Hit(v) => return Some(v),
                GetOnce::Miss => return None,
                GetOnce::Retry => {}
            }
        }
        None
    }

    /// One `cuora get` invocation, classified for [`Self::get_cli`]'s
    /// retry loop.
    fn get_cli_once(&self, name: &str) -> GetOnce {
        let Ok(o) = Command::new(&self.bin)
            .arg("--actor")
            .arg(&self.actor)
            .arg("get")
            .arg(self.rref(name))
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
        else {
            // Spawn/io failure (fork pressure, missing binary on a
            // flaky PATH) — transient by definition.
            return GetOnce::Retry;
        };
        if !o.status.success() {
            // stderr is small (one `cuora: …` line plus audit
            // ndjson) — keep enough to classify, cap the read the
            // pipe already bounded.
            let err = String::from_utf8_lossy(&o.stderr);
            return if is_definitive_miss(o.status.code(), &err) {
                GetOnce::Miss
            } else {
                GetOnce::Retry
            };
        }
        // `cuora get` prints the value via println! — strip exactly
        // one trailing newline (and a CR before it); interior
        // whitespace is part of the value. `put` refuses empty
        // secrets, so a successful exit can never legitimately
        // print nothing — empty stdout is itself the flake.
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        if let Some(stripped) = s.strip_suffix('\n') {
            s = stripped.strip_suffix('\r').unwrap_or(stripped).to_string();
        }
        if s.is_empty() {
            GetOnce::Retry
        } else {
            GetOnce::Hit(s)
        }
    }

    /// The `cli` transport's `set` — `cuora put` with the value on
    /// stdin (never argv — argv lands in shell history and `/proc`).
    fn set_cli(&self, name: &str, value: &str) -> std::io::Result<()> {
        let mut c = Command::new(&self.bin)
            .arg("--actor")
            .arg(&self.actor)
            .arg("put")
            .arg(self.rref(name))
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = c.stdin.take() {
            stdin.write_all(value.as_bytes())?;
        }
        let status = c.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "cuora put {} exited {status}",
                self.rref(name)
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable stub `cuora` that answers `get` from
    /// `$STUB_DIR/value` semantics baked into the script.
    fn stub(dir: &std::path::Path, script: &str) -> PathBuf {
        let bin = dir.join("cuora-stub.sh");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn rref_maps_name_under_scope() {
        assert_eq!(rref("bupha", "GITHUB_TOKEN"), "cuora://bupha/GITHUB_TOKEN");
        assert_eq!(
            rref("peregrin/push", "vapid"),
            "cuora://peregrin/push/vapid"
        );
        let s = CuoraSecrets::new("mimus");
        assert_eq!(s.rref("X"), "cuora://mimus/X");
    }

    #[test]
    fn new_defaults_to_cli_transport_and_path_bin() {
        let s = CuoraSecrets::new("demo");
        assert_eq!(s.transport(), CuoraTransport::Cli);
        assert_eq!(s.bin(), Path::new("cuora"));
        assert_eq!(s.scope(), "demo");
    }

    #[test]
    fn get_returns_value_and_strips_one_newline() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "#!/bin/sh\nprintf 's3cret\\n'\n");
        let s = CuoraSecrets::new("demo").with_bin(bin);
        assert_eq!(s.get("ANY_NAME"), Some("s3cret".to_string()));
    }

    #[test]
    fn get_answers_none_on_failure_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        let failing = stub(dir.path(), "#!/bin/sh\nexit 1\n");
        assert_eq!(CuoraSecrets::new("demo").with_bin(failing).get("X"), None);
        let empty = stub(dir.path(), "#!/bin/sh\nprintf '\\n'\n");
        assert_eq!(CuoraSecrets::new("demo").with_bin(empty).get("X"), None);
    }

    /// A stub that counts invocations in `$DIR/n` and runs `script`
    /// with `$n` = the 1-based call number — the retry fixtures.
    fn counting_stub(dir: &std::path::Path, body: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\n\
             n=$(cat '{dir}/n' 2>/dev/null || echo 0)\n\
             n=$((n+1)); echo $n > '{dir}/n'\n\
             {body}\n",
            dir = dir.display(),
            body = body,
        );
        stub(dir, &script)
    }

    fn calls(dir: &std::path::Path) -> u32 {
        std::fs::read_to_string(dir.join("n"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn get_retries_transient_failures_then_hits() {
        // T-059: a vault transport flake exits nonzero with empty
        // stdout and no definitive wording — the first two calls
        // fail that way, the third answers.
        let dir = tempfile::tempdir().unwrap();
        let bin = counting_stub(
            dir.path(),
            "if [ \"$n\" -lt 3 ]; then\n\
             \x20 echo 'cuora: get https://vault: connection reset' >&2\n\
             \x20 exit 1\n\
             fi\nprintf 's3cret\\n'\n",
        );
        let s = CuoraSecrets::new("demo").with_bin(bin);
        assert_eq!(s.get("TOKEN").as_deref(), Some("s3cret"));
        assert_eq!(calls(dir.path()), 3);
    }

    #[test]
    fn get_retries_success_with_empty_stdout() {
        // exit 0 + empty stdout can't be a real secret (`put`
        // refuses empty values) — it's the flake shape itself.
        let dir = tempfile::tempdir().unwrap();
        let bin = counting_stub(
            dir.path(),
            "if [ \"$n\" -eq 1 ]; then printf '\\n'; else printf 'v\\n'; fi\n",
        );
        let s = CuoraSecrets::new("demo").with_bin(bin);
        assert_eq!(s.get("X").as_deref(), Some("v"));
        assert_eq!(calls(dir.path()), 2);
    }

    #[test]
    fn get_treats_not_stored_as_definitive_miss() {
        // The engine's miss wording (`{rref} is not stored in env
        // {env}` on stderr, exit 1) is a real answer — no retry,
        // so the fallthrough chain doesn't pay for it.
        let dir = tempfile::tempdir().unwrap();
        let bin = counting_stub(
            dir.path(),
            "echo 'cuora: cuora://demo/X is not stored in env dev' >&2\nexit 1\n",
        );
        let s = CuoraSecrets::new("demo").with_bin(bin);
        assert_eq!(s.get("X"), None);
        assert_eq!(calls(dir.path()), 1);
    }

    #[test]
    fn get_gives_up_after_the_retry_budget() {
        // A *persistent* ambiguous failure exhausts the budget and
        // still answers None — retry must not loop forever.
        let dir = tempfile::tempdir().unwrap();
        let bin = counting_stub(dir.path(), "echo 'cuora: vault unreachable' >&2\nexit 1\n");
        let s = CuoraSecrets::new("demo").with_bin(bin);
        assert_eq!(s.get("X"), None);
        assert_eq!(calls(dir.path()), GET_ATTEMPTS);
    }

    #[test]
    fn get_answers_none_when_the_binary_cannot_spawn() {
        // Spawn/io failure is transient-shaped but bounded — a
        // missing binary retries then misses.
        let dir = tempfile::tempdir().unwrap();
        let s = CuoraSecrets::new("demo").with_bin(dir.path().join("no-such-cuora"));
        assert_eq!(s.get("X"), None);
    }

    #[test]
    fn set_writes_value_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        // The stub can't see our env — bake the capture path into
        // the generated script.
        let script = format!(
            "#!/bin/sh\ncat > '{}'\n",
            dir.path().join("stdin.txt").display()
        );
        let bin = stub(dir.path(), &script);
        CuoraSecrets::new("demo")
            .with_bin(bin)
            .set("NAME", "value-1")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap(),
            "value-1"
        );
    }

    #[test]
    fn set_errors_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "#!/bin/sh\nexit 3\n");
        assert!(
            CuoraSecrets::new("demo")
                .with_bin(bin)
                .set("NAME", "v")
                .is_err()
        );
    }

    #[test]
    fn serve_transport_roundtrips_through_the_wire() {
        // A `cuora serve` stub in the ndjson wire shape — the same
        // fake `serve.rs` tests use, inlined here so this test pins
        // the *Secrets* dispatch, not the codec.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let script = format!(
            "#!/bin/sh\n\
             while IFS= read -r line; do\n\
               op=$(printf '%s' \"$line\" | grep -o '\"op\":\"[a-z]*\"' | cut -d'\"' -f4)\n\
               ref=$(printf '%s' \"$line\" | grep -o '\"ref\":\"[^\"]*\"' | cut -d'\"' -f4)\n\
               val=$(printf '%s' \"$line\" | grep -o '\"value\":\"[^\"]*\"' | cut -d'\"' -f4)\n\
               key=${{ref##*/}}\n\
               if [ \"$op\" = put ]; then printf '%s' \"$val\" > '{db}-'\"$key\"; \\\n                 printf '{{\"id\":1,\"ok\":true,\"result\":{{}}}}\\n';\n\
               elif [ \"$op\" = get ] && [ -f '{db}-'\"$key\" ]; then \\\n                 printf '{{\"id\":1,\"ok\":true,\"result\":{{\"value\":\"%s\"}}}}\\n' \"$(cat '{db}-'\"$key\")\";\n\
               else printf '{{\"id\":1,\"ok\":false,\"error\":{{\"code\":\"not_found\",\"message\":\"x\"}}}}\\n'; fi\n\
             done\n",
            db = db.display(),
        );
        let bin = stub(dir.path(), &script);
        let s = CuoraSecrets::new("demo")
            .with_bin(bin)
            .with_transport(CuoraTransport::Serve);
        assert_eq!(s.get("MISSING"), None);
        s.set("TOKEN", "sekret").unwrap();
        assert_eq!(s.get("TOKEN").as_deref(), Some("sekret"));
    }
}
