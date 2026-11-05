//! The context-size reading a Claude Code `Stop` event carries, computed from
//! the transcript the event names (plan
//! `2026-10-28-one-session-lifecycle-authority` Phase 82, folding
//! qontinui-claude-config#1915's context-size Stop hook into the runner).
//!
//! ## What it computes
//!
//! `context_tokens` is `input_tokens + cache_creation_input_tokens +
//! cache_read_input_tokens` of the LAST main-thread assistant entry in the
//! transcript that carries `message.usage`: the size of the prompt the model
//! last read, which is what a cold prompt cache would have to re-write.
//! `last_turn_at` is that SAME entry's `timestamp`, so the pair always
//! describes one turn. Those two values plus the harness session id are the
//! body coord's `POST /coord/sessions/context` takes, and they are the only
//! things that leave the machine: the transcript holds prompts, file contents
//! and occasionally live credentials, and nothing else from it is kept.
//!
//! ## Where it runs
//!
//! Nowhere yet, on purpose. The `Stop` event reaches the runner through the
//! carrier's `type: "http"` hook ([`crate`]'s binary half,
//! `session::claude_hook`), and that hook is written only by a build that also
//! serves `/lifecycle/hook/<event>` (Phase 83). Phase 83's `Stop` handler
//! calls [`context_report`] and posts the result; until then this is the
//! tested half the route consumes, so the parser exists before the route that
//! feeds it.
//!
//! ## What reads UNKNOWN
//!
//! Every function here answers `None` rather than a number it did not read: no
//! session id, no main-thread assistant entry with usage, a usage block with
//! none of the three counters, a non-integer counter, or no timestamp. A
//! counter missing beside one that IS present counts as zero, as
//! qontinui-claude-config#1915's hook summed them; a usage block where ALL
//! THREE are absent is not a zero-token turn and reads `None`. Unlike that
//! hook, a malformed LAST turn is not skipped in favour of an older one: it
//! reads `None` (see [`last_main_turn`]); and a non-synthetic turn whose
//! counters are all present and `0` reports `0`, where that hook skipped any
//! entry totalling `<= 0`. The only zero-usage main-thread shape observed is
//! the `<synthetic>` one, which is skipped.

use serde::Serialize;

/// The body of coord's `POST /coord/sessions/context` (device JWT; coord
/// derives device and tenant from the token, never from this body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextReport {
    /// The harness session id (`session_id` in the Stop payload).
    pub claude_code_session_id: String,
    /// The prompt size of the last main-thread turn, in tokens.
    pub context_tokens: u64,
    /// That turn's transcript timestamp, verbatim (ISO-8601).
    pub last_turn_at: String,
}

/// The `(context_tokens, last_turn_at)` of the last main-thread assistant
/// entry in `transcript` (JSONL, oldest first) that carries `message.usage`.
/// PURE.
///
/// Two kinds of entry are not turns of this context and are skipped, as
/// qontinui-claude-config#1915's hook skipped them: sidechain entries
/// (`isSidechain: true`, a subagent's turns), which describe another context
/// than the one a continuation would land in; and `<synthetic>` entries
/// (`message.model == "<synthetic>"`), which Claude Code writes on the main
/// thread for an API error with every counter `0`, so reading one would report
/// a ~180k-token context as empty. Unparseable lines are skipped too, because a
/// transcript read while Claude Code is still appending can end in a partial
/// line.
pub fn last_main_turn(transcript: &str) -> Option<(u64, String)> {
    // Select the turn FIRST, then read it: a malformed last turn reads
    // UNKNOWN rather than falling back to an older, well-formed one.
    let entry = transcript
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter(|entry| entry.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .filter(|entry| entry.get("isSidechain").and_then(|s| s.as_bool()) != Some(true))
        .filter(|entry| {
            entry
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|m| m.as_str())
                != Some("<synthetic>")
        })
        .find(|entry| {
            entry
                .get("message")
                .and_then(|m| m.get("usage"))
                .is_some_and(|u| u.is_object())
        })?;
    let usage = entry.get("message")?.get("usage")?.as_object()?;
    let counters = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .map(|key| usage.get(key));
    if counters.iter().all(Option::is_none) {
        return None;
    }
    let mut total: u64 = 0;
    for counter in counters.into_iter().flatten() {
        // A present counter that is not a non-negative integer is a shape
        // this parser does not know; it reads UNKNOWN.
        total = total.checked_add(counter.as_u64()?)?;
    }
    let at = entry.get("timestamp")?.as_str()?.trim();
    if at.is_empty() {
        return None;
    }
    Some((total, at.to_string()))
}

/// The [`ContextReport`] for a `Stop` hook payload and the transcript text it
/// names (`transcript_path`, read by the caller). PURE.
pub fn context_report(stop_payload: &serde_json::Value, transcript: &str) -> Option<ContextReport> {
    let session_id = stop_payload.get("session_id")?.as_str()?.trim();
    if session_id.is_empty() {
        return None;
    }
    let (context_tokens, last_turn_at) = last_main_turn(transcript)?;
    Some(ContextReport {
        claude_code_session_id: session_id.to_string(),
        context_tokens,
        last_turn_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(ts: &str, usage: serde_json::Value) -> String {
        json!({"type": "assistant", "timestamp": ts, "message": {"usage": usage}}).to_string()
    }

    #[test]
    fn sums_the_three_counters_of_the_last_main_thread_turn() {
        let t = [
            assistant(
                "2026-11-05T01:00:00Z",
                json!({"input_tokens": 1, "cache_read_input_tokens": 1}),
            ),
            json!({"type": "user", "message": {"content": "hi"}}).to_string(),
            assistant(
                "2026-11-05T01:00:09Z",
                json!({"input_tokens": 3, "cache_creation_input_tokens": 6615,
                       "cache_read_input_tokens": 17997, "output_tokens": 40}),
            ),
        ]
        .join("\n");
        assert_eq!(
            last_main_turn(&t),
            Some((3 + 6615 + 17997, "2026-11-05T01:00:09Z".to_string())),
            "output_tokens is not prompt size and is not counted"
        );
    }

    #[test]
    fn a_sidechain_turn_and_a_partial_trailing_line_are_skipped() {
        let side =
            json!({"type": "assistant", "isSidechain": true, "timestamp": "2026-11-05T02:00:00Z",
                          "message": {"usage": {"input_tokens": 999}}})
            .to_string();
        let t = format!(
            "{}\n{}\n{{\"type\":\"assistant\",\"mess",
            assistant("2026-11-05T01:00:00Z", json!({"input_tokens": 10})),
            side
        );
        assert_eq!(
            last_main_turn(&t),
            Some((10, "2026-11-05T01:00:00Z".to_string()))
        );
    }

    #[test]
    fn a_synthetic_api_error_entry_is_not_the_last_turn() {
        // Claude Code's main-thread record of an API error (a 529, say): model
        // `<synthetic>`, every counter present and 0. Read as the last turn it
        // would report a large context as empty.
        let synthetic = json!({"type": "assistant", "isSidechain": false,
                               "isApiErrorMessage": true, "timestamp": "2026-11-05T02:00:00Z",
                               "message": {"model": "<synthetic>",
                                           "usage": {"input_tokens": 0,
                                                     "cache_creation_input_tokens": 0,
                                                     "cache_read_input_tokens": 0}}})
        .to_string();
        let t = format!(
            "{}\n{}",
            assistant(
                "2026-11-05T01:00:00Z",
                json!({"input_tokens": 2, "cache_read_input_tokens": 179_998})
            ),
            synthetic
        );
        assert_eq!(
            last_main_turn(&t),
            Some((180_000, "2026-11-05T01:00:00Z".to_string()))
        );
        // A transcript holding ONLY a synthetic entry has no turn to read.
        assert_eq!(last_main_turn(t.lines().nth(1).unwrap()), None);
    }

    #[test]
    fn unknown_shapes_read_none_never_zero() {
        // No usage counters at all: not a zero-token turn.
        assert_eq!(
            last_main_turn(&assistant(
                "2026-11-05T01:00:00Z",
                json!({"output_tokens": 5})
            )),
            None
        );
        // A non-integer counter on the LAST turn reads UNKNOWN; it never falls
        // back to an older well-formed turn.
        let t = format!(
            "{}\n{}",
            assistant("2026-11-05T00:00:00Z", json!({"input_tokens": 1})),
            assistant("2026-11-05T01:00:00Z", json!({"input_tokens": "7"}))
        );
        assert_eq!(last_main_turn(&t), None);
        // No timestamp.
        let no_ts =
            json!({"type": "assistant", "message": {"usage": {"input_tokens": 1}}}).to_string();
        assert_eq!(last_main_turn(&no_ts), None);
        // Empty transcript.
        assert_eq!(last_main_turn(""), None);
    }

    #[test]
    fn the_report_needs_a_session_id_and_carries_it_verbatim() {
        let t = assistant("2026-11-05T01:00:00Z", json!({"input_tokens": 4}));
        let payload = json!({"session_id": "0d1c5a0e-1111-4222-8333-944455556666",
                             "transcript_path": "/x.jsonl", "hook_event_name": "Stop"});
        let report = context_report(&payload, &t).expect("report");
        assert_eq!(
            report.claude_code_session_id,
            "0d1c5a0e-1111-4222-8333-944455556666"
        );
        assert_eq!(report.context_tokens, 4);
        let body = serde_json::to_value(&report).unwrap();
        assert_eq!(
            body.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["claude_code_session_id", "context_tokens", "last_turn_at"],
            "exactly coord's three fields leave the machine — never transcript content"
        );
        assert_eq!(context_report(&json!({"session_id": " "}), &t), None);
        assert_eq!(context_report(&json!({}), &t), None);
    }
}
