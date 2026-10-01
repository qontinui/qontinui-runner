//! Computer events (plan §3.3, Phase 2.4): `oom_kill`, `service_failed`,
//! `service_recovered`, `reboot`, `telemetry_gap` — derived by comparing this
//! tick's observation with the last one.
//!
//! ## Why the observer state is on disk
//!
//! Every event here is a CHANGE, and the runner process does not outlive the
//! things it has to notice: a reboot kills it, and an OOM-killed box is
//! exactly when a runner restart is likely. An in-memory "previous" would
//! reset to "first observation, no events" at the worst possible moment. So
//! the previous observation (boot id, the `oom_kill` counter, each unit's
//! state) and the unsent events live in `<runner_dir>/computer-observer.json`.
//!
//! ## Stable, content-derived event ids
//!
//! `client_event_id` is derived from what the event IS (kind, unit, boot, the
//! counter value or the unit's own `StateChangeTimestamp`) — never from when
//! this process happened to notice it — so a re-derivation after a crash that
//! lost the state write produces the SAME id and coord's
//! `UNIQUE (computer_id, client_event_id)` dedupes it.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::services::WatchedUnit;

/// Upper bound on unsent events per computer. Beyond it the OLDEST are
/// dropped and a `telemetry_gap` marker says how many — a silent drop would
/// read as "nothing happened".
pub(crate) const MAX_PENDING_PER_COMPUTER: usize = 200;

/// One `computer_events` row on the wire (contract §3 `events[]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ComputerEvent {
    pub(crate) client_event_id: String,
    pub(crate) kind: String,
    pub(crate) observed_at: String,
    pub(crate) detail: serde_json::Value,
}

/// The last observation of one unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct UnitObs {
    pub(crate) active_state: Option<String>,
    pub(crate) result: Option<String>,
    pub(crate) state_changed_at: Option<String>,
    pub(crate) n_restarts: Option<u32>,
    pub(crate) cgroup_oom_kill: Option<u64>,
    /// A `service_failed` was emitted (or the unit was first seen failed) and
    /// no `service_recovered` has closed it yet.
    #[serde(default)]
    pub(crate) failed_open: bool,
}

/// The last observation of one computer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct ComputerObs {
    pub(crate) boot_id: Option<String>,
    pub(crate) oom_kill_total: Option<u64>,
    pub(crate) units: BTreeMap<String, UnitObs>,
    /// Last reported boot time — the reboot signal where the platform has no
    /// boot id (Windows).
    #[serde(default)]
    pub(crate) booted_at: Option<String>,
    /// When this computer was last observed, for pruning vanished guests.
    #[serde(default)]
    pub(crate) last_seen_at: Option<String>,
    /// `host` | `wsl_guest` | … — set by the reporter.
    #[serde(default)]
    pub(crate) kind: Option<String>,
}

/// How far two `booted_at` readings may disagree and still be the same boot.
/// Windows derives boot time from uptime, which jitters by a second or so.
///
/// **Reboot-id precision (Windows).** The id is the boot time rounded to this
/// window. sysinfo derives it from uptime, which jitters by about a second, so
/// a boot whose true time sits within that jitter of a bucket edge can be
/// read into two buckets: roughly 1 boot in 120 yields two `reboot` events
/// with different ids. Conversely two restarts less than this window apart
/// read as ONE boot and the second is not reported. A stable source
/// (`Win32_OperatingSystem.LastBootUpTime`) needs a WMI query — a COM call or
/// a PowerShell fork per tick — which is not cheap on this path, so the
/// rounding is the documented trade-off.
///
/// **Blind spot — Windows Fast Startup.** With Fast Startup on (the Windows
/// default), "Shut down" hibernates the kernel instead of ending it, so a
/// shutdown + power-on keeps the old boot time and is NOT seen as a reboot.
/// Only a Restart (or a shutdown with Fast Startup off) moves `booted_at`.
pub(crate) const BOOTED_AT_TOLERANCE_SECS: i64 = 120;

/// A computer not observed for this long is forgotten (a deleted or renamed
/// WSL distro would otherwise keep its state and queue forever).
pub(crate) const PRUNE_AFTER_SECS: i64 = 7 * 24 * 3600;

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Everything persisted between ticks and across runner restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct ObserverState {
    #[serde(default)]
    pub(crate) computers: BTreeMap<String, ComputerObs>,
    /// Unsent events per `identity_hash`.
    #[serde(default)]
    pub(crate) pending: BTreeMap<String, Vec<ComputerEvent>>,
    /// When the gated loop last ticked (RFC3339), for `telemetry_gap`.
    #[serde(default)]
    pub(crate) last_tick_at: Option<String>,
    #[serde(default)]
    pub(crate) last_tick_boot_id: Option<String>,
}

/// This tick's observation of one computer, as the deriver needs it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ObsInput<'a> {
    pub(crate) boot_id: Option<&'a str>,
    pub(crate) booted_at: Option<&'a str>,
    pub(crate) oom_kill_total: Option<u64>,
    /// `None` = no service information this tick (keep the previous units).
    pub(crate) services: Option<&'a [WatchedUnit]>,
    pub(crate) services_complete: bool,
    /// Keep the previous `oom_kill_total` when this tick has none. `false`
    /// for a WSL guest: the VM-wide counter is owned by one guest per VM, and
    /// a guest that loses ownership must CLEAR its baseline — carrying it would
    /// produce a bogus delta if it regains ownership later.
    pub(crate) carry_oom_baseline: bool,
}

pub(crate) fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `<kind>:<32 hex>` — a SHA-256 over the kind and the identifying parts.
pub(crate) fn event_id(kind: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    for p in parts {
        h.update([0x1f]);
        h.update(p.as_bytes());
    }
    let digest = h.finalize();
    format!("{kind}:{}", hex::encode(&digest[..16]))
}

fn is_up(state: Option<&str>) -> bool {
    matches!(
        state,
        Some("active" | "reloading" | "activating" | "deactivating")
    )
}

fn is_down(state: Option<&str>) -> bool {
    matches!(state, Some("failed" | "inactive"))
}

/// Derive this tick's events and the next observation.
pub(crate) fn derive(
    prev: Option<&ComputerObs>,
    input: &ObsInput<'_>,
    now: DateTime<Utc>,
) -> (Vec<ComputerEvent>, ComputerObs) {
    let now_s = rfc3339(now);
    let empty = ComputerObs::default();
    let first_ever = prev.is_none();
    let prev = prev.unwrap_or(&empty);
    let boot = input.boot_id.unwrap_or("");
    let mut events = Vec::new();

    // ---- reboot ---------------------------------------------------------
    // By boot id where both readings have one; otherwise (Windows) by a
    // `booted_at` that moved by more than the uptime jitter.
    let by_boot_id = matches!(
        (prev.boot_id.as_deref(), input.boot_id),
        (Some(a), Some(b)) if a != b
    );
    let by_booted_at = (prev.boot_id.is_none() || input.boot_id.is_none())
        && match (
            prev.booted_at.as_deref().and_then(parse_ts),
            input.booted_at.and_then(parse_ts),
        ) {
            (Some(a), Some(b)) => (b - a).num_seconds().abs() > BOOTED_AT_TOLERANCE_SECS,
            _ => false,
        };
    let rebooted = by_boot_id || by_booted_at;
    if rebooted {
        let id = if by_boot_id {
            event_id("reboot", &[boot])
        } else {
            // Rounded to the tolerance window: Windows derives boot time from
            // uptime, so two runner processes can read the same boot a second
            // apart — the id must not differ for that.
            let bucket = input
                .booted_at
                .and_then(parse_ts)
                .map(|t| {
                    t.timestamp().div_euclid(BOOTED_AT_TOLERANCE_SECS) * BOOTED_AT_TOLERANCE_SECS
                })
                .map(|b| b.to_string())
                .unwrap_or_default();
            event_id("reboot", &["booted_at", &bucket])
        };
        events.push(ComputerEvent {
            client_event_id: id,
            kind: "reboot".into(),
            observed_at: input.booted_at.map(str::to_string).unwrap_or(now_s.clone()),
            detail: json!({
                "previous_boot_id": prev.boot_id,
                "boot_id": input.boot_id,
                "booted_at": input.booted_at,
            }),
        });
    }

    // ---- per-unit: OOM attribution and state transitions -----------------
    let mut units = prev.units.clone();
    let mut attributed: u64 = 0;
    if let Some(current) = input.services {
        if input.services_complete {
            units.retain(|name, _| current.iter().any(|w| &w.row.unit == name));
        }
        for w in current {
            let r = &w.row;
            let cur_state = r.active_state.as_deref();
            let Some(p) = prev.units.get(&r.unit) else {
                // First sight of this unit: its current state is a baseline,
                // not a transition. A unit first seen `failed` is marked open
                // so its eventual recovery is reported.
                units.insert(
                    r.unit.clone(),
                    UnitObs {
                        active_state: r.active_state.clone(),
                        result: r.result.clone(),
                        state_changed_at: r.state_changed_at.clone(),
                        n_restarts: r.n_restarts,
                        cgroup_oom_kill: w.cgroup_oom_kill,
                        failed_open: cur_state == Some("failed"),
                    },
                );
                continue;
            };
            let prev_state = p.active_state.as_deref();
            let transition_at = r.state_changed_at.clone().unwrap_or(now_s.clone());
            let changed =
                p.state_changed_at != r.state_changed_at || p.active_state != r.active_state;

            // OOM victim: a cgroup `memory.events` delta while the cgroup
            // lives, else the unit's own `Result=oom-kill` on this transition
            // (with `OOMPolicy=stop` the cgroup is gone by the next tick —
            // exactly the 2026-09-30 shape).
            let cg_delta = match (p.cgroup_oom_kill, w.cgroup_oom_kill) {
                (Some(a), Some(b)) if b > a && !rebooted => Some(b - a),
                _ => None,
            };
            let victim = if let Some(n) = cg_delta {
                Some((
                    n,
                    "cgroup_memory_events",
                    now_s.clone(),
                    w.cgroup_oom_kill.unwrap_or(0).to_string(),
                ))
            } else if r.result.as_deref() == Some("oom-kill")
                && is_down(cur_state)
                && (p.result.as_deref() != Some("oom-kill")
                    || p.state_changed_at != r.state_changed_at)
            {
                Some((
                    1,
                    "unit_result",
                    transition_at.clone(),
                    transition_at.clone(),
                ))
            } else {
                None
            };
            if let Some((count, how, at, key)) = victim {
                attributed = attributed.saturating_add(count);
                events.push(ComputerEvent {
                    client_event_id: event_id("oom_kill", &[boot, &r.unit, how, &key]),
                    kind: "oom_kill".into(),
                    observed_at: at,
                    detail: json!({
                        "victim_unit": r.unit,
                        "unit_kind": r.kind,
                        "count": count,
                        "attribution": how,
                        "oom_kill_total": input.oom_kill_total,
                        "oom_policy": r.oom_policy,
                        "memory_peak": r.memory_peak,
                        "runner_name": r.runner_name,
                    }),
                });
            }

            let mut failed_open = p.failed_open;
            let base_detail = |kind: &str| {
                json!({
                    "unit": r.unit,
                    "unit_kind": r.kind,
                    "from": prev_state,
                    "to": cur_state,
                    "sub_state": r.sub_state,
                    "result": r.result,
                    "restart_policy": r.restart_policy,
                    "oom_policy": r.oom_policy,
                    "n_restarts": r.n_restarts,
                    "runner_name": r.runner_name,
                    "event": kind,
                })
            };
            if is_down(cur_state)
                && !failed_open
                && changed
                && (cur_state == Some("failed") || is_up(prev_state))
            {
                events.push(ComputerEvent {
                    client_event_id: event_id("service_failed", &[boot, &r.unit, &transition_at]),
                    kind: "service_failed".into(),
                    observed_at: transition_at.clone(),
                    detail: base_detail("service_failed"),
                });
                failed_open = true;
            } else if cur_state == Some("active") && failed_open {
                events.push(ComputerEvent {
                    client_event_id: event_id(
                        "service_recovered",
                        &[boot, &r.unit, &transition_at],
                    ),
                    kind: "service_recovered".into(),
                    observed_at: transition_at.clone(),
                    detail: base_detail("service_recovered"),
                });
                failed_open = false;
            } else if cur_state == Some("active")
                && prev_state == Some("active")
                && matches!((p.n_restarts, r.n_restarts), (Some(a), Some(b)) if b > a)
            {
                // Died and was auto-restarted between two ticks: `NRestarts`
                // counts only automatic restarts, so this is a crash the
                // state words alone never show.
                let n = r.n_restarts.unwrap_or(0).to_string();
                let mut d = base_detail("service_failed");
                d["auto_restarted"] = json!(true);
                events.push(ComputerEvent {
                    client_event_id: event_id("service_failed", &[boot, &r.unit, "n_restarts", &n]),
                    kind: "service_failed".into(),
                    observed_at: transition_at.clone(),
                    detail: d,
                });
                let mut d = base_detail("service_recovered");
                d["auto_restarted"] = json!(true);
                events.push(ComputerEvent {
                    client_event_id: event_id(
                        "service_recovered",
                        &[boot, &r.unit, "n_restarts", &n],
                    ),
                    kind: "service_recovered".into(),
                    observed_at: transition_at.clone(),
                    detail: d,
                });
            }

            units.insert(
                r.unit.clone(),
                UnitObs {
                    active_state: r.active_state.clone(),
                    result: r.result.clone(),
                    state_changed_at: r.state_changed_at.clone(),
                    n_restarts: r.n_restarts,
                    cgroup_oom_kill: w.cgroup_oom_kill,
                    failed_open,
                },
            );
        }
    }

    // ---- machine-wide OOM kills no watched unit accounts for ------------
    // After a reboot the counter restarted at 0, so the baseline is 0: kills
    // between boot and this first tick (an early-boot OOM loop) are reported
    // rather than silently absorbed into the new baseline.
    if !first_ever {
        let baseline = if rebooted {
            Some(0)
        } else {
            prev.oom_kill_total
        };
        if let (Some(a), Some(b)) = (baseline, input.oom_kill_total) {
            if b > a {
                let unattributed = (b - a).saturating_sub(attributed);
                if unattributed > 0 {
                    let how = if rebooted {
                        "vmstat_since_boot"
                    } else {
                        "vmstat_delta"
                    };
                    events.push(ComputerEvent {
                        client_event_id: event_id("oom_kill", &[boot, "", &b.to_string()]),
                        kind: "oom_kill".into(),
                        observed_at: now_s.clone(),
                        detail: json!({
                            "victim_unit": null,
                            "count": unattributed,
                            "attribution": how,
                            "oom_kill_total": b,
                            "previous_oom_kill_total": a,
                        }),
                    });
                }
            }
        }
    }

    let next = ComputerObs {
        boot_id: input.boot_id.map(str::to_string).or(prev.boot_id.clone()),
        oom_kill_total: input
            .oom_kill_total
            .or(if rebooted || !input.carry_oom_baseline {
                None
            } else {
                prev.oom_kill_total
            }),
        units,
        booted_at: input
            .booted_at
            .map(str::to_string)
            .or(prev.booted_at.clone()),
        last_seen_at: Some(now_s),
        kind: prev.kind.clone(),
    };
    (events, next)
}

/// Forget computers (and their queues) not observed for
/// [`PRUNE_AFTER_SECS`], except `keep` (the host). Returns how many went.
pub(crate) fn prune(state: &mut ObserverState, keep: &str, now: DateTime<Utc>) -> usize {
    let stale: Vec<String> = state
        .computers
        .iter()
        .filter(|(id, c)| {
            id.as_str() != keep
                && c.last_seen_at
                    .as_deref()
                    .and_then(parse_ts)
                    .is_some_and(|t| (now - t).num_seconds() > PRUNE_AFTER_SECS)
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &stale {
        state.computers.remove(id);
        state.pending.remove(id);
    }
    stale.len()
}

/// A `telemetry_gap` when the gated loop's previous tick is more than 3× the
/// configured cadence ago — the sampler itself starved (a fleet host on
/// 2026-09-30: a 33-minute hole spanning the OOM kill), or the runner was not
/// running at all. `None` on the first tick ever and on a normal cadence.
pub(crate) fn telemetry_gap(
    last_tick_at: Option<&str>,
    last_tick_boot_id: Option<&str>,
    now: DateTime<Utc>,
    expected_secs: u64,
    boot_id: Option<&str>,
    load_1m_at_resume: Option<f64>,
) -> Option<ComputerEvent> {
    let last_s = last_tick_at?;
    let last = DateTime::parse_from_rfc3339(last_s)
        .ok()?
        .with_timezone(&Utc);
    let gap_secs = (now - last).num_seconds();
    let threshold = expected_secs.saturating_mul(3);
    if gap_secs <= i64::try_from(threshold).unwrap_or(i64::MAX) {
        return None;
    }
    let across_reboot = matches!((last_tick_boot_id, boot_id), (Some(a), Some(b)) if a != b);
    Some(ComputerEvent {
        client_event_id: event_id("telemetry_gap", &[boot_id.unwrap_or(""), last_s]),
        kind: "telemetry_gap".into(),
        observed_at: rfc3339(now),
        detail: json!({
            "gap_secs": gap_secs,
            "expected_secs": expected_secs,
            "threshold_secs": threshold,
            "last_tick_at": last_s,
            "resumed_at": rfc3339(now),
            "load_1m_at_resume": load_1m_at_resume,
            "across_reboot": across_reboot,
        }),
    })
}

/// Queue events for a computer: dedupe by id, then bound the queue, replacing
/// what overflowed with one `telemetry_gap` marker counting it.
pub(crate) fn push_pending(state: &mut ObserverState, identity: &str, new: Vec<ComputerEvent>) {
    let q = state.pending.entry(identity.to_string()).or_default();
    for e in new {
        if !q.iter().any(|x| x.client_event_id == e.client_event_id) {
            q.push(e);
        }
    }
    if q.len() > MAX_PENDING_PER_COMPUTER {
        let overflow = q.len() - (MAX_PENDING_PER_COMPUTER - 1);
        let dropped: Vec<ComputerEvent> = q.drain(..overflow).collect();
        // A previous marker folded into this one keeps the running total.
        let dropped_total: u64 = dropped
            .iter()
            .map(|e| {
                // Both marker kinds carry a running total: an earlier
                // buffer-full marker and a `rejected_by_coord` marker.
                if matches!(
                    e.detail.get("reason").and_then(|r| r.as_str()),
                    Some("pending_event_buffer_full" | "rejected_by_coord")
                ) {
                    e.detail
                        .get("dropped_events")
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0)
                } else {
                    1
                }
            })
            .sum();
        let first_id = dropped
            .first()
            .map(|e| e.client_event_id.as_str())
            .unwrap_or("");
        let marker = ComputerEvent {
            client_event_id: event_id(
                "telemetry_gap",
                &["dropped", first_id, &dropped_total.to_string()],
            ),
            kind: "telemetry_gap".into(),
            observed_at: dropped
                .first()
                .map(|e| e.observed_at.clone())
                .unwrap_or_default(),
            detail: json!({
                "reason": "pending_event_buffer_full",
                "dropped_events": dropped_total,
            }),
        };
        q.insert(0, marker);
    }
}

/// Load the observer state; a missing file is a fresh state (first
/// observation — baselines, no events).
///
/// A file that exists but does not parse is NOT silently replaced: it is
/// copied aside to `*.json.corrupt` (the queued events in it are evidence)
/// and logged, and the reporter starts fresh.
pub(crate) fn load(path: &Path) -> ObserverState {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ObserverState::default(),
        Err(e) => {
            // Exists but unreadable (permissions, I/O, a directory): keep
            // whatever can be copied and say so — never silently fresh.
            let aside = path.with_extension("json.corrupt");
            let kept = std::fs::copy(path, &aside).is_ok();
            tracing::warn!(
                "fleet::computer: observer state {} could not be read ({e}); starting fresh{}",
                path.display(),
                if kept {
                    " (a copy is kept as .json.corrupt)"
                } else {
                    ""
                }
            );
            return ObserverState::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            let aside = path.with_extension("json.corrupt");
            let kept = std::fs::copy(path, &aside).is_ok();
            tracing::warn!(
                "fleet::computer: observer state {} does not parse ({e}); starting fresh{}",
                path.display(),
                if kept {
                    format!(", the unreadable file is kept at {}", aside.display())
                } else {
                    String::new()
                }
            );
            ObserverState::default()
        }
    }
}

/// Atomic, durable write: temp file, `fsync`, rename. Best-effort: a failure
/// costs this tick's persistence, which the content-derived ids make safe.
pub(crate) fn save(path: &Path, state: &ObserverState) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec(state)?)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::super::services::{parse_systemctl_show, row_from_props, tests as svc};
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const BOOT: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";

    fn watched(show: &str, cgroup: Option<u64>) -> Vec<WatchedUnit> {
        parse_systemctl_show(show)
            .iter()
            .map(|p| WatchedUnit {
                row: row_from_props(p, None),
                cgroup_oom_kill: cgroup,
            })
            .collect()
    }

    fn input<'a>(units: &'a [WatchedUnit], oom: Option<u64>) -> ObsInput<'a> {
        ObsInput {
            boot_id: Some(BOOT),
            booted_at: Some("2026-09-21T14:13:20Z"),
            oom_kill_total: oom,
            services: Some(units),
            services_complete: true,
            carry_oom_baseline: true,
        }
    }

    const ACTIVE_BEFORE: &str = "\
Id=actions.runner.example-org-example-repo.fleetbox.service
ActiveState=active
SubState=running
Result=success
Restart=no
OOMPolicy=stop
NRestarts=0
StateChangeTimestamp=Tue 2026-09-29 09:12:40 UTC
ControlGroup=/system.slice/actions.runner.example-org-example-repo.fleetbox.service
";

    /// The incident, replayed: active (vmstat oom_kill=5) → the unit is
    /// OOM-killed and stays down (vmstat 6, cgroup gone). Expect exactly one
    /// attributed `oom_kill` and one `service_failed`, no unattributed kill.
    #[test]
    fn the_incident_yields_an_attributed_oom_kill_and_a_service_failure() {
        let before = watched(ACTIVE_BEFORE, Some(0));
        let (ev0, obs0) = derive(None, &input(&before, Some(5)), t("2026-09-30T01:40:00Z"));
        assert!(ev0.is_empty(), "first observation is a baseline");

        let after = watched(svc::INCIDENT_SHOW, None);
        let (ev, obs1) = derive(
            Some(&obs0),
            &input(&after, Some(6)),
            t("2026-09-30T01:48:40Z"),
        );
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["oom_kill", "service_failed"]);

        let oom = &ev[0];
        assert_eq!(oom.observed_at, "2026-09-30T01:48:16Z");
        assert_eq!(
            oom.detail["victim_unit"],
            "actions.runner.example-org-example-repo.fleetbox.service"
        );
        assert_eq!(oom.detail["attribution"], "unit_result");
        assert_eq!(oom.detail["count"], 1);
        assert_eq!(oom.detail["oom_kill_total"], 6);
        assert_eq!(oom.detail["oom_policy"], "stop");

        let failed = &ev[1];
        assert_eq!(failed.detail["from"], "active");
        assert_eq!(failed.detail["to"], "failed");
        assert_eq!(failed.detail["result"], "oom-kill");
        assert_eq!(failed.detail["restart_policy"], "no");

        // Idempotent: the same observation again derives nothing new.
        let (again, _) = derive(
            Some(&obs1),
            &input(&after, Some(6)),
            t("2026-09-30T01:49:10Z"),
        );
        assert!(again.is_empty(), "{again:?}");

        // Stable ids: a re-derivation from the same prior state (a lost
        // state write) yields byte-identical ids.
        let (ev_redo, _) = derive(
            Some(&obs0),
            &input(&after, Some(6)),
            t("2026-09-30T01:49:30Z"),
        );
        let ids = |v: &[ComputerEvent]| {
            v.iter()
                .map(|e| e.client_event_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&ev), ids(&ev_redo));
    }

    #[test]
    fn a_cgroup_delta_attributes_and_the_remainder_is_unattributed() {
        let before = watched(ACTIVE_BEFORE, Some(2));
        let (_, obs0) = derive(None, &input(&before, Some(10)), t("2026-09-30T01:00:00Z"));
        // The runner's cgroup killed one process (cgroup 2 → 3) but the unit
        // survived (OOMPolicy=continue); the machine killed two more elsewhere.
        let after = watched(ACTIVE_BEFORE, Some(3));
        let (ev, _) = derive(
            Some(&obs0),
            &input(&after, Some(13)),
            t("2026-09-30T01:00:30Z"),
        );
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(ev[0].detail["count"], 1);
        assert_eq!(ev[1].detail["attribution"], "vmstat_delta");
        assert_eq!(ev[1].detail["victim_unit"], serde_json::Value::Null);
        assert_eq!(ev[1].detail["count"], 2);
        assert_eq!(ev[1].detail["previous_oom_kill_total"], 10);
    }

    #[test]
    fn recovery_closes_a_failure_exactly_once() {
        let failed = watched(svc::INCIDENT_SHOW, None);
        let (_, obs0) = derive(None, &input(&failed, Some(6)), t("2026-09-30T02:00:00Z"));
        let back = watched(
            &ACTIVE_BEFORE.replace("Tue 2026-09-29 09:12:40 UTC", "Wed 2026-09-30 18:28:57 UTC"),
            Some(0),
        );
        let (ev, obs1) = derive(
            Some(&obs0),
            &input(&back, Some(6)),
            t("2026-09-30T18:29:10Z"),
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "service_recovered");
        assert_eq!(ev[0].observed_at, "2026-09-30T18:28:57Z");
        let (ev2, _) = derive(
            Some(&obs1),
            &input(&back, Some(6)),
            t("2026-09-30T18:29:40Z"),
        );
        assert!(ev2.is_empty());
    }

    #[test]
    fn an_auto_restart_between_ticks_is_a_failure_and_a_recovery() {
        let before = watched(ACTIVE_BEFORE, None);
        let (_, obs0) = derive(None, &input(&before, None), t("2026-09-30T01:00:00Z"));
        let after = watched(
            &ACTIVE_BEFORE
                .replace("NRestarts=0", "NRestarts=1")
                .replace("Tue 2026-09-29 09:12:40 UTC", "Wed 2026-09-30 01:00:12 UTC"),
            None,
        );
        let (ev, _) = derive(Some(&obs0), &input(&after, None), t("2026-09-30T01:00:30Z"));
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["service_failed", "service_recovered"]);
        assert_eq!(ev[0].detail["auto_restarted"], true);
    }

    #[test]
    fn a_boot_id_change_is_a_reboot_and_resets_the_oom_baseline() {
        let units = watched(ACTIVE_BEFORE, None);
        let (_, obs0) = derive(None, &input(&units, Some(40)), t("2026-09-30T01:00:00Z"));
        let new_boot = "11111111-2222-4333-8444-555555555555";
        let inp = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-09-30T03:00:00Z"),
            oom_kill_total: Some(1),
            services: Some(&units),
            services_complete: true,
            carry_oom_baseline: true,
        };
        let (ev, obs1) = derive(Some(&obs0), &inp, t("2026-09-30T03:01:00Z"));
        // The reboot, plus the one kill counted since that boot (baseline 0).
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev[0].kind, "reboot");
        assert_eq!(ev[1].detail["attribution"], "vmstat_since_boot");
        assert_eq!(ev[0].observed_at, "2026-09-30T03:00:00Z");
        assert_eq!(ev[0].detail["previous_boot_id"], BOOT);
        assert_eq!(ev[0].client_event_id, event_id("reboot", &[new_boot]));
        assert_eq!(obs1.boot_id.as_deref(), Some(new_boot));
        assert_eq!(obs1.oom_kill_total, Some(1));
    }

    /// Kills between boot and the first tick are reported, attributed to
    /// nothing, against a baseline of 0 — not absorbed into the new baseline.
    #[test]
    fn after_a_reboot_the_oom_baseline_is_zero() {
        let units = watched(ACTIVE_BEFORE, None);
        let (_, obs0) = derive(None, &input(&units, Some(40)), t("2026-09-30T01:00:00Z"));
        let new_boot = "11111111-2222-4333-8444-555555555555";
        let inp = ObsInput {
            boot_id: Some(new_boot),
            booted_at: Some("2026-09-30T03:00:00Z"),
            oom_kill_total: Some(2),
            services: Some(&units),
            services_complete: true,
            carry_oom_baseline: true,
        };
        let (ev, _) = derive(Some(&obs0), &inp, t("2026-09-30T03:01:00Z"));
        let kinds: Vec<&str> = ev.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["reboot", "oom_kill"]);
        assert_eq!(ev[1].detail["count"], 2);
        assert_eq!(ev[1].detail["attribution"], "vmstat_since_boot");
        assert_eq!(ev[1].detail["previous_oom_kill_total"], 0);
    }

    /// Windows: no boot id, so a reboot is a `booted_at` that moved by more
    /// than the uptime jitter; a jitter-sized move is not one.
    #[test]
    fn without_a_boot_id_a_moved_booted_at_is_a_reboot() {
        let mk = |booted: &'static str| ObsInput {
            boot_id: None,
            booted_at: Some(booted),
            oom_kill_total: None,
            services: None,
            services_complete: false,
            carry_oom_baseline: true,
        };
        let (_, o0) = derive(None, &mk("2026-09-29T08:00:00Z"), t("2026-09-30T01:00:00Z"));
        let (jitter, o1) = derive(
            Some(&o0),
            &mk("2026-09-29T08:00:02Z"),
            t("2026-09-30T01:00:30Z"),
        );
        assert!(jitter.is_empty());
        let (ev, _) = derive(
            Some(&o1),
            &mk("2026-09-30T02:00:00Z"),
            t("2026-09-30T02:05:00Z"),
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "reboot");
        assert_eq!(ev[0].observed_at, "2026-09-30T02:00:00Z");
        // The id is keyed on the 120 s bucket (1790733600 = 02:00:00Z), so a
        // second reader of the same boot a second later mints the SAME id.
        assert_eq!(
            ev[0].client_event_id,
            event_id("reboot", &["booted_at", "1790733600"])
        );
        let (ev2, _) = derive(
            Some(&o1),
            &mk("2026-09-30T02:00:01Z"),
            t("2026-09-30T02:05:00Z"),
        );
        assert_eq!(ev2[0].client_event_id, ev[0].client_event_id);
    }

    /// A guest that loses VM-wide OOM ownership clears its baseline; a host
    /// keeps its baseline through a tick with no reading.
    #[test]
    fn a_guest_that_loses_oom_ownership_clears_its_baseline() {
        let units = watched(ACTIVE_BEFORE, None);
        let (_, o0) = derive(None, &input(&units, Some(7)), t("2026-09-30T01:00:00Z"));
        let lost = ObsInput {
            oom_kill_total: None,
            carry_oom_baseline: false,
            ..input(&units, None)
        };
        let (_, o1) = derive(Some(&o0), &lost, t("2026-09-30T01:00:30Z"));
        assert_eq!(o1.oom_kill_total, None);
        let host_gap = ObsInput {
            oom_kill_total: None,
            ..input(&units, None)
        };
        let (_, h1) = derive(Some(&o0), &host_gap, t("2026-09-30T01:00:30Z"));
        assert_eq!(h1.oom_kill_total, Some(7));
        // Regaining ownership with the VM counter at 9: no bogus 7→9 delta,
        // the reading is a fresh baseline.
        let (ev, o2) = derive(
            Some(&o1),
            &input(&units, Some(9)),
            t("2026-09-30T01:01:00Z"),
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(o2.oom_kill_total, Some(9));
    }

    #[test]
    fn vanished_guests_are_pruned_but_the_host_never_is() {
        let mut st = ObserverState::default();
        let old = ComputerObs {
            last_seen_at: Some("2026-09-01T00:00:00Z".into()),
            ..ComputerObs::default()
        };
        let fresh = ComputerObs {
            last_seen_at: Some("2026-09-29T00:00:00Z".into()),
            ..ComputerObs::default()
        };
        st.computers.insert("host".into(), old.clone());
        st.computers.insert("gone-guest".into(), old);
        st.computers.insert("live-guest".into(), fresh);
        st.pending.insert("gone-guest".into(), vec![]);
        assert_eq!(prune(&mut st, "host", t("2026-09-30T00:00:00Z")), 1);
        let left: Vec<&str> = st.computers.keys().map(String::as_str).collect();
        assert_eq!(left, vec!["host", "live-guest"]);
        assert!(!st.pending.contains_key("gone-guest"));
    }

    #[test]
    fn a_corrupt_state_file_is_kept_aside_not_overwritten_silently() {
        let dir = std::env::temp_dir().join(format!(
            "qontinui-computer-observer-corrupt-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("computer-observer.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), ObserverState::default());
        assert_eq!(
            std::fs::read(dir.join("computer-observer.json.corrupt")).unwrap(),
            b"{not json"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_service_information_keeps_the_previous_units() {
        let units = watched(ACTIVE_BEFORE, None);
        let (_, obs0) = derive(None, &input(&units, Some(1)), t("2026-09-30T01:00:00Z"));
        let inp = ObsInput {
            services: None,
            services_complete: false,
            ..input(&units, Some(1))
        };
        let (ev, obs1) = derive(Some(&obs0), &inp, t("2026-09-30T01:00:30Z"));
        assert!(ev.is_empty());
        assert_eq!(obs1.units, obs0.units);
    }

    /// The Phase 0 census shape: the sampler starved for 33 minutes across
    /// the OOM kill (01:14:53 → 01:48:16).
    #[test]
    fn a_starved_loop_emits_a_telemetry_gap() {
        let ev = telemetry_gap(
            Some("2026-09-30T01:14:53Z"),
            Some(BOOT),
            t("2026-09-30T01:48:16Z"),
            30,
            Some(BOOT),
            Some(41.2),
        )
        .unwrap();
        assert_eq!(ev.kind, "telemetry_gap");
        assert_eq!(ev.observed_at, "2026-09-30T01:48:16Z");
        assert_eq!(
            ev.detail,
            json!({
                "gap_secs": 2003,
                "expected_secs": 30,
                "threshold_secs": 90,
                "last_tick_at": "2026-09-30T01:14:53Z",
                "resumed_at": "2026-09-30T01:48:16Z",
                "load_1m_at_resume": 41.2,
                "across_reboot": false,
            })
        );
        assert_eq!(
            ev.client_event_id,
            event_id("telemetry_gap", &[BOOT, "2026-09-30T01:14:53Z"])
        );
        // On cadence (even with jitter) and at the exact threshold: nothing.
        assert!(telemetry_gap(
            Some("2026-09-30T01:14:53Z"),
            None,
            t("2026-09-30T01:15:29Z"),
            30,
            None,
            None
        )
        .is_none());
        assert!(telemetry_gap(
            Some("2026-09-30T01:14:53Z"),
            None,
            t("2026-09-30T01:16:23Z"),
            30,
            None,
            None
        )
        .is_none());
        assert!(telemetry_gap(None, None, t("2026-09-30T01:16:23Z"), 30, None, None).is_none());
    }

    /// The in-guest runner owns the VM-wide counter, with full cgroup
    /// attribution: a kill inside `qontinui-runner.service`'s cgroup is ONE
    /// attributed event, with no unattributed remainder beside it; a kill
    /// elsewhere in the VM is the remainder, under the shared VM-wide id
    /// `oom_kill:[boot, "", total]` that the Windows host's probe would also
    /// use (so a race between the two dedupes in coord).
    #[test]
    fn a_kill_in_the_runners_own_cgroup_is_counted_once() {
        const RUNNER: &str = "\
Id=qontinui-runner.service
ActiveState=active
SubState=running
Result=success
OOMPolicy=continue
NRestarts=0
StateChangeTimestamp=Wed 2026-09-30 01:00:00 UTC
ControlGroup=/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service
";
        let before = watched(RUNNER, Some(0));
        let (_, o0) = derive(None, &input(&before, Some(5)), t("2026-09-30T01:40:00Z"));
        let after = watched(RUNNER, Some(1));
        let (ev, o1) = derive(
            Some(&o0),
            &input(&after, Some(6)),
            t("2026-09-30T01:40:30Z"),
        );
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].detail["attribution"], "cgroup_memory_events");
        assert_eq!(ev[0].detail["victim_unit"], "qontinui-runner.service");

        // A kill elsewhere in the VM: the remainder, under the shared id.
        let (ev, _) = derive(
            Some(&o1),
            &input(&after, Some(7)),
            t("2026-09-30T01:41:00Z"),
        );
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].detail["attribution"], "vmstat_delta");
        assert_eq!(
            ev[0].client_event_id,
            event_id("oom_kill", &[BOOT, "", "7"])
        );
    }

    #[test]
    fn overflow_carries_a_rejection_markers_total_forward() {
        let mut st = ObserverState::default();
        let marker = ComputerEvent {
            client_event_id: "telemetry_gap:rejected".into(),
            kind: "telemetry_gap".into(),
            observed_at: "t".into(),
            detail: json!({"reason": "rejected_by_coord", "dropped_events": 7}),
        };
        let mk = |i: usize| ComputerEvent {
            client_event_id: format!("e{i}"),
            kind: "oom_kill".into(),
            observed_at: format!("t{i}"),
            detail: json!({}),
        };
        let mut evs = vec![marker];
        evs.extend((0..MAX_PENDING_PER_COMPUTER).map(mk));
        push_pending(&mut st, "h", evs);
        let q = &st.pending["h"];
        assert_eq!(q.len(), MAX_PENDING_PER_COMPUTER);
        // Dropped: the rejection marker (7) + e0 (1) = 8.
        assert_eq!(q[0].detail["reason"], "pending_event_buffer_full");
        assert_eq!(q[0].detail["dropped_events"], 8);
    }

    #[test]
    fn the_pending_queue_is_bounded_and_says_what_it_dropped() {
        let mut st = ObserverState::default();
        let mk = |i: usize| ComputerEvent {
            client_event_id: format!("e{i}"),
            kind: "oom_kill".into(),
            observed_at: format!("t{i}"),
            detail: json!({}),
        };
        push_pending(
            &mut st,
            "h",
            (0..MAX_PENDING_PER_COMPUTER).map(mk).collect(),
        );
        assert_eq!(st.pending["h"].len(), MAX_PENDING_PER_COMPUTER);
        // Duplicates are not re-queued.
        push_pending(&mut st, "h", vec![mk(3)]);
        assert_eq!(st.pending["h"].len(), MAX_PENDING_PER_COMPUTER);
        push_pending(&mut st, "h", (1000..1005).map(mk).collect());
        let q = &st.pending["h"];
        assert_eq!(q.len(), MAX_PENDING_PER_COMPUTER);
        assert_eq!(q[0].kind, "telemetry_gap");
        assert_eq!(q[0].detail["dropped_events"], 6);
        assert_eq!(q.last().unwrap().client_event_id, "e1004");
    }

    #[test]
    fn observer_state_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "qontinui-computer-observer-test-{}",
            std::process::id()
        ));
        let path = dir.join("computer-observer.json");
        let mut st = ObserverState {
            last_tick_at: Some("2026-09-30T01:14:53Z".into()),
            ..ObserverState::default()
        };
        push_pending(
            &mut st,
            "h",
            vec![ComputerEvent {
                client_event_id: "x".into(),
                kind: "reboot".into(),
                observed_at: "2026-09-30T01:14:53Z".into(),
                detail: json!({"a": 1}),
            }],
        );
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
        assert_eq!(load(&dir.join("absent.json")), ObserverState::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
