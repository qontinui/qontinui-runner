//! Shim-bin **materializer** — writes the per-terminal PATH-shim scripts into a
//! temp bin dir at terminal spawn, and computes the env-seam mutations the
//! terminal session applies just before the PTY child spawns.
//!
//! Per the Windows shadowing policy (plan §6): the npm-family tools (`npm`,
//! `npx`, `pnpm`, `yarn`) ship as `.cmd` on Windows, so the materializer writes
//! BOTH a `<name>.cmd` (shadows the real `.cmd` under PowerShell/cmd `PATHEXT`
//! order) AND an extensionless `<name>` script (Git Bash resolves the
//! extensionless file first). The `.exe`-shipped tools (`cargo`, `pip`, `pip3`)
//! cannot be shadowed by a `.cmd` (`.EXE` precedes `.CMD` in PATHEXT), so for
//! those three the materializer ALSO copies the compiled `qontinui-shim` stub
//! binary into the shim dir as `<name>.exe` (Phase 4 — [`copy_exe_stub`]), which
//! DOES win under PowerShell/cmd; if the stub is absent (dev builds) it falls
//! back to scripts-only with a debug log (fail-open), and under Git Bash the
//! extensionless shim still intercepts them. On Unix a single
//! extensionless shell script per tool suffices. The script payloads are
//! `include_str!`'d (precedent: `terminal/session.rs` shell-integration scripts)
//! and have their `@@…@@` placeholders substituted from the [`super::classify`]
//! verb tables — keeping the scripts and the Rust classifier on one source of
//! truth (seven tools: `npm`, `pnpm`, `yarn`, `npx`, `cargo`, `pip`, `pip3`).
//!
//! Master flag: nothing here runs unless `QONTINUI_INSTALL_INTERCEPT_ENABLED`
//! is set (the seam checks [`intercept_enabled`] first). When off, no shim dir
//! is created and the terminal env is byte-identical to today.

use std::path::{Path, PathBuf};

use super::classify::{self, ShimTool};
use super::gate::InterceptMode;

/// Bash shim template (extensionless `npm` on Windows-Git-Bash + Unix).
const SHIM_BASH: &str = include_str!("../../../resources/intercept/shim.bash");
/// `.cmd` shim template (Windows cmd/PowerShell shadow).
#[cfg(target_os = "windows")]
const SHIM_CMD: &str = include_str!("../../../resources/intercept/shim.cmd");

/// Always-on session-restore IDENTITY shim template (bash / Git-Bash / Unix).
/// Wraps `claude`/`gemini` to append `--session-id $QONTINUI_PINNED_SESSION_ID`.
const IDENTITY_SHIM_BASH: &str = include_str!("../../../resources/intercept/identity_shim.bash");
/// Always-on identity shim template (`.cmd` Windows cmd/PowerShell shadow).
#[cfg(target_os = "windows")]
const IDENTITY_SHIM_CMD: &str = include_str!("../../../resources/intercept/identity_shim.cmd");

/// Always-on CODEX identity shim template (bash / Git-Bash / Unix). DISTINCT
/// from [`IDENTITY_SHIM_BASH`]: codex has NO `--session-id` flag, so this
/// wrapper NEVER mutates the user's argv — it only SIGNALS the runner (POST
/// `/control/session-open` with NO `session_id`) to start the read-back capture,
/// then execs the real codex with the args unchanged.
const CODEX_IDENTITY_SHIM_BASH: &str =
    include_str!("../../../resources/intercept/codex_identity_shim.bash");
/// Always-on CODEX identity shim template (`.cmd` Windows cmd/PowerShell shadow).
#[cfg(target_os = "windows")]
const CODEX_IDENTITY_SHIM_CMD: &str =
    include_str!("../../../resources/intercept/codex_identity_shim.cmd");

/// The provider id whose identity rides a READ-BACK channel (no `--session-id`
/// pin). The materializer selects the codex-specific wrapper template for this
/// tool; every other [`IDENTITY_TOOLS`] entry gets the pinning template.
pub const CODEX_TOOL: &str = "codex";

/// The always-on identity tool family (plan §3b). These shims are ALWAYS
/// materialized for every terminal regardless of the install-intercept master
/// flag, so the out-of-box session-restore guarantee never rides a default-dark
/// flag. `claude` is provider #1, `gemini` #2, `codex` #3 (read-back — its
/// wrapper never appends `--session-id`).
pub const IDENTITY_TOOLS: &[&str] = &["claude", "gemini", CODEX_TOOL];

/// Filename prefix for a per-terminal IDENTITY shim dir
/// (`qontinui-identity-<terminal_id>`). Distinct from [`SHIM_DIR_PREFIX`] so the
/// always-on identity family has its own lifecycle (it is materialized even when
/// install interception is off), while [`sweep_stale`]/[`cleanup`] reap BOTH.
pub const IDENTITY_DIR_PREFIX: &str = "qontinui-identity-";

/// Env var carrying the per-PTY terminal id the identity shim echoes back when
/// it confirms a session (plan §3b). Always injected at spawn.
pub const TERMINAL_ID_ENV: &str = "QONTINUI_TERMINAL_ID";
/// Env var carrying the runner-pre-generated session UUID the identity shim
/// pins via `--session-id` (plan §3b "Determinism mechanism"). Always injected.
pub const PINNED_SESSION_ID_ENV: &str = "QONTINUI_PINNED_SESSION_ID";

/// The shim tools materialized per terminal. Phase 2 ships all seven
/// (`npm`, `pnpm`, `yarn`, `npx`, `cargo`, `pip`, `pip3`) — the single source of
/// truth is [`classify::SHIM_TOOLS`].
pub const SHIM_TOOLS: &[ShimTool] = classify::SHIM_TOOLS;

/// The master enable flag. Default OFF — interception ships dark. When unset,
/// the seam injects nothing and the shim dir is never created.
pub const ENABLE_FLAG: &str = "QONTINUI_INSTALL_INTERCEPT_ENABLED";
/// Env var carrying the bound runner API port the shim loops back to.
pub const PORT_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_PORT";
/// Env var carrying the interception mode (`observe` in Phase 1/2, `gate` in
/// Phase 3).
pub const MODE_ENV: &str = "QONTINUI_INSTALL_INTERCEPT_MODE";

/// Filename prefix for a per-terminal shim dir (`qontinui-shim-<terminal_id>`).
pub const SHIM_DIR_PREFIX: &str = "qontinui-shim-";

/// Age beyond which an orphaned per-terminal shim dir is swept at materialize
/// time (plan §4 Phase 4 cleanup): 24h. A live terminal re-materializes per
/// spawn, so a dir older than this is from a session that never cleaned up.
pub const STALE_SHIM_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Cap on how many stale dirs a single sweep removes — keeps the best-effort GC
/// O(cap) so a pathological temp dir never stalls a terminal spawn.
const STALE_SWEEP_CAP: usize = 256;

/// Is interception enabled? Reads the master flag from the runner's OWN env
/// (the seam decision fn). Truthy = `1`/`true`/`yes` (case-insensitive).
pub fn intercept_enabled() -> bool {
    enabled_from(std::env::var(ENABLE_FLAG).ok().as_deref())
}

/// Pure form of [`intercept_enabled`] for tests.
pub fn enabled_from(val: Option<&str>) -> bool {
    matches!(
        val.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Resolve the interception MODE the terminals get from the runner's OWN env
/// (`QONTINUI_INSTALL_INTERCEPT_MODE`, plan §3 step 3). The runner process env
/// decides what every spawned terminal receives: default `observe` (Phase 1/2
/// posture — never blocks); `gate` enables Phase-3 gating. ANY other value
/// (typo, empty, absent) normalizes to `observe` — fail-open: a garbled mode
/// must never start blocking installs. The parse is [`InterceptMode::parse`].
pub fn resolve_mode() -> InterceptMode {
    mode_from(std::env::var(MODE_ENV).ok().as_deref())
}

/// Pure form of [`resolve_mode`] for tests. `None`/garbled ⇒ `Observe`.
pub fn mode_from(val: Option<&str>) -> InterceptMode {
    InterceptMode::parse(val.unwrap_or(""))
}

/// The mutations the terminal env-seam must apply to the PTY child command when
/// interception is on. Returned by [`prepare_for_terminal`] so the seam stays a
/// thin "apply these" step (and is unit-testable without a real PTY).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeamEnv {
    /// Absolute path of the per-terminal shim bin dir to PREPEND to `PATH`.
    pub shim_dir: PathBuf,
    /// The bound runner API port (`QONTINUI_INSTALL_INTERCEPT_PORT`).
    pub port: u16,
    /// The interception mode token the seam injects as
    /// `QONTINUI_INSTALL_INTERCEPT_MODE` — `observe` (Phase 1/2, never blocks)
    /// or `gate` (Phase 3). Resolved from the runner's OWN env via
    /// [`resolve_mode`] (plan §3 step 3); a garbled runner-env value fails open
    /// to `observe`.
    pub mode: String,
}

/// Build the new `PATH` value (shim dir prepended) given the child's current
/// `PATH`. Kept pure so the seam test asserts the prepend without a PTY.
pub fn prepend_path(shim_dir: &Path, current_path: Option<&str>) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let dir = shim_dir.to_string_lossy();
    match current_path {
        Some(p) if !p.is_empty() => format!("{dir}{sep}{p}"),
        _ => dir.to_string(),
    }
}

/// Render a shim script payload for `tool` by substituting the `@@…@@`
/// placeholders. `body` is one of the `include_str!`'d templates. Pure.
///
/// Substitution keys (single source = [`classify`]):
/// - `@@TOOL@@`          — the shadowed program name (`npm`, `npx`, `pip3`, …).
/// - `@@PM_WIRE_NAME@@`  — the wire `package_manager` string the pre-call sends
///   (`npx`→`npm`, `pip3`→`pip`), since the producer only `parse_token`s the
///   coord enum tokens.
/// - `@@SHIM_DIR@@`      — this shim's own bin dir (skipped in the real-tool scan).
/// - `@@INSTALL_VERBS@@` — the verb-table for the verb-table PMs; for the
///   special-cased tools (npx/pip) the template branches on `@@TOOL@@` instead.
/// - `@@LOCKSYNC_VERBS@@`— verbs that are lockfile-sync by name (`npm ci`,
///   `cargo update`) — never gated even with args (A6).
fn render(body: &str, tool: ShimTool, shim_dir: &Path) -> String {
    let verbs = classify::install_verbs(tool.wire_pm()).join(" ");
    let locksync = classify::lockfile_sync_verbs(tool.wire_pm()).join(" ");
    // `@@NEVER_GATE@@` is the tool-level Phase-3 carve-out (npx, plan §3 step 6):
    // a never-gate tool is never blocked even on an escalate. It rides into the
    // shim's LOCAL gate decision so the script checks its own tool property, not
    // only the server verdict.
    let never_gate = if tool.never_gate() { "1" } else { "0" };
    body.replace("@@TOOL@@", tool.program())
        .replace("@@PM_WIRE_NAME@@", tool.wire_pm().as_str())
        .replace("@@SHIM_DIR@@", &shim_dir.to_string_lossy())
        .replace("@@INSTALL_VERBS@@", &verbs)
        .replace("@@LOCKSYNC_VERBS@@", &locksync)
        .replace("@@NEVER_GATE@@", never_gate)
}

/// Test-only: render a shim template for `tool`/`shim_dir` (exposes the private
/// [`render`] to the bash-smoke test in the parent module, which materializes a
/// single runnable npm shim).
#[cfg(test)]
pub fn render_for_test(body: &str, tool: ShimTool, shim_dir: &Path) -> String {
    render(body, tool, shim_dir)
}

/// Materialize the Phase-1 shims for `terminal_id` into a fresh per-terminal bin
/// dir and return the [`SeamEnv`] the env-seam applies. `port` is the BOUND
/// runner API port (plan §6 — NOT the bootstrap default). `base_dir` is where
/// the per-terminal dir is created (the system temp dir in prod; a tempdir in
/// tests).
///
/// On any IO failure the whole thing returns `None` (fail-open: a shim we
/// couldn't write must not break the terminal — the seam just injects nothing).
pub fn materialize(
    base_dir: &Path,
    terminal_id: &str,
    port: u16,
    mode: InterceptMode,
) -> Option<SeamEnv> {
    // Best-effort: sweep stale per-terminal shim dirs left by sessions that
    // closed without cleanup (crash / kill) before we add ours. Capped + cheap.
    sweep_stale(base_dir, STALE_SHIM_MAX_AGE);

    let shim_dir = base_dir.join(format!("{SHIM_DIR_PREFIX}{terminal_id}"));
    if let Err(e) = std::fs::create_dir_all(&shim_dir) {
        tracing::warn!(error = %e, dir = %shim_dir.display(), "install-intercept: shim dir create failed — interception off for this terminal");
        return None;
    }

    for &tool in SHIM_TOOLS {
        if let Err(e) = write_shims_for(&shim_dir, tool) {
            tracing::warn!(error = %e, tool = tool.program(), "install-intercept: shim write failed — interception off for this terminal");
            return None;
        }
    }

    Some(SeamEnv {
        shim_dir,
        port,
        // The mode the seam injects as QONTINUI_INSTALL_INTERCEPT_MODE — the
        // runner-env-resolved `observe`/`gate` (plan §3 step 3). The shim's gate
        // branch reads THIS value to decide whether to honor an escalate.
        mode: mode.as_str().to_string(),
    })
}

/// Materialize the ALWAYS-ON session-restore identity shims for `terminal_id`
/// into a fresh per-terminal identity bin dir and return its absolute path to
/// PREPEND to the child `PATH` (plan §3b). Unlike [`materialize`], this is NOT
/// gated by the install-intercept master flag — the out-of-box session-restore
/// guarantee applies to every user with zero setup.
///
/// The identity shims (`claude`, `gemini`) append
/// `--session-id $QONTINUI_PINNED_SESSION_ID` to the real provider argv (the
/// runner pre-generates that id per terminal and injects it as env). On any IO
/// failure returns `None` (fail-open: a shim we couldn't write must never break
/// the terminal — the seam just doesn't prepend the identity dir).
pub fn materialize_identity(base_dir: &Path, terminal_id: &str) -> Option<PathBuf> {
    let dir = base_dir.join(format!("{IDENTITY_DIR_PREFIX}{terminal_id}"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            error = %e,
            dir = %dir.display(),
            "session-restore: identity shim dir create failed — identity capture off for this terminal"
        );
        return None;
    }
    for &tool in IDENTITY_TOOLS {
        if let Err(e) = write_identity_shims_for(&dir, tool) {
            tracing::warn!(
                error = %e,
                tool,
                "session-restore: identity shim write failed — identity capture off for this terminal"
            );
            return None;
        }
    }
    Some(dir)
}

/// Render an identity shim template by substituting its `@@…@@` placeholders.
/// Pure. Keys: `@@TOOL@@` (provider program), `@@SHIM_DIR@@` (own dir, skipped
/// in the real-tool scan).
fn render_identity(body: &str, tool: &str, shim_dir: &Path) -> String {
    body.replace("@@TOOL@@", tool)
        .replace("@@SHIM_DIR@@", &shim_dir.to_string_lossy())
}

/// Test-only accessor for [`render_identity`] over a tool's selected bash
/// template (the pinning template for claude/gemini, the codex template for
/// codex — see [`identity_bash_template`]).
#[cfg(test)]
pub fn render_identity_for_test(tool: &str, shim_dir: &Path) -> String {
    render_identity(identity_bash_template(tool), tool, shim_dir)
}

/// Select the bash identity-shim TEMPLATE for `tool`. `codex` rides the
/// read-back channel (no `--session-id` flag exists), so it gets the distinct
/// [`CODEX_IDENTITY_SHIM_BASH`] which only signals the runner and execs codex
/// with the args UNCHANGED; every other identity tool (claude/gemini) gets the
/// pinning [`IDENTITY_SHIM_BASH`] that appends `--session-id`.
fn identity_bash_template(tool: &str) -> &'static str {
    if tool == CODEX_TOOL {
        CODEX_IDENTITY_SHIM_BASH
    } else {
        IDENTITY_SHIM_BASH
    }
}

/// Select the `.cmd` identity-shim TEMPLATE for `tool` (codex → read-back
/// wrapper; everything else → pinning wrapper). See [`identity_bash_template`].
#[cfg(target_os = "windows")]
fn identity_cmd_template(tool: &str) -> &'static str {
    if tool == CODEX_TOOL {
        CODEX_IDENTITY_SHIM_CMD
    } else {
        IDENTITY_SHIM_CMD
    }
}

/// Write the platform-appropriate identity shim file(s) for `tool`
/// (`claude`/`gemini`/`codex`) into `dir`. Mirrors [`write_shims_for`]: always
/// an extensionless script (Git Bash + Unix), plus a `.cmd` on Windows. No
/// `.exe` stub is needed — these providers ship as `.cmd`/scripts (not `.exe`),
/// so a `.cmd`/extensionless shim wins under both shells. The TEMPLATE is
/// selected per tool ([`identity_bash_template`] / [`identity_cmd_template`]):
/// claude/gemini get the pinning `--session-id` wrapper; codex gets the
/// read-back signal-only wrapper (no flag injection).
fn write_identity_shims_for(dir: &Path, tool: &str) -> std::io::Result<()> {
    let bash = render_identity(identity_bash_template(tool), tool, dir);
    let extensionless = dir.join(tool);
    std::fs::write(&extensionless, bash.as_bytes())?;
    set_executable(&extensionless)?;

    #[cfg(target_os = "windows")]
    {
        let cmd_body = render_identity(identity_cmd_template(tool), tool, dir);
        std::fs::write(dir.join(format!("{tool}.cmd")), cmd_body.as_bytes())?;
    }

    Ok(())
}

/// Write the platform-appropriate shim file(s) for `tool` into `shim_dir`.
fn write_shims_for(shim_dir: &Path, tool: ShimTool) -> std::io::Result<()> {
    let name = tool.program();

    // Extensionless script (Git Bash + Unix). Always written. This is the
    // primary surface for ALL seven tools — agent terminals predominantly use
    // Git Bash, which resolves the extensionless file first for every tool
    // family (no PATHEXT), so it wins for cargo/pip just as for npm.
    let bash = render(SHIM_BASH, tool, shim_dir);
    let extensionless = shim_dir.join(name);
    std::fs::write(&extensionless, bash.as_bytes())?;
    set_executable(&extensionless)?;

    // Windows: also write `<name>.cmd` to shadow the real tool under PATHEXT for
    // cmd/PowerShell.
    //   * npm / npx / pnpm / yarn ship as `<name>.cmd` → a `.cmd` shim WINS
    //     (PATHEXT `.CMD` order, no `.exe` to outrank it).
    //   * cargo / pip / pip3 ship as `<name>.exe` → a `.cmd` CANNOT shadow a
    //     `.exe` (`.EXE` precedes `.CMD` in PATHEXT). For those three we ALSO
    //     materialize a compiled `<name>.exe` stub (Phase 4 — see
    //     [`copy_exe_stub`]) which DOES win under PowerShell/cmd. If the stub
    //     binary is unavailable (dev builds where `qontinui-shim` wasn't built),
    //     we fall back to scripts-only with a debug log — fail-open. The `.cmd`
    //     is still written for every tool (harmless + fail-open) so a cmd user
    //     who fronts the shim dir on a `.cmd`-only PATH still gets coverage.
    #[cfg(target_os = "windows")]
    {
        let cmd_body = render(SHIM_CMD, tool, shim_dir);
        std::fs::write(shim_dir.join(format!("{name}.cmd")), cmd_body.as_bytes())?;
        // cargo/pip/pip3 need the compiled `.exe` to win over the real `.exe`.
        if exe_shadow_needed(tool) {
            copy_exe_stub(shim_dir, name);
        }
    }

    Ok(())
}

/// Whether `tool` ships on Windows as a `<name>.exe` and therefore needs the
/// compiled `<name>.exe` stub to shadow it (a `.cmd` cannot — plan §6). True for
/// `cargo`/`pip`/`pip3`; false for the npm-family `.cmd`-shipped tools.
#[cfg(target_os = "windows")]
pub fn exe_shadow_needed(tool: ShimTool) -> bool {
    matches!(tool, ShimTool::Cargo | ShimTool::Pip | ShimTool::Pip3)
}

/// Copy the compiled `qontinui-shim` stub (it sits next to the runner exe at
/// runtime — resolved via `current_exe().parent()`) into `shim_dir` as
/// `<name>.exe`. Best-effort: any failure (or an absent stub in a dev build) is
/// logged at debug and the materializer falls back to the scripts only —
/// fail-open, never breaks the terminal. The stub detects which tool it is from
/// `argv[0]`, so a single binary serves all three names.
#[cfg(target_os = "windows")]
fn copy_exe_stub(shim_dir: &Path, name: &str) {
    let stub = match locate_stub_exe() {
        Some(p) => p,
        None => {
            tracing::debug!(
                tool = name,
                "install-intercept: qontinui-shim.exe stub not found next to the runner exe \
                 — PowerShell/cmd shadow for this .exe-shipped tool is unavailable (scripts only)"
            );
            return;
        }
    };
    let dest = shim_dir.join(format!("{name}.exe"));
    if let Err(e) = std::fs::copy(&stub, &dest) {
        tracing::debug!(
            tool = name,
            error = %e,
            "install-intercept: failed to copy qontinui-shim.exe stub — scripts-only fallback"
        );
    }
}

/// Locate the `qontinui-shim` stub binary next to the runner exe. Returns the
/// first existing candidate or `None` (dev build / not packaged).
#[cfg(target_os = "windows")]
fn locate_stub_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for cand in ["qontinui-shim.exe", "qontinui_shim.exe"] {
        let p = dir.join(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Mark a materialized shim executable (Unix). No-op on Windows (resolution is
/// by extension, not the executable bit).
#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Remove a single terminal's shim dir (plan §4 Phase 4 cleanup). Called from
/// the terminal session teardown (`TerminalSession::close`) so the per-terminal
/// `qontinui-shim-<id>` dir does not leak after the PTY child is reaped.
/// Best-effort: a removal failure is logged at debug and ignored — cleanup must
/// never interfere with teardown, and the stale-sweep ([`sweep_stale`]) reaps
/// any leftover on the next materialize.
pub fn cleanup(base_dir: &Path, terminal_id: &str) {
    // Reap BOTH the install-intercept shim dir and the always-on identity shim
    // dir for this terminal.
    for prefix in [SHIM_DIR_PREFIX, IDENTITY_DIR_PREFIX] {
        let dir = base_dir.join(format!("{prefix}{terminal_id}"));
        if !dir.exists() {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::debug!(
                error = %e,
                dir = %dir.display(),
                "session/install shim dir cleanup failed (will be swept later)"
            );
        }
    }
}

/// Sweep orphaned per-terminal shim dirs older than `max_age` from `base_dir`
/// (plan §4 Phase 4). Best-effort + capped ([`STALE_SWEEP_CAP`]): a session that
/// crashed/was-killed without calling [`cleanup`] leaves a `qontinui-shim-*`
/// dir behind; this reaps it lazily at the next materialize. Any IO error
/// (unreadable dir, racing peer) is silently skipped — never fatal.
pub fn sweep_stale(base_dir: &Path, max_age: std::time::Duration) {
    let entries = match std::fs::read_dir(base_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if removed >= STALE_SWEEP_CAP {
            break;
        }
        let path = entry.path();
        // Only our own per-terminal dirs (install-intercept OR always-on
        // identity).
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(SHIM_DIR_PREFIX) || n.starts_with(IDENTITY_DIR_PREFIX))
            .unwrap_or(false);
        if !is_ours || !path.is_dir() {
            continue;
        }
        // Age by directory mtime; if unreadable, conservatively skip (don't
        // delete something we can't date).
        let aged_out = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .map(|age| age >= max_age)
            .unwrap_or(false);
        if aged_out && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::debug!(
            removed,
            base = %base_dir.display(),
            "install-intercept: swept stale shim dirs"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_flag_truthiness() {
        for v in ["1", "true", "TRUE", "Yes", "on", " on "] {
            assert!(enabled_from(Some(v)), "{v:?} should enable");
        }
        for v in ["0", "false", "no", "off", "", "  "] {
            assert!(!enabled_from(Some(v)), "{v:?} should NOT enable");
        }
        assert!(!enabled_from(None), "unset = disabled (ships dark)");
    }

    #[test]
    fn prepend_path_puts_shim_first() {
        let dir = Path::new("/tmp/qontinui-shim-abc");
        let sep = if cfg!(windows) { ";" } else { ":" };
        let got = prepend_path(dir, Some("/usr/bin:/bin"));
        assert!(got.starts_with(&dir.to_string_lossy().to_string()));
        assert!(got.contains(sep));
        assert!(got.ends_with("/usr/bin:/bin") || got.ends_with("/bin"));
    }

    #[test]
    fn prepend_path_empty_current() {
        let dir = Path::new("/tmp/shim");
        assert_eq!(prepend_path(dir, None), "/tmp/shim");
        assert_eq!(prepend_path(dir, Some("")), "/tmp/shim");
    }

    #[test]
    fn render_substitutes_all_placeholders() {
        let dir = Path::new("/tmp/qontinui-shim-xyz");
        let out = render(SHIM_BASH, ShimTool::Npm, dir);
        assert!(!out.contains("@@TOOL@@"));
        assert!(!out.contains("@@SHIM_DIR@@"));
        assert!(!out.contains("@@INSTALL_VERBS@@"));
        assert!(!out.contains("@@PM_WIRE_NAME@@"));
        assert!(!out.contains("@@LOCKSYNC_VERBS@@"));
        assert!(!out.contains("@@NEVER_GATE@@"));
        assert!(out.contains("TOOL=\"npm\""));
        assert!(out.contains("/tmp/qontinui-shim-xyz"));
        // The install-verb table came from the Rust classifier (single source).
        assert!(out.contains("install i add ci"));
    }

    #[test]
    fn render_each_pm_carries_its_verb_table_and_wire_name() {
        let dir = Path::new("/tmp/qontinui-shim-x");
        // npx wires to npm; pip3 wires to pip.
        let npx = render(SHIM_BASH, ShimTool::Npx, dir);
        assert!(npx.contains("TOOL=\"npx\""));
        assert!(npx.contains("PM_WIRE_NAME=\"npm\""), "npx wires to npm");
        assert!(npx.contains("NEVER_GATE=\"1\""), "npx is a never-gate tool");
        let pip3 = render(SHIM_BASH, ShimTool::Pip3, dir);
        assert!(pip3.contains("TOOL=\"pip3\""));
        assert!(pip3.contains("PM_WIRE_NAME=\"pip\""), "pip3 wires to pip");
        // npm honors the gate (not a never-gate tool).
        assert!(render(SHIM_BASH, ShimTool::Npm, dir).contains("NEVER_GATE=\"0\""));
        // cargo's verb table + lockfile-sync verb came from the Rust source.
        let cargo = render(SHIM_BASH, ShimTool::Cargo, dir);
        assert!(cargo.contains("INSTALL_VERBS=\"add update\""));
        assert!(cargo.contains("LOCKSYNC_VERBS=\"update\""));
        // pnpm + yarn verb tables.
        assert!(render(SHIM_BASH, ShimTool::Pnpm, dir).contains("INSTALL_VERBS=\"add install i\""));
        assert!(render(SHIM_BASH, ShimTool::Yarn, dir).contains("INSTALL_VERBS=\"add install\""));
    }

    #[test]
    fn mode_resolution_validates_and_fails_open_to_observe() {
        // gate enables gating; everything else (incl. unset/garbled) ⇒ observe.
        assert_eq!(mode_from(Some("gate")), InterceptMode::Gate);
        assert_eq!(mode_from(Some("GATE")), InterceptMode::Gate);
        assert_eq!(mode_from(Some("observe")), InterceptMode::Observe);
        assert_eq!(mode_from(Some("garbage")), InterceptMode::Observe);
        assert_eq!(mode_from(Some("")), InterceptMode::Observe);
        assert_eq!(mode_from(None), InterceptMode::Observe, "unset ⇒ observe");
    }

    #[test]
    fn materialize_mode_observe_vs_gate_lands_on_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let obs = materialize(tmp.path(), "t-obs", 1, InterceptMode::Observe).unwrap();
        assert_eq!(obs.mode, "observe");
        let gat = materialize(tmp.path(), "t-gate", 1, InterceptMode::Gate).unwrap();
        assert_eq!(gat.mode, "gate", "gate mode rides onto the seam");
    }

    #[test]
    fn rendered_bash_carries_gate_branch_and_a4_message_and_override_key() {
        // Phase 3: the gate branch, the verbatim A4 stderr text, the
        // override-env read, and the override_escalation JSON key must all be
        // present in the rendered bash (the script is the runtime gate, so the
        // template render test is the contract that the gating UX shipped).
        let dir = Path::new("/tmp/qontinui-shim-g");
        let out = render(SHIM_BASH, ShimTool::Npm, dir);
        // Mode gate honored.
        assert!(out.contains("QONTINUI_INSTALL_INTERCEPT_MODE"));
        assert!(out.contains("QONTINUI_INSTALL_OVERRIDE"));
        // The A4 UX (verbatim leading line + the override re-run hint).
        assert!(out.contains("this install is predicted RISKY and was blocked"));
        assert!(out.contains("QONTINUI_INSTALL_OVERRIDE=1"));
        // The override path flips override_escalation on the pre-call JSON.
        assert!(out.contains("override_escalation"));
        // The robust gate-field substring check the shim uses.
        assert!(
            out.contains("\\\"gate\\\":\\\"escalate\\\"") || out.contains("\"gate\":\"escalate\"")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn rendered_cmd_carries_gate_branch_and_message() {
        // The coarse .cmd variant also honors gate mode (findstr escalate +
        // simplified blocked message + override read).
        let dir = Path::new("C:\\tmp\\qontinui-shim-g");
        let out = render(SHIM_CMD, ShimTool::Npm, dir);
        assert!(out.contains("QONTINUI_INSTALL_INTERCEPT_MODE"));
        assert!(out.contains("QONTINUI_INSTALL_OVERRIDE"));
        assert!(out.contains("predicted RISKY"));
        assert!(out.contains("override_escalation"));
    }

    #[test]
    fn materialize_writes_all_seven_tools_and_returns_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let seam = materialize(tmp.path(), "term-123", 9876, InterceptMode::Observe)
            .expect("materialize ok");
        assert_eq!(seam.port, 9876);
        assert_eq!(seam.mode, "observe");
        for tool in ["npm", "pnpm", "yarn", "npx", "cargo", "pip", "pip3"] {
            let f = seam.shim_dir.join(tool);
            assert!(f.exists(), "extensionless {tool} shim must exist");
            let body = std::fs::read_to_string(&f).unwrap();
            assert!(body.contains(&format!("TOOL=\"{tool}\"")));
            // Windows also gets <tool>.cmd for each.
            #[cfg(target_os = "windows")]
            assert!(
                seam.shim_dir.join(format!("{tool}.cmd")).exists(),
                "{tool}.cmd must exist on Windows"
            );
            // Unix shim is executable.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&f).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{tool} shim must be executable");
            }
        }
    }

    #[test]
    fn materialize_identity_writes_claude_gemini_and_codex_always() {
        // The identity family is materialized with NO master-flag gate — it is
        // the out-of-box session-restore guarantee. Includes the read-back
        // provider `codex` (provider #3).
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path(), "term-id").expect("identity materialize ok");
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(IDENTITY_DIR_PREFIX),
            "identity dir uses its own prefix"
        );
        // codex MUST be in the always-on family.
        assert!(
            IDENTITY_TOOLS.contains(&CODEX_TOOL),
            "codex is an always-on identity tool"
        );
        for tool in IDENTITY_TOOLS {
            let f = dir.join(tool);
            assert!(f.exists(), "extensionless {tool} identity shim must exist");
            let body = std::fs::read_to_string(&f).unwrap();
            assert!(body.contains(&format!("TOOL=\"{tool}\"")));
            // Every identity shim respects the recursion guard and signals via
            // the control route.
            assert!(body.contains("QONTINUI_INSTALL_INTERCEPT_GUARD"));
            assert!(body.contains("/control/session-open"));

            if *tool == CODEX_TOOL {
                // The codex wrapper is the READ-BACK signal-only wrapper: it
                // must NOT pin/append a `--session-id` (codex has no such flag —
                // appending one would break it), and must NOT read the pinned-id
                // env. Identity is read back from the rollout file instead.
                assert!(
                    !body.contains("--session-id"),
                    "codex wrapper must NOT append --session-id (no such flag)"
                );
                assert!(
                    !body.contains("QONTINUI_PINNED_SESSION_ID"),
                    "codex wrapper does not pin an id"
                );
            } else {
                // claude/gemini get the pinning wrapper: it reads the
                // runner-injected pinned id and appends `--session-id`.
                assert!(body.contains("QONTINUI_PINNED_SESSION_ID"));
                assert!(body.contains("--session-id"));
            }
            #[cfg(target_os = "windows")]
            {
                let cmd_path = dir.join(format!("{tool}.cmd"));
                assert!(
                    cmd_path.exists(),
                    "{tool}.cmd identity shim must exist on Windows"
                );
                let cmd_body = std::fs::read_to_string(&cmd_path).unwrap();
                if *tool == CODEX_TOOL {
                    assert!(
                        !cmd_body.contains("--session-id"),
                        "codex .cmd wrapper must NOT append --session-id"
                    );
                } else {
                    assert!(
                        cmd_body.contains("--session-id"),
                        "{tool} .cmd wrapper appends --session-id"
                    );
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&f).unwrap().permissions().mode();
                assert_eq!(
                    mode & 0o111,
                    0o111,
                    "{tool} identity shim must be executable"
                );
            }
        }
    }

    #[test]
    fn render_identity_substitutes_placeholders() {
        let dir = Path::new("/tmp/qontinui-identity-x");
        let out = render_identity_for_test("claude", dir);
        assert!(!out.contains("@@TOOL@@"));
        assert!(!out.contains("@@SHIM_DIR@@"));
        assert!(out.contains("TOOL=\"claude\""));
        assert!(out.contains("/tmp/qontinui-identity-x"));
    }

    #[test]
    fn cleanup_and_sweep_cover_identity_dirs() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        // Materialize BOTH families for one terminal; cleanup must reap both.
        let install = materialize(tmp.path(), "term-x", 1, InterceptMode::Observe).unwrap();
        let identity = materialize_identity(tmp.path(), "term-x").unwrap();
        assert!(install.shim_dir.exists() && identity.exists());
        cleanup(tmp.path(), "term-x");
        assert!(!install.shim_dir.exists(), "install dir reaped");
        assert!(!identity.exists(), "identity dir reaped");

        // sweep_stale also reaps an orphaned identity dir at zero max-age.
        let orphan = materialize_identity(tmp.path(), "orphan").unwrap();
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(!orphan.exists(), "stale identity dir swept");
    }

    #[test]
    fn materialize_dirs_are_per_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let a = materialize(tmp.path(), "term-A", 1, InterceptMode::Observe).unwrap();
        let b = materialize(tmp.path(), "term-B", 1, InterceptMode::Observe).unwrap();
        assert_ne!(a.shim_dir, b.shim_dir, "each terminal gets its own bin dir");
    }

    #[test]
    fn cleanup_removes_the_terminal_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let seam = materialize(tmp.path(), "term-clean", 1, InterceptMode::Observe).unwrap();
        assert!(seam.shim_dir.exists(), "materialize created the shim dir");
        cleanup(tmp.path(), "term-clean");
        assert!(!seam.shim_dir.exists(), "cleanup removed the shim dir");
        // Idempotent: a second cleanup (dir already gone) is a no-op.
        cleanup(tmp.path(), "term-clean");
    }

    #[test]
    fn cleanup_unknown_terminal_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        // No dir for this id — must not panic / error.
        cleanup(tmp.path(), "never-materialized");
    }

    #[test]
    fn sweep_stale_removes_old_keeps_fresh() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();

        // A FRESH per-terminal dir (just materialized).
        let fresh = materialize(tmp.path(), "fresh", 1, InterceptMode::Observe).unwrap();

        // An OLD orphan dir: create it, then backdate its mtime via a manual
        // stale marker. We can't easily set mtime portably without filetime, so
        // assert the policy by age threshold: sweep with a ZERO max-age makes the
        // fresh dir itself "aged out", proving the predicate; and sweep with a
        // huge max-age keeps everything. (The mtime-based path is exercised by
        // the two boundary sweeps below.)
        let orphan = tmp.path().join(format!("{SHIM_DIR_PREFIX}orphan"));
        std::fs::create_dir_all(&orphan).unwrap();

        // Huge max-age: nothing is old enough → both survive.
        sweep_stale(tmp.path(), Duration::from_secs(10 * 365 * 24 * 3600));
        assert!(fresh.shim_dir.exists(), "fresh dir kept under huge max-age");
        assert!(orphan.exists(), "orphan kept under huge max-age");

        // Zero max-age: every shim dir is "aged out" → all swept.
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(
            !fresh.shim_dir.exists(),
            "zero max-age sweeps the fresh dir"
        );
        assert!(!orphan.exists(), "zero max-age sweeps the orphan");
    }

    #[test]
    fn sweep_stale_ignores_non_shim_dirs() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        // A foreign dir NOT matching our prefix must never be touched, even at
        // zero max-age.
        let foreign = tmp.path().join("some-other-tooldir");
        std::fs::create_dir_all(&foreign).unwrap();
        let foreign_file = tmp.path().join("a-loose-file.txt");
        std::fs::write(&foreign_file, b"keep me").unwrap();
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(foreign.exists(), "non-prefixed dir must be left alone");
        assert!(foreign_file.exists(), "loose files must be left alone");
    }

    #[test]
    fn sweep_stale_missing_base_dir_is_noop() {
        let missing = std::env::temp_dir().join("qontinui-nonexistent-sweep-base-zzz");
        // Must not panic when the base dir doesn't exist.
        sweep_stale(&missing, STALE_SHIM_MAX_AGE);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn exe_shadow_needed_only_for_exe_shipped_tools() {
        // cargo/pip/pip3 ship as .exe → need the compiled stub.
        assert!(exe_shadow_needed(ShimTool::Cargo));
        assert!(exe_shadow_needed(ShimTool::Pip));
        assert!(exe_shadow_needed(ShimTool::Pip3));
        // npm-family ship as .cmd → a .cmd shim already wins.
        assert!(!exe_shadow_needed(ShimTool::Npm));
        assert!(!exe_shadow_needed(ShimTool::Npx));
        assert!(!exe_shadow_needed(ShimTool::Pnpm));
        assert!(!exe_shadow_needed(ShimTool::Yarn));
    }
}
