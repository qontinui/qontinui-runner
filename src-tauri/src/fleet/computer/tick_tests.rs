//! `Reporter::tick` entered the way the sampler loop enters it
//! (`fleet::spawn_budget_republisher`), against a loopback coord, with every
//! publisher prerequisite satisfied by [`crate::test_coord::coord_fixture`]
//! and the host service scan fed from the test through
//! [`Reporter::scan_override`] — so the service-row bookkeeping
//! (`plan_report`'s delta filter, `on_delivered`'s `last_sent`) is exercised on
//! every box, not only on one that happens to host runner units. Plan
//! `2026-11-15-fleet-publishers-loopback-coord-harness-and-detect-environment-race`
//! Phase 3.
//!
//! `tick` returns before every coord prerequisite on a box with no OS machine
//! id, so each case first asserts one exists ([`host_id`]): such a box reds
//! loudly instead of passing vacuously. The cases are compiled for Linux and
//! Windows, where the machine id is read.

#![cfg(any(target_os = "linux", windows))]

use super::events::{self, ObserverState};
use super::services::{classify_unit, ServiceRow, ServiceScan, WatchedUnit};
use super::{identity, Reporter};
use crate::test_coord::{coord_fixture, CoordFixture, ScriptedCoord};

/// The computers-report route on the double.
const ROUTE: &str = "/coord/computers/report";

const UNIT_A: &str = "fixture-a.service";
const UNIT_B: &str = "fixture-b.service";

/// This host's identity hash. Asserted, because without it `tick` returns
/// before reaching any coord prerequisite and every case would pass vacuously.
fn host_id() -> String {
    identity::host_identity_hash().expect(
        "this box has no valid OS machine id (/etc/machine-id, or the Windows MachineGuid): \
         Reporter::tick returns before any coord prerequisite without one, so this test \
         cannot enter it",
    )
}

/// One watched unit in state `active`/`sub`, last changed at `changed_at`.
fn unit(name: &str, active: &str, sub: &str, changed_at: &str) -> WatchedUnit {
    WatchedUnit {
        row: ServiceRow {
            unit: name.to_string(),
            kind: classify_unit(name).to_string(),
            active_state: Some(active.to_string()),
            sub_state: Some(sub.to_string()),
            result: Some("success".to_string()),
            restart_policy: None,
            oom_policy: None,
            memory_max: None,
            memory_peak: None,
            n_restarts: Some(0),
            state_changed_at: Some(changed_at.to_string()),
            runner_name: None,
            repo: None,
        },
        cgroup_oom_kill: None,
    }
}

/// A scan holding `units` and completing no kind (a pure delta for removals).
fn scan(units: Vec<WatchedUnit>) -> ServiceScan {
    ServiceScan {
        units: Some(units),
        complete_kinds: Vec::new(),
        completable_kinds: &[],
    }
}

/// Both fixture units up.
fn both_running() -> ServiceScan {
    scan(vec![
        unit(UNIT_A, "active", "running", "2026-11-15T00:00:00Z"),
        unit(UNIT_B, "active", "running", "2026-11-15T00:00:00Z"),
    ])
}

/// coord's 200 outcome accepting the host with nothing to report.
fn accepting(host: &str) -> (u16, String) {
    (
        200,
        serde_json::json!({ "computers": [{ "identity_hash": host }] }).to_string(),
    )
}

/// A started double answering the report route from `script`, with the
/// fixture's `$COORD_HTTP_URL` pointed at it.
async fn coord_answering(fx: &CoordFixture, script: Vec<(u16, String)>) -> ScriptedCoord {
    let coord = ScriptedCoord::start(vec![(ROUTE, script)]).await;
    fx.connect(&coord.base);
    coord
}

/// The host's entry in the `n`th report the double received.
fn host_in(coord: &ScriptedCoord, n: usize, host: &str) -> serde_json::Value {
    let bodies = coord.bodies(ROUTE);
    bodies[n]["computers"]
        .as_array()
        .expect("a report carries a computers array")
        .iter()
        .find(|c| c["identity_hash"] == host)
        .cloned()
        .unwrap_or_else(|| panic!("report {n} carries no entry for the host"))
}

/// The unit names in a reported computer's `services`.
fn service_units(computer: &serde_json::Value) -> Vec<String> {
    computer["services"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r["unit"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The event kinds in a reported computer's `events`.
fn event_kinds(computer: &serde_json::Value) -> Vec<String> {
    computer["events"]
        .as_array()
        .map(|evs| {
            evs.iter()
                .filter_map(|e| e["kind"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The kinds of the host's still-queued events.
fn pending_kinds(reporter: &Reporter, host: &str) -> Vec<String> {
    reporter
        .state
        .as_ref()
        .and_then(|s| s.pending.get(host))
        .map(|q| q.iter().map(|e| e.kind.clone()).collect())
        .unwrap_or_default()
}

/// The unit names `last_sent` remembers coord holding for the host.
fn last_sent_units(reporter: &Reporter, host: &str) -> Vec<String> {
    reporter
        .last_sent
        .get(host)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// (a) A first tick against a coord that accepts everything: one POST
/// carrying this device and this host, with the fixture's bearer; `on_delivered`
/// records the delivered rows in `last_sent`; `persist` wrote the observer
/// state under the fixture's runner dir.
#[tokio::test]
async fn tick_delivers_a_full_report_and_records_what_coord_holds() {
    let fx = coord_fixture();
    let host = host_id();
    let coord = coord_answering(&fx, vec![accepting(&host)]).await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());

    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 1, "exactly one report reached coord");
    assert_eq!(coord.bodies(ROUTE)[0]["device_id"], "fixture-device");
    assert_eq!(
        service_units(&host_in(&coord, 0, &host)),
        vec![UNIT_A.to_string(), UNIT_B.to_string()]
    );
    assert_eq!(
        coord.authorizations(ROUTE),
        vec![Some(format!("Bearer {}", fx.bearer))]
    );
    assert_eq!(
        last_sent_units(&reporter, &host),
        vec![UNIT_A.to_string(), UNIT_B.to_string()],
        "on_delivered records the rows coord now holds"
    );
    assert!(
        fx.ambient
            .dir()
            .join("runner")
            .join("computer-observer.json")
            .is_file(),
        "the observer state is persisted under the fixture's runner dir"
    );
}

/// (b) A 200 whose body `parse_outcomes` cannot read: per `parse_outcomes`'
/// contract every computer counts as delivered (the pre-outcome behaviour), so
/// its rows are recorded in `last_sent` and its queue is drained.
#[tokio::test]
async fn tick_counts_every_computer_delivered_on_an_unparsed_200() {
    let fx = coord_fixture();
    let host = host_id();
    let coord = coord_answering(&fx, vec![(200, "not json".to_string())]).await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());

    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 1);
    assert_eq!(
        last_sent_units(&reporter, &host),
        vec![UNIT_A.to_string(), UNIT_B.to_string()]
    );
    assert_eq!(pending_kinds(&reporter, &host), Vec::<String>::new());
}

/// (c) After a delivered full report, the next tick is a delta: `plan_report`
/// sends only the row whose state changed, never the unchanged one.
#[tokio::test]
async fn tick_sends_only_changed_rows_after_a_delivered_report() {
    let fx = coord_fixture();
    let host = host_id();
    let coord = coord_answering(&fx, vec![accepting(&host)]).await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());
    reporter.tick().await;

    reporter.scan_override = Some(scan(vec![
        unit(UNIT_A, "active", "exited", "2026-11-15T00:00:00Z"),
        unit(UNIT_B, "active", "running", "2026-11-15T00:00:00Z"),
    ]));
    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 2);
    assert_eq!(
        service_units(&host_in(&coord, 0, &host)),
        vec![UNIT_A.to_string(), UNIT_B.to_string()],
        "the first report is full"
    );
    assert_eq!(
        service_units(&host_in(&coord, 1, &host)),
        vec![UNIT_A.to_string()],
        "the delta carries only the changed row"
    );
}

/// (d) A 503 backs the route off: the next immediate tick sends nothing.
#[tokio::test]
async fn tick_backs_off_after_a_503() {
    let fx = coord_fixture();
    let _host = host_id();
    let coord = coord_answering(&fx, vec![(503, "{}".to_string())]).await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());

    reporter.tick().await;
    assert!(reporter.route_absent_until.is_some(), "a 503 backs off");
    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 1, "the backed-off tick sent no report");
}

/// (e) A 422 whose problems name no event puts the reporter on stripped
/// reports (`on_rejected` step 3).
#[tokio::test]
async fn tick_strips_reports_after_a_422_naming_no_event() {
    let fx = coord_fixture();
    let _host = host_id();
    let coord = coord_answering(
        &fx,
        vec![(
            422,
            r#"{"problems":["computers[0].hostname: must be at most 255 characters"]}"#.to_string(),
        )],
    )
    .await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());

    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 1);
    assert!(
        reporter.strip_until.is_some(),
        "a 422 naming no event strips"
    );
}

/// (f) The `telemetry_gap` that `tick` itself queues (a previous tick a day
/// ago) survives a failed POST and is drained by the next accepted one.
#[tokio::test]
async fn tick_keeps_a_telemetry_gap_queued_until_coord_accepts_it() {
    let fx = coord_fixture();
    let host = host_id();
    let state_path = fx
        .ambient
        .dir()
        .join("runner")
        .join("computer-observer.json");
    let seeded = ObserverState {
        last_tick_at: Some(events::rfc3339(
            chrono::Utc::now() - chrono::Duration::days(1),
        )),
        ..ObserverState::default()
    };
    events::save(&state_path, &seeded).expect("seed the observer state");
    let coord = coord_answering(&fx, vec![(500, "{}".to_string()), accepting(&host)]).await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());

    reporter.tick().await;
    assert_eq!(coord.hits(ROUTE), 1);
    assert_eq!(
        pending_kinds(&reporter, &host),
        vec!["telemetry_gap".to_string()],
        "the gap stays queued after a 500"
    );

    // A 500 backs the route off; lift it so the next tick posts.
    reporter.route_absent_until = None;
    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 2);
    assert_eq!(
        event_kinds(&host_in(&coord, 1, &host)),
        vec!["telemetry_gap".to_string()]
    );
    assert_eq!(
        pending_kinds(&reporter, &host),
        Vec::<String>::new(),
        "the accepted report drains the queue"
    );
}

/// (g) A unit moving from active to failed between ticks: `derive_events`
/// queues a `service_failed` event, which survives a failed POST and is
/// drained by `on_delivered` once coord accepts it.
#[tokio::test]
async fn tick_queues_a_service_failure_until_coord_accepts_it() {
    let fx = coord_fixture();
    let host = host_id();
    let coord = coord_answering(
        &fx,
        vec![accepting(&host), (500, "{}".to_string()), accepting(&host)],
    )
    .await;
    let mut reporter = Reporter::new();
    reporter.scan_override = Some(both_running());
    reporter.tick().await;
    assert_eq!(pending_kinds(&reporter, &host), Vec::<String>::new());

    reporter.scan_override = Some(scan(vec![
        unit(UNIT_A, "failed", "failed", "2026-11-15T00:05:00Z"),
        unit(UNIT_B, "active", "running", "2026-11-15T00:00:00Z"),
    ]));
    reporter.tick().await;
    assert_eq!(coord.hits(ROUTE), 2);
    assert_eq!(
        pending_kinds(&reporter, &host),
        vec!["service_failed".to_string()],
        "derive_events queued the failure, and the 500 left it queued"
    );

    reporter.route_absent_until = None;
    reporter.tick().await;

    assert_eq!(coord.hits(ROUTE), 3);
    assert_eq!(
        event_kinds(&host_in(&coord, 2, &host)),
        vec!["service_failed".to_string()]
    );
    assert_eq!(
        pending_kinds(&reporter, &host),
        Vec::<String>::new(),
        "the accepted report drains the queue"
    );
}
