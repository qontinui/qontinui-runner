//! `coord_pr_ci_states(prs)` — a batched read of PR CI state from coord's PR
//! twin, keyed by `(repo, number)`, for the Session Manager's `is:closable`
//! (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! D5 and its 2026-10-13 vet correction).
//!
//! ## Why coord
//!
//! The runner's own PR ledger (`project.session_prs`) carries only
//! open / closed / merged — no CI. Served policy `finished-criteria`
//! `session-finished-at-green-not-at-merged` makes "initially green" the bar,
//! and coord's `coord_pr_status` card is where CI lives: every observed check
//! for the head, its own `confidence`, and `rebase_block_disposition`. The
//! runner reaches it the way the coord outside observer reaches coord's tool
//! surface: a device-authed JSON-RPC `tools/call` against
//! [`resolve_coord_mcp_door`]'s `/mcp` URL — one call per PR, bounded in
//! concurrency and time, cached briefly.
//!
//! ## Never a guessed green
//!
//! Each PR reads one of `green` | `pending` | `red` | `landed` | `closed` |
//! `unknown`. Anything coord could not establish — no checks observed, a card
//! whose `confidence` is not `fresh`, an explicit "required-check status
//! unknown" blocker, coord unreachable, a refused call — is `unknown` with a
//! reason, which the query engine evaluates as UNKNOWN (the row lands in the
//! "Unknown for this filter" tail) rather than as closable or not. `red` is
//! not "code error" either: attribution (`finished-criteria`
//! `a-red-check-is-not-automatically-a-code-error`) is not established here,
//! so `is:closable` treats red as UNKNOWN too. `authorActs` is
//! `rebase_block_disposition == author_acts` — owed work, so not closable
//! (`finished-criteria` `a-textual-conflict-is-owed-not-watched`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::TenantScope;
use qontinui_runner_lib::coord_doctor::resolve_coord_mcp_door;
use qontinui_runner_lib::tenant_pin::TenantPin;

/// coord tool calls in flight at once.
pub const PR_CI_CONCURRENCY: usize = 4;
/// Per-call bound, enforced off the tokio runtime (`off_runtime::deadline`).
pub const PR_CI_CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// How long an answered card is reused.
pub const PR_CI_CACHE_TTL: Duration = Duration::from_secs(60);
/// PRs read per command call; the rest are `unknown` (`request_cap`).
pub const PR_CI_MAX_PER_CALL: usize = 100;

const TOOL: &str = "coord_pr_status";

/// A PR the caller asks about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrRef {
    /// `owner/name` or a bare name — coord resolves both.
    pub repo: String,
    pub number: i64,
}

/// One PR's CI state as `is:closable` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrCiRead {
    pub repo: String,
    pub number: i64,
    /// `green` | `pending` | `red` | `landed` | `closed` | `unknown`.
    pub state: String,
    /// `rebase_block_disposition == author_acts`; `None` when coord served no
    /// disposition (UNKNOWN, not "coord retries").
    pub author_acts: Option<bool>,
    /// Why `state` is `unknown`, or what made it `red` / `pending`.
    pub reason: Option<String>,
    /// The card's own `confidence` (`fresh` | `stale` | `unknown`), when read.
    pub confidence: Option<String>,
    /// Unix millis this answer was read from coord.
    pub observed_at: i64,
}

impl PrCiRead {
    fn unknown(pr: &PrRef, reason: impl Into<String>) -> Self {
        Self {
            repo: pr.repo.clone(),
            number: pr.number,
            state: "unknown".to_string(),
            author_acts: None,
            reason: Some(reason.into()),
            confidence: None,
            observed_at: chrono::Utc::now().timestamp_millis(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrCiStatesResponse {
    /// In request order, one per distinct requested PR.
    pub prs: Vec<PrCiRead>,
}

const FAILING: &[&str] = &[
    "failure",
    "timed_out",
    "cancelled",
    "action_required",
    "startup_failure",
    "stale",
];
const PASSING: &[&str] = &["success", "neutral", "skipped"];

/// Classify one `coord_pr_status` card. Pure.
pub fn classify_card(pr: &PrRef, card: &Value, observed_at: i64) -> PrCiRead {
    let s = |k: &str| card.get(k).and_then(Value::as_str).map(str::to_string);
    let confidence = s("confidence");
    let author_acts = s("rebase_block_disposition").map(|d| d == "author_acts");
    let read = |state: &str, reason: Option<String>| PrCiRead {
        repo: pr.repo.clone(),
        number: pr.number,
        state: state.to_string(),
        author_acts,
        reason,
        confidence: confidence.clone(),
        observed_at,
    };
    match s("pr_state").as_deref() {
        Some("merged") => return read("landed", None),
        Some("closed") => return read("closed", None),
        Some("open") | Some("draft") => {}
        other => {
            return read(
                "unknown",
                Some(format!("pr_state {}", other.unwrap_or("absent"))),
            )
        }
    }
    if confidence.as_deref() != Some("fresh") {
        return read(
            "unknown",
            Some(format!(
                "card confidence {}",
                confidence.as_deref().unwrap_or("absent")
            )),
        );
    }
    let blocker_unknown = card
        .get("blockers")
        .and_then(Value::as_array)
        .is_some_and(|b| {
            b.iter()
                .filter_map(Value::as_str)
                .any(|t| t.contains("required-check status unknown"))
        });
    if blocker_unknown {
        return read("unknown", Some("required-check status unknown".to_string()));
    }
    let Some(checks) = card.get("checks").and_then(Value::as_array) else {
        return read("unknown", Some("card carried no checks".to_string()));
    };
    if checks.is_empty() {
        return read("unknown", Some("no checks observed".to_string()));
    }
    let mut failing = Vec::new();
    let mut running = Vec::new();
    let mut odd = Vec::new();
    for c in checks {
        let name = c
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
        let conclusion = c.get("conclusion").and_then(Value::as_str);
        match conclusion {
            Some(cl) if FAILING.contains(&cl) => failing.push(name),
            Some(cl) if PASSING.contains(&cl) && status == "completed" => {}
            None if status != "completed" => running.push(name),
            _ => odd.push(name),
        }
    }
    if !failing.is_empty() {
        return read("red", Some(format!("failing: {}", failing.join(", "))));
    }
    if !running.is_empty() {
        return read("pending", Some(format!("running: {}", running.join(", "))));
    }
    if !odd.is_empty() {
        return read(
            "unknown",
            Some(format!("unclassified checks: {}", odd.join(", "))),
        );
    }
    read("green", None)
}

/// The tool result's JSON body, or why there is none. Pure.
pub fn unwrap_tool_result(rpc: &Value) -> Result<Value, String> {
    if let Some(err) = rpc.get("error").filter(|v| !v.is_null()) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("(no message)");
        return Err(format!("coord JSON-RPC error: {msg}"));
    }
    let result = rpc
        .get("result")
        .ok_or("coord response carried no result")?;
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .ok_or("coord result carried no content text")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "coord refused {TOOL}: {}",
            text.chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(text).map_err(|e| format!("coord {TOOL} text is not JSON: {e}"))
}

/// The tenant scope a coord `/mcp` call from this runner presents — the same
/// mapping the coord outside observer uses.
fn scope_for(pin: TenantPin) -> TenantScope {
    match pin {
        TenantPin::Pinned(t) => TenantScope::Owned(t),
        TenantPin::Unpinned => TenantScope::Device,
        TenantPin::Unresolvable => TenantScope::Unresolved,
    }
}

async fn fetch_card(client: &reqwest::Client, url: &str, pin: TenantPin, pr: &PrRef) -> PrCiRead {
    fetch_card_within(client, url, pin, pr, PR_CI_CALL_TIMEOUT).await
}

/// [`fetch_card`] under an explicit bound — the tested core.
async fn fetch_card_within(
    client: &reqwest::Client,
    url: &str,
    pin: TenantPin,
    pr: &PrRef,
    bound: Duration,
) -> PrCiRead {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": TOOL, "arguments": { "repo": pr.repo, "number": pr.number } },
    });
    let rb = crate::auth::attach_device_auth_for(client.post(url).json(&body), scope_for(pin));
    // Bounded by `off_runtime::deadline`, like the sibling commands — never a
    // tokio-timer-driven bound (reqwest `.timeout()` included), which goes
    // inert when every worker is parked. Covers the send AND the body read.
    let call = async {
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("coord unreachable: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("coord /mcp HTTP {status}"));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| format!("coord body unreadable: {e}"))
    };
    let rpc: Value = tokio::select! {
        r = call => match r {
            Ok(v) => v,
            Err(reason) => return PrCiRead::unknown(pr, reason),
        },
        _ = crate::off_runtime::deadline(bound) => {
            return PrCiRead::unknown(
                pr,
                format!("coord call timed out after {} ms", bound.as_millis()),
            );
        }
    };
    match unwrap_tool_result(&rpc) {
        Ok(card) => classify_card(pr, &card, chrono::Utc::now().timestamp_millis()),
        Err(reason) => PrCiRead::unknown(pr, reason),
    }
}

type CiCache = Mutex<HashMap<PrRef, (Instant, PrCiRead)>>;

fn ci_cache() -> &'static CiCache {
    static CACHE: OnceLock<CiCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Dedupe `prs` (keeping request order) and split off the request cap.
pub fn plan_request(prs: Vec<PrRef>) -> (Vec<PrRef>, Vec<PrRef>) {
    let mut seen = std::collections::HashSet::new();
    let distinct: Vec<PrRef> = prs
        .into_iter()
        .filter(|p| !p.repo.trim().is_empty() && p.number > 0)
        .filter(|p| seen.insert(p.clone()))
        .collect();
    let cut = distinct.len().min(PR_CI_MAX_PER_CALL);
    let (keep, over) = distinct.split_at(cut);
    (keep.to_vec(), over.to_vec())
}

/// `coord_pr_ci_states(prs)` — CI state per `(repo, number)`, from coord's PR
/// twin. Never `Err`: an unreachable coord answers every PR `unknown` with
/// the reason.
#[tauri::command]
pub async fn coord_pr_ci_states(prs: Vec<PrRef>) -> Result<PrCiStatesResponse, String> {
    let (keep, over) = plan_request(prs);
    let now = Instant::now();
    let mut answers: HashMap<PrRef, PrCiRead> = HashMap::new();
    let mut to_fetch = Vec::new();
    if let Ok(c) = ci_cache().lock() {
        for pr in &keep {
            match c
                .get(pr)
                .filter(|(at, _)| now.saturating_duration_since(*at) < PR_CI_CACHE_TTL)
            {
                Some((_, read)) => {
                    answers.insert(pr.clone(), read.clone());
                }
                None => to_fetch.push(pr.clone()),
            }
        }
    } else {
        to_fetch = keep.clone();
    }

    if !to_fetch.is_empty() {
        let door = resolve_coord_mcp_door();
        match crate::coord_http::coord_client() {
            None => {
                for pr in &to_fetch {
                    answers.insert(pr.clone(), PrCiRead::unknown(pr, "no coord http client"));
                }
            }
            Some(client) => {
                let semaphore = tokio::sync::Semaphore::new(PR_CI_CONCURRENCY);
                let fetched = futures::future::join_all(to_fetch.iter().map(|pr| {
                    let semaphore = &semaphore;
                    let url = door.url.as_str();
                    let pin = door.pin;
                    async move {
                        let _permit = semaphore.acquire().await;
                        fetch_card(client, url, pin, pr).await
                    }
                }))
                .await;
                if let Ok(mut c) = ci_cache().lock() {
                    c.retain(|_, (at, _)| now.saturating_duration_since(*at) < PR_CI_CACHE_TTL);
                    for (pr, read) in to_fetch.iter().zip(&fetched) {
                        // Only an answer coord gave is cached; a transport
                        // failure is retried on the next call.
                        if read.confidence.is_some() || read.state != "unknown" {
                            c.insert(pr.clone(), (now, read.clone()));
                        }
                    }
                }
                for (pr, read) in to_fetch.into_iter().zip(fetched) {
                    answers.insert(pr, read);
                }
            }
        }
    }

    let mut out: Vec<PrCiRead> = keep
        .iter()
        .map(|pr| {
            answers
                .remove(pr)
                .unwrap_or_else(|| PrCiRead::unknown(pr, "not read"))
        })
        .collect();
    out.extend(over.iter().map(|pr| PrCiRead::unknown(pr, "request_cap")));
    Ok(PrCiStatesResponse { prs: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr() -> PrRef {
        PrRef {
            repo: "qontinui/qontinui-runner".into(),
            number: 7,
        }
    }

    fn check(name: &str, status: &str, conclusion: Option<&str>) -> Value {
        json!({ "name": name, "status": status, "conclusion": conclusion })
    }

    fn open_card(checks: Vec<Value>) -> Value {
        json!({
            "pr_state": "open",
            "confidence": "fresh",
            "rebase_block_disposition": "coord_retries",
            "blockers": [],
            "checks": checks,
        })
    }

    #[test]
    fn all_passing_checks_are_green() {
        let r = classify_card(
            &pr(),
            &open_card(vec![
                check("build", "completed", Some("success")),
                check("lint", "completed", Some("skipped")),
            ]),
            1,
        );
        assert_eq!(r.state, "green");
        assert_eq!(r.author_acts, Some(false));
    }

    #[test]
    fn a_failing_check_is_red_even_beside_a_running_one() {
        let r = classify_card(
            &pr(),
            &open_card(vec![
                check("build", "completed", Some("failure")),
                check("test", "in_progress", None),
            ]),
            1,
        );
        assert_eq!(r.state, "red");
        assert_eq!(r.reason.as_deref(), Some("failing: build"));
    }

    #[test]
    fn a_running_check_is_pending() {
        let r = classify_card(&pr(), &open_card(vec![check("test", "queued", None)]), 1);
        assert_eq!(r.state, "pending");
    }

    #[test]
    fn no_checks_or_a_non_fresh_card_is_unknown_not_green() {
        assert_eq!(classify_card(&pr(), &open_card(vec![]), 1).state, "unknown");
        let mut stale = open_card(vec![check("b", "completed", Some("success"))]);
        stale["confidence"] = json!("stale");
        let r = classify_card(&pr(), &stale, 1);
        assert_eq!(r.state, "unknown");
        assert_eq!(r.confidence.as_deref(), Some("stale"));
        let mut blind = open_card(vec![check("b", "completed", Some("success"))]);
        blind["blockers"] = json!(["required-check status unknown for this head"]);
        assert_eq!(classify_card(&pr(), &blind, 1).state, "unknown");
    }

    #[test]
    fn merged_and_closed_are_terminal() {
        assert_eq!(
            classify_card(&pr(), &json!({"pr_state": "merged"}), 1).state,
            "landed"
        );
        assert_eq!(
            classify_card(&pr(), &json!({"pr_state": "closed"}), 1).state,
            "closed"
        );
        assert_eq!(classify_card(&pr(), &json!({}), 1).state, "unknown");
    }

    #[test]
    fn author_acts_is_carried_and_absent_is_unknown() {
        let mut card = open_card(vec![check("b", "completed", Some("success"))]);
        card["rebase_block_disposition"] = json!("author_acts");
        assert_eq!(classify_card(&pr(), &card, 1).author_acts, Some(true));
        card.as_object_mut()
            .unwrap()
            .remove("rebase_block_disposition");
        assert_eq!(classify_card(&pr(), &card, 1).author_acts, None);
    }

    #[test]
    fn the_tool_envelope_unwraps_and_a_refusal_is_an_error() {
        let ok =
            json!({"result": {"content": [{"type": "text", "text": "{\"pr_state\":\"open\"}"}]}});
        assert_eq!(unwrap_tool_result(&ok).unwrap()["pr_state"], json!("open"));
        let refused = json!({"result": {"isError": true, "content": [{"text": "no such PR"}]}});
        assert!(unwrap_tool_result(&refused)
            .unwrap_err()
            .contains("no such PR"));
        let rpc_err = json!({"error": {"code": -32601, "message": "masked"}});
        assert!(unwrap_tool_result(&rpc_err).unwrap_err().contains("masked"));
    }

    #[test]
    fn a_request_is_deduped_validated_and_capped() {
        let mut prs = vec![
            pr(),
            pr(),
            PrRef {
                repo: " ".into(),
                number: 1,
            },
        ];
        prs.extend((1..=PR_CI_MAX_PER_CALL as i64 + 2).map(|n| PrRef {
            repo: "r".into(),
            number: n,
        }));
        let (keep, over) = plan_request(prs);
        assert_eq!(keep.len(), PR_CI_MAX_PER_CALL);
        assert_eq!(keep[0], pr());
        assert_eq!(over.len(), 3);
    }

    /// A coord that accepts the connection and never answers is bounded by
    /// the off-runtime deadline and reads `unknown` with the reason — never
    /// a hang and never a guessed state.
    #[tokio::test]
    async fn pr_ci_state_a_silent_coord_is_unknown_after_the_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock); // accept, never respond
            }
        });
        let client = reqwest::Client::new();
        let started = Instant::now();
        let read = fetch_card_within(
            &client,
            &format!("http://{addr}/mcp"),
            TenantPin::Unpinned,
            &pr(),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(read.state, "unknown");
        assert!(
            read.reason.as_deref().unwrap_or("").contains("timed out"),
            "{read:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        hold.abort();
    }
}
