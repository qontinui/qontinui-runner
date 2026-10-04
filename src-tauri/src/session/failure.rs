//! One failure taxonomy, one classifier, one recovery table.
//!
//! Plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 7. Before this module the runner handled an AI session's failures
//! through five mechanisms that did not know about each other: a grid-scraped
//! limit phrase migrated the session to another account, a stream-json child's
//! stderr rotated the account and restarted in place, a substring classifier
//! for one-shot calls put `"overloaded"` in its rate-limit set (so a provider
//! overload rotated accounts — a fault no account switch fixes), a missed
//! resume handshake raised a frontend-only banner, and a child exit carried an
//! exit code and no cause.
//!
//! Every one of those sources is now a [`FailureSignal`]. [`classify`] turns a
//! signal into a [`SessionFailure`] (the `qontinui-types` vocabulary), and
//! [`policy_for`] is the ONE table that says what the runner does about each
//! [`FailureKind`]. Executing that policy is `super::failure_recovery`'s job;
//! this module is pure (the only impurity is the minted failure id) so the
//! whole taxonomy is table-testable.
//!
//! **Confidence is part of the answer.** A structured protocol event or a CLI
//! hook payload STATES a failure, so it is [`FailureConfidence::Confirmed`]; a
//! phrase scraped off the grid, a stderr line or a handshake that never
//! appeared only SUGGESTS one, so it is [`FailureConfidence::Hint`] and its
//! title reads "may have …" until something confirms it.
//!
//! **Unknown is a kind, never success.** A signal that says a failure happened
//! but not which one classifies as [`FailureKind::Unknown`]. A signal that does
//! not say a failure happened at all (a clean exit, a stderr line with no error
//! in it, a rate-limit event reporting `allowed`) classifies as `None`.

use qontinui_types::cli_session::{
    CliProfile, FailureAction, FailureCategory, FailureConfidence, FailureEvidence,
    FailureEvidenceSource, FailureKind, FailureSeverity, RecoveryPolicy, RestoreTier,
    SessionFailure,
};

/// Longest verbatim `reason` a failure carries. Stderr can be arbitrarily long;
/// the reason is for diagnosis, not a log.
const MAX_REASON_CHARS: usize = 500;

/// One observation that an AI session may have failed, from whichever lane saw
/// it. The classifier's only input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureSignal {
    /// A typed failure event on a structured protocol lane (Claude stream-json,
    /// a future Codex app-server lane).
    StructuredEvent {
        /// The provider's typed error code, when the frame carries one — on
        /// Claude's stream-json lane the errored turn's synthetic assistant
        /// frame carries `error: "model_not_found"` etc., the same vocabulary
        /// as the `StopFailure` hook's `error_type` (Phase 2 probe Q3).
        error_code: Option<String>,
        /// The HTTP status the provider API answered, when stated
        /// (`api_error_status`).
        api_status: Option<u16>,
        /// The status of a rate-limit event (`rate_limit_info.status`), when
        /// the frame is one. `allowed` / `allowed_warning` are not failures.
        rate_limit_status: Option<String>,
        /// The human-readable error text, when the frame carries one.
        message: Option<String>,
        /// When the limit resets (RFC 3339), when the frame states it.
        reset_at: Option<String>,
    },
    /// A CLI hook payload — Claude's `StopFailure` carries a typed
    /// `error_type`. The PTY lane's only `Confirmed`-grade failure source.
    Hook {
        /// The hook event name (`StopFailure`).
        event: String,
        /// The typed error the hook reports.
        error_type: String,
    },
    /// The CLI's stderr text.
    Stderr(String),
    /// A phrase the grid scanner matched on a terminal's rendered screen,
    /// taken from `provider`'s own profile (its usage-limit phrases or its
    /// resume-failure markers).
    GridPhrase {
        /// The profile id whose phrase list produced the match.
        provider: String,
        /// The matched phrase, verbatim.
        phrase: String,
    },
    /// The CLI process exited. `None` when the exit status could not be read.
    Exit {
        /// The exit code.
        code: Option<i32>,
    },
    /// A resume by id was typed and the CLI's handshake never appeared.
    HandshakeTimeout,
}

// ============================================================================
// Kind tables
// ============================================================================

/// The typed-error vocabulary shared by Claude's `StopFailure` hook
/// `error_type` and the stream-json errored turn's assistant `error` code.
/// `None` for a code this table does not know — the caller turns that into
/// [`FailureKind::Unknown`], never into success.
pub fn kind_for_error_type(error_type: &str) -> Option<FailureKind> {
    Some(match error_type.trim() {
        "rate_limit" => FailureKind::RateLimited,
        "overloaded" => FailureKind::Overloaded,
        "authentication_failed" | "oauth_org_not_allowed" | "cloud_credential_error" => {
            FailureKind::AuthRequired
        }
        "account_on_hold" => FailureKind::AccessDenied,
        "billing_error" => FailureKind::BudgetExhausted,
        "invalid_request" | "model_not_found" => FailureKind::BadRequest,
        "server_error" => FailureKind::InternalError,
        "max_output_tokens" => FailureKind::ContextExhausted,
        "unknown" => FailureKind::Unknown,
        _ => return None,
    })
}

/// The kind an HTTP status from the provider API implies, when it implies one.
fn kind_for_api_status(status: u16) -> Option<FailureKind> {
    Some(match status {
        429 => FailureKind::RateLimited,
        529 => FailureKind::Overloaded,
        401 => FailureKind::AuthRequired,
        403 => FailureKind::AccessDenied,
        413 => FailureKind::ContextExhausted,
        400 | 404 | 422 => FailureKind::BadRequest,
        500..=599 => FailureKind::InternalError,
        _ => return None,
    })
}

/// The kind an error text names, by the substrings providers and CLIs print.
/// `None` when the text names no failure this table knows.
///
/// This is the classifier's text arm, and the one call `ai_provider::retry`
/// makes: its old `is_rate_limit_error` set is gone, and so is its
/// conflation — `"overloaded"` classifies as [`FailureKind::Overloaded`],
/// whose policy is a backoff on the same account, not a rotation.
///
/// Order matters: an overload or a rate limit is checked before the generic
/// "limit" phrasings, so `"rate limit reached"` is a rate limit and not a
/// quota.
pub fn kind_of_error_text(text: &str) -> Option<FailureKind> {
    let lower = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    if has(&["overloaded", "(529)"]) {
        return Some(FailureKind::Overloaded);
    }
    // A monthly spend limit or an empty credit balance is a budget, and it is
    // per account — another account has its own.
    if has(&["spend limit", "credit balance is too low", "billing_error"]) {
        return Some(FailureKind::BudgetExhausted);
    }
    if has(&["(429)", "rate limit", "rate_limit", "too many requests"]) {
        return Some(FailureKind::RateLimited);
    }
    // The account's usage window is used up. "token limit" is kept here
    // because the one-shot lane always treated it as account capacity.
    if has(&[
        "usage limit",
        "token limit",
        "out of usage",
        "out of extra usage",
    ]) {
        return Some(FailureKind::QuotaExhausted);
    }
    if has(&[
        "invalid api key",
        "invalid x-api-key",
        "authentication_error",
        "not logged in",
        "please run /login",
        "oauth token has expired",
        "(401)",
    ]) {
        return Some(FailureKind::AuthRequired);
    }
    if has(&["permission_error", "(403)", "forbidden"]) {
        return Some(FailureKind::AccessDenied);
    }
    if has(&[
        "prompt is too long",
        "context window",
        "context_length_exceeded",
        "max_output_tokens",
    ]) {
        return Some(FailureKind::ContextExhausted);
    }
    if has(&[
        "invalid_request_error",
        "model_not_found",
        "not_found_error",
        "(400)",
        "(404)",
    ]) {
        return Some(FailureKind::BadRequest);
    }
    if has(&[
        "internal server error",
        "api_error",
        "(500)",
        "(502)",
        "(503)",
        "(504)",
    ]) {
        return Some(FailureKind::InternalError);
    }
    None
}

// ============================================================================
// The recovery table
// ============================================================================

/// Whether a profile's sessions are resumed automatically — the condition
/// under which a lost or exited session is resumed rather than abandoned.
///
/// That is the profile's honest restore tier
/// ([`qontinui_runner_lib::cli_profile::restore_tier`]) being `Full`, not
/// merely its declaring a by-id resume argv. Codex declares `codex resume
/// {id}` but is `TerminalOnly` until an end-to-end resume has been observed
/// (its manifest says why); a by-id argv alone therefore once made a Codex
/// exit `resume_same_id`, and the PTY lane's resume — which is Claude's —
/// relaunched it as `claude --resume <codex id>`.
pub fn profile_is_resumable(profile: &CliProfile) -> bool {
    qontinui_runner_lib::cli_profile::restore_tier(profile) == RestoreTier::Full
}

/// THE recovery table: what the runner does automatically about each kind.
///
/// `resumable` is whether the failed session's profile resumes by id; it only
/// decides the two process-level kinds.
///
/// | kind | policy | why |
/// |---|---|---|
/// | `rate_limited`, `overloaded` | `backoff_then_retry` | waiting fixes both; no account switch does |
/// | `quota_exhausted`, `budget_exhausted` | `migrate_account` | per-account; another account has capacity |
/// | `context_exhausted` | `handoff_new_session` | the conversation no longer fits |
/// | `transport_lost`, `process_exited` | `resume_same_id` iff resumable, else `never` | the conversation is intact on disk |
/// | everything else | `never` | no automatic action fixes it; the actions say what a person can do |
pub fn policy_for(kind: FailureKind, resumable: bool) -> RecoveryPolicy {
    match kind {
        FailureKind::RateLimited | FailureKind::Overloaded => RecoveryPolicy::BackoffThenRetry,
        FailureKind::QuotaExhausted | FailureKind::BudgetExhausted => {
            RecoveryPolicy::MigrateAccount
        }
        FailureKind::ContextExhausted => RecoveryPolicy::HandoffNewSession,
        FailureKind::TransportLost | FailureKind::ProcessExited if resumable => {
            RecoveryPolicy::ResumeSameId
        }
        FailureKind::TransportLost
        | FailureKind::ProcessExited
        | FailureKind::AuthRequired
        | FailureKind::AccessDenied
        | FailureKind::ResumeFailed
        | FailureKind::SpawnFailed
        | FailureKind::BadRequest
        | FailureKind::InternalError
        | FailureKind::Unknown => RecoveryPolicy::Never,
    }
}

/// The actions a surface may offer for a kind, in display order.
pub fn actions_for(kind: FailureKind, resumable: bool) -> Vec<FailureAction> {
    use FailureAction as A;
    match kind {
        FailureKind::RateLimited | FailureKind::Overloaded | FailureKind::InternalError => {
            vec![A::Retry]
        }
        FailureKind::QuotaExhausted | FailureKind::BudgetExhausted | FailureKind::AccessDenied => {
            vec![A::SwitchAccount]
        }
        FailureKind::ContextExhausted => vec![A::NewSession],
        FailureKind::AuthRequired => vec![A::Login],
        FailureKind::TransportLost | FailureKind::ProcessExited if resumable => {
            vec![A::Resume, A::NewSession]
        }
        FailureKind::TransportLost | FailureKind::ProcessExited => vec![A::NewSession],
        FailureKind::ResumeFailed => vec![A::Resume, A::NewSession],
        FailureKind::SpawnFailed => vec![A::Retry],
        FailureKind::BadRequest | FailureKind::Unknown => vec![A::None],
    }
}

/// The coarse category of a kind.
pub fn category_for(kind: FailureKind) -> FailureCategory {
    match kind {
        FailureKind::RateLimited | FailureKind::QuotaExhausted | FailureKind::BudgetExhausted => {
            FailureCategory::Limit
        }
        FailureKind::ContextExhausted => FailureCategory::Context,
        FailureKind::AuthRequired | FailureKind::AccessDenied => FailureCategory::Auth,
        FailureKind::Overloaded | FailureKind::InternalError => FailureCategory::Provider,
        FailureKind::TransportLost | FailureKind::SpawnFailed | FailureKind::ProcessExited => {
            FailureCategory::Process
        }
        FailureKind::ResumeFailed => FailureCategory::Session,
        FailureKind::BadRequest => FailureCategory::Request,
        FailureKind::Unknown => FailureCategory::Unknown,
    }
}

/// How serious a kind is: the two that clear by waiting are warnings, and so is
/// an unclassified failure (it is shown, not dramatised); the rest stop the
/// session until something recovers it.
pub fn severity_for(kind: FailureKind) -> FailureSeverity {
    match kind {
        FailureKind::RateLimited | FailureKind::Overloaded | FailureKind::Unknown => {
            FailureSeverity::Warning
        }
        _ => FailureSeverity::Error,
    }
}

/// The one-line title, worded by confidence: a `Hint` says "may have …".
pub fn title_for(kind: FailureKind, confidence: FailureConfidence) -> &'static str {
    let (confirmed, hint) = match kind {
        FailureKind::RateLimited => ("Rate limited", "May have been rate limited"),
        FailureKind::QuotaExhausted => (
            "Usage quota exhausted",
            "May have exhausted its usage quota",
        ),
        FailureKind::BudgetExhausted => (
            "Spend budget exhausted",
            "May have exhausted its spend budget",
        ),
        FailureKind::ContextExhausted => (
            "Context window exhausted",
            "May have exhausted its context window",
        ),
        FailureKind::AuthRequired => ("Authentication required", "May need to authenticate"),
        FailureKind::AccessDenied => ("Access denied", "May have been denied access"),
        FailureKind::Overloaded => ("Provider overloaded", "Provider may be overloaded"),
        FailureKind::TransportLost => (
            "Connection to the CLI lost",
            "May have lost its connection to the CLI",
        ),
        FailureKind::ResumeFailed => ("Session resume failed", "Session resume may have failed"),
        FailureKind::SpawnFailed => ("CLI failed to start", "CLI may have failed to start"),
        FailureKind::ProcessExited => ("CLI process exited", "CLI process may have exited"),
        FailureKind::BadRequest => ("Request rejected", "Request may have been rejected"),
        FailureKind::InternalError => (
            "Provider internal error",
            "Provider may have hit an internal error",
        ),
        FailureKind::Unknown => (
            "Session failed (cause unknown)",
            "Session may have failed (cause unknown)",
        ),
    };
    match confidence {
        FailureConfidence::Confirmed => confirmed,
        FailureConfidence::Hint => hint,
    }
}

/// The first half of a failure's details: what the kind means.
fn what_happened(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::RateLimited => "The provider is limiting the request rate.",
        FailureKind::QuotaExhausted => "The account's usage quota for its window is used up.",
        FailureKind::BudgetExhausted => "The account's spend or billing budget is used up.",
        FailureKind::ContextExhausted => "The conversation no longer fits the model's context.",
        FailureKind::AuthRequired => "The CLI needs to be authenticated.",
        FailureKind::AccessDenied => "The account or organisation is not allowed to do this.",
        FailureKind::Overloaded => "The provider is overloaded; switching accounts does not help.",
        FailureKind::TransportLost => "The connection to the CLI was lost.",
        FailureKind::ResumeFailed => "The resumed conversation's UI never appeared.",
        FailureKind::SpawnFailed => "The CLI process could not be started.",
        FailureKind::ProcessExited => "The CLI process exited without a more specific cause.",
        FailureKind::BadRequest => "The provider rejected the request as invalid.",
        FailureKind::InternalError => "The provider reported an internal error.",
        FailureKind::Unknown => "A failure was observed but could not be classified.",
    }
}

/// The longer explanation for a kind: what the runner does about it.
fn details_for(kind: FailureKind, policy: RecoveryPolicy) -> String {
    let what = what_happened(kind);
    let then = match policy {
        RecoveryPolicy::Never => "The runner takes no automatic action.",
        // The structured lane's wording: there the runner itself backs off
        // and restarts. A PTY session's CLI retries on its own, and the
        // runner says so instead ([`with_cli_retry`]).
        RecoveryPolicy::BackoffThenRetry => {
            "The runner waits a bounded backoff and retries on the same account."
        }
        RecoveryPolicy::WaitUntilReset => "The runner waits until the limit resets.",
        RecoveryPolicy::MigrateAccount => {
            "The runner confirms the exhaustion and moves the session to another account."
        }
        RecoveryPolicy::LoginThenResume => "The runner resumes the session after a login.",
        RecoveryPolicy::ResumeSameId => "The runner resumes the same session id.",
        RecoveryPolicy::HandoffNewSession => {
            "The runner hands the work off to a continuation session."
        }
    };
    format!("{what} {then}")
}

// ============================================================================
// The classifier
// ============================================================================

/// What one signal says, before it is dressed as a [`SessionFailure`].
struct Verdict {
    kind: FailureKind,
    source: FailureEvidenceSource,
    confidence: FailureConfidence,
    reason: Option<String>,
    reset_at: Option<String>,
}

/// Classify one signal against the failed session's profile. `None` when the
/// signal does not report a failure at all.
pub fn classify(signal: &FailureSignal, profile: &CliProfile) -> Option<SessionFailure> {
    classify_for(signal, &profile.id, Some(profile))
}

/// [`classify`] for a session whose provider has no profile (a record naming a
/// provider this runner does not know). Every profile-dependent arm answers
/// [`FailureKind::Unknown`], and nothing is resumable.
pub fn classify_for(
    signal: &FailureSignal,
    provider: &str,
    profile: Option<&CliProfile>,
) -> Option<SessionFailure> {
    let verdict = verdict_for(signal, provider, profile)?;
    let resumable = profile.is_some_and(profile_is_resumable);
    let policy = policy_for(verdict.kind, resumable);
    Some(SessionFailure {
        id: uuid::Uuid::new_v4().to_string(),
        kind: verdict.kind,
        category: category_for(verdict.kind),
        severity: severity_for(verdict.kind),
        title: title_for(verdict.kind, verdict.confidence).to_string(),
        details: Some(details_for(verdict.kind, policy)),
        reason: verdict.reason.map(truncate_reason),
        provider: provider.to_string(),
        account: None,
        turn_id: None,
        reset_at: verdict.reset_at,
        evidence: FailureEvidence {
            source: verdict.source,
            confidence: verdict.confidence,
        },
        actions: actions_for(verdict.kind, resumable),
        recovery_policy: policy,
    })
}

fn verdict_for(
    signal: &FailureSignal,
    provider: &str,
    profile: Option<&CliProfile>,
) -> Option<Verdict> {
    let confirmed = |kind, source, reason| Verdict {
        kind,
        source,
        confidence: FailureConfidence::Confirmed,
        reason,
        reset_at: None,
    };
    let hint = |kind, source, reason| Verdict {
        kind,
        source,
        confidence: FailureConfidence::Hint,
        reason,
        reset_at: None,
    };
    match signal {
        FailureSignal::StructuredEvent {
            error_code,
            api_status,
            rate_limit_status,
            message,
            reset_at,
        } => {
            // A rate-limit event is a status report every turn; only a
            // non-allowed status is a failure.
            let limit_kind = match rate_limit_status.as_deref().map(str::trim) {
                None => None,
                Some("allowed" | "allowed_warning") => {
                    if error_code.is_none() && api_status.is_none() && message.is_none() {
                        return None;
                    }
                    None
                }
                Some("rejected") => Some(FailureKind::QuotaExhausted),
                // A status this table does not know is a failure it cannot
                // name, not an all-clear.
                Some(_) => Some(FailureKind::Unknown),
            };
            let kind = error_code
                .as_deref()
                .map(|c| kind_for_error_type(c).unwrap_or(FailureKind::Unknown))
                .or_else(|| api_status.and_then(kind_for_api_status))
                .or(limit_kind)
                .or_else(|| message.as_deref().and_then(kind_of_error_text))
                .unwrap_or(FailureKind::Unknown);
            let reason = error_code
                .clone()
                .or_else(|| rate_limit_status.clone())
                .or_else(|| api_status.map(|s| format!("HTTP {s}")))
                .or_else(|| message.clone());
            let mut v = confirmed(kind, FailureEvidenceSource::StructuredEvent, reason);
            v.reset_at = reset_at.clone();
            Some(v)
        }
        FailureSignal::Hook { event, error_type } => Some(confirmed(
            kind_for_error_type(error_type).unwrap_or(FailureKind::Unknown),
            FailureEvidenceSource::Hook,
            Some(format!("{event}: {error_type}")),
        )),
        FailureSignal::Stderr(text) => {
            // The profile's own usage-limit phrasings first: they are how this
            // CLI words an exhausted quota.
            let normalized = normalize(text);
            let quota = profile.is_some_and(|p| {
                p.usage_limit_phrases
                    .iter()
                    .any(|ph| normalized.contains(&ph.to_lowercase()))
            });
            let kind = if quota {
                // A generic error text still wins when it is more specific
                // (a rate limit also reads "limit reached").
                match kind_of_error_text(text) {
                    Some(k @ (FailureKind::RateLimited | FailureKind::Overloaded)) => k,
                    Some(FailureKind::BudgetExhausted) => FailureKind::BudgetExhausted,
                    _ => FailureKind::QuotaExhausted,
                }
            } else {
                kind_of_error_text(text)?
            };
            Some(hint(
                kind,
                FailureEvidenceSource::Stderr,
                last_nonempty_line(text),
            ))
        }
        FailureSignal::GridPhrase {
            provider: phrase_provider,
            phrase,
        } => {
            let reason = Some(phrase.clone());
            // A phrase from another CLI's list says nothing this profile can
            // interpret.
            let Some(profile) = profile.filter(|_| phrase_provider == provider) else {
                return Some(hint(
                    FailureKind::Unknown,
                    FailureEvidenceSource::GridScrape,
                    reason,
                ));
            };
            let kind = if is_resume_failure_marker(profile, phrase) {
                FailureKind::ResumeFailed
            } else if profile
                .usage_limit_phrases
                .iter()
                .any(|ph| ph.eq_ignore_ascii_case(phrase.trim()))
            {
                FailureKind::QuotaExhausted
            } else {
                FailureKind::Unknown
            };
            Some(hint(kind, FailureEvidenceSource::GridScrape, reason))
        }
        FailureSignal::Exit { code: Some(0) } => None,
        FailureSignal::Exit { code } => Some(confirmed(
            FailureKind::ProcessExited,
            FailureEvidenceSource::ExitStatus,
            Some(match code {
                Some(c) => format!("exit code {c}"),
                None => "exit status unknown".to_string(),
            }),
        )),
        FailureSignal::HandshakeTimeout => Some(hint(
            FailureKind::ResumeFailed,
            FailureEvidenceSource::HandshakeTimeout,
            Some("resume handshake never appeared".to_string()),
        )),
    }
}

/// The first line of `screen` that is one of `profile`'s resume-failure
/// markers (`No conversation found with session ID: …`), trimmed. `None` when
/// no line is.
pub fn resume_failure_line(profile: &CliProfile, screen: &str) -> Option<String> {
    screen
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && is_resume_failure_marker(profile, line))
        .map(str::to_string)
}

/// `failure`, re-stated for a recovery the runner is NOT going to run: its
/// policy becomes [`RecoveryPolicy::Never`] and its details say why, so a
/// surface never promises an automatic action the runner declined (the cap
/// spent, the runner draining, a mechanism that cannot drive this CLI). The
/// actions are kept — they are what a person can still do.
pub fn with_manual_recovery(mut failure: SessionFailure, why: &str) -> SessionFailure {
    failure.recovery_policy = RecoveryPolicy::Never;
    failure.details = Some(format!(
        "{} The runner is taking no automatic action: {why}.",
        what_happened(failure.kind)
    ));
    failure
}

/// `failure` on a PTY session whose recovery is the CLI's own retry: the
/// runner does nothing but keep the account, and the details say exactly
/// that — never that the runner waits or retries.
pub fn with_cli_retry(mut failure: SessionFailure) -> SessionFailure {
    failure.details = Some(format!(
        "{} The CLI retries on its own; the account is kept.",
        what_happened(failure.kind)
    ));
    failure
}

/// Does `phrase` match one of the profile's resume-failure markers (its
/// substring list or its regexes, case-insensitively — the manifest's dialect)?
fn is_resume_failure_marker(profile: &CliProfile, phrase: &str) -> bool {
    let lower = phrase.to_lowercase();
    profile
        .handshake
        .failure
        .iter()
        .any(|s| !s.is_empty() && lower.contains(&s.to_lowercase()))
        || profile.handshake.failure_regex.iter().any(|src| {
            regex::RegexBuilder::new(src)
                .case_insensitive(true)
                .build()
                .is_ok_and(|re| re.is_match(phrase))
        })
}

/// Lowercase and collapse whitespace runs, the same normalization the grid
/// scanners apply before a phrase match.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn last_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn truncate_reason(reason: String) -> String {
    if reason.chars().count() <= MAX_REASON_CHARS {
        return reason;
    }
    let mut out: String = reason.chars().take(MAX_REASON_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> &'static CliProfile {
        qontinui_runner_lib::cli_profile::profile_for(qontinui_runner_lib::cli_profile::claude::ID)
            .expect("claude profile")
    }

    fn kind_of(signal: FailureSignal) -> Option<FailureKind> {
        classify(&signal, profile()).map(|f| f.kind)
    }

    fn stderr(s: &str) -> FailureSignal {
        FailureSignal::Stderr(s.to_string())
    }

    /// The first regression test the plan names: `ai_provider/retry.rs` put
    /// `"overloaded"` in its rate-limit set, so an overload rotated accounts.
    /// Overload and rate limit are distinct kinds, and neither rotates.
    #[test]
    fn overloaded_is_not_a_rate_limit_and_neither_migrates_the_account() {
        for text in [
            "API error (529): Overloaded",
            "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}",
        ] {
            assert_eq!(
                kind_of_error_text(text),
                Some(FailureKind::Overloaded),
                "{text}"
            );
        }
        for text in ["API error (429): rate limit", "Too Many Requests"] {
            assert_eq!(
                kind_of_error_text(text),
                Some(FailureKind::RateLimited),
                "{text}"
            );
        }
        assert_ne!(FailureKind::Overloaded, FailureKind::RateLimited);
        for kind in [FailureKind::Overloaded, FailureKind::RateLimited] {
            assert_eq!(policy_for(kind, true), RecoveryPolicy::BackoffThenRetry);
            assert_ne!(policy_for(kind, true), RecoveryPolicy::MigrateAccount);
        }
        // Account capacity does migrate.
        assert_eq!(
            policy_for(FailureKind::QuotaExhausted, true),
            RecoveryPolicy::MigrateAccount
        );
    }

    /// The sibling plan's `StopFailure` `error_type` → kind table, row by row.
    #[test]
    fn stop_failure_error_types_map_through_the_one_table_as_confirmed_hook_evidence() {
        let table = [
            ("rate_limit", FailureKind::RateLimited),
            ("overloaded", FailureKind::Overloaded),
            ("authentication_failed", FailureKind::AuthRequired),
            ("oauth_org_not_allowed", FailureKind::AuthRequired),
            ("cloud_credential_error", FailureKind::AuthRequired),
            ("account_on_hold", FailureKind::AccessDenied),
            ("billing_error", FailureKind::BudgetExhausted),
            ("invalid_request", FailureKind::BadRequest),
            ("model_not_found", FailureKind::BadRequest),
            ("server_error", FailureKind::InternalError),
            ("max_output_tokens", FailureKind::ContextExhausted),
            ("unknown", FailureKind::Unknown),
            // A type the table does not know is unknown, never success.
            ("brand_new_error", FailureKind::Unknown),
        ];
        for (error_type, kind) in table {
            let f = classify(
                &FailureSignal::Hook {
                    event: "StopFailure".into(),
                    error_type: error_type.into(),
                },
                profile(),
            )
            .unwrap_or_else(|| panic!("{error_type}: a hook failure is always a failure"));
            assert_eq!(f.kind, kind, "{error_type}");
            assert_eq!(f.evidence.source, FailureEvidenceSource::Hook);
            assert_eq!(f.evidence.confidence, FailureConfidence::Confirmed);
            assert_eq!(f.provider, profile().id);
        }
    }

    /// Scraped and inferred signals are hints and say "may have"; stated ones
    /// are confirmed.
    #[test]
    fn grid_stderr_and_timeout_are_hints_structured_hook_exit_are_confirmed() {
        let p = profile();
        let grid = classify(
            &FailureSignal::GridPhrase {
                provider: p.id.clone(),
                phrase: "usage limit reached".into(),
            },
            p,
        )
        .unwrap();
        assert_eq!(grid.kind, FailureKind::QuotaExhausted);
        assert_eq!(grid.evidence.confidence, FailureConfidence::Hint);
        assert_eq!(grid.evidence.source, FailureEvidenceSource::GridScrape);
        assert!(grid.title.starts_with("May "), "{}", grid.title);

        let err = classify(&stderr("Error: API error (429) rate limit"), p).unwrap();
        assert_eq!(err.evidence.confidence, FailureConfidence::Hint);
        assert_eq!(err.evidence.source, FailureEvidenceSource::Stderr);

        let timeout = classify(&FailureSignal::HandshakeTimeout, p).unwrap();
        assert_eq!(timeout.kind, FailureKind::ResumeFailed);
        assert_eq!(timeout.evidence.confidence, FailureConfidence::Hint);
        assert_eq!(timeout.title, "Session resume may have failed");

        let structured = classify(
            &FailureSignal::StructuredEvent {
                error_code: Some("model_not_found".into()),
                api_status: Some(404),
                rate_limit_status: None,
                message: None,
                reset_at: None,
            },
            p,
        )
        .unwrap();
        assert_eq!(structured.kind, FailureKind::BadRequest);
        assert_eq!(structured.evidence.confidence, FailureConfidence::Confirmed);
        assert!(!structured.title.starts_with("May "));

        let exit = classify(&FailureSignal::Exit { code: Some(137) }, p).unwrap();
        assert_eq!(exit.kind, FailureKind::ProcessExited);
        assert_eq!(exit.evidence.confidence, FailureConfidence::Confirmed);
    }

    /// Every grid fixture the manifest's shared screen fixtures mark as a
    /// usage limit classifies as a quota hint through the profile's own
    /// phrases, and every resume-failure marker as `resume_failed`.
    #[test]
    fn profile_phrases_classify_through_the_manifest() {
        let p = profile();
        for phrase in &p.usage_limit_phrases {
            assert_eq!(
                kind_of(FailureSignal::GridPhrase {
                    provider: p.id.clone(),
                    phrase: phrase.clone(),
                }),
                Some(FailureKind::QuotaExhausted),
                "{phrase}"
            );
        }
        for phrase in [
            "No conversation found with session ID: x",
            "Select a conversation to resume",
        ] {
            assert_eq!(
                kind_of(FailureSignal::GridPhrase {
                    provider: p.id.clone(),
                    phrase: phrase.into(),
                }),
                Some(FailureKind::ResumeFailed),
                "{phrase}"
            );
        }
        // A phrase attributed to another provider is not interpretable here.
        assert_eq!(
            kind_of(FailureSignal::GridPhrase {
                provider: "someone-else".into(),
                phrase: "usage limit reached".into(),
            }),
            Some(FailureKind::Unknown)
        );
    }

    /// The Phase 2 probe's errored turn (Q3): `subtype: success` +
    /// `is_error: true`, `api_error_status: 404`, assistant `error:
    /// model_not_found`, stderr `[claude-code:unrecognized_model]`.
    #[test]
    fn the_recorded_errored_turn_is_a_confirmed_bad_request() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cli_protocol/claude/2.1.285/errored_turn_invalid_model.ndjson");
        let text = std::fs::read_to_string(&fixture).expect("errored-turn fixture");
        let mut code = None;
        let mut status = None;
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            match v.get("type").and_then(|t| t.as_str()) {
                Some("assistant") => {
                    code = v.get("error").and_then(|e| e.as_str()).map(str::to_string);
                }
                Some("result") => {
                    assert_eq!(v.get("is_error"), Some(&serde_json::Value::Bool(true)));
                    status = v
                        .get("api_error_status")
                        .and_then(|s| s.as_u64())
                        .map(|s| s as u16);
                }
                _ => {}
            }
        }
        let f = classify(
            &FailureSignal::StructuredEvent {
                error_code: code,
                api_status: status,
                rate_limit_status: None,
                message: None,
                reset_at: None,
            },
            profile(),
        )
        .unwrap();
        assert_eq!(f.kind, FailureKind::BadRequest);
        assert_eq!(f.recovery_policy, RecoveryPolicy::Never);
        assert_eq!(f.reason.as_deref(), Some("model_not_found"));
    }

    /// Non-failures are `None`; an observed-but-unnamed failure is `Unknown`.
    #[test]
    fn unknown_is_a_kind_and_non_failures_are_none() {
        let p = profile();
        assert_eq!(kind_of(FailureSignal::Exit { code: Some(0) }), None);
        assert_eq!(kind_of(stderr("warning: something harmless")), None);
        assert_eq!(kind_of(stderr("")), None);
        // The per-turn rate-limit event reporting `allowed` (probe Q2).
        for status in ["allowed", "allowed_warning"] {
            assert_eq!(
                kind_of(FailureSignal::StructuredEvent {
                    error_code: None,
                    api_status: None,
                    rate_limit_status: Some(status.into()),
                    message: None,
                    reset_at: None,
                }),
                None,
                "{status}"
            );
        }
        let rejected = classify(
            &FailureSignal::StructuredEvent {
                error_code: None,
                api_status: None,
                rate_limit_status: Some("rejected".into()),
                message: None,
                reset_at: Some("2026-09-30T12:00:00Z".into()),
            },
            p,
        )
        .unwrap();
        assert_eq!(rejected.kind, FailureKind::QuotaExhausted);
        assert_eq!(rejected.reset_at.as_deref(), Some("2026-09-30T12:00:00Z"));
        assert_eq!(
            kind_of(FailureSignal::StructuredEvent {
                error_code: None,
                api_status: None,
                rate_limit_status: Some("some_new_status".into()),
                message: None,
                reset_at: None,
            }),
            Some(FailureKind::Unknown)
        );
        // An exit whose status could not be read is still an exit.
        assert_eq!(
            kind_of(FailureSignal::Exit { code: None }),
            Some(FailureKind::ProcessExited)
        );
        // A structured error with nothing recognisable in it.
        assert_eq!(
            kind_of(FailureSignal::StructuredEvent {
                error_code: None,
                api_status: None,
                rate_limit_status: None,
                message: Some("???".into()),
                reset_at: None,
            }),
            Some(FailureKind::Unknown)
        );
        let unknown = classify(
            &FailureSignal::Hook {
                event: "StopFailure".into(),
                error_type: "unknown".into(),
            },
            p,
        )
        .unwrap();
        assert_eq!(unknown.recovery_policy, RecoveryPolicy::Never);
        assert_eq!(unknown.actions, vec![FailureAction::None]);
    }

    /// The stderr arm: the one-shot and structured lanes' phrasings, and the
    /// profile's own usage-limit phrases.
    #[test]
    fn stderr_text_table() {
        let table = [
            (
                "Claude AI usage limit reached|1790727000",
                FailureKind::QuotaExhausted,
            ),
            (
                "5-hour limit reached ∙ resets 3am",
                FailureKind::QuotaExhausted,
            ),
            (
                "You've hit your monthly spend limit",
                FailureKind::BudgetExhausted,
            ),
            ("Credit balance is too low", FailureKind::BudgetExhausted),
            ("rate limit reached for requests", FailureKind::RateLimited),
            (
                "API Error: 529 {\"type\":\"overloaded_error\"}",
                FailureKind::Overloaded,
            ),
            (
                "Invalid API key · Please run /login",
                FailureKind::AuthRequired,
            ),
            ("Prompt is too long", FailureKind::ContextExhausted),
            (
                "API error (500): Internal server error",
                FailureKind::InternalError,
            ),
            (
                "API error (400): invalid_request_error",
                FailureKind::BadRequest,
            ),
        ];
        for (text, kind) in table {
            assert_eq!(kind_of(stderr(text)), Some(kind), "{text}");
        }
        // The reason is the last non-empty line, verbatim.
        let f = classify(&stderr("noise\nAPI error (429): slow down\n\n"), profile()).unwrap();
        assert_eq!(f.reason.as_deref(), Some("API error (429): slow down"));
    }

    /// Process-level kinds resume only when the profile resumes by id.
    #[test]
    fn resume_same_id_needs_a_resumable_profile() {
        let p = profile();
        assert!(profile_is_resumable(p));
        let f = classify(&FailureSignal::Exit { code: Some(1) }, p).unwrap();
        assert_eq!(f.recovery_policy, RecoveryPolicy::ResumeSameId);
        assert_eq!(
            f.actions,
            vec![FailureAction::Resume, FailureAction::NewSession]
        );

        let mut no_resume = p.clone();
        no_resume.resume = qontinui_types::cli_session::ResumeSpec::Unknown;
        let f = classify(&FailureSignal::Exit { code: Some(1) }, &no_resume).unwrap();
        assert_eq!(f.recovery_policy, RecoveryPolicy::Never);
        assert_eq!(f.actions, vec![FailureAction::NewSession]);

        // C2: a by-id resume argv is not enough — the tier must be Full.
        // Codex declares `codex resume {id}` and is TerminalOnly, so its exit
        // is never auto-resumed (the PTY lane's resume is Claude's).
        let codex = qontinui_runner_lib::cli_profile::profile_for(
            qontinui_runner_lib::cli_profile::codex::ID,
        )
        .unwrap();
        assert!(matches!(
            codex.resume,
            qontinui_types::cli_session::ResumeSpec::ByIdArgv { .. }
        ));
        assert!(!profile_is_resumable(codex));
        let f = classify(&FailureSignal::Exit { code: Some(1) }, codex).unwrap();
        assert_eq!(f.recovery_policy, RecoveryPolicy::Never);
        assert_eq!(f.actions, vec![FailureAction::NewSession]);

        // An unprofiled provider resumes nothing and interprets no phrase.
        let f = classify_for(&FailureSignal::Exit { code: Some(1) }, "mystery", None).unwrap();
        assert_eq!(f.recovery_policy, RecoveryPolicy::Never);
        assert_eq!(f.provider, "mystery");
        let f = classify_for(
            &FailureSignal::GridPhrase {
                provider: "mystery".into(),
                phrase: "usage limit reached".into(),
            },
            "mystery",
            None,
        )
        .unwrap();
        assert_eq!(f.kind, FailureKind::Unknown);
    }

    /// The whole table is total and self-consistent: every kind has a policy,
    /// actions, a category and titles in both confidences.
    #[test]
    fn every_kind_is_covered() {
        use FailureKind as K;
        let all = [
            K::RateLimited,
            K::QuotaExhausted,
            K::BudgetExhausted,
            K::ContextExhausted,
            K::AuthRequired,
            K::AccessDenied,
            K::Overloaded,
            K::TransportLost,
            K::ResumeFailed,
            K::SpawnFailed,
            K::ProcessExited,
            K::BadRequest,
            K::InternalError,
            K::Unknown,
        ];
        for kind in all {
            for resumable in [true, false] {
                assert!(!actions_for(kind, resumable).is_empty(), "{kind:?}");
                let _ = policy_for(kind, resumable);
            }
            assert_ne!(
                title_for(kind, FailureConfidence::Confirmed),
                title_for(kind, FailureConfidence::Hint)
            );
            assert!(
                title_for(kind, FailureConfidence::Hint).starts_with("May ")
                    || title_for(kind, FailureConfidence::Hint).contains(" may ")
            );
            let _ = category_for(kind);
        }
        assert_eq!(policy_for(K::AuthRequired, true), RecoveryPolicy::Never);
        assert_eq!(
            actions_for(K::AuthRequired, true),
            vec![FailureAction::Login]
        );
        assert_eq!(
            policy_for(K::ContextExhausted, true),
            RecoveryPolicy::HandoffNewSession
        );
        assert_eq!(policy_for(K::Unknown, true), RecoveryPolicy::Never);
    }

    /// A declined recovery is never announced as the automatic one: the
    /// policy reads `never`, the details say why, the actions stay.
    #[test]
    fn a_declined_recovery_is_stated_as_manual() {
        let f = classify(&FailureSignal::Exit { code: Some(1) }, profile()).unwrap();
        assert_eq!(f.recovery_policy, RecoveryPolicy::ResumeSameId);
        let manual = with_manual_recovery(f.clone(), "its automatic-resume cap is spent");
        assert_eq!(manual.recovery_policy, RecoveryPolicy::Never);
        let details = manual.details.unwrap();
        assert!(details.contains("cap is spent"), "{details}");
        assert!(
            !details.contains("resumes the same session id"),
            "{details}"
        );
        assert_eq!(manual.actions, f.actions);
        assert_eq!(manual.id, f.id);
    }

    #[test]
    fn resume_failure_lines_are_found_on_a_screen() {
        let screen = "some output\n\n  No conversation found with session ID: abc  \n$ ";
        assert_eq!(
            resume_failure_line(profile(), screen).as_deref(),
            Some("No conversation found with session ID: abc")
        );
        assert_eq!(resume_failure_line(profile(), "all fine\n$ "), None);
    }

    #[test]
    fn a_long_reason_is_truncated_on_a_char_boundary() {
        let long = "é".repeat(MAX_REASON_CHARS + 10);
        let out = truncate_reason(long);
        assert_eq!(out.chars().count(), MAX_REASON_CHARS + 1);
        assert!(out.ends_with('…'));
    }
}
