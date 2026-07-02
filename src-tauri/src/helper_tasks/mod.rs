//! Helper Task Queue — runner emit + consume surfaces (plan
//! `2026-06-29-helper-task-queue-non-programmer-dev.md`, Phase 1.3).
//!
//! A *helper task* brokers a small unit of human judgment the runner cannot
//! make on its own ("does this page look right?"). Phase 1 ships the
//! `spot_check` kind end-to-end:
//!
//! - **Emit** — [`HelperTaskRegistrar`] records a `CreateHelperTaskRequest`
//!   on the shared session outbox (`SessionEventKind::HelperTaskCreated`); the
//!   `CoordSync` drain POSTs it to `POST /coord/helper-tasks` with the device
//!   JWT, reusing the existing drain → auth → retry machinery. The emit hook
//!   lives in `spec_api::spec_check` and fires on a Yellow (partial-match)
//!   classification when the owner setting
//!   (`settings.helper_tasks.emit_enabled`, default OFF) allows it.
//! - **Consume** — [`poller`] periodically GETs
//!   `/coord/helper-tasks/answers?since=<cursor>` (device JWT), folds returned
//!   [`HelperAnswer`]s into the in-memory store here, and persists the cursor
//!   next to `settings.json`. [`reflection_context_section`] renders the
//!   collected verdicts as a small additive section for the reflection
//!   agent's prompt (an approve = confirmation; a reject with reasons = a
//!   high-signal fix target).
//!
//! Everything here is best-effort: a coord 503 (helper-task tables not
//! migrated yet) is treated as "feature not available yet" and never blocks
//! or crash-loops the runner.

pub mod poller;
pub mod registrar;

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use qontinui_types::helper_task::HelperAnswer;
use tracing::debug;

pub use registrar::HelperTaskRegistrar;

/// Cap on answers retained in memory. Old answers age out FIFO; the reflection
/// section and Review tab only ever need the recent tail.
const MAX_STORED_ANSWERS: usize = 200;

fn answers_store() -> &'static Mutex<VecDeque<HelperAnswer>> {
    static ANSWERS: OnceLock<Mutex<VecDeque<HelperAnswer>>> = OnceLock::new();
    ANSWERS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Fold newly polled answers into the store, deduplicating by answer `id`
/// (the `since` cursor is inclusive-boundary-safe this way) and trimming to
/// [`MAX_STORED_ANSWERS`] FIFO.
pub fn record_answers(new_answers: Vec<HelperAnswer>) {
    if new_answers.is_empty() {
        return;
    }
    let Ok(mut store) = answers_store().lock() else {
        return;
    };
    for a in new_answers {
        if store.iter().any(|existing| existing.id == a.id) {
            continue;
        }
        if store.len() >= MAX_STORED_ANSWERS {
            store.pop_front();
        }
        store.push_back(a);
    }
}

/// Snapshot of the retained answers, oldest first (poll order).
pub fn recent_answers() -> Vec<HelperAnswer> {
    answers_store()
        .lock()
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default()
}

/// Max verdict lines rendered into the reflection prompt — keep the section
/// small and additive (newest answers win).
const MAX_REFLECTION_VERDICTS: usize = 20;

/// Render collected helper verdicts as a concise reflection-prompt section.
/// Returns `None` when no answers have been collected — the reflection prompt
/// is then unchanged (zero cost for the common no-helpers case).
pub fn reflection_context_section() -> Option<String> {
    let answers = recent_answers();
    if answers.is_empty() {
        return None;
    }

    let mut section = String::from(
        "\n\n## Human helper verdicts\n\n\
         Human helpers reviewed spot-check tasks emitted by this runner (Helper \
         Task Queue). An `approve` is confirmation the page looked right to a \
         human; a `reject` with reasons is a high-signal fix target — weigh it \
         above automated heuristics for the page it names.\n\n",
    );
    // Newest first, capped.
    for a in answers.iter().rev().take(MAX_REFLECTION_VERDICTS) {
        let verdict = serde_json::to_value(a.verdict)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        let mut line = format!("- task {}: {}", a.task_id, verdict);
        if !a.reasons.is_empty() {
            line.push_str(&format!(" (reasons: {})", a.reasons.join(", ")));
        }
        if let Some(text) = a.free_text.as_deref().filter(|t| !t.trim().is_empty()) {
            line.push_str(&format!(" — \"{}\"", text.trim()));
        }
        section.push_str(&line);
        section.push('\n');
    }
    Some(section)
}

/// Emit-hook entrypoint for the yellow-band spec-check classification
/// (`spec_api::spec_check`). Resolves the managed [`HelperTaskRegistrar`]
/// from Tauri state and emits a `spot_check` task for the page. Best-effort:
/// a missing registrar (early boot, tests) or a disabled owner setting is a
/// silent no-op — the spec-check response is never affected.
///
/// `screenshot_url` is `None` at this call site: the runner's screenshot
/// upload path (`commands::screenshot::capture_and_upload_screenshot`)
/// requires the Python-bridge compartment plus a qontinui-web `project_id`
/// and user auth, none of which exist in the spec-check HTTP context. The
/// helper portal renders the prompt without an image, so the answer flow
/// still works end-to-end.
pub fn maybe_emit_spot_check(
    app_handle: &tauri::AppHandle,
    app_id: &str,
    page_id: &str,
    match_rate: f64,
) {
    use tauri::Manager;
    let Some(registrar) = app_handle.try_state::<HelperTaskRegistrar>() else {
        debug!("helper_tasks: registrar not managed yet — skipping spot-check emit");
        return;
    };
    let prompt = format!("Does this page look right? Page: {page_id} (app: {app_id})");
    registrar.emit_spot_check(
        app_id,
        &prompt,
        None,
        Some(page_id.to_string()),
        Some(match_rate),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::helper_task::HelperVerdict;

    fn answer(id: &str, verdict: HelperVerdict, reasons: Vec<&str>) -> HelperAnswer {
        HelperAnswer {
            id: id.to_string(),
            task_id: format!("task-for-{id}"),
            helper_user_id: "helper-1".to_string(),
            verdict,
            reasons: reasons.into_iter().map(String::from).collect(),
            free_text: None,
            created_at: "2026-07-01T00:00:00Z".to_string(),
        }
    }

    /// The store + section render are process-global; run the assertions in
    /// one test body so parallel test threads don't interleave.
    #[test]
    fn store_dedups_and_section_renders_verdicts() {
        record_answers(vec![
            answer("a1", HelperVerdict::Approve, vec![]),
            answer(
                "a2",
                HelperVerdict::Reject,
                vec!["text_cut_off", "overlapping"],
            ),
        ]);
        // Re-recording the same ids is a no-op (cursor boundary safety).
        record_answers(vec![answer("a1", HelperVerdict::Approve, vec![])]);

        let all = recent_answers();
        assert_eq!(all.iter().filter(|a| a.id == "a1").count(), 1);
        assert_eq!(all.iter().filter(|a| a.id == "a2").count(), 1);

        let section = reflection_context_section().expect("answers present → section");
        assert!(section.contains("## Human helper verdicts"));
        assert!(section.contains("task task-for-a1: approve"));
        assert!(section.contains("task task-for-a2: reject (reasons: text_cut_off, overlapping)"));
    }

    #[test]
    fn helper_tasks_settings_default_off() {
        let s = crate::settings::HelperTasksSettings::default();
        assert!(!s.emit_enabled, "emit must default OFF — opt-in only");
        assert_eq!(s.emit_kinds, vec!["spot_check".to_string()]);
    }
}
