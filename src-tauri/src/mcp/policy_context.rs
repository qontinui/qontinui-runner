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
//! ## The body normally arrives at SPAWN, not here
//!
//! Since plan `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`
//! the protocol body rides the system prompt: spawn seams compose it into one
//! `--append-system-prompt-file` ([`crate::session::spawn_prompt`]) from a
//! per-tenant cache THIS route writes after each successful fetch. The hook
//! batch is the boundary a phantom auto-submitted turn was localized to, and an
//! ~11 KB render was the largest thing crossing it.
//!
//! This route still makes the SAME attributed coord read on every start — that
//! read is the compliance record — and only its render shrinks: a short
//! confirmation plus the index, and ONLY on a `startup`/`compact` whose
//! delivered-SHA marker equals the hash of the body just fetched
//! ([`render_for_session`]). A `resume` (whose conversation re-sends the system
//! prompt recorded when it began), no marker, a stale spawn-time copy, or a cold
//! cache all get the full body, exactly as before. The confirmation says what
//! the runner composed, never that it saw the copy in the model's prompt.
//!
//! ## The degrade is LOUD, not silent (plan
//! `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`, Phase 2)
//!
//! The honesty gate above FAILS OPEN: anything it cannot confirm gets the full
//! body. That is the right direction, and for its first five days in production
//! it was also invisible — a marker that never arrived was indistinguishable,
//! in every log and metric, from a marker deliberately withheld on a `resume`.
//! Measured 2026-09-21 (coord finding `401c6d88`): 26 of 40 live transcripts
//! carried the full body, zero carried the confirmation, and nothing anywhere
//! said so. The feature had regressed to its pre-change behaviour on day one.
//!
//! So every render decision now names itself. [`render_for_session`] returns a
//! [`PolicyRenderDecision`] — a typed [`PolicyRenderReason`], the four raw facts
//! it was derived from, and BOTH SHAs — and [`policy_context`] carries all of it
//! on the one per-injection `tracing::info!` that already existed
//! (`policy-context: injecting fleet policy at SessionStart`), then folds the
//! reason into a process-lifetime counter readable at
//! `GET /sessions/policy-context-stats` ([`render_stats`]). It EXTENDS that
//! event rather than adding a second one: two events per decision make a grep
//! count answer twice. That counter's unit is an INJECTION, not a session —
//! `compact` makes the two diverge, and [`PolicyRenderStats`] says how far.
//!
//! The reason is the FIRST unmet precondition, and the arms are deliberately
//! not collapsed: a reason that lumps two causes together is the same blindness
//! one level down. The raw facts ride the same event, so a session that failed
//! two preconditions is fully readable even though its reason names one.
//!
//! The sharpest instance, and the one Phase 0 paid for: an ABSENT `source` and
//! a `resume` are both "not confirmable", and [`normalize_source`] labels the
//! first of them `startup`. The production defect was the first; the log said
//! the second was indistinguishable from it. [`PolicyRenderReason::SourceAbsent`],
//! [`PolicyRenderReason::SourceUnrecognized`] and
//! [`PolicyRenderReason::SourceNotConfirmable`] are three arms for that reason.
//!
//! Both SHAs are logged (truncated to [`LOGGED_SHA_PREFIX`]) because Phase 0
//! recorded their absence as the thing that made a past injection impossible to
//! re-adjudicate from logs. They are digests of a public policy document.
//!
//! None of this changes what the route SERVES. The honesty property — confirm
//! only on a matching SHA and a confirmable source — is load-bearing and
//! untouched; only the reporting is new.
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
//!
//! ## Whose policy — the SESSION's tenant, not the device default
//!
//! Plan `2026-11-13-runner-policy-injection-uses-the-device-default-tenant-not-the-sessions`.
//! On a device bound to several tenants, the policy that governs a session is
//! its OWNING tenant's, and coord scopes the agent door's rows from the bearer
//! it is shown. So every fetch here presents the credential of the tenant the
//! coord-mcp proxy decides for this session
//! ([`crate::mcp::policy_tenant::route_policy_decision`]) through the
//! tenant-stating seam (`coord_http::coord_get_for`), the in-process cache is
//! keyed on `(coord base, tenant)`, the spawn cache file is named for the
//! tenant whose credential actually fetched the body, and the injected body
//! carries a `[tenant: …]` line directly under its version header — inside the
//! hashed bytes, so a spawn-time copy for one tenant can never confirm a fetch
//! for another. A multi-bound session whose tenant cannot be resolved gets the
//! fail-open notice ([`PolicyRenderReason::TenantUnresolved`]), never the
//! default tenant's documents.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
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
// Mode (pure) — the SHAPE of `continuation_verdict::Mode`, not its default
// ===========================================================================

/// The tri-state injection mode, parsed from [`FLAG_ENV`].
///
/// Deliberately a structural copy of [`crate::mcp::continuation_verdict::Mode`]
/// rather than a shared type: the two flags are switched independently and a
/// shared enum invites a future change to one from silently retuning the other.
/// **The PARSE deliberately DIVERGES** — unknown/empty/absent ⇒ [`Mode::On`]
/// here, where the sibling reads them as `Off`. See [`Mode::from_flag`] for why
/// "do nothing" is the failure rather than the safe state for this flag, and
/// [`FLAG_ENV`] for the convention it breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Feature dark: the route answers an EMPTY body with zero coord traffic.
    /// Reached ONLY by the explicit literal `off` — never by an unset, empty or
    /// misspelled flag, all of which resolve to the [`Mode::On`] default.
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
/// to the caller's tenant from the bearer — never pass a tenant here. Which
/// tenant that is, is decided by WHICH bearer the fetch presents: the session's
/// own tenant's (see the module docs), never a tenant written into the URL.
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
    /// The tenant whose documents these are — the tenant whose credential the
    /// fetch presented ([`crate::mcp::policy_tenant::PolicyTarget::tenant`]).
    /// Rendered as the `[tenant: …]` line of [`render_policy_body`] and named
    /// on the first line of every render. `None` only for a default-slot
    /// credential that names no tenant, which renders as an explicit
    /// `[tenant: unknown]` rather than as nothing.
    pub tenant: Option<uuid::Uuid>,
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

/// The first line of a render that carries policy: [`header`] plus whose
/// documents they are. Naming the tenant on the line a session reads first is
/// what lets it compare the injection against the tenant it acts as
/// (`coord_query_identity` `tenant_id`) without parsing the body. The fail-open
/// notice keeps the bare [`header`]: it carries no tenant's documents.
fn payload_header(source: &str, fetched_at: &str, tenant: Option<uuid::Uuid>) -> String {
    let whose = match tenant {
        Some(t) => {
            format!("These are tenant {t}'s policy documents, read under that tenant's credential.")
        }
        None => "The credential that read them names no tenant, so whose documents these are is \
                 UNKNOWN."
            .to_string(),
    };
    format!("{} {whose}", header(source, fetched_at))
}

/// The `[tenant: …]` line [`render_policy_body`] emits directly under the
/// `===== policy/session-protocol (vN) =====` header (Design §4).
///
/// A SEPARATE line, so the version header every existing reader parses (the
/// `/unattended` held rule, [`render_confirmation`]'s quote) stays
/// byte-identical. Inside the hashed bytes, so the delivered-SHA becomes
/// tenant-specific: a spawn-time body for tenant A can never confirm a fetch
/// for tenant B.
///
/// **The slug always reads `slug unknown`.** The uuid is always rendered; the
/// slug would be rendered only from a coord answer already in hand, and none
/// carries it on this path — the agent-door list and document responses name
/// no tenant, and at SessionStart the session's nonce has received no coord
/// answer yet. A per-session source would also be wrong here even when it
/// exists: these bytes are hashed and cached per TENANT, so one session's
/// knowing the slug and the next one's not would make the same tenant's body
/// hash two ways, and every such pair would read as a stale spawn-time copy.
/// Never invented.
pub(crate) fn tenant_line(tenant: Option<uuid::Uuid>) -> String {
    match tenant {
        Some(t) => format!("[tenant: slug unknown {t}]"),
        None => "[tenant: unknown]".to_string(),
    }
}

/// The spawn-time policy body: an intro paragraph plus the delimited
/// `policy/session-protocol` text. `None` when the fetch carried no body.
///
/// This is the EXACT byte sequence the runner delivers in both channels — the
/// per-tenant cache ([`crate::session::spawn_prompt::write_policy_body_cache`])
/// the spawn seams compose into the system prompt, and the body section of the
/// full hook render ([`render_injection`]). It is therefore also what
/// [`crate::session::spawn_prompt::policy_body_sha`] hashes on both sides of
/// the delivered-SHA check, which is why it is deterministic: no fetch time,
/// no `source`, nothing that differs between two renders of one version.
///
/// The one per-render value it DOES carry is the tenant ([`tenant_line`],
/// directly under the version header), and that is deliberate: two tenants'
/// bodies must never hash alike, or one tenant's spawn-time copy would confirm
/// another tenant's fetch.
pub fn render_policy_body(payload: &PolicyPayload) -> Option<String> {
    let body = payload.protocol_body.as_deref()?;
    let version = version_label(payload.protocol_version);
    let mut out = String::with_capacity(body.len() + 1024);
    out.push_str(&format!(
        "[qontinui-runner] This is the canonical text of `policy/{PROTOCOL_DOC_NAME}` \
         ({version}), pulled from coord `GET /coord/agent-prompt-documents` and delivered \
         by the runner so that Step 0 of the session protocol is satisfied for this session \
         without you fetching it. Treat it as the authority. You do NOT need to re-read \
         `{PROTOCOL_DOC_NAME}`; you DO still need to read the category bodies it names — the \
         policy document index (`kind={KIND}`) lists every one with its current version.\n\n"
    ));
    // The version header line is byte-identical to what it was before the
    // tenant line existed; the tenant line follows it, then the blank line.
    out.push_str(&format!(
        "===== policy/{PROTOCOL_DOC_NAME} ({version}) =====\n"
    ));
    out.push_str(&tenant_line(payload.tenant));
    out.push_str("\n\n");
    out.push_str(body.trim_end());
    out.push('\n');
    Some(out)
}

/// Append the versioned policy index section to `out`.
///
/// Shared by the full render and the short confirmation: the index is small
/// (tens of lines) and is what lets a session tell a stale memory of any
/// document from a current one, so it rides the hook in BOTH shapes.
fn push_index(out: &mut String, payload: &PolicyPayload) {
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
        return;
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
}

/// Render the FULL `additionalContext` for a successful (or partially
/// successful) pull: header, the [`render_policy_body`] section, and the index.
///
/// This is what every session got before the spawn-time carrier existed, and
/// it is still what a session gets whenever the runner cannot PROVE the body
/// already reached its system prompt — see [`render_for_session`].
///
/// Pure: everything time- or network-dependent is a parameter, so the exact
/// injected text is asserted in tests against a fixed payload.
pub fn render_injection(payload: &PolicyPayload, source: &str, fetched_at: &str) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(&payload_header(source, fetched_at, payload.tenant));
    out.push_str("\n\n");

    match render_policy_body(payload) {
        Some(body) => {
            out.push_str(&body);
            out.push('\n');
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

    push_index(&mut out, payload);
    out
}

/// Render the SHORT `additionalContext`: the runner composed this exact body
/// into the session's spawn-time system-prompt file, so only a confirmation and
/// the index cross the hook boundary.
///
/// The wording states only what the runner KNOWS — that it composed a
/// spawn-time system-prompt file whose policy body hashes to `delivered_sha`,
/// and that the hash equals the body coord serves now. It does not claim the
/// runner saw that copy inside the model's system prompt: nothing on this side
/// can observe the request Claude Code actually sends. Names the protocol
/// version and a short prefix of the hash so a session holding the spawn-time
/// copy through many `compact` cycles can match the block it has against what
/// this hook vouched for.
///
/// It quotes the block's version header AND its `[tenant: …]` line, and names
/// the tenant on its first line: the hash it vouches for is "the body coord
/// serves right now" only for the tenant whose credential read it, so the
/// session is told which tenant that is.
pub fn render_confirmation(
    payload: &PolicyPayload,
    delivered_sha: &str,
    source: &str,
    fetched_at: &str,
) -> String {
    let version = version_label(payload.protocol_version);
    let tenant = tenant_line(payload.tenant);
    // The SAME prefix the render-decision log line carries, so an operator
    // comparing a transcript against a log is comparing one string.
    let short_sha: String = delivered_sha.chars().take(LOGGED_SHA_PREFIX).collect();
    let mut out = String::with_capacity(2 * 1024);
    out.push_str(&payload_header(source, fetched_at, payload.tenant));
    out.push_str("\n\n");
    out.push_str(&format!(
        "The body of `policy/{PROTOCOL_DOC_NAME}` is not repeated here. At spawn the runner \
         composed a system-prompt file for this `claude` process (passed via \
         `--append-system-prompt-file`) containing that body ({version}, sha256 {short_sha}…), \
         and that hash equals the body coord serves right now to the tenant named above. If \
         your system prompt holds the block headed \
         `===== policy/{PROTOCOL_DOC_NAME} ({version}) =====` followed by the line \
         `{tenant}`, it is the canonical text and Step 0 of the session protocol is satisfied \
         by it. If you cannot find that block, treat Step 0 as NOT satisfied and fetch it \
         yourself: \
         `coord_get_prompt_document(kind=\"{KIND}\", name=\"{PROTOCOL_DOC_NAME}\")`, or \
         `GET /coord/agent-prompt-documents/{KIND}/{PROTOCOL_DOC_NAME}`. You DO still need to \
         read the category bodies it names.\n\n"
    ));
    push_index(&mut out, payload);
    out
}

/// May a SessionStart with this RAW hook `source` receive the short
/// confirmation at all?
///
/// Only `startup` and `compact`. Claude Code records a conversation's system
/// prompt on its first request and a `--resume` re-sends THAT record
/// (`--system-prompt-snapshot`, on by default) until the next compaction, even
/// when the relaunch passed a different file — so on `resume` a matching marker
/// names the body this process was launched with, not the body the model is
/// being sent. `clear` is excluded too: whether `/clear` resets that snapshot
/// is unverified, and doubt yields the full body. A missing or unrecognized
/// source is not evidence of either, so it is treated like `resume`. Deliberately reads the raw value:
/// [`normalize_source`] maps "absent" to `startup`, which is right for the
/// header label and wrong for this decision.
///
/// **One gate, one implementation.** This DELEGATES to [`classify_source`],
/// which is what the route actually calls. It used to carry its own `matches!`
/// over the same accepted set, and after Phase 2 that copy had no production
/// caller at all — the honesty property was enforced by `classify_source` while
/// every doc in the module still named this function, and the only thing
/// keeping the two agreeing was a hand-picked table in one test. A later change
/// admitting, say, `resume` or a future `restart` to `classify_source`'s `Ok`
/// arm would have killed the property here while this function went on
/// "correctly" returning false and gating nothing. Delegation makes that
/// impossible: there is now one accepted set, and widening it widens both.
///
/// ⚠️ **What delegation does NOT do is make the agreement testable — it makes
/// the question vacuous.** In `classify_source_splits_the_three_non_confirmable_shapes`
/// the `assert!(source_permits_confirmation(raw))` beside each
/// `assert_eq!(classify_source(raw), …)` now calls the same code twice and
/// cannot detect a disagreement, because there is no longer anything that could
/// disagree. Harmless, and worth naming so nobody reads those lines as
/// protection. The real pin on the ACCEPTED SET is the hand-written
/// `expect_confirmed` column in
/// `only_startup_and_compact_may_confirm_a_matching_marker`, which pairs this
/// predicate against [`render_for_session`]'s actual verdict over nine rows —
/// that is the test the round-2 mutation (`classify_source` admitting `resume`)
/// was caught by.
pub fn source_permits_confirmation(raw_source: Option<&str>) -> bool {
    classify_source(raw_source).is_ok()
}

// ===========================================================================
// Why this session got the render it got (plan
// `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`, Phase 2)
// ===========================================================================

/// Why one `SessionStart` got the render it got — one arm per distinguishable
/// cause, and NEVER two causes folded into one arm.
///
/// Six of the nine mean the FULL body crossed the hook boundary
/// ([`Self::served_full_body`]); [`Self::Confirmed`] is the short confirmation —
/// the denominator, without which the full-body count is a number with nothing
/// to divide by; [`Self::PullFailed`] and [`Self::TenantUnresolved`] are
/// neither, because those sessions got the fail-open notice and no policy body
/// at all — and they are two arms, not one, because "coord could not be read"
/// and "whose policy this session needs is unknown" are different findings
/// with different remedies.
///
/// **The three source arms are three different findings, not one.** Collapsing
/// them is the exact blindness this phase exists to remove: Phase 0 of the plan
/// found the production cause to be a MISSING `source` parameter (the
/// materialized hook gated its payload parse on `command -v python`, which does
/// not exist on a Linux box, so it built a URL with no `source=` at all), and
/// [`normalize_source`] maps an absent source to the label `startup` — so 98
/// injections on 2026-09-20 were logged as ordinary startups holding a valid
/// marker, and the cause took a day to find. [`Self::SourceAbsent`] is that
/// finding; [`Self::SourceNotConfirmable`] is a `resume`/`clear`, which is
/// correct behaviour; [`Self::SourceUnrecognized`] is a value from a future
/// Claude release, which is a third thing again. All three are labelled
/// `startup` by `normalize_source` unless a `resume`/`clear` came through.
///
/// The reason is the FIRST unmet precondition, tested in the order the decision
/// is actually made — the source gate, then a body to hash, then a marker to
/// compare, then the comparison. A session can fail several at once; the facts
/// on [`PolicyRenderDecision`] ride the same event, so the ones that did not win
/// the precedence are still readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRenderReason {
    /// The hook sent NO `source` at all (absent, or empty/whitespace). The
    /// measured production cause, and the arm that exists because
    /// [`normalize_source`] renders it indistinguishable from a real `startup`.
    SourceAbsent,
    /// A `source` arrived that this runner does not know (`resume`, `clear`,
    /// `startup` and `compact` are the known set). A future Claude release, a
    /// hook bug, or a typo — and, like [`Self::SourceAbsent`], labelled
    /// `startup` by [`normalize_source`].
    SourceUnrecognized,
    /// A KNOWN source that legitimately cannot confirm: `resume` (the
    /// conversation re-sends the system prompt recorded when it began) or
    /// `clear` (an unverified snapshot reset). Correct behaviour, not a defect —
    /// but counted, or the full-body total silently absorbs a population that
    /// was never supposed to be confirmable.
    SourceNotConfirmable,
    /// Coord's payload carried no `session-protocol` body, so there was nothing
    /// to hash and nothing to compare a marker against. The render is the
    /// PARTIAL one, not the full body — see [`render_injection`].
    BodyUnavailable,
    /// A confirmable source and a body, but the session forwarded no
    /// delivered-SHA marker: it was not given the file carrier (cold cache at
    /// spawn, a write failure, a wrapper fall-back arm, a shim strip).
    MarkerAbsent,
    /// A marker arrived and hashes to a DIFFERENT body than the one coord
    /// serves now — the spawn-time copy is stale.
    MarkerMismatched,
    /// The short confirmation was sent: confirmable source, body present,
    /// marker present and equal to the current body's SHA.
    Confirmed,
    /// The coord pull failed outright, so the session got the fail-open notice
    /// ([`render_failure_notice`]) rather than either render. Counted so that
    /// [`PolicyRenderStats::injections`] equals every `SessionStart` this
    /// route answered — a denominator with a hole in it is not a denominator.
    PullFailed,
    /// The session's tenant could not be resolved on a device bound to several
    /// tenants (no nonce and no registry answer, a proxy refusal, a repo still
    /// unsettled after the proxy's budget), or it resolved to a tenant this
    /// runner holds no usable credential for — so NOTHING was fetched and the
    /// session got the fail-open notice naming the cause, rather than the
    /// default tenant's documents (plan
    /// `2026-11-13-runner-policy-injection-uses-the-device-default-tenant-not-the-sessions`
    /// Design §5). Never folded into [`Self::PullFailed`]: that arm means coord
    /// was asked and did not answer; this one means the runner declined to ask
    /// under a credential that might be the wrong tenant's.
    TenantUnresolved,
}

impl PolicyRenderReason {
    /// How many arms there are — the counter array's width. Pinned to
    /// [`Self::ALL`] by `the_arm_count_is_pinned_to_the_arm_list`, so the two
    /// cannot drift.
    pub const COUNT: usize = 9;

    /// Every arm, in the precedence order documented on the enum. The single
    /// source of truth for the counter's indexing and for exhaustive tests.
    pub const ALL: [PolicyRenderReason; PolicyRenderReason::COUNT] = [
        PolicyRenderReason::SourceAbsent,
        PolicyRenderReason::SourceUnrecognized,
        PolicyRenderReason::SourceNotConfirmable,
        PolicyRenderReason::BodyUnavailable,
        PolicyRenderReason::MarkerAbsent,
        PolicyRenderReason::MarkerMismatched,
        PolicyRenderReason::Confirmed,
        PolicyRenderReason::PullFailed,
        PolicyRenderReason::TenantUnresolved,
    ];

    /// The stable snake_case label this reason is logged and counted under.
    /// Part of the grep contract — changing one of these breaks every saved
    /// query, so change it only deliberately.
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyRenderReason::SourceAbsent => "source_absent",
            PolicyRenderReason::SourceUnrecognized => "source_unrecognized",
            PolicyRenderReason::SourceNotConfirmable => "source_not_confirmable",
            PolicyRenderReason::BodyUnavailable => "body_unavailable",
            PolicyRenderReason::MarkerAbsent => "marker_absent",
            PolicyRenderReason::MarkerMismatched => "marker_mismatched",
            PolicyRenderReason::Confirmed => "confirmed",
            PolicyRenderReason::PullFailed => "pull_failed",
            PolicyRenderReason::TenantUnresolved => "tenant_unresolved",
        }
    }

    /// Did the FULL policy body cross the SessionStart hook boundary?
    ///
    /// False for [`Self::Confirmed`] (the short confirmation) and for
    /// [`Self::PullFailed`] / [`Self::TenantUnresolved`] (the fail-open notice,
    /// which carries no body). [`Self::BodyUnavailable`] is true: the render is
    /// the partial injection, which still carries everything the payload had.
    pub fn served_full_body(self) -> bool {
        !matches!(
            self,
            PolicyRenderReason::Confirmed
                | PolicyRenderReason::PullFailed
                | PolicyRenderReason::TenantUnresolved
        )
    }

    /// Index into the counter array, DERIVED from [`Self::ALL`] rather than
    /// hand-written beside it.
    ///
    /// The hand-written version claimed to be "pinned to `ALL`'s order by
    /// construction" and was not — nothing derived it, and the compiler does
    /// not force `ALL` to grow when an arm is added: `ALL: [Self; COUNT]` stays
    /// well-typed as a `COUNT`-element literal, so one more arm compiles, passes
    /// every test that iterates `ALL` (it never sees the new arm), and then
    /// indexes `COUNT` into a `COUNT`-element array on the first session that
    /// hits it. A panic there
    /// is a panic inside a FAIL-OPEN axum handler — the connection drops, the
    /// hook gets no response, and the session lands in exactly the no-policy
    /// state this module's header says it exists to end. Deriving the position
    /// makes the doc claim true; [`record_render`] not indexing blindly makes
    /// the remaining out-of-range case survivable.
    ///
    /// `None` means this arm is absent from `ALL` — unreachable while the two
    /// agree, which `the_every_arm_is_listed_and_slots_are_in_range` asserts.
    fn slot(self) -> Option<usize> {
        Self::ALL.iter().position(|r| *r == self)
    }
}

/// Classify the RAW hook `source` on its own — the first gate of
/// [`render_for_session`], and the one whose three outcomes
/// [`normalize_source`] cannot tell apart.
///
/// `Ok(())` means the source permits a confirmation (`startup`/`compact`);
/// `Err(reason)` names which of the three non-confirmable shapes it was.
///
/// **This is the ONE implementation of the honesty gate** —
/// [`source_permits_confirmation`] delegates here, so the accepted set cannot
/// be widened in one place and left narrow in the other. Deliberately reads the
/// RAW value: [`normalize_source`] maps an absent source to `startup`, which is
/// right for the header label and wrong for this decision.
pub fn classify_source(raw_source: Option<&str>) -> Result<(), PolicyRenderReason> {
    match raw_source.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("startup" | "compact") => Ok(()),
        Some("resume" | "clear") => Err(PolicyRenderReason::SourceNotConfirmable),
        None | Some("") => Err(PolicyRenderReason::SourceAbsent),
        Some(_) => Err(PolicyRenderReason::SourceUnrecognized),
    }
}

/// How many leading hex characters of a SHA-256 reach the log.
///
/// 12, the same prefix [`render_confirmation`] shows a session, so an operator
/// reading a transcript and an operator reading the log are comparing the same
/// string. These are content digests of a PUBLIC policy document, not secrets —
/// the truncation is for line width, not for hygiene.
pub const LOGGED_SHA_PREFIX: usize = 12;

/// Truncate a SHA for logging, or name its absence. Never an empty field: an
/// empty string in a log line reads as "the field is broken", not "there was
/// no marker".
fn short_sha(sha: Option<&str>) -> String {
    // The empty arm is the whole point of the contract: `Some("")` would
    // otherwise render as a blank field, which reads as "this field is broken"
    // rather than "there was no marker". A doc that states a property the
    // function does not enforce is worth less than no doc.
    match sha.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.chars().take(LOGGED_SHA_PREFIX).collect(),
        None => ABSENT_FIELD.to_string(),
    }
}

/// What an absent SHA renders as in the decision event. Never an empty field —
/// see [`short_sha`].
const ABSENT_FIELD: &str = "<none>";

/// What an absent hook `source` renders as in the decision event. A separate
/// sentinel from [`ABSENT_FIELD`] on purpose: "no marker" and "no source" are
/// different findings, and a reader scanning the line should not have to check
/// which field a `<none>` belongs to.
const ABSENT_SOURCE: &str = "<absent>";

/// The raw hook `source` as it reaches the log: what the hook actually sent, or
/// [`ABSENT_SOURCE`] when it sent nothing or nothing but whitespace.
///
/// Distinct from [`normalize_source`]'s LABEL, which maps an absent source to
/// `startup` and so cannot be used to tell the two apart — the exact ambiguity
/// [`PolicyRenderReason::SourceAbsent`] exists to end. Empty is folded into
/// absent for the same reason [`short_sha`] folds it: a blank field reads as a
/// broken field rather than as an absent value.
///
/// **Deliberately logs the value UNTRIMMED**, where `short_sha` trims. The
/// decision is made on the trimmed, lowercased value ([`classify_source`]), so
/// the trimmed form is already implied by `reason`; what this field adds is
/// what the hook literally put on the wire, and a `" startup "` whose padding
/// were silently removed here would hide a hook bug rather than expose one.
/// Only the emptiness TEST trims — a whitespace-only source is absent, not a
/// value worth echoing.
fn source_for_log(raw_source: Option<&str>) -> &str {
    match raw_source {
        Some(raw) if !raw.trim().is_empty() => raw,
        _ => ABSENT_SOURCE,
    }
}

/// One render decision: the typed [`PolicyRenderReason`], the raw facts it was
/// derived from, and both SHAs.
///
/// The facts are carried separately on purpose. The reason names the first
/// unmet precondition, which is what a counter can be keyed on; the facts say
/// what ELSE was wrong with the same session, which is what a reason alone
/// cannot. A `resume` that also carried no marker is one event here and two
/// readable facts, rather than a `source_not_confirmable` that quietly hides a
/// second defect.
///
/// **Both SHAs are kept in full.** Phase 0 recorded as UNKNOWN that "neither
/// `delivered_sha` nor the computed `policy_body_sha` is logged, so no past
/// injection can be re-adjudicated from logs". They are logged truncated to
/// [`LOGGED_SHA_PREFIX`]; a programmatic consumer gets the whole value here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRenderDecision {
    /// The first unmet precondition — see [`PolicyRenderReason`].
    pub reason: PolicyRenderReason,
    /// Did the RAW hook source permit a confirmation at all?
    pub source_confirmable: bool,
    /// Did the session forward a well-formed delivered-SHA marker?
    pub marker_present: bool,
    /// Did coord's payload carry a `session-protocol` body?
    pub body_present: bool,
    /// Marker present, body present, and the two hashes equal.
    pub marker_matches: bool,
    /// The marker the session forwarded, in full.
    pub delivered_sha: Option<String>,
    /// SHA-256 of the body coord served on THIS call, in full.
    pub body_sha: Option<String>,
}

impl PolicyRenderDecision {
    /// The decision for a session whose coord pull failed outright. The source
    /// and marker facts are still known and still worth recording; nothing
    /// about the body is.
    pub fn pull_failed(raw_source: Option<&str>, delivered_sha: Option<&str>) -> Self {
        Self {
            reason: PolicyRenderReason::PullFailed,
            source_confirmable: classify_source(raw_source).is_ok(),
            marker_present: delivered_sha.is_some(),
            body_present: false,
            marker_matches: false,
            delivered_sha: delivered_sha.map(str::to_owned),
            body_sha: None,
        }
    }

    /// The decision for a session whose tenant could not be resolved, so
    /// nothing was fetched ([`PolicyRenderReason::TenantUnresolved`]). Same
    /// known facts as [`Self::pull_failed`]; nothing about a body is known
    /// because none was asked for.
    pub fn tenant_unresolved(raw_source: Option<&str>, delivered_sha: Option<&str>) -> Self {
        Self {
            reason: PolicyRenderReason::TenantUnresolved,
            ..Self::pull_failed(raw_source, delivered_sha)
        }
    }

    /// Was the short confirmation sent? The predicate the route used to return
    /// as a bare `bool`.
    pub fn confirmed(&self) -> bool {
        self.reason == PolicyRenderReason::Confirmed
    }

    /// Did the full body cross the hook boundary? See
    /// [`PolicyRenderReason::served_full_body`].
    pub fn served_full_body(&self) -> bool {
        self.reason.served_full_body()
    }

    /// The marker as it reaches the log — [`LOGGED_SHA_PREFIX`] chars, or
    /// `<none>`.
    pub fn delivered_sha_short(&self) -> String {
        short_sha(self.delivered_sha.as_deref())
    }

    /// The served body's SHA as it reaches the log — [`LOGGED_SHA_PREFIX`]
    /// chars, or `<none>`.
    pub fn body_sha_short(&self) -> String {
        short_sha(self.body_sha.as_deref())
    }
}

/// Process-lifetime tally, one slot per [`PolicyRenderReason`].
///
/// Atomics rather than a lock, and a plain array rather than a metrics crate:
/// `knowledge_acquisition::stats::ProviderCounters` is the shape this runner
/// already counts things in, and the runner pulls in no metrics registry at
/// all. Adding one for nine counters would be a second mechanism for the thing
/// the first one already does.
fn render_counts() -> &'static [AtomicU64; PolicyRenderReason::COUNT] {
    static COUNTS: OnceLock<[AtomicU64; PolicyRenderReason::COUNT]> = OnceLock::new();
    COUNTS.get_or_init(|| std::array::from_fn(|_| AtomicU64::new(0)))
}

/// Does a render made in this [`Mode`] belong in the tally?
///
/// ONLY [`Mode::On`]. `Off` never reaches a decision at all, and `Observe`
/// renders the would-be payload and then injects NOTHING — counting it would
/// make `full_body_total` a count of renders that were never delivered, so a
/// runner soaked in `observe` would report thousands of full-body injections
/// having made none.
///
/// A function rather than a sentence in a comment, because the rule is worth
/// pinning: moving `record_render` above [`policy_context`]'s `observe`
/// early-return compiles, passes every other test, and silently inverts the
/// counter's meaning.
///
/// ⚠️ **Be precise about what this buys.** At its one call site the guard is
/// unreachable-false — `Off` and `Observe` have both already returned, so
/// `mode` there can only be `On` — and moving that call above the early return
/// would still pass CI, because no TEST calls [`policy_context`]. (Production
/// does: `mcp::sessions` calls it on every `SessionStart`. It is `pub async`
/// and perfectly callable; what is hard is making it EXERCISABLE under test,
/// since it reads env and talks to coord.) So position still decides behaviour
/// at that site; what the predicate adds is that the rule is now testable
/// (`the_tally_counts_only_the_mode_that_actually_injects`) and that a future
/// caller inherits it by asking rather than by being placed correctly.
pub fn should_count(mode: Mode) -> bool {
    mode == Mode::On
}

/// Fold one decision into the process-lifetime tally. Called exactly once per
/// injection, gated on [`should_count`].
pub fn record_render(reason: PolicyRenderReason) {
    // Belt and braces with the derived `slot()`: this runs inside a FAIL-OPEN
    // route, so an out-of-range index would turn a missing counter slot into a
    // panicking hook and a session with no policy at all. Losing a count is a
    // reporting gap; panicking here recreates the incident the module exists to
    // prevent. `debug_assert` still makes the drift loud in a test build.
    debug_assert!(
        reason.slot().is_some(),
        "{} is missing from PolicyRenderReason::ALL",
        reason.as_str()
    );
    if let Some(slot) = reason.slot().and_then(|i| render_counts().get(i)) {
        slot.fetch_add(1, Ordering::Relaxed);
    }
}

/// A readable snapshot of the tally — the answer to "how many INJECTIONS got
/// the full body since this runner started, and why", with no log grep.
///
/// ⚠️ **The unit is an INJECTION, not a session, and `compact` is what makes
/// the two diverge.** This route fires on every `SessionStart` hook, and
/// `compact` is a CONFIRMABLE source, so one long-lived session contributes one
/// count per compaction. A session that starts with no marker and then compacts
/// four times with a matching one tallies `marker_absent: 1, confirmed: 4,
/// injections: 5` — and an operator reading that as "80% confirmed, the seam is
/// healthy" would have it exactly backwards: the true population is ONE session
/// that got no confirmation at the only moment that mattered, its start. The
/// `confirmed` share is inflated by precisely the longest-running sessions.
///
/// So read a per-SOURCE split before drawing a per-session conclusion, and
/// treat `source_absent` / `marker_absent` as counts of STARTS gone wrong
/// rather than as a minority of a population. A metric built to end a
/// measurement error must not ship with one; this doc is the guard, because
/// nothing in the numbers themselves says which unit they are in.
///
/// Served at `GET /sessions/policy-context-stats`. Counts are since process
/// start: the runner writes nothing durable here, because a per-session row
/// belongs in `coord.session_policy_reads` (which already gets one) and a
/// second durable store for the same event is a second thing to reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PolicyRenderStats {
    /// The hook sent no `source` at all — logged as `startup` by
    /// [`normalize_source`], so invisible before this counter existed.
    pub source_absent: u64,
    /// A `source` this runner does not know.
    pub source_unrecognized: u64,
    /// `resume`/`clear` — full body BY DESIGN.
    pub source_not_confirmable: u64,
    /// Coord served no protocol body — the partial render.
    pub body_unavailable: u64,
    /// No delivered-SHA marker reached the route.
    pub marker_absent: u64,
    /// A marker arrived naming a different body.
    pub marker_mismatched: u64,
    /// The short confirmation — the denominator.
    pub confirmed: u64,
    /// The coord pull failed; the fail-open notice went out instead.
    pub pull_failed: u64,
    /// The session's tenant was unresolvable (or had no usable credential) on
    /// a multi-bound device; nothing was fetched and the fail-open notice went
    /// out instead of the default tenant's documents.
    pub tenant_unresolved: u64,
    /// The six full-body arms summed.
    pub full_body_total: u64,
    /// Every arm summed — the number of INJECTIONS this route made, which is
    /// the number of `SessionStart` hooks it answered in [`Mode::On`], NOT the
    /// number of sessions: one session contributes one count per compaction.
    /// See the type's own doc for why that distinction inverts a naive ratio.
    pub injections: u64,
}

/// Snapshot [`record_render`]'s tally. Relaxed loads: the slots are counters,
/// not a consistent cut, and a reader that sees one slot a beat ahead of
/// another is reading a live runner, which is the honest thing to show.
pub fn render_stats() -> PolicyRenderStats {
    let c = render_counts();
    // Same non-panicking read as `record_render`'s write: an arm absent from
    // `ALL` reads as 0 rather than taking the stats route down with it.
    let at = |r: PolicyRenderReason| {
        r.slot()
            .and_then(|i| c.get(i))
            .map_or(0, |s| s.load(Ordering::Relaxed))
    };
    let source_absent = at(PolicyRenderReason::SourceAbsent);
    let source_unrecognized = at(PolicyRenderReason::SourceUnrecognized);
    let source_not_confirmable = at(PolicyRenderReason::SourceNotConfirmable);
    let body_unavailable = at(PolicyRenderReason::BodyUnavailable);
    let marker_absent = at(PolicyRenderReason::MarkerAbsent);
    let marker_mismatched = at(PolicyRenderReason::MarkerMismatched);
    let confirmed = at(PolicyRenderReason::Confirmed);
    let pull_failed = at(PolicyRenderReason::PullFailed);
    let tenant_unresolved = at(PolicyRenderReason::TenantUnresolved);
    // DERIVED from `ALL`, not written out as a six-term sum: a seventh full-body
    // arm added to the enum, to `ALL` and to the struct would compile against a
    // hand-written sum and silently under-report — the same class the derived
    // `slot()` closed one level down. `served_full_body` is the single source
    // of truth for which arms belong here.
    //
    // NOTE for whoever adds the next arm: `served_full_body` is a NEGATIVE list
    // (everything except `Confirmed`, `PullFailed` and `TenantUnresolved` — the
    // last added exactly this way, because it carries no body), so a new arm lands
    // INSIDE this total by default. That is the conservative direction for a
    // metric whose job is catching a leaking seam — an over-count is visible,
    // an under-count is the silence this whole phase exists to end — but it is
    // a default, not a decision: if the new arm carries no body, add it to
    // `served_full_body`'s exclusion list as well as to `ALL`.
    let full_body_total: u64 = PolicyRenderReason::ALL
        .iter()
        .filter(|r| r.served_full_body())
        .map(|r| at(*r))
        .sum();
    PolicyRenderStats {
        source_absent,
        source_unrecognized,
        source_not_confirmable,
        body_unavailable,
        marker_absent,
        marker_mismatched,
        confirmed,
        pull_failed,
        tenant_unresolved,
        full_body_total,
        // Likewise derived: every arm, so a new one cannot be left out of the
        // denominator by forgetting to add a term here.
        injections: PolicyRenderReason::ALL.iter().map(|r| at(*r)).sum(),
    }
}

/// Choose the render for one session — the honesty decision, pure.
///
/// The short confirmation is sent ONLY when the RAW hook source permits it
/// (`startup` or `compact`)
/// ([`source_permits_confirmation`]) AND the session's delivered-SHA marker
/// equals [`crate::session::spawn_prompt::policy_body_sha`] of the body this
/// call just fetched. Every other case gets the full render:
///
/// - **`resume`, `clear`, an absent source, or one this runner does not know** —
///   a resumed conversation re-sends the system prompt recorded when it began,
///   so a matching marker proves nothing about what the model holds; a `/clear`
///   snapshot reset is unverified; and a source that never arrived is no
///   evidence of anything. These are THREE reasons, not one — see
///   [`PolicyRenderReason`];
/// - **no marker** — the session was not given the file carrier (cold cache at
///   spawn, a write failure, a wrapper fall-back, a seam that has none), and the
///   presence of a cache file NOW says nothing about what it received THEN;
/// - **a different SHA** — the spawn-time copy is stale (coord versioned the
///   document since the pane or process started);
/// - **no body fetched** — nothing to compare, and the partial render says so.
///
/// `raw_source` is the hook's value as received; the header label is its
/// [`normalize_source`]. Returns the text and the [`PolicyRenderDecision`] —
/// which render, why, the facts that decided it, and both SHAs.
///
/// **The decision is computed for every session, not only the confirmable
/// ones.** The body is rendered and hashed even on a `resume`, which costs one
/// ~8 KB render and one SHA-256 per SessionStart that would previously have
/// skipped both. That is deliberate: the alternative leaves `body_present`,
/// `marker_matches` and `body_sha` UNKNOWN on the commonest arm, and an
/// observability change whose facts go dark exactly where the population is
/// largest is not worth making. The cost sits next to a coord HTTP round trip
/// on the same path.
pub fn render_for_session(
    payload: &PolicyPayload,
    delivered_sha: Option<&str>,
    raw_source: Option<&str>,
    fetched_at: &str,
) -> (String, PolicyRenderDecision) {
    let source = normalize_source(raw_source);
    let source_verdict = classify_source(raw_source);
    let body = render_policy_body(payload);
    let current = body
        .as_deref()
        .map(crate::session::spawn_prompt::policy_body_sha);

    let marker_present = delivered_sha.is_some();
    let body_present = current.is_some();
    let marker_matches = matches!(
        (delivered_sha, current.as_deref()),
        (Some(marker), Some(now)) if marker == now
    );

    // First unmet precondition wins, tested in the order the decision is
    // actually made: the source gate, then a body to hash, then a marker to
    // compare, then the comparison itself.
    let reason = match source_verdict {
        Err(why) => why,
        Ok(()) if !body_present => PolicyRenderReason::BodyUnavailable,
        Ok(()) if !marker_present => PolicyRenderReason::MarkerAbsent,
        Ok(()) if !marker_matches => PolicyRenderReason::MarkerMismatched,
        Ok(()) => PolicyRenderReason::Confirmed,
    };

    let decision = PolicyRenderDecision {
        reason,
        source_confirmable: source_verdict.is_ok(),
        marker_present,
        body_present,
        marker_matches,
        delivered_sha: delivered_sha.map(str::to_owned),
        body_sha: current.clone(),
    };

    let text = match (reason, current.as_deref()) {
        (PolicyRenderReason::Confirmed, Some(now)) => {
            render_confirmation(payload, now, source, fetched_at)
        }
        _ => render_injection(payload, source, fetched_at),
    };
    (text, decision)
}

/// The request header the bundled policy hook forwards the delivered-SHA marker
/// in. A header, not a query parameter: the runner's HTTP `TraceLayer` logs
/// request URIs, and a per-session body hash has no business in the trace log.
pub const DELIVERED_SHA_HEADER: &str = "x-qontinui-policy-delivered-sha";

/// Read and [`parse_delivered_sha`] the marker from [`DELIVERED_SHA_HEADER`].
/// A missing, non-UTF-8 or malformed header is no marker.
pub fn delivered_sha_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    parse_delivered_sha(
        headers
            .get(DELIVERED_SHA_HEADER)
            .and_then(|v| v.to_str().ok()),
    )
}

/// Parse the hook-forwarded delivered-SHA marker. Strict: exactly 64 hex
/// characters, normalized to lower case. Anything else is no marker — the
/// route then renders in full, which is the safe direction.
pub fn parse_delivered_sha(raw: Option<&str>) -> Option<String> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    (raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_lowercase())
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

/// What the cache holds for one `(coord base, tenant)`.
#[derive(Default)]
struct CacheSnapshot {
    /// The payload, when it is still within [`CACHE_TTL`].
    fresh: Option<PolicyPayload>,
    /// The payload at any age — what gets served when coord is unreachable.
    any: Option<PolicyPayload>,
    list_etag: Option<String>,
    doc_etag: Option<String>,
}

/// One cache slot: the resolved coord base URL and the tenant whose credential
/// fetched the payload ([`crate::mcp::policy_tenant::PolicyTarget::tenant`]).
type CacheKey = (String, Option<uuid::Uuid>);

/// Keyed on `(coord base, tenant)`.
///
/// It used to be keyed on the coord base alone, on the reasoning that tenancy
/// is lifted server-side from the bearer and re-pointing coord was "the only
/// way its tenant changes without a restart". That is false on a device bound
/// to several tenants: two sessions on one coord base can need two tenants'
/// policies at the same moment, and a base-only key served whichever tenant
/// fetched last to both (plan
/// `2026-11-13-runner-policy-injection-uses-the-device-default-tenant-not-the-sessions`
/// Design §3). The tenant is the one the fetch PRESENTS a credential for, so
/// an entry can never be read back for a session of another tenant — and the
/// `ETag`s replayed from it are that tenant's own validators. A cache cold per
/// tenant costs one full fetch per tenant per process; the conditional fetch
/// bounds the steady state.
static CACHE: OnceLock<Mutex<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<CacheKey, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_snapshot(base: &str, tenant: Option<uuid::Uuid>) -> CacheSnapshot {
    let Ok(g) = cache().lock() else {
        return CacheSnapshot::default();
    };
    let Some(entry) = g.get(&(base.to_string(), tenant)) else {
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
    tenant: Option<uuid::Uuid>,
    list_etag: Option<String>,
    doc_etag: Option<String>,
    payload: PolicyPayload,
) {
    if let Ok(mut g) = cache().lock() {
        g.insert(
            (base.to_string(), tenant),
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

/// Fetch the injection payload for `base`, under `target`'s credential.
///
/// Returns the payload and the tenant the PROTOCOL fetch's bearer claimed —
/// the claim [`persist_policy_body_cache`] names the spawn cache file from
/// (Design §2a). `None` when no request carried one this call (a fresh cache
/// hit, or a request that could not be built), which writes no file.
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
    target: crate::mcp::policy_tenant::PolicyTarget,
    attribution: Option<uuid::Uuid>,
) -> Result<(PolicyPayload, Option<uuid::Uuid>), String> {
    let snap = cache_snapshot(base, target.tenant);
    if attribution.is_none() {
        if let Some(payload) = snap.fresh.clone() {
            debug!(
                "policy-context: served from cache — no attributable session, so the \
                 recording round-trip would buy nothing"
            );
            return Ok((payload, None));
        }
    }

    let client = crate::mcp::continuation_verdict::http_client()?;

    // Both reads go out, CONCURRENTLY: the route now awaits the session's
    // tenant (bounded by `repo_tenant::CWD_TENANT_BUDGET`) before fetching, and
    // the hook gives the whole answer `--max-time 15`, so two sequential 4 s
    // reads after a slow resolution could outlive the hook. `coord_get_for`
    // attaches the TARGET tenant's bearer itself (re-read per call — the JWT
    // has a short TTL) and drives the data-plane auth-coverage metric; the
    // caller's `coord_client_parts` token is used only to detect "unpaired"
    // and is not threaded down here: one credential source, read as late as
    // possible.
    let ((list, _), (protocol, presented)) = tokio::join!(
        fetch_list(
            &client,
            base,
            target,
            snap.list_etag.as_deref(),
            attribution
        ),
        fetch_protocol(&client, base, target, snap.doc_etag.as_deref(), attribution),
    );

    // ---- Compose. Each half falls back to its cached counterpart independently,
    // ---- so one failing door never discards the other's fresh answer. The
    // ---- cached counterpart is THIS tenant's: the snapshot is keyed on it.
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
        tenant: target.tenant,
    };
    // Do NOT cache a partial: a transient blip on the body fetch must not pin an
    // index-only payload for a full TTL when the real body is one retry away.
    // Same reasoning `continuation_verdict` applies to its fallback.
    if payload.protocol_body.is_some() {
        cache_store(base, target.tenant, list_etag, doc_etag, payload.clone());
    }
    if payload.protocol_body.is_none() && payload.index.is_empty() {
        return Err(
            "coord returned neither the policy index nor the session-protocol body".to_string(),
        );
    }
    Ok((payload, presented))
}

/// The tenant a built request's bearer claims — the credential it will
/// ACTUALLY present, read off its `Authorization` header rather than
/// re-resolved, so the check below and the cache-file name are about the bytes
/// on the wire. `None` for no header, a non-bearer one, or a token whose
/// `tenant_id` claim is absent or not a uuid. The token itself goes no further.
fn presented_claim(headers: &reqwest::header::HeaderMap) -> Option<uuid::Uuid> {
    let value = headers.get(reqwest::header::AUTHORIZATION)?.to_str().ok()?;
    tenant_of_jwt(value.trim().strip_prefix("Bearer ")?)
}

/// Refuse to send a policy read whose credential is not the target tenant's.
///
/// `coord_get_for(Owned(t))` sends UNAUTHENTICATED when `t` has no usable slot
/// (and a slot can lapse between the tenant decision and the fetch), and an
/// answer to such a read could be another tenant's — which would then be
/// rendered, cached and filed as `t`'s. The tenant decision already refuses the
/// no-credential case up front ([`crate::mcp::policy_tenant::policy_tenant_decision`]);
/// this is the same rule at the last moment, on the request that would go out.
/// A target naming no tenant (a default slot whose credential claims none)
/// asserts nothing, so it checks nothing.
fn check_presented(
    target: Option<uuid::Uuid>,
    presented: Option<uuid::Uuid>,
) -> Result<(), String> {
    match (target, presented) {
        (Some(t), Some(p)) if t == p => Ok(()),
        (Some(t), Some(p)) => Err(format!(
            "the credential this runner would present for tenant {t} claims tenant {p}, so the \
             policy read was not sent"
        )),
        (Some(t), None) => Err(format!(
            "this runner holds no usable credential claiming tenant {t}, so the policy read was \
             not sent"
        )),
        (None, _) => Ok(()),
    }
}

/// Build one tenant-stating policy GET — `target`'s credential through
/// `coord_http::coord_get_for`, the attribution header, the validator — and
/// [`check_presented`] it BEFORE it can be sent. Returns the request and the
/// tenant its bearer claims.
fn build_policy_get(
    client: &reqwest::Client,
    url: String,
    target: crate::mcp::policy_tenant::PolicyTarget,
    etag: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> Result<(reqwest::Request, Option<uuid::Uuid>), String> {
    let mut req = attach_attribution(
        crate::coord_http::coord_get_for(client, url, target.scope),
        attribution,
    );
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let request = req
        .build()
        .map_err(|e| format!("could not build the policy request: {e}"))?;
    let presented = presented_claim(request.headers());
    check_presented(target.tenant, presented)?;
    Ok((request, presented))
}

/// Conditionally fetch the `kind=policy` index under `target`'s credential.
/// Returns the outcome and the tenant the presented bearer claimed.
async fn fetch_list(
    client: &reqwest::Client,
    base: &str,
    target: crate::mcp::policy_tenant::PolicyTarget,
    etag: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> (ListOutcome, Option<uuid::Uuid>) {
    let (request, presented) =
        match build_policy_get(client, list_url(base), target, etag, attribution) {
            Ok(built) => built,
            Err(reason) => return (ListOutcome::Failed(reason), None),
        };
    let resp = match client.execute(request).await {
        Ok(r) => r,
        Err(e) => {
            return (
                ListOutcome::Failed(format!("coord unreachable: {e}")),
                presented,
            )
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return (ListOutcome::NotModified, presented);
    }
    if !status.is_success() {
        return (
            ListOutcome::Failed(format!(
                "coord answered HTTP {} to the policy-document list",
                status.as_u16()
            )),
            presented,
        );
    }
    let etag = response_etag(&resp);
    let outcome = match resp.json::<Value>().await {
        Ok(body) => ListOutcome::Fresh {
            index: parse_index(&body),
            etag,
        },
        Err(e) => ListOutcome::Failed(format!("coord policy-document list was undecodable: {e}")),
    };
    (outcome, presented)
}

/// Conditionally fetch `policy/session-protocol`'s body + version under
/// `target`'s credential. Returns the outcome and the tenant the presented
/// bearer claimed.
///
/// The `If-None-Match` here is what turns the per-session-start recording read
/// into a near-empty one: coord's agent single-document door answers 304 with no
/// body and still records the read at the version it would have served.
async fn fetch_protocol(
    client: &reqwest::Client,
    base: &str,
    target: crate::mcp::policy_tenant::PolicyTarget,
    etag: Option<&str>,
    attribution: Option<uuid::Uuid>,
) -> (ProtocolOutcome, Option<uuid::Uuid>) {
    let (request, presented) = match build_policy_get(
        client,
        document_url(base, PROTOCOL_DOC_NAME),
        target,
        etag,
        attribution,
    ) {
        Ok(built) => built,
        Err(reason) => return (ProtocolOutcome::Failed(reason), None),
    };
    let resp = match client.execute(request).await {
        Ok(r) => r,
        Err(e) => return (ProtocolOutcome::Failed(format!("request: {e}")), presented),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return (ProtocolOutcome::NotModified, presented);
    }
    if !status.is_success() {
        return (
            ProtocolOutcome::Failed(format!("coord answered HTTP {}", status.as_u16())),
            presented,
        );
    }
    let etag = response_etag(&resp);
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                ProtocolOutcome::Failed(format!("undecodable: {e}")),
                presented,
            )
        }
    };
    // Reuse the settled row-envelope reader — it unwraps `document`, trims, and
    // treats an empty body as absent.
    let outcome = match crate::mcp::continuation_verdict::rules_from_doc_body(&body) {
        Some(text) => ProtocolOutcome::Fresh {
            body: text,
            version: parse_document_version(&body),
            etag,
        },
        None => {
            ProtocolOutcome::Failed("coord returned an empty session-protocol body".to_string())
        }
    };
    (outcome, presented)
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
///
/// `route_id` is the route's `{id}` (the runner TERMINAL id the hook sends,
/// falling back to the Claude session id) and `session_key` is what
/// `mcp::sessions::resolve_session_key` made of it. Together they name the
/// session whose tenant the coord-mcp proxy decides
/// ([`crate::mcp::policy_tenant::route_policy_decision`]) — the tenant whose
/// credential every fetch here presents.
///
/// `delivered_sha` is the session's spawn-time marker
/// ([`crate::session::spawn_prompt::POLICY_DELIVERED_SHA_ENV`], already parsed
/// by [`parse_delivered_sha`]). It changes only the RENDER
/// ([`render_for_session`]) — the attributed coord read happens identically
/// either way, so `coord.session_policy_reads` keeps its
/// `session_start_injection` row for every session whose tenant resolves.
pub async fn policy_context(
    route_id: &str,
    session_key: &str,
    source: Option<&str>,
    attribution: Option<uuid::Uuid>,
    delivered_sha: Option<&str>,
) -> Option<Value> {
    let mode = Mode::from_env();
    let raw_source = source;
    let source = normalize_source(raw_source);

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
    // `session_compliance` both use — the coord base, and the "is this runner
    // paired at all" gate. Its error is the honest "unpaired" reason, which the
    // notice renders verbatim. Its token is NOT the fetch credential any more:
    // that is the session's tenant's, decided next.
    let (text, decision, tenant) = match crate::mcp::continuation_verdict::coord_client_parts() {
        Ok((base, _default_jwt)) => {
            // Whose policy: the tenant the coord-mcp proxy decides for THIS
            // session, awaited (SessionStart is its first call). An UNKNOWN on a
            // multi-bound device is no target at all, so nothing is fetched.
            let tenant_decision =
                crate::mcp::policy_tenant::route_policy_decision(route_id, session_key).await;
            match crate::mcp::policy_tenant::fetch_target(
                tenant_decision,
                crate::mcp::policy_tenant::default_slot_tenant,
            ) {
                Err(reason) => {
                    warn!(session = %session_key, source, reason = %reason, "policy-context: session tenant unresolved — injecting the fail-open notice instead of the default tenant's policy");
                    (
                        render_failure_notice(&reason, source, &fetched_at),
                        PolicyRenderDecision::tenant_unresolved(raw_source, delivered_sha),
                        None,
                    )
                }
                Ok(target) => match fetch_payload(&base, target, attribution).await {
                    Ok((payload, presented)) => {
                        if mode == Mode::On {
                            persist_policy_body_cache(target.tenant, presented, &payload);
                        }
                        let (text, decision) =
                            render_for_session(&payload, delivered_sha, raw_source, &fetched_at);
                        (text, decision, target.tenant)
                    }
                    Err(reason) => {
                        warn!(session = %session_key, source, reason = %reason, "policy-context: pull failed — injecting the fail-open notice");
                        (
                            render_failure_notice(&reason, source, &fetched_at),
                            PolicyRenderDecision::pull_failed(raw_source, delivered_sha),
                            target.tenant,
                        )
                    }
                },
            }
        }
        Err(reason) => {
            warn!(session = %session_key, source, reason = %reason, "policy-context: cannot consult coord — injecting the fail-open notice");
            (
                render_failure_notice(&reason, source, &fetched_at),
                PolicyRenderDecision::pull_failed(raw_source, delivered_sha),
                None,
            )
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

    // ONE event per injected session, never zero and never two. This is the
    // event that already existed — message and `delivered_marker` /
    // `confirmed_spawn_delivery` field names kept verbatim so saved greps still
    // match — EXTENDED with the typed reason, the facts behind it, both SHAs,
    // and the tenant whose credential fetched it. A second event beside it
    // would make a grep count answer twice.
    //
    // `source` is `normalize_source`'s LABEL and maps an absent source to
    // `startup`; `source_raw` is what the hook actually sent, and `reason`
    // separates the three non-confirmable shapes the label cannot.
    //
    // Gated on BOTH `should_count` and position, and the honest reading is that
    // POSITION is still what decides behaviour here: `Off` returned above, and
    // so did `Observe`, so by this line `mode` can only be `On` and
    // `should_count(mode)` is a constant `true`. Moving this call above the
    // `observe` early-return would still compile and still pass CI, because no
    // TEST exercises this function. It is not dead — `mcp::sessions` calls it
    // for every `SessionStart`, and the route is registered there — it is simply
    // untested: it reads env and talks to coord, so making it EXERCISABLE under
    // test is a bigger change than this phase.
    //
    // What the predicate buys is therefore NOT coverage of this call site. It
    // is that the RULE — "only the mode that actually injects is counted" — now
    // exists as a testable thing (`the_tally_counts_only_the_mode_that_actually
    // _injects`) instead of only as a sentence in a comment, and that any FUTURE
    // caller inherits it by calling `should_count` rather than by being placed
    // correctly. The residual is real and is recorded, not closed.
    if should_count(mode) {
        record_render(decision.reason);
    }
    info!(
        session = %session_key,
        source,
        source_raw = source_for_log(raw_source),
        mode = mode.as_str(),
        bytes = text.len(),
        reason = decision.reason.as_str(),
        served_full_body = decision.served_full_body(),
        confirmed_spawn_delivery = decision.confirmed(),
        source_confirmable = decision.source_confirmable,
        delivered_marker = decision.marker_present,
        body_present = decision.body_present,
        marker_matches = decision.marker_matches,
        delivered_sha = %decision.delivered_sha_short(),
        policy_body_sha = %decision.body_sha_short(),
        tenant = %tenant.map_or_else(|| ABSENT_FIELD.to_string(), |t| t.to_string()),
        "policy-context: injecting fleet policy at SessionStart"
    );
    Some(envelope(&text))
}

// ===========================================================================
// Spawn-time body cache (the source the spawn seams compose from)
// ===========================================================================

/// The tenant a device JWT names, from its unverified `tenant_id` claim.
///
/// Used only to NAME a local cache file and to check that a request presents
/// the target tenant's credential, never to authorize anything — the same
/// posture `coord_mcp::device_jwt_claim_tenant` takes. Coord resolves the
/// policy rows from this same bearer, so the claim is the right scope for both.
fn tenant_of_jwt(jwt: &str) -> Option<uuid::Uuid> {
    let raw = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(jwt.trim())?;
    uuid::Uuid::parse_str(&raw).ok()
}

/// Write the rendered body to the per-tenant spawn cache, best-effort.
///
/// Called by the route after each successful fetch (no extra coord read — see
/// [`crate::session::spawn_prompt`] for why a background refresher was
/// rejected). See [`persist_policy_body_cache_in`] for what is written where.
fn persist_policy_body_cache(
    session_tenant: Option<uuid::Uuid>,
    presented: Option<uuid::Uuid>,
    payload: &PolicyPayload,
) {
    let dir = crate::session::claude_hook::session_restore_dir();
    if let Err(e) = persist_policy_body_cache_in(&dir, session_tenant, presented, payload) {
        warn!(
            error = %e,
            dir = %dir.display(),
            "policy-context: spawn body cache write failed — spawns keep the inline briefing \
             and the body keeps riding this hook"
        );
    }
}

/// [`persist_policy_body_cache`] over an explicit dir — the unit-test surface.
/// Returns the path written, `Ok(None)` when nothing was.
///
/// The file is named for `presented` — the tenant the bearer the fetch
/// ACTUALLY presented claims — and only when that is the session's tenant
/// ([`crate::mcp::policy_tenant::cache_file_tenant`], Design §2a): a body
/// fetched under any other credential is never filed under the session
/// tenant's name, and there is deliberately no unscoped name a prompt could
/// cross tenants through. A payload with no body writes nothing, so a transient
/// body failure never blanks a good cache.
fn persist_policy_body_cache_in(
    dir: &std::path::Path,
    session_tenant: Option<uuid::Uuid>,
    presented: Option<uuid::Uuid>,
    payload: &PolicyPayload,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let Some(body) = render_policy_body(payload) else {
        return Ok(None);
    };
    let Some(tenant) = crate::mcp::policy_tenant::cache_file_tenant(session_tenant, presented)
    else {
        debug!(
            session_tenant = ?session_tenant,
            presented = ?presented,
            "policy-context: the presented credential's tenant is not the session's (or is \
             unknown) — spawn body cache not written"
        );
        return Ok(None);
    };
    crate::session::spawn_prompt::write_policy_body_cache(dir, &tenant, &body).map(Some)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test that touches the process-global render counters.
    ///
    /// Module-level, not a `static` inside one test's body, because the counters
    /// are shared by MORE THAN ONE test and `cargo test` runs a binary's tests on
    /// parallel threads in one process (`.qontinui/ci.toml` runs `cargo test`,
    /// not `nextest`, so there is no process-per-test isolation to fall back on).
    /// A function-local lock cannot be taken by a sibling test, which is exactly
    /// how the first version of this leaked: a second test recorded eight
    /// increments unsynchronized, and when they landed between the counter test's
    /// `before` and `after` every delta went off by one and failed as `2 != 1` —
    /// reading as a SLOT-MAPPING BUG, the very defect this module was being fixed
    /// for. An intermittent failure that accuses the wrong code is worse than no
    /// test.
    ///
    /// Poison recovery per `CONTRIBUTING.md` ("Rust unit tests — env-var
    /// hygiene"): a panicking holder must not cascade-fail its siblings. No
    /// `Drop` guard is needed here — unlike an env var, a counter is never
    /// "restored", and every assertion is on a DELTA precisely so that whatever a
    /// previous holder left behind is irrelevant.
    static COUNTER_LOCK: Mutex<()> = Mutex::new(());

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
            tenant: Some(sample_tenant()),
        }
    }

    /// The tenant [`sample_payload`] was fetched for.
    fn sample_tenant() -> uuid::Uuid {
        uuid::Uuid::parse_str("ce1ec20c-9cf1-427a-857d-81d6449cc6e4").unwrap()
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
        let tenant = Some(sample_tenant());
        assert!(cache_snapshot(&base, tenant).any.is_none(), "starts empty");

        cache_store(
            &base,
            tenant,
            Some("\"14:aaaa\"".to_string()),
            Some("\"1:bbbb\"".to_string()),
            sample_payload(),
        );

        let snap = cache_snapshot(&base, tenant);
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

    /// Tests bullet 2 / Design §3. One coord base, two tenants: an entry for
    /// (base, A) is never served for (base, B), and vice versa — on a
    /// multi-bound device two sessions on one coord can need two tenants'
    /// policies at once.
    #[test]
    fn the_cache_never_serves_one_tenants_entry_for_another_on_the_same_base() {
        let base = format!("https://cache-tenant-{}.example.com", uuid::Uuid::new_v4());
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let payload_for = |t: uuid::Uuid, body: &str| PolicyPayload {
            protocol_body: Some(body.to_string()),
            tenant: Some(t),
            ..sample_payload()
        };

        cache_store(
            &base,
            Some(a),
            Some("\"a-list\"".to_string()),
            Some("\"a-doc\"".to_string()),
            payload_for(a, "A's protocol"),
        );
        let for_b = cache_snapshot(&base, Some(b));
        assert!(
            for_b.any.is_none() && for_b.fresh.is_none(),
            "tenant A's entry must not be served to a tenant-B session"
        );
        assert_eq!(
            (for_b.list_etag, for_b.doc_etag),
            (None, None),
            "and A's validators must not be replayed under B's credential"
        );

        cache_store(
            &base,
            Some(b),
            Some("\"b-list\"".to_string()),
            Some("\"b-doc\"".to_string()),
            payload_for(b, "B's protocol"),
        );
        let for_a = cache_snapshot(&base, Some(a));
        let for_b = cache_snapshot(&base, Some(b));
        assert_eq!(
            for_a.any.and_then(|p| p.protocol_body).as_deref(),
            Some("A's protocol"),
            "storing B's entry must not overwrite A's"
        );
        assert_eq!(
            for_b.any.and_then(|p| p.protocol_body).as_deref(),
            Some("B's protocol")
        );
        assert_eq!(for_a.doc_etag.as_deref(), Some("\"a-doc\""));
        assert_eq!(for_b.doc_etag.as_deref(), Some("\"b-doc\""));
        // A default slot naming no tenant is its own slot, too.
        assert!(cache_snapshot(&base, None).any.is_none());
    }

    // ── The presented credential (Design §2a) ────────────────────────────

    /// A JWT-shaped token carrying `tenant_id` — the same shape
    /// `tenant_of_jwt_reads_the_claim_and_rejects_non_uuids` builds.
    fn jwt_claiming(tenant: uuid::Uuid) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&serde_json::json!({"tenant_id": tenant.to_string()})).unwrap()
            )
        )
    }

    /// The claim is read off the request's own `Authorization` header — the
    /// bearer that will actually be presented — and nothing else.
    #[test]
    fn the_presented_claim_is_read_off_the_requests_own_bearer() {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
        let t = uuid::Uuid::new_v4();
        let mut headers = HeaderMap::new();
        assert_eq!(presented_claim(&headers), None, "no header, no claim");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", jwt_claiming(t))).unwrap(),
        );
        assert_eq!(presented_claim(&headers), Some(t));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(presented_claim(&headers), None, "a non-bearer credential");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );
        assert_eq!(presented_claim(&headers), None, "a bearer naming no tenant");
    }

    /// The last-moment guard: a read is sent only when its credential claims
    /// the target tenant. A target naming no tenant asserts nothing.
    #[test]
    fn a_policy_read_is_sent_only_under_the_target_tenants_own_credential() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert_eq!(check_presented(Some(b), Some(b)), Ok(()));
        let wrong = check_presented(Some(b), Some(a)).expect_err("A's credential for B");
        assert!(wrong.contains(&format!("claims tenant {a}")), "{wrong}");
        let none = check_presented(Some(b), None).expect_err("no credential for B");
        assert!(
            none.contains(&format!("no usable credential claiming tenant {b}")),
            "{none}"
        );
        assert_eq!(check_presented(None, None), Ok(()));
        assert_eq!(check_presented(None, Some(a)), Ok(()));
    }

    /// Tests bullet 5, at the request layer: for a tenant this runner holds no
    /// usable credential for, `coord_get_for(Owned(t))` would go out
    /// UNAUTHENTICATED — and `build_policy_get` refuses it before it can be
    /// sent, so no fetch is made.
    ///
    /// `t` is a fresh random uuid, so no slot can exist for it whatever store
    /// the test process reads (`cargo-guard.sh test` points it at an empty
    /// per-run dir); the read is read-only either way.
    #[test]
    fn no_policy_request_is_built_for_a_tenant_with_no_usable_credential() {
        let client = reqwest::Client::new();
        let t = uuid::Uuid::new_v4();
        let target = crate::mcp::policy_tenant::PolicyTarget {
            scope: crate::auth::TenantScope::Owned(t),
            tenant: Some(t),
        };
        for url in [
            list_url("https://coord.example.com"),
            document_url("https://coord.example.com", PROTOCOL_DOC_NAME),
        ] {
            let refused = build_policy_get(&client, url, target, None, None)
                .expect_err("an unauthenticated read for tenant t must never be built to send");
            assert!(
                refused.contains(&format!("no usable credential claiming tenant {t}")),
                "{refused}"
            );
        }
    }

    // ── The spawn cache file (Design §2a) ────────────────────────────────

    /// Tests bullet 6. The file is named from the PRESENTED bearer's claim, and
    /// nothing is written when that claim is not the session's tenant.
    #[test]
    fn the_spawn_cache_file_is_named_for_the_presented_claim_and_skipped_on_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let payload = PolicyPayload {
            tenant: Some(b),
            ..sample_payload()
        };

        // Mismatch: the session is B, the fetch presented A's credential.
        let written = persist_policy_body_cache_in(dir.path(), Some(b), Some(a), &payload)
            .expect("no I/O error");
        assert_eq!(written, None, "nothing is filed when the claim is not B");
        // No presented claim at all (a cache hit, an unbuildable request).
        assert_eq!(
            persist_policy_body_cache_in(dir.path(), Some(b), None, &payload).unwrap(),
            None
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no file of any name was written on a mismatch"
        );

        // Match: B's file, named from the presented claim, holding B's body.
        let written = persist_policy_body_cache_in(dir.path(), Some(b), Some(b), &payload)
            .expect("no I/O error")
            .expect("a file was written");
        assert_eq!(
            written,
            crate::session::spawn_prompt::policy_body_cache_path(dir.path(), &b)
        );
        assert_eq!(
            crate::session::spawn_prompt::read_policy_body_cache(dir.path(), &b),
            render_policy_body(&payload)
        );
        assert_eq!(
            crate::session::spawn_prompt::read_policy_body_cache(dir.path(), &a),
            None,
            "and nothing under A"
        );
    }

    // ── The tenant line (Design §4) ──────────────────────────────────────

    /// Tests bullet 7. The body carries `[tenant: …]` on the line directly
    /// after the version header, the header line is byte-identical to what it
    /// was, and the line is inside the hashed bytes — two tenants' bodies never
    /// share a delivered-SHA.
    #[test]
    fn the_protocol_body_carries_the_tenant_line_under_a_byte_identical_header() {
        let t = sample_tenant();
        let body = render_policy_body(&sample_payload()).expect("a body");
        let lines: Vec<&str> = body.lines().collect();
        // The literal header — the string the `/unattended` held rule parses.
        let at = lines
            .iter()
            .position(|l| *l == "===== policy/session-protocol (v6) =====")
            .expect("the version header line is byte-identical");
        assert_eq!(
            lines.get(at + 1).copied(),
            Some(format!("[tenant: slug unknown {t}]").as_str()),
            "the tenant line follows the header directly"
        );
        assert_eq!(lines.get(at + 2).copied(), Some(""), "then the blank line");
        assert_eq!(
            lines.get(at + 3).copied(),
            Some("Step 0 — read the policies, fresh."),
            "then the body"
        );

        // Inside the hashed bytes: another tenant's render hashes differently.
        let other = PolicyPayload {
            tenant: Some(uuid::Uuid::new_v4()),
            ..sample_payload()
        };
        assert_ne!(
            crate::session::spawn_prompt::policy_body_sha(&body),
            crate::session::spawn_prompt::policy_body_sha(&render_policy_body(&other).unwrap()),
            "a spawn-time body for one tenant must never confirm a fetch for another"
        );

        // A default credential naming no tenant says so, never renders blank.
        let untenanted = PolicyPayload {
            tenant: None,
            ..sample_payload()
        };
        assert!(render_policy_body(&untenanted)
            .unwrap()
            .contains("===== policy/session-protocol (v6) =====\n[tenant: unknown]\n\n"));
    }

    /// The tenant line through the full SessionStart render
    /// ([`render_injection`], which `render_for_session` serves every session
    /// that is not confirmed by a spawn-time copy): the line directly under the
    /// version header must name the tenant. Without the line it is the blank
    /// separator instead, and this fails.
    #[test]
    fn the_full_render_names_the_tenant_directly_under_the_version_header() {
        let payload = PolicyPayload {
            protocol_body: Some("Step 0 — read the policies, fresh.".to_string()),
            protocol_version: Some(13),
            ..PolicyPayload::default()
        };
        let text = render_injection(&payload, "startup", "2026-11-13T00:00:00Z");
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "===== policy/session-protocol (v13) =====")
            .expect("the version header line is byte-identical");
        let next = lines.get(at + 1).copied().unwrap_or_default();
        assert!(
            next.starts_with("[tenant: "),
            "the line directly under the version header must name the tenant, got {next:?}"
        );
    }

    /// Tests bullet 7, the confirmation half. `render_confirmation` quotes the
    /// version header AND the tenant line, and the first line of every render
    /// names the tenant.
    #[test]
    fn render_confirmation_quotes_the_header_and_the_tenant_line() {
        let payload = sample_payload();
        let t = sample_tenant();
        let sha = current_sha(&payload);
        let text = render_confirmation(&payload, &sha, "startup", "2026-11-13T12:00:00Z");
        assert!(
            text.contains(&format!(
                "`===== policy/session-protocol (v6) =====` followed by the line \
                 `[tenant: slug unknown {t}]`"
            )),
            "{text}"
        );
        let first = text.lines().next().unwrap_or_default();
        assert!(
            first.contains(&format!("These are tenant {t}'s policy documents")),
            "the SessionStart confirmation line names the tenant: {first}"
        );
        let full = render_injection(&payload, "startup", "2026-11-13T12:00:00Z");
        assert!(full
            .lines()
            .next()
            .unwrap_or_default()
            .contains(&format!("These are tenant {t}'s policy documents")));
    }

    /// Tests bullet 9 (render half). A single-bound device renders exactly as
    /// before apart from the tenant: removing the `[tenant: …]` line from the
    /// body gives back the pre-change bytes, literally.
    #[test]
    fn a_single_bound_render_is_todays_bytes_apart_from_the_tenant_line() {
        let t = sample_tenant();
        let body = render_policy_body(&sample_payload()).unwrap();
        let without_tenant = body.replacen(&format!("[tenant: slug unknown {t}]\n"), "", 1);
        let legacy = "[qontinui-runner] This is the canonical text of `policy/session-protocol` \
             (v6), pulled from coord `GET /coord/agent-prompt-documents` and delivered by the \
             runner so that Step 0 of the session protocol is satisfied for this session without \
             you fetching it. Treat it as the authority. You do NOT need to re-read \
             `session-protocol`; you DO still need to read the category bodies it names — the \
             policy document index (`kind=policy`) lists every one with its current version.\n\n\
             ===== policy/session-protocol (v6) =====\n\n\
             Step 0 — read the policies, fresh.\n";
        assert_eq!(without_tenant, legacy);

        // And the full render's first line is today's header plus the tenant
        // sentence, nothing else.
        let full = render_injection(&sample_payload(), "startup", "2026-08-19T12:00:00Z");
        assert_eq!(
            full.lines().next().unwrap_or_default(),
            format!(
                "{} These are tenant {t}'s policy documents, read under that tenant's credential.",
                header("startup", "2026-08-19T12:00:00Z")
            )
        );
    }

    // ── TenantUnresolved (Design §5) ─────────────────────────────────────

    /// Tests bullet 8 (decision half). The unresolved-tenant decision is its
    /// own arm — never `pull_failed` — carries no body, and keeps the facts it
    /// can still know.
    #[test]
    fn an_unresolved_tenant_reasons_tenant_unresolved_and_is_not_a_full_body() {
        let sha = "cd".repeat(32);
        let d = PolicyRenderDecision::tenant_unresolved(Some("startup"), Some(&sha));
        assert_eq!(d.reason, PolicyRenderReason::TenantUnresolved);
        assert_eq!(d.reason.as_str(), "tenant_unresolved");
        assert_ne!(
            d.reason,
            PolicyRenderReason::PullFailed,
            "\"coord unreachable\" and \"whose policy is unknown\" are different findings"
        );
        assert!(!d.served_full_body() && !d.confirmed());
        assert!(d.source_confirmable && d.marker_present);
        assert!(!d.body_present && !d.marker_matches);
        assert_eq!(d.body_sha, None);
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
            tenant: Some(sample_tenant()),
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
            tenant: Some(sample_tenant()),
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
            tenant: Some(sample_tenant()),
        };
        let text = render_injection(&payload, "startup", "2026-08-19T12:00:00Z");
        assert!(text.contains("(kind=policy, 0 documents)"));
        assert!(
            text.contains("treat it as a failed read, not as an empty policy set"),
            "an empty list must never read as 'this tenant has no policies': {text}"
        );
    }

    // ── Spawn-time body + the honesty decision ──────────────────────────

    /// The cached/composed body is deterministic and is byte-for-byte the body
    /// section of the full hook render — so the SHA the spawn side computes and
    /// the SHA the route computes are over the same bytes.
    #[test]
    fn render_policy_body_is_deterministic_and_embedded_verbatim_in_the_full_render() {
        let body = render_policy_body(&sample_payload()).expect("a body was fetched");
        assert_eq!(Some(&body), render_policy_body(&sample_payload()).as_ref());
        assert!(body.contains("===== policy/session-protocol (v6) ====="));
        assert!(body.contains("Step 0 — read the policies, fresh."));
        // No per-render values: a fetch time or source would change the SHA on
        // every start and the confirmation branch could never fire.
        assert!(!body.contains("source:"));
        assert!(!body.contains("2026-08-19T12:00:00Z"));

        let full = render_injection(&sample_payload(), "startup", "2026-08-19T12:00:00Z");
        assert!(
            full.contains(&body),
            "the full render carries the same bytes"
        );

        let no_body = PolicyPayload {
            protocol_body: None,
            ..sample_payload()
        };
        assert_eq!(render_policy_body(&no_body), None);
    }

    #[test]
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn a_matching_marker_gets_the_short_confirmation_without_the_body() {
        let payload = sample_payload();
        let sha =
            crate::session::spawn_prompt::policy_body_sha(&render_policy_body(&payload).unwrap());
        let (text, decision) = render_for_session(
            &payload,
            Some(&sha),
            Some("compact"),
            "2026-08-19T12:00:00Z",
        );
        assert!(decision.confirmed());
        // Still attributable, still versioned.
        assert!(text.starts_with("[qontinui-runner]"));
        assert!(text.contains("source: compact"));
        assert!(text.contains(&format!("v6, sha256 {}", &sha[..12])));
        // It says what the runner KNOWS — it composed the spawn file — and
        // never that it verified the copy inside the model's system prompt.
        assert!(text.contains("composed a system-prompt file"));
        assert!(!text.contains("verified"), "{text}");
        assert!(!text.contains("was delivered into"), "{text}");
        // The body is NOT repeated — that is the whole point.
        assert!(!text.contains("Step 0 — read the policies, fresh."));
        // The index still rides the hook.
        assert!(text.contains("(kind=policy, 2 documents)"));
        assert!(text.contains("- escalation-bar — Escalation Bar (v4)"));
        // And it tells a session that cannot find the block what to do.
        assert!(text.contains("treat Step 0 as NOT satisfied"));

        // At a realistic body size (~8 KB, as measured for session-protocol)
        // the confirmation is a fraction of the full render — the point of
        // moving the body off the hook. (The one-line sample body above is
        // shorter than the confirmation's own prose, so it cannot show this.)
        let real = PolicyPayload {
            protocol_body: Some("policy clause text. ".repeat(400)),
            ..sample_payload()
        };
        let real_sha =
            crate::session::spawn_prompt::policy_body_sha(&render_policy_body(&real).unwrap());
        let (short, decision) = render_for_session(
            &real,
            Some(&real_sha),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert!(decision.confirmed());
        let full = render_injection(&real, "startup", "2026-08-19T12:00:00Z");
        assert!(
            short.len() * 3 < full.len(),
            "confirmation {} bytes vs full {} bytes",
            short.len(),
            full.len()
        );
    }

    /// The resume-snapshot rule, per RAW source: a matching marker confirms
    /// only on `startup`/`compact`. `resume` re-sends the system prompt recorded
    /// when the conversation began, a `/clear` snapshot reset is unverified,
    /// and a missing or unknown source is no evidence either way, so all of
    /// those get the full body — even though [`normalize_source`] labels an
    /// absent source `startup`.
    #[test]
    fn only_startup_and_compact_may_confirm_a_matching_marker() {
        let payload = sample_payload();
        let sha =
            crate::session::spawn_prompt::policy_body_sha(&render_policy_body(&payload).unwrap());
        let at = "2026-08-19T12:00:00Z";
        for (raw, expect_confirmed, label) in [
            (Some("startup"), true, "startup"),
            (Some("compact"), true, "compact"),
            (Some(" COMPACT "), true, "compact"),
            (Some("clear"), false, "clear"),
            (Some(" CLEAR "), false, "clear"),
            (Some("resume"), false, "resume"),
            (None, false, "startup"),
            (Some(""), false, "startup"),
            (Some("reload"), false, "startup"),
        ] {
            assert_eq!(
                source_permits_confirmation(raw),
                expect_confirmed,
                "{raw:?}"
            );
            let (text, decision) = render_for_session(&payload, Some(&sha), raw, at);
            assert_eq!(decision.confirmed(), expect_confirmed, "{raw:?}");
            assert!(text.contains(&format!("source: {label}")), "{raw:?}");
            if !expect_confirmed {
                assert_eq!(text, render_injection(&payload, label, at), "{raw:?}");
                assert!(text.contains("Step 0 — read the policies, fresh."));
            }
        }
    }

    #[test]
    fn a_missing_or_stale_marker_gets_the_full_body() {
        let payload = sample_payload();
        let full = render_injection(&payload, "startup", "2026-08-19T12:00:00Z");

        // No marker: the session was not given the file carrier.
        let (text, decision) =
            render_for_session(&payload, None, Some("startup"), "2026-08-19T12:00:00Z");
        assert!(!decision.confirmed());
        assert_eq!(text, full);

        // A marker for an OLDER body: the spawn-time copy is stale.
        let stale = crate::session::spawn_prompt::policy_body_sha("an older session-protocol");
        let (text, decision) = render_for_session(
            &payload,
            Some(&stale),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert!(!decision.confirmed());
        assert_eq!(text, full);

        // No body fetched: nothing to compare, the partial render stands.
        let partial = PolicyPayload {
            protocol_body: None,
            ..sample_payload()
        };
        let (text, decision) = render_for_session(
            &partial,
            Some(&stale),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert!(!decision.confirmed());
        assert!(text.contains("Step 0 is NOT satisfied"));
    }

    // =========================================================================
    // Why the full body was served — the typed reason, the facts beside it, the
    // two SHAs, and the counter.
    //
    // Plan `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`
    // Phase 2. One test per arm of `PolicyRenderReason`: the arms exist so that
    // no two causes share a label, and a test that covered several at once
    // would be unable to tell them apart either.
    // =========================================================================

    /// The current body's SHA, the value a correctly-carried marker holds.
    fn current_sha(payload: &PolicyPayload) -> String {
        crate::session::spawn_prompt::policy_body_sha(&render_policy_body(payload).unwrap())
    }

    /// ARM: `confirmed`. Confirmable source, body present, marker present and
    /// matching. This is the denominator — without it the full-body count has
    /// nothing to divide by.
    #[test]
    fn a_matching_marker_on_a_confirmable_source_reasons_confirmed() {
        let payload = sample_payload();
        let sha = current_sha(&payload);
        for source in ["startup", "compact", " COMPACT "] {
            let (_, d) =
                render_for_session(&payload, Some(&sha), Some(source), "2026-08-19T12:00:00Z");
            assert_eq!(d.reason, PolicyRenderReason::Confirmed, "{source}");
            assert_eq!(d.reason.as_str(), "confirmed");
            assert!(d.confirmed(), "{source}");
            assert!(
                !d.served_full_body(),
                "a confirmation must never be counted as a full body: {source}"
            );
            assert!(d.source_confirmable && d.marker_present && d.body_present && d.marker_matches);
        }
    }

    /// ARM: `source_absent` — the cause Phase 0 measured in production, and the
    /// single most valuable arm here.
    ///
    /// The materialized hook gated its payload parse on `command -v python`,
    /// which does not exist on a Linux box, so it built a URL with NO `source=`
    /// parameter and the route saw `None`. `normalize_source` labels that
    /// `startup`, so 98 injections on 2026-09-20 looked like ordinary startups
    /// holding a valid marker. This test is the assertion that an absent source
    /// is never again filed under `resume`'s reason — or under `startup`'s.
    ///
    /// The marker here is CORRECT and matching, so nothing but the source can
    /// have produced this reason.
    #[test]
    fn an_absent_source_reasons_source_absent_and_not_the_resume_arm() {
        let payload = sample_payload();
        let sha = current_sha(&payload);
        for source in [None, Some(""), Some("   ")] {
            let (text, d) =
                render_for_session(&payload, Some(&sha), source, "2026-08-19T12:00:00Z");
            assert_eq!(d.reason, PolicyRenderReason::SourceAbsent, "{source:?}");
            assert_eq!(d.reason.as_str(), "source_absent");
            assert!(d.served_full_body(), "{source:?}");
            assert!(!d.source_confirmable, "{source:?}");
            // Everything else about this session was fine — which is exactly
            // what made the cause invisible.
            assert!(
                d.marker_present && d.body_present && d.marker_matches,
                "{source:?}"
            );
            // And the label the event's `source` field would carry is the
            // misleading one, which is why the typed reason has to exist.
            assert_eq!(normalize_source(source), "startup", "{source:?}");
            assert!(text.contains("source: startup"), "{source:?}");
        }
    }

    /// ARM: `source_unrecognized`. A value this runner does not know — a future
    /// Claude release or a hook bug. Also labelled `startup` by
    /// `normalize_source`, and a third finding again: neither a defect in the
    /// hook's shell nor correct behaviour.
    #[test]
    fn an_unknown_source_reasons_source_unrecognized() {
        let payload = sample_payload();
        let sha = current_sha(&payload);
        for source in ["reload", "rewind", "startup2"] {
            let (_, d) =
                render_for_session(&payload, Some(&sha), Some(source), "2026-08-19T12:00:00Z");
            assert_eq!(d.reason, PolicyRenderReason::SourceUnrecognized, "{source}");
            assert_eq!(d.reason.as_str(), "source_unrecognized");
            assert!(d.served_full_body(), "{source}");
            assert!(!d.source_confirmable, "{source}");
            assert!(
                d.marker_present && d.body_present && d.marker_matches,
                "{source}"
            );
            assert_eq!(normalize_source(Some(source)), "startup", "{source}");
        }
    }

    /// ARM: `source_not_confirmable`. A KNOWN source that gets the full body BY
    /// DESIGN. That population must be separable from the two above — conflating
    /// them is how the plan's headline 524-vs-2 split overstated the defect.
    #[test]
    fn a_resume_or_clear_reasons_source_not_confirmable() {
        let payload = sample_payload();
        let sha = current_sha(&payload);
        for source in ["resume", "clear", " CLEAR "] {
            let (_, d) =
                render_for_session(&payload, Some(&sha), Some(source), "2026-08-19T12:00:00Z");
            assert_eq!(
                d.reason,
                PolicyRenderReason::SourceNotConfirmable,
                "{source}"
            );
            assert_eq!(d.reason.as_str(), "source_not_confirmable");
            assert!(d.served_full_body(), "{source}");
            assert!(!d.source_confirmable, "{source}");
            assert!(
                d.marker_present && d.body_present && d.marker_matches,
                "{source}"
            );
        }
    }

    /// ARM: `body_unavailable`. Coord served no protocol body, so there was
    /// nothing to hash. It gets its OWN arm rather than folding into
    /// `marker_absent`/`marker_mismatched`, because it names a coord-side
    /// degradation and it changes the render itself (the partial notice).
    #[test]
    fn a_payload_with_no_body_reasons_body_unavailable() {
        let bodyless = PolicyPayload {
            protocol_body: None,
            ..sample_payload()
        };
        // A marker IS present and a confirmable source IS given, so nothing but
        // the missing body can explain this reason.
        let marker = crate::session::spawn_prompt::policy_body_sha("anything at all");
        let (text, d) = render_for_session(
            &bodyless,
            Some(&marker),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert_eq!(d.reason, PolicyRenderReason::BodyUnavailable);
        assert_eq!(d.reason.as_str(), "body_unavailable");
        assert!(d.source_confirmable && d.marker_present);
        assert!(!d.body_present);
        assert!(!d.marker_matches);
        assert_eq!(d.body_sha, None);
        assert_eq!(d.body_sha_short(), "<none>");
        assert!(text.contains("Step 0 is NOT satisfied"));
    }

    /// ARM: `marker_absent`. A confirmable source and a body, but the session
    /// forwarded no marker — the seam did not carry it.
    #[test]
    fn a_confirmable_source_with_no_marker_reasons_marker_absent() {
        let payload = sample_payload();
        for source in ["startup", "compact"] {
            let (_, d) = render_for_session(&payload, None, Some(source), "2026-08-19T12:00:00Z");
            assert_eq!(d.reason, PolicyRenderReason::MarkerAbsent, "{source}");
            assert_eq!(d.reason.as_str(), "marker_absent");
            assert!(d.served_full_body(), "{source}");
            assert!(d.source_confirmable && d.body_present, "{source}");
            assert!(!d.marker_present && !d.marker_matches, "{source}");
            assert_eq!(d.delivered_sha_short(), "<none>", "{source}");
        }
    }

    /// ARM: `marker_mismatched`. A marker arrived and names ANOTHER body — the
    /// spawn-time copy is stale. Distinct from `marker_absent` because the
    /// remedies are different: a stale copy means coord versioned the document
    /// since the process started, not that a seam dropped the value.
    #[test]
    fn a_marker_for_another_body_reasons_marker_mismatched() {
        let payload = sample_payload();
        let stale = crate::session::spawn_prompt::policy_body_sha("an older session-protocol");
        assert_ne!(stale, current_sha(&payload));
        let (_, d) = render_for_session(
            &payload,
            Some(&stale),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert_eq!(d.reason, PolicyRenderReason::MarkerMismatched);
        assert_eq!(d.reason.as_str(), "marker_mismatched");
        assert!(d.served_full_body());
        assert!(d.source_confirmable && d.marker_present && d.body_present);
        assert!(!d.marker_matches);
    }

    /// ARM: `pull_failed`. The coord read failed, so the session got the
    /// fail-open notice and NO policy body. Counted — otherwise `injections` is
    /// not every `SessionStart` this route answered — but never counted as a
    /// full body, which it is not.
    #[test]
    fn a_failed_pull_reasons_pull_failed_and_is_not_a_full_body() {
        let sha = "ab".repeat(32);
        let d = PolicyRenderDecision::pull_failed(Some("startup"), Some(&sha));
        assert_eq!(d.reason, PolicyRenderReason::PullFailed);
        assert_eq!(d.reason.as_str(), "pull_failed");
        assert!(!d.served_full_body());
        assert!(!d.confirmed());
        // The facts it CAN still know are kept, and the one it cannot is false
        // rather than invented.
        assert!(d.source_confirmable && d.marker_present);
        assert!(!d.body_present && !d.marker_matches);
        assert_eq!(d.body_sha, None);

        let d = PolicyRenderDecision::pull_failed(Some("resume"), None);
        assert!(!d.source_confirmable && !d.marker_present);
    }

    /// `classify_source` is the whole source gate, and it agrees with
    /// `source_permits_confirmation` on the confirmable/not split while adding
    /// the three-way "why not".
    #[test]
    fn classify_source_splits_the_three_non_confirmable_shapes() {
        // NOTE: the `source_permits_confirmation` assertions below are NOT a
        // cross-check — that function delegates here, so they call the same code
        // twice and nothing could disagree. They are kept as documentation that
        // the two answers are one answer. The genuine pin on the accepted set is
        // `only_startup_and_compact_may_confirm_a_matching_marker`.
        for raw in [Some("startup"), Some("compact"), Some(" Startup ")] {
            assert_eq!(classify_source(raw), Ok(()), "{raw:?}");
            assert!(source_permits_confirmation(raw), "{raw:?}");
            // A present, non-blank source is echoed to the log VERBATIM —
            // untrimmed, so padding a hook accidentally sent stays visible.
            assert_eq!(source_for_log(raw), raw.unwrap(), "{raw:?}");
        }
        for (raw, expect) in [
            (None, PolicyRenderReason::SourceAbsent),
            (Some(""), PolicyRenderReason::SourceAbsent),
            (Some("  "), PolicyRenderReason::SourceAbsent),
            (Some("resume"), PolicyRenderReason::SourceNotConfirmable),
            (Some("clear"), PolicyRenderReason::SourceNotConfirmable),
            (Some("reload"), PolicyRenderReason::SourceUnrecognized),
        ] {
            assert_eq!(classify_source(raw), Err(expect), "{raw:?}");
            assert!(!source_permits_confirmation(raw), "{raw:?}");
        }
    }

    /// The raw source reaches the log as itself, or as an explicit sentinel —
    /// never as a BLANK field, which reads as a broken field rather than as an
    /// absent value. That was a round-1 finding on `short_sha`'s sibling, and
    /// the fix shipped untested, so both halves are pinned here.
    #[test]
    fn the_raw_source_logs_verbatim_or_says_it_was_absent() {
        // Absent, empty and whitespace-only all render as the sentinel. The
        // middle two are the round-1 defect: without the emptiness test they
        // would log as an empty field.
        for raw in [None, Some(""), Some("   "), Some("\t\n")] {
            // The LITERAL, not the const: this field is a grep contract like
            // `PolicyRenderReason::as_str`'s eight labels, and comparing a
            // const against itself would let the spelling change under every
            // saved query without a test objecting.
            assert_eq!(source_for_log(raw), "<absent>", "{raw:?}");
            assert!(!source_for_log(raw).is_empty(), "{raw:?}");
        }
        // Anything with content is echoed EXACTLY — the field's job is to say
        // what the hook put on the wire, so neither case nor padding is
        // normalized away.
        for raw in ["startup", " startup ", "COMPACT", "reload", "x"] {
            assert_eq!(source_for_log(Some(raw)), raw);
        }
        // And the two sentinels stay distinct, so a reader never has to work out
        // which field a `<none>` belonged to.
        assert_ne!(ABSENT_SOURCE, ABSENT_FIELD);
    }

    /// Both SHAs reach the decision, and the log form is the SAME 12-character
    /// prefix the confirmation text shows a session — so a transcript and a log
    /// line are comparable by eye. Phase 0 recorded the absence of these two
    /// values as the reason no past injection could be re-adjudicated.
    #[test]
    fn the_decision_carries_both_shas_and_logs_the_same_prefix_the_text_shows() {
        let payload = sample_payload();
        let sha = current_sha(&payload);
        let (text, d) = render_for_session(
            &payload,
            Some(&sha),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );

        assert_eq!(d.delivered_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(d.body_sha.as_deref(), Some(sha.as_str()));

        let short: String = sha.chars().take(LOGGED_SHA_PREFIX).collect();
        assert_eq!(LOGGED_SHA_PREFIX, 12);
        assert_eq!(d.delivered_sha_short(), short);
        assert_eq!(d.body_sha_short(), short);
        assert!(
            text.contains(&short),
            "the confirmation shows the same prefix the log does"
        );

        // A stale marker is reported as ITSELF, not as the body's SHA — the
        // point of logging both is that they can disagree.
        let stale = crate::session::spawn_prompt::policy_body_sha("an older session-protocol");
        let (_, d) = render_for_session(
            &payload,
            Some(&stale),
            Some("startup"),
            "2026-08-19T12:00:00Z",
        );
        assert_ne!(d.delivered_sha_short(), d.body_sha_short());
        assert_eq!(d.delivered_sha.as_deref(), Some(stale.as_str()));
        assert_eq!(d.body_sha.as_deref(), Some(sha.as_str()));
    }

    /// The precedence is the documented one, and a session that fails SEVERAL
    /// preconditions still reports every fact.
    ///
    /// This is the anti-lumping guard: one reason per event is only honest if
    /// the facts that did not win the precedence are still readable.
    #[test]
    fn the_reason_names_the_first_unmet_precondition_and_the_facts_all_survive() {
        let bodyless = PolicyPayload {
            protocol_body: None,
            ..sample_payload()
        };

        // Source AND body AND marker all bad: the source gate is first.
        let (_, d) = render_for_session(&bodyless, None, Some("resume"), "2026-08-19T12:00:00Z");
        assert_eq!(d.reason, PolicyRenderReason::SourceNotConfirmable);
        assert!(!d.source_confirmable && !d.body_present && !d.marker_present);

        // Body AND marker both missing, source fine: a body must exist before a
        // marker can be compared to anything, so the body wins.
        let (_, d) = render_for_session(&bodyless, None, Some("startup"), "2026-08-19T12:00:00Z");
        assert_eq!(d.reason, PolicyRenderReason::BodyUnavailable);
        assert!(d.source_confirmable);
        assert!(!d.body_present && !d.marker_present);
    }

    /// The label set is the grep contract — every saved query keys on these
    /// strings — and exactly six of the nine arms mean a full body crossed the
    /// hook boundary.
    #[test]
    fn every_reason_has_a_distinct_stable_label_and_six_mean_full_body() {
        let labels: Vec<&str> = PolicyRenderReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "source_absent",
                "source_unrecognized",
                "source_not_confirmable",
                "body_unavailable",
                "marker_absent",
                "marker_mismatched",
                "confirmed",
                "pull_failed",
                "tenant_unresolved",
            ]
        );
        let full: Vec<&str> = PolicyRenderReason::ALL
            .iter()
            .filter(|r| r.served_full_body())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(
            full,
            vec![
                "source_absent",
                "source_unrecognized",
                "source_not_confirmable",
                "body_unavailable",
                "marker_absent",
                "marker_mismatched",
            ],
            "a confirmation, a failed pull and an unresolved tenant carry no body and must not \
             be counted as one"
        );
        // Slots are distinct, so no two reasons share a counter.
        let mut slots: Vec<Option<usize>> =
            PolicyRenderReason::ALL.iter().map(|r| r.slot()).collect();
        assert!(
            slots.iter().all(Option::is_some),
            "every arm must resolve to a slot: {slots:?}"
        );
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), PolicyRenderReason::ALL.len());
    }

    /// The counter array is sized by `COUNT`; the arms are listed in `ALL`.
    /// They are two declarations of one number, so pin them together.
    #[test]
    fn the_arm_count_is_pinned_to_the_arm_list() {
        assert_eq!(PolicyRenderReason::ALL.len(), PolicyRenderReason::COUNT);
        // Nine since `TenantUnresolved` (plan
        // `2026-11-13-runner-policy-injection-uses-the-device-default-tenant-not-the-sessions`
        // Design §5): a literal, so the arm cannot be dropped from both
        // declarations at once without this objecting.
        assert_eq!(PolicyRenderReason::COUNT, 9);
        assert!(PolicyRenderReason::ALL.contains(&PolicyRenderReason::TenantUnresolved));
    }

    /// `record_render` files each reason in its own slot, and the two derived
    /// totals are the sums they claim to be.
    ///
    /// The counters are process-global, so this test serializes against itself
    /// and asserts on DELTAS — an absolute assertion would be a race with any
    /// future test that records.
    #[test]
    fn the_counter_files_each_reason_separately_and_the_totals_add_up() {
        // Excludes every other counter-touching test in this binary — see
        // `COUNTER_LOCK`. Assertions are still on DELTAS rather than absolutes,
        // so an earlier holder's increments are irrelevant.
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let before = render_stats();
        // A distinct, non-uniform number of hits per reason, so a slot that
        // counts into a neighbour cannot pass by symmetry.
        for (i, reason) in PolicyRenderReason::ALL.iter().enumerate() {
            for _ in 0..=i {
                record_render(*reason);
            }
        }
        let after = render_stats();

        assert_eq!(after.source_absent - before.source_absent, 1);
        assert_eq!(after.source_unrecognized - before.source_unrecognized, 2);
        assert_eq!(
            after.source_not_confirmable - before.source_not_confirmable,
            3
        );
        assert_eq!(after.body_unavailable - before.body_unavailable, 4);
        assert_eq!(after.marker_absent - before.marker_absent, 5);
        assert_eq!(after.marker_mismatched - before.marker_mismatched, 6);
        assert_eq!(after.confirmed - before.confirmed, 7);
        assert_eq!(after.pull_failed - before.pull_failed, 8);
        assert_eq!(after.tenant_unresolved - before.tenant_unresolved, 9);

        // Hand-computed literals, NOT the same sum `render_stats` just made
        // from the same locals: re-deriving `full_body_total` from the fields
        // beside it passes on any implementation, including one that reads
        // every slot as 0. 1+2+3+4+5+6 = 21 full-body; +7 confirmed +8 pull
        // failed +9 tenant unresolved = 45 injections.
        assert_eq!(after.full_body_total - before.full_body_total, 21);
        assert_eq!(after.injections - before.injections, 45);
    }

    /// `should_count` is the rule that keeps the tally a count of INJECTIONS.
    ///
    /// Phase 2 originally expressed this as the POSITION of `record_render`
    /// below `policy_context`'s `observe` early-return, guarded by five lines of
    /// comment and nothing else. Moving that statement up compiled, passed every
    /// test, and turned the counter into a tally of renders that were never
    /// injected — a runner soaked in `observe` reporting a four-figure
    /// `full_body_total` having delivered nothing. A predicate can be pinned.
    #[test]
    fn the_tally_counts_only_the_mode_that_actually_injects() {
        assert!(should_count(Mode::On));
        assert!(
            !should_count(Mode::Observe),
            "observe renders the payload and injects NOTHING — counting it makes \
             full_body_total a count of renders that were never delivered"
        );
        assert!(!should_count(Mode::Off));
    }

    /// Every arm is reachable in the counter array, and none indexes past it.
    ///
    /// The slot is derived from `ALL`, so this is what makes the derivation's
    /// promise real: one more arm left out of `ALL` compiles (`ALL: [Self; COUNT]`
    /// stays a well-typed `COUNT`-element literal) and would otherwise be found only
    /// by a panic inside the fail-open route.
    #[test]
    fn the_every_arm_is_listed_and_slots_are_in_range() {
        for reason in PolicyRenderReason::ALL {
            let slot = reason
                .slot()
                .unwrap_or_else(|| panic!("{} missing from ALL", reason.as_str()));
            assert!(slot < PolicyRenderReason::COUNT, "{}", reason.as_str());
        }
        // Recording never panics, whatever the arm. This TOUCHES the shared
        // counters, so it takes `COUNTER_LOCK` like every other test that does —
        // without it these increments race the delta assertions in
        // `the_counter_files_each_reason_separately_and_the_totals_add_up` and
        // make it fail intermittently as though the slot mapping were wrong.
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for reason in PolicyRenderReason::ALL {
            record_render(reason);
        }
    }

    /// The snapshot serializes under the exact keys the stats route publishes —
    /// they are the operator-facing half of the grep contract.
    #[test]
    fn the_stats_snapshot_publishes_one_key_per_reason_plus_the_two_totals() {
        let json = serde_json::to_value(render_stats()).unwrap();
        let obj = json.as_object().expect("stats serialize as an object");
        for reason in PolicyRenderReason::ALL {
            assert!(
                obj.contains_key(reason.as_str()),
                "stats must carry a key named for {}",
                reason.as_str()
            );
        }
        assert!(obj.contains_key("full_body_total"));
        // The new arm's operator-facing key, by its literal name (the stats
        // route is a grep contract like `as_str`'s labels).
        assert!(
            obj.contains_key("tenant_unresolved"),
            "`/sessions/policy-context-stats` must publish the tenant_unresolved arm"
        );
        assert!(
            obj.contains_key("injections"),
            "the unit is an injection, not a session — the key says so"
        );
        assert!(
            !obj.contains_key("total"),
            "`total` was the per-SESSION lie; it must not come back"
        );
        assert_eq!(obj.len(), PolicyRenderReason::ALL.len() + 2);
    }

    #[test]
    fn delivered_sha_parses_only_a_full_sha256_hex() {
        let sha = "AB".repeat(32);
        assert_eq!(parse_delivered_sha(Some(&sha)), Some("ab".repeat(32)));
        assert_eq!(
            parse_delivered_sha(Some(&format!(" {} ", "0f".repeat(32)))),
            Some("0f".repeat(32))
        );
        for bad in [
            "",
            "   ",
            "abc",
            &"g".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            assert_eq!(parse_delivered_sha(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(parse_delivered_sha(None), None);
    }

    /// The route reads the marker from the request HEADER (case-insensitive
    /// name), strictly; the query string no longer carries it.
    #[test]
    fn delivered_sha_is_read_from_the_header_only() {
        use axum::http::{HeaderMap, HeaderValue};
        let sha = "AB".repeat(32);
        let mut headers = HeaderMap::new();
        assert_eq!(delivered_sha_from_headers(&headers), None);
        headers.insert(
            "X-Qontinui-Policy-Delivered-Sha",
            HeaderValue::from_str(&sha).unwrap(),
        );
        assert_eq!(delivered_sha_from_headers(&headers), Some("ab".repeat(32)));
        headers.insert(DELIVERED_SHA_HEADER, HeaderValue::from_static("abc"));
        assert_eq!(delivered_sha_from_headers(&headers), None);
        assert_eq!(DELIVERED_SHA_HEADER, "x-qontinui-policy-delivered-sha");
    }

    #[test]
    fn tenant_of_jwt_reads_the_claim_and_rejects_non_uuids() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let tenant = uuid::Uuid::new_v4();
        let jwt_with = |claims: serde_json::Value| {
            format!(
                "h.{}.s",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
            )
        };
        assert_eq!(
            tenant_of_jwt(&jwt_with(
                serde_json::json!({"tenant_id": tenant.to_string()})
            )),
            Some(tenant)
        );
        assert_eq!(
            tenant_of_jwt(&jwt_with(serde_json::json!({"tenant_id": "acme"}))),
            None
        );
        assert_eq!(tenant_of_jwt(&jwt_with(serde_json::json!({}))), None);
        assert_eq!(tenant_of_jwt("opaque-token"), None);
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
