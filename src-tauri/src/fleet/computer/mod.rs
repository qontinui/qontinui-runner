//! The fleet computer as a first-class entity — runner-side collection (plan
//! `2026-09-30-the-fleet-machine-is-not-a-first-class-coord-entity-and-coord-has-no-resource-model`,
//! Phase 2, plus the runner half of Phase 6).
//!
//! Each tick of the already-gated sampler loop, the runner observes the
//! computer it runs on (and, on Windows, every RUNNING WSL guest), derives
//! events, and POSTs `POST /coord/computers/report`:
//!
//! * a **full snapshot** every [`REPORT_FULL_SECS`] (identity, static capacity,
//!   every watched service, `services_complete` when every source answered);
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
//! logged once; the events it names are quarantined behind a `telemetry_gap`
//! marker, and repeated unexplained refusals quarantine the queue — no single
//! event can wedge the reporter. A 200 whose outcome says `identity_conflict`
//! keeps that computer's events queued (coord did not store them).

pub(crate) mod access;
pub(crate) mod events;
pub(crate) mod identity;
pub(crate) mod services;
pub(crate) mod wsl_guest;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

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
    pub(crate) services_complete: bool,
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
        services_complete: false,
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
            services_complete: false,
            events: Vec::new(),
        },
        oom_kill_total: g.oom_kill_total,
        // Without systemd as PID 1 there was no one to ask: no information,
        // not "no units".
        // The list is authoritative only when `systemctl list-units` itself
        // exited 0; a failed listing is a partial (delta) scan, never a
        // snapshot that would delete the rows it missed.
        scan: if g.systemd {
            ServiceScan {
                units: Some(units),
                complete: g.list_rc == Some(0),
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
        c.services_complete = rows.is_some() && obs.scan.complete;
        c.services = rows;
        return Some(c);
    }
    let changed: Vec<ServiceRow> = rows
        .unwrap_or_default()
        .into_iter()
        .filter(|r| last_sent.and_then(|m| m.get(&r.unit)) != Some(&state_key(r)))
        .collect();
    c.services_complete = false;
    c.services = (!changed.is_empty()).then_some(changed);
    (c.services.is_some() || !c.events.is_empty()).then_some(c)
}

/// coord's bounds on one report (`computers.rs` `MAX_*`); a report over them
/// is refused whole, so the reporter never builds one.
pub(crate) const MAX_COMPUTERS_PER_REPORT: usize = 16;
pub(crate) const MAX_EVENTS_PER_COMPUTER: usize = 500;
pub(crate) const MAX_EVENTS_PER_REPORT: usize = 1_000;

/// Consecutive 422s after which the whole queue of every computer in the
/// refused report is quarantined (replaced by one `telemetry_gap` marker each)
/// — the escape from a refusal the problem list did not pin on an event.
pub(crate) const QUARANTINE_AFTER_REJECTIONS: u32 = 3;

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

/// The per-computer `identity_conflict` flags from a 200 response, or `None`
/// when the body is not the documented shape (then every computer counts as
/// delivered, the pre-outcome behaviour). PURE.
pub(crate) fn parse_outcomes(body: &str) -> Option<BTreeMap<String, bool>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let arr = v.get("computers")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|c| {
                let id = c.get("identity_hash")?.as_str()?.to_string();
                let conflict = c
                    .get("identity_conflict")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                Some((id, conflict))
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

/// Remove `ids` (all of the queue when `ids` is `None`) from one computer's
/// queue and leave ONE `telemetry_gap` marker saying how many coord refused
/// and why — never a silent drop. Returns how many were removed.
pub(crate) fn quarantine(
    state: &mut ObserverState,
    identity: &str,
    ids: Option<&[String]>,
    problems: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> usize {
    let Some(q) = state.pending.get_mut(identity) else {
        return 0;
    };
    let before = q.len();
    q.retain(|e| match ids {
        Some(ids) => !ids.contains(&e.client_event_id),
        None => false,
    });
    let removed = before - q.len();
    if removed > 0 {
        let at = events::rfc3339(now);
        q.push(ComputerEvent {
            client_event_id: events::event_id(
                "telemetry_gap",
                &["rejected", identity, &at, &removed.to_string()],
            ),
            kind: "telemetry_gap".into(),
            observed_at: at,
            detail: serde_json::json!({
                "reason": "rejected_by_coord",
                "dropped_events": removed,
                "problems": problems.iter().take(5).collect::<Vec<_>>(),
            }),
        });
    }
    removed
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
    rejected_logged: bool,
    conflict_logged: bool,
    dedupe_logged: bool,
    consecutive_rejections: u32,
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
            rejected_logged: false,
            conflict_logged: false,
            dedupe_logged: false,
            consecutive_rejections: 0,
            failures_logged: 0,
            no_identity_logged: false,
            facts: None,
            access: Access::default(),
            #[cfg(target_os = "linux")]
            watcher: services::linux::Watcher::default(),
        }
    }

    fn persist(&self) {
        if let (Some(path), Some(state)) = (self.state_path.as_ref(), self.state.as_ref()) {
            if let Err(e) = events::save(path, state) {
                debug!("fleet::computer: could not persist observer state: {e}");
            }
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
        self.route_absent_until =
            Some(tokio::time::Instant::now() + Duration::from_secs(REPORT_FULL_SECS));
    }

    /// One tick. Best-effort end to end: every failure returns quietly.
    pub(crate) async fn tick(&mut self) {
        let now = chrono::Utc::now();
        let axes = crate::fleet::host_axes::collect();
        let Some(host_id) = identity::host_identity_hash() else {
            if !self.no_identity_logged {
                self.no_identity_logged = true;
                info!("fleet::computer: no valid OS machine id — this computer cannot be identified, so it is not reported");
            }
            return;
        };
        if self.state.is_none() {
            self.state = Some(
                self.state_path
                    .as_deref()
                    .map(events::load)
                    .unwrap_or_default(),
            );
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
            self.persist();
            return;
        };
        let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
            self.persist();
            return;
        };
        let Some(bearer) = crate::coord_mcp::read_usable_device_jwt().await else {
            self.persist();
            return;
        };

        let full = self
            .last_full
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(REPORT_FULL_SECS));
        let facts_due = self
            .last_facts_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(REPORT_FULL_SECS));
        if facts_due || self.facts.is_none() {
            self.facts = tokio::task::spawn_blocking(identity::collect_static)
                .await
                .ok();
            self.access = access::collect().await;
            self.last_facts_at = Some(tokio::time::Instant::now());
        }
        let facts = self.facts.clone().unwrap_or_default();

        // ---- observe --------------------------------------------------
        let mut observed = vec![Observed {
            base: host_report(
                host_id.clone(),
                identity::host_kind(),
                &facts,
                axes.boot_id.clone(),
                Some(self.access.clone()),
            ),
            oom_kill_total: axes.oom_kill_total,
            scan: self.scan_host_services().await,
        }];
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

        // ---- derive events --------------------------------------------
        let state = self.state.get_or_insert_with(ObserverState::default);
        for o in &observed {
            let id = &o.base.identity_hash;
            let input = ObsInput {
                boot_id: o.base.boot_id.as_deref(),
                booted_at: o.base.booted_at.as_deref(),
                oom_kill_total: o.oom_kill_total,
                services: o.scan.units.as_deref(),
                services_complete: o.scan.complete,
            };
            let (evs, mut next) = events::derive(state.computers.get(id), &input, now);
            next.kind = Some(o.base.kind.clone());
            state.computers.insert(id.clone(), next);
            if !evs.is_empty() {
                events::push_pending(state, id, evs);
            }
        }
        let pruned = events::prune(state, &host_id, now);
        if pruned > 0 {
            debug!("fleet::computer: forgot {pruned} computer(s) unseen for a week");
        }
        // Persist BEFORE the POST: a crash mid-send must not lose events.
        self.persist();

        if let Some(until) = self.route_absent_until {
            if tokio::time::Instant::now() < until {
                return;
            }
        }

        // ---- plan the report -------------------------------------------
        let state = self.state.clone().unwrap_or_default();
        let computers: Vec<ComputerReport> = observed
            .iter()
            .filter_map(|o| {
                let id = &o.base.identity_hash;
                let pending = state.pending.get(id).map(Vec::as_slice).unwrap_or(&[]);
                plan_computer(o, full, self.last_sent.get(id), pending)
            })
            .collect();
        let computers = cap_report(computers);
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
                self.consecutive_rejections = 0;
                let outcomes = parse_outcomes(&text);
                self.on_delivered(&body, full, outcomes.as_ref());
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
            422 => self.on_rejected(&body, &text, now),
            _ => self.log_failure(format!("POST {url} -> HTTP {status}")),
        }
    }

    /// coord refused the report (422). Log the problems once; quarantine the
    /// events it named; after [`QUARANTINE_AFTER_REJECTIONS`] consecutive
    /// refusals quarantine every queue in the report, so no event can wedge
    /// the reporter forever.
    fn on_rejected(
        &mut self,
        body: &ComputerReportReq,
        text: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let problems = parse_problems(text);
        self.consecutive_rejections = self.consecutive_rejections.saturating_add(1);
        if !self.rejected_logged {
            self.rejected_logged = true;
            warn!(
                "fleet::computer: coord refused the computers report (422): {}",
                problems
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        let named = rejected_events(&problems, body);
        let mut removed = 0;
        if let Some(state) = self.state.as_mut() {
            for (id, ids) in &named {
                removed += quarantine(state, id, Some(ids), &problems, now);
            }
            if removed == 0 && self.consecutive_rejections >= QUARANTINE_AFTER_REJECTIONS {
                for c in &body.computers {
                    removed += quarantine(state, &c.identity_hash, None, &problems, now);
                }
                self.consecutive_rejections = 0;
            }
        }
        if removed > 0 {
            self.persist();
        } else {
            // Nothing this reporter can remove explains the refusal; do not
            // hammer coord with the same body every tick.
            self.back_off();
        }
    }

    /// coord accepted `body`: drop the delivered events and remember what it
    /// now holds — except for a computer coord reported as an
    /// `identity_conflict`, whose services and events it did NOT store; those
    /// stay queued.
    fn on_delivered(
        &mut self,
        body: &ComputerReportReq,
        full: bool,
        outcomes: Option<&BTreeMap<String, bool>>,
    ) {
        if full {
            self.last_full = Some(tokio::time::Instant::now());
        }
        let mut conflicted = 0;
        if let Some(state) = self.state.as_mut() {
            for c in &body.computers {
                if outcomes
                    .and_then(|o| o.get(&c.identity_hash))
                    .copied()
                    .unwrap_or(false)
                {
                    conflicted += 1;
                    continue;
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
                    if c.services_complete {
                        sent.clear();
                    }
                    for r in rows {
                        sent.insert(r.unit.clone(), state_key(r));
                    }
                }
            }
            state.pending.retain(|_, q| !q.is_empty());
        }
        if conflicted > 0 && !self.conflict_logged {
            self.conflict_logged = true;
            warn!(
                "fleet::computer: coord reported an identity_conflict for {conflicted} computer(s) \
                 (another live reporter shares the machine id); their events stay queued"
            );
        }
        self.persist();
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
                        })
                        .collect(),
                ),
                complete: true,
            },
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
                services_complete: true,
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
                "services_complete": true,
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
                            "runner_name": "fleetbox"
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
        assert!(!c.services_complete);
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
    fn a_partial_scan_is_never_sent_as_complete_and_no_scan_is_null() {
        let mut obs = observed(svc::HEALTHY_SHOW);
        obs.scan.complete = false;
        let c = plan_computer(&obs, true, None, &[]).unwrap();
        assert!(!c.services_complete);
        assert_eq!(c.services.as_ref().map(Vec::len), Some(2));

        obs.scan = ServiceScan::default();
        let c = plan_computer(&obs, true, None, &[]).unwrap();
        assert_eq!(c.services, None);
        assert!(!c.services_complete);
    }

    #[test]
    fn a_guest_reports_as_a_child_of_its_host_and_never_attaches_the_device() {
        let g = wsl_guest::parse_guest_probe(
            "MACHINE_ID\tfedcba9876543210fedcba9876543210\nHOSTNAME\tbad host;id\nPID1\tsystemd\nLIST_RC\t0\nSHOW_BEGIN\nSHOW_END\nPROBE_END\n",
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
        assert!(
            o.scan.complete,
            "systemd answered: zero units is a real zero"
        );

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
        assert!(!o.scan.complete);
        let c = plan_computer(&o, true, None, &[]).unwrap();
        assert!(!c.services_complete);
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
        assert_eq!(o.get(HOST_ID), Some(&true));
        assert_eq!(parse_outcomes("not json"), None);
    }

    #[test]
    fn a_422_quarantines_exactly_the_events_it_names() {
        let mut c = host_report(HOST_ID.into(), "host", &facts(), None, None);
        c.events = vec![ev("good-1"), ev("bad-1"), ev("good-2")];
        let sent = ComputerReportReq {
            device_id: "d".into(),
            computers: vec![c.clone()],
        };
        let body = r#"{"error":"invalid computer report","problems":["computers[0].events[1].observed_at: 2027-01-01 is more than 300s in the future (reporter clock?)"]}"#;
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
            1
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
        // The whole-queue quarantine leaves only the marker.
        assert_eq!(quarantine(&mut st, HOST_ID, None, &other, t), 3);
        assert_eq!(st.pending[HOST_ID].len(), 1);
        assert_eq!(st.pending[HOST_ID][0].detail["dropped_events"], 3);
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

    /// No timer, task or thread of its own — a task would escape the gate.
    #[test]
    fn the_reporter_has_no_timer_of_its_own() {
        for (name, src) in [
            ("mod.rs", include_str!("mod.rs")),
            ("services.rs", include_str!("services.rs")),
            ("events.rs", include_str!("events.rs")),
            ("wsl_guest.rs", include_str!("wsl_guest.rs")),
            ("access.rs", include_str!("access.rs")),
            ("identity.rs", include_str!("identity.rs")),
        ] {
            let prod = src
                .split_once("\n#[cfg(test)]")
                .map(|(a, _)| a)
                .unwrap_or(src);
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
