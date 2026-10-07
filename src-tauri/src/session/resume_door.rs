//! `POST /control/sessions/resume {ids?, generation?}` — the runner's own door
//! for re-launching CLOSED sessions on THIS device.
//!
//! Plan `2026-10-06-closed-sessions-whose-work-is-unfinished-are-found-fleet-wide-and-resumed`
//! Phase 4. It is the respawn receiver's materialise flow
//! ([`super::respawn`]) with a LOCAL source: the transcript is already under the
//! row's own `CLAUDE_CONFIG_DIR`, so there is no coord fetch, no handoff bundle
//! and no account re-pin — the session comes back under the account it ran on,
//! in the cwd it ran in. The flow ends in the same shared seam the receiver
//! uses, [`crate::terminal::account_migration::spawn_resumed_pane`] (an argv
//! spawn, so no typed-command retry exists to fail).
//!
//! It is a backend route, deliberately independent of any mounted React
//! frontend (an HTTP door must work on a headless runner).
//!
//! ## Per-id flow ([`resume_one`])
//!
//! 1. the id is well-formed, has a lifecycle row, the row is `closed`, not
//!    marked finished, owned by the claude provider, with a known config dir and
//!    a cwd that still exists;
//! 2. the transcript exists locally — `--resume` against nothing would start an
//!    EMPTY conversation wearing the old id, the fabricated half this plan
//!    forbids;
//! 3. the device drain gate, as [`SpawnOrigin::Respawn`], deferred (never
//!    dropped) while the device is drained;
//! 4. the per-session account-hop budget, `MIGRATION_CAP`, shared with the
//!    automatic migration and the respawn receiver (checked last, immediately
//!    before the spawn, because the check consumes a slot);
//! 5. spawn, then VERIFY BY READING THE PROCESS: exactly one `claude --resume
//!    <id>` under the new pane, whose cwd and `CLAUDE_CONFIG_DIR` (where the
//!    platform exposes them) are the ones the row recorded.
//!
//! Every id gets exactly one verdict: `resumed`, `failed(<reason>)` or
//! `skipped(<reason>)`. A refusal before the spawn is a skip; anything that
//! went wrong at or after the spawn is a failure.
//!
//! ## `generation`
//!
//! The rebuild plan's ledger-generation restore shares this route. The ledger
//! carries no generation concept today ([`super::session_ledger`] has none), so
//! the field is ACCEPTED, logged and echoed back, and filters nothing. It is
//! not silently ignored: the report says `generation_applied: false`.
//!
//! The admission-lease path (plan 2026-09-13 spawn-admission) is not landed
//! here; once it is, a mid-batch refusal marks the rest `skipped(admission)`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use super::SessionRegistry;
use crate::coord_drain_state::{DrainGate, SpawnOrigin};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::TerminalManager;

/// Most ids one call will act on. Each resume is verified by a process read, so
/// an unbounded batch would pin the request for minutes.
pub const MAX_IDS: usize = 50;

/// How long to wait for the resumed `claude` to appear before calling the
/// spawn failed.
const VERIFY_DEADLINE: Duration = Duration::from_secs(20);
const VERIFY_POLL: Duration = Duration::from_millis(500);

/// Routes contributed by this module; merged in `mcp_api.rs`. A runner-native
/// route, not part of the UI-Bridge SDK manifest.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/control/sessions/resume", post(post_sessions_resume))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ResumeRequest {
    /// Claude session ids to resume. Absent = every closed, unfinished claude
    /// session this device's lifecycle store holds.
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// The rebuild plan's ledger generation. Accepted and echoed; see the
    /// module docs.
    #[serde(default)]
    pub generation: Option<String>,
}

/// One id's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeRow {
    pub id: String,
    /// `resumed` | `failed` | `skipped`.
    pub outcome: &'static str,
    /// Present for `failed` and `skipped`.
    pub reason: Option<String>,
    /// `resumed` | `failed(<reason>)` | `skipped(<reason>)` — the spelling the
    /// plan names, for a reader that wants one string.
    pub verdict: String,
    /// What the process read observed (only on `resumed`, or on a `failed`
    /// that got far enough to observe something).
    pub evidence: Option<ProcessEvidence>,
}

impl ResumeRow {
    fn resumed(id: &str, evidence: ProcessEvidence) -> Self {
        Self {
            id: id.to_string(),
            outcome: "resumed",
            reason: None,
            verdict: "resumed".to_string(),
            evidence: Some(evidence),
        }
    }
    fn failed(id: &str, reason: impl Into<String>, evidence: Option<ProcessEvidence>) -> Self {
        let reason = reason.into();
        Self {
            id: id.to_string(),
            outcome: "failed",
            verdict: format!("failed({reason})"),
            reason: Some(reason),
            evidence,
        }
    }
    fn skipped(id: &str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            id: id.to_string(),
            outcome: "skipped",
            verdict: format!("skipped({reason})"),
            reason: Some(reason),
            evidence: None,
        }
    }
}

/// What reading the process table showed about the resumed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessEvidence {
    pub terminal_id: String,
    pub pid: Option<u32>,
    pub cmdline: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub cwd: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub claude_config_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResumeReport {
    pub generation: Option<String>,
    /// `false` — no ledger generation exists to filter by. See module docs.
    pub generation_applied: bool,
    pub requested: usize,
    pub resumed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub results: Vec<ResumeRow>,
}

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/// A row that passed every precondition that needs no I/O beyond the row.
#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    config_dir: String,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
}

/// Why a lifecycle row cannot be resumed, or the candidate it yields.
fn classify(id: &str, rec: Option<&TerminalSessionRecord>) -> Result<Candidate, String> {
    if !super::session_id::is_valid_session_id(id) {
        return Err("invalid_id".to_string());
    }
    let Some(rec) = rec else {
        return Err("not_found".to_string());
    };
    if rec.provider != super::session_lifecycle_store::DEFAULT_PROVIDER {
        return Err(format!("provider_{}", rec.provider));
    }
    if rec.state != "closed" {
        return Err("not_closed".to_string());
    }
    if rec.finished_at.is_some() {
        return Err("finished".to_string());
    }
    let config_dir = rec
        .config_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no_config_dir".to_string())?;
    let working_dir = rec
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "no_working_dir".to_string())?;
    let short = id.get(..8).unwrap_or(id);
    Ok(Candidate {
        id: id.to_string(),
        config_dir: config_dir.to_string(),
        working_dir: working_dir.to_string(),
        title: rec
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("Resumed {short}")),
        page_id: rec.page_id.clone(),
        zone_index: rec.zone_index,
    })
}

/// The ids this call acts on: the caller's, de-duplicated in order, or every
/// closed unfinished claude row when none were given.
fn select_ids(req: &ResumeRequest, all: &[TerminalSessionRecord]) -> Vec<String> {
    match &req.ids {
        Some(ids) => {
            let mut seen = std::collections::HashSet::new();
            ids.iter()
                .map(|i| i.trim().to_string())
                .filter(|i| seen.insert(i.clone()))
                .collect()
        }
        None => {
            let mut rows: Vec<&TerminalSessionRecord> = all
                .iter()
                .filter(|r| {
                    r.state == "closed"
                        && r.finished_at.is_none()
                        && r.provider == super::session_lifecycle_store::DEFAULT_PROVIDER
                })
                .collect();
            // Most recently closed first: that is the work most likely still wanted.
            rows.sort_by_key(|r| std::cmp::Reverse(r.closed_at.unwrap_or(0)));
            rows.into_iter()
                .map(|r| r.claude_session_id.clone())
                .collect()
        }
    }
}

/// The admission decision after the I/O preconditions: drain first, then the
/// account-hop budget. `cap_permits` is called ONLY when the drain allows —
/// it records-and-checks, so charging a slot for a deferred attempt would let
/// a drain burn a session's whole 24h budget.
fn admit(gate: DrainGate, cap_permits: impl FnOnce() -> bool) -> Result<(), String> {
    if let DrainGate::Defer { reason, .. } = gate {
        return Err(format!("drain: {reason}"));
    }
    if !cap_permits() {
        return Err("cap_reached".to_string());
    }
    Ok(())
}

/// One live `claude` process as read from the process table.
#[derive(Debug, Clone)]
struct ClaudeProc {
    pid: u32,
    cmdline: String,
    cwd: Option<String>,
    config_dir: Option<String>,
}

/// `Ok` only when exactly one `claude --resume <id>` is present and every
/// fact the platform could read agrees with the row. `Err(None)` = not there
/// YET (keep polling); `Err(Some(reason))` = wrong, stop.
fn judge(
    procs: &[ClaudeProc],
    cand: &Candidate,
    terminal_id: &str,
) -> Result<ProcessEvidence, Option<String>> {
    let matching: Vec<&ClaudeProc> = procs
        .iter()
        .filter(|p| {
            crate::process_capture::process_tree::parse_session_id_from_cmdline(&p.cmdline)
                .as_deref()
                == Some(cand.id.as_str())
        })
        .collect();
    let p = match matching.as_slice() {
        [] => return Err(None),
        [one] => *one,
        many => return Err(Some(format!("multiple_resume_processes({})", many.len()))),
    };
    if let Some(cwd) = &p.cwd {
        if !same_path(cwd, &cand.working_dir) {
            return Err(Some(format!(
                "cwd_mismatch(want {}, got {cwd})",
                cand.working_dir
            )));
        }
    }
    if let Some(dir) = &p.config_dir {
        if !same_path(dir, &cand.config_dir) {
            return Err(Some(format!(
                "config_dir_mismatch(want {}, got {dir})",
                cand.config_dir
            )));
        }
    }
    Ok(ProcessEvidence {
        terminal_id: terminal_id.to_string(),
        pid: Some(p.pid),
        cmdline: Some(p.cmdline.clone()),
        cwd: p.cwd.clone(),
        claude_config_dir: p.config_dir.clone(),
    })
}

/// Path equality tolerant of trailing separators, `\` vs `/`, and symlinks
/// where both sides resolve.
fn same_path(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let canon = std::fs::canonicalize(s)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| s.to_string());
        canon
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }
    norm(a) == norm(b)
}

/// `CLAUDE_CONFIG_DIR` of a live process. Linux only (`/proc/<pid>/environ`);
/// elsewhere `None`, which is "could not say", never a match.
#[cfg(target_os = "linux")]
fn config_dir_of_pid(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    bytes
        .split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CLAUDE_CONFIG_DIR=").map(str::to_string))
        .filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn config_dir_of_pid(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// I/O: the spawn site
// ---------------------------------------------------------------------------

/// Read the claude processes under `terminal_id`'s pane.
async fn read_claude_procs(
    tm: &Arc<TerminalManager>,
    terminal_id: &str,
) -> Option<Vec<ClaudeProc>> {
    use crate::process_capture::process_tree as pt;
    let root = tm.get(terminal_id)?.child_pid()?;
    let snap = pt::snapshot_process_table_public().await;
    let pids = pt::claude_pids_in_inclusive_subtree(root, &snap);
    let cmdlines = pt::command_lines_for_pids(&pids).await;
    let cwds = pt::working_directories_for_pids(&pids).await;
    Some(
        pids.into_iter()
            .filter_map(|pid| {
                Some(ClaudeProc {
                    pid,
                    cmdline: cmdlines.get(&pid)?.clone(),
                    cwd: cwds.get(&pid).cloned(),
                    config_dir: config_dir_of_pid(pid),
                })
            })
            .collect(),
    )
}

/// Poll the process table until the resumed claude is proven (or disproven).
async fn verify(
    tm: &Arc<TerminalManager>,
    cand: &Candidate,
    terminal_id: &str,
) -> Result<ProcessEvidence, String> {
    let deadline = tokio::time::Instant::now() + VERIFY_DEADLINE;
    loop {
        if tm.get(terminal_id).is_none() {
            return Err("pane_vanished_before_claude_appeared".to_string());
        }
        if let Some(procs) = read_claude_procs(tm, terminal_id).await {
            match judge(&procs, cand, terminal_id) {
                Ok(ev) => return Ok(ev),
                Err(Some(reason)) => return Err(reason),
                Err(None) => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("no_claude_resume_process_observed".to_string());
        }
        tokio::time::sleep(VERIFY_POLL).await;
    }
}

/// Resume one session: gate, budget, spawn, verify. The spawn site registered
/// in `runner_spawn_sites.txt` — the drain gate lives HERE and branches on its
/// result.
async fn resume_one(
    app: &tauri::AppHandle,
    tm: &Arc<TerminalManager>,
    registry: &Arc<SessionRegistry>,
    cand: Candidate,
) -> ResumeRow {
    // The transcript, local — a missing one must never become `--resume` of
    // nothing.
    let transcript = crate::terminal::transcript::session_transcript_path(
        Path::new(&cand.config_dir),
        &cand.working_dir,
        &cand.id,
    );
    if !transcript.is_file() {
        return ResumeRow::skipped(&cand.id, "no_transcript");
    }
    if !Path::new(&cand.working_dir).is_dir() {
        return ResumeRow::skipped(&cand.id, "working_dir_missing");
    }
    let gate = match crate::coord_drain_state::drain_gate_for_work(
        SpawnOrigin::Respawn,
        &format!("resume:{}", cand.id),
    ) {
        DrainGate::Allow => DrainGate::Allow,
        deferred => deferred,
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    if let Err(why) = admit(gate, || {
        crate::terminal::account_migration::migration_cap_permits(&cand.id, now_ms)
    }) {
        return ResumeRow::skipped(&cand.id, why);
    }

    let spawn = {
        let (app, tm, registry, cand) = (app.clone(), tm.clone(), registry.clone(), cand.clone());
        tokio::task::spawn_blocking(move || {
            let model_transcript = crate::terminal::transcript::session_transcript_path(
                Path::new(&cand.config_dir),
                &cand.working_dir,
                &cand.id,
            );
            crate::terminal::account_migration::spawn_resumed_pane(
                &app,
                &tm,
                &registry,
                crate::terminal::account_migration::ResumeSpawn {
                    claude_session_id: &cand.id,
                    working_dir: &cand.working_dir,
                    model_transcript,
                    config_dir: &cand.config_dir,
                    title: cand.title.clone(),
                    page_id: cand.page_id.clone(),
                    zone_index: cand.zone_index,
                    work_unit_slug: None,
                    correlation_topic: None,
                    intent_repo: None,
                    coord_lineage: Some(crate::commands::terminal::CoordSessionLineage {
                        parent_session_id: None,
                        claude_code_session_id: Some(cand.id.clone()),
                    }),
                    // Creates a new process: respect the spawn-time resource
                    // floor like every other autonomous spawn.
                    resource_override: false,
                    // The source's continuation registration, if any, is not
                    // re-claimed from a closed row.
                    gate_identity: None,
                },
            )
        })
        .await
    };
    let terminal_id = match spawn {
        Ok(Ok((terminal_id, _coord_id))) => terminal_id,
        Ok(Err(e)) => return ResumeRow::failed(&cand.id, format!("spawn: {e}"), None),
        Err(e) => return ResumeRow::failed(&cand.id, format!("spawn_task: {e}"), None),
    };
    match verify(tm, &cand, &terminal_id).await {
        Ok(ev) => ResumeRow::resumed(&cand.id, ev),
        Err(reason) => ResumeRow::failed(
            &cand.id,
            reason,
            Some(ProcessEvidence {
                terminal_id,
                pid: None,
                cmdline: None,
                cwd: None,
                claude_config_dir: None,
            }),
        ),
    }
}

fn tally(generation: Option<String>, requested: usize, results: Vec<ResumeRow>) -> ResumeReport {
    let n = |o: &str| results.iter().filter(|r| r.outcome == o).count();
    ResumeReport {
        generation,
        generation_applied: false,
        requested,
        resumed: n("resumed"),
        failed: n("failed"),
        skipped: n("skipped"),
        results,
    }
}

async fn post_sessions_resume(
    State(state): State<Arc<ApiState>>,
    body: Bytes,
) -> Result<Json<ApiResponse<ResumeReport>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    use axum::http::StatusCode;
    use tauri::Manager;

    let req: ResumeRequest = if body.iter().all(u8::is_ascii_whitespace) {
        ResumeRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "body must be {{ids?, generation?}}: {e}"
                ))),
            )
        })?
    };
    if req.ids.as_ref().is_some_and(|i| i.len() > MAX_IDS) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("at most {MAX_IDS} ids per call"))),
        ));
    }
    let unavailable = |what: &str| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(format!(
                "{what} not available — NOTHING was resumed"
            ))),
        )
    };
    let store = state
        .app_handle
        .try_state::<Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("lifecycle store"))?;
    let tm = state
        .app_handle
        .try_state::<Arc<TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("terminal manager"))?;
    let registry = state
        .app_handle
        .try_state::<Arc<SessionRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| unavailable("session registry"))?;

    let ids = select_ids(&req, &store.all_records());
    if ids.len() > MAX_IDS {
        warn!(
            n = ids.len(),
            "resume_door: more unfinished sessions than one call takes; acting on the first {MAX_IDS}"
        );
    }
    info!(
        requested = ids.len(),
        generation = ?req.generation,
        "resume_door: resuming closed sessions"
    );

    let mut results = Vec::new();
    for id in ids.iter().take(MAX_IDS) {
        let row = match classify(id, store.get(id).as_ref()) {
            Err(why) => ResumeRow::skipped(id, why),
            Ok(cand) => resume_one(&state.app_handle, &tm, &registry, cand).await,
        };
        info!(id = %row.id, verdict = %row.verdict, "resume_door: verdict");
        results.push(row);
    }
    Ok(Json(ApiResponse::success(tally(
        req.generation,
        ids.len().min(MAX_IDS),
        results,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord_drain_state::DeferClass;

    const ID: &str = "0b9f6c1e-1111-4222-8333-444455556666";

    fn rec(state: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: ID.to_string(),
            config_dir: Some("/cfg".to_string()),
            working_dir: Some("/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 2,
            title: None,
            terminal_id: "t".to_string(),
            opened_at: 0,
            last_seen_at: 0,
            state: state.to_string(),
            closed_at: Some(5),
            close_reason: None,
            provider: super::super::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: None,
            restore_pending_at: None,
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
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
        }
    }

    fn reason(r: Result<Candidate, String>) -> String {
        r.expect_err("must be skipped")
    }

    #[test]
    fn classify_names_every_skip_reason() {
        assert_eq!(
            reason(classify("bad id!", Some(&rec("closed")))),
            "invalid_id"
        );
        assert_eq!(reason(classify(ID, None)), "not_found");
        assert_eq!(reason(classify(ID, Some(&rec("open")))), "not_closed");
        let mut finished = rec("closed");
        finished.finished_at = Some(9);
        assert_eq!(reason(classify(ID, Some(&finished))), "finished");
        let mut other = rec("closed");
        other.provider = "gemini".to_string();
        assert_eq!(reason(classify(ID, Some(&other))), "provider_gemini");
        let mut nocfg = rec("closed");
        nocfg.config_dir = Some("  ".to_string());
        assert_eq!(reason(classify(ID, Some(&nocfg))), "no_config_dir");
        let mut nowd = rec("closed");
        nowd.working_dir = None;
        assert_eq!(reason(classify(ID, Some(&nowd))), "no_working_dir");
        let ok = classify(ID, Some(&rec("closed"))).unwrap();
        assert_eq!((ok.config_dir.as_str(), ok.zone_index), ("/cfg", 2));
    }

    #[test]
    fn select_ids_defaults_to_closed_unfinished_newest_first_and_dedups_explicit() {
        let mut a = rec("closed");
        a.claude_session_id = "a".into();
        a.closed_at = Some(1);
        let mut b = rec("closed");
        b.claude_session_id = "b".into();
        b.closed_at = Some(9);
        let mut fin = rec("closed");
        fin.claude_session_id = "fin".into();
        fin.finished_at = Some(1);
        let mut open = rec("open");
        open.claude_session_id = "open".into();
        let all = [a, b, fin, open];
        assert_eq!(select_ids(&ResumeRequest::default(), &all), ["b", "a"]);
        let explicit = ResumeRequest {
            ids: Some(vec![" x ".into(), "x".into(), "y".into()]),
            generation: None,
        };
        assert_eq!(select_ids(&explicit, &all), ["x", "y"]);
    }

    #[test]
    fn drain_defers_without_charging_the_cap() {
        let mut asked = false;
        let gate = DrainGate::Defer {
            reason: "device drained".into(),
            class: DeferClass::Drained,
        };
        let r = admit(gate, || {
            asked = true;
            true
        });
        assert_eq!(r, Err("drain: device drained".to_string()));
        assert!(!asked, "a deferred attempt must not consume a budget slot");
    }

    #[test]
    fn the_cap_admits_three_then_refuses_the_fourth() {
        let id = "cap-test-1b2c3d4e-resume-door";
        let now = chrono::Utc::now().timestamp_millis();
        let permits = || crate::terminal::account_migration::migration_cap_permits(id, now);
        for _ in 0..crate::terminal::account_migration::MIGRATION_CAP {
            assert_eq!(admit(DrainGate::Allow, permits), Ok(()));
        }
        assert_eq!(
            admit(DrainGate::Allow, permits),
            Err("cap_reached".to_string())
        );
    }

    fn cand() -> Candidate {
        classify(ID, Some(&rec("closed"))).unwrap()
    }

    fn proc(cmd: &str, cwd: Option<&str>, cfg: Option<&str>) -> ClaudeProc {
        ClaudeProc {
            pid: 42,
            cmdline: cmd.to_string(),
            cwd: cwd.map(String::from),
            config_dir: cfg.map(String::from),
        }
    }

    #[test]
    fn judge_requires_exactly_one_matching_resume_process() {
        let good = format!("/bin/claude --permission-mode bypassPermissions --resume {ID}");
        // Not there yet: keep polling.
        assert_eq!(judge(&[], &cand(), "t").unwrap_err(), None);
        let other = "/bin/claude --resume 11111111-1111-4111-8111-111111111111";
        assert_eq!(
            judge(&[proc(other, None, None)], &cand(), "t").unwrap_err(),
            None
        );
        // One match, unreadable cwd/config: accepted, fields stay null.
        let ev = judge(&[proc(&good, None, None)], &cand(), "t").unwrap();
        assert_eq!(
            (ev.pid, ev.cwd, ev.claude_config_dir),
            (Some(42), None, None)
        );
        // Two matches: a double spawn is a failure, not a success.
        let two = [proc(&good, None, None), proc(&good, None, None)];
        assert_eq!(
            judge(&two, &cand(), "t").unwrap_err(),
            Some("multiple_resume_processes(2)".to_string())
        );
    }

    #[test]
    fn judge_fails_on_a_wrong_cwd_or_config_dir_but_not_on_unknown() {
        let good = format!("claude --resume {ID}");
        let bad_cwd = judge(&[proc(&good, Some("/elsewhere"), None)], &cand(), "t");
        assert!(bad_cwd.unwrap_err().unwrap().starts_with("cwd_mismatch"));
        let bad_cfg = judge(&[proc(&good, Some("/work"), Some("/other"))], &cand(), "t");
        assert!(bad_cfg
            .unwrap_err()
            .unwrap()
            .starts_with("config_dir_mismatch"));
        assert!(judge(&[proc(&good, Some("/work/"), Some("/cfg"))], &cand(), "t").is_ok());
    }

    #[test]
    fn verdict_strings_use_the_plan_spelling() {
        assert_eq!(
            ResumeRow::skipped("i", "cap_reached").verdict,
            "skipped(cap_reached)"
        );
        assert_eq!(ResumeRow::failed("i", "boom", None).verdict, "failed(boom)");
        let report = tally(
            Some("g1".into()),
            3,
            vec![
                ResumeRow::skipped("a", "x"),
                ResumeRow::failed("b", "y", None),
                ResumeRow::skipped("c", "z"),
            ],
        );
        assert_eq!((report.resumed, report.failed, report.skipped), (0, 1, 2));
        assert!(!report.generation_applied);
    }
}
