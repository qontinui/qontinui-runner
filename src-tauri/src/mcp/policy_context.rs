//! SessionStart policy injection — deliver the tenant's policy documents into
//! every Claude session's context (plan
//! `2026-08-08-runner-enforced-policy-pull.md`, Phase 1).
//!
//! ## The failure class this removes
//!
//! `policy/session-protocol` Step 0 says: *"Never work from memory of these
//! documents; they version frequently."* It depended entirely on a session
//! VOLUNTARILY calling `coord_list_prompt_documents` +
//! `coord_get_prompt_document` at turn one, and nothing checked that it did.
//! The failure was silent and total — a session that skipped Step 0 did not
//! degrade, it simply operated with no policy at all while producing work that
//! looked normal. The motivating incident was a full `/vet-imp` cycle (vet,
//! implement, ship, two PRs merged) run with no policy pull, found only
//! because the operator asked.
//!
//! Detect-and-nag was considered and rejected as the PRIMARY mechanism: it
//! catches a failure that already happened and then depends on the agent
//! complying with a nudge — the same voluntary-compliance assumption that just
//! failed. Delivering the policy at `SessionStart` removes the failure class
//! outright and is immune to agent discipline.
//!
//! ## Shape
//!
//! A dumb bundled script (`resources/session-restore/claude_policy_hook.sh`)
//! curls this runner's `GET /sessions/{id}/policy-context` and prints the
//! response verbatim. Everything else — the flag, the fetch, the cache, the
//! rendering, the fail-open notice — is here, in Rust, where it is
//! unit-testable. That is the same division of labour
//! [`crate::mcp::continuation_verdict`] uses for the `Stop` hook (plan D4).
//!
//! The response body IS the Claude hook contract, not prose:
//! `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`
//! — the envelope the fleet already uses at
//! `qontinui-claude-config/.claude/settings.json:154`. Rendering it here is what
//! keeps the script dumb, and it is the only place the fail-open notice can be
//! phrased.
//!
//! ## What gets injected, and why not more
//!
//! The FULL body of `policy/session-protocol`, plus an INDEX (name,
//! description, `current_version`) of every `kind=policy` document.
//!
//! Measured 2026-08-19: the protocol body is ~7.2 KB (~1.8k tokens) and a
//! 14-entry index ~1.4 KB (~350 tokens), so the payload costs ~2.2k
//! tokens/session. Injecting all 14 bodies would cost ~18k. The rejected
//! middle option — curating the "top 4" bodies — was rejected on
//! SCALABILITY: a hardcoded list must be re-curated whenever a policy document
//! is added or renamed, and it drifts *silently*, because a session shown four
//! bodies reasonably infers those are the policy that matters. The index scales
//! with the document set for free. It also buys nothing the protocol does not:
//! Step 0 is itself the instructions for fetching the rest.
//!
//! `current_version` rides every index entry deliberately — it is what lets a
//! session tell a stale memory from a current one.
//!
//! ## Fires on every `source`
//!
//! `startup | resume | compact` all inject. A resumed session carries its old
//! context but not the policies as they now stand, and a compacted one has just
//! had them evicted — both are exactly the cases Step 0 exists for.
//!
//! ## Fail-open, always
//!
//! Coord unreachable, no device JWT, non-2xx, undecodable body: this module
//! still returns a 200 carrying an `additionalContext` that SAYS the pull
//! failed and names the door the session must use itself. It never 5xxs and
//! never blocks a session start. Mirrors [`crate::session::claude_hook`]'s
//! posture, where a materialize failure just omits `--settings`.
//!
//! ## Attribution — the read is recorded against the SESSION, not the terminal
//!
//! Every coord fetch carries `?via=session_start_injection` and, when the hook
//! supplied a parseable one, `X-Coord-Caller-Session: <claude session id>`.
//! Coord records the read in `coord.session_policy_reads` and the compliance
//! reconciler reads those rows to answer "did this session pull policy?".
//!
//! Two ids are in play and they are NOT interchangeable. The route's path
//! segment is the runner TERMINAL id (what `resolve_session_key` returns); the
//! header carries the CLAUDE session id, which the hook script lifts from the
//! `SessionStart` stdin payload and passes as its own query param. One terminal
//! hosts several Claude sessions in sequence, so attributing a read to the
//! terminal id would file every one of them under the same session. The runner's
//! device JWT is what makes coord's fail-closed `session_on_device` binding
//! accept the header at all — that is why an injection is attributable.
//!
//! No id, or an unparseable one, means NO header: coord records
//! `claude_session_id = NULL`, which the compliance signal reads as
//! `unavailable`. Never fabricated, never substituted with the terminal id — a
//! fabricated provenance value is worse than an admitted gap, because the
//! reconciler reads that column as fact.
//!
//! ## Caching does not suppress recording
//!
//! See [`fetch_payload`]. The 45 s cache exists to avoid re-fetching ~8 KB of
//! body, not to make a session's pull invisible — so when there is a session to
//! attribute to, both coord reads go out on every call, CONDITIONALLY, and coord
//! answers 304 while still recording the read.
//!
//! ## Transport — the agent door, never the operator door
//!
//! All fetches go to `/coord/agent-prompt-documents{,/{kind}/{name}}`, coord's
//! device/agent (`require_jwt`) sub-router. The sibling
//! `/coord/prompt-documents/*` surface is the OPERATOR door: it resolves
//! tenancy solely from a verified Cognito operator context and 403s a device
//! JWT (documented at length on
//! [`crate::mcp::continuation_verdict`]'s rules URL, and confirmed against
//! prod). Because this path fail-opens, pointing at the operator door would not
//! break loudly — it would silently inject the "pull failed" notice forever.
//! The URL-shape regression tests below pin this the same way
//! `continuation_verdict.rs` and `prompt_library.rs` do.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{debug, info, warn};

// ===========================================================================
// Flag + constants
// ===========================================================================

/// Env flag gating the whole feature. **`on` is the DEFAULT** — unset means
/// inject. `off` ⇒ no injection and zero coord traffic; `observe` ⇒ render +
/// log what WOULD be injected, inject nothing; `on` ⇒ inject.
///
/// DELIBERATELY NOT the fail-safe-to-`off` convention its siblings
/// (`continuation_verdict`, `context_handoff`) use, and the divergence is the
/// point: those flags add BEHAVIOUR, so silence should mean "do nothing". This
/// one delivers the tenant's POLICY, and a session that silently runs without
/// it is the exact incident this module exists to prevent — a full `/vet-imp`
/// cycle shipped with no policy pulled and nothing objected. "Do nothing" is
/// the failure here, not the safe state, so the default is `on` and the only
/// way to disable is to say `off` out loud.
pub const FLAG_ENV: &str = "QONTINUI_POLICY_INJECTION";

/// The coord `prompt_documents.kind` this module serves.
const KIND: &str = "policy";

/// The one document whose FULL BODY is injected. Its Step 0 is the
/// instructions for fetching everything else, which is why the index is
/// sufficient for the remaining documents.
const PROTOCOL_DOC_NAME: &str = "session-protocol";

/// TTL for the process-global payload cache. Documents version frequently but
/// not per-second. Deliberately the SAME 45s constant
/// `continuation_verdict::CACHE_TTL` and `prompt_library::CACHE_TTL` both use —
/// a third freshness number would be a third thing to reason about.
const CACHE_TTL: Duration = Duration::from_secs(45);

/// The `source` label applied when the hook payload carried none or carried an
/// unrecognised one. `startup` is the conservative read: it is the case that
/// definitely needs the policy.
const DEFAULT_SOURCE: &str = "startup";

/// The `?via=` marker coord's agent door maps to
/// `session_policy_reads.source = 'session_start_injection'`.
///
/// Physically the same HTTP door any agent reads through; semantically a
/// distinct event — the RUNNER reading on a session's behalf and injecting the
/// result into that session's context. It must count as the session having
/// pulled policy, because under this phase that is precisely what happened.
///
/// Coord honours only this exact literal and degrades anything else to
/// `http_door`, so the two spellings are one contract (coord:
/// `PolicyReadSource::from_http_via`).
const VIA_MARKER: &str = "session_start_injection";

// ===========================================================================
// Mode (pure) — mirrors `continuation_verdict::Mode` exactly
// ===========================================================================

/// The tri-state injection mode, parsed from [`FLAG_ENV`].
///
/// Deliberately a structural copy of [`crate::mcp::continuation_verdict::Mode`]
/// rather than a shared type: the two flags are switched independently and a
/// shared enum invites a future change to one from silently retuning the other.
/// The PARSE, however, must not drift — unknown/empty/absent ⇒ `Off`, so a
/// typo'd flag value fails SAFE (dark), never open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Feature dark (default): the route answers an EMPTY body with zero coord
    /// traffic. Unknown flag values also read as `Off` (fail-safe).
    Off,
    /// Render + log the would-be injection, then answer an empty body. The
    /// soak posture — proves the fetch and the payload before arming.
    Observe,
    /// Inject: the route answers the full hook envelope.
    On,
}

impl Mode {
    /// Parse the flag value. Unset / empty / UNRECOGNISED ⇒ `On` (the default);
    /// only the exact literal `off` disables.
    ///
    /// An unrecognised value resolves to the DEFAULT rather than to `off`, so a
    /// typo cannot quietly switch policy delivery off and leave every session
    /// running unpoliced — the silent-omission failure this module was built
    /// for. It is logged at `warn` so the typo is visible rather than absorbed.
    pub fn from_flag(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") => Mode::On,
            Some("on") => Mode::On,
            Some("observe") => Mode::Observe,
            Some("off") => Mode::Off,
            Some(other) => {
                warn!(
                    flag = FLAG_ENV,
                    value = other,
                    "unrecognised policy-injection mode; only the literal `off` disables — defaulting to `on`"
                );
                Mode::On
            }
        }
    }

    /// Read the live mode from the process env (the handler's entry).
    pub fn from_env() -> Self {
        Self::from_flag(std::env::var(FLAG_ENV).ok().as_deref())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Observe => "observe",
            Mode::On => "on",
        }
    }
}

// ===========================================================================
// URL builders (the regression-test surface — see tests below)
// ===========================================================================

/// The agent-door LIST url, filtered to policy documents. Coord scopes the rows
/// to the caller's tenant from the bearer — never pass a tenant here.
///
/// Same shape as `prompt_library::list_url`, which is the settled builder for
/// this door; only the `kind` and the [`VIA_MARKER`] differ.
fn list_url(base: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents?kind={KIND}&via={VIA_MARKER}",
        base.trim_end_matches('/')
    )
}

/// The agent-door single-document url.
///
/// `name` is percent-encoded rather than interpolated raw: today's only caller
/// passes the compile-time [`PROTOCOL_DOC_NAME`], but the builder must stay
/// safe for a server-supplied name (a space would build an unparseable URL, a
/// slash a path-traversing one). Slug-shaped names encode to themselves.
fn document_url(base: &str, name: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents/{KIND}/{}?via={VIA_MARKER}",
        base.trim_end_matches('/'),
        urlencoding::encode(name)
    )
}

/// Attach the session-attribution header, or send bare.
///
/// Coord validates this header FAIL-CLOSED (`agent_sessions::session_on_device`)
/// against the device the runner's JWT identifies, which is the whole reason an
/// injection is attributable at all: the runner is a trusted device asserting
/// "this read was for session S", and coord checks that S really is bound here.
///
/// `None` sends nothing. Coord then records `claude_session_id = NULL`, which
/// the compliance signal reads as `unavailable` — an admitted blind spot, never
/// a non-compliance verdict. **Never fabricate an id and never substitute the
/// runner terminal id**: the terminal id is a different id space, and seating it
/// in coord's durable provenance column would manufacture attribution that the
/// reconciler then reads as fact.
fn attach_attribution(
    req: reqwest::RequestBuilder,
    session: Option<uuid::Uuid>,
) -> reqwest::RequestBuilder {
    match session {
        Some(s) => req.header(crate::coord_mcp::CALLER_SESSION_HEADER, s.to_string()),
        None => req,
    }
}

/// Parse the hook-supplied Claude session id for attribution.
///
/// Strict UUID parse, no repair and no fallback. The route hands whatever the
/// hook extracted from the `SessionStart` stdin payload; anything that is not a
/// UUID is simply no attribution. This is the ONLY place a session id enters the
/// fetch path, which is what makes "never fabricate one" checkable.
pub fn parse_attribution_session(raw: Option<&str>) -> Option<uuid::Uuid> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    match uuid::Uuid::parse_str(raw) {
        Ok(id) => Some(id),
        Err(e) => {
            // `debug`, not `warn`: a session id the runner cannot parse is a
            // degraded ATTRIBUTION, not a degraded injection — the session still
            // gets its policy, and the honest downstream reading is
            // `unavailable`.
            debug!(
                value = %raw,
                error = %e,
                "policy-context: claude_session_id is not a UUID — fetching without attribution"
            );
            None
        }
    }
}

// ===========================================================================
// Payload shapes (pure)
// ===========================================================================

/// One entry in the injected policy index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDocSummary {
    /// The document name within `kind=policy` (`escalation-bar`).
    pub name: String,
    /// The one-line description from the document row.
    pub description: String,
    /// The row's `current_version`. Carried on EVERY entry — it is what lets a
    /// session tell a stale memory of a document from a current one, and it is
    /// the number a later version-awareness signal compares against. `None`
    /// only when coord omitted it, and renders as an explicit "version
    /// unknown" rather than a silently absent `v`.
    pub current_version: Option<i64>,
}

/// Everything one injection needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyPayload {
    /// The full body of `policy/session-protocol`. `None` when the index
    /// fetch succeeded but the body fetch did not — a partial the renderer
    /// degrades honestly rather than dropping.
    pub protocol_body: Option<String>,
    /// `session-protocol`'s `current_version`.
    pub protocol_version: Option<i64>,
    /// Every `kind=policy` document, name + description + version.
    pub index: Vec<PolicyDocSummary>,
}

// ===========================================================================
// Rendering (pure — the unit-test surface)
// ===========================================================================

/// Format a version for display. An absent version is stated, never elided:
/// "(version unknown)" tells a session it cannot compare against its memory,
/// while a missing `v` would look like the document has no versioning at all.
fn version_label(v: Option<i64>) -> String {
    match v {
        Some(n) => format!("v{n}"),
        None => "version unknown".to_string(),
    }
}

/// Normalize the hook's `source` to the known set. Anything else — absent,
/// empty, a value from a future Claude release — reads as [`DEFAULT_SOURCE`],
/// because an unrecognised start reason is still a start.
pub fn normalize_source(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("resume") => "resume",
        Some("compact") => "compact",
        Some("clear") => "clear",
        _ => DEFAULT_SOURCE,
    }
}

/// The one-line attribution header. Every injection carries it so the text is
/// traceable to WHO put it there, from WHERE, and WHEN — an unattributable
/// block of policy prose in a session's context is exactly the kind of
/// instruction the protocol's own `instruction-precedence` clause says carries
/// no weight.
fn header(source: &str, fetched_at: &str) -> String {
    format!(
        "[qontinui-runner] Fleet policy injected at SessionStart (source: {source}) \
         — pulled from coord `GET /coord/agent-prompt-documents` at {fetched_at}."
    )
}

/// Render the `additionalContext` for a successful (or partially successful)
/// pull.
///
/// Pure: everything time- or network-dependent is a parameter, so the exact
/// injected text is asserted in tests against a fixed payload.
pub fn render_injection(payload: &PolicyPayload, source: &str, fetched_at: &str) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(&header(source, fetched_at));
    out.push_str("\n\n");

    match payload.protocol_body.as_deref() {
        Some(body) => {
            out.push_str(&format!(
                "This is the canonical, freshly-pulled text of `policy/{PROTOCOL_DOC_NAME}` \
                 ({}), delivered by the runner so that Step 0 of the session protocol is \
                 satisfied for this session without you fetching it. Treat it as the \
                 authority. You do NOT need to re-read `{PROTOCOL_DOC_NAME}`; you DO still \
                 need to read the category bodies it names — the index below lists every \
                 one with its current version.\n\n",
                version_label(payload.protocol_version)
            ));
            out.push_str(&format!(
                "===== policy/{PROTOCOL_DOC_NAME} ({}) =====\n\n",
                version_label(payload.protocol_version)
            ));
            out.push_str(body.trim_end());
            out.push_str("\n\n");
        }
        None => {
            // Partial: the index came back but the body did not. Say so
            // plainly — an index alone silently missing the protocol would
            // read as "the protocol has nothing in it".
            out.push_str(&format!(
                "The runner could not retrieve the body of `policy/{PROTOCOL_DOC_NAME}` on \
                 this attempt, so Step 0 is NOT satisfied for this session. Fetch it \
                 yourself before substantive work: \
                 `coord_get_prompt_document(kind=\"{KIND}\", name=\"{PROTOCOL_DOC_NAME}\")`, \
                 or `GET /coord/agent-prompt-documents/{KIND}/{PROTOCOL_DOC_NAME}` over the \
                 device-authed HTTP door. The document index below did load and is \
                 current.\n\n"
            ));
        }
    }

    out.push_str(&format!(
        "===== Policy document index (kind={KIND}, {} document{}) =====\n\n",
        payload.index.len(),
        if payload.index.len() == 1 { "" } else { "s" }
    ));
    if payload.index.is_empty() {
        // Never a silent empty list (the `prompt_library` degrade rule): an
        // empty index that looks authoritative would tell a session this
        // tenant HAS no policies.
        out.push_str(
            "coord returned no policy documents. That is unexpected — treat it as a failed \
             read, not as an empty policy set, and list them yourself with \
             `coord_list_prompt_documents(kind=\"policy\")`.\n",
        );
        return out;
    }
    out.push_str(&format!(
        "Read a body with `coord_get_prompt_document(kind=\"{KIND}\", name=\"<name>\")`, or \
         `GET /coord/agent-prompt-documents/{KIND}/<name>` over the device-authed HTTP door \
         if the coord MCP tools are masked from your allow-set. The version on each line is \
         the CURRENT one — if you remember a document at a lower version, your memory is \
         stale.\n\n"
    ));
    for doc in &payload.index {
        let desc = doc.description.trim();
        if desc.is_empty() {
            out.push_str(&format!(
                "- {} ({})\n",
                doc.name,
                version_label(doc.current_version)
            ));
        } else {
            out.push_str(&format!(
                "- {} — {} ({})\n",
                doc.name,
                desc,
                version_label(doc.current_version)
            ));
        }
    }
    out
}

/// Render the `additionalContext` for a pull that failed outright.
///
/// This is the fail-open payload and the reason the route answers 200 on every
/// path: an injection that silently vanishes leaves the session in exactly the
/// pre-plan state — no policy and no signal. Saying "the pull failed, here is
/// the door" at least restores the advisory the briefing already carried, with
/// the specific reason attached.
pub fn render_failure_notice(reason: &str, source: &str, fetched_at: &str) -> String {
    format!(
        "{}\n\n\
         POLICY PULL FAILED: {reason}\n\n\
         The runner tried to deliver this tenant's policy documents into your context and \
         could not, so Step 0 of `policy/{PROTOCOL_DOC_NAME}` is NOT satisfied for this \
         session. Fetch the policies yourself before substantive work:\n\n\
         - `coord_list_prompt_documents(kind=\"{KIND}\")` then \
         `coord_get_prompt_document(kind=\"{KIND}\", name=\"{PROTOCOL_DOC_NAME}\")`; or\n\
         - if the coord MCP tools are masked from your allow-set, the equal-authority \
         device-authed HTTP door: `GET /coord/agent-prompt-documents` (list, optional \
         `?kind=` filter) and `GET /coord/agent-prompt-documents/{{kind}}/{{name}}` (one \
         body).\n\n\
         Do not work from memory of these documents — they version frequently.",
        header(source, fetched_at)
    )
}

/// Wrap rendered text in the Claude `SessionStart` hook envelope — the shape
/// Claude reads from a hook's stdout and splices into the session's context.
pub fn envelope(additional_context: &str) -> Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": additional_context,
        }
    })
}

// ===========================================================================
// Response parsing (pure)
// ===========================================================================

/// Pull the summary array out of coord's list response — `documents: [...]`
/// or a bare array. Mirrors `prompt_library::list_documents`; both doors have
/// served both shapes.
fn list_documents(body: &Value) -> Vec<Value> {
    if let Some(docs) = body.get("documents").and_then(Value::as_array) {
        return docs.clone();
    }
    body.as_array().cloned().unwrap_or_default()
}

/// Unwrap coord's row envelope: some surfaces serve the row flat, others under
/// `document`.
fn unwrap_document(body: &Value) -> &Value {
    body.get("document").unwrap_or(body)
}

/// Build the injected index from a coord list body.
///
/// A summary with no `name` is skipped (it cannot be fetched, so listing it
/// would be an instruction to call a route that does not resolve). Everything
/// else degrades rather than drops: a missing description renders bare, a
/// missing version renders as "version unknown".
fn parse_index(body: &Value) -> Vec<PolicyDocSummary> {
    list_documents(body)
        .iter()
        .filter_map(|doc| {
            let name = doc.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some(PolicyDocSummary {
                name: name.to_string(),
                description: doc
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                current_version: doc.get("current_version").and_then(Value::as_i64),
            })
        })
        .collect()
}

/// The `current_version` of a single fetched document row.
fn parse_document_version(body: &Value) -> Option<i64> {
    unwrap_document(body)
        .get("current_version")
        .and_then(Value::as_i64)
}

// ===========================================================================
// Cache (45s TTL + conditional ETag) — `prompt_library` posture
// ===========================================================================

struct CacheEntry {
    at: Instant,
    /// The LIST response's `ETag`, replayed as `If-None-Match` so a warm cache
    /// costs coord a 304 instead of a full list.
    list_etag: Option<String>,
    /// The `session-protocol` DOCUMENT response's `ETag`, replayed the same way.
    ///
    /// This one is what makes the recording round-trip affordable: the body is
    /// ~8 KB and the validator is ~20 bytes, so a warm cache re-reads the
    /// protocol for the price of a 304 — see the module docs on caching vs
    /// recording.
    doc_etag: Option<String>,
    payload: PolicyPayload,
}

/// What the cache holds for one coord base.
#[derive(Default)]
struct CacheSnapshot {
    /// The payload, when it is still within [`CACHE_TTL`].
    fresh: Option<PolicyPayload>,
    /// The payload at any age — what gets served when coord is unreachable.
    any: Option<PolicyPayload>,
    list_etag: Option<String>,
    doc_etag: Option<String>,
}

/// Keyed on the resolved coord base URL.
///
/// The plan asks for a tenant key; the coord base is the closest thing this
/// process can actually observe, because tenancy is lifted server-side from the
/// device JWT and never appears in the URL. Re-pointing the runner at a
/// different coord (the only way its tenant changes without a restart) changes
/// this key, so the cache cannot serve one coord's policies against another's.
static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_snapshot(base: &str) -> CacheSnapshot {
    let Ok(g) = cache().lock() else {
        return CacheSnapshot::default();
    };
    let Some(entry) = g.get(base) else {
        return CacheSnapshot::default();
    };
    CacheSnapshot {
        fresh: (entry.at.elapsed() < CACHE_TTL).then(|| entry.payload.clone()),
        any: Some(entry.payload.clone()),
        list_etag: entry.list_etag.clone(),
        doc_etag: entry.doc_etag.clone(),
    }
}

fn cache_store(
    base: &str,
    list_etag: Option<String>,
    doc_etag: Option<String>,
    payload: PolicyPayload,
) {
    if let Ok(mut g) = cache().lock() {
        g.insert(
            base.to_string(),
            CacheEntry {
                at: Instant::now(),
                list_etag,
                doc_etag,
                payload,
            },
        );
    }
}

// ===========================================================================
// Coord fetch (every failure ⇒ Err ⇒ the fail-open notice)
// ===========================================================================

/// What one conditional fetch of the LIST came back as.
enum ListOutcome {
    /// 304 — the cached index still stands, and coord recorded the read.
    NotModified,
    Fresh {
        index: Vec<PolicyDocSummary>,
        etag: Option<String>,
    },
    /// Transport error, non-2xx, or an undecodable body. Carries the HUMAN
    /// reason that would be rendered into the fail-open notice.
    Failed(String),
}

/// What one conditional fetch of `policy/session-protocol` came back as.
enum ProtocolOutcome {
    /// 304 — the cached body still stands, and coord recorded the read. This is
    /// the arm the whole conditional-fetch design exists to produce.
    NotModified,
    Fresh {
        body: String,
        version: Option<i64>,
        etag: Option<String>,
    },
    Failed(String),
}

/// Fetch the injection payload for `base`.
///
/// ## Caching and RECORDING are separate concerns (the crux)
///
/// The 45 s cache exists to avoid re-FETCHING ~8 KB of document bodies. It must
/// NOT suppress coord's record of the read, because that record is the only
/// evidence a session ever pulled policy — and a cache that silenced it would
/// make the compliance signal blind on precisely the warm-cache session starts
/// that are the common case, reporting "never pulled" about sessions the runner
/// had just handed the policy to.
///
/// So when there is a session to attribute the read to, BOTH coord reads go out
/// on EVERY call, conditionally (`If-None-Match` against the stored validators).
/// Coord answers 304 with an empty payload — and records a 304 as a real read on
/// both doors, deliberately: a conditional re-poll IS a read, the caller now
/// holds current content verified against coord's own ETag, and counting it
/// otherwise would make a well-behaved caching client look less compliant than a
/// naive one. Net cost per session start: two near-empty round-trips, which is
/// exactly the granularity wanted.
///
/// The TTL still short-circuits when there is NO attribution — a NULL-attributed
/// row is unusable by the signal, so the round-trip would buy nothing and the
/// cache's original purpose stands unchanged.
///
/// `Err(reason)` is a HUMAN reason that gets rendered verbatim into the
/// fail-open notice, so it must name what went wrong, not just that something
/// did.
async fn fetch_payload(
    base: &str,
    attribution: Option<uuid::Uuid>,
) -> Result<PolicyPayload, String> {
    let snap = cache_snapshot(base);
    if attribution.is_none() {
        if let Some(payload) = snap.fresh.clone() {
            debug!(
                "policy-context: served from cache — no attributable session, so the \
                 recording round-trip would buy nothing"
            );
            return Ok(payload);
        }
    }

    let client = crate::mcp::continuation_verdict::http_client()?;

    // Both reads go out. `coord_get` attaches the device bearer itself (re-read
    // per call — the JWT has a short TTL) and drives the data-plane
    // auth-coverage metric, which is why the caller's `coord_client_parts`
    // token is used only to detect "unpaired" and is not threaded down here:
    // one credential source, read as late as possible.
    let list = fetch_list(&client, base, snap.list_etag.as_deref(), attribution).await;
    let protocol = fetch_protocol(&client, base, snap.doc_etag.as_deref(), attribution).await;

    // ---- Compose. Each half falls back to its cached counterpart independently,
    // ---- so one failing door never discards the other's fresh answer.
    let (index, list_etag) = match list {
        ListOutcome::Fresh { index, etag } => (index, etag),
        ListOutcome::NotModified => (
            snap.any
                .as_ref()
                .map(|p| p.index.clone())
                .unwrap_or_default(),
            snap.list_etag.clone(),
        ),
        ListOutcome::Failed(ref reason) => {
            // No cached payload and no list ⇒ nothing to serve but the notice.
            // With a cached one, serve it: yesterday's policy beats the "pull
            // failed" notice, and the injection header states the fetch time so
            // the staleness is visible.
            let Some(stale) = snap.any.as_ref() else {
                return Err(reason.clone());
            };
            warn!(
                reason = %reason,
                "policy-context: list fetch failed — serving the cached index"
            );
            (stale.index.clone(), snap.list_etag.clone())
        }
    };

    let (protocol_body, protocol_version, doc_etag) = match protocol {
        ProtocolOutcome::Fresh {
            body,
            version,
            etag,
        } => (Some(body), version, etag),
        ProtocolOutcome::NotModified => (
            snap.any.as_ref().and_then(|p| p.protocol_body.clone()),
            snap.any.as_ref().and_then(|p| p.protocol_version),
            snap.doc_etag.clone(),
        ),
        ProtocolOutcome::Failed(ref reason) => {
            // A body failure is a PARTIAL, not a failure: the index is still
            // worth delivering, and the renderer says plainly that the protocol
            // body is missing.
            warn!(
                reason = %reason,
                "policy-context: session-protocol fetch failed — falling back to the cached body"
            );
            (
                snap.any.as_ref().and_then(|p| p.protocol_body.clone()),
                snap.any.as_ref().and_then(|p| p.protocol_version),
                snap.doc_etag.clone(),
            )
        }
    };

    let payload = PolicyPayload {
        protocol_body,
        protocol_version,
        index,
    };
    // Do NOT cache a partial: a transient blip on the body fetch must not pin an
    // index-only payload for a full TTL when the real body is one retry away.
    // Same reasoning `continuation_verdict` applies to its fallback.
    if payload.protocol_body.is_some() {
        cache_store(base, list_etag, doc_etag, payload.clone());
    }
    if payload.protocol_body.is_none() && payload.index.is_empty() {
        return Err(
            "coord returned neither the policy index nor the session-protocol body".to_string(),
        );
    }
    Ok(payload)
}

/// Conditionally fetch the `kind=policy` index.
async fn fetch_list(
    client: &reqwest::Client,
    base: &str,
    etag: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> ListOutcome {
    let mut req = attach_attribution(
        crate::coord_http::coord_get(client, list_url(base)),
        attribution,
    );
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return ListOutcome::Failed(format!("coord unreachable: {e}")),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return ListOutcome::NotModified;
    }
    if !status.is_success() {
        return ListOutcome::Failed(format!(
            "coord answered HTTP {} to the policy-document list",
            status.as_u16()
        ));
    }
    let etag = response_etag(&resp);
    match resp.json::<Value>().await {
        Ok(body) => ListOutcome::Fresh {
            index: parse_index(&body),
            etag,
        },
        Err(e) => ListOutcome::Failed(format!("coord policy-document list was undecodable: {e}")),
    }
}

/// Conditionally fetch `policy/session-protocol`'s body + version.
///
/// The `If-None-Match` here is what turns the per-session-start recording read
/// into a near-empty one: coord's agent single-document door answers 304 with no
/// body and still records the read at the version it would have served.
async fn fetch_protocol(
    client: &reqwest::Client,
    base: &str,
    etag: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> ProtocolOutcome {
    let mut req = attach_attribution(
        crate::coord_http::coord_get(client, document_url(base, PROTOCOL_DOC_NAME)),
        attribution,
    );
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return ProtocolOutcome::Failed(format!("request: {e}")),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return ProtocolOutcome::NotModified;
    }
    if !status.is_success() {
        return ProtocolOutcome::Failed(format!("coord answered HTTP {}", status.as_u16()));
    }
    let etag = response_etag(&resp);
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return ProtocolOutcome::Failed(format!("undecodable: {e}")),
    };
    // Reuse the settled row-envelope reader — it unwraps `document`, trims, and
    // treats an empty body as absent.
    match crate::mcp::continuation_verdict::rules_from_doc_body(&body) {
        Some(text) => ProtocolOutcome::Fresh {
            body: text,
            version: parse_document_version(&body),
            etag,
        },
        None => {
            ProtocolOutcome::Failed("coord returned an empty session-protocol body".to_string())
        }
    }
}

/// The response's `ETag`, when it carries a header-safe one.
fn response_etag(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// UTC timestamp for the attribution header, second resolution.
fn now_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ===========================================================================
// Endpoint entry
// ===========================================================================

/// Produce the `SessionStart` hook envelope to inject, or `None` to inject
/// nothing.
///
/// `None` is returned ONLY for the two dark modes — it is never an error path.
/// Every failure resolves to `Some(envelope(fail-open notice))`, because a
/// session that silently receives nothing is in exactly the pre-plan state this
/// module exists to end.
pub async fn policy_context(
    session_key: &str,
    source: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> Option<Value> {
    let mode = Mode::from_env();
    let source = normalize_source(source);

    if mode == Mode::Off {
        debug!(
            session = %session_key,
            source,
            "policy-context: {FLAG_ENV} is off — no injection, no coord traffic"
        );
        return None;
    }

    if attribution.is_none() {
        debug!(
            session = %session_key,
            source,
            "policy-context: no parseable Claude session id — fetching without the \
             attribution header; coord will record this read with a NULL session"
        );
    }

    let fetched_at = now_stamp();
    // `coord_client_parts` is the shared accessor `continuation_verdict` and
    // `session_compliance` both use — same credential, same coord base. Its
    // error is the honest "unpaired" reason, which the notice renders verbatim.
    let text = match crate::mcp::continuation_verdict::coord_client_parts() {
        Ok((base, _jwt)) => match fetch_payload(&base, attribution).await {
            Ok(payload) => render_injection(&payload, source, &fetched_at),
            Err(reason) => {
                warn!(session = %session_key, source, reason = %reason, "policy-context: pull failed — injecting the fail-open notice");
                render_failure_notice(&reason, source, &fetched_at)
            }
        },
        Err(reason) => {
            warn!(session = %session_key, source, reason = %reason, "policy-context: cannot consult coord — injecting the fail-open notice");
            render_failure_notice(&reason, source, &fetched_at)
        }
    };

    if mode == Mode::Observe {
        // The soak: prove the fetch and the payload without touching a single
        // session's context. The summary goes to `info` and the full text to
        // `debug`, because the payload is ~8 KB and this fires per session.
        info!(
            session = %session_key,
            source,
            mode = mode.as_str(),
            bytes = text.len(),
            first_line = text.lines().next().unwrap_or_default(),
            "policy-context: WOULD inject (observe mode — nothing was injected)"
        );
        debug!(session = %session_key, would_inject = %text, "policy-context: observe payload");
        return None;
    }

    info!(
        session = %session_key,
        source,
        mode = mode.as_str(),
        bytes = text.len(),
        "policy-context: injecting fleet policy at SessionStart"
    );
    Some(envelope(&text))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> PolicyPayload {
        PolicyPayload {
            protocol_body: Some("Step 0 — read the policies, fresh.".to_string()),
            protocol_version: Some(6),
            index: vec![
                PolicyDocSummary {
                    name: "escalation-bar".to_string(),
                    description: "Escalation Bar".to_string(),
                    current_version: Some(4),
                },
                PolicyDocSummary {
                    name: "session-protocol".to_string(),
                    description: "The session protocol every coord-mcp session pulls at start"
                        .to_string(),
                    current_version: Some(6),
                },
            ],
        }
    }

    // ── Flag parse (defaults to ON) ─────────────────────────────────────

    #[test]
    fn mode_parses_the_three_values_and_defaults_to_on() {
        assert_eq!(Mode::from_flag(Some("on")), Mode::On);
        assert_eq!(Mode::from_flag(Some("observe")), Mode::Observe);
        assert_eq!(Mode::from_flag(Some("off")), Mode::Off);
        // Case + whitespace tolerant, exactly like the continuation flag.
        assert_eq!(Mode::from_flag(Some("  ON  ")), Mode::On);
        assert_eq!(Mode::from_flag(Some("Observe")), Mode::Observe);

        // THE HEADLINE: an unconfigured runner INJECTS. This is the assertion a
        // mutant restoring `_ => Mode::Off` has to fail.
        assert_eq!(Mode::from_flag(None), Mode::On, "unset ⇒ inject");
        assert_eq!(Mode::from_flag(Some("")), Mode::On, "empty ⇒ inject");
        assert_eq!(Mode::from_flag(Some("   ")), Mode::On, "blank ⇒ inject");

        // This REVERSES the invariant the flag shipped with — "everything else
        // is DARK; a typo must never arm an injection". With the default at
        // `on` a typo can no longer ARM anything, it is already armed, so the
        // only reachable typo hazard is the opposite one: silently DISARMING
        // policy delivery, which is the incident this module exists to prevent.
        for typo in ["true", "enabled", "onn", "0", "false", "no", "of"] {
            assert_eq!(
                Mode::from_flag(Some(typo)),
                Mode::On,
                "{typo:?}: only the literal `off` disables injection"
            );
        }

        // ...and the disable path still works, case- and space-tolerant, or the
        // escape hatch this design rests on would be unusable.
        for disable in ["off", "OFF", "  Off  "] {
            assert_eq!(Mode::from_flag(Some(disable)), Mode::Off, "{disable:?}");
        }
    }

    #[test]
    fn mode_flag_env_name_is_the_documented_one() {
        assert_eq!(FLAG_ENV, "QONTINUI_POLICY_INJECTION");
        assert_eq!(Mode::Off.as_str(), "off");
        assert_eq!(Mode::Observe.as_str(), "observe");
        assert_eq!(Mode::On.as_str(), "on");
    }

    // ── URL shape (the door regression) ─────────────────────────────────

    /// The fetches MUST target coord's device/agent door, never the operator
    /// `TenantId`-gated one: `/coord/prompt-documents/*` 403s a device JWT
    /// (confirmed against prod), and because this path fail-opens, pointing
    /// there would hide the mistake forever behind a permanent "pull failed"
    /// notice. Mirrors `continuation_verdict.rs` and `prompt_library.rs`.
    #[test]
    fn list_url_uses_the_device_authed_door_not_the_operator_one() {
        let url = list_url("https://coord.example.com");
        assert_eq!(
            url,
            "https://coord.example.com/coord/agent-prompt-documents?kind=policy&via=session_start_injection"
        );
        assert!(url.contains("/coord/agent-prompt-documents"));
        assert!(
            !url.contains("/coord/prompt-documents"),
            "the operator TenantId door 403s a device JWT: {url}"
        );
    }

    #[test]
    fn document_url_uses_the_device_authed_door_not_the_operator_one() {
        let url = document_url("https://coord.example.com/", PROTOCOL_DOC_NAME);
        assert_eq!(
            url,
            "https://coord.example.com/coord/agent-prompt-documents/policy/session-protocol?via=session_start_injection"
        );
        assert!(url.contains("/coord/agent-prompt-documents/"));
        assert!(
            !url.contains("/coord/prompt-documents/"),
            "the operator TenantId door 403s a device JWT: {url}"
        );
    }

    #[test]
    fn document_url_percent_encodes_the_name_and_urls_never_carry_a_tenant() {
        let url = document_url("https://coord.example.com", "odd name/../policy");
        assert!(url.starts_with("https://coord.example.com/coord/agent-prompt-documents/policy/"));
        assert!(!url.contains(' '), "space must be encoded: {url}");
        assert!(
            !url.contains("/../"),
            "a traversal segment must be encoded, not preserved: {url}"
        );
        // Coord scopes rows from the bearer; a tenant in the URL is the
        // operator-door pattern leaking back in.
        for url in [
            list_url("https://coord.example.com"),
            document_url("https://coord.example.com", PROTOCOL_DOC_NAME),
        ] {
            assert!(!url.contains("tenant"), "no tenant in the URL: {url}");
        }
    }

    /// Both doors carry the `?via=` marker coord maps to
    /// `source = 'session_start_injection'`. Without it the read is recorded as
    /// a plain `http_door` pull and the injection stops being distinguishable
    /// from a session reading for itself — which is the one thing the source
    /// column exists to tell apart.
    #[test]
    fn both_urls_carry_the_session_start_injection_marker() {
        for url in [
            list_url("https://coord.example.com"),
            document_url("https://coord.example.com", PROTOCOL_DOC_NAME),
        ] {
            assert!(
                url.contains("via=session_start_injection"),
                "the read must be attributable to the injection: {url}"
            );
        }
        // The literal is a contract with coord's `PolicyReadSource::from_http_via`,
        // which honours ONLY this exact spelling and degrades anything else to
        // `http_door`. Pin it here so a rename cannot silently demote the source.
        assert_eq!(VIA_MARKER, "session_start_injection");
    }

    // ── Attribution ─────────────────────────────────────────────────────

    /// The Claude session id is parsed STRICTLY. Anything that is not a UUID —
    /// including a runner terminal id, which is the id that would otherwise be
    /// in reach — yields no attribution at all rather than a fabricated one.
    #[test]
    fn attribution_parses_only_a_real_uuid_and_never_invents_one() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            parse_attribution_session(Some(&id.to_string())),
            Some(id),
            "a canonical UUID is the attribution"
        );
        assert_eq!(
            parse_attribution_session(Some(&format!("  {id}  "))),
            Some(id),
            "surrounding whitespace from a shell-built query string is tolerated"
        );

        for hostile in [
            "",
            "   ",
            "not-a-uuid",
            // A runner TERMINAL id — the exact wrong id space. Coord's
            // `session_on_device` would reject it, but the runner must not send
            // it in the first place.
            "term-4",
            "terminal-0f2a",
            // Half a UUID, and a UUID with junk appended.
            "23d4d5c0-ddef-4fc2-a541",
            "23d4d5c0-ddef-4fc2-a541-5324a1eea8f6-extra",
        ] {
            assert_eq!(
                parse_attribution_session(Some(hostile)),
                None,
                "`{hostile}` must not become an attribution"
            );
        }
        assert_eq!(parse_attribution_session(None), None);
    }

    /// `attach_attribution` is the only place the header is set, so its two
    /// arms ARE the "never fabricate an id" rule.
    #[test]
    fn the_caller_session_header_is_set_only_when_there_is_a_session() {
        let client = reqwest::Client::new();
        let id = uuid::Uuid::new_v4();

        let with = attach_attribution(client.get("http://127.0.0.1/x"), Some(id))
            .build()
            .expect("request builds");
        assert_eq!(
            with.headers()
                .get(crate::coord_mcp::CALLER_SESSION_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(id.to_string().as_str())
        );

        let without = attach_attribution(client.get("http://127.0.0.1/x"), None)
            .build()
            .expect("request builds");
        assert!(
            without
                .headers()
                .get(crate::coord_mcp::CALLER_SESSION_HEADER)
                .is_none(),
            "no session ⇒ no header at all; coord then records a NULL session, \
             which reads as `unavailable` rather than as non-compliance"
        );
    }

    // ── Cache vs recording ──────────────────────────────────────────────

    /// The cache stores BOTH validators. The document one is the load-bearing
    /// addition: it is what lets a warm cache re-read `session-protocol` — and
    /// so make the session's pull observable — for the price of a 304 instead
    /// of ~8 KB of body.
    #[test]
    fn the_cache_round_trips_both_validators() {
        let base = format!("https://cache-test-{}.example.com", uuid::Uuid::new_v4());
        assert!(cache_snapshot(&base).any.is_none(), "starts empty");

        cache_store(
            &base,
            Some("\"14:aaaa\"".to_string()),
            Some("\"1:bbbb\"".to_string()),
            sample_payload(),
        );

        let snap = cache_snapshot(&base);
        assert_eq!(snap.list_etag.as_deref(), Some("\"14:aaaa\""));
        assert_eq!(
            snap.doc_etag.as_deref(),
            Some("\"1:bbbb\""),
            "without a document validator the recording read would have to pull the body"
        );
        assert_eq!(snap.any.as_ref(), Some(&sample_payload()));
        assert!(
            snap.fresh.is_some(),
            "a just-stored entry is inside the TTL"
        );
    }

    // ── Rendering ───────────────────────────────────────────────────────

    #[test]
    fn render_injection_carries_the_protocol_body_and_a_versioned_index() {
        let text = render_injection(&sample_payload(), "startup", "2026-08-19T12:00:00Z");

        // Attributable: who, from where, when, and why the session is starting.
        assert!(text.starts_with("[qontinui-runner]"), "{text}");
        assert!(text.contains("source: startup"));
        assert!(text.contains("2026-08-19T12:00:00Z"));
        assert!(text.contains("GET /coord/agent-prompt-documents"));

        // The FULL protocol body, with its version.
        assert!(text.contains("Step 0 — read the policies, fresh."));
        assert!(text.contains("policy/session-protocol (v6)"));

        // The index: every document, with `current_version` on EVERY entry —
        // that number is what distinguishes a stale memory from a current one.
        assert!(text.contains("(kind=policy, 2 documents)"));
        assert!(text.contains("- escalation-bar — Escalation Bar (v4)"));
        assert!(text.contains(
            "- session-protocol — The session protocol every coord-mcp session pulls at start (v6)"
        ));

        // Only ONE body is injected — the index names the others, it does not
        // inline them (the ~2.2k vs ~18k tokens/session decision).
        assert!(
            !text.contains("Escalation Bar\n\n=====") && text.matches("=====").count() == 4,
            "exactly two delimited sections: the protocol body and the index"
        );

        // Both doors named for the rest, so a masked-tools session is not stuck.
        assert!(text.contains("coord_get_prompt_document(kind=\"policy\", name=\"<name>\")"));
        assert!(text.contains("GET /coord/agent-prompt-documents/policy/<name>"));
    }

    #[test]
    fn render_injection_states_an_unknown_version_instead_of_eliding_it() {
        let payload = PolicyPayload {
            protocol_body: Some("body".to_string()),
            protocol_version: None,
            index: vec![PolicyDocSummary {
                name: "coordination".to_string(),
                description: String::new(),
                current_version: None,
            }],
        };
        let text = render_injection(&payload, "resume", "2026-08-19T12:00:00Z");
        assert!(text.contains("policy/session-protocol (version unknown)"));
        // A description-less entry renders bare, never with a dangling dash.
        assert!(text.contains("- coordination (version unknown)"));
        assert!(!text.contains("coordination — ("));
        // Singular/plural agreement on the count.
        assert!(text.contains("(kind=policy, 1 document)"));
    }

    #[test]
    fn render_injection_degrades_honestly_when_only_the_body_is_missing() {
        let payload = PolicyPayload {
            protocol_body: None,
            protocol_version: None,
            index: sample_payload().index,
        };
        let text = render_injection(&payload, "compact", "2026-08-19T12:00:00Z");
        assert!(text.contains("source: compact"));
        assert!(
            text.contains("could not retrieve the body of `policy/session-protocol`"),
            "the missing body is STATED, not silently absent: {text}"
        );
        assert!(text.contains("Step 0 is NOT satisfied"));
        // The index still ships — a partial is worth more than a total failure.
        assert!(text.contains("- escalation-bar — Escalation Bar (v4)"));
    }

    #[test]
    fn render_injection_never_presents_an_empty_index_as_authoritative() {
        let payload = PolicyPayload {
            protocol_body: Some("body".to_string()),
            protocol_version: Some(6),
            index: Vec::new(),
        };
        let text = render_injection(&payload, "startup", "2026-08-19T12:00:00Z");
        assert!(text.contains("(kind=policy, 0 documents)"));
        assert!(
            text.contains("treat it as a failed read, not as an empty policy set"),
            "an empty list must never read as 'this tenant has no policies': {text}"
        );
    }

    // ── Fail-open notice ────────────────────────────────────────────────

    #[test]
    fn failure_notice_names_the_reason_and_the_agent_door() {
        let text = render_failure_notice(
            "no device JWT (unpaired)",
            "startup",
            "2026-08-19T12:00:00Z",
        );
        assert!(text.starts_with("[qontinui-runner]"));
        assert!(text.contains("POLICY PULL FAILED: no device JWT (unpaired)"));
        assert!(text.contains("Step 0 of `policy/session-protocol` is NOT satisfied"));
        // The escape hatch must name the AGENT door — the whole point of the
        // plan's briefing fix is that the operator door 403s a device JWT.
        assert!(text.contains("GET /coord/agent-prompt-documents"));
        assert!(
            !text.contains("GET /coord/prompt-documents"),
            "the notice must never advertise the operator door: {text}"
        );
        assert!(text.contains("coord_list_prompt_documents"));
        assert!(text.contains("coord_get_prompt_document"));
    }

    // ── Envelope ────────────────────────────────────────────────────────

    #[test]
    fn envelope_is_the_claude_session_start_hook_contract() {
        let v = envelope("hello");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "hello");
        // Serializes to the exact wire shape the hook script prints verbatim.
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"hookSpecificOutput\""));
        assert!(s.contains("\"hookEventName\":\"SessionStart\""));
    }

    // ── source normalization ────────────────────────────────────────────

    #[test]
    fn every_source_is_accepted_and_unknown_ones_read_as_startup() {
        // All three plan-named sources inject: a resumed session lacks the
        // policies as they NOW stand, a compacted one just had them evicted.
        assert_eq!(normalize_source(Some("startup")), "startup");
        assert_eq!(normalize_source(Some("resume")), "resume");
        assert_eq!(normalize_source(Some("compact")), "compact");
        assert_eq!(normalize_source(Some("clear")), "clear");
        assert_eq!(normalize_source(Some("  RESUME ")), "resume");
        // An unrecognised start is still a start.
        assert_eq!(normalize_source(None), "startup");
        assert_eq!(normalize_source(Some("")), "startup");
        assert_eq!(normalize_source(Some("teleport")), "startup");
    }

    // ── Response parsing ────────────────────────────────────────────────

    #[test]
    fn parse_index_reads_both_envelope_shapes_and_keeps_current_version() {
        let enveloped = serde_json::json!({
            "documents": [
                {"name": "coordination", "description": "Coordination policy", "current_version": 11},
                {"name": "ux-priorities", "description": "UX Priorities", "current_version": 1},
            ],
            "total": 2
        });
        let index = parse_index(&enveloped);
        assert_eq!(index.len(), 2);
        assert_eq!(index[0].name, "coordination");
        assert_eq!(index[0].current_version, Some(11));

        let bare = serde_json::json!([{"name": "a", "current_version": 3}]);
        assert_eq!(parse_index(&bare).len(), 1);
        assert_eq!(parse_index(&serde_json::json!({})).len(), 0);
    }

    #[test]
    fn parse_index_skips_unaddressable_entries_and_tolerates_missing_fields() {
        let body = serde_json::json!({
            "documents": [
                {"description": "no name at all"},
                {"name": "   "},
                {"name": "ok"},
            ]
        });
        let index = parse_index(&body);
        assert_eq!(index.len(), 1, "only the addressable row survives");
        assert_eq!(index[0].name, "ok");
        assert_eq!(index[0].description, "");
        assert_eq!(index[0].current_version, None);
    }

    #[test]
    fn parse_document_version_reads_flat_and_enveloped_rows() {
        let flat = serde_json::json!({"name": "session-protocol", "current_version": 6});
        assert_eq!(parse_document_version(&flat), Some(6));
        let enveloped = serde_json::json!({"document": {"current_version": 6}, "found": true});
        assert_eq!(parse_document_version(&enveloped), Some(6));
        assert_eq!(parse_document_version(&serde_json::json!({})), None);
    }
}
