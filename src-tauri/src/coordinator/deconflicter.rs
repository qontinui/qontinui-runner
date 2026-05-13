//! Rust deconflicter loop — Phase 1 of the "Coord as Deconflicter, not
//! Dispatcher" plan (§4.1 of plans/2026-05-13-coord-as-deconflicter-plan.md).
//!
//! Listens on a `tokio::sync::broadcast::Sender<TouchEvent>` that
//! `claude_session::dispatcher::auto_register_file` fires every time it
//! UPSERTs a row into `coord.session_touched_files`. For each touch we
//! query the table for any *other* recent toucher of the same path whose
//! task is still in flight; if a non-empty set comes back we emit an
//! advisory row into `coord.coordinator_decisions` (rule=`deconflict`,
//! action=`advise-with-text`, session_id=`deconflicter-rust`) and fire a
//! `coordinator-decision-created` Tauri event so the in-session banner
//! can pick it up.
//!
//! The deconflicter never assigns or merges — its allow-list is hard-
//! capped at `advise-with-text` by `must_advise_only` (which gates on the
//! `deconflicter-` session-id prefix; see `act.rs:68-73`). The HTTP layer
//! 403s any deconflicter session that tries a richer action.
//!
//! ## Rate limiting (plan §4.1)
//!
//! In-memory `HashMap<(task_run_id, file_path), Instant>` suppresses
//! duplicate advisories for the same `(session, path)` within 5 minutes.
//! The map is bounded at `DEDUP_MAX_ENTRIES`; when the cap is exceeded we
//! sweep entries older than the dedup window so the long-tail of
//! deduped pairs doesn't grow unbounded (the touch-trail itself is
//! bounded by `clear_files_touched` at commit time).
//!
//! ## Multi-runner safety (plan §5.2)
//!
//! No lease for Phase 1. Each runner observes its own touch broadcast;
//! advisories are scoped to the touches the runner saw. Defer the
//! canonical-PG lease to Phase 2 when multi-runner becomes real.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

use crate::database::pg::coordinator_decisions::InsertCoordinatorDecisionInput;
use crate::database::pg::PgDb;

/// One touch event broadcast from `claude_session::dispatcher::auto_register_file`
/// after the PG `record_file_touched` insert kicks off. The send is
/// best-effort (drop-on-no-receiver is fine); the channel capacity is
/// generous enough to absorb burst writes during decompose-and-spawn.
#[derive(Debug, Clone)]
pub struct TouchEvent {
    /// `task_run_id` column from `coord.session_touched_files` — for
    /// terminal-launched AI sessions this is also the Claude session id;
    /// for workers it is the worker's task_run_id (which doubles as the
    /// session id in `SessionManager`).
    pub task_run_id: String,
    /// `file_path` column from the same row.
    pub file_path: String,
}

/// Reserved session-id prefix for the Rust deconflicter. Any decision row
/// whose `session_id` matches this is restricted to `advise-with-text`
/// only by `act::must_advise_only`. UUID v4 strings never start with
/// `deconflicter-`, so the prefix is effectively a reserved namespace.
pub const DECONFLICTER_SESSION_ID: &str = "deconflicter-rust";

/// How far back to look for "other recent toucher" rows. Tunable seed
/// value per plan §3.3 — promote to a `coord.deconflicter_settings` row
/// later if we need per-runner adjustments.
const RECENT_TOUCH_WINDOW_MINUTES: i64 = 15;

/// Dedup window per `(session, path)`. Within this window we suppress
/// duplicate advisories so a session that re-edits the same path doesn't
/// re-fire the banner.
const DEDUP_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Bound on the dedup map. When exceeded we sweep entries older than
/// `DEDUP_WINDOW`. Sized for the upper bound of an active multi-session
/// session's touch fan-out within the window.
const DEDUP_MAX_ENTRIES: usize = 4096;

/// Cap on the number of conflicting sessions surfaced per advisory.
/// Mirrors the `LIMIT 5` in the SQL query — keeps the rendered text
/// terse even when many sessions have recently touched the path.
const MAX_CONFLICTS_PER_ADVISORY: usize = 5;

/// One row of the "who else recently touched this file" query result.
/// `task_id` is the `coord.tasks.id` cast to text; `other_session_id`
/// is the foreign session's `task_run_id` (`coord.tasks.assigned_session_id`);
/// `recorded_at` is the timestamp from the touch row.
struct ConflictRow {
    /// `coord.tasks.id::text` — currently unused at the rendering layer
    /// but kept for the eventual "click to jump to the conflicting
    /// session's tab" affordance. Plus, having it in scope here makes the
    /// SELECT shape match the SQL in plan §3.3 verbatim.
    #[allow(dead_code)]
    task_id: String,
    other_session_id: String,
    recorded_at: chrono::DateTime<chrono::Utc>,
}

/// Loop owner. The fields are private; construct via `start`.
pub struct DeconflicterLoop {
    pg: Arc<PgDb>,
    app: AppHandle,
    rx: broadcast::Receiver<TouchEvent>,
    dedup: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl DeconflicterLoop {
    /// Spawn the deconflicter loop on the tokio runtime. The returned
    /// `JoinHandle` is typically left to detach — the loop runs until the
    /// broadcast sender is dropped (i.e. process exit).
    pub fn start(
        pg: Arc<PgDb>,
        app: AppHandle,
        rx: broadcast::Receiver<TouchEvent>,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let loop_ = Self {
            pg,
            app,
            rx,
            dedup: Arc::new(Mutex::new(HashMap::with_capacity(64))),
        };
        // Use tauri::async_runtime::spawn (not tokio::spawn) so the call works
        // inside Tauri's `setup` closure, which executes before a tokio runtime
        // has been entered on the calling thread. Mirrors the pattern in
        // `start_session_snapshot_pruner` / `start_dreamer_scheduler`.
        tauri::async_runtime::spawn(loop_.run())
    }

    async fn run(mut self) {
        info!(
            "deconflicter: loop started (session_id={})",
            DECONFLICTER_SESSION_ID
        );
        loop {
            match self.rx.recv().await {
                Ok(t) => {
                    if let Err(e) = self.handle(&t).await {
                        warn!(
                            "deconflicter: handle({}, {}) failed: {}",
                            t.task_run_id, t.file_path, e
                        );
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("deconflicter: channel closed, exiting");
                    return;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "deconflicter: lagged {} touches (channel capacity exceeded)",
                        n
                    );
                    continue;
                }
            }
        }
    }

    async fn handle(&self, t: &TouchEvent) -> Result<(), String> {
        // Dedup window: suppress duplicate (session, path) advisories
        // for `DEDUP_WINDOW`. The same session re-editing the same path
        // is the dominant duplicate-advisory case.
        if !self.dedup_allow(&t.task_run_id, &t.file_path).await {
            return Ok(());
        }

        let conflicts = self.find_overlapping(t).await?;
        if conflicts.is_empty() {
            return Ok(());
        }

        let text = render_advisory(&t.file_path, &conflicts);
        self.emit_decision(t, &text).await?;
        Ok(())
    }

    /// Returns true if this `(session, path)` pair should be allowed to
    /// fire an advisory, false if we just fired one within
    /// `DEDUP_WINDOW`. Updates the map on the "allow" branch so the next
    /// call within the window is suppressed.
    async fn dedup_allow(&self, session_id: &str, path: &str) -> bool {
        let key = (session_id.to_string(), path.to_string());
        let now = Instant::now();
        let mut map = self.dedup.lock().await;

        if let Some(prev) = map.get(&key) {
            if now.duration_since(*prev) < DEDUP_WINDOW {
                return false;
            }
        }

        // If the map is over the cap, sweep stale entries. We don't
        // implement strict LRU (which would need a linked-list /
        // doubly-keyed structure); instead, since entries auto-expire at
        // `DEDUP_WINDOW`, a simple "drop everything older than the
        // window" sweep keeps the map size bounded under realistic load.
        // Worst case (all entries fresh) the map grows past the cap
        // briefly — that is fine; the cap is an order-of-magnitude
        // guardrail, not a hard limit.
        if map.len() >= DEDUP_MAX_ENTRIES {
            map.retain(|_, prev| now.duration_since(*prev) < DEDUP_WINDOW);
        }

        map.insert(key, now);
        true
    }

    /// Run the plan §3.3 SQL: find other live sessions that touched
    /// `t.file_path` within `RECENT_TOUCH_WINDOW_MINUTES`, excluding the
    /// touching session itself. Returns at most `MAX_CONFLICTS_PER_ADVISORY`
    /// rows ordered by recency.
    async fn find_overlapping(&self, t: &TouchEvent) -> Result<Vec<ConflictRow>, String> {
        let conn = self
            .pg
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // SQL is the §3.3 post-vet shape:
        //   - `coord.tasks` has no `task_run_id` column — sessions
        //     attach via `assigned_session_id` (text).
        //   - Recency column on `coord.session_touched_files` is
        //     `recorded_at`, not `last_touched_at`.
        //   - Emergent-task statuses (`in_progress`, `assigned`,
        //     `running`) are the "still in flight" set.
        //   - LIMIT mirrors `MAX_CONFLICTS_PER_ADVISORY`.
        let rows = conn
            .query(
                r#"
                SELECT DISTINCT
                    t.id::text          AS task_id,
                    t.assigned_session_id AS other_session_id,
                    stf.recorded_at     AS recorded_at
                FROM coord.session_touched_files stf
                JOIN coord.tasks t
                  ON t.assigned_session_id = stf.task_run_id
                WHERE stf.file_path = $1
                  AND stf.task_run_id != $2
                  AND stf.recorded_at > NOW() - make_interval(mins => $3::double precision)
                  AND t.status IN ('in_progress', 'assigned', 'running')
                ORDER BY recorded_at DESC
                LIMIT $4
                "#,
                &[
                    &t.file_path,
                    &t.task_run_id,
                    &(RECENT_TOUCH_WINDOW_MINUTES as f64),
                    &(MAX_CONFLICTS_PER_ADVISORY as i64),
                ],
            )
            .await
            .map_err(|e| format!("deconflicter: find_overlapping query failed: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ConflictRow {
                task_id: r.get::<_, String>(0),
                // `assigned_session_id` is `TEXT` nullable; the JOIN
                // already filters NULL out, but be defensive in the row
                // mapper so a NULL would surface as an empty string
                // rather than a panic.
                other_session_id: r.get::<_, Option<String>>(1).unwrap_or_default(),
                recorded_at: r.get::<_, chrono::DateTime<chrono::Utc>>(2),
            })
            .collect())
    }

    /// Persist the advisory and emit a `coordinator-decision-created`
    /// Tauri event so the in-session banner can render it without
    /// polling.
    async fn emit_decision(&self, t: &TouchEvent, text: &str) -> Result<(), String> {
        let row = self
            .pg
            .insert_coordinator_decision(&InsertCoordinatorDecisionInput {
                session_id: DECONFLICTER_SESSION_ID,
                iteration: 0,
                rule: "deconflict",
                action: "advise-with-text",
                // `target_id` is the touching session's task_run_id —
                // that is also its emergent task's `assigned_session_id`,
                // which is what the in-session banner filters on
                // (`target_id === current taskRunId`).
                target_id: Some(t.task_run_id.as_str()),
                reasoning: text,
                auto_acted: true,
                observation_hash: "",
            })
            .await?;

        let payload = serde_json::json!({
            "decision_id": row.id,
            "session_id": row.session_id,
            "rule": row.rule,
            "action": row.action,
            "target_id": row.target_id,
            "reasoning": row.reasoning,
        });
        if let Err(e) = self.app.emit("coordinator-decision-created", &payload) {
            warn!(
                "deconflicter: emit coordinator-decision-created failed: {}",
                e
            );
        }
        Ok(())
    }
}

/// Format a relative-time string in the style of `Nm ago` / `Ns ago`.
/// Stable, no allocations beyond the returned `String`. Sub-1s deltas
/// (the same-tick re-touch corner case) render as `"just now"`.
fn format_relative(ts: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = (now - ts).num_seconds();
    if delta < 1 {
        return "just now".to_string();
    }
    if delta < 60 {
        return format!("{}s ago", delta);
    }
    let minutes = delta / 60;
    format!("{}m ago", minutes)
}

/// Render the advisory text per plan §3.3. Pluralizes for >1 conflicting
/// session. Pure / no IO so the deconflicter loop's only side-effect
/// surface stays in `emit_decision`.
fn render_advisory(file_path: &str, conflicts: &[ConflictRow]) -> String {
    debug_assert!(
        !conflicts.is_empty(),
        "render_advisory should only be called with at least one conflict"
    );

    if conflicts.len() == 1 {
        let c = &conflicts[0];
        return format!(
            "Session {} touched {} {} and is still in progress. Consider checking with that session before continuing — they may have an uncommitted change you'd overwrite.",
            c.other_session_id,
            file_path,
            format_relative(c.recorded_at),
        );
    }

    // Plural form: lead with the oldest-most-recent toucher, then list
    // counterpart count. The SQL already orders by `recorded_at DESC` so
    // `conflicts[0]` is the freshest.
    let primary = &conflicts[0];
    let others = conflicts.len() - 1;
    let others_label = if others == 1 {
        "1 other session".to_string()
    } else {
        format!("{} other sessions", others)
    };
    format!(
        "Session {} touched {} {} and is still in progress (plus {}). Consider checking with those sessions before continuing — they may have uncommitted changes you'd overwrite.",
        primary.other_session_id,
        file_path,
        format_relative(primary.recorded_at),
        others_label,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(session: &str, secs_ago: i64) -> ConflictRow {
        ConflictRow {
            task_id: "task-fixture".to_string(),
            other_session_id: session.to_string(),
            recorded_at: chrono::Utc::now() - chrono::Duration::seconds(secs_ago),
        }
    }

    #[test]
    fn render_advisory_singular_form_names_one_session() {
        let text = render_advisory("src/foo.rs", &[conflict("worker-1", 240)]);
        assert!(
            text.contains("worker-1"),
            "must name the conflicting session: {}",
            text
        );
        assert!(text.contains("src/foo.rs"), "must name the path: {}", text);
        assert!(
            text.contains("4m ago"),
            "must format relative time: {}",
            text
        );
        assert!(
            !text.contains("other sessions"),
            "singular form must not mention plurals: {}",
            text
        );
    }

    #[test]
    fn render_advisory_plural_form_counts_others() {
        let text = render_advisory(
            "src/foo.rs",
            &[
                conflict("worker-3", 30),
                conflict("worker-2", 90),
                conflict("worker-1", 240),
            ],
        );
        assert!(
            text.contains("worker-3"),
            "must lead with freshest toucher: {}",
            text
        );
        assert!(
            text.contains("2 other sessions"),
            "must enumerate other sessions: {}",
            text
        );
    }

    #[test]
    fn render_advisory_plural_form_singular_others() {
        let text = render_advisory(
            "src/foo.rs",
            &[conflict("worker-2", 30), conflict("worker-1", 240)],
        );
        assert!(
            text.contains("worker-2"),
            "must lead with freshest toucher: {}",
            text
        );
        assert!(
            text.contains("1 other session"),
            "single-other label must not pluralize: {}",
            text
        );
    }

    #[test]
    fn format_relative_handles_subsecond_and_minutes() {
        // We can't test sub-1s reliably without freezing the clock, but
        // we CAN test the minute branch by passing a known-past instant.
        let past = chrono::Utc::now() - chrono::Duration::seconds(125);
        assert_eq!(format_relative(past), "2m ago");

        let seconds_only = chrono::Utc::now() - chrono::Duration::seconds(7);
        assert_eq!(format_relative(seconds_only), "7s ago");
    }

    #[test]
    fn deconflicter_session_id_prefix_reserved() {
        // The `must_advise_only` gate keys on the `deconflicter-` prefix
        // (see act.rs). Defend the prefix here so a future rename of
        // `DECONFLICTER_SESSION_ID` doesn't silently break the gate.
        assert!(DECONFLICTER_SESSION_ID.starts_with("deconflicter-"));
    }
}
