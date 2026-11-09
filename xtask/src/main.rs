//! hypha-xtask — workspace automation.
//!
//! ```text
//! cargo xtask check      # fmt --check + clippy -D warnings
//! cargo xtask test       # cargo nextest run (falls back to cargo test)
//! cargo xtask coverage   # llvm-cov report (extra args pass through)
//! cargo xtask fmt        # cargo fmt --all
//! ```

use std::process::{Command, ExitCode};

/// Run `program args…` inheriting stdio; returns true on success.
fn run(program: &str, args: &[&str]) -> bool {
    match Command::new(program).args(args).status() {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("xtask: cannot run {program}: {e}");
            false
        }
    }
}

/// True when the `cargo-nextest` subcommand is installed.
fn nextest_available() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// True when the `cargo-llvm-cov` subcommand is installed.
fn llvm_cov_available() -> bool {
    Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let ok = match args.next().as_deref() {
        Some("check") => {
            run("cargo", &["fmt", "--all", "--", "--check"])
                && run(
                    "cargo",
                    &[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                )
        }
        Some("test") => {
            let cmd: &[&str] = if nextest_available() {
                &["nextest", "run", "--workspace"]
            } else {
                &["test", "--workspace"]
            };
            run("cargo", cmd)
        }
        // `cargo xtask coverage [llvm-cov args…]` — extra args pass
        // through to cargo-llvm-cov (e.g. `--lcov --output-path
        // lcov.info`); passing any report flag replaces the default
        // --html.
        Some("coverage") => {
            if !llvm_cov_available() {
                eprintln!(
                    "xtask: cargo-llvm-cov not installed — \
                     `cargo install cargo-llvm-cov` (needs the llvm-tools component)"
                );
                return ExitCode::FAILURE;
            }
            let extra: Vec<String> = args.collect();
            let wants_report = extra.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--text"
                        | "--summary-only"
                        | "--json"
                        | "--lcov"
                        | "--cobertura"
                        | "--codecov"
                        | "--open"
                )
            });
            let html = if wants_report { "" } else { " --html" };
            // Isolate coverage artifacts in a repo-local target dir —
            // a shared CARGO_TARGET_DIR lets foreign profraws/objects
            // land in this report at 0% and corrupt TOTAL.
            let cov_target = std::env::current_dir()
                .map(|d| d.join("target/llvm-cov-target"))
                .unwrap_or_else(|_| "target/llvm-cov-target".into());
            let cov = format!(
                "eval \"$(cargo llvm-cov show-env --export-prefix)\" \
                 && cargo llvm-cov --workspace{html} \"$@\""
            );
            let status = Command::new("sh")
                .args(["-c", &cov, "xtask-coverage"])
                .args(&extra)
                .env("CARGO_LLVM_COV_TARGET_DIR", &cov_target)
                .status();
            match status {
                Ok(s) => s.success(),
                Err(e) => {
                    eprintln!("xtask: cannot run coverage: {e}");
                    false
                }
            }
        }
        Some("fmt") => run("cargo", &["fmt", "--all"]),
        Some("install") => {
            if !run("cargo", &["build", "--release", "--bins"]) {
                return ExitCode::FAILURE;
            }
            let home = std::env::var_os("HOME")
                .map_or_else(|| std::path::PathBuf::from("/"), std::path::PathBuf::from);
            let out = home.join(".local/bin");
            if let Err(e) = std::fs::create_dir_all(&out) {
                eprintln!("xtask: cannot create {}: {e}", out.display());
                return ExitCode::FAILURE;
            }
            let mut ok = true;
            for bin in ["hypha", "hypha-hub"] {
                let src = format!("target/release/{bin}");
                match std::fs::copy(&src, out.join(bin)) {
                    Ok(_) => println!("installed {bin} to {}", out.display()),
                    Err(e) => {
                        eprintln!("xtask: cannot install {bin}: {e}");
                        ok = false;
                    }
                }
            }
            ok
        }
        _ => {
            eprintln!("usage: cargo xtask <check|test|coverage|fmt|install>");
            return ExitCode::from(2);
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
