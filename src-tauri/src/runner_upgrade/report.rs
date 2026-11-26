//! Reporting an upgrade's outcome to coord (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3 step 6; served `production-and-cost` `runner-self-upgrade`: "A rollback,
//! an unverified resume or a deferral past three windows is an immediate
//! operator notification … never a silent residue").
//!
//! Two writes per escalating outcome, both device-authed, both read back by
//! status:
//!
//! - a **finding** (`POST /coord/agent-findings`, the door
//!   `coord_outside_observer` posts through) — the durable record a later
//!   session can find; and
//! - an **operator notification** (`POST /coord/agent-notifications`, the HTTP
//!   twin of `coord_notify_sensitive_action`).
//!
//! The record carries `reported_state`, so each escalating state is reported
//! once and a later one (a `probation_failed` that became `rolled_back`) is
//! reported again. A report that did not land leaves `reported_state` alone and
//! is retried at the next boot.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{info, warn};

use super::record::{self, UpgradeRecord, UpgradeState};

/// The finding topic every upgrade report carries.
pub const FINDING_TOPIC: &str = "runner-self-upgrade";

/// The finding body (coord's `PostFindingBody`: `title`, `body`, `kind`,
/// `topic`, `resource_keys`; unknown fields are a 400).
pub fn finding_body(rec: &UpgradeRecord, device: &str) -> Value {
    let mut body = format!(
        "Runner upgrade {} on device {device}: {} -> {} ended `{}`.\n\nReason: {}\n",
        rec.id,
        rec.from_sha,
        rec.to_sha,
        rec.state.as_str(),
        rec.outcome_reason.as_deref().unwrap_or("(none recorded)")
    );
    if let Some(p) = &rec.probation {
        for f in &p.failures {
            body.push_str(&format!("- probation: {f}\n"));
        }
        for r in p.resumes.iter().filter(|r| !r.verified) {
            body.push_str(&format!(
                "- unverified resume: {} {} — {}\n",
                r.kind,
                r.session_id.as_deref().unwrap_or("(no id)"),
                r.verdict
            ));
        }
    }
    if rec.pg_schema_unverified {
        body.push_str("- the rollback could not be checked against an embedded-PG schema change\n");
    }
    body.push_str(&format!(
        "\nRecord: {}/{}.json (from {} to {}).",
        super::sys::upgrades_dir().display(),
        rec.id,
        rec.from_dir.display(),
        rec.to_dir.display()
    ));
    json!({
        "title": format!("runner upgrade {} -> {}: {} ({device})", rec.from_sha, rec.to_sha, rec.state.as_str()),
        "body": body,
        "kind": "investigation",
        "topic": FINDING_TOPIC,
        "resource_keys": [
            "qontinui-runner/src-tauri/src/runner_upgrade",
            "2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback",
        ],
    })
}

/// The operator notification (coord's `AgentNotificationBody`). A rollback is
/// coord's `rollback_dispatched`; every other escalation is `other`.
pub fn notification_body(rec: &UpgradeRecord) -> Value {
    let action = match rec.state {
        UpgradeState::RolledBack => "rollback_dispatched",
        _ => "other",
    };
    let artifact = match rec.state {
        UpgradeState::RolledBack => format!("the runner upgrade {} -> {} (back on {})", rec.from_sha, rec.to_sha, rec.from_sha),
        UpgradeState::RollbackRefused => format!(
            "the runner upgrade {} -> {}: rollback REFUSED across an embedded-PG schema change; the successor still serves",
            rec.from_sha, rec.to_sha
        ),
        UpgradeState::RollbackFailed => format!(
            "the runner upgrade {} -> {}: rollback FAILED; the runner may not be serving",
            rec.from_sha, rec.to_sha
        ),
        _ => format!(
            "the runner upgrade {} -> {}: probation failed (a holder not adopted or a resume unverified); the guard rolls back",
            rec.from_sha, rec.to_sha
        ),
    };
    let mut body = json!({
        "action": action,
        "artifact": artifact,
        "reversible": "restore",
        "checks": "upgrade preflight passed; probation and rollback guard",
    });
    if rec.state == UpgradeState::RolledBack {
        body["undo"] = Value::String(format!(
            "re-run POST /runner/upgrade with candidate {}",
            rec.to_dir.display()
        ));
    }
    body
}

/// Post one record's outcome; `Ok` only when BOTH writes were accepted.
pub async fn post_outcome(rec: &UpgradeRecord) -> Result<(), String> {
    let base = qontinui_runner_lib::profiles::connected_coord_base()
        .ok_or_else(|| "no coord base configured".to_string())?;
    let client =
        crate::coord_http::coord_client().ok_or_else(|| "no coord HTTP client".to_string())?;
    let device = qontinui_runner_lib::machine_identity::read_device_id()
        .unwrap_or_else(|_| "unknown".to_string());
    let base = base.trim_end_matches('/');
    for (url, body) in [
        (
            format!("{base}/coord/agent-findings"),
            finding_body(rec, &device),
        ),
        (
            format!("{base}/coord/agent-notifications"),
            notification_body(rec),
        ),
    ] {
        let resp =
            crate::coord_http::coord_post_for(client, &url, crate::auth::TenantScope::Device)
                .timeout(Duration::from_secs(10))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("POST {url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("POST {url}: {status}: {text}"));
        }
        // coord answers 200 `{"posted": false}` when its table is not
        // provisioned: accepted is not stored.
        if serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("posted").and_then(Value::as_bool))
            == Some(false)
        {
            return Err(format!(
                "POST {url}: coord accepted but did not store it: {text}"
            ));
        }
    }
    Ok(())
}

/// Report `rec` (at `path`) and mark it reported when both writes landed.
pub async fn report_record(path: &Path, rec: &mut UpgradeRecord) {
    if !rec.report_due() {
        return;
    }
    match post_outcome(rec).await {
        Ok(()) => {
            // Marked under the record's lock against the record as it now
            // stands, never by writing this copy back: the POSTs can take
            // ~20 s, and a guard may have concluded the record meanwhile.
            let now = chrono::Utc::now();
            let (p, state) = (path.to_path_buf(), rec.state);
            match tokio::task::spawn_blocking(move || record::mark_reported(&p, state, now)).await {
                Ok(Ok(true)) => {
                    rec.reported_state = Some(rec.state);
                    rec.reported_at = Some(record::rfc3339(now));
                }
                Ok(Ok(false)) => info!(
                    id = %rec.id,
                    "runner upgrade: the record moved on while its outcome was reported; its new state is reported in turn"
                ),
                Ok(Err(e)) => {
                    warn!(error = %e, "runner upgrade: reported, but the record could not be marked")
                }
                Err(e) => {
                    warn!(error = %e, "runner upgrade: reported, but the record could not be marked")
                }
            }
            info!(id = %rec.id, state = rec.state.as_str(), "runner upgrade: outcome reported to coord");
        }
        Err(e) => warn!(
            id = %rec.id,
            state = rec.state.as_str(),
            error = %e,
            "runner upgrade: the outcome did NOT reach coord; retried at the next boot"
        ),
    }
}

/// Report every record of the runner on `port` whose escalating outcome coord
/// has not heard yet. Coord auth may not be up the moment the runner boots, so
/// a miss is retried a few times before it waits for the next boot.
pub async fn report_pending(dir: &Path, port: u16) {
    for attempt in 0..5u32 {
        let (records, unreadable) = record::all_records(dir);
        for e in unreadable {
            warn!(error = %e, "runner upgrade: an upgrade record is unreadable");
        }
        let mut due = 0;
        for (path, mut rec) in records
            .into_iter()
            .filter(|(_, r)| r.api_port == port && r.report_due())
        {
            report_record(&path, &mut rec).await;
            if rec.report_due() {
                due += 1;
            }
        }
        if due == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(30 * u64::from(attempt + 1))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_upgrade::record::{sample_record, ProbationReport, ResumeOutcome};

    #[test]
    fn a_rollback_notifies_as_rollback_dispatched_with_an_undo() {
        let mut r = sample_record(std::path::Path::new("/p"));
        r.conclude(
            UpgradeState::RolledBack,
            "did not pass probation",
            "2026-11-24T22:03:00Z",
        );
        let n = notification_body(&r);
        assert_eq!(n["action"], "rollback_dispatched");
        assert!(n["undo"].as_str().unwrap().contains("/runner/upgrade"));
        // No full sha or uuid: coord would render "an unnamed artifact".
        assert!(n["artifact"].as_str().unwrap().contains(&r.from_sha));
        let f = finding_body(&r, "dev-1");
        assert_eq!(f["topic"], FINDING_TOPIC);
        assert_eq!(f["kind"], "investigation");
        let keys: Vec<&str> = f.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 5, "{keys:?}");
    }

    #[test]
    fn an_unverified_resume_is_named_in_the_finding() {
        let mut r = sample_record(std::path::Path::new("/p"));
        r.probation = Some(ProbationReport {
            resumes: vec![ResumeOutcome {
                session_id: Some("s-1".into()),
                kind: "in_process_pane".into(),
                verdict: "failed(respawn-died)".into(),
                verified: false,
            }],
            failures: vec!["in_process_pane s-1 not resumed".into()],
            ..Default::default()
        });
        r.conclude(UpgradeState::ProbationFailed, "probation failed", "t");
        let f = finding_body(&r, "dev-1");
        assert!(f["body"]
            .as_str()
            .unwrap()
            .contains("unverified resume: in_process_pane s-1"));
        assert_eq!(notification_body(&r)["action"], "other");
    }

    #[test]
    fn each_escalating_state_is_due_once() {
        let mut r = sample_record(std::path::Path::new("/p"));
        assert!(!r.report_due(), "in flight is not an outcome");
        r.state = UpgradeState::ProbationFailed;
        assert!(r.report_due());
        r.reported_state = Some(UpgradeState::ProbationFailed);
        assert!(!r.report_due());
        r.state = UpgradeState::RolledBack;
        assert!(
            r.report_due(),
            "the rollback after a failed probation is reported again"
        );
        r.state = UpgradeState::ProbationPassed;
        assert!(!r.report_due());
    }
}
