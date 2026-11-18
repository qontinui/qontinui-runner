//! `publish_once` entered the way the sampler loop enters it
//! (`fleet::spawn_budget_republisher`), against a loopback coord, with every
//! prerequisite satisfied by [`crate::test_coord::coord_fixture`] rather than
//! bypassed. Each case asserts the returned [`PublishOutcome`] against literals
//! spelled out here — never a constant or a classifier's own output — so a
//! broken classification reds. Plan
//! `2026-11-15-fleet-publishers-loopback-coord-harness-and-detect-environment-race`
//! Phase 2.
//!
//! Log loudness (`warn_once`'s budget, `log_sample_ack`'s once-flag) is
//! process-global and deliberately not asserted, so test order cannot matter.

#[cfg(any(target_os = "linux", windows))]
use super::SampleAck;
use super::{publish_once, PublishOutcome, SkipReason};
#[cfg(any(target_os = "linux", windows))]
use crate::test_coord::a_port_nothing_listens_on;
use crate::test_coord::{coord_fixture, CoordFixture, ScriptedCoord};

/// The prefix of the resource-sample route on the double.
const ROUTE: &str = "/coord/devices/";

/// A started double answering the sample route from `script`, with the
/// fixture's `$COORD_HTTP_URL` pointed at it.
async fn coord_answering(fx: &CoordFixture, script: Vec<(u16, String)>) -> ScriptedCoord {
    let coord = ScriptedCoord::start(vec![(ROUTE, script)]).await;
    fx.connect(&coord.base);
    coord
}

/// (a) A 200 with coord's documented clean outcome is `Stored`, carrying
/// coord's `persisted` and the number of lanes actually sent — and the request
/// went to this device's route with the fixture's device JWT as its bearer.
///
/// Valid only where the host lane collects a sample (procfs on Linux, the OS on
/// Windows); elsewhere the outcome would be `Skipped(NoSamples)`, so the case
/// is compiled for those two only.
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn publish_once_hands_a_clean_2xx_body_to_sample_ack() {
    let fx = coord_fixture();
    let coord = coord_answering(
        &fx,
        vec![(
            200,
            r#"{"received":9,"accepted":9,"persisted":99,"rejected":[],"schema_pending":false,"unknown_fields":[]}"#
                .to_string(),
        )],
    )
    .await;

    let outcome = publish_once().await;

    assert_eq!(coord.hits(ROUTE), 1, "exactly one POST reached coord");
    let bodies = coord.bodies(ROUTE);
    let sent = bodies[0]["samples"]
        .as_array()
        .expect("the POST body carries a samples array")
        .len();
    assert!(sent >= 1, "the host lane produced a sample");
    assert_eq!(
        outcome,
        PublishOutcome::Acked(SampleAck::Stored {
            persisted: 99,
            sent
        })
    );
    assert_eq!(bodies[0]["device_id"], "fixture-device");
    assert_eq!(
        coord.paths(ROUTE),
        vec!["/coord/devices/fixture-device/resource-sample".to_string()]
    );
    assert_eq!(
        coord.authorizations(ROUTE),
        vec![Some(format!("Bearer {}", fx.bearer))]
    );
}

/// (b) A 200 whose body is not coord's documented outcome is UNKNOWN, never
/// "stored".
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn publish_once_reads_an_undocumented_2xx_body_as_unknown() {
    let fx = coord_fixture();
    let coord = coord_answering(&fx, vec![(200, "{}".to_string())]).await;

    assert_eq!(
        publish_once().await,
        PublishOutcome::Acked(SampleAck::Unknown)
    );
    assert_eq!(coord.hits(ROUTE), 1);
}

/// (c) A 200 naming a rejected item is a loss, carrying the problem line.
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn publish_once_reports_a_2xx_that_names_a_rejected_sample() {
    let fx = coord_fixture();
    let coord = coord_answering(
        &fx,
        vec![(
            200,
            r#"{"received":1,"accepted":1,"persisted":1,"rejected":["samples[0].lane: unknown lane"]}"#
                .to_string(),
        )],
    )
    .await;

    assert_eq!(
        publish_once().await,
        PublishOutcome::Acked(SampleAck::Problems(vec![
            "coord dropped invalid sample(s): samples[0].lane: unknown lane".to_string()
        ]))
    );
    assert_eq!(coord.hits(ROUTE), 1);
}

/// (d) A 503 is an HTTP failure of class `http:503`.
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn publish_once_classifies_a_503_as_an_http_failure() {
    let fx = coord_fixture();
    let coord = coord_answering(&fx, vec![(503, "{}".to_string())]).await;

    assert_eq!(
        publish_once().await,
        PublishOutcome::HttpFailure("http:503".to_string())
    );
    assert_eq!(coord.hits(ROUTE), 1);
}

/// (e) A coord base nobody listens on is a transport failure of class
/// `transport:connect`.
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn publish_once_classifies_a_refused_connect_as_a_transport_failure() {
    let fx = coord_fixture();
    let (_closed, port) = a_port_nothing_listens_on();
    fx.connect(&format!("http://127.0.0.1:{port}"));

    assert_eq!(
        publish_once().await,
        PublishOutcome::TransportFailure("transport:connect")
    );
}

/// (f) Without `machine.json` there is no device to publish for: the tick is
/// skipped before anything is sent.
#[tokio::test]
async fn publish_once_without_machine_json_skips_and_sends_nothing() {
    let fx = coord_fixture();
    std::fs::remove_file(fx.ambient.machine_json_path()).expect("remove the fixture machine.json");
    let coord = coord_answering(&fx, vec![(200, "{}".to_string())]).await;

    assert_eq!(
        publish_once().await,
        PublishOutcome::Skipped(SkipReason::NoDevice)
    );
    assert_eq!(coord.hits(ROUTE), 0, "a skipped tick sends nothing");
}
