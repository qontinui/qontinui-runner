//! `GET /apps/{app_id}/journey/health` — is the journey ledger actually being
//! written?
//!
//! The co-occurrence precedent only `warn!`s when capture fails, so a pipeline
//! that stopped writing looks identical to one with nothing to write. Here
//! every outcome is counted and the state is DERIVED from the counts:
//!
//! - `schema_absent` — the probe found a journey table missing;
//! - `write_failing` — a write (edge, frontier, or an event the queue could
//!   not hold) failed within the last [`RECENT_WINDOW`] attempts, or the probe
//!   itself failed;
//! - `writing` — the probe found both tables and the recent window is clean.
//!
//! **Never `writing` on no evidence.** Until the probe has completed the state
//! is `write_failing` with a `detail` beginning `unknown:` — the shared
//! `LedgerState` vocabulary is closed, so "not yet known" is said in `detail`
//! and in [`JourneyHealthResponse::schema_probe`], never by reading `writing`.
//! The handler runs the probe itself when it has not run yet, so this arm is
//! reached only when the database cannot be asked at all.
//!
//! The counters are process-wide (the ledger lives in one database); the
//! `appId` path segment scopes the route like its `/apps/{app_id}/spec/*`
//! siblings and is echoed back.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::Json;
use qontinui_types::journey::{JourneyLedgerHealth, LedgerState};
use serde::Serialize;

use crate::mcp::types::ApiState;

use super::capture::{journey_schema_state, journey_schema_supported, SchemaProbe};

/// How many recent write attempts the failure count looks back over.
pub(crate) const RECENT_WINDOW: usize = 20;

/// The `schema_absent` detail on an embedded database — plan Phase 0
/// decision 4: the runner applies its vendored schema only to a FRESH
/// embedded database, so an existing one never receives new tables.
pub(crate) const EMBEDDED_SCHEMA_ABSENT_DETAIL: &str = "embedded DB predates the journey tables — they are created only on a fresh database; see plan Phase 0 decision 4";

static EDGES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static FRONTIER_ROWS_CLEARED: AtomicU64 = AtomicU64::new(0);
static FRONTIER_UPSERTS: AtomicU64 = AtomicU64::new(0);
static WRITES_NOT_ATTEMPTED: AtomicU64 = AtomicU64::new(0);
static WRITE_FAILURES_TOTAL: AtomicU64 = AtomicU64::new(0);
static PENDING_OPEN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default)]
struct Recent {
    /// `true` = success, newest at the back, at most [`RECENT_WINDOW`].
    outcomes: VecDeque<bool>,
    last_error: Option<String>,
    last_write_at: Option<String>,
    last_prune_at: Option<String>,
    last_prune_deleted: Option<u64>,
    last_prune_skipped: Option<String>,
}

fn recent() -> &'static Mutex<Recent> {
    static RECENT: std::sync::OnceLock<Mutex<Recent>> = std::sync::OnceLock::new();
    RECENT.get_or_init(|| Mutex::new(Recent::default()))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn push_outcome(ok: bool, error: Option<String>) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.outcomes.push_back(ok);
    while r.outcomes.len() > RECENT_WINDOW {
        r.outcomes.pop_front();
    }
    if ok {
        r.last_write_at = Some(now_iso());
    }
    if let Some(e) = error {
        r.last_error = Some(e);
    }
}

/// An edge row landed; `frontier_cleared` rows left the frontier with it.
pub(crate) fn record_edge_written(frontier_cleared: u64) {
    EDGES_WRITTEN.fetch_add(1, Ordering::Relaxed);
    FRONTIER_ROWS_CLEARED.fetch_add(frontier_cleared, Ordering::Relaxed);
    push_outcome(true, None);
}

/// A frontier upsert statement landed, touching `rows` rows.
pub(crate) fn record_frontier_upserted(rows: u64) {
    FRONTIER_UPSERTS.fetch_add(rows, Ordering::Relaxed);
    push_outcome(true, None);
}

/// A write failed: a contract validation refusal, a refused statement, a pool
/// error, or an event the queue could not hold. Never a silent drop.
pub(crate) fn record_write_failure(error: String) {
    tracing::warn!("journey ledger write failed: {}", error);
    WRITE_FAILURES_TOTAL.fetch_add(1, Ordering::Relaxed);
    push_outcome(false, Some(error));
}

/// A write was not attempted because the probe said the schema cannot take
/// it. Counted separately: the state already says why.
pub(crate) fn record_not_written() {
    WRITES_NOT_ATTEMPTED.fetch_add(1, Ordering::Relaxed);
}

/// The worker's count of open pending edges.
pub(crate) fn set_pending_open(n: u64) {
    PENDING_OPEN.store(n, Ordering::Relaxed);
}

/// One retention pass finished.
pub(crate) fn record_prune(deleted: u64) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.last_prune_at = Some(now_iso());
    r.last_prune_deleted = Some(deleted);
    r.last_prune_skipped = None;
}

/// A retention pass did not run (schema absent, probe failed, pool error) or
/// failed part-way; `deleted` is what it removed before stopping.
pub(crate) fn record_prune_skipped(reason: String, deleted: u64) {
    let mut r = recent().lock().unwrap_or_else(|p| p.into_inner());
    r.last_prune_at = Some(now_iso());
    r.last_prune_deleted = Some(deleted);
    r.last_prune_skipped = Some(reason);
}

/// The probe's answer, as the wire string of
/// [`JourneyHealthResponse::schema_probe`].
fn probe_str(probe: Option<&SchemaProbe>) -> &'static str {
    match probe {
        None => "unknown",
        Some(SchemaProbe::Present) => "present",
        Some(SchemaProbe::Absent { .. }) => "absent",
        Some(SchemaProbe::Failed { .. }) => "failed",
    }
}

/// Derive the ledger state. Pure, so every arm is tested.
///
/// `embedded` selects the `schema_absent` detail: on the runner's embedded
/// database the cause is Phase 0 decision 4; on an external one it is simply a
/// missing table.
pub(crate) fn derive_ledger_health(
    probe: Option<&SchemaProbe>,
    recent_failures: usize,
    last_error: Option<&str>,
    edges_written: u64,
    embedded: bool,
) -> JourneyLedgerHealth {
    match probe {
        None => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: "unknown: the journey schema probe has not completed (the database could \
                     not be asked), so whether the ledger is being written is not known"
                .to_string(),
        },
        Some(SchemaProbe::Absent { missing }) => JourneyLedgerHealth {
            state: LedgerState::SchemaAbsent,
            detail: if embedded {
                format!("{EMBEDDED_SCHEMA_ABSENT_DETAIL} (missing: {})", missing.join(", "))
            } else {
                format!(
                    "missing table(s) {} in this database — apply qontinui-web migration \
                     journey_01_edge_ledger, then restart the runner to re-probe",
                    missing.join(", ")
                )
            },
        },
        Some(SchemaProbe::Failed { error }) => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: format!(
                "the journey schema probe failed ({error}); nothing is written for the life of \
                 this process — fix the database, then restart the runner to re-probe"
            ),
        },
        Some(SchemaProbe::Present) if recent_failures > 0 => JourneyLedgerHealth {
            state: LedgerState::WriteFailing,
            detail: format!(
                "{recent_failures} of the last {RECENT_WINDOW} journey writes failed; last error: {}",
                last_error.unwrap_or("<none recorded>")
            ),
        },
        Some(SchemaProbe::Present) => JourneyLedgerHealth {
            state: LedgerState::Writing,
            detail: if edges_written == 0 {
                "journey tables present and readable; no edge written yet by this process"
                    .to_string()
            } else {
                format!("journey tables present; {edges_written} edge(s) written by this process")
            },
        },
    }
}

/// Process-wide counters reported beside the ledger state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JourneyCounters {
    pub edges_written: u64,
    pub pending_edges_open: u64,
    /// Frontier rows inserted or refreshed.
    pub frontier_upserts: u64,
    /// Frontier rows removed because an edge activated them.
    pub frontier_rows_cleared: u64,
    /// Writes skipped because the probe said the schema cannot take them.
    pub writes_not_attempted: u64,
    pub write_failures_total: u64,
    pub recent_write_failures: u64,
    pub recent_window: u64,
    pub last_write_at: Option<String>,
    pub last_error: Option<String>,
    pub last_prune_at: Option<String>,
    pub last_prune_deleted: Option<u64>,
    /// Why the last retention pass did not run (or stopped), when it did not.
    pub last_prune_skipped: Option<String>,
}

/// Body of `GET /apps/{app_id}/journey/health`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JourneyHealthResponse {
    pub app_id: String,
    /// The shared ledger block every journey read repeats.
    pub ledger: JourneyLedgerHealth,
    /// `unknown` until the once-per-process probe completes, then `present`,
    /// `absent` or `failed`.
    pub schema_probe: &'static str,
    pub counters: JourneyCounters,
}

fn is_embedded() -> bool {
    matches!(
        crate::embedded_pg::db_arm(),
        crate::embedded_pg::DbArm::EmbeddedOwned | crate::embedded_pg::DbArm::EmbeddedAttached
    )
}

/// Assemble the health body from the current counters.
pub(crate) fn snapshot_health(app_id: String) -> JourneyHealthResponse {
    let probe = journey_schema_state();
    let (recent_failures, counters) = {
        let r = recent().lock().unwrap_or_else(|p| p.into_inner());
        let failures = r.outcomes.iter().filter(|ok| !**ok).count();
        (
            failures,
            JourneyCounters {
                edges_written: EDGES_WRITTEN.load(Ordering::Relaxed),
                pending_edges_open: PENDING_OPEN.load(Ordering::Relaxed),
                frontier_upserts: FRONTIER_UPSERTS.load(Ordering::Relaxed),
                frontier_rows_cleared: FRONTIER_ROWS_CLEARED.load(Ordering::Relaxed),
                writes_not_attempted: WRITES_NOT_ATTEMPTED.load(Ordering::Relaxed),
                write_failures_total: WRITE_FAILURES_TOTAL.load(Ordering::Relaxed),
                recent_write_failures: failures as u64,
                recent_window: RECENT_WINDOW as u64,
                last_write_at: r.last_write_at.clone(),
                last_error: r.last_error.clone(),
                last_prune_at: r.last_prune_at.clone(),
                last_prune_deleted: r.last_prune_deleted,
                last_prune_skipped: r.last_prune_skipped.clone(),
            },
        )
    };
    let ledger = derive_ledger_health(
        probe,
        recent_failures,
        counters.last_error.as_deref(),
        counters.edges_written,
        is_embedded(),
    );
    JourneyHealthResponse {
        app_id,
        ledger,
        schema_probe: probe_str(probe),
        counters,
    }
}

/// `GET /apps/{app_id}/journey/health`.
///
/// Runs the once-per-process schema probe itself when nothing has run it yet,
/// so a runner that has not seen an agent action still answers `writing` /
/// `schema_absent` from evidence. When the database cannot be asked (no
/// pooled connection within the pool's own timeout), the probe is left
/// un-run — so a later write can still run it — and the answer says
/// `unknown`.
pub async fn get_journey_health(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
) -> Json<JourneyHealthResponse> {
    if journey_schema_state().is_none() {
        if let Ok(conn) = state.app_state.pg_db.pool().get().await {
            journey_schema_supported(&conn).await;
        }
    }
    Json(snapshot_health(app_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_probe_never_reads_writing() {
        let h = derive_ledger_health(None, 0, None, 0, true);
        assert_ne!(h.state, LedgerState::Writing);
        assert!(h.detail.starts_with("unknown"), "{}", h.detail);
        assert_eq!(probe_str(None), "unknown");
    }

    #[test]
    fn schema_absent_on_embedded_names_phase_0_decision_4() {
        let probe = SchemaProbe::Absent {
            missing: vec![
                "project.journey_edge_observations",
                "project.journey_frontier",
            ],
        };
        let h = derive_ledger_health(Some(&probe), 0, None, 0, true);
        assert_eq!(h.state, LedgerState::SchemaAbsent);
        assert!(
            h.detail.starts_with(EMBEDDED_SCHEMA_ABSENT_DETAIL),
            "{}",
            h.detail
        );
        assert!(h.detail.contains("project.journey_frontier"));
    }

    #[test]
    fn schema_absent_on_external_is_the_plain_missing_table() {
        let probe = SchemaProbe::Absent {
            missing: vec!["project.journey_frontier"],
        };
        let h = derive_ledger_health(Some(&probe), 3, Some("x"), 0, false);
        assert_eq!(
            h.state,
            LedgerState::SchemaAbsent,
            "absent outranks failures"
        );
        assert!(h
            .detail
            .starts_with("missing table(s) project.journey_frontier"));
        assert!(!h.detail.contains("Phase 0"));
    }

    #[test]
    fn recent_failures_read_write_failing_with_the_last_error() {
        let h = derive_ledger_health(
            Some(&SchemaProbe::Present),
            2,
            Some("[23514] check_violation"),
            9,
            true,
        );
        assert_eq!(h.state, LedgerState::WriteFailing);
        assert!(h.detail.contains("2 of the last 20"));
        assert!(h.detail.contains("check_violation"));
    }

    #[test]
    fn a_failed_probe_is_write_failing_not_absent() {
        let h = derive_ledger_health(
            Some(&SchemaProbe::Failed {
                error: "permission denied".into(),
            }),
            0,
            None,
            0,
            true,
        );
        assert_eq!(h.state, LedgerState::WriteFailing);
        assert!(h.detail.contains("permission denied"));
    }

    #[test]
    fn present_and_clean_is_writing() {
        let h = derive_ledger_health(Some(&SchemaProbe::Present), 0, None, 0, true);
        assert_eq!(h.state, LedgerState::Writing);
        assert!(h.detail.contains("no edge written yet"));
        let h = derive_ledger_health(Some(&SchemaProbe::Present), 0, None, 4, false);
        assert!(h.detail.contains("4 edge(s)"));
    }

    #[test]
    fn the_response_serializes_camel_case_with_the_ledger_block() {
        let body = serde_json::to_value(snapshot_health("qontinui-web".into())).unwrap();
        assert_eq!(body["appId"], "qontinui-web");
        assert!(body["ledger"]["state"].is_string());
        assert!(body["ledger"]["detail"].is_string());
        assert!(body["schemaProbe"].is_string());
        for field in [
            "edgesWritten",
            "pendingEdgesOpen",
            "frontierUpserts",
            "lastWriteAt",
            "lastPruneAt",
            "lastPruneDeleted",
            "recentWriteFailures",
        ] {
            assert!(
                body["counters"].get(field).is_some(),
                "missing counters.{field}"
            );
        }
    }
}
