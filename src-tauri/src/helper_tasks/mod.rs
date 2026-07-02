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
//!   (`settings.helper_tasks.emit_enabled`, default OFF) allows it. A
//!   per-(app, page) emit cooldown ([`EMIT_COOLDOWN`]) stops a still-yellow
//!   page from re-emitting on every evaluation.
//! - **Consume** — [`poller`] periodically GETs
//!   `/coord/helper-tasks/answers?since=<cursor>` (device JWT), folds returned
//!   [`HelperAnswer`]s into the store here, and persists the cursor next to
//!   `settings.json`. [`reflection_context_section`] renders the collected
//!   verdicts as a small additive section for the reflection agent's prompt
//!   (an approve = confirmation; a reject with reasons = a high-signal fix
//!   target).
//!
//! The store (answers + task metadata + emit cooldowns) is persisted as one
//! JSON file next to the poll cursor ([`STORE_FILE`]) so a runner restart
//! keeps the collected human verdicts; [`load_persisted_store`] restores it
//! at boot (called from `main.rs` setup).
//!
//! Everything here is best-effort: a coord `helper_task_queue_unavailable`
//! 503 (helper-task tables not migrated yet) is treated as "feature not
//! available yet" and never blocks or crash-loops the runner.

pub mod poller;
pub mod registrar;

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use qontinui_types::helper_task::HelperAnswer;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

pub use registrar::HelperTaskRegistrar;

/// Cap on answers retained in the store. Old answers age out FIFO; the
/// reflection section and Review tab only ever need the recent tail.
const MAX_STORED_ANSWERS: usize = 200;

/// Cap on task-metadata entries retained in the store. When exceeded, entries
/// not referenced by any retained answer are pruned first (metadata for a task
/// whose answers already aged out is dead weight).
const MAX_TASK_META: usize = 500;

/// Persisted store file, co-located with `settings.json` (honors the
/// per-instance `QONTINUI_CONFIG_DIR` override, so temp runners don't share
/// the primary's store). Same atomic-write posture as the poll cursor.
const STORE_FILE: &str = "helper-tasks-store.json";

/// Minimum interval between spot-check emissions for the same
/// `(app_id, page_id)`. A still-yellow page re-classifies on every spec-check
/// evaluation; without a cooldown each evaluation re-emits a duplicate task
/// and floods the helper queue. 24h keeps at most one task per page per day.
pub(crate) const EMIT_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);

/// Max age of a verdict rendered into the reflection prompt. Verdicts age out
/// naturally so the same stale verdicts are not re-injected into every
/// reflection for the whole process lifetime — a day-old human judgment about
/// a page that has since been rebuilt is noise, not signal.
const REFLECTION_VERDICT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Per-task provenance captured from the drain's `POST /coord/helper-tasks`
/// 201 response (`id` / `appId` / `source.pageId`), keyed by task id in the
/// store. Lets reflection verdict lines name the page/app instead of an
/// unactionable coord task UUID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMeta {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
}

/// The persisted helper-task store: collected answers, task provenance, and
/// per-page emit cooldowns. Serialized as one JSON document ([`STORE_FILE`]).
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreState {
    #[serde(default)]
    answers: VecDeque<HelperAnswer>,
    /// task_id → provenance from the drain's 201 responses.
    #[serde(default)]
    task_meta: HashMap<String, TaskMeta>,
    /// `cooldown_key(app_id, page_id)` → last spot-check emit (unix seconds).
    #[serde(default)]
    emit_cooldowns: HashMap<String, u64>,
}

fn store() -> &'static Mutex<StoreState> {
    static STORE: OnceLock<Mutex<StoreState>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(StoreState::default()))
}

fn store_path() -> Option<std::path::PathBuf> {
    crate::settings::get_config_dir()
        .ok()
        .map(|d| d.join(STORE_FILE))
}

/// Restore the persisted store at boot (answers + task metadata + emit
/// cooldowns) so a runner restart does not discard collected human verdicts —
/// coord only serves answers with `created_at > since`, so anything already
/// polled is otherwise gone for good. Best-effort: a missing file is first
/// boot, a corrupt file is logged and ignored.
pub fn load_persisted_store() {
    let Some(path) = store_path() else {
        return;
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return, // first boot — nothing persisted yet
    };
    match serde_json::from_str::<StoreState>(&raw) {
        Ok(mut loaded) => {
            while loaded.answers.len() > MAX_STORED_ANSWERS {
                loaded.answers.pop_front();
            }
            let (answers, tasks) = (loaded.answers.len(), loaded.task_meta.len());
            if let Ok(mut s) = store().lock() {
                *s = loaded;
            }
            info!("helper_tasks: restored persisted store ({answers} answer(s), {tasks} task(s))");
        }
        Err(e) => warn!("helper_tasks: persisted store unreadable — starting empty: {e}"),
    }
}

/// Persist the store atomically (same pattern as the poll cursor).
/// Best-effort — a write failure only costs durability across the next
/// restart, never the in-memory state.
#[cfg(not(test))]
fn persist_store(state: &StoreState) {
    let Some(path) = store_path() else {
        return;
    };
    let body = match serde_json::to_vec(state) {
        Ok(b) => b,
        Err(e) => {
            warn!("helper_tasks: store serialize failed (best-effort): {e}");
            return;
        }
    };
    if let Err(e) = crate::fs_atomic::atomic_write(&path, &body) {
        warn!("helper_tasks: store persist failed (best-effort): {e}");
    }
}

/// Test builds never touch the real config dir.
#[cfg(test)]
fn persist_store(_state: &StoreState) {}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// `candidate` is strictly newer than `current`. Falls back to lexicographic
/// comparison when either timestamp fails to parse (RFC 3339 in a uniform
/// format sorts lexicographically).
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_ts(candidate), parse_ts(current)) {
        (Some(c), Some(e)) => c > e,
        _ => candidate > current,
    }
}

/// Fold newly polled answers into the store, keyed by answer `id`, and trim
/// to [`MAX_STORED_ANSWERS`] FIFO. An id already present is normally a poll
/// re-read (the cursor overlap window); but coord also UPSERTS verdict
/// *revisions* under the SAME id with a bumped `created_at` — on id match the
/// stored answer is REPLACED when the incoming `created_at` is newer, so a
/// helper changing their verdict is reflected instead of dropped.
pub fn record_answers(new_answers: Vec<HelperAnswer>) {
    if new_answers.is_empty() {
        return;
    }
    let Ok(mut s) = store().lock() else {
        return;
    };
    let mut changed = false;
    for a in new_answers {
        if let Some(existing) = s.answers.iter_mut().find(|e| e.id == a.id) {
            if is_newer(&a.created_at, &existing.created_at) {
                *existing = a;
                changed = true;
            }
            continue;
        }
        if s.answers.len() >= MAX_STORED_ANSWERS {
            s.answers.pop_front();
        }
        s.answers.push_back(a);
        changed = true;
    }
    if changed {
        persist_store(&s);
    }
}

/// Record task provenance parsed from the drain's `POST /coord/helper-tasks`
/// 201 response, so reflection verdict lines can render "page X (app Y)"
/// instead of the coord task UUID. Persisted with the rest of the store.
pub fn record_task_metadata(task_id: &str, app_id: &str, page_id: Option<String>) {
    let Ok(mut s) = store().lock() else {
        return;
    };
    s.task_meta.insert(
        task_id.to_string(),
        TaskMeta {
            app_id: app_id.to_string(),
            page_id,
        },
    );
    if s.task_meta.len() > MAX_TASK_META {
        let referenced: std::collections::HashSet<String> =
            s.answers.iter().map(|a| a.task_id.clone()).collect();
        s.task_meta
            .retain(|k, _| k == task_id || referenced.contains(k));
    }
    persist_store(&s);
}

/// True when the store knows about anything (answers or emitted tasks) — used
/// by the poller so pausing emission does not also pause collecting answers
/// to tasks already outstanding.
pub fn store_has_content() -> bool {
    store()
        .lock()
        .map(|s| !s.answers.is_empty() || !s.task_meta.is_empty())
        .unwrap_or(false)
}

fn cooldown_key(app_id: &str, page_id: &str) -> String {
    format!("{app_id}\u{1f}{page_id}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when a spot-check for `(app_id, page_id)` was emitted within
/// [`EMIT_COOLDOWN`] — the registrar then skips the duplicate emit.
pub fn emit_cooldown_active(app_id: &str, page_id: &str) -> bool {
    let now = unix_now();
    store()
        .lock()
        .map(|s| {
            s.emit_cooldowns
                .get(&cooldown_key(app_id, page_id))
                .is_some_and(|&t| now.saturating_sub(t) < EMIT_COOLDOWN.as_secs())
        })
        .unwrap_or(false)
}

/// Stamp "now" as the last spot-check emit for `(app_id, page_id)`. Expired
/// entries are pruned on each stamp so the map stays bounded to
/// actively-emitting pages. Persisted with the rest of the store.
pub fn record_emit_timestamp(app_id: &str, page_id: &str) {
    let now = unix_now();
    let Ok(mut s) = store().lock() else {
        return;
    };
    let window = EMIT_COOLDOWN.as_secs();
    s.emit_cooldowns
        .retain(|_, t| now.saturating_sub(*t) < window);
    s.emit_cooldowns.insert(cooldown_key(app_id, page_id), now);
    persist_store(&s);
}

/// Snapshot of the retained answers, oldest first (poll order).
pub fn recent_answers() -> Vec<HelperAnswer> {
    store()
        .lock()
        .map(|s| s.answers.iter().cloned().collect())
        .unwrap_or_default()
}

/// Max verdict lines rendered into the reflection prompt — keep the section
/// small and additive (newest answers win).
const MAX_REFLECTION_VERDICTS: usize = 20;

/// Render collected helper verdicts as a concise reflection-prompt section.
/// Only answers younger than [`REFLECTION_VERDICT_MAX_AGE`] are included
/// (answers whose `created_at` doesn't parse are excluded — they can't be
/// aged). Returns `None` when nothing qualifies — the reflection prompt is
/// then unchanged (zero cost for the common no-helpers case).
pub fn reflection_context_section() -> Option<String> {
    let (answers, task_meta) = store()
        .lock()
        .map(|s| {
            (
                s.answers.iter().cloned().collect::<Vec<_>>(),
                s.task_meta.clone(),
            )
        })
        .unwrap_or_default();
    let cutoff = chrono::Utc::now()
        - chrono::Duration::from_std(REFLECTION_VERDICT_MAX_AGE)
            .unwrap_or_else(|_| chrono::Duration::hours(24));
    let fresh: Vec<&HelperAnswer> = answers
        .iter()
        .filter(|a| parse_ts(&a.created_at).is_some_and(|t| t >= cutoff))
        .collect();
    if fresh.is_empty() {
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
    for a in fresh.iter().rev().take(MAX_REFLECTION_VERDICTS) {
        let verdict = serde_json::to_value(a.verdict)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        // Name the page/app when the drain recorded provenance for the task;
        // fall back to the raw task UUID otherwise.
        let target = match task_meta.get(&a.task_id) {
            Some(meta) => match meta.page_id.as_deref() {
                Some(page) => format!("page {page} (app {})", meta.app_id),
                None => format!("app {}", meta.app_id),
            },
            None => format!("task {}", a.task_id),
        };
        let mut line = format!("- {target}: {verdict}");
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

/// Resolve the managed [`HelperTaskRegistrar`] from Tauri state. `None`
/// during early boot or in tests — callers treat that as a silent no-op
/// (best-effort posture).
pub fn registrar_from_app(app_handle: &tauri::AppHandle) -> Option<HelperTaskRegistrar> {
    use tauri::Manager;
    match app_handle.try_state::<HelperTaskRegistrar>() {
        Some(reg) => Some(reg.inner().clone()),
        None => {
            debug!("helper_tasks: registrar not managed yet — skipping spot-check emit");
            None
        }
    }
}

/// Emit-hook entrypoint for the yellow-band spec-check classification
/// (`spec_api::spec_check`). Emits a `spot_check` task for the page via the
/// given registrar. Best-effort: a disabled owner setting or an active
/// per-page cooldown is a silent no-op — the spec-check response is never
/// affected.
///
/// `screenshot_url` is `None` at this call site: the runner's screenshot
/// upload path (`commands::screenshot::capture_and_upload_screenshot`)
/// requires the Python-bridge compartment plus a qontinui-web `project_id`
/// and user auth, none of which exist in the spec-check HTTP context. The
/// helper portal renders the prompt without an image, so the answer flow
/// still works end-to-end.
pub fn maybe_emit_spot_check(
    registrar: &HelperTaskRegistrar,
    app_id: &str,
    page_id: &str,
    match_rate: f64,
) {
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

    fn answer_at(
        id: &str,
        verdict: HelperVerdict,
        reasons: Vec<&str>,
        created_at: &str,
    ) -> HelperAnswer {
        HelperAnswer {
            id: id.to_string(),
            task_id: format!("task-for-{id}"),
            helper_user_id: "helper-1".to_string(),
            verdict,
            reasons: reasons.into_iter().map(String::from).collect(),
            free_text: None,
            created_at: created_at.to_string(),
        }
    }

    fn answer(id: &str, verdict: HelperVerdict, reasons: Vec<&str>) -> HelperAnswer {
        answer_at(id, verdict, reasons, &chrono::Utc::now().to_rfc3339())
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
        // Re-recording the same id with the same created_at is a no-op
        // (cursor-overlap re-read safety).
        let all_before = recent_answers();
        let a1_ts = all_before
            .iter()
            .find(|a| a.id == "a1")
            .unwrap()
            .created_at
            .clone();
        record_answers(vec![answer_at(
            "a1",
            HelperVerdict::Approve,
            vec![],
            &a1_ts,
        )]);

        let all = recent_answers();
        assert_eq!(all.iter().filter(|a| a.id == "a1").count(), 1);
        assert_eq!(all.iter().filter(|a| a.id == "a2").count(), 1);

        let section = reflection_context_section().expect("answers present → section");
        assert!(section.contains("## Human helper verdicts"));
        assert!(section.contains("task task-for-a1: approve"));
        assert!(section.contains("task task-for-a2: reject (reasons: text_cut_off, overlapping)"));
    }

    #[test]
    fn revision_with_newer_created_at_replaces_stored_answer() {
        let old_ts = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
        let new_ts = chrono::Utc::now().to_rfc3339();
        record_answers(vec![answer_at(
            "rev1",
            HelperVerdict::Approve,
            vec![],
            &old_ts,
        )]);
        // Coord UPSERTS a revision under the SAME id with a bumped created_at.
        record_answers(vec![answer_at(
            "rev1",
            HelperVerdict::Reject,
            vec!["wrong_color"],
            &new_ts,
        )]);
        let stored: Vec<_> = recent_answers()
            .into_iter()
            .filter(|a| a.id == "rev1")
            .collect();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].verdict, HelperVerdict::Reject);
        assert_eq!(stored[0].created_at, new_ts);

        // An OLDER created_at under the same id never regresses the store.
        let older_ts = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        record_answers(vec![answer_at(
            "rev1",
            HelperVerdict::Approve,
            vec![],
            &older_ts,
        )]);
        let stored: Vec<_> = recent_answers()
            .into_iter()
            .filter(|a| a.id == "rev1")
            .collect();
        assert_eq!(stored[0].verdict, HelperVerdict::Reject);
    }

    #[test]
    fn reflection_section_ages_out_stale_verdicts_and_names_pages() {
        let stale_ts = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        record_answers(vec![
            answer_at(
                "stale1",
                HelperVerdict::Reject,
                vec!["overlapping"],
                &stale_ts,
            ),
            answer("fresh1", HelperVerdict::Approve, vec![]),
        ]);
        record_task_metadata(
            "task-for-fresh1",
            "my-app",
            Some("settings-general".to_string()),
        );

        let section = reflection_context_section().expect("fresh answer present → section");
        // Stale verdicts age out (REFLECTION_VERDICT_MAX_AGE).
        assert!(!section.contains("task-for-stale1"));
        // Mapped tasks render page/app context instead of the task UUID.
        assert!(section.contains("page settings-general (app my-app): approve"));
        assert!(!section.contains("task task-for-fresh1"));
    }

    #[test]
    fn emit_cooldown_gates_repeat_emits_per_page() {
        assert!(!emit_cooldown_active("cool-app", "page-x"));
        record_emit_timestamp("cool-app", "page-x");
        assert!(emit_cooldown_active("cool-app", "page-x"));
        // Cooldown is per-(app, page): sibling pages/apps are unaffected.
        assert!(!emit_cooldown_active("cool-app", "page-y"));
        assert!(!emit_cooldown_active("other-app", "page-x"));
    }

    #[test]
    fn store_state_round_trips_through_json() {
        let mut state = StoreState::default();
        state
            .answers
            .push_back(answer("persist1", HelperVerdict::Approve, vec![]));
        state.task_meta.insert(
            "task-for-persist1".to_string(),
            TaskMeta {
                app_id: "app".to_string(),
                page_id: Some("page".to_string()),
            },
        );
        state
            .emit_cooldowns
            .insert(cooldown_key("app", "page"), unix_now());

        let json = serde_json::to_string(&state).unwrap();
        let loaded: StoreState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.answers.len(), 1);
        assert_eq!(loaded.answers[0].id, "persist1");
        assert_eq!(
            loaded.task_meta.get("task-for-persist1").unwrap().page_id,
            Some("page".to_string())
        );
        assert_eq!(loaded.emit_cooldowns.len(), 1);
    }

    #[test]
    fn helper_tasks_settings_default_off() {
        let s = crate::settings::HelperTasksSettings::default();
        assert!(!s.emit_enabled, "emit must default OFF — opt-in only");
        assert_eq!(s.emit_kinds, vec!["spot_check".to_string()]);
    }
}
