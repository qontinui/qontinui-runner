//! Premature-bailout detection: classify how an agent's turn ENDED.
//!
//! `finish-to-zero` is enforced entirely by the model choosing to comply, and a
//! bailing-out turn *looks* like a completed one — well-formed, polite, often
//! summarizing real work before declining the rest. Nothing else in the stack
//! reads what the turn actually SAID:
//!
//! - [`super::idle::snapshot_looks_idle`] answers "is the tab ready for input",
//!   from a rendered VT grid.
//! - `terminal::transcript`'s `likely_frozen` keys on timing + message SHAPE
//!   (`last_assistant_had_tool_use`), so a prose-only bail — no tool use —
//!   fails its predicate by construction.
//! - The `_loop-control` rubric's stall rule compares work fingerprints
//!   BETWEEN rounds, so it needs a second round to fire; a bailout ends the
//!   loop and never produces one.
//!
//! This module is the missing content classifier. It is **pure**: `&str` in,
//! [`TurnEnding`] out, no I/O. That is structurally enforced by placement —
//! `terminal` is a bin-target module and is NOT exported from `lib.rs`, so this
//! lib module cannot reach the transcript reader even by accident. The impure
//! half (resolving a session's transcript and pulling the final assistant text)
//! is bin-side glue, exactly as [`super::policy`] gathers its `TickInput`.
//!
//! ## The rule
//!
//! A turn is judged to be stopping when the **last non-empty paragraph** starts
//! with a known stop pattern. Anchoring to the paragraph START is the whole
//! trick: a turn that *discusses* stopping mid-paragraph and then keeps working
//! is [`TurnEnding::Complete`]. The plan that specified this module is itself a
//! fixture for that case, and so is this doc comment.
//!
//! ## Three endings, not two
//!
//! Fleet policy `planning-and-scope` `dependency-wait-and-resume` authorizes
//! stopping on an unmet dependency: an observable signal with a short wait keeps
//! the session alive, and *"for longer or unbounded waits (a signed release, a
//! human decision), register the gate + continuation and stop with status
//! waiting."* So stopping on a human decision is the PRESCRIBED ending, not a
//! bailout — provided a gate was registered.
//!
//! The detector cannot see gate state, so it does not claim one. It reports
//! [`TurnEnding::UserDeflection`] for the "waiting on a human" shape and leaves
//! the join to the consumer:
//!
//! | Detector says | Gate registered? | Consumer's verdict |
//! |---|---|---|
//! | `UserDeflection` | no | **bailout** — the ungated-blocked-item case the policy forbids |
//! | `UserDeflection` | yes | policy-compliant `stop with status waiting` |
//! | `Bailout` | — | bailout |
//! | `WaitingOnSignal` | — | policy-compliant |
//!
//! Collapsing that distinction would flag every correctly `/blocked`-closed
//! session, which is the fastest way to get a control switched off.
//!
//! ## Unknown is not Complete
//!
//! The transcript reader is bounded, so the final assistant record can be
//! missing from the window — and a bail is characteristically a LONG turn, i.e.
//! exactly the one that overruns a small cap. Folding an unread turn into
//! `Complete` would make a shadow corpus look clean when it was never read, so
//! unreadable input is [`TurnEnding::Unknown`] and is counted in its own bucket.

use std::fmt;

/// Which labelled stop pattern matched. Fieldless and `Copy` so a verdict can
/// ride in [`super::policy::TickInput`], which is `#[derive(Copy)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternId {
    /// "I am unable to proceed / continue / complete …"
    UnableToProceed,
    /// "I cannot complete / finish …"
    CannotComplete,
    /// "I'm giving up on …"
    GivingUp,
    /// "I'll stop here."
    StoppingHere,
    /// "I won't continue / proceed …"
    WillNotContinue,
    /// "I'll leave the rest to …"
    LeavingTheRest,
    /// "That's outside the scope of …" as the FINAL word on the work.
    DeclaredOutOfScope,
    /// "You'll need to … / You should … / Please …"
    HandedBackToUser,
    /// "Let me know … / Tell me …"
    AwaitingInstruction,
    /// "Retry once … / Try again when …"
    RetryWhen,
}

impl PatternId {
    /// Every variant, for exhaustive iteration in the drift guard.
    pub const ALL: &'static [PatternId] = &[
        PatternId::UnableToProceed,
        PatternId::CannotComplete,
        PatternId::GivingUp,
        PatternId::StoppingHere,
        PatternId::WillNotContinue,
        PatternId::LeavingTheRest,
        PatternId::DeclaredOutOfScope,
        PatternId::HandedBackToUser,
        PatternId::AwaitingInstruction,
        PatternId::RetryWhen,
    ];

    /// Stable label for journals and reports. Never derive this from `Debug` —
    /// a rename would silently re-key a recorded shadow corpus.
    pub fn label(self) -> &'static str {
        match self {
            PatternId::UnableToProceed => "PATTERN_UNABLE_TO_PROCEED",
            PatternId::CannotComplete => "PATTERN_CANNOT_COMPLETE",
            PatternId::GivingUp => "PATTERN_GIVING_UP",
            PatternId::StoppingHere => "PATTERN_STOPPING_HERE",
            PatternId::WillNotContinue => "PATTERN_WILL_NOT_CONTINUE",
            PatternId::LeavingTheRest => "PATTERN_LEAVING_THE_REST",
            PatternId::DeclaredOutOfScope => "PATTERN_DECLARED_OUT_OF_SCOPE",
            PatternId::HandedBackToUser => "PATTERN_HANDED_BACK_TO_USER",
            PatternId::AwaitingInstruction => "PATTERN_AWAITING_INSTRUCTION",
            PatternId::RetryWhen => "PATTERN_RETRY_WHEN",
        }
    }

    /// The literal paragraph-leading phrases for this pattern.
    ///
    /// **These are PREFIXES, not regexes, and that is the anchoring
    /// mechanism.** A regex source can be un-anchored by deleting one `^` and
    /// the mistake compiles, runs, and starts matching mid-paragraph — the
    /// exact drift the design has to prevent. Prefix matching makes anchoring a
    /// property of the operation instead of a convention in the data, so there
    /// is no `^` for an edit to drop. The drift guard below still asserts it
    /// behaviourally.
    pub fn prefixes(self) -> &'static [&'static str] {
        match self {
            PatternId::UnableToProceed => &[
                "i am unable to",
                "i'm unable to",
                "im unable to",
                "unable to proceed",
                "unable to continue",
            ],
            PatternId::CannotComplete => &[
                "i cannot complete",
                "i can't complete",
                "i cant complete",
                "i cannot finish",
                "i can't finish",
                "i cannot do",
                "i can't do",
            ],
            PatternId::GivingUp => &["i'm giving up", "i am giving up", "giving up on"],
            PatternId::StoppingHere => &[
                "i'll stop here",
                "i will stop here",
                "stopping here",
                "i'm stopping here",
                "i am stopping here",
                "i'll leave it here",
                "i will leave it here",
            ],
            PatternId::WillNotContinue => &[
                "i won't continue",
                "i wont continue",
                "i will not continue",
                "i won't proceed",
                "i will not proceed",
            ],
            PatternId::LeavingTheRest => &[
                "i'll leave the rest",
                "i will leave the rest",
                "i'll leave the remaining",
                "i will leave the remaining",
                "the rest is left",
            ],
            PatternId::DeclaredOutOfScope => &[
                "this is out of scope",
                "that is out of scope",
                "this is outside the scope",
                "that is outside the scope",
                "this is beyond the scope",
            ],
            PatternId::HandedBackToUser => &[
                "you'll need to",
                "you will need to",
                "you should",
                "you can then",
                "please run",
                "please confirm",
                "please review",
                "please verify",
            ],
            PatternId::AwaitingInstruction => &[
                "let me know",
                "tell me how",
                "tell me whether",
                "tell me if",
                "would you like me to",
                "do you want me to",
                "should i",
            ],
            PatternId::RetryWhen => &[
                "retry when",
                "retry once",
                "try again when",
                "try again once",
                "re-run when",
                "rerun when",
                "resume when",
                "resume once",
            ],
        }
    }
}

impl fmt::Display for PatternId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why the turn-final text could not be classified. `Copy` for the same reason
/// as [`PatternId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnreadReason {
    /// The text was empty or entirely whitespace.
    EmptyText,
    /// The bounded transcript window held no assistant record at all.
    NoAssistantRecord,
    /// An assistant record was present but truncated by the read cap, so its
    /// tail — the part this detector classifies — is missing.
    TruncatedAtCap,
    /// A record was found but did not parse.
    MalformedRecord,
    /// No transcript file exists for the session.
    TranscriptMissing,
    /// I/O failed while reading the transcript.
    ReadError,
}

impl UnreadReason {
    pub fn label(self) -> &'static str {
        match self {
            UnreadReason::EmptyText => "empty_text",
            UnreadReason::NoAssistantRecord => "no_assistant_record",
            UnreadReason::TruncatedAtCap => "truncated_at_cap",
            UnreadReason::MalformedRecord => "malformed_record",
            UnreadReason::TranscriptMissing => "transcript_missing",
            UnreadReason::ReadError => "read_error",
        }
    }
}

impl fmt::Display for UnreadReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// How a turn ended.
///
/// Not `Copy` — `WaitingOnSignal` owns the signal text for reporting. Use
/// [`TurnEnding::verdict`] for the `Copy` form a tick core can carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnding {
    /// The turn did not end on a stop pattern. The overwhelmingly common case.
    Complete,
    /// Stopping on an OBSERVABLE signal with a bounded wait — the
    /// `dependency-wait-and-resume` keep-alive arm. Carries the signal phrase
    /// that matched, for the journal.
    WaitingOnSignal { signal: String },
    /// Stopping on a HUMAN — approval, a decision, an instruction. **Not a
    /// bailout claim**: the consumer joins this with gate state (see the module
    /// docs) before deciding.
    UserDeflection { pattern: PatternId },
    /// Stopping with neither an observable signal nor a human to wait on.
    Bailout { pattern: PatternId },
    /// The turn-final text could not be recovered. Never fold this into
    /// `Complete`.
    Unknown { reason: UnreadReason },
}

/// The `Copy` projection of a [`TurnEnding`], for embedding in a tick core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnVerdict {
    Complete,
    WaitingOnSignal,
    UserDeflection(PatternId),
    Bailout(PatternId),
    Unknown(UnreadReason),
}

impl TurnEnding {
    /// Project to the `Copy` verdict, dropping the owned signal text.
    pub fn verdict(&self) -> TurnVerdict {
        match self {
            TurnEnding::Complete => TurnVerdict::Complete,
            TurnEnding::WaitingOnSignal { .. } => TurnVerdict::WaitingOnSignal,
            TurnEnding::UserDeflection { pattern } => TurnVerdict::UserDeflection(*pattern),
            TurnEnding::Bailout { pattern } => TurnVerdict::Bailout(*pattern),
            TurnEnding::Unknown { reason } => TurnVerdict::Unknown(*reason),
        }
    }

    /// Stable journal key, so a recorded shadow corpus survives refactors.
    pub fn kind_label(&self) -> &'static str {
        match self {
            TurnEnding::Complete => "complete",
            TurnEnding::WaitingOnSignal { .. } => "waiting_on_signal",
            TurnEnding::UserDeflection { .. } => "user_deflection",
            TurnEnding::Bailout { .. } => "bailout",
            TurnEnding::Unknown { .. } => "unknown",
        }
    }
}

/// Observable-signal phrases: things a gate can watch, per
/// `dependency-wait-and-resume`'s "observable signal and a short expected wait".
///
/// Deliberately noun-phrase shaped. A bare verb like "passes" would match
/// prose about anything.
const SIGNAL_MARKERS: &[&str] = &[
    "the build",
    "the ci",
    "ci is",
    "ci goes",
    "ci passes",
    "the tests",
    "the test suite",
    "the deploy",
    "the deployment",
    "the pr",
    "the pull request",
    "the merge",
    "the merge train",
    "the gate",
    "the workflow",
    "the pipeline",
    "the migration",
    "the release",
    "the timer",
    "it lands",
    "it merges",
    "it clears",
    "it goes green",
    "main is",
];

/// Second-person REQUEST phrases: a human is the thing being waited on.
///
/// Bare `"you"` is deliberately absent — it appears in ordinary completed
/// turns ("you'll see the fix on the dashboard") and would swamp the signal.
/// Every entry here is a request or a decision handed to a person.
const HUMAN_MARKERS: &[&str] = &[
    "you approve",
    "your approval",
    "you confirm",
    "your confirmation",
    "you decide",
    "your decision",
    "you sign off",
    "you tell me",
    "you let me know",
    "let me know",
    "tell me",
    "waiting on you",
    "waiting for you",
    "want me to",
    "would you like",
    "should i",
    "the operator",
    "please confirm",
    "please approve",
    "please decide",
    "please review",
    "please verify",
    "please run",
    "please let me know",
    "you'll need to",
    "you will need to",
    "you should",
];

/// Patterns that are hand-backs BY DEFINITION — the pattern itself is the
/// deflection, so no marker scan is needed or wanted.
///
/// Without this, tightening [`HUMAN_MARKERS`] away from a bare `"please"` (which
/// fired on innocuous closers like "please see the PR") would have made
/// "Please run the migration yourself." fall through to `Bailout` — the wrong
/// arm, since it plainly hands the work to a person.
const INHERENTLY_HUMAN: &[PatternId] =
    &[PatternId::HandedBackToUser, PatternId::AwaitingInstruction];

/// The last paragraph of `text` that contains a non-whitespace character.
///
/// Paragraphs are blank-line separated. Returns `None` when `text` has no
/// non-whitespace content at all. The returned slice is trimmed.
///
/// This is the anchoring surface: everything else in this module matches
/// against the START of this string.
pub fn last_non_empty_paragraph(text: &str) -> Option<&str> {
    let mut end = text.len();
    let bytes = text.as_bytes();

    // Walk backwards over paragraphs, newest first, and return the first one
    // with content. A "paragraph break" is a newline followed (after optional
    // horizontal whitespace) by another newline.
    loop {
        let slice = &text[..end];
        let start = match find_paragraph_break(slice) {
            Some(idx) => idx,
            None => {
                let p = slice.trim();
                return if p.is_empty() { None } else { Some(p) };
            }
        };
        let para = text[start..end].trim();
        if !para.is_empty() {
            return Some(para);
        }
        end = start;
        // Consume the break itself so the next iteration looks further back.
        while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
    }
}

/// Byte index just AFTER the last paragraph break in `s`, or `None` if there
/// is no break. A break is `\n` + optional spaces/tabs + `\n`.
fn find_paragraph_break(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        if b[i] != b'\n' {
            continue;
        }
        // Scan back over horizontal whitespace and a possible \r.
        let mut j = i;
        while j > 0 {
            let c = b[j - 1];
            if c == b' ' || c == b'\t' || c == b'\r' {
                j -= 1;
            } else {
                break;
            }
        }
        if j > 0 && b[j - 1] == b'\n' {
            return Some(i + 1);
        }
    }
    None
}

/// Strip leading markdown decoration so a bulleted or bolded final paragraph
/// still anchors on its first WORD.
///
/// Only layout characters are removed — list bullets, blockquote carets,
/// heading hashes, emphasis markers, and the numeric part of an ordered-list
/// marker. Nothing that carries meaning is touched, so anchoring still applies
/// to the paragraph's actual first word.
fn strip_leading_decoration(p: &str) -> &str {
    let mut s = p.trim_start();
    loop {
        let before = s;
        s = s.trim_start_matches(['>', '#', '*', '_', '`', '-', '+', ' ', '\t']);
        // An ordered-list marker: digits followed by '.' or ')'.
        let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            let rest = &s[digits..];
            if let Some(stripped) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
                s = stripped.trim_start();
            }
        }
        if s == before {
            return s;
        }
    }
}

/// Normalize a paragraph for prefix matching: strip decoration, lowercase,
/// and collapse internal whitespace runs to single spaces.
///
/// Collapsing is safe HERE — unlike on the terminal grid, where
/// `terminal::output_scan::normalize` also collapses NEWLINES and so destroys
/// the paragraph structure this module depends on. By this point the paragraph
/// has already been split out, so only intra-paragraph wrapping is flattened.
fn normalize_paragraph(p: &str) -> String {
    let s = strip_leading_decoration(p);
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The stop pattern this paragraph STARTS with, if any.
///
/// Anchored by construction: only a prefix can match. A paragraph that mentions
/// a stop phrase anywhere but its start returns `None`.
pub fn matched_stop_pattern(paragraph: &str) -> Option<PatternId> {
    let norm = normalize_paragraph(paragraph);
    for id in PatternId::ALL {
        for prefix in id.prefixes() {
            if norm.starts_with(prefix) {
                return Some(*id);
            }
        }
    }
    None
}

/// Connectives that introduce a WAIT. A signal only counts when something is
/// waiting ON it.
///
/// `dependency-wait-and-resume` speaks of a dependency *having* an observable
/// signal — not of a paragraph mentioning one. The distinction is load-bearing:
/// "I'll stop here. Please see the PR for details." names a PR without waiting
/// on anything, and matching the bare noun phrase turned that genuine bailout
/// into `WaitingOnSignal` — a false negative in the one direction that matters,
/// since it silently excuses the failure this module exists to catch.
const WAIT_CONNECTIVES: &[&str] = &[
    "when ",
    "once ",
    "until ",
    "after ",
    "as soon as ",
    "pending ",
    "waiting on ",
    "waiting for ",
    "blocked on ",
    "depends on ",
];

/// The first observable-signal phrase that some wait connective introduces.
///
/// Requires BOTH a connective and a signal phrase at or after it, so a signal
/// merely named in passing does not count.
fn matched_signal(norm: &str) -> Option<&'static str> {
    let wait_at = WAIT_CONNECTIVES.iter().filter_map(|c| norm.find(c)).min()?;
    let tail = &norm[wait_at..];
    SIGNAL_MARKERS.iter().copied().find(|m| tail.contains(m))
}

/// Whether `norm` hands the wait to a person.
fn mentions_human(norm: &str) -> bool {
    HUMAN_MARKERS.iter().any(|m| norm.contains(m))
}

/// Classify how a turn ended, from its final text.
///
/// Pure and total: no I/O, no panics, terminates on any input.
///
/// Order of decision:
/// 1. No non-whitespace content ⇒ `Unknown { EmptyText }` — NOT `Complete`.
/// 2. The last non-empty paragraph does not START with a stop pattern
///    ⇒ `Complete`.
/// 3. It does, and hands the wait to a person ⇒ `UserDeflection`. "A person"
///    means either an inherently-hand-back pattern ([`INHERENTLY_HUMAN`]) or a
///    [`HUMAN_MARKERS`] phrase. A person outranks a signal when both appear:
///    the consumer's gate join is what settles it, and `UserDeflection` makes
///    no bailout claim on its own.
/// 4. It does, and WAITS ON an observable signal (a wait connective followed by
///    a signal phrase — not a signal merely mentioned) ⇒ `WaitingOnSignal`.
/// 5. Neither ⇒ `Bailout`.
pub fn classify_turn_ending(final_text: &str) -> TurnEnding {
    let Some(paragraph) = last_non_empty_paragraph(final_text) else {
        return TurnEnding::Unknown {
            reason: UnreadReason::EmptyText,
        };
    };

    let Some(pattern) = matched_stop_pattern(paragraph) else {
        return TurnEnding::Complete;
    };

    let norm = normalize_paragraph(paragraph);

    if INHERENTLY_HUMAN.contains(&pattern) || mentions_human(&norm) {
        return TurnEnding::UserDeflection { pattern };
    }

    if let Some(signal) = matched_signal(&norm) {
        return TurnEnding::WaitingOnSignal {
            signal: signal.to_string(),
        };
    }

    TurnEnding::Bailout { pattern }
}

/// Resolve a [`TurnEnding::UserDeflection`] against whether the session
/// registered a coord gate for the work it is stopping on.
///
/// This is the consumer-side conjunct from the module docs — the one fact the
/// text cannot carry. Non-deflection endings pass through unchanged.
///
/// `dependency-wait-and-resume`: *"Never end a session with a blocked item that
/// has no registered gate."* An ungated deflection is that forbidden ending; a
/// gated one is the prescribed `stop with status waiting`.
pub fn resolve_with_gate_state(ending: TurnEnding, gate_registered: bool) -> TurnEnding {
    match ending {
        TurnEnding::UserDeflection { pattern } => {
            if gate_registered {
                TurnEnding::WaitingOnSignal {
                    signal: "registered coord gate".to_string(),
                }
            } else {
                TurnEnding::Bailout { pattern }
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── last_non_empty_paragraph ─────────────────────────────────────────

    #[test]
    fn last_paragraph_skips_trailing_blank_lines() {
        let t = "first para\n\nsecond para\n\n   \n\n";
        assert_eq!(last_non_empty_paragraph(t), Some("second para"));
    }

    #[test]
    fn single_paragraph_is_its_own_last() {
        assert_eq!(last_non_empty_paragraph("just one"), Some("just one"));
    }

    #[test]
    fn multiline_paragraph_is_returned_whole() {
        let t = "intro\n\nline one\nline two";
        assert_eq!(last_non_empty_paragraph(t), Some("line one\nline two"));
    }

    #[test]
    fn crlf_paragraph_breaks_are_recognized() {
        let t = "intro\r\n\r\nfinal para";
        assert_eq!(last_non_empty_paragraph(t), Some("final para"));
    }

    #[test]
    fn whitespace_only_text_has_no_paragraph() {
        assert_eq!(last_non_empty_paragraph(""), None);
        assert_eq!(last_non_empty_paragraph("   \n\n \t \n"), None);
    }

    // ── the two grok-build distinctions (plan §6.1) ──────────────────────

    #[test]
    fn retry_when_you_approve_is_user_deflection_not_bailout() {
        let e = classify_turn_ending("Did the first half.\n\nRetry when you approve.");
        assert_eq!(
            e,
            TurnEnding::UserDeflection {
                pattern: PatternId::RetryWhen
            },
            "waiting on a human who was never asked"
        );
        // It only BECOMES a bailout once we know no gate was registered.
        assert!(matches!(
            resolve_with_gate_state(e.clone(), false),
            TurnEnding::Bailout { .. }
        ));
        assert!(matches!(
            resolve_with_gate_state(e, true),
            TurnEnding::WaitingOnSignal { .. }
        ));
    }

    #[test]
    fn retry_when_the_build_passes_is_waiting_on_signal() {
        let e = classify_turn_ending("Pushed the fix.\n\nRetry when the build passes.");
        assert!(
            matches!(e, TurnEnding::WaitingOnSignal { .. }),
            "observable signal with a bounded wait, got {e:?}"
        );
    }

    #[test]
    fn blocked_shaped_close_never_reaches_bailout() {
        // The fleet's own /blocked session-close protocol. Policy
        // `dependency-wait-and-resume` PRESCRIBES this ending.
        let t = "Phase 2 is done and pushed.\n\n\
                 Resume once the deploy goes healthy.";
        let e = classify_turn_ending(t);
        assert!(
            matches!(e, TurnEnding::WaitingOnSignal { .. }),
            "got {e:?} — flagging a correctly-gated close is the fastest way to \
             get this control switched off"
        );
        assert!(!matches!(
            resolve_with_gate_state(e, true),
            TurnEnding::Bailout { .. }
        ));
    }

    #[test]
    fn a_human_mention_outranks_a_signal_mention() {
        // Both present: stays UserDeflection so the gate join decides.
        let t = "Stopping here until the deploy is done — let me know how to proceed.";
        assert!(matches!(
            classify_turn_ending(t),
            TurnEnding::UserDeflection { .. }
        ));
    }

    // ── anchoring (plan §6.2) ────────────────────────────────────────────

    #[test]
    fn mid_paragraph_stop_phrase_is_complete() {
        let t = "I considered whether I am unable to proceed, decided I could, \
                 and finished every item.";
        assert_eq!(classify_turn_ending(t), TurnEnding::Complete);
    }

    #[test]
    fn this_modules_own_prose_classifies_complete() {
        // The plan named its §1 as a fixture: a document that DISCUSSES giving
        // up must not read as giving up.
        let t = "The failure is near-invisible because a bailing-out turn looks \
                 like a completed one.\n\n\
                 Long-running skills make it worse: an early stop in those is a \
                 silent no-op that looks like a clean tick. I'll stop here is the \
                 kind of phrase the detector matches, but only at a paragraph start.";
        assert_eq!(classify_turn_ending(t), TurnEnding::Complete);
    }

    #[test]
    fn an_earlier_paragraph_bailing_out_does_not_count() {
        let t = "I am unable to proceed with the old approach.\n\n\
                 So I switched approaches and landed all five phases.";
        assert_eq!(classify_turn_ending(t), TurnEnding::Complete);
    }

    #[test]
    fn decoration_does_not_defeat_anchoring() {
        for decorated in [
            "- I'll stop here.",
            "**I'll stop here.**",
            "> I'll stop here.",
            "1. I'll stop here.",
            "### I'll stop here.",
        ] {
            let t = format!("did the work\n\n{decorated}");
            assert!(
                matches!(classify_turn_ending(&t), TurnEnding::Bailout { .. }),
                "decoration hid the anchor in {decorated:?}"
            );
        }
    }

    // ── ordinary completed turns must not trip ───────────────────────────

    #[test]
    fn a_normal_completion_summary_is_complete() {
        for t in [
            "All five phases landed. PRs #101 and #102 are open and green.",
            "Done — tests pass (running 42 tests; test result: ok).",
            "Fixed the bug in transcript.rs and pushed. You'll see the fix on the \
             dashboard once it deploys.",
            "Shipped. The plan is stamped SHIPPED and the gate cleared.",
        ] {
            assert_eq!(
                classify_turn_ending(t),
                TurnEnding::Complete,
                "false positive on: {t}"
            );
        }
    }

    #[test]
    fn an_innocuous_please_does_not_turn_a_bailout_into_a_deflection() {
        // "please" alone is not someone being waited ON. Before this was
        // tightened, any polite closer downgraded the verdict.
        assert!(matches!(
            classify_turn_ending("I'll stop here. Please see the PR for details."),
            TurnEnding::Bailout { .. }
        ));
    }

    #[test]
    fn inherently_human_patterns_need_no_marker() {
        // The other half of that tightening: a hand-back must still read as a
        // deflection even when no HUMAN_MARKERS phrase is present.
        assert!(matches!(
            classify_turn_ending("Please run the migration yourself."),
            TurnEnding::UserDeflection {
                pattern: PatternId::HandedBackToUser
            }
        ));
    }

    #[test]
    fn a_signal_merely_mentioned_is_not_a_signal_waited_on() {
        // No wait connective => not waiting on the PR, just pointing at it.
        assert_eq!(matched_signal("see the pr for details"), None);
        // With one, it is a genuine wait.
        assert_eq!(matched_signal("resume once the pr merges"), Some("the pr"));
    }

    #[test]
    fn bare_you_does_not_mark_a_human_wait() {
        // The reason HUMAN_MARKERS excludes bare "you".
        assert!(!mentions_human("you will see the fix on the dashboard"));
    }

    // ── the bailout arm ──────────────────────────────────────────────────

    #[test]
    fn ungated_stop_with_no_signal_is_a_bailout() {
        for (t, want) in [
            ("I am unable to proceed.", PatternId::UnableToProceed),
            (
                "I cannot complete the remaining phases.",
                PatternId::CannotComplete,
            ),
            ("I'm giving up on this approach.", PatternId::GivingUp),
            ("I'll stop here.", PatternId::StoppingHere),
            (
                "I won't continue with the rest.",
                PatternId::WillNotContinue,
            ),
            (
                "I'll leave the rest for another session.",
                PatternId::LeavingTheRest,
            ),
            (
                "This is out of scope for this session.",
                PatternId::DeclaredOutOfScope,
            ),
        ] {
            assert_eq!(
                classify_turn_ending(t),
                TurnEnding::Bailout { pattern: want },
                "on: {t}"
            );
        }
    }

    #[test]
    fn handing_back_to_the_user_is_deflection() {
        for t in [
            "You'll need to run the migration yourself.",
            "Let me know whether to continue.",
            "Should I proceed with phase 3?",
            "Please confirm before I continue.",
        ] {
            assert!(
                matches!(classify_turn_ending(t), TurnEnding::UserDeflection { .. }),
                "on: {t}"
            );
        }
    }

    // ── Unknown is not Complete (plan §3.1.2 / §6.4a) ────────────────────

    #[test]
    fn empty_text_is_unknown_not_complete() {
        assert_eq!(
            classify_turn_ending(""),
            TurnEnding::Unknown {
                reason: UnreadReason::EmptyText
            }
        );
        assert_eq!(
            classify_turn_ending("  \n\n\t \n "),
            TurnEnding::Unknown {
                reason: UnreadReason::EmptyText
            }
        );
    }

    #[test]
    fn unknown_and_complete_are_distinguishable_in_a_corpus() {
        // The whole point: a shadow corpus must never confuse "read it, fine"
        // with "never read it".
        assert_ne!(
            classify_turn_ending("").kind_label(),
            classify_turn_ending("all done").kind_label()
        );
    }

    // ── drift guard (plan §6.3) ──────────────────────────────────────────

    #[test]
    fn every_pattern_is_anchored_and_only_anchored() {
        for id in PatternId::ALL {
            for prefix in id.prefixes() {
                // (a) The prefix matches at a paragraph start.
                let at_start = format!("{prefix} the remaining work");
                assert_eq!(
                    matched_stop_pattern(&at_start),
                    Some(*id),
                    "{} lost its match on {prefix:?}",
                    id.label()
                );

                // (b) The SAME text mid-paragraph must NOT match. This is the
                // regression that an un-anchoring edit would introduce.
                let mid = format!("I checked whether {prefix} anything, and it was fine");
                assert_eq!(
                    matched_stop_pattern(&mid),
                    None,
                    "{} matched mid-paragraph on {prefix:?} — anchoring is broken",
                    id.label()
                );
            }
        }
    }

    #[test]
    fn pattern_prefixes_are_well_formed() {
        for id in PatternId::ALL {
            let prefixes = id.prefixes();
            assert!(!prefixes.is_empty(), "{} has no prefixes", id.label());
            for p in prefixes {
                assert!(!p.is_empty(), "{} has an empty prefix", id.label());
                assert_eq!(
                    *p,
                    p.to_lowercase(),
                    "{} prefix {p:?} is not lowercase — normalize_paragraph \
                     lowercases before matching, so it could never fire",
                    id.label()
                );
                assert!(
                    !p.starts_with('^'),
                    "{} prefix {p:?} carries a regex anchor; these are literal \
                     prefixes and a '^' would only ever fail to match",
                    id.label()
                );
                assert_eq!(
                    *p,
                    p.trim(),
                    "{} prefix {p:?} has surrounding whitespace",
                    id.label()
                );
            }
        }
    }

    #[test]
    fn pattern_labels_are_unique_and_stable() {
        let mut seen = std::collections::HashSet::new();
        for id in PatternId::ALL {
            assert!(
                seen.insert(id.label()),
                "duplicate label {} — a shadow corpus would re-key",
                id.label()
            );
            assert!(id.label().starts_with("PATTERN_"));
        }
        assert_eq!(seen.len(), PatternId::ALL.len());
    }

    #[test]
    fn all_covers_every_variant() {
        // A new variant added without extending ALL would silently never match.
        // `label()` is an exhaustive match, so this pins the count alongside it.
        assert_eq!(PatternId::ALL.len(), 10);
    }

    // ── purity / termination (plan §6.4) ─────────────────────────────────

    #[test]
    fn terminates_and_never_panics_on_adversarial_input() {
        // Deterministic pseudo-random corpus — no external crate, no clock.
        let alphabet: Vec<char> = "\n\r \t*#->_`0123456789.aeiou I'mstop!".chars().collect();
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for len in [0usize, 1, 2, 7, 64, 513, 4096] {
            for _ in 0..40 {
                let mut s = String::with_capacity(len);
                for _ in 0..len {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let idx = (state >> 33) as usize % alphabet.len();
                    s.push(alphabet[idx]);
                }
                // Must return; must not panic; must be self-consistent.
                let e = classify_turn_ending(&s);
                assert_eq!(e.verdict(), classify_turn_ending(&s).verdict());
            }
        }
    }

    #[test]
    fn multibyte_input_does_not_panic_on_slicing() {
        for t in [
            "— — —\n\nI'll stop here.",
            "日本語のテキスト\n\nI am unable to proceed.",
            "emoji 🎉🎉\n\n",
            "\u{200b}\u{200b}",
        ] {
            let _ = classify_turn_ending(t);
        }
    }

    #[test]
    fn verdict_projection_is_copy_and_lossless_on_kind() {
        let e = classify_turn_ending("I'll stop here.");
        let v = e.verdict();
        let v2 = v; // still usable => Copy
        assert_eq!(v, v2);
        assert!(matches!(v, TurnVerdict::Bailout(PatternId::StoppingHere)));
    }
}
