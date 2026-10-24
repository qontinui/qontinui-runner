//! The places an [`OsHealthSnapshot`] leaves the process: `/health`
//! `osHealth` (also served to the App-root banner by the `os_health_get`
//! command and the `os-health-changed` event), and the resource sample's host
//! lane.
//!
//! Both read the cell and never the machine, so neither can be made to wait on
//! a kernel query or a spawn.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use super::verdict::{current_verdict_at, OsVerdict};
use super::{current, injected, is_stale, OsHealthSnapshot, OsPlatform, PoolTag, STALE_AFTER_SECS};

/// How long `verdict = breach` must hold before the App-root banner shows
/// (plan D5). The UI reads it from `osHealth.bannerDwellSecs`.
pub(crate) const BANNER_DWELL_SECS: u64 = 600;

/// DEBUG AND TEST BUILDS ONLY: `QONTINUI_OS_HEALTH_BANNER_DWELL_SECS` shortens
/// the dwell for a live check of the banner (see `os_health::INJECT_ENV`). A
/// release build never reads it and always serves [`BANNER_DWELL_SECS`].
#[cfg(any(debug_assertions, test))]
pub(crate) const BANNER_DWELL_ENV: &str = "QONTINUI_OS_HEALTH_BANNER_DWELL_SECS";

#[cfg(any(debug_assertions, test))]
fn banner_dwell_secs() -> u64 {
    std::env::var(BANNER_DWELL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(BANNER_DWELL_SECS)
}

#[cfg(not(any(debug_assertions, test)))]
fn banner_dwell_secs() -> u64 {
    BANNER_DWELL_SECS
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `/health` `osHealth`, read from the cell — never blocking on a fresh read —
/// with the local verdict (plan D5). It does NOT feed `derived_status`: that
/// says whether THIS RUNNER is healthy, and a runner restart cannot fix a
/// kernel leak.
pub(crate) fn health_json() -> Value {
    let platform = OsPlatform::current();
    let snap = current();
    let now = Utc::now();
    let verdict = current_verdict_at(platform, snap.as_deref(), now);
    let mut v = health_json_from(platform, snap.as_deref(), &verdict, now);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("injected".into(), Value::Bool(injected()));
        obj.insert("bannerDwellSecs".into(), json!(banner_dwell_secs()));
    }
    v
}

/// [`health_json`] over an explicit platform, snapshot, verdict and clock:
/// the readings ([`readings_json`]) with the verdict's `verdict`, `reasons`,
/// `axes`, `tokenSlopePerHour` and `thresholds` beside them.
pub(crate) fn health_json_from(
    platform: OsPlatform,
    snap: Option<&OsHealthSnapshot>,
    verdict: &OsVerdict,
    now: DateTime<Utc>,
) -> Value {
    let mut v = readings_json(platform, snap, now);
    if let (Some(obj), Value::Object(fields)) = (v.as_object_mut(), verdict.json_fields()) {
        obj.extend(fields);
    }
    v
}

/// The readings half of `osHealth`, over an explicit platform, snapshot and
/// clock.
///
/// Before the first tick `readings` is `null` and a `note` says so: UNKNOWN,
/// never healthy. After it, each reading is a number or `null`; a `null` named
/// in `notApplicable` does not exist on this OS, any other `null` is UNKNOWN.
///
/// `ageSecs` is how old the readings are at `now`, and `stale` is true once
/// that passes `staleAfterSecs` — the `os-health` thread has stopped storing
/// (died, panicking every tick, or wedged). The readings are still shown so an
/// operator can see the last thing it measured, but they are not current, and
/// the resource sample stops publishing them.
fn readings_json(
    platform: OsPlatform,
    snap: Option<&OsHealthSnapshot>,
    now: DateTime<Utc>,
) -> Value {
    let Some(s) = snap else {
        return json!({
            "readings": null,
            "source": null,
            "readAt": null,
            "ageSecs": null,
            "stale": null,
            "staleAfterSecs": STALE_AFTER_SECS,
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
        "ageSecs": (now - s.read_at).num_seconds(),
        "stale": is_stale(s, now),
        "staleAfterSecs": STALE_AFTER_SECS,
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
///
/// Empty while a debug build serves an INJECTED snapshot
/// (`os_health::INJECT_ENV`): a faked reading must never reach coord.
pub(crate) fn sample_fields() -> OsHealthSampleFields {
    if injected() {
        return OsHealthSampleFields::default();
    }
    sample_fields_at(current().as_deref(), Utc::now())
}

/// [`sample_fields`] over an explicit snapshot and clock.
///
/// **A stale snapshot publishes NOTHING.** Every resource sample is stamped
/// with its own `sampled_at`, so republishing a reading the `os-health` thread
/// took ten minutes ago would tell coord the box measured it now — and a box
/// whose collector has died would keep "measuring" its last numbers forever.
/// Past [`STALE_AFTER_SECS`] the kernel columns are omitted (NULL, UNKNOWN),
/// which coord holds on rather than grades.
pub(crate) fn sample_fields_at(
    snap: Option<&OsHealthSnapshot>,
    now: DateTime<Utc>,
) -> OsHealthSampleFields {
    match snap {
        Some(s) if is_stale(s, now) => {
            tracing::debug!(
                "os-health: the cached snapshot is {} s old (> {STALE_AFTER_SECS} s) — the \
                 resource sample omits the kernel readings",
                (now - s.read_at).num_seconds()
            );
            OsHealthSampleFields::default()
        }
        other => OsHealthSampleFields::from_snapshot(other),
    }
}

#[cfg(test)]
mod tests {
    use super::super::verdict::{grade, Slope, Thresholds};
    use super::super::{incident_snapshot_for_tests, store_for_tests};
    use super::*;

    /// `health_json_from` at `now`, graded with the defaults and no slope.
    fn json_at(platform: OsPlatform, snap: Option<&OsHealthSnapshot>, now: DateTime<Utc>) -> Value {
        let v = grade(
            platform,
            snap,
            Slope::Unknown("test"),
            Thresholds::DEFAULT,
            now,
        );
        health_json_from(platform, snap, &v, now)
    }

    /// [`json_at`] at the snapshot's own read time (never stale).
    fn json_of(platform: OsPlatform, snap: Option<&OsHealthSnapshot>) -> Value {
        let now = snap.map_or_else(Utc::now, |s| s.read_at);
        json_at(platform, snap, now)
    }

    #[test]
    fn health_json_is_unknown_before_the_first_tick() {
        let v = json_of(OsPlatform::Windows, None);
        assert_eq!(v["readings"], Value::Null);
        assert_eq!(v["source"], Value::Null);
        assert_eq!(v["readAt"], Value::Null);
        assert_eq!(v["platform"], "windows");
        assert!(v["note"].as_str().unwrap().contains("UNKNOWN"));
    }

    #[test]
    fn health_json_serves_the_readings_by_name() {
        let v = json_of(OsPlatform::Windows, Some(&incident_snapshot_for_tests()));
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
        assert_eq!(
            v["source"],
            "windows:getperformanceinfo+objecttypes+pooltags+spawn-probe"
        );
        assert_eq!(v["ageSecs"], 0);
        assert_eq!(v["stale"], false);
        assert_eq!(v["readAt"], "2026-10-19T08:20:52Z");
        assert_eq!(v["notApplicable"], json!([]));
    }

    #[test]
    fn health_json_renders_unknown_as_null_never_zero() {
        let mut s = incident_snapshot_for_tests();
        s.kernel_token_objects = None;
        s.spawn_probe = None;
        s.kernel_pool_top_tags = None;
        let v = json_of(OsPlatform::Windows, Some(&s));
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

    /// A snapshot read ten minutes ago is NOT republished as a fresh sample,
    /// and `/health` says it is stale and how old.
    #[test]
    fn a_ten_minute_old_snapshot_is_not_republished_and_reads_stale() {
        let s = incident_snapshot_for_tests();
        let now = s.read_at + chrono::Duration::minutes(10);
        assert_eq!(
            sample_fields_at(Some(&s), now),
            OsHealthSampleFields::default()
        );
        assert_eq!(
            serde_json::to_value(sample_fields_at(Some(&s), now)).unwrap(),
            json!({}),
            "not one kernel key on the wire"
        );
        let v = json_at(OsPlatform::Windows, Some(&s), now);
        assert_eq!(v["stale"], true);
        assert_eq!(
            v["verdict"], "unknown",
            "a stale breach is not a current breach"
        );
        assert_eq!(v["ageSecs"], 600);
        assert_eq!(v["staleAfterSecs"], STALE_AFTER_SECS);
        assert_eq!(
            v["readings"]["tokenObjects"], 4_365_962,
            "still shown, flagged stale"
        );

        // Inside the bound it is published as read.
        let fresh = s.read_at + chrono::Duration::seconds(STALE_AFTER_SECS);
        assert_eq!(
            sample_fields_at(Some(&s), fresh),
            OsHealthSampleFields::from_snapshot(Some(&s))
        );
        assert_eq!(
            json_at(OsPlatform::Windows, Some(&s), fresh)["stale"],
            false
        );
        assert_eq!(
            json_at(OsPlatform::Windows, Some(&s), fresh)["verdict"],
            "breach"
        );
    }

    #[test]
    fn health_json_carries_the_verdict_beside_the_readings() {
        let v = json_of(OsPlatform::Windows, Some(&incident_snapshot_for_tests()));
        assert_eq!(v["verdict"], "breach");
        assert_eq!(
            v["axes"],
            json!({"S7": "breach", "S8": "breach", "S9": "breach"})
        );
        let rules: Vec<&str> = v["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rule"].as_str().unwrap())
            .collect();
        assert_eq!(
            rules,
            ["token_objects", "pool_total", "pool_nonpaged", "spawn_p50"]
        );
        assert_eq!(v["reasons"][0]["axis"], "S7");
        assert_eq!(v["reasons"][0]["band"], "breach");
        assert_eq!(v["reasons"][0]["value"], 4_365_962);
        assert_eq!(v["thresholds"]["tokenObjectsBreach"], 1_000_000);
        // The readings are still there.
        assert_eq!(v["readings"]["tokenObjects"], 4_365_962);

        let v = json_of(OsPlatform::Windows, None);
        assert_eq!(v["verdict"], "unknown");
        assert_eq!(v["readings"], Value::Null);
    }

    /// The live door reads the PROCESS-GLOBAL cell (through the test seam) and
    /// grades it; an empty cell is UNKNOWN.
    #[test]
    fn health_json_reads_the_cell_and_grades_it() {
        let mut s = incident_snapshot_for_tests();
        s.read_at = Utc::now();
        // The platform is spelled out rather than taken from
        // `OsPlatform::current()`, so this test names only the entry point
        // `/health` calls (`health_json`) and never a producer-side symbol.
        let platform = if cfg!(windows) {
            OsPlatform::Windows
        } else if cfg!(target_os = "linux") {
            OsPlatform::Linux
        } else {
            OsPlatform::Unsupported
        };
        s.platform = platform;
        {
            let _cell = store_for_tests(Some(s));
            let v = health_json();
            assert_eq!(v["readings"]["tokenObjects"], 4_365_962);
            assert_eq!(
                v["injected"], false,
                "the test seam is not the env injection"
            );
            assert_eq!(v["bannerDwellSecs"], BANNER_DWELL_SECS);
            assert_eq!(v["verdict"], "breach");
            if platform != OsPlatform::Windows {
                // Off Windows S7/S8 are skipped; S9 still breaches.
                assert_eq!(v["axes"]["S7"], "not_applicable");
            }
        }
        {
            let _cell = store_for_tests(None);
            assert_eq!(health_json()["verdict"], "unknown");
        }
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
