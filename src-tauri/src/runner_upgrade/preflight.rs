//! `GET /runner/upgrade/preflight?candidate=<dir>[&rollback=true]` — may this
//! runner upgrade itself to `candidate` right now, with every session kept?
//!
//! Plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3.1 and Phase 5; it is the preflight served `production-and-cost`
//! `runner-self-upgrade` reads ("passed for that candidate with no UNKNOWN
//! check").
//!
//! **One census, one verdict.** The session and holder census — holder
//! protocol `incompatible`, `holder_unresolved_panes`, per-pane `protection`,
//! `safe_with_handoff` — is `GET /restart-readiness`'s verdict
//! ([`crate::mcp::restart_readiness::compute_restart_readiness`]), read here,
//! never re-derived. This module adds only what that verdict lacks: the
//! candidate's provenance and holder protocol list, the git ancestry, listener
//! inheritance, the restart mechanism, the rehome of non-holder sessions, the
//! rollback guard's prerequisites and the embedded-PG schema.
//!
//! Every check is `pass`, `fail` or `unknown`, with a detail. ANY `fail` or
//! `unknown` makes the verdict `not_ok`: a check that could not be read is
//! never a pass [policy: silent-empty-is-unknown].

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::provenance::{
    self, holder_binary_in, parse_binary_provenance, read_promoted_provenance, runner_binary_in,
    verify_promoted_files, BinaryProvenance, PromotedProvenance,
};
use super::record::{HolderSnapshot, ResumeSpec};
use super::rehome::RehomeOutcome;
use super::sys::{self, sha_agrees, CommandRunner, COMMAND_TIMEOUT};
use crate::mcp::restart_readiness::{
    AiPlane, HeadlessPlane, HolderPlane, RestartReadiness, TerminalPlane,
};
use crate::session::tracking_health::HolderPaneState;

/// One check's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Unknown,
}

/// One preflight check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub verdict: Verdict,
    pub detail: String,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Pass,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Fail,
            detail: detail.into(),
        }
    }
    fn unknown(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Unknown,
            detail: detail.into(),
        }
    }
    fn not_evaluated(name: &'static str, because: &str) -> Self {
        Self::unknown(name, format!("not evaluated: {because}"))
    }
}

/// The check names, in the order they are reported.
pub mod names {
    pub const RESTART_CENSUS: &str = "restart_census";
    pub const HANDOFF_AVAILABLE: &str = "handoff_available";
    pub const NON_HOLDER_SESSIONS: &str = "non_holder_sessions";
    pub const CANDIDATE_LAYOUT: &str = "candidate_layout";
    pub const CANDIDATE_PROVENANCE: &str = "candidate_provenance";
    pub const CANDIDATE_BINARY: &str = "candidate_binary";
    pub const CANDIDATE_DESCENDS: &str = "candidate_descends";
    pub const HOLDER_PROTOCOLS: &str = "holder_protocols";
    pub const LISTENER_INHERITANCE: &str = "listener_inheritance";
    pub const RESTART_MECHANISM: &str = "restart_mechanism";
    pub const ROLLBACK_GUARD: &str = "rollback_guard";
    pub const PG_SCHEMA: &str = "pg_schema";
}

/// How the successor will be started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestartPath {
    /// A systemd user unit restarts the process (`Restart=always`) through the
    /// `current` link; a socket unit holds the listener.
    Systemd { unit: String },
    /// The runner starts its successor itself and passes the listener fds.
    SelfManaged,
}

/// Where promoted builds live, relative to this process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layout {
    /// `target/primary/` — every candidate is a direct child of it.
    pub promoted_root: PathBuf,
    /// The directory the serving binary runs from.
    pub serving_dir: PathBuf,
    /// `target/primary/current`.
    pub current_link: PathBuf,
    /// The runner git checkout the ancestry is read in.
    pub runner_repo: Option<PathBuf>,
}

impl Layout {
    /// Resolve from this process: the serving dir is the real directory of
    /// `current_exe` (the kernel resolves the `current` link), the promoted
    /// root its parent unless `QONTINUI_RUNNER_PROMOTED_ROOT` says otherwise,
    /// and the repo `QONTINUI_RUNNER_REPO` or the checkout holding
    /// `<repo>/target/primary`.
    pub fn resolve() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let exe = std::fs::canonicalize(&exe)
            .map_err(|e| format!("canonicalize {}: {e}", exe.display()))?;
        let serving_dir = exe
            .parent()
            .ok_or_else(|| "the runner binary has no parent directory".to_string())?
            .to_path_buf();
        let promoted_root = match env_path("QONTINUI_RUNNER_PROMOTED_ROOT") {
            Some(p) => std::fs::canonicalize(&p)
                .map_err(|e| format!("QONTINUI_RUNNER_PROMOTED_ROOT {}: {e}", p.display()))?,
            None => serving_dir
                .parent()
                .ok_or_else(|| "the serving directory has no parent".to_string())?
                .to_path_buf(),
        };
        let runner_repo = env_path("QONTINUI_RUNNER_REPO").or_else(|| {
            let repo = promoted_root.parent()?.parent()?;
            repo.join(".git").exists().then(|| repo.to_path_buf())
        });
        Ok(Self {
            current_link: promoted_root.join("current"),
            promoted_root,
            serving_dir,
            runner_repo,
        })
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

/// What the preflight reads from the restart-readiness verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CensusFacts {
    /// `null` whenever the verdict was UNKNOWN.
    pub safe_with_handoff: Option<bool>,
    pub reason: String,
    /// Holder panes whose holder answered (state `live`).
    pub live_holders: Vec<HolderSnapshot>,
    /// `unknown` + `incompatible` panes.
    pub unresolved_panes: usize,
    /// Sessions not in a holder, that a restart would end.
    pub dies_with_runner: Vec<ResumeSpec>,
    /// `live_claude.undecided`.
    pub undecided: usize,
}

/// Map the restart-readiness verdict. `config_dir_of(session_id)` supplies a
/// session's account from the lifecycle store.
pub fn census_facts(
    v: &RestartReadiness,
    config_dir_of: &dyn Fn(&str) -> Option<String>,
) -> Result<CensusFacts, String> {
    let (Some(t), Some(h), Some(hp), Some(a)) = (
        v.terminal_sessions.as_ref(),
        v.headless_sessions.as_ref(),
        v.holder_sessions.as_ref(),
        v.ai_sessions.as_ref(),
    ) else {
        return Err(format!(
            "the restart-readiness census is incomplete: {}",
            v.reason
        ));
    };
    Ok(CensusFacts {
        safe_with_handoff: v.safe_with_handoff,
        reason: v.reason.clone(),
        live_holders: live_holders_from(hp, t),
        unresolved_panes: hp.unresolved_panes,
        dies_with_runner: dies_with_runner_from(t, h, hp, a, config_dir_of),
        undecided: v.live_claude.as_ref().map(|l| l.undecided).unwrap_or(0),
    })
}

/// The live holder panes, each with the sessions running in it (joined by the
/// pane's terminal id and by the `claude` pids in its tree).
pub fn live_holders_from(hp: &HolderPlane, t: &TerminalPlane) -> Vec<HolderSnapshot> {
    hp.panes
        .iter()
        .filter(|p| p.state == HolderPaneState::Live)
        .map(|p| {
            let mut session_ids: Vec<String> = t
                .sessions
                .iter()
                .filter(|s| s.terminal_id.as_deref() == Some(p.pane_id.as_str()))
                .map(|s| s.id.clone())
                .collect();
            for proc_ in t.processes.iter().chain(hp.processes.iter()) {
                if p.claude_pids.contains(&proc_.pid) && !proc_.nested_under_claude {
                    if let Some(id) = &proc_.session_id {
                        if !session_ids.contains(id) {
                            session_ids.push(id.clone());
                        }
                    }
                }
            }
            HolderSnapshot {
                pane_id: p.pane_id.clone(),
                holder_pid: p.holder_pid,
                child_pid: p.child_pid,
                negotiated_version: p.negotiated_version,
                session_ids,
            }
        })
        .collect()
}

/// Every top-level session a restart would end: in-process terminal panes
/// (not finished — a session that declared itself finished is discounted
/// exactly as `safe_with_handoff` discounts it), unclassified and headless
/// `claude`s, and AI/task-run sessions.
pub fn dies_with_runner_from(
    t: &TerminalPlane,
    h: &HeadlessPlane,
    hp: &HolderPlane,
    a: &AiPlane,
    config_dir_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<ResumeSpec> {
    let held: std::collections::HashSet<u32> = hp
        .panes
        .iter()
        .flat_map(|p| p.claude_pids.iter().copied())
        .collect();
    let mut out = Vec::new();
    for p in &t.processes {
        if held.contains(&p.pid) || p.nested_under_claude || !p.blocks_restart {
            continue;
        }
        let terminal_id = p.session_id.as_ref().and_then(|id| {
            t.sessions
                .iter()
                .find(|s| &s.id == id)
                .and_then(|s| s.terminal_id.clone())
        });
        out.push(ResumeSpec {
            kind: "in_process_pane".into(),
            session_id: p.session_id.clone(),
            pid: Some(p.pid),
            cwd: p.cwd.clone(),
            config_dir: p.session_id.as_deref().and_then(config_dir_of),
            terminal_id,
        });
    }
    for (kind, procs) in [
        ("unclassified", &t.unclassified_processes),
        ("headless", &h.processes),
    ] {
        for p in procs.iter().filter(|p| !p.nested_under_claude) {
            out.push(ResumeSpec {
                kind: kind.into(),
                session_id: p.session_id.clone(),
                pid: Some(p.pid),
                cwd: p.cwd.clone(),
                config_dir: None,
                terminal_id: None,
            });
        }
    }
    for s in &a.sessions {
        out.push(ResumeSpec {
            kind: "ai_session".into(),
            session_id: Some(s.id.clone()),
            pid: None,
            cwd: None,
            config_dir: None,
            terminal_id: s.terminal_id.clone(),
        });
    }
    out
}

/// Everything the preflight evaluates, gathered by the route.
pub struct PreflightInputs<'a> {
    pub candidate: &'a Path,
    pub rollback: bool,
    pub serving_sha: &'a str,
    pub layout: &'a Layout,
    /// The census — `Err` when the restart-readiness verdict could not be read.
    pub census: Result<CensusFacts, String>,
    pub rehome: RehomeOutcome,
    /// `/health` `listenerInheritance.api`: `Ok(None)` when this build serves
    /// no such field (plan Phase 2 not on it), `Err` when `/health` was not
    /// read.
    pub listener_api: Result<Option<String>, String>,
    /// This process's systemd user unit.
    pub systemd_unit: Option<String>,
    /// Listener fds a self-managed handoff would pass
    /// ([`super::listener_fds::registered`]).
    pub self_managed_fds: usize,
    pub pg_schema: Result<String, String>,
    pub checked_at: String,
}

/// The preflight verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreflightReport {
    /// Every check passed.
    pub ok: bool,
    /// `pass` | `not_ok`.
    pub verdict: &'static str,
    /// The checks that did not pass, by name.
    pub blocking: Vec<&'static str>,
    pub candidate: PathBuf,
    pub rollback: bool,
    pub serving_sha: String,
    pub candidate_sha: Option<String>,
    pub restart_path: RestartPath,
    pub checks: Vec<Check>,
    pub holders: Vec<HolderSnapshot>,
    pub dies_with_runner: Vec<ResumeSpec>,
    pub rehome: RehomeOutcome,
    pub pg_schema: Option<String>,
    pub layout: Layout,
    pub checked_at: String,
}

/// Run every check. Each `runner` call is a candidate binary, `git` or
/// `systemctl` read; nothing here changes the box.
pub fn evaluate(inputs: PreflightInputs<'_>, runner: &dyn CommandRunner) -> PreflightReport {
    use names::*;
    let mut checks = Vec::new();

    let restart_path = match &inputs.systemd_unit {
        Some(unit) => RestartPath::Systemd { unit: unit.clone() },
        None => RestartPath::SelfManaged,
    };

    // ── The census (restart-readiness) ─────────────────────────────────────
    let census = inputs.census.as_ref();
    checks.push(match census {
        Err(e) => Check::unknown(RESTART_CENSUS, e.clone()),
        Ok(c) if c.safe_with_handoff.is_none() => Check::unknown(RESTART_CENSUS, c.reason.clone()),
        Ok(c) => Check::pass(
            RESTART_CENSUS,
            format!(
                "{} live holder pane(s), {} session(s) outside a holder",
                c.live_holders.len(),
                c.dies_with_runner.len()
            ),
        ),
    });
    checks.push(match census {
        Err(_) => Check::not_evaluated(HANDOFF_AVAILABLE, "the census was not read"),
        Ok(c) => match c.safe_with_handoff {
            None => Check::unknown(
                HANDOFF_AVAILABLE,
                format!("safe_with_handoff is null (UNKNOWN verdict, or a build without the handoff quit): {}", c.reason),
            ),
            Some(true) => Check::pass(HANDOFF_AVAILABLE, "safe_with_handoff: true — every holder pane survives a handoff exit"),
            Some(false) => Check::fail(
                HANDOFF_AVAILABLE,
                format!(
                    "safe_with_handoff: false ({} unresolved pane(s), {} undecided process(es)): {}",
                    c.unresolved_panes, c.undecided, c.reason
                ),
            ),
        },
    });
    checks.push(non_holder_check(census, &inputs.rehome));

    // ── The candidate ──────────────────────────────────────────────────────
    let layout = candidate_layout(inputs.candidate, inputs.layout);
    let layout_ok = layout.verdict == Verdict::Pass;
    checks.push(layout);

    let mut candidate_sha: Option<String> = None;
    let promoted: Option<PromotedProvenance> = if layout_ok {
        let (check, p) = candidate_provenance(inputs.candidate);
        checks.push(check);
        p
    } else {
        checks.push(Check::not_evaluated(
            CANDIDATE_PROVENANCE,
            "candidate_layout did not pass",
        ));
        None
    };
    let files_ok = promoted.is_some();
    let binary: Option<BinaryProvenance> = if files_ok {
        let (check, b) = candidate_binary(inputs.candidate, promoted.as_ref(), runner);
        checks.push(check);
        b
    } else {
        checks.push(Check::not_evaluated(
            CANDIDATE_BINARY,
            "the candidate's files are not verified, so none of its binaries is run",
        ));
        None
    };
    if let Some(b) = &binary {
        candidate_sha = Some(b.git_sha.clone());
    }
    checks.push(match &candidate_sha {
        Some(sha) => candidate_descends(
            inputs.serving_sha,
            sha,
            inputs.rollback,
            inputs.layout.runner_repo.as_deref(),
            runner,
        ),
        None => Check::not_evaluated(CANDIDATE_DESCENDS, "the candidate's sha is not established"),
    });
    checks.push(if files_ok {
        holder_protocols(inputs.candidate, census, runner)
    } else {
        Check::not_evaluated(HOLDER_PROTOCOLS, "the candidate's files are not verified")
    });

    // ── The handoff machinery ─────────────────────────────────────────────
    checks.push(listener_inheritance(
        &restart_path,
        &inputs.listener_api,
        inputs.self_managed_fds,
        runner,
    ));
    checks.push(restart_mechanism(&restart_path, inputs.layout, runner));
    checks.push(rollback_guard(&restart_path, runner));
    checks.push(match &inputs.pg_schema {
        Ok(fp) => Check::pass(
            PG_SCHEMA,
            format!("embedded-PG schema {fp} recorded for the rollback rule"),
        ),
        Err(e) => Check::unknown(PG_SCHEMA, e.clone()),
    });

    let blocking: Vec<&'static str> = checks
        .iter()
        .filter(|c| c.verdict != Verdict::Pass)
        .map(|c| c.name)
        .collect();
    let ok = blocking.is_empty();
    let (holders, dies_with_runner) = match census {
        Ok(c) => (c.live_holders.clone(), c.dies_with_runner.clone()),
        Err(_) => (Vec::new(), Vec::new()),
    };
    PreflightReport {
        ok,
        verdict: if ok { "pass" } else { "not_ok" },
        blocking,
        candidate: inputs.candidate.to_path_buf(),
        rollback: inputs.rollback,
        serving_sha: inputs.serving_sha.to_string(),
        candidate_sha,
        restart_path,
        checks,
        holders,
        dies_with_runner,
        rehome: inputs.rehome,
        pg_schema: inputs.pg_schema.ok(),
        layout: inputs.layout.clone(),
        checked_at: inputs.checked_at,
    }
}

fn non_holder_check(census: Result<&CensusFacts, &String>, rehome: &RehomeOutcome) -> Check {
    use names::NON_HOLDER_SESSIONS as N;
    let Ok(c) = census else {
        return Check::not_evaluated(N, "the census was not read");
    };
    let name = |s: &ResumeSpec| {
        s.session_id
            .clone()
            .or_else(|| s.pid.map(|p| format!("pid {p}")))
            .unwrap_or_else(|| s.kind.clone())
    };
    match rehome {
        RehomeOutcome::NothingToRehome if c.dies_with_runner.is_empty() => {
            Check::pass(N, "every session runs in a PTY holder")
        }
        RehomeOutcome::NothingToRehome => Check::unknown(
            N,
            format!(
                "{} session(s) outside a holder were not offered to the rehome: {}",
                c.dies_with_runner.len(),
                c.dies_with_runner.iter().map(name).collect::<Vec<_>>().join(", ")
            ),
        ),
        RehomeOutcome::Unavailable { reason } => Check::unknown(N, reason.clone()),
        RehomeOutcome::Report(r) if !r.busy_deferred.is_empty() => Check::fail(
            N,
            format!(
                "deferred: {} busy session(s) outside a holder — the upgrade waits for them and never \
                 kills one mid-turn: {}",
                r.busy_deferred.len(),
                r.busy_deferred.join(", ")
            ),
        ),
        RehomeOutcome::Report(r) if !r.failed.is_empty() => Check::fail(
            N,
            format!(
                "rehome failed for {}",
                r.failed
                    .iter()
                    .map(|(s, why)| format!("{s} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        RehomeOutcome::Report(r) => Check::pass(
            N,
            format!("{} session(s) rehomed into a holder: {}", r.rehomed.len(), r.rehomed.join(", ")),
        ),
    }
}

/// The candidate is a real directory directly under the promoted root, not the
/// serving build and not the `current` link, holding the two binaries.
pub fn candidate_layout(candidate: &Path, layout: &Layout) -> Check {
    use names::CANDIDATE_LAYOUT as N;
    if !candidate.is_absolute() {
        return Check::fail(
            N,
            format!("{} is not an absolute path", candidate.display()),
        );
    }
    let meta = match std::fs::symlink_metadata(candidate) {
        Ok(m) => m,
        Err(e) => return Check::fail(N, format!("{}: {e}", candidate.display())),
    };
    if meta.file_type().is_symlink() {
        return Check::fail(
            N,
            format!(
                "{} is a link; name the promoted directory itself",
                candidate.display()
            ),
        );
    }
    if !meta.is_dir() {
        return Check::fail(N, format!("{} is not a directory", candidate.display()));
    }
    let canon = match std::fs::canonicalize(candidate) {
        Ok(c) => c,
        Err(e) => return Check::unknown(N, format!("canonicalize {}: {e}", candidate.display())),
    };
    if canon.parent() != Some(layout.promoted_root.as_path()) {
        return Check::fail(
            N,
            format!(
                "{} is not a promoted build: candidates live directly under {}",
                canon.display(),
                layout.promoted_root.display()
            ),
        );
    }
    if canon == layout.serving_dir {
        return Check::fail(N, format!("{} is the serving build", canon.display()));
    }
    for bin in [runner_binary_in(&canon), holder_binary_in(&canon)] {
        if !bin.is_file() {
            return Check::fail(N, format!("{} is missing", bin.display()));
        }
    }
    Check::pass(N, format!("{} is a promoted build", canon.display()))
}

/// `PROVENANCE.json` is present, readable, names a sha agreeing with the
/// directory, and every listed file hashes to its recorded digest.
pub fn candidate_provenance(candidate: &Path) -> (Check, Option<PromotedProvenance>) {
    use names::CANDIDATE_PROVENANCE as N;
    let p = match read_promoted_provenance(candidate) {
        Ok(p) => p,
        Err(e @ provenance::ProvenanceError::Unreadable(_)) => {
            return (Check::unknown(N, e.to_string()), None)
        }
        Err(e) => return (Check::fail(N, e.to_string()), None),
    };
    let dir_name = candidate
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !sha_agrees(&dir_name, &p.sha) {
        return (
            Check::fail(
                N,
                format!(
                    "the directory {dir_name} does not name PROVENANCE.json's sha {}",
                    p.sha
                ),
            ),
            None,
        );
    }
    let v = verify_promoted_files(candidate, &p);
    if !v.ok() {
        return (Check::fail(N, v.describe()), None);
    }
    (
        Check::pass(
            N,
            format!(
                "sha {} build {} — {}; sibling pins: {}",
                p.sha,
                p.build_id,
                v.describe(),
                p.sibling_pins
                    .iter()
                    .map(|(k, v)| format!("{k}@{v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        Some(p),
    )
}

/// The candidate binary's own `--provenance`: clean (`gitDirty: false`), and
/// the same commit and build `PROVENANCE.json` names.
pub fn candidate_binary(
    candidate: &Path,
    promoted: Option<&PromotedProvenance>,
    runner: &dyn CommandRunner,
) -> (Check, Option<BinaryProvenance>) {
    use names::CANDIDATE_BINARY as N;
    let exe = runner_binary_in(candidate);
    let out = match runner.run(&exe, &["--provenance".to_string()], COMMAND_TIMEOUT) {
        Ok(o) if o.success() => o,
        Ok(o) => {
            return (
                Check::unknown(
                    N,
                    format!("{} --provenance: {}", exe.display(), o.summary()),
                ),
                None,
            )
        }
        Err(e) => return (Check::unknown(N, e), None),
    };
    let b = match parse_binary_provenance(&out.stdout) {
        Ok(b) => b,
        Err(e) => return (Check::unknown(N, e), None),
    };
    if let Some(p) = promoted {
        if !sha_agrees(&b.git_sha, &p.sha) {
            return (
                Check::fail(
                    N,
                    format!(
                        "the binary says {} but PROVENANCE.json says {}",
                        b.git_sha, p.sha
                    ),
                ),
                None,
            );
        }
        if b.build_id != p.build_id {
            return (
                Check::fail(
                    N,
                    format!(
                        "the binary's build {} is not PROVENANCE.json's {}",
                        b.build_id, p.build_id
                    ),
                ),
                None,
            );
        }
    }
    let check = match b.git_dirty {
        Some(false) => Check::pass(N, format!("{} built clean from {}", b.build_id, b.git_sha)),
        Some(true) => Check::fail(N, format!("{} was built from a DIRTY tree", b.build_id)),
        None => Check::unknown(
            N,
            format!("{} does not know whether its tree was dirty", b.build_id),
        ),
    };
    (check, Some(b))
}

/// The candidate's sha descends from the serving sha (git ancestry in the
/// runner checkout); with `rollback`, the candidate must instead be an
/// ancestor — an explicit downgrade, never an accidental one.
pub fn candidate_descends(
    serving: &str,
    candidate: &str,
    rollback: bool,
    repo: Option<&Path>,
    runner: &dyn CommandRunner,
) -> Check {
    use names::CANDIDATE_DESCENDS as N;
    if sha_agrees(serving, candidate) {
        return Check::fail(
            N,
            format!("the candidate {candidate} is the serving build {serving}"),
        );
    }
    let Some(repo) = repo else {
        return Check::unknown(
            N,
            "no runner checkout to read ancestry in (set QONTINUI_RUNNER_REPO, or promote under <repo>/target/primary)",
        );
    };
    let (older, newer) = if rollback {
        (candidate, serving)
    } else {
        (serving, candidate)
    };
    let args = vec![
        "-C".to_string(),
        repo.display().to_string(),
        "merge-base".to_string(),
        "--is-ancestor".to_string(),
        older.to_string(),
        newer.to_string(),
    ];
    match runner.run(Path::new(sys::GIT), &args, COMMAND_TIMEOUT) {
        Ok(o) if o.code == Some(0) => Check::pass(
            N,
            if rollback {
                format!("explicit rollback: {candidate} is an ancestor of the serving {serving}")
            } else {
                format!("{candidate} descends from the serving {serving}")
            },
        ),
        Ok(o) if o.code == Some(1) => {
            Check::fail(
                N,
                if rollback {
                    format!("rollback asked, but {candidate} is not an ancestor of the serving {serving}")
                } else {
                    format!(
                    "{candidate} does not descend from the serving {serving} — a downgrade needs rollback=true"
                )
                },
            )
        }
        Ok(o) => Check::unknown(
            N,
            format!(
                "git merge-base in {} could not decide ({}) — fetch the checkout",
                repo.display(),
                o.summary()
            ),
        ),
        Err(e) => Check::unknown(N, e),
    }
}

/// Every live holder's negotiated protocol version is one the candidate's
/// holder binary speaks (`--protocol-versions`, applied through
/// `qontinui_pty_holder::protocol::negotiate`).
pub fn holder_protocols(
    candidate: &Path,
    census: Result<&CensusFacts, &String>,
    runner: &dyn CommandRunner,
) -> Check {
    use names::HOLDER_PROTOCOLS as N;
    use qontinui_pty_holder::protocol::{negotiate, parse_protocol_versions};
    let Ok(c) = census else {
        return Check::not_evaluated(N, "the census was not read");
    };
    let exe = holder_binary_in(candidate);
    let speaks = match runner.run(&exe, &["--protocol-versions".to_string()], COMMAND_TIMEOUT) {
        Ok(o) if o.success() => match parse_protocol_versions(&o.stdout) {
            Some(v) => v,
            None => {
                return Check::unknown(
                    N,
                    format!(
                        "{} --protocol-versions answered {:?}",
                        exe.display(),
                        o.stdout.trim()
                    ),
                )
            }
        },
        Ok(o) => {
            return Check::unknown(
                N,
                format!("{} --protocol-versions: {}", exe.display(), o.summary()),
            )
        }
        Err(e) => return Check::unknown(N, e),
    };
    let mut problems = Vec::new();
    let mut unknown = Vec::new();
    for h in &c.live_holders {
        match h.negotiated_version {
            None => unknown.push(format!("pane {} has no negotiated version", h.pane_id)),
            Some(v) if negotiate(&[v], &speaks) != Some(v) => problems.push(format!(
                "pane {} speaks v{v}, the candidate speaks {speaks:?}",
                h.pane_id
            )),
            Some(_) => {}
        }
    }
    if c.unresolved_panes > 0 {
        unknown.push(format!(
            "{} holder pane(s) unresolved (unknown or incompatible)",
            c.unresolved_panes
        ));
    }
    if !problems.is_empty() {
        Check::fail(N, problems.join("; "))
    } else if !unknown.is_empty() {
        Check::unknown(N, unknown.join("; "))
    } else {
        Check::pass(
            N,
            format!(
                "the candidate speaks {speaks:?}; all {} live holder(s) keep their version",
                c.live_holders.len()
            ),
        )
    }
}

/// The listener outlives the process: under systemd a socket unit triggers
/// this unit (or this build reports the listener as inherited); self-managed,
/// the listener fds are registered for passing.
pub fn listener_inheritance(
    path: &RestartPath,
    api: &Result<Option<String>, String>,
    self_managed_fds: usize,
    runner: &dyn CommandRunner,
) -> Check {
    use names::LISTENER_INHERITANCE as N;
    if let Ok(Some(a)) = api {
        if a == "inherited" {
            return Check::pass(N, "/health listenerInheritance.api is \"inherited\"");
        }
    }
    match path {
        RestartPath::Systemd { unit } => {
            let triggered_by = match sys::systemd_show(runner, unit, "TriggeredBy") {
                Ok(t) => t,
                Err(e) => return Check::unknown(N, e),
            };
            let sockets: Vec<&str> = triggered_by
                .split_whitespace()
                .filter(|u| u.ends_with(".socket"))
                .collect();
            if sockets.is_empty() {
                return Check::fail(
                    N,
                    format!(
                        "no socket unit triggers {unit} (install it with install-runner-socket-unit.sh, plan \
                         Phase 3); /health listenerInheritance: {}",
                        describe_api(api)
                    ),
                );
            }
            for s in &sockets {
                match sys::systemctl_user(runner, &["is-active", *s]) {
                    Ok(o) if o.stdout.trim() == "active" => {
                        return Check::pass(
                            N,
                            format!("{s} is active and holds the listener for {unit}"),
                        )
                    }
                    Ok(_) => {}
                    Err(e) => return Check::unknown(N, e),
                }
            }
            Check::fail(
                N,
                format!("socket unit(s) {} not active", sockets.join(", ")),
            )
        }
        RestartPath::SelfManaged => {
            if !cfg!(unix) {
                return Check::fail(
                    N,
                    "a self-managed listener handoff on this OS is plan Phase 8",
                );
            }
            if self_managed_fds == 0 {
                Check::fail(
                    N,
                    format!(
                        "not under systemd, and no listener fd is registered for a self-managed handoff \
                         (plan Phase 2 registers it); /health listenerInheritance: {}",
                        describe_api(api)
                    ),
                )
            } else {
                Check::pass(
                    N,
                    format!("{self_managed_fds} listener fd(s) registered for the successor"),
                )
            }
        }
    }
}

fn describe_api(api: &Result<Option<String>, String>) -> String {
    match api {
        Ok(Some(a)) => format!("api={a:?}"),
        Ok(None) => "absent on this build".to_string(),
        Err(e) => format!("unread ({e})"),
    }
}

/// The successor will actually start from the candidate: under systemd the
/// unit restarts on any exit (`Restart=always`), its `ExecStart` runs through
/// the `current` link, and that link points at the serving build today.
pub fn restart_mechanism(path: &RestartPath, layout: &Layout, runner: &dyn CommandRunner) -> Check {
    use names::RESTART_MECHANISM as N;
    let link_now = match sys::link_target(&layout.current_link) {
        Ok(t) => std::fs::canonicalize(&t).unwrap_or(t),
        Err(e) => {
            return Check::fail(
                N,
                format!(
                    "{} is not a readable link: {e}",
                    layout.current_link.display()
                ),
            )
        }
    };
    if link_now != layout.serving_dir {
        return Check::fail(
            N,
            format!(
                "{} points at {}, not at the serving build {}",
                layout.current_link.display(),
                link_now.display(),
                layout.serving_dir.display()
            ),
        );
    }
    match path {
        RestartPath::Systemd { unit } => {
            let restart = match sys::systemd_show(runner, unit, "Restart") {
                Ok(r) => r,
                Err(e) => return Check::unknown(N, e),
            };
            if restart != "always" {
                return Check::fail(
                    N,
                    format!("{unit} has Restart={restart}; the handoff exit needs Restart=always"),
                );
            }
            let exec = match sys::systemd_show(runner, unit, "ExecStart") {
                Ok(r) => r,
                Err(e) => return Check::unknown(N, e),
            };
            let through = layout.current_link.display().to_string();
            if !exec.contains(&through) {
                return Check::fail(
                    N,
                    format!(
                        "{unit}'s ExecStart does not run through {through} (a per-sha drop-in? plan Phase 3 \
                         normalizes it): {exec}"
                    ),
                );
            }
            Check::pass(
                N,
                format!("{unit} restarts through {through} (Restart=always)"),
            )
        }
        RestartPath::SelfManaged => {
            if cfg!(unix) {
                Check::pass(
                    N,
                    "self-managed: the runner starts its successor from the candidate itself",
                )
            } else {
                Check::fail(N, "a self-managed successor on this OS is plan Phase 8")
            }
        }
    }
}

/// The rollback guard can be armed OUTSIDE this runner's cgroup:
/// `systemd-run --user` under systemd, a detached guard process otherwise.
pub fn rollback_guard(path: &RestartPath, runner: &dyn CommandRunner) -> Check {
    use names::ROLLBACK_GUARD as N;
    match path {
        RestartPath::Systemd { .. } => {
            match runner.run(
                Path::new(sys::SYSTEMD_RUN),
                &["--version".to_string()],
                COMMAND_TIMEOUT,
            ) {
                Ok(o) if o.success() => Check::pass(
                    N,
                    "systemd-run is available to arm the guard as a transient user timer",
                ),
                Ok(o) => Check::unknown(N, format!("systemd-run --version: {}", o.summary())),
                Err(e) => Check::unknown(N, e),
            }
        }
        RestartPath::SelfManaged if cfg!(unix) => Check::pass(
            N,
            "self-managed: the guard runs as a detached process in its own session",
        ),
        RestartPath::SelfManaged => Check::fail(N, "the Windows guard is plan Phase 8"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runner_upgrade::provenance::{write_promoted_provenance, PromotionFacts};
    use crate::runner_upgrade::rehome::RehomeReport;
    use crate::runner_upgrade::sys::CmdOutput;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::Duration;

    pub(crate) const SERVING: &str = "4d524e71bcc3";
    pub(crate) const CANDIDATE_SHA: &str = "9f00aa11bb22cc33dd44ee55ff6600112233aabb";

    /// A scripted [`CommandRunner`]: the first rule whose program and argument
    /// substring match answers; every call is recorded.
    #[derive(Default)]
    pub(crate) struct FakeRunner {
        pub rules: Mutex<Vec<(String, String, Result<CmdOutput, String>)>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        pub(crate) fn on(
            &self,
            program: &str,
            args_contain: &str,
            out: Result<CmdOutput, String>,
        ) -> &Self {
            self.rules
                .lock()
                .unwrap()
                .push((program.to_string(), args_contain.to_string(), out));
            self
        }
        pub(crate) fn ok(&self, program: &str, args_contain: &str, stdout: &str) -> &Self {
            self.on(
                program,
                args_contain,
                Ok(CmdOutput {
                    code: Some(0),
                    stdout: stdout.into(),
                    stderr: String::new(),
                }),
            )
        }
        pub(crate) fn exit(&self, program: &str, args_contain: &str, code: i32) -> &Self {
            self.on(
                program,
                args_contain,
                Ok(CmdOutput {
                    code: Some(code),
                    stdout: String::new(),
                    stderr: format!("exit {code}"),
                }),
            )
        }
        pub(crate) fn called(&self, needle: &str) -> bool {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains(needle))
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &Path, args: &[String], _t: Duration) -> Result<CmdOutput, String> {
            let line = format!("{} {}", program.display(), args.join(" "));
            self.calls.lock().unwrap().push(line.clone());
            let rules = self.rules.lock().unwrap();
            for (p, needle, out) in rules.iter() {
                if program.to_string_lossy().ends_with(p.as_str()) && line.contains(needle.as_str())
                {
                    return out.clone();
                }
            }
            Err(format!("no fake rule for `{line}`"))
        }
    }

    /// A promoted root with a serving build, a candidate with a valid
    /// `PROVENANCE.json`, and `current` → serving.
    pub(crate) struct Fixture {
        pub _dir: tempfile::TempDir,
        pub layout: Layout,
        pub candidate: PathBuf,
    }

    pub(crate) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let serving = root.join("4d524e71b");
        let candidate = root.join("9f00aa11b");
        for d in [&serving, &candidate] {
            std::fs::create_dir(d).unwrap();
            std::fs::write(runner_binary_in(d), b"runner").unwrap();
            std::fs::write(holder_binary_in(d), b"holder").unwrap();
        }
        write_promoted_provenance(
            &candidate,
            &PromotionFacts {
                sha: CANDIDATE_SHA.into(),
                build_id: "9f00aa11b-1".into(),
                build_profile: Some("release-checked".into()),
                sibling_pins: BTreeMap::from([("qontinui-schemas".into(), "a7b273d8".into())]),
                files: vec![
                    provenance::RUNNER_BINARY.into(),
                    provenance::HOLDER_BINARY.into(),
                ],
            },
            "2026-11-24T00:00:00Z",
        )
        .unwrap();
        #[cfg(unix)]
        super::sys::swap_link_atomically(&root.join("current"), &serving).unwrap();
        Fixture {
            layout: Layout {
                current_link: root.join("current"),
                promoted_root: root.clone(),
                serving_dir: serving,
                runner_repo: Some(root.join("repo")),
            },
            candidate,
            _dir: dir,
        }
    }

    pub(crate) fn healthy_census() -> CensusFacts {
        CensusFacts {
            safe_with_handoff: Some(true),
            reason: "safe".into(),
            live_holders: vec![HolderSnapshot {
                pane_id: "pane-1".into(),
                holder_pid: Some(100),
                child_pid: Some(101),
                negotiated_version: Some(3),
                session_ids: vec!["11111111-1111-1111-1111-111111111111".into()],
            }],
            unresolved_panes: 0,
            dies_with_runner: vec![],
            undecided: 0,
        }
    }

    /// Every external read a passing systemd preflight makes.
    pub(crate) fn healthy_runner(f: &Fixture) -> FakeRunner {
        let r = FakeRunner::default();
        let runner_exe = runner_binary_in(&f.candidate).display().to_string();
        r.ok(
            &runner_exe,
            "--provenance",
            &format!(
                r#"{{"gitSha":"{}","gitDirty":false,"buildId":"9f00aa11b-1","buildProfile":"release-checked"}}"#,
                CANDIDATE_SHA.get(..12).unwrap()
            ),
        )
        .ok(&holder_binary_in(&f.candidate).display().to_string(), "--protocol-versions", "[1,2,3]\n")
        .exit("git", "--is-ancestor", 0)
        .ok("systemctl", "-p TriggeredBy", "qontinui-runner.socket\n")
        .ok("systemctl", "is-active qontinui-runner.socket", "active\n")
        .ok("systemctl", "-p Restart", "always\n")
        .ok(
            "systemctl",
            "-p ExecStart",
            &format!(
                "{{ path={0}/qontinui-runner ; argv[]={0}/qontinui-runner ; }}",
                f.layout.current_link.display()
            ),
        )
        .ok("systemd-run", "--version", "systemd 257\n");
        r
    }

    pub(crate) fn inputs<'a>(
        f: &'a Fixture,
        census: Result<CensusFacts, String>,
    ) -> PreflightInputs<'a> {
        PreflightInputs {
            candidate: &f.candidate,
            rollback: false,
            serving_sha: SERVING,
            layout: &f.layout,
            census,
            rehome: RehomeOutcome::NothingToRehome,
            listener_api: Ok(None),
            systemd_unit: Some("qontinui-runner.service".into()),
            self_managed_fds: 0,
            pg_schema: Ok("md5:abc/10".into()),
            checked_at: "2026-11-24T22:00:00Z".into(),
        }
    }

    fn verdict_of(r: &PreflightReport, name: &str) -> Verdict {
        r.checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check {name}"))
            .verdict
    }

    #[cfg(unix)]
    #[test]
    fn a_healthy_box_passes_every_check() {
        let f = fixture();
        let runner = healthy_runner(&f);
        let r = evaluate(inputs(&f, Ok(healthy_census())), &runner);
        assert!(r.ok, "{:#?}", r.checks);
        assert_eq!(r.verdict, "pass");
        assert_eq!(r.checks.len(), 12);
        assert!(r.checks.iter().all(|c| c.verdict == Verdict::Pass));
        assert_eq!(r.candidate_sha.as_deref(), Some(&CANDIDATE_SHA[..12]));
        assert!(runner.called("merge-base --is-ancestor 4d524e71bcc3 9f00aa11bb22"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_census_is_unknown_and_blocks() {
        let f = fixture();
        let r = evaluate(
            inputs(&f, Err("SessionManager did not resolve".into())),
            &healthy_runner(&f),
        );
        assert!(!r.ok);
        assert_eq!(verdict_of(&r, names::RESTART_CENSUS), Verdict::Unknown);
        assert_eq!(verdict_of(&r, names::HANDOFF_AVAILABLE), Verdict::Unknown);
        assert_eq!(verdict_of(&r, names::HOLDER_PROTOCOLS), Verdict::Unknown);
        let mut c = healthy_census();
        c.safe_with_handoff = None;
        c.reason = "UNKNOWN, so treated as unsafe: the PTY-holder census could not be taken".into();
        let r = evaluate(inputs(&f, Ok(c)), &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::RESTART_CENSUS), Verdict::Unknown);
        assert!(r.blocking.contains(&names::HANDOFF_AVAILABLE));
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_that_would_kill_a_pane_fails() {
        let f = fixture();
        let mut c = healthy_census();
        c.safe_with_handoff = Some(false);
        let r = evaluate(inputs(&f, Ok(c)), &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::HANDOFF_AVAILABLE), Verdict::Fail);
        assert!(!r.ok);
    }

    fn dwr() -> ResumeSpec {
        ResumeSpec {
            kind: "in_process_pane".into(),
            session_id: Some("22222222-2222-2222-2222-222222222222".into()),
            pid: Some(555),
            cwd: Some("/w".into()),
            config_dir: Some("/home/u/.claude-a".into()),
            terminal_id: Some("t-2".into()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_busy_non_holder_session_defers_the_upgrade() {
        let f = fixture();
        let mut c = healthy_census();
        c.dies_with_runner = vec![dwr()];
        let mut i = inputs(&f, Ok(c));
        i.rehome = RehomeOutcome::Report(RehomeReport {
            rehomed: vec![],
            busy_deferred: vec!["22222222-2222-2222-2222-222222222222".into()],
            failed: vec![],
        });
        let r = evaluate(i, &healthy_runner(&f));
        let check = r
            .checks
            .iter()
            .find(|c| c.name == names::NON_HOLDER_SESSIONS)
            .unwrap();
        assert_eq!(check.verdict, Verdict::Fail);
        assert!(check.detail.starts_with("deferred:"), "{}", check.detail);
        assert!(check.detail.contains("22222222-2222"), "names the session");
        assert!(!r.ok);
    }

    #[cfg(unix)]
    #[test]
    fn non_holder_sessions_without_a_rehome_are_unknown_and_rehomed_ones_pass() {
        let f = fixture();
        let mut c = healthy_census();
        c.dies_with_runner = vec![dwr()];
        let mut i = inputs(&f, Ok(c.clone()));
        i.rehome = RehomeOutcome::Unavailable {
            reason: "no rehome on this build".into(),
        };
        let r = evaluate(i, &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::NON_HOLDER_SESSIONS), Verdict::Unknown);

        let mut i = inputs(&f, Ok(c));
        i.rehome = RehomeOutcome::Report(RehomeReport {
            rehomed: vec!["22222222-2222-2222-2222-222222222222".into()],
            ..Default::default()
        });
        let r = evaluate(i, &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::NON_HOLDER_SESSIONS), Verdict::Pass);
    }

    #[cfg(unix)]
    #[test]
    fn candidate_outside_the_promoted_root_or_the_serving_dir_fails_and_nothing_is_run() {
        let f = fixture();
        let elsewhere = tempfile::tempdir().unwrap();
        let runner = healthy_runner(&f);
        let mut i = inputs(&f, Ok(healthy_census()));
        i.candidate = elsewhere.path();
        let r = evaluate(i, &runner);
        assert_eq!(verdict_of(&r, names::CANDIDATE_LAYOUT), Verdict::Fail);
        assert_eq!(verdict_of(&r, names::CANDIDATE_BINARY), Verdict::Unknown);
        assert!(
            !runner.called("--provenance"),
            "an unverified candidate is never executed"
        );

        let serving = f.layout.serving_dir.clone();
        let mut i = inputs(&f, Ok(healthy_census()));
        i.candidate = &serving;
        let r = evaluate(i, &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::CANDIDATE_LAYOUT), Verdict::Fail);

        let link = f.layout.current_link.clone();
        let mut i = inputs(&f, Ok(healthy_census()));
        i.candidate = &link;
        let r = evaluate(i, &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::CANDIDATE_LAYOUT), Verdict::Fail);
    }

    #[cfg(unix)]
    #[test]
    fn a_tampered_candidate_fails_provenance_and_is_not_run() {
        let f = fixture();
        std::fs::write(runner_binary_in(&f.candidate), b"swapped").unwrap();
        let runner = healthy_runner(&f);
        let r = evaluate(inputs(&f, Ok(healthy_census())), &runner);
        assert_eq!(verdict_of(&r, names::CANDIDATE_PROVENANCE), Verdict::Fail);
        assert!(!runner.called("--provenance"));
        std::fs::remove_file(f.candidate.join(provenance::PROVENANCE_FILE)).unwrap();
        let r = evaluate(inputs(&f, Ok(healthy_census())), &healthy_runner(&f));
        assert_eq!(verdict_of(&r, names::CANDIDATE_PROVENANCE), Verdict::Fail);
    }

    #[cfg(unix)]
    #[test]
    fn a_dirty_or_unmeasured_candidate_binary_blocks() {
        let f = fixture();
        for (dirty, want) in [("true", Verdict::Fail), ("null", Verdict::Unknown)] {
            let runner = FakeRunner::default();
            runner.ok(
                &runner_binary_in(&f.candidate).display().to_string(),
                "--provenance",
                &format!(
                    r#"{{"gitSha":"9f00aa11bb22","gitDirty":{dirty},"buildId":"9f00aa11b-1","buildProfile":null}}"#
                ),
            );
            let (c, _) = candidate_binary(&f.candidate, None, &runner);
            assert_eq!(c.verdict, want, "{dirty}: {}", c.detail);
        }
        // A binary whose answer cannot be read is UNKNOWN.
        let (c, _) = candidate_binary(&f.candidate, None, &FakeRunner::default());
        assert_eq!(c.verdict, Verdict::Unknown);
    }

    #[test]
    fn ancestry_decides_and_an_undecidable_one_is_unknown() {
        let repo = PathBuf::from("/repo");
        let ok = FakeRunner::default();
        ok.exit("git", "--is-ancestor", 0);
        assert_eq!(
            candidate_descends(SERVING, "9f00aa11bb22", false, Some(&repo), &ok).verdict,
            Verdict::Pass
        );
        let not = FakeRunner::default();
        not.exit("git", "--is-ancestor", 1);
        let c = candidate_descends(SERVING, "9f00aa11bb22", false, Some(&repo), &not);
        assert_eq!(c.verdict, Verdict::Fail);
        assert!(c.detail.contains("rollback=true"));
        let missing = FakeRunner::default();
        missing.exit("git", "--is-ancestor", 128);
        assert_eq!(
            candidate_descends(SERVING, "9f00aa11bb22", false, Some(&repo), &missing).verdict,
            Verdict::Unknown
        );
        assert_eq!(
            candidate_descends(SERVING, "9f00aa11bb22", false, None, &ok).verdict,
            Verdict::Unknown
        );
        // The same commit is not an upgrade.
        assert_eq!(
            candidate_descends(SERVING, "4d524e71b", false, Some(&repo), &ok).verdict,
            Verdict::Fail
        );
        // An explicit rollback reverses the question.
        let rb = FakeRunner::default();
        rb.exit("git", "--is-ancestor 9f00aa11bb22 4d524e71bcc3", 0);
        assert_eq!(
            candidate_descends(SERVING, "9f00aa11bb22", true, Some(&repo), &rb).verdict,
            Verdict::Pass
        );
        assert!(rb.called("--is-ancestor 9f00aa11bb22 4d524e71bcc3"));
    }

    #[cfg(unix)]
    #[test]
    fn a_holder_the_candidate_cannot_speak_to_fails_and_an_unresolved_one_is_unknown() {
        let f = fixture();
        let runner = FakeRunner::default();
        runner.ok(
            &holder_binary_in(&f.candidate).display().to_string(),
            "--protocol-versions",
            "[3,4]",
        );
        let mut c = healthy_census();
        c.live_holders[0].negotiated_version = Some(2);
        let check = holder_protocols(&f.candidate, Ok(&c), &runner);
        assert_eq!(check.verdict, Verdict::Fail, "{}", check.detail);
        c.live_holders[0].negotiated_version = Some(3);
        assert_eq!(
            holder_protocols(&f.candidate, Ok(&c), &runner).verdict,
            Verdict::Pass
        );
        c.unresolved_panes = 1;
        assert_eq!(
            holder_protocols(&f.candidate, Ok(&c), &runner).verdict,
            Verdict::Unknown
        );
        c.unresolved_panes = 0;
        c.live_holders[0].negotiated_version = None;
        assert_eq!(
            holder_protocols(&f.candidate, Ok(&c), &runner).verdict,
            Verdict::Unknown
        );
        // A holder binary that does not know the flag (a pre-Phase-5 build).
        let old = FakeRunner::default();
        old.exit(
            &holder_binary_in(&f.candidate).display().to_string(),
            "--protocol-versions",
            2,
        );
        assert_eq!(
            holder_protocols(&f.candidate, Ok(&healthy_census()), &old).verdict,
            Verdict::Unknown
        );
    }

    #[test]
    fn listener_inheritance_reads_the_socket_unit_or_the_health_field() {
        let unit = RestartPath::Systemd {
            unit: "qontinui-runner.service".into(),
        };
        let none = FakeRunner::default();
        none.ok("systemctl", "-p TriggeredBy", "\n");
        assert_eq!(
            listener_inheritance(&unit, &Ok(None), 0, &none).verdict,
            Verdict::Fail
        );
        let inactive = FakeRunner::default();
        inactive
            .ok("systemctl", "-p TriggeredBy", "qontinui-runner.socket")
            .on(
                "systemctl",
                "is-active",
                Ok(CmdOutput {
                    code: Some(3),
                    stdout: "inactive\n".into(),
                    stderr: String::new(),
                }),
            );
        assert_eq!(
            listener_inheritance(&unit, &Ok(None), 0, &inactive).verdict,
            Verdict::Fail
        );
        let unreadable = FakeRunner::default();
        unreadable.exit("systemctl", "-p TriggeredBy", 1);
        assert_eq!(
            listener_inheritance(&unit, &Ok(None), 0, &unreadable).verdict,
            Verdict::Unknown
        );
        // The served field alone suffices.
        assert_eq!(
            listener_inheritance(
                &unit,
                &Ok(Some("inherited".into())),
                0,
                &FakeRunner::default()
            )
            .verdict,
            Verdict::Pass
        );
        // Self-managed needs registered fds.
        let sm = RestartPath::SelfManaged;
        assert_eq!(
            listener_inheritance(&sm, &Ok(Some("bound".into())), 0, &FakeRunner::default()).verdict,
            Verdict::Fail
        );
        if cfg!(unix) {
            assert_eq!(
                listener_inheritance(&sm, &Ok(None), 1, &FakeRunner::default()).verdict,
                Verdict::Pass
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn restart_mechanism_needs_restart_always_and_the_current_link() {
        let f = fixture();
        let unit = RestartPath::Systemd {
            unit: "qontinui-runner.service".into(),
        };
        let on_failure = FakeRunner::default();
        on_failure.ok("systemctl", "-p Restart", "on-failure");
        assert_eq!(
            restart_mechanism(&unit, &f.layout, &on_failure).verdict,
            Verdict::Fail
        );
        let dropin = FakeRunner::default();
        dropin.ok("systemctl", "-p Restart", "always").ok(
            "systemctl",
            "-p ExecStart",
            "{ path=/x/target/primary/4d524e71b/qontinui-runner ; }",
        );
        let c = restart_mechanism(&unit, &f.layout, &dropin);
        assert_eq!(c.verdict, Verdict::Fail);
        assert!(c.detail.contains("drop-in"));
        // `current` pointing elsewhere fails before any unit read.
        super::sys::swap_link_atomically(&f.layout.current_link, &f.candidate).unwrap();
        assert_eq!(
            restart_mechanism(&unit, &f.layout, &FakeRunner::default()).verdict,
            Verdict::Fail
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_schema_or_guard_is_unknown() {
        let f = fixture();
        let mut i = inputs(&f, Ok(healthy_census()));
        i.pg_schema = Err("no PostgreSQL configured".into());
        let runner = healthy_runner(&f);
        runner
            .rules
            .lock()
            .unwrap()
            .retain(|(p, _, _)| p != "systemd-run");
        let r = evaluate(i, &runner);
        assert_eq!(verdict_of(&r, names::PG_SCHEMA), Verdict::Unknown);
        assert_eq!(verdict_of(&r, names::ROLLBACK_GUARD), Verdict::Unknown);
        assert_eq!(r.blocking, vec![names::ROLLBACK_GUARD, names::PG_SCHEMA]);
    }

    // ── The census mapping, through the real restart-readiness planes ──────

    use crate::mcp::restart_readiness::{
        headless_plane_from, holder_plane_from, terminal_plane_from,
    };
    use crate::session::session_lifecycle_store::TerminalSessionRecord;
    use crate::session::tracking_health::{
        HandoffReason, HolderPane, HolderProtection, LiveClaudeProcess, PaneHandoff,
        TrackingHealthReport,
    };

    fn proc_(pid: u32, session: Option<&str>) -> LiveClaudeProcess {
        LiveClaudeProcess {
            pid,
            parent_pid: None,
            image: Some("claude".into()),
            age_s: Some(60),
            cwd: Some(format!("/w/{pid}")),
            has_live_children: Some(false),
            nested_under_claude: false,
            session_id: session.map(str::to_string),
            session_status: None,
            session_status_since: None,
            blocks_restart: true,
            wind_down: None,
            stray_wait_shells: 0,
        }
    }

    fn pane(id: &str, state: HolderPaneState, pids: &[u32]) -> HolderPane {
        HolderPane {
            pane_id: id.into(),
            state,
            holder_pid: Some(900),
            child_pid: Some(901),
            holder_build: Some("qontinui-pty-holder 0.1".into()),
            negotiated_version: Some(3),
            holder_versions: vec![1, 2, 3],
            detail: None,
            claude_pids: pids.to_vec(),
            protection: HolderProtection::Protected,
            handoff: PaneHandoff::Survivable,
            handoff_reason: None::<HandoffReason>,
        }
    }

    fn rec(id: &str, terminal: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: Some(format!("/acct/{id}")),
            working_dir: Some("/w".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: terminal.to_string(),
            opened_at: 1,
            last_seen_at: 1,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: None,
            restore_pending_at: None,
            awaiting_account_since: None,
            confirmed_at: None,
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            finish_coord_session_id: None,
            finish_coord_readdressed: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
        }
    }

    #[test]
    fn census_mapping_splits_holder_panes_from_the_dies_with_runner_set() {
        let held_session = "11111111-1111-1111-1111-111111111111";
        let loose_session = "22222222-2222-2222-2222-222222222222";
        let mut finished = proc_(30, Some("33333333-3333-3333-3333-333333333333"));
        finished.blocks_restart = false;
        let report = TrackingHealthReport {
            checked_at_ms: 1,
            live_claude_total: 4,
            tracked_open_total: 2,
            terminal_hosted: vec![
                proc_(10, Some(held_session)),
                proc_(20, Some(loose_session)),
                finished,
            ],
            ai_plane: vec![],
            headless_exempt: vec![proc_(40, None)],
            live_untracked: vec![],
            tracked_dead: vec![],
            holder_hosted: vec![],
            holder_panes: vec![
                pane("pane-1", HolderPaneState::Live, &[10]),
                pane("pane-dead", HolderPaneState::Dead, &[]),
            ],
            holder_census_error: None,
            undecided: Vec::new(),
        };
        let records = vec![rec(held_session, "pane-1"), rec(loose_session, "t-2")];
        let t = terminal_plane_from(&report, &records, 1000);
        let h = headless_plane_from(&report);
        let hp = holder_plane_from(&report);
        let a = AiPlane {
            count: 1,
            drain_covers_these: true,
            wip_capture_eligible: 0,
            oldest_session_age_s: None,
            sessions: vec![crate::mcp::restart_readiness::SessionEntry {
                id: "task-run-7".into(),
                age_s: None,
                state: "running".into(),
                terminal_id: None,
                title: None,
            }],
            processes: vec![],
        };
        let holders = live_holders_from(&hp, &t);
        assert_eq!(holders.len(), 1, "only the LIVE pane");
        assert_eq!(holders[0].pane_id, "pane-1");
        assert_eq!(holders[0].session_ids, vec![held_session.to_string()]);
        let cfg = |id: &str| Some(format!("/acct/{id}"));
        let dwr = dies_with_runner_from(&t, &h, &hp, &a, &cfg);
        let kinds: Vec<(&str, Option<&str>)> = dwr
            .iter()
            .map(|s| (s.kind.as_str(), s.session_id.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("in_process_pane", Some(loose_session)),
                ("headless", None),
                ("ai_session", Some("task-run-7")),
            ],
            "the holder-backed and the finished session are not in it"
        );
        assert_eq!(dwr[0].terminal_id.as_deref(), Some("t-2"));
        assert_eq!(
            dwr[0].config_dir.as_deref(),
            Some("/acct/22222222-2222-2222-2222-222222222222")
        );
    }
}
