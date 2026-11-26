//! `POST /control/shim-beacon`, entered where the identity shim enters it: the
//! route handler [`super::post_shim_beacon`], fed the literal JSON body the bash
//! identity shim's `curl` sends (plan
//! `2026-11-10-shim-settle-wait-and-in-memory-launch-liveness`, Phase 2).
//!
//! The bodies are spelled here rather than shared with the shim tests: this
//! file names nothing but the route handler, so it compiles against a runner
//! that predates the launch-failure record, where the WARN line is absent and
//! the first test goes RED rather than failing to build.

/// The bash identity shim's `resolve-failed` beacon, verbatim.
const RESOLVE_FAILED: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"resolve-failed","detail":"waited=1 stub=false settings=true user_session_id=true"}"#;

/// The bash identity shim's `invoked` beacon, verbatim.
const INVOKED: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"invoked","detail":"user_session_id=true settings=true mcp_config=false pinned=true"}"#;

const WARN_LINE: &str = "control/shim-beacon: identity shim could not launch claude";
const INFO_LINE: &str = "control/shim-beacon: identity shim invoked";

/// Run the route handler on `body` and return what it logged at INFO and up.
fn post(body: &str) -> String {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ((), logs) = crate::resource_guard::capture_logs(|| {
        runtime.block_on(async {
            let _response =
                super::post_shim_beacon(axum::Json(serde_json::from_str(body).unwrap())).await;
        });
    });
    logs
}

/// The lines of `logs` that carry `needle`.
fn lines_with<'a>(logs: &'a str, needle: &str) -> Vec<&'a str> {
    logs.lines().filter(|l| l.contains(needle)).collect()
}

/// A launch failure posted through the route is logged under its own WARN
/// line, once, naming its event — and not as an ordinary invocation.
#[test]
fn a_launch_failure_beacon_through_the_route_logs_its_warn_line() {
    let logs = post(RESOLVE_FAILED);
    let warns = lines_with(&logs, WARN_LINE);
    assert_eq!(warns.len(), 1, "exactly one WARN line:\n{logs}");
    assert!(warns[0].contains(" WARN "), "{}", warns[0]);
    assert!(warns[0].contains("event=resolve-failed"), "{}", warns[0]);
    assert!(lines_with(&logs, INFO_LINE).is_empty(), "{logs}");
}

/// An ordinary `invoked` beacon stays one INFO line and raises no WARN.
#[test]
fn an_invoked_beacon_through_the_route_stays_info() {
    let logs = post(INVOKED);
    let infos = lines_with(&logs, INFO_LINE);
    assert_eq!(infos.len(), 1, "exactly one INFO line:\n{logs}");
    assert!(infos[0].contains(" INFO "), "{}", infos[0]);
    assert!(infos[0].contains("event=invoked"), "{}", infos[0]);
    assert!(lines_with(&logs, WARN_LINE).is_empty(), "{logs}");
}
