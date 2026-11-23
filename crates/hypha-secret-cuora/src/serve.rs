//! Persistent `cuora serve` client — the ndjson JSON-RPC transport
//! alternative to shelling the `cuora` CLI per call.
//!
//! `cuora serve` is one JSON request per stdin line → one JSON
//! response per stdout line (protocol `cuora-serve/1`); audit ndjson
//! stays on stderr, which this client discards (`Stdio::null`) so it
//! never pollutes the parent's stream. The child spawns lazily on
//! the first request and is reused for the client's lifetime; a
//! dead child, a closed stdout (EOF), a malformed reply line, or a
//! request timeout drops the session and the next call respawns.
//!
//! Cold-start deaths get a wider retry budget than warm ones: at
//! service boot a `cuora serve` child can race vault/keyring
//! readiness or a SIGKILL'd predecessor's lock, and the millisecond
//! flakes the per-request retry covers clear long before the child
//! is spawnable. Until the first reply ever lands, `call` keeps
//! respawning with backoff for [`CuoraServe::dead_budget`]; after
//! that a crash-looping child is a broken `cuora` and gets the
//! historical three fast attempts. A genuinely missing binary
//! (`ENOENT` at spawn) never enters either budget — it fails fast
//! on every call.
//!
//! Requests serialize on a mutex — the wire is one-request-at-a-time
//! by design, and the timeout applies to the whole
//! write+read roundtrip while the lock is held.
//!
//! Value handling mirrors the CLI surface deliberately: [`CuoraServe::get`]
//! answers `Option<String>` (`None` on *any* failure — missing binary,
//! transport error, `not_found`, `denied` — so callers' fallthrough
//! chains behave identically), [`CuoraServe::put`] answers
//! `io::Result<()>`. `put` carries the value in the request's
//! `value` field — never stdin of a side channel, never argv.
//!
//! The audit `actor` is a constructor param ([`CuoraServe::new`]) —
//! each consumer passes its own name (`peregrin`, `bupha`, …); it
//! seeds both the child's `--actor` flag and every request's
//! `actor` field.

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Default per-request deadline — generous because a `[serve.pdp]`
/// consult alone may take its 5s default before the vault is even
/// touched. Override with [`CuoraServe::with_timeout`].
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long [`Drop`] waits for a clean `shutdown`-less exit (stdin
/// EOF) before killing the child.
const DROP_GRACE: Duration = Duration::from_millis(500);

/// Cold-start dead-child budget — how long `call` keeps respawning
/// while *no reply has ever landed* on this client. Boot-time
/// readiness races (vault/keyring not up yet, a predecessor's lock
/// still held) clear in seconds; the per-request backoff in
/// [`crate::GET_RETRY_BACKOFF`] is sized for millisecond flakes and
/// can't see them. Bounded — a `cuora` that never answers within
/// the budget is broken, not warming up.
const COLD_DEAD_BUDGET: Duration = Duration::from_secs(10);
/// First cold-retry sleep — doubles each dead attempt up to
/// [`COLD_DEAD_BACKOFF_MAX`]. Small enough that a child dying on
/// the first write doesn't stall the common path.
const COLD_DEAD_BACKOFF: Duration = Duration::from_millis(50);
const COLD_DEAD_BACKOFF_MAX: Duration = Duration::from_secs(1);
/// Warm-path dead attempts — once any reply has landed, a child
/// that keeps dying is a broken `cuora`; the historical three fast
/// respawns, no sleep (the caller's own retry — `get`'s
/// [`crate::GET_ATTEMPTS`] — still applies on top).
const WARM_DEAD_ATTEMPTS: u32 = 3;

/// Default max session age — a `cuora serve` child caches spawn-time
/// state (binary image, vault/keyring resolution). Observed
/// 2026-11-19: children spawned before a `cuora` upgrade kept
/// serving misses for ~2 days until the service restarted. The
/// mtime check catches binary replacements; this bounds every other
/// drift (locked keyring windows, env-side changes) — respawning is
/// one fork, so a short bound costs nothing. Override with
/// [`CuoraServe::with_max_age`].
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(900);

/// Which cuora transport [`crate::CuoraSecrets`] uses.
///
/// Consumers select it via their own env knob — the convention is
/// `<PRODUCT>_CUORA_TRANSPORT` (e.g. `PEREGRIN_CUORA_TRANSPORT`,
/// `BUPHA_CUORA_TRANSPORT`); [`CuoraTransport::from_env`] takes the
/// variable *name* so the shared crate stays product-agnostic. Only
/// `serve` (any case) selects the serve transport; unset, `cli`, or
/// anything else stays on the CLI so a typo can't silently change
/// the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CuoraTransport {
    /// Shell `cuora get`/`put` per call — the historical behavior.
    #[default]
    Cli,
    /// Persistent `cuora serve` child speaking ndjson on stdio.
    Serve,
}

impl CuoraTransport {
    /// Read the transport knob from the env var `name` — see the
    /// type docs for the `<PRODUCT>_CUORA_TRANSPORT` convention.
    #[must_use]
    pub fn from_env(name: &str) -> Self {
        Self::parse(std::env::var(name).ok().as_deref())
    }

    /// The parse rule, split out so tests pin it without touching
    /// the process environment: only `serve` (any case) flips.
    #[must_use]
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some(v) if v.eq_ignore_ascii_case("serve") => Self::Serve,
            _ => Self::Cli,
        }
    }
}

/// A live `cuora serve` child plus the plumbing to talk to it.
struct Session {
    child: Child,
    stdin: ChildStdin,
    /// Reply lines from the stdout reader thread — `Err` carries the
    /// reader's io failure; channel close means stdout hit EOF.
    lines: Receiver<io::Result<String>>,
    /// When the session spawned — feeds [`MAX_SESSION_AGE`].
    spawned_at: Instant,
    /// The resolved binary's mtime at spawn — a later mtime means
    /// the on-disk `cuora` was replaced (upgrade/reinstall) and this
    /// child still runs the old image.
    bin_mtime: Option<std::time::SystemTime>,
}

/// Why a roundtrip failed at the transport layer (as opposed to a
/// well-formed `ok:false` reply, which is a cuora answer, not a
/// transport failure).
enum Fail {
    /// Write/read/EOF/protocol desync — the session is dead.
    Dead,
    /// Spawn failed `ENOENT` — the binary (or its interpreter) is
    /// absent. Permanent for the process lifetime: no retry budget
    /// applies, `call` fails fast so a missing `cuora` under the
    /// serve transport misses as quickly as under the CLI.
    NoBinary,
    /// The deadline passed — session dropped, caller sees the error.
    Timeout,
}

/// Persistent `cuora serve` client. Spawn is lazy — construction is
/// free; the child appears on the first request. Cloning shares the
/// child via [`Arc`](std::sync::Arc) at the [`crate::CuoraSecrets`] layer; `CuoraServe`
/// itself is not `Clone`.
pub struct CuoraServe {
    /// The `cuora` binary — the consumer's `*_CUORA_BIN` override or
    /// `cuora` on PATH.
    bin: PathBuf,
    /// Audit actor — seeds the child's `--actor` and each request's
    /// `actor` field (a claimed label, like every local actor claim).
    actor: String,
    timeout: Duration,
    /// Cold-start dead-child budget — see [`COLD_DEAD_BUDGET`].
    dead_budget: Duration,
    /// Session recycle age — see [`DEFAULT_MAX_AGE`].
    max_age: Duration,
    /// Set on the first reply of any kind (`ok` or `ok:false` —
    /// either proves the wire works). Gates the cold/warm dead
    /// retry regimes in [`Self::call`].
    ever_connected: AtomicBool,
    next_id: AtomicU64,
    session: Mutex<Option<Session>>,
}

impl CuoraServe {
    /// A client over `bin` attributing requests to `actor` with
    /// `DEFAULT_TIMEOUT`.
    #[must_use]
    pub fn new(bin: PathBuf, actor: impl Into<String>) -> Self {
        Self {
            bin,
            actor: actor.into(),
            timeout: DEFAULT_TIMEOUT,
            dead_budget: COLD_DEAD_BUDGET,
            max_age: DEFAULT_MAX_AGE,
            ever_connected: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            session: Mutex::new(None),
        }
    }

    /// Override the per-request deadline (tests use tiny values).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the cold-start dead-child budget (tests use tiny
    /// values — the default is sized for real boot-time readiness
    /// races, which no test wants to wait out).
    #[must_use]
    pub fn with_dead_budget(mut self, dead_budget: Duration) -> Self {
        self.dead_budget = dead_budget;
        self
    }

    /// Override the session recycle age (tests use tiny values).
    #[must_use]
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// `get` — the secret value, or `None` on *any* failure.
    ///
    /// `Ok` replies yield `result.value` (a JSON string); empty
    /// values answer `None`, matching the CLI transport's
    /// empty-stdout rule. `not_found`, `denied`, transport errors,
    /// and a missing binary all collapse to `None` — "not here" —
    /// so secret-chain fallthrough is identical across transports.
    ///
    /// Transient answers retry with the same bounded budget as the
    /// CLI leg (T-059): a dead child already retries inside
    /// `Self::call`, but an `ok:false` `op_error`/`pdp_unreachable`
    /// is a *cuora* answer — a vault transport flake wearing a
    /// well-formed reply — which used to collapse straight to
    /// `None`. Definitive codes (`not_found`, `denied`,
    /// `bad_request`, `unknown_op`) still miss on the first reply.
    pub fn get(&self, rref: &str) -> Option<String> {
        for attempt in 0..crate::GET_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(crate::GET_RETRY_BACKOFF * attempt);
            }
            let Ok(reply) = self.call(&json!({
                "op": "get",
                "ref": rref,
                "actor": self.actor,
            })) else {
                continue;
            };
            if reply.get("ok").and_then(Value::as_bool) != Some(true) {
                if transient_reply(&reply) {
                    continue;
                }
                return None;
            }
            let value = reply.get("result")?.get("value")?.as_str()?.to_string();
            return (!value.is_empty()).then_some(value);
        }
        None
    }

    /// `put` — store `value` under `rref`. The value rides the
    /// request's `value` field (the wire's designed channel — the
    /// parent owns the pipe), capped at 64 KiB by cuora itself.
    pub fn put(&self, rref: &str, value: &str) -> io::Result<()> {
        let reply = self
            .call(&json!({
                "op": "put",
                "ref": rref,
                "value": value,
                "actor": self.actor,
            }))
            .map_err(io::Error::other)?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "cuora serve put {rref}: {}",
                reply_error(&reply)
            )))
        }
    }

    /// `check` — the config + declared-surface report, `result`
    /// verbatim. Useful for diagnostics; not on the secret path.
    pub fn check(&self) -> io::Result<Value> {
        let reply = self
            .call(&json!({"op": "check", "actor": self.actor}))
            .map_err(io::Error::other)?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(io::Error::other(format!(
                "cuora serve check: {}",
                reply_error(&reply)
            )))
        }
    }

    /// One request → one reply. Transport failures retry on a
    /// freshly spawned child (a request that never got answered is
    /// safe to replay — `get` is idempotent and `put`/`rotate`
    /// re-putting the same value is a no-op semantically); a parsed
    /// reply — `ok` or `ok:false` — returns as-is.
    ///
    /// Dead-child retries come in two regimes (see the module docs):
    /// *cold* — no reply has ever landed on this client, so the
    /// spawn/readiness race gets [`Self::dead_budget`] of respawns
    /// with backoff; *warm* — a session proved itself once, so a
    /// crash-looping child is a broken `cuora` and gets
    /// [`WARM_DEAD_ATTEMPTS`] fast attempts. `Fail::NoBinary` is
    /// neither — `ENOENT` at spawn is permanent and fails fast.
    fn call(&self, request: &Value) -> Result<Value, String> {
        let mut req = request.clone();
        // `id` is echoed verbatim for correlation; absent → null is
        // legal, but a real id makes a desynced wire diagnosable.
        req["id"] = json!(self.next_id.fetch_add(1, Ordering::Relaxed));
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        // The cold deadline is fixed at call entry; `ever_connected`
        // flipping mid-call (a concurrent request landed a reply)
        // demotes this call to the warm regime on the next dead
        // attempt — readiness is a property of the transport, not
        // the request.
        let cold_deadline = (!self.ever_connected.load(Ordering::Relaxed))
            .then(|| Instant::now() + self.dead_budget);
        let mut backoff = COLD_DEAD_BACKOFF;
        let mut dead_attempts = 0u32;
        loop {
            match self.roundtrip(&line) {
                Ok(reply) => {
                    self.ever_connected.store(true, Ordering::Relaxed);
                    return Ok(reply);
                }
                Err(Fail::NoBinary) => {
                    eprintln!("hypha-secret-cuora: cuora serve binary unavailable");
                    return Err("cuora serve binary unavailable".to_string());
                }
                Err(Fail::Dead) => {
                    self.drop_session();
                    dead_attempts += 1;
                    let cold =
                        cold_deadline.filter(|_| !self.ever_connected.load(Ordering::Relaxed));
                    match cold {
                        Some(deadline) => {
                            let now = Instant::now();
                            if now >= deadline {
                                break;
                            }
                            thread::sleep(backoff.min(deadline - now));
                            backoff = (backoff * 2).min(COLD_DEAD_BACKOFF_MAX);
                        }
                        None => {
                            if dead_attempts >= WARM_DEAD_ATTEMPTS {
                                break;
                            }
                        }
                    }
                }
                Err(Fail::Timeout) => {
                    self.drop_session();
                    eprintln!(
                        "hypha-secret-cuora: cuora serve {} timed out after {:?}",
                        request.get("op").and_then(Value::as_str).unwrap_or("?"),
                        self.timeout
                    );
                    return Err(format!(
                        "cuora serve {} timed out after {:?}",
                        request.get("op").and_then(Value::as_str).unwrap_or("?"),
                        self.timeout
                    ));
                }
            }
        }
        // The only `break` is the dead-child budget — every other
        // outcome returned above.
        eprintln!("hypha-secret-cuora: cuora serve child died");
        Err("cuora serve child died".to_string())
    }

    /// Write `line` + `\n`, flush, read one reply line, parse it.
    /// Any transport-level failure marks the session dead.
    fn roundtrip(&self, line: &str) -> Result<Value, Fail> {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A live-but-stale child is worse than a dead one: it answers
        // well-formed misses forever. Recycle on binary replacement
        // or max age before writing the request.
        if let Some(reason) = guard.as_ref().and_then(|s| self.stale_reason(s)) {
            if let Some(mut session) = guard.take() {
                reap(&mut session.child);
            }
            eprintln!("hypha-secret-cuora: recycling stale cuora serve session ({reason})");
        }
        if guard.is_none() {
            match self.spawn() {
                Ok(session) => *guard = Some(session),
                // `ENOENT` — binary or interpreter absent. Permanent:
                // surface it so `call` fails fast instead of burning
                // the cold budget on a spawn that can never succeed.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(Fail::NoBinary);
                }
                // Any other spawn/io failure (fork pressure, EAGAIN)
                // is transient-shaped — the dead-child retry path.
                Err(_) => return Err(Fail::Dead),
            }
        }
        let Some(session) = guard.as_mut() else {
            return Err(Fail::Dead);
        };
        session.exchange(line, self.timeout)
    }

    /// Why a live session should be recycled, if it should —
    /// `Some(reason)` drops it. Binary mtime is compared against the
    /// spawn-time snapshot; an unresolvable binary skips the check
    /// (PATH-less names still spawn fine via execvp).
    fn stale_reason(&self, session: &Session) -> Option<&'static str> {
        if session.spawned_at.elapsed() >= self.max_age {
            return Some("max session age");
        }
        let current = resolve_bin(&self.bin)
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        match (session.bin_mtime, current) {
            (Some(spawned), Some(now)) if now > spawned => Some("binary replaced"),
            _ => None,
        }
    }

    /// Spawn `cuora serve` — stdin/stdout piped, stderr discarded
    /// (audit ndjson is cuora's own stream; inheriting it would
    /// interleave with the parent's). A stdout reader thread feeds
    /// reply lines into a channel so [`Session::exchange`] can apply
    /// the timeout without blocking the read itself.
    fn spawn(&self) -> io::Result<Session> {
        let mut child = Command::new(&self.bin)
            // `--actor` seeds the audit default; each request also
            // carries `actor` explicitly (the serve wire's field).
            .arg("--actor")
            .arg(&self.actor)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("cuora serve spawned without stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("cuora serve spawned without stdout"))?;
        let (tx, lines) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    // EOF or a read error — the child's stdout is
                    // gone; channel close tells the client.
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(Ok(buf.clone())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Session {
            child,
            stdin,
            lines,
            spawned_at: Instant::now(),
            bin_mtime: resolve_bin(&self.bin)
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok()),
        })
    }

    /// Kill + reap the current session, if any. Dropping the
    /// `ChildStdin` first gives cuora its EOF → clean exit; we then
    /// wait briefly before killing so `Drop` stays bounded.
    fn drop_session(&self) {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut session) = guard.take() {
            reap(&mut session.child);
        }
    }
}

impl Session {
    /// One request/response on the live child. `try_wait` catches an
    /// already-exited child before we write into a dead pipe; the
    /// channel states (timeout / disconnect / reader error) catch
    /// everything else.
    fn exchange(&mut self, line: &str, timeout: Duration) -> Result<Value, Fail> {
        // Already reaped-exited — dead before we even write.
        if let Ok(Some(_)) = self.child.try_wait() {
            return Err(Fail::Dead);
        }
        if self
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .is_err()
        {
            return Err(Fail::Dead);
        }
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(reply)) => serde_json::from_str(&reply).map_err(|_| Fail::Dead),
            // Reader thread hit an io error or stdout EOF.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err(Fail::Dead),
            Err(RecvTimeoutError::Timeout) => Err(Fail::Timeout),
        }
    }
}

/// Resolve `bin` to a concrete path for mtime checks — PATH
/// lookup for bare names (mirroring execvp), the path itself
/// otherwise. `None` when nothing resolves; callers treat that as
/// "can't check", never as an error.
fn resolve_bin(bin: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if bin.components().count() > 1 {
        return Some(bin.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    })
}

/// Close stdin (EOF → cuora exits cleanly), wait up to [`DROP_GRACE`],
/// then kill + reap. Bounded — a wedged child can't hang drop.
fn reap(child: &mut Child) {
    // `stdin` was moved into the Session; dropping the Session
    // closes it. Here we only own `child` — close is implicit when
    // the Session dropped. Wait briefly, then kill.
    let deadline = Instant::now() + DROP_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

impl Drop for CuoraServe {
    fn drop(&mut self) {
        // Take the session out so its `ChildStdin` closes (EOF) —
        // `cuora serve` exits cleanly on stdin EOF — then reap.
        if let Ok(mut guard) = self.session.lock() {
            if let Some(mut session) = guard.take() {
                drop(session.stdin);
                reap(&mut session.child);
            }
        }
    }
}

impl fmt::Debug for CuoraServe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CuoraServe")
            .field("bin", &self.bin)
            .field("actor", &self.actor)
            .field("timeout", &self.timeout)
            .field("dead_budget", &self.dead_budget)
            .finish_non_exhaustive()
    }
}

/// Is an `ok:false` reply a *transient* answer worth retrying?
/// `op_error` wraps vault/ops failures (HTTP flakes, decrypt
/// transport errors) and `pdp_unreachable` is a policy engine
/// that may come back; `not_found`/`denied`/`bad_request`/
/// `unknown_op` are definitive — a retry can't change them.
/// Unknown future codes default to definitive: retrying is the
/// risky direction (it can mask a real denial behind latency),
/// missing is the safe one.
fn transient_reply(reply: &Value) -> bool {
    matches!(
        reply
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str),
        Some("op_error" | "pdp_unreachable")
    )
}

/// `"code: message"` for an `ok:false` reply — diagnostics only.
fn reply_error(reply: &Value) -> String {
    reply.get("error").map_or_else(
        || "no error detail".to_string(),
        |e| {
            format!(
                "{}: {}",
                e.get("code").and_then(Value::as_str).unwrap_or("?"),
                e.get("message").and_then(Value::as_str).unwrap_or("?")
            )
        },
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `cuora` speaking the ndjson serve wire. Requests are
    /// compact single-line JSON we generate, so `grep -o` field
    /// extraction is exact. State lives in `$DIR/db-<key>` files;
    /// `$DIR/spawns` counts child invocations (spawn-on-demand and
    /// respawn tests read it). `$DIR/die-once` makes the *next*
    /// spawn consume one line then exit without replying — EOF
    /// mid-request. `$DIR/die-after` makes the *running* child exit
    /// right after its next reply — death between requests.
    /// `$DIR/hang` makes every spawn sleep forever — the timeout
    /// trigger. `$DIR/operr-<key>` makes the next `get` for that
    /// key answer a transient `op_error` once (consumed);
    /// `$DIR/operr-always` makes every `get` answer it — the
    /// T-059 retry-budget fixtures. `$DIR/flaky` holds a count N:
    /// the next N spawns exit 1 immediately without reading —
    /// the cold-start readiness-race fixture.
    fn stub(dir: &std::path::Path, extra: &str) -> PathBuf {
        let script = format!(
            "#!/bin/sh\n\
             dir='{dir}'\n\
             echo x >> \"$dir/spawns\"\n\
             {extra}\n\
             if [ -f \"$dir/flaky\" ]; then\n\
               n=$(cat \"$dir/flaky\")\n\
               if [ \"$n\" -gt 0 ]; then\n\
                 echo $((n - 1)) > \"$dir/flaky\"\n\
                 exit 1\n\
               fi\n\
             fi\n\
             if [ -f \"$dir/die-once\" ]; then\n\
               rm \"$dir/die-once\"\n\
               IFS= read -r _line\n\
               exit 0\n\
             fi\n\
             if [ -f \"$dir/hang\" ]; then exec sleep 60; fi\n\
             while IFS= read -r line; do\n\
               op=$(printf '%s' \"$line\" | grep -o '\"op\":\"[a-z]*\"' | cut -d'\"' -f4)\n\
               ref=$(printf '%s' \"$line\" | grep -o '\"ref\":\"[^\"]*\"' | cut -d'\"' -f4)\n\
               val=$(printf '%s' \"$line\" | grep -o '\"value\":\"[^\"]*\"' | cut -d'\"' -f4)\n\
               key=${{ref##*/}}\n\
               case \"$op\" in\n\
                 get)\n\
                   f=\"$dir/db-$key\"\n\
                   if [ -f \"$dir/operr-always\" ] || [ -f \"$dir/operr-$key\" ]; then\n\
                     rm -f \"$dir/operr-$key\"\n\
                     printf '{{\"id\":1,\"ok\":false,\"error\":{{\"code\":\"op_error\",\"message\":\"vault transport\"}}}}\\n'\n\
                   elif [ -f \"$f\" ]; then\n\
                     v=$(cat \"$f\")\n\
                     printf '{{\"id\":1,\"ok\":true,\"result\":{{\"ref\":\"%s\",\"value\":\"%s\"}}}}\\n' \"$ref\" \"$v\"\n\
                   else\n\
                     printf '{{\"id\":1,\"ok\":false,\"error\":{{\"code\":\"not_found\",\"message\":\"absent\"}}}}\\n'\n\
                   fi ;;\n\
                 put)\n\
                   printf '%s' \"$val\" > \"$dir/db-$key\"\n\
                   printf '{{\"id\":1,\"ok\":true,\"result\":{{\"ref\":\"%s\",\"version\":1}}}}\\n' \"$ref\" ;;\n\
                 check)\n\
                   printf '{{\"id\":1,\"ok\":true,\"result\":{{\"ok\":true,\"vaults\":[]}}}}\\n' ;;\n\
                 shutdown)\n\
                   printf '{{\"id\":1,\"ok\":true,\"result\":{{\"shutdown\":true}}}}\\n'\n\
                   exit 0 ;;\n\
                 *) printf '{{\"id\":1,\"ok\":false,\"error\":{{\"code\":\"unknown_op\",\"message\":\"x\"}}}}\\n' ;;\n\
               esac\n\
               if [ -f \"$dir/die-after\" ]; then\n\
                 rm \"$dir/die-after\"\n\
                 exit 0\n\
               fi\n\
             done\n",
            dir = dir.display(),
            extra = extra,
        );
        let bin = dir.join("cuora-stub.sh");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn spawns(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("spawns")).map_or(0, |s| s.lines().count())
    }

    #[test]
    fn transport_parse_only_flips_on_serve() {
        // The env var can't be set in edition-2024 tests — assert the
        // parse rule on the value space instead: only "serve" flips.
        assert_eq!(CuoraTransport::default(), CuoraTransport::Cli);
        assert_eq!(CuoraTransport::parse(None), CuoraTransport::Cli);
        assert_eq!(CuoraTransport::parse(Some("cli")), CuoraTransport::Cli);
        assert_eq!(CuoraTransport::parse(Some("")), CuoraTransport::Cli);
        assert_eq!(CuoraTransport::parse(Some("bogus")), CuoraTransport::Cli);
        assert_eq!(CuoraTransport::parse(Some("SERVE")), CuoraTransport::Serve);
        assert_eq!(CuoraTransport::parse(Some("Serve")), CuoraTransport::Serve);
    }

    #[test]
    fn spawn_is_lazy_and_get_put_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin, "test-actor");
        // Construction spawned nothing.
        assert_eq!(spawns(dir.path()), 0);
        // Miss → None, and only now does the child exist.
        assert_eq!(serve.get("cuora://test/MISSING"), None);
        assert_eq!(spawns(dir.path()), 1);
        // put then get — one child serves both.
        serve.put("cuora://test/NAME", "s3cret").unwrap();
        assert_eq!(serve.get("cuora://test/NAME").as_deref(), Some("s3cret"));
        assert_eq!(spawns(dir.path()), 1, "the child is reused");
        // check rides the same session.
        let report = serve.check().unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(spawns(dir.path()), 1);
    }

    #[test]
    fn respawns_after_child_death() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin, "test-actor");
        serve.put("cuora://test/A", "one").unwrap();
        assert_eq!(spawns(dir.path()), 1);
        // `die-after`: the running child answers this get, then
        // exits — death *between* requests.
        std::fs::write(dir.path().join("die-after"), "").unwrap();
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(spawns(dir.path()), 1, "the reply landed before exit");
        // Next call finds the child dead → respawn. `die-once` arms
        // that respawn to eat the retried line and die *mid-request*
        // (EOF, no reply) → a second respawn serves it.
        std::fs::write(dir.path().join("die-once"), "").unwrap();
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(spawns(dir.path()), 3, "dead-detect + EOF-die + serve");
        // And the session is healthy again — no more spawns.
        serve.put("cuora://test/B", "two").unwrap();
        assert_eq!(serve.get("cuora://test/B").as_deref(), Some("two"));
        assert_eq!(spawns(dir.path()), 3);
    }

    #[test]
    fn timeout_maps_to_none_and_err() {
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        std::fs::write(dir.path().join("hang"), "").unwrap();
        let serve = CuoraServe::new(bin, "test-actor").with_timeout(Duration::from_millis(100));
        assert_eq!(serve.get("cuora://test/X"), None);
        assert!(serve.put("cuora://test/X", "v").is_err());
        assert!(serve.check().is_err());
    }

    #[test]
    fn missing_binary_is_none_and_err_not_panic() {
        let serve = CuoraServe::new(PathBuf::from("/nonexistent/cuora"), "test-actor");
        assert_eq!(serve.get("cuora://test/X"), None);
        assert!(serve.put("cuora://test/X", "v").is_err());
    }

    #[test]
    fn get_retries_a_transient_op_error_reply() {
        // T-059: an `ok:false` `op_error` is a vault transport flake
        // wearing a well-formed reply — it must not collapse to
        // `None` on the first answer.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin, "test-actor");
        serve.put("cuora://test/T", "sekret").unwrap();
        std::fs::write(dir.path().join("operr-T"), "").unwrap();
        assert_eq!(serve.get("cuora://test/T").as_deref(), Some("sekret"));
        // The one-shot file is consumed — a second get is clean.
        assert!(!dir.path().join("operr-T").exists());
        assert_eq!(serve.get("cuora://test/T").as_deref(), Some("sekret"));
    }

    #[test]
    fn get_gives_up_after_the_retry_budget() {
        // A *persistent* transient answer exhausts the same bounded
        // budget as the CLI leg, then answers None.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        std::fs::write(dir.path().join("operr-always"), "").unwrap();
        let serve = CuoraServe::new(bin, "test-actor");
        assert_eq!(serve.get("cuora://test/X"), None);
    }

    #[test]
    fn get_retries_cold_start_deaths_until_the_child_is_ready() {
        // The boot race: the first spawns die before answering —
        // vault/keyring not up yet, a SIGKILL'd predecessor's lock
        // still held. Until the first reply lands the client keeps
        // respawning with backoff, so a readiness window measured
        // in seconds doesn't collapse to a permanent miss.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        std::fs::write(dir.path().join("db-T"), "sekret").unwrap();
        std::fs::write(dir.path().join("flaky"), "3").unwrap();
        let serve = CuoraServe::new(bin, "test-actor").with_dead_budget(Duration::from_secs(5));
        assert_eq!(serve.get("cuora://test/T").as_deref(), Some("sekret"));
        assert_eq!(spawns(dir.path()), 4, "3 dead spawns + 1 serving");
    }

    #[test]
    fn warm_deaths_keep_the_fast_budget() {
        // Once a reply has landed the transport is proven — a
        // crash-looping child is a broken `cuora`, so dead retries
        // go back to the historical three fast attempts per call
        // (`get`'s own GET_ATTEMPTS on top), not the cold budget.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        std::fs::write(dir.path().join("db-A"), "one").unwrap();
        // A generous budget proves the warm path ignores it.
        let serve = CuoraServe::new(bin, "test-actor").with_dead_budget(Duration::from_secs(30));
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(spawns(dir.path()), 1);
        // `die-after` retires the live session (it answers once
        // more, then exits); `flaky` then kills every respawn —
        // warm regime: each `call` gets WARM_DEAD_ATTEMPTS
        // roundtrips, `get` retries GET_ATTEMPTS calls.
        std::fs::write(dir.path().join("die-after"), "").unwrap();
        std::fs::write(dir.path().join("flaky"), "100").unwrap();
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(serve.get("cuora://test/A"), None);
        // 1 serving spawn + the warm retries: the first `call`'s
        // opening roundtrip dies on the *stale* session (no spawn),
        // leaving WARM_DEAD_ATTEMPTS-1 respawns; each later `get`
        // attempt spawns WARM_DEAD_ATTEMPTS children.
        assert_eq!(
            spawns(dir.path()),
            1 + (WARM_DEAD_ATTEMPTS - 1) as usize
                + WARM_DEAD_ATTEMPTS as usize * (crate::GET_ATTEMPTS - 1) as usize,
            "warm calls give up after WARM_DEAD_ATTEMPTS each"
        );
    }

    #[test]
    fn not_found_never_enters_the_dead_budget() {
        // A well-formed `not_found` is a cuora answer, not a
        // transport failure — it misses on the first reply with no
        // respawn, so fallthrough chains don't pay for it.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin, "test-actor");
        assert_eq!(serve.get("cuora://test/MISSING"), None);
        assert_eq!(serve.get("cuora://test/MISSING"), None);
        assert_eq!(spawns(dir.path()), 1, "one child serves every miss");
    }

    #[test]
    fn recycles_the_session_when_the_binary_is_replaced() {
        // The 2026-11-19 incident: a `cuora` upgrade left long-lived
        // serve children running the old image, serving misses for
        // days. Rewriting the stub bumps its mtime — the next call
        // must reap the stale child and respawn.
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin.clone(), "test-actor");
        serve.put("cuora://test/A", "one").unwrap();
        assert_eq!(spawns(dir.path()), 1);
        // Same file, newer mtime — as a package upgrade leaves it.
        // The replacement still counts spawns, then exits without
        // serving: the respawn is observable, the call fails
        // transport-side rather than reusing the stale session.
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho x >> '{}'\nexit 0\n",
                dir.path().join("spawns").display()
            ),
        )
        .unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&bin).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        drop(f);
        // The stub is now `exit 0` with no serve loop — but the
        // point is the respawn happened: the call fails transport-
        // side rather than reusing the stale session.
        assert_eq!(serve.get("cuora://test/A"), None);
        assert!(spawns(dir.path()) >= 2, "stale child was recycled");
    }

    #[test]
    fn recycles_the_session_after_max_age() {
        // Binary untouched — the age bound is the backstop for
        // drift the mtime check can't see (locked keyring windows,
        // env-side changes).
        let dir = tempfile::tempdir().unwrap();
        let bin = stub(dir.path(), "");
        let serve = CuoraServe::new(bin, "test-actor").with_max_age(Duration::from_millis(50));
        serve.put("cuora://test/A", "one").unwrap();
        assert_eq!(spawns(dir.path()), 1);
        thread::sleep(Duration::from_millis(80));
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(spawns(dir.path()), 2, "aged-out child was recycled");
        // Within the new session's window — reused again.
        assert_eq!(serve.get("cuora://test/A").as_deref(), Some("one"));
        assert_eq!(spawns(dir.path()), 2);
    }

    #[test]
    fn get_does_not_retry_definitive_codes() {
        // `not_found` is the engine's real miss — retrying it would
        // stall every fallthrough lookup. The stub only counts
        // *spawns*, not replies, so pin the behavior differently:
        // a definitive miss returns None while a same-session
        // transient still retries (covered above). Here we assert
        // the classification helper directly.
        let transient = serde_json::json!({
            "id": 1, "ok": false,
            "error": {"code": "op_error", "message": "x"}
        });
        let pdp = serde_json::json!({
            "id": 1, "ok": false,
            "error": {"code": "pdp_unreachable", "message": "x"}
        });
        let definitive = serde_json::json!({
            "id": 1, "ok": false,
            "error": {"code": "not_found", "message": "x"}
        });
        let denied = serde_json::json!({
            "id": 1, "ok": false,
            "error": {"code": "denied", "message": "x"}
        });
        let unknown = serde_json::json!({"id": 1, "ok": false});
        assert!(transient_reply(&transient));
        assert!(transient_reply(&pdp));
        assert!(!transient_reply(&definitive));
        assert!(!transient_reply(&denied));
        // No error object / unknown code → definitive (miss is the
        // safe direction).
        assert!(!transient_reply(&unknown));
    }
}
