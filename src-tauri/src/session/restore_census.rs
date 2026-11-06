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
//! [`crate::session::shutdown_marker::ShutdownMarker`] is `{clean, at, teardown_complete}`: a
//! BOUNDARY, carrying no session census. It cannot supply `expected`, and R3
//! forbids the only other available source — the POST-restart registry, which
//! would make the census self-referential ("everything that came back was
//! expected", verdict always `match`).
//!
//! So `expected` is LATCHED ONCE AT BOOT ([`latch_expected`]), from
//! `store.restorable_records(prior_marker_at, boot_was_clean)` — already
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

use crate::session::session_ledger::{observe_row, LiveView, LiveViewGap, Observed};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
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
/// `missing` reason: its live pane's process read was unavailable, so
/// whether it came back is UNKNOWN — and so is the verdict.
pub const MISSING_PROCESS_READ_UNAVAILABLE: &str = "process-read-unavailable";

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
    /// The row's terminal; omitted for an UNBOUND row (blank id — e.g. one held
    /// for an account choice), as in `past_sessions` ([`bound_terminal_id`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
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
    /// Omitted for an unbound row ([`bound_terminal_id`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    pub zone_index: i32,
    /// `"resumed"` in [`RestoreCensusResponse::restored`]; `"terminal-only"` in
    /// [`RestoreCensusResponse::terminal_only`]. A `"failed"` tier is NOT a
    /// restore — it lands in `missing` with [`MISSING_RESUME_FAILED`].
    pub restore_tier: Option<String>,
    pub restored_from_boot_at: Option<i64>,
}

/// An expected session that did not come back, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusMissing {
    pub claude_session_id: String,
    /// [`MISSING_NOT_RESTORABLE`] | [`MISSING_RESUME_FAILED`] |
    /// [`MISSING_NO_ATTEMPT`] | [`MISSING_PROCESS_READ_UNAVAILABLE`].
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
    /// Expected sessions whose terminal came back with NO conversation
    /// (tier `terminal-only`). Not a restore, and not a miss either: the
    /// restore did what it could for a row with nothing to resume (plan
    /// `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`,
    /// D2). Counted on its own, and it keeps the verdict off `match`.
    pub terminal_only: Vec<CensusRestored>,
    pub missing: Vec<CensusMissing>,
    pub unexpected: Vec<CensusUnexpected>,
    /// [`RESTORE_VERDICT_MATCH`] | [`RESTORE_VERDICT_PARTIAL`] |
    /// [`RESTORE_VERDICT_MISMATCH`] | [`RESTORE_VERDICT_UNKNOWN`].
    pub verdict: String,
}

/// One observation of a record THIS boot restored or adopted: it carries this
/// boot's restore stamp, or it is an `open` row on a live pane whose process
/// read verifies it. The pure diff's input, so the diff tests without a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredObservation {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub zone_index: i32,
    pub restore_tier: Option<String>,
    pub restored_from_boot_at: Option<i64>,
    /// What THE per-row predicate the session ledger serves `returned` from
    /// ([`crate::session::session_ledger::observe_row`]) says of this row.
    /// Only [`Observed::Back`] — tier `resumed` this boot, or a process read
    /// of its live pane showing its `claude` running — is a restore, and
    /// [`Observed::Unknown`] makes the verdict unknown, so the census and the
    /// ledger cannot disagree about a pane (plan
    /// `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`, D2).
    pub observed: Option<Observed>,
    /// The row is `open` now. Only an open row is a TILE: a row resumed
    /// this boot and since closed did come back, but it is history, not a
    /// second pane beside the session's current one.
    pub open: bool,
}

/// The outcome of [`diff_census`] — the three buckets plus the verdict over
/// them. A named struct rather than a 4-tuple so each bucket is impossible to
/// mis-order at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusDiff {
    pub restored: Vec<CensusRestored>,
    pub terminal_only: Vec<CensusRestored>,
    pub missing: Vec<CensusMissing>,
    pub unexpected: Vec<CensusUnexpected>,
    pub verdict: String,
    /// Which unknown, whenever the verdict is [`RESTORE_VERDICT_UNKNOWN`].
    pub unknown_reason: Option<String>,
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

impl BootCensus {
    /// Does `rec` still await the boot restore that latched it? True when it
    /// is in the latched `expected` set, which is the restorable set, and has
    /// not been FINISHED since (a finished session is never restored). The
    /// liveness poll holds such a row open rather than closing it `no-terminal`
    /// before its page is opened (plan
    /// `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`,
    /// Phase 4; see
    /// [`crate::session::session_lifecycle_store::withheld_boot_restore_candidate`]).
    pub fn awaits_restore(&self, rec: &TerminalSessionRecord) -> bool {
        rec.finished_at.is_none()
            && self
                .expected
                .iter()
                .any(|e| e.claude_session_id == rec.claude_session_id)
    }
}

static BOOT_CENSUS: OnceLock<BootCensus> = OnceLock::new();

/// A row's terminal id as the census emits it: `None` for a blank id (an
/// unbound row has no terminal), the id otherwise — the same rule
/// `past_sessions` applies.
fn bound_terminal_id(terminal_id: &str) -> Option<String> {
    Some(terminal_id.to_string()).filter(|t| !t.trim().is_empty())
}

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
                terminal_id: bound_terminal_id(&rec.terminal_id),
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
    let boot_was_clean = boot.map(|c| !c.crash_recovery).unwrap_or(false);
    let census = BootCensus {
        boot_at_ms: now_ms,
        expected: boot_expected(store, probe, boot),
    };
    let expected_count = census.expected.len();
    if BOOT_CENSUS.set(census).is_err() {
        warn!("restore_census: expected set already latched — ignoring re-latch (R3)");
        return;
    }
    info!(
        expected = expected_count,
        clean_shutdown = boot_was_clean,
        teardown_complete = boot.is_some_and(|c| c.teardown_complete),
        "restore_census: latched the pre-restore expected set"
    );
}

/// The `expected` set [`latch_expected`] latches — the pure half, without the
/// process-wide latch, so it is testable.
pub(crate) fn boot_expected(
    store: &SessionLifecycleStore,
    probe: &dyn TranscriptProbe,
    boot: Option<BootClassification>,
) -> Vec<CensusExpected> {
    let prior_marker_at = boot.and_then(|c| c.prior_marker_at);
    let boot_was_clean = boot.map(|c| !c.crash_recovery).unwrap_or(false);
    // Exactly the two arguments `terminal_session_list_open` passes (the boot
    // line is the store's own), so the latched set IS the set the restore path
    // will consume.
    expected_rows(
        store.restorable_records(prior_marker_at, boot_was_clean),
        probe,
    )
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
/// `live_gap` says why no live view was read, when none was. Without one an
/// open row can never verify, so no outcome is stated: the verdict is unknown
/// with the gap's own reason (`live_view_unavailable` or `live_view_timeout`),
/// as it is when any expected row's process read was unavailable
/// (`process_read_unavailable`).
///
/// Classification:
/// - only a VERIFIED observation ([`RestoredObservation::observed`] is
///   [`Observed::Back`]) is a
///   restore — including a pane this boot ADOPTED with its `claude` still
///   running, which carries no restore stamp at all;
/// - an OPEN row the ledger observes [`Observed::TerminalOnly`] is NOT a
///   restore — a terminal came back, the conversation did not — so it is
///   counted in `terminal_only`, neither restored nor missing, and the
///   verdict is not `match` (a closed one is `missing`, as on the ledger);
/// - any other unverified stamp (a `failed` tier: attempted, not landed) is
///   NOT a restore either — the tile may exist but the session did not come
///   back, so it is `missing` / [`MISSING_RESUME_FAILED`];
/// - a session's rows are merged first, by the ledger's [`Observed`] order,
///   and ids compare case-insensitively, as on the ledger;
/// - a session on more than one TILE — rows observed back or as a bare
///   terminal — is `unexpected` / [`UNEXPECTED_DUPLICATE`] once per extra
///   tile (the 2026-07-23 restore duplication: a respawn beside a live
///   session, whose second tile may verify or may sit bare). A closed or
///   unverified row is history, not a second tile;
/// - an observation of an id not in `expected` is
///   `unexpected` / [`UNEXPECTED_FOREIGN`] (2026-07-19 ghost re-admission,
///   2026-08-10 temp-runner inheritance).
pub fn diff_census(
    expected: &[CensusExpected],
    observed: &[RestoredObservation],
    clean_shutdown: Option<bool>,
    live_gap: Option<LiveViewGap>,
) -> CensusDiff {
    // Every id compares lowercased, as the ledger keys its observations.
    let key = |id: &str| id.to_ascii_lowercase();
    let expected_ids: HashSet<String> =
        expected.iter().map(|e| key(&e.claude_session_id)).collect();
    // Ids this boot attempted — consulted when an expected session did NOT
    // come back, to say WHY.
    let attempted: HashSet<String> = observed.iter().map(|o| key(&o.claude_session_id)).collect();
    // One observation per session, merged by the ledger's own order (the
    // strongest `Observed` stands), so a session with a closed stamped row
    // and an open adopted one reads as the ledger reads it.
    let mut best: Vec<&RestoredObservation> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    let mut tiles: HashMap<String, (usize, &RestoredObservation)> = HashMap::new();
    for obs in observed {
        let id = key(&obs.claude_session_id);
        if obs.open && matches!(obs.observed, Some(Observed::Back | Observed::TerminalOnly)) {
            tiles.entry(id.clone()).or_insert((0, obs)).0 += 1;
        }
        match slot.get(&id) {
            Some(&i) if best[i].observed >= obs.observed => {}
            Some(&i) => best[i] = obs,
            None => {
                slot.insert(id, best.len());
                best.push(obs);
            }
        }
    }
    let unreadable: HashSet<String> = best
        .iter()
        .filter(|o| o.observed == Some(Observed::Unknown))
        .map(|o| key(&o.claude_session_id))
        .collect();

    let mut restored: Vec<CensusRestored> = Vec::new();
    let mut terminal_only: Vec<CensusRestored> = Vec::new();
    let mut unexpected: Vec<CensusUnexpected> = tiles
        .values()
        .flat_map(|(n, first)| {
            (1..*n).map(|_| CensusUnexpected {
                claude_session_id: first.claude_session_id.clone(),
                reason: UNEXPECTED_DUPLICATE.to_string(),
            })
        })
        .collect();

    for obs in best {
        let id = key(&obs.claude_session_id);
        let verified = obs.observed == Some(Observed::Back);
        // The ledger's own verdict, so a CLOSED terminal-only stamp (missing
        // there) is not a bare terminal here either.
        let bare_terminal = obs.observed == Some(Observed::TerminalOnly);
        if !verified && !bare_terminal {
            // Attempted and not landed — accounted for under `missing` below,
            // never as a restore.
            continue;
        }
        if !expected_ids.contains(&id) {
            unexpected.push(CensusUnexpected {
                claude_session_id: obs.claude_session_id.clone(),
                reason: UNEXPECTED_FOREIGN.to_string(),
            });
            continue;
        }
        let row = CensusRestored {
            claude_session_id: obs.claude_session_id.clone(),
            terminal_id: bound_terminal_id(&obs.terminal_id),
            zone_index: obs.zone_index,
            restore_tier: obs.restore_tier.clone(),
            restored_from_boot_at: obs.restored_from_boot_at,
        };
        if verified {
            restored.push(row);
        } else {
            terminal_only.push(row);
        }
    }

    // Scoped so the `&str`s borrowed out of `restored` are definitively dead
    // before the sort below takes it mutably — the intent, spelled, rather than
    // left to NLL inference.
    let mut missing: Vec<CensusMissing> = {
        let restored_ids: HashSet<String> = restored
            .iter()
            .chain(&terminal_only)
            .map(|r| key(&r.claude_session_id))
            .collect();
        expected
            .iter()
            .filter(|e| !restored_ids.contains(&key(&e.claude_session_id)))
            .map(|e| CensusMissing {
                claude_session_id: e.claude_session_id.clone(),
                reason: if unreadable.contains(&key(&e.claude_session_id)) {
                    MISSING_PROCESS_READ_UNAVAILABLE.to_string()
                } else if !e.restorable {
                    MISSING_NOT_RESTORABLE.to_string()
                } else if attempted.contains(&key(&e.claude_session_id)) {
                    MISSING_RESUME_FAILED.to_string()
                } else {
                    MISSING_NO_ATTEMPT.to_string()
                },
            })
            .collect()
    };

    restored.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    terminal_only.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    missing.sort_by(|a, b| a.claude_session_id.cmp(&b.claude_session_id));
    unexpected.sort_by(|a, b| {
        a.claude_session_id
            .cmp(&b.claude_session_id)
            .then_with(|| a.reason.cmp(&b.reason))
    });

    let any_unreadable = missing
        .iter()
        .any(|m| m.reason == MISSING_PROCESS_READ_UNAVAILABLE);
    let unknown_why = if clean_shutdown != Some(true) {
        Some(unknown_reason(clean_shutdown))
    } else if let Some(gap) = live_gap {
        Some(gap.reason())
    } else if any_unreadable {
        Some("process_read_unavailable")
    } else {
        None
    };
    let verdict = if unknown_why.is_some() {
        RESTORE_VERDICT_UNKNOWN
    } else if missing.is_empty() && unexpected.is_empty() && terminal_only.is_empty() {
        RESTORE_VERDICT_MATCH
    } else if !restored.is_empty() {
        RESTORE_VERDICT_PARTIAL
    } else {
        RESTORE_VERDICT_MISMATCH
    };
    CensusDiff {
        restored,
        terminal_only,
        missing,
        unexpected,
        verdict: verdict.to_string(),
        unknown_reason: unknown_why.map(str::to_string),
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

/// Observations of every record carrying a boot-restore stamp from THIS boot,
/// plus every `open` row this boot ADOPTED whose live pane's process read
/// verifies it (no stamp: a holder pane that survived the restart, its
/// `claude` still running). Each is judged by the ledger's per-row back
/// predicate ([`crate::session::session_ledger::observe_row`]) over `live`,
/// so the two surfaces count the same rows back.
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
    live: Option<&LiveView>,
) -> Vec<RestoredObservation> {
    let mut out: Vec<RestoredObservation> = store
        .all_records()
        .into_iter()
        .filter_map(|r| {
            let stamped = r.restored_from_boot_at.is_some_and(|at| at >= boot_at_ms);
            let observed = observe_row(&r, Some(boot_at_ms), live);
            // An unstamped row counts only when the predicate has something
            // to say of THIS boot's pane: it is back, or it cannot be read.
            let adopted = matches!(observed, Some(Observed::Back | Observed::Unknown));
            (stamped || adopted).then(|| RestoredObservation {
                claude_session_id: r.claude_session_id,
                terminal_id: r.terminal_id,
                zone_index: r.zone_index,
                restore_tier: r.restore_tier,
                restored_from_boot_at: r.restored_from_boot_at,
                observed,
                open: r.state == "open",
            })
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
///
/// `live` is the same [`LiveView`] the session ledger reads, or the
/// [`LiveViewGap`] that kept it from being read (only `resumed` stamps can
/// then verify, and the verdict names the gap).
pub fn build_census(
    store: &SessionLifecycleStore,
    boot: Option<BootClassification>,
    now_ms: i64,
    live: Result<&LiveView, LiveViewGap>,
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
            terminal_only: Vec::new(),
            missing: Vec::new(),
            unexpected: Vec::new(),
            verdict: RESTORE_VERDICT_UNKNOWN.to_string(),
        };
    };
    let observed = observe_restored(store, census.boot_at_ms, live.ok());
    let diff = diff_census(&census.expected, &observed, clean_shutdown, live.err());
    let reason = diff.unknown_reason.clone();
    RestoreCensusResponse {
        status: STATUS_OK.to_string(),
        reason,
        generated_at: now_ms,
        shutdown_at,
        clean_shutdown,
        expected: census.expected.clone(),
        restored: diff.restored,
        terminal_only: diff.terminal_only,
        missing: diff.missing,
        unexpected: diff.unexpected,
        verdict: diff.verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::{
        RESTORE_TIER_FAILED, RESTORE_TIER_RESUMED, RESTORE_TIER_TERMINAL_ONLY,
    };

    fn expected(id: &str, restorable: bool) -> CensusExpected {
        CensusExpected {
            claude_session_id: id.to_string(),
            terminal_id: Some(format!("term-{id}")),
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
            open: true,
            // What the ledger's stamp arm says of a row with no live pane.
            observed: Some(match tier {
                RESTORE_TIER_RESUMED => Observed::Back,
                RESTORE_TIER_TERMINAL_ONLY => Observed::TerminalOnly,
                _ => Observed::Failed,
            }),
        }
    }

    #[test]
    fn exact_match_after_a_clean_shutdown() {
        let exp = [expected("a", true), expected("b", true)];
        let obs = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("b", RESTORE_TIER_RESUMED),
        ];
        let d = diff_census(&exp, &obs, Some(true), None);
        assert_eq!(d.restored.len(), 2);
        assert!(d.terminal_only.is_empty());
        assert!(d.missing.is_empty());
        assert!(d.unexpected.is_empty());
        assert_eq!(d.verdict, RESTORE_VERDICT_MATCH);
    }

    /// A terminal that came back without its conversation is not a restore:
    /// counted on its own, never in `restored`, never `missing`, and it keeps
    /// the verdict off `match` (plan
    /// `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`, Phase 1).
    #[test]
    fn a_terminal_only_restore_is_counted_apart_from_restored() {
        let exp = [expected("a", true), expected("b", false)];
        let obs = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("b", RESTORE_TIER_TERMINAL_ONLY),
        ];
        let d = diff_census(&exp, &obs, Some(true), None);
        assert_eq!(d.restored.len(), 1);
        assert_eq!(d.restored[0].claude_session_id, "a");
        assert_eq!(d.terminal_only.len(), 1);
        assert_eq!(d.terminal_only[0].claude_session_id, "b");
        assert!(d.missing.is_empty(), "{:?}", d.missing);
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
        // Only terminals came back: nothing expected was restored.
        let only = diff_census(&exp[1..], &obs[1..], Some(true), None);
        assert_eq!(only.verdict, RESTORE_VERDICT_MISMATCH);
    }

    #[test]
    fn a_missing_session_is_partial_with_a_reason() {
        let exp = [expected("a", true), expected("b", true)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(true), None);
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
        let d = diff_census(&exp, &obs, Some(true), None);
        assert_eq!(d.missing[0].claude_session_id, "phantom");
        assert_eq!(d.missing[0].reason, MISSING_NOT_RESTORABLE);
        assert_eq!(d.verdict, RESTORE_VERDICT_PARTIAL);
    }

    #[test]
    fn a_failed_resume_is_missing_resume_failed_never_a_restore() {
        let exp = [expected("a", true)];
        let obs = [observed("a", RESTORE_TIER_FAILED)];
        let d = diff_census(&exp, &obs, Some(true), None);
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
        let d = diff_census(&exp, &obs, Some(true), None);
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
        let d = diff_census(&exp, &obs, Some(true), None);
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

    /// The census states no outcome it could not read: an expected row whose
    /// pane read was unavailable, or no live view at all, is `unknown` with
    /// its reason — never `match`, and never a confident `resume-failed`.
    #[test]
    fn an_unreadable_pane_or_no_live_view_makes_the_census_unknown() {
        let exp = [expected("a", true), expected("b", true)];
        let mut unread = observed("b", RESTORE_TIER_FAILED);
        unread.observed = Some(Observed::Unknown);
        let d = diff_census(
            &exp,
            &[observed("a", RESTORE_TIER_RESUMED), unread],
            Some(true),
            None,
        );
        assert_eq!(d.restored.len(), 1);
        assert_eq!(d.missing[0].reason, MISSING_PROCESS_READ_UNAVAILABLE);
        assert_eq!(d.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(
            d.unknown_reason.as_deref(),
            Some("process_read_unavailable")
        );

        let all = [
            observed("a", RESTORE_TIER_RESUMED),
            observed("b", RESTORE_TIER_RESUMED),
        ];
        let blind = diff_census(&exp, &all, Some(true), Some(LiveViewGap::Unavailable));
        assert_eq!(blind.restored.len(), 2, "the evidence is still shown");
        assert_eq!(blind.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(
            blind.unknown_reason.as_deref(),
            Some("live_view_unavailable")
        );
        let seen = diff_census(&exp, &all, Some(true), None);
        assert_eq!(seen.verdict, RESTORE_VERDICT_MATCH);
        assert_eq!(seen.unknown_reason, None);
    }

    /// One session, two rows: a closed row this boot stamped and an open row
    /// it adopted with its claude verified. The census keeps the strongest
    /// observation, as the ledger does, so the session reads restored — and
    /// one verified row is not a duplicate.
    #[test]
    fn a_session_is_merged_to_its_strongest_row_before_bucketing() {
        let exp = [expected("a", true)];
        let mut closed = observed("a", RESTORE_TIER_FAILED);
        closed.observed = Some(Observed::StampedClosed);
        let mut adopted = observed("a", RESTORE_TIER_RESUMED);
        adopted.restore_tier = None;
        adopted.restored_from_boot_at = None;
        let d = diff_census(&exp, &[closed, adopted], Some(true), None);
        assert_eq!(d.restored.len(), 1, "{d:?}");
        assert!(d.missing.is_empty());
        assert!(d.unexpected.is_empty());
        assert_eq!(d.verdict, RESTORE_VERDICT_MATCH);
    }

    /// A respawn beside a live session whose second tile sat bare is still
    /// a duplicate; ids compare case-insensitively, as on the ledger.
    #[test]
    fn a_second_tile_and_a_case_variant_id_are_judged_like_the_ledger() {
        let exp = [expected("abc", true)];
        let live = observed("abc", RESTORE_TIER_RESUMED);
        let bare = observed("ABC", RESTORE_TIER_TERMINAL_ONLY);
        let d = diff_census(&exp, &[live, bare], Some(true), None);
        assert_eq!(d.restored.len(), 1, "{d:?}");
        assert_eq!(d.unexpected.len(), 1, "{d:?}");
        assert_eq!(d.unexpected[0].reason, UNEXPECTED_DUPLICATE);
        // A case-variant id alone is the expected session, not a foreigner.
        let upper = observed("ABC", RESTORE_TIER_RESUMED);
        let d = diff_census(&exp, &[upper], Some(true), None);
        assert_eq!(d.restored.len(), 1, "{d:?}");
        assert!(d.unexpected.is_empty() && d.missing.is_empty(), "{d:?}");
        assert_eq!(d.verdict, RESTORE_VERDICT_MATCH);
    }

    /// A row resumed this boot and since CLOSED, then a new open row for the
    /// same session verified back: one tile, not a duplicate — `match`.
    #[test]
    fn a_closed_resumed_row_beside_its_open_successor_is_not_a_duplicate() {
        let exp = [expected("a", true)];
        let mut closed = observed("a", RESTORE_TIER_RESUMED);
        closed.open = false;
        let mut successor = observed("a", RESTORE_TIER_RESUMED);
        successor.restore_tier = None;
        successor.restored_from_boot_at = None;
        let d = diff_census(&exp, &[closed, successor], Some(true), None);
        assert!(d.unexpected.is_empty(), "{d:?}");
        assert_eq!(d.restored.len(), 1);
        assert_eq!(d.verdict, RESTORE_VERDICT_MATCH);
    }

    /// A live view that TIMED OUT is reported as a timeout, not as an
    /// unavailable terminal manager — the reason the warn! line names.
    #[test]
    fn a_live_view_timeout_is_named_as_a_timeout() {
        let exp = [expected("a", true)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(true), Some(LiveViewGap::TimedOut));
        assert_eq!(d.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(d.unknown_reason.as_deref(), Some("live_view_timeout"));
        let d = diff_census(&exp, &obs, Some(true), Some(LiveViewGap::Unavailable));
        assert_eq!(d.unknown_reason.as_deref(), Some("live_view_unavailable"));
    }

    /// T10, as a pure test: a hard-crash boot must NEVER claim `match`.
    #[test]
    fn no_clean_shutdown_marker_is_unknown_even_when_everything_came_back() {
        let exp = [expected("a", true)];
        let obs = [observed("a", RESTORE_TIER_RESUMED)];
        let d = diff_census(&exp, &obs, Some(false), None);
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
        let d = diff_census(&[], &[], None, None);
        assert_eq!(d.verdict, RESTORE_VERDICT_UNKNOWN);
        assert_eq!(unknown_reason(None), "boot_not_classified");
    }

    /// The empty-`expected` discrimination, stated as its own test because it
    /// is the exact failure the plan calls out: an empty set is only `match`
    /// when a marker PROVES the runner had zero sessions.
    #[test]
    fn empty_expected_is_unknown_without_a_marker_and_match_with_one() {
        let crashed = diff_census(&[], &[], Some(false), None).verdict;
        assert_eq!(
            crashed, RESTORE_VERDICT_UNKNOWN,
            "an empty census after a crash proves nothing"
        );
        let clean = diff_census(&[], &[], Some(true), None).verdict;
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
            awaiting_account_since: None,
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
            wind_down_outcome: None,
            wind_down_at: None,
            wind_down_arm: None,
            finish_reason: None,
            finish_synced: false,
            spawn_device_default: None,
            adopted_from: None,
            last_session_start_at: None,
            gate_id: None,
            gate_consuming_device_id: None,
            gate_bound_boot_ms: None,
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

    /// The census half of plan
    /// `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`:
    /// a cohort whose liveness touches starved (rows spread T..T+39m, never
    /// touched after T) and a clean marker at T+77m. The boot census must
    /// expect all five, not zero — it reads the same admission the restore
    /// read does — with the observation epoch the boot attaches first.
    #[test]
    fn boot_expected_sees_a_snapshot_starved_cohort() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terminal-sessions.json");
        let t = 1_700_000_000_000_i64;
        let ids = [
            "starved-0000",
            "starved-0010",
            "starved-0020",
            "starved-0030",
            "starved-0039",
        ];
        let mut map = std::collections::HashMap::new();
        for (m, id) in [0_i64, 10, 20, 30, 39].iter().zip(ids) {
            let mut r = record(id, true);
            r.last_seen_at = t + m * 60_000;
            r.opened_at = r.last_seen_at - 1_000;
            map.insert(id.to_string(), r);
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&map).unwrap()).unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();
        let boot_at = t + 3 * 3_600_000;
        store.set_boot_ms_for_test(boot_at);
        // The prior process last SAW the process table at T (its snapshots
        // failed afterwards); main.rs attaches the epoch before the latch.
        let epoch_path = dir
            .path()
            .join(crate::session::observation_epoch::EPOCH_FILE_NAME);
        crate::session::observation_epoch::ObservationEpoch::open(&epoch_path, t - 3_600_000)
            .record_ok(t);
        store.attach_observation_epoch(std::sync::Arc::new(
            crate::session::observation_epoch::ObservationEpoch::open(&epoch_path, boot_at),
        ));
        // ...and the teardown bit from the same classification the latch
        // reads: admission takes it from the store, not from the argument.
        store.attach_prior_teardown_complete(false);

        let rows = boot_expected(
            &store,
            &FakeProbe {
                with_transcripts: vec![],
            },
            Some(BootClassification {
                crash_recovery: false,
                prior_marker_at: Some(t + 77 * 60_000),
                booted_at_ms: boot_at,
                // The incident's marker: stamped clean, teardown cut short.
                teardown_complete: false,
            }),
        );
        let got: Vec<&str> = rows.iter().map(|r| r.claude_session_id.as_str()).collect();
        assert_eq!(
            got,
            ids.to_vec(),
            "the census expects the whole starved cohort"
        );
    }

    /// Review fixes 6, item 5: an unbound row (blank terminal id) emits NO
    /// `terminalId` — in `expected` and in `restored` — consistent with
    /// `past_sessions`; a bound row still carries it.
    #[test]
    fn a_blank_terminal_id_is_omitted_from_the_census() {
        let probe = FakeProbe {
            with_transcripts: vec![],
        };
        let mut held = record("held", true);
        held.terminal_id = String::new();
        let rows = expected_rows(vec![held, record("bound", true)], &probe);
        assert_eq!(rows[0].terminal_id.as_deref(), Some("term-bound"));
        assert_eq!(rows[1].terminal_id, None);
        let json = serde_json::to_value(&rows[1]).unwrap();
        assert!(
            json.get("terminalId").is_none(),
            "omitted, not \"\": {json}"
        );
        assert_eq!(
            serde_json::to_value(&rows[0]).unwrap()["terminalId"],
            "term-bound"
        );

        let exp = [expected("a", true)];
        let mut obs = observed("a", RESTORE_TIER_RESUMED);
        obs.terminal_id = "  ".to_string();
        let d = diff_census(&exp, &[obs], Some(true), None);
        assert_eq!(d.restored.len(), 1);
        assert_eq!(d.restored[0].terminal_id, None);
        let json = serde_json::to_value(&d.restored[0]).unwrap();
        assert!(json.get("terminalId").is_none(), "omitted: {json}");
    }
}
