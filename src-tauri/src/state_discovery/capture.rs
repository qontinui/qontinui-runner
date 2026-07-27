//! Capture hook for co-occurrence observations.
//!
//! Called fire-and-forget from the UI Bridge snapshot handler. Converts a
//! snapshot into a single observation row keyed by (fingerprints,
//! snapshot_metadata). Never returns an error to the caller — the snapshot
//! response must never be blocked or failed by observation capture.

use std::sync::Arc;

use tracing::warn;
use uuid::Uuid;

use crate::database::pg::PgDb;
use crate::spec_api::slug::pathname_to_spec_id;

use super::fingerprint::stable_element_fingerprint;

/// Sample rate for observation capture. K=1 captures every snapshot; set
/// higher to downsample if the observations table grows too fast. See the
/// "Observation volume" risk in the design doc — partitioning and
/// downsampling are the two levers we'll reach for first.
const SAMPLE_RATE: u32 = 1;

/// Resolve the page identity that labels this observation.
///
/// This is a **selection label, not a partition key.** Derivation stays global
/// — co-occurrence clustering groups elements that appear in the same set of
/// renders, so restricting the render pool to one page would collapse that
/// page's persistent elements into a single mega-state and destroy the
/// cross-view signal the algorithm exists to find. The label is what later
/// lets authoring project the global state set `S` down to the states active
/// on one page (`S_Ξ ⊆ S`).
///
/// Precedence:
/// 1. `page.pageContext.meta.tabId` — a stable developer-supplied view id.
///    Desktop SPAs route in React state, so the URL never moves; the tab id is
///    the only thing distinguishing one view from another. (The runner's whole
///    corpus sits at `http://tauri.localhost/`.)
/// 2. top-level `activeTab` — the same identity, supplied by the SDK's own
///    `getActiveTab` provider rather than by `usePageContext`. It is already
///    present in every runner snapshot, so labelling works on runners built
///    before `meta.tabId` existed instead of waiting for a rebuild.
/// 3. `page.pageContext.name` — slugged. For apps that call `usePageContext`
///    without a tab id. Display labels drift on rename and collapse all
///    twelve `settings-*` views onto one name, so this ranks below both ids.
/// 4. `page.pathname` — slugged. Correct for real-URL apps (qontinui-web).
///
/// Blank candidates are skipped rather than accepted, so an empty `tabId`
/// falls through to the next source instead of shadowing it. Returns `None`
/// when nothing yields a usable slug; the observation is then recorded
/// unlabelled rather than dropped, since it still carries co-occurrence
/// signal for the global derivation.
fn resolve_page_label(snapshot: &serde_json::Value) -> Option<String> {
    let page = snapshot.get("page");
    let page_context = page.and_then(|p| p.get("pageContext"));

    let candidates = [
        page_context
            .and_then(|c| c.get("meta"))
            .and_then(|m| m.get("tabId")),
        snapshot.get("activeTab"),
        page_context.and_then(|c| c.get("name")),
        page.and_then(|p| p.get("pathname")),
    ];

    let raw = candidates
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|s| !s.trim().is_empty())?;

    Some(pathname_to_spec_id(raw))
}

/// Enqueue a single observation derived from a UI Bridge snapshot.
///
/// Extracts a fingerprint per element, resolves the page label (see
/// [`resolve_page_label`]), records minimal page metadata, and inserts one row
/// into `co_occurrence_observations`. All errors downgrade to WARN logs — the
/// snapshot path must never fail because of observation capture.
pub async fn enqueue_observation(
    pg_db: Arc<PgDb>,
    snapshot: &serde_json::Value,
    runner_instance: String,
) {
    // Sample-rate gate. SAMPLE_RATE == 1 means every call goes through;
    // K > 1 is the planned lever for volume management.
    if SAMPLE_RATE > 1 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // SAMPLE_RATE > 1 is guaranteed above, so modulo is well-defined.
        if !n.is_multiple_of(SAMPLE_RATE) {
            return;
        }
    }

    // Extract fingerprints from elements[].
    let elements = match snapshot.get("elements").and_then(|e| e.as_array()) {
        Some(arr) => arr,
        None => {
            // No elements to fingerprint — nothing to record. Not an error;
            // e.g. native-capture fallback snapshots have empty element
            // arrays and contain no co-occurrence signal.
            return;
        }
    };

    if elements.is_empty() {
        return;
    }

    let fingerprints: Vec<String> = elements
        .iter()
        .map(stable_element_fingerprint)
        .collect::<std::collections::BTreeSet<_>>() // dedup + deterministic order
        .into_iter()
        .collect();

    if fingerprints.is_empty() {
        return;
    }

    let element_count = elements.len() as i64;

    // Build snapshot_metadata from shallow page fields.
    let page = snapshot.get("page");
    let pathname = page
        .and_then(|p| p.get("pathname"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let url = page
        .and_then(|p| p.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let viewport = page.and_then(|p| p.get("viewport")).cloned();
    // Keep the raw developer context alongside the derived label so a
    // mislabelled corpus is diagnosable without replaying snapshots.
    let page_context = page.and_then(|p| p.get("pageContext")).cloned();

    let spec_id = resolve_page_label(snapshot);

    let snapshot_metadata = serde_json::json!({
        "pathname": pathname,
        "url": url,
        "viewport": viewport,
        "element_count": element_count,
        "page_context": page_context,
    });

    let fingerprints_json = serde_json::Value::Array(
        fingerprints
            .into_iter()
            .map(serde_json::Value::String)
            .collect(),
    );

    let id = Uuid::new_v4().to_string();

    // Insert. Soft-fail on any error — observation capture must never
    // compromise the snapshot response.
    let conn = match pg_db.pool().get().await {
        Ok(c) => c,
        Err(e) => {
            warn!("state_discovery::capture: PG pool error: {}", e);
            return;
        }
    };

    // Cast $1 to uuid via ::text::uuid so tokio-postgres serializes the String
    // as text (its uuid feature is not enabled) and PG coerces to uuid at
    // insertion. A bare $1::uuid makes PG infer the parameter as uuid and
    // String serialization fails. Cast $4/$5 to jsonb so the serde_json
    // payload lands in JSONB columns rather than text.
    let res = conn
        .execute(
            r#"INSERT INTO co_occurrence_observations
               (id, spec_id, runner_instance, fingerprints, snapshot_metadata)
               VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5::jsonb)"#,
            &[
                &id,
                &spec_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &runner_instance,
                &fingerprints_json,
                &snapshot_metadata,
            ],
        )
        .await;

    if let Err(e) = res {
        warn!(
            "state_discovery::capture: failed to insert observation ({} elements): {}",
            element_count, e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tab_id_wins_over_name_and_pathname() {
        let snap = json!({
            "page": {
                "pathname": "/",
                "pageContext": {
                    "name": "DAG Workflow Editor",
                    "meta": { "tabId": "dag-workflow-editor" }
                }
            }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("dag-workflow-editor")
        );
    }

    #[test]
    fn falls_back_to_top_level_active_tab() {
        // Runners built before `meta.tabId` existed still emit the SDK's own
        // top-level `activeTab`, so labelling must not depend on a rebuild.
        let snap = json!({
            "activeTab": "config-log-sources",
            "page": { "pathname": "/", "pageContext": { "name": "Settings" } }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("config-log-sources"),
            "activeTab must outrank the display name, which collapses every settings-* view"
        );
    }

    #[test]
    fn blank_candidate_falls_through_instead_of_shadowing() {
        let snap = json!({
            "activeTab": "capture",
            "page": { "pageContext": { "meta": { "tabId": "   " } } }
        });
        assert_eq!(resolve_page_label(&snap).as_deref(), Some("capture"));
    }

    #[test]
    fn falls_back_to_slugged_context_name() {
        let snap = json!({
            "page": { "pathname": "/", "pageContext": { "name": "Active Dashboard" } }
        });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("active-dashboard")
        );
    }

    #[test]
    fn falls_back_to_pathname_for_real_url_apps() {
        // qontinui-web (Next.js) has no pageContext but a meaningful pathname.
        let snap = json!({ "page": { "pathname": "/account/billing" } });
        assert_eq!(
            resolve_page_label(&snap).as_deref(),
            Some("account-billing")
        );
    }

    #[test]
    fn desktop_spa_root_pathname_is_the_degenerate_case() {
        // The failure this whole change exists to fix: a Tauri SPA reports
        // `/` for every one of its views, so pathname alone cannot tell them
        // apart. Without a pageContext there is nothing better to key on.
        let snap = json!({ "page": { "pathname": "/", "url": "http://tauri.localhost/" } });
        assert_eq!(resolve_page_label(&snap).as_deref(), Some("root"));
    }

    #[test]
    fn missing_page_or_blank_label_yields_none() {
        assert_eq!(resolve_page_label(&json!({})), None);
        assert_eq!(resolve_page_label(&json!({ "page": {} })), None);
        assert_eq!(
            resolve_page_label(&json!({ "page": { "pathname": "   " } })),
            None
        );
    }

    #[test]
    fn label_is_idempotent_under_reslugging() {
        // The label is written to `spec_id` and later compared against spec
        // ids on disk; re-slugging an already-canonical label must not move it.
        let snap = json!({
            "page": { "pageContext": { "meta": { "tabId": "config-log-sources" } } }
        });
        let once = resolve_page_label(&snap).unwrap();
        assert_eq!(pathname_to_spec_id(&once), once);
    }
}
