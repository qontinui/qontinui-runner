//! The two places an [`OsHealthSnapshot`] leaves the process: `/health`
//! `osHealth`, and the resource sample's host lane.
//!
//! Both read the cell and never the machine, so neither can be made to wait on
//! a kernel query or a spawn.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use super::{current, OsHealthSnapshot, OsPlatform, PoolTag};

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `/health` `osHealth`, read from the cell — never blocking on a fresh read.
///
/// Phase 5 adds `verdict` and `reasons` to this same object.
pub(crate) fn health_json() -> Value {
    health_json_from(OsPlatform::current(), current().as_deref())
}

/// [`health_json`] over an explicit platform and snapshot.
///
/// Before the first tick `readings` is `null` and a `note` says so: UNKNOWN,
/// never healthy. After it, each reading is a number or `null`; a `null` named
/// in `notApplicable` does not exist on this OS, any other `null` is UNKNOWN.
pub(crate) fn health_json_from(platform: OsPlatform, snap: Option<&OsHealthSnapshot>) -> Value {
    let Some(s) = snap else {
        return json!({
            "readings": null,
            "source": null,
            "readAt": null,
            "platform": platform.as_str(),
            "notApplicable": platform.not_applicable(),
            "note": "no os-health tick has completed in this process yet — UNKNOWN, never healthy",
        });
    };
    json!({
        "readings": {
            "kernelPagedBytes": s.kernel_paged_bytes,
            "kernelNonpagedBytes": s.kernel_nonpaged_bytes,
            "handleCount": s.kernel_handle_count,
            "tokenObjects": s.kernel_token_objects,
            "processObjects": s.kernel_process_objects,
            "processCount": s.os_process_count,
            "bootedAt": s.os_booted_at.map(rfc3339),
            "poolTopTags": s.kernel_pool_top_tags.as_ref().map(|tags| {
                tags.iter()
                    .map(|t| json!({
                        "tag": t.tag,
                        "bytes": t.bytes,
                        "outstandingAllocs": t.outstanding_allocs,
                    }))
                    .collect::<Vec<_>>()
            }),
            "poolTagsAt": s.kernel_pool_tags_at.map(rfc3339),
            "spawnProbe": s.spawn_probe.map(|p| json!({
                "p50Ms": p.p50_ms,
                "maxMs": p.max_ms,
                "count": p.count,
                "timeouts": p.timeouts,
                "errors": p.errors,
                "errorCode": p.error_code,
            })),
        },
        "source": s.kernel_source,
        "readAt": rfc3339(s.read_at),
        "platform": s.platform.as_str(),
        "notApplicable": s.platform.not_applicable(),
    })
}

/// The kernel readings as `POST /coord/devices/{device_id}/resource-sample`
/// carries them — flattened into `ResourceSample`, so the wire names below ARE
/// coord's `coord.device_resource_samples` column names (plan D2).
///
/// Every field is omitted when `None`, so an older coord sees no new key from a
/// runner that read nothing, and a row's NULL keeps meaning "never read / read
/// failed / not applicable per `kernel_source`" rather than a fabricated 0.
///
/// `bigint` columns are `i64`; `integer` columns are `i32`, and every narrowing
/// into one is CLAMPED (a wrapped count lands negative and grades `ok` against
/// every threshold). Timestamps are RFC 3339 strings.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct OsHealthSampleFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_paged_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_nonpaged_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_handle_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_token_objects: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_process_objects: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) os_process_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) os_booted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_pool_top_tags: Option<Vec<PoolTag>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_pool_tags_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_p50_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_max_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_timeouts: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_errors: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_probe_error_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel_source: Option<String>,
}

/// Saturate a count into an `integer` column.
fn clamp_i32_from_i64(v: i64) -> i32 {
    i32::try_from(v.max(0)).unwrap_or(i32::MAX)
}

/// Saturate a count into an `integer` column.
fn clamp_i32_from_u32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

impl OsHealthSampleFields {
    /// The sample fields for `snap`; all `None` before the first tick.
    pub(crate) fn from_snapshot(snap: Option<&OsHealthSnapshot>) -> Self {
        let Some(s) = snap else {
            return Self::default();
        };
        let p = s.spawn_probe;
        Self {
            kernel_paged_bytes: s.kernel_paged_bytes,
            kernel_nonpaged_bytes: s.kernel_nonpaged_bytes,
            kernel_handle_count: s.kernel_handle_count,
            kernel_token_objects: s.kernel_token_objects,
            kernel_process_objects: s.kernel_process_objects,
            os_process_count: s.os_process_count.map(clamp_i32_from_i64),
            os_booted_at: s.os_booted_at.map(rfc3339),
            kernel_pool_top_tags: s.kernel_pool_top_tags.clone(),
            kernel_pool_tags_at: s.kernel_pool_tags_at.map(rfc3339),
            spawn_probe_p50_ms: p.and_then(|p| p.p50_ms).map(clamp_i32_from_u32),
            spawn_probe_max_ms: p.and_then(|p| p.max_ms).map(clamp_i32_from_u32),
            spawn_probe_count: p.map(|p| clamp_i32_from_u32(p.count)),
            spawn_probe_timeouts: p.map(|p| clamp_i32_from_u32(p.timeouts)),
            spawn_probe_errors: p.map(|p| clamp_i32_from_u32(p.errors)),
            spawn_probe_error_code: p.and_then(|p| p.error_code),
            kernel_source: s.kernel_source.clone(),
        }
    }
}

/// The host lane's copy of the cached snapshot. Cheap — a lock and a clone —
/// so `collect_host_lane` pays nothing for it.
pub(crate) fn sample_fields() -> OsHealthSampleFields {
    OsHealthSampleFields::from_snapshot(current().as_deref())
}

#[cfg(test)]
mod tests {
    use super::super::incident_snapshot_for_tests;
    use super::*;

    #[test]
    fn health_json_is_unknown_before_the_first_tick() {
        let v = health_json_from(OsPlatform::Windows, None);
        assert_eq!(v["readings"], Value::Null);
        assert_eq!(v["source"], Value::Null);
        assert_eq!(v["readAt"], Value::Null);
        assert_eq!(v["platform"], "windows");
        assert!(v["note"].as_str().unwrap().contains("UNKNOWN"));
    }

    #[test]
    fn health_json_serves_the_readings_by_name() {
        let v = health_json_from(OsPlatform::Windows, Some(&incident_snapshot_for_tests()));
        let r = &v["readings"];
        assert_eq!(r["kernelPagedBytes"], 24_282_075_136_i64);
        assert_eq!(r["kernelNonpagedBytes"], 15_075_627_008_i64);
        assert_eq!(r["handleCount"], 406_585);
        assert_eq!(r["tokenObjects"], 4_365_962);
        assert_eq!(r["processObjects"], 2_930);
        assert_eq!(r["processCount"], 1_442);
        assert_eq!(r["bootedAt"], "2026-10-15T06:12:00Z");
        assert_eq!(r["poolTopTags"][0]["tag"], "File");
        assert_eq!(r["poolTopTags"][0]["bytes"], 9_693_669_888_i64);
        assert_eq!(r["poolTopTags"][0]["outstandingAllocs"], 24_238_128);
        assert_eq!(r["poolTopTags"][2]["tag"], "D2d ");
        assert_eq!(r["poolTagsAt"], "2026-10-19T08:20:52Z");
        assert_eq!(
            r["spawnProbe"],
            json!({
                "p50Ms": 2_400,
                "maxMs": 30_000,
                "count": 10,
                "timeouts": 1,
                "errors": 1,
                "errorCode": 1450,
            })
        );
        assert_eq!(v["source"], "windows:getperformanceinfo+objecttypes+pooltags");
        assert_eq!(v["readAt"], "2026-10-19T08:20:52Z");
        assert_eq!(v["notApplicable"], json!([]));
    }

    #[test]
    fn health_json_renders_unknown_as_null_never_zero() {
        let mut s = incident_snapshot_for_tests();
        s.kernel_token_objects = None;
        s.spawn_probe = None;
        s.kernel_pool_top_tags = None;
        let v = health_json_from(OsPlatform::Windows, Some(&s));
        assert_eq!(v["readings"]["tokenObjects"], Value::Null);
        assert_eq!(v["readings"]["spawnProbe"], Value::Null);
        assert_eq!(v["readings"]["poolTopTags"], Value::Null);
    }

    #[test]
    fn sample_fields_narrow_by_clamping() {
        assert_eq!(clamp_i32_from_i64(1_442), 1_442);
        assert_eq!(clamp_i32_from_i64(i64::MAX), i32::MAX);
        assert_eq!(clamp_i32_from_i64(-5), 0);
        assert_eq!(clamp_i32_from_u32(u32::MAX), i32::MAX);
        let mut s = incident_snapshot_for_tests();
        s.os_process_count = Some(i64::from(i32::MAX) + 10);
        let f = OsHealthSampleFields::from_snapshot(Some(&s));
        assert_eq!(f.os_process_count, Some(i32::MAX));
    }

    #[test]
    fn sample_fields_are_empty_before_the_first_tick() {
        assert_eq!(
            OsHealthSampleFields::from_snapshot(None),
            OsHealthSampleFields::default()
        );
        assert_eq!(
            serde_json::to_value(OsHealthSampleFields::default()).unwrap(),
            json!({})
        );
    }
}
