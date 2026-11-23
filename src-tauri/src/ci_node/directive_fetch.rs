//! Fetch this device's stored CI-node settings directive from coord on connect
//! (plan `2026-11-22-ci-node-directive-replays-on-connect`, Phase 3).
//!
//! # Why
//!
//! Coord publishes a settings directive on `events.ci.settings_requested.<device>`
//! exactly once. A device that was offline at that moment had no subscriber,
//! the publish still "succeeded", and the directive was lost — the owner
//! enabled a box's CI lane from the web and the box never heard. Coord now
//! also STORES the latest accepted directive per device and serves it at
//! `GET /devenv/agent-ci-node-directive` (the device is the token's own claim,
//! never a parameter). This module reads it with the device bearer and hands
//! it to [`super::settings_directive::apply_fetched`], the same gate and the
//! same compare-and-write the live frame goes through. Newest-wins and the
//! id-equality dedupe live there, so fetching more than once is free.
//!
//! # When ([`super::subscription`])
//!
//! A fetch timed off the 101 upgrade can read the store before a directive is
//! stored while that directive's publish lands before coord's PSUBSCRIBE —
//! lost again. So the fetch that closes the race is the one spawned on coord's
//! `{"channel":"_control","type":"subscribed"}` frame, sent after its
//! PSUBSCRIBE: anything published after it arrives on the socket. Against a
//! coord that never sends that frame, the pump fetches at connect and again
//! [`LEGACY_REFETCH_AFTER`] later ([`ReplaySchedule`]).
//!
//! # Failure is UNKNOWN, never a value
//!
//! `{"directive": null}` (nothing ever stored), 404 (a coord that predates the
//! route), 401/403, 5xx, an unparseable body and a transport error each log one
//! line naming the outcome and leave `settings.json` byte-identical. Nothing
//! here resolves an unknown to `enabled = false` or `true`; the next connect
//! retries.

use std::time::Duration;

use tracing::{debug, info, warn};

use super::settings_directive::{self, ApplyOutcome, CiSettingsDirective};

/// Coord's read door for this device's stored directive.
pub(crate) const DIRECTIVE_ROUTE: &str = "/devenv/agent-ci-node-directive";

/// Bound on one fetch. It runs off the pump (spawned), so this bounds only how
/// long a stuck coord holds a task, never the socket.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Against a coord that sends no `subscribed` control frame, the second fetch
/// runs this long after connect — late enough that coord's PSUBSCRIBE (which
/// follows the upgrade by milliseconds) has almost surely landed.
pub(crate) const LEGACY_REFETCH_AFTER: Duration = Duration::from_secs(5);

/// What one fetch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchOutcome {
    /// Coord served a directive; this is what the gate did with it.
    Received(ApplyOutcome),
    /// `200 {"directive": null}` — nothing was ever stored for this device.
    NoDirective,
    /// 404 — a coord that predates the directive store.
    NotServed,
    /// 401 / 403 — the device bearer was not accepted.
    Unauthorized(u16),
    /// Any other non-2xx (5xx included).
    HttpError(u16),
    /// A 2xx whose body is not `{"directive": <payload>|null}`.
    BadBody(String),
    /// The request never got an answer.
    Transport(String),
    /// No device JWT could be resolved.
    NoBearer,
    /// The runner is isolated — no coord to ask.
    NoCoord,
}

/// PURE: classify one response. `Ok(directive)` when coord served one to apply;
/// otherwise the terminal outcome. A missing `directive` key is a bad body, not
/// a null: an absent answer is not "nothing stored".
pub(crate) fn interpret_response(
    status: u16,
    body: &str,
) -> Result<CiSettingsDirective, FetchOutcome> {
    match status {
        200..=299 => {}
        404 => return Err(FetchOutcome::NotServed),
        401 | 403 => return Err(FetchOutcome::Unauthorized(status)),
        other => return Err(FetchOutcome::HttpError(other)),
    }
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| FetchOutcome::BadBody(format!("not JSON: {e}")))?;
    match value.get("directive") {
        None => Err(FetchOutcome::BadBody("no `directive` key".to_string())),
        Some(serde_json::Value::Null) => Err(FetchOutcome::NoDirective),
        Some(payload) => serde_json::from_value::<CiSettingsDirective>(payload.clone())
            .map_err(|e| FetchOutcome::BadBody(format!("directive did not parse: {e}"))),
    }
}

/// When the pump should fetch. Fetch at connect, on coord's `subscribed` frame,
/// and — only if that frame never came — once more at [`LEGACY_REFETCH_AFTER`].
/// Every method answers "fetch now?".
#[derive(Debug, Default)]
pub(crate) struct ReplaySchedule {
    subscribed_seen: bool,
}

impl ReplaySchedule {
    /// The socket is up. Always fetch: on a coord with no `subscribed` frame
    /// this is the first of the two legacy fetches, and on a newer one it is
    /// merely redundant (the dedupe makes it free).
    pub(crate) fn on_connect(&mut self) -> bool {
        true
    }

    /// Coord's PSUBSCRIBE is live. The fetch that closes the race.
    pub(crate) fn on_subscribed(&mut self) -> bool {
        let first = !self.subscribed_seen;
        self.subscribed_seen = true;
        first
    }

    /// [`LEGACY_REFETCH_AFTER`] has passed since connect. Fetch again only when
    /// coord never confirmed its subscription.
    pub(crate) fn on_legacy_deadline(&mut self) -> bool {
        !self.subscribed_seen
    }
}

/// Spawn one fetch off the pump. The pump must never wait on HTTP.
pub(crate) fn spawn_fetch(device_id: uuid::Uuid, trigger: &'static str) {
    tokio::spawn(async move {
        let outcome = fetch_and_apply(device_id).await;
        log_outcome(device_id, trigger, &outcome);
    });
}

/// Resolve coord and the device bearer, then fetch and apply.
pub(crate) async fn fetch_and_apply(_device_id: uuid::Uuid) -> FetchOutcome {
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return FetchOutcome::NoCoord;
    };
    let Some(bearer) = super::reporting::device_bearer().await else {
        return FetchOutcome::NoBearer;
    };
    fetch_and_apply_from(&base, &bearer).await
}

/// Fetch from `base` with `bearer` and apply. The seam the tests drive.
pub(crate) async fn fetch_and_apply_from(base: &str, bearer: &str) -> FetchOutcome {
    let url = format!("{}{DIRECTIVE_ROUTE}", base.trim_end_matches('/'));
    let client = match reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return FetchOutcome::Transport(format!("client build: {e}")),
    };
    let (status, body) = match client.get(&url).bearer_auth(bearer).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.text().await {
                Ok(body) => (status, body),
                Err(e) => return FetchOutcome::Transport(format!("body read: {e}")),
            }
        }
        Err(e) => return FetchOutcome::Transport(e.to_string()),
    };
    match interpret_response(status, &body) {
        Ok(directive) => FetchOutcome::Received(settings_directive::apply_fetched(directive)),
        Err(outcome) => outcome,
    }
}

/// One line per fetch. A received directive was already logged by the gate
/// with its `directive_id` and `source=fetched`; this names the fetch itself.
fn log_outcome(device_id: uuid::Uuid, trigger: &str, outcome: &FetchOutcome) {
    match outcome {
        FetchOutcome::Received(applied) => {
            debug!("ci_node: directive fetch ({trigger}) for device_id={device_id}: {applied:?}")
        }
        FetchOutcome::NoDirective => info!(
            "ci_node: directive fetch ({trigger}): coord holds no directive for \
             device_id={device_id}; settings untouched"
        ),
        FetchOutcome::NotServed => info!(
            "ci_node: directive fetch ({trigger}): coord answered 404 (it predates the \
             directive store); settings untouched"
        ),
        FetchOutcome::NoCoord => {
            debug!("ci_node: directive fetch ({trigger}): runner is isolated; settings untouched")
        }
        other => warn!(
            "ci_node: directive fetch ({trigger}) FAILED for device_id={device_id}: {other:?}; \
             settings untouched, retried on the next connect"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CiNodeSettings;
    use std::sync::{Arc, Mutex};

    /// A coord serving `GET /devenv/agent-ci-node-directive` with whatever
    /// `(status, body)` the test sets, recording the bearer it was shown.
    struct FakeCoord {
        base: String,
        answer: Arc<Mutex<(u16, String)>>,
        bearers: Arc<Mutex<Vec<String>>>,
    }

    impl FakeCoord {
        async fn start(status: u16, body: &str) -> Self {
            let answer = Arc::new(Mutex::new((status, body.to_string())));
            let bearers = Arc::new(Mutex::new(Vec::new()));
            let (a, b) = (answer.clone(), bearers.clone());
            let app = axum::Router::new().route(
                DIRECTIVE_ROUTE,
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let (a, b) = (a.clone(), b.clone());
                    async move {
                        if let Some(h) = headers.get("authorization") {
                            b.lock().unwrap().push(h.to_str().unwrap_or("").to_string());
                        }
                        let (status, body) = a.lock().unwrap().clone();
                        (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            [("content-type", "application/json")],
                            body,
                        )
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            Self {
                base: format!("http://{addr}"),
                answer,
                bearers,
            }
        }

        fn set(&self, status: u16, body: &str) {
            *self.answer.lock().unwrap() = (status, body.to_string());
        }
    }

    fn stored(id: uuid::Uuid, at: &str, enabled: bool) -> String {
        serde_json::json!({
            "directive": {
                "enabled": enabled,
                "max_concurrent_builds": 2,
                "repo_allowlist": ["qontinui/qontinui-runner"],
                "min_free_disk_gb": 20,
                "machine_id": "m-1",
                "requested_at": at,
                "directive_id": id.to_string(),
            }
        })
        .to_string()
    }

    /// A settings.json carrying a known, non-default `ci_node` section, so
    /// "untouched" is a real comparison and not "still the default".
    const SEEDED: &str =
        r#"{"ci_node":{"enabled":false,"repo_allowlist":["seed/repo"],"min_free_disk_gb":33}}"#;

    /// (a) THE incident: nothing ever applied, `enabled = true` was stored
    /// while the box was offline, and the connect fetch turns the lane on.
    #[tokio::test]
    async fn a_fetched_directive_applies_when_none_was_ever_applied() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        let id = uuid::Uuid::now_v7();
        let coord = FakeCoord::start(200, &stored(id, "2026-11-22T10:00:00.123456Z", true)).await;

        let outcome = fetch_and_apply_from(&coord.base, "dev-jwt").await;

        assert_eq!(outcome, FetchOutcome::Received(ApplyOutcome::Applied));
        let ci = crate::settings::get_ci_node_settings();
        assert!(ci.enabled, "the offline-sent enable must reach the box");
        assert_eq!(ci.repo_allowlist, vec!["qontinui/qontinui-runner"]);
        assert_eq!(ci.last_directive_id, Some(id));
        assert_eq!(
            coord.bearers.lock().unwrap().as_slice(),
            ["Bearer dev-jwt"],
            "the fetch must present the device bearer"
        );
    }

    /// (g) Every way a fetch can fail leaves settings.json BYTE-identical —
    /// no outcome resolves an unknown to a value.
    #[tokio::test]
    async fn every_failed_fetch_leaves_settings_byte_identical() {
        let amb = crate::test_env::isolated_ambient();
        let path = amb.write_settings_json(SEEDED);
        let before = std::fs::read(&path).unwrap();
        let coord = FakeCoord::start(200, "").await;

        let cases: [(u16, &str, FetchOutcome); 7] = [
            (200, r#"{"directive":null}"#, FetchOutcome::NoDirective),
            (404, r#"{"error":"not found"}"#, FetchOutcome::NotServed),
            (
                401,
                r#"{"error":"unauthorized"}"#,
                FetchOutcome::Unauthorized(401),
            ),
            (403, "", FetchOutcome::Unauthorized(403)),
            (500, "boom", FetchOutcome::HttpError(500)),
            (
                503,
                r#"{"error":"schema_migration_pending"}"#,
                FetchOutcome::HttpError(503),
            ),
            (
                200,
                "{}",
                FetchOutcome::BadBody("no `directive` key".to_string()),
            ),
        ];
        for (status, body, expected) in cases {
            coord.set(status, body);
            let got = fetch_and_apply_from(&coord.base, "dev-jwt").await;
            assert_eq!(got, expected, "status {status} body {body:?}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "status {status} must leave settings.json untouched"
            );
        }

        // Transport: nothing listens on a port we bound and released.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let got = fetch_and_apply_from(&dead, "dev-jwt").await;
        assert!(matches!(got, FetchOutcome::Transport(_)), "{got:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            crate::settings::get_ci_node_settings(),
            CiNodeSettings {
                repo_allowlist: vec!["seed/repo".to_string()],
                min_free_disk_gb: 33,
                ..CiNodeSettings::default()
            }
        );
    }

    /// The response classifier, purely: null is NoDirective, a missing key is
    /// NOT (an absent answer is not "nothing stored"), and the status arms.
    #[test]
    fn interpret_response_arms() {
        assert_eq!(
            interpret_response(200, r#"{"directive":null}"#).unwrap_err(),
            FetchOutcome::NoDirective
        );
        assert!(matches!(
            interpret_response(200, "{}").unwrap_err(),
            FetchOutcome::BadBody(_)
        ));
        assert!(matches!(
            interpret_response(200, "nope").unwrap_err(),
            FetchOutcome::BadBody(_)
        ));
        assert!(matches!(
            interpret_response(200, r#"{"directive":"x"}"#).unwrap_err(),
            FetchOutcome::BadBody(_)
        ));
        assert_eq!(
            interpret_response(404, "").unwrap_err(),
            FetchOutcome::NotServed
        );
        assert_eq!(
            interpret_response(502, "").unwrap_err(),
            FetchOutcome::HttpError(502)
        );
        let id = uuid::Uuid::now_v7();
        let d = interpret_response(200, &stored(id, "2026-11-22T10:00:00Z", true)).unwrap();
        assert_eq!(d.directive_id, Some(id));
        assert!(d.enabled);
    }

    /// (i) A directive stored AFTER the connect fetch, whose live frame was
    /// dropped (published before coord's PSUBSCRIBE), is applied by the fetch
    /// the `subscribed` frame triggers.
    #[tokio::test]
    async fn the_post_subscribed_fetch_applies_a_directive_the_connect_fetch_missed() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        let coord = FakeCoord::start(200, r#"{"directive":null}"#).await;
        let mut schedule = ReplaySchedule::default();

        // Connect: coord has nothing stored yet.
        assert!(schedule.on_connect());
        assert_eq!(
            fetch_and_apply_from(&coord.base, "j").await,
            FetchOutcome::NoDirective
        );
        // The directive is stored now; its publish lands before PSUBSCRIBE, so
        // no live frame ever arrives.
        let id = uuid::Uuid::now_v7();
        coord.set(200, &stored(id, "2026-11-22T10:00:01Z", true));

        // Coord confirms its subscription: the schedule must fetch, and that
        // fetch applies the directive.
        assert!(
            schedule.on_subscribed(),
            "the subscribed frame must trigger a fetch"
        );
        assert_eq!(
            fetch_and_apply_from(&coord.base, "j").await,
            FetchOutcome::Received(ApplyOutcome::Applied)
        );
        let ci = crate::settings::get_ci_node_settings();
        assert!(ci.enabled);
        assert_eq!(ci.last_directive_id, Some(id));
        // A coord that confirmed its subscription needs no legacy re-fetch.
        assert!(!schedule.on_legacy_deadline());
    }

    /// An isolated runner (no coord resolves) asks nobody and writes nothing.
    #[tokio::test]
    async fn an_isolated_runner_fetches_nothing() {
        let amb = crate::test_env::isolated_ambient();
        let path = amb.write_settings_json(r#"{"tier":"local","ci_node":{"enabled":false}}"#);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            fetch_and_apply(uuid::Uuid::now_v7()).await,
            FetchOutcome::NoCoord
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// The legacy arm: a coord that never sends `subscribed` gets the connect
    /// fetch and the re-fetch at the deadline.
    #[test]
    fn a_coord_without_the_subscribed_frame_gets_the_legacy_refetch() {
        let mut schedule = ReplaySchedule::default();
        assert!(schedule.on_connect());
        assert!(
            schedule.on_legacy_deadline(),
            "no subscribed frame: fetch again at the deadline"
        );
    }
}
