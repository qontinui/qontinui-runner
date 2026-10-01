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
//! Until coord's Phase 3 route deploys, the POST answers 404/405. That is
//! expected, logged ONCE at info, and backs off to the full-snapshot cadence;
//! events stay queued (bounded) and are delivered once the route exists. The
//! sampler is never blocked by it.

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
        hostname: facts.hostname.clone(),
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
            hostname: g.hostname.clone(),
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
        scan: if g.systemd {
            ServiceScan {
                units: Some(units),
                complete: true,
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

/// The per-process reporter, owned by the sampler loop's closure.
pub(crate) struct Reporter {
    state: Option<ObserverState>,
    state_path: Option<PathBuf>,
    last_full: Option<tokio::time::Instant>,
    /// Row state keys coord last acknowledged, per identity hash.
    last_sent: BTreeMap<String, BTreeMap<String, StateKey>>,
    /// Backoff after a 404/405 from a coord that predates the route.
    route_absent_until: Option<tokio::time::Instant>,
    route_absent_logged: bool,
    failures_logged: u32,
    no_identity_logged: bool,
    facts: Option<StaticFacts>,
    access: Option<Access>,
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
            last_sent: BTreeMap::new(),
            route_absent_until: None,
            route_absent_logged: false,
            failures_logged: 0,
            no_identity_logged: false,
            facts: None,
            access: None,
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

    /// One tick. Best-effort end to end: every failure returns quietly.
    pub(crate) async fn tick(&mut self) {
        let now = chrono::Utc::now();
        let axes = crate::fleet::host_axes::collect();
        let Some(host_id) = identity::host_identity_hash() else {
            if !self.no_identity_logged {
                self.no_identity_logged = true;
                info!("fleet::computer: no OS machine id readable — this computer cannot be identified, so it is not reported");
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
        if full || self.facts.is_none() {
            self.facts = tokio::task::spawn_blocking(identity::collect_static)
                .await
                .ok();
            self.access = access::collect().await;
        }
        let facts = self.facts.clone().unwrap_or_default();

        // ---- observe --------------------------------------------------
        let mut observed = vec![Observed {
            base: host_report(
                host_id.clone(),
                identity::host_kind(),
                &facts,
                axes.boot_id.clone(),
                self.access.clone(),
            ),
            oom_kill_total: axes.oom_kill_total,
            scan: self.scan_host_services().await,
        }];
        #[cfg(windows)]
        for g in wsl_guest::probe::collect().await {
            observed.push(guest_observed(&g, &host_id));
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
            let (evs, next) = events::derive(state.computers.get(id), &input, now);
            state.computers.insert(id.clone(), next);
            if !evs.is_empty() {
                events::push_pending(state, id, evs);
            }
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
        match client
            .post(&url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    "fleet::computer: reported {} computer(s) ({})",
                    body.computers.len(),
                    if full { "full" } else { "delta" }
                );
                self.route_absent_until = None;
                self.on_delivered(&body, full);
            }
            Ok(resp)
                if resp.status() == reqwest::StatusCode::NOT_FOUND
                    || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED =>
            {
                if !self.route_absent_logged {
                    self.route_absent_logged = true;
                    info!(
                        "fleet::computer: coord answered HTTP {} for /coord/computers/report — this coord predates \
                         the computers route; events stay queued and the report retries every {REPORT_FULL_SECS}s",
                        resp.status()
                    );
                }
                self.route_absent_until =
                    Some(tokio::time::Instant::now() + Duration::from_secs(REPORT_FULL_SECS));
            }
            Ok(resp) => self.log_failure(format!("POST {url} -> HTTP {}", resp.status())),
            Err(e) => self.log_failure(format!("POST {url} failed: {e}")),
        }
    }

    /// coord accepted `body`: drop the delivered events and remember what it
    /// now holds.
    fn on_delivered(&mut self, body: &ComputerReportReq, full: bool) {
        if full {
            self.last_full = Some(tokio::time::Instant::now());
        }
        if let Some(state) = self.state.as_mut() {
            for c in &body.computers {
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
            "MACHINE_ID\tguest-machine-id-1\nPID1\tsystemd\nSHOW_BEGIN\nSHOW_END\nPROBE_END\n",
        )
        .unwrap();
        let o = guest_observed(&g, HOST_ID);
        assert_eq!(o.base.kind, "wsl_guest");
        assert_eq!(o.base.parent_identity_hash.as_deref(), Some(HOST_ID));
        assert!(!o.base.attach_device);
        assert_eq!(o.scan.units.as_ref().map(Vec::len), Some(0));
        assert!(
            o.scan.complete,
            "systemd answered: zero units is a real zero"
        );

        let no_systemd = wsl_guest::parse_guest_probe(
            "MACHINE_ID\tguest-machine-id-1\nPID1\tinit\nSHOW_BEGIN\nSHOW_END\nPROBE_END\n",
        )
        .unwrap();
        assert_eq!(
            guest_observed(&no_systemd, HOST_ID).scan,
            ServiceScan::default()
        );
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
