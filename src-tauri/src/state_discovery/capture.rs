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

/// Resolve the app that produced this snapshot, from the snapshot itself.
///
/// Read untyped off the top level, exactly like [`resolve_page_label`] reads
/// `activeTab` — and for the same reason. The snapshot handler dispatches by
/// `window_label` and holds no app identity of its own (the call site in
/// `mcp::ui_bridge::elements` says so outright: "the caller has no scope
/// knowledge the snapshot doesn't already carry"), so the id must arrive
/// inside the snapshot rather than as a parameter. The runner frontend stamps
/// it through its `runner-tabs` snapshot enricher from the single
/// `RUNNER_APP_ID` constant.
///
/// Blank values collapse to `None` so a whitespace-only `appId` records "app
/// unknown" rather than a key that can never match `project.apps`. The value
/// is trimmed, so incidental padding still matches the registry instead of
/// silently attributing the row to nothing.
///
/// Returning `None` is meaningful, not a failure: the column is nullable and
/// `NULL` means exactly "app unknown". Attribution is never fabricated — an
/// absent id stays absent, and an id the registry does not know is collapsed
/// to `NULL` by the INSERT itself (see [`OBSERVATION_INSERT_SQL`]).
fn resolve_app_id(snapshot: &serde_json::Value) -> Option<String> {
    let raw = snapshot.get("appId")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// The observation INSERT, hoisted to a `const` so its shape can be pinned by
/// a test.
///
/// This repo uses raw `tokio_postgres`, not sqlx: there is no compile-time
/// statement checking anywhere in the runner, so a wrong column name or arity
/// compiles clean and surfaces only as a `warn!` on a fire-and-forget path
/// nobody watches. The string-assertion test below is the only verification
/// available without a live PG — the same guard
/// `page_query_is_bounded_by_artifact_ids_not_by_time` puts on
/// `PAGE_RENDERS_SQL`.
///
/// Casts are deliberate. `$1::text::uuid` because tokio-postgres' `uuid`
/// feature is not enabled, so the id is serialized as text and PG coerces it
/// at insertion (a bare `$1::uuid` makes PG infer the parameter as uuid and
/// `String` serialization then fails). `$4::jsonb`/`$5::jsonb` land the serde
/// payloads in JSONB columns rather than text. `$6` needs **no** cast:
/// `project.apps.app_id` is `TEXT PRIMARY KEY` (so the lookup is unique and
/// index-backed) and `co_occurrence_observations.app_id` is `TEXT`.
///
/// `app_id` is validated **inside this statement** rather than with a second
/// round trip, so capture never pays an extra query per observation: an id the
/// registry does not know collapses to `NULL`, preserving "absent or unknown
/// writes NULL and logs, never fabricate".
///
/// **Dependency worth naming:** the subquery references `project.apps`, so if
/// that relation is missing the *whole INSERT fails* and the observation is
/// silently lost — strictly worse than not attributing it. That is safe only
/// because the runner is the canonical author of that table and self-heals it
/// with `CREATE TABLE IF NOT EXISTS project.apps` at pool init
/// (`crate::database::pg` pool bootstrap) on the very pool that runs this
/// statement. **If that self-heal is ever removed, this design must change**
/// to the `table_exists`-guarded form `validate_app_filter` uses in
/// `bin/qontinui_specs.rs`.
///
/// `RETURNING app_id` is what makes the reject case observable, which is why
/// the call below is `query_opt` and not `execute` — `execute` returns a row
/// count and would discard it.
const OBSERVATION_INSERT_SQL: &str = r#"INSERT INTO co_occurrence_observations
               (id, spec_id, runner_instance, fingerprints, snapshot_metadata, app_id)
               VALUES ($1::text::uuid, $2, $3, $4::jsonb, $5::jsonb,
                       (SELECT a.app_id FROM project.apps a WHERE a.app_id = $6))
               RETURNING app_id"#;

/// Enqueue a single observation derived from a UI Bridge snapshot.
///
/// Extracts a fingerprint per element, resolves the page label (see
/// [`resolve_page_label`]) and the producing app (see [`resolve_app_id`]),
/// records minimal page metadata, and inserts one row into
/// `co_occurrence_observations`. All errors downgrade to WARN logs — the
/// snapshot path must never fail because of observation capture.
///
/// Takes no `app_id` parameter on purpose: the id is read off the snapshot,
/// which keeps the call site's invariant that the caller carries no scope
/// knowledge the snapshot doesn't, and avoids threading a value through the
/// fire-and-forget `tokio::spawn` that invokes this.
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
    let app_id = resolve_app_id(snapshot);

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

    // See `OBSERVATION_INSERT_SQL` for why each cast is there and why the
    // `project.apps` subquery is safe. `query_opt` rather than `execute`
    // because `execute` returns a row count, which would discard the
    // `RETURNING app_id` that makes an unknown id observable.
    let res = conn
        .query_opt(
            OBSERVATION_INSERT_SQL,
            &[
                &id,
                &spec_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &runner_instance,
                &fingerprints_json,
                &snapshot_metadata,
                &app_id,
            ],
        )
        .await;

    match res {
        Err(e) => {
            warn!(
                "state_discovery::capture: failed to insert observation ({} elements): {}",
                element_count, e
            );
        }
        Ok(row) => {
            // The stamped id came back NULL while we supplied one: the app is
            // not registered in `project.apps`. The row is kept and recorded
            // un-attributed rather than dropped — it still carries
            // co-occurrence signal — but this is a producer/consumer key
            // disagreement, so say so loudly enough to diagnose.
            // `try_get`, not `get`: `get` panics on a type mismatch, and this
            // runs inside a fire-and-forget task whose whole contract is that
            // it cannot disturb the snapshot response.
            let stored: Option<String> = row
                .and_then(|r| r.try_get::<_, Option<String>>(0).ok())
                .flatten();
            if let (Some(candidate), None) = (app_id.as_deref(), stored.as_deref()) {
                warn!(
                    "state_discovery::capture: snapshot claimed appId {:?}, which is not \
                     registered in project.apps — observation recorded with app_id NULL \
                     (un-attributed). Register the app or fix the producer's id.",
                    candidate
                );
            }
        }
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

    // ---- app attribution -------------------------------------------------

    #[test]
    fn app_id_is_read_from_the_snapshot_top_level() {
        // The runner frontend's `runner-tabs` enricher stamps this from
        // `RUNNER_APP_ID`; the handler never passes it in.
        let snap = json!({ "appId": "qontinui-runner", "page": { "pathname": "/" } });
        assert_eq!(resolve_app_id(&snap).as_deref(), Some("qontinui-runner"));
    }

    #[test]
    fn absent_app_id_is_none_not_a_fabricated_default() {
        // A snapshot from a producer that does not stamp an id records
        // "app unknown". Defaulting to `qontinui-runner` here would be the
        // confidently-wrong inference this whole change exists to avoid.
        assert_eq!(resolve_app_id(&json!({})), None);
        assert_eq!(
            resolve_app_id(&json!({ "page": { "pathname": "/" } })),
            None
        );
    }

    #[test]
    fn blank_app_id_is_none() {
        // Mirrors `resolve_page_label`'s blank handling: an empty or
        // whitespace-only id is not a key, so it must not be sent to the
        // registry lookup as if it were one.
        assert_eq!(resolve_app_id(&json!({ "appId": "" })), None);
        assert_eq!(resolve_app_id(&json!({ "appId": "   " })), None);
    }

    #[test]
    fn non_string_app_id_is_none() {
        // The field is untyped on the wire; anything that is not a string
        // cannot be an app id.
        assert_eq!(resolve_app_id(&json!({ "appId": 42 })), None);
        assert_eq!(resolve_app_id(&json!({ "appId": null })), None);
        assert_eq!(
            resolve_app_id(&json!({ "appId": ["qontinui-runner"] })),
            None
        );
    }

    #[test]
    fn app_id_is_trimmed_so_padding_still_matches_the_registry() {
        // `project.apps.app_id` is matched by exact equality, so an untrimmed
        // value would collapse to NULL for a reason nobody could see.
        assert_eq!(
            resolve_app_id(&json!({ "appId": "  qontinui-runner  " })).as_deref(),
            Some("qontinui-runner")
        );
    }

    #[test]
    fn observation_insert_stamps_app_id_and_validates_it_in_one_statement() {
        // Shape guard, in the spirit of
        // `page_query_is_bounded_by_artifact_ids_not_by_time` in
        // `workflow_generation::spec_authoring`. There is no sqlx in this repo
        // — every statement is a runtime-parsed string, so a wrong column name
        // or arity compiles clean and fails only as a `warn!` on a
        // fire-and-forget path. This test is the only pre-PG verification of
        // this statement that exists.
        assert!(
            OBSERVATION_INSERT_SQL.contains(
                "(id, spec_id, runner_instance, fingerprints, snapshot_metadata, app_id)"
            ),
            "the six-column list must stay in sync with the six binds. Got: {OBSERVATION_INSERT_SQL}"
        );
        assert!(
            OBSERVATION_INSERT_SQL
                .contains("(SELECT a.app_id FROM project.apps a WHERE a.app_id = $6)"),
            "app_id must be validated against the registry inside this statement, so an \
             unknown id collapses to NULL instead of being written as fact. Got: \
             {OBSERVATION_INSERT_SQL}"
        );
        assert!(
            OBSERVATION_INSERT_SQL.contains("RETURNING app_id"),
            "without RETURNING, the collapsed-to-NULL case is invisible and the `query_opt` \
             call below has nothing to read"
        );
        // The pre-existing casts are load-bearing; see the const's doc comment.
        assert!(
            OBSERVATION_INSERT_SQL.contains("$1::text::uuid"),
            "tokio-postgres' uuid feature is not enabled — the id must round-trip as text"
        );
        assert!(
            OBSERVATION_INSERT_SQL.contains("$4::jsonb")
                && OBSERVATION_INSERT_SQL.contains("$5::jsonb"),
            "the serde payloads must land in JSONB columns, not text"
        );
        // $6 must NOT be cast: both sides of the lookup are already TEXT, and a
        // cast here would only obscure that.
        assert!(
            !OBSERVATION_INSERT_SQL.contains("$6::"),
            "project.apps.app_id and co_occurrence_observations.app_id are both TEXT — $6 \
             needs no cast"
        );
    }
}
