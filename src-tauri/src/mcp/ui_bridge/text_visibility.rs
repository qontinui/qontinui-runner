//! Text visibility — the runner's direct `/ui-bridge/control/text-visibility`
//! route (plan 2026-10-13-image-generation-and-visual-judgment, Phase 16).
//!
//! The SDK's `POST /control/text-visibility {hidden, ttlMs?}` makes every glyph
//! on the page transparent without moving anything (`hidden: true`), and puts
//! the page back exactly (`hidden: false`). A caller captures frame A (text
//! rendered) and frame B (text hidden) against one snapshot; inside each text
//! element's box the two frames differ exactly where glyphs paint, and frame B
//! shows what those glyphs sit on. That is how vision-core's
//! `text_over_image_contrast` measures text over an image on real pixels.
//!
//! Three layers, the F2 network-stub precedent (`stubs.rs`):
//!   1. this file — validation + the HTTP route, forwarding to the runner's
//!      OWN webview over `ui_bridge_request_sync`;
//!   2. `mcp/sdk_client.rs` — `/ui-bridge/sdk/control/text-visibility`, the
//!      same command over the active SDK app's transport (WS or HTTP);
//!   3. `src/hooks/ui-bridge-events/textVisibilityRequest.ts` — the frontend
//!      hook that runs the SDK engine in the runner's webview.
//!
//! It is a DEV CONTROL. The SDK engine answers `capability_unavailable_in_build`
//! unless the realm opted in (`@qontinui/ui-bridge` `core/dev-controls.ts`): the
//! injected runtime always does. The runner's webview opts in only where its
//! `UIBridgeProvider` passes `features.devControls: import.meta.env.DEV` — a
//! Vite dev runner, once qontinui-runner#2311 lands that line. Everywhere else,
//! every exe-mode runner included, this route and the runner arm below answer
//! `capability_unavailable_in_build`. The Phase 16 measurement on a web page
//! goes through inject-cli (`/verify-web`); this arm covers only the runner's
//! own window where its webview opted in. The route is mounted regardless,
//! because the SDK manifest-parity test requires every SDK route to be exposed.
//! The gate is not a security boundary: hiding text adds no capability beyond
//! the runner's existing `page_evaluate` / eval surface.
//!
//! Validation mirrors the SDK's `validateTextVisibilityRequest` for an early
//! 400; the SDK copy is the authority. Unlike the media override, there is no
//! page-side wait to fit inside a transport budget: the SDK's hide and restore
//! are synchronous, so one body serves every transport arm.
//!
//! Every hide or restore changes rendered pixels the vision capture cache keys
//! on, so each one bumps `vision_mutation_id` (the `bump` callbacks below):
//! otherwise a capture cached before a hide is served as "current" during it,
//! and one cached during a hide is served after the restore.
//!
//! [`capture_text_hidden_pair`] is the runner arm of plan step 2: frame A, hide,
//! frame B, restore — the orchestration `vision/analyze` and `vision/assert`
//! run for `captureTextHidden: true`.

use std::future::Future;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::{ui_bridge_request_sync, wrap_ipc_result_keeping_failure_data};

/// SDK bounds, mirrored (`control/text-visibility.ts`).
pub(crate) const MIN_TTL_MS: u64 = 1_000;
pub(crate) const MAX_TTL_MS: u64 = 600_000;
/// The SDK's TTL when a hide names none (`DEFAULT_TEXT_VISIBILITY_TTL_MS`).
pub(crate) const DEFAULT_TTL_MS: u64 = 30_000;

/// How long past a hide's TTL the scheduled mutation bump fires: the SDK's
/// own timer restores the page at the TTL, with no request this runner sees,
/// so a bump a little later keeps the capture cache from serving a frame
/// cached while the text was hidden. Unconditional — a bump after a restore
/// that already happened costs one cache miss and nothing else.
pub(crate) const TTL_BUMP_MARGIN_MS: u64 = 1_000;

/// When a body that HIDES text should have its TTL-restore bump scheduled:
/// the TTL it names (or the SDK default) plus [`TTL_BUMP_MARGIN_MS`]. `None`
/// for a restore, which has no TTL.
pub(crate) fn ttl_bump_delay_ms(body: &Value) -> Option<u64> {
    if body.get("hidden").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let ttl = body
        .get("ttlMs")
        .and_then(whole_u64)
        .unwrap_or(DEFAULT_TTL_MS);
    Some(ttl + TTL_BUMP_MARGIN_MS)
}

/// Bump `state`'s vision mutation id after `delay_ms`: the TTL-restore bump
/// [`ttl_bump_delay_ms`] times.
pub(crate) fn schedule_mutation_bump(state: &Arc<ApiState>, delay_ms: u64) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        super::vision_routes::bump_mutation_id(&state);
    });
}

/// The IPC request type the frontend hook answers.
pub(crate) const REQUEST_TYPE: &str = "text_visibility";

/// The SDK command name on the `/sdk/` wrapper's WebSocket arm.
pub(crate) const SDK_ACTION: &str = "textVisibility";

/// The SDK route, relative to the app's ui-bridge mount (the `/sdk/` HTTP arm).
pub(crate) const SDK_PATH: &str = "/control/text-visibility";

/// The TTL the runner arm ([`capture_text_hidden_pair`]) hides with. Long enough for
/// one runner-window capture (a monitor-crop fallback included) between the
/// hide and the restore; short enough that a runner dying between the two
/// leaves the page altered for seconds, not the SDK's default half minute.
pub(crate) const CAPTURE_TTL_MS: u64 = 15_000;
const _: () = assert!(CAPTURE_TTL_MS >= MIN_TTL_MS && CAPTURE_TTL_MS <= MAX_TTL_MS);

/// A JSON number the SDK's `Number.isInteger` accepts, as a `u64`: `5000` and
/// `5000.0` alike. A fraction, a negative or a non-number is `None`.
fn whole_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && (0.0..=u64::MAX as f64).contains(f))
            .map(|f| f as u64)
    })
}

/// Validate a `POST /control/text-visibility` body. Mirrors the SDK rules:
/// `hidden` is a boolean; `ttlMs` is checked only on a hide (a show has no TTL,
/// so whatever a caller sends along with `hidden: false` is ignored, not
/// refused).
pub(crate) fn validate_text_visibility_body(body: &Value) -> Result<(), String> {
    let obj = body.as_object().ok_or("body must be a JSON object")?;
    let hidden = match obj.get("hidden") {
        Some(Value::Bool(b)) => *b,
        other => {
            let got = other.map_or_else(|| "undefined".to_string(), Value::to_string);
            return Err(format!("hidden must be a boolean (got {got})"));
        }
    };
    if !hidden {
        return Ok(());
    }
    match obj.get("ttlMs") {
        None => Ok(()),
        Some(v) => match whole_u64(v) {
            Some(n) if (MIN_TTL_MS..=MAX_TTL_MS).contains(&n) => Ok(()),
            _ => Err(format!(
                "ttlMs must be an integer in {MIN_TTL_MS}..{MAX_TTL_MS} (got {v})"
            )),
        },
    }
}

type HandlerResult = Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<Value>>)>;

fn bad_request(msg: String) -> (StatusCode, Json<ApiResponse<Value>>) {
    let e = api_error(msg);
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            success: e.success,
            data: None,
            error: e.error,
            error_detail: e.error_detail,
            hint: e.hint,
            code: e.code,
            suggestions: e.suggestions,
        }),
    )
}

/// The direct route's body, over what a test can construct: the request body
/// and `send` — the request to the runner's own webview
/// (`ui_bridge_request_sync` in the axum handler, a recording closure in a
/// test). Validation runs HERE, before `send`, so a test entering through this
/// function observes it.
pub(crate) async fn direct_set<S, F, B, T>(
    body: Value,
    send: S,
    bump: B,
    bump_after_ttl: T,
) -> HandlerResult
where
    S: FnOnce(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
    B: Fn(),
    T: FnOnce(u64),
{
    validate_text_visibility_body(&body).map_err(bad_request)?;
    info!(
        "UI Bridge API: text visibility hidden={}",
        body["hidden"].as_bool().unwrap_or_default()
    );
    // A hide's TTL restores the page with no request this runner sees.
    if let Some(delay_ms) = ttl_bump_delay_ms(&body) {
        bump_after_ttl(delay_ms);
    }
    // Rendered pixels change between these two bumps; a capture keyed on the
    // mutation id from either side of them is not reused across the change.
    bump();
    let result = send(REQUEST_TYPE, body).await;
    bump();
    // The SDK's own failure code (`capability_unavailable_in_build`,
    // `HIDE_FAILED`, …) is a verdict field the envelope does not forward when
    // the runner has no typed code by that name, so it is put back into
    // `data`, where a caller branches on it exactly as on the other transports.
    let sdk_code = result
        .as_ref()
        .ok()
        .filter(|v| v.get("success").and_then(Value::as_bool) == Some(false))
        .and_then(|v| v.get("code"))
        .cloned();
    wrap_ipc_result_keeping_failure_data(result).map_err(|(status, Json(mut body))| {
        if let Some(code) = sdk_code {
            if let Some(data) = body
                .data
                .get_or_insert_with(|| Value::Object(Default::default()))
                .as_object_mut()
            {
                data.entry("code").or_insert(code);
            }
        }
        (status, Json(body))
    })
}

/// POST /ui-bridge/control/text-visibility — hide or restore every glyph in the runner's webview.
pub async fn ui_bridge_text_visibility_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<Value>,
) -> HandlerResult {
    let bump_state = Arc::clone(&state);
    let ttl_state = Arc::clone(&state);
    direct_set(
        body,
        |kind, body| async move { ui_bridge_request_sync(&state, kind, body).await },
        move || super::vision_routes::bump_mutation_id(&bump_state),
        move |delay_ms| schedule_mutation_bump(&ttl_state, delay_ms),
    )
    .await
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route(
        "/ui-bridge/control/text-visibility",
        post(ui_bridge_text_visibility_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("POST", "/ui-bridge/control/text-visibility")]
}

// ============================================================================
// The runner arm (plan step 2): frame A, hide, frame B, restore
// ============================================================================

/// What the runner arm did to the page, reported beside frame B so a caller
/// can tell "no frame B because the hide was refused" from "frame B present"
/// and can prove the page ended restored.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextVisibilityRun {
    /// What the webview answered to the hide: the SDK's own result
    /// (`{success, hidden, changed, coverage, ttlMs, paintFence, …}`) or its
    /// refusal (`{success: false, code, reason, error}` —
    /// `capability_unavailable_in_build` wherever the webview has not opted
    /// into dev controls), or `{success: false, error}` when the request never
    /// reached the SDK.
    pub hide: Value,
    /// What the webview answered to the restore, the SDK's `{hidden: false,
    /// changed, restore: {verified, …}}`. `None` when no restore was sent: the
    /// hide failed, or the text was already hidden by another caller
    /// (`wasHidden: true`) — then the hook did not touch that hide at all (not
    /// even its TTL), and it is left to its caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<Value>,
    /// True only when a restore was sent and the SDK verified it
    /// (`restore.verified: true`); false when it was sent and not verified or
    /// failed; `None` when none was sent (see `restore`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_restored: Option<bool>,
    /// Something the caller should know that does not by itself void frame B:
    /// the hide answered after its paint fence TIMED OUT, so frame B may have
    /// been composited before the glyphs went transparent (vision-core then
    /// reads the glyphs as still painted and answers unknown, never a pass).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// The outcome of [`capture_text_hidden_pair`]: frame A, frame B when it was
/// taken (else why not), and the hide/restore record when a hide was sent.
#[derive(Debug)]
pub struct TextHiddenPair<Fr> {
    pub frame_a: Result<Fr, String>,
    pub frame_b: Result<Fr, String>,
    pub run: Option<TextVisibilityRun>,
}

fn is_true(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

/// A hide request that never left the runner, as one line.
fn never_sent_message(hide: &Value) -> String {
    let error = hide
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no error message");
    format!("the hide request never left the runner, so nothing was hidden: {error}")
}

/// A failed or refused hide, as one line naming the SDK's code when it gave one.
fn hide_failure_message(hide: &Value) -> String {
    let code = hide.get("code").and_then(Value::as_str);
    let error = hide
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no error message");
    match code {
        Some(code) => format!("the runner webview refused to hide its text ({code}): {error}"),
        None => format!("the runner webview did not hide its text: {error}"),
    }
}

/// Serializes the runner arm: one webview, one hide state. Two concurrent
/// `captureTextHidden` calls would otherwise interleave — the second's frame A
/// taken during the first's hide, or the first's restore landing before the
/// second's frame B — and both would measure the wrong pixels. An `Arc`'d
/// mutex so the drop guard's spawned restore can carry the lock with it: the
/// next arm then cannot take its frame A until that restore has landed.
fn pair_lock() -> Arc<tokio::sync::Mutex<()>> {
    static LOCK: std::sync::OnceLock<Arc<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();
    Arc::clone(LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(()))))
}

/// The lock the arm holds, as handed to its restore-on-drop callback.
pub(crate) type PairLockGuard = tokio::sync::OwnedMutexGuard<()>;

/// Holds the arm's lock AND its capture permits for the whole call and, while
/// ARMED, restores the page if dropped: the arm's future cancelled (the HTTP
/// client went away) or a panic unwinding through it. The restore callback is
/// handed both, so the restore it spawns releases them only once it has
/// landed and bumped: until then no other arm can start and no other capture
/// can see the text-hidden page (and cache it under the post-hide mutation id).
struct ArmGuard<P, R: FnOnce(PairLockGuard, P)> {
    restore: Option<R>,
    held: Option<(PairLockGuard, P)>,
}

impl<P, R: FnOnce(PairLockGuard, P)> ArmGuard<P, R> {
    fn new(lock: PairLockGuard, permits: P) -> Self {
        Self {
            restore: None,
            held: Some((lock, permits)),
        }
    }

    fn arm(&mut self, restore: R) {
        self.restore = Some(restore);
    }

    fn disarm(&mut self) {
        self.restore = None;
    }
}

impl<P, R: FnOnce(PairLockGuard, P)> Drop for ArmGuard<P, R> {
    fn drop(&mut self) {
        if let (Some(restore), Some((lock, permits))) = (self.restore.take(), self.held.take()) {
            warn!("vision: the runner arm ended before its restore; restoring the webview's text");
            restore(lock, permits);
        }
    }
}

/// Whose hide the reply shows — with no clock at all. The hook reads the SDK's
/// `textHidden()` immediately before `setTextVisibility`, in the same
/// synchronous JavaScript run, and returns it as `wasHidden`: nothing can hide
/// or restore the page between the two. `wasHidden: false` on a successful
/// hide means THIS request created it; `true` means another caller's hide was
/// already there (and, under `onlyIfVisible`, the hook left it untouched).
///
/// `Some(true)`: ours. `Some(false)`: another caller's. `None`: the reply
/// carries no boolean `wasHidden` (the SDK could not tell: another copy of it
/// owns the state) — unknown.
///
/// Not `hiddenAt`: comparing it with a receive stamp needed the webview's wall
/// clock not to step between the two readings, and still could not separate a
/// hide made in the same millisecond. Not `changed` either: a re-hide answers
/// `changed: true` whenever the app added elements meanwhile.
fn hide_is_ours(hide: &Value) -> Option<bool> {
    hide.get("wasHidden")
        .and_then(Value::as_bool)
        .map(|was| !was)
}

/// The read-only probe the arm sends before frame A: the hook answers
/// `{hidden}` from `textHidden()` and changes nothing.
pub(crate) fn probe_body() -> Value {
    serde_json::json!({"query": true})
}

/// The arm's hide: [`CAPTURE_TTL_MS`], and `onlyIfVisible` so that a page some
/// other caller already hid is left exactly as it is — not re-hidden, and its
/// TTL not re-armed.
pub(crate) fn arm_hide_body() -> Value {
    serde_json::json!({"hidden": true, "ttlMs": CAPTURE_TTL_MS, "onlyIfVisible": true})
}

/// Whether a failed hide request PROVABLY never reached the webview: the error
/// is one `ui_bridge_request_sync` returns before the request is emitted — an
/// open circuit breaker, the frontend never becoming ready, the UI Bridge
/// concurrency gate, an unknown window, or the request failing to serialize.
/// Nothing was hidden, so nothing is restored. Any other error (a timeout after the send
/// above all) leaves it unknown whether the hide landed, and is restored.
///
/// Matched on the messages `request.rs` and `circuit_breaker.rs` produce; a
/// test pins that each one still exists there, so a reworded message fails the
/// build's tests rather than silently turning into "unknown".
pub(crate) fn hide_never_sent(error: &str) -> bool {
    BEFORE_SEND.iter().any(|m| error.contains(m))
}

/// The errors [`hide_never_sent`] reads as "never left the runner". An emit
/// failure is deliberately NOT one: Tauri's emit can fail after a partial
/// delivery, so that error is restored (a redundant restore costs one request).
pub(crate) const BEFORE_SEND: [&str; 6] = [
    "UI Bridge temporarily unavailable (circuit breaker open)",
    "frontend_not_ready",
    "UI Bridge semaphore closed",
    "UI Bridge concurrency limit reached (timeout acquiring permit)",
    "No runner window labeled",
    "Failed to serialize UiBridgeRequestEnvelope",
];

/// Frame A, then every glyph in the runner's own webview hidden, frame B, and
/// the page restored — under one lock, with the capture permits held
/// throughout.
///
/// - `send` is the request to the webview (`ui_bridge_request_sync`): the
///   probe, the hide and the restore.
/// - `acquire` takes the capture permits BEFORE the probe — ALL of them in
///   production, so no other runner-window capture can see the page while its
///   text is hidden. They are held until the restore attempt has returned (or,
///   on a cancel, until the guard's restore has landed). A DOM read — a
///   snapshot, a discover — during the hide is not covered: it reads the DOM,
///   not pixels, and the hide changes only paint properties, but its computed
///   colours would read transparent.
/// - `capture` takes one frame of the runner window, the permits already held:
///   frame A, and — only after the webview confirmed THIS call's hide — frame B.
/// - `bump` bumps the vision mutation id: before the hide and after the restore.
/// - `bump_after_ttl` schedules the bump for the SDK's own TTL restore.
/// - `abort_restore` restores the page if this future is cancelled or unwinds
///   while armed. It is handed the lock and the permits and must spawn the
///   restore holding both, release them only after the restore and its bump,
///   and not block.
///
/// Before frame A the webview is asked whether its text is already hidden
/// ([`probe_body`]). Only an answer of `hidden: false` proceeds: hidden, or no
/// usable answer, refuses at once — no frame A, no hide, nothing to restore.
///
/// The hide ([`arm_hide_body`]) then says whose hide it is ([`hide_is_ours`]):
/// - THIS call's: frame B is taken and the page restored, whether or not frame
///   B was captured. A restore that fails or is not verified is reported in
///   [`TextVisibilityRun::page_restored`] and frame B is kept: what it shows
///   was fixed when it was taken. A restore answering `changed: false` means
///   the SDK's TTL had already restored the page, at an unknown moment that may
///   precede frame B, so frame B is discarded.
/// - Another caller's (`wasHidden: true`): it appeared after the probe, so it
///   may have covered frame A. Frame A is refused, no frame B is taken, and the
///   hide — which the hook did not touch — is not restored.
/// - UNKNOWN — the hide request errored or timed out after it may have been
///   sent, the call was cancelled before the hide reply, or the reply carries
///   no `wasHidden`: the page is RESTORED (holding the lock and permits), then
///   the id bumped, and no frame B is returned. Frame A stands: the probe saw
///   the text visible before it, and the unknown is about the hide only. The
///   arm's TTL is short, and a page left text-hidden poisons every capture
///   until it fires; the one hide this can wrongly undo is a concurrent
///   dev-only direct-route (`control/text-visibility`) hide.
/// - A refused hide (`success: false`), or a request that provably never left
///   the runner ([`hide_never_sent`]): nothing was hidden, nothing restored, and
///   the permits are released at once.
///
/// Accepted remainder: a hide by another caller that appears after the probe
/// AND is gone again before the hook reads `wasHidden` — made and restored (or
/// expired) entirely while frame A was being captured — is invisible to both
/// checks, and frame A may show its text-hidden page. Only the dev-only direct
/// route can hide this webview's text besides this arm, and the arm's lock
/// keeps two arms apart.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn capture_text_hidden_pair<S, F, A, AF, P, C, CF, Fr, B, T, R>(
    send: S,
    acquire: A,
    capture: C,
    bump: B,
    bump_after_ttl: T,
    abort_restore: R,
) -> TextHiddenPair<Fr>
where
    S: Fn(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<P, String>>,
    C: Fn() -> CF,
    CF: Future<Output = Result<Fr, String>>,
    B: Fn(),
    T: FnOnce(u64),
    R: FnOnce(PairLockGuard, P),
{
    let lock = pair_lock().lock_owned().await;
    let not_taken = |why: &str| Err(format!("frame B was not taken: {why}"));
    let permits = match acquire().await {
        Ok(p) => p,
        Err(e) => {
            return TextHiddenPair {
                frame_a: Err(format!("capture permit: {e}")),
                frame_b: not_taken("no capture permit"),
                run: None,
            }
        }
    };
    let mut guard = ArmGuard::new(lock, permits);

    // Is the text already hidden? Asked before frame A, so a frame A taken of a
    // page someone else had hidden is never returned as "text rendered".
    let probe = send(REQUEST_TYPE, probe_body()).await;
    let refused_before_a = match probe.as_ref().map(|v| v.get("hidden")) {
        Ok(Some(Value::Bool(false))) => None,
        Ok(Some(Value::Bool(true))) => Some(
            "the page's text was already hidden by another caller, so frame A would not show \
             it"
            .to_string(),
        ),
        Ok(_) => Some(
            "whether the page's text was already hidden is unknown (the webview's answer \
             carries no hidden flag)"
                .to_string(),
        ),
        Err(e) => Some(format!(
            "whether the page's text was already hidden is unknown (the webview did not \
             answer: {e})"
        )),
    };
    if let Some(why) = refused_before_a {
        return TextHiddenPair {
            frame_a: Err(format!("frame A was not taken: {why}")),
            frame_b: not_taken("frame A was not taken"),
            run: None,
        };
    }

    let mut frame_a = capture().await;
    if frame_a.is_err() {
        // Altering the page for a frame B nothing can be paired with buys nothing.
        return TextHiddenPair {
            frame_a,
            frame_b: not_taken("frame A could not be captured"),
            run: None,
        };
    }

    let hide_body = arm_hide_body();
    if let Some(delay_ms) = ttl_bump_delay_ms(&hide_body) {
        bump_after_ttl(delay_ms);
    }
    // Armed BEFORE the send: whether the hide landed is unknown until its reply,
    // and an unknown hide is restored (see the doc above) — on a cancel, by the
    // guard, still holding the lock and the permits. That restore may undo a
    // concurrent dev-only direct-route hide; it is the price of never leaving
    // the page text-hidden for the arm's TTL.
    guard.arm(abort_restore);
    bump();
    let hide_reply = send(REQUEST_TYPE, hide_body).await;
    let (hide, landed, never_sent) = match hide_reply {
        Ok(v) => (v, true, false),
        Err(e) => {
            let never_sent = hide_never_sent(&e);
            (
                serde_json::json!({"success": false, "error": e}),
                false,
                never_sent,
            )
        }
    };
    if never_sent || (landed && !(is_true(&hide, "success") && is_true(&hide, "hidden"))) {
        // Nothing was hidden — the request never left the runner, or the
        // webview answered and refused — so nothing is restored, and the lock
        // and permits go at once.
        guard.disarm();
        let error = if never_sent {
            never_sent_message(&hide)
        } else {
            hide_failure_message(&hide)
        };
        return TextHiddenPair {
            frame_a,
            frame_b: Err(error),
            run: Some(TextVisibilityRun {
                hide,
                restore: None,
                page_restored: None,
                warning: None,
            }),
        };
    }
    let ours = if landed { hide_is_ours(&hide) } else { None };
    if ours == Some(false) {
        // Another caller hid the text after the probe: left to that caller
        // untouched, and frame A — possibly taken under it — is refused.
        guard.disarm();
        frame_a = Err(
            "frame A was not kept: another caller hid the page's text after the pre-frame-A \
             check, possibly while frame A was being captured"
                .into(),
        );
        return TextHiddenPair {
            frame_a,
            frame_b: not_taken("the page was already text-hidden by another caller"),
            run: Some(TextVisibilityRun {
                hide,
                restore: None,
                page_restored: None,
                warning: None,
            }),
        };
    }
    let warning = (hide.get("paintFence").and_then(Value::as_str) == Some("timeout")).then(|| {
        "the hide's paint fence timed out (the webview painted no frame within it), so \
         frame B may have been composited before the glyphs went transparent"
            .to_string()
    });

    // Ours: frame B. Unknown: none — restore only.
    let mut frame_b = match ours {
        Some(true) => capture()
            .await
            .map_err(|e| format!("frame B (text hidden) capture failed: {e}")),
        _ => not_taken(if landed {
            "whose hide it is is unknown (the reply carries no wasHidden); the page was \
             restored"
        } else {
            "the hide request failed, so whether it landed is unknown; the page was restored"
        }),
    };

    // The permits are still held: until this attempt returns, no other capture
    // can see the page. If it fails, the page may stay hidden until the TTL.
    let restore = match send(REQUEST_TYPE, serde_json::json!({"hidden": false})).await {
        Ok(v) => v,
        Err(e) => serde_json::json!({"success": false, "error": e}),
    };
    guard.disarm();
    bump();
    let verified = is_true(&restore, "success")
        && restore.get("hidden").and_then(Value::as_bool) == Some(false)
        && restore
            .get("restore")
            .is_some_and(|r| is_true(r, "verified"));
    if !verified {
        warn!("vision: the runner webview's text restore was not verified: {restore}");
    }
    if is_true(&restore, "success")
        && restore.get("changed").and_then(Value::as_bool) == Some(false)
        && frame_b.is_ok()
    {
        frame_b = Err(format!(
            "frame B discarded: the restore found the text already visible, so the \
             {CAPTURE_TTL_MS} ms TTL had restored the page at a moment that may precede \
             frame B"
        ));
    }

    TextHiddenPair {
        frame_a,
        frame_b,
        run: Some(TextVisibilityRun {
            hide,
            restore: Some(restore),
            page_restored: Some(verified),
            warning,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn the_sdk_rules_are_mirrored() {
        for ok in [
            json!({"hidden": true}),
            json!({"hidden": false}),
            json!({"hidden": true, "ttlMs": 1_000}),
            json!({"hidden": true, "ttlMs": 600_000}),
            json!({"hidden": true, "ttlMs": 5_000.0}),
            // A show has no TTL: anything sent along is ignored, not refused.
            json!({"hidden": false, "ttlMs": "nonsense"}),
        ] {
            assert!(validate_text_visibility_body(&ok).is_ok(), "{ok}");
        }
        let cases = [
            (json!([]), "JSON object"),
            (json!({}), "hidden must be a boolean (got undefined)"),
            (json!({"hidden": "true"}), "hidden must be a boolean"),
            (json!({"hidden": 1}), "hidden must be a boolean"),
            (
                json!({"hidden": true, "ttlMs": 999}),
                "ttlMs must be an integer",
            ),
            (
                json!({"hidden": true, "ttlMs": 600_001}),
                "ttlMs must be an integer",
            ),
            (
                json!({"hidden": true, "ttlMs": 1500.5}),
                "ttlMs must be an integer",
            ),
            (
                json!({"hidden": true, "ttlMs": "5000"}),
                "ttlMs must be an integer",
            ),
        ];
        for (body, needle) in cases {
            let err = validate_text_visibility_body(&body).expect_err(&body.to_string());
            assert!(err.contains(needle), "{body}: {err} lacks {needle}");
        }
    }

    // ── the direct route, entered through its handler body ────────────────

    type Sent = Arc<Mutex<Vec<(&'static str, Value)>>>;

    /// A stand-in for the runner's webview: records every request it is sent
    /// and answers each with the next of `replies`.
    fn webview(
        replies: Vec<Result<Value, String>>,
    ) -> (
        Sent,
        impl Fn(&'static str, Value) -> std::future::Ready<Result<Value, String>>,
    ) {
        let sent: Sent = Default::default();
        let log = sent.clone();
        let replies = Mutex::new(replies.into_iter());
        let send = move |kind: &'static str, body: Value| {
            log.lock().unwrap().push((kind, body));
            let reply = replies
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| Err("no more replies scripted".into()));
            std::future::ready(reply)
        };
        (sent, send)
    }

    #[tokio::test]
    async fn the_direct_route_refuses_an_invalid_body_before_the_webview_sees_it() {
        let (sent, send) = webview(vec![]);
        let (status, Json(body)) = direct_set(json!({"hidden": "yes"}), send, || {}, |_| {})
            .await
            .expect_err("must be refused");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.error
                .unwrap_or_default()
                .contains("hidden must be a boolean"),
            "the SDK's message"
        );
        assert!(
            sent.lock().unwrap().is_empty(),
            "nothing may reach the webview"
        );
    }

    #[tokio::test]
    async fn the_direct_route_sends_the_webview_the_body_as_given() {
        let (sent, send) = webview(vec![Ok(json!({"success": true, "hidden": true}))]);
        let out = direct_set(json!({"hidden": true, "ttlMs": 5_000}), send, || {}, |_| {})
            .await
            .expect("forwarded");
        assert_eq!(out.0.data, Some(json!({"success": true, "hidden": true})));
        assert_eq!(
            *sent.lock().unwrap(),
            vec![(REQUEST_TYPE, json!({"hidden": true, "ttlMs": 5_000}))]
        );
    }

    #[tokio::test]
    async fn the_direct_route_relays_the_dev_controls_refusal_as_a_typed_failure() {
        let refusal = json!({
            "success": false,
            "code": "capability_unavailable_in_build",
            "reason": "dev_controls_disabled",
            "error": "dev control",
        });
        let (_sent, send) = webview(vec![Ok(refusal)]);
        let (status, Json(body)) = direct_set(json!({"hidden": true}), send, || {}, |_| {})
            .await
            .expect_err("a refusal is a failure, never a success");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let data = body.data.expect("the refusal's fields are kept");
        assert_eq!(data["code"], json!("capability_unavailable_in_build"));
        assert_eq!(data["reason"], json!("dev_controls_disabled"));
    }

    #[test]
    fn route_entries_match_the_router() {
        assert_eq!(
            route_entries(),
            &[("POST", "/ui-bridge/control/text-visibility")]
        );
    }

    #[tokio::test]
    async fn the_direct_route_bumps_the_mutation_id_around_the_change() {
        let log: Sent = Default::default();
        let (l1, l2) = (log.clone(), log.clone());
        let send = move |kind: &'static str, body: Value| {
            l1.lock().unwrap().push((kind, body));
            std::future::ready(Ok(json!({"success": true, "hidden": true})))
        };
        let bump = move || l2.lock().unwrap().push(("<bump>", Value::Null));
        let _forwarded = direct_set(json!({"hidden": true}), send, bump, |_| {})
            .await
            .expect("forwarded");
        let kinds: Vec<&str> = log.lock().unwrap().iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, ["<bump>", REQUEST_TYPE, "<bump>"]);
    }

    /// The SDK's TTL restores the page with no request the runner sees: a hide
    /// schedules a bump past its TTL; a restore schedules none.
    #[tokio::test]
    async fn a_hide_schedules_a_bump_past_its_ttl() {
        let scheduled = |body: Value| {
            let at = Arc::new(Mutex::new(Vec::<u64>::new()));
            let a = at.clone();
            let (_sent, send) = webview(vec![Ok(json!({"success": true}))]);
            let run = direct_set(body, send, || {}, move |ms| a.lock().unwrap().push(ms));
            async move {
                let _ = run.await;
                let v = at.lock().unwrap().clone();
                v
            }
        };
        assert_eq!(
            scheduled(json!({"hidden": true, "ttlMs": 5_000})).await,
            [5_000 + TTL_BUMP_MARGIN_MS]
        );
        assert_eq!(
            scheduled(json!({"hidden": true})).await,
            [DEFAULT_TTL_MS + TTL_BUMP_MARGIN_MS]
        );
        assert!(scheduled(json!({"hidden": false})).await.is_empty());
    }

    // ── the runner arm: frame A, hide, frame B, restore ───────────────────

    type Events = Arc<Mutex<Vec<String>>>;

    /// A hide THIS call made: the hook found the text visible (`wasHidden:
    /// false`) and created the hide.
    fn hid() -> Value {
        json!({"success": true, "hidden": true, "changed": true, "ttlMs": CAPTURE_TTL_MS,
               "hiddenAt": 2_005, "wasHidden": false})
    }

    /// Another caller's hide, found by the hook (`onlyIfVisible`) and left
    /// untouched: no SDK call, so no `hiddenAt`.
    fn theirs() -> Value {
        json!({"success": true, "hidden": true, "changed": false, "wasHidden": true})
    }

    /// The probe's answers.
    fn visible() -> Value {
        json!({"success": true, "query": true, "hidden": false})
    }

    fn already_hidden() -> Value {
        json!({"success": true, "query": true, "hidden": true})
    }

    fn restored(verified: bool) -> Value {
        json!({"success": true, "hidden": false, "changed": true,
               "restore": {"trigger": "request", "verified": verified}})
    }

    type BoxFut<T> = std::pin::Pin<Box<dyn Future<Output = T> + Send>>;

    /// A scripted answer: `Some` answers, `None` never answers (and notifies
    /// the test that the arm reached it).
    type Step<T> = Option<Result<T, String>>;

    fn ok<T>(v: T) -> Step<T> {
        Some(Ok(v))
    }

    fn fails<T>(e: &str) -> Step<T> {
        Some(Err(e.into()))
    }

    /// One scripted run of the arm against a fake webview and captures; every
    /// step lands in `events` — "probe" for the read-only query, "hide" for a
    /// hide asking `onlyIfVisible` ("hide-unconditional" otherwise), "show"
    /// for a restore. `stalled` is notified when a `None` step is reached.
    fn run_scripted(
        events: Events,
        stalled: Arc<tokio::sync::Notify>,
        probe: Step<Value>,
        replies: Vec<Step<Value>>,
        frames: Vec<Step<&'static str>>,
    ) -> BoxFut<TextHiddenPair<&'static str>> {
        let probe = Arc::new(Mutex::new(Some(probe)));
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        let frames = Arc::new(Mutex::new(frames.into_iter()));
        let (e1, e2, e3, e4, e5, e6) = (
            events.clone(),
            events.clone(),
            events.clone(),
            events.clone(),
            events.clone(),
            events,
        );
        let stall2 = stalled.clone();
        let send = move |_kind: &'static str, body: Value| {
            let step = if body.get("query") == Some(&json!(true)) {
                e1.lock().unwrap().push("probe".into());
                probe
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or_else(|| fails("probed twice"))
            } else {
                let event = match (body["hidden"].as_bool(), body.get("onlyIfVisible")) {
                    (Some(true), Some(Value::Bool(true))) => "hide",
                    (Some(true), _) => "hide-unconditional",
                    _ => "show",
                };
                e1.lock().unwrap().push(event.into());
                replies
                    .lock()
                    .unwrap()
                    .next()
                    .unwrap_or_else(|| fails("no more replies scripted"))
            };
            let fut: BoxFut<Result<Value, String>> = match step {
                Some(r) => Box::pin(std::future::ready(r)),
                None => {
                    stalled.notify_one();
                    Box::pin(std::future::pending())
                }
            };
            fut
        };
        let acquire = move || {
            e2.lock().unwrap().push("permits".into());
            std::future::ready(Ok::<(), String>(()))
        };
        let capture = move || {
            e3.lock().unwrap().push("capture".into());
            let step = frames.lock().unwrap().next().unwrap_or(None);
            let fut: BoxFut<Result<&'static str, String>> = match step {
                Some(f) => Box::pin(std::future::ready(f)),
                None => {
                    stall2.notify_one();
                    Box::pin(std::future::pending())
                }
            };
            fut
        };
        let bump = move || e4.lock().unwrap().push("bump".into());
        let ttl = move |ms: u64| e5.lock().unwrap().push(format!("ttl-bump {ms}"));
        let abort = move |_lock: PairLockGuard, _permits: ()| {
            e6.lock().unwrap().push("abort-restore".into())
        };
        Box::pin(capture_text_hidden_pair(
            send, acquire, capture, bump, ttl, abort,
        ))
    }

    async fn run_with_probe(
        probe: Step<Value>,
        replies: Vec<Step<Value>>,
        frames: Vec<Step<&'static str>>,
    ) -> (TextHiddenPair<&'static str>, Vec<String>) {
        let events: Events = Default::default();
        let out = run_scripted(events.clone(), Default::default(), probe, replies, frames).await;
        let events = events.lock().unwrap().clone();
        (out, events)
    }

    async fn run(
        replies: Vec<Step<Value>>,
        frames: Vec<Step<&'static str>>,
    ) -> (TextHiddenPair<&'static str>, Vec<String>) {
        run_with_probe(ok(visible()), replies, frames).await
    }

    /// Run until the arm reaches a `None` step, then drop it (the caller went
    /// away): deterministic, no timing.
    async fn run_until_stalled_then_cancel(
        replies: Vec<Step<Value>>,
        frames: Vec<Step<&'static str>>,
    ) -> Vec<String> {
        let events: Events = Default::default();
        let stalled = Arc::new(tokio::sync::Notify::new());
        let fut = run_scripted(
            events.clone(),
            stalled.clone(),
            ok(visible()),
            replies,
            frames,
        );
        tokio::select! {
            _ = fut => panic!("the arm was scripted to stall"),
            _ = stalled.notified() => {}
        }
        let events = events.lock().unwrap().clone();
        events
    }

    fn ttl_bump() -> String {
        format!("ttl-bump {}", CAPTURE_TTL_MS + TTL_BUMP_MARGIN_MS)
    }

    fn has(events: &[String], e: &str) -> bool {
        events.iter().any(|x| x == e)
    }

    #[tokio::test]
    async fn the_arm_takes_the_permits_then_probes_then_a_then_hides_then_b_then_restores() {
        let (out, events) = run(
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        assert_eq!(out.frame_b, Ok("frame-b"));
        assert_eq!(
            events,
            [
                "permits".to_string(),
                "probe".into(),
                "capture".into(),
                ttl_bump(),
                "bump".into(),
                "hide".into(),
                "capture".into(),
                "show".into(),
                "bump".into(),
            ],
            "the permits, then the probe, before frame A; a TTL bump scheduled with the \
             hide, which asks onlyIfVisible; a bump before the hide and after the restore; \
             no guard restore once restored"
        );
        let run = out.run.expect("a hide was sent");
        assert_eq!(run.page_restored, Some(true));
        assert_eq!(run.hide, hid());
        assert_eq!(run.warning, None);
    }

    #[tokio::test]
    async fn no_frame_a_means_nothing_is_hidden() {
        let (out, events) = run(vec![], vec![fails("Runner window not found")]).await;
        assert!(out.frame_a.is_err());
        assert!(out
            .frame_b
            .unwrap_err()
            .contains("frame A could not be captured"));
        assert!(out.run.is_none());
        assert!(!has(&events, "hide"), "{events:?}");
    }

    #[tokio::test]
    async fn a_refused_hide_captures_no_frame_b_and_names_the_code() {
        let refusal = json!({"success": false, "code": "capability_unavailable_in_build",
                             "error": "dev control"});
        let (out, events) = run(vec![ok(refusal.clone())], vec![ok("frame-a")]).await;
        let err = out.frame_b.expect_err("no frame B without a hide");
        assert!(err.contains("capability_unavailable_in_build"), "{err}");
        assert!(!has(&events, "show"), "the webview hid nothing: {events:?}");
        assert!(!has(&events, "abort-restore"), "{events:?}");
        assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
        let run = out.run.expect("the hide was sent");
        assert_eq!(run.hide, refusal);
        assert_eq!(run.restore, None);
    }

    /// The hide request erred (a transport error or a timeout): whether it
    /// landed is unknown, so the page is restored, and no frame B is taken.
    #[tokio::test]
    async fn an_unanswered_hide_is_restored_not_measured() {
        let (out, events) = run(
            vec![fails("UI Bridge request timed out"), ok(restored(true))],
            vec![ok("frame-a")],
        )
        .await;
        let err = out.frame_b.expect_err("no frame B for an unknown hide");
        assert!(err.contains("whether it landed is unknown"), "{err}");
        assert!(has(&events, "show"), "{events:?}");
        assert_eq!(events.last().map(String::as_str), Some("bump"));
        assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
        let run = out.run.unwrap();
        assert_eq!(run.hide["success"], json!(false));
        assert_eq!(run.page_restored, Some(true));
    }

    #[tokio::test]
    async fn a_failed_frame_b_still_restores_the_page() {
        let (out, events) = run(
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), fails("capture backend failed")],
        )
        .await;
        let err = out.frame_b.expect_err("the capture failed");
        assert!(
            err.contains("frame B (text hidden) capture failed"),
            "{err}"
        );
        assert_eq!(events.last().map(String::as_str), Some("bump"));
        assert!(has(&events, "show"), "{events:?}");
        assert_eq!(out.run.unwrap().page_restored, Some(true));
    }

    #[tokio::test]
    async fn an_unverified_or_failed_restore_is_reported_and_frame_b_kept() {
        for reply in [
            ok(restored(false)),
            ok(json!({"success": false, "error": "STATE_SCHEMA_MISMATCH"})),
            fails("transport"),
        ] {
            let (out, _) = run(
                vec![ok(hid()), reply.clone()],
                vec![ok("frame-a"), ok("frame-b")],
            )
            .await;
            assert_eq!(out.frame_b, Ok("frame-b"), "{reply:?}");
            assert_eq!(out.run.unwrap().page_restored, Some(false), "{reply:?}");
        }
    }

    #[tokio::test]
    async fn a_restore_the_ttl_already_made_discards_frame_b() {
        let late = json!({"success": true, "hidden": false, "changed": false,
                          "restore": {"trigger": "ttl", "verified": true}});
        let (out, _) = run(
            vec![ok(hid()), ok(late)],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        let err = out
            .frame_b
            .expect_err("frame B may postdate the TTL restore");
        assert!(err.contains("TTL had restored the page"), "{err}");
        assert_eq!(out.frame_a, Ok("frame-a"));
    }

    #[tokio::test]
    async fn a_timed_out_paint_fence_is_a_warning() {
        let mut fenced = hid();
        fenced["paintFence"] = json!("timeout");
        let (out, _) = run(
            vec![ok(fenced), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(out.frame_b, Ok("frame-b"));
        let warning = out.run.unwrap().warning.expect("a warning");
        assert!(warning.contains("paint fence timed out"), "{warning}");
    }

    /// The page was already text-hidden when the arm began: refused before
    /// anything is captured or hidden, so no frame A of a hidden page is ever
    /// returned, and the other caller's hide is not touched.
    #[tokio::test]
    async fn a_page_already_hidden_before_frame_a_is_refused_before_any_capture() {
        let (out, events) = run_with_probe(ok(already_hidden()), vec![], vec![ok("frame-a")]).await;
        let err = out.frame_a.expect_err("no frame A of a hidden page");
        assert!(err.contains("already hidden by another caller"), "{err}");
        assert!(out.frame_b.is_err());
        assert!(out.run.is_none());
        assert_eq!(events, ["permits", "probe"]);
    }

    /// No usable answer to the probe — an error, `hidden: null` (the SDK
    /// cannot tell), or no flag — is unknown, never "visible": refused the
    /// same way.
    #[tokio::test]
    async fn an_unanswered_probe_takes_no_frame_a_and_hides_nothing() {
        for probe in [
            fails("UI Bridge request timed out"),
            ok(json!({"success": true, "query": true, "hidden": null})),
            ok(json!({"success": false, "error": "x"})),
        ] {
            let (out, events) = run_with_probe(probe.clone(), vec![], vec![ok("frame-a")]).await;
            let err = out.frame_a.expect_err("unknown is not visible");
            assert!(err.contains("unknown"), "{probe:?}: {err}");
            assert!(out.run.is_none(), "{probe:?}");
            assert_eq!(events, ["permits", "probe"], "{probe:?}");
        }
    }

    /// Another caller hid the text between the probe and the hide — possibly
    /// during frame A. The hook left that hide alone (`wasHidden: true`), so
    /// frame A is refused, no frame B is taken, and nothing is restored.
    #[tokio::test]
    async fn a_hide_found_by_the_hook_refuses_frame_a_and_is_left_alone() {
        let (out, events) = run(vec![ok(theirs())], vec![ok("frame-a"), ok("frame-b")]).await;
        let err = out.frame_a.expect_err("frame A may show the hidden page");
        assert!(err.contains("another caller hid the page's text"), "{err}");
        let err = out.frame_b.expect_err("not this call's hide");
        assert!(
            err.contains("already text-hidden by another caller"),
            "{err}"
        );
        assert!(!has(&events, "show"), "{events:?}");
        assert!(!has(&events, "abort-restore"), "{events:?}");
        assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
        let run = out.run.unwrap();
        assert_eq!(run.restore, None);
        assert_eq!(run.hide, theirs());
    }

    /// Ownership is `wasHidden` alone — never a clock. A reply whose stamps
    /// would read "another caller's" under a `hiddenAt >= receivedAt` rule is
    /// ours when the hook found the text visible, and the reverse.
    #[tokio::test]
    async fn ownership_reads_was_hidden_alone_never_the_clocks() {
        let mut ours = hid();
        ours["hiddenAt"] = json!(1_000);
        ours["receivedAt"] = json!(2_000);
        let (out, events) = run(
            vec![ok(ours), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        assert_eq!(out.frame_b, Ok("frame-b"));
        assert!(has(&events, "show"), "{events:?}");

        let mut foreign = theirs();
        foreign["hiddenAt"] = json!(3_000);
        foreign["receivedAt"] = json!(2_000);
        let (out, events) = run(vec![ok(foreign)], vec![ok("frame-a"), ok("frame-b")]).await;
        assert!(out.frame_a.is_err());
        assert!(out.frame_b.is_err());
        assert!(!has(&events, "show"), "{events:?}");
    }

    /// No boolean `wasHidden`: whose hide it is is unknown, so the page is
    /// restored and no frame B is returned. Frame A stands: the probe saw the
    /// text visible before it, and the unknown concerns the hide only.
    #[tokio::test]
    async fn an_unattributable_hide_is_restored_not_measured() {
        for anon in [
            json!({"success": true, "hidden": true, "changed": true}),
            json!({"success": true, "hidden": true, "wasHidden": null}),
            json!({"success": true, "hidden": true, "hiddenAt": 2_005, "receivedAt": 2_003}),
        ] {
            let (out, events) = run(
                vec![ok(anon.clone()), ok(restored(true))],
                vec![ok("frame-a"), ok("frame-b")],
            )
            .await;
            assert!(out.frame_b.unwrap_err().contains("unknown"), "{anon}");
            assert_eq!(out.frame_a, Ok("frame-a"), "{anon}");
            assert!(has(&events, "show"), "{anon}: {events:?}");
            assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
        }
    }

    #[tokio::test]
    async fn a_cancelled_arm_restores_the_page_from_its_guard() {
        // Frame B never finishes; the caller goes away while text is hidden.
        let events = run_until_stalled_then_cancel(
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), None],
        )
        .await;
        assert_eq!(events.last().map(String::as_str), Some("abort-restore"));
        assert!(!has(&events, "show"), "{events:?}");
    }

    #[tokio::test]
    async fn a_cancel_while_the_hide_is_in_flight_still_restores() {
        let events = run_until_stalled_then_cancel(vec![None], vec![ok("frame-a")]).await;
        assert_eq!(events.last().map(String::as_str), Some("abort-restore"));
        assert!(has(&events, "hide"), "{events:?}");
    }

    #[tokio::test]
    async fn a_completed_arm_does_not_also_restore_from_its_guard() {
        let (_, events) = run(
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert!(!has(&events, "abort-restore"), "{events:?}");
    }

    /// The guard hands its restore the lock AND the capture permits, so the
    /// next arm cannot start, and no other capture can run, until that restore
    /// has finished with them.
    #[tokio::test]
    async fn the_guard_restore_carries_the_lock_and_the_permits() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let sem = Arc::new(tokio::sync::Semaphore::new(2));
        type Held = (PairLockGuard, tokio::sync::OwnedSemaphorePermit);
        let kept: Arc<Mutex<Option<Held>>> = Default::default();
        let k = kept.clone();
        {
            let permits = Arc::clone(&sem).acquire_many_owned(2).await.unwrap();
            let mut guard = ArmGuard::new(Arc::clone(&lock).lock_owned().await, permits);
            guard.arm(move |held, permits| *k.lock().unwrap() = Some((held, permits)));
        }
        assert!(
            lock.try_lock().is_err(),
            "the lock stays held by the restore the guard handed it to"
        );
        assert_eq!(
            sem.available_permits(),
            0,
            "so do the capture permits: no capture can see the hidden page"
        );
        kept.lock().unwrap().take();
        assert!(
            lock.try_lock().is_ok(),
            "released once the restore drops it"
        );
        assert_eq!(sem.available_permits(), 2);

        // Disarmed: both are released with the guard, no restore runs.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = ran.clone();
        {
            let permits = Arc::clone(&sem).acquire_many_owned(2).await.unwrap();
            let mut guard = ArmGuard::new(Arc::clone(&lock).lock_owned().await, permits);
            guard.arm(move |_, _| r.store(true, std::sync::atomic::Ordering::SeqCst));
            guard.disarm();
        }
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(lock.try_lock().is_ok());
        assert_eq!(sem.available_permits(), 2);
    }

    /// Two arms at once against one webview whose frames show whether its text
    /// is hidden: each must see its own frame A with text and frame B without,
    /// and each arm's steps (frame A, hide, frame B, restore) must run as one
    /// block — never interleaved with the other arm's.
    #[tokio::test]
    async fn concurrent_arms_do_not_interleave() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Whether the page's text is hidden.
        let hidden = Arc::new(AtomicBool::new(false));
        // Which arm took each step, in order.
        let steps: Arc<Mutex<Vec<u8>>> = Default::default();
        let arm = |id: u8, hidden: Arc<AtomicBool>| {
            let steps = steps.clone();
            async move {
                let (h1, s1) = (hidden.clone(), steps.clone());
                // The hook, over a fake engine: the probe reads, the hide reads
                // `wasHidden` before it writes, `onlyIfVisible` leaves a hidden
                // page alone.
                let send = move |_kind: &'static str, body: Value| {
                    let h = h1.clone();
                    s1.lock().unwrap().push(id);
                    async move {
                        tokio::task::yield_now().await;
                        let was = h.load(Ordering::SeqCst);
                        let reply = if body.get("query") == Some(&json!(true)) {
                            json!({"success": true, "query": true, "hidden": was})
                        } else if body["hidden"].as_bool() == Some(true) {
                            if !was {
                                h.store(true, Ordering::SeqCst);
                            }
                            json!({"success": true, "hidden": true, "changed": !was,
                                   "wasHidden": was})
                        } else {
                            h.store(false, Ordering::SeqCst);
                            json!({"success": true, "hidden": false, "changed": was,
                                   "restore": {"verified": true}})
                        };
                        tokio::task::yield_now().await;
                        Ok(reply)
                    }
                };
                let (h2, s2) = (hidden.clone(), steps.clone());
                let capture = move || {
                    let h = h2.clone();
                    s2.lock().unwrap().push(id);
                    async move {
                        tokio::task::yield_now().await;
                        let text_hidden = h.load(Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok::<bool, String>(text_hidden)
                    }
                };
                capture_text_hidden_pair(
                    send,
                    || std::future::ready(Ok::<(), String>(())),
                    capture,
                    || {},
                    |_| {},
                    |_, _| {},
                )
                .await
            }
        };
        let (one, two) = tokio::join!(arm(1, hidden.clone()), arm(2, hidden.clone()));
        let mut order = steps.lock().unwrap().clone();
        assert_eq!(order.len(), 10, "five steps per arm: {order:?}");
        order.dedup();
        assert_eq!(order.len(), 2, "the two arms' steps interleaved: {order:?}");
        for out in [one, two] {
            assert_eq!(out.frame_a, Ok(false), "frame A must show the text");
            assert_eq!(out.frame_b, Ok(true), "frame B must be the hidden page");
        }
        assert!(!hidden.load(Ordering::SeqCst), "the page ends restored");
    }

    /// An error returned before the request was emitted proves the hide never
    /// left the runner: nothing to restore, and the permits go at once. A
    /// timeout after the send, or an unknown error, is still restored.
    #[tokio::test]
    async fn a_hide_that_provably_never_left_is_not_restored() {
        for error in [
            "UI Bridge temporarily unavailable (circuit breaker open)",
            "UI Bridge concurrency limit reached (timeout acquiring permit)",
            "UI Bridge semaphore closed",
            r#"{"error":"frontend_not_ready","detail":"..."}"#,
            "No runner window labeled 'x'. Discover live windows via ...",
            "Failed to serialize UiBridgeRequestEnvelope: key must be a string",
        ] {
            assert!(hide_never_sent(error), "{error}");
            let (out, events) = run(vec![fails(error)], vec![ok("frame-a")]).await;
            let err = out.frame_b.expect_err(error);
            assert!(err.contains("never left the runner"), "{error}: {err}");
            assert!(!has(&events, "show"), "{error}: {events:?}");
            assert!(!has(&events, "abort-restore"), "{error}: {events:?}");
            assert_eq!(out.frame_a, Ok("frame-a"), "{error}");
        }
        for error in [
            "UI Bridge request timed out after 10000ms",
            // An emit can fail after a partial delivery: restored.
            "Failed to emit UI Bridge request: closed",
            "something else",
        ] {
            assert!(!hide_never_sent(error), "{error}");
            let (_, events) =
                run(vec![fails(error), ok(restored(true))], vec![ok("frame-a")]).await;
            assert!(has(&events, "show"), "{error}: {events:?}");
        }
    }

    /// Every pre-send message `hide_never_sent` matches — the real
    /// [`BEFORE_SEND`], not a copy — is one the UI Bridge request path actually
    /// returns: reword one there and this fails.
    #[test]
    fn the_pre_send_messages_still_exist_where_they_are_produced() {
        let request = include_str!("request.rs");
        let breaker = include_str!("circuit_breaker.rs");
        for message in BEFORE_SEND {
            assert!(
                request.contains(message) || breaker.contains(message),
                "neither request.rs nor circuit_breaker.rs says {message}"
            );
        }
        assert!(
            !BEFORE_SEND.iter().any(|m| m.contains("Failed to emit")),
            "an emit failure may follow a partial delivery: it is restored"
        );
    }

    /// The production call sites, pinned by source: the guard path is wired to
    /// a real, lock- and permit-carrying restore, and the direct route's TTL
    /// bump to the real scheduler.
    #[test]
    fn the_direct_route_wires_the_real_ttl_bump() {
        let src = include_str!("text_visibility.rs");
        let (_, body) = src
            .split_once("pub async fn ui_bridge_text_visibility_handler(")
            .expect("handler");
        let (body, _) = body.split_once("\n}\n").expect("handler ends");
        assert!(
            body.contains("schedule_mutation_bump(&ttl_state, delay_ms)"),
            "{body}"
        );
    }

    #[test]
    fn the_run_record_serializes_camel_case_and_omits_what_was_not_sent() {
        let run = TextVisibilityRun {
            hide: json!({"success": false}),
            restore: None,
            page_restored: None,
            warning: None,
        };
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            json!({"hide": {"success": false}})
        );
        let run = TextVisibilityRun {
            hide: hid(),
            restore: Some(restored(true)),
            page_restored: Some(true),
            warning: Some("w".into()),
        };
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["pageRestored"], json!(true));
        assert_eq!(v["warning"], json!("w"));
        assert_eq!(v["restore"]["restore"]["verified"], json!(true));
    }
}
