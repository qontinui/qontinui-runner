//! Render the runner-injected session prompts from coord's operator-editable
//! `session_briefing` documents, with a compiled-in fallback.
//!
//! Plan `2026-08-20-runner-session-briefing-versioned-and-operator-editable`,
//! Phase 3 + the runner half of Phase 4.
//!
//! # What this module is
//!
//! Two prompts the runner injects into sessions it hosts used to be Rust string
//! literals: [`crate::terminal::runner_context`]'s briefing and
//! [`crate::mcp::ai_session`]'s rules block. They now live in coord as
//! `(kind = session_briefing, name)` prompt documents, are refreshed by
//! [`crate::mcp::fleet_policy_poller`] into a process-global cache, and are
//! RENDERED here.
//!
//! This module owns the three parts of that render that are pure and therefore
//! testable on their own:
//!
//! 1. [`substitute`] — the CLOSED placeholder vocabulary
//!    (`{{runner_api_base}}`, `{{coord_http_base}}`). A surviving `{{token}}` is
//!    dropped with a warning, never shipped into a system prompt.
//! 2. [`validate_body`] — the RENDER-TIME invariant guard.
//! 3. [`Provenance`] — the line-2 label saying which text was actually used.
//!
//! # Why validation happens HERE as well as in coord
//!
//! Coord validates the same invariants at write time (Phase 2 step 6/7). That
//! is not sufficient on its own: coord has more than one write door, a cache
//! file on this disk can be edited directly, and a runner that renders whatever
//! it is handed has no structural guarantee left at the exact point where the
//! prompt is built. So a body that is over-size, carries an unknown
//! `{{token}}`, opens with a forged `[source: …]` / `[briefing: …]` marker
//! line, names coord's OPERATOR door, or carries tenant/agent identity is
//! REFUSED here, logged once, and the compiled-in builtin is rendered instead
//! with provenance `builtin-fallback (rejected coord v<N>)`.
//!
//! # The marker contract is untouched
//!
//! Line 1 of a rendered block stays BYTE-IDENTICAL to the source marker
//! ([`crate::terminal::RUNNER_CONTEXT_SOURCE_MARKER`] /
//! [`crate::mcp::ai_session::AI_SESSION_SOURCE_MARKER`]). Provenance goes on
//! its own SECOND line. Several tests across `terminal` and `mcp::ai_session`
//! assert line-1 EQUALITY, and `/whereami` parses line 1 for the spawn SHA with
//! no shape-guard, so a same-line suffix would silently corrupt an external
//! parser rather than fail loudly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use tracing::warn;

use crate::mcp::fleet_policy_poller::{
    self, BriefingProvenance, BRIEFING_AI_SESSION_RULES, BRIEFING_KIND,
    BRIEFING_PLAN_CAPTURE_CLAUSE, BRIEFING_RUNNER_SESSION,
};
use crate::mcp::types::{ApiResponse, ApiState};

// ===========================================================================
// The closed placeholder vocabulary
// ===========================================================================

/// The runner's own loopback API base, `http://127.0.0.1:<api_port>`.
pub(crate) const PLACEHOLDER_RUNNER_API_BASE: &str = "runner_api_base";

/// The coord HTTP base this runner talks to.
pub(crate) const PLACEHOLDER_COORD_HTTP_BASE: &str = "coord_http_base";

/// Every `{{token}}` an editable body may carry. CLOSED — coord rejects an
/// unknown one at write time and [`validate_body`] rejects it again here, so a
/// new placeholder is a deliberate two-ended change rather than a silent
/// literal in somebody's system prompt.
pub(crate) const PLACEHOLDERS: [&str; 2] =
    [PLACEHOLDER_RUNNER_API_BASE, PLACEHOLDER_COORD_HTTP_BASE];

/// Hard ceiling on an editable body. It lands in the system prompt of EVERY
/// session this runner hosts, so an unbounded body is a per-session token cost
/// with no ceiling.
///
/// A CEILING, never a target: the pull-first, protocol-and-links-only contract
/// the briefing follows means a body anywhere near this size is already wrong.
///
/// Measured on the RAW body, before substitution. A placeholder expands to a
/// URL, so the rendered text can exceed this by a few hundred bytes — bounded
/// by the placeholder count, which is itself bounded by the raw size.
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024;

/// Coord's OPERATOR door. A body naming it re-opens the bug plan
/// `2026-08-08-runner-enforced-policy-pull` Phase 1.8 fixed: the route 403s the
/// device JWT a session carries, so it is an escape hatch that fails exactly
/// when it is needed.
///
/// Matched as a FULL path segment — `/coord/agent-prompt-documents` (the
/// correct, agent door) CONTAINS `prompt-documents`, so a looser match would
/// reject every correct body.
pub(crate) const OPERATOR_DOOR_PATH: &str = "/coord/prompt-documents";

/// Identity-shaped keys a briefing must never carry. Named-field half of the
/// RCE-class invariant (a prompt must never cross tenants); the UUID-shape scan
/// in [`contains_uuid_shaped_token`] is the other half.
const IDENTITY_KEYS: [&str; 10] = [
    "tenant_id",
    "tenantId",
    "organization_id",
    "organizationId",
    "agent_id",
    "agentId",
    "device_id",
    "deviceId",
    "scope_key",
    "scopeKey",
];

// ===========================================================================
// Provenance — the line-2 honesty mechanism
// ===========================================================================

/// Where a rendered block's text actually came from.
///
/// This is the WHOLE honesty mechanism of the plan: any transcript, and any
/// session running `printenv QONTINUI_RUNNER_CONTEXT | head -2`, shows exactly
/// which text was in force. The one thing that must never happen is claiming a
/// coord version while serving the builtin, which is why the rejected arm
/// carries the refused version rather than collapsing into plain `builtin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// A coord body, confirmed against coord by a live poll in this process.
    Coord { name: String, version: i64 },
    /// A coord body restored from the on-disk last-good and not yet
    /// re-confirmed — the runner may be offline or unpaired.
    Cached { version: i64 },
    /// The compiled-in fallback: no document cached, or a poisoned lock.
    Builtin,
    /// The compiled-in fallback BECAUSE the cached coord body failed
    /// [`validate_body`]. Names the version it refused.
    BuiltinRejected { version: i64 },
}

/// Version `0` is UNKNOWN, never a generation number.
///
/// It is what a coord list row carrying no `current_version` decodes to and
/// what a persisted cache entry written by a build predating the field carries.
/// `fleet_policy_poller`'s version gate already refuses to read `0 == 0` as
/// "current" for exactly this reason, and says why in a comment naming the
/// string this function exists to stop: printing `[briefing: coord … v0]` for
/// text whose generation the runner cannot state is the "claiming a coord
/// version" lie the plan forbids, in its quietest form.
fn known_version(version: i64) -> Option<i64> {
    (version != 0).then_some(version)
}

/// How a version renders on the `coord` arm of [`Provenance::describe`]: `v7`,
/// or the honest absence when the runner cannot state which generation it
/// holds. The other two arms already carry a parenthetical and fold the
/// absence into it rather than stacking a second one.
fn version_suffix(version: i64) -> String {
    match known_version(version) {
        Some(v) => format!("v{v}"),
        None => "(version unknown)".to_string(),
    }
}

impl Provenance {
    /// The bracket-free description, e.g. `coord session_briefing/runner-session v7`.
    ///
    /// A version of `0` renders as `(version unknown)` rather than `v0` — see
    /// [`known_version`].
    pub(crate) fn describe(&self) -> String {
        match self {
            Provenance::Coord { name, version } => {
                format!("coord {BRIEFING_KIND}/{name} {}", version_suffix(*version))
            }
            // The two parentheticals collapse into one rather than reading
            // `cached (version unknown) (stale)` — this is a line humans read
            // out of transcripts.
            Provenance::Cached { version } => match known_version(*version) {
                Some(v) => format!("cached v{v} (stale)"),
                None => "cached (version unknown, stale)".to_string(),
            },
            Provenance::Builtin => "builtin-fallback".to_string(),
            Provenance::BuiltinRejected { version } => match known_version(*version) {
                Some(v) => format!("builtin-fallback (rejected coord v{v})"),
                None => "builtin-fallback (rejected coord body of unknown version)".to_string(),
            },
        }
    }

    /// The line-2 label, e.g. `[briefing: coord session_briefing/runner-session v7]`.
    pub(crate) fn line(&self) -> String {
        format!("[briefing: {}]", self.describe())
    }

    /// The coarse token the visibility route reports: `coord` | `cached` |
    /// `builtin`. A REJECTED body reports `builtin`, because builtin is what
    /// the session got — the refused version stays visible in [`describe`].
    ///
    /// [`describe`]: Provenance::describe
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Provenance::Coord { .. } => "coord",
            Provenance::Cached { .. } => "cached",
            Provenance::Builtin | Provenance::BuiltinRejected { .. } => "builtin",
        }
    }

    /// The document version whose text was RENDERED, or `None` when the
    /// builtin was. A rejected version is deliberately NOT reported here — it
    /// was not rendered, and reporting it would be the exact "claiming a coord
    /// version while serving the builtin" lie the plan forbids.
    ///
    /// `None` ALSO for a document-backed block whose version is `0`
    /// ([`known_version`]), so `GET /session-briefing` reports the same absence
    /// to a `curl` reader that the settings panel already renders as
    /// `— (unknown)`. The two must not disagree: they describe one fact.
    pub(crate) fn rendered_version(&self) -> Option<i64> {
        match self {
            Provenance::Coord { version, .. } | Provenance::Cached { version } => {
                known_version(*version)
            }
            Provenance::Builtin | Provenance::BuiltinRejected { .. } => None,
        }
    }
}

/// One resolved block: the text to inject, plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedBlock {
    /// The substituted body — WITHOUT any marker or provenance line. Callers
    /// own their own marker contract.
    pub(crate) text: String,
    pub(crate) provenance: Provenance,
    /// When the rendered body was last confirmed against coord (RFC 3339), or
    /// `None` for the builtin.
    pub(crate) fetched_at: Option<String>,
}

// ===========================================================================
// Substitution (pure)
// ===========================================================================

/// Substitute the closed placeholder vocabulary into an editable body.
///
/// A surviving `{{token}}` — one coord's write-time validator should have
/// rejected, so its presence means a body reached the cache another way — is
/// DROPPED with a warning rather than shipped. A literal `{{tenant_id}}` in an
/// agent's system prompt is worse than a gap in a sentence.
///
/// Note single braces are untouched: today's briefing legitimately contains
/// `{kind}` / `{name}` as literal URL-template text.
pub(crate) fn substitute(body: &str, runner_api_base: &str, coord_http_base: &str) -> String {
    let mut out = body
        .replace(
            &format!("{{{{{PLACEHOLDER_RUNNER_API_BASE}}}}}"),
            runner_api_base,
        )
        .replace(
            &format!("{{{{{PLACEHOLDER_COORD_HTTP_BASE}}}}}"),
            coord_http_base,
        );

    // Every remaining `{{…}}` is unknown by construction (the two known ones
    // are gone above). Each pass strictly shrinks the string, so this
    // terminates.
    while let Some(open) = out.find("{{") {
        let Some(rel_close) = out[open + 2..].find("}}") else {
            break;
        };
        let close = open + 2 + rel_close;
        let token = out[open + 2..close].to_string();
        warn!(
            token = %token,
            "session_briefing: dropping unknown placeholder from a rendered prompt"
        );
        out.replace_range(open..close + 2, "");
    }

    out
}

/// Every `{{token}}` in `body`, in order.
fn placeholder_tokens(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break;
        };
        out.push(after[..close].to_string());
        rest = &after[close + 2..];
    }
    out
}

// ===========================================================================
// The render-time invariant guard
// ===========================================================================

/// Is `token` shaped like a UUID (`8-4-4-4-12` hex)?
///
/// Allocation-free: this runs once per dash-joined word of a body that may be
/// 16 KiB, on every session spawn.
fn is_uuid_shaped(token: &str) -> bool {
    const WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut parts = token.split('-');
    for width in WIDTHS {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != width || !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

/// Does `body` carry a UUID-shaped token anywhere?
///
/// The structural half of the identity scan. The named-key half
/// ([`IDENTITY_KEYS`]) catches `tenant_id: …`; this catches a bare id pasted in
/// as a literal, which is exactly what the pre-existing
/// `the_clause_carries_no_tenant_or_agent_identity` test admits it cannot see
/// once the body stops being a static format string.
fn contains_uuid_shaped_token(body: &str) -> bool {
    body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .any(is_uuid_shaped)
}

/// Does `line` open a runner-owned marker?
///
/// Trims INVISIBLE prefixes as well as whitespace. `str::trim` matches
/// `char::is_whitespace`, which excludes U+FEFF (BOM) and U+200B (zero-width
/// space) — both `Cf`, both invisible in a transcript — so a body starting
/// with one of those followed by `[source: …]` would sail past a `trim`-only
/// guard while still rendering as a forged marker line.
fn opens_a_runner_marker(line: &str) -> bool {
    let trimmed = line.trim_matches(|c: char| {
        c.is_whitespace()
            || c == '\u{FEFF}'
            || c == '\u{200B}'
            || c == '\u{200C}'
            || c == '\u{200D}'
    });
    trimmed.starts_with("[source:") || trimmed.starts_with("[briefing:")
}

/// Validate an editable body before it is used in a prompt.
///
/// `Err(reason)` ⇒ the caller renders the builtin instead. Every check mirrors
/// a coord write-time rejection; see the module docs for why one end is not
/// enough.
pub(crate) fn validate_body(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("body is empty".to_string());
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "body is {} bytes, over the {MAX_BODY_BYTES}-byte ceiling",
            body.len()
        ));
    }
    for token in placeholder_tokens(body) {
        if !PLACEHOLDERS.contains(&token.as_str()) {
            return Err(format!("unknown placeholder `{{{{{token}}}}}`"));
        }
    }
    // EVERY line, not just the first. Line 1 of the render is always the real
    // marker, so `/whereami`'s parse is safe either way — but a forged
    // `[source: …]` anywhere in the body gives a second hit to anyone grepping
    // a transcript for where an instruction came from, which is the whole
    // point of the marker.
    if body.lines().any(opens_a_runner_marker) {
        return Err(
            "body carries a forged runner-owned `[source: …]` / `[briefing: …]` line".to_string(),
        );
    }
    if body.contains(OPERATOR_DOOR_PATH) {
        return Err(format!(
            "body names coord's operator door `{OPERATOR_DOOR_PATH}`, which 403s a device JWT"
        ));
    }
    for key in IDENTITY_KEYS {
        if body.contains(key) {
            return Err(format!("body carries identity-shaped key `{key}`"));
        }
    }
    if contains_uuid_shaped_token(body) {
        return Err("body carries a UUID-shaped identity token".to_string());
    }
    Ok(())
}

// ===========================================================================
// Resolution — cache → guard → substitution → provenance
// ===========================================================================

/// Refusals already logged, keyed by document name → the reason last logged.
/// "Logged ONCE, on a transition" — a rejected body is re-rendered on every
/// single spawn, so an unkeyed `warn!` here would be one line per session.
static LOGGED_REJECTIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn log_rejection_once(name: &str, version: i64, reason: &str) {
    let key = format!("v{version}: {reason}");
    let Ok(mut seen) = LOGGED_REJECTIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return;
    };
    if seen.get(name).map(String::as_str) == Some(key.as_str()) {
        return;
    }
    seen.insert(name.to_string(), key);
    warn!(
        document = %format!("{BRIEFING_KIND}/{name}"),
        version,
        reason = %reason,
        "session_briefing: REFUSED the coord body at render time — \
         rendering the compiled-in builtin instead"
    );
}

/// Resolve one `session_briefing` document to the text that will be injected.
///
/// SYNCHRONOUS and lock-only: this runs on the spawn path (see
/// [`crate::terminal::runner_context`]), so it must never do I/O. Every failure
/// arm — absent document, poisoned lock, failed guard — yields `builtin`, which
/// is the value that cannot make the runner do anything it would not have done
/// before this plan existed.
pub(crate) fn resolve(
    name: &str,
    builtin: &str,
    runner_api_base: &str,
    coord_http_base: &str,
) -> RenderedBlock {
    resolve_requiring(name, builtin, runner_api_base, coord_http_base, &[])
}

/// [`resolve`] plus a per-document ALLOW list: substrings the edited body must
/// still contain, or it is refused like any other guard failure.
///
/// [`validate_body`] is otherwise a deny list — it bounds what a body may
/// contain, not what it must, which is the right shape for prose. This is the
/// escape hatch for the rare clause whose DELETION is the hazard rather than
/// its wording. Today that is exactly one:
/// [`crate::mcp::ai_session::AI_SESSION_RULES_REQUIRED_PROHIBITION`], because
/// restarting the runner directly terminates every live session on the box.
///
/// Keep this list tiny. Every entry is a sentence an operator cannot edit, and
/// a required phrase that is merely *nice to have* turns a legitimate reword
/// into a silent fallback to the builtin.
pub(crate) fn resolve_requiring(
    name: &str,
    builtin: &str,
    runner_api_base: &str,
    coord_http_base: &str,
    required: &[&str],
) -> RenderedBlock {
    let builtin_block = || RenderedBlock {
        text: builtin.to_string(),
        provenance: Provenance::Builtin,
        fetched_at: None,
    };

    let Some(doc) = fleet_policy_poller::cached_briefing(name) else {
        return builtin_block();
    };

    let guard = validate_body(&doc.body).and_then(|()| {
        match required.iter().find(|needle| !doc.body.contains(**needle)) {
            Some(missing) => Err(format!("body no longer carries the required `{missing}`")),
            None => Ok(()),
        }
    });

    if let Err(reason) = guard {
        log_rejection_once(name, doc.version, &reason);
        return RenderedBlock {
            text: builtin.to_string(),
            provenance: Provenance::BuiltinRejected {
                version: doc.version,
            },
            fetched_at: None,
        };
    }

    let provenance = match doc.provenance {
        BriefingProvenance::Coord => Provenance::Coord {
            name: name.to_string(),
            version: doc.version,
        },
        BriefingProvenance::Cached => Provenance::Cached {
            version: doc.version,
        },
    };

    RenderedBlock {
        text: substitute(&doc.body, runner_api_base, coord_http_base),
        provenance,
        // An EMPTY stamp is an absence, not a time. It is the serde default for
        // a store entry written by a build that predates the field, and
        // `fleet_policy_poller`'s `BriefingDial` already reports it as UNKNOWN
        // for that reason. Reported verbatim here it reached the visibility
        // route as `"fetched_at": ""`, which the panel rendered as
        // `last confirmed:` followed by nothing at all — the one thing a
        // provenance surface must not do.
        fetched_at: Some(doc.fetched_at.clone()).filter(|s| !s.is_empty()),
    }
}

/// The provenance a render of `name` WOULD carry right now, without asking for
/// the text.
///
/// For the visibility route, which takes its TEXT from
/// [`crate::terminal::runner_context`] itself (a panel that re-derives the text
/// can disagree with the prompt) but still has to report which document that
/// text came from. The empty builtin passed here is never read — only the
/// provenance and the stamp are.
pub(crate) fn provenance_of(name: &str) -> (Provenance, Option<String>) {
    let block = resolve(name, "", "", "");
    (block.provenance, block.fetched_at)
}

/// The runner's own loopback API base for `{{runner_api_base}}`.
///
/// Spelled `127.0.0.1`, never `localhost`: Windows resolves `localhost` to
/// `::1` first and the runner binds the IPv4 loopback only, so a `localhost`
/// URL pays a doomed IPv6 connect before the socket that answers.
pub(crate) fn runner_api_base(api_port: u16) -> String {
    format!("http://127.0.0.1:{api_port}")
}

// ===========================================================================
// Phase 4 — the visibility route
// ===========================================================================

/// The four keys that say WHERE a piece of the prompt came from.
///
/// Every document-state reading in this payload emits them from HERE — the two
/// text blocks and the plan-capture clause alike. The clause used to spell them
/// out by hand in a `json!` literal, which is the same drift the payload
/// factoring was done to close, one level down: a rename in `block_json` would
/// have left the clause serving the old key names, and the panel reads the two
/// through one shared `BlockMetaRow`.
///
/// Returns the MAP rather than a `Value`, so a caller that has to add a field
/// to it — every caller does — needs no `as_object_mut()` and therefore has no
/// unwrap-or-skip branch. The `Value` form left the assembly below either
/// panicking on the request path or silently dropping every key it wanted to
/// add, and neither is a thing a visibility route should be able to do.
fn document_state_json(
    provenance: &Provenance,
    fetched_at: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut state = serde_json::Map::new();
    state.insert("provenance".to_string(), json!(provenance.kind()));
    state.insert(
        "provenance_detail".to_string(),
        json!(provenance.describe()),
    );
    state.insert(
        "document_version".to_string(),
        json!(provenance.rendered_version()),
    );
    state.insert("fetched_at".to_string(), json!(fetched_at));
    state
}

/// JSON for one rendered block: its document state, plus the text itself.
fn block_json(
    text: &str,
    provenance: &Provenance,
    fetched_at: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut block = document_state_json(provenance, fetched_at);
    block.insert("text".to_string(), json!(text));
    block
}

/// The fleet-gated plan-capture clause's state, as `GET /session-briefing`
/// reports it.
///
/// Carried whether or not the clause is INCLUDED. An operator asking "why is my
/// edited clause not in the prompt?" needs the fleet dial AND the document
/// state, not one of them: the dial is the authorization, the document is only
/// the content, and either one alone answers the wrong half of the question.
struct ClauseReport {
    /// Is the fleet plan-capture dial at `record`, i.e. is this document's text
    /// actually appended to the briefing?
    included: bool,
    provenance: Provenance,
    fetched_at: Option<String>,
}

/// Assemble the `GET /session-briefing` payload. PURE.
///
/// The handler owns the I/O — the supervisor probe, the cache reads, the
/// `runner_context()` render — and this owns the SHAPE, which is the contract
/// `src/components/settings/SessionBriefingPanel.tsx` reads.
///
/// Factored out because nothing pinned that contract. The object used to be
/// assembled inline behind `if let Some(obj) = payload.as_object_mut()`, which
/// made every key past the first block conditional on a branch that cannot
/// fail — and left a renamed or dropped key as a silent panel regression that
/// neither `cargo test` nor `tsc` could see, since the two ends meet only over
/// JSON. `the_payload_shape_is_the_contract_the_panel_reads` now pins it.
fn briefing_payload(
    api_port: u16,
    briefing_text: &str,
    base_provenance: &Provenance,
    base_fetched_at: Option<&str>,
    clause: &ClauseReport,
    rules: &crate::mcp::ai_session::RenderedRules,
) -> Value {
    let mut obj = block_json(briefing_text, base_provenance, base_fetched_at);

    obj.insert("api_port".to_string(), json!(api_port));
    obj.insert(
        "document".to_string(),
        json!(format!("{BRIEFING_KIND}/{BRIEFING_RUNNER_SESSION}")),
    );

    // Kept beside the nested object rather than replaced by it: the flat
    // boolean is the key Phase 4 of the plan specifies, so a `curl` reader or a
    // script that predates the nested object keeps working. Both are built from
    // the same value and can never disagree; the nested one additionally
    // carries the document state the flat boolean cannot express, and that is
    // the one the settings panel reads.
    obj.insert(
        "plan_capture_clause_included".to_string(),
        json!(clause.included),
    );
    let mut clause_json = document_state_json(&clause.provenance, clause.fetched_at.as_deref());
    clause_json.insert("included".to_string(), json!(clause.included));
    clause_json.insert(
        "document".to_string(),
        json!(format!("{BRIEFING_KIND}/{BRIEFING_PLAN_CAPTURE_CLAUSE}")),
    );
    obj.insert(
        "plan_capture_clause".to_string(),
        Value::Object(clause_json),
    );

    let mut rules_json = block_json(&rules.text, &rules.provenance, rules.fetched_at.as_deref());
    rules_json.insert(
        "document".to_string(),
        json!(format!("{BRIEFING_KIND}/{BRIEFING_AI_SESSION_RULES}")),
    );
    obj.insert("ai_session_rules".to_string(), Value::Object(rules_json));

    Value::Object(obj)
}

/// `GET /session-briefing` — exactly what THIS runner will inject.
///
/// The briefing half renders by CALLING [`crate::terminal::runner_context`],
/// not by re-deriving the text: a panel that re-derives can disagree with the
/// prompt, and a visibility surface that can lie is worse than none.
pub async fn session_briefing_handler(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Value>> {
    // The ACTUALLY BOUND port, not `get_mcp_api_port()` — that helper is
    // env-var-only and answers 9876 on a secondary or temp runner, which would
    // make this panel report the primary's URLs.
    let api_port = crate::mcp::types::runner_api_port(&state.app_state);

    // The exact injected text.
    let text = crate::terminal::runner_context(api_port);

    // The provenance of the same render. This is a pure cache read, so it
    // cannot disagree with the read inside `runner_context` unless a poll
    // landed between the two — in which case both answers were true when they
    // were taken.
    let (base_provenance, base_fetched_at) = provenance_of(BRIEFING_RUNNER_SESSION);
    let (clause_provenance, clause_fetched_at) = provenance_of(BRIEFING_PLAN_CAPTURE_CLAUSE);
    let clause = ClauseReport {
        included: fleet_policy_poller::effective_plan_capture_level()
            == fleet_policy_poller::PLAN_CAPTURE_RECORD,
        provenance: clause_provenance,
        fetched_at: clause_fetched_at,
    };

    // `check_supervisor_available` is a BLOCKING 500ms TCP connect; parking a
    // tokio worker on it for every panel load is not acceptable on a shared
    // runtime.
    let supervisor_available =
        tokio::task::spawn_blocking(crate::mcp::auto_continue::check_supervisor_available)
            .await
            .unwrap_or(false);
    let rules = crate::mcp::ai_session::runner_rules_prefix(supervisor_available, api_port);

    Json(ApiResponse::success(briefing_payload(
        api_port,
        &text,
        &base_provenance,
        base_fetched_at.as_deref(),
        &clause,
        &rules,
    )))
}

/// The door's route table, as data: `(method, path)`.
///
/// `Router` has no public introspection and there is no global `:9876` route
/// manifest, so this table plus its count test is what catches a route added to
/// [`routes`] and forgotten in `mcp_api`'s `.merge(…)` — the `plan_library`
/// pattern.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("GET", "/session-briefing")]
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/session-briefing", get(session_briefing_handler))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- substitution ------------------------------------------------------

    /// The substitution table: both placeholders, everywhere they appear, and
    /// nothing else touched.
    #[test]
    fn substitution_replaces_the_closed_vocabulary() {
        let body = "api {{runner_api_base}}/x and coord {{coord_http_base}}/y \
                    and again {{runner_api_base}}/z, literal {kind}/{name} untouched";
        let out = substitute(body, "http://127.0.0.1:9876", "https://coord.example.com");
        assert_eq!(
            out,
            "api http://127.0.0.1:9876/x and coord https://coord.example.com/y \
             and again http://127.0.0.1:9876/z, literal {kind}/{name} untouched"
        );
    }

    /// An unknown token is DROPPED, never shipped as a literal.
    #[test]
    fn substitution_drops_an_unknown_token() {
        let out = substitute("a {{tenant_id}} b", "http://127.0.0.1:9876", "https://c");
        assert_eq!(out, "a  b");
        assert!(!out.contains("{{"));
    }

    /// An UNCLOSED `{{` is left alone rather than eating the rest of the body.
    #[test]
    fn substitution_leaves_an_unclosed_token_alone_and_terminates() {
        let out = substitute("a {{ b", "http://127.0.0.1:9876", "https://c");
        assert_eq!(out, "a {{ b");
    }

    #[test]
    fn placeholder_tokens_are_extracted_in_order() {
        assert_eq!(
            placeholder_tokens("x {{a}} y {{b}} z"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(placeholder_tokens("no tokens {here}").is_empty());
    }

    // ---- the render-time guard --------------------------------------------

    const GOOD: &str = "You are running inside the Qontinui Runner. \
                        Runner HTTP API: {{runner_api_base}}. \
                        GET {{coord_http_base}}/coord/agent-prompt-documents/{kind}/{name}.";

    #[test]
    fn a_well_formed_body_passes_the_guard() {
        validate_body(GOOD).expect("the seeded shape must validate");
    }

    #[test]
    fn the_guard_rejects_an_empty_body() {
        assert!(validate_body("   \n ").is_err());
    }

    /// The 16 KiB ceiling. One byte under passes, one byte over does not.
    #[test]
    fn the_guard_rejects_a_body_over_16_kib() {
        let at_ceiling = "x".repeat(MAX_BODY_BYTES);
        assert!(validate_body(&at_ceiling).is_ok());
        let over = "x".repeat(MAX_BODY_BYTES + 1);
        let err = validate_body(&over).expect_err("over-size must be refused");
        assert!(err.contains("ceiling"), "{err}");
    }

    #[test]
    fn the_guard_rejects_an_unknown_placeholder() {
        let err = validate_body("hello {{organization}} world")
            .expect_err("an unknown token must be refused");
        assert!(err.contains("organization"), "{err}");
    }

    /// The marker is RUNNER-owned and must not be forgeable from a document —
    /// otherwise an editable body could impersonate a build identity.
    #[test]
    fn the_guard_rejects_a_forged_source_marker() {
        let err = validate_body("[source: qontinui-runner/runner_context@0.1.0+deadbee]\nbody")
            .expect_err("a forged marker must be refused");
        assert!(err.contains("forged"), "{err}");
        // …including behind leading blank lines and indentation.
        assert!(validate_body("\n\n  [source: x]\nbody").is_err());
    }

    #[test]
    fn the_guard_rejects_a_forged_provenance_line() {
        assert!(validate_body("[briefing: coord session_briefing/runner-session v9]\nb").is_err());
    }

    /// The operator door 403s a device JWT, so advertising it hands a session a
    /// link it cannot follow — the bug `2026-08-08` Phase 1.8 fixed. The AGENT
    /// door must still pass, which is the load-bearing half: its path CONTAINS
    /// the operator door's trailing segment.
    #[test]
    fn the_guard_rejects_the_operator_door_but_allows_the_agent_door() {
        let err = validate_body("see GET https://coord.example.com/coord/prompt-documents")
            .expect_err("the operator door must be refused");
        assert!(err.contains("operator door"), "{err}");
        validate_body("see GET https://coord.example.com/coord/agent-prompt-documents")
            .expect("the agent door must be allowed");
    }

    /// The RCE-class invariant, named-key half.
    #[test]
    fn the_guard_rejects_identity_shaped_keys() {
        for key in IDENTITY_KEYS {
            let body = format!("your {key} is set");
            let err = validate_body(&body).expect_err("identity keys must be refused");
            assert!(err.contains(key), "{err}");
        }
    }

    /// …and the structural half: a bare UUID pasted in as a literal, which the
    /// old static-format-string guarantee used to make impossible.
    #[test]
    fn the_guard_rejects_a_uuid_shaped_token() {
        let err = validate_body("you belong to 01a01eb4-718a-7303-825a-94ec0d0ade91 now")
            .expect_err("a UUID must be refused");
        assert!(err.contains("UUID"), "{err}");
    }

    #[test]
    fn uuid_shape_detection_is_exact() {
        assert!(is_uuid_shaped("01a01eb4-718a-7303-825a-94ec0d0ade91"));
        assert!(!is_uuid_shaped("01a01eb4-718a-7303-825a-94ec0d0ade9")); // short
        assert!(!is_uuid_shaped("01a01eb4-718a-7303-825a")); // 4 groups
        assert!(!is_uuid_shaped("zzzzzzzz-718a-7303-825a-94ec0d0ade91")); // non-hex
        assert!(!is_uuid_shaped("plan-library-artifacts-and-links"));
        // A realistic briefing must not trip the scan.
        assert!(!contains_uuid_shaped_token(GOOD));
    }

    // ---- provenance --------------------------------------------------------

    /// The exact line-2 vocabulary. These strings are read by humans out of
    /// transcripts, so pin them.
    #[test]
    fn provenance_lines_are_the_documented_vocabulary() {
        assert_eq!(
            Provenance::Coord {
                name: BRIEFING_RUNNER_SESSION.to_string(),
                version: 7
            }
            .line(),
            "[briefing: coord session_briefing/runner-session v7]"
        );
        assert_eq!(
            Provenance::Cached { version: 7 }.line(),
            "[briefing: cached v7 (stale)]"
        );
        assert_eq!(Provenance::Builtin.line(), "[briefing: builtin-fallback]");
        assert_eq!(
            Provenance::BuiltinRejected { version: 7 }.line(),
            "[briefing: builtin-fallback (rejected coord v7)]"
        );
    }

    /// Version `0` is UNKNOWN, and every surface that renders a version has to
    /// say so — not just the settings panel's `version:` field.
    ///
    /// `describe()` is the string that lands on LINE 2 of the system prompt of
    /// every session this runner hosts, and in the panel's provenance badge.
    /// `fleet_policy_poller`'s version gate names `[briefing: coord … v0]` in
    /// its own comment as the claim the plan forbids, and then a list row with
    /// no `current_version` reaches `store_briefing(name, body, 0)` anyway —
    /// with provenance `Coord`, because the body really was fetched. Only the
    /// generation is unknown.
    #[test]
    fn version_zero_is_reported_as_unknown_not_as_v0() {
        let coord = Provenance::Coord {
            name: BRIEFING_RUNNER_SESSION.to_string(),
            version: 0,
        };
        assert_eq!(
            coord.line(),
            "[briefing: coord session_briefing/runner-session (version unknown)]"
        );
        assert!(!coord.describe().contains("v0"));
        // Still a coord body — the UNKNOWN is about the generation only.
        assert_eq!(coord.kind(), "coord");
        assert_eq!(coord.rendered_version(), None);

        let cached = Provenance::Cached { version: 0 };
        assert_eq!(cached.line(), "[briefing: cached (version unknown, stale)]");
        assert_eq!(cached.rendered_version(), None);
        // Still `cached`, and it still SAYS stale — the unknown is the version.
        assert_eq!(cached.kind(), "cached");

        let rejected = Provenance::BuiltinRejected { version: 0 };
        assert_eq!(
            rejected.line(),
            "[briefing: builtin-fallback (rejected coord body of unknown version)]"
        );
        assert!(!rejected.describe().contains("v0"));

        // A real version is untouched by the rule.
        assert_eq!(
            Provenance::Cached { version: 1 }.rendered_version(),
            Some(1)
        );
        assert_eq!(known_version(0), None);
        assert_eq!(known_version(7), Some(7));
    }

    /// The route serves the same absence the panel renders. Before this, a
    /// `curl` reader got `"document_version": 0` while the panel beside it
    /// printed `— (unknown)` — one fact, two answers.
    #[test]
    fn the_payload_reports_an_unknown_version_as_null() {
        let rules = crate::mcp::ai_session::RenderedRules {
            text: "RULES".to_string(),
            provenance: Provenance::Cached { version: 0 },
            fetched_at: None,
        };
        let clause = ClauseReport {
            included: false,
            provenance: Provenance::Coord {
                name: BRIEFING_PLAN_CAPTURE_CLAUSE.to_string(),
                version: 0,
            },
            fetched_at: None,
        };
        let payload = briefing_payload(
            9876,
            "BRIEFING",
            &Provenance::Coord {
                name: BRIEFING_RUNNER_SESSION.to_string(),
                version: 0,
            },
            None,
            &clause,
            &rules,
        );

        assert_eq!(payload["document_version"], Value::Null);
        assert_eq!(
            payload["plan_capture_clause"]["document_version"],
            Value::Null
        );
        assert_eq!(payload["ai_session_rules"]["document_version"], Value::Null);
        // …and the coarse token still says the text came from a document, so
        // the panel reads `— (unknown)` rather than `— (compiled-in fallback)`.
        assert_eq!(payload["provenance"], "coord");
        assert_eq!(payload["plan_capture_clause"]["provenance"], "coord");
    }

    /// The clause's document state is emitted by the SAME function as the two
    /// text blocks', so it cannot drift from what the panel's shared
    /// `BlockMetaRow` reads. Asserted structurally rather than by eye: a key
    /// added to `document_state_json` must appear on all three.
    #[test]
    fn every_document_state_reading_carries_the_same_keys() {
        let rules = crate::mcp::ai_session::RenderedRules {
            text: "RULES".to_string(),
            provenance: Provenance::Builtin,
            fetched_at: None,
        };
        let clause = ClauseReport {
            included: false,
            provenance: Provenance::Builtin,
            fetched_at: None,
        };
        let payload = briefing_payload(9876, "B", &Provenance::Builtin, None, &clause, &rules);

        let keys = |v: &Value| {
            let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
            k.sort();
            k
        };
        let state_keys = keys(&Value::Object(document_state_json(
            &Provenance::Builtin,
            None,
        )));
        assert_eq!(
            state_keys,
            vec![
                "document_version".to_string(),
                "fetched_at".to_string(),
                "provenance".to_string(),
                "provenance_detail".to_string(),
            ]
        );
        for reading in [
            &payload,
            &payload["plan_capture_clause"],
            &payload["ai_session_rules"],
        ] {
            let present = keys(reading);
            for key in &state_keys {
                assert!(present.contains(key), "missing `{key}` in {reading}");
            }
        }
    }

    /// A REJECTED coord body must never be reported as a rendered version —
    /// "claiming a coord version while serving the builtin" is the one thing
    /// the plan says must never happen.
    #[test]
    fn a_rejected_version_is_never_reported_as_rendered() {
        let p = Provenance::BuiltinRejected { version: 7 };
        assert_eq!(p.kind(), "builtin");
        assert_eq!(p.rendered_version(), None);
        // …but it stays VISIBLE, so the refusal is diagnosable.
        assert!(p.describe().contains("rejected coord v7"));
    }

    // ---- the payload shape -------------------------------------------------

    /// Every key `src/components/settings/SessionBriefingPanel.tsx` reads,
    /// pinned.
    ///
    /// The panel and this route meet only over JSON: a renamed key compiles on
    /// both sides and fails at runtime, in a read-only settings tab nobody
    /// loads until they are already debugging something else. So the contract
    /// is asserted here rather than left to whoever last edited the handler.
    ///
    /// The CLAUSE half is asserted with `included: false` deliberately. That is
    /// the arm where the clause's text is not in the prompt at all, and it is
    /// exactly the arm where the document state still has to be reported —
    /// otherwise "my edited clause is not showing up" has no answer.
    #[test]
    fn the_payload_shape_is_the_contract_the_panel_reads() {
        let rules = crate::mcp::ai_session::RenderedRules {
            text: "[source: x]\n[briefing: builtin-fallback]\nRULES".to_string(),
            provenance: Provenance::Builtin,
            fetched_at: None,
        };
        let clause = ClauseReport {
            included: false,
            provenance: Provenance::Coord {
                name: BRIEFING_PLAN_CAPTURE_CLAUSE.to_string(),
                version: 4,
            },
            fetched_at: Some("2026-08-24T00:00:00+00:00".to_string()),
        };
        let payload = briefing_payload(
            9877,
            "[source: y]\n[briefing: coord session_briefing/runner-session v3]\nBRIEFING",
            &Provenance::Coord {
                name: BRIEFING_RUNNER_SESSION.to_string(),
                version: 3,
            },
            Some("2026-08-24T00:00:01+00:00"),
            &clause,
            &rules,
        );

        // The base block, at the TOP level — the panel spreads it into its own
        // `BriefingBlock` rather than nesting it.
        assert_eq!(
            payload["text"],
            "[source: y]\n[briefing: coord session_briefing/runner-session v3]\nBRIEFING"
        );
        assert_eq!(payload["provenance"], "coord");
        assert_eq!(
            payload["provenance_detail"],
            "coord session_briefing/runner-session v3"
        );
        assert_eq!(payload["document_version"], 3);
        assert_eq!(payload["fetched_at"], "2026-08-24T00:00:01+00:00");
        assert_eq!(payload["document"], "session_briefing/runner-session");
        // The port the panel prints, and the one substituted into the body.
        assert_eq!(payload["api_port"], 9877);

        // The flat boolean Phase 4 specifies, and the nested object that says
        // WHICH document the dial is gating.
        assert_eq!(payload["plan_capture_clause_included"], false);
        let c = &payload["plan_capture_clause"];
        assert_eq!(c["included"], false);
        assert_eq!(c["document"], "session_briefing/plan-capture-clause");
        assert_eq!(c["provenance"], "coord");
        assert_eq!(
            c["provenance_detail"],
            "coord session_briefing/plan-capture-clause v4"
        );
        assert_eq!(c["document_version"], 4);
        assert_eq!(c["fetched_at"], "2026-08-24T00:00:00+00:00");

        // The second injected prompt, marker line and all.
        let r = &payload["ai_session_rules"];
        assert_eq!(
            r["text"],
            "[source: x]\n[briefing: builtin-fallback]\nRULES"
        );
        assert_eq!(r["provenance"], "builtin");
        assert_eq!(r["provenance_detail"], "builtin-fallback");
        assert_eq!(r["document_version"], Value::Null);
        assert_eq!(r["fetched_at"], Value::Null);
        assert_eq!(r["document"], "session_briefing/ai-session-rules");
    }

    /// A REJECTED coord body reports `builtin` with a null rendered version —
    /// on the clause too, not only on the base block. The panel colours the
    /// badge from `provenance` and prints `provenance_detail` verbatim, so this
    /// is what stops it from claiming a version the session never saw.
    #[test]
    fn a_rejected_clause_body_reports_builtin_with_no_rendered_version() {
        let rules = crate::mcp::ai_session::RenderedRules {
            text: "RULES".to_string(),
            provenance: Provenance::Builtin,
            fetched_at: None,
        };
        let clause = ClauseReport {
            included: true,
            provenance: Provenance::BuiltinRejected { version: 9 },
            fetched_at: None,
        };
        let payload = briefing_payload(
            9876,
            "BRIEFING",
            &Provenance::Builtin,
            None,
            &clause,
            &rules,
        );

        let c = &payload["plan_capture_clause"];
        assert_eq!(c["included"], true);
        assert_eq!(c["provenance"], "builtin");
        assert_eq!(
            c["provenance_detail"],
            "builtin-fallback (rejected coord v9)"
        );
        assert_eq!(c["document_version"], Value::Null);
    }

    // ---- routes ------------------------------------------------------------

    /// The route table is in lockstep with `routes()`. There is no global
    /// `:9876` manifest, so this count test plus the `.merge(…)` line in
    /// `mcp_api` is the whole registration contract.
    #[test]
    fn the_route_table_is_in_lockstep_with_routes() {
        let entries = route_entries();
        assert_eq!(entries.len(), 1, "keep in lockstep with routes()");
        assert_eq!(entries[0], ("GET", "/session-briefing"));
        let _r: Router<Arc<ApiState>> = routes();
    }

    // ---- resolution fail-safe arms ----------------------------------------

    /// With NOTHING cached — the arm every runner runs on until coord's half
    /// ships — resolution yields the builtin verbatim, labelled honestly.
    #[test]
    fn resolution_with_no_cached_document_yields_the_builtin() {
        let _pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        let block = resolve(
            BRIEFING_RUNNER_SESSION,
            "BUILTIN TEXT",
            "http://127.0.0.1:9876",
            "https://coord.example.com",
        );
        assert_eq!(block.text, "BUILTIN TEXT");
        assert_eq!(block.provenance, Provenance::Builtin);
        assert_eq!(block.fetched_at, None);
    }

    /// A cached, VALID coord body is rendered with substitution applied and
    /// labelled `coord`.
    #[test]
    fn resolution_renders_a_valid_coord_body() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            fleet_policy_poller::briefing_for_test(
                "coord body at {{runner_api_base}}",
                7,
                BriefingProvenance::Coord,
            ),
        );
        let block = resolve(
            BRIEFING_RUNNER_SESSION,
            "BUILTIN TEXT",
            "http://127.0.0.1:9876",
            "https://coord.example.com",
        );
        assert_eq!(block.text, "coord body at http://127.0.0.1:9876");
        assert_eq!(
            block.provenance,
            Provenance::Coord {
                name: BRIEFING_RUNNER_SESSION.to_string(),
                version: 7
            }
        );
    }

    /// A disk-restored body that no poll has re-confirmed is labelled `cached
    /// (stale)` — never `coord`.
    #[test]
    fn resolution_labels_an_unconfirmed_disk_body_as_cached() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            fleet_policy_poller::briefing_for_test("cached body", 3, BriefingProvenance::Cached),
        );
        let block = resolve(
            BRIEFING_RUNNER_SESSION,
            "BUILTIN TEXT",
            "http://127.0.0.1:9876",
            "https://coord.example.com",
        );
        assert_eq!(block.text, "cached body");
        assert_eq!(block.provenance, Provenance::Cached { version: 3 });
    }

    /// An EMPTY `fetched_at` is UNKNOWN, never an empty timestamp.
    ///
    /// The field carries `#[serde(default)]` so a store written before it
    /// existed restores as `""`. `fleet_policy_poller`'s `BriefingDial` already
    /// filters that for the config report; `resolve` did not, so the same
    /// absence reached `GET /session-briefing` as `"fetched_at": ""` and the
    /// settings panel printed `last confirmed:` with nothing after it.
    #[test]
    fn an_empty_stamp_is_reported_as_absent_not_as_a_timestamp() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        let mut doc = fleet_policy_poller::briefing_for_test("body", 3, BriefingProvenance::Cached);
        doc.fetched_at = String::new();
        pin.set_briefing(BRIEFING_RUNNER_SESSION, doc);

        let block = resolve(
            BRIEFING_RUNNER_SESSION,
            "BUILTIN TEXT",
            "http://127.0.0.1:9876",
            "https://coord.example.com",
        );
        // The BODY is still the cached one — this is a statement about the
        // stamp only, not a reason to fall back to the builtin.
        assert_eq!(block.text, "body");
        assert_eq!(block.provenance, Provenance::Cached { version: 3 });
        assert_eq!(block.fetched_at, None);
    }

    /// Every guard failure falls back to the builtin and SAYS which version it
    /// refused. Drives each rejection through the shipping resolution path
    /// rather than asserting about `validate_body` alone.
    #[test]
    fn every_render_time_rejection_falls_back_to_the_builtin() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        for bad in [
            "x".repeat(MAX_BODY_BYTES + 1),
            "hello {{organization}}".to_string(),
            "[source: forged]\nbody".to_string(),
            "GET https://coord.example.com/coord/prompt-documents".to_string(),
            "your tenant_id is here".to_string(),
            "id 01a01eb4-718a-7303-825a-94ec0d0ade91".to_string(),
        ] {
            pin.set_briefing(
                BRIEFING_RUNNER_SESSION,
                fleet_policy_poller::briefing_for_test(&bad, 9, BriefingProvenance::Coord),
            );
            let block = resolve(
                BRIEFING_RUNNER_SESSION,
                "BUILTIN TEXT",
                "http://127.0.0.1:9876",
                "https://coord.example.com",
            );
            assert_eq!(block.text, "BUILTIN TEXT", "body: {bad:.40}");
            assert_eq!(
                block.provenance,
                Provenance::BuiltinRejected { version: 9 },
                "body: {bad:.40}"
            );
        }
    }

    /// THE MOST LIKELY SILENT NO-OP. If coord seeds `session_briefing/*` with
    /// today's shipped text — which is exactly what the plan specifies, a
    /// verbatim move — and the render guard refuses it, every runner on the
    /// fleet serves `builtin-fallback (rejected coord v1)` forever and the only
    /// signal is one `warn!`. Pin that the compiled-in bodies validate.
    #[test]
    fn every_compiled_in_builtin_passes_the_render_guard() {
        validate_body(&crate::terminal::builtin_briefing_body(
            9876,
            "https://coord.example.com",
        ))
        .expect("the builtin briefing must validate — coord seeds it verbatim");
        validate_body(&crate::terminal::plan_capture_clause_body(
            9876,
            "https://coord.example.com",
        ))
        .expect("the builtin clause must validate — coord seeds it verbatim");
        validate_body(crate::mcp::ai_session::AI_SESSION_RULES_SUPERVISOR_AVAILABLE)
            .expect("the builtin ai-session rules must validate — coord seeds them verbatim");
    }

    /// …and the placeholder-rewritten form coord will actually store. The seeds
    /// swap `http://127.0.0.1:<port>` and the coord base for the two
    /// placeholders, so validate that shape too rather than only the rendered
    /// one.
    #[test]
    fn the_placeholder_rewritten_seed_shape_also_passes_the_guard() {
        let seeded = crate::terminal::builtin_briefing_body(9876, "COORD")
            .replace("http://127.0.0.1:9876", "{{runner_api_base}}")
            .replace("COORD", "{{coord_http_base}}");
        assert!(seeded.contains("{{runner_api_base}}"));
        assert!(seeded.contains("{{coord_http_base}}"));
        validate_body(&seeded).expect("the seeded placeholder form must validate");
    }

    /// The marker guard reads EVERY line, not just the first, and strips
    /// invisible format characters. `str::trim` matches `char::is_whitespace`,
    /// which excludes U+FEFF and U+200B — so a `trim`-only guard would let a
    /// forged marker through while it still renders as one.
    #[test]
    fn the_marker_guard_sees_past_zero_width_prefixes_and_past_line_one() {
        assert!(validate_body("first line\n[source: forged]\nmore").is_err());
        assert!(validate_body("first line\n[briefing: forged v9]").is_err());
        assert!(validate_body("\u{FEFF}[source: forged]\nbody").is_err());
        assert!(validate_body("\u{200B}[source: forged]\nbody").is_err());
        assert!(validate_body("\u{200D}  [briefing: forged]\nbody").is_err());
        // A bracketed line that is NOT a runner marker stays legal — the guard
        // must not become "no square brackets".
        validate_body("see [the docs](https://example.com) for detail")
            .expect("ordinary bracketed prose must still validate");
    }

    /// A per-document required phrase: dropping it is refused like any other
    /// guard failure, and the builtin renders. Rewording AROUND it is allowed.
    #[test]
    fn a_required_phrase_may_not_be_deleted_by_an_edit() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");

        pin.set_briefing(
            BRIEFING_AI_SESSION_RULES,
            fleet_policy_poller::briefing_for_test(
                "Rules: be nice. Restart via the supervisor.",
                8,
                BriefingProvenance::Coord,
            ),
        );
        let dropped = resolve_requiring(
            BRIEFING_AI_SESSION_RULES,
            "BUILTIN",
            "http://127.0.0.1:9876",
            "https://c",
            &["Do NOT restart the qontinui-runner directly"],
        );
        assert_eq!(dropped.text, "BUILTIN");
        assert_eq!(
            dropped.provenance,
            Provenance::BuiltinRejected { version: 8 }
        );

        pin.set_briefing(
            BRIEFING_AI_SESSION_RULES,
            fleet_policy_poller::briefing_for_test(
                "Reworded preamble. Do NOT restart the qontinui-runner directly. Reworded tail.",
                8,
                BriefingProvenance::Coord,
            ),
        );
        let kept = resolve_requiring(
            BRIEFING_AI_SESSION_RULES,
            "BUILTIN",
            "http://127.0.0.1:9876",
            "https://c",
            &["Do NOT restart the qontinui-runner directly"],
        );
        assert!(kept.text.starts_with("Reworded preamble"), "{}", kept.text);
        assert_eq!(kept.provenance.kind(), "coord");
    }

    /// An empty required list is the ordinary `resolve` path — no floor.
    #[test]
    fn resolve_with_no_required_phrases_is_plain_resolution() {
        let pin = fleet_policy_poller::pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            fleet_policy_poller::briefing_for_test("anything at all", 2, BriefingProvenance::Coord),
        );
        let a = resolve(
            BRIEFING_RUNNER_SESSION,
            "B",
            "http://127.0.0.1:9876",
            "https://c",
        );
        let b = resolve_requiring(
            BRIEFING_RUNNER_SESSION,
            "B",
            "http://127.0.0.1:9876",
            "https://c",
            &[],
        );
        assert_eq!(a, b);
        assert_eq!(a.text, "anything at all");
    }
}
