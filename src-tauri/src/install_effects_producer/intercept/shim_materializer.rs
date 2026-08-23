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

/// The always-on identity tool family (plan §3b). These shims are ALWAYS
/// materialized for every terminal regardless of the install-intercept master
/// flag, so the out-of-box session-restore guarantee never rides a default-dark
/// flag. `claude` is provider #1, `gemini` #2.
pub const IDENTITY_TOOLS: &[&str] = &["claude", "gemini"];

/// Filename prefix for the CONTENT-ADDRESSED identity shim dir
/// (`qontinui-identity-<build-tag>`, see [`identity_build_tag`]). Distinct from
/// [`SHIM_DIR_PREFIX`] so the always-on identity family has its own lifecycle
/// (it is materialized even when install interception is off).
///
/// Historically this was `qontinui-identity-<terminal_id>` — one dir, 4 script
/// writes, 2 `qontinui-shim.exe` copies and a `qontinui-pr.exe` hardlink PER
/// TERMINAL SPAWN, with no cache of any kind. The bytes are not
/// terminal-specific ([`render_identity`] substitutes only `@@TOOL@@` and
/// `@@SHIM_DIR@@`; the terminal id rides env vars), so the dir is now shared
/// across every terminal of one runner build. The prefix is unchanged so
/// [`sweep_stale`] still reaps the legacy per-terminal dirs.
pub const IDENTITY_DIR_PREFIX: &str = "qontinui-identity-";

/// Completion marker written LAST inside a materialized identity dir. Its
/// presence means "this dir is complete and matches its build tag"; its absence
/// means a materialize was interrupted and must be redone. Also carries the
/// dir's liveness timestamp (see [`refresh_identity_liveness`]).
const IDENTITY_MARKER: &str = ".qontinui-identity-build";

/// How stale the identity dir's liveness marker may get before an in-use dir
/// re-touches it. Far below [`STALE_SHIM_MAX_AGE`], so a dir in continuous use
/// is never swept, while a burst of spawns costs ZERO writes.
const IDENTITY_TOUCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Minimum spacing between orphan sweeps driven off the always-on identity
/// path. The sweep is a `read_dir` of the system temp dir — cheap, but not
/// something a 40-terminal restore should do 40 times.
const SWEEP_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// When the always-on identity path last swept orphans.
static LAST_SWEEP: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Serializes the (rare) materialization of the shared identity dir so a burst
/// of concurrent spawns writes it once rather than N times over each other.
static IDENTITY_MATERIALIZE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Materialize the ALWAYS-ON session-restore identity shims into the
/// CONTENT-ADDRESSED shim dir for this runner build and return its absolute path
/// to PREPEND to the child `PATH` (plan §3b). Unlike [`materialize`], this is
/// NOT gated by the install-intercept master flag — the out-of-box
/// session-restore guarantee applies to every user with zero setup.
///
/// The identity shims (`claude`, `gemini`) append
/// `--session-id $QONTINUI_PINNED_SESSION_ID` to the real provider argv (the
/// runner pre-generates that id per terminal and injects it as env). On any IO
/// failure returns `None` (fail-open: a shim we couldn't write must never break
/// the terminal — the seam just doesn't prepend the identity dir).
///
/// SHARED, not per-terminal (Phase 6, B2). The rendered bytes contain no
/// terminal-specific data — [`render_identity`] substitutes only `@@TOOL@@` and
/// `@@SHIM_DIR@@`, and the terminal id rides `QONTINUI_TERMINAL_ID` /
/// `QONTINUI_PINNED_SESSION_ID` in the child env — so one dir per runner build
/// serves every terminal. After the first materialize a spawn costs a single
/// `stat`. The three things that made per-terminal dirs load-bearing are
/// handled explicitly: teardown no longer deletes it ([`cleanup`]), the orphan
/// reaper now actually runs ([`maybe_sweep_stale`]), and staleness across a
/// runner update is an explicit check ([`identity_build_tag`]).
/// The BASENAME of this runner build's identity-shim directory — the ONE
/// definition of the `prefix + build tag` rule.
///
/// Every producer of that name goes through here or through [`identity_dir`]:
/// [`materialize_identity`], [`identity_dir_if_materialized`],
/// [`touch_identity_liveness`] and [`sweep_stale`]'s own-dir exclusion. The
/// claim used to be made in [`identity_dir`]'s doc while `touch_identity_liveness`
/// and `sweep_stale` each hand-rolled the same `format!` two hundred lines
/// apart — a stated invariant that was already false in its own file, which is
/// the exact second-copy hazard the config report exists to expose.
///
/// It is not cosmetic. If the prefix rule moved and the liveness toucher kept
/// the old spelling, it would silently stop refreshing the marker of the dir
/// every live pane has PATH-prepended, and [`sweep_stale`] would reap it out
/// from under them after [`STALE_SHIM_MAX_AGE`]; if the exclusion in
/// `sweep_stale` kept the old spelling, the sweep would reap this build's own
/// dir immediately.
fn identity_dir_name() -> String {
    format!("{IDENTITY_DIR_PREFIX}{}", identity_build_tag())
}

/// The identity-shim directory for THIS runner build — resolved, never
/// materialized. The name comes from [`identity_dir_name`], so nothing can
/// carry a second copy of the `prefix + build tag` rule.
pub fn identity_dir(base_dir: &Path) -> PathBuf {
    base_dir.join(identity_dir_name())
}

/// The identity-shim directory **iff it is already materialized for this
/// build** — a pure `stat` of the completion marker, with none of
/// [`materialize_identity`]'s effects: it writes no scripts, copies no exes,
/// does not refresh the liveness mtime, and never triggers the orphan sweep.
///
/// Exists for `config_report`'s G3 generation. G3 answers "what does a PTY
/// child spawned RIGHT NOW inherit?", and the single most consequential answer
/// is `PATH` — it decides which binary a child resolves, which is why an
/// operator asking "why does `cargo` in a runner pane hit the interception
/// shim?" must not be told the runner's own `PATH` is what a child gets. But
/// the seam that installs it MATERIALIZES, and a diagnostic that materializes
/// the thing it describes changes the answer by asking the question. So the
/// report asks this instead, and reports the prepend only when the spawn seam
/// would actually perform it — a `None` here is exactly the fail-open case
/// where the real seam prepends nothing either.
pub fn identity_dir_if_materialized(base_dir: &Path) -> Option<PathBuf> {
    let dir = identity_dir(base_dir);
    dir.join(IDENTITY_MARKER).is_file().then_some(dir)
}

pub fn materialize_identity(base_dir: &Path) -> Option<PathBuf> {
    let dir = identity_dir(base_dir);
    let marker = dir.join(IDENTITY_MARKER);

    // FAST PATH — a complete dir for THIS build already exists. Zero writes:
    // no scripts, no exe copies, no hardlink. This is what every spawn after the
    // first one costs.
    //
    // The orphan sweep runs AFTER this, never before it: liveness is refreshed
    // here, so sweeping first would let the very dir this call is about to
    // PATH-prepend be reaped in the same call — the "no new terminal for >24h,
    // then the operator opens one" case, where every live pane's PATH suddenly
    // names a directory being `remove_dir_all`'d. See [`maybe_sweep_stale`].
    if let Ok(md) = std::fs::metadata(&marker) {
        refresh_identity_liveness(&marker, &md);
        maybe_sweep_stale(base_dir);
        return Some(dir);
    }

    let _guard = IDENTITY_MATERIALIZE_LOCK.lock();
    // Re-check under the lock: a peer spawn may have just materialized it.
    if std::fs::metadata(&marker).is_ok() {
        return Some(dir);
    }

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
    // Also deliver the `qontinui-pr` session CLI (plan
    // qontinui-pr-credential-provisioning, Phase 2b) onto the same always-on
    // PATH dir. Best-effort, fail-open: an absent/uncopyable binary never
    // breaks the terminal — the identity dir still materializes.
    materialize_session_cli(&dir);

    // The marker goes LAST and is what makes the fast path safe: a crash
    // part-way through the writes above leaves no marker, so the next spawn
    // rewrites the dir rather than PATH-prepending a half-written one.
    if let Err(e) = std::fs::write(&marker, identity_build_tag().as_bytes()) {
        tracing::warn!(
            error = %e,
            dir = %dir.display(),
            "session-restore: identity marker write failed — the dir will be re-materialized next spawn"
        );
    }
    tracing::debug!(
        dir = %dir.display(),
        "session-restore: materialized the shared identity shim dir for this runner build"
    );
    // Q5(b): the orphan reaper used to be invoked ONLY from `materialize`, the
    // flag-gated install-intercept path — dark by default — so identity dirs
    // leaked forever. It now hangs off THIS always-on path (rate-limited), which
    // is the only one every terminal actually takes. Deliberately LAST: the dir
    // we just wrote must exist and be current before anything sweeps.
    maybe_sweep_stale(base_dir);
    Some(dir)
}

/// Re-touch THIS runner build's shared identity dir so the orphan sweeper can
/// see that it is in use, driven by the runner's liveness poll rather than by a
/// terminal spawn.
///
/// [`materialize_identity`] is the only other liveness source, and it only runs
/// when a NEW terminal spawns. On a long-lived automation box — 20 panes open,
/// no new pane opened for over [`STALE_SHIM_MAX_AGE`] — that made "in use" mean
/// "someone spawned recently", which is not the same thing at all: the dir every
/// live pane has PATH-prepended would age out and be reaped, and any `claude`
/// launched afterwards would resolve the real binary instead of the shim and
/// lose its `--session-id` pin, silently making that session unrestorable.
/// Called from the 45s poll whenever ANY terminal is alive, so "in use" is a
/// real signal. Costs one `stat` per tick and (at most) one marker write per
/// [`IDENTITY_TOUCH_INTERVAL`].
pub fn touch_identity_liveness(base_dir: &Path) {
    let marker = identity_dir(base_dir).join(IDENTITY_MARKER);
    if let Ok(md) = std::fs::metadata(&marker) {
        refresh_identity_liveness(&marker, &md);
    }
}

/// Content/version tag for the identity shim dir — the Q5(c) staleness check
/// that had NO equivalent before.
///
/// Nothing used to keep a materialized shim current across a runner update
/// except the accident that every single spawn re-rendered the scripts and
/// re-copied `qontinui-shim.exe` from `current_exe().parent()`. Caching removes
/// that accidental refresh, so staleness has to become explicit: the tag hashes
/// everything whose change would make a materialized dir wrong —
///
/// - the runner executable's path, size and mtime (a runner update changes at
///   least one, and it is the source of both copied binaries);
/// - the `qontinui-shim` stub's own size and mtime, and the `qontinui-pr`
///   session CLI's, since those are copied in verbatim;
/// - the shim TEMPLATE bodies and the tool list, so editing
///   `identity_shim.bash` invalidates every materialized dir.
///
/// A changed tag yields a different directory NAME, so the new dir is
/// materialized fresh and the old one is reaped by [`sweep_stale`] once nothing
/// has touched it for [`STALE_SHIM_MAX_AGE`] — no in-place overwrite of files a
/// live terminal may be executing. This is the hazard
/// [`materialize_persistent_identity`] still demonstrates: written once on an
/// operator click, no refresh path, stale `claude.exe` after every update.
fn identity_build_tag() -> &'static str {
    use std::hash::{Hash, Hasher};
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TAG.get_or_init(|| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Template + tool-set identity.
        IDENTITY_SHIM_BASH.hash(&mut h);
        #[cfg(target_os = "windows")]
        IDENTITY_SHIM_CMD.hash(&mut h);
        IDENTITY_TOOLS.hash(&mut h);
        SESSION_CLI_BIN.hash(&mut h);
        // Runner build identity + every binary copied into the dir.
        if let Ok(exe) = std::env::current_exe() {
            exe.to_string_lossy().hash(&mut h);
            hash_file_identity(&mut h, &exe);
            if let Some(parent) = exe.parent() {
                hash_file_identity(&mut h, &parent.join(SESSION_CLI_BIN));
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(stub) = locate_stub_exe() {
            hash_file_identity(&mut h, &stub);
        }
        format!("{:016x}", h.finish())
    })
}

/// Fold a file's size + mtime into `h`. An unreadable/absent file contributes a
/// stable "absent" marker rather than being skipped, so the tag still changes
/// when a binary appears or disappears.
fn hash_file_identity(h: &mut impl std::hash::Hasher, path: &Path) {
    use std::hash::Hash;
    match std::fs::metadata(path) {
        Ok(md) => {
            md.len().hash(h);
            let nanos = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            nanos.hash(h);
        }
        Err(_) => u128::MAX.hash(h),
    }
}

/// Keep a shared identity dir out of the orphan sweeper's sights while it is in
/// use, WITHOUT refcounting terminals (Q5(a)): rewriting the marker bumps its
/// mtime, and [`sweep_stale`] ages a dir by the newest mtime it contains. Only
/// done once [`IDENTITY_TOUCH_INTERVAL`] has passed, so a burst of spawns
/// performs no writes at all.
fn refresh_identity_liveness(marker: &Path, md: &std::fs::Metadata) {
    let stale = md
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age >= IDENTITY_TOUCH_INTERVAL)
        .unwrap_or(true);
    if stale {
        let _ = std::fs::write(marker, identity_build_tag().as_bytes());
    }
}

/// Rate-limited [`sweep_stale`] for the always-on identity path.
fn maybe_sweep_stale(base_dir: &Path) {
    let due = match LAST_SWEEP.lock() {
        Ok(mut last) => {
            let due = last.is_none_or(|t| t.elapsed() >= SWEEP_MIN_INTERVAL);
            if due {
                *last = Some(std::time::Instant::now());
            }
            due
        }
        Err(_) => false,
    };
    if due {
        sweep_stale(base_dir, STALE_SHIM_MAX_AGE);
    }
}

/// The session-CLI binary filename [`materialize_session_cli`] delivers.
/// Named `qontinui-pr` (NOT `qontinui`): this dir is PATH-prepended in every
/// runner terminal, so a `qontinui` bin would shadow the Python qontinui
/// library's `qontinui` console script.
pub const SESSION_CLI_BIN: &str = if cfg!(windows) {
    "qontinui-pr.exe"
} else {
    "qontinui-pr"
};

/// One-shot latch: warn about the missing session-CLI binary ONCE per process,
/// not once per terminal spawn.
static SESSION_CLI_MISSING_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Hardlink (or copy) the built `qontinui-pr` session CLI binary — it sits
/// next to the runner exe at runtime: bundled installs carry it as a Tauri
/// `externalBin` sidecar (`tauri.conf.json` + `scripts/bundle-profile-sidecar.mjs`,
/// same mechanism as `qontinui_profile`), and dev/supervisor builds have it in
/// the shared cargo target dir — into the per-terminal identity dir so
/// `qontinui-pr create` is on every session's PATH. Best-effort, fail-open:
/// every failure (dev build without the binary, cross-volume hardlink refusal,
/// copy error) only logs and the terminal spawns unaffected. Never panics.
/// An ABSENT binary warns once per process (not per terminal) so a broken
/// bundle is diagnosable instead of silently degrading.
fn materialize_session_cli(dir: &Path) {
    let name = SESSION_CLI_BIN;
    let src = match std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
    {
        Some(p) if p.is_file() => p,
        _ => {
            if SESSION_CLI_MISSING_WARNED
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                tracing::warn!(
                    tool = name,
                    "session cli: {name} not found next to the runner exe — \
                     `qontinui-pr create` unavailable in every terminal of this \
                     runner (dev build without the bin, or a bundle missing the \
                     externalBin sidecar)"
                );
            }
            return;
        }
    };
    let dest = dir.join(name);
    // Hardlink first (zero extra disk when temp shares the exe's volume);
    // fall back to a plain copy (temp on another volume, FS without links).
    if std::fs::hard_link(&src, &dest).is_ok() {
        return;
    }
    if let Err(e) = std::fs::copy(&src, &dest) {
        tracing::debug!(
            tool = name,
            error = %e,
            "session cli: failed to materialize {name} into the identity shim dir"
        );
    }
}

/// Render an identity shim template by substituting its `@@…@@` placeholders.
/// Pure. Keys: `@@TOOL@@` (provider program), `@@SHIM_DIR@@` (own dir, skipped
/// in the real-tool scan).
fn render_identity(body: &str, tool: &str, shim_dir: &Path) -> String {
    body.replace("@@TOOL@@", tool)
        .replace("@@SHIM_DIR@@", &shim_dir.to_string_lossy())
}

/// Test-only accessor for [`render_identity`].
#[cfg(test)]
pub fn render_identity_for_test(tool: &str, shim_dir: &Path) -> String {
    render_identity(IDENTITY_SHIM_BASH, tool, shim_dir)
}

/// Write the platform-appropriate identity shim file(s) for `tool`
/// (`claude`/`gemini`) into `dir`. Mirrors [`write_shims_for`]: always an
/// extensionless script (Git Bash + Unix); on Windows ALSO a `.cmd` AND a copy
/// of the compiled `qontinui-shim` stub as `<tool>.exe` ([`copy_exe_stub`]).
///
/// The native `.exe` is the PRIMARY PowerShell/cmd surface (`.EXE` precedes
/// `.CMD` in `PATHEXT`, so when both exist the exe wins). The earlier
/// `.cmd`-only policy here was wrong twice over: (a) its premise —
/// "claude/gemini ship as `.cmd`/scripts, not `.exe`" — does not hold (claude
/// ships as a native `claude.exe` on some installs, which a `.cmd` cannot
/// shadow at all), and (b) batch shims are fundamentally unsafe for argument
/// passing: they are launched via cmd.exe, which cannot accept multi-line or
/// cmd-metachar arguments, so the `.cmd` shim broke EVERY runner pane `claude`
/// launch on 2026-07-03 (the shell-integration function passes a multi-line
/// `--append-system-prompt`; live failure: "The syntax of the command is
/// incorrect."). The `.cmd` is kept ONLY as a fail-open fallback: if the stub
/// copy fails (builds without the sidecar, dev setups) behavior degrades to
/// exactly today's `.cmd` path, logged at debug.
fn write_identity_shims_for(dir: &Path, tool: &str) -> std::io::Result<()> {
    let bash = render_identity(IDENTITY_SHIM_BASH, tool, dir);
    let extensionless = dir.join(tool);
    std::fs::write(&extensionless, bash.as_bytes())?;
    set_executable(&extensionless)?;

    #[cfg(target_os = "windows")]
    {
        let cmd_body = render_identity(IDENTITY_SHIM_CMD, tool, dir);
        std::fs::write(dir.join(format!("{tool}.cmd")), cmd_body.as_bytes())?;
        // The native exe stub (best-effort, fail-open): the stub detects the
        // identity tool from argv[0], so the one binary serves claude + gemini.
        copy_exe_stub(dir, tool);
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
/// `argv[0]`, so a single binary serves every materialized name (the
/// `.exe`-shipped install tools AND the always-on identity family
/// `claude`/`gemini`, where the native exe avoids cmd.exe's multi-line-argument
/// limitation).
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

// ===========================================================================
// PERSISTENT identity shim (plan
// 2026-07-17-universal-coord-device-identity-for-any-session §6)
// ===========================================================================
//
// Everything above materializes PER-PTY into `temp_dir()/qontinui-identity-<id>`
// and is prepended to the CHILD env only — so a BARE terminal (one the runner
// did not spawn) never sees a shim at all, and the shim's self-provisioning
// fallback (`bin/qontinui_shim.rs`) is dead code for exactly the sessions it
// targets. This section closes that: a STABLE dir (`profile_cli::identity_shim_dir`)
// holding one `claude.exe`, added to the USER PATH from the Settings panel.
//
// ⚠ That Settings toggle IS the delivery gate (trust-boundary option C). It is
// an operator opt-in, independent of the runner-side mint flag
// (`QONTINUI_SESSION_COORD_IDENTITY_ENABLED`) + marker: neither alone grants a
// bare session identity. Do not collapse them, and do not make either implicit.
//
// ⚠ SELF-SPAWN TRAP. A persistent on-PATH `claude.exe` makes the trap the shim's
// module docs already name materially more likely: if `own_shim_dirs` misses this
// dir, the stub resolves ITSELF as the "real" claude. `own_shim_dirs` excludes it
// explicitly via `profile_cli::identity_shim_dir()` — the SAME function used here
// — which is why that path lives in the lib crate rather than being spelled twice.

/// The identity tool the PERSISTENT dir shadows. Deliberately `claude` only,
/// not [`IDENTITY_TOOLS`]: the fallback this dir exists to deliver is
/// `--mcp-config`, which is a claude-CLI flag (`identity_mcp_config_args`
/// returns nothing for gemini), and the per-PTY concerns that justify shadowing
/// gemini (a pinned `--session-id`) do not exist in a bare terminal. Shadowing a
/// tool we would only ever pass through is pure risk with no benefit.
#[cfg(target_os = "windows")]
pub const PERSISTENT_IDENTITY_TOOL: &str = "claude";

/// Materialize the persistent identity dir: create
/// [`crate::profile_cli::identity_shim_dir`] and copy the `qontinui-shim` stub
/// into it as `claude.exe`, reusing [`copy_exe_stub`] / [`locate_stub_exe`].
///
/// Unlike every other materializer here this is **fail-CLOSED** and returns a
/// `Result`: it runs from an explicit operator click, not from a terminal spawn,
/// so a failure must be reported to the Settings UI rather than silently
/// degrading. Adding the dir to PATH without the stub in it would be a silent
/// no-op that reads as "enabled".
#[cfg(target_os = "windows")]
pub fn materialize_persistent_identity() -> Result<PathBuf, String> {
    let dir = qontinui_runner_lib::profile_cli::identity_shim_dir()
        .ok_or_else(|| "home directory unresolvable".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    copy_exe_stub(&dir, PERSISTENT_IDENTITY_TOOL);
    let stub = dir.join(format!("{PERSISTENT_IDENTITY_TOOL}.exe"));
    if !stub.is_file() {
        return Err(format!(
            "the qontinui-shim stub could not be copied to {} — it is not next to the runner \
             executable (a dev build without `cargo build --bin qontinui-shim`?). Refusing to \
             put an empty dir on your PATH.",
            stub.display()
        ));
    }
    Ok(dir)
}

#[cfg(not(target_os = "windows"))]
pub fn materialize_persistent_identity() -> Result<PathBuf, String> {
    Err("the persistent identity shim is Windows-only for now".to_string())
}

/// Delete the persistent identity dir. Idempotent (an absent dir is `Ok`) — the
/// second half of the reversibility that makes the Settings toggle the right
/// gate. Called AFTER the PATH entry is removed, so no window exists in which
/// PATH names a dir whose stub is already gone.
pub fn remove_persistent_identity() -> Result<(), String> {
    let Some(dir) = qontinui_runner_lib::profile_cli::identity_shim_dir() else {
        return Ok(()); // no home dir ⇒ nothing was ever materialized
    };
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", dir.display())),
    }
}

/// Is the persistent identity shim fully installed — the stub materialized AND
/// its dir on the USER PATH? Both are required: either alone does nothing.
pub fn persistent_identity_installed() -> Result<bool, String> {
    let Some(dir) = qontinui_runner_lib::profile_cli::identity_shim_dir() else {
        return Ok(false);
    };
    #[cfg(target_os = "windows")]
    let stub_present = dir
        .join(format!("{PERSISTENT_IDENTITY_TOOL}.exe"))
        .is_file();
    #[cfg(not(target_os = "windows"))]
    let stub_present = false;
    if !stub_present {
        return Ok(false);
    }
    qontinui_runner_lib::profile_cli::dir_on_user_path(&dir)
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
/// Reaps ONLY the per-terminal install-intercept dir (`qontinui-shim-<id>`).
///
/// Q5(a): it used to reap `qontinui-identity-<id>` as well, which is precisely
/// what made the identity dir un-shareable — one terminal exiting would
/// `remove_dir_all` a directory every other live terminal still has on its
/// PATH. The identity dir is now content-addressed and shared per runner build
/// ([`materialize_identity`]), so it has no per-terminal owner to clean up
/// after; its lifetime is handled by [`sweep_stale`], which reaps it once
/// nothing has touched it for [`STALE_SHIM_MAX_AGE`]. Refcounting terminals
/// would have been the alternative, and it does not survive a runner crash —
/// mtime liveness does.
pub fn cleanup(base_dir: &Path, terminal_id: &str) {
    let dir = base_dir.join(format!("{SHIM_DIR_PREFIX}{terminal_id}"));
    if !dir.exists() {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        tracing::debug!(
            error = %e,
            dir = %dir.display(),
            "install shim dir cleanup failed (will be swept later)"
        );
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
    // THIS process's own shared identity dir — never a sweep candidate, at any
    // age. Belt-and-braces beside the ordering fix in [`materialize_identity`]:
    // mtime liveness is a heuristic (a partial `remove_dir_all` on Windows can
    // leave the dir present but missing files, with no marker, and the dir is
    // shared by every live pane), whereas "this is the directory I am currently
    // handing out on PATH" is a fact this process knows for certain.
    let own_identity_dir = identity_dir_name();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if removed >= STALE_SWEEP_CAP {
            break;
        }
        let path = entry.path();
        // Only our own per-terminal dirs (install-intercept OR always-on
        // identity).
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_ours = name.starts_with(SHIM_DIR_PREFIX) || name.starts_with(IDENTITY_DIR_PREFIX);
        if !is_ours || name == own_identity_dir || !path.is_dir() {
            continue;
        }
        // Age by the NEWEST mtime the dir carries — its own, or any file
        // directly inside it. The directory's own mtime is not enough: on
        // Windows it only moves when an entry is created/removed, so rewriting
        // the shared identity dir's liveness marker (which is how a dir with no
        // per-terminal owner says "still in use") would be invisible and an
        // actively-used dir would be deleted out from under live terminals.
        // Unreadable ⇒ conservatively skip (never delete what we cannot date).
        let aged_out = newest_mtime(&path, entry.metadata().ok().as_ref())
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

/// Newest modification time of `dir` itself or any entry directly inside it.
/// `dir_meta` is the caller's already-fetched metadata for `dir` (avoids a
/// second stat). `None` when nothing could be dated.
fn newest_mtime(dir: &Path, dir_meta: Option<&std::fs::Metadata>) -> Option<std::time::SystemTime> {
    let mut newest = dir_meta.and_then(|m| m.modified().ok());
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(mtime) = e.metadata().and_then(|m| m.modified()) {
                newest = Some(match newest {
                    Some(cur) if cur >= mtime => cur,
                    _ => mtime,
                });
            }
        }
    }
    newest
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

    /// §6: the PERSISTENT dir is a stable app-data path, NOT a per-PTY temp dir
    /// — the whole point is that it survives to be on the USER PATH. It must
    /// also carry neither the per-PTY prefix nor live where `sweep_stale`
    /// reaps: a swept-away identity dir would leave a PATH entry pointing at
    /// nothing.
    #[test]
    fn persistent_identity_dir_is_stable_and_never_swept() {
        let Some(dir) = qontinui_runner_lib::profile_cli::identity_shim_dir() else {
            return; // no home dir in this environment
        };
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "the persistent dir must NOT live where sweep_stale reaps, got {}",
            dir.display()
        );
        assert!(
            !dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(IDENTITY_DIR_PREFIX),
            "must not wear the per-PTY prefix — sweep_stale keys off it"
        );
    }

    /// `remove_persistent_identity` is idempotent: an absent dir is `Ok`, and a
    /// second call is a no-op. Reversibility is the reason the Settings toggle
    /// beats a dotfile, so uninstall must never fail on a partial install.
    #[test]
    fn remove_persistent_identity_is_idempotent() {
        // Never materialized (or already removed) ⇒ Ok, no panic. Runs against
        // the real path but only ever REMOVES what this feature owns, and only
        // when a previous test/opt-in put it there.
        if qontinui_runner_lib::profile_cli::identity_shim_dir()
            .map(|d| d.exists())
            .unwrap_or(false)
        {
            return; // the operator has really opted in — don't delete their dir
        }
        assert!(remove_persistent_identity().is_ok());
        assert!(remove_persistent_identity().is_ok(), "idempotent");
    }

    /// A partial install must read as NOT installed: the stub and the PATH entry
    /// are both required, so `persistent_identity_installed` can never report
    /// `true` off an un-materialized dir.
    #[test]
    fn persistent_identity_not_installed_without_the_stub() {
        let installed = persistent_identity_installed().unwrap_or(false);
        let stub_present = qontinui_runner_lib::profile_cli::identity_shim_dir()
            .map(|d| d.join("claude.exe").is_file())
            .unwrap_or(false);
        if !stub_present {
            assert!(
                !installed,
                "no stub materialized ⇒ must never report installed"
            );
        }
    }

    #[test]
    fn materialize_identity_writes_claude_and_gemini_always() {
        // The identity family is materialized with NO master-flag gate — it is
        // the out-of-box session-restore guarantee.
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("identity materialize ok");
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(IDENTITY_DIR_PREFIX),
            "identity dir uses its own prefix"
        );
        for tool in IDENTITY_TOOLS {
            let f = dir.join(tool);
            assert!(f.exists(), "extensionless {tool} identity shim must exist");
            let body = std::fs::read_to_string(&f).unwrap();
            assert!(body.contains(&format!("TOOL=\"{tool}\"")));
            // The shim pins the runner-injected session id and respects the
            // recursion guard.
            assert!(body.contains("QONTINUI_PINNED_SESSION_ID"));
            assert!(body.contains("--session-id"));
            assert!(body.contains("QONTINUI_INSTALL_INTERCEPT_GUARD"));
            // It confirms via the new control route.
            assert!(body.contains("/control/session-open"));
            #[cfg(target_os = "windows")]
            assert!(
                dir.join(format!("{tool}.cmd")).exists(),
                "{tool}.cmd identity shim must exist on Windows"
            );
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

    /// Q5(a). The identity dir is SHARED across every terminal of this runner
    /// build, so a terminal exiting must NOT delete it — that would rip the
    /// PATH shims out from under every other live pane. `cleanup` therefore
    /// reaps only the per-terminal install-intercept dir; the identity dir's
    /// lifetime belongs to `sweep_stale`.
    #[test]
    fn cleanup_reaps_the_install_dir_but_never_the_shared_identity_dir() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let install = materialize(tmp.path(), "term-x", 1, InterceptMode::Observe).unwrap();
        let identity = materialize_identity(tmp.path()).unwrap();
        assert!(install.shim_dir.exists() && identity.exists());
        cleanup(tmp.path(), "term-x");
        assert!(!install.shim_dir.exists(), "install dir reaped");
        assert!(
            identity.exists(),
            "the shared identity dir must survive one terminal's teardown — other panes are \
             still running its shims"
        );

        // The sweep owns the identity dir's end-of-life, but never THIS
        // process's own — see `sweep_never_reaps_this_builds_own_identity_dir`.
        // A foreign build's identity dir at zero max-age still goes.
        let foreign = tmp.path().join(format!("{IDENTITY_DIR_PREFIX}0ldbu1ld"));
        std::fs::create_dir_all(&foreign).unwrap();
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(
            !foreign.exists(),
            "a previous build's identity dir is swept"
        );
        assert!(
            identity.exists(),
            "this build's own dir is never a candidate"
        );
    }

    /// G2(3). The sweeper must never reap the identity dir THIS process is
    /// handing out on every terminal's PATH — at any age.
    ///
    /// mtime liveness is a heuristic: the dir has no per-terminal owner, a
    /// partial `remove_dir_all` on Windows (sharing violation on a running
    /// `claude.exe`) can leave it present but missing files with no marker, and
    /// a busy sibling runner instance sweeping the SHARED system temp dir would
    /// otherwise age out an idle instance's dir. "This is the directory I am
    /// currently PATH-prepending" is a fact, not a heuristic — so it wins.
    #[test]
    fn sweep_never_reaps_this_builds_own_identity_dir() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let own = materialize_identity(tmp.path()).expect("identity materialize");
        // Zero max-age = "everything is stale". The own dir still survives.
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(
            own.exists(),
            "the dir every live pane has on its PATH must never be swept out from under them"
        );
        assert!(
            own.join(IDENTITY_MARKER).exists(),
            "and it must survive intact, not as a hollowed-out shell"
        );
    }

    /// G2(1). The sweep must not run BEFORE the fast path that refreshes
    /// liveness — on a box where no new terminal has spawned for longer than
    /// `STALE_SHIM_MAX_AGE`, sweeping first would make the very dir this call is
    /// about to return a candidate in the same call. Asserted end-to-end: even
    /// with the rate limiter forced open and a zero-age policy in play, a
    /// `materialize_identity` always returns a dir that EXISTS and is complete.
    #[test]
    fn materialize_identity_never_returns_a_dir_it_just_swept() {
        let tmp = tempfile::tempdir().unwrap();
        // Force the rate limiter open so the sweep really runs in this call.
        if let Ok(mut last) = LAST_SWEEP.lock() {
            *last = None;
        }
        let dir = materialize_identity(tmp.path()).expect("first materialize");
        assert!(dir.join(IDENTITY_MARKER).exists());

        if let Ok(mut last) = LAST_SWEEP.lock() {
            *last = None;
        }
        let again = materialize_identity(tmp.path()).expect("cached materialize");
        assert_eq!(dir, again);
        assert!(
            again.join(IDENTITY_MARKER).exists(),
            "the returned dir must still be complete after the in-call sweep"
        );
        for tool in IDENTITY_TOOLS {
            assert!(again.join(tool).exists(), "{tool} shim must survive");
        }
    }

    /// G2(2). Liveness is refreshable WITHOUT a terminal spawn — the runner's
    /// 45s poll drives it whenever any terminal is alive, so "in use" stops
    /// meaning "someone spawned recently".
    #[test]
    fn poll_driven_touch_refreshes_identity_liveness() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("materialize");
        let marker = dir.join(IDENTITY_MARKER);
        // Backdate past the touch interval by rewriting the marker through a
        // path that leaves an OLD mtime is not portable; instead assert the
        // contract that matters: the touch is safe, idempotent, and leaves the
        // marker in place (the write itself is gated by IDENTITY_TOUCH_INTERVAL).
        touch_identity_liveness(tmp.path());
        assert!(marker.exists(), "touch must never remove the marker");
        let before = std::fs::read(&marker).unwrap();
        touch_identity_liveness(tmp.path());
        assert_eq!(std::fs::read(&marker).unwrap(), before, "idempotent");

        // An absent dir (nothing materialized yet) is a silent no-op — the poll
        // runs on every tick, including before the first terminal spawn.
        let empty = tempfile::tempdir().unwrap();
        touch_identity_liveness(empty.path());
        assert!(
            !empty
                .path()
                .join(format!("{IDENTITY_DIR_PREFIX}{}", identity_build_tag()))
                .exists(),
            "the touch must never CREATE the dir — materialize owns that"
        );
    }

    /// **F-second-pass-5 regression.** The poll-driven toucher refreshes the
    /// SAME directory `materialize_identity` handed out and `sweep_stale`
    /// excludes.
    ///
    /// [`identity_dir`] documented itself as "the ONE definition of the name"
    /// while [`touch_identity_liveness`] and [`sweep_stale`] each hand-rolled
    /// `format!("{IDENTITY_DIR_PREFIX}{}", identity_build_tag())` of their own.
    /// No divergence had occurred yet — which is precisely why no test caught
    /// it — but the consequence of one is severe and silent: the toucher would
    /// refresh a directory nobody is using, the real one would age past
    /// [`STALE_SHIM_MAX_AGE`], and the sweep would `remove_dir_all` the dir every
    /// live pane has PATH-prepended, so the next `claude` would resolve the real
    /// binary, lose its `--session-id` pin, and become unrestorable.
    ///
    /// `poll_driven_touch_refreshes_identity_liveness` above CANNOT catch that:
    /// a toucher aimed at a non-existent directory is a silent no-op, and every
    /// one of its assertions ("the marker still exists", "the bytes are
    /// unchanged") passes for a no-op. So this test BACKDATES the marker past
    /// [`IDENTITY_TOUCH_INTERVAL`] and asserts the mtime actually MOVES — an
    /// assertion only a toucher resolving the same directory can satisfy.
    #[test]
    fn touch_and_sweep_resolve_the_same_identity_dir_as_materialize() {
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("materialize");
        let marker = dir.join(IDENTITY_MARKER);

        // Backdate well past IDENTITY_TOUCH_INTERVAL so the refresh is DUE.
        let backdated = SystemTime::now() - Duration::from_secs(6 * 60 * 60);
        filetime::set_file_mtime(&marker, filetime::FileTime::from_system_time(backdated))
            .expect("backdate the marker");
        let before = std::fs::metadata(&marker).unwrap().modified().unwrap();
        assert!(
            before < SystemTime::now() - Duration::from_secs(60 * 60),
            "the fixture must actually be stale, or the touch has nothing to do"
        );

        touch_identity_liveness(tmp.path());

        let after = std::fs::metadata(&marker).unwrap().modified().unwrap();
        assert!(
            after > before,
            "the poll-driven toucher did not refresh the dir materialize handed out — the two \
             resolved DIFFERENT names"
        );

        // And the sweep's own-dir exclusion names that same directory: with
        // "everything is stale" it survives, while a foreign identity dir of the
        // same prefix does not.
        let foreign = tmp.path().join(format!("{IDENTITY_DIR_PREFIX}0ldbu1ld"));
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join(IDENTITY_MARKER), b"old").unwrap();
        sweep_stale(tmp.path(), Duration::from_secs(0));
        assert!(
            dir.exists() && marker.is_file(),
            "the sweep reaped the dir the toucher had just refreshed"
        );
        assert!(
            !foreign.exists(),
            "…and the sweep must still be doing its job, or the survival above is vacuous"
        );
    }

    /// Every terminal of one runner build reuses ONE content-addressed identity
    /// dir — the whole point of B2 (it used to be 4 script writes + 2 exe
    /// copies + a hardlink PER SPAWN).
    #[test]
    fn identity_dir_is_shared_and_reused_across_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let first = materialize_identity(tmp.path()).expect("first materialize");
        let marker = first.join(IDENTITY_MARKER);
        assert!(marker.exists(), "a complete dir is marked complete");

        // A sentinel proves the second call does not rewrite the dir.
        let sentinel = first.join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let second = materialize_identity(tmp.path()).expect("second materialize");
        assert_eq!(first, second, "both spawns share one dir");
        assert!(sentinel.exists(), "the cached fast path performs no writes");
    }

    /// Q5(c). Nothing kept a materialized shim current across a runner update
    /// except the accident that every spawn re-rendered it. With that gone the
    /// build tag has to change when the inputs do — otherwise a stale
    /// `claude.exe` from the previous runner build stays on every PATH.
    #[test]
    fn identity_build_tag_changes_when_the_runner_binary_changes() {
        use std::hash::{Hash, Hasher};
        let tmp = tempfile::tempdir().unwrap();
        let fake_exe = tmp.path().join("runner-build.bin");

        // Reproduces `identity_build_tag`'s file-identity contribution, which
        // is the term a runner update moves.
        let tag_for = |p: &Path| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            IDENTITY_SHIM_BASH.hash(&mut h);
            hash_file_identity(&mut h, p);
            format!("{:016x}", h.finish())
        };

        std::fs::write(&fake_exe, b"build-one").unwrap();
        let before = tag_for(&fake_exe);
        // A runner update replaces the binary — different size (and mtime).
        std::fs::write(&fake_exe, b"build-two-is-a-different-length").unwrap();
        let after = tag_for(&fake_exe);
        assert_ne!(
            before, after,
            "a changed runner binary must change the identity build tag, or the cache would \
             pin every terminal to the previous build's shims forever"
        );

        // An absent binary is a STABLE, distinct input — not "skip", which would
        // make a missing and a present binary hash identically.
        let absent = tag_for(&tmp.path().join("does-not-exist"));
        assert_ne!(absent, after);
        assert_eq!(absent, tag_for(&tmp.path().join("also-missing")));
    }

    /// A dir whose marker is missing (crash part-way through materializing) is
    /// rewritten rather than PATH-prepended half-written.
    #[test]
    fn incomplete_identity_dir_is_rematerialized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).unwrap();
        std::fs::remove_file(dir.join(IDENTITY_MARKER)).unwrap();
        std::fs::remove_file(dir.join(IDENTITY_TOOLS[0])).unwrap();
        let again = materialize_identity(tmp.path()).unwrap();
        assert_eq!(again, dir);
        assert!(
            dir.join(IDENTITY_TOOLS[0]).exists(),
            "an unmarked dir must be rewritten in full"
        );
        assert!(dir.join(IDENTITY_MARKER).exists());
    }

    /// The shared dir has no per-terminal owner, so it says "still in use" by
    /// carrying a fresh mtime inside it. `sweep_stale` must therefore age a dir
    /// by its NEWEST content, not by the directory node alone — otherwise a
    /// continuously-used identity dir is deleted out from under live terminals
    /// after 24h.
    #[test]
    fn sweep_ages_a_dir_by_its_newest_content() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(format!("{IDENTITY_DIR_PREFIX}deadbeef"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(IDENTITY_MARKER), b"live").unwrap();

        // A max-age far in the past keeps nothing; a huge one keeps everything.
        // The property under test is that the FILE's mtime participates: with a
        // huge max-age the dir survives, and the sweep does not panic reading it.
        sweep_stale(tmp.path(), Duration::from_secs(10 * 365 * 24 * 3600));
        assert!(dir.exists(), "a freshly-touched dir is never swept");
        assert!(
            newest_mtime(&dir, std::fs::metadata(&dir).ok().as_ref()).is_some(),
            "the dir must be dateable from its contents"
        );
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
    fn materialize_identity_copies_native_exe_when_stub_present() {
        // P0 2026-07-03: the identity dir must carry a NATIVE `<tool>.exe`
        // (PATHEXT: .EXE beats .CMD) so PowerShell/cmd never launch the batch
        // shim, which cannot accept multi-line args. Arrange the stub the way
        // `locate_stub_exe` finds it: a `qontinui-shim.exe` next to
        // `current_exe()` (the test binary's own dir). Create-if-absent and
        // clean up only what we created, so a dev target dir that already has
        // the real stub still passes.
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let stub = exe_dir.join("qontinui-shim.exe");
        let created = if stub.exists() {
            false
        } else {
            std::fs::write(&stub, b"fake stub payload").unwrap();
            true
        };

        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("identity materialize ok");
        for tool in IDENTITY_TOOLS {
            let exe = dir.join(format!("{tool}.exe"));
            assert!(exe.is_file(), "{tool}.exe must be materialized");
            // The .cmd fallback is STILL written alongside (fail-open ladder).
            assert!(dir.join(format!("{tool}.cmd")).is_file());
            assert!(dir.join(tool).is_file());
        }

        if created {
            let _ = std::fs::remove_file(&stub);
        }
    }

    #[test]
    fn materialize_identity_delivers_session_cli_when_binary_present() {
        // Phase 2b (qontinui-pr-credential-provisioning): the identity dir must
        // carry the `qontinui-pr` session CLI so `qontinui-pr create` is on
        // every terminal's PATH. Arrange the binary the way
        // `materialize_session_cli` finds it: next to `current_exe()` (the test
        // binary's own dir). Create-if-absent and clean up only what we
        // created, so a dev target dir that already has the real CLI still
        // passes.
        let name = SESSION_CLI_BIN;
        assert!(
            name.starts_with("qontinui-pr"),
            "the session CLI must NOT be named plain `qontinui` — it would \
             shadow the Python qontinui console script on the PATH-prepended \
             shim dir (got {name})"
        );
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let cli = exe_dir.join(name);
        let created = if cli.exists() {
            false
        } else {
            std::fs::write(&cli, b"fake cli payload").unwrap();
            true
        };

        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("identity materialize ok");
        assert!(
            dir.join(name).is_file(),
            "{name} must be materialized into the identity dir"
        );

        if created {
            let _ = std::fs::remove_file(&cli);
        }
    }

    #[test]
    fn materialize_identity_survives_absent_session_cli() {
        // Fail-open: when no `qontinui-pr` binary sits next to current_exe the
        // identity dir still materializes with the claude/gemini shims. (If a
        // real CLI binary happens to be present in the test target dir, the
        // stronger delivery assertion is covered by the test above.)
        let tmp = tempfile::tempdir().unwrap();
        let dir = materialize_identity(tmp.path()).expect("identity materialize ok");
        for tool in IDENTITY_TOOLS {
            assert!(dir.join(tool).is_file());
        }
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
