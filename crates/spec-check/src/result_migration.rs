//! Result schema-version forward migration. Step 9 of Plan 02.
//!
//! `SpecCheckResult.result_schema_version` is the first field on the wire;
//! the current version is [`crate::evaluator::RESULT_SCHEMA_VERSION`].
//! `#[serde(default)]` on the field means a missing value deserializes as 0
//! ("pre-versioned" — never written intentionally, but reachable for legacy
//! / test inputs).
//!
//! Versions:
//! - **v1** — assertion verdicts are `pass` / `fail`.
//! - **v2** — a verdict may also be `{ "status": "unknown", "code": … }`
//!   (plan `2026-11-06-no-verification-instrument-reads-an-absence-as-a-pass`
//!   D4). Nothing else changed, so every v1 row is already a valid v2 row and
//!   `migrate_v1_to_v2` only restamps the version. A build predating v2
//!   that reads through this shim refuses a v2 row with
//!   [`MigrationError::UnsupportedVersion`]; a reader that deserializes the
//!   JSONB directly (no production reader calls this shim today) fails a
//!   row carrying an `unknown` verdict with a deserialize error instead.
//!
//! Each schema bump adds a `migrate_v(N)_to_v(N+1)` helper here. Persisted
//! JSONB is never rewritten in place — all migration runs on read.

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
/// - missing or `0` → `migrate_v0_to_v1` → `migrate_v1_to_v2`.
/// - `1` → `migrate_v1_to_v2` (identity transform stamping
///   `resultSchemaVersion = 2`).
/// - `2` → pass through unchanged.
/// - any other → `MigrationError::UnsupportedVersion(N)`.
pub fn deserialize_result_with_migration(
    value: serde_json::Value,
) -> Result<SpecCheckResult, MigrationError> {
    let version = value.get(VERSION_KEY).and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let migrated = match version {
        0 => migrate_v1_to_v2(migrate_v0_to_v1(value)),
        1 => migrate_v1_to_v2(value),
        2 => value,
        n => return Err(MigrationError::UnsupportedVersion(n)),
    };

    serde_json::from_value(migrated).map_err(MigrationError::Deserialize)
}

/// v0 → v1: identity transform. v1 is the first version; the only work is
/// stamping the field so downstream consumers see a consistent shape.
fn migrate_v0_to_v1(value: serde_json::Value) -> serde_json::Value {
    stamp_version(value, 1)
}

/// v1 → v2: identity transform. v2 only added the `unknown` verdict, so a v1
/// row (whose verdicts are all `pass` / `fail`) is already a valid v2 row;
/// the only work is stamping the version.
fn migrate_v1_to_v2(value: serde_json::Value) -> serde_json::Value {
    stamp_version(value, 2)
}

fn stamp_version(mut value: serde_json::Value, version: u32) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert(VERSION_KEY.to_string(), serde_json::Value::from(version));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssertionSeverityCounts, BridgeFingerprint, MatchOutcome, MissReason, SpecAssertionVerdict,
        SpecCheckResult, SpecCheckSummary, ThresholdConfig, UnknownCode,
    };

    /// The version this build writes and every older row migrates to.
    const CURRENT: u32 = crate::evaluator::RESULT_SCHEMA_VERSION;

    fn sample_result(version: u32) -> SpecCheckResult {
        SpecCheckResult {
            result_schema_version: version,
            snapshot_id: "scs_test".to_string(),
            snapshot_sha256: None,
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
            classification: ThresholdConfig::default().classify_match_rate(1.0),
            thresholds_used: ThresholdConfig::default(),
        }
    }

    /// A stored row at `version` holding one state whose assertions carry
    /// `outcomes`, written as the wire JSON a JSONB column holds.
    fn row(version: u32, outcomes: &[serde_json::Value]) -> serde_json::Value {
        let assertions: Vec<serde_json::Value> = outcomes
            .iter()
            .enumerate()
            .map(|(i, outcome)| {
                serde_json::json!({
                    "assertionId": format!("a{i}"),
                    "description": "d",
                    "severity": "critical",
                    "category": "a11y",
                    "outcome": outcome,
                })
            })
            .collect();
        let state = serde_json::json!({
            "stateId": "s1",
            "stateName": "Loaded",
            "matchRate": 0.5,
            "classification": "red",
            "assertions": assertions,
        });
        let result = sample_result(version);
        let mut value = serde_json::to_value(result).expect("serialize");
        let obj = value.as_object_mut().unwrap();
        assert_eq!(obj["resultSchemaVersion"], version, "fixture version");
        obj.insert("stateResults".to_string(), serde_json::json!([state]));
        value
    }

    /// Mutation: leaving `RESULT_SCHEMA_VERSION` at 1 (the `unknown` verdict
    /// on a v1 wire) reds the version assertion.
    #[test]
    fn current_version_round_trip_returns_equivalent_result() {
        assert_eq!(CURRENT, 2, "v2 added the unknown verdict");
        let original = sample_result(CURRENT);
        let value = serde_json::to_value(&original).expect("serialize");
        let restored = deserialize_result_with_migration(value).expect("migrate");

        // Field-by-field equivalence — SpecCheckResult does not derive PartialEq.
        assert_eq!(restored.result_schema_version, CURRENT);
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

    /// A row stored at v1 still loads: v2 only added the `unknown` verdict,
    /// so its `pass` / `fail` verdicts read unchanged and only the version
    /// is restamped. Mutations: dropping the `1` arm (the row then answers
    /// `UnsupportedVersion(1)`) or skipping the v1 → v2 stamp reds this.
    #[test]
    fn stored_v1_row_still_loads() {
        let outcomes = [
            serde_json::json!({ "status": "pass", "matched": { "path": "#save" } }),
            serde_json::json!({ "status": "fail", "miss": { "reason": "no_candidates" } }),
        ];
        let value = row(1, &outcomes);
        let restored = deserialize_result_with_migration(value).expect("v1 loads");
        assert_eq!(restored.result_schema_version, CURRENT);

        let assertions = &restored.state_results[0].assertions;
        match &assertions[0].outcome {
            SpecAssertionVerdict::Pass { matched } => assert_eq!(matched.path, "#save"),
            other => panic!("expected pass, got {other:?}"),
        }
        match &assertions[1].outcome {
            SpecAssertionVerdict::Fail { miss } => {
                assert_eq!(miss.reason, MissReason::NoCandidates)
            }
            other => panic!("expected fail, got {other:?}"),
        }
    }

    /// A v2 row carrying an `unknown` verdict loads with its code intact.
    /// Mutation: dropping the `2` arm (the row then answers
    /// `UnsupportedVersion(2)`) reds this.
    #[test]
    fn v2_row_with_unknown_verdict_loads() {
        let unknown = serde_json::json!({ "status": "unknown", "code": "no_expectation" });
        let value = row(2, &[unknown]);
        let restored = deserialize_result_with_migration(value).expect("v2 loads");
        assert_eq!(restored.result_schema_version, 2);
        let outcome = &restored.state_results[0].assertions[0].outcome;
        assert!(
            matches!(
                outcome,
                SpecAssertionVerdict::Unknown {
                    code: UnknownCode::NoExpectation
                }
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn missing_version_field_migrates_to_current() {
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
            restored.result_schema_version, CURRENT,
            "migration must stamp the current resultSchemaVersion explicitly"
        );
    }

    #[test]
    fn explicit_version_zero_migrates_to_current() {
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        value.as_object_mut().unwrap().insert(
            "resultSchemaVersion".to_string(),
            serde_json::Value::from(0_u32),
        );

        let restored = deserialize_result_with_migration(value).expect("migrate");
        assert_eq!(
            restored.result_schema_version, CURRENT,
            "v0 → v1 → v2 must stamp the result with the current version"
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
    fn migration_path_returns_current_version_explicitly() {
        // A v0 blob lacking the version field — the migration must produce a
        // result whose result_schema_version is the current one, not 0.
        let mut value = serde_json::to_value(sample_result(1)).expect("serialize");
        value.as_object_mut().unwrap().remove("resultSchemaVersion");

        let restored = deserialize_result_with_migration(value).expect("migrate");
        assert_eq!(restored.result_schema_version, CURRENT);
    }
}
