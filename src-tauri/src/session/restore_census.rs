//! Restore CENSUS — the machine-checkable statement "these N sessions were open
//! before the restart; these N came back; they are the same N" (plan
//! `2026-08-16-runner-session-info-dropdown-and-restore-verification`, D6/P5,
//! closing G6).
//!
//! ## Why this exists
//!
//! The runner has shipped at least eight rounds of session-restore work and the
//! feature still is not trusted, because nothing ever COMPARED a pre-restart
//! census to a post-restart one. `GET /control/sessions/restore-health` answers
//! *"is each open record theoretically restorable"* — a PRECONDITION, not an
//! outcome. This module answers the outcome.
//!
//! ## Where `expected` comes from — and why not the shutdown marker
//!
//! [`crate::session::shutdown_marker::ShutdownMarker`] is `{clean, at}`: a
//! BOUNDARY, carrying no session census. It cannot supply `expected`, and R3
//! forbids the only other available source — the POST-restart registry, which
//! would make the census self-referential ("everything that came back was
//! expected", verdict always `match`).
//!
//! So `expected` is LATCHED ONCE AT BOOT ([`latch_expected`]), from
//! `store.restorable_records(now, prior_marker_at, boot_was_clean)` — already
//! the exact set the boot-restore path consumes — read BEFORE any restore,
//! reconcile pass or liveness tick can mutate a record, and held in a
//! process-wide `OnceLock` for the life of the process. That is the
//! shutdown-time set, read before the restore touches it, which is precisely
//! what R3 demands.
//!
//! ## Why an absent latch is `unknown`, not empty
//!
//! Every degraded branch returns [`RESTORE_VERDICT_UNKNOWN`] with a stated
//! reason. An empty `expected` is only ever `match` when a CLEAN shutdown
//! marker proves the runner genuinely had zero sessions; a crash boot (no clean
//! marker) cannot state what was open, so it says `unknown` rather than
//! declaring victory over an empty set (served policy
//! `verification-and-evidence` `silent-empty-is-unknown`).
//!
//! ## Instance scoping
//!
//! The census owns NO path of its own. It reads the caller's
//! [`SessionLifecycleStore`] (opened from
//! [`crate::session::session_lifecycle_store::store_path`]) and the process's
//! [`crate::session::shutdown_marker::boot_classification`] (from
//! [`crate::session::shutdown_marker::marker_path`]) — both already resolved
//! through [`crate::instance::scope_path`]. A temp runner therefore cannot
//! inherit the primary's census, which is the `2026-08-10` regression this
//! route must never re-introduce.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Serialize;
use tracing::{info, warn};

use crate::session::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, RESTORE_TIER_FAILED,
};
use crate::session::shutdown_marker::BootClassification;
use crate::session::snapshot_history::{is_restorable_identity, TranscriptProbe};

/// Every expected session came back, and nothing unexpected did.
pub const RESTORE_VERDICT_MATCH: &str = "match";
/// Some expected sessions came back; some are missing and/or something
/// unexpected appeared.
pub const RESTORE_VERDICT_PARTIAL: &str = "partial";
/// Nothing that was expected came back (and/or only foreign sessions did).
pub const RESTORE_VERDICT_MISMATCH: &str = "mismatch";
/// The census cannot state an outcome. MANDATORY and load-bearing: a hard-crash
/// boot (no clean shutdown marker) has no trustworthy pre-restart set, and an
/// un-latched census has no set at all. Never collapse this into `match` on an
/// empty `expected`.
pub const RESTORE_VERDICT_UNKNOWN: &str = "unknown";

/// Census status: the route answered.
pub const STATUS_OK: &str = "ok";
/// Census status: the route could not answer, and `reason` says why.
pub const STATUS_UNAVAILABLE: &str = "unavailable";

/// `missing` reason: the record was never identity-restorable in the first
/// place (no confirmation and/or no transcript) — restore could not have
/// brought its conversation back.
pub const MISSING_NOT_RESTORABLE: &str = "not-restorable";
/// `missing` reason: a resume WAS attempted for this record and has not landed
/// (including a restore still in flight — the fail-closed reading).
pub const MISSING_RESUME_FAILED: &str = "resume-failed";
/// `missing` reason: nothing ever stamped this record as restored — no restore
/// was attempted for it at all.
pub const MISSING_NO_ATTEMPT: &str = "no-attempt";

/// `unexpected` reason: a session came back that the pre-restart census never
/// contained (ghost re-admission, or another instance's session leaking in).
pub const UNEXPECTED_FOREIGN: &str = "foreign";
/// `unexpected` reason: the same session id was restored more than once
/// (the 2026-07-23 restore-duplication class).
pub const UNEXPECTED_DUPLICATE: &str = "duplicate";

/// One session as it stood in the LATCHED pre-restart set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusExpected {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub page_id: String,
    pub zone_index: i32,
    pub account_label: Option<String>,
    pub session_name: Option<String>,
    /// `confirmed && transcriptExists` at LATCH time — the same join
    /// `/control/sessions/restore-health` reports. A `false` here says the
    /// conversation was never resumable, so its later absence is not a restore
    /// defect.
    pub restorable: bool,
}

/// One session observed as restored THIS boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusRestored {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub zone_index: i32,
    /// `"resumed"` | `"terminal-only"`. A `"failed"` tier is NOT a restore —
    /// it lands in `missing` with [`MISSING_RESUME_FAILED`].
    pub restore_tier: Option<String>,
    pub restored_from_boot_at: Option<i64>,
}

/// An expected session that did not come back, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusMissing {
    pub claude_session_id: String,
    /// [`MISSING_NOT_RESTORABLE`] | [`MISSING_RESUME_FAILED`] |
    /// [`MISSING_NO_ATTEMPT`].
    pub reason: String,
}

/// A session that came back but was not in the pre-restart census.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusUnexpected {
    pub claude_session_id: String,
    /// [`UNEXPECTED_FOREIGN`] | [`UNEXPECTED_DUPLICATE`].
    pub reason: String,
}

/// Response body of `GET /control/sessions/restore-census`.
///
/// `status`/`reason` are additive to D6's shape and are what make acceptance
/// criterion 4 hold: no branch can return a bare empty census that is
/// indistinguishable from "this runner had nothing to restore".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreCensusResponse {
    /// [`STATUS_OK`] | [`STATUS_UNAVAILABLE`].
    pub status: String,
    /// REQUIRED whenever `status != "ok"`, and also set whenever the verdict is
    /// `unknown`, naming which unknown it is.
    pub reason: Option<String>,
    pub generated_at: i64,
    /// `at` of the PRIOR shutdown marker; `null` when this boot found none.
    pub shutdown_at: Option<i64>,
    /// `true` iff the previous shutdown was clean. `null` when this process
    /// never classified its boot (no [`crate::session::shutdown_marker::classify_boot`]
    /// call yet) — UNKNOWN, not `false`.
    pub clean_shutdown: Option<bool>,
    pub expected: Vec<CensusExpected>,
    pub restored: Vec<CensusRestored>,
    pub missing: Vec<CensusMissing>,
    pub unexpected: Vec<CensusUnexpected>,
    /// [`RESTORE_VERDICT_MATCH`] | [`RESTORE_VERDICT_PARTIAL`] |
    /// [`RESTORE_VERDICT_MISMATCH`] | [`RESTORE_VERDICT_UNKNOWN`].
    pub verdict: String,
}

/// One observation of a record that carries a boot-restore stamp from THIS
/// boot. The pure diff's input, so the diff tests without a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredObservation {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub zone_index: i32,
    pub restore_tier: Option<String>,
    pub restored_from_boot_at: Option<i64>,
}

/// The outcome of [`diff_census`] — the three buckets plus the verdict over
/// them. A named struct rather than a 4-tuple so each bucket is impossible to
/// mis-order at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusDiff {
    pub restored: Vec<CensusRestored>,
    pub missing: Vec<CensusMissing>,
    pub unexpected: Vec<CensusUnexpected>,
    pub verdict: String,
}

/// The once-per-process latch: the pre-restore `expected` set plus the instant
/// it was taken.
#[derive(Debug, Clone)]
pub struct BootCensus {
    /// Unix millis when the latch ran — i.e. this process's boot. Used to
    /// discriminate a restore stamp from THIS boot against a sticky stamp left
    /// by a PREVIOUS one (`restored_from_boot_at` survives restarts by design).
    pub boot_at_ms: i64,
    pub expected: Vec<CensusExpected>,
}

static BOOT_CENSUS: OnceLock<BootCensus> = OnceLock::new();

/// Project restorable records into the latched `expected` rows. Pure over its
/// inputs (the probe is injected), so it tests without a disk.
pub fn expected_rows(
    records: Vec<TerminalSessionRecord>,
    probe: &dyn TranscriptProbe,
) -> Vec<CensusExpected> {
    let mut rows: Vec<CensusExpected> = records
        .into_iter()
        .map(|rec| {
            let confirmed = rec.confirmed_at.is_some();
            let transcript_exists =
                probe.transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref());
            CensusExpected {
                claude_session_id: rec.claude_session_id,
                terminal_id: rec.terminal_id,
                page_id: rec.page_id,
                zone_index: rec.zone_index,
                account_label: rec.account_label,
                session_name: rec.session_name,
                restorable: is_restorable_identity(confirmed, transcript_exists),
            }
        })
        .collect();
    // Stable order so two probes of the same census diff cleanly.
    rows.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    rows
}

/// LATCH the pre-restart census, exactly once, at boot.
///
/// MUST be called from `main.rs` setup after the lifecycle store is opened and
/// BEFORE anything can mutate a record — the boot reconcile pass, the liveness
/// poll, and the frontend's restore all run later. Idempotent: a second call is
/// ignored (with a warn), so no ordering accident can silently re-latch a
/// POST-restore set and hand the census the self-referential `expected` R3
/// forbids.
///
/// `boot` is the process's ONE-SHOT boot classification, read via
/// [`crate::session::shutdown_marker::boot_classification`] — never
/// `classify_boot`, which rewrites the marker on first call.
pub fn latch_expected(
    store: &SessionLifecycleStore,
    probe: &dyn TranscriptProbe,
    now_ms: i64,
    boot: Option<BootClassification>,
) {
    let prior_marker_at = boot.and_then(|c| c.prior_marker_at);
    let boot_was_clean = boot.map(|c| !c.crash_recovery).unwrap_or(false);
    // Exactly the three arguments `terminal_session_list_open` passes, so the
    // latched set IS the set the restore path will consume.
    let records = store.restorable_records(now_ms, prior_marker_at, boot_was_clean);
    let census = BootCensus {
        boot_at_ms: now_ms,
        expected: expected_rows(records, probe),
    };
    let expected_count = census.expected.len();
    if BOOT_CENSUS.set(census).is_err() {
        warn!("restore_census: expected set already latched — ignoring re-latch (R3)");
        return;
    }
    info!(
        expected = expected_count,
        clean_shutdown = boot_was_clean,
        "restore_census: latched the pre-restore expected set"
    );
}

/// The latched pre-restart census, if [`latch_expected`] has run. `None` means
/// UNKNOWN (never "zero sessions").
pub fn latched() -> Option<&'static BootCensus> {
    BOOT_CENSUS.get()
}

/// The pure diff: pre-restart `expected` vs the sessions observed restored this
/// boot ⇒ `restored` / `missing` / `unexpected` / `verdict`.
///
/// `clean_shutdown` is the ONLY gate on stating an outcome at all. When it is
/// not `Some(true)` the verdict is [`RESTORE_VERDICT_UNKNOWN`]: without a clean
/// shutdown boundary the runner has no trustworthy statement of what was open
/// when it died, so `restorable_records` is a recency HEURISTIC rather than a
/// census — and a `match` computed from it would be the "wrong in the
/// operator's favour" outcome R3 names as worse than no census at all. The
/// buckets are still populated so an operator can see the evidence behind the
/// `unknown`.
///
/// Classification:
/// - a `failed`-tier stamp is NOT a restore — the tile may exist but the
///   session did not come back, so it is `missing` / [`MISSING_RESUME_FAILED`];
/// - a second observation of an already-seen id is
///   `unexpected` / [`UNEXPECTED_DUPLICATE`] (2026-07-23 restore duplication);
/// - an observation of an id not in `expected` is
///   `unexpected` / [`UNEXPECTED_FOREIGN`] (2026-07-19 ghost re-admission,
///   2026-08-10 temp-runner inheritance).
pub fn diff_census(
    expected: &[CensusExpected],
    observed: &[RestoredObservation],
    clean_shutdown: Option<bool>,
) -> CensusDiff {
    let expected_ids: HashSet<&str> = expected
        .iter()
        .map(|e| e.claude_session_id.as_str())
        .collect();
    // Tier per observed id — consulted when an expected session did NOT come
    // back, to say WHY.
    let observed_tier: HashMap<&str, Option<&str>> = observed
        .iter()
        .map(|o| (o.claude_session_id.as_str(), o.restore_tier.as_deref()))
        .collect();

    let mut restored: Vec<CensusRestored> = Vec::new();
    let mut unexpected: Vec<CensusUnexpected> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for obs in observed {
        let id = obs.claude_session_id.as_str();
        if !seen.insert(id) {
            unexpected.push(CensusUnexpected {
                claude_session_id: obs.claude_session_id.clone(),
                reason: UNEXPECTED_DUPLICATE.to_string(),
            });
            continue;
        }
        if obs.restore_tier.as_deref() == Some(RESTORE_TIER_FAILED) {
            // Attempted and not landed — accounted for under `missing` below,
            // never as a restore.
            continue;
        }
        if !expected_ids.contains(id) {
            unexpected.push(CensusUnexpected {
                claude_session_id: obs.claude_session_id.clone(),
                reason: UNEXPECTED_FOREIGN.to_string(),
            });
            continue;
        }
        restored.push(CensusRestored {
            claude_session_id: obs.claude_session_id.clone(),
            terminal_id: obs.terminal_id.clone(),
            zone_index: obs.zone_index,
            restore_tier: obs.restore_tier.clone(),
            restored_from_boot_at: obs.restored_from_boot_at,
        });
    }

    // Scoped so the `&str`s borrowed out of `restored` are definitively dead
    // before the sort below takes it mutably — the intent, spelled, rather than
    // left to NLL inference.
    let mut missing: Vec<CensusMissing> = {
        let restored_ids: HashSet<&str> = restored
            .iter()
            .map(|r| r.claude_session_id.as_str())
            .collect();
        expected
            .iter()
            .filter(|e| !restored_ids.contains(e.claude_session_id.as_str()))
            .map(|e| CensusMissing {
                claude_session_id: e.claude_session_id.clone(),
                reason: if !e.restorable {
                    MISSING_NOT_RESTORABLE.to_string()
                } else if observed_tier.get(e.claude_session_id.as_str())
                    == Some(&Some(RESTORE_TIER_FAILED))
                {
                    MISSING_RESUME_FAILED.to_string()
                } else {
                    MISSING_NO_ATTEMPT.to_string()
                },
            })
            .collect()
    };

    restored.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    missing.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    unexpected.sort_by(|a, b| {
        a.claude_session_id
            .cmp(&b.claude_session_id)
            .then_with(|| a.reason.cmp(&b.reason))
    });

    let verdict = if clean_shutdown != Some(true) {
        RESTORE_VERDICT_UNKNOWN
    } else if missing.is_empty() && unexpected.is_empty() {
        RESTORE_VERDICT_MATCH
    } else if !restored.is_empty() {
        RESTORE_VERDICT_PARTIAL
    } else {
        RESTORE_VERDICT_MISMATCH
    };
    CensusDiff {
        restored,
        missing,
        unexpected,
        verdict: verdict.to_string(),
    }
}

/// Name the specific UNKNOWN so the route never returns a bare `unknown`.
fn unknown_reason(clean_shutdown: Option<bool>) -> &'static str {
    match clean_shutdown {
        None => "boot_not_classified",
        Some(false) => "no_clean_shutdown_marker",
        Some(true) => "unknown",
    }
}

/// Observations of every record carrying a boot-restore stamp from THIS boot.
///
/// The `boot_at_ms` gate is load-bearing: `restored_from_boot_at` is STICKY
/// across restarts by design (a record must not forget it was once restored),
/// so a stamp older than this process's own latch belongs to a PREVIOUS boot
/// and must not count as evidence for this one.
///
/// Reads ALL records, open and closed alike: a session that came back and was
/// then closed by the operator did in fact come back, and dropping it would
/// report a `missing` the restore is not guilty of.
pub fn observe_restored(
    store: &SessionLifecycleStore,
    boot_at_ms: i64,
) -> Vec<RestoredObservation> {
    let mut out: Vec<RestoredObservation> = store
        .all_records()
        .into_iter()
        .filter(|r| r.restored_from_boot_at.is_some_and(|at| at >= boot_at_ms))
        .map(|r| RestoredObservation {
            claude_session_id: r.claude_session_id,
            terminal_id: r.terminal_id,
            zone_index: r.zone_index,
            restore_tier: r.restore_tier,
            restored_from_boot_at: r.restored_from_boot_at,
        })
        .collect();
    out.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    out
}

/// Build the full census response.
///
/// Degraded branches, each with a stated reason and verdict `unknown`:
/// - `census_not_latched` — [`latch_expected`] never ran (a boot that failed
///   before the latch, or an ephemeral/unit-test app). NOT "zero sessions".
/// - `boot_not_classified` — this process never classified its boot, so the
///   shutdown boundary is unknown.
/// - `no_clean_shutdown_marker` — the previous shutdown was not clean, so there
///   is no trustworthy pre-restart set to compare against (T10).
pub fn build_census(
    store: &SessionLifecycleStore,
    boot: Option<BootClassification>,
    now_ms: i64,
) -> RestoreCensusResponse {
    let clean_shutdown = boot.map(|b| !b.crash_recovery);
    let shutdown_at = boot.and_then(|b| b.prior_marker_at);
    let Some(census) = latched() else {
        return RestoreCensusResponse {
            status: STATUS_UNAVAILABLE.to_string(),
            reason: Some("census_not_latched".to_string()),
            generated_at: now_ms,
            shutdown_at,
            clean_shutdown,
            expected: Vec::new(),
            restored: Vec::new(),
            missing: Vec::new(),
            unexpected: Vec::new(),
            verdict: RESTORE_VERDICT_UNKNOWN.to_string(),
        };
    };
    let observed = observe_restored(store, census.boot_at_ms);
    let diff = diff_census(&census.expected, &observed, clean_shutdown);
    let reason = (diff.verdict == RESTORE_VERDICT_UNKNOWN)
        .then(|| unknown_reason(clean_shutdown).to_string());
    RestoreCensusResponse {
        status: STATUS_OK.to_string(),
        reason,
        generated_at: now_ms,
        shutdown_at,
        clean_shutdown,
        expected: census.expected.clone(),
        restored: diff.restored,
        missing: diff.missing,
        unexpected: diff.unexpected,
        verdict: diff.verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{
        RESTORE_TIER_RESUMED, RESTORE_TIER_TERMINAL_ONLY,
    };

    fn expected(id: &str, restorable: bool) -> CensusExpected {
        CensusExpected {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            page_id: "default".to_string(),
            zone_index: 0,
            account_label: Some("paktis".to_string()),
            session_name: Some(format!("name-{id}")),
            restorable,
        }
    }

    fn observed(id: &str, tier: &str) -> RestoredObservation {
        RestoredObservation {
            claude_session_id: id.to_string(),
            terminal_id: format!("term-{id}"),
            zone_index: 0,
            restore_tier: Some(tier.to_string()),
            restored_from_boot_at: Some(9_000),
        }
    }

    #[test]
    fn exact_match_after_a_clean_shutdown() {
        let exp = [expected("a", true), expected("b", true)];
        let obs = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("b", RESTORE_TIER_TERMINAL_ONLY),
        ];
        let d = diff_census(&exp, &obs, Some(true));
        assert_eq!(d.restored.len(), 2);
        assert!(d.missing.is_empty());
        assert!(d.unexpected.is_empty());
        assert_eq!(d.verdict, RESTORE_VERDICT_MATCH);
    }

    #[test]
    fn a_missing_session_is_partial_with_a_reason() {
        let exp = [expected("a", true), expected("b", true)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(true));
        assert_eq!(d.restored.len(), 1);
        assert_eq!(d.missing.len(), 1);
        assert_eq!(d.missing[0].claude_session_id, "b");
        assert_eq!(d.missing[0].reason, MISSING_NO_ATTEMPT);
        assert!(d.unexpected.is_empty());
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
    }

    #[test]
    fn a_never_restorable_expected_session_says_so_rather_than_blaming_the_restore() {
        let exp = [expected("a", true), expected("phantom", false)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(true));
        assert_eq!(d.missing[0].claude_session_id, "phantom");
        assert_eq!(d.missing[0].reason, MISSING_NOT_RESTORABLE);
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
    }

    #[test]
    fn a_failed_resume_is_missing_resume_failed_never_a_restore() {
        let exp = [expected("a", true)];
        let obs = [observed("a", RESTORE_TIER_FAILED)];
        let d = diff_census(&exp, &obs, Some(true));
        assert!(
            d.restored.is_empty(),
            "a resume that never landed is not a d.restored session"
        );
        assert_eq!(d.missing[0].reason, MISSING_RESUME_FAILED);
        assert!(d.unexpected.is_empty());
        // Nothing expected came back at all.
        assert_eq!(d.verdict, RESTORE_VERDICT_MISMATCH);
    }

    #[test]
    fn a_foreign_session_is_unexpected() {
        // 2026-07-19 ghost re-admission / 2026-08-10 temp-runner inheritance.
        let exp = [expected("a", true)];
        let obs = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("stranger", RESTORE_TIER_RESUMED),
        ];
        let d = diff_census(&exp, &obs, Some(true));
        assert_eq!(d.restored.len(), 1);
        assert!(d.missing.is_empty());
        assert_eq!(d.unexpected.len(), 1);
        assert_eq!(d.unexpected[0].claude_session_id, "stranger");
        assert_eq!(d.unexpected[0].reason, UNEXPECTED_FOREIGN);
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
    }

    #[test]
    fn a_duplicate_restore_is_unexpected() {
        // 2026-07-23 restore duplication.
        let exp = [expected("a", true)];
        let obs = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("a", RESTORE_TIER_RESUMED),
        ];
        let d = diff_census(&exp, &obs, Some(true));
        assert_eq!(
            d.restored.len(),
            1,
            "the session itself came back exactly once"
        );
        assert!(d.missing.is_empty());
        assert_eq!(d.unexpected.len(), 1);
        assert_eq!(d.unexpected[0].reason, UNEXPECTED_DUPLICATE);
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
    }

    /// T10, as a pure test: a hard-crash boot must NEVER claim `match`.
    #[test]
    fn no_clean_shutdown_marker_is_unknown_even_when_everything_came_back() {
        let exp = [expected("a", true)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(false));
        // The evidence is still shown…
        assert_eq!(d.restored.len(), 1);
        assert!(d.missing.is_empty());
        assert!(d.unexpected.is_empty());
        // …but the runner does not assert an outcome it cannot establish.
        assert_eq!(d.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(unknown_reason(Some(false)), "no_clean_shutdown_marker");
    }

    #[test]
    fn an_unclassified_boot_is_unknown_with_its_own_reason() {
        let d = diff_census(&[], &[], None);
        assert_eq!(d.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(unknown_reason(None), "boot_not_classified");
    }

    /// The empty-`expected` discrimination, stated as its own test because it
    /// is the exact failure the plan calls out: an empty set is only `match`
    /// when a marker PROVES the runner had zero sessions.
    #[test]
    fn empty_expected_is_unknown_without_a_marker_and_match_with_one() {
        let crashed = diff_census(&[], &[], Some(false)).verdict;
        assert_eq!(
            crashed, RESTORE_VERDICT_UNKNOWN,
            "an empty census after a crash proves nothing"
        );
        let clean = diff_census(&[], &[], Some(true)).verdict;
        assert_eq!(
            clean, RESTORE_VERDICT_MATCH,
            "a clean shutdown marker with zero sessions is a genuine match"
        );
    }

    #[derive(Debug)]
    struct FakeProbe {
        with_transcripts: Vec<String>,
    }

    impl TranscriptProbe for FakeProbe {
        fn transcript_exists(&self, session_id: &str, _working_dir: Option<&str>) -> bool {
            self.with_transcripts.iter().any(|s| s == session_id)
        }
    }

    fn record(id: &str, confirmed: bool) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 2,
            title: None,
            terminal_id: format!("term-{id}"),
            opened_at: 1,
            last_seen_at: 2,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: "claude".to_string(),
            origin: None,
            restore_pending_at: None,
            confirmed_at: confirmed.then_some(3),
            handle: None,
            account_label: Some("paktis".to_string()),
            account_wrapper: None,
            session_name: Some("n".to_string()),
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
            finished_at: None,
            finish_reason: None,
            finish_synced: false,
        }
    }

    #[test]
    fn expected_rows_join_confirmation_with_the_transcript_probe() {
        let probe = FakeProbe {
            with_transcripts: vec!["has-transcript".to_string()],
        };
        let rows = expected_rows(
            vec![
                record("has-transcript", true),
                record("no-transcript", true),
                record("unconfirmed", false),
            ],
            &probe,
        );
        assert_eq!(rows.len(), 3);
        // Sorted by id: has-transcript, no-transcript, unconfirmed.
        assert!(rows[0].restorable);
        assert!(!rows[1].restorable, "no transcript ⇒ not restorable");
        assert!(!rows[2].restorable, "unconfirmed ⇒ not restorable");
        assert_eq!(rows[0].account_label.as_deref(), Some("paktis"));
        assert_eq!(rows[0].zone_index, 2);
    }
}
