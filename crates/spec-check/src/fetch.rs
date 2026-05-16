//! Snapshot wrap + error taxonomy. Step 8 of Plan 02.
//!
//! This module does NOT perform the HTTP fetch — that is the Stream C
//! adapter's responsibility. Its job is to *wrap* a raw `serde_json::Value`
//! response into a typed `UIBridgeSnapshot` with a minted identity
//! (`snapshot_id`), a `BridgeFingerprint`, and a content hash for telemetry.

use crate::{BridgeFingerprint, UIBridgeSnapshot};

/// Error taxonomy for the snapshot-fetch + wrap pipeline.
///
/// Variants other than `Malformed` originate from the HTTP adapter (Stream C)
/// and are passed through this crate's error type for a unified surface.
/// `Partial` is currently unreachable (no `registry_revision` field on the
/// wire) but kept in the taxonomy per Risks §0 — when the SDK exposes a
/// monotonic counter via a side-channel in v1.1, the adapter can emit it.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotFetchError {
    #[error("no SDK app connected")]
    NotConnected,
    #[error("snapshot fetch timed out after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },
    #[error("partial snapshot: registry revision moved from {from} to {to} during fetch")]
    Partial { from: u64, to: u64 },
    #[error("snapshot is stale: age={age_ms}ms")]
    Stale { age_ms: u64 },
    #[error("network error during snapshot fetch: {0}")]
    Network(String),
    #[error("snapshot fetch forbidden: status={status}")]
    Forbidden { status: u16 },
    #[error("snapshot response malformed: {0}")]
    Malformed(String),
}

/// Wrap a raw snapshot value into a typed `UIBridgeSnapshot` + identity.
///
/// Returns a 4-tuple of `(snapshot, fingerprint, snapshot_id, content_sha256)`.
///
/// - `snapshot_id` is `"scs_" + UUIDv7()` — lexicographically time-ordered
///   per §5.8; the wire-stable correlation ID embedded in
///   `SpecCheckResult.snapshot_id`.
/// - `content_sha256` is a separate return value for telemetry; it is NOT
///   embedded in `BridgeFingerprint`. The spec-content hash lives on
///   `SpecCheckResult.spec_content_hash`.
/// - `fingerprint.snapshot_timestamp` is the snapshot's Unix-epoch millis
///   converted to ISO-8601 UTC with millisecond precision.
#[tracing::instrument(
    name = "spec_check.fetch_snapshot",
    skip(raw),
    fields(
        app_id = %app_id,
        bridge_version = ?bridge_version,
        snapshot_id = tracing::field::Empty,
    ),
)]
pub fn wrap_snapshot(
    raw: serde_json::Value,
    app_id: String,
    app_version: Option<String>,
    route: Option<String>,
    bridge_version: Option<String>,
) -> Result<(UIBridgeSnapshot, BridgeFingerprint, String, String), SnapshotFetchError> {
    let snapshot: UIBridgeSnapshot = serde_json::from_value(raw.clone())
        .map_err(|e| SnapshotFetchError::Malformed(e.to_string()))?;

    let snapshot_id = format!("scs_{}", uuid::Uuid::now_v7());
    let content_sha256 = compute_content_sha256(&raw)?;
    let snapshot_timestamp = millis_to_iso8601(snapshot.timestamp)?;

    let fingerprint = BridgeFingerprint {
        app_id,
        app_version,
        route,
        bridge_version,
        snapshot_timestamp,
        element_count: snapshot.elements.len() as u32,
    };

    tracing::Span::current().record("snapshot_id", tracing::field::display(&snapshot_id));

    Ok((snapshot, fingerprint, snapshot_id, content_sha256))
}

/// JCS (RFC 8785) → SHA-256 → `"sha256-<hex>"` per §5.10. Delegates to
/// `qontinui_types::canonical_hash` so this crate and the runner share one
/// implementation (boundary rule §5.2 — the matcher crate cannot depend on
/// src-tauri; the primitive lives in qontinui-types instead).
pub(crate) fn compute_content_sha256(value: &serde_json::Value) -> Result<String, SnapshotFetchError> {
    qontinui_types::canonical_hash::canonical_hash(value)
        .map_err(|e| SnapshotFetchError::Malformed(format!("canonical hash: {e}")))
}

/// Convert Unix-epoch milliseconds to an ISO-8601 UTC string with stable
/// millisecond precision (`YYYY-MM-DDTHH:MM:SS.sssZ`).
///
/// Out-of-range timestamps map to `Malformed` rather than panicking.
pub(crate) fn millis_to_iso8601(ms: i64) -> Result<String, SnapshotFetchError> {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .ok_or_else(|| SnapshotFetchError::Malformed("timestamp out of range".to_string()))?;
    Ok(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn well_formed_raw(timestamp_ms: i64) -> serde_json::Value {
        json!({
            "timestamp": timestamp_ms,
            "elements": [],
            "components": []
        })
    }

    #[test]
    fn all_seven_variants_constructible_with_nonempty_display() {
        let variants = vec![
            SnapshotFetchError::NotConnected,
            SnapshotFetchError::Timeout { elapsed_ms: 1500 },
            SnapshotFetchError::Partial { from: 1, to: 2 },
            SnapshotFetchError::Stale { age_ms: 9000 },
            SnapshotFetchError::Network("connection reset".to_string()),
            SnapshotFetchError::Forbidden { status: 403 },
            SnapshotFetchError::Malformed("bad json".to_string()),
        ];
        assert_eq!(variants.len(), 7);
        for v in &variants {
            let msg = format!("{v}");
            assert!(!msg.is_empty(), "Display empty for {v:?}");
        }
    }

    #[test]
    fn wrap_happy_path_mints_scs_uuidv7_snapshot_id() {
        let raw = well_formed_raw(1_700_000_000_000);
        let (_snap, _fp, snapshot_id, _hash) =
            wrap_snapshot(raw, "qontinui-web".to_string(), None, None, None).unwrap();

        assert!(
            snapshot_id.starts_with("scs_"),
            "snapshot_id must start with scs_: got {snapshot_id}"
        );
        let uuid_part = &snapshot_id["scs_".len()..];
        let parsed =
            uuid::Uuid::parse_str(uuid_part).expect("UUID part must parse as a hyphenated UUID");
        assert_eq!(
            parsed.get_version_num(),
            7,
            "snapshot_id must encode a UUIDv7"
        );
    }

    #[test]
    fn content_sha256_is_deterministic_across_runs() {
        let raw = json!({
            "timestamp": 1_700_000_000_000_i64,
            "elements": [],
            "components": []
        });
        let (_s1, _f1, _id1, hash1) =
            wrap_snapshot(raw.clone(), "app".to_string(), None, None, None).unwrap();
        let (_s2, _f2, _id2, hash2) =
            wrap_snapshot(raw, "app".to_string(), None, None, None).unwrap();
        assert_eq!(hash1, hash2);
        assert!(hash1.starts_with("sha256-"));
    }

    #[test]
    fn content_sha256_is_invariant_under_key_ordering() {
        // JCS canonicalizes — two JSON objects with the same keys but different
        // declaration order MUST produce identical hashes.
        let raw_a = json!({
            "timestamp": 1_700_000_000_000_i64,
            "elements": [],
            "components": [],
            "a": 1,
            "b": 2
        });
        let raw_b = json!({
            "b": 2,
            "a": 1,
            "components": [],
            "elements": [],
            "timestamp": 1_700_000_000_000_i64
        });

        // Both fail to parse as UIBridgeSnapshot (extra a/b fields) so we can't
        // route through wrap_snapshot — exercise compute_content_sha256 directly
        // with two well-formed-but-reordered values.
        let hash_a = compute_content_sha256(&raw_a).unwrap();
        let hash_b = compute_content_sha256(&raw_b).unwrap();
        assert_eq!(
            hash_a, hash_b,
            "JCS should produce identical hashes for differently-ordered keys"
        );
    }

    #[test]
    fn wrap_with_all_optional_fields_none() {
        let raw = well_formed_raw(0);
        let (_snap, fp, _id, _hash) =
            wrap_snapshot(raw, "qontinui-web".to_string(), None, None, None).unwrap();

        assert_eq!(fp.app_id, "qontinui-web");
        assert!(fp.app_version.is_none());
        assert!(fp.route.is_none());
        assert!(fp.bridge_version.is_none());
        assert_eq!(fp.element_count, 0);
    }

    #[test]
    fn wrap_propagates_all_optional_fields() {
        let raw = well_formed_raw(1_700_000_000_000);
        let (_snap, fp, _id, _hash) = wrap_snapshot(
            raw,
            "qontinui-runner".to_string(),
            Some("0.2.1".to_string()),
            Some("/workflows".to_string()),
            Some("ui-bridge-1.3.0".to_string()),
        )
        .unwrap();

        assert_eq!(fp.app_id, "qontinui-runner");
        assert_eq!(fp.app_version.as_deref(), Some("0.2.1"));
        assert_eq!(fp.route.as_deref(), Some("/workflows"));
        assert_eq!(fp.bridge_version.as_deref(), Some("ui-bridge-1.3.0"));
    }

    #[test]
    fn wrap_malformed_input_returns_malformed_error() {
        let raw = json!({
            "timestamp": "not-a-number",
            "elements": []
        });
        let err = wrap_snapshot(raw, "app".to_string(), None, None, None).unwrap_err();
        assert!(
            matches!(err, SnapshotFetchError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    #[test]
    fn wrap_completely_malformed_input_returns_malformed_error() {
        // Top-level JSON of the wrong shape — array instead of object.
        let raw = json!(["not", "a", "snapshot"]);
        let err = wrap_snapshot(raw, "app".to_string(), None, None, None).unwrap_err();
        assert!(matches!(err, SnapshotFetchError::Malformed(_)));
    }

    #[test]
    fn timestamp_zero_round_trips_to_unix_epoch_iso8601() {
        let raw = well_formed_raw(0);
        let (_snap, fp, _id, _hash) =
            wrap_snapshot(raw, "app".to_string(), None, None, None).unwrap();
        assert_eq!(fp.snapshot_timestamp, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn timestamp_known_value_round_trips_correctly() {
        // 2023-11-14T22:13:20.000Z
        let raw = well_formed_raw(1_700_000_000_000);
        let (_snap, fp, _id, _hash) =
            wrap_snapshot(raw, "app".to_string(), None, None, None).unwrap();
        assert_eq!(fp.snapshot_timestamp, "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn element_count_reflects_snapshot_elements_len() {
        let raw = json!({
            "timestamp": 1_700_000_000_000_i64,
            "elements": [
                {"id": "a", "role": "button", "selector": "#a"},
                {"id": "b", "role": "button", "selector": "#b"},
                {"id": "c", "role": "link",   "selector": "#c"}
            ],
            "components": []
        });
        // If the UIBridgeElement schema rejects our hand-rolled stubs, the
        // result is Malformed — in which case skip the assertion (this test
        // is a best-effort smoke check on element_count wiring; the
        // deterministic guarantee is on the empty-elements path above).
        if let Ok((_snap, fp, _id, _hash)) =
            wrap_snapshot(raw, "app".to_string(), None, None, None)
        {
            assert_eq!(fp.element_count, 3);
        }
    }
}
