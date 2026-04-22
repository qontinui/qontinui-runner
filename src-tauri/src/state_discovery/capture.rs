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

use super::fingerprint::stable_element_fingerprint;

/// Sample rate for observation capture. K=1 captures every snapshot; set
/// higher to downsample if the observations table grows too fast. See the
/// "Observation volume" risk in the design doc — partitioning and
/// downsampling are the two levers we'll reach for first.
const SAMPLE_RATE: u32 = 1;

/// Enqueue a single observation derived from a UI Bridge snapshot.
///
/// Extracts a fingerprint per element, records minimal page metadata
/// (pathname, url, viewport, element_count), and inserts one row into
/// `co_occurrence_observations`. All errors downgrade to WARN logs — the
/// snapshot path must never fail because of observation capture.
pub async fn enqueue_observation(
    pg_db: Arc<PgDb>,
    snapshot: &serde_json::Value,
    spec_id: Option<String>,
    runner_instance: String,
) {
    // Sample-rate gate. SAMPLE_RATE == 1 means every call goes through;
    // K > 1 is the planned lever for volume management.
    if SAMPLE_RATE > 1 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        if n % SAMPLE_RATE != 0 {
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

    let snapshot_metadata = serde_json::json!({
        "pathname": pathname,
        "url": url,
        "viewport": viewport,
        "element_count": element_count,
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

    // Cast $1 to uuid (the runner's tokio-postgres lacks with-uuid, so we
    // pass the id as text and let PG parse). Cast $4/$5 to jsonb so the
    // serde_json payload lands in JSONB columns rather than text.
    let res = conn
        .execute(
            r#"INSERT INTO co_occurrence_observations
               (id, spec_id, runner_instance, fingerprints, snapshot_metadata)
               VALUES ($1::uuid, $2, $3, $4::jsonb, $5::jsonb)"#,
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
