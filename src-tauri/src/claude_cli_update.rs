//! The runner-owned Claude Code CLI updater, and the child-env pair that hands
//! CLI updates to it.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`,
//! Phases 2 and 3 (design decision D1, option C).
//!
//! # Why the runner owns updates
//!
//! Claude Code's own updater checks npm at start-up and every 30 minutes, and it
//! compares npm `latest` against the RUNNING version, not the one on disk. So
//! every long-lived session started on an older binary runs
//! `npm install --global @anthropic-ai/claude-code@<latest>` every 30 minutes for
//! as long as it lives, even though the current version is already installed.
//! Measured on merytshost (Phase 1b): 32 same-version installs in 16 minutes from
//! 32 distinct stale sessions, 27 of them runner-spawned. npm's reify moves
//! `bin/claude` aside for each install, and a migration respawn that resolves
//! `claude` inside that window dies with exit 127.
//!
//! The fix has two halves, and they ship together:
//!
//! 1. **One updater** ([`run_periodic`]): at boot (after [`BOOT_DELAY`]) and then
//!    every [`TICK_INTERVAL`] it reads the installed version (`claude --version`
//!    through [`crate::agent_runtime::resolve_claude_bin`]) and the published one
//!    (`npm view @anthropic-ai/claude-code version`), and installs **only when
//!    they differ**, pinned to the version it read, into the npm prefix that owns
//!    the resolved binary. A same-version install cannot happen here. After an
//!    install it reads the version again. If that is not the published version
//!    it logs `cli-update: install-did-not-take` and backs off for
//!    [`DID_NOT_TAKE_BACKOFF`], so a prefix mismatch cannot loop.
//! 2. **Children defer to it** ([`child_env_pairs`]): every runner-spawned
//!    `claude` gets `DISABLE_AUTOUPDATER=1`, which Claude Code honours (Phase 1a,
//!    verified against a control that did install). The pair is set **only** when
//!    the updater can actually serve this box, see [`ChildEnvGate`]. Setting it
//!    anywhere else would stop updates altogether (option B, which the plan
//!    rejected).
//!
//! # The user's choices win
//!
//! - `claude_cli.runner_manages_cli_updates` (default `true`, on the AI settings
//!   page) is the in-product off-switch. Off means no install from here and no
//!   env var on any child, which is exactly the behaviour before this module.
//! - When the runner's OWN environment carries `DISABLE_AUTOUPDATER` or
//!   `DISABLE_UPDATES` ([`OPT_OUT_ENV_VARS`]), the user has turned updates off for
//!   everything, and the updater installs nothing.
//! - A setting that cannot be read resolves to the last value that WAS read, and
//!   to "unknown" (no install, no env var) before any read has succeeded. It
//!   never falls back to the permissive default.
//!
//! # For the resume seam: the in-progress flag
//!
//! While an install runs, the updater holds an in-process flag. A spawn that
//! resolves the `claude` binary should wait for it first, so an install the
//! runner chose never races a spawn the runner makes:
//!
//! ```ignore
//! match crate::claude_cli_update::wait_if_updating(Duration::from_secs(120)).await {
//!     UpdateWait::NotUpdating | UpdateWait::Finished(_) => { /* resolve now */ }
//!     UpdateWait::TimedOut(_) => { /* resolve anyway; the retry budget covers it */ }
//! }
//! ```
//!
//! [`update_in_progress`] is the synchronous read and
//! [`subscribe_update_in_progress`] hands out a `tokio::sync::watch::Receiver`
//! for callers that want to compose their own wait.
//!
//! # Scope
//!
//! - **Primary runner only.** Installing is a machine-wide duty, and two runners
//!   on one box would race each other's installs. `main.rs` starts
//!   [`run_periodic`] only when `instance::is_secondary()` is false. A secondary
//!   never completes a version read, so it never sets the child env pair and its
//!   children keep the CLI's own updater.
//! - **npm-global installs on unix only** ([`locate_npm_global_install`]). A
//!   native install (`~/.local/share/claude/versions/...`) is not under an npm
//!   prefix. On Windows the running `claude.exe` is locked, so an npm install
//!   there fails while any session is open. Both are reported as an unsupported
//!   install method, and on those boxes nothing changes.

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{info, warn};

/// The npm package the updater manages.
pub const CLI_PACKAGE: &str = "@anthropic-ai/claude-code";

/// The variable set on every runner-spawned child (Phase 3), and its value.
pub const SUPPRESS_ENV: &str = "DISABLE_AUTOUPDATER";
const SUPPRESS_VALUE: &str = "1";

/// Variables that, when present (non-empty) in the RUNNER's own environment,
/// mean the user turned CLI updates off for everything. The updater then
/// installs nothing.
pub const OPT_OUT_ENV_VARS: [&str; 2] = ["DISABLE_AUTOUPDATER", "DISABLE_UPDATES"];

/// First tick after boot. Long enough that boot-time spawns are not competing
/// with an install, short enough that a freshly started runner settles the
/// child env gate within minutes.
pub const BOOT_DELAY: Duration = Duration::from_secs(120);
/// Interval between ticks after the first.
pub const TICK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long the updater stops installing after an install that did not change
/// the installed version.
pub const DID_NOT_TAKE_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);

const VERSION_READ_TIMEOUT: Duration = Duration::from_secs(30);
const NPM_VIEW_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

// =============================================================================
// The in-progress flag (the public API for the resume seam)
// =============================================================================

/// An in-process "a CLI install is running" flag over a `tokio::sync::watch`.
///
/// One process-wide instance backs the public functions below. Tests build their
/// own, so they never touch the global.
pub(crate) struct UpdateFlag {
    tx: watch::Sender<bool>,
}

impl UpdateFlag {
    pub(crate) fn new() -> Self {
        Self {
            tx: watch::channel(false).0,
        }
    }

    fn is_updating(&self) -> bool {
        *self.tx.borrow()
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }

    /// Raise the flag until the returned guard drops. `send_replace` updates
    /// the value even when no receiver exists, which plain `send` does not.
    fn hold(&self) -> InProgressGuard<'_> {
        self.tx.send_replace(true);
        InProgressGuard { flag: self }
    }

    async fn wait_if_updating(&self, timeout: Duration) -> UpdateWait {
        let mut rx = self.subscribe();
        if !*rx.borrow_and_update() {
            return UpdateWait::NotUpdating;
        }
        let started = Instant::now();
        match tokio::time::timeout(timeout, rx.wait_for(|updating| !*updating)).await {
            // `Err` from `wait_for` means the sender is gone, so no install can
            // still be running.
            Ok(_) => UpdateWait::Finished(started.elapsed()),
            Err(_) => UpdateWait::TimedOut(started.elapsed()),
        }
    }
}

struct InProgressGuard<'a> {
    flag: &'a UpdateFlag,
}

impl Drop for InProgressGuard<'_> {
    fn drop(&mut self) {
        self.flag.tx.send_replace(false);
    }
}

fn global_flag() -> &'static UpdateFlag {
    static FLAG: OnceLock<UpdateFlag> = OnceLock::new();
    FLAG.get_or_init(UpdateFlag::new)
}

/// What [`wait_if_updating`] observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateWait {
    /// No install was running when the wait started. It returned at once.
    NotUpdating,
    /// An install was running and finished after this long.
    Finished(Duration),
    /// An install was still running when the timeout ran out. The caller should
    /// go ahead and rely on its own retry budget.
    TimedOut(Duration),
}

/// `true` while the runner-owned updater is running `npm install`.
pub fn update_in_progress() -> bool {
    global_flag().is_updating()
}

/// A receiver for the in-progress flag. It reads `true` while an install runs.
pub fn subscribe_update_in_progress() -> watch::Receiver<bool> {
    global_flag().subscribe()
}

/// Wait, at most `timeout`, for a running CLI install to finish. Returns at once
/// when none is running. Callers that resolve the `claude` binary for a spawn
/// should call this first. See the module docs.
pub async fn wait_if_updating(timeout: Duration) -> UpdateWait {
    global_flag().wait_if_updating(timeout).await
}

// =============================================================================
// Phase 3: the child env pair
// =============================================================================

/// The three conditions under which runner-spawned children get
/// `DISABLE_AUTOUPDATER=1`. All three must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildEnvGate {
    /// `claude_cli.runner_manages_cli_updates` read `true` (now or, when it
    /// cannot be read, the last time it could).
    pub setting_on: bool,
    /// The resolved `claude` is an npm-global install the updater can serve.
    pub install_method_supported: bool,
    /// The updater has read both the installed and the published version at
    /// least once. Until then it has not shown it works on this box.
    pub version_read_completed: bool,
}

const SUPPRESS_PAIRS: &[(&str, &str)] = &[(SUPPRESS_ENV, SUPPRESS_VALUE)];

/// The env pairs for one [`ChildEnvGate`]. Pure.
pub fn child_env_pairs_for(gate: ChildEnvGate) -> &'static [(&'static str, &'static str)] {
    if gate.setting_on && gate.install_method_supported && gate.version_read_completed {
        SUPPRESS_PAIRS
    } else {
        &[]
    }
}

/// The env pairs every runner-spawned child must carry: `[("DISABLE_AUTOUPDATER",
/// "1")]` when this box's CLI updates are owned by the runner, otherwise none.
///
/// The ONE source for the three child-env seams
/// (`TerminalSession::finalize_child_env`, `finalize_headless_child_env`,
/// `ClaudeSession::finalize_child_env`), so they cannot drift.
///
/// Cheap: two atomic loads, then (only when both pass) the mtime-cached
/// non-writing settings read.
pub fn child_env_pairs() -> &'static [(&'static str, &'static str)] {
    #[cfg(test)]
    if let Some(gate) = GATE_OVERRIDE.with(|g| g.get()) {
        return child_env_pairs_for(gate);
    }
    let state = box_state();
    let version_read_completed = state.version_read_completed.load(Ordering::Acquire);
    let install_method_supported = state.install_method_supported.load(Ordering::Acquire);
    if !(version_read_completed && install_method_supported) {
        // Short-circuit before the settings read. In unit tests no updater tick
        // ever runs, so every seam test that sets no override lands here.
        return &[];
    }
    child_env_pairs_for(ChildEnvGate {
        setting_on: runner_manages_cli_updates() == Some(true),
        install_method_supported,
        version_read_completed,
    })
}

/// Apply [`child_env_pairs`] through any env setter. The seams call this with
/// their own builder's `env`.
pub fn apply_child_env(mut set: impl FnMut(&str, &str)) {
    for (k, v) in child_env_pairs() {
        set(k, v);
    }
}

#[cfg(test)]
thread_local! {
    /// TEST-ONLY per-thread override of the gate, so a seam test can drive each
    /// arm without touching process-global state other tests can see.
    static GATE_OVERRIDE: std::cell::Cell<Option<ChildEnvGate>> =
        const { std::cell::Cell::new(None) };
}

/// TEST-ONLY: run `f` with [`child_env_pairs`] answering for `gate` on this
/// thread.
#[cfg(test)]
pub(crate) fn with_gate<R>(gate: ChildEnvGate, f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            GATE_OVERRIDE.with(|g| g.set(None));
        }
    }
    GATE_OVERRIDE.with(|g| g.set(Some(gate)));
    let _reset = Reset;
    f()
}

/// The gate with every condition met, and each of the three off arms. For the
/// seam tests.
#[cfg(test)]
pub(crate) mod test_gates {
    use super::ChildEnvGate;

    pub(crate) const ON: ChildEnvGate = ChildEnvGate {
        setting_on: true,
        install_method_supported: true,
        version_read_completed: true,
    };

    /// `(name, gate)` for each arm that must yield no pair.
    pub(crate) const OFF_ARMS: [(&str, ChildEnvGate); 3] = [
        (
            "setting off",
            ChildEnvGate {
                setting_on: false,
                ..ON
            },
        ),
        (
            "install method unsupported",
            ChildEnvGate {
                install_method_supported: false,
                ..ON
            },
        ),
        (
            "no completed version read",
            ChildEnvGate {
                version_read_completed: false,
                ..ON
            },
        ),
    ];
}

// =============================================================================
// Process-wide state published by the updater
// =============================================================================

const SETTING_UNKNOWN: u8 = 0;
const SETTING_OFF: u8 = 1;
const SETTING_ON: u8 = 2;

struct BoxState {
    install_method_supported: AtomicBool,
    version_read_completed: AtomicBool,
    /// The last authoritative read of the setting (`SETTING_*`).
    last_known_setting: AtomicU8,
}

fn box_state() -> &'static BoxState {
    static STATE: OnceLock<BoxState> = OnceLock::new();
    STATE.get_or_init(|| BoxState {
        install_method_supported: AtomicBool::new(false),
        version_read_completed: AtomicBool::new(false),
        last_known_setting: AtomicU8::new(SETTING_UNKNOWN),
    })
}

/// Resolve the setting from one read: an authoritative read wins and becomes
/// the last known value; a failed read falls back to the last known value.
/// Pure, so the "unreadable never means the default" rule is testable.
fn resolve_setting(read: Option<bool>, last_known: u8) -> (Option<bool>, u8) {
    match read {
        Some(v) => (Some(v), if v { SETTING_ON } else { SETTING_OFF }),
        None => (
            match last_known {
                SETTING_ON => Some(true),
                SETTING_OFF => Some(false),
                _ => None,
            },
            last_known,
        ),
    }
}

/// `claude_cli.runner_manages_cli_updates`, read through the non-writing,
/// mtime-cached [`crate::settings::read_settings_from_disk`]. `None` when the
/// file cannot be read and no read has ever succeeded.
pub(crate) fn runner_manages_cli_updates() -> Option<bool> {
    let loaded = crate::settings::read_settings_from_disk();
    let read = loaded
        .provenance
        .is_authoritative()
        .then_some(loaded.settings.ai.claude_cli.runner_manages_cli_updates);
    let state = box_state();
    let (value, store) = resolve_setting(read, state.last_known_setting.load(Ordering::Acquire));
    state.last_known_setting.store(store, Ordering::Release);
    value
}

// =============================================================================
// Pure pieces: version parsing, prefix derivation, the decision
// =============================================================================

/// A version string the updater is willing to compare and pin: starts with a
/// digit, then only digits, letters, `.`, `-` and `+`.
fn plausible_version(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// `claude --version` prints `2.1.304 (Claude Code)`. Keep what comes before the
/// first space.
pub(crate) fn parse_cli_version(stdout: &str) -> Option<String> {
    let token = stdout.lines().next()?.split_whitespace().next()?;
    plausible_version(token).then(|| token.to_string())
}

/// `npm view <pkg> version` prints the bare version. Take the last non-empty
/// line, in case npm prints anything before it.
pub(crate) fn parse_npm_view_version(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    plausible_version(line).then(|| line.to_string())
}

/// The npm prefix that owns a canonical `claude` path:
/// `<prefix>/lib/node_modules/@anthropic-ai/claude-code/...` gives `<prefix>`.
/// `None` for anything else (a native install, a copied binary).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn npm_prefix_for_package_path(canonical: &Path) -> Option<PathBuf> {
    let comps: Vec<Component<'_>> = canonical.components().collect();
    let want = ["lib", "node_modules", "@anthropic-ai", "claude-code"];
    let at = comps.windows(want.len()).position(|w| {
        w.iter()
            .zip(want.iter())
            .all(|(c, n)| c.as_os_str() == std::ffi::OsStr::new(n))
    })?;
    if at == 0 {
        return None;
    }
    Some(comps[..at].iter().collect())
}

/// Why a tick did not install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The runner's own environment carries this opt-out variable.
    OptOutEnv(String),
    SettingOff,
    /// The setting has never been read successfully.
    SettingUnknown,
    UnsupportedInstallMethod,
    InstalledUnreadable,
    PublishedUnreadable,
    UpToDate,
    /// An earlier install did not take; installs are paused until the backoff ends.
    BackingOff,
}

impl SkipReason {
    fn as_str(&self) -> String {
        match self {
            Self::OptOutEnv(name) => format!("opt-out-env:{name}"),
            Self::SettingOff => "setting-off".into(),
            Self::SettingUnknown => "setting-unknown".into(),
            Self::UnsupportedInstallMethod => "unsupported-install-method".into(),
            Self::InstalledUnreadable => "installed-unreadable".into(),
            Self::PublishedUnreadable => "published-unreadable".into(),
            Self::UpToDate => "up-to-date".into(),
            Self::BackingOff => "backing-off".into(),
        }
    }
}

/// What one tick decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Install { published: String },
    Skip(SkipReason),
}

/// Everything the decision depends on. The tick reads these; [`decide`] is pure.
#[derive(Debug, Clone, Default)]
pub(crate) struct TickInputs {
    pub opt_out_env: Option<String>,
    pub setting: Option<bool>,
    pub install_method_supported: bool,
    pub installed: Option<String>,
    pub published: Option<String>,
    pub backing_off: bool,
}

/// The decision table. The user's opt-outs come first, then whether this box
/// can be served, then the two reads, then the comparison. The backoff is
/// checked last so a tick inside it still reports both versions, and an
/// up-to-date box reads `up-to-date` rather than `backing-off`.
pub(crate) fn decide(i: &TickInputs) -> Decision {
    if let Some(name) = &i.opt_out_env {
        return Decision::Skip(SkipReason::OptOutEnv(name.clone()));
    }
    match i.setting {
        None => return Decision::Skip(SkipReason::SettingUnknown),
        Some(false) => return Decision::Skip(SkipReason::SettingOff),
        Some(true) => {}
    }
    if !i.install_method_supported {
        return Decision::Skip(SkipReason::UnsupportedInstallMethod);
    }
    let Some(installed) = &i.installed else {
        return Decision::Skip(SkipReason::InstalledUnreadable);
    };
    let Some(published) = &i.published else {
        return Decision::Skip(SkipReason::PublishedUnreadable);
    };
    if installed == published {
        return Decision::Skip(SkipReason::UpToDate);
    }
    if i.backing_off {
        return Decision::Skip(SkipReason::BackingOff);
    }
    Decision::Install {
        published: published.clone(),
    }
}

/// The first opt-out variable present and non-empty in the environment `get`
/// reads.
pub(crate) fn opt_out_env_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    OPT_OUT_ENV_VARS
        .iter()
        .find(|name| get(name).is_some_and(|v| !v.trim().is_empty()))
        .map(|name| name.to_string())
}

// =============================================================================
// The tick
// =============================================================================

/// An npm-global `claude` install the updater can serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NpmGlobalInstall {
    /// The resolved `claude` (the prefix's `bin/claude`).
    pub claude_bin: PathBuf,
    /// The npm prefix that owns it.
    pub prefix: PathBuf,
    /// `<prefix>/bin/npm`.
    pub npm: PathBuf,
}

/// The outcome of one `npm install`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallRun {
    /// `None` when the install timed out, could not start, or was killed.
    pub exit_code: Option<i32>,
    pub duration: Duration,
    /// A short tail of stderr, for the log line on failure.
    pub stderr_tail: String,
}

/// The side effects a tick performs, behind a trait so the whole tick runs in a
/// test with fakes.
pub(crate) trait CliOps {
    fn opt_out_env(&self) -> Option<String>;
    fn setting(&self) -> impl Future<Output = Option<bool>> + Send;
    fn locate(&self) -> impl Future<Output = Option<NpmGlobalInstall>> + Send;
    fn installed_version(
        &self,
        install: &NpmGlobalInstall,
    ) -> impl Future<Output = Option<String>> + Send;
    fn published_version(
        &self,
        install: &NpmGlobalInstall,
    ) -> impl Future<Output = Option<String>> + Send;
    fn install(
        &self,
        install: &NpmGlobalInstall,
        version: &str,
    ) -> impl Future<Output = InstallRun> + Send;
}

/// What the updater carries from one tick to the next.
#[derive(Debug, Default)]
pub(crate) struct UpdaterState {
    pub backoff_until: Option<Instant>,
    pub install_method_supported: bool,
    pub version_read_completed: bool,
}

/// What one tick saw and did. Logged by [`run_periodic`]; asserted by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TickReport {
    pub decision: Decision,
    pub installed: Option<String>,
    pub published: Option<String>,
    pub install: Option<InstallRun>,
    /// The version read back after an install.
    pub reread: Option<String>,
    /// `true` when the install did not leave the published version installed,
    /// which starts the backoff.
    pub did_not_take: bool,
}

/// One tick. `now` is injected so the backoff is testable without sleeping.
pub(crate) async fn tick<O: CliOps>(
    ops: &O,
    state: &mut UpdaterState,
    flag: &UpdateFlag,
    now: Instant,
) -> TickReport {
    let mut inputs = TickInputs {
        opt_out_env: ops.opt_out_env(),
        backing_off: state.backoff_until.is_some_and(|until| now < until),
        ..TickInputs::default()
    };
    // The opt-outs need no reads at all.
    if inputs.opt_out_env.is_none() {
        inputs.setting = ops.setting().await;
    }
    let mut located = None;
    if inputs.opt_out_env.is_none() && inputs.setting == Some(true) {
        located = ops.locate().await;
        inputs.install_method_supported = located.is_some();
        state.install_method_supported = inputs.install_method_supported;
        if let Some(install) = &located {
            inputs.installed = ops.installed_version(install).await;
            inputs.published = ops.published_version(install).await;
            if inputs.installed.is_some() && inputs.published.is_some() {
                state.version_read_completed = true;
            }
        }
    }

    let decision = decide(&inputs);
    let mut report = TickReport {
        decision: decision.clone(),
        installed: inputs.installed.clone(),
        published: inputs.published.clone(),
        install: None,
        reread: None,
        did_not_take: false,
    };

    if let (Decision::Install { published }, Some(install)) = (&decision, &located) {
        let run = {
            let _held = flag.hold();
            ops.install(install, published).await
        };
        report.install = Some(run);
        report.reread = ops.installed_version(install).await;
        if report.reread.as_deref() != Some(published.as_str()) {
            report.did_not_take = true;
            state.backoff_until = Some(now + DID_NOT_TAKE_BACKOFF);
        } else {
            state.backoff_until = None;
        }
    }
    report
}

fn log_report(r: &TickReport) {
    let installed = r.installed.as_deref().unwrap_or("-");
    let published = r.published.as_deref().unwrap_or("-");
    match &r.decision {
        Decision::Skip(reason) => info!(
            "cli-update: installed={installed} published={published} action=skip reason={}",
            reason.as_str()
        ),
        Decision::Install { published: to } => {
            info!("cli-update: installed={installed} published={published} action=install to={to}");
        }
    }
    if let Some(run) = &r.install {
        let exit = run
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".into());
        info!(
            "cli-update: install exit={exit} duration_ms={}",
            run.duration.as_millis()
        );
        if run.exit_code != Some(0) && !run.stderr_tail.is_empty() {
            warn!("cli-update: install stderr tail: {}", run.stderr_tail);
        }
        let reread = r.reread.as_deref().unwrap_or("-");
        if r.did_not_take {
            warn!(
                "cli-update: install-did-not-take installed={reread} expected={published} backoff_h={}",
                DID_NOT_TAKE_BACKOFF.as_secs() / 3600
            );
        } else {
            info!("cli-update: install took installed={reread}");
        }
    }
}

/// Publish what the tick learned to the process-wide state [`child_env_pairs`]
/// reads.
fn publish_state(state: &UpdaterState) {
    let s = box_state();
    s.install_method_supported
        .store(state.install_method_supported, Ordering::Release);
    if state.version_read_completed {
        s.version_read_completed.store(true, Ordering::Release);
    }
}

/// The boot-armed periodic task: once after [`BOOT_DELAY`], then every
/// [`TICK_INTERVAL`]. Primary runner only (see the module docs).
pub async fn run_periodic() {
    tokio::time::sleep(BOOT_DELAY).await;
    let ops = RealOps;
    let mut state = UpdaterState::default();
    loop {
        let report = tick(&ops, &mut state, global_flag(), Instant::now()).await;
        publish_state(&state);
        log_report(&report);
        tokio::time::sleep(TICK_INTERVAL).await;
    }
}

// =============================================================================
// The real side effects
// =============================================================================

/// Find the npm-global install that owns the resolved `claude`. Blocking (file
/// stats); called through `spawn_blocking`.
pub(crate) fn locate_npm_global_install() -> Option<NpmGlobalInstall> {
    #[cfg(not(unix))]
    {
        // Windows: the running `claude.exe` is locked, so `npm install` fails
        // while any session is open. Not served; see the module docs.
        None
    }
    #[cfg(unix)]
    {
        let resolved = PathBuf::from(crate::agent_runtime::resolve_claude_bin());
        if !resolved.is_absolute() {
            // The bare-name fallback: nothing resolved on PATH right now.
            return None;
        }
        let canonical = std::fs::canonicalize(&resolved).ok()?;
        let prefix = npm_prefix_for_package_path(&canonical)?;
        let npm = prefix.join("bin").join("npm");
        npm.is_file().then_some(NpmGlobalInstall {
            claude_bin: resolved,
            prefix,
            npm,
        })
    }
}

/// `PATH` for an npm child: the prefix's `bin` first, so npm's
/// `#!/usr/bin/env node` finds the node that owns this prefix.
fn npm_child_path(prefix: &Path) -> std::ffi::OsString {
    let mut dirs = vec![prefix.join("bin")];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

fn tail(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    let start = s.len().saturating_sub(max);
    let start = (start..=s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(s.len());
    s[start..].to_string()
}

async fn run_capture(
    mut cmd: tokio::process::Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => Some(out),
        Ok(Err(e)) => {
            warn!("cli-update: could not run child: {e}");
            None
        }
        Err(_) => {
            warn!("cli-update: child timed out after {}s", timeout.as_secs());
            None
        }
    }
}

struct RealOps;

impl CliOps for RealOps {
    fn opt_out_env(&self) -> Option<String> {
        opt_out_env_from(|k| std::env::var(k).ok())
    }

    async fn setting(&self) -> Option<bool> {
        tokio::task::spawn_blocking(runner_manages_cli_updates)
            .await
            .ok()
            .flatten()
    }

    async fn locate(&self) -> Option<NpmGlobalInstall> {
        tokio::task::spawn_blocking(locate_npm_global_install)
            .await
            .ok()
            .flatten()
    }

    fn installed_version(
        &self,
        install: &NpmGlobalInstall,
    ) -> impl Future<Output = Option<String>> + Send {
        let bin = install.claude_bin.clone();
        async move {
            let mut cmd = crate::process_helpers::tokio_no_window(&bin);
            cmd.arg("--version").env(SUPPRESS_ENV, SUPPRESS_VALUE);
            let out = run_capture(cmd, VERSION_READ_TIMEOUT).await?;
            if !out.status.success() {
                return None;
            }
            parse_cli_version(&String::from_utf8_lossy(&out.stdout))
        }
    }

    fn published_version(
        &self,
        install: &NpmGlobalInstall,
    ) -> impl Future<Output = Option<String>> + Send {
        let npm = install.npm.clone();
        let path = npm_child_path(&install.prefix);
        async move {
            let mut cmd = crate::process_helpers::tokio_no_window(&npm);
            cmd.args(["view", CLI_PACKAGE, "version"]).env("PATH", path);
            let out = run_capture(cmd, NPM_VIEW_TIMEOUT).await?;
            if !out.status.success() {
                return None;
            }
            parse_npm_view_version(&String::from_utf8_lossy(&out.stdout))
        }
    }

    fn install(
        &self,
        install: &NpmGlobalInstall,
        version: &str,
    ) -> impl Future<Output = InstallRun> + Send {
        let npm = install.npm.clone();
        let prefix = install.prefix.clone();
        let path = npm_child_path(&install.prefix);
        let spec = format!("{CLI_PACKAGE}@{version}");
        async move {
            let mut cmd = crate::process_helpers::tokio_no_window(&npm);
            cmd.arg("install")
                .arg("--global")
                .arg("--prefix")
                .arg(&prefix)
                .args(["--no-fund", "--no-audit"])
                .arg(&spec)
                .env("PATH", path);
            let started = Instant::now();
            let out = run_capture(cmd, INSTALL_TIMEOUT).await;
            InstallRun {
                exit_code: out.as_ref().and_then(|o| o.status.code()),
                duration: started.elapsed(),
                stderr_tail: out.map(|o| tail(&o.stderr, 400)).unwrap_or_default(),
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn inputs(installed: &str, published: &str) -> TickInputs {
        TickInputs {
            opt_out_env: None,
            setting: Some(true),
            install_method_supported: true,
            installed: Some(installed.to_string()),
            published: Some(published.to_string()),
            backing_off: false,
        }
    }

    // ---- The decision table ------------------------------------------------

    #[test]
    fn cli_update_decide_equal_versions_never_install() {
        assert_eq!(
            decide(&inputs("2.1.304", "2.1.304")),
            Decision::Skip(SkipReason::UpToDate)
        );
    }

    #[test]
    fn cli_update_decide_differing_versions_install_the_published_one() {
        assert_eq!(
            decide(&inputs("2.1.303", "2.1.304")),
            Decision::Install {
                published: "2.1.304".into()
            }
        );
    }

    #[test]
    fn cli_update_decide_unreadable_installed_never_installs() {
        let i = TickInputs {
            installed: None,
            ..inputs("x", "2.1.304")
        };
        assert_eq!(decide(&i), Decision::Skip(SkipReason::InstalledUnreadable));
    }

    #[test]
    fn cli_update_decide_unreadable_published_never_installs() {
        let i = TickInputs {
            published: None,
            ..inputs("2.1.303", "x")
        };
        assert_eq!(decide(&i), Decision::Skip(SkipReason::PublishedUnreadable));
    }

    #[test]
    fn cli_update_decide_opt_out_env_wins_over_everything() {
        for name in OPT_OUT_ENV_VARS {
            let i = TickInputs {
                opt_out_env: Some(name.to_string()),
                ..inputs("2.1.303", "2.1.304")
            };
            assert_eq!(
                decide(&i),
                Decision::Skip(SkipReason::OptOutEnv(name.to_string()))
            );
        }
    }

    #[test]
    fn cli_update_decide_setting_off_or_unknown_never_installs() {
        let off = TickInputs {
            setting: Some(false),
            ..inputs("2.1.303", "2.1.304")
        };
        assert_eq!(decide(&off), Decision::Skip(SkipReason::SettingOff));
        let unknown = TickInputs {
            setting: None,
            ..inputs("2.1.303", "2.1.304")
        };
        assert_eq!(decide(&unknown), Decision::Skip(SkipReason::SettingUnknown));
    }

    #[test]
    fn cli_update_decide_unsupported_install_method_never_installs() {
        let i = TickInputs {
            install_method_supported: false,
            ..inputs("2.1.303", "2.1.304")
        };
        assert_eq!(
            decide(&i),
            Decision::Skip(SkipReason::UnsupportedInstallMethod)
        );
    }

    #[test]
    fn cli_update_decide_within_backoff_never_installs() {
        let i = TickInputs {
            backing_off: true,
            ..inputs("2.1.303", "2.1.304")
        };
        assert_eq!(decide(&i), Decision::Skip(SkipReason::BackingOff));
    }

    #[test]
    fn cli_update_opt_out_env_reads_present_non_empty_only() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(opt_out_env_from(env(&[])), None);
        assert_eq!(opt_out_env_from(env(&[("DISABLE_AUTOUPDATER", "")])), None);
        assert_eq!(
            opt_out_env_from(env(&[("DISABLE_AUTOUPDATER", "1")])),
            Some("DISABLE_AUTOUPDATER".into())
        );
        assert_eq!(
            opt_out_env_from(env(&[("DISABLE_UPDATES", "true")])),
            Some("DISABLE_UPDATES".into())
        );
    }

    #[test]
    fn cli_update_unreadable_setting_keeps_the_last_known_value() {
        // Never read: unknown, not the permissive default.
        assert_eq!(
            resolve_setting(None, SETTING_UNKNOWN),
            (None, SETTING_UNKNOWN)
        );
        // A read wins and is remembered.
        assert_eq!(
            resolve_setting(Some(false), SETTING_ON),
            (Some(false), SETTING_OFF)
        );
        assert_eq!(
            resolve_setting(Some(true), SETTING_OFF),
            (Some(true), SETTING_ON)
        );
        // A failed read keeps what was last read.
        assert_eq!(
            resolve_setting(None, SETTING_OFF),
            (Some(false), SETTING_OFF)
        );
        assert_eq!(resolve_setting(None, SETTING_ON), (Some(true), SETTING_ON));
    }

    // ---- Parsing and prefix ------------------------------------------------

    #[test]
    fn cli_update_parses_both_version_sources_to_the_same_string() {
        assert_eq!(
            parse_cli_version("2.1.304 (Claude Code)\n").as_deref(),
            Some("2.1.304")
        );
        assert_eq!(
            parse_npm_view_version("2.1.304\n").as_deref(),
            Some("2.1.304")
        );
        assert_eq!(parse_cli_version(""), None);
        assert_eq!(
            parse_cli_version("Error: claude native binary not installed"),
            None
        );
        assert_eq!(parse_npm_view_version("\n\n"), None);
        assert_eq!(parse_npm_view_version("npm ERR! 404"), None);
    }

    #[test]
    fn cli_update_prefix_is_the_dir_above_lib_node_modules() {
        let p = Path::new(
            "/home/u/.nvm/versions/node/v24.19.0/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe",
        );
        assert_eq!(
            npm_prefix_for_package_path(p),
            Some(PathBuf::from("/home/u/.nvm/versions/node/v24.19.0"))
        );
        // A native install is not under an npm prefix.
        assert_eq!(
            npm_prefix_for_package_path(Path::new("/home/u/.local/share/claude/versions/2.1.304")),
            None
        );
        // A different package under node_modules is not this one.
        assert_eq!(
            npm_prefix_for_package_path(Path::new(
                "/usr/lib/node_modules/@anthropic-ai/other/bin/claude"
            )),
            None
        );
    }

    // ---- The tick, end to end with fakes -----------------------------------

    struct FakeOps {
        opt_out: Option<String>,
        setting: Option<bool>,
        located: Option<NpmGlobalInstall>,
        /// Successive answers to `installed_version`; the last one repeats.
        installed: Mutex<Vec<Option<String>>>,
        published: Option<String>,
        installs: AtomicUsize,
        flag_seen_during_install: Mutex<Option<bool>>,
        flag: std::sync::Arc<UpdateFlag>,
    }

    impl FakeOps {
        fn new(installed: &[Option<&str>], published: Option<&str>) -> Self {
            Self {
                opt_out: None,
                setting: Some(true),
                located: Some(NpmGlobalInstall {
                    claude_bin: "/p/bin/claude".into(),
                    prefix: "/p".into(),
                    npm: "/p/bin/npm".into(),
                }),
                installed: Mutex::new(installed.iter().map(|v| v.map(str::to_string)).collect()),
                published: published.map(str::to_string),
                installs: AtomicUsize::new(0),
                flag_seen_during_install: Mutex::new(None),
                flag: std::sync::Arc::new(UpdateFlag::new()),
            }
        }
    }

    impl CliOps for FakeOps {
        fn opt_out_env(&self) -> Option<String> {
            self.opt_out.clone()
        }
        fn setting(&self) -> impl Future<Output = Option<bool>> + Send {
            let v = self.setting;
            async move { v }
        }
        fn locate(&self) -> impl Future<Output = Option<NpmGlobalInstall>> + Send {
            let v = self.located.clone();
            async move { v }
        }
        fn installed_version(
            &self,
            _: &NpmGlobalInstall,
        ) -> impl Future<Output = Option<String>> + Send {
            let mut q = self.installed.lock().unwrap();
            let v = if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            };
            async move { v }
        }
        fn published_version(
            &self,
            _: &NpmGlobalInstall,
        ) -> impl Future<Output = Option<String>> + Send {
            let v = self.published.clone();
            async move { v }
        }
        fn install(
            &self,
            _: &NpmGlobalInstall,
            _: &str,
        ) -> impl Future<Output = InstallRun> + Send {
            self.installs.fetch_add(1, Ordering::SeqCst);
            *self.flag_seen_during_install.lock().unwrap() = Some(self.flag.is_updating());
            async {
                InstallRun {
                    exit_code: Some(0),
                    duration: Duration::from_millis(5),
                    stderr_tail: String::new(),
                }
            }
        }
    }

    #[tokio::test]
    async fn cli_update_tick_installs_once_holds_the_flag_and_clears_it() {
        let ops = FakeOps::new(&[Some("2.1.303"), Some("2.1.304")], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let now = Instant::now();
        let r = tick(&ops, &mut state, &ops.flag, now).await;
        assert_eq!(ops.installs.load(Ordering::SeqCst), 1);
        assert_eq!(*ops.flag_seen_during_install.lock().unwrap(), Some(true));
        assert!(!ops.flag.is_updating(), "the flag drops after the install");
        assert!(!r.did_not_take);
        assert_eq!(r.reread.as_deref(), Some("2.1.304"));
        assert!(state.version_read_completed && state.install_method_supported);
        assert!(state.backoff_until.is_none());

        // Next tick: now equal, so nothing is installed.
        let r2 = tick(&ops, &mut state, &ops.flag, now).await;
        assert_eq!(r2.decision, Decision::Skip(SkipReason::UpToDate));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cli_update_tick_install_did_not_take_backs_off_24h() {
        // The re-read after the install still says 2.1.303.
        let ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let now = Instant::now();
        let r = tick(&ops, &mut state, &ops.flag, now).await;
        assert!(r.did_not_take);
        assert_eq!(ops.installs.load(Ordering::SeqCst), 1);
        assert_eq!(state.backoff_until, Some(now + DID_NOT_TAKE_BACKOFF));

        // Within the backoff: no install.
        let r2 = tick(&ops, &mut state, &ops.flag, now + Duration::from_secs(3600)).await;
        assert_eq!(r2.decision, Decision::Skip(SkipReason::BackingOff));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 1);

        // After it: tries again.
        let r3 = tick(
            &ops,
            &mut state,
            &ops.flag,
            now + DID_NOT_TAKE_BACKOFF + Duration::from_secs(1),
        )
        .await;
        assert!(matches!(r3.decision, Decision::Install { .. }));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cli_update_tick_opt_outs_and_unreadables_read_and_install_nothing() {
        // Opt-out env: not even the setting is consulted, nothing installed,
        // no version read completes.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.opt_out = Some("DISABLE_AUTOUPDATER".into());
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::OptOutEnv("DISABLE_AUTOUPDATER".into()))
        );
        assert!(!state.version_read_completed);

        // Setting off.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.setting = Some(false);
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::SettingOff));
        assert!(!state.version_read_completed);

        // Unsupported install method.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.located = None;
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::UnsupportedInstallMethod)
        );
        assert!(!state.install_method_supported);

        // Unreadable published: no install and the read is not complete.
        let ops = FakeOps::new(&[Some("2.1.303")], None);
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::PublishedUnreadable));
        assert!(!state.version_read_completed);

        // Unreadable installed.
        let ops = FakeOps::new(&[None], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::InstalledUnreadable));
        assert!(!state.version_read_completed);
    }

    // ---- The in-progress flag ----------------------------------------------

    #[tokio::test]
    async fn cli_update_wait_if_updating_returns_at_once_waits_and_times_out() {
        let flag = std::sync::Arc::new(UpdateFlag::new());
        assert_eq!(
            flag.wait_if_updating(Duration::from_secs(5)).await,
            UpdateWait::NotUpdating
        );

        // Held for the whole wait: times out.
        {
            let _held = flag.hold();
            assert!(flag.is_updating());
            let w = flag.wait_if_updating(Duration::from_millis(50)).await;
            assert!(matches!(w, UpdateWait::TimedOut(_)), "{w:?}");
        }
        assert!(!flag.is_updating());

        // Released part-way: finishes.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let holder = std::sync::Arc::clone(&flag);
        let raised = tokio::sync::Notify::new();
        let raised = std::sync::Arc::new(raised);
        let raised2 = std::sync::Arc::clone(&raised);
        let task = tokio::spawn(async move {
            let _held = holder.hold();
            raised2.notify_one();
            let _ = rx.await;
        });
        raised.notified().await;
        let waiter = {
            let flag = std::sync::Arc::clone(&flag);
            tokio::spawn(async move { flag.wait_if_updating(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send(());
        task.await.unwrap();
        let w = waiter.await.unwrap();
        assert!(matches!(w, UpdateWait::Finished(_)), "{w:?}");
    }

    // ---- The child env pair ------------------------------------------------

    #[test]
    fn cli_update_child_env_pairs_only_when_all_three_hold() {
        assert_eq!(
            child_env_pairs_for(test_gates::ON),
            &[("DISABLE_AUTOUPDATER", "1")]
        );
        for (arm, gate) in test_gates::OFF_ARMS {
            assert!(child_env_pairs_for(gate).is_empty(), "{arm}");
        }
    }

    /// Phase 3 seam test for `agent_runtime::finalize_headless_child_env`
    /// (kept here rather than in the 18k-line `agent_runtime.rs`): the headless
    /// seam carries `DISABLE_AUTOUPDATER=1` when the runner owns CLI updates,
    /// and nothing on each of the three off arms.
    #[test]
    fn headless_finalize_child_env_defers_cli_updates_to_the_runner() {
        let build = || {
            let mut cmd = tokio::process::Command::new("dummy");
            crate::agent_runtime::finalize_headless_child_env(
                &mut cmd,
                crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
                &crate::served_corpus::ServedCorpus::unknown("test"),
            );
            cmd.as_std()
                .get_envs()
                .find(|(k, _)| *k == "DISABLE_AUTOUPDATER")
                .and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()))
        };
        assert_eq!(with_gate(test_gates::ON, build).as_deref(), Some("1"));
        for (arm, gate) in test_gates::OFF_ARMS {
            assert_eq!(with_gate(gate, build), None, "off arm: {arm}");
        }
    }

    #[test]
    fn cli_update_child_env_pairs_without_a_tick_is_empty() {
        // No updater tick runs in unit tests, so the process-wide gate has no
        // completed version read and the seams set nothing by default.
        assert!(child_env_pairs().is_empty());
        with_gate(test_gates::ON, || {
            assert_eq!(child_env_pairs(), &[("DISABLE_AUTOUPDATER", "1")]);
        });
        assert!(child_env_pairs().is_empty(), "the override resets");
    }
}
