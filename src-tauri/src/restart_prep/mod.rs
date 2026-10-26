//! Operator "prepare for restart" (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`).
//!
//! The operator brings the runner to a point where it can be restarted safely:
//! spawns held until the rebuild, live sessions told to save and pause,
//! limit-hit sessions kept on their account, and every session recorded so the
//! rebuilt runner can restore it in place, one at a time.
//!
//! ## What this phase (1) ships: the record and its reads, no side effects
//!
//! * [`effort`] — the effort a session runs at, read off its pane, and the
//!   baseline a row's `held` flag compares against (D5).
//! * [`cohort`] — the set of sessions, built from the ledger's own capture
//!   (D4), with effort, model, state at prepare and `held` per row.
//! * The prepare record ([`PrepareRecord`]): `prepare_id`, `started_at`,
//!   `prepared_build` (this binary's `RUNNER_BUILD_ID`, which the next boot
//!   compares against to tell a rebuild from a same-build restart), the
//!   lifecycle state, the hold status and the cohort. Persisted atomically as
//!   `restart-prep.json` in the runner data dir; a secondary instance writes
//!   under its own instance dir.
//! * Reads only: the Tauri command `restart_prep_status` and
//!   `GET /control/restart-prep`. Both report the persisted record (if any)
//!   and a LIVE cohort preview computed in memory; neither writes anything.
//!
//! ## Phase 3: the hold, the cancel, the build-change release, the freeze
//!
//! [`prepare`] holds and releases spawns: the prepare action, its cancel, the
//! boot step that releases the hold once the runner is rebuilt, the account
//! freeze, and the runner-local hold that gates `drain_gate` where there is
//! no coord lever (D1, D3).
//!
//!
//! ## Phase 4: the notice loop and the verdict
//!
//! [`notice`] tells each live session to save its work and pause — through the
//! session message poller's idle arm, or its busy-prompt arm for a session
//! still mid-turn (D2) — reads every row's state from the restart census each
//! tick, and computes `ready_to_restart`, served in the record here and as the
//! `restart_prep` block of `GET /restart-readiness`.
//!
//! ## Phase 6: the backend restore of the cohort
//!
//! [`restore`] is what a rebuilt runner restores the cohort through: which
//! rows the backend resume core may take while the record is `restoring`, the
//! effort each row is resumed at, the read-back that confirms it, the
//! per-row restore verdicts, and the boot-side split that turns cohort rows
//! into placeholders instead of auto-resuming them. The resume itself is the
//! backend door (`session::resume_door`), reached by the operator through its
//! twin `operator_resume_sessions`.

pub mod cohort;
pub mod effort;
pub mod notice;
pub mod prepare;
pub mod restore;

pub use prepare::{accounts_frozen, freeze_hit, local_hold_reason, on_boot};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use cohort::Cohort;

/// Bumped only on a shape change a reader must branch on. A record whose
/// version is not this one is ABSENT to [`load_from`], never half-parsed.
pub const RECORD_VERSION: u32 = 1;

/// The record's file name, in the instance-scoped runner data dir.
pub const RECORD_FILE: &str = "restart-prep.json";

/// The ledger capture reason a prepare's cohort is taken under.
pub const CAPTURE_REASON: &str = "restart-prep";

/// Where a prepare stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepState {
    /// Hold taken (or attempted), notices going out, sessions pausing.
    Preparing,
    /// Every unfinished row paused or limit-hit, the hold confirmed: restart.
    Ready,
    /// A rebuilt runner is restoring the cohort.
    Restoring,
    /// Every row restored or dismissed.
    Done,
    /// The operator cancelled; the hold is released.
    Cancelled,
}

/// The spawn hold's status, as observed rather than as intended (D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HoldStatus {
    /// No hold has been attempted yet.
    NotAttempted,
    /// Coord's `/me/drain` read back `drained` with this prepare's reason.
    Confirmed,
    /// A hold was attempted and could not be read back; `cause` says why.
    Unconfirmed { cause: String },
    /// A runner with no coord lever (`NotEnrolled`): the runner-local hold.
    Local,
    /// A secondary instance: the runner-local hold over its own spawns only.
    /// A secondary never drains the device, which the primary shares.
    LocalSecondary,
    /// The hold was let go (cancel, or the rebuilt runner's boot). `outcome`
    /// is what the release returned, VERBATIM — coord's `released |
    /// not_owner | absent | unreadable`, or `local` for a runner-local hold,
    /// or why no release could be sent. Never collapsed to "ok".
    ReleaseSent { outcome: String },
}

impl HoldStatus {
    /// The panel's spelling: `confirmed`, `unconfirmed: <cause>`, `local`,
    /// `local (secondary)`.
    pub fn label(&self) -> String {
        match self {
            Self::NotAttempted => "not attempted".to_string(),
            Self::Confirmed => "confirmed".to_string(),
            Self::Unconfirmed { cause } => format!("unconfirmed: {cause}"),
            Self::Local => "local".to_string(),
            Self::LocalSecondary => "local (secondary)".to_string(),
            Self::ReleaseSent { outcome } => format!("release: {outcome}"),
        }
    }

    /// Whether this status is a runner-local hold (no coord lever).
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local | Self::LocalSecondary)
    }
}

/// What let the hold go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTrigger {
    /// The operator cancelled the prepare (or pressed Release hold).
    Cancel,
    /// A runner on a different build booted over the record.
    Rebuild,
}

/// One release of the hold, as it was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseReport {
    pub at_ms: i64,
    pub at: String,
    pub trigger: ReleaseTrigger,
    /// The build that sent the release.
    pub on_build: String,
    /// The release's answer, verbatim (see [`HoldStatus::ReleaseSent`]).
    pub outcome: String,
    /// The device's own drain as read back after the release (`clear`,
    /// `drained (<reason>)`, `unreadable: <cause>`); `None` when no coord
    /// release was sent.
    pub read_back: Option<String>,
}

/// The persisted prepare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareRecord {
    pub record_version: u32,
    pub prepare_id: String,
    /// Unix millis and RFC 3339 of the prepare.
    pub started_at_ms: i64,
    pub started_at: String,
    /// `RUNNER_BUILD_ID` of the binary that prepared. A boot on a different
    /// build is the rebuild the hold was waiting for; the same build is a
    /// restart without one, and the hold stays.
    pub prepared_build: String,
    pub state: PrepState,
    pub hold: HoldStatus,
    pub cohort: Cohort,
    /// The operator's reason, as sent to coord (after the hold's own prefix).
    #[serde(default)]
    pub reason: String,
    /// When the hold lapses by itself: the backstop for a rebuild that never
    /// happens (D1). Coord's own `until` once a coord hold answered, else the
    /// runner's default. A record past it reads "hold expired at …", and
    /// neither the local hold nor the account freeze applies any more.
    #[serde(default)]
    pub hold_until_ms: Option<i64>,
    #[serde(default)]
    pub hold_until: Option<String>,
    /// The release, once the hold was let go.
    #[serde(default)]
    pub release: Option<ReleaseReport>,
    /// Unix millis of the last change to a row that the cohort's re-capture
    /// did not itself produce — a notice delivered, a `limit_hit` marked. The
    /// verdict is ready only once the cohort was re-captured at or after it
    /// (Phase 4, condition 3). `None` reads as `started_at_ms`.
    #[serde(default)]
    pub last_change_at_ms: Option<i64>,
    /// The notice loop's last verdict (Phase 4). `None` before its first tick.
    #[serde(default)]
    pub verdict: Option<notice::PrepVerdict>,
    /// The runner PROCESS that prepared ([`running_process`]), so "has the
    /// runner restarted since the prepare?" is answered by the record itself,
    /// whether or not the boot step has run yet (Phase 6). `None` on a record
    /// written before this field existed.
    #[serde(default)]
    pub prepared_process: Option<String>,
    /// Whether coord ever CONFIRMED a hold for this prepare (its read-back
    /// named the prepare). A release answered `route_absent` settles only
    /// while this is false: a coord without the self-hold route never held
    /// anything for us, but one that confirmed a hold and then answers `404`
    /// to its release has not let it go.
    #[serde(default)]
    pub coord_hold_confirmed: bool,
}

impl PrepareRecord {
    /// A fresh record over `cohort`, prepared by this binary.
    pub fn new(prepare_id: String, now_ms: i64, cohort: Cohort) -> Self {
        Self {
            record_version: RECORD_VERSION,
            prepare_id,
            started_at_ms: now_ms,
            started_at: rfc3339(now_ms),
            prepared_build: running_build().to_string(),
            state: PrepState::Preparing,
            hold: HoldStatus::NotAttempted,
            cohort,
            reason: String::new(),
            hold_until_ms: None,
            hold_until: None,
            release: None,
            last_change_at_ms: None,
            verdict: None,
            prepared_process: Some(running_process().to_string()),
            coord_hold_confirmed: false,
        }
    }

    /// Whether the runner restarted since this record was prepared: another
    /// build, or another process of the same build. A record that predates
    /// [`Self::prepared_process`] answers only by build.
    pub fn restarted_since(&self) -> bool {
        self.prepared_build != running_build()
            || self
                .prepared_process
                .as_deref()
                .is_some_and(|p| p != running_process())
    }

    /// A prepare still in its restart window: preparing or ready. A record
    /// that is restoring, done or cancelled holds nothing.
    pub fn in_window(&self) -> bool {
        matches!(self.state, PrepState::Preparing | PrepState::Ready)
    }

    /// Whether the hold's `until` has passed at `now_ms`. A record with no
    /// `until` has not expired.
    pub fn hold_expired(&self, now_ms: i64) -> bool {
        self.hold_until_ms.is_some_and(|u| now_ms >= u)
    }

    /// In its window and not past its `until`: the prepare is ACTIVE — the
    /// account freeze applies, and a local hold holds.
    pub fn is_active(&self, now_ms: i64) -> bool {
        self.in_window() && !self.hold_expired(now_ms)
    }
}

pub(crate) fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default()
}

/// A per-process identity, minted once: two processes of one build differ.
pub fn running_process() -> &'static str {
    static ID: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());
    &ID
}

/// This binary's build id — the value `/health` serves as `buildId`.
pub fn running_build() -> &'static str {
    env!("RUNNER_BUILD_ID")
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// `~/.qontinui/runner[/instance-<name>]/restart-prep.json`. Instance-scoped
/// like the session ledger: a temp or secondary runner must never read or
/// clobber the primary's record.
pub fn record_path() -> PathBuf {
    crate::instance::scope_path(&qontinui_runner_lib::ambient::runner_dir_or_cwd())
        .join(RECORD_FILE)
}

/// The bytes [`persist_to`] writes for `record`.
pub fn encode(record: &PrepareRecord) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec_pretty(record)
}

/// Write `record` to `path` atomically (temp file + rename), creating the
/// parent directory.
pub fn persist_to(path: &Path, record: &PrepareRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = encode(record).map_err(std::io::Error::other)?;
    crate::fs_atomic::atomic_write(path, &bytes)
}

/// Read the record at `path`. `None` on a missing or unreadable file and on a
/// `record_version` this build does not know — an unreadable record is absent,
/// never half-parsed.
pub fn load_from(path: &Path) -> Option<PrepareRecord> {
    let raw = std::fs::read(path).ok()?;
    match serde_json::from_slice::<PrepareRecord>(&raw) {
        Ok(r) if r.record_version == RECORD_VERSION => Some(r),
        Ok(r) => {
            warn!(
                version = r.record_version,
                path = %path.display(),
                "restart_prep: record version not recognised — treated as absent"
            );
            None
        }
        Err(e) => {
            warn!(error = %e, path = %path.display(), "restart_prep: record unparseable — treated as absent");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// What `restart_prep_status` and `GET /control/restart-prep` answer.
#[derive(Debug, Clone, Serialize)]
pub struct RestartPrepStatus {
    /// The persisted prepare, when one exists. `None` means no readable
    /// record — not proof that no prepare ever ran. Since Phase 4 it carries
    /// each row's `pause_state` and the notice loop's last `verdict`
    /// (`ready_to_restart` and its `blockers`); `GET /restart-readiness`
    /// re-evaluates the same verdict against its own fresh census.
    pub record: Option<PrepareRecord>,
    /// Where the record lives (or would).
    pub record_path: String,
    /// This binary's build id, to compare with `record.prepared_build`.
    pub running_build: String,
    /// The cohort a prepare taken NOW would pin. Computed in memory; nothing
    /// is written.
    pub live_cohort: Cohort,
    /// The observed CLI default effort the `held` baseline falls back to, and
    /// the CLI version it was observed on.
    pub cli_default_effort: Option<String>,
    pub cli_default_observed_on: String,
    /// What the record means NOW: the hold as the panel spells it (including
    /// "hold expired at …"), whether the freeze and the local hold apply, and
    /// the "same build — spawns still held" flag. `None` with no record.
    pub view: Option<prepare::PrepView>,
}

/// The cohort a prepare taken now pins, from the running app. Errs only when
/// the lifecycle store is not managed — a cohort read without it would be an
/// empty list that looks like "no sessions open". Disk work: call it off the
/// async runtime.
pub fn live_cohort_from_app(app: &tauri::AppHandle) -> Result<Cohort, String> {
    use tauri::Manager;

    let store = app
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| {
            "lifecycle store not available — the cohort cannot be read (UNKNOWN, not empty)"
                .to_string()
        })?;
    let terminals = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|t| t.inner().clone());
    let probe = cohort::LiveCohortProbe {
        store: store.clone(),
        terminals,
    };
    let transcripts = crate::session::reconcile::DiskTranscriptIndex::discover();
    let live_cohort = cohort::capture_cohort(
        &store,
        &transcripts,
        &probe,
        CAPTURE_REASON,
        chrono::Utc::now().timestamp_millis(),
        crate::session::shutdown_marker::boot_classification(),
    );
    Ok(live_cohort)
}

/// Assemble the status from the running app. Errs when the cohort cannot be
/// read (see [`live_cohort_from_app`]).
pub fn status_from_app(app: &tauri::AppHandle) -> Result<RestartPrepStatus, String> {
    let live_cohort = live_cohort_from_app(app)?;
    let path = record_path();
    let record = load_from(&path);
    let view = record.as_ref().map(|r| {
        prepare::view_of(
            r,
            prepare::same_build_restart(),
            chrono::Utc::now().timestamp_millis(),
        )
    });
    Ok(RestartPrepStatus {
        record,
        view,
        record_path: path.to_string_lossy().replace('\\', "/"),
        running_build: running_build().to_string(),
        live_cohort,
        cli_default_effort: effort::CLI_DEFAULT_EFFORT.map(str::to_string),
        cli_default_observed_on: effort::CLI_DEFAULT_OBSERVED_ON.to_string(),
    })
}

/// [`status_from_app`] off the async runtime: it reads every open session's
/// transcript tail and the transcript index, which is disk work.
pub async fn status_from_app_blocking(app: tauri::AppHandle) -> Result<RestartPrepStatus, String> {
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || status_from_app(&app))
        .await
        .map_err(|e| format!("restart_prep status task failed: {e}"))?
}

/// Routes contributed by this module; merged in `mcp_api.rs`. A runner-native
/// route, not part of the UI-Bridge SDK manifest.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/control/restart-prep", get(get_restart_prep))
}

/// `GET /control/restart-prep` — the persisted prepare record and the live
/// cohort. Read-only.
async fn get_restart_prep(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<RestartPrepStatus>>, (StatusCode, Json<ApiResponse<()>>)> {
    match status_from_app_blocking(state.app_handle.clone()).await {
        Ok(status) => Ok(Json(ApiResponse::success(status))),
        Err(e) => Err((StatusCode::SERVICE_UNAVAILABLE, Json(api_error(e)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart_prep::cohort::{build_row, CohortRow, RowFacts};
    use crate::session::session_ledger::LedgerEntry;
    use crate::terminal::graceful_exit::ScreenText;

    fn entry(id: &str, zone: i32) -> LedgerEntry {
        LedgerEntry {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "page-a".to_string(),
            zone_index: zone,
            title: Some(format!("tab-{id} — ünïcødé")),
            session_name: Some(format!("name-{id}")),
            account_label: Some("iris".to_string()),
            config_dir: Some("/home/u/.claude-iris".to_string()),
            working_dir: Some("/home/u/repo".to_string()),
            restorable: None,
            worktree_path: Some("/home/u/repo".to_string()),
            plan_slug: Some("2026-10-21-operator-prepare".to_string()),
            work_unit_id: None,
            wip_state: Some("captured".to_string()),
            wip_ref: Some(format!("refs/wip/{id}")),
            custody_session_mismatch: false,
            name_source: None,
            resume_config_dir: None,
            resume_command: None,
            provider: None,
            last_seen_at: None,
            finished: false,
        }
    }

    fn row(id: &str, zone: i32, indicator: &str, finished: bool) -> CohortRow {
        build_row(
            entry(id, zone),
            RowFacts {
                screen: Some(ScreenText {
                    lines: vec![
                        indicator.to_string(),
                        "────".to_string(),
                        "❯\u{a0}".to_string(),
                    ],
                    cursor_row: 2,
                    cursor_col: 2,
                }),
                finished: Some(finished),
                model: Some("claude-opus-5-5".to_string()),
                settings_effort: None,
                transcript_effort: Default::default(),
                provider: Some("claude".to_string()),
            },
        )
    }

    fn record() -> PrepareRecord {
        let mut r = PrepareRecord::new(
            "prep-0001".to_string(),
            1_760_000_000_000,
            Cohort {
                captured_at_ms: 1_760_000_000_000,
                captured_at: "2025-10-09T08:53:20+00:00".to_string(),
                reason: CAPTURE_REASON.to_string(),
                rows: vec![
                    row("a", 0, "◉ max · /effort", false),
                    row("b", 1, "● high · /effort", true),
                    row("c", 2, "no indicator", false),
                ],
            },
        );
        r.cohort.rows[0].notified_at = Some(1_760_000_001_000);
        r.cohort.rows[2].limit_reset_at = Some(1_760_000_900_000);
        r.hold = HoldStatus::Unconfirmed {
            cause: "coord unreachable".to_string(),
        };
        r
    }

    #[test]
    fn persist_and_reload_round_trips_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance-x").join(RECORD_FILE);
        let original = record();
        persist_to(&path, &original).unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        let reloaded = load_from(&path).expect("the record reloads");
        assert_eq!(reloaded, original);
        assert_eq!(
            encode(&reloaded).unwrap(),
            on_disk,
            "re-encoding the reloaded record reproduces the file byte for byte"
        );
        // A second write of the same record leaves the same bytes.
        persist_to(&path, &reloaded).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), on_disk);
    }

    #[test]
    fn an_unknown_version_or_a_torn_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        assert_eq!(load_from(&path), None, "missing");
        let mut future = record();
        future.record_version = RECORD_VERSION + 1;
        persist_to(&path, &future).unwrap();
        assert_eq!(load_from(&path), None, "unknown version");
        std::fs::write(&path, b"{\"record_version\":1,\"prepare_id\":").unwrap();
        assert_eq!(load_from(&path), None, "torn");
    }

    #[test]
    fn a_new_record_names_this_build_and_starts_preparing() {
        let r = PrepareRecord::new(
            "p".to_string(),
            0,
            Cohort {
                captured_at_ms: 0,
                captured_at: String::new(),
                reason: CAPTURE_REASON.to_string(),
                rows: vec![],
            },
        );
        assert_eq!(r.prepared_build, env!("RUNNER_BUILD_ID"));
        assert_eq!(r.state, PrepState::Preparing);
        assert_eq!(r.hold, HoldStatus::NotAttempted);
        assert_eq!(r.record_version, RECORD_VERSION);
    }

    #[test]
    fn the_hold_status_expresses_every_later_phase_arm() {
        let labels: Vec<String> = [
            HoldStatus::Confirmed,
            HoldStatus::Unconfirmed {
                cause: "read-back missing reason".to_string(),
            },
            HoldStatus::Local,
            HoldStatus::LocalSecondary,
        ]
        .iter()
        .map(HoldStatus::label)
        .collect();
        assert_eq!(
            labels,
            [
                "confirmed",
                "unconfirmed: read-back missing reason",
                "local",
                "local (secondary)"
            ]
        );
        assert_eq!(
            serde_json::to_value(HoldStatus::LocalSecondary).unwrap(),
            serde_json::json!({"status": "local_secondary"})
        );
        assert_eq!(
            serde_json::to_value(HoldStatus::Unconfirmed {
                cause: "x".to_string()
            })
            .unwrap(),
            serde_json::json!({"status": "unconfirmed", "cause": "x"})
        );
    }

    #[test]
    fn every_prep_state_spells_the_plan_vocabulary() {
        let spelled: Vec<serde_json::Value> = [
            PrepState::Preparing,
            PrepState::Ready,
            PrepState::Restoring,
            PrepState::Done,
            PrepState::Cancelled,
        ]
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();
        assert_eq!(
            spelled,
            ["preparing", "ready", "restoring", "done", "cancelled"]
        );
    }

    #[test]
    fn the_record_path_is_instance_scoped_and_named() {
        let p = record_path();
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(RECORD_FILE));
        let ledger = crate::session::session_ledger::ledger_path();
        assert_eq!(
            p.parent(),
            ledger.parent(),
            "the record sits beside the instance-scoped session ledger"
        );
    }
}
