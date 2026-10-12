//! The follow-up family of the `:9876/plan-library/*` door (plan
//! `2026-10-10-small-followups-are-recorded-but-nothing-burns-them-down`
//! Phase 4).
//!
//! A follow-up is a one-ended `spawned_followup` edge: work an implementation
//! deliberately did not do. qontinui-web gives it file anchors, a size class
//! and a closure state of its own (that plan's Phase 3) and a reconcile that
//! closes `fixing` rows from coord's land record (its Phase 5). These five
//! forwards put those web routes on the runner's loopback, so a session
//! reaches them the way it reaches the rest of the library: the runner
//! attaches its own device JWT, and the caller presents nothing but — for a
//! write — the session's proxy nonce.
//!
//! The parent module's forward table is the doc-of-record for every route,
//! these five included; this module holds their handlers because
//! `plan_library.rs` is past the size where a new family belongs in it
//! (served policy `engineering-priorities` `modular-code-and-file-size`).
//!
//! ## Verbatim: path, query, body and status
//!
//! Each forward sends the request's path, query string and body to the web
//! route unchanged, and answers with the web route's own status — a 2xx keeps
//! its code (a `201` stays a `201`), and a non-2xx is forwarded with its
//! status and body, tagged [`CODE_UPSTREAM_STATUS`] so a caller can tell the
//! web route's refusal from this door's own.
//!
//! **The query is NOT allowlisted**, unlike the parent's list reads. Those
//! allowlists exist so "a typo cannot silently widen a read". Every
//! plan-library web route is a `StrictQueryRoute`, which already turns an
//! unknown key into a loud `422` naming the keys it accepts. A runner-side
//! allowlist in front of that would DROP the unknown key instead, so
//! `?stat=fixing` would come back as a `200` page of OPEN follow-ups — the
//! silent widening the allowlist exists to prevent (dossier
//! `agent-door-filters-silently-ignored`). Forwarding the raw query also
//! means a filter web adds later works through this door with no runner
//! release.
//!
//! The body is forwarded as the caller's own bytes. It is parsed once — for
//! well-formedness only, after the write gate — and never re-serialized, so
//! key order, spacing and number spelling arrive exactly as sent. An empty
//! body is forwarded as no body (`POST …/followups/reconcile` takes none).
//!
//! The one normalization is the edge `{id}` segment, which must be a UUID and
//! is forwarded in canonical hyphenated form — the parent's rule for every id
//! segment, so no `..`, `/` or `?` can reach the wire through it.
//!
//! ## Gating
//!
//! The three writes — `PUT …/edges/{id}/anchors`,
//! `POST …/edges/{id}/resolution` and `POST …/followups/reconcile` — call the
//! parent's `authorize_write` FIRST (nonce → kill switch → dial), and take the
//! body as raw [`Bytes`], so a request without a registered nonce answers
//! `401` whatever its body or id looks like. The two reads —
//! `GET …/followups` and `POST …/followups/match` — are ungated and fold the
//! parent's capability block into every answer, failures included. The match
//! is a `POST` only because it carries a set of up to 500 paths; it mutates
//! nothing.
//!
//! ## A failed exchange is UNKNOWN, never an empty answer
//!
//! `/implement-plan` Step 4.3 reads the match proxy before every PR, and
//! "nothing matched" changes what it does. So a web backend that did not
//! answer is a `502` carrying [`CODE_WEB_UNREACHABLE`] and a sentence saying
//! the answer is UNKNOWN, with `webBackendReachable: false` in `data` — never
//! a `200` with an empty list (served policy `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`). On a write the same failure says
//! whether the request can have reached the backend at all: a refused
//! connection was never sent, but a timeout or a broken answer may have been
//! applied, and the text says to read the row back before retrying.
//!
//! ## No attribution fold
//!
//! The parent's `links` writes fold the nonce's principal into their free-text
//! `note` / `reason`. These forwards do not, and must not: `resolution_note`
//! is the very field web's CHECK requires to be non-blank on a `dismissed`
//! row, so appending an attribution would let a reasonless dismissal through.
//! Who sent each write is logged here instead (`plan-library follow-up
//! write`), and web records its own audit actor.

use axum::body::Bytes;
use axum::extract::{Path, RawQuery};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::Value;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ===========================================================================
// The family's route table
// ===========================================================================

/// `GET` — the follow-up queue, filters and cursor forwarded verbatim.
const FOLLOWUPS: &str = "/plan-library/followups";
/// `POST` — the open and `fixing` follow-ups anchored on a set of paths.
const FOLLOWUPS_MATCH: &str = "/plan-library/followups/match";
/// `POST` — move every `fixing` follow-up by coord's land verdict.
const FOLLOWUPS_RECONCILE: &str = "/plan-library/followups/reconcile";
/// `PUT` — replace one follow-up's anchor set.
const EDGE_ANCHORS: &str = "/plan-library/edges/{id}/anchors";
/// `POST` — one follow-up's closure-state transition.
const EDGE_RESOLUTION: &str = "/plan-library/edges/{id}/resolution";

/// This family's rows of the door's route table, `(method, path,
/// requires_a_nonce)`, composed into the parent's `route_entries()`. Keep in
/// lockstep with [`router`]; a test here pins the count and the gating split,
/// and the parent's forward-table test requires a doc row for each.
pub(super) const ROUTE_ENTRIES: [(&str, &str, bool); 5] = [
    ("GET", FOLLOWUPS, false),
    ("POST", FOLLOWUPS_MATCH, false),
    ("PUT", EDGE_ANCHORS, true),
    ("POST", EDGE_RESOLUTION, true),
    ("POST", FOLLOWUPS_RECONCILE, true),
];

/// The family's routes, typed for the runner's API state. Merged by the
/// parent's `routes()`, which is what mounts them.
pub(super) fn routes() -> Router<Arc<ApiState>> {
    router()
}

/// The registration itself, generic over the router state so the tests drive
/// THIS table rather than a hand-copied one: every handler is stateless.
fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(FOLLOWUPS, get(list_followups_handler))
        .route(FOLLOWUPS_MATCH, post(match_followups_handler))
        .route(FOLLOWUPS_RECONCILE, post(reconcile_followups_handler))
        .route(EDGE_ANCHORS, put(replace_anchors_handler))
        .route(EDGE_RESOLUTION, post(transition_followup_handler))
}

// ===========================================================================
// The closed vocabularies these routes carry
// ===========================================================================

/// A follow-up's `size_class` — the closed set D2 of the plan above defines,
/// and the field `/implement-plan` Step 4.3 folds by.
///
/// Published as `writeVocabulary.size_class` so a writer learns the set before
/// writing. This door does not refuse a value outside it: the field rides in a
/// body forwarded verbatim, and the web route is the authority on its own
/// request model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FollowupSizeClass {
    Trivial,
    Small,
    PlanSized,
}

impl FollowupSizeClass {
    /// Every size class, smallest first.
    pub(super) const ALL: [FollowupSizeClass; 3] = [
        FollowupSizeClass::Trivial,
        FollowupSizeClass::Small,
        FollowupSizeClass::PlanSized,
    ];

    /// The wire string.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            FollowupSizeClass::Trivial => "trivial",
            FollowupSizeClass::Small => "small",
            FollowupSizeClass::PlanSized => "plan_sized",
        }
    }
}

/// A follow-up's stored closure state — D1 of the plan above. An OPEN
/// follow-up has none (`resolution IS NULL`), so open is not a member.
///
/// Published as `writeVocabulary.resolution`. Which transitions between these
/// are legal, and how a reopen is spelled, is the web route's contract; this
/// door forwards the body verbatim and refuses nothing itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FollowupResolution {
    Fixing,
    Fixed,
    AlreadyFixed,
    Dismissed,
}

impl FollowupResolution {
    /// Every closure state, in D1's order.
    pub(super) const ALL: [FollowupResolution; 4] = [
        FollowupResolution::Fixing,
        FollowupResolution::Fixed,
        FollowupResolution::AlreadyFixed,
        FollowupResolution::Dismissed,
    ];

    /// The wire string.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            FollowupResolution::Fixing => "fixing",
            FollowupResolution::Fixed => "fixed",
            FollowupResolution::AlreadyFixed => "already_fixed",
            FollowupResolution::Dismissed => "dismissed",
        }
    }
}

/// This family's entries in the parent's `writeVocabulary`, rendered from the
/// two enums above — never retyped.
pub(super) fn write_vocabulary_entries() -> [(&'static str, Value); 2] {
    let size_class: Vec<&str> = FollowupSizeClass::ALL.iter().map(|s| s.as_str()).collect();
    let resolution: Vec<&str> = FollowupResolution::ALL.iter().map(|r| r.as_str()).collect();
    [
        ("size_class", Value::from(size_class)),
        ("resolution", Value::from(resolution)),
    ]
}

/// This family's entries in the parent's `writeCorrection`: per write door,
/// how a WRONG write is corrected — said BEFORE the write, because two of the
/// three answers are "it cannot be, through this door".
pub(super) fn write_correction_entries() -> [(&'static str, Value); 3] {
    [
        (
            EDGE_ANCHORS,
            Value::from(
                "Verb: the PUT REPLACES the follow-up's whole anchor set, so a wrong anchor is \
                 corrected by PUTting the right set. The body is forwarded verbatim; the web \
                 route is the authority on its shape (repo as owner/name, a repo-relative path \
                 with forward slashes, no leading `/` and no `..` segment; lines are hints).",
            ),
        ),
        (
            EDGE_RESOLUTION,
            Value::from(
                "Verb, inside the web route's legal-transition table only: open -> fixing | \
                 already_fixed | dismissed, and fixing -> fixed | open; any other move answers \
                 409 naming the current state. So a follow-up wrongly moved to `fixing` is \
                 corrected by moving it back to open, and a wrong closure is corrected by \
                 REOPENING it: a resting state (`fixed`, `already_fixed`, `dismissed`) may move \
                 back to open, which needs a non-blank `resolution_note` saying why the \
                 closure was wrong. Re-posting the same note from the same artifact returns \
                 that resolved row rather than a new open one. `dismissed` needs a non-blank \
                 `resolution_note`.",
            ),
        ),
        (
            FOLLOWUPS_RECONCILE,
            Value::from(
                "Not a correction target: the reconcile moves only `fixing` follow-ups, by \
                 coord's land verdict for the cited PR (landed -> fixed with the land commit; \
                 closed unlanded -> open again; pending or unknown -> unchanged), and appends \
                 each move to the row's resolution history. Re-running it is safe.",
            ),
        ),
    ]
}

// ===========================================================================
// Answers
// ===========================================================================

/// `code` on the `502` this family answers when the qontinui-web backend did
/// not answer at all (refused, unresolvable, timed out, or broken off). The
/// match proxy's caller reads it as UNKNOWN; see the module docs.
pub const CODE_WEB_UNREACHABLE: &str = "PLAN_LIBRARY_WEB_UNREACHABLE";

/// `code` on the `502` this family answers when the web route answered 2xx
/// with a body that is not JSON — an answer, but not a usable one.
pub const CODE_UPSTREAM_UNPARSEABLE: &str = "PLAN_LIBRARY_UPSTREAM_UNPARSEABLE";

/// `code` on every upstream non-2xx, which is forwarded with its own status.
/// It is what separates the web route's `401` (the runner's device JWT was
/// refused) from this door's own `401` (no proxy nonce,
/// `COORD_MCP_PROXY_UNAUTHORIZED`) — two failures with opposite remedies.
pub const CODE_UPSTREAM_STATUS: &str = "PLAN_LIBRARY_UPSTREAM_STATUS";

/// A failed exchange's answer: the parent's error envelope.
type Refusal = (StatusCode, Json<ApiResponse<()>>);

/// The reads' result: the upstream's own 2xx status with the capability block
/// folded in, or the parent's read failure (which carries the block too).
type ReadAnswer =
    Result<(StatusCode, Json<ApiResponse<Value>>), (StatusCode, Json<ApiResponse<Value>>)>;

/// The writes' result: the upstream's own 2xx status and body, or a refusal.
type WriteAnswer = Result<(StatusCode, Json<ApiResponse<Value>>), Refusal>;

/// Which side of the door an exchange is on — it decides what a transport
/// failure means for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exchange {
    /// Nothing changes upstream; a failure leaves the ANSWER unknown.
    Read,
    /// A failure may leave the OUTCOME unknown, unless the request provably
    /// never left this process.
    Write,
}

/// The `502` for a web backend that did not answer, typed with
/// [`CODE_WEB_UNREACHABLE`]. `maybe_sent` is false only when the request
/// provably never left this process (a refused connection, a URL that could
/// not be formed); a timeout or a broken answer may have been applied.
fn web_unreachable(
    exchange: Exchange,
    upstream_path: &str,
    base: &str,
    base_source: &str,
    error: &str,
    maybe_sent: bool,
) -> Refusal {
    let mut message = super::transport_failure_message(upstream_path, base, base_source, error);
    message.push_str(match (exchange, maybe_sent) {
        (Exchange::Read, _) => {
            ". The answer is UNKNOWN: nothing was read, so this is NOT an empty result and must \
             not be reported as \"no follow-ups\"."
        }
        (Exchange::Write, false) => {
            ". The write was NOT sent: the request never left this runner, so nothing changed \
             upstream."
        }
        (Exchange::Write, true) => {
            ". The write's OUTCOME is UNKNOWN: it may have reached the backend before the \
             failure. Read the follow-up back (GET /plan-library/followups) before retrying."
        }
    });
    super::coded_error(StatusCode::BAD_GATEWAY, CODE_WEB_UNREACHABLE, message)
}

/// The `502` for a 2xx answer that is not JSON, typed with
/// [`CODE_UPSTREAM_UNPARSEABLE`]. On a write the 2xx means it WAS applied, and
/// the text says so, so a caller does not retry a write that landed.
fn upstream_unparseable(
    exchange: Exchange,
    upstream_path: &str,
    status: reqwest::StatusCode,
    error: &serde_json::Error,
) -> Refusal {
    let consequence = match exchange {
        Exchange::Read => "the answer is UNKNOWN",
        Exchange::Write => "the write WAS accepted, but its answer cannot be read",
    };
    super::coded_error(
        StatusCode::BAD_GATEWAY,
        CODE_UPSTREAM_UNPARSEABLE,
        format!(
            "{upstream_path}: upstream answered {status} with a body that is not JSON \
             ({error}); {consequence}"
        ),
    )
}

/// The absolute upstream URL: the web base, `upstream_path`, and the caller's
/// raw query string set VERBATIM. `Url::set_query` percent-encodes only what a
/// query may not contain (a `#` cannot end the query and start a fragment), so
/// every key, value, repeat and order arrives as the caller sent it.
fn upstream_url(
    base: &str,
    upstream_path: &str,
    raw_query: Option<&str>,
) -> Result<reqwest::Url, url::ParseError> {
    let mut url = reqwest::Url::parse(&format!("{base}{upstream_path}"))?;
    if let Some(query) = raw_query.filter(|q| !q.is_empty()) {
        url.set_query(Some(query));
    }
    Ok(url)
}

/// One exchange with the web route: `method` on `upstream_path`, the raw
/// query and body forwarded unchanged, under the runner's device JWT.
///
/// Answers the upstream's own 2xx status and its JSON body (`null` for an
/// empty one); every failure is a typed [`Refusal`].
async fn forward(
    exchange: Exchange,
    method: reqwest::Method,
    upstream_path: &str,
    raw_query: Option<&str>,
    body: Option<Bytes>,
) -> Result<(StatusCode, Value), Refusal> {
    // Resolved ONCE, so the URL dialled and the base a failure names are the
    // same value — the parent's `transport_failure` re-resolves it.
    let (base, arm) = crate::api_config::get_api_base_url_with_source();
    let base = base.trim_end_matches('/');
    let url = upstream_url(base, upstream_path, raw_query).map_err(|e| {
        web_unreachable(
            exchange,
            upstream_path,
            base,
            arm.as_str(),
            &e.to_string(),
            false,
        )
    })?;
    let mut request = super::upstream_client().request(method, url);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    // coord-tenant-scope(work-owed): web_base() is qontinui-web, not coord; the follow-up family forwards with the same session-less posture as the parent's forwards, and web derives organization_id from the device principal. E3: the coord-tenant to web-organization mapping is unresolved.
    let resp = crate::auth::attach_device_auth(request)
        .send()
        .await
        .map_err(|e| {
            web_unreachable(
                exchange,
                upstream_path,
                base,
                arm.as_str(),
                &e.to_string(),
                !(e.is_connect() || e.is_builder()),
            )
        })?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| {
        web_unreachable(
            exchange,
            upstream_path,
            base,
            arm.as_str(),
            &e.to_string(),
            true,
        )
    })?;
    if !status.is_success() {
        let (code, mut answer) = super::upstream_failure(
            upstream_path,
            status,
            String::from_utf8_lossy(&bytes).into_owned(),
        );
        answer.0.code = Some(CODE_UPSTREAM_STATUS.to_string());
        return Err((code, answer));
    }
    let value = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|e| upstream_unparseable(exchange, upstream_path, status, &e))?
    };
    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK),
        value,
    ))
}

// ===========================================================================
// Request plumbing
// ===========================================================================

/// The body to forward: `None` for an empty (or whitespace-only) one, else the
/// caller's bytes UNCHANGED once they parse as JSON. The parse only refuses a
/// malformed body locally; the parsed value is discarded, never re-serialized.
fn json_body(body: &Bytes) -> Result<Option<Bytes>, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice::<serde::de::IgnoredAny>(body)
        .map(|_| Some(body.clone()))
        .map_err(|e| format!("Failed to deserialize the JSON body: {e}"))
}

/// The upstream path for one follow-up edge's `leaf` sub-route, or the 400
/// text for an `id` that is not a UUID. The canonical hyphenated form is what
/// reaches the wire, so no `..`, `/` or `?` can travel in this segment.
fn edge_upstream_path(raw: &str, leaf: &str, route: &str) -> Result<String, String> {
    match uuid::Uuid::parse_str(raw.trim()) {
        Ok(id) => Ok(format!(
            "/api/v1/plan-library/edges/{}/{leaf}",
            id.hyphenated()
        )),
        Err(e) => Err(format!(
            "{route}: `id` must be a follow-up edge UUID (the `edge_id` of a \
             GET /plan-library/followups item or a POST /plan-library/followups/match match), \
             got {raw:?}: {e}"
        )),
    }
}

/// A local `400` on the write side.
fn bad_request(message: String) -> Refusal {
    (StatusCode::BAD_REQUEST, Json(api_error(message)))
}

/// Forward one read and fold the capability block into its answer — on the
/// failure arm too, with `webBackendReachable` saying whether web answered.
async fn read(
    method: reqwest::Method,
    upstream_path: &str,
    raw_query: Option<String>,
    body: Option<Bytes>,
) -> ReadAnswer {
    let (status, upstream) = forward(
        Exchange::Read,
        method,
        upstream_path,
        raw_query.as_deref(),
        body,
    )
    .await
    .map_err(super::read_failure)?;
    Ok((
        status,
        Json(ApiResponse::success(super::with_write_capability(upstream))),
    ))
}

/// Forward one write that has ALREADY passed `authorize_write`, and record who
/// sent it (see the module docs on why the body carries no attribution).
async fn write(
    principal: super::WritePrincipal,
    route: &'static str,
    method: reqwest::Method,
    upstream_path: &str,
    raw_query: Option<String>,
    body: &Bytes,
) -> WriteAnswer {
    let body = json_body(body).map_err(bad_request)?;
    let answer = forward(
        Exchange::Write,
        method,
        upstream_path,
        raw_query.as_deref(),
        body,
    )
    .await;
    let status = match &answer {
        Ok((status, _)) | Err((status, _)) => status.as_u16(),
    };
    tracing::info!(
        route,
        upstream = upstream_path,
        actor = %principal.actor,
        session = principal.session_label.as_deref().unwrap_or("-"),
        status,
        "plan-library follow-up write"
    );
    let (status, upstream) = answer?;
    Ok((status, Json(ApiResponse::success(upstream))))
}

// ===========================================================================
// Handlers
// ===========================================================================

/// `GET /plan-library/followups` — the follow-up queue (oldest first, keyset
/// paged), its filters and `cursor` forwarded verbatim. **Ungated**, and
/// advertises the write layers (on the error arm too).
pub async fn list_followups_handler(RawQuery(query): RawQuery) -> ReadAnswer {
    read(
        reqwest::Method::GET,
        "/api/v1/plan-library/followups",
        query,
        None,
    )
    .await
}

/// `POST /plan-library/followups/match` — the open and `fixing` follow-ups
/// anchored on a `{repo, paths}` set, plus `unanchored_open_total` so a zero
/// match is self-describing. A READ: **ungated**, and advertises the write
/// layers. An unreachable web is a typed `502` ([`CODE_WEB_UNREACHABLE`])
/// that reads as UNKNOWN, never an empty match.
pub async fn match_followups_handler(RawQuery(query): RawQuery, body: Bytes) -> ReadAnswer {
    let body = json_body(&body)
        .map_err(|message| super::read_refused_before_dial(StatusCode::BAD_REQUEST, message))?;
    read(
        reqwest::Method::POST,
        "/api/v1/plan-library/followups/match",
        query,
        body,
    )
    .await
}

/// `PUT /plan-library/edges/{id}/anchors` — replace one follow-up's anchor
/// set. **Nonce-authorized, then kill switch, then dial**; the id and body are
/// examined only after all three pass.
pub async fn replace_anchors_handler(
    headers: HeaderMap,
    Path(edge_id): Path<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> WriteAnswer {
    let principal = super::authorize_write(&headers)?;
    let upstream_path =
        edge_upstream_path(&edge_id, "anchors", EDGE_ANCHORS).map_err(bad_request)?;
    write(
        principal,
        EDGE_ANCHORS,
        reqwest::Method::PUT,
        &upstream_path,
        query,
        &body,
    )
    .await
}

/// `POST /plan-library/edges/{id}/resolution` — move one follow-up through
/// its closure states (`fixing`, `fixed`, `already_fixed`, `dismissed`, or
/// back to open). **Nonce-authorized, then kill switch, then dial**; the web
/// route owns the legal-transition table and answers `409` outside it.
pub async fn transition_followup_handler(
    headers: HeaderMap,
    Path(edge_id): Path<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> WriteAnswer {
    let principal = super::authorize_write(&headers)?;
    let upstream_path =
        edge_upstream_path(&edge_id, "resolution", EDGE_RESOLUTION).map_err(bad_request)?;
    write(
        principal,
        EDGE_RESOLUTION,
        reqwest::Method::POST,
        &upstream_path,
        query,
        &body,
    )
    .await
}

/// `POST /plan-library/followups/reconcile` — move every `fixing` follow-up by
/// coord's land verdict for its cited PR, answering web's counts. A WRITE (it
/// moves rows): **nonce-authorized, then kill switch, then dial**.
pub async fn reconcile_followups_handler(
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> WriteAnswer {
    let principal = super::authorize_write(&headers)?;
    write(
        principal,
        FOLLOWUPS_RECONCILE,
        reqwest::Method::POST,
        "/api/v1/plan-library/followups/reconcile",
        query,
        &body,
    )
    .await
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::plan_library::{
        CODE_DIAL_OFF, CODE_DIAL_UNANSWERED, CODE_PROXY_UNAUTHORIZED, CODE_WRITE_KILLED,
        PLAN_LIBRARY_WRITE_FLAG,
    };
    use axum::body::Body;
    use axum::http::{Method, Request, Uri};
    use qontinui_runner_lib::plan_workunit_adapter::body_push::WEB_BACKEND_URL_ENV;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// Every env var the dialling tests redirect, restored on the way out.
    /// `web_base()` reaches `settings::load_settings()`, a WRITER with two
    /// roots, so both config roots are redirected along with the base — see
    /// the parent's `the_handler_dials_upstream_with_only_the_allowlisted_param`
    /// for why both, and why the roster half is Linux/macOS-only.
    const REDIRECTED: [&str; 4] = [
        PLAN_LIBRARY_WRITE_FLAG,
        WEB_BACKEND_URL_ENV,
        "QONTINUI_CONFIG_DIR",
        "XDG_CONFIG_HOME",
    ];

    /// Point the door at `base` with the config roots under `cfg` and the kill
    /// switch absent. Writes env, so every caller holds `env_lock()` itself.
    fn redirect_env(base: &str, cfg: &std::path::Path) {
        std::env::remove_var(PLAN_LIBRARY_WRITE_FLAG);
        std::env::set_var(WEB_BACKEND_URL_ENV, base);
        std::env::set_var("QONTINUI_CONFIG_DIR", cfg);
        std::env::set_var("XDG_CONFIG_HOME", cfg);
    }

    /// Pin the tenant dial — a child of `env_lock` in the test-lock hierarchy,
    /// so the `env_lock()` each test takes after it nests rather than contends.
    fn pin(level: &str) -> crate::mcp::fleet_policy_poller::PlanCaptureLevelPin {
        crate::mcp::fleet_policy_poller::pin_plan_capture_level_for_test(level)
    }

    /// A nonce this runner registered — a fresh agent binding.
    fn registered_nonce() -> String {
        let wd = format!("D:/plan-library-followups-test-{}", uuid::Uuid::now_v7());
        crate::coord_mcp::register_agent_proxy_nonce(&wd, uuid::Uuid::new_v4())
    }

    /// One request the mock web backend received.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        query: Option<String>,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    /// A stand-in qontinui-web: records every request and answers each with
    /// `status` and `body`. `Connection: close` on every answer, so the
    /// process-wide pooled client never reuses a socket into a mock a finished
    /// test has torn down.
    async fn mock_web(status: StatusCode, body: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let log = Arc::clone(&log);
                async move {
                    log.lock().unwrap().push(Seen {
                        method: method.to_string(),
                        path: uri.path().to_string(),
                        query: uri.query().map(str::to_string),
                        headers,
                        body: bytes.to_vec(),
                    });
                    (
                        status,
                        [
                            (axum::http::header::CONTENT_TYPE, "application/json"),
                            (axum::http::header::CONNECTION, "close"),
                        ],
                        body,
                    )
                }
            },
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), seen)
    }

    /// A base URL nothing listens on: bind an ephemeral port, then free it.
    async fn dead_base() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    /// Drive the family's REAL router over one request.
    async fn call(method: &str, uri: &str, nonce: Option<&str>, body: &str) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(nonce) = nonce {
            builder = builder.header("authorization", format!("Bearer {nonce}"));
        }
        let resp = router::<()>()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    const EDGE: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

    /// The three writes, each with a body its web route would accept.
    fn the_writes() -> [(&'static str, String, &'static str); 3] {
        [
            (
                "PUT",
                format!("/plan-library/edges/{EDGE}/anchors"),
                r#"{"anchors": [{"repo": "owner/repo", "path": "src/lib.rs", "line_start": 7}]}"#,
            ),
            (
                "POST",
                format!("/plan-library/edges/{EDGE}/resolution"),
                r#"{"resolution": "fixing", "resolution_pr": "owner/repo#12"}"#,
            ),
            ("POST", FOLLOWUPS_RECONCILE.to_string(), ""),
        ]
    }

    // ---- the route table ----------------------------------------------------

    /// Five rows, and the gating split is exactly the three-layer model: the
    /// three writes require a nonce, the two reads do not — pinned by
    /// identity, so a route that quietly joins either half fails here.
    #[test]
    fn the_family_table_is_three_gated_writes_and_two_open_reads() {
        assert_eq!(ROUTE_ENTRIES.len(), 5);
        let gated: Vec<(&str, &str)> = ROUTE_ENTRIES
            .iter()
            .filter(|(_, _, nonce)| *nonce)
            .map(|(m, p, _)| (*m, *p))
            .collect();
        assert_eq!(
            gated,
            vec![
                ("PUT", "/plan-library/edges/{id}/anchors"),
                ("POST", "/plan-library/edges/{id}/resolution"),
                ("POST", "/plan-library/followups/reconcile"),
            ]
        );
        let open: Vec<(&str, &str)> = ROUTE_ENTRIES
            .iter()
            .filter(|(_, _, nonce)| !*nonce)
            .map(|(m, p, _)| (*m, *p))
            .collect();
        assert_eq!(
            open,
            vec![
                ("GET", "/plan-library/followups"),
                // A read carried as a POST: its path set rides in a body.
                ("POST", "/plan-library/followups/match"),
            ]
        );
        let _r: Router<Arc<ApiState>> = routes();
    }

    /// The table is in lockstep with the registration: counted off the
    /// `router` FUNCTION BODY rather than trusted, the technique the parent's
    /// `route_entries_covers_every_route_registration_in_this_file` uses.
    #[test]
    fn the_family_router_registers_every_row_once() {
        // Built at runtime so this test's own source cannot match itself.
        let needle = format!(".{}(", "route");
        let header = format!("fn {}<S>() -> Router<S>", "router");
        let src = include_str!("followups.rs");
        let start = src
            .find(header.as_str())
            .expect("router()'s signature — update this test if it changes");
        let body = src
            .get(start..)
            .expect("a char boundary: `find` returned it");
        let end = body
            .find("\n}")
            .expect("router() must be closed by a brace at column 0");
        let registered = body
            .get(..end)
            .expect("a char boundary: `find` returned it")
            .matches(needle.as_str())
            .count();
        let mut paths: Vec<&str> = ROUTE_ENTRIES.iter().map(|(_, p, _)| *p).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), ROUTE_ENTRIES.len(), "one method per path here");
        assert_eq!(
            registered,
            paths.len(),
            "router() registers {registered} routes but ROUTE_ENTRIES lists {} paths",
            paths.len()
        );
    }

    /// The two vocabularies are pinned against LITERAL copies of the plan's D1
    /// (`resolution`) and D2 (`size_class`), the contract qontinui-web's
    /// follow-up columns implement — so an edit to either enum that does not
    /// update these literals goes red. Like the parent's
    /// `closed_vocabularies_match_the_web_literals`, a widening on the WEB side
    /// is not caught here; re-establish parity by hand when web's set changes.
    #[test]
    fn the_vocabularies_are_the_plans_closed_sets() {
        let entries: std::collections::BTreeMap<&str, Value> =
            write_vocabulary_entries().into_iter().collect();
        assert_eq!(
            entries["size_class"],
            serde_json::json!(["trivial", "small", "plan_sized"])
        );
        assert_eq!(
            entries["resolution"],
            serde_json::json!(["fixing", "fixed", "already_fixed", "dismissed"])
        );
    }

    /// Each write door says how a wrong write is corrected: a wrong closure by
    /// REOPENING it (with a reason), and the reconcile, which is not a
    /// correction target at all.
    #[test]
    fn every_write_door_states_its_correction_posture() {
        let entries: std::collections::BTreeMap<&str, Value> =
            write_correction_entries().into_iter().collect();
        let gated: std::collections::BTreeSet<&str> = ROUTE_ENTRIES
            .iter()
            .filter(|(_, _, nonce)| *nonce)
            .map(|(_, p, _)| *p)
            .collect();
        assert_eq!(
            entries
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            gated,
            "one correction entry per write door, and none for a read"
        );
        let resolution = entries[EDGE_RESOLUTION].as_str().unwrap();
        for claim in [
            "409",
            "REOPENING",
            "back to open",
            "dismissed",
            "resolution_note",
        ] {
            assert!(resolution.contains(claim), "omits `{claim}`: {resolution}");
        }
        assert!(entries[EDGE_ANCHORS].as_str().unwrap().contains("REPLACES"));
        assert!(entries[FOLLOWUPS_RECONCILE]
            .as_str()
            .unwrap()
            .contains("Not a correction target"));
    }

    /// What a READ publishes: the two vocabularies inside `writeVocabulary`
    /// and the three correction postures inside `writeCorrection` — asserted
    /// on the folded block a read answers with, not on the helpers, so a fold
    /// that drops either fails here.
    #[test]
    fn a_read_publishes_the_familys_vocabulary_and_correction_postures() {
        let _pin = pin("off");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[PLAN_LIBRARY_WRITE_FLAG]);
        std::env::remove_var(PLAN_LIBRARY_WRITE_FLAG);
        let block = super::super::with_write_capability(serde_json::json!({"items": []}));
        assert_eq!(
            block["writeVocabulary"]["size_class"],
            serde_json::json!(["trivial", "small", "plan_sized"])
        );
        assert_eq!(
            block["writeVocabulary"]["resolution"],
            serde_json::json!(["fixing", "fixed", "already_fixed", "dismissed"])
        );
        for door in [EDGE_ANCHORS, EDGE_RESOLUTION, FOLLOWUPS_RECONCILE] {
            assert!(
                block["writeCorrection"][door].is_string(),
                "writeCorrection omits {door}: {block}"
            );
        }
        // The instruction names every write, the family's three included.
        let instruction = block["writeInstruction"].as_str().unwrap();
        for (method, path, requires_a_nonce) in ROUTE_ENTRIES {
            if requires_a_nonce {
                assert!(
                    instruction.contains(&format!("{method} {path}")),
                    "writeInstruction omits {method} {path}: {instruction}"
                );
            }
        }
    }

    /// The parent's `writeContract` names this family's typed 502 by its
    /// literal string (a `const` cannot splice another module's `const`), so
    /// the two are pinned together here.
    #[test]
    fn the_write_contract_names_the_typed_502_and_the_family_routes() {
        let contract = super::super::WRITE_CONTRACT;
        assert!(contract.contains(CODE_WEB_UNREACHABLE), "{contract}");
        for (_, path, _) in ROUTE_ENTRIES {
            assert!(contract.contains(path), "writeContract omits {path}");
        }
        assert!(contract.contains("writeVocabulary.size_class"));
        assert!(contract.contains("writeVocabulary.resolution"));
        assert!(contract.contains("VERBATIM"));
    }

    // ---- pure pieces ----------------------------------------------------------

    /// The query is set verbatim — order, repeats and encoding — and a `#` in
    /// it cannot start a fragment and cut the query short.
    #[test]
    fn the_upstream_url_carries_the_raw_query_verbatim() {
        let url = upstream_url(
            "http://127.0.0.1:8000",
            "/api/v1/plan-library/followups",
            Some("state=fixing&state=open&repo=owner%2Frepo&limit=5"),
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:8000/api/v1/plan-library/followups\
             ?state=fixing&state=open&repo=owner%2Frepo&limit=5"
        );
        let hashed = upstream_url("http://127.0.0.1:8000", "/x", Some("q=a#b")).unwrap();
        assert_eq!(hashed.fragment(), None, "{hashed}");
        assert_eq!(hashed.query(), Some("q=a%23b"), "{hashed}");
        let bare = upstream_url("http://127.0.0.1:8000", "/x", Some("")).unwrap();
        assert_eq!(bare.as_str(), "http://127.0.0.1:8000/x", "no dangling `?`");
    }

    /// An empty body forwards as none; a JSON body forwards as the SAME bytes;
    /// anything else is refused with the parse error.
    #[test]
    fn a_body_is_forwarded_as_its_own_bytes_or_refused() {
        assert_eq!(json_body(&Bytes::from_static(b"")).unwrap(), None);
        assert_eq!(json_body(&Bytes::from_static(b" \n\t")).unwrap(), None);
        let odd = Bytes::from_static(b"{ \"z\": 1.50,\n  \"a\" : [ ] }");
        assert_eq!(json_body(&odd).unwrap().as_deref(), Some(&odd[..]));
        for bad in ["not json at all", "{", "{} trailing"] {
            let err = json_body(&Bytes::from(bad)).unwrap_err();
            assert!(
                err.contains("Failed to deserialize the JSON body"),
                "{bad}: {err}"
            );
        }
    }

    /// The edge id is a canonical UUID on the wire, or a 400 naming the route.
    #[test]
    fn the_edge_id_is_a_canonical_uuid_or_a_400() {
        assert_eq!(
            edge_upstream_path(
                "{3F2504E0-4F89-11D3-9A0C-0305E82C3301}",
                "anchors",
                EDGE_ANCHORS
            )
            .unwrap(),
            format!("/api/v1/plan-library/edges/{EDGE}/anchors")
        );
        for bad in ["not-a-uuid", "../followups", "a?b=c", ""] {
            let err = edge_upstream_path(bad, "resolution", EDGE_RESOLUTION).unwrap_err();
            assert!(
                err.contains("must be a follow-up edge UUID"),
                "{bad}: {err}"
            );
            assert!(err.contains(EDGE_RESOLUTION), "{bad}: {err}");
        }
    }

    /// A transport failure says what it means for the caller: a read's answer
    /// is UNKNOWN, a write that never left was NOT sent, and one that may have
    /// left has an UNKNOWN outcome to read back.
    #[test]
    fn a_transport_failure_says_what_the_caller_may_conclude() {
        let text = |exchange, maybe_sent| {
            let (status, body) = web_unreachable(
                exchange,
                "/api/v1/plan-library/followups/match",
                "http://127.0.0.1:1",
                "env:QONTINUI_WEB_BACKEND_URL",
                "connection refused",
                maybe_sent,
            );
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(body.0.code.as_deref(), Some(CODE_WEB_UNREACHABLE));
            body.0.error.unwrap()
        };
        let read = text(Exchange::Read, true);
        assert!(read.contains("UNKNOWN"), "{read}");
        assert!(read.contains("NOT an empty result"), "{read}");
        assert!(
            read.contains("http://127.0.0.1:1"),
            "names the base: {read}"
        );
        assert!(
            read.contains("env:QONTINUI_WEB_BACKEND_URL"),
            "names the base's source: {read}"
        );
        let unsent = text(Exchange::Write, false);
        assert!(unsent.contains("NOT sent"), "{unsent}");
        let maybe = text(Exchange::Write, true);
        assert!(maybe.contains("OUTCOME is UNKNOWN"), "{maybe}");
        assert!(maybe.contains("Read the follow-up back"), "{maybe}");
    }

    // ---- through the router: the write gate ---------------------------------

    /// Layer 1: no nonce ⇒ 401 on every write, EVEN on a malformed body and a
    /// malformed id — the gate runs before either is looked at — and the web
    /// backend is never dialled.
    #[tokio::test]
    async fn each_write_401s_without_a_nonce_even_on_a_malformed_body() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::OK, "{}").await;
        redirect_env(&base, cfg.path());

        let writes = [
            ("PUT", format!("/plan-library/edges/{EDGE}/anchors")),
            ("POST", format!("/plan-library/edges/{EDGE}/resolution")),
            ("POST", FOLLOWUPS_RECONCILE.to_string()),
            ("PUT", "/plan-library/edges/not-a-uuid/anchors".to_string()),
            (
                "POST",
                "/plan-library/edges/not-a-uuid/resolution".to_string(),
            ),
        ];
        for (method, uri) in &writes {
            for body in ["", "{}", "not json at all", r#"{"resolution": "#] {
                let (status, json) = call(method, uri, None, body).await;
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} with body {body:?}: {json}"
                );
                assert_eq!(json["code"], serde_json::json!(CODE_PROXY_UNAUTHORIZED));
            }
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "a refused write must never reach the web backend"
        );
    }

    /// Layer 2: a registered nonce meets the machine kill switch.
    #[tokio::test]
    async fn each_write_refuses_on_the_kill_switch() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::OK, "{}").await;
        redirect_env(&base, cfg.path());
        std::env::set_var(PLAN_LIBRARY_WRITE_FLAG, "0");
        let nonce = registered_nonce();

        for (method, uri, body) in the_writes() {
            let (status, json) = call(method, &uri, Some(&nonce), body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {json}");
            assert_eq!(json["code"], serde_json::json!(CODE_WRITE_KILLED));
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Layer 3, cold start: coord has not answered this runner's plan_capture
    /// poll, so the level's `record` default does not open the door.
    #[tokio::test]
    async fn each_write_refuses_on_an_unanswered_dial() {
        let pin = pin("record");
        pin.set_unanswered();
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::OK, "{}").await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        for (method, uri, body) in the_writes() {
            let (status, json) = call(method, &uri, Some(&nonce), body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {json}");
            assert_eq!(json["code"], serde_json::json!(CODE_DIAL_UNANSWERED));
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Layer 3: the tenant's dial reads `off`.
    #[tokio::test]
    async fn each_write_refuses_on_dial_off() {
        let _pin = pin("off");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::OK, "{}").await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        for (method, uri, body) in the_writes() {
            let (status, json) = call(method, &uri, Some(&nonce), body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {json}");
            assert_eq!(json["code"], serde_json::json!(CODE_DIAL_OFF));
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Past the gate, a malformed body or id is a LOCAL 400 — never forwarded.
    /// On the match read, the same refusal carries the capability block with
    /// `webBackendReachable: null`: nothing was dialled, so nothing is known.
    #[tokio::test]
    async fn past_the_gate_a_malformed_request_is_a_local_400_and_never_dials() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::OK, "{}").await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        for (method, uri, _) in the_writes() {
            let (status, json) = call(method, &uri, Some(&nonce), "not json").await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {json}");
            assert!(json["error"]
                .as_str()
                .unwrap()
                .contains("Failed to deserialize the JSON body"));
        }
        for uri in [
            "/plan-library/edges/not-a-uuid/anchors",
            "/plan-library/edges/not-a-uuid/resolution",
        ] {
            let method = if uri.ends_with("anchors") {
                "PUT"
            } else {
                "POST"
            };
            let (status, json) = call(method, uri, Some(&nonce), "{}").await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {json}");
            assert!(json["error"]
                .as_str()
                .unwrap()
                .contains("must be a follow-up edge UUID"));
        }
        let (status, json) = call("POST", FOLLOWUPS_MATCH, None, "{\"repo\": ").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["data"]["webBackendReachable"], Value::Null, "{json}");
        assert_eq!(json["data"]["writeRequiresNonce"], serde_json::json!(true));
        assert!(
            seen.lock().unwrap().is_empty(),
            "nothing malformed may reach the web backend"
        );
    }

    // ---- through the router: verbatim forwarding ------------------------------

    /// Every proxy sends its path, query and body to the web route VERBATIM and
    /// answers with the web route's own 2xx status: a `201` stays a `201`. The
    /// reads fold the capability block beside the upstream keys; the caller's
    /// proxy nonce never travels upstream.
    #[tokio::test]
    async fn each_proxy_forwards_path_query_body_and_status_verbatim() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(StatusCode::CREATED, r#"{"echo": "upstream"}"#).await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        // Odd spacing and key order on purpose: a re-serialization would
        // change these bytes, so arriving unchanged proves none happened.
        let match_body = "{ \"repo\" : \"owner/repo\",\n  \"paths\": [\"b/c.rs\", \"a.py\"] }";
        let anchors_body = "{\"anchors\":[{\"path\":\"src/x.rs\",\"repo\":\"owner/repo\"}],  \
                            \"size_class\":\"trivial\"}";
        let resolution_body = "{\"resolution\": \"fixing\", \"resolution_pr\": \"owner/repo#12\"}";
        let cases: [(&str, &str, Option<&str>, &str, &str, Option<&str>, bool); 5] = [
            (
                "GET",
                "/plan-library/followups?state=fixing&state=open&repo=owner%2Frepo&limit=5",
                None,
                "",
                "/api/v1/plan-library/followups",
                Some("state=fixing&state=open&repo=owner%2Frepo&limit=5"),
                true,
            ),
            (
                "POST",
                "/plan-library/followups/match",
                None,
                match_body,
                "/api/v1/plan-library/followups/match",
                None,
                true,
            ),
            (
                // Upper case on purpose: the id reaches the wire canonical.
                "PUT",
                "/plan-library/edges/3F2504E0-4F89-11D3-9A0C-0305E82C3301/anchors?dry_run=1",
                Some(&nonce),
                anchors_body,
                "/api/v1/plan-library/edges/3f2504e0-4f89-11d3-9a0c-0305e82c3301/anchors",
                Some("dry_run=1"),
                false,
            ),
            (
                "POST",
                "/plan-library/edges/3f2504e0-4f89-11d3-9a0c-0305e82c3301/resolution",
                Some(&nonce),
                resolution_body,
                "/api/v1/plan-library/edges/3f2504e0-4f89-11d3-9a0c-0305e82c3301/resolution",
                None,
                false,
            ),
            (
                "POST",
                "/plan-library/followups/reconcile",
                Some(&nonce),
                "",
                "/api/v1/plan-library/followups/reconcile",
                None,
                false,
            ),
        ];

        for (i, (method, uri, auth, body, upstream_path, upstream_query, is_read)) in
            cases.into_iter().enumerate()
        {
            let (status, json) = call(method, uri, auth, body).await;
            assert_eq!(
                status,
                StatusCode::CREATED,
                "{method} {uri}: the upstream's own 2xx status: {json}"
            );
            assert_eq!(json["success"], serde_json::json!(true), "{json}");
            assert_eq!(
                json["data"]["echo"],
                serde_json::json!("upstream"),
                "{json}"
            );
            assert_eq!(
                json["data"].get("writeRequiresNonce").is_some(),
                is_read,
                "{method} {uri}: the capability block rides on the reads only: {json}"
            );
            if is_read {
                assert_eq!(json["data"]["webBackendReachable"], serde_json::json!(true));
            }

            let got = seen.lock().unwrap().get(i).cloned().expect("dialled once");
            assert_eq!(got.method, method, "{uri}");
            assert_eq!(got.path, upstream_path, "{uri}");
            assert_eq!(got.query.as_deref(), upstream_query, "{uri}");
            assert_eq!(
                got.body,
                body.as_bytes(),
                "{uri}: the body must arrive byte for byte"
            );
            let content_type = got
                .headers
                .get("content-type")
                .map(|v| v.to_str().unwrap().to_string());
            if body.is_empty() {
                assert_eq!(content_type, None, "{uri}: no body, no content type");
            } else {
                assert_eq!(content_type.as_deref(), Some("application/json"), "{uri}");
            }
            // The nonce authorizes the LOOPBACK call; it is never a credential
            // for the web backend, and must not be handed to it.
            let leaked = got.headers.iter().any(|(_, v)| {
                v.to_str()
                    .map(|s| s.contains(nonce.as_str()))
                    .unwrap_or(false)
            });
            assert!(!leaked, "{uri}: the caller's nonce reached the web backend");
        }
        assert_eq!(seen.lock().unwrap().len(), 5, "one dial per call, no more");
    }

    /// An upstream non-2xx is forwarded with its OWN status and body, tagged
    /// `PLAN_LIBRARY_UPSTREAM_STATUS` — so the web route's 409 or 401 is never
    /// mistaken for this door's own refusal. A read's failure still carries
    /// the capability block, with `webBackendReachable: true`: web answered.
    #[tokio::test]
    async fn an_upstream_refusal_is_forwarded_with_its_own_status_and_body() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, seen) = mock_web(
            StatusCode::CONFLICT,
            r#"{"error": "illegal_transition", "current_state": "fixed"}"#,
        )
        .await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        let mut calls: Vec<(&str, String, Option<&str>, &str, bool)> = vec![
            ("GET", FOLLOWUPS.to_string(), None, "", true),
            (
                "POST",
                FOLLOWUPS_MATCH.to_string(),
                None,
                r#"{"repo": "owner/repo", "paths": ["a.py"]}"#,
                true,
            ),
        ];
        for (method, uri, body) in the_writes() {
            calls.push((method, uri, Some(nonce.as_str()), body, false));
        }
        for (method, uri, auth, body, is_read) in &calls {
            let (status, json) = call(method, uri, *auth, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {json}");
            assert_eq!(json["success"], serde_json::json!(false));
            assert_eq!(json["code"], serde_json::json!(CODE_UPSTREAM_STATUS));
            let err = json["error"].as_str().unwrap();
            assert!(
                err.contains("current_state"),
                "upstream body verbatim: {err}"
            );
            assert!(err.contains("409"), "{err}");
            if *is_read {
                assert_eq!(
                    json["data"]["webBackendReachable"],
                    serde_json::json!(true),
                    "{method} {uri}: the backend answered, even if it said no"
                );
            }
        }
        assert_eq!(seen.lock().unwrap().len(), calls.len());
    }

    /// An empty 2xx answer reads as `null`, not as a 502 — a route that
    /// answers `204` has said everything it means to.
    #[tokio::test]
    async fn an_empty_2xx_answer_is_null_not_a_failure() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let (base, _seen) = mock_web(StatusCode::NO_CONTENT, "").await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        let (status, json) = call("POST", FOLLOWUPS_RECONCILE, Some(&nonce), "").await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
    }

    /// The contract `/implement-plan` Step 4.3 is built on: a web backend that
    /// does not answer is a TYPED 502 — `PLAN_LIBRARY_WEB_UNREACHABLE`, the
    /// text saying UNKNOWN, `webBackendReachable: false` — never a 200 with an
    /// empty match a caller could read as "no follow-ups on these files".
    #[tokio::test]
    async fn the_match_proxy_maps_an_unreachable_web_to_a_typed_502() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let base = dead_base().await;
        redirect_env(&base, cfg.path());

        let (status, json) = call(
            "POST",
            FOLLOWUPS_MATCH,
            None,
            r#"{"repo": "owner/repo", "paths": ["src/lib.rs"]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
        assert_eq!(json["success"], serde_json::json!(false));
        assert_eq!(
            json["code"],
            serde_json::json!(CODE_WEB_UNREACHABLE),
            "{json}"
        );
        let err = json["error"].as_str().unwrap();
        assert!(err.contains("UNKNOWN"), "{err}");
        assert!(err.contains(&base), "names the base it dialled: {err}");
        assert_eq!(
            json["data"]["webBackendReachable"],
            serde_json::json!(false)
        );
        assert_eq!(json["data"]["writeRequiresNonce"], serde_json::json!(true));
        assert!(
            json["data"].get("matches").is_none(),
            "an unanswered match carries no match list at all: {json}"
        );

        // The list read fails the same typed way.
        let (status, json) = call("GET", FOLLOWUPS, None, "").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
        assert_eq!(json["code"], serde_json::json!(CODE_WEB_UNREACHABLE));
    }

    /// A write whose connection is refused was never sent, and says so —
    /// the same typed 502, with the consequence a retry decision needs.
    #[tokio::test]
    async fn a_write_to_an_unreachable_web_says_it_was_not_sent() {
        let _pin = pin("record");
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&REDIRECTED);
        let cfg = tempfile::tempdir().unwrap();
        let base = dead_base().await;
        redirect_env(&base, cfg.path());
        let nonce = registered_nonce();

        let (status, json) = call(
            "POST",
            &format!("/plan-library/edges/{EDGE}/resolution"),
            Some(&nonce),
            r#"{"resolution": "fixing", "resolution_pr": "owner/repo#12"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
        assert_eq!(json["code"], serde_json::json!(CODE_WEB_UNREACHABLE));
        assert!(
            json["error"].as_str().unwrap().contains("NOT sent"),
            "{json}"
        );
    }
}
