//! Coord's view of this device's tenant bindings, recorded beside
//! `paired_user.json` — the heartbeat-owned sidecar of plan
//! `2026-09-17-plan-adapter-mints-work-units-under-the-default-binding-of-a-multi-bound-device`
//! (D1).
//!
//! ## Why a second file, and why the heartbeat writes it
//!
//! `paired_user.json` records the bindings this runner holds a CREDENTIAL for.
//! `pair::reconcile_paired_bindings_with` deliberately never adds an entry it
//! has no JWT for — it reports such a tenant as `coord_only` and leaves the
//! file alone — so a device bound to three tenants in coord while holding one
//! credential slot reads, locally, as single-bound. Every local binding count
//! (`auth::device_binding_count`) inherits that blind spot, and the plan
//! adapter inherited it too: it minted `coord.work_units` rows under the
//! default credential on a device that could not, in fact, say which tenant
//! owned the plans it was scanning.
//!
//! Coord DOES tell the runner the true set: the register heartbeat
//! (`fleet::heartbeat`, every 30 s) answers with a hydrated `tenant_ids`
//! array. That answer is the only authoritative binding count this process
//! ever sees, and it is ephemeral — it lives for one heartbeat and is gone.
//! This module makes it durable enough to be read by a consumer that runs on a
//! different cadence (the adapter's ~68 s scan) and, at boot, before the first
//! heartbeat has answered.
//!
//! ## What it is NOT
//!
//! - **Not a credential.** It carries tenant UUIDs and a timestamp; nothing
//!   secret. It is written with the ordinary [`crate::fs_atomic::atomic_write`],
//!   not the owner-only variant.
//! - **Not a second writer of `paired_user.json`.** This module never opens
//!   that file. The reconcile in `pair` stays the only thing that does, and a
//!   sibling test pins it (a sentinel `paired_user.json` beside the sidecar is
//!   byte-identical after a record).
//! - **Not an input to the bearer degrade.** `auth::select_scoped_bearer_lazy`
//!   and `count_and_resolve_bearer` keep reading the LOCAL count. Widening the
//!   D2 bearer degrade to this count would flip a multi-bound device's session
//!   writes to unauthenticated — the vet's defect 1 — so the only reader is
//!   the plan adapter's write gate
//!   (`plan_workunit_adapter::trigger::device_work_unit_binding_reading`).
//!
//! ## Freshness
//!
//! A reading is answered as `Some(count)` only while its stamp is younger than
//! [`COORD_BOUND_TENANTS_MAX_AGE_SECS`] and not from the future; every other
//! state — absent, unparseable, stale, future — is `None`, UNKNOWN, which the
//! consumer combines with the local count by `max` so it can never LOWER
//! today's figure. The writer rewrites only when the set changes or the stamp
//! is older than [`COORD_BOUND_TENANTS_RESTAMP_SECS`], so the 30 s heartbeat
//! cadence costs no disk write at steady state and the stamp still proves,
//! hourly, that coord was recently heard from.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// File name of the sidecar, resolved beside `paired_user.json`.
pub const COORD_BOUND_TENANTS_FILE: &str = "coord_bound_tenants.json";

/// Rewrite an UNCHANGED set once the stamp is this old, so a reader can tell
/// "coord confirmed this set within the hour" from "this is what coord said
/// once, some time ago".
pub const COORD_BOUND_TENANTS_RESTAMP_SECS: i64 = 60 * 60;

/// A stamp older than this is UNKNOWN to the reader. Sized against the
/// restamp window with a wide margin: a runner that has not heard from coord
/// for a day is not one whose binding set anyone should be acting on.
pub const COORD_BOUND_TENANTS_MAX_AGE_SECS: i64 = 24 * 60 * 60;

/// How far into the future a stamp may sit before the reader calls it
/// UNKNOWN. The writer and the reader share one machine clock, but that clock
/// is stepped by NTP and by the operator; a stamp a few seconds ahead of the
/// reader's `now` is a stepped clock, not a forged file, and refusing it
/// would drop the adapter to the LOCAL count — the under-count this sidecar
/// exists to correct — for exactly as long as the step lasted.
pub const COORD_BOUND_TENANTS_FUTURE_SKEW_SECS: i64 = 5 * 60;

/// The on-disk shape: `{"tenant_ids":["<uuid>", …],"observed_at":"<RFC3339>"}`.
///
/// `tenant_ids` is stored as strings and re-parsed on read so a hand-edited or
/// half-written entry degrades to "one junk element" rather than "unparseable
/// file": the reader counts DISTINCT valid UUIDs and ignores the rest.
#[derive(Debug, Serialize, Deserialize)]
struct CoordBoundTenantsFile {
    tenant_ids: Vec<String>,
    observed_at: String,
}

/// What [`record_coord_bound_tenants_at`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Same set, fresh stamp — nothing was written.
    Unchanged,
    /// The set differs from what was on disk (or nothing readable was on
    /// disk) — written.
    Written,
    /// Same set, but the stamp had aged past the restamp window (or sat in
    /// the future) — rewritten with a fresh `observed_at`.
    Restamped,
}

/// Where the sidecar lives: the directory `pair::paired_user_path` resolves
/// (`QONTINUI_SECURE_STORAGE_DIR`, else `data_local_dir()/com.qontinui.runner`),
/// so the CLI bin and the GUI runner read one file — the same reason that
/// path is shared.
pub fn coord_bound_tenants_path() -> Option<PathBuf> {
    crate::pair::paired_user_path().map(|p| p.with_file_name(COORD_BOUND_TENANTS_FILE))
}

/// Record coord's hydrated binding set from the register heartbeat.
///
/// Best-effort by contract: the caller (`fleet::heartbeat`) logs an `Err` at
/// debug and never fails the heartbeat over it. Exactly one writer.
pub fn record_coord_bound_tenants(coord_set: &[Uuid]) -> Result<RecordOutcome, String> {
    let path = coord_bound_tenants_path()
        .ok_or_else(|| "could not resolve the secure storage dir".to_string())?;
    record_coord_bound_tenants_at(&path, coord_set, Utc::now())
}

/// [`record_coord_bound_tenants`] against an explicit path and clock, so the
/// tests run against a temp dir and a pinned `now` — never the operator's real
/// files, never the wall clock.
pub fn record_coord_bound_tenants_at(
    path: &Path,
    coord_set: &[Uuid],
    now: DateTime<Utc>,
) -> Result<RecordOutcome, String> {
    // Order-independent and deduplicated: coord returns a set, and a set that
    // arrives in a different order is not a change worth a disk write.
    let desired: BTreeSet<Uuid> = coord_set.iter().copied().collect();

    let outcome = match read_sidecar(path) {
        Some(existing) if existing.tenant_ids == desired => {
            match existing.observed_at {
                Some(stamp) if stamp_is_within_restamp_window(stamp, now) => {
                    return Ok(RecordOutcome::Unchanged);
                }
                // Same set but a stamp that no longer proves recency (aged
                // out, or from a clock that has since been stepped back) —
                // an unparseable stamp lands here too.
                _ => RecordOutcome::Restamped,
            }
        }
        _ => RecordOutcome::Written,
    };

    let file = CoordBoundTenantsFile {
        tenant_ids: desired.iter().map(Uuid::to_string).collect(),
        observed_at: now.to_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|e| format!("serialize {COORD_BOUND_TENANTS_FILE}: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    crate::fs_atomic::atomic_write(path, &bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(outcome)
}

/// How many tenants coord says this device is bound to, per the sidecar —
/// or `None` when the sidecar cannot answer (absent, unparseable, stale,
/// future-stamped). `None` is UNKNOWN, never zero: the consumer folds it in
/// with `max` against the local count, so an unknown can only ever leave
/// today's figure standing.
pub fn coord_bound_tenant_count() -> Option<usize> {
    let path = coord_bound_tenants_path()?;
    coord_bound_tenant_count_at(&path, Utc::now())
}

/// [`coord_bound_tenant_count`] against an explicit path and clock.
pub fn coord_bound_tenant_count_at(path: &Path, now: DateTime<Utc>) -> Option<usize> {
    let sidecar = read_sidecar(path)?;
    let observed_at = sidecar.observed_at?;
    if !stamp_is_fresh(observed_at, now) {
        return None;
    }
    Some(sidecar.tenant_ids.len())
}

/// The sidecar as read: the DISTINCT valid UUIDs it names (junk elements
/// dropped, never counted) and its stamp, `None` when the stamp does not parse
/// as RFC3339. `None` overall when the file is absent or is not the expected
/// JSON shape.
struct SidecarRead {
    tenant_ids: BTreeSet<Uuid>,
    observed_at: Option<DateTime<Utc>>,
}

fn read_sidecar(path: &Path) -> Option<SidecarRead> {
    let bytes = std::fs::read(path).ok()?;
    let file: CoordBoundTenantsFile = serde_json::from_slice(&bytes).ok()?;
    let tenant_ids = file
        .tenant_ids
        .iter()
        .filter_map(|s| Uuid::parse_str(s.trim()).ok())
        .collect();
    let observed_at = DateTime::parse_from_rfc3339(file.observed_at.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc));
    Some(SidecarRead {
        tenant_ids,
        observed_at,
    })
}

/// Reader freshness: not older than the max age, not further ahead of `now`
/// than the skew allowance.
fn stamp_is_fresh(stamp: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let age = (now - stamp).num_seconds();
    (-COORD_BOUND_TENANTS_FUTURE_SKEW_SECS..=COORD_BOUND_TENANTS_MAX_AGE_SECS).contains(&age)
}

/// Writer steady state: the stamp is inside the restamp window and not from
/// the future. A future stamp is restamped rather than left standing, because
/// a stamp ahead of the clock says nothing about when coord was last heard.
fn stamp_is_within_restamp_window(stamp: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let age = (now - stamp).num_seconds();
    (0..=COORD_BOUND_TENANTS_RESTAMP_SECS).contains(&age)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    /// The fixed clock every test writes and reads at.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 17, 12, 0, 0).unwrap()
    }

    fn tenants(n: usize) -> Vec<Uuid> {
        (1..=n)
            .map(|i| Uuid::parse_str(&format!("00000000-0000-0000-0000-{i:012}")).unwrap())
            .collect()
    }

    /// A temp dir holding a sentinel `paired_user.json`, plus the sidecar
    /// path beside it — the layout the real store has.
    fn store() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let paired = tmp.path().join("paired_user.json");
        std::fs::write(
            &paired,
            br#"{"user_id":"u","bindings":[{"tenant_id":"t","user_id":"u"}]}"#,
        )
        .unwrap();
        let sidecar = tmp.path().join(COORD_BOUND_TENANTS_FILE);
        (tmp, paired, sidecar)
    }

    #[test]
    fn record_then_count_round_trips_and_dedups() {
        let (_tmp, _paired, sidecar) = store();
        let three = tenants(3);
        // Duplicates in coord's answer are one binding each, whatever the order.
        let with_dupes: Vec<Uuid> = [three[2], three[0], three[1], three[0], three[2]].to_vec();
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &with_dupes, now()).unwrap(),
            RecordOutcome::Written
        );
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), Some(3));
        // The file names each tenant exactly once.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(raw["tenant_ids"].as_array().unwrap().len(), 3);
        assert_eq!(raw["observed_at"].as_str().unwrap(), now().to_rfc3339());
    }

    #[test]
    fn an_unchanged_set_inside_the_restamp_window_is_not_rewritten() {
        let (_tmp, _paired, sidecar) = store();
        let set = tenants(2);
        record_coord_bound_tenants_at(&sidecar, &set, now()).unwrap();
        let before = std::fs::read(&sidecar).unwrap();
        let mtime = std::fs::metadata(&sidecar).unwrap().modified().unwrap();

        // Same set, different order, 59 minutes later: steady state.
        let reordered = vec![set[1], set[0]];
        let later = now() + Duration::seconds(COORD_BOUND_TENANTS_RESTAMP_SECS - 60);
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &reordered, later).unwrap(),
            RecordOutcome::Unchanged
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            before,
            "content untouched"
        );
        assert_eq!(
            std::fs::metadata(&sidecar).unwrap().modified().unwrap(),
            mtime,
            "the file was not even rewritten in place"
        );
    }

    #[test]
    fn an_unchanged_set_past_the_restamp_window_is_restamped() {
        let (_tmp, _paired, sidecar) = store();
        let set = tenants(2);
        record_coord_bound_tenants_at(&sidecar, &set, now()).unwrap();
        let later = now() + Duration::seconds(COORD_BOUND_TENANTS_RESTAMP_SECS + 1);
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &set, later).unwrap(),
            RecordOutcome::Restamped
        );
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(raw["observed_at"].as_str().unwrap(), later.to_rfc3339());
        assert_eq!(coord_bound_tenant_count_at(&sidecar, later), Some(2));
    }

    #[test]
    fn a_future_stamp_is_restamped_rather_than_trusted() {
        let (_tmp, _paired, sidecar) = store();
        let set = tenants(1);
        let ahead = now() + Duration::hours(1);
        record_coord_bound_tenants_at(&sidecar, &set, ahead).unwrap();
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &set, now()).unwrap(),
            RecordOutcome::Restamped
        );
    }

    #[test]
    fn a_shrink_is_recorded_at_once() {
        let (_tmp, _paired, sidecar) = store();
        record_coord_bound_tenants_at(&sidecar, &tenants(3), now()).unwrap();
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), Some(3));
        // One second later coord says two — no waiting for the restamp window.
        let t = now() + Duration::seconds(1);
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &tenants(2), t).unwrap(),
            RecordOutcome::Written
        );
        assert_eq!(coord_bound_tenant_count_at(&sidecar, t), Some(2));
        // And a shrink to nothing is a recorded zero, not an absent file.
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &[], t).unwrap(),
            RecordOutcome::Written
        );
        assert_eq!(coord_bound_tenant_count_at(&sidecar, t), Some(0));
    }

    #[test]
    fn stale_future_unparseable_and_absent_all_read_as_unknown() {
        let (_tmp, _paired, sidecar) = store();
        // Absent.
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), None);

        // Stale: one second past the max age.
        record_coord_bound_tenants_at(&sidecar, &tenants(3), now()).unwrap();
        let stale_now = now() + Duration::seconds(COORD_BOUND_TENANTS_MAX_AGE_SECS + 1);
        assert_eq!(coord_bound_tenant_count_at(&sidecar, stale_now), None);
        // …and exactly at the max age it still answers.
        let edge = now() + Duration::seconds(COORD_BOUND_TENANTS_MAX_AGE_SECS);
        assert_eq!(coord_bound_tenant_count_at(&sidecar, edge), Some(3));

        // Future: stamped an hour ahead of the reader's clock.
        let early_now = now() - Duration::hours(1);
        assert_eq!(coord_bound_tenant_count_at(&sidecar, early_now), None);
        // …while a stamp inside the skew allowance is a stepped clock, not a
        // forged file.
        let skewed_now = now() - Duration::seconds(COORD_BOUND_TENANTS_FUTURE_SKEW_SECS);
        assert_eq!(coord_bound_tenant_count_at(&sidecar, skewed_now), Some(3));

        // Unparseable: not JSON at all.
        std::fs::write(&sidecar, b"{not json").unwrap();
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), None);
        // JSON of the wrong shape.
        std::fs::write(&sidecar, br#"{"tenant_ids":"three"}"#).unwrap();
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), None);
        // Right shape, unparseable stamp.
        std::fs::write(
            &sidecar,
            br#"{"tenant_ids":["00000000-0000-0000-0000-000000000001"],"observed_at":"yesterday"}"#,
        )
        .unwrap();
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), None);
    }

    #[test]
    fn junk_entries_never_inflate_the_count() {
        let (_tmp, _paired, sidecar) = store();
        let body = format!(
            r#"{{"tenant_ids":["00000000-0000-0000-0000-000000000001","not-a-uuid","",
                "00000000-0000-0000-0000-000000000001","  00000000-0000-0000-0000-000000000002 "],
                "observed_at":"{}"}}"#,
            now().to_rfc3339()
        );
        std::fs::write(&sidecar, body).unwrap();
        // Two valid distinct UUIDs (one duplicated, one whitespace-padded), two junk.
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), Some(2));
    }

    #[test]
    fn a_junk_laden_file_compares_by_its_valid_set() {
        // The writer compares against the VALID set on disk, so coord
        // re-sending the same two tenants is Unchanged even though the file
        // carried junk — and a Written outcome would strip it.
        let (_tmp, _paired, sidecar) = store();
        let two = tenants(2);
        let body = format!(
            r#"{{"tenant_ids":["{}","junk","{}"],"observed_at":"{}"}}"#,
            two[0],
            two[1],
            now().to_rfc3339()
        );
        std::fs::write(&sidecar, body).unwrap();
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &two, now()).unwrap(),
            RecordOutcome::Unchanged
        );
    }

    #[test]
    fn the_writer_never_touches_paired_user_json() {
        let (_tmp, paired, sidecar) = store();
        let sentinel = std::fs::read(&paired).unwrap();
        let mtime = std::fs::metadata(&paired).unwrap().modified().unwrap();

        record_coord_bound_tenants_at(&sidecar, &tenants(3), now()).unwrap();
        record_coord_bound_tenants_at(&sidecar, &tenants(1), now() + Duration::seconds(1)).unwrap();
        record_coord_bound_tenants_at(&sidecar, &tenants(1), now() + Duration::hours(2)).unwrap();

        assert_eq!(std::fs::read(&paired).unwrap(), sentinel);
        assert_eq!(
            std::fs::metadata(&paired).unwrap().modified().unwrap(),
            mtime
        );
        // And no temp file was left beside either of them.
        let leftovers: Vec<_> = std::fs::read_dir(sidecar.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != "paired_user.json" && n != COORD_BOUND_TENANTS_FILE)
            .collect();
        assert!(leftovers.is_empty(), "stray files: {leftovers:?}");
    }

    #[test]
    fn a_missing_parent_dir_is_created() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp
            .path()
            .join("nested")
            .join("deeper")
            .join(COORD_BOUND_TENANTS_FILE);
        assert_eq!(
            record_coord_bound_tenants_at(&sidecar, &tenants(2), now()).unwrap(),
            RecordOutcome::Written
        );
        assert_eq!(coord_bound_tenant_count_at(&sidecar, now()), Some(2));
    }

    #[test]
    fn the_sidecar_sits_beside_paired_user_json() {
        // Pure path arithmetic over the shared resolver: same directory,
        // different file name.
        let p = Path::new("/store/paired_user.json").with_file_name(COORD_BOUND_TENANTS_FILE);
        assert_eq!(p, Path::new("/store/coord_bound_tenants.json"));
    }
}
