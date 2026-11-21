#![cfg(test)]

use super::*;
use serde_json::json;

const BLOCK: &str = r#"<!-- POLICY_COMPLIANCE v1 -->
```json
{ "schema": "policy-compliance/1",
  "items": [{"ref":"A1","state":"landed","evidence":{"pr":"qontinui/qontinui-runner#1"}}],
  "clauses_applied": [{"id":"finish-to-zero","for":"A1"}],
  "policy_gaps": [], "escalations": [], "declined": [] }
```
"#;

fn assistant_line(texts: &[&str]) -> String {
    let content: Vec<Value> = texts
        .iter()
        .map(|t| json!({"type": "text", "text": t}))
        .collect();
    json!({
        "type": "assistant",
        "uuid": "u1",
        "timestamp": "2026-07-30T12:00:00Z",
        "message": {"model": "claude-opus-5", "content": content}
    })
    .to_string()
}

// ── extract_compliance_block ─────────────────────────────────────────

#[test]
fn extracts_a_fenced_block() {
    let v = extract_compliance_block(BLOCK).expect("block found");
    assert_eq!(v["schema"], SCHEMA);
    assert_eq!(v["items"][0]["ref"], "A1");
}

#[test]
fn extracts_an_unfenced_block() {
    let text =
        "<!-- POLICY_COMPLIANCE v1 -->\n{\"schema\":\"policy-compliance/1\",\"items\":[]}";
    assert!(extract_compliance_block(text).is_some());
}

#[test]
fn prose_alone_is_not_a_pass() {
    // The single most important negative: a session can print the words
    // having read nothing. That must not satisfy the check.
    let text = "I emitted the POLICY_COMPLIANCE footer and everything reconciled. \
                    All items landed. schema policy-compliance/1.";
    assert!(extract_compliance_block(text).is_none());
}

#[test]
fn an_inline_mention_cannot_locate_a_later_block() {
    // The opener must START a line. Prose naming the marker mid-sentence
    // must not reach forward and adopt an unrelated JSON object.
    let text = "As promised I wrote the <!-- POLICY_COMPLIANCE v1 --> footer.\n\
                    Here is some config: {\"schema\":\"policy-compliance/1\",\"items\":[]}";
    assert!(extract_compliance_block(text).is_none());
}

#[test]
fn an_indented_opener_still_matches() {
    let text = "   <!-- POLICY_COMPLIANCE v1 -->\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"A\"}]}";
    assert!(extract_compliance_block(text).is_some());
}

#[test]
fn a_stray_brace_in_the_opener_does_not_lose_the_block() {
    let text = "<!-- POLICY_COMPLIANCE v1 {see plan} -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"A\"}]}\n```";
    let v = extract_compliance_block(text).expect("block found past the stray brace");
    assert_eq!(v["items"][0]["ref"], "A");
}

#[test]
fn malformed_json_is_not_a_pass() {
    let text =
        "<!-- POLICY_COMPLIANCE v1 -->\n```json\n{ \"schema\": \"policy-compliance/1\", \
                    \"items\": [ {\"ref\": }\n```";
    assert!(extract_compliance_block(text).is_none());
}

#[test]
fn wrong_schema_string_is_not_a_pass() {
    let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/2\",\"items\":[]}\n```";
    assert!(extract_compliance_block(text).is_none());
    let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n{\"items\":[]}\n```";
    assert!(extract_compliance_block(text).is_none());
}

#[test]
fn items_must_be_an_array() {
    let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\",\"items\":{}}\n```";
    assert!(extract_compliance_block(text).is_none());
    let text = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                    {\"schema\":\"policy-compliance/1\"}\n```";
    assert!(extract_compliance_block(text).is_none());
}

#[test]
fn multiple_blocks_last_wins() {
    let first = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                     {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"first\"}]}\n```";
    let second = "<!-- POLICY_COMPLIANCE v1 -->\n```json\n\
                      {\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"second\"}]}\n```";
    let text = format!("{first}\n\nsome prose\n\n{second}");
    let v = extract_compliance_block(&text).expect("block found");
    assert_eq!(v["items"][0]["ref"], "second");
}

#[test]
fn a_later_prose_mention_does_not_invalidate_an_earlier_real_block() {
    let text = format!("{BLOCK}\n\nAs shown above, the POLICY_COMPLIANCE report is done.");
    let v = extract_compliance_block(&text).expect("block found");
    assert_eq!(v["items"][0]["ref"], "A1");
}

// ── scan_transcript ──────────────────────────────────────────────────

#[test]
fn scans_a_block_in_a_single_content_block() {
    let jsonl = format!(
        "{}\n{}\n",
        assistant_line(&["Working on it."]),
        assistant_line(&[BLOCK])
    );
    assert!(scan_transcript(&jsonl).is_some());
}

#[test]
fn scans_a_block_split_across_content_blocks() {
    // The recon hazard: Claude can emit the marker + fence in one text
    // block and the payload in the next. Joining is what makes it match.
    let head = "<!-- POLICY_COMPLIANCE v1 -->\n```json";
    let tail = "{\"schema\":\"policy-compliance/1\",\"items\":[{\"ref\":\"split\"}]}\n```";
    let jsonl = format!("{}\n", assistant_line(&[head, tail]));
    let v = scan_transcript(&jsonl).expect("block found across content blocks");
    assert_eq!(v["items"][0]["ref"], "split");
}

#[test]
fn scans_a_block_split_across_messages() {
    let head = "<!-- POLICY_COMPLIANCE v1 -->\n```json";
    let tail = "{\"schema\":\"policy-compliance/1\",\"items\":[]}\n```";
    let jsonl = format!("{}\n{}\n", assistant_line(&[head]), assistant_line(&[tail]));
    assert!(scan_transcript(&jsonl).is_some());
}

#[test]
fn user_records_and_tool_blocks_are_ignored() {
    // A user PASTING the block must not count as the assistant emitting it.
    let user = json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": BLOCK}]}
    })
    .to_string();
    assert!(scan_transcript(&user).is_none());

    let tool_only = json!({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"cmd": BLOCK}}]}
    })
    .to_string();
    assert!(scan_transcript(&tool_only).is_none());
}

#[test]
fn malformed_and_partial_lines_are_skipped_not_fatal() {
    let jsonl = format!(
        "{{ this is not json\n\n{}\n{{\"type\":\"assistant\"}}\n",
        assistant_line(&[BLOCK])
    );
    assert!(scan_transcript(&jsonl).is_some());
}

#[test]
fn an_empty_transcript_yields_no_block() {
    assert!(scan_transcript("").is_none());
    assert!(scan_transcript("\n\n  \n").is_none());
}

// ── ComplianceConfig / Applicability ─────────────────────────────────

#[test]
fn applicable_config_enforces() {
    let c = ComplianceConfig::from_body(&json!({
        "enabled": true, "mode": "nudge", "max_attempts": 1,
        "enforced_clause_ref": "policy/session-protocol#finish-to-zero",
        "applicable": true, "applicability_reason": "applicable",
        "clause_resolved_via": "clause_id", "prompt_document_version": 4
    }))
    .expect("parsed");
    assert_eq!(c.applicability(), Applicability::Enforce);
    assert!(c.applicability().emits());
    assert!(c.applicability().nudge_allowed(&c.mode));
}

#[test]
fn enforcement_disabled_still_emits_but_never_nudges() {
    // Report-only by default is the whole point of Phase 1: the verdict
    // emit is how the false-positive rate gets measured.
    let c = ComplianceConfig::from_body(&json!({
        "enabled": false, "mode": "nudge", "applicable": false,
        "applicability_reason": "enforcement_disabled"
    }))
    .expect("parsed");
    assert_eq!(c.applicability(), Applicability::ReportOnly);
    assert!(c.applicability().emits());
    assert!(!c.applicability().nudge_allowed(&c.mode));
}

#[test]
fn clause_absent_or_document_missing_is_fully_inert() {
    for reason in ["clause_absent", "document_missing"] {
        let c = ComplianceConfig::from_body(&json!({
            "enabled": true, "mode": "nudge", "applicable": false,
            "applicability_reason": reason
        }))
        .expect("parsed");
        assert_eq!(c.applicability(), Applicability::Inert, "reason {reason}");
        assert!(!c.applicability().emits(), "reason {reason}");
    }
}

#[test]
fn reopen_mode_never_nudges() {
    // A spawn that outlives the request needs a standing per-path opt-in
    // that does not exist — so `reopen` is inert on the nudge arm.
    let c = ComplianceConfig::from_body(&json!({
        "enabled": true, "mode": "reopen", "applicable": true,
        "applicability_reason": "applicable"
    }))
    .expect("parsed");
    assert_eq!(c.applicability(), Applicability::Enforce);
    assert!(c.applicability().emits());
    assert!(!c.applicability().nudge_allowed(&c.mode));
}

#[test]
fn an_unknown_reason_word_never_escalates_to_enforce() {
    let c = ComplianceConfig::from_body(&json!({
        "enabled": true, "mode": "nudge", "applicable": true,
        "applicability_reason": "some_future_reason"
    }))
    .expect("parsed");
    assert_eq!(c.applicability(), Applicability::ReportOnly);
    let c = ComplianceConfig::from_body(&json!({"applicability_reason": "some_future_reason"}))
        .expect("parsed");
    assert_eq!(c.applicability(), Applicability::Inert);
}

#[test]
fn an_empty_config_body_is_inert() {
    let c = ComplianceConfig::from_body(&json!({})).expect("parsed");
    assert!(!c.enabled);
    assert_eq!(c.applicability(), Applicability::Inert);
    assert!(ComplianceConfig::from_body(&json!("nope")).is_none());
    assert!(ComplianceConfig::from_body(&Value::Null).is_none());
}

// ── ComplianceVerdict / footprint ────────────────────────────────────

#[test]
fn verdict_parses_footprint_strings_and_objects() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "reconciliation": {},
        "footprint": {
            "prs": ["qontinui/qontinui-runner#42", {"ref": "qontinui/qontinui-web#7"}],
            "commits": [{"sha": "abc1234"}],
            "claims": []
        }
    }));
    assert_eq!(v.verdict, "unverified");
    assert_eq!(
        v.footprint_prs,
        vec!["qontinui/qontinui-runner#42", "qontinui/qontinui-web#7"]
    );
    assert_eq!(v.footprint_commits, vec!["abc1234"]);
    assert!(!v.footprint_is_empty());
}

#[test]
fn an_absent_footprint_reads_empty() {
    let v = ComplianceVerdict::from_body(&json!({"verdict": "unverified"}));
    assert!(v.footprint_is_empty());
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified",
        "footprint": {"prs": [], "commits": [], "claims": []}
    }));
    assert!(v.footprint_is_empty());
}

/// §A4 condition 3: CLAIMS ALONE MUST NOT ARM THE NUDGE.
///
/// coord builds `footprint.claims` from `claims_audit` rows of kind
/// `symbol`/`file_glob`, and the fleet's `/preflight` protocol acquires
/// file-glob claims before the first line of code is written. If claims
/// counted, this footprint would be non-empty at turn 1 of nearly every
/// session — and since an unfinished session's verdict is `unverified`
/// with `reason: "absent"` by construction, the nudge would fire at its
/// FIRST pause. That is the per-turn nag condition 3 exists to prevent.
#[test]
fn claims_alone_do_not_arm_the_nudge() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "footprint": {
            "prs": [], "commits": [],
            "claims": ["src/**/*.rs", "qontinui-runner:src/mcp/*"]
        }
    }));
    assert!(!v.footprint_claims.is_empty(), "claims still parsed");
    assert!(
        v.footprint_is_empty(),
        "claims measure work STARTED, not work DONE — they must not arm the nudge"
    );

    // A single PR or commit DOES arm it: a report is genuinely owed.
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
    }));
    assert!(!v.footprint_is_empty());
}

/// A commit object carrying a non-null `pr_number` — the normal case for a
/// commit pushed on a PR branch — must render as its SHA, not as a PR ref.
/// The session being nudged needs the sha to fill `evidence.sha`.
#[test]
fn a_commit_with_a_pr_number_renders_its_sha() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "footprint": {
            "prs": [],
            "commits": [{
                "sha": "abc1234", "repo": "qontinui/qontinui-runner",
                "branch": "feat/x", "pr_number": 923,
                "recorded_at": "2026-07-30T00:00:00Z"
            }],
            "claims": []
        }
    }));
    assert_eq!(
        v.footprint_commits,
        vec!["abc1234"],
        "a commit must not render as owner/repo#N"
    );
}

/// A block that WAS emitted but failed reconciliation must not be told it
/// emitted nothing — the nudge would contradict itself two lines later.
#[test]
fn the_nudge_does_not_claim_an_emitted_block_is_missing() {
    let emitted = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified",
        "reason": "2 of 5 item(s) could not be reconciled: A1, B2",
        "reconciliation": {},
        "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
    }));
    let text = nudge_text(&emitted);
    assert!(
        !text.contains("You did not emit"),
        "must not assert absence about a block that was emitted: {text}"
    );
    assert!(text.contains("did not reconcile"));

    let absent = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
    }));
    assert!(nudge_text(&absent).contains("You did not emit"));
}

// ── nudge text ───────────────────────────────────────────────────────

#[test]
fn nudge_carries_the_observed_footprint() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "absent",
        "footprint": {"prs": ["o/r#1"], "commits": ["deadbee"], "claims": []}
    }));
    let text = nudge_text(&v);
    assert!(text.contains("policy/session-protocol` v4 Step 3"));
    assert!(text.contains("PRs `o/r#1`"));
    assert!(text.contains("commits `deadbee`"));
    assert!(text.contains("claims `none`"));
    assert!(text.contains("reconciled against those observations"));
    // `absent` is the plain missing-block case — no contradiction sentence.
    assert!(!text.contains("failed to reconcile"));
}

#[test]
fn nudge_names_contradicted_claims_when_coord_supplies_them() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "contradicted",
        "reconciliation": {"unreconciled": [{"ref": "A1"}, "B2"]},
        "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
    }));
    let text = nudge_text(&v);
    assert!(text.contains("failed to reconcile"));
    assert!(text.contains("A1"));
    assert!(text.contains("B2"));
}

#[test]
fn nudge_falls_back_to_coords_reason_when_the_payload_is_unfamiliar() {
    let v = ComplianceVerdict::from_body(&json!({
        "verdict": "unverified", "reason": "contradicted",
        "reconciliation": {"some": "future shape"},
        "footprint": {"prs": ["o/r#1"], "commits": [], "claims": []}
    }));
    let text = nudge_text(&v);
    assert!(text.contains("could not reconcile every claim"));
    assert!(text.contains("contradicted"));
}

#[test]
fn unreconciled_refs_reads_the_shapes_it_knows_and_no_others() {
    assert_eq!(
        unreconciled_refs(&json!({"unreconciled": ["A", {"ref": "B"}]})),
        vec!["A", "B"]
    );
    assert_eq!(
        unreconciled_refs(&json!({"items": [
            {"ref": "A", "reconciled": true},
            {"ref": "B", "reconciled": false},
            {"ref": "C", "status": "reconciled"},
            {"ref": "D", "status": "contradicted"}
        ]})),
        vec!["B", "D"]
    );
    assert!(unreconciled_refs(&json!({"unknown": 1})).is_empty());
    assert!(unreconciled_refs(&Value::Null).is_empty());
}

// ── counted per-session cap ──────────────────────────────────────────
//
// `NUDGED` is process-global across the whole test binary, so every test
// below uses a session key unique to itself.

#[test]
fn mark_nudged_is_a_counted_compare_and_set() {
    let a = "session-compliance-test-marker-a";
    let b = "session-compliance-test-marker-b";
    let c = NudgeClass::Report;
    assert_eq!(nudge_attempts_for(a, c), 0);
    assert!(mark_nudged(a, c, 1), "first claim wins");
    assert!(
        !mark_nudged(a, c, 1),
        "second claim must lose at max_attempts=1"
    );
    assert_eq!(nudge_attempts_for(a, c), 1);
    assert_eq!(nudge_attempts_for(b, c), 0, "the cap is per session");
}

/// The headline behaviour of the change: the cap is the OPERATOR's number.
/// Under the old set-based guard this test was unwritable — delivery
/// stopped at one no matter what `max_attempts` said.
#[test]
fn a_higher_cap_allows_exactly_that_many() {
    let s = "session-compliance-test-cap-three";
    let c = NudgeClass::Report;
    for i in 1..=3 {
        assert!(mark_nudged(s, c, 3), "attempt {i} of 3 must be granted");
        assert_eq!(nudge_attempts_for(s, c), i);
    }
    assert!(!mark_nudged(s, c, 3), "the fourth is refused");
    assert_eq!(nudge_attempts_for(s, c), 3, "and does not increment");
}

/// `max_attempts: 0` means never nudge — a setting the old cap could not
/// express at all.
#[test]
fn a_zero_cap_never_nudges() {
    let s = "session-compliance-test-cap-zero";
    for c in NudgeClass::ALL {
        assert!(!mark_nudged(s, c, 0));
        assert_eq!(nudge_attempts_for(s, c), 0);
    }
    assert!(
        !any_class_under_cap(s, 0),
        "a zero cap leaves no class eligible, so the pre-coord screen              short-circuits before the verdict is even fetched"
    );
}

/// The entire backward-compatibility argument for this change: an
/// untouched config still nudges exactly once. Without this, flipping the
/// `unwrap_or` would triple the fleet's nudge rate with a green suite.
#[test]
fn an_absent_max_attempts_defaults_to_one() {
    assert_eq!(
        ComplianceConfig::from_body(&json!({}))
            .expect("parsed")
            .max_attempts,
        1
    );
    assert_eq!(
        ComplianceConfig::from_body(&json!({"max_attempts": null}))
            .expect("parsed")
            .max_attempts,
        1
    );
    assert_eq!(
        ComplianceConfig::from_body(&json!({"max_attempts": "3"}))
            .expect("parsed")
            .max_attempts,
        1,
        "a non-numeric value is not a cap"
    );
}

#[test]
fn an_absurd_max_attempts_is_clamped() {
    assert_eq!(
        ComplianceConfig::from_body(&json!({"max_attempts": 999_999}))
            .expect("parsed")
            .max_attempts,
        MAX_NUDGE_ATTEMPTS_CEILING as u32
    );
}

/// The timestamp goes on the wire, so its SHAPE is part of the contract.
/// `to_rfc3339()` would emit 0/3/6/9 fractional digits depending on the
/// clock reading; the 9-digit form is rejected by some RFC3339 parsers and
/// rounded by Postgres.
#[test]
fn the_nudge_timestamp_is_fixed_width_utc() {
    let s = "session-compliance-test-stamp";
    assert!(mark_nudged(s, NudgeClass::Report, 1));
    let (attempts, last_at) = nudge_state_for(s).expect("a nudged session has state");
    assert_eq!(attempts, 1);
    let at = last_at.expect("a delivered nudge stamps its time");
    assert!(at.ends_with('Z'), "UTC as Z, not +00:00: {at}");
    assert_eq!(at.len(), 24, "fixed-width milliseconds: {at}");
    assert!(chrono::DateTime::parse_from_rfc3339(&at).is_ok());
}

/// The restart bug, pinned at the seam it actually broke.
///
/// A session this run never saw reads `None`, and `None` must OMIT the
/// fields — not send `0`. Coord's store is last-write-wins, so a
/// fabricated zero at session close would erase a real count recorded
/// before the restart.
#[test]
fn an_unknown_session_omits_the_nudge_fields_rather_than_sending_zero() {
    assert!(nudge_state_for("session-compliance-test-never-seen").is_none());

    let body = compliance_body(None, Some(ABSENT_NO_BLOCK), None);
    assert!(
        body.get("nudge_attempts").is_none(),
        "a fabricated 0 would overwrite coord's real count: {body}"
    );
    assert!(body.get("last_nudged_at").is_none());
    // The report half of the post is unaffected.
    assert_eq!(body["absent_reason"], json!(ABSENT_NO_BLOCK));

    let carried = compliance_body(
        None,
        None,
        Some((2, Some("2026-08-08T10:00:00.000Z".into()))),
    );
    assert_eq!(carried["nudge_attempts"], json!(2));
    assert_eq!(carried["last_nudged_at"], json!("2026-08-08T10:00:00.000Z"));

    // A recorded count with no stamp still carries the count.
    let stampless = compliance_body(None, None, Some((1, None)));
    assert_eq!(stampless["nudge_attempts"], json!(1));
    assert!(stampless.get("last_nudged_at").is_none());
}

// ── per-CLASS keying (plan `2026-08-08-runner-enforced-policy-pull`
//    Phase 3) ─────────────────────────────────────────────────────────

/// THE regression this whole sub-change exists to prevent.
///
/// `NUDGED` used to be keyed on `session_id` ALONE. With a second nudge
/// class that makes the two MUTUALLY EXCLUSIVE per session — whichever
/// fires first permanently suppresses the other, so a session that got a
/// report-enforcement nudge could never be told it skipped the policy pull.
/// A verification arm silently swallowed by an unrelated arm is worse than
/// no arm, because the dashboard reads clean.
#[test]
fn one_classs_marker_does_not_suppress_the_other() {
    let s = "session-compliance-test-class-independence";

    assert!(mark_nudged(s, NudgeClass::Report, 1), "report claim wins");
    assert!(
        !mark_nudged(s, NudgeClass::Report, 1),
        "the report class is now at its cap"
    );

    // ...and the policy-pull class is untouched. Under the old key this
    // assertion failed: the session was simply "already nudged".
    assert_eq!(nudge_attempts_for(s, NudgeClass::PolicyPull), 0);
    assert!(
        mark_nudged(s, NudgeClass::PolicyPull, 1),
        "an exhausted report budget must not consume the policy-pull one"
    );
    assert_eq!(nudge_attempts_for(s, NudgeClass::PolicyPull), 1);
    assert_eq!(
        nudge_attempts_for(s, NudgeClass::Report),
        1,
        "and the policy-pull delivery did not touch the report counter"
    );
}

/// The pre-coord screen must stay open while ANY class has budget left,
/// or an exhausted class would suppress the other one turn earlier — the
/// same bug moved upstream of the marker.
#[test]
fn the_pre_coord_screen_stays_open_while_any_class_has_budget() {
    let s = "session-compliance-test-any-class";
    assert!(any_class_under_cap(s, 1));
    assert!(mark_nudged(s, NudgeClass::PolicyPull, 1));
    assert!(
        any_class_under_cap(s, 1),
        "one class spent, the other still due"
    );
    assert!(mark_nudged(s, NudgeClass::Report, 1));
    assert!(!any_class_under_cap(s, 1), "both classes spent");
}

/// The configured cap applies PER CLASS, and `max_attempts: 0` still means
/// never — for every class.
#[test]
fn the_configured_cap_applies_to_each_class_independently() {
    let s = "session-compliance-test-per-class-cap";
    for c in NudgeClass::ALL {
        for i in 1..=2 {
            assert!(mark_nudged(s, c, 2), "{} attempt {i} of 2", c.as_str());
        }
        assert!(!mark_nudged(s, c, 2), "{} is capped at 2", c.as_str());
        assert_eq!(nudge_attempts_for(s, c), 2);
    }
}

/// Coord stores ONE count per session, so the classes are SUMMED on the
/// wire — and the `None`-vs-`0` distinction survives that summing. `None`
/// still means "this run has no record of the session", which must OMIT the
/// fields rather than send a fabricated `0` into a last-write-wins store.
#[test]
fn the_reported_count_sums_the_classes_without_collapsing_none_to_zero() {
    let s = "session-compliance-test-sum";
    assert!(
        nudge_state_for(s).is_none(),
        "no record for either class is None, not Some((0, None))"
    );

    assert!(mark_nudged(s, NudgeClass::Report, 1));
    assert_eq!(nudge_state_for(s).expect("recorded").0, 1);

    assert!(mark_nudged(s, NudgeClass::PolicyPull, 1));
    let (attempts, last_at) = nudge_state_for(s).expect("recorded");
    assert_eq!(attempts, 2, "coord's per-session count spans the classes");
    assert!(
        last_at.is_some(),
        "the most recent delivery across the classes stamps the report"
    );

    // And the omission rule still holds end to end.
    let body = compliance_body(
        None,
        Some(ABSENT_NO_BLOCK),
        nudge_state_for("never-seen-sum"),
    );
    assert!(body.get("nudge_attempts").is_none());
}

/// The class labels are part of the observable contract (marker key,
/// operator-visible reason strings), and the policy-pull one must match
/// coord's signal name so the two sides read as one thing.
#[test]
fn the_class_labels_are_stable_and_distinct() {
    assert_eq!(NudgeClass::Report.as_str(), "report");
    assert_eq!(NudgeClass::PolicyPull.as_str(), "policy_pull");
    assert_eq!(NudgeClass::ALL.len(), 2);
    assert_ne!(NudgeClass::ALL[0], NudgeClass::ALL[1]);
    assert_eq!(
        NudgeClass::ALL[0],
        NudgeClass::PolicyPull,
        "the precondition arm is offered first: telling a session to fix its report \
             while it has never read the document defining the report is backwards"
    );
}

// ── the policy-pull arm ──────────────────────────────────────────────

fn verdict_with(reconciliation: Value, reason: &str) -> ComplianceVerdict {
    ComplianceVerdict::from_body(&json!({
        "verdict": "unverified",
        "reason": reason,
        "reconciliation": reconciliation,
        "footprint": {"prs": ["qontinui/qontinui-coord#1"], "commits": [], "claims": []},
    }))
}

fn policy_pull_signal(result: &str, detail: Value) -> Value {
    json!({"session_signals": [{
        "signal": "policy_pull",
        "result": result,
        "detail": detail,
    }]})
}

/// `unavailable` and `error` mean coord could not CHECK. Nudging on either
/// would assert something coord does not know — and it is exactly the
/// population (unattributable transports, a coord ahead of its migration)
/// that the `Unavailable` arm exists to protect.
#[test]
fn only_an_absent_policy_pull_signal_is_nudgeable() {
    let detail = json!({"why": "no read"});
    assert!(policy_pull_absent(&policy_pull_signal("absent", detail.clone())).is_some());
    for benign in ["found", "unavailable", "error", "", "ABSENT", "future_word"] {
        assert!(
            policy_pull_absent(&policy_pull_signal(benign, detail.clone())).is_none(),
            "`{benign}` must not produce a nudge"
        );
    }
    // A payload from a coord that does not emit the signal at all.
    assert!(policy_pull_absent(&json!({"items": []})).is_none());
    assert!(policy_pull_absent(&Value::Null).is_none());
    // A different session-level signal must not be mistaken for this one.
    assert!(policy_pull_absent(&json!({"session_signals": [
        {"signal": "something_else", "result": "absent", "detail": {}}
    ]}))
    .is_none());
}

/// The two arms are independent: a session whose block reconciled perfectly
/// but which never pulled policy gets the POLICY-PULL nudge, not "re-emit
/// the block" — which would be a false statement about its own work.
#[test]
fn a_clean_report_with_no_policy_pull_nudges_only_the_policy_arm() {
    let v = verdict_with(
        {
            let mut r = policy_pull_signal("absent", json!({"why": "no read"}));
            r["schema_ok"] = json!(true);
            r["unreconciled_refs"] = json!([]);
            r
        },
        "policy pull not observed: no read",
    );
    assert_eq!(due_nudge_classes(&v), vec![NudgeClass::PolicyPull]);
    assert!(
        !report_arm_failed(&v),
        "a reason that is ONLY the policy-pull clause is not a report failure"
    );
}

/// Both failing yields both classes, policy-pull first — so a single turn
/// delivers one and the other stays eligible instead of being swallowed.
#[test]
fn both_arms_failing_yields_both_classes_in_priority_order() {
    let mut r = policy_pull_signal("absent", json!({"why": "no read"}));
    r["schema_ok"] = json!(true);
    r["unreconciled_refs"] = json!(["#1"]);
    let v = verdict_with(
        r,
        "1 of 1 item(s) could not be reconciled: #1; policy pull not observed: no read",
    );
    assert_eq!(
        due_nudge_classes(&v),
        vec![NudgeClass::PolicyPull, NudgeClass::Report]
    );
}

/// A missing block with the policy pull FOUND is the pre-existing report
/// arm, untouched.
#[test]
fn a_missing_block_with_a_found_pull_is_the_report_arm_alone() {
    let v = verdict_with(policy_pull_signal("found", json!({})), "absent");
    assert_eq!(due_nudge_classes(&v), vec![NudgeClass::Report]);
}

/// The policy-pull prompt must correct STEP 0, name the doors that work,
/// and — when the read was merely stale — say so, because a session that
/// read v5 believes it is current and will not re-read unless told the
/// number moved.
#[test]
fn the_policy_pull_prompt_states_the_stale_case_distinctly() {
    let never = policy_pull_nudge_text(&json!({
        "current_version": 6,
        "why": "this session has attributed policy reads but never read \
                    policy/session-protocol",
        "stale_version_reads": [],
    }));
    assert!(never.contains("policy/session-protocol"));
    assert!(never.contains("Step 0"));
    assert!(
        never.contains("/coord/agent-prompt-documents"),
        "the masked-tools escape hatch must be named: {never}"
    );
    assert!(!never.contains("while v6 is current"));
    assert!(
        !never.contains("did not emit"),
        "this arm must never assert the block was missing: {never}"
    );

    let stale = policy_pull_nudge_text(&json!({
        "current_version": 6,
        "why": "this session read policy/session-protocol at a SUPERSEDED version",
        "stale_version_reads": [{"version": 5, "source": "mcp"}],
    }));
    assert!(
        stale.contains("You read v5 while v6 is current"),
        "the stale case must name both numbers: {stale}"
    );
}

/// Two Stops racing the same session must not both deliver — the stated
/// reason `mark_nudged` is a compare-and-set rather than check-then-set.
#[test]
fn concurrent_claims_yield_exactly_one_winner() {
    let s = "session-compliance-test-race";
    let winners: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(move || usize::from(mark_nudged(s, NudgeClass::Report, 1))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or(0)).sum()
    });
    assert_eq!(winners, 1, "exactly one of eight racing claims may win");
    assert_eq!(nudge_attempts_for(s, NudgeClass::Report), 1);
}

// ── coverage bound ───────────────────────────────────────────────────

#[test]
fn coverage_bound_is_static_and_names_the_bare_terminal_gap() {
    let c = coverage_bound();
    assert!(!c.derived, "must never be derived from a runner metric");
    assert_eq!(c.schema, "session-compliance-coverage/1");
    assert!(c.not_covered.len() >= 4);
    assert!(c
        .not_covered
        .iter()
        .any(|g| g.scope.contains("bare terminals")));
    assert!(c.note.contains("liveUntracked"));
}

// ── transcript tail reader ───────────────────────────────────────────

#[test]
fn tail_reader_drops_the_truncated_first_line() {
    // Unique per process AND per nanosecond: several test runs share this
    // box, and a collision here would be a confusing cross-run failure.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!(
        "qontinui-compliance-tail-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("t.jsonl");
    // One oversized junk line, then the real record. The junk line pushes
    // the file past the window, so it must be dropped as partial.
    let filler = "x".repeat((TRANSCRIPT_TAIL_BYTES + 1024) as usize);
    std::fs::write(&path, format!("{filler}\n{}\n", assistant_line(&[BLOCK]))).expect("write");
    let tail = read_transcript_tail(&path).expect("tail");
    assert!(!tail.contains("xxxx"), "partial first line must be dropped");
    assert!(scan_transcript(&tail).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_transcript_reads_none_and_never_panics() {
    assert!(read_transcript_tail(Path::new("/definitely/not/here.jsonl")).is_none());
    assert!(detect(Path::new("/definitely/not/here.jsonl")).is_none());
}

// ── The session's tenant (plan 2026-11-13-runner-stop-hook-…) ───────

fn tenant_n(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn config_with_version(version: i64) -> ComplianceConfig {
    ComplianceConfig::from_body(&json!({
        "enabled": true, "applicable": true,
        "applicability_reason": "applicable",
        "prompt_document_version": version
    }))
    .expect("config")
}

/// Tests bullet 3 (config half). The default credential claims A, the
/// session is B: B's config is cached under (base, B), and a session of
/// tenant A never reads it. The old single slot served the last read to
/// every tenant.
#[test]
fn tenant_bs_config_is_cached_under_base_b_and_never_served_to_a() {
    let (a, b) = (tenant_n(0xA1), tenant_n(0xB2));
    let base = "https://compliance-config-b-not-a.example";
    let now = Instant::now();
    config_cache_store(base, Some(b), now, Some(config_with_version(9)));
    assert_eq!(
        config_cache_lookup(base, Some(b), now),
        Some(Some(config_with_version(9)))
    );
    assert_eq!(config_cache_lookup(base, Some(a), now), None);
    assert_eq!(config_cache_lookup(base, None, now), None);
    // A cached "could not consult" is a hit, distinct from a miss.
    config_cache_store(base, Some(a), now, None);
    assert_eq!(config_cache_lookup(base, Some(a), now), Some(None));
    assert_eq!(
        config_cache_lookup(base, Some(b), now + CONFIG_TTL + Duration::from_secs(1)),
        None,
        "the TTL still applies per slot"
    );
}

/// Tests bullet 3 (request half). On a runner whose default credential
/// claims A and which holds B's own slot, the config GET and the compliance
/// POST for a session of tenant B present B's credential — the old pair
/// presented the device default (A's) — and the default-slot pair presents
/// A's.
#[test]
fn the_config_read_and_compliance_post_for_tenant_b_present_bs_credential() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let _runner = crate::mcp::policy_tenant::test_support::two_tenant_runner(a, b);
    let client = reqwest::Client::new();
    let base = "https://coord.example.com";
    let body = compliance_body(None, Some(ABSENT_NO_BLOCK), None);
    for (scope, tenant) in [
        (crate::auth::TenantScope::Owned(b), b),
        (crate::auth::TenantScope::Device, a),
    ] {
        let target = PolicyTarget {
            scope,
            tenant: Some(tenant),
        };
        let config = build_config_get(&client, base, target).expect("config read is built");
        let post = build_compliance_post(
            &client,
            base,
            "11111111-1111-1111-1111-111111111111",
            &body,
            target,
        )
        .expect("compliance POST is built");
        for request in [config, post] {
            assert_eq!(
                policy_tenant::presented_claim(request.headers()),
                Some(tenant),
                "{scope:?} {}",
                request.url()
            );
        }
    }
}

/// Tests bullet 4 (request half). For a tenant this runner holds no
/// usable credential for, neither the config GET nor the compliance POST is
/// built to send: no anonymous POST, and never under the default
/// credential.
#[test]
fn no_config_read_or_compliance_post_is_built_for_a_tenant_with_no_usable_credential() {
    let client = reqwest::Client::new();
    let t = Uuid::new_v4();
    let target = PolicyTarget {
        scope: crate::auth::TenantScope::Owned(t),
        tenant: Some(t),
    };
    let base = "https://coord.example.com";
    let config = build_config_get(&client, base, target)
        .expect_err("no config read for a tenant with no credential");
    let body = compliance_body(None, Some(ABSENT_NO_BLOCK), None);
    let post = build_compliance_post(
        &client,
        base,
        "11111111-1111-1111-1111-111111111111",
        &body,
        target,
    )
    .expect_err("no compliance POST for a tenant with no credential");
    for refused in [config, post] {
        assert!(
            refused.contains(&format!("no usable credential claiming tenant {t}")),
            "{refused}"
        );
    }
}

/// Tests bullet 3 (withhold half). An unresolvable tenant on a
/// multi-bound device is UNKNOWN: the turn end makes no request at all —
/// no config read, so no POST — and the withhold is counted.
#[tokio::test]
async fn an_unknown_tenant_on_a_multi_bound_device_posts_nothing_and_is_counted() {
    use crate::mcp::policy_tenant::{fetch_target, policy_tenant_decision, PolicyTenantRead};
    let tenant = fetch_target(
        policy_tenant_decision(
            PolicyTenantRead::Unnamed("no coord-mcp session key names this session".into()),
            true,
        ),
        || panic!("an Unknown never reads the default credential's tenant"),
    );
    assert!(tenant.is_err(), "{tenant:?}");

    // The turn end makes no request for it: no config read, so no POST.
    let hook = json!({
        "session_id": "22222222-2222-2222-2222-222222222222",
        "transcript_path": "/definitely/not/here.jsonl"
    });
    assert!(observe_turn_end(&hook, &tenant).await.is_none());
    // And the gate it goes through counts the withhold — read off its own
    // return, not off the process-global counter other tests also bump.
    let Err(Withheld { total }) = compliance_target(&tenant, "turn_end", "s") else {
        panic!("an UNKNOWN tenant is withheld");
    };
    assert!(
        total >= 1,
        "compliance_withheld_tenant_unknown counted it: {total}"
    );
}

/// Tests bullet 5, review A5. The close-time finalize resolves its tenant
/// from the lifecycle record — its terminal, its working dir, its recorded
/// coord row and its DURABLE tenant (what still decides after a runner
/// restart) — and an UNKNOWN decision withholds (counted) before any read.
#[tokio::test]
async fn the_close_time_finalize_resolves_from_the_record_and_unknown_withholds() {
    let row = Uuid::new_v4();
    let record: TerminalSessionRecord = serde_json::from_value(json!({
        "claudeSessionId": "33333333-3333-3333-3333-333333333333",
        "workingDir": "/work/bauhaus-b2b-shop",
        "pageId": "p", "zoneIndex": 0,
        "terminalId": "term-7",
        "openedAt": 1, "lastSeenAt": 2,
        "state": "closed",
        "finishCoordSessionId": row.to_string(),
        "tenantId": format!(" {} ", tenant_n(0xB2))
    }))
    .expect("a lifecycle record");
    assert_eq!(
        CloseTenantInputs::of(&record),
        CloseTenantInputs {
            terminal_id: "term-7".to_string(),
            working_dir: Some("/work/bauhaus-b2b-shop".to_string()),
            recorded_row: Some(row),
            recorded_tenant: Some(tenant_n(0xB2)),
        }
    );
    let mut unparseable = record.clone();
    unparseable.tenant_id = Some("not-a-uuid".to_string());
    assert_eq!(CloseTenantInputs::of(&unparseable).recorded_tenant, None);

    let withheld = finalize_impl(
        &record.claude_session_id,
        None,
        record.working_dir.as_deref(),
        &Err("this session's tenant is UNKNOWN".to_string()),
    )
    .await;
    let Err(Withheld { total }) = withheld else {
        panic!("an UNKNOWN close is withheld, got {withheld:?}");
    };
    assert!(total >= 1, "and counted: {total}");
}

/// Tests bullet 7. A single-bound device behaves exactly as before: an
/// inconclusive read decides the default slot, which IS a target — the
/// compliance turn proceeds under the default credential and nothing is
/// withheld.
#[test]
fn a_single_bound_device_files_under_the_default_slot_as_before() {
    use crate::mcp::policy_tenant::{fetch_target, policy_tenant_decision, PolicyTenantRead};
    let a = tenant_n(0xA1);
    let tenant = fetch_target(
        policy_tenant_decision(PolicyTenantRead::Unnamed("inconclusive".into()), false),
        || Some(a),
    );
    assert_eq!(
        compliance_target(&tenant, "turn_end", "s"),
        Ok(PolicyTarget {
            scope: crate::auth::TenantScope::Device,
            tenant: Some(a),
        })
    );
}

/// Design §4: the Stop route's tenant read fits inside the turn-end
/// budget, so a slow read decides UNKNOWN rather than eating the hook.
#[test]
fn the_stop_tenant_budget_is_below_the_turn_end_budget() {
    assert!(policy_tenant::STOP_TENANT_BUDGET < TURN_END_BUDGET);
}
