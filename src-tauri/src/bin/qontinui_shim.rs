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
//!
//! ## IDENTITY mode (claude / gemini)
//! When `argv[0]`'s stem is `claude` or `gemini` this binary runs the always-on
//! session-restore IDENTITY straddle instead of the install straddle, mirroring
//! `resources/intercept/identity_shim.cmd`: resolve the REAL provider outside
//! our own dir(s), honor the recursion guard, deliver the claude `--settings`
//! hook, best-effort POST `/control/session-open`, and append
//! `--session-id $QONTINUI_PINNED_SESSION_ID` unless the user already chose a
//! session. It exists as a NATIVE exe because a `.cmd` batch shim is launched
//! via cmd.exe, which CANNOT accept multi-line (or cmd-metachar) arguments —
//! the runner's shell integration passes a multi-line `--append-system-prompt`,
//! so the `.cmd`-only identity shim broke EVERY pane `claude` launch on
//! 2026-07-03 ("The syntax of the command is incorrect."). Native argv has no
//! such re-quoting layer; arguments pass through byte-exact.

use std::env;
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
/// stub's directory even if `current_exe` is unavailable. NOTE: the seam may
/// point this at EITHER the install shim dir or the identity shim dir (they are
/// injected independently), so [`own_shim_dirs`] excludes BOTH this env dir AND
/// `current_exe().parent()` — the copied identity exe physically lives in the
/// identity dir, and failing to exclude it would let the stub resolve ITSELF as
/// the "real" tool (self-spawn loop).
const SHIM_DIR_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR";

/// Env var carrying the runner-pre-generated session UUID the identity shim
/// pins via `--session-id` (identity mode only).
const PINNED_SESSION_ENV: &str = "QONTINUI_PINNED_SESSION_ID";
/// Env var carrying the per-PTY terminal id echoed back in the identity
/// confirmation POST.
const TERMINAL_ID_ENV: &str = "QONTINUI_TERMINAL_ID";
/// Env var carrying the absolute path of the runner-materialized claude
/// `--settings` hook file (identity mode, tool==claude only).
const CLAUDE_HOOK_SETTINGS_ENV: &str = "QONTINUI_CLAUDE_HOOK_SETTINGS";

fn main() -> std::process::ExitCode {
    // argv[0] → which tool we are impersonating. Fail-open to passthrough on
    // anything we don't recognize.
    let raw_args: Vec<String> = env::args().collect();
    let argv0 = raw_args.first().cloned();
    // Args after argv[0] — exactly what the bash shim sees in "$@".
    let args: Vec<String> = raw_args.into_iter().skip(1).collect();

    // IDENTITY mode (claude/gemini) is checked FIRST — the identity family is
    // disjoint from the install family, and its detection stays local to this
    // bin (install classification via `ShimTool` is a separate concern).
    if let Some(tool) = detect_identity_tool(argv0.as_deref()) {
        return std::process::ExitCode::from(code_to_u8(run_identity(tool, &args)));
    }

    let tool = match detect_tool(argv0.as_deref()) {
        Some(t) => t,
        None => {
            // Unknown invocation name: nothing sensible to do. Exit 0 rather
            // than panic (the materializer only ever names us a known tool).
            return std::process::ExitCode::from(0);
        }
    };

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

/// Lower-cased file stem of an argv[0] string: path components split on BOTH
/// separators, trailing extension stripped. Shared by [`detect_tool`] and
/// [`detect_identity_tool`].
fn argv0_stem(argv0: &str) -> String {
    // Hand-split on BOTH separators — `Path::file_stem` does not treat `\` as a
    // separator on unix, so a Windows-style argv0 (`C:\…\Cargo.EXE`) would be
    // read as one filename and the stem would carry the whole path. Cross-OS CI
    // (the ubuntu leg) catches this; mirror the `repo_basename` fix.
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    // Strip a trailing extension (`.exe`/`.cmd`/…) — everything before the last
    // `.`, or the whole base if there is no `.`.
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    stem.to_ascii_lowercase()
}

/// Detect the impersonated [`ShimTool`] from `argv[0]` (the file stem, lower-
/// cased, extension stripped). `cargo.exe` → `Cargo`, `pip3` → `Pip3`, etc.
/// Pure + total (returns `None` for an unknown stem) so it is unit-testable
/// without running the exe.
pub fn detect_tool(argv0: Option<&str>) -> Option<ShimTool> {
    ShimTool::from_program(&argv0_stem(argv0?))
}

// ===========================================================================
// IDENTITY mode (claude / gemini) — the always-on session-restore straddle.
// Kept LOCAL to this bin (not `intercept_core::classify::ShimTool`): install
// classification is a separate concern. Mirrors
// `resources/intercept/identity_shim.cmd` semantics exactly, with the same
// fail-open invariants as the install straddle.
// ===========================================================================

/// The identity provider this exe impersonates (from `argv[0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityTool {
    Claude,
    Gemini,
}

impl IdentityTool {
    /// The provider program name (also the wire `provider` string).
    pub fn program(self) -> &'static str {
        match self {
            IdentityTool::Claude => "claude",
            IdentityTool::Gemini => "gemini",
        }
    }
}

/// Detect an identity provider from `argv[0]` (stem, case-insensitive).
/// `None` ⇒ not an identity invocation (fall through to the install path).
pub fn detect_identity_tool(argv0: Option<&str>) -> Option<IdentityTool> {
    match argv0_stem(argv0?).as_str() {
        "claude" => Some(IdentityTool::Claude),
        "gemini" => Some(IdentityTool::Gemini),
        _ => None,
    }
}

/// Did the user's argv already choose a session? Then the shim must NOT
/// double-pin. Exact token match, case-insensitive, mirroring
/// `identity_shim.cmd`; plus the `--session-id=…`/`--resume=…` inline forms the
/// bash identity shim also honors. A merely-prefixed token
/// (`--session-id-ish`) does NOT match.
pub fn user_chose_session(args: &[String]) -> bool {
    args.iter().any(|a| {
        let t = a.to_ascii_lowercase();
        matches!(
            t.as_str(),
            "--session-id" | "--resume" | "-r" | "resume" | "--continue" | "-c"
        ) || t.starts_with("--session-id=")
            || t.starts_with("--resume=")
    })
}

/// The claude SessionStart-hook `--settings` args: tool==claude AND the env
/// path is non-empty AND that file exists ⇒ `["--settings", <path>]` (two argv
/// entries — native args, no quoting games). Otherwise empty (fail-open).
pub fn identity_settings_args(tool: IdentityTool, settings_path: Option<&str>) -> Vec<String> {
    if tool != IdentityTool::Claude {
        return Vec::new();
    }
    match settings_path {
        Some(p) if !p.trim().is_empty() && Path::new(p).is_file() => {
            vec!["--settings".to_string(), p.to_string()]
        }
        _ => Vec::new(),
    }
}

/// Compose the final identity argv (everything after the program): the
/// ORIGINAL args byte-exact, then the `--settings` pair (claude hook), then
/// `--session-id <pinned>` ONLY when the user did not choose a session and a
/// pinned id exists. Pure, so passthrough purity (incl. multi-line args) is
/// unit-testable.
pub fn identity_argv(
    original: &[String],
    settings_args: &[String],
    pinned: Option<&str>,
    user_chose: bool,
) -> Vec<String> {
    let mut out: Vec<String> = original.to_vec();
    out.extend(settings_args.iter().cloned());
    if !user_chose {
        if let Some(p) = pinned {
            if !p.is_empty() {
                out.push("--session-id".to_string());
                out.push(p.to_string());
            }
        }
    }
    out
}

/// The identity straddle, fail-open at every step. Returns the exit code to
/// propagate. Mirrors `identity_shim.cmd`:
/// resolve real → guard passthrough → settings/user-chose scan → best-effort
/// confirmation POST → exec real with the composed argv.
fn run_identity(tool: IdentityTool, args: &[String]) -> Option<i32> {
    let own_dirs = own_shim_dirs();
    let real = resolve_real(tool.program(), &own_dirs);

    // Recursion guard: a nested invocation never re-pins — pure passthrough.
    if env::var(GUARD_ENV).ok().as_deref() == Some("1") {
        return exec_real(&real, tool.program(), args);
    }

    let pinned = env::var(PINNED_SESSION_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());
    let user_chose = user_chose_session(args);
    let settings = identity_settings_args(tool, env::var(CLAUDE_HOOK_SETTINGS_ENV).ok().as_deref());

    // Fire-and-forget DIAGNOSTIC beacon on EVERY (non-nested) invocation —
    // INCLUDING the don't-double-pin passthrough that sends no session-open —
    // mirroring the `.bash`/`.cmd` shim beacons, with `"surface":"exe"` so the
    // runner logs reveal WHICH shim surface actually ran (post-#696 this exe is
    // the primary Windows surface; the `.cmd` is fail-open fallback only).
    // Log-only on the runner side; strictly fail-open here: absent port /
    // connect refused / timeout (hard-capped at BEACON_TIMEOUT per phase) are
    // all swallowed and never block or delay the real tool exec.
    if let Some(port) = env::var(PORT_ENV)
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
    {
        let _ = http_post_with_timeouts(
            port,
            "/control/shim-beacon",
            &shim_beacon_body(
                tool.program(),
                user_chose,
                !settings.is_empty(),
                pinned.is_some(),
            ),
            BEACON_TIMEOUT,
            BEACON_TIMEOUT,
        );
    }

    // Best-effort confirmation/liveness POST (never load-bearing; the runner
    // already recorded the session authoritatively at spawn). All failures —
    // absent port, connect refused, non-2xx — are ignored.
    if !user_chose {
        if let (Some(pin), Some(port)) = (
            pinned.as_deref(),
            env::var(PORT_ENV)
                .ok()
                .and_then(|p| p.trim().parse::<u16>().ok()),
        ) {
            let _ = http_post(
                port,
                "/control/session-open",
                &session_open_body(pin, tool.program()),
            );
        }
    }

    let final_args = identity_argv(args, &settings, pinned.as_deref(), user_chose);
    exec_real(&real, tool.program(), &final_args)
}

/// JSON body for the `/control/shim-beacon` diagnostic POST. Same fields the
/// `.bash`/`.cmd` shim beacons report (`terminal_id`, `tool`, `event`, and the
/// `user_session_id=… settings=… pinned=…` detail line), plus
/// `"surface":"exe"` — the discriminator that tells the runner logs the NATIVE
/// exe stub fired (the script shims omit the field). Pure aside from the env
/// read, so the shape is unit-testable.
fn shim_beacon_body(
    provider: &str,
    user_chose: bool,
    settings_delivered: bool,
    pinned_present: bool,
) -> String {
    let terminal_id = env::var(TERMINAL_ID_ENV).unwrap_or_default();
    format!(
        "{{\"terminal_id\":\"{}\",\"tool\":\"{}\",\"event\":\"invoked\",\"surface\":\"exe\",\"detail\":\"user_session_id={} settings={} pinned={}\"}}",
        json_escape(&terminal_id),
        json_escape(provider),
        user_chose,
        settings_delivered,
        pinned_present
    )
}

/// JSON body for the identity confirmation POST. Every value is
/// [`json_escape`]d.
fn session_open_body(session_id: &str, provider: &str) -> String {
    let terminal_id = env::var(TERMINAL_ID_ENV).unwrap_or_default();
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{{\"terminal_id\":\"{}\",\"session_id\":\"{}\",\"source\":\"startup\",\"provider\":\"{}\",\"cwd\":\"{}\"}}",
        json_escape(&terminal_id),
        json_escape(session_id),
        json_escape(provider),
        json_escape(&cwd)
    )
}

/// The full straddle, fail-open at every step. Returns the exit code the stub
/// should propagate (the real tool's, or the spawn-failure surrogate).
fn run(tool: ShimTool, args: &[String]) -> Option<i32> {
    let own_dirs = own_shim_dirs();
    let real = resolve_real(tool.program(), &own_dirs);

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
    // DYNAMIC interception mode (P4): start from the spawn-time env MODE and
    // OVERRIDE it with the producer-returned `effective_mode` when present, so
    // the fleet policy governs this already-injected stub. Absent (old
    // runner/coord) ⇒ keep the env-derived mode (full back-compat). Parity with
    // the bash shim's `eff_mode="${resp_eff_mode:-$MODE}"`.
    let mut eff_mode = mode;
    if let Some(port) = port {
        let body = pre_call_body(tool.wire_pm().as_str(), &packages, dev, override_on);
        match http_post(port, "/install-effects/run", &body) {
            Ok(resp) => {
                correlation_id = extract_correlation_id(&resp);
                escalate = resp.contains("\"gate\":\"escalate\"")
                    || resp.contains("\"gate\": \"escalate\"");
                risk_factors = extract_risk_factors(&resp);
                if let Some(em) = extract_effective_mode(&resp) {
                    eff_mode = InterceptMode::parse(&em);
                }
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
    // Uses `eff_mode` (effective_mode-over-env), NOT the raw spawn-time `mode`.
    if should_block(
        eff_mode,
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

/// Best-effort grab of `"effective_mode":"…"` from the pre-call response
/// (P4 — the dynamic per-install interception mode). `None` when absent (an
/// old runner/coord that omits the field) so the caller keeps the spawn-time
/// env mode. Mirrors `extract_correlation_id` + the bash shim's grep.
fn extract_effective_mode(resp: &str) -> Option<String> {
    let key = "\"effective_mode\"";
    let i = resp.find(key)? + key.len();
    let rest = &resp[i..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let after_q1 = &after[q1 + 1..];
    let q2 = after_q1.find('"')?;
    let val = after_q1[..q2].to_string();
    if val.trim().is_empty() {
        None
    } else {
        Some(val)
    }
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

/// This stub's own directory candidates (so the real-tool scan can skip them):
/// `current_exe().parent()` PLUS the env hint the materializer sets. BOTH are
/// excluded because the seams inject two shim dirs (install + identity) and
/// `SHIM_DIR_ENV` only names one of them — but the exe physically lives in the
/// dir it was copied to, so `current_exe().parent()` always covers the copy the
/// OS actually resolved. Missing either exclusion could let the stub resolve
/// ITSELF as the "real" tool.
fn own_shim_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(p) = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(p);
    }
    if let Ok(d) = env::var(SHIM_DIR_ENV) {
        if !d.is_empty() {
            let p = PathBuf::from(d);
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    dirs
}

/// Resolve the REAL tool: the first PATH entry (excluding `own_dirs`) that holds
/// `<name>.exe`, then `<name>.cmd`, then bare `<name>`. `None` ⇒ let the OS PATH
/// resolution handle it at exec time (last-resort fail-open).
fn resolve_real(name: &str, own_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    let own: Vec<PathBuf> = own_dirs.iter().map(|d| normalize_dir(d)).collect();
    for dir in env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if own.contains(&normalize_dir(&dir)) {
            continue; // skip our own dir(s) — never recurse
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
/// Hard cap per phase (connect / read / write) for the diagnostic shim beacon —
/// deliberately tiny so a wedged loopback can never delay the real tool exec
/// noticeably. The beacon is log-only; any timeout is silently swallowed.
const BEACON_TIMEOUT: Duration = Duration::from_millis(500);

fn http_post(port: u16, path: &str, body: &str) -> Result<String, String> {
    http_post_with_timeouts(port, path, body, CONNECT_TIMEOUT, RW_TIMEOUT)
}

fn http_post_with_timeouts(
    port: u16,
    path: &str,
    body: &str,
    connect_timeout: Duration,
    rw_timeout: Duration,
) -> Result<String, String> {
    let addr = format!("127.0.0.1:{port}");
    let sockaddr = addr
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no addr".to_string())?;
    let mut stream =
        TcpStream::connect_timeout(&sockaddr, connect_timeout).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(rw_timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(rw_timeout))
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
    fn extract_effective_mode_reads_field_or_falls_back() {
        // Present + non-empty ⇒ Some(value), driving the dynamic override.
        let resp = r#"{"gate":"escalate","effective_mode":"gate"}"#;
        assert_eq!(extract_effective_mode(resp).as_deref(), Some("gate"));
        assert_eq!(
            extract_effective_mode(r#"{"effective_mode":"observe"}"#).as_deref(),
            Some("observe")
        );
        // Absent (old runner/coord) ⇒ None ⇒ caller keeps the env mode.
        assert_eq!(extract_effective_mode(r#"{"gate":"proceed"}"#), None);
        assert_eq!(extract_effective_mode("{}"), None);
        // Empty string ⇒ None (treated as absent, not a parseable mode).
        assert_eq!(extract_effective_mode(r#"{"effective_mode":""}"#), None);
        // Back-compat: an unrecognized value still parses (InterceptMode::parse
        // fails open to Observe), but the extractor surfaces it verbatim.
        assert_eq!(
            extract_effective_mode(r#"{"effective_mode":"bogus"}"#).as_deref(),
            Some("bogus")
        );
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
        let got = resolve_real("cargo", std::slice::from_ref(&shim));
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

    #[test]
    fn resolve_real_skips_all_own_dirs() {
        // The identity seam can put TWO of our dirs on PATH (identity + install)
        // while SHIM_DIR_ENV names only one — resolution must skip every own
        // dir, or the stub resolves itself.
        let tmp = tempfile::tempdir().unwrap();
        let identity = tmp.path().join("identity");
        let install = tmp.path().join("install");
        let realbin = tmp.path().join("realbin");
        for d in [&identity, &install, &realbin] {
            std::fs::create_dir_all(d).unwrap();
        }
        let exe = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        std::fs::write(identity.join(exe), b"x").unwrap();
        std::fs::write(install.join(exe), b"x").unwrap();
        std::fs::write(realbin.join(exe), b"x").unwrap();

        let saved = env::var_os("PATH");
        let sep = if cfg!(windows) { ";" } else { ":" };
        env::set_var(
            "PATH",
            format!(
                "{}{sep}{}{sep}{}",
                identity.display(),
                install.display(),
                realbin.display()
            ),
        );
        let got = resolve_real("claude", &[identity.clone(), install.clone()]);
        if let Some(p) = saved {
            env::set_var("PATH", p);
        }
        let got = got.expect("resolve real claude");
        assert_eq!(
            normalize_dir(got.parent().unwrap()),
            normalize_dir(&realbin),
            "must skip BOTH own dirs"
        );
    }

    // ---- IDENTITY mode -----------------------------------------------------

    #[test]
    fn detect_identity_tool_from_argv0_variants() {
        // Plain names.
        assert_eq!(
            detect_identity_tool(Some("claude")),
            Some(IdentityTool::Claude)
        );
        assert_eq!(
            detect_identity_tool(Some("gemini")),
            Some(IdentityTool::Gemini)
        );
        // Extension + case variants (Windows materialized names).
        assert_eq!(
            detect_identity_tool(Some("claude.exe")),
            Some(IdentityTool::Claude)
        );
        assert_eq!(
            detect_identity_tool(Some("CLAUDE.EXE")),
            Some(IdentityTool::Claude)
        );
        // Full paths, both separators.
        assert_eq!(
            detect_identity_tool(Some("C:\\Temp\\qontinui-identity-x\\Claude.EXE")),
            Some(IdentityTool::Claude)
        );
        assert_eq!(
            detect_identity_tool(Some("/tmp/qontinui-identity-x/gemini")),
            Some(IdentityTool::Gemini)
        );
        // Unknown stems fall through to the install path (or exit-0).
        assert_eq!(detect_identity_tool(Some("cargo.exe")), None);
        assert_eq!(detect_identity_tool(Some("claudette")), None);
        assert_eq!(detect_identity_tool(Some("")), None);
        assert_eq!(detect_identity_tool(None), None);
        // And identity names are NOT install tools.
        assert_eq!(detect_tool(Some("claude")), None);
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn user_chose_session_token_scan() {
        // Positive: every token, incl. case variants and any position.
        for tok in [
            "--session-id",
            "--SESSION-ID",
            "--resume",
            "--Resume",
            "-r",
            "-R",
            "resume",
            "RESUME",
            "--continue",
            "-c",
            "-C",
        ] {
            assert!(
                user_chose_session(&strs(&["-p", "hi", tok])),
                "{tok} must mark user-chose"
            );
        }
        // Inline `=` forms (bash-shim parity).
        assert!(user_chose_session(&strs(&["--session-id=abc"])));
        assert!(user_chose_session(&strs(&["--resume=abc"])));
        // Negative: token-exact — prefixes/lookalikes must NOT match.
        assert!(!user_chose_session(&strs(&["--session-id-ish"])));
        assert!(!user_chose_session(&strs(&["--session-identifier"])));
        assert!(!user_chose_session(&strs(&["--continued"])));
        assert!(!user_chose_session(&strs(&["-cc"])));
        assert!(!user_chose_session(&strs(&["--print", "resume the work"])));
        assert!(!user_chose_session(&strs(&[])));
        // A VALUE that merely contains a token string is a separate argv entry
        // in native-args land and DOES match only if it IS the token.
        assert!(!user_chose_session(&strs(&[
            "--append-system-prompt",
            "use --resume-like flows"
        ])));
    }

    #[test]
    fn identity_settings_args_only_for_claude_with_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        std::fs::write(&file, b"{}").unwrap();
        let path = file.to_string_lossy().into_owned();

        // claude + existing file ⇒ the two-entry pair.
        assert_eq!(
            identity_settings_args(IdentityTool::Claude, Some(&path)),
            vec!["--settings".to_string(), path.clone()]
        );
        // gemini never gets --settings.
        assert!(identity_settings_args(IdentityTool::Gemini, Some(&path)).is_empty());
        // Missing file / empty / unset ⇒ nothing (fail-open).
        let missing = tmp.path().join("nope.json").to_string_lossy().into_owned();
        assert!(identity_settings_args(IdentityTool::Claude, Some(&missing)).is_empty());
        assert!(identity_settings_args(IdentityTool::Claude, Some("")).is_empty());
        assert!(identity_settings_args(IdentityTool::Claude, Some("  ")).is_empty());
        assert!(identity_settings_args(IdentityTool::Claude, None).is_empty());
    }

    #[test]
    fn identity_argv_composition() {
        let multiline = "You are in a runner pane.\nSecond line \"quoted\" & <cmd|metachars>";
        let orig = strs(&["--append-system-prompt", multiline, "--version"]);
        let settings = strs(&["--settings", "C:\\hooks\\s.json"]);

        // Pinned + not user-chose ⇒ original (byte-exact) + settings + pin.
        let got = identity_argv(&orig, &settings, Some("pin-123"), false);
        assert_eq!(
            got,
            strs(&[
                "--append-system-prompt",
                multiline,
                "--version",
                "--settings",
                "C:\\hooks\\s.json",
                "--session-id",
                "pin-123",
            ])
        );
        // The multi-line arg passes through BYTE-EXACT (the 2026-07-03 P0: the
        // .cmd shim mangled/refused exactly this argument class).
        assert_eq!(got[1], multiline);

        // User chose ⇒ no pin appended, settings still delivered.
        assert_eq!(
            identity_argv(&orig, &settings, Some("pin-123"), true),
            [orig.clone(), settings.clone()].concat()
        );
        // No pinned id ⇒ no pin appended.
        assert_eq!(
            identity_argv(&orig, &settings, None, false),
            [orig.clone(), settings.clone()].concat()
        );
        assert_eq!(
            identity_argv(&orig, &settings, Some(""), false),
            [orig.clone(), settings.clone()].concat()
        );
        // No settings ⇒ original + pin only.
        assert_eq!(
            identity_argv(&orig, &[], Some("p"), false),
            [orig.clone(), strs(&["--session-id", "p"])].concat()
        );
    }

    #[test]
    fn shim_beacon_body_reports_surface_and_decision_flags() {
        let body = shim_beacon_body("claude", false, true, true);
        assert!(body.starts_with('{') && body.ends_with('}'));
        // The exe surface discriminator — the script shims omit this field, so
        // its presence is what tells the runner logs the NATIVE stub fired.
        assert!(body.contains("\"surface\":\"exe\""));
        assert!(body.contains("\"tool\":\"claude\""));
        assert!(body.contains("\"event\":\"invoked\""));
        // Detail line mirrors the .cmd/.bash beacons' key=value format.
        assert!(body.contains("\"detail\":\"user_session_id=false settings=true pinned=true\""));
        assert!(body.contains("\"terminal_id\":"));
        assert!(!body.contains('\n'));

        // Flag permutations land verbatim in the detail line.
        let body2 = shim_beacon_body("gemini", true, false, false);
        assert!(body2.contains("\"tool\":\"gemini\""));
        assert!(body2.contains("\"detail\":\"user_session_id=true settings=false pinned=false\""));
    }

    #[test]
    fn session_open_body_is_valid_json_shape() {
        let body = session_open_body("sid-1", "claude");
        assert!(body.starts_with('{') && body.ends_with('}'));
        assert!(body.contains("\"session_id\":\"sid-1\""));
        assert!(body.contains("\"source\":\"startup\""));
        assert!(body.contains("\"provider\":\"claude\""));
        assert!(body.contains("\"terminal_id\":"));
        // cwd is escaped (Windows backslashes must not produce bare `\`).
        assert!(body.contains("\"cwd\":\""));
        assert!(!body.contains('\n'));
    }

    /// END-TO-END regression for the 2026-07-03 P0: invoking the identity shim
    /// EXE as `claude.exe` with an argument containing an EMBEDDED NEWLINE must
    /// launch the real tool and pass every argument byte-exact (plus append the
    /// pinned `--session-id`). The `.cmd` shim died here with "The syntax of
    /// the command is incorrect."
    ///
    /// Ignored by default: it needs the built `qontinui-shim.exe`
    /// (`cargo build --bin qontinui-shim` first; override the location with
    /// `QONTINUI_SHIM_E2E_EXE`) and a working `rustc` to compile a tiny
    /// arg-recording fake provider.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a built qontinui-shim.exe + rustc; run explicitly"]
    fn e2e_identity_exe_passes_multiline_arg_byte_exact() {
        // Locate the built shim exe (target/debug/qontinui-shim.exe relative to
        // this test binary in target/debug/deps/).
        let shim_exe = env::var("QONTINUI_SHIM_E2E_EXE")
            .map(PathBuf::from)
            .ok()
            .filter(|p| p.is_file())
            .or_else(|| {
                let me = env::current_exe().ok()?;
                let debug = me.parent()?.parent()?;
                let p = debug.join("qontinui-shim.exe");
                p.is_file().then_some(p)
            })
            .expect("qontinui-shim.exe not found — build it or set QONTINUI_SHIM_E2E_EXE");

        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("identity");
        let realbin = tmp.path().join("realbin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&realbin).unwrap();
        std::fs::copy(&shim_exe, shim_dir.join("claude.exe")).unwrap();

        // Compile a tiny NATIVE fake provider that records its argv (0x1F-
        // separated, so embedded newlines survive) to the file named by
        // QONTINUI_E2E_ARGS_OUT. It must be a real exe: the whole point is that
        // no cmd.exe re-quoting layer sits between the shim and the provider.
        let helper_src = tmp.path().join("fake_claude.rs");
        std::fs::write(
            &helper_src,
            r#"fn main() {
                let out = std::env::var("QONTINUI_E2E_ARGS_OUT").unwrap();
                let args: Vec<String> = std::env::args().skip(1).collect();
                std::fs::write(out, args.join("\x1f")).unwrap();
            }"#,
        )
        .unwrap();
        let rustc = Command::new("rustc")
            .args([
                helper_src.to_str().unwrap(),
                "-o",
                realbin.join("claude.exe").to_str().unwrap(),
            ])
            .output()
            .expect("rustc must be runnable for this e2e test");
        assert!(
            rustc.status.success(),
            "helper compile failed: {}",
            String::from_utf8_lossy(&rustc.stderr)
        );

        let args_out = tmp.path().join("argv.txt");
        let multiline = "You are running inside a qontinui-runner terminal.\nline two\nline three";
        let sep = ";";
        let path = format!(
            "{}{sep}{}{sep}{}",
            shim_dir.display(),
            realbin.display(),
            env::var("PATH").unwrap_or_default()
        );
        let status = Command::new(shim_dir.join("claude.exe"))
            .args(["--append-system-prompt", multiline, "--version"])
            .env("PATH", &path)
            .env("QONTINUI_E2E_ARGS_OUT", &args_out)
            .env("QONTINUI_PINNED_SESSION_ID", "e2e-pin-1")
            .env_remove("QONTINUI_INSTALL_INTERCEPT_GUARD")
            .env_remove("QONTINUI_INSTALL_INTERCEPT_PORT")
            .env_remove("QONTINUI_INSTALL_INTERCEPT_SHIM_DIR")
            .env_remove("QONTINUI_CLAUDE_HOOK_SETTINGS")
            .status()
            .expect("shim exe must spawn");
        assert!(status.success(), "shim must propagate the fake's exit 0");

        let recorded = std::fs::read_to_string(&args_out).expect("fake provider must have run");
        let got: Vec<&str> = recorded.split('\x1f').collect();
        assert_eq!(
            got,
            vec![
                "--append-system-prompt",
                multiline,
                "--version",
                "--session-id",
                "e2e-pin-1",
            ],
            "argv must arrive byte-exact (multi-line arg intact) + pinned id"
        );
    }
}
