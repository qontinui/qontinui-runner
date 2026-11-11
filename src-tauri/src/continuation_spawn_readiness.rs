//! Gate-continuation spawn readiness and the claim boundary.
//!
//! Plan `2026-11-04-gate-continuation-defers-unclaimed-on-unlaunchable-claude-cli-and-unsent-claim`
//! (the port of qontinui-runner#1925's unlanded residual onto current main).
//!
//! A gate continuation is CLAIMED at coord (step 2 of
//! `agent_runtime::run_gate_continuation_inner`) and only then reaches the
//! spawn. A spawn that fails after the claim is recorded `spawn_failed`, and
//! the continuation is burned. The pieces here move two local conditions to
//! the right side of that claim, and stop a claim that never left the box from
//! burning the row:
//!
//! - [`claude_cli_readiness`] / [`claim_if_spawn_ready`] (D2/D4): the `claude`
//!   CLI the spawn will exec is probed BEFORE the claim, over the SAME resolved
//!   string the spawn is then handed, and an unlaunchable CLI defers the row
//!   UNCLAIMED with a `claude_cli_unavailable:<kind>` stamp.
//! - [`ClaimSent`] / [`classify_claim_send_error`] (D5): how far a claim that
//!   ERRORED provably got. Only [`ClaimSent::NotSent`] lets a later seam
//!   refusal defer instead of posting `spawn_failed`.
//! - [`SeamRefusal`] / [`classify_seam_refusal`] / [`failed_spawn_disposition`]
//!   (D5 + Phase 4): what a failed presentation's error says about the seam,
//!   recognised by the resource guard's FROZEN wire prefix, by a typed headless
//!   [`crate::claude_cli_spawn::CliSpawnFailure`], or by RE-PROBING the CLI —
//!   never by grepping a foreign message format.
//! - [`outcome_detail`] (Phase 4): a post-claim CLI failure's `spawn_failed`
//!   detail leads with `claude_cli_unavailable:<kind>: `, the head coord's
//!   `continuation_spawn_retry::classify_spawn_failure` re-drives.
//!
//! Everything here is pure or takes its side effects as injected closures, so
//! the unit tests drive the production functions rather than copies of them.

use std::future::Future;
use std::io;
use std::path::Path;

use tracing::warn;

use crate::agent_runtime::ConsumeTarget;
use crate::claude_cli_spawn::CLI_UNAVAILABLE_HEAD;
use crate::resource_guard::{SpawnGate, CRITICAL_REFUSAL_PREFIX};

// ---------------------------------------------------------------------------
// D2/D4 — is the resolved `claude` launchable?
// ---------------------------------------------------------------------------

/// Why the `claude` CLI could not be launched, in the runner's ONE vocabulary
/// for it (`claude_cli_spawn::transient_kind`'s tokens, plus two of its own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliKind {
    /// A token [`crate::claude_cli_spawn::transient_kind`] names:
    /// `enoent` | `eacces` | `enoexec` | `etxtbsy` (and `sharing_violation` on
    /// Windows).
    Known(&'static str),
    /// Nothing on the runner's PATH resolved (unix only — see
    /// [`CliReadiness::Unresolved`]).
    NotOnPath,
    /// An OS error outside the shared table: `errno_<n>`.
    Errno(i32),
    /// No OS code and no recognisable kind.
    Unknown,
}

impl CliKind {
    /// Classify an exec-predicate failure through the shared table first, so a
    /// reinstall-window `ENOENT` reads `enoent` here exactly as it does in the
    /// headless retry's `claude_cli_unavailable:` head.
    pub(crate) fn from_io(e: &io::Error) -> Self {
        if let Some(kind) = crate::claude_cli_spawn::transient_kind(e) {
            return CliKind::Known(kind);
        }
        match e.raw_os_error() {
            Some(code) => CliKind::Errno(code),
            None => CliKind::Unknown,
        }
    }

    /// The wire token after `claude_cli_unavailable:`.
    pub(crate) fn token(&self) -> String {
        match self {
            CliKind::Known(kind) => (*kind).to_string(),
            CliKind::NotOnPath => "not_on_path".to_string(),
            CliKind::Errno(code) => format!("errno_{code}"),
            CliKind::Unknown => "unknown".to_string(),
        }
    }
}

/// The deferred-stamp reason (and Phase 4 detail head) for an unlaunchable
/// CLI: `claude_cli_unavailable:<kind>`. The `<class>:<detail>` grammar of the
/// other stamps in `agent_runtime::post_continuation_deferred`'s table, and the
/// same head [`CLI_UNAVAILABLE_HEAD`] the headless retry already writes.
pub(crate) fn claude_cli_unavailable_stamp_reason(kind: &CliKind) -> String {
    format!("{CLI_UNAVAILABLE_HEAD}{}", kind.token())
}

/// Is the `claude` a continuation would exec launchable?
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliReadiness {
    /// The resolved absolute path passes the exec seam's predicate.
    Ready,
    /// The resolved absolute path fails it.
    Unlaunchable(CliKind),
    /// A bare name with no real launchable PATH entry. On unix that is a
    /// `resolve_claude_bin` miss (it skips the shim dirs and npm's
    /// placeholder), so the continuation defers; a bare name that DOES have one
    /// (the headless arm's `launchable_or_bare` demotion) reads
    /// [`CliReadiness::Ready`] via [`probe_claude_cli_with_path`]. On Windows a bare name is the npm-only `.cmd`
    /// install shape `resolve_claude_bin` deliberately falls through on, so it
    /// PROCEEDS — fail-open on UNKNOWN.
    Unresolved,
}

/// PURE over the injected predicate: is `resolved` launchable?
///
/// Production passes [`exec_seam_access`] over the SAME string the spawn is
/// then handed, so the probe and the spawn name one file by construction
/// (D-B). A non-absolute `resolved` is [`CliReadiness::Unresolved`].
pub(crate) fn claude_cli_readiness(
    resolved: &str,
    access: &dyn Fn(&Path) -> io::Result<()>,
) -> CliReadiness {
    let path = Path::new(resolved);
    if !path.is_absolute() {
        return CliReadiness::Unresolved;
    }
    match access(path) {
        Ok(()) => CliReadiness::Ready,
        Err(e) => CliReadiness::Unlaunchable(CliKind::from_io(&e)),
    }
}

/// The exec seam's own predicate (unix): `access(X_OK)` — portable-pty's check,
/// which follows symlinks, so a dangling nvm link reads `ENOENT` — and then a
/// directory reads `EACCES`, because `access(X_OK)` succeeds on a searchable
/// directory that `execve` would refuse ("never pick a directory as claude",
/// #1925 round 3).
#[cfg(unix)]
pub(crate) fn exec_seam_access(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::ENOENT))?;
    // SAFETY: `c_path` is a valid NUL-terminated C string that outlives the
    // call; `access` only reads it.
    let rc = unsafe { libc::access(c_path.as_ptr(), libc::X_OK) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if path.is_dir() {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(())
}

/// The exec seam's own predicate (Windows): `resolve_claude_bin` only ever
/// returns an `.exe`/`.com` there, so "is it a regular file" is the check
/// `CreateProcessW` would make first. A directory or a missing file reads
/// `enoent`.
#[cfg(not(unix))]
pub(crate) fn exec_seam_access(path: &Path) -> io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
}

/// [`claude_cli_readiness`] with the production predicate. Blocking (one
/// `access` + one `stat`); cheap enough to share the resolve's blocking task.
pub(crate) fn probe_claude_cli(resolved: &str) -> CliReadiness {
    probe_claude_cli_with_path(resolved, std::env::var_os("PATH").as_deref())
}

/// [`probe_claude_cli`] over an explicit `PATH` value, so tests never read the
/// real process environment.
pub(crate) fn probe_claude_cli_with_path(
    resolved: &str,
    path_var: Option<&std::ffi::OsStr>,
) -> CliReadiness {
    match claude_cli_readiness(resolved, &exec_seam_access) {
        // A bare name is what the headless arm execs after `launchable_or_bare`
        // rejected a non-executable resolution; `execvp` searches PATH for it,
        // so it is launchable exactly when some PATH entry holds a REAL
        // launchable file of that name. "Real" uses `resolve_claude_bin`'s own
        // exclusions, so a resolve MISS (only shim dirs or npm's placeholder
        // stub hold a `claude`) stays Unresolved and defers as `not_on_path`.
        CliReadiness::Unresolved if bare_name_on_path(resolved, path_var) => CliReadiness::Ready,
        other => other,
    }
}

/// `execvp`'s search with the exec seam's predicate and `resolve_claude_bin`'s
/// exclusions: does any non-empty, non-shim entry of `path_var` hold a
/// launchable file named `name` that is not npm's placeholder? Unix only — on
/// Windows a bare name already proceeds ([`CliReadiness::Unresolved`]).
#[cfg(unix)]
pub(crate) fn bare_name_on_path(name: &str, path_var: Option<&std::ffi::OsStr>) -> bool {
    use crate::install_effects_producer::intercept::shim_materializer::{
        IDENTITY_DIR_PREFIX, SHIM_DIR_PREFIX,
    };
    use qontinui_runner_lib::intercept_core::npm_placeholder::is_npm_placeholder;
    if name.is_empty() || name.contains('/') {
        return false;
    }
    let is_shim_dir = |d: &Path| {
        d.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(IDENTITY_DIR_PREFIX) || n.starts_with(SHIM_DIR_PREFIX))
    };
    path_var.is_some_and(|p| {
        std::env::split_paths(p).any(|d| {
            if d.as_os_str().is_empty() || is_shim_dir(&d) {
                return false;
            }
            let cand = d.join(name);
            exec_seam_access(&cand).is_ok() && !is_npm_placeholder(&cand)
        })
    })
}

#[cfg(not(unix))]
pub(crate) fn bare_name_on_path(_name: &str, _path_var: Option<&std::ffi::OsStr>) -> bool {
    false
}

/// The kind a readiness reading defers on, per OS — `None` proceeds.
fn deferral_kind(readiness: &CliReadiness) -> Option<CliKind> {
    match readiness {
        CliReadiness::Ready => None,
        CliReadiness::Unlaunchable(kind) => Some(kind.clone()),
        CliReadiness::Unresolved => unresolved_deferral_kind(),
    }
}

#[cfg(unix)]
fn unresolved_deferral_kind() -> Option<CliKind> {
    Some(CliKind::NotOnPath)
}

#[cfg(not(unix))]
fn unresolved_deferral_kind() -> Option<CliKind> {
    None
}

/// What [`claim_if_spawn_ready`] decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PreClaim<D> {
    /// The CLI is not launchable: deferred UNCLAIMED through the injected
    /// deferral (which stamps and releases the local claim). The caller
    /// returns without spawning.
    Deferred { stamp: String },
    /// Proceed. `decision` is coord's answer to the consume claim for a
    /// [`ConsumeTarget::Gate`] — the only target with a coord claim — and
    /// `None` for the two that post none.
    Ready { decision: Option<D> },
}

/// The step-1b-to-step-2 transition of `run_gate_continuation_inner`: check
/// that the CLI is launchable, and only then post the consume CLAIM.
///
/// A claimed continuation whose spawn then fails is `spawn_failed` and, for the
/// PTY holder's CLI text, never re-driven (coord finding `22dff795`). A missing
/// CLI is a LOCAL condition, so like every other local guard it is checked
/// before the claim and defers UNCLAIMED. The check narrows the race with the
/// seam (the CLI can still vanish between here and the exec); Phase 4's detail
/// head covers what gets through.
///
/// - [`ConsumeTarget::None`] (the legacy no-`gate_id` path) ALWAYS proceeds:
///   it has no re-delivery, so a deferral there would be a silent drop.
/// - [`ConsumeTarget::Gate`] defers on an unready CLI, else calls `post_claim`
///   exactly once.
/// - [`ConsumeTarget::Dispatch`] defers on an unready CLI (the injected
///   deferral only releases its local claim — it has no coord stamp), else
///   proceeds WITHOUT calling `post_claim`: a unit dispatch has no claim.
///
/// Production passes `post_continuation_claim` and
/// `defer_continuation_unclaimed` in one statement
/// (`agent_runtime::pre_claim_step`), so a test drives this fn and counts both.
pub(crate) async fn claim_if_spawn_ready<D, C, Fut, F, FFut>(
    consume_target: ConsumeTarget,
    claude_bin: &str,
    readiness: &CliReadiness,
    post_claim: C,
    defer_unclaimed: F,
) -> PreClaim<D>
where
    C: FnOnce(uuid::Uuid) -> Fut,
    Fut: Future<Output = D>,
    F: FnOnce(String) -> FFut,
    FFut: Future<Output = ()>,
{
    if !matches!(consume_target, ConsumeTarget::None) {
        if let Some(kind) = deferral_kind(readiness) {
            let stamp = claude_cli_unavailable_stamp_reason(&kind);
            warn!(
                "agent_runtime: gate-continuation deferred before its claim: the claude CLI \
                 at {claude_bin} is not launchable ({}) — row left pending, re-delivered once \
                 it is back (stamp={stamp}, consume_target={consume_target:?})",
                kind.token()
            );
            defer_unclaimed(stamp.clone()).await;
            return PreClaim::Deferred { stamp };
        }
    }
    let decision = match consume_target {
        ConsumeTarget::Gate(gate_id, _) => Some(post_claim(gate_id).await),
        ConsumeTarget::Dispatch(_) | ConsumeTarget::None => None,
    };
    PreClaim::Ready { decision }
}

// ---------------------------------------------------------------------------
// D5 — how far an errored claim got
// ---------------------------------------------------------------------------

/// How far a consume claim that ERRORED provably got. Structural, never
/// string-matched.
///
/// It exists for one decision: whether a spawn that then fails at the seam may
/// be deferred UNCLAIMED. That is only safe when coord cannot have recorded the
/// claim. A claim that DID land leaves the row consumed, and deferring it would
/// strand it consumed-with-no-outcome — strictly worse than the `spawn_failed`
/// coord's re-drive can recover. So anything short of proof is
/// [`ClaimSent::Unknown`], and `Unknown` changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimSent {
    /// No request bytes reached a server: a connect failure on the requested
    /// URL itself, or no shared coord client to send with.
    NotSent,
    /// The claim may have reached coord: a timeout, a send error after the
    /// connection was made, a connect failure on a redirect target, or any
    /// HTTP status coord answered with.
    Unknown,
}

/// Classify a claim POST's send error. PURE over the three facts it needs.
///
/// `is_connect` is the one reqwest error class raised before any request bytes
/// are written — but only for the connection it names. The shared coord client
/// follows redirects (a per-CLIENT policy), so a connect failure on a REDIRECT
/// target means the original POST did reach a server first (#1925 review round
/// 1). `NotSent` therefore also requires the failing URL to be the one this
/// request was sent to; a failing URL that differs, or is absent, is
/// [`ClaimSent::Unknown`].
pub(crate) fn classify_claim_send_error(
    is_connect: bool,
    failed_url: Option<&reqwest::Url>,
    requested_url: &reqwest::Url,
) -> ClaimSent {
    if is_connect && failed_url == Some(requested_url) {
        ClaimSent::NotSent
    } else {
        ClaimSent::Unknown
    }
}

// ---------------------------------------------------------------------------
// D5 + Phase 4 — what a failed presentation says about the seam
// ---------------------------------------------------------------------------

/// The error a failed terminal-session create returns. Its `Display` is the
/// seam's text verbatim — so `e.to_string()`, and with it the `spawn_failed`
/// detail, is byte-identical to the untyped error it replaced — and it carries
/// the exact `claude` string THIS attempt exec'd, so step 4 re-probes the file
/// the spawn named rather than a re-resolution (D-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalSeamFailure {
    pub(crate) claude_bin: String,
    pub(crate) message: String,
}

impl std::fmt::Display for TerminalSeamFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TerminalSeamFailure {}

/// A spawn-seam refusal an unattended continuation may RETRY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SeamRefusal {
    /// The resource guard's CRITICAL refusal (detail starts with
    /// [`CRITICAL_REFUSAL_PREFIX`]). `stamp` is the deferral reason, built by
    /// [`resource_guard_refusal_stamp`].
    ResourceGuard { stamp: String },
    /// The `claude` CLI could not be launched.
    ClaudeCli(CliKind),
}

/// The deferral stamp for a resource-guard refusal.
///
/// The lane comes ONLY from the frozen `resource_guard:critical:<metric>` token
/// (`metric_token`); the human text after `<metric>: ` may be reworded freely
/// (`resource_guard.rs`, "The wire after the prefix") and is never parsed. The
/// numbers come from a FRESH reading (`now`): if it is still Critical on that
/// same lane, the stamp is `load_pressure`'s grammar with real numbers
/// (`thread_pressure:critical:<o>_over_<l>` / `commit_pressure:critical:…`);
/// otherwise it is the refusal's own token, `resource_guard:critical:<metric>`.
pub(crate) fn resource_guard_refusal_stamp(metric_token: &str, now: &SpawnGate) -> String {
    if let SpawnGate::Critical(observation) = now {
        if observation.metric.wire_name() == metric_token {
            return crate::agent_runtime::load_pressure_stamp_reason("critical", observation);
        }
    }
    format!("{CRITICAL_REFUSAL_PREFIX}{metric_token}")
}

/// Read a failed presentation's error for a [`SeamRefusal`].
///
/// - A headless [`crate::claude_cli_spawn::CliSpawnFailure`] (downcast through
///   the `anyhow` context `spawn_claude_child` attaches) is a CLI refusal only
///   when its `transient_kind` is `Some` — an exhausted CLI-launch retry.
/// - An error whose text starts with [`CRITICAL_REFUSAL_PREFIX`] is the resource
///   guard; `critical_now` supplies the fresh reading for its stamp.
/// - A [`TerminalSeamFailure`] is a CLI refusal when RE-PROBING its own
///   `claude_bin` (`cli_probe`) now reads [`CliReadiness::Unlaunchable`] — a
///   status signal that observes the state it names, instead of matching the
///   holder's or portable-pty's text. If the CLI reappeared in between, the
///   failure is classified non-CLI and posted as today: nothing is deferred on
///   a guess.
///
/// Anything else is `None`.
pub(crate) fn classify_seam_refusal(
    err: &anyhow::Error,
    cli_probe: &dyn Fn(&str) -> CliReadiness,
    critical_now: &dyn Fn() -> SpawnGate,
) -> Option<SeamRefusal> {
    if let Some(described) = err.downcast_ref::<crate::claude_cli_spawn::DescribedCliSpawnFailure>()
    {
        return described
            .failure
            .transient_kind
            .map(|kind| SeamRefusal::ClaudeCli(CliKind::Known(kind)));
    }
    let text = err.to_string();
    if let Some(rest) = text.strip_prefix(CRITICAL_REFUSAL_PREFIX) {
        let metric_token = rest.split(':').next().unwrap_or("").trim();
        return Some(SeamRefusal::ResourceGuard {
            stamp: resource_guard_refusal_stamp(metric_token, &critical_now()),
        });
    }
    if let Some(failure) = err.downcast_ref::<TerminalSeamFailure>() {
        if let CliReadiness::Unlaunchable(kind) = cli_probe(&failure.claude_bin) {
            return Some(SeamRefusal::ClaudeCli(kind));
        }
    }
    None
}

/// What step 4 records for a gate continuation whose spawn failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailedSpawnDisposition {
    /// Defer UNCLAIMED with this stamp: the claim never left the box and the
    /// seam refused for a retriable, local reason.
    DeferUnclaimed(String),
    /// Post the `spawn_failed` outcome, as ever.
    PostOutcome,
}

/// PURE: step 4's decision for a failed gate-continuation spawn (D5).
///
/// Only [`ClaimSent::NotSent`] with a [`SeamRefusal`] defers. An accepted claim
/// (`None` — the decision was not an error), a claim that may have landed
/// ([`ClaimSent::Unknown`]), or an unrelated failure posts the outcome, which
/// coord's `continuation_spawn_retry` re-drives for the retriable heads.
pub(crate) fn failed_spawn_disposition(
    claim_sent: Option<ClaimSent>,
    refusal: Option<&SeamRefusal>,
) -> FailedSpawnDisposition {
    if claim_sent != Some(ClaimSent::NotSent) {
        return FailedSpawnDisposition::PostOutcome;
    }
    match refusal {
        Some(SeamRefusal::ResourceGuard { stamp }) => {
            FailedSpawnDisposition::DeferUnclaimed(stamp.clone())
        }
        Some(SeamRefusal::ClaudeCli(kind)) => {
            FailedSpawnDisposition::DeferUnclaimed(claude_cli_unavailable_stamp_reason(kind))
        }
        None => FailedSpawnDisposition::PostOutcome,
    }
}

/// Phase 4: the `spawn_failed` detail for a failed spawn, BEFORE the first-line
/// cap.
///
/// A CLI refusal leads with `claude_cli_unavailable:<kind>: `, the head coord's
/// `classify_spawn_failure` (qontinui-coord `continuation_spawn_retry.rs`, the
/// `claude_cli_unavailable:` arm) re-drives as a missing CLI. The PTY holder's
/// own text (`PTY holder spawn failed: …`) matches none of coord's retriable
/// heads, so without this every post-claim CLI failure through the holder was
/// alerted as deterministic and never retried. The head goes FIRST so coord's
/// 200-character first-line cap cannot cut it. A detail that already carries
/// the head (the headless retry's own) is left alone, and so is every other
/// refusal — the resource guard's text is matched by coord as it stands.
pub(crate) fn outcome_detail(refusal: Option<&SeamRefusal>, raw: &str) -> String {
    match refusal {
        Some(SeamRefusal::ClaudeCli(kind)) if !raw.starts_with(CLI_UNAVAILABLE_HEAD) => {
            format!("{}: {raw}", claude_cli_unavailable_stamp_reason(kind))
        }
        _ => raw.to_string(),
    }
}

/// Step 4 for a gate continuation whose spawn FAILED: decide
/// ([`failed_spawn_disposition`]) and act through the injected posters.
///
/// `post_spawn_failed(detail)` receives the Phase 4 detail
/// ([`outcome_detail`], first line only); `defer_unclaimed(stamp)` the deferral
/// reason. Production passes `post_spawn_outcome` and
/// `defer_continuation_unclaimed`, so the unit test drives this wiring.
///
/// On a deferral the cleanup matches the pre-claim deferral's: the worktree
/// claim heartbeat and the anchor reservation were moved into the presentation
/// fn and dropped on its `Err`; the agent-lifecycle `spawn-failed` post it made
/// abandons coord's allocation, which a re-delivery re-acquires.
pub(crate) async fn settle_failed_gate_spawn<P, PFut, D, DFut>(
    gate_id: uuid::Uuid,
    claim_sent: Option<ClaimSent>,
    refusal: Option<&SeamRefusal>,
    raw_error: &str,
    post_spawn_failed: P,
    defer_unclaimed: D,
) -> FailedSpawnDisposition
where
    P: FnOnce(String) -> PFut,
    PFut: Future<Output = ()>,
    D: FnOnce(String) -> DFut,
    DFut: Future<Output = ()>,
{
    let disposition = failed_spawn_disposition(claim_sent, refusal);
    match &disposition {
        FailedSpawnDisposition::DeferUnclaimed(stamp) => {
            warn!(
                "agent_runtime: gate-continuation refused at the spawn seam after a claim that \
                 never reached coord — deferring UNCLAIMED instead of posting spawn_failed, row \
                 left pending for re-delivery: {} (stamp={stamp}, gate_id={gate_id})",
                crate::agent_runtime::first_line(raw_error)
            );
            defer_unclaimed(stamp.clone()).await;
        }
        FailedSpawnDisposition::PostOutcome => {
            post_spawn_failed(crate::agent_runtime::first_line(&outcome_detail(
                refusal, raw_error,
            )))
            .await;
        }
    }
    disposition
}

// ---------------------------------------------------------------------------
// D3 — what a deferred-stamp POST achieved
// ---------------------------------------------------------------------------

/// What a `continuation-deferred` stamp POST achieved — the three answers the
/// hourly rate limit treats differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredStampPost {
    /// coord recorded the stamp (2xx).
    Posted,
    /// coord answered and refused (any 4xx, including the 404 of a coord that
    /// has not deployed the route). Re-posting within the hour would get the
    /// same answer, so it holds the hour like a success.
    Rejected,
    /// No answer worth counting: a transport error, a 5xx, a redirect, any
    /// other status, or no coord base / client to send with. The provisional
    /// rate-limit record is rolled back so the next deferral tries again.
    Unreached,
}

/// PURE: classify a deferred-stamp POST's HTTP status (`None` = the request got
/// no response at all).
pub(crate) fn classify_deferred_stamp_status(status: Option<u16>) -> DeferredStampPost {
    match status {
        Some(s) if (200..300).contains(&s) => DeferredStampPost::Posted,
        Some(s) if (400..500).contains(&s) => DeferredStampPost::Rejected,
        _ => DeferredStampPost::Unreached,
    }
}

#[cfg(test)]
mod tests;
