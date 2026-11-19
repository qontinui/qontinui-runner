//! The `finished_close` arm's close guard: may THIS finished session be closed
//! now? One pure function over every input served `finished-criteria` names,
//! each read fresh for the one close it decides.
//!
//! Plan
//! `2026-11-16-finished-close-mints-its-close-authority-from-the-served-close-conditions`,
//! Phase 1 (D2, D4, D5, D6). It is the seam the lifecycle kernel's `decide()`
//! later replaces (lifecycle plan Phase 93): only the guard's BODY changes then.
//!
//! ## Shadow only, in this phase
//!
//! [`judge`] decides and the executor LOGS and PUBLISHES the verdict
//! (`close guard: would mint | held <reason> | unknown <reason>`, and
//! `windDown.closeGuard` on `/restart-readiness`). A non-[`GuardVerdict::Close`]
//! verdict stops the close attempt; a `Close` verdict lets the attempt proceed
//! exactly as before, still presenting NO close authority, so the primitive
//! still refuses it `no_close_authority`. Minting from a [`GuardClose`] is
//! Phase 2, gated on this phase's shadow evidence.
//!
//! ## The arms, in evaluation order (every one fail-closed)
//!
//! 1. **`eligibility`** — conditions 1 (finished) and 2 (idle, no children),
//!    from the per-close re-observed `windDown` verdict, which already carries
//!    `work_status_unknown` and `children_unknown`. A pane the re-check could
//!    not find is UNKNOWN; a session that is not a terminal session is held.
//! 2. **`custody`** — condition 3, the per-close custody re-check: every
//!    worktree the session touched is clean and pushed.
//! 3. **`obligations`** — the remote half of `closeable`: every item coord's
//!    `coord_session_obligations` attributes to the session is watched.
//! 4. **`first_verdict`** — `the-product-holds-the-close-for-the-first-verdict`
//!    (D5), judged over the SAME obligations body as arm 3 (one read per
//!    close): every unsettled PR's current head must have a first CI verdict.
//! 5. **`revival`** — condition 4 (D6): no prompt or turn opener newer than the
//!    finish stamp + 1 s, none within ±1 s of it, and no assistant work more
//!    than 10 min past it, read from the newest transcript copy.
//! 6. **`undelivered_messages`** — condition 2's "no undelivered message":
//!    coord holds no undelivered message addressed to the session.
//!
//! The FIRST arm that is not `ok` decides. `held` and `unknown` both mean no
//! close; they differ only in what they report. Every arm is still evaluated,
//! so the log line and `windDown.closeGuard` show every input.

use serde_json::Value;

use super::transcript_facts::TranscriptFacts;
use qontinui_runner_lib::wind_down::{
    CloseGuardArmView, CloseGuardView, CustodyVerdict, ObligationsVerdict, SessionKind,
    WindDownView,
};

/// A turn within this of coord's stamp is the stamp's own band
/// (`BAND_MS` in `close-finished-sessions.py`).
pub const BAND_MS: i64 = 1000;

/// Assistant work this far past the finish stamp reopened the session
/// (`KEPT_WORKING_MS`). The script also requires "no closeout phrase"; this
/// port reads no phrases, so ANY such work holds — the conservative half.
pub const KEPT_WORKING_MS: i64 = 10 * 60_000;

/// coord's `PENDING_PAGE_LIMIT` for `GET /coord/session-messages/pending`. A
/// page this full may have left a recipient's row off it.
pub const PENDING_PAGE_LIMIT: usize = 50;

/// CI conclusions that are not a failure. A completed check with one of these
/// passed; any other conclusion is a (failing) verdict.
const PASSING_CONCLUSIONS: [&str; 3] = ["success", "skipped", "neutral"];

/// The arm names, in evaluation order.
pub const ARMS: [&str; 6] = [
    "eligibility",
    "custody",
    "obligations",
    "first_verdict",
    "revival",
    "undelivered_messages",
];

/// One arm's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmVerdict {
    Ok,
    /// A known input rules the close out.
    Held {
        reason: &'static str,
        detail: String,
    },
    /// An input could not be determined.
    Unknown {
        reason: &'static str,
        detail: String,
    },
}

impl ArmVerdict {
    fn held(reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Held {
            reason,
            detail: detail.into(),
        }
    }

    fn unknown(reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Unknown {
            reason,
            detail: detail.into(),
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Held { .. } => "held",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::Held { reason, .. } | Self::Unknown { reason, .. } => Some(reason),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Held { detail, .. } | Self::Unknown { detail, .. } => Some(detail),
        }
    }
}

/// The proof that [`judge`] answered `Close` for exactly this session in
/// exactly this pane. Its fields are private and it has no constructor outside
/// this module, so the only way to hold one is a `Close` from [`judge`]. Phase
/// 2's minter consumes it by value.
#[derive(Debug, PartialEq, Eq)]
pub struct GuardClose {
    session_id: String,
    terminal_id: String,
}

impl GuardClose {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }
}

/// Why the guard did not answer `Close`: the deciding arm and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardReason {
    pub arm: &'static str,
    pub reason: &'static str,
    pub detail: String,
}

/// The guard's verdict.
#[derive(Debug, PartialEq, Eq)]
pub enum GuardVerdict {
    Close(GuardClose),
    Held(GuardReason),
    Unknown(GuardReason),
}

impl GuardVerdict {
    /// `would_mint` | `held` | `unknown` — the log and readiness word.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Close(_) => "would_mint",
            Self::Held(_) => "held",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn reason(&self) -> Option<&GuardReason> {
        match self {
            Self::Close(_) => None,
            Self::Held(r) | Self::Unknown(r) => Some(r),
        }
    }
}

/// One undelivered coord message read for the close, with the session the
/// runner could attribute its `to_session` to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    pub message_id: String,
    pub to_session: String,
    /// The Claude session id `to_session` resolves to on this runner — `None`
    /// when it resolves to nothing the runner knows (the guard then cannot
    /// rule out that it is this session's).
    pub owner: Option<String>,
}

/// Everything [`judge`] reads, each read for THIS close.
#[derive(Debug, Clone, Copy)]
pub struct GuardInputs<'a> {
    pub session_id: &'a str,
    pub terminal_id: &'a str,
    /// The per-close re-observed `windDown` verdict; `None` when the re-check
    /// could not find the session in its pane.
    pub eligibility: Option<&'a WindDownView>,
    /// The per-close custody verdict; `None` when it was not read.
    pub custody: Option<&'a CustodyVerdict>,
    /// The per-close obligations verdict.
    pub obligations: &'a ObligationsVerdict,
    /// The SAME read's body; `None` when the read failed.
    pub obligations_body: Option<&'a Value>,
    /// The newest transcript copy's facts; `None` when not read.
    pub transcript: Option<&'a Result<TranscriptFacts, String>>,
    /// coord's undelivered messages for this device, attributed.
    pub messages: &'a Result<Vec<PendingMessage>, String>,
    /// The guard's clock (unix ms).
    pub now_ms: i64,
}

/// Arm 1: conditions 1 and 2, from the per-close re-observed verdict.
pub fn eligibility_arm(view: Option<&WindDownView>) -> ArmVerdict {
    let Some(view) = view else {
        return ArmVerdict::unknown(
            "session_not_observed",
            "the per-close re-check could not find the session in its pane",
        );
    };
    if view.kind != SessionKind::Terminal {
        return ArmVerdict::held(
            "not_a_terminal_session",
            format!("the pane holds a {:?} session", view.kind),
        );
    }
    match view.eligibility {
        "eligible" => ArmVerdict::Ok,
        "unknown" => ArmVerdict::unknown(
            view.reason.unwrap_or("eligibility_unknown"),
            "an eligibility input could not be determined",
        ),
        other => ArmVerdict::held(
            view.reason.unwrap_or(other),
            format!("the re-checked eligibility is `{other}`"),
        ),
    }
}

/// Arm 2: condition 3.
pub fn custody_arm(custody: Option<&CustodyVerdict>) -> ArmVerdict {
    match custody {
        None => ArmVerdict::unknown("custody_not_read", "custody was not re-judged"),
        Some(CustodyVerdict::Clean) => ArmVerdict::Ok,
        Some(v @ CustodyVerdict::Dirty(_)) => ArmVerdict::held(
            v.reason().unwrap_or("dirty"),
            v.detail().unwrap_or_default(),
        ),
        Some(v @ CustodyVerdict::Unknown(_)) => ArmVerdict::unknown(
            v.reason().unwrap_or("unknown"),
            v.detail().unwrap_or_default(),
        ),
    }
}

/// Arm 3: the remote half of `closeable`.
pub fn obligations_arm(verdict: &ObligationsVerdict) -> ArmVerdict {
    match verdict {
        ObligationsVerdict::Watched { .. } => ArmVerdict::Ok,
        ObligationsVerdict::Unwatched(_) => {
            ArmVerdict::held("not_watched", verdict.detail().unwrap_or_default())
        }
        ObligationsVerdict::Unknown(u) => {
            ArmVerdict::unknown(u.as_str(), verdict.detail().unwrap_or_default())
        }
    }
}

/// Arm 4: the first-verdict hold (D5), over the obligations body.
///
/// For each PR item (`item.pr_number` non-null) that is not settled — its
/// verdict `landed` or `retired`, or its `facts.pr_state` closed or merged:
/// `facts` null, `head_sha` missing, or `confidence` not `fresh` is UNKNOWN (a
/// stale card's checks may describe an older head). Otherwise the head has a
/// first verdict when some non-`coord_gate` check is `completed` with a
/// conclusion that is not a pass, or when every non-`coord_gate` check is
/// `completed` and there is at least one; a completed check with no
/// conclusion is UNKNOWN. Anything else — including a head with no
/// non-`coord_gate` check yet — is held `first_verdict_pending`. Branch-only
/// items (`item.pr_number` an explicit null) carry no facts; the obligations
/// arm already judged them. An item with no `item.pr_number` key at all is
/// UNKNOWN: coord writes that key on every item, so its absence says the item
/// was not read, not that it has no PR.
pub fn first_verdict_arm(body: Option<&Value>) -> ArmVerdict {
    let Some(body) = body else {
        return ArmVerdict::unknown(
            "obligations_unreadable",
            "no obligations body to read PR heads from",
        );
    };
    let Some(items) = body
        .get("prs")
        .and_then(|b| b.get("items"))
        .and_then(Value::as_array)
    else {
        return ArmVerdict::unknown(
            "prs_unreadable",
            "the obligations body carries no prs.items list",
        );
    };
    for item in items {
        let Some(pr) = item.get("item").and_then(|i| i.get("pr_number")) else {
            return ArmVerdict::unknown(
                "pr_item_unreadable",
                "a prs item carries no item.pr_number, so it cannot be told from a PR",
            );
        };
        if pr.is_null() {
            continue;
        }
        let name = item
            .get("item")
            .map_or_else(|| "-".to_string(), Value::to_string);
        let verdict = item.get("verdict").and_then(Value::as_str);
        if matches!(verdict, Some("landed" | "retired")) {
            continue;
        }
        let facts = item.get("facts").filter(|f| f.is_object());
        let state = facts
            .and_then(|f| f.get("pr_state"))
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        if matches!(state.as_deref(), Some("closed" | "merged")) {
            continue;
        }
        let Some(facts) = facts else {
            return ArmVerdict::unknown("pr_facts_absent", format!("PR {name} carries no facts"));
        };
        if facts
            .get("head_sha")
            .and_then(Value::as_str)
            .is_none_or(|h| h.trim().is_empty())
        {
            return ArmVerdict::unknown("pr_head_unknown", format!("PR {name} has no head_sha"));
        }
        let confidence = facts.get("confidence").and_then(Value::as_str);
        if !confidence.is_some_and(|c| c.eq_ignore_ascii_case("fresh")) {
            return ArmVerdict::unknown(
                "pr_facts_not_fresh",
                format!(
                    "PR {name}'s card confidence is {}, so its checks may describe an older head",
                    confidence.unwrap_or("absent")
                ),
            );
        }
        let Some(checks) = facts.get("checks").and_then(Value::as_array) else {
            return ArmVerdict::unknown(
                "pr_checks_unreadable",
                format!("PR {name} has no checks list"),
            );
        };
        let ci: Vec<&Value> = checks
            .iter()
            .filter(|c| c.get("coord_gate").and_then(Value::as_bool) != Some(true))
            .collect();
        let completed = |c: &Value| {
            c.get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case("completed"))
        };
        let mut failed = false;
        for check in ci.iter().copied().filter(|c| completed(c)) {
            match check.get("conclusion").and_then(Value::as_str) {
                None => {
                    return ArmVerdict::unknown(
                        "check_conclusion_unknown",
                        format!("PR {name} has a completed check with no conclusion"),
                    )
                }
                Some(c) if !PASSING_CONCLUSIONS.contains(&c.to_ascii_lowercase().as_str()) => {
                    failed = true;
                }
                Some(_) => {}
            }
        }
        let all_done = !ci.is_empty() && ci.iter().all(|c| completed(c));
        if !(failed || all_done) {
            return ArmVerdict::held(
                "first_verdict_pending",
                format!(
                    "PR {name}'s head has no CI verdict yet ({} of {} check(s) completed)",
                    ci.iter().filter(|c| completed(c)).count(),
                    ci.len()
                ),
            );
        }
    }
    ArmVerdict::Ok
}

/// Arm 5: condition 4 — the served revival rule (`finish_state`'s `revived`,
/// `in_band` and `untimed` arms), judged against coord's finish stamp from the
/// per-close re-check.
pub fn revival_arm(
    stamp_ms: Option<i64>,
    transcript: Option<&Result<TranscriptFacts, String>>,
    now_ms: i64,
) -> ArmVerdict {
    let Some(stamp) = stamp_ms else {
        return ArmVerdict::unknown(
            "untimed",
            "coord's finish carries no time, so no turn can be dated against it",
        );
    };
    if stamp > now_ms {
        // A stamp ahead of this clock proves coord's clock runs ahead, so it
        // is not compared with local transcript times.
        return ArmVerdict::unknown("untimed", "coord's finish time is later than this clock");
    }
    let facts = match transcript {
        None => return ArmVerdict::unknown("transcript_not_read", "the transcript was not read"),
        Some(Err(e)) => return ArmVerdict::unknown("transcript_unreadable", e.clone()),
        Some(Ok(f)) => f,
    };
    let after = |t: Option<i64>| t.filter(|t| *t > stamp + BAND_MS);
    if let Some(t) = after(facts.last_prompt_ms) {
        return ArmVerdict::held(
            "revived",
            format!("prompted {} s after coord's finish", (t - stamp) / 1000),
        );
    }
    if let Some(t) = after(facts.last_turn_open_ms) {
        return ArmVerdict::held(
            "revived",
            format!(
                "a turn opened {} s after coord's finish with no prompt",
                (t - stamp) / 1000
            ),
        );
    }
    if let Some(t) = facts.last_asst_ms.filter(|t| t - stamp > KEPT_WORKING_MS) {
        return ArmVerdict::held(
            "kept_working",
            format!(
                "assistant work {} min after coord's finish",
                (t - stamp) / 60_000
            ),
        );
    }
    if !facts.reaches(stamp, BAND_MS) {
        return ArmVerdict::unknown(
            "transcript_window_short",
            "the transcript tail read does not reach back to the finish, so a turn after it \
             cannot be ruled out",
        );
    }
    let in_band = |t: Option<i64>| t.is_some_and(|t| stamp - BAND_MS < t && t <= stamp + BAND_MS);
    if in_band(facts.last_prompt_ms) || in_band(facts.last_turn_open_ms) {
        return ArmVerdict::held(
            "in_band",
            "the last turn sits inside coord's 1 s stamp band, so the finish cannot be ordered \
             against it",
        );
    }
    ArmVerdict::Ok
}

/// Arm 6: condition 2's "no undelivered message".
///
/// `messages` is coord's `GET /coord/session-messages/pending` page for this
/// device (undelivered, unacked, unexpired), each row attributed by the
/// runner. A row addressed to this session holds the close. An empty page is
/// none. A FULL page ([`PENDING_PAGE_LIMIT`]) may have left this session's row
/// off it, and a row the runner cannot attribute may be this session's
/// (coord's `to_session` is an `agent_sessions` id the runner does not always
/// know); both are UNKNOWN. A message already delivered (injected) but not
/// yet acked is not on the page — it reached the pane as a turn, which the
/// revival arm catches.
pub fn messages_arm(
    session_id: &str,
    messages: &Result<Vec<PendingMessage>, String>,
) -> ArmVerdict {
    let rows = match messages {
        Err(e) => return ArmVerdict::unknown("undelivered_messages_unreadable", e.clone()),
        Ok(rows) => rows,
    };
    let ours = |id: &str| id.eq_ignore_ascii_case(session_id);
    if let Some(row) = rows
        .iter()
        .find(|r| ours(&r.to_session) || r.owner.as_deref().is_some_and(ours))
    {
        return ArmVerdict::held(
            "undelivered_message",
            format!(
                "coord holds undelivered message {} for this session",
                row.message_id
            ),
        );
    }
    if rows.len() >= PENDING_PAGE_LIMIT {
        return ArmVerdict::unknown(
            "undelivered_messages_page_full",
            format!(
                "coord's pending page is full ({} rows), so a row for this session may be off it",
                rows.len()
            ),
        );
    }
    if let Some(row) = rows.iter().find(|r| r.owner.is_none()) {
        return ArmVerdict::unknown(
            "undelivered_messages_unattributable",
            format!(
                "undelivered message {} is addressed to {}, which this runner cannot attribute",
                row.message_id, row.to_session
            ),
        );
    }
    ArmVerdict::Ok
}

/// Every arm's answer, in [`ARMS`] order.
pub fn arms(inputs: &GuardInputs<'_>) -> Vec<(&'static str, ArmVerdict)> {
    let stamp = inputs.eligibility.and_then(|v| v.finished_at_ms);
    vec![
        ("eligibility", eligibility_arm(inputs.eligibility)),
        ("custody", custody_arm(inputs.custody)),
        ("obligations", obligations_arm(inputs.obligations)),
        ("first_verdict", first_verdict_arm(inputs.obligations_body)),
        (
            "revival",
            revival_arm(stamp, inputs.transcript, inputs.now_ms),
        ),
        (
            "undelivered_messages",
            messages_arm(inputs.session_id, inputs.messages),
        ),
    ]
}

/// The verdict from the arms: the first that is not `ok` decides; all `ok` is
/// `Close`.
fn decide(inputs: &GuardInputs<'_>, arms: &[(&'static str, ArmVerdict)]) -> GuardVerdict {
    for (arm, verdict) in arms {
        match verdict {
            ArmVerdict::Ok => {}
            ArmVerdict::Held { reason, detail } => {
                return GuardVerdict::Held(GuardReason {
                    arm,
                    reason,
                    detail: detail.clone(),
                })
            }
            ArmVerdict::Unknown { reason, detail } => {
                return GuardVerdict::Unknown(GuardReason {
                    arm,
                    reason,
                    detail: detail.clone(),
                })
            }
        }
    }
    GuardVerdict::Close(GuardClose {
        session_id: inputs.session_id.to_string(),
        terminal_id: inputs.terminal_id.to_string(),
    })
}

/// The guard. Pure.
pub fn judge(inputs: &GuardInputs<'_>) -> GuardVerdict {
    judge_with_arms(inputs).0
}

/// [`judge`], plus every arm's answer for the log and `windDown.closeGuard`.
pub fn judge_with_arms(
    inputs: &GuardInputs<'_>,
) -> (GuardVerdict, Vec<(&'static str, ArmVerdict)>) {
    let arms = arms(inputs);
    (decide(inputs, &arms), arms)
}

/// The `windDown.closeGuard` view of one judgement.
pub fn view(
    verdict: &GuardVerdict,
    arms: &[(&'static str, ArmVerdict)],
    judged_at_ms: i64,
) -> CloseGuardView {
    CloseGuardView {
        verdict: verdict.word(),
        decided_by: verdict.reason().map(|r| r.arm),
        reason: verdict.reason().map(|r| r.reason),
        arms: arms
            .iter()
            .map(|(arm, v)| CloseGuardArmView {
                arm,
                verdict: v.word(),
                reason: v.reason(),
                detail: v.detail().map(|d| d.chars().take(300).collect()),
            })
            .collect(),
        judged_at: judged_at_ms,
    }
}

/// One `arm=verdict(reason)` token per arm, for the INFO line.
pub fn describe_arms(arms: &[(&'static str, ArmVerdict)]) -> String {
    arms.iter()
        .map(|(arm, v)| match v.reason() {
            Some(reason) => format!("{arm}={}({reason})", v.word()),
            None => format!("{arm}={}", v.word()),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_runner_lib::wind_down::{
        CustodyDirty, CustodyUnknown, Eligibility, IneligibleReason, ObligationsUnknown,
        ObligationsUnwatched, UnknownReason,
    };

    const M: i64 = 60_000;
    const NOW: i64 = 10_000 * M;

    fn eligible_view(stamp: Option<i64>) -> WindDownView {
        let mut v = WindDownView::from_verdict(
            &Eligibility::Eligible { since_ms: 0 },
            SessionKind::Terminal,
        );
        v.finished_at_ms = stamp;
        v
    }

    /// The script's base transcript: prompted at now-62m, last reply at now-60m.
    fn base_facts() -> TranscriptFacts {
        TranscriptFacts {
            last_prompt_ms: Some(NOW - 62 * M),
            last_turn_open_ms: Some(NOW - 62 * M),
            last_asst_ms: Some(NOW - 60 * M),
            window_floor_ms: Some(NOW - 63 * M),
            whole_file: true,
        }
    }

    /// coord's stamp in the script's cases: a minute after the prompt.
    const AFTER: i64 = NOW - 61 * M;

    fn pr_item(verdict: &str, facts: Value) -> Value {
        serde_json::json!({
            "item": {"repo": "o/r", "pr_number": 7}, "verdict": verdict,
            "attribution": "proven", "facts": facts,
        })
    }

    fn pr_facts(state: &str, confidence: &str, checks: Value) -> Value {
        serde_json::json!({
            "repo": "o/r", "pr_number": 7, "pr_state": state, "head_sha": "abc123",
            "confidence": confidence, "checks": checks,
        })
    }

    fn check(status: &str, conclusion: Option<&str>, coord_gate: bool) -> Value {
        serde_json::json!({"name": "ci", "status": status, "conclusion": conclusion, "coord_gate": coord_gate})
    }

    fn body(prs: Value) -> Value {
        serde_json::json!({"prs": {"coverage": "read", "items": prs}})
    }

    struct Fixture {
        view: Option<WindDownView>,
        custody: Option<CustodyVerdict>,
        obligations: ObligationsVerdict,
        body: Option<Value>,
        transcript: Option<Result<TranscriptFacts, String>>,
        messages: Result<Vec<PendingMessage>, String>,
    }

    impl Fixture {
        /// Every input ok: a finished, idle, clean, watched, unrevived session
        /// with no PR and no undelivered message.
        fn ok() -> Self {
            Self {
                view: Some(eligible_view(Some(AFTER))),
                custody: Some(CustodyVerdict::Clean),
                obligations: ObligationsVerdict::Watched { judged: 0 },
                body: Some(body(serde_json::json!([]))),
                transcript: Some(Ok(base_facts())),
                messages: Ok(Vec::new()),
            }
        }

        fn judge(&self) -> GuardVerdict {
            judge(&GuardInputs {
                session_id: "s-1",
                terminal_id: "t-1",
                eligibility: self.view.as_ref(),
                custody: self.custody.as_ref(),
                obligations: &self.obligations,
                obligations_body: self.body.as_ref(),
                transcript: self.transcript.as_ref(),
                messages: &self.messages,
                now_ms: NOW,
            })
        }

        /// (word, deciding arm, reason).
        fn outcome(&self) -> (&'static str, Option<&'static str>, Option<&'static str>) {
            let v = self.judge();
            (
                v.word(),
                v.reason().map(|r| r.arm),
                v.reason().map(|r| r.reason),
            )
        }
    }

    fn held(
        arm: &'static str,
        reason: &'static str,
    ) -> (&'static str, Option<&'static str>, Option<&'static str>) {
        ("held", Some(arm), Some(reason))
    }

    fn unknown(
        arm: &'static str,
        reason: &'static str,
    ) -> (&'static str, Option<&'static str>, Option<&'static str>) {
        ("unknown", Some(arm), Some(reason))
    }

    const CLOSE: (&str, Option<&str>, Option<&str>) = ("would_mint", None, None);

    #[test]
    fn every_input_ok_is_a_close_token_for_exactly_that_pane() {
        let GuardVerdict::Close(token) = Fixture::ok().judge() else {
            panic!("every input ok must close");
        };
        assert_eq!((token.session_id(), token.terminal_id()), ("s-1", "t-1"));
    }

    // ── Arm 1: eligibility × {ok, held, unknown} ─────────────────────────

    #[test]
    fn eligibility_arm_ok_held_unknown() {
        assert_eq!(Fixture::ok().outcome(), CLOSE);

        let mut f = Fixture::ok();
        let mut v = WindDownView::from_verdict(
            &Eligibility::Ineligible {
                reason: IneligibleReason::LiveChildren,
            },
            SessionKind::Terminal,
        );
        v.finished_at_ms = Some(AFTER);
        f.view = Some(v);
        assert_eq!(f.outcome(), held("eligibility", "live_children"));

        let mut f = Fixture::ok();
        f.view = Some(WindDownView::from_verdict(
            &Eligibility::NotYet {
                since_ms: 0,
                until_ms: 1,
            },
            SessionKind::Terminal,
        ));
        assert_eq!(f.outcome(), held("eligibility", "not_yet"));

        let mut f = Fixture::ok();
        f.view = Some(eligible_view(Some(AFTER)));
        f.view.as_mut().unwrap().kind = SessionKind::Looping;
        assert_eq!(f.outcome(), held("eligibility", "not_a_terminal_session"));

        let mut f = Fixture::ok();
        f.view = Some(WindDownView::from_verdict(
            &Eligibility::Unknown {
                reason: UnknownReason::ChildrenUnknown,
            },
            SessionKind::Terminal,
        ));
        assert_eq!(f.outcome(), unknown("eligibility", "children_unknown"));

        let mut f = Fixture::ok();
        f.view = Some(WindDownView::from_verdict(
            &Eligibility::Unknown {
                reason: UnknownReason::WorkStatusUnknown,
            },
            SessionKind::Terminal,
        ));
        assert_eq!(f.outcome(), unknown("eligibility", "work_status_unknown"));

        let mut f = Fixture::ok();
        f.view = None;
        assert_eq!(f.outcome(), unknown("eligibility", "session_not_observed"));
    }

    // ── Arm 2: custody ───────────────────────────────────────────────────

    #[test]
    fn custody_arm_ok_held_unknown() {
        assert_eq!(Fixture::ok().outcome(), CLOSE);
        let mut f = Fixture::ok();
        f.custody = Some(CustodyVerdict::Dirty(CustodyDirty::Unpushed {
            path: "/ws/a".to_string(),
            ahead: 2,
        }));
        assert_eq!(f.outcome(), held("custody", "unpushed"));
        let mut f = Fixture::ok();
        f.custody = Some(CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir));
        assert_eq!(f.outcome(), unknown("custody", "no_session_dir"));
        let mut f = Fixture::ok();
        f.custody = None;
        assert_eq!(f.outcome(), unknown("custody", "custody_not_read"));
    }

    // ── Arm 3: obligations ───────────────────────────────────────────────

    #[test]
    fn obligations_arm_ok_held_unknown() {
        assert_eq!(Fixture::ok().outcome(), CLOSE);
        let mut f = Fixture::ok();
        f.obligations = ObligationsVerdict::Unwatched(ObligationsUnwatched {
            source: "gates".to_string(),
            item: "g".to_string(),
            reasons: String::new(),
        });
        assert_eq!(f.outcome(), held("obligations", "not_watched"));
        // Phase 0's 5472e347: not_watched items whose attribution is uncertain.
        let mut f = Fixture::ok();
        f.obligations = ObligationsVerdict::Unknown(ObligationsUnknown::ItemUnknown {
            source: "gates".to_string(),
            item: "g".to_string(),
            verdict: "not_watched".to_string(),
            uncertain: true,
        });
        assert_eq!(f.outcome(), unknown("obligations", "item_unknown"));
        let mut f = Fixture::ok();
        f.obligations = ObligationsVerdict::unreadable("503");
        assert_eq!(
            f.outcome(),
            unknown("obligations", "obligations_unreadable")
        );
    }

    // ── Arm 4: first verdict (D5) ────────────────────────────────────────

    #[test]
    fn first_verdict_arm_ok_held_unknown() {
        let with = |prs: Value| {
            let mut f = Fixture::ok();
            f.body = Some(body(prs));
            f.outcome()
        };
        // ok: every CI check completed and green; one failed; a settled PR;
        // a branch-only item; the coord gate ignored.
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "fresh",
                    serde_json::json!([
                        check("completed", Some("success"), false),
                        check("completed", Some("skipped"), false),
                        check("in_progress", None, true),
                    ])
                )
            )])),
            CLOSE
        );
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "fresh",
                    serde_json::json!([
                        check("completed", Some("failure"), false),
                        check("queued", None, false),
                    ])
                )
            )])),
            CLOSE,
            "one failed check is a first verdict"
        );
        assert_eq!(
            with(serde_json::json!([pr_item("landed", Value::Null)])),
            CLOSE
        );
        assert_eq!(
            with(serde_json::json!([pr_item("retired", Value::Null)])),
            CLOSE
        );
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts("MERGED", "unknown", serde_json::json!([]))
            )])),
            CLOSE,
            "a merged PR has no verdict left to wait for"
        );
        assert_eq!(
            with(
                serde_json::json!([{"item": {"repo": "o/r", "branch": "b", "pr_number": null},
                                      "verdict": "watched", "facts": null}])
            ),
            CLOSE
        );

        // unknown: a prs item with no `item.pr_number` key is not branch-only.
        assert_eq!(
            with(serde_json::json!([{"item": {"repo": "o/r", "branch": "b"},
                                      "verdict": "watched", "facts": null}])),
            unknown("first_verdict", "pr_item_unreadable")
        );
        assert_eq!(
            with(serde_json::json!([{"verdict": "watched", "facts": null}])),
            unknown("first_verdict", "pr_item_unreadable")
        );

        // held: CI still running, or no CI check at all yet.
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "fresh",
                    serde_json::json!([
                        check("completed", Some("success"), false),
                        check("in_progress", None, false),
                    ])
                )
            )])),
            held("first_verdict", "first_verdict_pending")
        );
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "fresh",
                    serde_json::json!([check("completed", Some("success"), true),])
                )
            )])),
            held("first_verdict", "first_verdict_pending"),
            "only the coord gate has reported: no CI verdict"
        );

        // unknown: stale card, no facts, no head, a completed check with no
        // conclusion, and no body at all.
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "stale",
                    serde_json::json!([check("completed", Some("success"), false),])
                )
            )])),
            unknown("first_verdict", "pr_facts_not_fresh")
        );
        assert_eq!(
            with(serde_json::json!([pr_item("watched", Value::Null)])),
            unknown("first_verdict", "pr_facts_absent")
        );
        let mut headless = pr_facts("open", "fresh", serde_json::json!([]));
        headless["head_sha"] = Value::Null;
        assert_eq!(
            with(serde_json::json!([pr_item("watched", headless)])),
            unknown("first_verdict", "pr_head_unknown")
        );
        assert_eq!(
            with(serde_json::json!([pr_item(
                "watched",
                pr_facts(
                    "open",
                    "fresh",
                    serde_json::json!([check("completed", None, false),])
                )
            )])),
            unknown("first_verdict", "check_conclusion_unknown")
        );
        let mut f = Fixture::ok();
        f.body = Some(serde_json::json!({"gates": {"items": []}}));
        assert_eq!(f.outcome(), unknown("first_verdict", "prs_unreadable"));
        let mut f = Fixture::ok();
        f.body = None;
        assert_eq!(
            f.outcome(),
            unknown("first_verdict", "obligations_unreadable")
        );
    }

    // ── Arm 5: revival (D6) — the script's own finish_state cases ────────

    /// Translated from `close-finished-sessions.py`'s `stranded_cases` and the
    /// N5/N8 wake cases (coord-stamped arm): `now = NOW`, prompt at now-62m,
    /// coord's stamp `AFTER` = now-61m, last reply now-60m.
    #[test]
    fn the_revival_rule_matches_finish_states_fixture_table() {
        let worked = TranscriptFacts {
            last_asst_ms: Some(NOW - 30 * M),
            ..base_facts()
        };
        let woke = |offset: i64| TranscriptFacts {
            last_turn_open_ms: Some(AFTER + offset),
            last_asst_ms: Some(AFTER + offset + 30_000),
            ..base_facts()
        };
        let prompted = TranscriptFacts {
            last_prompt_ms: Some(AFTER + 3 * M),
            last_turn_open_ms: Some(AFTER + 3 * M),
            last_asst_ms: Some(AFTER + 8 * M),
            ..base_facts()
        };
        let lp = NOW - 62 * M;
        type Want = (&'static str, Option<&'static str>);
        let cases: Vec<(&str, Option<i64>, TranscriptFacts, Want)> = vec![
            (
                "({}, cf, base) FINISHED",
                Some(AFTER),
                base_facts(),
                ("ok", None),
            ),
            (
                "({}, cf, worked) kept_working",
                Some(AFTER),
                worked,
                ("held", Some("kept_working")),
            ),
            (
                "stamp lp+500: in_band",
                Some(lp + 500),
                base_facts(),
                ("held", Some("in_band")),
            ),
            (
                "stamp lp-5000: revived by the prompt",
                Some(lp - 5000),
                base_facts(),
                ("held", Some("revived")),
            ),
            (
                "coord finished with no time",
                None,
                base_facts(),
                ("unknown", Some("untimed")),
            ),
            (
                "N5: a scheduled wake 5 min after",
                Some(AFTER),
                woke(5 * M),
                ("held", Some("revived")),
            ),
            (
                "N5: a scheduled wake 40 min after",
                Some(AFTER),
                woke(40 * M),
                ("held", Some("revived")),
            ),
            (
                "N8: a Session Bus delivery 3 min after",
                Some(AFTER),
                woke(3 * M),
                ("held", Some("revived")),
            ),
            (
                "prompted 3 min after",
                Some(AFTER),
                prompted,
                ("held", Some("revived")),
            ),
            (
                "a turn exactly 1 s after the stamp is in the band",
                Some(lp - 1000),
                base_facts(),
                ("held", Some("in_band")),
            ),
            (
                "a stamp ahead of this clock",
                Some(NOW + M),
                base_facts(),
                ("unknown", Some("untimed")),
            ),
        ];
        for (name, stamp, facts, want) in cases {
            let got = revival_arm(stamp, Some(&Ok(facts)), NOW);
            assert_eq!((got.word(), got.reason()), want, "{name}");
        }
    }

    #[test]
    fn revival_arm_ok_held_unknown_through_the_guard() {
        assert_eq!(Fixture::ok().outcome(), CLOSE);
        let mut f = Fixture::ok();
        f.transcript = Some(Ok(TranscriptFacts {
            last_prompt_ms: Some(AFTER + 10 * M),
            ..base_facts()
        }));
        assert_eq!(f.outcome(), held("revival", "revived"));
        let mut f = Fixture::ok();
        f.transcript = Some(Err("no transcript".to_string()));
        assert_eq!(f.outcome(), unknown("revival", "transcript_unreadable"));
        let mut f = Fixture::ok();
        f.transcript = None;
        assert_eq!(f.outcome(), unknown("revival", "transcript_not_read"));
        // The stamp is the per-close re-check's: a view with none is untimed.
        let mut f = Fixture::ok();
        f.view = Some(eligible_view(None));
        assert_eq!(f.outcome(), unknown("revival", "untimed"));
        // A tail that does not reach back to the stamp cannot rule a turn out.
        let mut f = Fixture::ok();
        f.transcript = Some(Ok(TranscriptFacts {
            window_floor_ms: Some(AFTER + 5000),
            whole_file: false,
            last_prompt_ms: None,
            last_turn_open_ms: None,
            ..base_facts()
        }));
        assert_eq!(f.outcome(), unknown("revival", "transcript_window_short"));
    }

    /// Phase 0's d30f7c5f, end to end through the transcript reader: a
    /// compaction summary written 3284 s after the finish is not a revival.
    #[test]
    fn an_idle_compaction_summary_after_the_stamp_is_not_a_revival() {
        let iso = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let stamp = NOW - 120 * M;
        let tail = [
            serde_json::json!({"type": "user", "timestamp": iso(stamp - 60_000),
                               "message": {"content": "close out"}}),
            serde_json::json!({"type": "assistant", "timestamp": iso(stamp - 2_000),
                               "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "done"}]}}),
            serde_json::json!({"type": "user", "timestamp": iso(stamp + 3_284_000), "isCompactSummary": true,
                               "message": {"content": "This session is being continued from a previous conversation."}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let facts = super::super::transcript_facts::facts_from_tail(&tail, true);
        let got = revival_arm(Some(stamp), Some(&Ok(facts)), NOW);
        assert_eq!(got, ArmVerdict::Ok, "{got:?}");
    }

    // ── Arm 6: undelivered messages ──────────────────────────────────────

    fn msg(to: &str, owner: Option<&str>) -> PendingMessage {
        PendingMessage {
            message_id: format!("m-{to}"),
            to_session: to.to_string(),
            owner: owner.map(str::to_string),
        }
    }

    #[test]
    fn messages_arm_ok_held_unknown() {
        assert_eq!(Fixture::ok().outcome(), CLOSE);
        let mut f = Fixture::ok();
        f.messages = Ok(vec![msg("s-2", Some("s-2"))]);
        assert_eq!(
            f.outcome(),
            CLOSE,
            "another session's message does not hold this one"
        );

        let mut f = Fixture::ok();
        f.messages = Ok(vec![msg("S-1", None)]);
        assert_eq!(
            f.outcome(),
            held("undelivered_messages", "undelivered_message")
        );
        let mut f = Fixture::ok();
        f.messages = Ok(vec![msg("coord-uuid", Some("s-1"))]);
        assert_eq!(
            f.outcome(),
            held("undelivered_messages", "undelivered_message")
        );

        let mut f = Fixture::ok();
        f.messages = Err("coord returned 503".to_string());
        assert_eq!(
            f.outcome(),
            unknown("undelivered_messages", "undelivered_messages_unreadable")
        );
        let mut f = Fixture::ok();
        f.messages = Ok(vec![msg("who-knows", None)]);
        assert_eq!(
            f.outcome(),
            unknown(
                "undelivered_messages",
                "undelivered_messages_unattributable"
            )
        );
        let mut f = Fixture::ok();
        f.messages = Ok((0..PENDING_PAGE_LIMIT)
            .map(|i| msg(&format!("s-other-{i}"), Some("s-other")))
            .collect());
        assert_eq!(
            f.outcome(),
            unknown("undelivered_messages", "undelivered_messages_page_full")
        );
    }

    // ── Order and reporting ──────────────────────────────────────────────

    /// The first non-ok arm decides, and every arm is still evaluated for the
    /// report.
    #[test]
    fn the_first_failing_arm_decides_and_every_arm_is_reported() {
        let mut f = Fixture::ok();
        f.custody = Some(CustodyVerdict::Unknown(CustodyUnknown::NoSessionDir));
        f.messages = Ok(vec![msg("s-1", None)]);
        let inputs = GuardInputs {
            session_id: "s-1",
            terminal_id: "t-1",
            eligibility: f.view.as_ref(),
            custody: f.custody.as_ref(),
            obligations: &f.obligations,
            obligations_body: f.body.as_ref(),
            transcript: f.transcript.as_ref(),
            messages: &f.messages,
            now_ms: NOW,
        };
        let (verdict, arms) = judge_with_arms(&inputs);
        assert_eq!(verdict.reason().map(|r| r.arm), Some("custody"));
        assert_eq!(
            arms.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            ARMS.to_vec()
        );
        assert_eq!(
            describe_arms(&arms),
            "eligibility=ok custody=unknown(no_session_dir) obligations=ok first_verdict=ok \
             revival=ok undelivered_messages=held(undelivered_message)"
        );
        let v = view(&verdict, &arms, 42);
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["verdict"], "unknown");
        assert_eq!(json["decidedBy"], "custody");
        assert_eq!(json["reason"], "no_session_dir");
        assert_eq!(json["arms"][5]["verdict"], "held");
        assert_eq!(json["judgedAt"], 42);
        let close = view(&Fixture::ok().judge(), &arms, 1);
        assert_eq!(close.verdict, "would_mint");
        assert!(close.decided_by.is_none());
    }
}
