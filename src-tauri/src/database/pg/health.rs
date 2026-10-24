//! `/health`'s `database` block, read from CACHED state.
//!
//! Plan `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`,
//! Phase 5 (D7). `/health` used to await a fresh `SELECT 1` (capped at 2 s)
//! and a schema probe on EVERY poll, so while the database was down every
//! poll — the supervisor watchdog, the UI, every probe script — paid ~2 s.
//! That was iter4 B-5's deliberate "intentionally NOT cached" choice: it
//! bought freshness by charging every poll the outage. The same freshness now
//! comes from:
//!
//! * a background refresher ([`spawn_health_refresher`]) that runs a real
//!   `SELECT 1` and the schema probe every 5 s while the breaker is Closed —
//!   through the guarded pool, so a failure trips the breaker;
//! * the breaker's prober, whose trials are the observations while it is Open
//!   (at most 15 s + one ≤ 5 s trial apart);
//! * an explicit age bound: an observation older than [`STALE_AFTER`] renders
//!   `reachable: null` with `stale: true`, never the last value (served policy
//!   `verification-and-evidence` `unknown-must-not-render-as-a-default`).
//!
//! `/restart-readiness` is a GATING consumer and keeps a fresh read
//! (`mcp_api::pg_liveness_probe`, now the pool's `probe_now`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use qontinui_runner_lib::circuit::{CircuitSnapshot, CircuitState};
use qontinui_runner_lib::pg_circuit::{rfc3339, CauseClass, ProbeObservation};

/// An observation older than this is UNKNOWN, not current. Pinned above the
/// prober's backoff cap (15 s) by a test.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// How often the refresher probes while the breaker is Closed.
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// The schema half of the last refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaObservation {
    pub schema_ok: Option<bool>,
    pub schema_detail: Option<String>,
    pub at_instant: Instant,
}

static SCHEMA: Mutex<Option<SchemaObservation>> = Mutex::new(None);
static REFRESHER_STARTED: AtomicBool = AtomicBool::new(false);

/// Everything `/health` needs about the database, read synchronously and with
/// no I/O.
#[derive(Debug, Clone)]
pub struct PgHealthSnapshot {
    /// A global `PgDb` exists (PG is configured, or a degraded pool is).
    pub configured: bool,
    /// The last probe: a refresher `SELECT 1`, a prober trial, or a
    /// `/restart-readiness` fresh read.
    pub probe: Option<ProbeObservation>,
    pub schema: Option<SchemaObservation>,
    pub breaker: Option<CircuitSnapshot>,
    pub cause_class: CauseClass,
}

/// Read the cached snapshot. Synchronous and I/O-free.
pub fn pg_health_snapshot() -> PgHealthSnapshot {
    let schema = SCHEMA.lock().unwrap_or_else(|p| p.into_inner()).clone();
    match super::PgDb::try_global() {
        None => PgHealthSnapshot {
            configured: false,
            probe: None,
            schema: None,
            breaker: None,
            cause_class: CauseClass::Unknown,
        },
        Some(pg) => PgHealthSnapshot {
            configured: true,
            probe: pg.pool().last_probe(),
            schema,
            breaker: Some(pg.pool().breaker_snapshot()),
            cause_class: pg.pool().cause_class(),
        },
    }
}

impl PgHealthSnapshot {
    fn breaker_closed(&self) -> bool {
        self.breaker
            .as_ref()
            .is_none_or(|b| b.state == CircuitState::Closed)
    }

    /// Age of the last probe.
    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.probe
            .as_ref()
            .map(|p| now.saturating_duration_since(p.at_instant))
    }

    /// No usable observation: never probed, or older than [`STALE_AFTER`].
    pub fn stale(&self, now: Instant) -> bool {
        self.configured && self.age(now).is_none_or(|a| a > STALE_AFTER)
    }

    /// `database.reachable`, tri-state:
    ///
    /// * `null` — no PG configured, nothing observed yet, or the last
    ///   observation is stale;
    /// * while the breaker is NOT Closed: `false` only when the latest
    ///   connect-class failure was a refused connect; a connect TIMEOUT (the
    ///   server may be alive behind a stalled process or box) is `null`, and
    ///   `breaker.causeClass` says why (plan D2);
    /// * Closed: the last probe's verdict.
    pub fn reachable(&self, now: Instant) -> Option<bool> {
        if !self.configured || self.stale(now) {
            return None;
        }
        if !self.breaker_closed() {
            return match self.cause_class {
                CauseClass::ConnectRefused => Some(false),
                _ => None,
            };
        }
        self.probe.as_ref().and_then(|p| p.reachable)
    }

    /// The `derived_status` input (`HealthInputs.pg_reachable`). Unlike
    /// [`Self::reachable`], this reports the RUNNER's own state: while the
    /// breaker is not Closed every DB-backed call fails fast, so the runner is
    /// degraded whatever the server is doing — `Some(false)`. Closed, it is
    /// the age-bounded probe verdict.
    pub fn status_reachable(&self, now: Instant) -> Option<bool> {
        if !self.configured {
            return None;
        }
        if !self.breaker_closed() {
            return Some(false);
        }
        self.reachable(now)
    }

    /// `schema_ok` / `schema_detail`, through the unchanged
    /// `derive_schema_health` rule (None, never false, unless the connect
    /// succeeded and a relation is provably absent).
    pub fn schema(&self, now: Instant) -> (Option<bool>, Option<String>) {
        let reachable = self.reachable(now);
        if reachable != Some(true) {
            return crate::mcp_api::derive_schema_health(reachable, None);
        }
        match &self.schema {
            Some(s) if now.saturating_duration_since(s.at_instant) <= STALE_AFTER => {
                (s.schema_ok, s.schema_detail.clone())
            }
            _ => crate::mcp_api::derive_schema_health(reachable, None),
        }
    }
}

/// The `/health` `database` block.
pub fn database_health_json(s: &PgHealthSnapshot, now: Instant) -> serde_json::Value {
    let (schema_ok, schema_detail) = s.schema(now);
    let breaker = s.breaker.as_ref().map(|b| {
        let open = b.state != CircuitState::Closed;
        serde_json::json!({
            "state": b.state.as_str(),
            "since": b.since.map(rfc3339),
            "cause": b.cause,
            // Meaningful only while the breaker is not Closed.
            "causeClass": if open { Some(s.cause_class.as_str()) } else { None },
            "lastError": b.last_error,
            "retryAfterMs": b.retry_after.map(|d| d.as_millis() as u64),
            "failedTrials": b.failed_trials,
            "trips": b.trips,
            // A wait timeout is saturation OR every slot held by in-flight
            // work; it never trips the breaker, and is counted here.
            "waitTimeouts": {
                "events": b.wait_timeouts,
                "lastAt": b.wait_timeout_last_at.map(rfc3339),
            },
            "backendErrors": b.backend_errors,
        })
    });
    serde_json::json!({
        // `null` when unconfigured, unobserved, stale, or Open on a connect
        // timeout; see `PgHealthSnapshot::reachable`.
        "reachable": s.reachable(now),
        // WHICH database this process is on — `reachable` only says whether
        // it answers. One of `external` / `embedded-owned` /
        // `embedded-attached` / `degraded`; `unknown` only before boot
        // reaches the PG block. Fixed at boot: it names the arm taken, not
        // current liveness, so a cluster that dies later flips `reachable`
        // and `breaker.state`, not `arm`.
        "arm": crate::embedded_pg::db_arm().as_str(),
        // The embedded cluster's loopback port, null off the embedded arms.
        "embeddedPort": crate::embedded_pg::embedded_port(),
        // WHICH data dir is behind that port. Null off the embedded arms.
        "embeddedDataRoot": crate::embedded_pg::embedded_data_root(),
        // The duration of the LAST BACKGROUND probe (refresher, prober trial
        // or a /restart-readiness fresh read) — no longer a cost this
        // response paid. `/health` does no PG I/O at all.
        "probeMs": s.probe.as_ref().map(|p| p.duration.as_millis() as u64),
        "observedAt": s.probe.as_ref().map(|p| rfc3339(p.at)),
        "ageMs": s.age(now).map(|a| a.as_millis() as u64),
        "stale": s.stale(now),
        // Plan `2026-09-07-health-database-reachable-is-a-connect-probe-not-
        // a-schema-probe`: does the SCHEMA the runner needs exist. `None`
        // means "could not determine" and is never read as `false`;
        // `schema_detail` explains every non-`Some(true)` state.
        "schema_ok": schema_ok,
        "schema_detail": schema_detail,
        "breaker": breaker,
    })
}

/// `HealthInputs.pg_reachable` for the sinks that do no PG I/O of their own
/// (the operations heartbeat, the web-backend heartbeat relay, the UI-Bridge
/// signal gatherer). Age-bounded: stale or never observed is `None`.
pub fn status_pg_reachable() -> Option<bool> {
    pg_health_snapshot().status_reachable(Instant::now())
}

/// `HealthInputs.pg_schema_ok` for the same sinks.
pub fn status_pg_schema_ok() -> Option<bool> {
    pg_health_snapshot().schema(Instant::now()).0
}

fn store_schema(schema_ok: Option<bool>, schema_detail: Option<String>) {
    *SCHEMA.lock().unwrap_or_else(|p| p.into_inner()) = Some(SchemaObservation {
        schema_ok,
        schema_detail,
        at_instant: Instant::now(),
    });
}

/// One refresh: while the breaker is Closed, a fresh `SELECT 1` through the
/// guarded pool (a connect failure trips the breaker) and, when it answered,
/// the schema probe. While it is Open the prober's trials are the
/// observations, so this does nothing.
async fn refresh_once() {
    let Some(pg) = super::PgDb::try_global() else {
        return;
    };
    if !pg.pool().is_available() {
        return;
    }
    let probe = pg.pool().probe_now().await;
    if probe.reachable == Some(true) {
        let schema = crate::mcp_api::pg_schema_probe().await;
        let (ok, detail) = crate::mcp_api::derive_schema_health(Some(true), Some(schema));
        store_schema(ok, detail);
    }
}

/// Start the refresher on the CURRENT runtime, once per process. Called from
/// `mcp_api::start_server`, whose runtime lives for the whole process; never
/// from the `pg-boot-rt` current-thread runtime.
pub fn spawn_health_refresher() {
    if REFRESHER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async {
        let mut tick = tokio::time::interval(REFRESH_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            refresh_once().await;
        }
    });
}

/// Test support: an observation `age` old.
#[cfg(test)]
fn obs(age: Duration, reachable: bool, now: Instant) -> ProbeObservation {
    ProbeObservation {
        at: SystemTime::now() - age,
        at_instant: now - age,
        duration: Duration::from_millis(3),
        reachable: Some(reachable),
        error: (!reachable).then(|| "refused".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::circuit::{Circuit, Outcome};
    use qontinui_runner_lib::pg_circuit::PG_CIRCUIT_CONFIG;

    fn breaker(open: bool) -> CircuitSnapshot {
        let c = Circuit::new(PG_CIRCUIT_CONFIG);
        if open {
            c.record(Outcome::TripFailure("error connecting to server".into()));
        }
        c.snapshot()
    }

    fn snap(probe: Option<ProbeObservation>, open: bool, class: CauseClass) -> PgHealthSnapshot {
        PgHealthSnapshot {
            configured: true,
            probe,
            schema: None,
            breaker: Some(breaker(open)),
            cause_class: class,
        }
    }

    #[test]
    fn open_on_a_refused_connect_reads_unreachable_fast() {
        let now = Instant::now();
        let s = snap(
            Some(obs(Duration::from_secs(2), false, now)),
            true,
            CauseClass::ConnectRefused,
        );
        let t = Instant::now();
        let j = database_health_json(&s, now);
        assert!(t.elapsed() < Duration::from_millis(5), "{:?}", t.elapsed());
        assert_eq!(j["reachable"], false);
        assert_eq!(j["breaker"]["state"], "open");
        assert_eq!(j["breaker"]["causeClass"], "connect_refused");
        assert_eq!(j["stale"], false);
        assert_eq!(s.status_reachable(now), Some(false));
        assert_eq!(j["schema_ok"], serde_json::Value::Null);
    }

    #[test]
    fn open_on_a_connect_timeout_is_unknown_not_unreachable() {
        let now = Instant::now();
        let s = snap(
            Some(obs(Duration::from_secs(2), false, now)),
            true,
            CauseClass::ConnectTimeout,
        );
        let j = database_health_json(&s, now);
        assert_eq!(j["reachable"], serde_json::Value::Null);
        assert_eq!(j["breaker"]["causeClass"], "connect_timeout");
        // The runner itself is still degraded: its callers are failing fast.
        assert_eq!(s.status_reachable(now), Some(false));
    }

    #[test]
    fn a_31_second_old_observation_is_stale_and_unknown() {
        let now = Instant::now();
        let s = snap(
            Some(obs(Duration::from_secs(31), true, now)),
            false,
            CauseClass::Unknown,
        );
        let j = database_health_json(&s, now);
        assert_eq!(j["reachable"], serde_json::Value::Null);
        assert_eq!(j["stale"], true);
        assert_eq!(s.status_reachable(now), None);
        // ...while a fresh one is the probe's verdict.
        let fresh = snap(
            Some(obs(Duration::from_secs(3), true, now)),
            false,
            CauseClass::Unknown,
        );
        assert_eq!(database_health_json(&fresh, now)["reachable"], true);
        assert_eq!(database_health_json(&fresh, now)["ageMs"], 3000);
    }

    #[test]
    fn a_never_observed_snapshot_is_unknown() {
        let now = Instant::now();
        let s = snap(None, false, CauseClass::Unknown);
        let j = database_health_json(&s, now);
        assert_eq!(j["reachable"], serde_json::Value::Null);
        assert_eq!(j["stale"], true);
        assert_eq!(j["probeMs"], serde_json::Value::Null);
    }

    #[test]
    fn no_pg_configured_is_unknown_as_before() {
        let now = Instant::now();
        let s = PgHealthSnapshot {
            configured: false,
            probe: None,
            schema: None,
            breaker: None,
            cause_class: CauseClass::Unknown,
        };
        let j = database_health_json(&s, now);
        assert_eq!(j["reachable"], serde_json::Value::Null);
        assert_eq!(j["stale"], false);
        assert_eq!(j["breaker"], serde_json::Value::Null);
        assert_eq!(s.status_reachable(now), None);
    }

    #[test]
    fn the_prober_cap_stays_below_the_staleness_bound() {
        assert!(qontinui_runner_lib::pg_circuit::PROBER_BACKOFF_CAP < STALE_AFTER);
        // Worst case between two Open observations: the cap plus one trial.
        assert!(
            qontinui_runner_lib::pg_circuit::PROBER_BACKOFF_CAP
                + qontinui_runner_lib::pg_circuit::DEFAULT_TRIAL_TIMEOUT
                < STALE_AFTER
        );
    }

    #[test]
    fn a_fresh_schema_observation_is_served_only_while_reachable() {
        let now = Instant::now();
        let mut s = snap(
            Some(obs(Duration::from_secs(1), true, now)),
            false,
            CauseClass::Unknown,
        );
        s.schema = Some(SchemaObservation {
            schema_ok: Some(false),
            schema_detail: Some("missing relation: project.task_runs".into()),
            at_instant: now,
        });
        assert_eq!(
            s.schema(now),
            (
                Some(false),
                Some("missing relation: project.task_runs".to_string())
            )
        );
        // Breaker open: the schema is not attempted, and never `false`.
        s.breaker = Some(breaker(true));
        s.cause_class = CauseClass::ConnectRefused;
        assert_eq!(s.schema(now).0, None);
    }

    /// `/health` does no PG I/O: its handler reads the cached snapshot and
    /// awaits neither probe (the same source-region technique as the other
    /// `async fn health(` tests in `mcp_api`).
    #[test]
    fn the_health_handler_reads_the_cached_snapshot_and_awaits_no_probe() {
        let region = crate::mcp_api::mcp_api_item_source("async fn health(")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(region.contains("pg_health_snapshot("), "{region}");
        assert!(region.contains("database_health_json("), "{region}");
        assert!(!region.contains("pg_liveness_probe().await"), "{region}");
        assert!(!region.contains("pg_schema_probe().await"), "{region}");
    }

    /// The three sinks that do no PG I/O of their own now read the snapshot,
    /// so a down database reaches the off-box heartbeat's `derived_status`.
    #[test]
    fn the_off_box_sinks_read_the_snapshot() {
        for (name, src) in [
            ("heartbeat.rs", include_str!("../../heartbeat.rs")),
            (
                "mcp/backend_relay.rs",
                include_str!("../../mcp/backend_relay.rs"),
            ),
            (
                "mcp/ui_bridge/request.rs",
                include_str!("../../mcp/ui_bridge/request.rs"),
            ),
        ] {
            assert!(
                src.contains("pg_reachable: crate::database::pg::health::status_pg_reachable()"),
                "{name} must feed pg_reachable from the cached snapshot"
            );
            assert!(
                src.contains("pg_schema_ok: crate::database::pg::health::status_pg_schema_ok()"),
                "{name} must feed pg_schema_ok from the cached snapshot"
            );
        }
    }
}
