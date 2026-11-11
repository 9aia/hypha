//! hypha-serve-cli — the one-shot front door for ndjson `*-serve`
//! binaries (P-020). Every serve binary's `main.rs` is the same
//! three lines: no args → service loop; args → [`one_shot`] maps
//! `<op> [--dir D] [--set k=v]… [--yes|--dry-run] [--range R]` onto
//! the serve's request type and prints the response JSON.
//!
//! Deliberately dumb: ops stay JSON data, params ride `--set
//! k=v` — no per-op clap flags. Rich UX belongs to pavo views and
//! the product CLI, not here.
//!
//! ```ignore
//! fn main() {
//!     let args: Vec<String> = std::env::args().skip(1).collect();
//!     if args.is_empty() { std::process::exit(my_serve::serve()); }
//!     std::process::exit(hypha_serve_cli::one_shot(&args, |op, params| {
//!         my_serve::dispatch(&GitVcs, &Request { op: op.into(), params })
//!     }));
//! }
//! ```

use serde_json::{Value, json};

/// Parsed one-shot invocation: op name + params object.
#[derive(Debug, Clone)]
pub struct OneShot {
    pub op: String,
    pub params: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("missing op — usage: <bin> <op> [--dir D] [--set k=v]… [--yes|--dry-run]")]
    MissingOp,
    #[error("unknown flag {0:?}")]
    UnknownFlag(String),
    #[error("{0} requires a value")]
    MissingValue(String),
}

/// Parse argv (already stripped of the binary name) into a
/// [`OneShot`]. Recognized flags:
///
/// - `--dir <path>` → `params.dir` (canonicalized when it exists)
/// - `--set <k=v>` → `params[k]` (JSON-parsed value, bare string
///   fallback; repeatable)
/// - `--range <rev>` → `params.range`
/// - `--yes`/`--execute` → `params.dry_run = false`
/// - `--dry-run` → `params.dry_run = true`
/// - `-h`/`--help` → prints usage to stderr, `Ok` with op `"help"`
///
/// Anything else is an [`ParseError::UnknownFlag`].
pub fn parse(args: &[String]) -> Result<OneShot, ParseError> {
    let Some(op) = args.first().cloned() else {
        return Err(ParseError::MissingOp);
    };
    if op == "-h" || op == "--help" {
        return Ok(OneShot {
            op: "help".into(),
            params: json!({}),
        });
    }
    let mut params = json!({ "dir": "." });
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        let take = |i: &mut usize| -> Result<String, ParseError> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| ParseError::MissingValue(flag.to_string()))
        };
        match flag {
            "--dir" => params["dir"] = json!(take(&mut i)?),
            "--set" => {
                let kv = take(&mut i)?;
                if let Some((k, v)) = kv.split_once('=') {
                    params[k] = parse_value(v);
                }
            }
            "--range" => params["range"] = json!(take(&mut i)?),
            "--yes" | "--execute" => params["dry_run"] = json!(false),
            "--dry-run" => params["dry_run"] = json!(true),
            "-h" | "--help" => {
                return Ok(OneShot {
                    op: "help".into(),
                    params: json!({}),
                });
            }
            other => return Err(ParseError::UnknownFlag(other.to_string())),
        }
        i += 1;
    }
    // Canonicalize dir when it resolves — serve ops get a stable
    // absolute path regardless of caller cwd.
    if let Some(d) = params["dir"].as_str() {
        let p = std::path::PathBuf::from(d);
        params["dir"] = json!(p.canonicalize().unwrap_or(p));
    }
    Ok(OneShot { op, params })
}

/// `--set` values: JSON when parseable (`true`, `123`, `[…]`,
/// `{"…"}`), bare string otherwise.
fn parse_value(v: &str) -> Value {
    serde_json::from_str(v).unwrap_or_else(|_| json!(v))
}

/// Run a one-shot: parse args, dispatch via `f(op, params)`, print
/// `f`'s JSON result, exit 0 on success / 1 on failure / 2 on
/// usage errors. `f` returns the response value to print — serve
/// binaries pass their `dispatch` result serialized.
pub fn one_shot<F>(args: &[String], f: F) -> i32
where
    F: FnOnce(&str, &Value) -> Result<Value, String>,
{
    let shot = match parse(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if shot.op == "help" {
        eprintln!(
            "usage: <bin> <op> [--dir D] [--set k=v]… [--range R] [--yes|--dry-run]\n\
             no args = ndjson service on stdin/stdout"
        );
        return 0;
    }
    match f(&shot.op, &shot.params) {
        Ok(v) => {
            println!("{}", serde_json::to_string(&v).unwrap_or_default());
            // Serve convention: {"ok": bool, …} — ok:false exits 1.
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags() {
        let args: Vec<String> = [
            "release",
            "--dir",
            ".",
            "--set",
            "range=\"HEAD~3..HEAD\"",
            "--set",
            "dry_run=false",
            "--yes",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = parse(&args).unwrap();
        assert_eq!(s.op, "release");
        assert_eq!(s.params["range"], "HEAD~3..HEAD");
        assert_eq!(s.params["dry_run"], false);
        assert!(s.params["dir"].as_str().unwrap().starts_with('/'));
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(parse(&[]), Err(ParseError::MissingOp)));
        let args = vec!["plan".to_string(), "--bogus".to_string()];
        assert!(matches!(parse(&args), Err(ParseError::UnknownFlag(_))));
        let args = vec!["plan".to_string(), "--dir".to_string()];
        assert!(matches!(parse(&args), Err(ParseError::MissingValue(_))));
    }

    #[test]
    fn set_values_json_or_string() {
        assert_eq!(parse_value("true"), json!(true));
        assert_eq!(parse_value("42"), json!(42));
        assert_eq!(parse_value("abc"), json!("abc"));
        assert_eq!(parse_value("[1,2]"), json!([1, 2]));
    }
}
