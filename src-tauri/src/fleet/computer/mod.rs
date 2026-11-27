//! The fleet computer as a first-class entity — runner-side collection (plan
//! `2026-09-30-the-fleet-machine-is-not-a-first-class-coord-entity-and-coord-has-no-resource-model`,
//! Phase 2, plus the runner half of Phase 6).
//!
//! Each tick of the already-gated sampler loop, the runner observes the
//! computer it runs on (and, on Windows, every RUNNING WSL guest), derives
//! events, and POSTs `POST /coord/computers/report`:
//!
//! * a **full snapshot** every [`REPORT_FULL_SECS`] (identity, static capacity,
//!   every watched service, `services_complete_kinds` naming each kind whose
//!   every hosting source answered — contract amendment A2);
//! * a **delta** on the next tick after a watched service changes state, or
//!   whenever events are waiting.
//!
//! ## Why this has no timer and no gate of its own
//!
//! Same reason as `resource_sample`: it is ticked from inside
//! [`crate::fleet::spawn_budget_republisher`], which returns before spawning
//! anything on an instance that does not own shared machine-wide state. Every
//! runner on a box is the same computer; a secondary reporting it would be a
//! second writer of the same rows describing nothing different. The rule is
//! keyed in one place, and `a_secondary_never_reports_computers` pins it.
//!
//! ## An older coord
//!
//! Until coord's Phase 3 route deploys, the POST answers 404/405 (or 503
//! `schema_pending` before its migration is applied). That is expected,
//! logged ONCE at info, and backs off to the full-snapshot cadence; events stay
//! queued (bounded) and are delivered once the route exists. The sampler is
//! never blocked by it.
//!
//! ## coord's validation is the source of truth
//!
//! coord refuses a report WHOLE (422) on any invalid member, so every value
//! coord validates is pre-checked here with coord's own rule (hostname, access
//! facts, machine-id shape, the report bounds). A 422 that still happens is
//! logged (again whenever the problem list changes); the events it names are
//! quarantined behind one folded `telemetry_gap` marker; a refusal naming no
//! event is retried as a stripped report (no services/hostname/access/free
//! text), and only if THAT is refused too is the queue quarantined — nothing
//! can wedge the reporter. A 200 whose outcome says `identity_conflict` keeps
//! that computer's events queued (coord did not store them). 401/403 back off
//! to the full cadence; other 5xx back off exponentially (30 s → 30 min).

pub(crate) mod access;
pub(crate) mod events;
pub(crate) mod identity;
pub(crate) mod journal_oom;
pub(crate) mod services;
pub(crate) mod services_bound;
pub(crate) mod slice_oom;
pub(crate) mod wsl_guest;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::Serialize;
use tracing::{debug, info, warn};

use self::access::Access;
use self::events::{ComputerEvent, ObsInput, ObserverState};
use self::identity::StaticFacts;
use self::services::{ServiceRow, ServiceScan, WatchedUnit};

/// Full-snapshot cadence (contract §3: every 300 s).
pub(crate) const REPORT_FULL_SECS: u64 = 300;

/// HTTP timeout for one report POST.
const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// One computer on the wire (contract §3 `computers[]`). Every field is
/// always present; an unknown one is `null`, never a fabricated zero.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ComputerReport {
    pub(crate) identity_hash: String,
    pub(crate) kind: String,
    pub(crate) parent_identity_hash: Option<String>,
    pub(crate) attach_device: bool,
    pub(crate) hostname: Option<String>,
    pub(crate) os: Option<String>,
    pub(crate) os_version: Option<String>,
    pub(crate) kernel: Option<String>,
    pub(crate) arch: Option<String>,
    pub(crate) cpu_cores: Option<i32>,
    pub(crate) memory_total_bytes: Option<u64>,
    pub(crate) swap_total_bytes: Option<u64>,
    pub(crate) disk_total_bytes: Option<u64>,
    /// Not measured by any runner probe yet — `null` = UNKNOWN, not "none".
    pub(crate) gpus: Option<serde_json::Value>,
    pub(crate) boot_id: Option<String>,
    pub(crate) booted_at: Option<String>,
    pub(crate) access: Option<Access>,
    /// `null` = no service information in this report.
    pub(crate) services: Option<Vec<ServiceRow>>,
    /// Contract amendment A2: for each kind listed, `services` is the
    /// COMPLETE set of that kind (coord deletes stored rows of it not
    /// listed); kinds not listed are a delta. Empty = pure delta.
    pub(crate) services_complete_kinds: Vec<String>,
    pub(crate) events: Vec<ComputerEvent>,
}

/// `POST /coord/computers/report` body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ComputerReportReq {
    pub(crate) device_id: String,
    pub(crate) computers: Vec<ComputerReport>,
}

/// One computer observed this tick, before the full/delta decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Observed {
    pub(crate) base: ComputerReport,
    pub(crate) oom_kill_total: Option<u64>,
    pub(crate) scan: ServiceScan,
    /// The host's OOM-placement reads (`slice_oom`), taken before the derive;
    /// empty for a guest.
    pub(crate) oom: slice_oom::OomReads,
}

/// The deriver's view of one observed computer. PURE.
pub(crate) fn obs_input(o: &Observed) -> ObsInput<'_> {
    ObsInput {
        boot_id: o.base.boot_id.as_deref(),
        booted_at: o.base.booted_at.as_deref(),
        oom_kill_total: o.oom_kill_total,
        services: o.scan.units.as_deref(),
        complete_kinds: &o.scan.complete_kinds,
        // A guest's VM-wide counter moves between guests (see
        // `wsl_guest::attribute_vm_oom_once`); a guest that is not the
        // owner this tick must not keep a baseline it no longer tracks.
        carry_oom_baseline: o.base.kind != "wsl_guest" || o.base.attach_device,
        slices: o.oom.slices.as_ref(),
        journal: o.oom.journal.as_ref(),
    }
}

/// The part of a row whose change triggers an immediate delta. `memory_peak`
/// is deliberately NOT here: it moves on every tick of a busy unit and would
/// turn the delta path into a 30 s full report.
fn state_key(
    r: &ServiceRow,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u32>,
    Option<String>,
) {
    (
        r.active_state.clone(),
        r.sub_state.clone(),
        r.result.clone(),
        r.n_restarts,
        r.state_changed_at.clone(),
    )
}

type StateKey = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u32>,
    Option<String>,
);

/// Build the host computer's report skeleton. PURE.
pub(crate) fn host_report(
    identity_hash: String,
    kind: &str,
    facts: &StaticFacts,
    boot_id: Option<String>,
    access: Option<Access>,
) -> ComputerReport {
    ComputerReport {
        identity_hash,
        kind: kind.to_string(),
        parent_identity_hash: None,
        attach_device: true,
        hostname: identity::safe_hostname(facts.hostname.clone()),
        os: facts.os.clone(),
        os_version: facts.os_version.clone(),
        kernel: facts.kernel.clone(),
        arch: facts.arch.clone(),
        cpu_cores: facts.cpu_cores,
        memory_total_bytes: facts.memory_total_bytes,
        swap_total_bytes: facts.swap_total_bytes,
        disk_total_bytes: facts.disk_total_bytes,
        gpus: None,
        boot_id,
        booted_at: facts.booted_at.clone(),
        access,
        services: None,
        services_complete_kinds: Vec::new(),
        events: Vec::new(),
    }
}

/// Build a WSL guest's [`Observed`] from its probe. PURE.
pub(crate) fn guest_observed(g: &wsl_guest::GuestProbe, parent: &str) -> Observed {
    let units: Vec<WatchedUnit> = g
        .units
        .iter()
        .map(|p| {
            let runner = g
                .runner_files
                .get(&p.unit)
                .and_then(|j| services::parse_runner_file(j));
            WatchedUnit {
                row: services::row_from_props(p, runner),
                cgroup_oom_kill: None,
                control_group: p.control_group.clone(),
            }
        })
        .collect();
    Observed {
        base: ComputerReport {
            identity_hash: g.identity_hash.clone(),
            kind: "wsl_guest".to_string(),
            parent_identity_hash: Some(parent.to_string()),
            attach_device: false,
            hostname: identity::safe_hostname(g.hostname.clone()),
            os: g.os.clone(),
            os_version: g.os_version.clone(),
            kernel: g.kernel.clone(),
            arch: g.arch.clone(),
            cpu_cores: g.cpu_cores,
            memory_total_bytes: g.memory_total_bytes,
            swap_total_bytes: g.swap_total_bytes,
            disk_total_bytes: g.disk_total_bytes,
            gpus: None,
            boot_id: g.boot_id.clone(),
            booted_at: g.booted_at.clone(),
            access: None,
            services: None,
            services_complete_kinds: Vec::new(),
            events: Vec::new(),
        },
        oom_kill_total: g.oom_kill_total,
        oom: slice_oom::OomReads::default(),
        // Without systemd as PID 1 there was no one to ask: no information,
        // not "no units".
        //
        // Per-kind completeness (amendment A2): the host lists the guest's
        // `actions.runner.*` SYSTEM units (loaded units ∪ unit files), and that
        // kind is complete only when `GuestProbe::inventory_complete()` holds —
        // both listings and `systemctl show` exited 0 and `show` returned one
        // `Id=` record per listed unit. It is NEVER complete for
        // `qontinui_runner`: the host cannot see the guest's user bus, and the
        // guest's own runner reports those rows.
        //
        // And when a runner is ACTIVE inside the guest, the host claims no
        // completeness at all: that runner is the authority for the guest's
        // inventory, and two complete-inventory reporters for one computer
        // would flap on any residual difference between their views.
        scan: if g.systemd {
            ServiceScan {
                units: Some(units),
                complete_kinds: if g.inventory_complete() && g.runner_active != Some(true) {
                    services::GUEST_COMPLETABLE_KINDS.to_vec()
                } else {
                    Vec::new()
                },
                // While a runner reports from inside the guest, the host's
                // probe is not the inventory's authority and can complete
                // nothing: no kind may be held over waiting for it.
                completable_kinds: if g.runner_active == Some(true) {
                    &[]
                } else {
                    services::GUEST_COMPLETABLE_KINDS
                },
            }
        } else {
            ServiceScan::default()
        },
    }
}

/// Decide what one computer contributes to this tick's report. PURE.
///
/// Returns `None` when a delta tick has nothing to say about it.
pub(crate) fn plan_computer(
    obs: &Observed,
    full: bool,
    last_sent: Option<&BTreeMap<String, StateKey>>,
    pending: &[ComputerEvent],
) -> Option<ComputerReport> {
    let mut c = obs.base.clone();
    c.events = pending.to_vec();
    let rows: Option<Vec<ServiceRow>> = obs
        .scan
        .units
        .as_ref()
        .map(|u| u.iter().map(|w| w.row.clone()).collect());
    if full {
        c.services_complete_kinds = if rows.is_some() {
            obs.scan
                .complete_kinds
                .iter()
                .map(|k| k.to_string())
                .collect()
        } else {
            Vec::new()
        };
        c.services = rows;
        return Some(c);
    }
    let changed: Vec<ServiceRow> = rows
        .unwrap_or_default()
        .into_iter()
        .filter(|r| last_sent.and_then(|m| m.get(&r.unit)) != Some(&state_key(r)))
        .collect();
    c.services_complete_kinds = Vec::new();
    c.services = (!changed.is_empty()).then_some(changed);
    (c.services.is_some() || !c.events.is_empty()).then_some(c)
}

/// coord's bounds on one report (`computers.rs` `MAX_*`); a report over them
/// is refused whole, so the reporter never builds one.
pub(crate) const MAX_COMPUTERS_PER_REPORT: usize = 16;
pub(crate) const MAX_EVENTS_PER_COMPUTER: usize = 500;
pub(crate) const MAX_EVENTS_PER_REPORT: usize = 1_000;

/// Keep the first observation of each identity (the host is first), so two
/// cloned WSL distros sharing a machine-id cannot put one identity in a report
/// twice — coord refuses that report whole. Returns the dropped count. PURE.
pub(crate) fn dedupe_observed(observed: Vec<Observed>) -> (Vec<Observed>, usize) {
    let mut seen = std::collections::BTreeSet::new();
    let before = observed.len();
    let kept: Vec<Observed> = observed
        .into_iter()
        .filter(|o| seen.insert(o.base.identity_hash.clone()))
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// Enforce coord's report bounds: at most [`MAX_COMPUTERS_PER_REPORT`]
/// computers (the host is first and therefore always kept), at most
/// [`MAX_EVENTS_PER_COMPUTER`] events each and [`MAX_EVENTS_PER_REPORT`] in
/// all — OLDEST first (the queue is in arrival order); the rest stay queued
/// for the next tick. PURE.
pub(crate) fn cap_report(mut computers: Vec<ComputerReport>) -> Vec<ComputerReport> {
    computers.truncate(MAX_COMPUTERS_PER_REPORT);
    let mut budget = MAX_EVENTS_PER_REPORT;
    for c in &mut computers {
        let take = c.events.len().min(MAX_EVENTS_PER_COMPUTER).min(budget);
        c.events.truncate(take);
        budget -= take;
    }
    computers
}

/// One computer's result in a 200 response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Outcome {
    /// coord did NOT store this computer's services or events.
    pub(crate) identity_conflict: bool,
    /// Events coord dropped as older than its retention window.
    pub(crate) events_expired: u64,
    /// Timestamps coord clamped to its own now because they were ahead of
    /// its clock (contract amendment A3).
    pub(crate) timestamps_clamped: u64,
    /// A `parent_identity_hash` was sent but coord holds no computer with it
    /// in this tenant, so the parent link is UNKNOWN (left unset by coord).
    pub(crate) parent_unresolved: bool,
    /// Keys coord did not recognise and therefore did NOT store — a field this
    /// runner sends that the answering coord does not carry (contract drift).
    pub(crate) unknown_fields: Vec<String>,
}

/// The per-computer outcomes of a 200 response, or `None` when the body is
/// not the documented shape (then every computer counts as delivered, the
/// pre-outcome behaviour). PURE.
pub(crate) fn parse_outcomes(body: &str) -> Option<BTreeMap<String, Outcome>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = v.get("computers")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|c| {
                let id = c.get("identity_hash")?.as_str()?.to_string();
                let o = Outcome {
                    identity_conflict: c
                        .get("identity_conflict")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    events_expired: c
                        .get("events_expired")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    timestamps_clamped: c
                        .get("timestamps_clamped")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    parent_unresolved: c
                        .get("parent_unresolved")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    unknown_fields: c
                        .get("unknown_fields")
                        .and_then(serde_json::Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|k| k.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                Some((id, o))
            })
            .collect(),
    )
}

/// coord's 422 `problems` list. PURE.
pub(crate) fn parse_problems(body: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("problems")?.as_array().map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// The events a 422 names (`computers[i].events[j]…`), as
/// `identity_hash → client_event_ids`, resolved against the body that was
/// sent. A problem that names no event (a computer- or service-level field)
/// contributes nothing. PURE.
pub(crate) fn rejected_events(
    problems: &[String],
    sent: &ComputerReportReq,
) -> BTreeMap<String, Vec<String>> {
    static RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Some(re) = RE
        .get_or_init(|| regex::Regex::new(r"computers\[(\d+)\]\.events\[(\d+)\]").ok())
        .as_ref()
    else {
        return out;
    };
    for p in problems {
        for cap in re.captures_iter(p) {
            let (Some(i), Some(j)) = (
                cap.get(1).and_then(|m| m.as_str().parse::<usize>().ok()),
                cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok()),
            ) else {
                continue;
            };
            if let Some(e) = sent.computers.get(i).and_then(|c| c.events.get(j)) {
                let ids = out
                    .entry(sent.computers[i].identity_hash.clone())
                    .or_default();
                if !ids.contains(&e.client_event_id) {
                    ids.push(e.client_event_id.clone());
                }
            }
        }
    }
    out
}

const REJECTED_REASON: &str = "rejected_by_coord";

fn is_rejection_marker(e: &ComputerEvent) -> bool {
    e.detail.get("reason").and_then(|r| r.as_str()) == Some(REJECTED_REASON)
}

/// What one [`quarantine`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Quarantined {
    /// Real events removed.
    pub(crate) events: usize,
    /// Earlier `rejected_by_coord` markers removed (their counts are folded
    /// into the new marker).
    pub(crate) markers: usize,
}

/// Remove `ids` (all of the queue when `ids` is `None`) from one computer's
/// queue and leave ONE `telemetry_gap` marker saying how many coord refused
/// and why — never a silent drop. An earlier rejection marker that is removed
/// is FOLDED into the new one (its `dropped_events` carried forward), so the
/// running total survives repeated refusals.
pub(crate) fn quarantine(
    state: &mut ObserverState,
    identity: &str,
    ids: Option<&[String]>,
    problems: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Quarantined {
    let Some(q) = state.pending.get_mut(identity) else {
        return Quarantined::default();
    };
    let (gone, kept): (Vec<ComputerEvent>, Vec<ComputerEvent>) =
        std::mem::take(q).into_iter().partition(|e| match ids {
            Some(ids) => ids.contains(&e.client_event_id),
            None => true,
        });
    *q = kept;
    let markers = gone.iter().filter(|e| is_rejection_marker(e)).count();
    let out = Quarantined {
        events: gone.len() - markers,
        markers,
    };
    if gone.is_empty() {
        return out;
    }
    let dropped: u64 = gone
        .iter()
        .map(|e| {
            if is_rejection_marker(e) {
                e.detail
                    .get("dropped_events")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            } else {
                1
            }
        })
        .sum();
    let at = events::rfc3339(now);
    q.push(ComputerEvent {
        client_event_id: events::event_id(
            "telemetry_gap",
            &["rejected", identity, &at, &dropped.to_string()],
        ),
        kind: "telemetry_gap".into(),
        observed_at: at,
        detail: serde_json::json!({
            "reason": REJECTED_REASON,
            "dropped_events": dropped,
            "problems": problems.iter().take(5).collect::<Vec<_>>(),
        }),
    });
    out
}

/// coord's per-field text bound (`MAX_TEXT_LEN`) and unit bound
/// (`MAX_UNIT_LEN`).
pub(crate) const MAX_TEXT_LEN: usize = 512;
pub(crate) const MAX_UNIT_LEN: usize = 256;
/// coord's `MAX_SERVICES_PER_COMPUTER`: an entry listing more services is
/// refused, and the refusal costs the whole report.
pub(crate) const MAX_SERVICES_PER_COMPUTER: usize = 256;

fn too_long(v: &Option<String>) -> bool {
    v.as_ref().is_some_and(|s| s.chars().count() > MAX_TEXT_LEN)
}

/// Pre-apply coord's LENGTH checks, so a value coord would refuse costs that
/// value, never the whole report: a text field over [`MAX_TEXT_LEN`] chars is
/// nulled (a truncated unit or runner name would be a wrong key, not a
/// shorter one); a service whose unit name is over [`MAX_UNIT_LEN`] is
/// dropped; and a list over [`MAX_SERVICES_PER_COMPUTER`] is cut to the bound
/// from its LARGEST kind (the one that overflowed), and only the kinds that
/// lost rows leave `services_complete_kinds` — a truncated kind claimed
/// complete would make coord delete the stored rows it omits, while a kind
/// sent whole keeps its deletions. The cut rows are absent from `last_sent`,
/// so the next delta carries them. Timestamps are NOT checked here: coord is the authority on "now" and clamps a future
/// timestamp itself (contract amendment A3). PURE.
pub(crate) fn presanitize(c: &mut ComputerReport) {
    for f in [
        &mut c.hostname,
        &mut c.os,
        &mut c.os_version,
        &mut c.kernel,
        &mut c.arch,
        &mut c.boot_id,
    ] {
        if too_long(f) {
            *f = None;
        }
    }
    if let Some(rows) = c.services.as_mut() {
        rows.retain(services_bound::sendable);
        let cut = services_bound::cut_to_bound(rows);
        c.services_complete_kinds
            .retain(|k| !cut.contains(k.as_str()));
        for r in rows.iter_mut() {
            for f in [
                &mut r.active_state,
                &mut r.sub_state,
                &mut r.result,
                &mut r.restart_policy,
                &mut r.oom_policy,
                &mut r.runner_name,
                &mut r.repo,
            ] {
                if too_long(f) {
                    *f = None;
                }
            }
        }
    }
}

/// The fallback after a 422 that named no event: the same report with the
/// fields a computer-level refusal can sit in nulled — `services`, `hostname`,
/// `access` and the free-text facts — keeping identity, boot facts and
/// events. If coord refuses THIS too, the queue is quarantined. PURE.
pub(crate) fn stripped(c: &ComputerReport) -> ComputerReport {
    let mut c = c.clone();
    c.services = None;
    c.services_complete_kinds = Vec::new();
    c.hostname = None;
    c.access = None;
    c.os = None;
    c.os_version = None;
    c.kernel = None;
    c.arch = None;
    c
}

/// Exponential back-off for repeated 5xx: 30 s doubling, capped at the
/// full-snapshot cadence ([`REPORT_FULL_SECS`]) — a longer wait would leave a
/// recovered coord without this machine's state for longer than a missed
/// snapshot does. The caller adds ±20% jitter so a fleet that saw one coord
/// outage together does not retry in lockstep.
pub(crate) fn server_error_backoff(consecutive: u32) -> Duration {
    Duration::from_secs((30u64 << consecutive.min(6)).min(REPORT_FULL_SECS))
}

/// A 422 problem's PATH with its value stripped
/// (`computers[0].events[3].observed_at: 2027-… is more than …` →
/// `computers[0].events[3].observed_at`), so a refusal whose VALUE changes
/// every tick is still the same refusal and is not re-logged. PURE.
pub(crate) fn problem_paths(problems: &[String]) -> Vec<String> {
    let mut v: Vec<String> = problems
        .iter()
        .map(|p| p.split(':').next().unwrap_or(p).trim().to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// `telemetry_gap` reasons whose `dropped_events` is a running total.
const MARKER_REASONS: &[&str] = &[
    "pending_event_buffer_full",
    "rejected_by_coord",
    "events_expired",
];

fn marker_total(e: &ComputerEvent) -> Option<u64> {
    let reason = e.detail.get("reason").and_then(|r| r.as_str())?;
    MARKER_REASONS.contains(&reason).then(|| {
        e.detail
            .get("dropped_events")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    })
}

/// The `telemetry_gap` recording that coord dropped `events_expired` of the
/// `sent` events as older than its retention — or `None` when every expired
/// event was itself a marker (re-reporting a lost marker as a new loss would
/// be noise). Earlier markers among the expired are FOLDED: their totals are
/// carried into the new marker, as `quarantine` does.
///
/// coord reports only a count, and it expires strictly by AGE against its
/// own (configurable) retention and its own clock — so the expired set is
/// the `events_expired` OLDEST of `sent` by `observed_at`, never a local
/// retention constant compared against this machine's clock. An
/// `observed_at` that does not parse ranks as the OLDEST (it is the likeliest
/// to have been refused or dropped), and ties keep their order in `sent` (the
/// sort is stable). PURE.
pub(crate) fn expired_marker_for(
    identity: &str,
    sent: &[ComputerEvent],
    events_expired: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ComputerEvent> {
    if events_expired == 0 {
        return None;
    }
    let mut by_age: Vec<&ComputerEvent> = sent.iter().collect();
    by_age.sort_by_key(|e| {
        chrono::DateTime::parse_from_rfc3339(&e.observed_at)
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MIN)
    });
    let n = usize::try_from(events_expired).unwrap_or(usize::MAX);
    let expired: Vec<&ComputerEvent> = by_age.into_iter().take(n).collect();
    let markers: Vec<u64> = expired.iter().filter_map(|e| marker_total(e)).collect();
    let real = events_expired.saturating_sub(markers.len() as u64);
    if real == 0 {
        return None;
    }
    let total = real + markers.iter().sum::<u64>();
    let at = events::rfc3339(now);
    Some(ComputerEvent {
        client_event_id: events::event_id("telemetry_gap", &["expired", identity, &at]),
        kind: "telemetry_gap".into(),
        observed_at: at,
        detail: serde_json::json!({
            "reason": "events_expired",
            "dropped_events": total,
        }),
    })
}

/// The first 12 characters of an identity hash, for a log line. PURE.
fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// The per-process reporter, owned by the sampler loop's closure.
pub(crate) struct Reporter {
    state: Option<ObserverState>,
    state_path: Option<PathBuf>,
    last_full: Option<tokio::time::Instant>,
    /// When static facts and access facts were last collected — separate from
    /// `last_full`, which only advances on a DELIVERED full snapshot, so a
    /// failing coord does not make every tick re-enumerate disks and fork
    /// `tailscale`.
    last_facts_at: Option<tokio::time::Instant>,
    /// Row state keys coord last acknowledged, per identity hash.
    last_sent: BTreeMap<String, BTreeMap<String, StateKey>>,
    /// Backoff after a 404/405/503 (an older coord, or one whose schema is not
    /// applied yet) or an unexplained 422.
    route_absent_until: Option<tokio::time::Instant>,
    route_absent_logged: bool,
    /// The last 422 problem list logged — a CHANGED list is logged again.
    last_problems: Vec<String>,
    expired_logged: bool,
    /// Warned once that coord clamped this machine's future timestamps.
    clamped_logged: bool,
    /// After a 422 naming no event: send stripped reports until this instant.
    strip_until: Option<tokio::time::Instant>,
    /// Consecutive 5xx (other than 503), for exponential back-off.
    server_errors: u32,
    conflict_logged: bool,
    /// Warned once that a guest's parent link did not resolve on coord.
    parent_unresolved_logged: bool,
    /// The last `unknown_fields` set logged — a CHANGED set is logged again.
    last_unknown_fields: Vec<String>,
    /// Per identity hash: the rate limit on its over-bound WARN.
    over_bound_warns: BTreeMap<String, services_bound::WarnLimit>,
    dedupe_logged: bool,
    failures_logged: u32,
    no_identity_logged: bool,
    facts: Option<StaticFacts>,
    access: Access,
    #[cfg(target_os = "linux")]
    watcher: services::linux::Watcher,
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter {
    pub(crate) fn new() -> Self {
        Self {
            state: None,
            state_path: qontinui_runner_lib::ambient::runner_dir()
                .map(|d| d.join("computer-observer.json")),
            last_full: None,
            last_facts_at: None,
            last_sent: BTreeMap::new(),
            route_absent_until: None,
            route_absent_logged: false,
            last_problems: Vec::new(),
            expired_logged: false,
            clamped_logged: false,
            strip_until: None,
            server_errors: 0,
            conflict_logged: false,
            parent_unresolved_logged: false,
            last_unknown_fields: Vec::new(),
            over_bound_warns: BTreeMap::new(),
            dedupe_logged: false,
            failures_logged: 0,
            no_identity_logged: false,
            facts: None,
            access: Access::default(),
            #[cfg(target_os = "linux")]
            watcher: services::linux::Watcher::default(),
        }
    }

    /// Write the observer state. `events::save` fsyncs, so it runs as a
    /// tracked blocking body on a snapshot of the state — never on the
    /// `fleet-heartbeat` thread's single async worker, which also carries the
    /// coord liveness heartbeat. Awaited, so a caller's ordering (persist
    /// before the POST) holds.
    ///
    /// A cancelled tick (a supervised run torn down mid-save) leaves its
    /// blocking write to finish, and that write can land AFTER a rebuilt
    /// reporter's newer save, leaving the older snapshot on disk. The impact
    /// is bounded: each write is whole (its own temp file, fsync, rename), so
    /// the file is one snapshot or the other, never torn; event ids are
    /// content-derived, so an event re-queued from the older snapshot is a
    /// duplicate coord drops, not a second event; and the next tick persists
    /// the live state over it.
    async fn persist(&self) {
        let (Some(path), Some(state)) = (self.state_path.clone(), self.state.clone()) else {
            return;
        };
        match spawn_blocking_tracked(move || events::save(&path, &state)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!("fleet::computer: could not persist observer state: {e}"),
            Err(e) => debug!("fleet::computer: the observer-state write did not finish: {e}"),
        }
    }

    async fn scan_host_services(&mut self) -> ServiceScan {
        #[cfg(target_os = "linux")]
        {
            self.watcher.scan().await
        }
        #[cfg(windows)]
        {
            services::windows::scan().await
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            ServiceScan::default()
        }
    }

    fn back_off(&mut self) {
        self.route_absent_until = Some(
            tokio::time::Instant::now()
                + crate::fleet::resource_sample::jittered_sleep(REPORT_FULL_SECS),
        );
    }

    /// One tick. Best-effort end to end: every failure returns quietly.
    pub(crate) async fn tick(&mut self) {
        let now = chrono::Utc::now();
        // procfs reads: off the async worker, like the sampler's own
        // `collect_host_lane`. A panicked read is an all-UNKNOWN reading.
        let axes = spawn_blocking_tracked(crate::fleet::host_axes::collect)
            .await
            .unwrap_or_default();
        let Some(host_id) = identity::host_identity_hash() else {
            if !self.no_identity_logged {
                self.no_identity_logged = true;
                info!("fleet::computer: no valid OS machine id — this computer cannot be identified, so it is not reported");
            }
            return;
        };
        if self.state.is_none() {
            // A panicked load is a fresh state, as `events::load`'s own
            // fallback for an unreadable file.
            self.state = Some(match self.state_path.clone() {
                Some(path) => spawn_blocking_tracked(move || events::load(&path))
                    .await
                    .unwrap_or_default(),
                None => ObserverState::default(),
            });
        }

        // Loop starvation is a fact about THIS loop, independent of coord, so
        // it is recorded before any coord prerequisite can skip the tick.
        {
            let state = self.state.get_or_insert_with(ObserverState::default);
            if let Some(gap) = events::telemetry_gap(
                state.last_tick_at.as_deref(),
                state.last_tick_boot_id.as_deref(),
                now,
                crate::fleet::resource_sample::sample_interval_secs(),
                axes.boot_id.as_deref(),
                axes.load.map(|l| l.one),
            ) {
                events::push_pending(state, &host_id, vec![gap]);
            }
            state.last_tick_at = Some(events::rfc3339(now));
            state.last_tick_boot_id = axes.boot_id.clone();
        }

        let Some(device) = crate::fleet::load_device_file() else {
            self.persist().await;
            return;
        };
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            self.persist().await;
            return;
        };
        let Some(bearer) = crate::coord_mcp::read_usable_device_jwt().await else {
            self.persist().await;
            return;
        };

        let full = self
            .last_full
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(REPORT_FULL_SECS));
        let facts_due = self
            .last_facts_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(REPORT_FULL_SECS));
        if facts_due || self.facts.is_none() {
            self.facts = spawn_blocking_tracked(identity::collect_static).await.ok();
            self.access = access::collect().await;
            self.last_facts_at = Some(tokio::time::Instant::now());
        }
        let facts = self.facts.clone().unwrap_or_default();

        // ---- observe --------------------------------------------------
        let mut host = Observed {
            base: host_report(
                host_id.clone(),
                identity::host_kind(),
                &facts,
                axes.boot_id.clone(),
                Some(self.access.clone()),
            ),
            oom_kill_total: axes.oom_kill_total,
            scan: self.scan_host_services().await,
            oom: slice_oom::OomReads::default(),
        };
        // The OOM-placement reads run here, before the (sync, pure) derive,
        // against the previous observation.
        let oom = slice_oom::collect(
            self.state.as_ref().and_then(|s| s.computers.get(&host_id)),
            &obs_input(&host),
        )
        .await;
        host.oom = oom;
        let mut observed = vec![host];
        // Guests cost a `wsl.exe` fork each, so they are probed at the
        // full-snapshot cadence — or sooner when one has events waiting.
        #[cfg(windows)]
        {
            let guest_pending = self.state.as_ref().is_some_and(|s| {
                s.pending.iter().any(|(id, q)| {
                    id != &host_id
                        && !q.is_empty()
                        && s.computers.get(id).and_then(|c| c.kind.as_deref()) == Some("wsl_guest")
                })
            });
            if full || guest_pending {
                for g in wsl_guest::probe::collect().await {
                    observed.push(guest_observed(&g, &host_id));
                }
            }
        }
        let (observed, dup) = dedupe_observed(observed);
        if dup > 0 && !self.dedupe_logged {
            self.dedupe_logged = true;
            warn!(
                "fleet::computer: {dup} observed computer(s) share an identity with another \
                 (cloned WSL distros share /etc/machine-id); only the first is reported"
            );
        }

        self.derive_events(&observed, &host_id, now);
        // Persist BEFORE the POST: a crash mid-send must not lose events.
        self.persist().await;

        if let Some(until) = self.route_absent_until {
            if tokio::time::Instant::now() < until {
                return;
            }
        }

        let strip = self
            .strip_until
            .is_some_and(|t| tokio::time::Instant::now() < t);
        let computers = self.plan_report(&observed, full, strip);
        if computers.is_empty() {
            return;
        }
        let body = ComputerReportReq {
            device_id: device.device_id.clone(),
            computers,
        };
        let url = format!("{}/coord/computers/report", base.trim_end_matches('/'));
        let client = match reqwest::Client::builder().timeout(POST_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                debug!("fleet::computer: http client build failed: {e}");
                return;
            }
        };

        // coord-auth-exempt(device-jwt-required): resolves through
        // `coord_mcp::read_usable_device_jwt` above and returns early without
        // one, so a report is skipped rather than sent anonymously.
        let resp = match client
            .post(&url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                self.log_failure(format!("POST {url} failed: {e}"));
                return;
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            200..=299 => {
                debug!(
                    "fleet::computer: reported {} computer(s) ({})",
                    body.computers.len(),
                    if full { "full" } else { "delta" }
                );
                self.route_absent_until = None;
                self.server_errors = 0;
                let outcomes = parse_outcomes(&text);
                self.on_delivered(&body, full, outcomes.as_ref()).await;
            }
            404 | 405 | 503 => {
                if !self.route_absent_logged {
                    self.route_absent_logged = true;
                    info!(
                        "fleet::computer: coord answered HTTP {status} for /coord/computers/report — \
                         the route or its schema is not live on this coord yet; events stay queued \
                         and the report retries every {REPORT_FULL_SECS}s"
                    );
                }
                self.back_off();
            }
            422 => self.on_rejected(&body, &text, now, strip).await,
            401 | 403 => {
                self.log_failure(format!("POST {url} -> HTTP {status} (credential refused)"));
                self.back_off();
            }
            500..=599 => {
                let wait = crate::fleet::resource_sample::jittered_sleep(
                    server_error_backoff(self.server_errors).as_secs(),
                );
                self.server_errors = self.server_errors.saturating_add(1);
                self.log_failure(format!(
                    "POST {url} -> HTTP {status}; backing off {}s",
                    wait.as_secs()
                ));
                self.route_absent_until = Some(tokio::time::Instant::now() + wait);
            }
            _ => self.log_failure(format!("POST {url} -> HTTP {status}")),
        }
    }

    /// Derive this tick's events for every observed computer into the queue:
    /// state transitions from [`events::derive`], and a `services_over_bound`
    /// `telemetry_gap` from each computer's [`services_bound::OverBound`].
    fn derive_events(
        &mut self,
        observed: &[Observed],
        host_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let state = self.state.get_or_insert_with(ObserverState::default);
        for o in observed {
            let id = &o.base.identity_hash;
            let input = obs_input(o);
            let (mut evs, mut next) = events::derive(state.computers.get(id), &input, now);
            next.kind = Some(o.base.kind.clone());
            state.computers.insert(id.clone(), next);

            // Over the bound is decided from the OBSERVED scan — never from a
            // delta's own list, which holds only the rows that changed.
            let observed_rows: Option<Vec<ServiceRow>> = o
                .scan
                .units
                .as_ref()
                .map(|u| u.iter().map(|w| w.row.clone()).collect());
            let cut = observed_rows.as_deref().map(services_bound::services_cut);
            let counts = observed_rows
                .as_deref()
                .map(services_bound::kind_counts)
                .unwrap_or_default();
            let scan = cut.as_ref().map(|cut| services_bound::Scan {
                cut,
                counts: &counts,
                complete_kinds: &o.scan.complete_kinds,
                completable_kinds: o.scan.completable_kinds,
            });
            let previous = state.over_bound.get(id).cloned().unwrap_or_default();
            let (next_bound, actions) = previous.step(scan, now);
            let total: usize = counts.values().sum();
            let scan_counts = observed_rows.is_some().then_some(&counts);
            let limit = self.over_bound_warns.entry(id.clone()).or_default();
            for action in actions {
                match (action, &next_bound) {
                    (services_bound::Action::Emit, services_bound::OverBound::Over(over)) => {
                        evs.push(services_bound::over_bound_event(id, scan_counts, over, now));
                    }
                    (services_bound::Action::Warn, services_bound::OverBound::Over(over))
                        if !limit.admit(&over.kinds, now) =>
                    {
                        debug!(
                            "fleet::computer: computer {} is over coord's bound again with kind(s) \
                             {:?} (warned within the last day; the event records it)",
                            short_id(id),
                            over.kinds,
                        )
                    }
                    (services_bound::Action::Warn, services_bound::OverBound::Over(over)) => {
                        warn!(
                            "fleet::computer: computer {} is over coord's bound of \
                             {MAX_SERVICES_PER_COMPUTER} services ({total} in this scan); full \
                             reports cut kind(s) {:?} to fit, and coord does not apply removals \
                             of those kinds while they stay over (a services_over_bound \
                             telemetry_gap records it); held over because this scan did not see \
                             them completely: {:?}, last vouched counts {:?}",
                            short_id(id),
                            over.kinds,
                            over.held,
                            over.last_counts,
                        )
                    }
                    (services_bound::Action::Clear, _) => {
                        evs.push(services_bound::cleared_event(id, &counts, now));
                    }
                    (services_bound::Action::Info, _) if limit.info_due() => info!(
                        "fleet::computer: computer {} watches {total} services, back within \
                         coord's bound of {MAX_SERVICES_PER_COMPUTER}",
                        short_id(id),
                    ),
                    (services_bound::Action::Info, _) => debug!(
                        "fleet::computer: computer {} is back within coord's bound again",
                        short_id(id),
                    ),
                    // Emit and Warn come only with an Over state.
                    (_, services_bound::OverBound::Within) => {}
                }
            }
            state.over_bound.insert(id.clone(), next_bound);

            if !evs.is_empty() {
                events::push_pending(state, id, evs);
            }
        }
        let pruned = events::prune(state, host_id, now);
        if pruned > 0 {
            debug!("fleet::computer: forgot {pruned} computer(s) unseen for a week");
        }
        // A forgotten computer's over-bound state goes with it.
        let computers = &state.computers;
        state.over_bound.retain(|id, _| computers.contains_key(id));
        self.over_bound_warns
            .retain(|id, _| computers.contains_key(id));
    }

    /// This tick's report, ready to send: each observed computer's full or
    /// delta contribution with its queued events, capped at coord's report
    /// bounds, presanitized, and [`stripped`] while `strip` holds. Empty =
    /// nothing to send this tick.
    fn plan_report(&self, observed: &[Observed], full: bool, strip: bool) -> Vec<ComputerReport> {
        let empty = ObserverState::default();
        let state = self.state.as_ref().unwrap_or(&empty);
        let computers: Vec<ComputerReport> = observed
            .iter()
            .filter_map(|o| {
                let id = &o.base.identity_hash;
                let pending = state.pending.get(id).map(Vec::as_slice).unwrap_or(&[]);
                plan_computer(o, full, self.last_sent.get(id), pending)
            })
            .collect();
        let mut computers = cap_report(computers);
        for c in &mut computers {
            presanitize(c);
        }
        if strip {
            computers = computers.iter().map(stripped).collect();
        }
        computers
    }

    /// coord refused the report (422).
    ///
    /// 1. Events the problems name are quarantined (behind one folded
    ///    `telemetry_gap` marker) and the report is retried next tick.
    /// 2. If the only events named were earlier markers, back off.
    /// 3. If no event is named, the refusal sits in a computer-level field:
    ///    the next reports are sent [`stripped`] for one full cadence.
    /// 4. If a stripped report is ALSO refused, every queue in it is
    ///    quarantined and the reporter backs off — nothing can wedge it.
    async fn on_rejected(
        &mut self,
        body: &ComputerReportReq,
        text: &str,
        now: chrono::DateTime<chrono::Utc>,
        was_stripped: bool,
    ) {
        let problems = parse_problems(text);
        let paths = problem_paths(&problems);
        if paths != self.last_problems {
            warn!(
                "fleet::computer: coord refused the computers report (422): {}",
                problems
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            self.last_problems = paths;
        }
        // coord clamps future timestamps itself (contract amendment A3), so
        // no refusal here is a clock artefact: a named event is quarantined.
        let named = rejected_events(&problems, body);
        let mut q = Quarantined::default();
        if let Some(state) = self.state.as_mut() {
            for (id, ids) in &named {
                let r = quarantine(state, id, Some(ids), &problems, now);
                q.events += r.events;
                q.markers += r.markers;
            }
        }
        if q.events > 0 {
            self.persist().await;
            return;
        }
        if q.markers > 0 {
            self.persist().await;
            self.back_off();
            return;
        }
        if !was_stripped {
            self.strip_until =
                Some(tokio::time::Instant::now() + Duration::from_secs(REPORT_FULL_SECS));
            return;
        }
        if let Some(state) = self.state.as_mut() {
            for c in &body.computers {
                quarantine(state, &c.identity_hash, None, &problems, now);
            }
        }
        self.strip_until = None;
        self.persist().await;
        self.back_off();
    }

    /// coord accepted `body`: drop the delivered events and remember what it
    /// now holds — except for a computer coord reported as an
    /// `identity_conflict`, whose services and events it did NOT store; those
    /// stay queued.
    async fn on_delivered(
        &mut self,
        body: &ComputerReportReq,
        full: bool,
        outcomes: Option<&BTreeMap<String, Outcome>>,
    ) {
        if full {
            self.last_full = Some(tokio::time::Instant::now());
        }
        let mut conflicted = 0;
        let mut expired_total = 0;
        let mut clamped_total = 0;
        let mut parent_unresolved = 0;
        let mut unknown_fields = std::collections::BTreeSet::new();
        let now = chrono::Utc::now();
        if let Some(state) = self.state.as_mut() {
            for c in &body.computers {
                let outcome = outcomes
                    .and_then(|o| o.get(&c.identity_hash))
                    .cloned()
                    .unwrap_or_default();
                unknown_fields.extend(outcome.unknown_fields.iter().cloned());
                if outcome.parent_unresolved {
                    parent_unresolved += 1;
                }
                if outcome.identity_conflict {
                    conflicted += 1;
                    continue;
                }
                clamped_total += outcome.timestamps_clamped;
                if let Some(m) =
                    expired_marker_for(&c.identity_hash, &c.events, outcome.events_expired, now)
                {
                    expired_total += outcome.events_expired;
                    events::push_pending(state, &c.identity_hash, vec![m]);
                }
                if let Some(q) = state.pending.get_mut(&c.identity_hash) {
                    q.retain(|e| {
                        !c.events
                            .iter()
                            .any(|s| s.client_event_id == e.client_event_id)
                    });
                }
                if let Some(rows) = &c.services {
                    let sent = self.last_sent.entry(c.identity_hash.clone()).or_default();
                    // A kind sent complete replaces what coord holds of it.
                    sent.retain(|unit, _| {
                        !c.services_complete_kinds
                            .iter()
                            .any(|k| k == services::classify_unit(unit))
                    });
                    for r in rows {
                        sent.insert(r.unit.clone(), state_key(r));
                    }
                }
            }
            state.pending.retain(|_, q| !q.is_empty());
        }
        if clamped_total > 0 && !self.clamped_logged {
            self.clamped_logged = true;
            warn!(
                "fleet::computer: coord clamped {clamped_total} timestamp(s) that were ahead of its \
                 clock — this machine's clock is ahead of coord's (coord stored them as its own now)"
            );
        }
        if expired_total > 0 && !self.expired_logged {
            self.expired_logged = true;
            warn!(
                "fleet::computer: coord dropped {expired_total} queued event(s) as older than its \
                 retention window — this machine held them too long; a telemetry_gap records it"
            );
        }
        if conflicted > 0 && !self.conflict_logged {
            self.conflict_logged = true;
            warn!(
                "fleet::computer: coord reported an identity_conflict for {conflicted} computer(s) \
                 (another live reporter shares the machine id); their events stay queued"
            );
        }
        if parent_unresolved > 0 && !self.parent_unresolved_logged {
            self.parent_unresolved_logged = true;
            info!(
                "fleet::computer: coord could not resolve the parent of {parent_unresolved} \
                 computer(s) (no computer with that identity in this tenant), so their parent \
                 link is UNKNOWN until the host's own entry is stored"
            );
        }
        let unknown_fields: Vec<String> = unknown_fields.into_iter().collect();
        if unknown_fields != self.last_unknown_fields {
            if !unknown_fields.is_empty() {
                warn!(
                    "fleet::computer: coord did not recognise and did NOT store field(s) \
                     {unknown_fields:?} — this runner sends a field the answering coord does not carry"
                );
            }
            self.last_unknown_fields = unknown_fields;
        }
        self.persist().await;
    }

    fn log_failure(&mut self, msg: String) {
        self.failures_logged = self.failures_logged.saturating_add(1);
        if self.failures_logged <= 3 {
            warn!("fleet::computer: {msg} (best-effort — events stay queued)");
        } else {
            debug!("fleet::computer: {msg} (best-effort — events stay queued)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::services::{parse_systemctl_show, tests as svc};
    use super::*;

    fn facts() -> StaticFacts {
        StaticFacts {
            hostname: Some("fleetbox".into()),
            os: Some("Debian GNU/Linux".into()),
            os_version: Some("13".into()),
            kernel: Some("6.1.0-example-amd64".into()),
            arch: Some("x86_64".into()),
            cpu_cores: Some(16),
            memory_total_bytes: Some(274_877_906_944),
            swap_total_bytes: Some(34_359_738_368),
            disk_total_bytes: Some(1_000_204_886_016),
            booted_at: Some("2026-09-21T14:13:20Z".into()),
        }
    }

    const HOST_ID: &str = "14c5ba9f46f5b062dfae3190c6c7e1d25a6ba40a5043c5c78cb882c3215dc16f";

    fn observed(show: &str) -> Observed {
        Observed {
            base: host_report(
                HOST_ID.into(),
                "host",
                &facts(),
                Some("0f0e0d0c-0b0a-4908-8706-050403020100".into()),
                Some(Access {
                    tailnet_name: Some("fleetbox.tailnet-x.ts.net".into()),
                    tailnet_ip: Some("100.64.0.10".into()),
                    ssh_user: Some("runner".into()),
                }),
            ),
            oom_kill_total: Some(6),
            scan: ServiceScan {
                units: Some(
                    parse_systemctl_show(show)
                        .iter()
                        // No filesystem reads in a fixture test: `.runner`
                        // resolution is exercised by its own parser tests.
                        .map(|p| WatchedUnit {
                            row: services::row_from_props(p, None),
                            cgroup_oom_kill: None,
                            control_group: p.control_group.clone(),
                        })
                        .collect(),
                ),
                complete_kinds: vec!["gh_actions_runner", "qontinui_runner"],
                completable_kinds: services::LINUX_COMPLETABLE_KINDS,
            },
            oom: slice_oom::OomReads::default(),
        }
    }

    /// The full wire document for the incident, pinned literally — the
    /// contract §3 names, the null-not-zero unknowns, and the attributed OOM
    /// event, all from fixtures.
    #[test]
    fn a_full_report_built_from_the_incident_fixtures_is_the_contract_shape() {
        let obs = observed(svc::INCIDENT_SHOW);
        let before = observed(
            &svc::INCIDENT_SHOW
                .replace("ActiveState=failed", "ActiveState=active")
                .replace("SubState=failed", "SubState=running")
                .replace("Result=oom-kill", "Result=success")
                .replace("Wed 2026-09-30 01:48:16 UTC", "Tue 2026-09-29 09:12:40 UTC"),
        );
        let t = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        fn inp(o: &Observed, oom: u64) -> ObsInput<'_> {
            ObsInput {
                boot_id: o.base.boot_id.as_deref(),
                booted_at: o.base.booted_at.as_deref(),
                oom_kill_total: Some(oom),
                services: o.scan.units.as_deref(),
                complete_kinds: &o.scan.complete_kinds,
                carry_oom_baseline: true,
                slices: None,
                journal: None,
            }
        }
        let (_, obs0) = events::derive(None, &inp(&before, 5), t("2026-09-30T01:40:00Z"));
        let (evs, _) = events::derive(Some(&obs0), &inp(&obs, 6), t("2026-09-30T01:48:40Z"));

        let c = plan_computer(&obs, true, None, &evs).unwrap();
        let req = ComputerReportReq {
            device_id: "0a1b2c3d-0000-4000-8000-000000000001".into(),
            computers: vec![c],
        };
        let json = serde_json::to_value(&req).unwrap();
        let oom_id = events::event_id(
            "oom_kill",
            &[
                "0f0e0d0c-0b0a-4908-8706-050403020100",
                "actions.runner.example-org-example-repo.fleetbox.service",
                "unit_result",
                "2026-09-30T01:48:16Z",
            ],
        );
        let failed_id = events::event_id(
            "service_failed",
            &[
                "0f0e0d0c-0b0a-4908-8706-050403020100",
                "actions.runner.example-org-example-repo.fleetbox.service",
                "2026-09-30T01:48:16Z",
            ],
        );
        let expected = serde_json::json!({
            "device_id": "0a1b2c3d-0000-4000-8000-000000000001",
            "computers": [{
                "identity_hash": HOST_ID,
                "kind": "host",
                "parent_identity_hash": null,
                "attach_device": true,
                "hostname": "fleetbox",
                "os": "Debian GNU/Linux",
                "os_version": "13",
                "kernel": "6.1.0-example-amd64",
                "arch": "x86_64",
                "cpu_cores": 16,
                "memory_total_bytes": 274877906944_u64,
                "swap_total_bytes": 34359738368_u64,
                "disk_total_bytes": 1000204886016_u64,
                "gpus": null,
                "boot_id": "0f0e0d0c-0b0a-4908-8706-050403020100",
                "booted_at": "2026-09-21T14:13:20Z",
                "access": {
                    "tailnet_name": "fleetbox.tailnet-x.ts.net",
                    "tailnet_ip": "100.64.0.10",
                    "ssh_user": "runner"
                },
                "services": [{
                    "unit": "actions.runner.example-org-example-repo.fleetbox.service",
                    "kind": "gh_actions_runner",
                    "active_state": "failed",
                    "sub_state": "failed",
                    "result": "oom-kill",
                    "restart_policy": "no",
                    "oom_policy": "stop",
                    "memory_max": null,
                    "memory_peak": 21474836480_u64,
                    "n_restarts": 0,
                    "state_changed_at": "2026-09-30T01:48:16Z",
                    "runner_name": "fleetbox",
                    "repo": null
                }],
                "services_complete_kinds": ["gh_actions_runner", "qontinui_runner"],
                "events": [
                    {
                        "client_event_id": oom_id,
                        "kind": "oom_kill",
                        "observed_at": "2026-09-30T01:48:16Z",
                        "detail": {
                            "victim_unit": "actions.runner.example-org-example-repo.fleetbox.service",
                            "unit_kind": "gh_actions_runner",
                            "count": 1,
                            "attribution": "unit_result",
                            "oom_kill_total": 6,
                            "oom_policy": "stop",
                            "memory_peak": 21474836480_u64,
                            "runner_name": "fleetbox",
                            "victim_slice": null,
                            "slices": "not_supported"
                        }
                    },
                    {
                        "client_event_id": failed_id,
                        "kind": "service_failed",
                        "observed_at": "2026-09-30T01:48:16Z",
                        "detail": {
                            "unit": "actions.runner.example-org-example-repo.fleetbox.service",
                            "unit_kind": "gh_actions_runner",
                            "from": "active",
                            "to": "failed",
                            "sub_state": "failed",
                            "result": "oom-kill",
                            "restart_policy": "no",
                            "oom_policy": "stop",
                            "n_restarts": 0,
                            "runner_name": "fleetbox",
                            "event": "service_failed"
                        }
                    }
                ]
            }]
        });
        assert_eq!(
            json,
            expected,
            "{}",
            serde_json::to_string_pretty(&json).unwrap()
        );
    }

    #[test]
    fn a_delta_carries_only_changed_rows_and_never_claims_completeness() {
        let obs = observed(svc::HEALTHY_SHOW);
        let sent: BTreeMap<String, StateKey> = obs
            .scan
            .units
            .as_ref()
            .unwrap()
            .iter()
            .map(|w| (w.row.unit.clone(), state_key(&w.row)))
            .collect();
        // Nothing changed, nothing pending: a delta tick sends nothing.
        assert_eq!(plan_computer(&obs, false, Some(&sent), &[]), None);

        // One unit changed state.
        let changed =
            observed(&svc::HEALTHY_SHOW.replacen("ActiveState=active", "ActiveState=failed", 1));
        let c = plan_computer(&changed, false, Some(&sent), &[]).unwrap();
        assert!(c.services_complete_kinds.is_empty());
        let rows = c.services.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].active_state.as_deref(), Some("failed"));

        // A memory_peak change alone is not a state change.
        let peak = observed(
            &svc::HEALTHY_SHOW.replace("MemoryPeak=12884901888", "MemoryPeak=12884901999"),
        );
        assert_eq!(plan_computer(&peak, false, Some(&sent), &[]), None);
    }

    #[test]
    fn completeness_is_per_kind_and_no_scan_is_null() {
        let mut obs = observed(svc::HEALTHY_SHOW);
        // The session bus failed: only the GitHub-runner kind is complete.
        obs.scan.complete_kinds = vec!["gh_actions_runner"];
        let c = plan_computer(&obs, true, None, &[]).unwrap();
        assert_eq!(
            c.services_complete_kinds,
            vec!["gh_actions_runner".to_string()]
        );
        assert_eq!(c.services.as_ref().map(Vec::len), Some(2));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v["services_complete_kinds"],
            serde_json::json!(["gh_actions_runner"])
        );
        assert!(
            v.get("services_complete").is_none(),
            "the bool is gone from the wire"
        );

        // Nothing complete: a pure delta, rows still sent.
        obs.scan.complete_kinds = Vec::new();
        let c = plan_computer(&obs, true, None, &[]).unwrap();
        assert!(c.services_complete_kinds.is_empty());
        assert_eq!(c.services.as_ref().map(Vec::len), Some(2));

        // Nothing answered: services null, no kinds claimed.
        obs.scan = ServiceScan::default();
        let c = plan_computer(&obs, true, None, &[]).unwrap();
        assert_eq!(c.services, None);
        assert!(c.services_complete_kinds.is_empty());
    }

    #[test]
    fn a_guest_reports_as_a_child_of_its_host_and_never_attaches_the_device() {
        let g = wsl_guest::parse_guest_probe(
            "MACHINE_ID\tfedcba9876543210fedcba9876543210\nHOSTNAME\tbad host;id\nPID1\tsystemd\nLIST_RC\t0\nUNIT_COUNT\t0\nSHOW_BEGIN\nSHOW_END\nSHOW_RC\t0\nPROBE_END\n",
        )
        .unwrap();
        let o = guest_observed(&g, HOST_ID);
        assert_eq!(
            o.base.hostname, None,
            "a hostname coord would refuse is null"
        );
        assert_eq!(o.base.access, None, "guests carry no access facts");
        assert_eq!(o.base.kind, "wsl_guest");
        assert_eq!(o.base.parent_identity_hash.as_deref(), Some(HOST_ID));
        assert!(!o.base.attach_device);
        assert_eq!(o.scan.units.as_ref().map(Vec::len), Some(0));
        // A host-probed guest with a clean `list-units`: the GitHub-runner
        // kind is complete — and NEVER `qontinui_runner`, whose user units
        // the host cannot see.
        assert_eq!(o.scan.complete_kinds, vec!["gh_actions_runner"]);
        let c = plan_computer(&o, true, None, &[]).unwrap();
        assert_eq!(
            c.services_complete_kinds,
            vec!["gh_actions_runner".to_string()]
        );
        assert_eq!(c.services.as_ref().map(Vec::len), Some(0));

        let no_systemd = wsl_guest::parse_guest_probe(
            "MACHINE_ID\tfedcba9876543210fedcba9876543210\nPID1\tinit\nSHOW_BEGIN\nSHOW_END\nPROBE_END\n",
        )
        .unwrap();
        assert_eq!(
            guest_observed(&no_systemd, HOST_ID).scan,
            ServiceScan::default()
        );

        // systemd is PID 1 but `list-units` failed: rows (none) are sent as a
        // partial scan, which deletes nothing.
        let list_failed = wsl_guest::parse_guest_probe(
            "MACHINE_ID\tfedcba9876543210fedcba9876543210\nPID1\tsystemd\nLIST_RC\t1\nSHOW_BEGIN\nSHOW_END\nPROBE_END\n",
        )
        .unwrap();
        let o = guest_observed(&list_failed, HOST_ID);
        assert!(o.scan.complete_kinds.is_empty());
        let c = plan_computer(&o, true, None, &[]).unwrap();
        assert!(c.services_complete_kinds.is_empty());
    }

    /// With a runner reporting from inside the guest, the host's probe
    /// claims no completeness: the in-guest runner owns the inventory.
    #[test]
    fn the_host_claims_no_guest_completeness_while_a_guest_runner_reports() {
        let probe = |active: &str| {
            wsl_guest::parse_guest_probe(&format!(
                "MACHINE_ID\tfedcba9876543210fedcba9876543210\nPID1\tsystemd\nLIST_RC\t0\nRUNNER_ACTIVE\t{active}\nUNIT_COUNT\t1\nSHOW_BEGIN\nId=actions.runner.example-org-example-repo.wslbox.service\nActiveState=inactive\n\nSHOW_END\nSHOW_RC\t0\nPROBE_END\n"
            ))
            .unwrap()
        };
        let busy = guest_observed(&probe("1"), HOST_ID);
        assert!(busy.scan.complete_kinds.is_empty());
        let c = plan_computer(&busy, true, None, &[]).unwrap();
        assert!(c.services_complete_kinds.is_empty());
        assert_eq!(
            c.services.as_ref().map(Vec::len),
            Some(1),
            "rows still sent as a delta"
        );

        let idle = guest_observed(&probe("0"), HOST_ID);
        assert_eq!(idle.scan.complete_kinds, vec!["gh_actions_runner"]);
        // Undeterminable liveness: the host keeps its (loud) claim.
        let unknown = guest_observed(&probe(""), HOST_ID);
        assert_eq!(unknown.scan.complete_kinds, vec!["gh_actions_runner"]);
    }

    fn ev(id: &str) -> ComputerEvent {
        ComputerEvent {
            client_event_id: id.into(),
            kind: "oom_kill".into(),
            observed_at: "2026-09-30T01:48:16Z".into(),
            detail: serde_json::json!({}),
        }
    }

    #[test]
    fn duplicate_identities_keep_the_first_observation() {
        let host = observed(svc::HEALTHY_SHOW);
        let mut clone_a = observed(svc::HEALTHY_SHOW);
        clone_a.base.identity_hash = "c".repeat(64);
        clone_a.base.kind = "wsl_guest".into();
        let mut clone_b = clone_a.clone();
        clone_b.base.hostname = Some("second-clone".into());
        let (kept, dropped) = dedupe_observed(vec![host, clone_a, clone_b]);
        assert_eq!(dropped, 1);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].base.identity_hash, HOST_ID);
        assert_eq!(kept[1].base.hostname.as_deref(), Some("fleetbox"));
    }

    #[test]
    fn a_report_is_capped_at_coords_bounds_oldest_events_first() {
        let mk = |i: usize, n: usize| {
            let mut c = host_report(format!("{i:064}"), "wsl_guest", &facts(), None, None);
            c.events = (0..n).map(|j| ev(&format!("{i}-{j}"))).collect();
            c
        };
        let computers: Vec<ComputerReport> = vec![mk(0, 600), mk(1, 450), mk(2, 10)]
            .into_iter()
            .chain((3..20).map(|i| mk(i, 0)))
            .collect();
        let capped = cap_report(computers);
        assert_eq!(capped.len(), MAX_COMPUTERS_PER_REPORT);
        assert_eq!(capped[0].events.len(), 500, "per-computer bound");
        assert_eq!(capped[0].events[0].client_event_id, "0-0", "oldest first");
        assert_eq!(capped[1].events.len(), 450);
        assert_eq!(capped[2].events.len(), 10);
        let total: usize = capped.iter().map(|c| c.events.len()).sum();
        assert!(total <= MAX_EVENTS_PER_REPORT);

        let big: Vec<ComputerReport> = vec![mk(0, 500), mk(1, 500), mk(2, 500)];
        let capped = cap_report(big);
        let per: Vec<usize> = capped.iter().map(|c| c.events.len()).collect();
        assert_eq!(per, vec![500, 500, 0], "the report-wide bound of 1000");
    }

    #[test]
    fn a_200_outcome_names_identity_conflicts() {
        let body = format!(
            r#"{{"computers":[{{"identity_hash":"{HOST_ID}","computer_id":"00000000-0000-4000-8000-000000000001","services_upserted":1,"services_deleted":0,"events_inserted":0,"events_duplicate":0,"identity_conflict":true}}]}}"#
        );
        let o = parse_outcomes(&body).unwrap();
        assert_eq!(
            o.get(HOST_ID),
            Some(&Outcome {
                identity_conflict: true,
                ..Outcome::default()
            })
        );
        assert_eq!(parse_outcomes("not json"), None);

        let expired = format!(
            r#"{{"computers":[{{"identity_hash":"{HOST_ID}","identity_conflict":false,"events_expired":4,"timestamps_clamped":2}}]}}"#
        );
        let o = &parse_outcomes(&expired).unwrap()[HOST_ID];
        assert_eq!(o.events_expired, 4);
        assert_eq!(o.timestamps_clamped, 2);
        assert!(!o.parent_unresolved);
        assert!(o.unknown_fields.is_empty());

        let drift = format!(
            r#"{{"computers":[{{"identity_hash":"{HOST_ID}","parent_unresolved":true,"unknown_fields":["gpus_v2","x"]}}]}}"#
        );
        let o = &parse_outcomes(&drift).unwrap()[HOST_ID];
        assert!(o.parent_unresolved);
        assert_eq!(
            o.unknown_fields,
            vec!["gpus_v2".to_string(), "x".to_string()]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_service_list_over_coords_bound_cuts_the_overflowing_kind_and_defers_it() {
        let mut obs = observed(svc::INCIDENT_SHOW);
        let template = obs.scan.units.as_ref().unwrap()[0].clone();
        let unit = |name: String| {
            let mut w = template.clone();
            w.row.unit = name;
            w.row.kind = services::classify_unit(&w.row.unit).to_string();
            w
        };
        // More GitHub runners than coord allows, then two small-kind units
        // LAST — so a plain truncate would cut them, and only the sort that
        // moves the largest kind to the tail keeps them.
        let mut units: Vec<WatchedUnit> = (0..MAX_SERVICES_PER_COMPUTER + 1)
            .map(|i| unit(format!("actions.runner.r{i}.service")))
            .collect();
        units.push(unit("qontinui-runner.service".into()));
        units.push(unit("qontinui-runner-b.service".into()));
        obs.scan.units = Some(units);
        obs.scan.complete_kinds = vec![
            services::KIND_GH_ACTIONS_RUNNER,
            services::KIND_QONTINUI_RUNNER,
        ];

        let mut c = plan_computer(&obs, true, None, &[]).unwrap();
        presanitize(&mut c);
        let rows = c.services.as_ref().unwrap();
        assert_eq!(rows.len(), MAX_SERVICES_PER_COMPUTER);
        assert!(
            rows.iter().any(|r| r.unit == "qontinui-runner.service")
                && rows.iter().any(|r| r.unit == "qontinui-runner-b.service"),
            "the small kind survives whole"
        );
        assert_eq!(
            c.services_complete_kinds,
            vec![services::KIND_QONTINUI_RUNNER.to_string()],
            "only the truncated kind loses completeness — coord would delete what it omits"
        );

        // Delivered: the deferred units are what the next delta carries.
        let mut r = Reporter {
            state_path: None,
            ..Reporter::new()
        };
        r.state = Some(ObserverState::default());
        let body = ComputerReportReq {
            device_id: "d".into(),
            computers: vec![c],
        };
        r.on_delivered(&body, true, None).await;
        let delta = plan_computer(&obs, false, r.last_sent.get(HOST_ID), &[]).unwrap();
        let mut deferred: Vec<&str> = delta
            .services
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.unit.as_str())
            .collect();
        deferred.sort_unstable();
        assert_eq!(deferred.len(), 3, "{deferred:?}");
        assert!(deferred.iter().all(|u| u.starts_with("actions.runner.")));

        // Exactly at the bound: untouched, still complete.
        let mut ok = plan_computer(&observed(svc::INCIDENT_SHOW), true, None, &[]).unwrap();
        let kinds = ok.services_complete_kinds.clone();
        ok.services = Some(
            (0..MAX_SERVICES_PER_COMPUTER)
                .map(|i| unit(format!("actions.runner.r{i}.service")).row)
                .collect(),
        );
        presanitize(&mut ok);
        assert_eq!(
            ok.services.as_ref().map(Vec::len),
            Some(MAX_SERVICES_PER_COMPUTER)
        );
        assert_eq!(ok.services_complete_kinds, kinds);
    }

    /// Phase 1 of `2026-11-13-computer-services-cap-and-sample-outcome`: a
    /// scan over coord's bound queues ONE `services_over_bound` event, and it
    /// rides the same report as the first truncated list. A later delta —
    /// which lists only the deferred rows — does not read as "back within".
    #[tokio::test(flavor = "current_thread")]
    async fn a_scan_over_the_bound_queues_one_event_on_the_first_truncated_report() {
        let mut obs = observed(svc::HEALTHY_SHOW);
        let template = obs.scan.units.as_ref().unwrap()[0].clone();
        obs.scan.units = Some(
            (0..300)
                .map(|i| {
                    let mut w = template.clone();
                    w.row.unit = format!("actions.runner.r{i}.service");
                    w.row.kind = services::KIND_GH_ACTIONS_RUNNER.to_string();
                    w
                })
                .collect(),
        );
        let observed = std::slice::from_ref(&obs);
        let over_bound_events = |events: &[ComputerEvent]| -> Vec<ComputerEvent> {
            events
                .iter()
                .filter(|e| e.detail["reason"] == "services_over_bound")
                .cloned()
                .collect()
        };
        let mut r = Reporter {
            state_path: None,
            ..Reporter::new()
        };
        r.state = Some(ObserverState::default());

        r.derive_events(observed, HOST_ID, at("2026-11-14T00:00:00Z"));
        let report = r.plan_report(observed, true, false);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].services.as_ref().map(Vec::len), Some(256));
        let gaps = over_bound_events(&report[0].events);
        assert_eq!(gaps.len(), 1, "{:?}", report[0].events);
        assert_eq!(gaps[0].kind, "telemetry_gap");
        assert_eq!(gaps[0].detail["bound"], 256);
        assert_eq!(gaps[0].detail["observed"]["gh_actions_runner"], 300);
        assert_eq!(
            gaps[0].detail["truncated_kinds"],
            serde_json::json!(["gh_actions_runner"])
        );

        // The next tick, before delivery: still exactly one queued.
        r.derive_events(observed, HOST_ID, at("2026-11-14T00:00:30Z"));
        let queued = r.state.as_ref().unwrap().pending[HOST_ID].clone();
        assert_eq!(over_bound_events(&queued).len(), 1);

        // Delivered: the delta carries the 44 deferred rows and no new event,
        // and the computer is still over the bound.
        let body = ComputerReportReq {
            device_id: "d".into(),
            computers: report,
        };
        r.on_delivered(&body, true, None).await;
        r.derive_events(observed, HOST_ID, at("2026-11-14T00:01:00Z"));
        let delta = r.plan_report(observed, false, false);
        assert_eq!(delta[0].services.as_ref().map(Vec::len), Some(44));
        assert!(over_bound_events(&delta[0].events).is_empty());
        assert!(
            matches!(
                r.state.as_ref().unwrap().over_bound.get(HOST_ID),
                Some(services_bound::OverBound::Over(_))
            ),
            "a 44-row delta must not read as back within the bound"
        );
    }

    /// Review round 3, A2: leaving the over-bound state queues exactly one
    /// CLOSING `services_over_bound` event, so the newest event coord serves
    /// says "within" instead of reading "over" until it ages out.
    #[test]
    fn leaving_the_bound_queues_exactly_one_closing_event() {
        let units = |n: usize| -> Observed {
            let mut obs = observed(svc::HEALTHY_SHOW);
            let template = obs.scan.units.as_ref().unwrap()[0].clone();
            obs.scan.units = Some(
                (0..n)
                    .map(|i| {
                        let mut w = template.clone();
                        w.row.unit = format!("actions.runner.r{i}.service");
                        w.row.kind = services::KIND_GH_ACTIONS_RUNNER.to_string();
                        w
                    })
                    .collect(),
            );
            obs
        };
        let gaps = |r: &Reporter, cleared: bool| -> usize {
            r.state.as_ref().unwrap().pending[HOST_ID]
                .iter()
                .filter(|e| {
                    e.detail["reason"] == "services_over_bound" && e.detail["cleared"] == cleared
                })
                .count()
        };
        let mut r = Reporter {
            state_path: None,
            ..Reporter::new()
        };
        r.state = Some(ObserverState::default());
        let over = units(300);
        r.derive_events(
            std::slice::from_ref(&over),
            HOST_ID,
            at("2026-11-14T00:00:00Z"),
        );
        assert_eq!(gaps(&r, false), 1);

        let within = units(10);
        for (i, t) in [
            "2026-11-14T00:00:30Z",
            "2026-11-14T00:01:00Z",
            "2026-11-14T00:01:30Z",
            "2026-11-14T00:02:00Z",
            "2026-11-14T00:02:30Z",
        ]
        .into_iter()
        .enumerate()
        {
            r.derive_events(std::slice::from_ref(&within), HOST_ID, at(t));
            assert_eq!(gaps(&r, true), usize::from(i >= 2), "scan {}", i + 1);
        }
        let closing = r.state.as_ref().unwrap().pending[HOST_ID]
            .iter()
            .find(|e| e.detail["cleared"] == true)
            .unwrap()
            .clone();
        assert_eq!(closing.detail["truncated_kinds"], serde_json::json!([]));
        assert_eq!(
            r.state.as_ref().unwrap().over_bound.get(HOST_ID),
            Some(&services_bound::OverBound::Within)
        );
    }

    /// Review round 5, A1: while a runner reports from inside a WSL guest,
    /// the host's probe of that guest can complete nothing, so a guest kind
    /// over the bound is never HELD by it — the guest leaves the state on its
    /// counts, with one closing event, instead of being re-emitted as over.
    #[test]
    fn a_guest_whose_runner_reports_is_never_held_over() {
        let probe = |active: &str, n: usize| {
            let units: String = (0..n)
                .map(|i| format!("Id=actions.runner.r{i}.service\nActiveState=active\n\n"))
                .collect();
            wsl_guest::parse_guest_probe(&format!(
                "MACHINE_ID\tfedcba9876543210fedcba9876543210\nPID1\tsystemd\nLIST_RC\t0\nRUNNER_ACTIVE\t{active}\nUNIT_COUNT\t{n}\nSHOW_BEGIN\n{units}SHOW_END\nSHOW_RC\t0\nPROBE_END\n"
            ))
            .unwrap()
        };
        let reporting = guest_observed(&probe("1", 10), HOST_ID);
        assert!(reporting.scan.completable_kinds.is_empty());
        assert_eq!(
            guest_observed(&probe("0", 10), HOST_ID)
                .scan
                .completable_kinds,
            services::GUEST_COMPLETABLE_KINDS
        );

        let mut r = Reporter {
            state_path: None,
            ..Reporter::new()
        };
        r.state = Some(ObserverState::default());
        let over = guest_observed(&probe("0", 300), HOST_ID);
        let guest = over.base.identity_hash.clone();
        r.derive_events(
            std::slice::from_ref(&over),
            HOST_ID,
            at("2026-11-14T00:00:00Z"),
        );
        assert!(matches!(
            r.state.as_ref().unwrap().over_bound.get(&guest),
            Some(services_bound::OverBound::Over(_))
        ));
        for t in [
            "2026-11-14T00:00:30Z",
            "2026-11-14T00:01:00Z",
            "2026-11-14T00:01:30Z",
        ] {
            r.derive_events(std::slice::from_ref(&reporting), HOST_ID, at(t));
        }
        let state = r.state.as_ref().unwrap();
        assert_eq!(
            state.over_bound.get(&guest),
            Some(&services_bound::OverBound::Within)
        );
        let closing = state.pending[&guest]
            .iter()
            .filter(|e| e.detail["cleared"] == true)
            .count();
        assert_eq!(closing, 1);
    }

    /// Review round 5, A2: the over-bound state is persisted with the
    /// observer state, so a reporter rebuilt while a computer is over still
    /// queues the closing event when it leaves.
    #[tokio::test(flavor = "current_thread")]
    async fn the_over_bound_state_survives_a_reporter_rebuild() {
        let units = |n: usize| -> Observed {
            let mut obs = observed(svc::HEALTHY_SHOW);
            let template = obs.scan.units.as_ref().unwrap()[0].clone();
            obs.scan.units = Some(
                (0..n)
                    .map(|i| {
                        let mut w = template.clone();
                        w.row.unit = format!("actions.runner.r{i}.service");
                        w.row.kind = services::KIND_GH_ACTIONS_RUNNER.to_string();
                        w
                    })
                    .collect(),
            );
            obs
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-observer.json");
        let mut first = Reporter {
            state_path: Some(path.clone()),
            ..Reporter::new()
        };
        first.state = Some(ObserverState::default());
        first.derive_events(
            std::slice::from_ref(&units(300)),
            HOST_ID,
            at("2026-11-14T00:00:00Z"),
        );
        first.persist().await;
        drop(first);

        let mut rebuilt = Reporter {
            state_path: Some(path.clone()),
            ..Reporter::new()
        };
        rebuilt.state = Some(events::load(&path));
        assert!(matches!(
            rebuilt.state.as_ref().unwrap().over_bound.get(HOST_ID),
            Some(services_bound::OverBound::Over(_))
        ));
        let within = units(10);
        for t in [
            "2026-11-14T00:00:30Z",
            "2026-11-14T00:01:00Z",
            "2026-11-14T00:01:30Z",
        ] {
            rebuilt.derive_events(std::slice::from_ref(&within), HOST_ID, at(t));
        }
        let closing = rebuilt.state.as_ref().unwrap().pending[HOST_ID]
            .iter()
            .filter(|e| e.detail["cleared"] == true)
            .count();
        assert_eq!(closing, 1, "the rebuilt reporter closes the entry");
    }

    /// Review A3: when `events::prune` forgets a computer, its over-bound
    /// state is forgotten with it rather than kept for the process lifetime.
    /// The forgotten computer is one `events::prune` itself removes (unseen
    /// for over a week), so the test fails if the over-bound state is
    /// reconciled before the prune rather than after it.
    #[test]
    fn a_forgotten_computer_takes_its_over_bound_state_with_it() {
        let gone = "c".repeat(64);
        let mut state = ObserverState::default();
        state.computers.insert(
            gone.clone(),
            events::ComputerObs {
                last_seen_at: Some("2026-11-01T00:00:00Z".into()),
                ..events::ComputerObs::default()
            },
        );
        let mut r = Reporter {
            state_path: None,
            ..Reporter::new()
        };
        r.state = Some(state);
        let over = services_bound::Over {
            kinds: [services::KIND_GH_ACTIONS_RUNNER].into_iter().collect(),
            held: Default::default(),
            last_counts: Default::default(),
            emitted_at: at("2026-11-01T00:00:00Z"),
            proposed: None,
        };
        r.state
            .as_mut()
            .unwrap()
            .over_bound
            .insert(gone.clone(), services_bound::OverBound::Over(over));
        let obs = observed(svc::HEALTHY_SHOW);
        r.derive_events(
            std::slice::from_ref(&obs),
            HOST_ID,
            at("2026-11-14T00:00:00Z"),
        );
        assert!(
            !r.state.as_ref().unwrap().computers.contains_key(&gone),
            "events::prune forgot the computer"
        );
        assert!(
            !r.state.as_ref().unwrap().over_bound.contains_key(&gone),
            "{:?}",
            r.state.as_ref().unwrap().over_bound.keys()
        );
        assert_eq!(
            r.state.as_ref().unwrap().over_bound.get(HOST_ID),
            Some(&services_bound::OverBound::Within)
        );
    }

    #[test]
    fn an_expired_marker_folds_earlier_markers_and_skips_marker_only_loss() {
        let now = at("2026-11-15T00:00:00Z");
        let old = |id: &str| {
            let mut e = ev(id);
            e.observed_at = "2026-09-01T00:00:00Z".into();
            e
        };
        let mut old_marker = old("gap-1");
        old_marker.kind = "telemetry_gap".into();
        old_marker.detail = serde_json::json!({"reason": "rejected_by_coord", "dropped_events": 5});
        let fresh = ev("fresh");

        // Two real events and one marker expired: 2 real + the marker's 5.
        let sent = vec![old("a"), old("b"), old_marker.clone(), fresh.clone()];
        let m = expired_marker_for(HOST_ID, &sent, 3, now).unwrap();
        assert_eq!(m.kind, "telemetry_gap");
        assert_eq!(m.observed_at, "2026-11-15T00:00:00Z");
        assert_eq!(
            m.detail,
            serde_json::json!({"reason": "events_expired", "dropped_events": 7})
        );
        // Only a marker expired: no new marker (no loss loop).
        assert_eq!(
            expired_marker_for(HOST_ID, &[old_marker, fresh], 1, now),
            None
        );
        assert_eq!(expired_marker_for(HOST_ID, &sent, 0, now), None);
        // coord's count exceeds what was sent: coord's count is the loss.
        let m = expired_marker_for(HOST_ID, &[ev("x")], 2, now).unwrap();
        assert_eq!(m.detail["dropped_events"], 2);

        // The expired set is the OLDEST n by observed_at — whatever this
        // machine's clock or any local retention would say: here the only
        // expired event is the oldest, a marker, so no new marker.
        let mut future = ev("far-future");
        future.observed_at = "2030-01-01T00:00:00Z".into();
        let mut oldest_marker = old("gap-0");
        oldest_marker.observed_at = "2026-08-01T00:00:00Z".into();
        oldest_marker.kind = "telemetry_gap".into();
        oldest_marker.detail = serde_json::json!({"reason": "events_expired", "dropped_events": 3});
        assert_eq!(
            expired_marker_for(HOST_ID, &[future.clone(), oldest_marker.clone()], 1, now),
            None
        );
        // Two expired: the marker (3, folded) and the next-oldest real one.
        let m = expired_marker_for(HOST_ID, &[future, oldest_marker], 2, now).unwrap();
        assert_eq!(m.detail["dropped_events"], 4);
    }

    #[test]
    fn a_422_quarantines_exactly_the_events_it_names() {
        let mut c = host_report(HOST_ID.into(), "host", &facts(), None, None);
        c.events = vec![ev("good-1"), ev("bad-1"), ev("good-2")];
        let sent = ComputerReportReq {
            device_id: "d".into(),
            computers: vec![c.clone()],
        };
        let body = r#"{"error":"invalid computer report","problems":["computers[0].events[1].detail: 70000 bytes exceeds 65536"]}"#;
        let problems = parse_problems(body);
        assert_eq!(problems.len(), 1);
        let named = rejected_events(&problems, &sent);
        assert_eq!(named.get(HOST_ID), Some(&vec!["bad-1".to_string()]));

        let mut st = ObserverState::default();
        st.pending.insert(HOST_ID.into(), c.events.clone());
        let t = chrono::DateTime::parse_from_rfc3339("2026-09-30T02:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            quarantine(
                &mut st,
                HOST_ID,
                named.get(HOST_ID).map(Vec::as_slice),
                &problems,
                t
            ),
            Quarantined {
                events: 1,
                markers: 0
            }
        );
        let q: Vec<&str> = st.pending[HOST_ID]
            .iter()
            .map(|e| e.kind.as_str())
            .collect();
        assert_eq!(q, vec!["oom_kill", "oom_kill", "telemetry_gap"]);
        let marker = st.pending[HOST_ID].last().unwrap();
        assert_eq!(marker.detail["reason"], "rejected_by_coord");
        assert_eq!(marker.detail["dropped_events"], 1);

        // A problem naming no event (a computer-level field) names nothing.
        let other = vec!["computers[0].hostname: \"x y\" must be ...".to_string()];
        assert!(rejected_events(&other, &sent).is_empty());
        // The whole-queue quarantine leaves only ONE marker, with the earlier
        // marker's count folded in: 2 events + the 1 already dropped = 3.
        assert_eq!(
            quarantine(&mut st, HOST_ID, None, &other, t),
            Quarantined {
                events: 2,
                markers: 1
            }
        );
        assert_eq!(st.pending[HOST_ID].len(), 1);
        assert_eq!(st.pending[HOST_ID][0].detail["dropped_events"], 3);
        // A refusal of the marker alone folds again and reports markers only.
        let marker_id = st.pending[HOST_ID][0].client_event_id.clone();
        assert_eq!(
            quarantine(&mut st, HOST_ID, Some(&[marker_id]), &other, t),
            Quarantined {
                events: 0,
                markers: 1
            }
        );
        assert_eq!(st.pending[HOST_ID][0].detail["dropped_events"], 3);
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn over_long_values_are_nulled_before_sending_and_times_are_left_alone() {
        let mut c = plan_computer(&observed(svc::INCIDENT_SHOW), true, None, &[]).unwrap();
        c.kernel = Some("k".repeat(513));
        // A future boot time is coord's to clamp (amendment A3), not ours.
        c.booted_at = Some("2030-01-01T00:00:00Z".into());
        let rows = c.services.as_mut().unwrap();
        rows[0].runner_name = Some("r".repeat(513));
        rows[0].state_changed_at = Some("2030-01-01T00:00:00Z".into());
        let mut long_unit = rows[0].clone();
        long_unit.unit = "u".repeat(257);
        rows.push(long_unit);
        let mut e = ev("future");
        e.observed_at = "2030-01-01T00:00:00Z".into();
        c.events = vec![e];

        presanitize(&mut c);
        assert_eq!(c.kernel, None);
        assert_eq!(c.booted_at.as_deref(), Some("2030-01-01T00:00:00Z"));
        let rows = c.services.as_ref().unwrap();
        assert_eq!(rows.len(), 1, "an over-long unit name drops the row");
        assert_eq!(rows[0].runner_name, None);
        assert_eq!(
            rows[0].state_changed_at.as_deref(),
            Some("2030-01-01T00:00:00Z")
        );
        assert_eq!(c.events[0].observed_at, "2030-01-01T00:00:00Z");
        // Exactly at the bound is kept.
        let mut ok = plan_computer(&observed(svc::INCIDENT_SHOW), true, None, &[]).unwrap();
        ok.kernel = Some("k".repeat(512));
        presanitize(&mut ok);
        assert_eq!(ok.kernel.as_ref().map(String::len), Some(512));
    }

    #[test]
    fn a_stripped_report_keeps_identity_and_events_only() {
        let mut c = plan_computer(&observed(svc::INCIDENT_SHOW), true, None, &[ev("x")]).unwrap();
        assert!(!c.services_complete_kinds.is_empty());
        let s = stripped(&c);
        assert_eq!(s.services, None);
        assert!(s.services_complete_kinds.is_empty());
        assert_eq!(s.hostname, None);
        assert_eq!(s.access, None);
        assert_eq!(s.kernel, None);
        assert_eq!(s.identity_hash, c.identity_hash);
        assert_eq!(s.boot_id, c.boot_id);
        assert_eq!(s.events, c.events);
    }

    #[test]
    fn server_errors_back_off_exponentially_and_cap() {
        let secs: Vec<u64> = (0..9).map(|n| server_error_backoff(n).as_secs()).collect();
        // Capped at the 300 s full-snapshot cadence.
        assert_eq!(secs, vec![30, 60, 120, 240, 300, 300, 300, 300, 300]);
        // The jitter the caller applies stays inside ±20%.
        for _ in 0..50 {
            let j = crate::fleet::resource_sample::jittered_sleep(300).as_secs();
            assert!((240..=360).contains(&j), "{j}");
        }
    }

    #[test]
    fn problems_compare_by_path_not_value() {
        let a = vec![
            "computers[0].events[1].detail: 70000 bytes exceeds 65536".to_string(),
            "computers[0].hostname: \"x y\" must be 1-253 chars".to_string(),
        ];
        let b = vec![
            "computers[0].hostname: \"x z\" must be 1-253 chars".to_string(),
            "computers[0].events[1].detail: 70123 bytes exceeds 65536".to_string(),
        ];
        assert_eq!(problem_paths(&a), problem_paths(&b));
        assert_eq!(
            problem_paths(&a),
            vec![
                "computers[0].events[1].detail".to_string(),
                "computers[0].hostname".to_string()
            ]
        );
    }

    #[test]
    fn not_opted_in_sends_an_empty_access_object() {
        let c = host_report(
            HOST_ID.into(),
            "host",
            &facts(),
            None,
            Some(Access::default()),
        );
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["access"], serde_json::json!({}));
    }

    /// Production source of `fleet.rs`, test module split off.
    fn fleet_prod_src() -> &'static str {
        const SRC: &str = include_str!("../../fleet.rs");
        SRC.split_once("\n#[cfg(test)]")
            .map(|(a, _)| a)
            .unwrap_or(SRC)
    }

    /// Plan Phase 2: **a secondary never reports computers.** The reporter is
    /// driven from `spawn_budget_republisher`, after its shared-state
    /// ownership gate — the same structural pin as the sampler's
    /// `a_secondary_never_reaches_the_sampler`, extended to this call site.
    #[test]
    fn a_secondary_never_reports_computers() {
        let src = fleet_prod_src();
        let start = src
            .find("pub fn spawn_budget_republisher(")
            .expect("the reporter's host function must exist");
        let body = src.get(start..).unwrap();
        let end = body
            .get(1..)
            .and_then(|b| b.find("\npub "))
            .map_or(body.len(), |i| i + 1);
        let body = body.get(..end).unwrap();

        let gate = body
            .find("owns_shared_root_state()")
            .expect("spawn_budget_republisher must consult shared-state ownership");
        let reporter = body
            .find("computer::Reporter::new()")
            .expect("the computer reporter must be built inside the gated republisher");
        let ticked = body
            .find("reporter.tick().await")
            .expect("the computer reporter must be ticked from the gated loop");
        assert!(
            gate < reporter && gate < ticked,
            "the ownership gate must precede the reporter"
        );
        assert_eq!(
            src.matches("computer::Reporter::new()").count(),
            1,
            "exactly one production construction site"
        );
    }

    /// Every source file of the reporter, read by the structural pins below.
    const REPORTER_SOURCES: &[(&str, &str)] = &[
        ("mod.rs", include_str!("mod.rs")),
        ("services.rs", include_str!("services.rs")),
        ("services_bound.rs", include_str!("services_bound.rs")),
        ("events.rs", include_str!("events.rs")),
        ("wsl_guest.rs", include_str!("wsl_guest.rs")),
        ("access.rs", include_str!("access.rs")),
        ("identity.rs", include_str!("identity.rs")),
        ("slice_oom.rs", include_str!("slice_oom.rs")),
        ("journal_oom.rs", include_str!("journal_oom.rs")),
    ];

    /// Plan `2026-11-27-devops-oom-kills-inside-agent-build-slices-carry-no-victim`
    /// test 21: the OOM-placement modules are on the roster the structural
    /// pins read, so their IO and their forks are covered.
    #[test]
    fn the_oom_placement_modules_are_reporter_sources() {
        let names: Vec<&str> = REPORTER_SOURCES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"slice_oom.rs"), "{names:?}");
        assert!(names.contains(&"journal_oom.rs"), "{names:?}");
    }

    /// The production part of a source file: its test module split off.
    fn prod_part(src: &str) -> &str {
        src.split_once("\n#[cfg(test)]")
            .map(|(a, _)| a)
            .unwrap_or(src)
    }

    /// Plan `2026-11-13-computer-services-cap-and-sample-outcome` Phase 3: the
    /// async `persist` writes a state the next reporter's load reads back
    /// equal — on a current-thread runtime, the flavour the `fleet-heartbeat`
    /// thread runs.
    #[tokio::test(flavor = "current_thread")]
    async fn persist_writes_a_state_that_load_reads_back_equal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-observer.json");
        let mut state = ObserverState {
            last_tick_at: Some("2026-11-14T00:00:00Z".into()),
            ..ObserverState::default()
        };
        events::push_pending(&mut state, HOST_ID, vec![ev("persisted-1")]);
        let r = Reporter {
            state: Some(state.clone()),
            state_path: Some(path.clone()),
            ..Reporter::new()
        };
        r.persist().await;
        assert_eq!(events::load(&path), state);
    }

    /// The byte index just past the delimiter closing the one at `open`
    /// (`open_ch` … `close_ch`, nesting counted), or `None` when unbalanced.
    fn closing(src: &str, open: usize, open_ch: char, close_ch: char) -> Option<usize> {
        let mut depth = 0i32;
        for (i, ch) in src.get(open..)?.char_indices() {
            if ch == open_ch {
                depth += 1;
            } else if ch == close_ch {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + ch.len_utf8());
                }
            }
        }
        None
    }

    /// The byte spans of every `spawn_blocking_tracked(…)` call in `src`,
    /// found by paren matching from the call's opening paren.
    fn tracked_spans(src: &str) -> Vec<std::ops::Range<usize>> {
        const CALL: &str = "spawn_blocking_tracked(";
        src.match_indices(CALL)
            .filter_map(|(start, _)| {
                closing(src, start + CALL.len() - 1, '(', ')').map(|end| start..end)
            })
            .collect()
    }

    /// `needle` occurs in `body`, and only inside tracked blocking bodies.
    fn assert_only_tracked(name: &str, body: &str, needle: &str) {
        let spans = tracked_spans(body);
        let hits: Vec<usize> = body.match_indices(needle).map(|(i, _)| i).collect();
        assert!(!hits.is_empty(), "{name}: `{needle}` not found");
        for h in hits {
            assert!(
                spans.iter().any(|s| s.contains(&h)),
                "{name}: `{needle}` at byte {h} runs inline, outside spawn_blocking_tracked"
            );
        }
    }

    /// The text of the function whose signature `sig` first follows `after`.
    fn fn_text<'a>(src: &'a str, after: &str, sig: &str) -> &'a str {
        let from = src
            .find(after)
            .unwrap_or_else(|| panic!("{after} not found"));
        let start = src
            .get(from..)
            .and_then(|s| s.find(sig))
            .map(|i| from + i)
            .unwrap_or_else(|| panic!("{sig} not found"));
        src.get(start..)
            .and_then(|s| s.find('{'))
            .and_then(|i| closing(src, start + i, '{', '}'))
            .and_then(|end| src.get(start..end))
            .unwrap_or_else(|| panic!("{sig} has no closing brace"))
    }

    fn reporter_prod(name: &str) -> &'static str {
        REPORTER_SOURCES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, src)| prod_part(src))
            .unwrap_or_else(|| panic!("{name} is not a reporter source"))
    }

    /// Phase 3: the fsync'd state write and the state load run as tracked
    /// blocking bodies, and `persist` is the only writer.
    #[test]
    fn observer_state_io_runs_in_tracked_blocking_bodies() {
        let prod = reporter_prod("mod.rs");
        assert!(prod.contains("spawn_blocking_tracked(move || events::save"));
        assert_only_tracked("mod.rs", prod, "events::save(");
        assert_only_tracked("mod.rs", prod, "events::load(");
        assert_eq!(prod.matches("events::save(").count(), 1);
        assert_only_tracked(
            "persist",
            fn_text(prod, "impl Reporter", "async fn persist("),
            "events::save(",
        );
    }

    /// Phase 3: no blocking body escapes the house wrapper's lane accounting.
    /// The banned substring is the bare `spawn_blocking(`, which
    /// `spawn_blocking_tracked(` does not contain, so neither the
    /// `tokio::task::` spelling nor an imported `spawn_blocking(` passes.
    #[test]
    fn the_reporter_never_spawns_an_untracked_blocking_body() {
        for (name, src) in REPORTER_SOURCES {
            assert!(
                !prod_part(src).contains("spawn_blocking("),
                "{name} spawns an untracked blocking body"
            );
        }
        assert_only_tracked(
            "mod.rs",
            reporter_prod("mod.rs"),
            "identity::collect_static",
        );
    }

    /// Phase 3: the procfs host axes are never read on the async worker.
    #[test]
    fn host_axes_are_never_collected_inline() {
        let prod = reporter_prod("mod.rs");
        assert!(!prod.contains("host_axes::collect()"));
        assert_only_tracked("mod.rs", prod, "host_axes::collect");
    }

    /// Phase 3: each service scan's per-unit `.runner` / cgroup reads run in
    /// a tracked blocking body.
    #[test]
    fn the_service_scans_read_unit_files_in_tracked_bodies() {
        let services = reporter_prod("services.rs");
        let linux = fn_text(
            services,
            "pub(crate) mod linux",
            "pub(crate) async fn scan(",
        );
        assert_only_tracked("linux scan", linux, "watched_from_props(");
        let windows = fn_text(
            services,
            "pub(crate) mod windows",
            "pub(crate) async fn scan(",
        );
        assert_only_tracked("windows scan", windows, "read_runner_file(");
    }

    /// No timer, task or thread of its own — a task would escape the gate.
    #[test]
    fn the_reporter_has_no_timer_of_its_own() {
        for (name, src) in REPORTER_SOURCES {
            let prod = prod_part(src);
            for banned in [
                "tokio::spawn(",
                "tokio::time::interval",
                "std::thread::spawn",
            ] {
                assert!(!prod.contains(banned), "{name} contains {banned}");
            }
        }
    }
}
