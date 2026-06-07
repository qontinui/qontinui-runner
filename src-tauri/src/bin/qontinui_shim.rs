//! `qontinui-shim` — the compiled `.exe` install-interception shadow stub
//! (plan §6 Windows shadowing policy; deferred from Phase 2 to Phase 4).
//!
//! ## Why a compiled `.exe`
//! `cargo` / `pip` / `pip3` ship on Windows as `<name>.exe`. A `.cmd` shim
//! CANNOT shadow a `.exe` — `.EXE` precedes `.CMD` in `PATHEXT`, so under
//! PowerShell/cmd the real `cargo.exe` always wins over a `cargo.cmd`. To
//! intercept those three under PowerShell/cmd the shim MUST itself be a
//! `<name>.exe`. The materializer copies/hardlinks THIS built binary into the
//! per-terminal shim dir as `cargo.exe`, `pip.exe`, `pip3.exe`. The stub then
//! detects which tool it is impersonating from `argv[0]` and runs the same
//! classify → pre-call → gate → exec-real-tool → post-call straddle the bash
//! shim does, REUSING the lib-crate `intercept_core::{classify, gate}` (the
//! single source of truth — plan §4 Phase 4 option (a)).
//!
//! ## Hard invariants (mirrored from the bash shim / plan §6)
//! - **NEVER panic.** Every fallible step fails OPEN: on any error the stub
//!   execs the real tool unchanged. An agent's shell must never be bricked by
//!   interception. There is no `unwrap`/`expect` on a runtime path here.
//! - **Transparent stdio.** The real tool inherits this process's stdio, so its
//!   TTY detection / prompts / progress bars behave exactly as un-shimmed; the
//!   stub propagates the real exit code.
//! - **Recursion guard.** `QONTINUI_INSTALL_INTERCEPT_GUARD=1` already set ⇒
//!   pure passthrough (no runner contact); the guard is set on the real-tool
//!   child so an install script that re-invokes the tool does not re-enter.
//! - **Short connect timeout.** The pre-call uses a 3s connect timeout (plan §4
//!   Phase 4) via a dependency-free `std::net` HTTP/1.1 POST — no `reqwest`
//!   (heavy/async, wrong for a tiny synchronous stub) and no extra crates.
//! - **Real-tool resolution** scans `PATH` EXCLUDING the shim's own dir (so it
//!   never recurses), preferring `<name>.exe` then `<name>.cmd` then bare.
//!
//! Registry creds: like the rest of interception, the stub injects NO registry
//! secrets — the agent's shell already carries the operator's registry config.

use std::env;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use qontinui_runner_lib::intercept_core::classify::{self, Classification, ShimTool};
use qontinui_runner_lib::intercept_core::gate::{should_block, InterceptMode};
use qontinui_runner_lib::intercept_core::types::PackageSpecInput;

const GUARD_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_GUARD";
const PORT_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_PORT";
const MODE_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_MODE";
const OVERRIDE_ENV: &str = "QONTINUI_INSTALL_OVERRIDE";
/// Own-dir hint the materializer sets so the real-tool PATH scan can skip this
/// stub's directory even if `current_exe` is unavailable.
const SHIM_DIR_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR";

fn main() -> std::process::ExitCode {
    // argv[0] → which tool we are impersonating. Fail-open to passthrough on
    // anything we don't recognize.
    let raw_args: Vec<String> = env::args().collect();
    let tool = match detect_tool(raw_args.first().map(String::as_str)) {
        Some(t) => t,
        None => {
            // Unknown invocation name: nothing sensible to do. Exit 0 rather
            // than panic (the materializer only ever names us a known tool).
            return std::process::ExitCode::from(0);
        }
    };
    // Args after argv[0] — exactly what the bash shim sees in "$@".
    let args: Vec<String> = raw_args.into_iter().skip(1).collect();

    let code = run(tool, &args);
    // Clamp to the u8 ExitCode space; preserve 0 vs non-zero faithfully.
    std::process::ExitCode::from(code_to_u8(code))
}

/// Map a possibly-out-of-range exit code into the `ExitCode` u8 space while
/// preserving the zero/non-zero distinction (a non-zero code never collapses to
/// 0). `None` (signal-killed child) → 1.
fn code_to_u8(code: Option<i32>) -> u8 {
    match code {
        Some(0) => 0,
        Some(c) => {
            let b = (c & 0xff) as u8;
            if b == 0 {
                1
            } else {
                b
            }
        }
        None => 1,
    }
}

/// Detect the impersonated [`ShimTool`] from `argv[0]` (the file stem, lower-
/// cased, extension stripped). `cargo.exe` → `Cargo`, `pip3` → `Pip3`, etc.
/// Pure + total (returns `None` for an unknown stem) so it is unit-testable
/// without running the exe.
pub fn detect_tool(argv0: Option<&str>) -> Option<ShimTool> {
    let argv0 = argv0?;
    let stem = Path::new(argv0)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(argv0)
        .to_ascii_lowercase();
    ShimTool::from_program(&stem)
}

/// The full straddle, fail-open at every step. Returns the exit code the stub
/// should propagate (the real tool's, or the spawn-failure surrogate).
fn run(tool: ShimTool, args: &[String]) -> Option<i32> {
    let shim_dir = own_shim_dir();
    let real = resolve_real(tool.program(), shim_dir.as_deref());

    // Recursion guard: a nested invocation is a pure passthrough — no runner
    // contact, just run the real tool with the guard still set.
    if env::var(GUARD_ENV).ok().as_deref() == Some("1") {
        return exec_real(&real, tool.program(), args);
    }

    // Classify. Non-install (or unparseable) ⇒ zero-overhead passthrough.
    let classification = classify::classify_tool(tool, args);
    let (packages, dev, lockfile_sync) = match &classification {
        Classification::Install {
            packages,
            dev,
            lockfile_sync,
            ..
        } => (packages.clone(), *dev, *lockfile_sync),
        Classification::Passthrough => {
            return exec_real(&real, tool.program(), args);
        }
    };

    let port: Option<u16> = env::var(PORT_ENV).ok().and_then(|p| p.trim().parse().ok());
    let mode = InterceptMode::parse(&env::var(MODE_ENV).unwrap_or_default());
    let override_on = env::var(OVERRIDE_ENV).ok().as_deref() == Some("1");

    // ---- pre-call (best-effort, 3s connect timeout) ----------------------
    // Absent port / connect-refused / non-2xx / malformed ⇒ ONE stderr line +
    // passthrough. The "log at most once" is trivially per-invocation: a single
    // stub process makes at most one pre-call.
    let mut correlation_id: Option<String> = None;
    let mut escalate = false;
    let mut risk_factors = String::new();
    if let Some(port) = port {
        let body = pre_call_body(tool.wire_pm().as_str(), &packages, dev, override_on);
        match http_post(port, "/install-effects/run", &body) {
            Ok(resp) => {
                correlation_id = extract_correlation_id(&resp);
                escalate = resp.contains("\"gate\":\"escalate\"")
                    || resp.contains("\"gate\": \"escalate\"");
                risk_factors = extract_risk_factors(&resp);
            }
            Err(_) => {
                log_unavailable_once();
                return exec_real(&real, tool.program(), args);
            }
        }
    } else {
        // No port injected ⇒ interception unavailable. One stderr line, then run.
        log_unavailable_once();
        return exec_real(&real, tool.program(), args);
    }

    // ---- gate decision (lib-crate truth table) ---------------------------
    if should_block(
        mode,
        escalate,
        override_on,
        lockfile_sync,
        tool.never_gate(),
    ) {
        print_blocked(tool.program(), args, &packages, &risk_factors);
        return Some(1);
    }

    // ---- run the real tool (guarded child, inherited stdio) --------------
    let real_code = exec_real_child(&real, tool.program(), args);

    // ---- post-call (best-effort; never alters the agent-visible code) ----
    if let (Some(cid), Some(port)) = (correlation_id, port) {
        let body = format!(
            "{{\"correlation_id\":\"{}\",\"install_exit_code\":{}}}",
            json_escape(&cid),
            real_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "null".into())
        );
        // Post failures are silently ignored (observe is best-effort).
        let _ = http_post(port, "/install-effects/observe-verify", &body);
    }

    real_code
}

/// Build the pre-call JSON body. `$PWD` is the stub's own cwd (the install's
/// working dir), matching the bash shim.
fn pre_call_body(
    wire_pm: &str,
    packages: &[PackageSpecInput],
    dev: bool,
    override_on: bool,
) -> String {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pkgs_json = packages
        .iter()
        .map(|p| match &p.version_req {
            Some(v) => format!(
                "{{\"name\":\"{}\",\"version_req\":\"{}\"}}",
                json_escape(&p.name),
                json_escape(v)
            ),
            None => format!("{{\"name\":\"{}\"}}", json_escape(&p.name)),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"mode\":\"intercept\",\"repo_path\":\"{}\",\"package_manager\":\"{}\",\"packages\":[{}],\"dev\":{},\"override_escalation\":{}}}",
        json_escape(&cwd),
        wire_pm,
        pkgs_json,
        dev,
        override_on
    )
}

/// Minimal JSON string-escape (quotes + backslashes + control chars) — enough
/// for paths and package names. Dependency-free.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Best-effort grab of `"correlation_id":"…"` from a JSON response without a
/// JSON parser (mirrors the bash `grep`). `None` if absent.
fn extract_correlation_id(resp: &str) -> Option<String> {
    let key = "\"correlation_id\"";
    let i = resp.find(key)? + key.len();
    let rest = &resp[i..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let after_q1 = &after[q1 + 1..];
    let q2 = after_q1.find('"')?;
    Some(after_q1[..q2].to_string())
}

/// Best-effort display-only join of the `risk_factors` string array. Not
/// load-bearing (only the gate field is) — mirrors the bash extraction.
fn extract_risk_factors(resp: &str) -> String {
    let key = "\"risk_factors\"";
    let Some(i) = resp.find(key) else {
        return String::new();
    };
    let rest = &resp[i + key.len()..];
    let Some(open) = rest.find('[') else {
        return String::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return String::new();
    };
    let inner = &rest[open + 1..open + close];
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Print the A4 blocked UX to stderr (verbatim with the bash shim's leading
/// line + override hint). Never panics.
fn print_blocked(tool: &str, args: &[String], packages: &[PackageSpecInput], risk_factors: &str) {
    let pkgs_disp = packages
        .iter()
        .map(|p| match &p.version_req {
            Some(v) => format!("{}@{}", p.name, v),
            None => p.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "\u{26a0} qontinui: this install is predicted RISKY and was blocked."
    );
    let _ = writeln!(err, "  package(s): {pkgs_disp}");
    let _ = writeln!(err, "  risks: {risk_factors}");
    let _ = writeln!(
        err,
        "  To override (record an audited +overridden install), re-run with:"
    );
    let _ = writeln!(
        err,
        "      QONTINUI_INSTALL_OVERRIDE=1 {tool} {}",
        args.join(" ")
    );
}

/// One-time stderr notice that interception is unavailable (pre-call failure
/// path only — never on the post-call). Per-invocation by construction.
fn log_unavailable_once() {
    let _ = writeln!(
        std::io::stderr(),
        "qontinui: install interception unavailable — running normally"
    );
}

/// This stub's own directory (so the real-tool scan can skip it). Prefer the
/// env hint the materializer sets; fall back to `current_exe().parent()`.
fn own_shim_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var(SHIM_DIR_ENV) {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Resolve the REAL tool: the first PATH entry (excluding `shim_dir`) that holds
/// `<name>.exe`, then `<name>.cmd`, then bare `<name>`. `None` ⇒ let the OS PATH
/// resolution handle it at exec time (last-resort fail-open).
fn resolve_real(name: &str, shim_dir: Option<&Path>) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    let own = shim_dir.map(normalize_dir);
    for dir in env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Some(own) = &own {
            if normalize_dir(&dir) == *own {
                continue; // skip our own dir — never recurse
            }
        }
        // Windows real tools: cargo/pip = .exe; npm-family = .cmd. Prefer .exe.
        for cand_name in candidate_names(name) {
            let cand = dir.join(&cand_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Candidate filenames to probe for the real tool, in resolution-preference
/// order: `<name>.exe`, `<name>.cmd`, then bare `<name>` (Windows) / bare only
/// (Unix).
fn candidate_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    }
}

/// Normalize a dir path for comparison (canonicalize if possible; else a
/// trailing-separator-stripped form).
fn normalize_dir(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| {
        let s = p.to_string_lossy();
        PathBuf::from(s.trim_end_matches(['/', '\\']))
    })
}

/// Exec the real tool, REPLACING this process where possible (Unix) or as a
/// guarded child whose status we propagate (Windows / fallback). Used for the
/// passthrough paths (no post-call needed). Sets the recursion guard.
fn exec_real(real: &Option<PathBuf>, name: &str, args: &[String]) -> Option<i32> {
    exec_real_child(real, name, args)
}

/// Run the real tool as a guarded child inheriting stdio; return its exit code.
/// (We need the code for the post-call, so we don't `exec`-replace even on
/// Unix.) A spawn failure fails open to a non-fatal surrogate code so the shell
/// is never bricked.
fn exec_real_child(real: &Option<PathBuf>, name: &str, args: &[String]) -> Option<i32> {
    let mut cmd = match real {
        Some(p) => Command::new(p),
        // No resolved real tool: dispatch by name and let the OS PATH find it.
        // (Our own dir is still ahead on PATH, but the guard short-circuits a
        // re-entry to a pure passthrough — no infinite loop.)
        None => Command::new(name),
    };
    cmd.args(args)
        .env(GUARD_ENV, "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match cmd.status() {
        Ok(st) => st.code(),
        Err(_) => Some(127), // command-not-found surrogate; never panic
    }
}

// ===========================================================================
// Minimal dependency-free HTTP/1.1 POST (3s connect timeout). The runner loop-
// back is plain HTTP on 127.0.0.1; a ~40-line std::net client is more robust
// for a fail-open stub than pulling in reqwest's async stack. Returns the
// response BODY on a 2xx; `Err` on connect failure / non-2xx / timeout / any IO
// error — all of which the caller treats as "interception unavailable".
// ===========================================================================

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RW_TIMEOUT: Duration = Duration::from_secs(20);

fn http_post(port: u16, path: &str, body: &str) -> Result<String, String> {
    let addr = format!("127.0.0.1:{port}");
    let sockaddr = addr
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no addr".to_string())?;
    let mut stream =
        TcpStream::connect_timeout(&sockaddr, CONNECT_TIMEOUT).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(RW_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(RW_TIMEOUT))
        .map_err(|e| e.to_string())?;

    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw).into_owned();

    // Parse the status line.
    let status_ok = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .map(|c| (200..300).contains(&c))
        .unwrap_or(false);
    if !status_ok {
        return Err(format!("non-2xx: {}", text.lines().next().unwrap_or("")));
    }
    // Body is after the first blank line.
    let body_start = text
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| text.find("\n\n").map(|i| i + 2));
    Ok(match body_start {
        Some(i) => text[i..].to_string(),
        None => String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_tool_from_argv0_variants() {
        // Plain names.
        assert_eq!(detect_tool(Some("cargo")), Some(ShimTool::Cargo));
        assert_eq!(detect_tool(Some("pip")), Some(ShimTool::Pip));
        assert_eq!(detect_tool(Some("pip3")), Some(ShimTool::Pip3));
        assert_eq!(detect_tool(Some("npm")), Some(ShimTool::Npm));
        // With .exe extension (Windows materialized name).
        assert_eq!(detect_tool(Some("cargo.exe")), Some(ShimTool::Cargo));
        assert_eq!(detect_tool(Some("pip3.exe")), Some(ShimTool::Pip3));
        // Full path + extension + mixed case.
        assert_eq!(
            detect_tool(Some("C:\\tmp\\qontinui-shim-x\\Cargo.EXE")),
            Some(ShimTool::Cargo)
        );
        assert_eq!(
            detect_tool(Some("/tmp/qontinui-shim-x/pip")),
            Some(ShimTool::Pip)
        );
        // Unknown / absent.
        assert_eq!(detect_tool(Some("rustc")), None);
        assert_eq!(detect_tool(Some("")), None);
        assert_eq!(detect_tool(None), None);
    }

    #[test]
    fn code_to_u8_preserves_zero_and_nonzero() {
        assert_eq!(code_to_u8(Some(0)), 0);
        assert_eq!(code_to_u8(Some(1)), 1);
        assert_eq!(code_to_u8(Some(2)), 2);
        // A code whose low byte is 0 but is non-zero must NOT collapse to 0.
        assert_eq!(code_to_u8(Some(256)), 1);
        // Signal-killed (None) → 1.
        assert_eq!(code_to_u8(None), 1);
    }

    #[test]
    fn json_escape_handles_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), "a\\\"b\\\\c");
        assert_eq!(json_escape("C:\\path\\to"), "C:\\\\path\\\\to");
        assert_eq!(json_escape("plain"), "plain");
    }

    #[test]
    fn extract_correlation_id_finds_uuid() {
        let resp = r#"{"correlation_id":"11111111-2222-3333-4444-555555555555","gate":"proceed"}"#;
        assert_eq!(
            extract_correlation_id(resp).as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(extract_correlation_id("{}"), None);
    }

    #[test]
    fn extract_risk_factors_joins_array() {
        let resp = r#"{"risk_factors":["major version bump","high-severity advisory"]}"#;
        assert_eq!(
            extract_risk_factors(resp),
            "major version bump; high-severity advisory"
        );
        assert_eq!(extract_risk_factors(r#"{"risk_factors":[]}"#), "");
        assert_eq!(extract_risk_factors("{}"), "");
    }

    #[test]
    fn pre_call_body_shapes_packages_and_override() {
        let pkgs = vec![
            PackageSpecInput {
                name: "serde".into(),
                version_req: Some("1.0".into()),
            },
            PackageSpecInput {
                name: "anyhow".into(),
                version_req: None,
            },
        ];
        let body = pre_call_body("cargo", &pkgs, true, false);
        assert!(body.contains("\"mode\":\"intercept\""));
        assert!(body.contains("\"package_manager\":\"cargo\""));
        assert!(body.contains("\"name\":\"serde\",\"version_req\":\"1.0\""));
        assert!(body.contains("\"name\":\"anyhow\"}"));
        assert!(body.contains("\"dev\":true"));
        assert!(body.contains("\"override_escalation\":false"));
        // Override flips the flag.
        let body2 = pre_call_body("pip", &[], false, true);
        assert!(body2.contains("\"override_escalation\":true"));
        assert!(body2.contains("\"packages\":[]"));
    }

    #[test]
    fn resolve_real_skips_own_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("shim");
        let realbin = tmp.path().join("realbin");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&realbin).unwrap();
        // Put a fake "cargo" in BOTH dirs; resolution must skip the shim dir.
        let exe = if cfg!(windows) { "cargo.exe" } else { "cargo" };
        std::fs::write(shim.join(exe), b"x").unwrap();
        let real_path = realbin.join(exe);
        std::fs::write(&real_path, b"x").unwrap();

        let saved = env::var_os("PATH");
        let sep = if cfg!(windows) { ";" } else { ":" };
        env::set_var(
            "PATH",
            format!("{}{sep}{}", shim.display(), realbin.display()),
        );
        let got = resolve_real("cargo", Some(shim.as_path()));
        if let Some(p) = saved {
            env::set_var("PATH", p);
        }
        let got = got.expect("resolve real cargo");
        assert_eq!(
            normalize_dir(got.parent().unwrap()),
            normalize_dir(&realbin),
            "must resolve the real tool OUTSIDE the shim dir"
        );
    }
}
