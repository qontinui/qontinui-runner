//! `GET /restart-readiness` — the one surface that answers *"is it safe to
//! restart this runner?"* (plan
//! `2026-08-29-no-single-answer-to-is-it-safe-to-restart-the-runner`, Phase 1).
//!
//! ## The incident this exists to prevent
//!
//! An operator nearly restarted a runner carrying 23 live agent sessions.
//! They reasoned correctly from the evidence the runner offered:
//! `GET /task-runs/running` returned `[]` (it is a port-filtered *workflow*
//! ledger) and `GET /sessions/history` showed one closed row (it is a
//! DISPLAY-only record of *past* terminal sessions). Both answered their own
//! narrow question truthfully; both read as *idle*. The authoritative count
//! was a nested field inside `/health` → `data.sessionTracking`, under a key
//! that reads like a subsystem-health metric.
//!
//! ## Two disjoint session planes — the crux
//!
//! There is no single number, so this endpoint never emits one (D6):
//!
//! | | `terminal_sessions` | `ai_sessions` |
//! |---|---|---|
//! | Population | `claude` processes in the runner's **inclusive process subtree**, cross-referenced against open `SessionLifecycleStore` records — i.e. terminal-hosted agent sessions ([`crate::session::tracking_health`]) | `SessionManager::active_claude_sessions()` — the AI / task-run plane, keyed by `task_run_id` |
//! | `POST /drain` acts on it | **NO** | yes |
//!
//! The census **explicitly exempts** the AI plane (it subtracts precisely the
//! set `drain()` operates on), so a session counted in `terminal_sessions` is
//! by definition a session a drain will not touch. Measured on `merytshost`
//! 2026-08-29: `liveClaudeTotal: 25` while `/task-runs/running` returned `[]`,
//! so a drain right then would have taken its
//! `"drain: no live AI sessions — fast no-op"` branch and reported
//! `drained_sessions: 0` while 25 live agent sessions carried on.
//!
//! **Hence D3: this endpoint never emits `drain_required`, and never
//! recommends a drain for the terminal plane.** A verdict that said
//! *"unsafe → drain → now safe"* for that population would manufacture a false
//! safe carrying the runner's own authority — strictly worse than the status
//! quo it replaces.
//!
//! ## Fresh, not cached (D5)
//!
//! The verdict calls [`crate::session::tracking_health::compute`] on demand.
//! It never reads `tracking_health::latest()`: `CHECK_INTERVAL` is **600 s**,
//! so the `/health` cache is routinely minutes stale (measured: `lastCheckAt`
//! advanced exactly once across 19 polls, by 600,024 ms), and for the first
//! 120 s after boot the count fields are *absent from the object entirely* —
//! a consumer's `?? 0` reads that as idle during the highest-risk window there
//! is, because boot-restore is re-opening exactly the sessions a restart would
//! destroy. The background census's age is reported in `census` as
//! **observability only**; the verdict does not depend on it.
//!
//! Computing fresh is reuse, not a second census (D1): it is the same
//! `evaluate` body, over the same handles, keyed on the same
//! `primary_boot_unix_millis`. Nothing here counts anything on its own.
//!
//! ## Fail closed
//!
//! `safe_to_restart` is `false` on every unknown — an unreadable process
//! table, an unresolvable `SessionManager`/`TerminalManager`/lifecycle store,
//! an uninitialized PID-reuse reference — with the cause named in `reason` and
//! the affected plane serialized as `null` rather than `0`. This surface is
//! consulted precisely when someone is about to do something destructive.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::mcp::types::ApiState;
use crate::session::session_lifecycle_store::TerminalSessionRecord;
use crate::session::tracking_health::{self, TrackingHealthReport};

/// What the subtree cross-reference structurally cannot see. Emitted verbatim
/// on every response so a reader is never invited to infer omniscience from a
/// confident-looking count.
pub const BOUNDARY: &str = "counts `claude` processes in this runner's inclusive process subtree; a session doing non-`claude` work, or a child that escaped the subtree, is not represented";

/// `drain.covers` — the constant, honest scope of `POST /drain`.
pub const DRAIN_COVERS: &str = "ai_sessions only";

/// How many recent task runs to pull when resolving AI-plane `age_s`. One
/// lightweight, deliberately **un**-port-filtered query (the port filter is
/// what made `/task-runs/running` return `[]` during the incident).
const TASK_RUN_AGE_LOOKUP_LIMIT: u32 = 500;

// ---------------------------------------------------------------------------
// Response shape
// ---------------------------------------------------------------------------

/// One live session in either plane.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    /// Seconds since this session started, or `null` where no creation
    /// timestamp is reachable. NEVER a fabricated value: `ClaudeSession` has
    /// no creation field at all (only `last_activity_tracker()`, which mutates
    /// on activity and is not a start time), so an AI-plane entry whose
    /// `task_runs` join misses is honestly unknown.
    pub age_s: Option<i64>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The terminal-hosted agent-session plane — the population in the incident,
/// and the one `liveClaudeTotal` reports.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TerminalPlane {
    /// Live `claude` processes in the runner's inclusive subtree
    /// (`live_claude_total`) — the process-level truth, which includes any
    /// live session that has NO durable record (`live_untracked_count`).
    pub count: usize,
    /// **Always `false`.** `drain()` operates on `active_claude_sessions()`,
    /// a set the census explicitly exempts. See D3.
    pub drain_covers_these: bool,
    /// Open `SessionLifecycleStore` records at compute time.
    pub tracked_open_total: usize,
    /// Live `claude` processes no open record accounts for — these would be
    /// dropped silently by a restart AND are absent from `sessions` below.
    pub live_untracked_count: usize,
    /// Open records whose terminal is gone / whose subtree has no live
    /// `claude`. Reported for observability; does NOT affect the verdict.
    pub tracked_dead_count: usize,
    /// Max over the non-`null` `age_s` in `sessions`, or `null`.
    pub oldest_session_age_s: Option<i64>,
    /// The live tracked sessions (open records minus the tracked-dead ones).
    pub sessions: Vec<SessionEntry>,
}

/// The AI / task-run plane — `SessionManager::active_claude_sessions()`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiPlane {
    pub count: usize,
    /// `true` — this is the only plane `POST /drain` acts on.
    pub drain_covers_these: bool,
    /// Subset holding an isolated `worktree()`. A drain writes
    /// `refs/wip/<agent_session_id>` only for these; a session in the shared
    /// cwd is deliberately skipped so a shared checkout is not polluted.
    pub wip_capture_eligible: usize,
    pub oldest_session_age_s: Option<i64>,
    pub sessions: Vec<SessionEntry>,
}

/// Drain state, reported — never performed. A drain is TERMINAL (`DRAINING`
/// is never reset), so triggering one on an operator's behalf would silently
/// end the runner's useful life.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DrainInfo {
    pub already_drained: bool,
    pub is_draining: bool,
    /// `true` when `ai_sessions.count == 0` (or a drain already completed) —
    /// i.e. calling `POST /drain` right now would change nothing.
    pub would_be_noop: bool,
    /// Always [`DRAIN_COVERS`].
    pub covers: &'static str,
    pub call: String,
}

/// Age/health of the BACKGROUND `tracking_health` task.
///
/// ⚠ **Observability only.** This endpoint computes its own fresh pass; none
/// of these fields feeds `safe_to_restart` (D5).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CensusInfo {
    /// `checked_at_ms` of the cached report `/health` serves, or `null` before
    /// the first background pass completes.
    pub background_last_check_at: Option<i64>,
    pub background_age_s: Option<i64>,
    pub check_interval_s: u64,
    /// `false` when `background_age_s > 2 * check_interval_s`; `null` while no
    /// background pass has completed (the task's 120 s initial delay may
    /// simply not have elapsed — unknown, not healthy).
    pub periodic_task_healthy: Option<bool>,
}

/// The `GET /restart-readiness` body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RestartReadiness {
    pub safe_to_restart: bool,
    pub reason: String,
    /// `null` when the plane could not be determined — never `0`.
    pub terminal_sessions: Option<TerminalPlane>,
    /// `null` when the plane could not be determined — never `0`.
    pub ai_sessions: Option<AiPlane>,
    pub drain: DrainInfo,
    pub census: CensusInfo,
    pub boundary: &'static str,
}

// ---------------------------------------------------------------------------
// Pure shaping + verdict
// ---------------------------------------------------------------------------

fn age_s_from(started_ms: i64, now_ms: i64) -> Option<i64> {
    if started_ms <= 0 {
        return None;
    }
    let secs = (now_ms - started_ms) / 1000;
    // A record stamped in the future is a clock artifact, not an age.
    if secs < 0 {
        None
    } else {
        Some(secs)
    }
}

fn oldest(sessions: &[SessionEntry]) -> Option<i64> {
    sessions.iter().filter_map(|s| s.age_s).max()
}

/// Shape the terminal plane from one [`tracking_health`] pass.
///
/// `sessions` is the open records MINUS the pass's `tracked_dead` set (a stale
/// row masquerading as a restorable session is not live work). `count` stays
/// the process-level `live_claude_total` so a live-but-untracked `claude` —
/// which by definition has no record to list — is still counted.
pub fn terminal_plane_from(
    report: &TrackingHealthReport,
    open_records: &[TerminalSessionRecord],
    now_ms: i64,
) -> TerminalPlane {
    let dead: std::collections::HashSet<&str> = report
        .tracked_dead
        .iter()
        .map(|d| d.claude_session_id.as_str())
        .collect();

    let sessions: Vec<SessionEntry> = open_records
        .iter()
        .filter(|r| !dead.contains(r.claude_session_id.as_str()))
        .map(|r| SessionEntry {
            id: r.claude_session_id.clone(),
            age_s: age_s_from(r.opened_at, now_ms),
            state: r.state.clone(),
            terminal_id: Some(r.terminal_id.clone()),
            title: r.title.clone(),
        })
        .collect();

    TerminalPlane {
        count: report.live_claude_total,
        // D3: never true. `drain()` acts on a set this census subtracts.
        drain_covers_these: false,
        tracked_open_total: report.tracked_open_total,
        live_untracked_count: report.live_untracked.len(),
        tracked_dead_count: report.tracked_dead.len(),
        oldest_session_age_s: oldest(&sessions),
        sessions,
    }
}

/// One AI-plane session as collected from `SessionManager` + the `task_runs`
/// creation-time join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSessionInput {
    /// `task_run_id` — the AI plane's key.
    pub id: String,
    pub state: String,
    /// Holds an isolated `worktree()`, so a drain could write a WIP ref.
    pub has_worktree: bool,
    /// `task_runs.created_at` in unix millis, or `None` where the join missed.
    pub created_at_ms: Option<i64>,
}

pub fn ai_plane_from(entries: &[AiSessionInput], now_ms: i64) -> AiPlane {
    let sessions: Vec<SessionEntry> = entries
        .iter()
        .map(|e| SessionEntry {
            id: e.id.clone(),
            age_s: e.created_at_ms.and_then(|ms| age_s_from(ms, now_ms)),
            state: e.state.clone(),
            terminal_id: None,
            title: None,
        })
        .collect();

    AiPlane {
        count: entries.len(),
        drain_covers_these: true,
        wip_capture_eligible: entries.iter().filter(|e| e.has_worktree).count(),
        oldest_session_age_s: oldest(&sessions),
        sessions,
    }
}

/// Shape the background-census observability block. Never feeds the verdict.
pub fn census_info(latest: Option<&TrackingHealthReport>, now_ms: i64) -> CensusInfo {
    let interval_s = tracking_health::CHECK_INTERVAL.as_secs();
    match latest {
        Some(r) => {
            let age_s = ((now_ms - r.checked_at_ms) / 1000).max(0);
            CensusInfo {
                background_last_check_at: Some(r.checked_at_ms),
                background_age_s: Some(age_s),
                check_interval_s: interval_s,
                periodic_task_healthy: Some(age_s <= (interval_s as i64) * 2),
            }
        }
        None => CensusInfo {
            background_last_check_at: None,
            background_age_s: None,
            check_interval_s: interval_s,
            // Unknown, not healthy — the 120 s initial delay may not have
            // elapsed, or the task may have died before its first pass.
            periodic_task_healthy: None,
        },
    }
}

/// Compose the verdict.
///
/// **Fail closed.** Any `unknowns` entry, or either plane missing, forces
/// `safe_to_restart: false` with the cause named. There is no path on which an
/// unknown resolves to `true`.
///
/// **D3.** The reason string never recommends a drain, and no `drain_required`
/// field exists to be set.
pub fn build_verdict(
    terminal: Option<TerminalPlane>,
    ai: Option<AiPlane>,
    unknowns: Vec<String>,
    drain: DrainInfo,
    census: CensusInfo,
) -> RestartReadiness {
    let mut unknowns = unknowns;
    if terminal.is_none() && !unknowns.iter().any(|u| u.contains("terminal")) {
        unknowns.push("the terminal-session plane could not be determined".to_string());
    }
    if ai.is_none() && !unknowns.iter().any(|u| u.contains("AI")) {
        unknowns.push("the AI/task-run plane could not be determined".to_string());
    }

    if !unknowns.is_empty() {
        return RestartReadiness {
            safe_to_restart: false,
            reason: format!("UNKNOWN, so treated as unsafe: {}", unknowns.join("; ")),
            terminal_sessions: terminal,
            ai_sessions: ai,
            drain,
            census,
            boundary: BOUNDARY,
        };
    }

    // Both planes resolved.
    let t = terminal.expect("checked above");
    let a = ai.expect("checked above");

    let mut parts: Vec<String> = Vec::new();
    if t.count > 0 {
        parts.push(format!(
            "{} terminal-hosted agent session{} {} live; no graceful stop path exists for them, so their in-flight work will be lost",
            t.count,
            if t.count == 1 { "" } else { "s" },
            if t.count == 1 { "is" } else { "are" },
        ));
    }
    if t.live_untracked_count > 0 {
        parts.push(format!(
            "{} of those live `claude` process{} have no durable lifecycle record at all",
            t.live_untracked_count,
            if t.live_untracked_count == 1 {
                ""
            } else {
                "es"
            },
        ));
    }
    if a.count > 0 {
        parts.push(format!(
            "{} AI/task-run session{} {} live ({} hold an isolated worktree)",
            a.count,
            if a.count == 1 { "" } else { "s" },
            if a.count == 1 { "is" } else { "are" },
            a.wip_capture_eligible,
        ));
    }

    let safe = t.count == 0 && a.count == 0;
    let reason = if safe {
        "no live agent sessions in either plane".to_string()
    } else {
        parts.join("; ")
    };

    RestartReadiness {
        safe_to_restart: safe,
        reason,
        terminal_sessions: Some(t),
        ai_sessions: Some(a),
        drain,
        census,
        boundary: BOUNDARY,
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Parse `task_runs.created_at` (RFC 3339, per `PgDb`'s `to_rfc3339()`) into
/// unix millis. Anything unparseable is `None` — an honest unknown.
fn rfc3339_to_millis(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// `GET /restart-readiness` — see the module docs.
pub async fn restart_readiness_handler(
    State(state): State<Arc<ApiState>>,
) -> Json<RestartReadiness> {
    use tauri::Manager;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let app = &state.app_handle;
    let mut unknowns: Vec<String> = Vec::new();

    // ── Drain state: reported, never performed ────────────────────────────
    let port = state
        .app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);
    let already_drained = crate::drain::already_drained();

    // ── AI plane ──────────────────────────────────────────────────────────
    let ai_raw: Option<Vec<(String, String, bool)>> =
        match app.try_state::<Arc<crate::claude_session::SessionManager>>() {
            Some(sm) => Some(
                sm.active_claude_sessions()
                    .into_iter()
                    .map(|(task_run_id, session)| {
                        (
                            task_run_id,
                            session.state().to_string(),
                            session.worktree().is_some(),
                        )
                    })
                    .collect(),
            ),
            None => {
                unknowns.push(
                    "the AI/task-run plane could not be determined: SessionManager did not resolve"
                        .to_string(),
                );
                None
            }
        };

    // Creation times for the AI plane come from `task_runs.created_at` —
    // `ClaudeSession` carries none. ONE lightweight query, deliberately NOT
    // port-filtered (the port filter is exactly what made `/task-runs/running`
    // read as idle during the incident). A failure here costs `age_s: null`
    // per entry, never a fabricated age and never the verdict.
    let ai = match ai_raw {
        Some(raw) if raw.is_empty() => Some(ai_plane_from(&[], now_ms)),
        Some(raw) => {
            let created: std::collections::HashMap<String, i64> = match state
                .app_state
                .pg_db
                .get_recent_task_runs(TASK_RUN_AGE_LOOKUP_LIMIT, None)
                .await
            {
                Ok(runs) => runs
                    .into_iter()
                    .filter_map(|r| rfc3339_to_millis(&r.created_at).map(|ms| (r.id, ms)))
                    .collect(),
                Err(e) => {
                    tracing::debug!(
                        "restart-readiness: task_runs creation-time lookup failed ({e}) — \
                         AI-plane age_s will be null"
                    );
                    std::collections::HashMap::new()
                }
            };
            let inputs: Vec<AiSessionInput> = raw
                .into_iter()
                .map(|(id, st, has_worktree)| AiSessionInput {
                    created_at_ms: created.get(&id).copied(),
                    id,
                    state: st,
                    has_worktree,
                })
                .collect();
            Some(ai_plane_from(&inputs, now_ms))
        }
        None => None,
    };

    // ── Terminal plane: a FRESH tracking_health pass (D5), never latest() ──
    let terminal = 'terminal: {
        let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: TerminalManager did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(store) =
            app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionLifecycleStore did not resolve"
                    .to_string(),
            );
            break 'terminal None;
        };
        let Some(sm) = app.try_state::<Arc<crate::claude_session::SessionManager>>() else {
            unknowns.push(
                "the terminal-session plane could not be determined: SessionManager did not resolve, so the exempt AI plane cannot be subtracted"
                    .to_string(),
            );
            break 'terminal None;
        };
        // NEVER `now()` here — that reference feeds the PID-reuse guard and
        // substituting it falsely flips live idle sessions to tracked-dead.
        let Some(boot_ms) = tracking_health::primary_boot_unix_millis() else {
            unknowns.push(
                "the terminal-session plane could not be determined: the PID-reuse guard's primary-boot reference is not initialized yet (the runner is still starting)"
                    .to_string(),
            );
            break 'terminal None;
        };

        match tracking_health::compute(tm.inner(), store.inner(), sm.inner(), boot_ms).await {
            Some(pass) => Some(terminal_plane_from(
                &pass.report,
                &pass.open_records,
                now_ms,
            )),
            None => {
                unknowns.push(
                    "the terminal-session plane could not be determined: the process table is unreadable (snapshot_process_table_public returned an empty parent_map), so live `claude` processes cannot be enumerated"
                        .to_string(),
                );
                None
            }
        }
    };

    let drain = DrainInfo {
        already_drained,
        is_draining: crate::drain::is_draining(),
        would_be_noop: already_drained || ai.as_ref().map(|p| p.count == 0).unwrap_or(false),
        covers: DRAIN_COVERS,
        call: format!("POST http://127.0.0.1:{port}/drain"),
    };

    let census = census_info(tracking_health::latest().as_ref(), now_ms);

    Json(build_verdict(terminal, ai, unknowns, drain, census))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tracking_health::{
        evaluate, LiveUntrackedProcess, TrackedDeadRecord, TrackingHealthReport,
    };
    use std::collections::{HashMap, HashSet};

    // Mirrors `tracking_health::tests::snap_with` — the same synthetic-snapshot
    // harness, so these tests drive the REAL `evaluate` body rather than a
    // hand-written stand-in.
    fn snap_with(
        parent_map: &[(u32, &[u32])],
        creation: &[(u32, i64)],
        names: &[(u32, &str)],
    ) -> crate::process_capture::process_tree::ProcessSnapshot {
        let mut s = crate::process_capture::process_tree::ProcessSnapshot::default();
        for (p, kids) in parent_map {
            s.parent_map.insert(*p, kids.to_vec());
        }
        for (pid, t) in creation {
            s.creation_times.insert(*pid, *t);
        }
        for (pid, n) in names {
            s.names.insert(*pid, n.to_string());
        }
        s
    }

    fn record(claude_session_id: &str, terminal_id: &str, opened_at: i64) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: claude_session_id.to_string(),
            config_dir: None,
            working_dir: Some("D:/work".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: Some(format!("title-{claude_session_id}")),
            terminal_id: terminal_id.to_string(),
            opened_at,
            last_seen_at: opened_at,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
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
            finish_reason: None,
            finish_synced: false,
        }
    }

    fn idle_drain() -> DrainInfo {
        DrainInfo {
            already_drained: false,
            is_draining: false,
            would_be_noop: true,
            covers: DRAIN_COVERS,
            call: "POST http://127.0.0.1:9876/drain".to_string(),
        }
    }

    fn fresh_census(now_ms: i64) -> CensusInfo {
        census_info(
            Some(&TrackingHealthReport {
                checked_at_ms: now_ms - 60_000,
                live_claude_total: 0,
                tracked_open_total: 0,
                live_untracked: vec![],
                tracked_dead: vec![],
            }),
            now_ms,
        )
    }

    /// **The D3 regression test — the one that matters most.**
    ///
    /// With terminal-hosted agent sessions live the verdict is `false`, and
    /// NOTHING in the response recommends a drain: `drain_covers_these` is
    /// `false`, `would_be_noop` is `true`, the reason never mentions a drain,
    /// and the field `drain_required` does not exist anywhere in the payload.
    ///
    /// Shipping this without the assertion would have produced a *worse*
    /// system than the status quo — an operator who drains, sees
    /// `drained_sessions: 0`, and restarts believing the work was captured.
    #[test]
    fn terminal_sessions_live_is_unsafe_and_never_recommends_a_drain() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shell 5 → claude 10 (tracked "t-5"), shell 6 → claude 11
        // (tracked "t-6"). Two live terminal-hosted agent sessions.
        let snap = snap_with(
            &[(1, &[5, 6]), (5, &[10]), (6, &[11])],
            &[(5, now_s), (6, now_s), (10, now_s), (11, now_s)],
            &[
                (5, "powershell.exe"),
                (6, "powershell.exe"),
                (10, "claude.exe"),
                (11, "claude.exe"),
            ],
        );
        let opened_at = now_ms - 2_231_000;
        let records = vec![
            record("sess-a", "t-5", opened_at),
            record("sess-b", "t-6", now_ms - 60_000),
        ];
        let terminal_pids: HashMap<String, u32> =
            [("t-5".to_string(), 5u32), ("t-6".to_string(), 6u32)]
                .into_iter()
                .collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &HashSet::new(),
            now_ms,
            now_ms,
        );
        assert_eq!(report.live_claude_total, 2);

        let terminal = terminal_plane_from(&report, &records, now_ms);
        // The AI plane is genuinely empty — exactly the incident's shape.
        let ai = ai_plane_from(&[], now_ms);

        let v = build_verdict(
            Some(terminal),
            Some(ai),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );

        assert!(!v.safe_to_restart, "verdict must be unsafe: {v:?}");

        let t = v.terminal_sessions.as_ref().unwrap();
        assert_eq!(t.count, 2);
        assert_eq!(t.sessions.len(), 2);
        assert!(
            !t.drain_covers_these,
            "D3: a drain NEVER covers the terminal plane"
        );
        assert_eq!(t.oldest_session_age_s, Some(2231));

        let a = v.ai_sessions.as_ref().unwrap();
        assert_eq!(a.count, 0);
        assert!(a.drain_covers_these);

        assert!(
            v.drain.would_be_noop,
            "a drain with 0 AI sessions is a no-op"
        );
        assert_eq!(v.drain.covers, "ai_sessions only");

        // The reason must say what is lost, and must NOT point at the drain.
        assert!(
            !v.reason.to_lowercase().contains("drain"),
            "D3: the reason must not recommend a drain: {}",
            v.reason
        );
        assert!(v.reason.contains("no graceful stop path"), "{}", v.reason);
        assert!(v.reason.contains("will be lost"), "{}", v.reason);

        // And the deleted field must not have crept back in.
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains("drain_required"),
            "D3: `drain_required` is deleted, not renamed: {json}"
        );
        assert!(json.contains("\"boundary\""));
    }

    /// A genuinely idle runner: both planes empty → `safe_to_restart: true`.
    #[test]
    fn idle_runner_is_safe() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → a bare shell, no claude anywhere.
        let snap = snap_with(&[(1, &[5])], &[(5, now_s)], &[(5, "powershell.exe")]);

        let report = evaluate(
            &snap,
            1,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            now_ms,
            now_ms,
        );
        assert_eq!(report.live_claude_total, 0);

        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], now_ms)),
            Some(ai_plane_from(&[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );

        assert!(v.safe_to_restart, "{v:?}");
        assert_eq!(v.reason, "no live agent sessions in either plane");
        assert_eq!(v.terminal_sessions.as_ref().unwrap().count, 0);
        assert_eq!(
            v.terminal_sessions.as_ref().unwrap().oldest_session_age_s,
            None
        );
        assert_eq!(v.ai_sessions.as_ref().unwrap().count, 0);
    }

    /// Fail-closed: an unreadable process table (empty `parent_map`) is the
    /// existing fail-OPEN skip for the periodic task and must be fail-CLOSED
    /// here, with the cause named and the plane serialized `null` — never `0`.
    #[test]
    fn empty_process_snapshot_is_unsafe_with_the_cause_named() {
        let now_ms = chrono::Utc::now().timestamp_millis();

        // `compute` returns None on an empty parent_map; the handler turns that
        // into this unknown. Assert the composition end of that contract.
        let cause = "the terminal-session plane could not be determined: the process table is \
                     unreadable (snapshot_process_table_public returned an empty parent_map), so \
                     live `claude` processes cannot be enumerated";
        let v = build_verdict(
            None,
            Some(ai_plane_from(&[], now_ms)),
            vec![cause.to_string()],
            idle_drain(),
            fresh_census(now_ms),
        );

        assert!(!v.safe_to_restart, "an unknown must never read as safe");
        assert!(v.reason.starts_with("UNKNOWN, so treated as unsafe:"));
        assert!(
            v.reason.contains("process table is unreadable"),
            "{}",
            v.reason
        );
        assert!(v.terminal_sessions.is_none());

        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["terminal_sessions"],
            serde_json::Value::Null,
            "an undetermined plane is null, never 0"
        );
        assert_eq!(json["safe_to_restart"], serde_json::Value::Bool(false));
    }

    /// A missing plane with no explicit cause still fails closed and still
    /// names which plane went unknown (no silent `true`).
    #[test]
    fn missing_plane_without_a_stated_cause_still_fails_closed() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            live_untracked: vec![],
            tracked_dead: vec![],
        };
        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], now_ms)),
            None,
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );
        assert!(!v.safe_to_restart);
        assert!(v.reason.contains("AI/task-run plane"), "{}", v.reason);
        assert!(v.ai_sessions.is_none());
    }

    /// A stalled background census reports `periodic_task_healthy: false` —
    /// and the verdict is UNAFFECTED by it, because the verdict comes from
    /// this endpoint's own fresh pass (D5).
    #[test]
    fn stale_background_census_flags_the_task_but_not_the_verdict() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let interval_s = tracking_health::CHECK_INTERVAL.as_secs() as i64;

        // Stale: 3x the interval old (> the 2x threshold).
        let stale = TrackingHealthReport {
            checked_at_ms: now_ms - interval_s * 3 * 1000,
            // Deliberately DISAGREES with the fresh pass below: the cache says
            // 7 sessions were live 30 minutes ago. If the verdict ever read the
            // cache, this test would fail.
            live_claude_total: 7,
            tracked_open_total: 7,
            live_untracked: vec![LiveUntrackedProcess {
                pid: 42,
                image: Some("claude.exe".to_string()),
            }],
            tracked_dead: vec![TrackedDeadRecord {
                claude_session_id: "ghost".to_string(),
                terminal_id: "t-ghost".to_string(),
                title: None,
            }],
        };
        let census = census_info(Some(&stale), now_ms);
        assert_eq!(census.periodic_task_healthy, Some(false));
        assert_eq!(census.background_age_s, Some(interval_s * 3));
        assert_eq!(census.check_interval_s, interval_s as u64);

        // Fresh pass: genuinely idle.
        let fresh = TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            live_untracked: vec![],
            tracked_dead: vec![],
        };
        let v = build_verdict(
            Some(terminal_plane_from(&fresh, &[], now_ms)),
            Some(ai_plane_from(&[], now_ms)),
            vec![],
            idle_drain(),
            census,
        );

        assert!(
            v.safe_to_restart,
            "a stalled BACKGROUND task must not flip the verdict (D5): {v:?}"
        );
        assert_eq!(v.census.periodic_task_healthy, Some(false));
        assert_eq!(v.terminal_sessions.as_ref().unwrap().count, 0);
    }

    /// Before the first background pass the census age is UNKNOWN — `null`,
    /// never "healthy" and never `0`.
    #[test]
    fn census_before_first_background_pass_is_unknown_not_healthy() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let c = census_info(None, now_ms);
        assert_eq!(c.background_last_check_at, None);
        assert_eq!(c.background_age_s, None);
        assert_eq!(c.periodic_task_healthy, None);

        let json = serde_json::to_value(c).unwrap();
        assert_eq!(json["periodic_task_healthy"], serde_json::Value::Null);
    }

    /// Live AI sessions: the plane is reported honestly (count, worktree
    /// eligibility, `age_s` null where the `task_runs` join missed), the
    /// verdict is unsafe, and `would_be_noop` is false.
    #[test]
    fn ai_plane_reports_wip_eligibility_and_null_age_on_a_missed_join() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let inputs = vec![
            AiSessionInput {
                id: "run-1".to_string(),
                state: "processing".to_string(),
                has_worktree: true,
                created_at_ms: Some(now_ms - 300_000),
            },
            AiSessionInput {
                id: "run-2".to_string(),
                state: "ready".to_string(),
                has_worktree: false,
                // Join missed — honestly unknown, NOT 0 and NOT `now`.
                created_at_ms: None,
            },
        ];
        let ai = ai_plane_from(&inputs, now_ms);
        assert_eq!(ai.count, 2);
        assert_eq!(ai.wip_capture_eligible, 1);
        assert_eq!(ai.sessions[0].age_s, Some(300));
        assert_eq!(ai.sessions[1].age_s, None);
        assert_eq!(ai.oldest_session_age_s, Some(300));

        let report = TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            live_untracked: vec![],
            tracked_dead: vec![],
        };
        let drain = DrainInfo {
            would_be_noop: false,
            ..idle_drain()
        };
        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], now_ms)),
            Some(ai),
            vec![],
            drain,
            fresh_census(now_ms),
        );
        assert!(!v.safe_to_restart);
        assert!(
            v.reason.contains("2 AI/task-run sessions are live"),
            "{}",
            v.reason
        );
        assert!(!v.drain.would_be_noop);

        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["ai_sessions"]["sessions"][1]["age_s"],
            serde_json::Value::Null
        );
    }

    /// A tracked-dead record is excluded from the live session list, while a
    /// live-but-untracked `claude` still counts toward `count` (it has no
    /// record to list) and is named in the reason.
    #[test]
    fn tracked_dead_excluded_and_live_untracked_counted() {
        let now_s = chrono::Utc::now().timestamp();
        let now_ms = now_s * 1000;
        // Runner (1) → shell 5 → claude 10 (tracked "t-5"); stray claude 20 with
        // no record; record "sess-ghost" points at a terminal that is gone.
        let snap = snap_with(
            &[(1, &[5, 20]), (5, &[10])],
            &[(5, now_s), (10, now_s), (20, now_s)],
            &[
                (5, "powershell.exe"),
                (10, "claude.exe"),
                (20, "claude.exe"),
            ],
        );
        let records = vec![
            record("sess-live", "t-5", now_ms - 10_000),
            record("sess-ghost", "t-gone", now_ms - 10_000),
        ];
        let terminal_pids: HashMap<String, u32> = [("t-5".to_string(), 5u32)].into_iter().collect();

        let report = evaluate(
            &snap,
            1,
            &records,
            &terminal_pids,
            &HashSet::new(),
            now_ms,
            now_ms,
        );
        let t = terminal_plane_from(&report, &records, now_ms);

        assert_eq!(t.count, 2, "live_claude_total counts the untracked stray");
        assert_eq!(t.tracked_open_total, 2);
        assert_eq!(t.live_untracked_count, 1);
        assert_eq!(t.tracked_dead_count, 1);
        let ids: Vec<&str> = t.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["sess-live"],
            "the dead record is not a live session"
        );

        let v = build_verdict(
            Some(t),
            Some(ai_plane_from(&[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );
        assert!(!v.safe_to_restart);
        assert!(
            v.reason.contains("have no durable lifecycle record"),
            "{}",
            v.reason
        );
    }

    /// A record stamped in the future (clock skew) yields `age_s: null`, never
    /// a negative or fabricated age.
    #[test]
    fn future_or_missing_timestamps_yield_null_age() {
        let now_ms = 1_000_000_000i64;
        assert_eq!(age_s_from(0, now_ms), None);
        assert_eq!(age_s_from(-5, now_ms), None);
        assert_eq!(age_s_from(now_ms + 60_000, now_ms), None);
        assert_eq!(age_s_from(now_ms - 5_000, now_ms), Some(5));
    }

    /// The boundary statement ships verbatim on every response.
    #[test]
    fn boundary_is_emitted_verbatim() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let report = TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            live_untracked: vec![],
            tracked_dead: vec![],
        };
        let v = build_verdict(
            Some(terminal_plane_from(&report, &[], now_ms)),
            Some(ai_plane_from(&[], now_ms)),
            vec![],
            idle_drain(),
            fresh_census(now_ms),
        );
        assert_eq!(
            v.boundary,
            "counts `claude` processes in this runner's inclusive process subtree; a session doing non-`claude` work, or a child that escaped the subtree, is not represented"
        );
    }

    #[test]
    fn rfc3339_parsing() {
        assert_eq!(rfc3339_to_millis("1970-01-01T00:00:01Z"), Some(1000),);
        assert_eq!(rfc3339_to_millis("not-a-timestamp"), None);
    }
}
