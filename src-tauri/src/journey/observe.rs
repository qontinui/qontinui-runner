//! `POST /apps/{app_id}/journey/observe` — passive real-session capture (plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`,
//! Phase 3).
//!
//! An embedding app's UI Bridge provider relays each interaction a HUMAN takes
//! (ui-bridge's `InteractionInterceptor`) here. The route feeds the Phase 1
//! producer exactly as an agent action does — a pending edge from the node
//! the cursor last resolved, `run_kind = passive_session` — and then asks the
//! app for ONE unfiltered snapshot through the same snapshot path agents use,
//! which closes that edge. One node-identity source, as for every other edge.
//!
//! ## Typed text is never recorded — structurally
//!
//! The body is a CLOSED shape ([`PassiveInteraction`], `deny_unknown_fields`):
//! an action TYPE and a target element ID, plus an optional tab, session and
//! app version. There is no field for a value, so a relay that sends one
//! (`value`, `text`, `enteredValue`, …) is refused 422 and counted on
//! `/journey/health` — never stored, and never echoed into a log. The element
//! id resolves to the journey fingerprint, which already leaves an input's
//! text out (`node::journey_element_fingerprint`); the id itself is not
//! stored. The only free text that IS stored is `sessionId` (as `run_id`) and
//! `appVersion`, and both are held to an identifier alphabet.
//!
//! ## Trust
//!
//! The route reads no principal: the origin guard decides. The runner's own
//! webview (FirstParty), non-browser callers and Trusted origins
//! (`origin_guard::TRUSTED_ROUTES`) reach it. A Foreign page and an extension
//! page are refused 403 under EVERY route policy
//! (`origin_guard::LEDGER_WRITE_ROUTES`), so no page can forge an edge — and a
//! closing edge retires a frontier row. That is also why the body's content
//! type is not checked: the `text/plain` POST a page can send without a
//! preflight never gets here. Only the guard's kill switch
//! (`QONTINUI_RUNNER_ORIGIN_GUARD=0`) lifts this. Capture is ON whenever a page
//! relays — there is no flag — and its state is `passiveCapture` on
//! `/journey/health`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use qontinui_types::journey::{ChokePoint, NavigationTriggerKind};
use serde::Deserialize;
use serde_json::json;

use crate::mcp::types::ApiState;

use super::capture;
use super::cursor::{ActionSpec, CursorKey};
use super::health;

/// The recorder's `InteractionActionType` — the closed set the route accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassiveActionType {
    Click,
    Type,
    Select,
    Submit,
    Navigate,
    Check,
    Uncheck,
}

impl PassiveActionType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Type => "type",
            Self::Select => "select",
            Self::Submit => "submit",
            Self::Navigate => "navigate",
            Self::Check => "check",
            Self::Uncheck => "uncheck",
        }
    }
}

/// One relayed interaction. CLOSED: there is no slot for what was typed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PassiveInteraction {
    pub action_type: PassiveActionType,
    /// The registry id of the element acted on (not stored — it resolves to
    /// the journey fingerprint). `None` for a `navigate`.
    #[serde(default)]
    pub target_element_id: Option<String>,
    /// The relay tab the page is (the cursor scope).
    #[serde(default)]
    pub tab_id: Option<String>,
    /// The page's recording / relay session — stored as the edge's `run_id`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The app's self-reported version (U4) — stored as `app_version`.
    #[serde(default)]
    pub app_version: Option<String>,
}

/// Longest identifier accepted in a stored field.
pub(crate) const MAX_ID_LEN: usize = 128;

/// An identifier: 1..=[`MAX_ID_LEN`] of `[A-Za-z0-9._:+-]`. Holds the two
/// stored free-text fields to what an id or a version looks like, so neither
/// can carry a sentence a page put there.
pub(crate) fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '+' | '-'))
}

/// A refused body: the status and a fixed phrase.
pub(crate) type BodyRefusal = (StatusCode, &'static str);

/// Why a body was refused — a fixed phrase, never the body or serde's
/// message (which can quote a value) — and with which status: the split
/// axum's `Json` extractor makes on the runner's other JSON routes, and the
/// explore route makes too. A body that is not JSON at all (a syntax error,
/// a truncated body) is a 400; JSON of the wrong shape is a 422.
fn refusal_reason(e: &serde_json::Error) -> BodyRefusal {
    match e.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            (StatusCode::BAD_REQUEST, "body is not JSON")
        }
        serde_json::error::Category::Data => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "body is not the closed observe shape (an unknown field such as a value, or an \
             unknown action type)",
        ),
        // Unreachable over a buffered body (axum maps it to its syntax
        // rejection, a 400, for the same reason).
        serde_json::error::Category::Io => (StatusCode::BAD_REQUEST, "body could not be read"),
    }
}

/// Parse and validate a relayed interaction into the action it records.
pub(crate) fn parse_interaction(
    body: &[u8],
) -> Result<(PassiveInteraction, ActionSpec), BodyRefusal> {
    let unprocessable = |why| (StatusCode::UNPROCESSABLE_ENTITY, why);
    let i: PassiveInteraction = serde_json::from_slice(body).map_err(|e| refusal_reason(&e))?;
    for stored in [&i.session_id, &i.app_version, &i.tab_id]
        .into_iter()
        .flatten()
    {
        if !is_identifier(stored) {
            return Err(unprocessable(
                "sessionId / appVersion / tabId must be an identifier ([A-Za-z0-9._:+-], ≤128)",
            ));
        }
    }
    let target = i
        .target_element_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let action = match (i.action_type, target) {
        (PassiveActionType::Navigate, _) => {
            ActionSpec::navigation("navigate", NavigationTriggerKind::Push)
        }
        (t, Some(id)) => ActionSpec::element(id, t.as_str(), ChokePoint::ElementAction),
        (_, None) => {
            return Err(unprocessable(
                "targetElementId is required for every action but navigate",
            ))
        }
    };
    Ok((i, action))
}

/// How long after an interaction the closing snapshot is taken: the
/// interceptor fires in the capture phase, BEFORE the app's own handlers
/// run, so the page needs a moment to become the result.
pub(crate) const PASSIVE_SETTLE: Duration = Duration::from_millis(500);

/// Most cursors the generation map tracks. Past it the least recently
/// touched cursor is evicted — at worst its pending snapshot task then skips
/// (it is no longer "latest"), and that edge closes `to_node_unobserved`.
pub(crate) const MAX_TRACKED_CURSORS: usize = 512;

/// Latest interaction per cursor: `(generation, last touched)`. A snapshot
/// task whose interaction has been superseded skips its snapshot: the newer
/// interaction already displaced that pending edge (`to_node_unobserved`),
/// and its own snapshot closes the newer one. Generations come from one
/// process-wide counter, so an evicted-and-recreated cursor never reuses one.
#[derive(Debug, Default)]
pub(crate) struct Generations {
    next: u64,
    map: HashMap<CursorKey, (u64, std::time::Instant)>,
}

impl Generations {
    pub(crate) fn bump(&mut self, key: &CursorKey, now: std::time::Instant) -> u64 {
        if !self.map.contains_key(key) && self.map.len() >= MAX_TRACKED_CURSORS {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
        self.next += 1;
        self.map.insert(key.clone(), (self.next, now));
        self.next
    }

    pub(crate) fn is_latest(&self, key: &CursorKey, generation: u64) -> bool {
        self.map.get(key).map(|(g, _)| *g) == Some(generation)
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

fn generations() -> &'static Mutex<Generations> {
    static GEN: OnceLock<Mutex<Generations>> = OnceLock::new();
    GEN.get_or_init(|| Mutex::new(Generations::default()))
}

fn bump(key: &CursorKey) -> u64 {
    generations()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .bump(key, std::time::Instant::now())
}

fn is_latest(key: &CursorKey, generation: u64) -> bool {
    generations()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_latest(key, generation)
}

/// Take the closing snapshot through the SDK snapshot route, pinned to the
/// app (and tab) the interaction came from. The route itself hands it to the
/// producer — inside [`capture::as_passive`], so the snapshot is held off the
/// queue's reserve exactly as the relayed action was, and a refusal is
/// counted on `/journey/health`. A failure or a refusal leaves the pending
/// edge to close as `to_node_unobserved` (by the next event or the TTL
/// sweep).
async fn close_with_snapshot(state: Arc<ApiState>, key: CursorKey, generation: u64) {
    let _ = close_with_snapshot_via(key, generation, PASSIVE_SETTLE, |query| async move {
        crate::mcp::sdk_client::handle_snapshot(State(state), Query(query))
            .await
            .into_response()
    })
    .await;
}

/// The body of [`close_with_snapshot`] over the snapshot call `snap`, so a
/// test drives it: after `settle`, unless the interaction was superseded
/// (`None`, `snap` not called), run `snap` with the query that pins the app
/// (and tab) INSIDE [`capture::as_passive`], and return what it returned.
async fn close_with_snapshot_via<F, Fut, T>(
    key: CursorKey,
    generation: u64,
    settle: Duration,
    snap: F,
) -> Option<T>
where
    F: FnOnce(HashMap<String, String>) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    tokio::time::sleep(settle).await;
    if !is_latest(&key, generation) {
        return None;
    }
    // Always NAMED: the route admits no runner-app relay ([`observe_app_id`]),
    // so the snapshot is pinned to the app — and tab — the action was keyed by.
    let mut query = HashMap::new();
    query.insert("app_id".to_string(), key.app_id.clone());
    if let Some(tab) = &key.scope {
        query.insert("tabId".to_string(), tab.clone());
    }
    Some(capture::as_passive(snap(query)).await)
}

/// The app id a relay may be recorded under, or why not (a 422).
///
/// - An identifier ([`is_identifier`]).
/// - Not the runner's OWN webview (compared case-insensitively). Its closing
///   snapshot cannot be pinned to the relaying cursor: the runner app is not
///   addressed by `?app_id=`, so an unnamed snapshot reaches whatever SDK app
///   is active (or the runner's IPC view, under no tab scope) while the
///   action was keyed by its `tabId`. The edge would only ever close as
///   `to_node_unobserved` on a cursor no snapshot reaches — so it is refused
///   outright, never recorded on the wrong cursor.
pub(crate) fn observe_app_id(app_id: &str) -> Result<&str, &'static str> {
    if !is_identifier(app_id) {
        return Err("app id must be an identifier ([A-Za-z0-9._:+-], ≤128)");
    }
    if app_id.eq_ignore_ascii_case(crate::spec_api::storage::RUNNER_APP_ID) {
        return Err(
            "passive capture does not record the runner's own webview: its closing snapshot \
             cannot be pinned to the relaying cursor",
        );
    }
    Ok(app_id)
}

/// Which app a refusal is counted against: the path's app id when it is an
/// identifier (the runner's own id included — refused by rule, but still an
/// app), else none (unattributed).
pub(crate) fn refusal_target(app_id: &str) -> Option<&str> {
    is_identifier(app_id).then_some(app_id)
}

fn refuse(status: StatusCode, app_id: Option<&str>, reason: &'static str) -> Response {
    health::record_passive_refused(app_id, reason.to_string());
    (status, Json(json!({ "error": reason }))).into_response()
}

/// `POST /apps/{app_id}/journey/observe`.
///
/// - 202: recorded;
/// - 403 (the origin guard, before this handler): a Foreign or Extension
///   origin, under every route policy (see the module's Trust section);
/// - 400: the body is not JSON at all (as `POST …/journey/explore`, and as
///   axum's `Json` extractor answers on the runner's other JSON routes);
/// - 422: the app id is not an identifier or names the runner's own app
///   ([`observe_app_id`]) — checked exactly as given, untrimmed, as the
///   explore route checks it —, or the JSON body is not the closed shape;
/// - 429: no room — the journey queue is down to the slots passive capture
///   leaves to agents and the explorer (`capture::PASSIVE_QUEUE_RESERVE`), or
///   the per-app counter map is full (`health::MAX_PASSIVE_APPS`);
/// - 503: the journey producer is not running (nothing was recorded).
///
/// Every refusal is counted on `/journey/health`.
pub async fn post_observe(
    State(state): State<Arc<ApiState>>,
    Path(app_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(reason) = observe_app_id(&app_id) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            refusal_target(&app_id),
            reason,
        );
    }
    // A refusal never creates an accepted-relay counter slot
    // (`record_passive_refused` counts it in the app's slot, else in the
    // separately capped refused-only map): only an accepted body takes one,
    // so a stream of junk ids cannot fill that map and lock real apps out.
    let (interaction, action) = match parse_interaction(&body) {
        Ok(p) => p,
        Err((status, reason)) => return refuse(status, Some(&app_id), reason),
    };
    // The app's counter slot is CLAIMED here, under one lock, before the
    // event is queued: another app cannot take the last slot between this
    // check and the receipt below.
    if !health::claim_passive_slot(&app_id) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            Some(&app_id),
            "passive capture tracks no more apps in this process",
        );
    }
    let key = CursorKey::new(app_id.clone(), interaction.tab_id.as_deref());
    let queued = capture::record_passive(
        state.app_state.pg_db.clone(),
        key.clone(),
        action,
        interaction.app_version,
        interaction.session_id,
    );
    match queued {
        Ok(()) => {}
        Err(capture::NotQueued::Closed) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                Some(&app_id),
                "the journey producer is not running; nothing was recorded",
            )
        }
        Err(capture::NotQueued::Reserve | capture::NotQueued::Full) => {
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                Some(&app_id),
                "the journey queue is down to its reserve for agent and explorer events",
            )
        }
    }
    health::record_passive_received(&app_id);
    let generation = bump(&key);
    tokio::spawn(close_with_snapshot(Arc::clone(&state), key, generation));
    (StatusCode::ACCEPTED, Json(json!({ "recorded": true }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::cursor::{resolve_trigger, TriggerTarget};
    use crate::journey::node::extract_affordances;

    const SENTINEL: &str = "s3ntinel-typed-secret";

    #[test]
    fn a_body_carrying_a_value_is_refused_whatever_the_key() {
        for key in ["value", "text", "enteredValue", "typed", "innerText"] {
            let body = json!({
                "actionType": "type",
                "targetElementId": "search-box",
                key: SENTINEL,
            });
            let err = parse_interaction(body.to_string().as_bytes()).expect_err(key);
            assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY, "{key}");
            assert!(
                !err.1.contains(SENTINEL),
                "the refusal must never quote the body: {err:?}"
            );
        }
    }

    #[test]
    fn a_typed_value_has_nowhere_to_go_in_the_recorded_trigger() {
        // A `type` interaction on an input whose live value is the sentinel:
        // the recorded trigger names the action and the input's journey
        // fingerprint, which excludes what was typed.
        let body = json!({ "actionType": "type", "targetElementId": "q" });
        let (_, action) = parse_interaction(body.to_string().as_bytes()).unwrap();
        assert_eq!(action.target, TriggerTarget::Element("q".into()));
        let snapshot = json!({ "elements": [{
            "id": "q", "type": "input", "actions": ["type"],
            "text": SENTINEL, "state": { "value": SENTINEL }
        }]});
        let trigger = resolve_trigger(&action, &extract_affordances(&snapshot));
        let wire = serde_json::to_string(&trigger).unwrap();
        assert!(!wire.contains(SENTINEL), "{wire}");
        assert_eq!(trigger.action_type, "type");
        assert!(trigger.target_fingerprint.is_some());
    }

    #[test]
    fn an_unknown_action_type_is_refused_without_quoting_it() {
        let body = json!({ "actionType": SENTINEL, "targetElementId": "x" });
        let err = parse_interaction(body.to_string().as_bytes()).unwrap_err();
        assert!(!err.1.contains(SENTINEL), "{err:?}");
    }

    #[test]
    fn the_stored_free_text_fields_must_be_identifiers() {
        let ok = json!({
            "actionType": "click", "targetElementId": "save",
            "sessionId": "rec-2026-10-28:abc_1", "appVersion": "1.4.0+g3f2a"
        });
        assert!(parse_interaction(ok.to_string().as_bytes()).is_ok());
        for (field, v) in [
            ("sessionId", "my password is hunter2"),
            ("appVersion", "1.0 / search?q=x"),
            ("sessionId", ""),
        ] {
            let mut body = json!({ "actionType": "click", "targetElementId": "save" });
            body[field] = json!(v);
            assert!(
                parse_interaction(body.to_string().as_bytes()).is_err(),
                "{field}={v:?}"
            );
        }
        assert!(!is_identifier(&"a".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn interactions_map_to_the_closed_action_kinds() {
        let parse = |v: serde_json::Value| parse_interaction(v.to_string().as_bytes());
        let (_, nav) = parse(json!({ "actionType": "navigate" })).unwrap();
        assert_eq!(nav.choke_point, ChokePoint::Navigation);
        assert_eq!(nav.target, TriggerTarget::Unresolved);
        let (i, click) = parse(json!({
            "actionType": "click", "targetElementId": " save ", "tabId": "t1"
        }))
        .unwrap();
        assert_eq!(click.choke_point, ChokePoint::ElementAction);
        assert_eq!(click.target, TriggerTarget::Element("save".into()));
        assert_eq!(i.tab_id.as_deref(), Some("t1"));
        assert!(parse(json!({ "actionType": "click" })).is_err());
        assert!(parse(json!({ "actionType": "click", "targetElementId": "  " })).is_err());
    }

    #[test]
    fn a_superseded_interaction_skips_its_snapshot() {
        let key = CursorKey::new("observe-test-app", Some("tab-gen"));
        let first = bump(&key);
        let second = bump(&key);
        assert!(!is_latest(&key, first));
        assert!(is_latest(&key, second));
    }

    #[test]
    fn the_generation_map_is_capped_and_evicts_the_least_recent() {
        let t0 = std::time::Instant::now();
        let mut g = Generations::default();
        let key = |i: usize| CursorKey::new("app", Some(&format!("tab-{i}")));
        let first = g.bump(&key(0), t0);
        for i in 1..MAX_TRACKED_CURSORS {
            g.bump(&key(i), t0 + std::time::Duration::from_millis(i as u64));
        }
        assert_eq!(g.len(), MAX_TRACKED_CURSORS);
        let later = t0 + std::time::Duration::from_secs(10);
        g.bump(&key(MAX_TRACKED_CURSORS), later);
        assert_eq!(g.len(), MAX_TRACKED_CURSORS, "bounded");
        assert!(!g.is_latest(&key(0), first), "the oldest was evicted");
        // Re-created, it never reuses a generation.
        let again = g.bump(&key(0), later);
        assert_ne!(again, first);
    }

    #[tokio::test]
    async fn passive_capture_never_takes_the_queues_reserve() {
        use crate::journey::capture::{reserve_passive_slot, NotQueued, PASSIVE_QUEUE_RESERVE};
        let cap = PASSIVE_QUEUE_RESERVE * 4;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(cap);
        for _ in 0..(cap - PASSIVE_QUEUE_RESERVE - 1) {
            tx.try_send(0).unwrap();
        }
        // One slot above the reserve: a passive event takes it.
        let held = reserve_passive_slot(&tx).expect("one slot above the reserve");
        assert_eq!(tx.capacity(), PASSIVE_QUEUE_RESERVE);
        // While that permit is held the next passive event is refused, and the
        // refused attempt gives its slot back.
        assert_eq!(reserve_passive_slot(&tx).err(), Some(NotQueued::Reserve));
        assert_eq!(tx.capacity(), PASSIVE_QUEUE_RESERVE, "nothing leaked");
        // Agent and explorer events still take the reserve.
        tx.try_send(1).unwrap();
        held.send(2);
        // A full queue refuses a passive event; a drained one takes it again.
        while tx.capacity() > 0 {
            tx.try_send(0).unwrap();
        }
        assert_eq!(reserve_passive_slot(&tx).err(), Some(NotQueued::Reserve));
        while rx.try_recv().is_ok() {}
        assert!(reserve_passive_slot(&tx).is_ok());
        drop(rx);
        assert_eq!(reserve_passive_slot(&tx).err(), Some(NotQueued::Closed));
    }

    #[tokio::test]
    async fn the_closing_snapshot_is_recorded_as_passive_capture() {
        // `close_with_snapshot` runs the snapshot route inside `as_passive`,
        // which is what holds its event off the reserve (`enqueue_edge_observation`).
        // Driven through the real body, with the route call stubbed.
        assert!(!capture::in_passive_scope());
        let key = CursorKey::new("observe-close-app", Some("tab-close"));
        let generation = bump(&key);
        let seen = close_with_snapshot_via(key, generation, Duration::ZERO, |query| async move {
            (capture::in_passive_scope(), query)
        })
        .await
        .expect("the latest interaction takes its snapshot");
        assert!(seen.0, "the snapshot ran inside as_passive");
        assert_eq!(
            seen.1.get("app_id").map(String::as_str),
            Some("observe-close-app")
        );
        assert_eq!(seen.1.get("tabId").map(String::as_str), Some("tab-close"));

        // A superseded interaction takes no snapshot at all.
        let key = CursorKey::new("observe-close-app", Some("tab-superseded"));
        let stale = bump(&key);
        let _ = bump(&key);
        let called = std::sync::atomic::AtomicBool::new(false);
        let skipped = close_with_snapshot_via(key, stale, Duration::ZERO, |_| async {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .await;
        assert!(skipped.is_none());
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_runner_app_is_refused_and_every_closing_snapshot_names_its_app() {
        let runner = crate::spec_api::storage::RUNNER_APP_ID;
        assert!(observe_app_id(runner).is_err());
        assert!(observe_app_id(&runner.to_ascii_uppercase()).is_err());
        assert_eq!(observe_app_id("qontinui-web"), Ok("qontinui-web"));
        // An app the route admits is always snapshotted BY NAME, with its tab.
        let key = CursorKey::new("observe-named-app", Some("tab-n"));
        let generation = bump(&key);
        let query = close_with_snapshot_via(key, generation, Duration::ZERO, |q| async move { q })
            .await
            .unwrap();
        assert_eq!(
            query.get("app_id").map(String::as_str),
            Some("observe-named-app")
        );
        assert_eq!(query.get("tabId").map(String::as_str), Some("tab-n"));
    }

    #[test]
    fn a_refused_runner_app_relay_is_counted_against_the_runner_app() {
        let runner = crate::spec_api::storage::RUNNER_APP_ID;
        assert!(observe_app_id(runner).is_err());
        assert_eq!(
            refusal_target(runner),
            Some(runner),
            "a valid id: attributed"
        );
        assert_eq!(
            refusal_target("../etc"),
            None,
            "not an identifier: unattributed"
        );
    }

    #[test]
    fn a_tab_id_must_be_an_identifier() {
        let body = json!({ "actionType": "click", "targetElementId": "x", "tabId": "a b/c" });
        assert!(parse_interaction(body.to_string().as_bytes()).is_err());
        assert!(!is_identifier("../etc"));
    }

    #[test]
    fn a_body_that_is_not_json_is_a_400_as_on_the_explore_route() {
        // The explore route's split, which is axum's `Json` extractor's on the
        // runner's other JSON routes: not JSON → 400, the wrong shape → 422.
        for not_json in [&b"{not json"[..], b"", b"{\"actionType\": \"click\""] {
            let (status, why) = parse_interaction(not_json).unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{not_json:?}: {why}");
            let explore = crate::journey::explorer::parse_explore_request(not_json).unwrap_err();
            assert_eq!(explore.0, status, "the two routes agree on {not_json:?}");
        }
        let (status, _) = parse_interaction(br#"{"actionType": "click", "x": 1}"#).unwrap_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let explore = crate::journey::explorer::parse_explore_request(b"[1]").unwrap_err();
        assert_eq!(explore.0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn a_padded_app_id_is_refused_by_both_routes_never_trimmed() {
        for padded in [" qontinui-web", "qontinui-web ", "\tqontinui-web"] {
            assert!(observe_app_id(padded).is_err(), "{padded:?}");
            let refusal = crate::journey::explorer::explore_app_id(padded)
                .expect_err("the explore route checks the id as given, too");
            assert_eq!(refusal.0, StatusCode::UNPROCESSABLE_ENTITY, "{padded:?}");
        }
        assert_eq!(observe_app_id("qontinui-web"), Ok("qontinui-web"));
        assert_eq!(
            crate::journey::explorer::explore_app_id("qontinui-web").map_err(|r| r.1),
            Ok("qontinui-web".to_string())
        );
    }

    #[test]
    fn the_observe_route_admits_trusted_origins_and_refuses_page_origins() {
        use crate::mcp::origin_guard::{LEDGER_WRITE_ROUTES, TRUSTED_ROUTES};
        let route = ("POST", "/apps/{app_id}/journey/observe");
        assert!(TRUSTED_ROUTES.contains(&route));
        assert!(LEDGER_WRITE_ROUTES.contains(&route));
    }
}
