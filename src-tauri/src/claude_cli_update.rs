//! The runner-owned Claude Code CLI updater.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`,
//! Phase 2 (design decision D1, option C).
//!
//! # Why the runner owns updates
//!
//! Claude Code's own updater checks npm at start-up and every 30 minutes, and it
//! compares npm `latest` against the RUNNING version, not the one on disk. So
//! every long-lived session started on an older binary ran
//! `npm install --global @anthropic-ai/claude-code@<latest>` every 30 minutes for
//! as long as it lived (measured on merytshost in that plan's Phase 1b: 32
//! same-version installs in 16 minutes from 32 distinct stale sessions, 27 of
//! them runner-spawned). npm's reify moves `bin/claude` aside for each install,
//! and a `--resume` respawn that resolved `claude` inside that window died.
//!
//! The runner now exports `DISABLE_AUTOUPDATER=1` into its OWN environment at
//! start-up, so every runner-spawned `claude` inherits it and no runner-hosted
//! session runs its own updater (`session::claude_hook::export_auto_updater_off`,
//! lifecycle plan Phase 82). That stops the churn, and it also strands a box
//! whose `claude` sessions are all runner-hosted on whatever version is
//! installed. This module is the other half: ONE updater ([`run_periodic`]) that
//! at boot (after [`BOOT_DELAY`]) and then every [`TICK_INTERVAL`] reads the
//! installed version (`claude --version` through
//! [`crate::agent_runtime::resolve_claude_bin`]) and the published one (`npm view
//! @anthropic-ai/claude-code@<dist-tag> version`), and installs **only when the
//! published version is semver-NEWER** than the installed one, pinned to the
//! version it read, into the npm prefix that owns the resolved binary. Outside
//! the one-shot repair below, a same-version install cannot happen here, and
//! neither can a downgrade of an install that is ahead of the dist-tag. After
//! an install it reads the version again; if that is not the published version
//! it logs `cli-update: install-did-not-take` and backs off for
//! [`DID_NOT_TAKE_BACKOFF`], so a prefix mismatch is retried at most once a day.
//! An install that left NO readable CLI (killed mid-reify, or cut off by a
//! runner stop — recorded durably before npm runs) is repaired once by
//! reinstalling the published version through the remembered prefix.
//!
//! The dist-tag follows the CLI's own `autoUpdatesChannel` setting
//! ([`dist_tag_for_channel`]): `latest` (the default) and `stable` are served;
//! any other channel is not, and the tick says so in its log line.
//!
//! # The user's choices win
//!
//! - `claude_cli.runner_manages_cli_updates` (default `true`, on the AI settings
//!   page) is the in-product off-switch. Off means the runner installs nothing,
//!   which is the runner's behaviour before this module: runner-spawned sessions
//!   stay on the installed version until `claude update` is run by hand.
//! - When the USER's environment carried an opt-out variable
//!   ([`OPT_OUT_ENV_VARS`]) when the runner started, or a Claude config file
//!   opts out the way the CLI's own update check reads it (`autoUpdates: false`
//!   in `.claude.json`, or an opt-out variable in a `settings.json` `env` block;
//!   [`config_opt_out_in`]), the updater installs nothing.
//!   That environment is read BEFORE the runner exports its own
//!   `DISABLE_AUTOUPDATER=1` ([`capture_user_opt_out`]), so the runner's own
//!   export is never mistaken for the user's opt-out.
//! - A setting that cannot be read resolves to the last value that WAS read, and
//!   to "unknown" (no install) before any read has succeeded. It never falls
//!   back to the permissive default.
//!
//! # For spawns: the in-progress flag
//!
//! While an install runs, the updater holds an in-process flag
//! ([`wait_if_updating`]). Headless spawns through
//! `claude_cli_spawn::spawn_tokio_with_retry` wait it out. PTY panes and the
//! `--resume` respawn seam do not wait yet, and neither does a secondary runner
//! on the same box (the flag is in-process).
//!
//! # Scope
//!
//! - **Primary runner only.** Installing is a machine-wide duty, and two runners
//!   on one box would race each other's installs. `main.rs` starts
//!   [`run_periodic`] only when `instance::is_secondary()` is false.
//! - **npm-global installs on unix only** ([`locate_npm_global_install`]). A
//!   native install (`~/.local/share/claude/versions/...`) is not under an npm
//!   prefix. On Windows the running `claude.exe` is locked, so an npm install
//!   there fails while any session is open. Both are reported as an unsupported
//!   install method, and on those boxes nothing changes.

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{info, warn};

/// The npm package the updater manages.
pub const CLI_PACKAGE: &str = "@anthropic-ai/claude-code";

/// Variables that, when present in the USER's environment at runner start,
/// mean the user turned CLI updates off for everything — the same variables
/// Claude Code's own update check honours. The updater then installs nothing.
/// `DISABLE_AUTOUPDATER` spelled off (`0`, `false`, `no`, `off`) is not an
/// opt-out; the other two count whenever they are set non-empty, as the CLI
/// reads them.
pub const OPT_OUT_ENV_VARS: [&str; 3] = [
    "DISABLE_AUTOUPDATER",
    "DISABLE_UPDATES",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
];

/// First tick after boot. Long enough that boot-time spawns are not competing
/// with an install, short enough that a freshly started runner picks up a
/// release published while it was down within minutes.
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
        // `Ok(Err(_))` from `wait_for` means the sender is gone, so no install
        // can still be running. Reduced to a bool so the `Ref` the inner
        // result borrows from `rx` is dropped before `rx` is.
        let finished = tokio::time::timeout(timeout, rx.wait_for(|updating| !*updating))
            .await
            .is_ok();
        if finished {
            UpdateWait::Finished(started.elapsed())
        } else {
            UpdateWait::TimedOut(started.elapsed())
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
// Process-wide state published by the updater
// =============================================================================

const SETTING_UNKNOWN: u8 = 0;
const SETTING_OFF: u8 = 1;
const SETTING_ON: u8 = 2;

/// The last authoritative read of the setting (`SETTING_*`).
static LAST_KNOWN_SETTING: AtomicU8 = AtomicU8::new(SETTING_UNKNOWN);

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
    let (value, store) = resolve_setting(read, LAST_KNOWN_SETTING.load(Ordering::Acquire));
    LAST_KNOWN_SETTING.store(store, Ordering::Release);
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
    /// The user's environment at runner start carried this opt-out variable.
    OptOutEnv(String),
    /// A Claude config file opts out of updates (`autoUpdates: false`, or an
    /// opt-out variable in a `settings.json` `env` block).
    OptOutConfig(String),
    SettingOff,
    /// The setting has never been read successfully.
    SettingUnknown,
    UnsupportedInstallMethod,
    InstalledUnreadable,
    PublishedUnreadable,
    UpToDate,
    /// The installed version is NEWER than the dist-tag read (`latest` or
    /// `stable`) — an `@next` / `rc` install, or a hand install ahead of the
    /// tag. Never downgraded.
    InstalledNewer,
    /// One of the two versions is not semver, so "newer" cannot be decided.
    VersionsUncomparable,
    /// The CLI's own settings pick a release channel this updater does not
    /// serve (`autoUpdatesChannel` other than `latest` / `stable`, see
    /// [`dist_tag_for_channel`]). Nothing installs it.
    NonDefaultChannel(String),
    /// An earlier install did not take; installs are paused until the backoff ends.
    BackingOff,
}

impl SkipReason {
    fn as_str(&self) -> String {
        match self {
            Self::OptOutEnv(name) => format!("opt-out-env:{name}"),
            Self::OptOutConfig(what) => format!("opt-out-config:{what}"),
            Self::SettingOff => "setting-off".into(),
            Self::SettingUnknown => "setting-unknown".into(),
            Self::UnsupportedInstallMethod => "unsupported-install-method".into(),
            Self::InstalledUnreadable => "installed-unreadable".into(),
            Self::PublishedUnreadable => "published-unreadable".into(),
            Self::UpToDate => "up-to-date".into(),
            Self::InstalledNewer => "installed-newer".into(),
            Self::VersionsUncomparable => "versions-uncomparable".into(),
            Self::NonDefaultChannel(c) => format!("non-default-channel:{c}"),
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
    pub opt_out_config: Option<String>,
    pub setting: Option<bool>,
    pub install_method_supported: bool,
    pub installed: Option<String>,
    pub published: Option<String>,
    pub backing_off: bool,
    /// The CLI release channel configured in a Claude settings file that this
    /// updater cannot serve ([`dist_tag_for_channel`]).
    pub non_default_channel: Option<String>,
}

/// The decision table. The user's opt-outs come first, then whether this box
/// can be served, then the two reads, then the comparison. The backoff is
/// checked last so a tick inside it still reports both versions, and an
/// up-to-date box reads `up-to-date` rather than `backing-off`.
pub(crate) fn decide(i: &TickInputs) -> Decision {
    if let Some(name) = &i.opt_out_env {
        return Decision::Skip(SkipReason::OptOutEnv(name.clone()));
    }
    if let Some(what) = &i.opt_out_config {
        return Decision::Skip(SkipReason::OptOutConfig(what.clone()));
    }
    match i.setting {
        None => return Decision::Skip(SkipReason::SettingUnknown),
        Some(false) => return Decision::Skip(SkipReason::SettingOff),
        Some(true) => {}
    }
    // A channel with no dist-tag this updater reads is not served: installing
    // `latest` over a user who picked another channel would override their
    // choice.
    if let Some(channel) = &i.non_default_channel {
        return Decision::Skip(SkipReason::NonDefaultChannel(channel.clone()));
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
    // Install only an UPGRADE. A plain inequality would downgrade an `@next`
    // install (or anything ahead of `latest`) back to `latest` every tick.
    match (
        semver::Version::parse(installed),
        semver::Version::parse(published),
    ) {
        (Ok(have), Ok(want)) => match want.cmp(&have) {
            std::cmp::Ordering::Equal => return Decision::Skip(SkipReason::UpToDate),
            std::cmp::Ordering::Less => return Decision::Skip(SkipReason::InstalledNewer),
            std::cmp::Ordering::Greater => {}
        },
        _ if installed == published => return Decision::Skip(SkipReason::UpToDate),
        _ => return Decision::Skip(SkipReason::VersionsUncomparable),
    }
    if i.backing_off {
        return Decision::Skip(SkipReason::BackingOff);
    }
    Decision::Install {
        published: published.clone(),
    }
}

/// The user's opt-out as the runner's environment carried it BEFORE the runner
/// exported its own `DISABLE_AUTOUPDATER=1`. Set once, by [`capture_user_opt_out`].
static USER_OPT_OUT: OnceLock<Option<String>> = OnceLock::new();

/// Record the user's opt-out from the environment as it is NOW. `main` calls
/// this immediately BEFORE `session::claude_hook::export_auto_updater_off`:
/// read after that export, the runner's own `DISABLE_AUTOUPDATER=1` would look
/// like a user opt-out on every box and nothing would ever install. Only the
/// first call records.
pub fn capture_user_opt_out() {
    let _ = USER_OPT_OUT.set(opt_out_env_from(|k| std::env::var(k).ok()));
}

/// Pure: the opt-out a tick honours, from the [`capture_user_opt_out`] snapshot.
/// A snapshot that was never taken is UNKNOWN, and reads as an opt-out
/// (`uncaptured`) — never as "the user did not opt out".
fn user_opt_out_from(captured: Option<&Option<String>>) -> Option<String> {
    match captured {
        Some(snapshot) => snapshot.clone(),
        None => Some("uncaptured".to_string()),
    }
}

/// The first opt-out variable present and non-empty in the environment `get`
/// reads.
pub(crate) fn opt_out_env_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    OPT_OUT_ENV_VARS
        .iter()
        .find(|name| {
            get(name).is_some_and(|v| {
                if **name == "DISABLE_AUTOUPDATER" {
                    env_truthy(&v)
                } else {
                    !v.trim().is_empty()
                }
            })
        })
        .map(|name| name.to_string())
}

/// Pure: a CLI-config opt-out in one config dir's files, as Claude Code's own
/// update check reads them — `autoUpdates: false` in its `.claude.json`, or a
/// [`OPT_OUT_ENV_VARS`] entry in the `env` block of its `settings.json`.
/// `None` when neither file says so (or neither could be read).
pub(crate) fn config_opt_out_in(
    claude_json: Option<&str>,
    settings_json: Option<&str>,
) -> Option<String> {
    let parse = |t: Option<&str>| t.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok());
    if parse(claude_json)
        .is_some_and(|v| v.get("autoUpdates") == Some(&serde_json::Value::Bool(false)))
    {
        return Some("autoUpdates=false".to_string());
    }
    let env = parse(settings_json)?.get("env")?.clone();
    opt_out_env_from(|k| env.get(k).and_then(|v| v.as_str().map(str::to_string)))
        .map(|name| format!("settings.env.{name}"))
}

/// A set, non-empty variable that does not spell "off": `0`, `false`, `no`
/// and `off` (any case) are a user saying the opt-out is NOT on.
fn env_truthy(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty()
        && !["0", "false", "no", "off"]
            .iter()
            .any(|f| v.eq_ignore_ascii_case(f))
}

// =============================================================================
// The tick
// =============================================================================

/// An npm-global `claude` install the updater can serve.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The `autoUpdatesChannel` the first Claude settings file that names one
    /// names (`latest` included), or `None` when none does.
    fn non_default_channel(&self) -> impl Future<Output = Option<String>> + Send;
    /// A Claude config file's opt-out ([`config_opt_out_in`]), if any.
    fn config_opt_out(&self) -> impl Future<Output = Option<String>> + Send;
    /// Durably record (`Some`) or clear (`None`) an install in flight, so an
    /// install cut off by a runner stop is repaired after the next boot.
    fn record_pending_install(&self, pending: Option<&NpmGlobalInstall>);
    fn locate(&self) -> impl Future<Output = Option<NpmGlobalInstall>> + Send;
    fn installed_version(
        &self,
        install: &NpmGlobalInstall,
    ) -> impl Future<Output = Option<String>> + Send;
    /// The version npm publishes under `dist_tag` (`latest` or `stable`).
    fn published_version(
        &self,
        install: &NpmGlobalInstall,
        dist_tag: &str,
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
    /// An install of OURS left the CLI unreadable (killed mid-reify by the
    /// timeout, or failed with `bin/claude` moved aside). The next tick repairs
    /// it — once — through this remembered install, even though `locate` /
    /// `claude --version` can no longer find it; a repair that also fails
    /// starts the ordinary [`DID_NOT_TAKE_BACKOFF`].
    pub repair: Option<NpmGlobalInstall>,
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
        inputs.opt_out_config = ops.config_opt_out().await;
    }
    if inputs.opt_out_env.is_none() && inputs.opt_out_config.is_none() {
        inputs.setting = ops.setting().await;
    }
    let mut located = None;
    let mut dist_tag = "latest";
    if inputs.opt_out_env.is_none()
        && inputs.opt_out_config.is_none()
        && inputs.setting == Some(true)
    {
        match dist_tag_for_channel(ops.non_default_channel().await.as_deref()) {
            Ok(tag) => dist_tag = tag,
            Err(channel) => inputs.non_default_channel = Some(channel),
        }
        located = ops.locate().await;
        inputs.install_method_supported = located.is_some();
        if let Some(install) = &located {
            inputs.installed = ops.installed_version(install).await;
            // An unserved channel has no dist-tag to read.
            if inputs.non_default_channel.is_none() {
                inputs.published = ops.published_version(install, dist_tag).await;
            }
        }
    }

    let mut decision = decide(&inputs);
    let mut install_target = located.clone();
    let mut is_repair = false;
    if let Some(broken) = state.repair.clone() {
        let cannot_read = matches!(
            decision,
            Decision::Skip(SkipReason::InstalledUnreadable)
                | Decision::Skip(SkipReason::UnsupportedInstallMethod)
        );
        if cannot_read {
            // Our own install broke it: repair rather than skip forever.
            if let Some(p) = ops.published_version(&broken, dist_tag).await {
                inputs.published = Some(p.clone());
                decision = Decision::Install { published: p };
                install_target = Some(broken);
                is_repair = true;
            }
        } else if matches!(
            decision,
            Decision::Install { .. }
                | Decision::Skip(SkipReason::UpToDate)
                | Decision::Skip(SkipReason::InstalledNewer)
        ) {
            // Readable again by other means (someone reinstalled it, or npm
            // finished before the runner stopped): the durable record goes
            // too, or a later boot would act on it against a CLI that is fine.
            state.repair = None;
            ops.record_pending_install(None);
        }
    }
    let mut report = TickReport {
        decision: decision.clone(),
        installed: inputs.installed.clone(),
        published: inputs.published.clone(),
        install: None,
        reread: None,
        did_not_take: false,
    };

    if let (Decision::Install { published }, Some(install)) = (&decision, &install_target) {
        // Recorded durably BEFORE npm runs: an install cut off by a runner
        // stop leaves `bin/claude` moved aside, and `locate` cannot find it
        // after the next boot — the record is what lets that boot repair it.
        ops.record_pending_install(Some(install));
        let run = {
            let _held = flag.hold();
            ops.install(install, published).await
        };
        report.install = Some(run);
        report.reread = ops.installed_version(install).await;
        if report.reread.as_deref() != Some(published.as_str()) {
            report.did_not_take = true;
            if report.reread.is_none() && !is_repair {
                // The install left NO readable CLI: repair next tick (once)
                // instead of leaving it broken behind a 24 h backoff.
                state.repair = Some(install.clone());
            } else {
                state.repair = None;
                state.backoff_until = Some(now + DID_NOT_TAKE_BACKOFF);
            }
        } else {
            state.repair = None;
            state.backoff_until = None;
        }
        ops.record_pending_install(state.repair.as_ref());
    }
    // A tick that finds the CLI current has nothing pending: any backoff from
    // an earlier install that did not take is over.
    if decision == Decision::Skip(SkipReason::UpToDate) {
        state.backoff_until = None;
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

/// The Claude settings files whose `autoUpdatesChannel` this box's CLI reads:
/// `$CLAUDE_CONFIG_DIR` (the runner's own), the runner's resolved default
/// account dir, and `~/.claude`. Per-account dirs beyond the default are not
/// enumerated — a channel set only in a secondary account is not seen.
fn channel_settings_files() -> Vec<PathBuf> {
    config_dirs()
        .into_iter()
        .map(|d| d.join("settings.json"))
        .collect()
}

/// `~/.claude`, the home default account dir. The one ambient home read here.
fn dirs_home_default() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// The Claude config dirs this box's CLI reads: `$CLAUDE_CONFIG_DIR` (the
/// runner's own), the runner's resolved default account dir, and `~/.claude`,
/// deduplicated in that order. Account dirs beyond the default are NOT read,
/// so an opt-out set only in a secondary account (e.g. one account's
/// `autoUpdates: false`) is not seen — it never protected the shared binary
/// from the other accounts' own updaters either.
fn config_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d));
    }
    if let Some(d) = crate::ai_provider::get_resolved_config_dir() {
        dirs.push(PathBuf::from(d));
    }
    if let Some(h) = dirs_home_default() {
        dirs.push(h);
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// The npm dist-tag that carries a CLI release channel (`autoUpdatesChannel`,
/// the CLI's schema `latest | stable | rc`): `None` or `latest` reads `latest`,
/// and `stable` reads `stable` — both are published dist-tags of the package
/// (plan Phase 1e: `dist-tags: latest/stable 2.1.304, next 2.1.305`). Any other
/// channel is `Err(channel)`: no dist-tag is known to carry it, so the updater
/// installs nothing rather than override the user's choice with `latest`.
pub(crate) fn dist_tag_for_channel(channel: Option<&str>) -> Result<&'static str, String> {
    match channel {
        None | Some("latest") => Ok("latest"),
        Some("stable") => Ok("stable"),
        Some(other) => Err(other.to_string()),
    }
}

/// Pure: the `autoUpdatesChannel` one settings file's text names, `latest`
/// included. The CLI's schema is `"latest" | "stable" | "rc"`, optional,
/// default `latest`; `None` when the key is absent or null (the file names no
/// channel). A non-string value is reported as itself (and is unserved).
pub(crate) fn channel_in(settings_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(settings_json).ok()?;
    let ch = v.get("autoUpdatesChannel")?;
    match ch.as_str() {
        Some(name) => Some(name.to_string()),
        None if ch.is_null() => None,
        None => Some(ch.to_string()),
    }
}

/// The boot-armed periodic task: once after [`BOOT_DELAY`], then every
/// [`TICK_INTERVAL`]. Primary runner only (see the module docs).
pub async fn run_periodic() {
    tokio::time::sleep(BOOT_DELAY).await;
    let ops = RealOps;
    let mut state = UpdaterState {
        repair: load_pending_install(),
        ..UpdaterState::default()
    };
    if let Some(p) = &state.repair {
        info!(
            "cli-update: found a pending-install record for {} — the first tick repairs it only if the CLI there is unreadable",
            p.prefix.display()
        );
    }
    loop {
        let report = tick(&ops, &mut state, global_flag(), Instant::now()).await;
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
    s.get(start..).unwrap_or_default().to_string()
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

/// Where an install in flight is recorded: `<qontinui>/runner/cli-update-pending.json`.
fn pending_install_path() -> Option<PathBuf> {
    qontinui_runner_lib::ambient::runner_dir().map(|d| d.join("cli-update-pending.json"))
}

/// The install a previous runner left in flight, if it recorded one.
fn load_pending_install() -> Option<NpmGlobalInstall> {
    let text = std::fs::read_to_string(pending_install_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

struct RealOps;

impl CliOps for RealOps {
    fn opt_out_env(&self) -> Option<String> {
        user_opt_out_from(USER_OPT_OUT.get())
    }

    async fn setting(&self) -> Option<bool> {
        tokio::task::spawn_blocking(runner_manages_cli_updates)
            .await
            .ok()
            .flatten()
    }

    async fn config_opt_out(&self) -> Option<String> {
        tokio::task::spawn_blocking(|| {
            let home_default = dirs_home_default();
            config_dirs().iter().find_map(|d| {
                let settings = std::fs::read_to_string(d.join("settings.json")).ok();
                // The global config of the HOME default `~/.claude` is
                // `~/.claude.json` (beside the dir) without `CLAUDE_CONFIG_DIR`
                // and `~/.claude/.claude.json` with it: either opting out counts.
                let mut candidates = vec![d.join(".claude.json")];
                if home_default.as_deref() == Some(d.as_path()) {
                    candidates.push(d.with_extension("json"));
                }
                candidates.iter().find_map(|c| {
                    let claude_json = std::fs::read_to_string(c).ok();
                    config_opt_out_in(claude_json.as_deref(), settings.as_deref())
                })
            })
        })
        .await
        .ok()
        .flatten()
    }

    fn record_pending_install(&self, pending: Option<&NpmGlobalInstall>) {
        let Some(path) = pending_install_path() else {
            return;
        };
        let result = match pending {
            Some(p) => serde_json::to_string(p)
                .map_err(std::io::Error::other)
                .and_then(|json| std::fs::write(&path, json)),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        if let Err(e) = result {
            warn!(
                "cli-update: could not record the pending install at {}: {e}",
                path.display()
            );
        }
    }

    async fn non_default_channel(&self) -> Option<String> {
        tokio::task::spawn_blocking(|| {
            // The FIRST file that names a channel decides (the runner's own
            // `$CLAUDE_CONFIG_DIR` before the default account, before
            // `~/.claude`), `latest` included.
            channel_settings_files()
                .iter()
                .filter_map(|f| std::fs::read_to_string(f).ok())
                .find_map(|text| channel_in(&text))
        })
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
            cmd.arg("--version")
                .env(crate::session::claude_hook::DISABLE_AUTOUPDATER_ENV, "1");
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
        dist_tag: &str,
    ) -> impl Future<Output = Option<String>> + Send {
        let npm = install.npm.clone();
        let path = npm_child_path(&install.prefix);
        let spec = format!("{CLI_PACKAGE}@{dist_tag}");
        async move {
            let mut cmd = crate::process_helpers::tokio_no_window(&npm);
            cmd.args(["view", spec.as_str(), "version"])
                .env("PATH", path);
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
            opt_out_config: None,
            setting: Some(true),
            install_method_supported: true,
            installed: Some(installed.to_string()),
            published: Some(published.to_string()),
            backing_off: false,
            non_default_channel: None,
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
        assert_eq!(
            opt_out_env_from(
                |k| (k == "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").then(|| "1".to_string())
            )
            .as_deref(),
            Some("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
        );
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
        channel: Option<String>,
        /// Every dist-tag `published_version` was asked for.
        asked_tags: Mutex<Vec<String>>,
        config_opt_out: Option<String>,
        /// Every `record_pending_install` call, in order.
        pending: Mutex<Vec<Option<NpmGlobalInstall>>>,
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
                channel: None,
                asked_tags: Mutex::new(Vec::new()),
                config_opt_out: None,
                pending: Mutex::new(Vec::new()),
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
        fn non_default_channel(&self) -> impl Future<Output = Option<String>> + Send {
            let v = self.channel.clone();
            async move { v }
        }
        fn config_opt_out(&self) -> impl Future<Output = Option<String>> + Send {
            let v = self.config_opt_out.clone();
            async move { v }
        }
        fn record_pending_install(&self, pending: Option<&NpmGlobalInstall>) {
            self.pending.lock().unwrap().push(pending.cloned());
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
            dist_tag: &str,
        ) -> impl Future<Output = Option<String>> + Send {
            self.asked_tags.lock().unwrap().push(dist_tag.to_string());
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
        assert_eq!(r.installed.as_deref(), Some("2.1.303"));
        assert_eq!(*ops.asked_tags.lock().unwrap(), vec!["latest".to_string()]);
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
        // no version is read.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.opt_out = Some("DISABLE_AUTOUPDATER".into());
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::OptOutEnv("DISABLE_AUTOUPDATER".into()))
        );
        assert!(r.installed.is_none() && r.published.is_none());

        // Setting off.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.setting = Some(false);
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::SettingOff));
        assert!(r.installed.is_none() && r.published.is_none());

        // Unsupported install method.
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.located = None;
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::UnsupportedInstallMethod)
        );
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);

        // Unreadable published: no install and the read is not complete.
        let ops = FakeOps::new(&[Some("2.1.303")], None);
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::PublishedUnreadable));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);

        // Unreadable installed.
        let ops = FakeOps::new(&[None], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::InstalledUnreadable));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);
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

    // ---- Review findings 5 and 6 -------------------------------------------

    #[test]
    fn cli_update_decide_never_downgrades_a_newer_install() {
        assert_eq!(
            decide(&inputs("2.2.0-next.3", "2.1.304")),
            Decision::Skip(SkipReason::InstalledNewer)
        );
        assert_eq!(
            decide(&inputs("2.1.305", "2.1.304")),
            Decision::Skip(SkipReason::InstalledNewer)
        );
        // Numeric, not lexical: 2.1.1000 is newer than 2.1.999.
        assert_eq!(
            decide(&inputs("2.1.999", "2.1.1000")),
            Decision::Install {
                published: "2.1.1000".into()
            }
        );
        assert_eq!(
            decide(&inputs("2.1.1000", "2.1.999")),
            Decision::Skip(SkipReason::InstalledNewer)
        );
        assert_eq!(
            decide(&inputs("2.1", "2.1.304")),
            Decision::Skip(SkipReason::VersionsUncomparable)
        );
    }

    #[test]
    fn cli_update_decide_defers_to_an_unserved_channel() {
        let i = TickInputs {
            non_default_channel: Some("rc".into()),
            ..inputs("2.1.303", "2.1.304")
        };
        assert_eq!(
            decide(&i),
            Decision::Skip(SkipReason::NonDefaultChannel("rc".into()))
        );
    }

    #[test]
    fn cli_update_channel_maps_to_its_dist_tag_or_is_unserved() {
        assert_eq!(dist_tag_for_channel(None), Ok("latest"));
        assert_eq!(dist_tag_for_channel(Some("latest")), Ok("latest"));
        assert_eq!(dist_tag_for_channel(Some("stable")), Ok("stable"));
        assert_eq!(dist_tag_for_channel(Some("rc")), Err("rc".to_string()));
    }

    /// The user's opt-out is read from the snapshot taken BEFORE the runner
    /// exported its own `DISABLE_AUTOUPDATER=1`; a snapshot never taken is an
    /// opt-out (UNKNOWN), never "no opt-out".
    #[test]
    fn cli_update_opt_out_reads_the_pre_export_snapshot() {
        assert_eq!(user_opt_out_from(Some(&None)), None);
        assert_eq!(
            user_opt_out_from(Some(&Some("DISABLE_UPDATES".to_string()))).as_deref(),
            Some("DISABLE_UPDATES")
        );
        assert_eq!(user_opt_out_from(None).as_deref(), Some("uncaptured"));
    }

    #[test]
    fn cli_update_reads_the_auto_updates_channel_key() {
        assert_eq!(channel_in("{}"), None);
        assert_eq!(channel_in(r#"{"autoUpdatesChannel":null}"#), None);
        assert_eq!(
            channel_in(r#"{"autoUpdatesChannel":"latest"}"#).as_deref(),
            Some("latest"),
            "an explicit latest is a channel, so it decides over a later file"
        );
        assert_eq!(
            channel_in(r#"{"autoUpdatesChannel":"stable"}"#).as_deref(),
            Some("stable")
        );
        assert_eq!(
            channel_in(r#"{"autoUpdatesChannel":"rc","model":"x"}"#).as_deref(),
            Some("rc")
        );
        assert_eq!(channel_in("not json"), None);
    }

    /// The CLI's own config opt-outs win too: `autoUpdates: false` in
    /// `.claude.json`, or an opt-out variable in `settings.json`'s `env`.
    #[test]
    fn cli_update_reads_the_cli_config_opt_outs() {
        assert_eq!(config_opt_out_in(None, None), None);
        assert_eq!(
            config_opt_out_in(Some(r#"{"autoUpdates":true}"#), Some("{}")),
            None
        );
        assert_eq!(
            config_opt_out_in(Some(r#"{"autoUpdates":false}"#), None).as_deref(),
            Some("autoUpdates=false")
        );
        assert_eq!(
            config_opt_out_in(None, Some(r#"{"env":{"DISABLE_AUTOUPDATER":"1"}}"#)).as_deref(),
            Some("settings.env.DISABLE_AUTOUPDATER")
        );
        assert_eq!(
            config_opt_out_in(None, Some(r#"{"env":{"DISABLE_AUTOUPDATER":"0"}}"#)),
            None,
            "spelled off is not an opt-out"
        );
        assert_eq!(
            config_opt_out_in(None, Some(r#"{"env":{"DISABLE_UPDATES":"0"}}"#)).as_deref(),
            Some("settings.env.DISABLE_UPDATES"),
            "the CLI reads DISABLE_UPDATES as set whenever it is non-empty"
        );
    }

    #[tokio::test]
    async fn cli_update_a_config_opt_out_reads_and_installs_nothing() {
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.config_opt_out = Some("autoUpdates=false".into());
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::OptOutConfig("autoUpdates=false".into()))
        );
        assert!(r.installed.is_none() && r.published.is_none());
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);
    }

    /// An install is recorded durably BEFORE npm runs and cleared once it took,
    /// so an install cut off by a runner stop is repaired after the next boot.
    #[tokio::test]
    async fn cli_update_records_an_install_in_flight_and_clears_it_when_it_took() {
        let ops = FakeOps::new(&[Some("2.1.303"), Some("2.1.304")], Some("2.1.304"));
        let mut state = UpdaterState::default();
        tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        let calls = ops.pending.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0], ops.located, "recorded before the install");
        assert_eq!(calls[1], None, "cleared after an install that took");

        // An install that leaves the CLI unreadable stays recorded.
        let ops = FakeOps::new(&[Some("2.1.303"), None], Some("2.1.304"));
        let mut state = UpdaterState::default();
        tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        let calls = ops.pending.lock().unwrap().clone();
        assert_eq!(calls.last().cloned().flatten(), ops.located);
    }

    /// A pending-install record a previous runner left behind is cleared when
    /// the first tick finds the CLI healthy, so no later boot repairs (installs
    /// into) a prefix the user may have deliberately emptied.
    #[tokio::test]
    async fn cli_update_a_healthy_cli_clears_a_stale_pending_install_record() {
        let ops = FakeOps::new(&[Some("2.1.304")], Some("2.1.304"));
        let mut state = UpdaterState {
            repair: ops.located.clone(),
            ..UpdaterState::default()
        };
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::UpToDate));
        assert!(state.repair.is_none());
        assert_eq!(*ops.pending.lock().unwrap(), vec![None]);
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cli_update_an_unserved_channel_installs_nothing() {
        let mut ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        ops.channel = Some("rc".into());
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Skip(SkipReason::NonDefaultChannel("rc".into()))
        );
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);
        assert!(
            ops.asked_tags.lock().unwrap().is_empty(),
            "no dist-tag is read for a channel nothing serves"
        );
    }

    /// A user on the `stable` channel is served from the `stable` dist-tag, not
    /// `latest`.
    #[tokio::test]
    async fn cli_update_the_stable_channel_reads_and_installs_the_stable_dist_tag() {
        let mut ops = FakeOps::new(&[Some("2.1.303"), Some("2.1.304")], Some("2.1.304"));
        ops.channel = Some("stable".into());
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(
            r.decision,
            Decision::Install {
                published: "2.1.304".into()
            }
        );
        assert_eq!(*ops.asked_tags.lock().unwrap(), vec!["stable".to_string()]);
        assert_eq!(ops.installs.load(Ordering::SeqCst), 1);
    }

    // ---- Round 2 nits ------------------------------------------------------

    #[test]
    fn cli_update_an_opt_out_spelled_off_is_not_an_opt_out() {
        for off in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert_eq!(
                opt_out_env_from(|k| (k == "DISABLE_AUTOUPDATER").then(|| off.to_string())),
                None,
                "{off:?}"
            );
        }
        assert_eq!(
            opt_out_env_from(|k| (k == "DISABLE_AUTOUPDATER").then(|| "1".to_string())),
            Some("DISABLE_AUTOUPDATER".into())
        );
    }

    #[tokio::test]
    async fn cli_update_uncomparable_versions_install_nothing() {
        let ops = FakeOps::new(&[Some("2.1")], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let r = tick(&ops, &mut state, &ops.flag, Instant::now()).await;
        assert_eq!(r.decision, Decision::Skip(SkipReason::VersionsUncomparable));
        assert_eq!(ops.installs.load(Ordering::SeqCst), 0);
    }

    // ---- Round 3 -----------------------------------------------------------

    /// Nit: a tick inside the backoff that finds the CLI up to date ends the
    /// backoff (nothing is pending).
    #[tokio::test]
    async fn cli_update_an_up_to_date_tick_ends_the_backoff() {
        let ops = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let now = Instant::now();
        tick(&ops, &mut state, &ops.flag, now).await;
        assert!(
            state.backoff_until.is_some(),
            "fixture: did-not-take backs off"
        );
        let ops = FakeOps::new(&[Some("2.1.304")], Some("2.1.304"));
        tick(&ops, &mut state, &ops.flag, now + Duration::from_secs(60)).await;
        assert!(state.backoff_until.is_none());
    }

    // ---- Round 4 -----------------------------------------------------------

    /// Finding 4: an install killed mid-reify leaves the CLI unreadable (and
    /// `locate` blind to it). The next tick REPAIRS through the remembered
    /// install instead of skipping forever.
    #[tokio::test]
    async fn cli_update_an_install_that_leaves_the_cli_unreadable_is_repaired_next_tick() {
        let ops = FakeOps::new(&[Some("2.1.303"), None], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let now = Instant::now();
        let r = tick(&ops, &mut state, &ops.flag, now).await;
        assert!(r.did_not_take && r.reread.is_none());
        assert!(state.repair.is_some(), "a repair is pending");
        assert!(
            state.backoff_until.is_none(),
            "no 24 h backoff over a broken CLI"
        );

        let mut ops2 = FakeOps::new(&[Some("2.1.304")], Some("2.1.304"));
        ops2.located = None; // `bin/claude` is gone: locate cannot find it
        let r2 = tick(&ops2, &mut state, &ops2.flag, now + TICK_INTERVAL).await;
        assert_eq!(
            r2.decision,
            Decision::Install {
                published: "2.1.304".into()
            }
        );
        assert_eq!(ops2.installs.load(Ordering::SeqCst), 1, "repaired");
        assert!(state.repair.is_none());
    }

    #[tokio::test]
    async fn cli_update_a_repair_that_also_fails_backs_off() {
        let ops = FakeOps::new(&[Some("2.1.303"), None], Some("2.1.304"));
        let mut state = UpdaterState::default();
        let now = Instant::now();
        tick(&ops, &mut state, &ops.flag, now).await;
        let mut ops2 = FakeOps::new(&[None], Some("2.1.304"));
        ops2.located = None;
        tick(&ops2, &mut state, &ops2.flag, now + TICK_INTERVAL).await;
        assert_eq!(ops2.installs.load(Ordering::SeqCst), 1);
        assert!(state.repair.is_none(), "one repair, not a loop");
        assert!(state.backoff_until.is_some());
        // A readable, older CLI on a located install: only the backoff can
        // stop this tick from installing.
        let ops3 = FakeOps::new(&[Some("2.1.303")], Some("2.1.304"));
        let r3 = tick(&ops3, &mut state, &ops3.flag, now + TICK_INTERVAL * 2).await;
        assert_eq!(r3.decision, Decision::Skip(SkipReason::BackingOff));
        assert_eq!(
            ops3.installs.load(Ordering::SeqCst),
            0,
            "bounded by the backoff"
        );
    }
}
