//! The one door a RUNNER REPORT takes to `coord.device_status.details`.
//!
//! Plan `2026-11-27-devops-runner-database-degraded-is-invisible-to-coord`
//! Phase 1 (D2). Coord reads several top-level keys of a device's status bag
//! as runner reports and turns them into alerts (`runner_reports.rs`,
//! `RUNNER_REPORT_KEYS`). Every publisher of such a key needs the same six
//! decisions, and this module makes them once:
//!
//! 1. **Primary only.** Every runner on a box shares one `device_id`, so a temp
//!    or secondary runner that posted would write ITS view over the primary's
//!    row — a database alert raised on the primary's device by a throwaway
//!    runner, or a real one cleared. [`post_runner_report`] consults
//!    [`super::machine_state_publish_allowed`] at the point the report leaves
//!    the process, the rule that function's doc states for every
//!    machine-scoped writer.
//! 2. **A standalone runner posts nothing.** The base comes from
//!    `profiles::connected_coord_base`, where `None` is a no-op. The
//!    String-family resolver (`coord_mcp::coord_base_url_with_source`) always
//!    answers with a URL and is never the door for a feature that must no-op
//!    when standalone (`profiles.rs`, `connected_coord_base`'s doc).
//! 3. **The paired device bearer.** `POST /coord/status` is unauthenticated
//!    and coord writes a runner-report key only on a PAIRED device token for
//!    the body's device, except the anonymous `coord_credential` `ok:false`
//!    carve-out (rule 6; `status_report_auth.rs`). The bearer and the body's
//!    `tenant_id` come from ONE [`crate::auth::TenantScope`], so they cannot
//!    disagree.
//! 4. **No `null` ever leaves.** Under coord's per-key merge a `null` REMOVES
//!    the key, and a paired post keeps null removals — a null would delete the
//!    stored report. A publisher with nothing to say omits its key; a runner
//!    that could not look sends the key's `*_error` sibling, which coord ranks
//!    by receipt time against the key (`runner_reports::effective`). So
//!    [`status_body`] refuses a null-valued key outright.
//! 5. **Best-effort, bounded.** One attempt with a 5 s timeout through the OS
//!    resolver (`outbound_net::os_dns`). The caller's next tick is the retry.
//! 6. **A 200 is not a write.** Coord answers `200` to a post that did not
//!    prove this device (no bearer, an expired one, or a token that is not
//!    this device's paired one) and strips from it the keys this module
//!    sends, `runner_database` and `runner_database_error`, once coord lists
//!    them in `RUNNER_REPORT_KEYS` (plan Phase 2; until then they are
//!    unlisted and stored as posted). Not every runner-report key is
//!    stripped: coord keeps an unproven `coord_credential` whose value can
//!    only raise its alert (`ok: false`, the anonymous carve-out in coord's
//!    `status_report_auth.rs`), except on a token for a DIFFERENT device
//!    (`Mismatch`), which strips it too. So [`reply_outcome`] reads the reply's
//!    `details` and calls the post
//!    [`ReportPost::Sent`] only when every key it sent is there. Anything else
//!    is an `Err`, which the caller's failure streak logs once.
//!
//! ## Who must use it
//!
//! `fleet::database_report` (the `runner_database` key) is the first caller.
//! The `wedge_incidents` and `capability` publishers that coord's
//! `runner_reports.rs` names (`fleet/wedge_report.rs`, plan
//! `2026-09-20-the-second-ratchet-domain-is-operations-and-its-cost-is-compared-to-the-first`
//! Phase 5) MUST post through [`post_runner_report`] too, not a copy of it: a
//! second copy is how a publisher loses the primary guard. The
//! `coord_credential` publisher (`mcp/device_jwt_refresher.rs`
//! `publish_coord_credential_status`) predates this module and posts without
//! the primary guard; moving it here is that publisher's own follow-up, since
//! it changes what a secondary reports.

use std::time::Duration;

use serde_json::{Map, Value};
use tracing::debug;

/// One attempt's bound: 5 s, as in `fleet::heartbeat_to_coord`,
/// `fleet::publish_tree_state` and the `coord_credential` publisher
/// (`mcp/device_jwt_refresher.rs`). The budget publisher waits 10 s.
pub(crate) const POST_TIMEOUT: Duration = Duration::from_secs(5);

/// What one [`post_runner_report`] call did, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportPost {
    /// Coord answered 2xx, and its reply shows every key this post sent.
    Sent,
    /// Nothing was sent, by design, for the stated reason.
    NotSent(&'static str),
}

/// Assemble the `POST /coord/status` body for `details`.
///
/// Pure, so the wire contract is tested without a network. Refuses an empty
/// bag (a post that carries nothing is a bug in the caller) and any key whose
/// value is JSON `null` (see the module doc, rule 4).
pub(crate) fn status_body(
    device_id: uuid::Uuid,
    declared_tenant: Option<uuid::Uuid>,
    details: Map<String, Value>,
) -> Result<Value, String> {
    if details.is_empty() {
        return Err("a runner report must carry at least one key".to_string());
    }
    if let Some((key, _)) = details.iter().find(|(_, v)| v.is_null()) {
        return Err(format!(
            "refusing to send `{key}: null` — coord's per-key merge would DELETE the stored \
             report; omit the key, or send its `*_error` sibling"
        ));
    }
    let mut body = serde_json::json!({
        "device_id": device_id,
        "details": Value::Object(details),
    });
    if let Some(t) = declared_tenant {
        body["tenant_id"] = serde_json::json!(t);
    }
    Ok(body)
}

/// Post one runner report (`details`, top-level keys only) for this device.
///
/// `Ok(NotSent(..))` when this instance may not publish machine state, the
/// device file is unreadable, or no coord is connected. `Err` for a refused
/// body, a transport failure, a non-2xx answer, or a 2xx whose reply does not
/// show every key sent ([`reply_outcome`]), worded for a log line.
pub(crate) async fn post_runner_report(details: Map<String, Value>) -> Result<ReportPost, String> {
    // Rule 1, at the point the report leaves the process.
    if !super::machine_state_publish_allowed(crate::instance::owns_shared_root_state()) {
        return Ok(ReportPost::NotSent(
            "a secondary instance publishes no runner report",
        ));
    }
    let Some(device) = super::load_device_file() else {
        return Ok(ReportPost::NotSent(
            "~/.qontinui/machine.json is missing, unreadable, or names no device_id/hostname",
        ));
    };
    let device_id = uuid::Uuid::parse_str(device.device_id.trim())
        .map_err(|e| format!("machine.json device_id is not a UUID ({e})"))?;
    // Rule 2.
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        return Ok(ReportPost::NotSent("no coord is connected"));
    };

    // Rule 3: ONE scope drives both the declared tenant and the bearer. A
    // default binding this device holds no credential for is `Unresolved` and
    // declares nothing, rather than claiming a tenant the bearer cannot back.
    let scope = crate::auth::TenantScope::for_bound_device_default(
        super::resolve_tenant_id(),
        &crate::auth::device_holds_usable_binding,
    );
    let sent_keys: Vec<String> = details.keys().cloned().collect();
    let body = status_body(device_id, scope.declared_tenant(), details)?;

    let client = qontinui_runner_lib::outbound_net::os_dns::client_builder()
        .timeout(POST_TIMEOUT)
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    let url = format!("{base}/coord/status");
    let resp = crate::auth::attach_device_auth_for(client.post(&url).json(&body), scope)
        .send()
        .await
        .map_err(|e| format!("POST {url} failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let outcome = reply_outcome(&url, status, &text, &sent_keys);
    if outcome.is_ok() {
        debug!("fleet::status_report: POST {url} -> {status}, wrote {sent_keys:?}");
    }
    outcome
}

/// Read coord's answer to one post (rule 6). Pure, so both reply shapes are
/// tested without a network.
///
/// Coord's 200 body carries `details` in both shapes it answers with
/// (`status.rs` `response_body`): the stored row for a post that proved the
/// body's device, or only what this post wrote (`response_scope: "written"`)
/// for any other. Either way, a sent key absent from it is a key coord did
/// not write. `response_scope` alone is not the test: a coord that does not
/// list a key yet answers an unproven post `"written"` and still stores it.
pub(crate) fn reply_outcome(
    url: &str,
    status: reqwest::StatusCode,
    body: &str,
    sent_keys: &[String],
) -> Result<ReportPost, String> {
    if !status.is_success() {
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("POST {url} -> HTTP {status}: {excerpt}"));
    }
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let Some(written) = parsed
        .as_ref()
        .and_then(|v| v.get("details"))
        .and_then(Value::as_object)
    else {
        return Err(format!(
            "POST {url} -> HTTP {status}, but the reply carries no `details` object, so \
             whether coord wrote the report is UNKNOWN"
        ));
    };
    let missing: Vec<&str> = sent_keys
        .iter()
        .map(String::as_str)
        .filter(|k| !written.contains_key(*k))
        .collect();
    if missing.is_empty() {
        return Ok(ReportPost::Sent);
    }
    Err(format!(
        "coord answered {status} but did not write {}: the post did not prove this device",
        missing.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id() -> uuid::Uuid {
        uuid::Uuid::parse_str("bcf4751a-cd29-4036-9b1f-31fdf4e057ea").unwrap()
    }

    fn details(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("an object")
    }

    #[test]
    fn the_body_carries_the_device_the_details_and_a_declared_tenant() {
        let t = uuid::Uuid::parse_str("c231d9da-0ca8-4fe4-bd81-0e3d6c20339a").unwrap();
        let body = status_body(
            id(),
            Some(t),
            details(json!({"runner_database": {"arm": "x"}})),
        )
        .expect("a body");
        assert_eq!(body["device_id"], json!(id()));
        assert_eq!(body["tenant_id"], json!(t));
        assert_eq!(body["details"]["runner_database"]["arm"], "x");
    }

    #[test]
    fn an_undeclared_tenant_is_omitted_not_null() {
        let body = status_body(id(), None, details(json!({"runner_database": {}}))).unwrap();
        assert!(
            body.get("tenant_id").is_none(),
            "no tenant_id key at all: {body}"
        );
    }

    #[test]
    fn a_null_valued_key_is_refused_because_coord_would_delete_the_report() {
        let err = status_body(id(), None, details(json!({"runner_database": null})))
            .expect_err("a null must never reach the wire");
        assert!(err.contains("runner_database"), "{err}");
        // Mixed with a real key, still refused: one null is one deletion.
        assert!(status_body(
            id(),
            None,
            details(json!({"runner_database_error": "x", "runner_database": null}))
        )
        .is_err());
    }

    #[test]
    fn an_empty_report_is_refused() {
        assert!(status_body(id(), None, Map::new()).is_err());
    }

    const URL: &str = "https://coord.example/coord/status";

    fn sent() -> Vec<String> {
        vec!["runner_database".to_string()]
    }

    /// An unproven post: coord answers 200 with only what it wrote, and the
    /// runner-report key was stripped. That is an `Err`, not `Sent`.
    #[test]
    fn a_200_that_stripped_the_report_is_an_error_naming_the_key() {
        let body = json!({
            "device_id": id(),
            "details": {},
            "response_scope": "written",
        })
        .to_string();
        let err = reply_outcome(URL, reqwest::StatusCode::OK, &body, &sent())
            .expect_err("a stripped report was not written");
        assert!(err.contains("did not write runner_database"), "{err}");
        assert!(err.contains("did not prove this device"), "{err}");
    }

    /// Both kept shapes are `Sent`: the stored row a proven post gets back,
    /// and the `written` receipt of a coord that stores the key unlisted.
    #[test]
    fn a_200_that_kept_the_report_is_sent() {
        let stored_row = json!({
            "device_id": id(),
            "details": {
                "runner_database": {"arm": "degraded"},
                "coord_credential": {"ok": true},
                "_coord_received_at": {"runner_database": "2026-11-27T03:37:28Z"},
            },
        })
        .to_string();
        assert_eq!(
            reply_outcome(URL, reqwest::StatusCode::OK, &stored_row, &sent()),
            Ok(ReportPost::Sent)
        );
        let written = json!({
            "device_id": id(),
            "details": {"runner_database": {"arm": "degraded"}},
            "response_scope": "written",
        })
        .to_string();
        assert_eq!(
            reply_outcome(URL, reqwest::StatusCode::OK, &written, &sent()),
            Ok(ReportPost::Sent)
        );
    }

    /// A 2xx whose body is not a status row says nothing about the write: an
    /// `Err` reading UNKNOWN, never `Sent`. A non-2xx keeps its status.
    #[test]
    fn an_unreadable_reply_is_unknown_and_a_non_2xx_is_an_error() {
        for body in ["", "not json", "{}", r#"{"details": "x"}"#] {
            let err = reply_outcome(URL, reqwest::StatusCode::OK, body, &sent())
                .expect_err("no details object");
            assert!(err.contains("UNKNOWN"), "{body:?}: {err}");
        }
        let err = reply_outcome(
            URL,
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"device_id is not registered"}"#,
            &sent(),
        )
        .expect_err("a 400");
        assert!(err.contains("HTTP 400"), "{err}");
    }
}
