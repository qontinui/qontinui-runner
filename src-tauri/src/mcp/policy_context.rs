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

/// Env flag gating the whole feature. `off` (default) ⇒ no injection and zero
/// coord traffic; `observe` ⇒ render + log what WOULD be injected, inject
/// nothing; `on` ⇒ inject.
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
    /// Parse the flag value. `None`/empty/unknown ⇒ `Off` (fail-safe).
    pub fn from_flag(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("on") => Mode::On,
            Some("observe") => Mode::Observe,
            _ => Mode::Off,
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
/// this door; only the `kind` differs.
fn list_url(base: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents?kind={KIND}",
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
        "{}/coord/agent-prompt-documents/{KIND}/{}",
        base.trim_end_matches('/'),
        urlencoding::encode(name)
    )
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
    /// The list response's `ETag`, replayed as `If-None-Match` so a warm cache
    /// costs coord a 304 instead of a full list + hydration.
    etag: Option<String>,
    payload: PolicyPayload,
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

/// `(fresh payload, replayable etag, any cached payload)` for `base`.
fn cache_snapshot(base: &str) -> (Option<PolicyPayload>, Option<String>, Option<PolicyPayload>) {
    let Ok(g) = cache().lock() else {
        return (None, None, None);
    };
    let Some(entry) = g.get(base) else {
        return (None, None, None);
    };
    let fresh = if entry.at.elapsed() < CACHE_TTL {
        Some(entry.payload.clone())
    } else {
        None
    };
    (fresh, entry.etag.clone(), Some(entry.payload.clone()))
}

fn cache_store(base: &str, etag: Option<String>, payload: PolicyPayload) {
    if let Ok(mut g) = cache().lock() {
        g.insert(
            base.to_string(),
            CacheEntry {
                at: Instant::now(),
                etag,
                payload,
            },
        );
    }
}

/// On a 304, refresh the TTL stamp so the next 45s serve straight from cache
/// without another conditional round-trip.
fn cache_touch(base: &str) {
    if let Ok(mut g) = cache().lock() {
        if let Some(entry) = g.get_mut(base) {
            entry.at = Instant::now();
        }
    }
}

// ===========================================================================
// Coord fetch (every failure ⇒ Err ⇒ the fail-open notice)
// ===========================================================================

/// Fetch the injection payload for `base`, serving the 45s cache when warm.
///
/// `Err(reason)` is a HUMAN reason that gets rendered verbatim into the
/// fail-open notice, so it must name what went wrong, not just that something
/// did.
async fn fetch_payload(base: &str) -> Result<PolicyPayload, String> {
    let (fresh, etag, any_cached) = cache_snapshot(base);
    if let Some(payload) = fresh {
        debug!("policy-context: served from cache");
        return Ok(payload);
    }

    let client = crate::mcp::continuation_verdict::http_client()?;

    // Conditional list fetch. `coord_get` attaches the device bearer itself
    // (re-read per call — the JWT has a short TTL) and drives the data-plane
    // auth-coverage metric, which is why the caller's `coord_client_parts`
    // token is used only to detect "unpaired" and is not threaded down here:
    // one credential source, read as late as possible.
    let mut req = crate::coord_http::coord_get(&client, list_url(base));
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("coord unreachable: {e}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        cache_touch(base);
        return any_cached.ok_or_else(|| {
            // A 304 with nothing cached means the entry was evicted between the
            // snapshot and the response. Report it rather than serving empty.
            "coord answered 304 but the cached policy payload was gone".to_string()
        });
    }
    if !status.is_success() {
        // Serve a stale payload rather than nothing: yesterday's policy beats
        // the "pull failed" notice, and the header states the fetch time so the
        // staleness is visible.
        if let Some(stale) = any_cached {
            warn!(
                status = status.as_u16(),
                "policy-context: list non-2xx — serving stale cached payload"
            );
            return Ok(stale);
        }
        return Err(format!(
            "coord answered HTTP {} to the policy-document list",
            status.as_u16()
        ));
    }

    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let list_body: Value = resp
        .json()
        .await
        .map_err(|e| format!("coord policy-document list was undecodable: {e}"))?;
    let index = parse_index(&list_body);

    // Hydrate the ONE body we inject. A failure here is a partial, not a
    // failure: the index is still worth delivering, and the renderer says
    // plainly that the protocol body is missing.
    let (protocol_body, protocol_version) = match fetch_protocol(&client, base).await {
        Ok(pair) => pair,
        Err(e) => {
            warn!(reason = %e, "policy-context: session-protocol body fetch failed — injecting index only");
            (None, None)
        }
    };

    let payload = PolicyPayload {
        protocol_body,
        protocol_version,
        index,
    };
    // Do NOT cache a partial: a transient blip on the body fetch must not pin
    // an index-only payload for a full TTL when the real body is one retry
    // away. Same reasoning `continuation_verdict` applies to its fallback.
    if payload.protocol_body.is_some() {
        cache_store(base, new_etag, payload.clone());
    }
    Ok(payload)
}

/// Fetch `policy/session-protocol`'s body + version.
async fn fetch_protocol(
    client: &reqwest::Client,
    base: &str,
) -> Result<(Option<String>, Option<i64>), String> {
    let url = document_url(base, PROTOCOL_DOC_NAME);
    let resp = crate::coord_http::coord_get(client, url)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("coord answered HTTP {}", status.as_u16()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("undecodable: {e}"))?;
    // Reuse the settled row-envelope reader — it unwraps `document`, trims, and
    // treats an empty body as absent.
    let text = crate::mcp::continuation_verdict::rules_from_doc_body(&body)
        .ok_or_else(|| "coord returned an empty session-protocol body".to_string())?;
    Ok((Some(text), parse_document_version(&body)))
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
pub async fn policy_context(session_key: &str, source: Option<&str>) -> Option<Value> {
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

    let fetched_at = now_stamp();
    // `coord_client_parts` is the shared accessor `continuation_verdict` and
    // `session_compliance` both use — same credential, same coord base. Its
    // error is the honest "unpaired" reason, which the notice renders verbatim.
    let text = match crate::mcp::continuation_verdict::coord_client_parts() {
        Ok((base, _jwt)) => match fetch_payload(&base).await {
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

    // ── Flag parse (fail-safe) ──────────────────────────────────────────

    #[test]
    fn mode_parses_the_three_values_and_fails_safe_to_off() {
        assert_eq!(Mode::from_flag(Some("on")), Mode::On);
        assert_eq!(Mode::from_flag(Some("observe")), Mode::Observe);
        assert_eq!(Mode::from_flag(Some("off")), Mode::Off);
        // Case + whitespace tolerant, exactly like the continuation flag.
        assert_eq!(Mode::from_flag(Some("  ON  ")), Mode::On);
        assert_eq!(Mode::from_flag(Some("Observe")), Mode::Observe);
        // Everything else is DARK. A typo must never arm an injection.
        assert_eq!(Mode::from_flag(None), Mode::Off);
        assert_eq!(Mode::from_flag(Some("")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("   ")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("true")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("enabled")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("onn")), Mode::Off);
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
            "https://coord.example.com/coord/agent-prompt-documents?kind=policy"
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
            "https://coord.example.com/coord/agent-prompt-documents/policy/session-protocol"
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
