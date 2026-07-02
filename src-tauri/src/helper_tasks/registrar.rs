//! `HelperTaskRegistrar` — outbox writer for helper-task creation
//! (plan `2026-06-29-helper-task-queue`, Phase 1.3 Part A).
//!
//! Mirrors [`crate::claude_session::coord_register::AiCoordRegistrar`]'s
//! shape: a plain cloneable struct wrapping `Arc<Inner>` that holds the SAME
//! shared [`OutboxWriter`] the `CoordSync` drain loop reads. Emitting a task
//! is therefore one durable local write; the drain maps
//! [`SessionEventKind::HelperTaskCreated`] to `POST /coord/helper-tasks` with
//! the device JWT under a best-effort posture: transient failures get a
//! bounded per-record retry that never blocks session events queued behind
//! them, and a coord `helper_task_queue_unavailable` 503 (helper-task tables
//! not migrated) is dropped with a warn, never a retry loop.

use std::sync::Arc;

use qontinui_types::helper_task::{
    HelperAnswerSchema, HelperTaskKind, HelperTaskPayload, HelperTaskSource, HelperVerdict,
};
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::session::local_store::OutboxWriter;
use crate::session::SessionEventKind;

/// Preset reject-reason chips offered to the helper on a `spot_check` task.
const SPOT_CHECK_PRESET_REASONS: [&str; 4] = [
    "text_cut_off",
    "overlapping",
    "wrong_color",
    "button_missing",
];

/// Records helper-task creation requests on the shared session outbox.
///
/// Managed as Tauri state (see `main.rs`, constructed alongside
/// `AiCoordRegistrar` with the same outbox + machine id); cheap to clone.
#[derive(Clone)]
pub struct HelperTaskRegistrar {
    inner: Arc<Inner>,
}

struct Inner {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
}

impl HelperTaskRegistrar {
    /// Construct from the shared session outbox + this device's `machine_id`.
    /// The outbox MUST be the same `Arc` the `CoordSync` drain loop reads.
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        Self {
            inner: Arc::new(Inner { outbox, machine_id }),
        }
    }

    /// Emit a `spot_check` helper task for `app_id`. Gated on the owner
    /// setting (`settings.helper_tasks`): emit must be enabled AND
    /// `"spot_check"` must be in `emit_kinds`. Also gated on the per-
    /// `(app_id, page_id)` emit cooldown ([`super::EMIT_COOLDOWN`]) so a
    /// still-yellow page doesn't re-emit a duplicate task on every
    /// evaluation. Returns `true` when a task was recorded on the outbox.
    /// Best-effort throughout — a disabled gate, an active cooldown, or an
    /// outbox write error never disturbs the caller.
    pub fn emit_spot_check(
        &self,
        app_id: &str,
        prompt: &str,
        screenshot_url: Option<String>,
        page_id: Option<String>,
        match_rate: Option<f64>,
    ) -> bool {
        let cfg = crate::settings::get_helper_tasks_settings();
        if !cfg.emit_enabled {
            debug!("helper_tasks: emit disabled (settings.helper_tasks.emit_enabled=false) — skipping spot-check for {app_id}");
            return false;
        }
        if !cfg.emit_kinds.iter().any(|k| k == "spot_check") {
            debug!("helper_tasks: kind spot_check not in emit_kinds — skipping for {app_id}");
            return false;
        }
        let page_key = page_id.as_deref().unwrap_or("").to_string();
        if super::emit_cooldown_active(app_id, &page_key) {
            debug!(
                "helper_tasks: spot-check for {app_id}/{page_key} within emit cooldown — skipping"
            );
            return false;
        }
        let recorded = self.record_spot_check(app_id, prompt, screenshot_url, page_id, match_rate);
        if recorded {
            super::record_emit_timestamp(app_id, &page_key);
        }
        recorded
    }

    /// Build the `CreateHelperTaskRequest` body and record it on the outbox.
    /// Split from [`Self::emit_spot_check`] so tests can exercise the wire
    /// shape without the settings-file gate.
    ///
    /// Wire shape: snake_case top-level fields; nested structs serialize
    /// camelCase via the canonical `qontinui_types::helper_task` types.
    fn record_spot_check(
        &self,
        app_id: &str,
        prompt: &str,
        screenshot_url: Option<String>,
        page_id: Option<String>,
        match_rate: Option<f64>,
    ) -> bool {
        let payload = HelperTaskPayload {
            screenshot_url,
            ..Default::default()
        };
        let answer_schema = HelperAnswerSchema {
            verdicts: vec![
                HelperVerdict::Approve,
                HelperVerdict::Reject,
                HelperVerdict::NotSure,
            ],
            preset_reasons: SPOT_CHECK_PRESET_REASONS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            allow_free_text: true,
            allow_not_sure: true,
        };
        let source = HelperTaskSource {
            finding_id: None,
            page_id,
            match_rate,
        };
        // `expires_at` is omitted — coord's broker default applies.
        let body = json!({
            "app_id": app_id,
            "kind": HelperTaskKind::SpotCheck,
            "prompt": prompt,
            "payload": payload,
            "answer_schema": answer_schema,
            "required_votes": 1,
            "source": source,
        });

        // The outbox keys its monotonic `seq` on `(machine_id, session_id)`,
        // but helper tasks have no real session — mint a deterministic UUIDv5
        // per app so all tasks for one app share a seq lane (stable ordering,
        // idempotent replay) without colliding across apps. Coord never reads
        // this id (mirrors `AiCoordRegistrar::report_commits`).
        let session_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("helper-task:{app_id}").as_bytes(),
        );

        match self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::HelperTaskCreated,
            body,
        ) {
            Ok(_) => {
                info!("helper_tasks: enqueued spot_check task for app {app_id}");
                true
            }
            Err(e) => {
                warn!("helper_tasks: outbox HelperTaskCreated write failed for {app_id} (best-effort): {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn registrar() -> (HelperTaskRegistrar, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        (HelperTaskRegistrar::new(outbox, Uuid::new_v4()), dir)
    }

    #[test]
    fn record_spot_check_writes_create_request_shape() {
        let (reg, _dir) = registrar();
        assert!(reg.record_spot_check(
            "my-app",
            "Does this page look right? Page: settings-general (app: my-app)",
            None,
            Some("settings-general".to_string()),
            Some(0.72),
        ));

        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.event_kind, SessionEventKind::HelperTaskCreated.as_str());

        let body = &row.payload;
        // Top-level fields are snake_case.
        assert_eq!(body["app_id"], json!("my-app"));
        assert_eq!(body["kind"], json!("spot_check"));
        assert_eq!(body["required_votes"], json!(1));
        assert!(body.get("expires_at").is_none());
        // Nested structs are camelCase (canonical schema serialization).
        assert_eq!(
            body["answer_schema"]["verdicts"],
            json!(["approve", "reject", "not_sure"])
        );
        assert_eq!(
            body["answer_schema"]["presetReasons"],
            json!([
                "text_cut_off",
                "overlapping",
                "wrong_color",
                "button_missing"
            ])
        );
        assert_eq!(body["answer_schema"]["allowFreeText"], json!(true));
        assert_eq!(body["answer_schema"]["allowNotSure"], json!(true));
        assert_eq!(body["source"]["pageId"], json!("settings-general"));
        assert_eq!(body["source"]["matchRate"], json!(0.72));
        // SpotCheck without a screenshot omits the field entirely.
        assert!(body["payload"].get("screenshotUrl").is_none());
    }

    #[test]
    fn spot_checks_for_one_app_share_a_seq_lane() {
        let (reg, _dir) = registrar();
        assert!(reg.record_spot_check("app-a", "p1", None, None, None));
        assert!(reg.record_spot_check("app-a", "p2", None, None, None));
        assert!(reg.record_spot_check("app-b", "p3", None, None, None));

        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 3);
        let a_rows: Vec<_> = pending
            .iter()
            .filter(|r| r.payload["app_id"] == json!("app-a"))
            .collect();
        assert_eq!(a_rows.len(), 2);
        assert_eq!(a_rows[0].session_id, a_rows[1].session_id);
        assert_eq!(a_rows[0].seq, 1);
        assert_eq!(a_rows[1].seq, 2);
        let b_row = pending
            .iter()
            .find(|r| r.payload["app_id"] == json!("app-b"))
            .unwrap();
        assert_ne!(b_row.session_id, a_rows[0].session_id);
    }

    #[test]
    fn screenshot_url_is_forwarded_when_present() {
        let (reg, _dir) = registrar();
        assert!(reg.record_spot_check(
            "app",
            "p",
            Some("https://example.com/shot.png".to_string()),
            None,
            None,
        ));
        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(
            pending[0].payload["payload"]["screenshotUrl"],
            json!("https://example.com/shot.png")
        );
    }
}
