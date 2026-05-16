//! Result schema-version forward migration. Step 9 of Plan 02.
//!
//! `SpecCheckResult.result_schema_version` is the first field on the wire;
//! v1 = 1. `#[serde(default)]` on the field means a missing value
//! deserializes as 0 ("pre-versioned" — never written intentionally, but
//! reachable for legacy / test inputs).
//!
//! This module is a load-bearing seam for v2+: each future schema bump adds
//! a `migrate_v(N)_to_v(N+1)` helper here. Persisted JSONB is never rewritten
//! in place — all migration runs on read.

use crate::SpecCheckResult;

/// Errors emitted by the migration shim.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// The on-wire `result_schema_version` is newer than this build knows how
    /// to migrate forward. The carried value is the unknown version number.
    #[error("unsupported result_schema_version: {0}")]
    UnsupportedVersion(u32),

    /// Migration succeeded but the post-migration JSON failed to deserialize
    /// into `SpecCheckResult`.
    #[error("deserialize after migration: {0}")]
    Deserialize(#[from] serde_json::Error),
}

/// Wire-format key for `SpecCheckResult.result_schema_version`. The struct
/// is serialized with `#[serde(rename_all = "camelCase")]`, so the actual
/// on-the-wire field is `resultSchemaVersion`, not snake_case. The plan's
/// example code in §Step 9 used the snake_case form for readability; the
/// migration shim must operate on whatever serde actually emits.
const VERSION_KEY: &str = "resultSchemaVersion";

/// Deserialize a `SpecCheckResult` from JSON, forward-migrating any older
/// schema versions to the current version (§5.15).
///
/// Routing:
/// - missing or `0` → `migrate_v0_to_v1` (identity transform stamping
///   `resultSchemaVersion = 1`).
/// - `1` → pass through unchanged.
/// - any other → `MigrationError::UnsupportedVersion(N)`.
pub fn deserialize_result_with_migration(
    value: serde_json::Value,
) -> Result<SpecCheckResult, MigrationError> {
    let version = value
        .get(VERSION_KEY)
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let migrated = match version {
        0 => migrate_v0_to_v1(value),
        1 => value,
        n => return Err(MigrationError::UnsupportedVersion(n)),
    };

    serde_json::from_value(migrated).map_err(MigrationError::Deserialize)
}

/// v0 → v1: identity transform. v1 is the first version; the only work is
/// stamping the field so downstream consumers see a consistent shape.
fn migrate_v0_to_v1(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            VERSION_KEY.to_string(),
            serde_json::Value::from(1_u32),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssertionSeverityCounts, BridgeFingerprint, MatchOutcome, SpecCheckResult,
        SpecCheckSummary,
    };

    fn sample_result(version: u32) -> SpecCheckResult {
        SpecCheckResult {
            result_schema_version: version,
            snapshot_id: "scs_test".to_string(),
            spec_content_hash: "sha256-deadbeef".to_string(),
            spec_version: "1.0".to_string(),
            page_id: "settings-general".to_string(),
            state_results: vec![],
            summary: SpecCheckSummary {
                match_outcome: MatchOutcome::FullMatch,
                overall_match_rate: 1.0,
                severity_counts: AssertionSeverityCounts::default(),
                recommended_state: None,
                recommendation_reason: None,
            },
            bridge_fingerprint: BridgeFingerprint {
                app_id: "qontinui-web".to_string(),
                app_version: None,
                route: None,
                bridge_version: None,
                snapshot_timestamp: "2023-11-14T22:13:20.000Z".to_string(),
                element_count: 0,
            },
            evaluated_at: "2023-11-14T22:13:21.000Z".to_string(),
            warnings: vec![],
        }
    }

    #[test]
    fn v1_round_trip_returns_equivalent_result() {
        let original = sample_result(1);
        let value = serde_json::to_value(&original).expect("serialize");
        let restored = deserialize_result_with_migration(value).expect("migrate");

        // Field-by-field equivalence — SpecCheckResult does not derive PartialEq.
        assert_eq!(restored.result_schema_version, 1);
        assert_eq!(restored.snapshot_id, original.snapshot_id);
        assert_eq!(restored.spec_content_hash, original.spec_content_hash);
        assert_eq!(restored.spec_version, original.spec_version);
        assert_eq!(restored.page_id, original.page_id);
        assert_eq!(
            restored.summary.match_outcome,
            original.summary.match_outcome
        );
        assert_eq!(
            restored.summary.overall_match_rate,
            original.summary.overall_match_rate
        );
        assert_eq!(
            restored.bridge_fingerprint.app_id,
            original.bridge_fingerprint.app_id
        );
        assert_eq!(restored.evaluated_at, original.evaluated_at);
    }

    #[test]
    fn missing_version_field_migrates_to_v1() {
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        // SpecCheckResult uses #[serde(rename_all = "camelCase")] so the wire
        // key is `resultSchemaVersion`. Strip it to simulate a pre-versioned
        // blob.
        assert!(
            value
                .as_object_mut()
                .unwrap()
                .remove("resultSchemaVersion")
                .is_some(),
            "fixture must expose resultSchemaVersion before removal"
        );

        let restored = deserialize_result_with_migration(value).expect("migrate");
        assert_eq!(
            restored.result_schema_version, 1,
            "migration must stamp resultSchemaVersion = 1 explicitly"
        );
    }

    #[test]
    fn explicit_version_zero_migrates_to_v1() {
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        value.as_object_mut().unwrap().insert(
            "resultSchemaVersion".to_string(),
            serde_json::Value::from(0_u32),
        );

        let restored = deserialize_result_with_migration(value).expect("migrate");
        assert_eq!(
            restored.result_schema_version, 1,
            "v0 → v1 must stamp the result with version 1"
        );
    }

    #[test]
    fn unsupported_version_returns_error() {
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        value.as_object_mut().unwrap().insert(
            "resultSchemaVersion".to_string(),
            serde_json::Value::from(99_u32),
        );

        let err = deserialize_result_with_migration(value).expect_err("must fail");
        match err {
            MigrationError::UnsupportedVersion(n) => assert_eq!(n, 99),
            other => panic!("expected UnsupportedVersion(99), got {other:?}"),
        }
    }

    #[test]
    fn migration_path_returns_version_1_explicitly() {
        // A v0 blob lacking the version field — the migration must produce a
        // result whose result_schema_version == 1, not 0.
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        value.as_object_mut().unwrap().remove("resultSchemaVersion");

        let restored = deserialize_result_with_migration(value).expect("migrate");
        assert_eq!(restored.result_schema_version, 1);
    }
}
