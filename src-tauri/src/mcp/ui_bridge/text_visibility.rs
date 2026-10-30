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
///
/// The delay is counted from when the hide's REPLY arrived, never from when it
/// was sent: the SDK arms its timer while handling the hide, so by the time the
/// reply is here the timer is running, and reply + TTL + margin is past the
/// moment an on-time timer fires. A webview timer can fire LATE (a hidden
/// page's timers are aligned to coarse wake-ups, a busy event loop runs a due
/// timer only once it frees), which is what [`TTL_BUMP_LATE_EXTRA_MS`] covers.
pub(crate) const TTL_BUMP_MARGIN_MS: u64 = 1_000;

/// The second scheduled bump, this long after the first: it retires a capture
/// cached under the first bump's id when the SDK's TTL restore ran late (see
/// [`TTL_BUMP_MARGIN_MS`]). Not covered, and for the same reason — the page
/// changes after both bumps, with no request this runner sees:
/// - a restore later still: a webview whose timers are paused outright;
/// - a HIDE the webview runs late: a stalled webview whose hide request timed
///   out here (the runner's UI Bridge timeout) and whose restores then never
///   left (the circuit breaker opened), so the arm held the lock and the
///   permits until its reply time + TTL + margin and released them — and the
///   webview later runs the queued hide, which hides the page for a full TTL
///   after both bumps.
///
/// A capture made in either window is served under the second bump's id until
/// the next mutation.
pub(crate) const TTL_BUMP_LATE_EXTRA_MS: u64 = 4_000;

/// When a body that HIDES text should have its TTL-restore bump scheduled,
/// counted from the hide's reply: the TTL it names (or the SDK default) plus
/// [`TTL_BUMP_MARGIN_MS`]. `None` for a restore, which has no TTL.
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

/// Run `bump` after `delay_ms`, and again [`TTL_BUMP_LATE_EXTRA_MS`] later:
/// the TTL-restore bumps [`ttl_bump_delay_ms`] times, as one future.
/// [`schedule_mutation_bump`] spawns it over the real mutation id.
pub(crate) async fn ttl_bumps(delay_ms: u64, bump: impl Fn()) {
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    bump();
    tokio::time::sleep(std::time::Duration::from_millis(TTL_BUMP_LATE_EXTRA_MS)).await;
    bump();
}

/// Bump `state`'s vision mutation id after `delay_ms`, and again
/// [`TTL_BUMP_LATE_EXTRA_MS`] later ([`ttl_bumps`]), in one spawned task.
pub(crate) fn schedule_mutation_bump(state: &Arc<ApiState>, delay_ms: u64) {
    let state = Arc::clone(state);
    tokio::spawn(ttl_bumps(delay_ms, move || {
        super::vision_routes::bump_mutation_id(&state)
    }));
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

/// Frame B is kept only when it FINISHED at least this long before the
/// earliest moment the SDK's TTL can restore the page: the hide's send plus
/// [`CAPTURE_TTL_MS`]. Slack for the webview's and the runner's clocks
/// measuring the same interval differently.
pub(crate) const TTL_FRAME_B_MARGIN_MS: u64 = 1_000;
const _: () = assert!(TTL_FRAME_B_MARGIN_MS < CAPTURE_TTL_MS);

/// The pause between two attempts at a restore that never left the runner
/// ([`hide_never_sent`]: the circuit breaker open, the UI Bridge concurrency
/// gate full, …). Retried while the next attempt can still land before the
/// arm's TTL would restore the page anyway.
pub(crate) const RESTORE_RETRY_DELAY_MS: u64 = 250;

/// A JSON number the SDK's `Number.isInteger` accepts, as a `u64`: `5000` and
/// `5000.0` alike. A fraction, a negative or a non-number is `None`.
fn whole_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && (0.0..=u64::MAX as f64).contains(f))
            .map(|f| f as u64)
    })
}

/// Fields only [`capture_text_hidden_pair`] sends to the hook
/// ([`probe_body`], [`arm_hide_body`]). [`validate_text_visibility_body`]
/// refuses them, so neither caller-facing route accepts them: the direct route
/// answers 400, the `/sdk/` route `{success: false, code: "INVALID_REQUEST"}`.
pub(crate) const ARM_ONLY_FIELDS: [&str; 2] = ["query", "onlyIfVisible"];

/// Validate a `POST /control/text-visibility` body. Mirrors the SDK rules:
/// `hidden` is a boolean; `ttlMs` is checked only on a hide (a show has no TTL,
/// so whatever a caller sends along with `hidden: false` is ignored, not
/// refused).
pub(crate) fn validate_text_visibility_body(body: &Value) -> Result<(), String> {
    let obj = body.as_object().ok_or("body must be a JSON object")?;
    // The runner arm's own fields, refused from any caller: forwarded, `query`
    // would turn a restore into a probe that answers success, and
    // `onlyIfVisible` would change what an explicit hide does.
    for arm_only in ARM_ONLY_FIELDS {
        if obj.contains_key(arm_only) {
            return Err(format!(
                "`{arm_only}` is not accepted: `query` and `onlyIfVisible` are reserved for \
                 the runner's internal capture arm"
            ));
        }
    }
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
///
/// A valid body then takes the runner arm's lock ([`pair_lock`]) and holds it
/// for the whole change, bumps and send included: a direct hide or restore
/// waits for a running `captureTextHidden` arm (up to its TTL hold, when the
/// arm's restore failed) and never lands between that arm's hide and its
/// restore, where a restore-then-re-hide would make the arm's frame B show
/// text and its restore answer `changed: true`.
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
    let _arm_lock = pair_lock().lock_owned().await;
    info!(
        "UI Bridge API: text visibility hidden={}",
        body["hidden"].as_bool().unwrap_or_default()
    );
    let ttl_bump = ttl_bump_delay_ms(&body);
    // Rendered pixels change between these two bumps; a capture keyed on the
    // mutation id from either side of them is not reused across the change.
    bump();
    let result = send(REQUEST_TYPE, body).await;
    bump();
    // A hide's TTL restores the page with no request this runner sees: its
    // bump is counted from the reply, by which time the SDK's timer runs
    // (see `TTL_BUMP_MARGIN_MS`).
    if let Some(delay_ms) = ttl_bump {
        bump_after_ttl(delay_ms);
    }
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
    /// into dev controls), or `{success: false, error}` when the request
    /// failed here: it never left the runner ([`hide_never_sent`]), or it
    /// errored or timed out after it may have reached the SDK — then whether
    /// it hid anything is unknown, and the page was restored (see `restore`).
    /// When the probe already answered `devControls: false`, no hide is sent
    /// at all: this is then the runner's own record of that refusal
    /// ([`dev_controls_off_refusal`], `hideSent: false`).
    pub hide: Value,
    /// What the webview answered to the restore, the SDK's `{hidden: false,
    /// changed, restore: {verified, …}}`, or `{success: false, error,
    /// attempts}` when no attempt got an answer. `None` only when no restore
    /// was sent: the hide was a typed refusal (`success: false` with a `code`
    /// or a boolean `wasHidden`), its request never left the runner, or the
    /// text was already hidden by another caller (`wasHidden: true`) — then
    /// the hook did not touch that hide at all (not even its TTL), and it is
    /// left to its caller. A hide whose fate is unknown — its request failed
    /// after it may have been sent, or its reply is neither a hide nor a typed
    /// refusal — IS restored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<Value>,
    /// True only when a restore was sent and the SDK verified it
    /// (`restore.verified: true`); false when it was sent and not verified or
    /// failed; `None` when none was sent (see `restore`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_restored: Option<bool>,
    /// Something the caller should know beyond frame B's own verdict: the hide
    /// answered after its paint fence TIMED OUT, so frame B may have been
    /// composited before the glyphs went transparent (vision-core then reads
    /// the glyphs as still painted and answers unknown, never a pass); or a
    /// hide that threw part-way was not provably rolled back (`HIDE_FAILED`
    /// with `rolledBack: null`, or a `rolledBack` that is not `verified`), so
    /// the page may be left partly text-hidden — the SDK cleared its state, so
    /// no restore can reach what is left.
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

/// Whether a restore reply shows the SDK restored the page and verified it:
/// `{success: true, hidden: false, restore: {verified: true}}`. The arm's and
/// the cancel path's one reading of a restore.
pub(crate) fn restore_verified(restore: &Value) -> bool {
    is_true(restore, "success")
        && restore.get("hidden").and_then(Value::as_bool) == Some(false)
        && restore
            .get("restore")
            .is_some_and(|r| is_true(r, "verified"))
}

/// Whether a `success: false` hide reply is a TYPED refusal — one the hook or
/// the SDK answered on purpose, carrying a `code` or the hook's boolean
/// `wasHidden` — so that what it says about the page can be believed. A
/// `success: false` with neither is the hook's own catch-all (it threw: its
/// reply is the bare `{success: false, error}` envelope) and says nothing
/// about the page: whether a hide landed is UNKNOWN, so it is restored.
fn is_typed_refusal(hide: &Value) -> bool {
    hide.get("success").and_then(Value::as_bool) == Some(false)
        && (hide.get("code").and_then(Value::as_str).is_some()
            || hide.get("wasHidden").and_then(Value::as_bool).is_some())
}

/// A `HIDE_FAILED` whose rollback did not prove the page restored: the
/// rollback threw too (`rolledBack: null`, where the SDK says the page may be
/// partly hidden), or it ran and did not verify (`rolledBack.verified` not
/// `true`: a read-back mismatch, a leftover sheet, markers it could not vouch
/// for or restore), or the reply carries no rollback at all. The SDK cleared
/// its state either way, so a restore cannot reach what is left. `None` for
/// every other refusal, and for a rollback that verified.
fn partial_hide_warning(hide: &Value) -> Option<String> {
    if hide.get("code").and_then(Value::as_str) != Some("HIDE_FAILED") {
        return None;
    }
    let why = match hide.get("rolledBack") {
        Some(Value::Null) => "its rollback threw too (rolledBack: null)",
        Some(r @ Value::Object(_)) if is_true(r, "verified") => return None,
        Some(Value::Object(_)) => "its rollback did not verify (rolledBack.verified is not true)",
        _ => "its reply carries no rollback record",
    };
    Some(format!(
        "the hide threw part-way (HIDE_FAILED) and {why}: the page may be left partly \
         text-hidden, and the SDK cleared its state, so no restore can reach what is left"
    ))
}

/// Whether the probe says this webview has not opted into dev controls
/// (`devControls: false`, which only the hook's dev-controls-off arm sends):
/// nothing can be hidden there, so the arm sends no hide at all. An older hook
/// sends no such field, and the arm then learns it from the hide's refusal.
fn probe_says_dev_controls_off(probe: Option<&Value>) -> bool {
    probe
        .and_then(|v| v.get("devControls"))
        .and_then(Value::as_bool)
        == Some(false)
}

/// The run record's `hide` when the probe found dev controls off and no hide
/// was sent: the dev-controls refusal, in the SDK's vocabulary, marked
/// `hideSent: false` so it is not read as the webview's answer to a hide.
pub(crate) fn dev_controls_off_refusal() -> Value {
    serde_json::json!({
        "success": false,
        "code": "capability_unavailable_in_build",
        "reason": "dev_controls_disabled",
        "error": "the runner webview has not opted into dev controls (its probe answered \
                  devControls: false), so no hide was sent",
        "hideSent": false,
    })
}

/// Whether a REFUSED hide still leaves frame A proven: the hook read
/// `wasHidden: false` before the SDK refused, or the refusal is the hook's own
/// dev-controls one (`capability_unavailable_in_build`, which carries no
/// `wasHidden`: nothing can be hidden in that realm). `wasHidden: true`, a
/// null, or a missing one on any other refusal leaves the same unknown as a
/// lost `wasHidden`.
fn refusal_keeps_frame_a(hide: &Value) -> bool {
    match hide.get("wasHidden") {
        Some(Value::Bool(was)) => !was,
        None => hide.get("code").and_then(Value::as_str) == Some("capability_unavailable_in_build"),
        Some(_) => false,
    }
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
/// second's frame B — and both would measure the wrong pixels. The direct
/// route ([`direct_set`]) takes it too, so a direct change never lands inside
/// an arm. An `Arc`'d mutex so the drop guard's spawned restore can carry the
/// lock with it: the next arm then cannot take its frame A until that restore
/// has landed.
fn pair_lock() -> Arc<tokio::sync::Mutex<()>> {
    static LOCK: std::sync::OnceLock<Arc<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();
    Arc::clone(LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(()))))
}

/// The lock the arm holds, as handed to its restore-on-drop callback.
pub(crate) type PairLockGuard = tokio::sync::OwnedMutexGuard<()>;

/// When the arm's hide was sent and when its reply came: what the restore
/// path counts from ([`restore_until_known`]). `sent` is taken before the hide
/// is sent, so no SDK timer for it can start earlier; `answered` is `None`
/// while the reply is not in — a cancel during the hide's request — and the
/// TTL hold is then counted from the restore's own start (the cancel), the
/// latest instant this runner has for "an SDK timer that handled the hide is
/// running by now" (a webview that runs the hide later still is the remainder
/// [`TTL_BUMP_LATE_EXTRA_MS`] names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HideTimes {
    pub sent: tokio::time::Instant,
    pub answered: Option<tokio::time::Instant>,
}

/// Holds the arm's lock AND its capture permits for the whole call and, while
/// ARMED, restores the page if dropped: the arm's future cancelled (the HTTP
/// client went away) or a panic unwinding through it. The restore callback is
/// handed both, with the hide's [`HideTimes`], so the restore it spawns
/// ([`restore_after_cancel`]) releases them only once the page is known
/// restored — or the TTL has had time to restore it — and the id is bumped:
/// until then no other arm can start and no other VISION capture can see the
/// text-hidden page (and cache it under the post-hide mutation id). Captures
/// that take no permit are not held off — see [`capture_text_hidden_pair`]'s
/// `acquire`.
struct ArmGuard<P, R: FnOnce(PairLockGuard, P, HideTimes)> {
    restore: Option<(R, HideTimes)>,
    held: Option<(PairLockGuard, P)>,
}

impl<P, R: FnOnce(PairLockGuard, P, HideTimes)> ArmGuard<P, R> {
    fn new(lock: PairLockGuard, permits: P) -> Self {
        Self {
            restore: None,
            held: Some((lock, permits)),
        }
    }

    /// Armed from just before the hide is sent, at `sent`.
    fn arm(&mut self, restore: R, sent: tokio::time::Instant) {
        self.restore = Some((
            restore,
            HideTimes {
                sent,
                answered: None,
            },
        ));
    }

    /// The hide's reply came at `at`: a cancel from here on holds counting
    /// from it.
    fn answered(&mut self, at: tokio::time::Instant) {
        if let Some((_, times)) = self.restore.as_mut() {
            times.answered = Some(at);
        }
    }

    fn disarm(&mut self) {
        self.restore = None;
    }
}

impl<P, R: FnOnce(PairLockGuard, P, HideTimes)> Drop for ArmGuard<P, R> {
    fn drop(&mut self) {
        if let (Some((restore, times)), Some((lock, permits))) =
            (self.restore.take(), self.held.take())
        {
            warn!("vision: the runner arm ended before its restore; restoring the webview's text");
            restore(lock, permits, times);
        }
    }
}

/// The arm's restore, on the main path and on a cancel alike: `{hidden:
/// false}` is sent, and one that never left the runner ([`hide_never_sent`]:
/// it changed nothing, so it is safe to send again) is retried every
/// [`RESTORE_RETRY_DELAY_MS`] for as long as the next attempt can still land
/// before the TTL would restore the page anyway (`times.sent` +
/// [`CAPTURE_TTL_MS`]). When no attempt got a `success: true` reply, the page
/// may stay text-hidden until the TTL, so this then waits until the TTL has
/// fired — `times.answered` (or, with no reply, this call's start) +
/// [`CAPTURE_TTL_MS`] + [`TTL_BUMP_MARGIN_MS`] — before returning: a caller
/// holding the lock and the permits holds them that long, and no other vision
/// capture sees (or caches) the hidden page. Returns the last reply, or
/// `{success: false, error, attempts}`.
pub(crate) async fn restore_until_known<S, F>(send: &S, times: HideTimes) -> Value
where
    S: Fn(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
{
    let start = tokio::time::Instant::now();
    let ttl = std::time::Duration::from_millis(CAPTURE_TTL_MS);
    let retry_delay = std::time::Duration::from_millis(RESTORE_RETRY_DELAY_MS);
    let retry_until = times.sent + ttl;
    let mut attempts: u32 = 0;
    let restore = loop {
        attempts += 1;
        match send(REQUEST_TYPE, serde_json::json!({"hidden": false})).await {
            Ok(v) => break v,
            Err(e)
                if hide_never_sent(&e)
                    && tokio::time::Instant::now() + retry_delay < retry_until =>
            {
                warn!(
                    "vision: the runner webview's text restore never left the runner \
                     (attempt {attempts}): {e}; retrying"
                );
                tokio::time::sleep(retry_delay).await;
            }
            Err(e) => break serde_json::json!({"success": false, "error": e, "attempts": attempts}),
        }
    };
    if !is_true(&restore, "success") {
        warn!(
            "vision: the runner webview's text restore failed ({restore}); holding the \
             capture permits until the {CAPTURE_TTL_MS} ms TTL has restored the page"
        );
        tokio::time::sleep_until(
            times.answered.unwrap_or(start)
                + ttl
                + std::time::Duration::from_millis(TTL_BUMP_MARGIN_MS),
        )
        .await;
    }
    restore
}

/// What the arm's drop guard spawns on a cancel (see [`ArmGuard`]): the same
/// [`restore_until_known`] as the main path — retrying a restore that never
/// left, and holding until the TTL when none answered — then `bump`, and only
/// then are `permits` and `lock` released. A restore that was not verified is
/// logged, and `bump_after_ttl` is handed the time left until the TTL has
/// fired (counted as the hold counts it; zero once the hold has waited it out)
/// so the TTL-restore bumps ([`ttl_bumps`]) retire a capture cached before a
/// late TTL restore. Returns the restore's reply.
pub(crate) async fn restore_after_cancel<S, F, B, T, P>(
    send: S,
    bump: B,
    bump_after_ttl: T,
    lock: PairLockGuard,
    permits: P,
    times: HideTimes,
) -> Value
where
    S: Fn(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
    B: FnOnce(),
    T: FnOnce(u64),
{
    let start = tokio::time::Instant::now();
    let restore = restore_until_known(&send, times).await;
    bump();
    if !restore_verified(&restore) {
        warn!(
            "vision: the cancelled runner arm's text restore was not verified ({restore}); \
             the {CAPTURE_TTL_MS} ms TTL restores the page"
        );
        let ttl_fired = times.answered.unwrap_or(start)
            + std::time::Duration::from_millis(CAPTURE_TTL_MS + TTL_BUMP_MARGIN_MS);
        let left = ttl_fired.saturating_duration_since(tokio::time::Instant::now());
        bump_after_ttl(u64::try_from(left.as_millis()).unwrap_or(u64::MAX));
    }
    drop(permits);
    drop(lock);
    restore
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

/// Why a probe answering `hidden: false` still does not prove frame A would
/// show the text: a restore writes its state at once and paints later, so the
/// answer counts only once the hook's paint fence released on a painted frame
/// (`animation_frame`), or the hook said no fence was needed (`not_needed`:
/// the SDK's last restore is older than the hook's window, or none happened,
/// or dev controls are off and nothing can have been hidden in that realm).
/// `None` when it is proven.
fn probe_fence_unproven(probe: Option<&Value>) -> Option<String> {
    match probe
        .and_then(|v| v.get("paintFence"))
        .and_then(Value::as_str)
    {
        Some("animation_frame" | "not_needed") => None,
        Some(other) => Some(format!(
            "whether the page's text is painted visible is unknown (the probe's paint fence \
             answered {other:?}, so a restore may not have been painted yet)"
        )),
        None => Some(
            "whether the page's text is painted visible is unknown (the probe carries no \
             paint fence)"
                .to_string(),
        ),
    }
}

/// The probe the arm sends before frame A: the hook answers `{hidden,
/// paintFence}` from `textHidden()`, with no page-state change. It reads on
/// both sides of a paint fence only while a restore may still be unpainted (a
/// hidden page, or the SDK's last restore within the hook's window); otherwise
/// it answers at once with `paintFence: "not_needed"`.
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
/// The arm reads a failed RESTORE the same way: one that never left the runner
/// changed nothing, so it is safe to send again, and is retried.
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
///   production, so no other vision capture (the `vision/*` routes, which take
///   a permit in `vision_routes::capture_runner_window`) can see the page while
///   its text is hidden. They are held until the restore has answered, and
///   past that until the TTL when no restore answered `success: true`
///   ([`restore_until_known`]) — on a cancel too: the guard's spawned restore
///   ([`restore_after_cancel`]) runs the same retry and hold before it
///   releases them, counting the hold from the hide's reply or, when the
///   cancel came before it, from the cancel. NOT covered, because they
///   take no permit: `screenshots::capture_runner_window_base64` (the snapshot
///   and error fallbacks in `elements.rs`, `errors.rs` and `mcp_api.rs`),
///   exploration's and streaming's xcap window grabs, and a monitor capture
///   through the screen HAL — any of them can capture the text-hidden page. A
///   DOM read — a snapshot, a discover — during the hide is not covered
///   either: it reads the DOM, not pixels, and the hide changes only paint
///   properties, but its computed colours would read transparent.
/// - `capture` takes one frame of the runner window, the permits already held:
///   frame A, and — only after the webview confirmed THIS call's hide — frame B.
/// - `bump` bumps the vision mutation id: before the hide and after the restore.
/// - `bump_after_ttl` schedules the bump for the SDK's own TTL restore, once
///   the hide's reply has arrived (or its request failed after it may have
///   been sent): counted from then, it cannot fire before an on-time SDK timer.
/// - `abort_restore` restores the page if this future is cancelled or unwinds
///   while armed. It is handed the lock, the permits and the hide's
///   [`HideTimes`] (`answered: None` when the cancel came before the hide's
///   reply), and must spawn [`restore_after_cancel`] holding both — which
///   releases them only after the retry, the hold and the bump — and not
///   block.
///
/// Before frame A the webview is asked whether its text is already hidden
/// ([`probe_body`]). Only `hidden: false` behind a painted fence, or with
/// `paintFence: "not_needed"` (no recent restore, or dev controls off), proceeds
/// ([`probe_fence_unproven`]): hidden, a timed-out or missing fence, or no
/// usable answer refuses at once — no frame A, no hide, nothing to restore.
/// The probe is one more webview round trip, and it runs with every capture
/// permit held: on a stalled webview, other vision captures wait for it up to
/// the UI Bridge request timeout. A probe answering `devControls: false` (the
/// webview never opted in, so nothing can be hidden in it) gets frame A and
/// nothing else: no hide, no mutation bumps, no TTL bump, frame B refused with
/// `capability_unavailable_in_build` ([`dev_controls_off_refusal`]).
///
/// The hide ([`arm_hide_body`]) then says whose hide it is ([`hide_is_ours`]):
/// - THIS call's: frame B is taken and the page restored, whether or not frame
///   B was captured. A restore that never left the runner ([`hide_never_sent`])
///   is retried until the TTL would beat it. A restore that fails or is not
///   verified is reported in [`TextVisibilityRun::page_restored`] and frame B
///   is kept: what it shows was fixed when it was taken. One that FAILED (no
///   `success: true` reply at all) may have left the page hidden, so the lock
///   and the permits are then held until the TTL has restored it, and the id
///   bumped. Frame B is discarded when the SDK's TTL may have restored the page
///   before it was taken: the restore answered `changed: false` (the TTL got
///   there first), or frame B finished within [`TTL_FRAME_B_MARGIN_MS`] of the
///   earliest moment the TTL can fire — counted from when the hide was SENT,
///   which no SDK timer can precede. The time check catches what the reply
///   cannot: a restore that errored, or one answering `changed: true` because a
///   hide from outside the runner re-hid the page after the TTL restore.
/// - Another caller's (`wasHidden: true`): it appeared after the probe, so it
///   may have covered frame A. Frame A is refused, no frame B is taken, and the
///   hide — which the hook did not touch — is not restored.
/// - UNKNOWN — the hide request errored or timed out after it may have been
///   sent, the call was cancelled before the hide reply, the reply carries no
///   `wasHidden`, or it is neither a hide nor a typed refusal (a `success:
///   false` with no `code` and no `wasHidden`: the hook's own catch-all, which
///   says nothing about the page — [`is_typed_refusal`]): the page is RESTORED
///   (holding the lock and permits), then the id bumped, and no frame B is
///   returned. A restore is idempotent, so restoring a hide that never landed
///   costs one request. Frame A is refused too: the lost `wasHidden` is the
///   only thing that could have shown another caller's hide landing after the
///   probe, during frame A. The arm's TTL is short, and a page left
///   text-hidden poisons every capture until it fires; the one hide this can
///   wrongly undo is one made in the webview by a door outside the runner
///   (devtools, page script) — a direct-route (`control/text-visibility`)
///   hide waits on the arm's lock and cannot be concurrent.
/// - A TYPED refusal (`success: false` with a `code` or a boolean
///   `wasHidden`), or a request that provably never left the runner
///   ([`hide_never_sent`]): nothing is restored, and the permits are released
///   at once. A typed refusal hid nothing — except a `HIDE_FAILED` whose
///   rollback is not proven (`rolledBack: null`, where the SDK's own rollback
///   threw, or a `rolledBack` that is not `verified`): the page may be left
///   PARTLY hidden; the SDK cleared its state, so a restore cannot reach it,
///   and the run record carries a warning ([`partial_hide_warning`]).
///   Frame A is refused here too unless the hook's answer proves it
///   ([`refusal_keeps_frame_a`]): a never-sent hide has no answer at all, and a
///   refusal must carry `wasHidden: false` or be the dev-controls refusal —
///   otherwise nothing checked whether another caller hid the page after the
///   probe (the UNKNOWN case's reason).
///
/// Accepted remainder — frame A may show a text-hidden page, unseen, when:
/// - a hide by another caller appears after the probe AND is gone again before
///   the hook reads `wasHidden` (made and restored, or expired, entirely while
///   frame A was being captured);
/// - another caller's hide AND its restore both land inside the probe's own
///   paint-fence wait, so the state reads the same on both sides of it;
/// - the probe skipped its fence because the last restore was older than the
///   hook's window (`RECENT_RESTORE_MS`), yet a webview with paused frames had
///   still not painted it — or the webview's wall clock stepped forward past
///   the window.
///
/// Only the dev-only direct route can hide this webview's text besides this
/// arm, and the arm's lock keeps two arms apart — and keeps the direct route
/// out of an arm: [`direct_set`] takes the same lock, so a direct restore and
/// re-hide cannot land between this arm's hide and its restore (which would
/// leave frame B showing text with the restore answering `changed: true`). A
/// caller hiding the page through the webview's SDK by some other door
/// (devtools, page script) is not serialized; vision-core's still-painted and
/// not-hidden checks then read such a frame B as unknown, not as a pass.
///
/// A minimized or occluded dev runner does not get a usable pair: its webview
/// paints no frames, so the HIDE's paint fence times out (reported in
/// [`TextVisibilityRun::warning`]; vision-core then reads frame B as still
/// painting its glyphs and answers unknown), and a probe right after a restore
/// refuses on its own timed-out fence. What such a runner does get is frame A
/// when no restore is recent: the probe then answers `not_needed` without
/// waiting on a paint.
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
    R: FnOnce(PairLockGuard, P, HideTimes),
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
        Ok(Some(Value::Bool(false))) => probe_fence_unproven(probe.as_ref().ok()),
        Ok(Some(Value::Bool(true))) => Some(
            "the page's text was already hidden by another caller, so frame A would not show \
             it"
            .to_string(),
        ),
        Ok(Some(Value::Null)) => Some(
            "whether the page's text was already hidden is unknown (its state differed \
             before and after the probe's paint fence, or the SDK could not tell)"
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

    // Read before frame A, used after it: the probe already said whether this
    // webview can hide anything at all.
    let dev_controls_off = probe_says_dev_controls_off(probe.as_ref().ok());

    let mut frame_a = capture().await;
    if frame_a.is_err() {
        // Altering the page for a frame B nothing can be paired with buys nothing.
        return TextHiddenPair {
            frame_a,
            frame_b: not_taken("frame A could not be captured"),
            run: None,
        };
    }

    if dev_controls_off {
        // Nothing can be hidden in a realm that never opted in (the gate is
        // one-way), so frame A stands and no hide is sent: nothing changes on
        // the page, so there is nothing to bump and no TTL to wait out. The
        // guard is not armed yet; dropping it releases the lock and permits.
        let hide = dev_controls_off_refusal();
        return TextHiddenPair {
            frame_a,
            frame_b: Err(hide_failure_message(&hide)),
            run: Some(TextVisibilityRun {
                hide,
                restore: None,
                page_restored: None,
                warning: None,
            }),
        };
    }

    let hide_body = arm_hide_body();
    let ttl_bump = ttl_bump_delay_ms(&hide_body);
    // Armed BEFORE the send: whether the hide landed is unknown until its reply,
    // and an unknown hide is restored (see the doc above) — on a cancel, by the
    // guard, still holding the lock and the permits. That restore may undo a
    // hide made in the webview by a door outside the runner; it is the price
    // of never leaving the page text-hidden for the arm's TTL.
    //
    // No SDK timer for this hide can start before this instant, so its TTL
    // cannot restore the page before `hide_sent_at + CAPTURE_TTL_MS`.
    let hide_sent_at = tokio::time::Instant::now();
    guard.arm(abort_restore, hide_sent_at);
    bump();
    let hide_reply = send(REQUEST_TYPE, hide_body).await;
    // ...and by this one an SDK timer that handled the hide is running: a
    // cancel from here on counts its TTL hold from it.
    let hide_answered_at = tokio::time::Instant::now();
    guard.answered(hide_answered_at);
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
    let hid = landed && is_true(&hide, "success") && is_true(&hide, "hidden");
    let refused = landed && !hid && is_typed_refusal(&hide);
    if never_sent || refused {
        // The request never left the runner, or the webview answered with a
        // typed refusal: nothing a restore could undo was hidden, so nothing is
        // restored, and the lock and permits go at once. (A HIDE_FAILED whose
        // rollback threw or did not verify may have left the page partly
        // hidden; the SDK cleared its state, so a restore cannot reach it —
        // reported as a warning.)
        guard.disarm();
        let error = if never_sent {
            never_sent_message(&hide)
        } else {
            hide_failure_message(&hide)
        };
        let warning = if never_sent {
            None
        } else {
            partial_hide_warning(&hide)
        };
        // Unsent: no hook answer, so nothing checked whether another caller hid
        // the page after the probe, during frame A — the same unknown as a lost
        // `wasHidden`.
        if never_sent {
            frame_a = Err(
                "frame A was not kept: the hide request never left the runner, so whether \
                 another caller's hide covered frame A is unknown"
                    .into(),
            );
        } else if !refusal_keeps_frame_a(&hide) {
            frame_a = Err(format!(
                "frame A was not kept: the hide was refused, and its reply does not show the \
                 text was visible when it was asked (wasHidden: {}), so another caller's hide \
                 may have covered frame A",
                hide.get("wasHidden")
                    .map_or("absent".into(), Value::to_string)
            ));
        }
        return TextHiddenPair {
            frame_a,
            frame_b: Err(error),
            run: Some(TextVisibilityRun {
                hide,
                restore: None,
                page_restored: None,
                warning,
            }),
        };
    }
    // The page may now be text-hidden — by this hide, or by one whose fate is
    // unknown — so the SDK's TTL restore gets its bump, counted from the reply.
    if let Some(delay_ms) = ttl_bump {
        bump_after_ttl(delay_ms);
    }
    let ours = if hid { hide_is_ours(&hide) } else { None };
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

    // Unknown: a hide by another caller may have landed after the probe and
    // covered frame A (its `wasHidden: true` would have said so), so frame A
    // is not kept either.
    let unknown_why = if !landed {
        "the hide request failed, so whether it landed is unknown"
    } else if hid {
        "whose hide it is is unknown (the reply carries no wasHidden)"
    } else {
        "the hide's reply is neither a hide nor a typed refusal (a refusal names a code or \
         wasHidden), so whether it landed is unknown"
    };
    if ours.is_none() {
        frame_a = Err(format!(
            "frame A was not kept: {unknown_why}, so another caller's hide may have covered \
             frame A"
        ));
    }

    // Ours: frame B. Unknown: none — restore only.
    let mut frame_b = match ours {
        Some(true) => capture()
            .await
            .map_err(|e| format!("frame B (text hidden) capture failed: {e}")),
        _ => not_taken(&format!(
            "{unknown_why}; a restore was sent (see pageRestored)"
        )),
    };
    // Frame B must be done before the SDK's TTL could have restored the page:
    // the restore's reply cannot always say (it errored, or a re-hide after the
    // TTL restore makes it answer `changed: true`), the clock can.
    if frame_b.is_ok() {
        let finished = hide_sent_at.elapsed();
        if finished >= std::time::Duration::from_millis(CAPTURE_TTL_MS - TTL_FRAME_B_MARGIN_MS) {
            frame_b = Err(format!(
                "frame B discarded: it finished {} ms after the hide was sent, within \
                 {TTL_FRAME_B_MARGIN_MS} ms of (or past) the {CAPTURE_TTL_MS} ms TTL, so the \
                 SDK's own restore may have come before it",
                finished.as_millis()
            ));
        }
    }

    // The permits are still held, and the guard still armed: a cancel during
    // the restore, its retries or its TTL hold hands the guard's restore the
    // same times, and it runs the same retry and hold before releasing them.
    let restore = restore_until_known(
        &send,
        HideTimes {
            sent: hide_sent_at,
            answered: Some(hide_answered_at),
        },
    )
    .await;
    guard.disarm();
    bump();
    let verified = restore_verified(&restore);
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

    /// The arm's own fields are refused on the direct route: `query` would turn
    /// a restore into a probe answering 200, `onlyIfVisible` would change what
    /// an explicit hide does.
    #[tokio::test]
    async fn the_direct_route_refuses_the_arm_only_fields() {
        for body in [
            json!({"hidden": false, "query": true}),
            json!({"hidden": true, "query": false}),
            json!({"hidden": true, "onlyIfVisible": true}),
            json!({"hidden": false, "onlyIfVisible": false}),
        ] {
            let (sent, send) = webview(vec![]);
            let (status, Json(reply)) = direct_set(body.clone(), send, || {}, |_| {})
                .await
                .expect_err("must be refused");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let error = reply.error.unwrap_or_default();
            assert!(
                error.contains("reserved for the runner's internal capture arm"),
                "{body}: {error}"
            );
            assert!(
                sent.lock().unwrap().is_empty(),
                "{body}: nothing may reach the webview"
            );
        }
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

    /// `route_entries()` against what `routes()` really mounts, read from this
    /// file's source (axum 0.8 has no router introspection; the same stop-gap
    /// as `mod.rs`'s manifest test): every `.route(` call's path and method,
    /// in order, must be exactly the listed entries.
    #[test]
    fn route_entries_match_the_router() {
        let src = include_str!("text_visibility.rs");
        let (_, rest) = src.split_once("\npub fn routes() ").expect("routes()");
        let (body, _) = rest.split_once("\n}\n").expect("routes() ends");
        let mounted: Vec<(String, String)> = body
            .split(".route(")
            .skip(1)
            .map(|call| {
                let path = call.split('"').nth(1).expect("a path literal");
                let method = ["post(", "get(", "put(", "patch(", "delete("]
                    .iter()
                    .filter_map(|m| call.find(m).map(|at| (at, m)))
                    .min()
                    .map(|(_, m)| m.trim_end_matches('(').to_uppercase())
                    .expect("a method router");
                (method, path.to_string())
            })
            .collect();
        let listed: Vec<(String, String)> = route_entries()
            .iter()
            .map(|(m, p)| (m.to_string(), p.to_string()))
            .collect();
        assert!(!mounted.is_empty(), "the scan found no .route( call");
        assert_eq!(mounted, listed);
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

    /// The direct route takes the runner arm's lock for the whole change: while
    /// an arm holds it, a direct hide or restore waits — nothing reaches the
    /// webview, nothing is bumped — and runs once the arm is done.
    #[tokio::test(start_paused = true)]
    async fn the_direct_route_waits_for_a_running_arm() {
        let arm = pair_lock().lock_owned().await;
        let log: Sent = Default::default();
        let (l1, l2) = (log.clone(), log.clone());
        let send = move |kind: &'static str, body: Value| {
            l1.lock().unwrap().push((kind, body));
            std::future::ready(Ok(json!({"success": true, "hidden": false})))
        };
        let bump = move || l2.lock().unwrap().push(("<bump>", Value::Null));
        let call = direct_set(json!({"hidden": false}), send, bump, |_| {});
        tokio::pin!(call);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(60), &mut call)
                .await
                .is_err(),
            "the direct route must wait for the arm's lock"
        );
        assert!(
            log.lock().unwrap().is_empty(),
            "nothing ran while the arm held it"
        );
        drop(arm);
        call.await.expect("forwarded once the arm is done");
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
        json!({"success": true, "query": true, "hidden": false, "paintFence": "animation_frame"})
    }

    fn already_hidden() -> Value {
        json!({"success": true, "query": true, "hidden": true, "paintFence": "animation_frame"})
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
        // Records whether the guard knew the hide's reply time when it fired.
        let abort = move |_lock: PairLockGuard, _permits: (), times: HideTimes| {
            let event = match times.answered {
                Some(_) => "abort-restore answered",
                None => "abort-restore unanswered",
            };
            e6.lock().unwrap().push(event.into())
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

    /// Whether the guard's restore ran, whatever times it was handed.
    fn aborted(events: &[String]) -> bool {
        events.iter().any(|x| x.starts_with("abort-restore"))
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
                "bump".into(),
                "hide".into(),
                ttl_bump(),
                "capture".into(),
                "show".into(),
                "bump".into(),
            ],
            "the permits, then the probe, before frame A; the hide asks onlyIfVisible, and \
             its TTL bump is scheduled once its reply is in; a bump before the hide and \
             after the restore; no guard restore once restored"
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
        assert!(!aborted(&events), "{events:?}");
        assert!(
            !has(&events, &ttl_bump()),
            "nothing hidden, no TTL: {events:?}"
        );
        assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
        assert_eq!(
            out.frame_a,
            Ok("frame-a"),
            "nothing can be hidden where dev controls are off"
        );
        let run = out.run.expect("the hide was sent");
        assert_eq!(run.hide, refusal);
        assert_eq!(run.restore, None);
    }

    /// Any other refusal keeps frame A only when the hook read `wasHidden:
    /// false` before the SDK refused; `true`, null, or none leaves it unknown
    /// whether another caller's hide covered frame A.
    #[tokio::test]
    async fn a_refused_hide_keeps_frame_a_only_when_was_hidden_proves_it() {
        let refused = |was: Option<Value>| {
            let mut v = json!({"success": false, "code": "HIDE_FAILED", "error": "threw"});
            if let Some(was) = was {
                v["wasHidden"] = was;
            }
            v
        };
        let (out, events) = run(vec![ok(refused(Some(json!(false))))], vec![ok("frame-a")]).await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        assert!(!has(&events, "show"), "{events:?}");
        for was in [Some(json!(true)), Some(Value::Null), None] {
            let (out, events) = run(vec![ok(refused(was.clone()))], vec![ok("frame-a")]).await;
            let err = out.frame_a.expect_err("frame A is not proven");
            assert!(err.contains("the hide was refused"), "{was:?}: {err}");
            assert!(out.frame_b.is_err(), "{was:?}");
            assert!(!has(&events, "show"), "{was:?}: {events:?}");
            assert!(!aborted(&events), "{was:?}: {events:?}");
        }
    }

    /// The hook's catch-all reply — it threw, and answered the bare envelope
    /// `{success: false, error}` — carries no code and no `wasHidden`: it says
    /// nothing about the page, so it is UNKNOWN and restored, never read as
    /// "refused, nothing hidden". Likewise a reply that is neither a hide nor
    /// a refusal at all.
    #[tokio::test]
    async fn a_failure_reply_with_no_code_and_no_was_hidden_is_restored() {
        for reply in [
            json!({"requestId": "r", "type": REQUEST_TYPE, "success": false,
                   "error": "Text visibility failed: boom", "timestamp": 1}),
            json!({"success": true, "hidden": false}),
        ] {
            let (out, events) = run(
                vec![ok(reply.clone()), ok(restored(true))],
                vec![ok("frame-a"), ok("frame-b")],
            )
            .await;
            assert!(has(&events, "show"), "{reply}: {events:?}");
            assert!(has(&events, &ttl_bump()), "{reply}: {events:?}");
            assert_eq!(events.last().map(String::as_str), Some("bump"), "{reply}");
            assert_eq!(events.iter().filter(|e| *e == "capture").count(), 1);
            let err = out.frame_b.expect_err("no frame B for an unknown hide");
            assert!(
                err.contains("neither a hide nor a typed refusal"),
                "{reply}: {err}"
            );
            let err = out.frame_a.expect_err("frame A is not proven");
            assert!(err.contains("frame A was not kept"), "{reply}: {err}");
            assert_eq!(out.run.unwrap().page_restored, Some(true), "{reply}");
        }
    }

    /// A HIDE_FAILED whose rollback threw too may have left the page partly
    /// hidden; the SDK cleared its state, so no restore is sent (it could not
    /// reach what is left) and the run record says so. A rolled-back
    /// HIDE_FAILED hid nothing, and carries no warning.
    #[tokio::test]
    async fn a_hide_failed_whose_rollback_threw_warns_the_page_may_be_partly_hidden() {
        let failed = |rolled_back: Value| {
            json!({"success": false, "code": "HIDE_FAILED", "error": "threw",
                   "rolledBack": rolled_back, "wasHidden": false})
        };
        let (out, events) = run(vec![ok(failed(Value::Null))], vec![ok("frame-a")]).await;
        assert!(!has(&events, "show"), "{events:?}");
        assert_eq!(out.frame_a, Ok("frame-a"));
        let warning = out.run.unwrap().warning.expect("a warning");
        assert!(warning.contains("partly text-hidden"), "{warning}");

        let (out, _) = run(
            vec![ok(failed(json!({"trigger": "rollback", "verified": true})))],
            vec![ok("frame-a")],
        )
        .await;
        assert_eq!(out.run.unwrap().warning, None);

        // A rollback that ran but did not verify (a read-back mismatch, a
        // leftover sheet, markers it could not restore) proves nothing either;
        // nor does a HIDE_FAILED carrying no rollback at all.
        for (rolled_back, why) in [
            (
                Some(json!({"trigger": "rollback", "verified": false})),
                "did not verify",
            ),
            (Some(json!({"trigger": "rollback"})), "did not verify"),
            (None, "no rollback record"),
        ] {
            let mut hide = failed(Value::Null);
            match rolled_back.clone() {
                Some(r) => hide["rolledBack"] = r,
                None => {
                    hide.as_object_mut().unwrap().remove("rolledBack");
                }
            }
            let (out, events) = run(vec![ok(hide)], vec![ok("frame-a")]).await;
            assert!(!has(&events, "show"), "{rolled_back:?}: {events:?}");
            let warning = out.run.unwrap().warning.expect("a warning");
            assert!(
                warning.contains("partly text-hidden") && warning.contains(why),
                "{rolled_back:?}: {warning}"
            );
        }
    }

    /// The probe answered `devControls: false`: nothing can be hidden in that
    /// webview, so the arm takes frame A and stops — no hide, no mutation
    /// bump, no scheduled TTL bump — and reports the refusal as its own record.
    #[tokio::test]
    async fn dev_controls_off_takes_frame_a_and_sends_no_hide() {
        let probe = json!({"success": true, "query": true, "hidden": false,
                           "paintFence": "not_needed", "devControls": false});
        let (out, events) = run_with_probe(ok(probe), vec![], vec![ok("frame-a")]).await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        let err = out.frame_b.expect_err("nothing can be hidden there");
        assert!(err.contains("capability_unavailable_in_build"), "{err}");
        assert_eq!(events, ["permits", "probe", "capture"]);
        let record = out.run.expect("the refusal is reported");
        assert_eq!(record.hide, dev_controls_off_refusal());
        assert_eq!(record.hide["hideSent"], json!(false));
        assert_eq!(record.restore, None);
    }

    /// Frame B must be done before the SDK's TTL could have restored the page,
    /// counted from the hide's send — whatever the restore answers: it may have
    /// errored, or a re-hide after the TTL restore makes it answer `changed:
    /// true`.
    #[tokio::test(start_paused = true)]
    async fn a_frame_b_that_ran_into_the_ttl_is_discarded_whatever_the_restore_says() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let limit = CAPTURE_TTL_MS - TTL_FRAME_B_MARGIN_MS;
        for (frame_b_ms, restore, kept) in [
            (limit - 1, Ok(restored(true)), true),
            (limit, Ok(restored(true)), false),
            (CAPTURE_TTL_MS + 2_000, Ok(restored(true)), false),
            (
                CAPTURE_TTL_MS + 2_000,
                Err("UI Bridge request timed out".to_string()),
                false,
            ),
        ] {
            let replies = Mutex::new(vec![Ok(visible()), Ok(hid()), restore.clone()].into_iter());
            let send = move |_kind: &'static str, _body: Value| {
                let reply = replies
                    .lock()
                    .unwrap()
                    .next()
                    .unwrap_or_else(|| Err("no more replies scripted".into()));
                std::future::ready(reply)
            };
            let calls = Arc::new(AtomicUsize::new(0));
            let capture = move || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        return Ok::<_, String>("frame-a");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(frame_b_ms)).await;
                    Ok("frame-b")
                }
            };
            let out = capture_text_hidden_pair(
                send,
                || std::future::ready(Ok::<(), String>(())),
                capture,
                || {},
                |_| {},
                |_, _, _| {},
            )
            .await;
            if kept {
                assert_eq!(out.frame_b, Ok("frame-b"), "{frame_b_ms} ms, {restore:?}");
            } else {
                let err = out
                    .frame_b
                    .expect_err("frame B may postdate the TTL restore");
                assert!(
                    err.contains("frame B discarded") && err.contains("TTL"),
                    "{frame_b_ms} ms, {restore:?}: {err}"
                );
            }
            assert_eq!(out.frame_a, Ok("frame-a"));
        }
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
        let err = out
            .frame_a
            .expect_err("frame A may be under another caller's hide");
        assert!(err.contains("frame A was not kept"), "{err}");
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

    // Paused clock: a failed restore holds the permits until the TTL.
    #[tokio::test(start_paused = true)]
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

    /// A restore that FAILED (no `success: true` reply) may have left the page
    /// text-hidden: the arm keeps the lock and the permits until the TTL has
    /// restored it, and bumps only then. One that answered — verified or not —
    /// is not held: the SDK did its restore, and the TTL would change nothing.
    #[tokio::test(start_paused = true)]
    async fn a_failed_restore_holds_the_permits_until_the_ttl_has_restored_the_page() {
        let ttl_passed = std::time::Duration::from_millis(CAPTURE_TTL_MS + TTL_BUMP_MARGIN_MS);
        for (reply, held) in [
            (fails("UI Bridge request timed out"), true),
            (ok(json!({"success": false, "error": "x"})), true),
            (ok(restored(false)), false),
            (ok(restored(true)), false),
        ] {
            let started = tokio::time::Instant::now();
            let (out, events) = run(
                vec![ok(hid()), reply.clone()],
                vec![ok("frame-a"), ok("frame-b")],
            )
            .await;
            let took = started.elapsed();
            assert_eq!(took >= ttl_passed, held, "{reply:?}: took {took:?}");
            assert_eq!(events.last().map(String::as_str), Some("bump"), "{reply:?}");
            assert_eq!(out.frame_b, Ok("frame-b"), "{reply:?}");
        }
    }

    /// A restore that provably never left the runner changed nothing: it is
    /// sent again, and a later attempt that lands verifies the page.
    #[tokio::test(start_paused = true)]
    async fn a_restore_that_never_left_the_runner_is_retried() {
        let busy = "UI Bridge concurrency limit reached (timeout acquiring permit)";
        let (out, events) = run(
            vec![ok(hid()), fails(busy), fails(busy), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(
            events.iter().filter(|e| *e == "show").count(),
            3,
            "{events:?}"
        );
        assert_eq!(out.run.unwrap().page_restored, Some(true));
        assert_eq!(out.frame_b, Ok("frame-b"));

        // A restore that fails AFTER it may have been sent is not retried: it
        // is not known to have changed nothing.
        let (_, events) = run(
            vec![
                ok(hid()),
                fails("UI Bridge request timed out"),
                ok(restored(true)),
            ],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(
            events.iter().filter(|e| *e == "show").count(),
            1,
            "{events:?}"
        );
    }

    /// The retries stop once another attempt could not land before the TTL
    /// restores the page anyway; the arm then holds until the TTL.
    #[tokio::test(start_paused = true)]
    async fn the_restore_retries_stop_at_the_ttl() {
        let busy = "UI Bridge temporarily unavailable (circuit breaker open)";
        let mut replies = vec![ok(hid())];
        replies.extend(std::iter::repeat_with(|| fails(busy)).take(200));
        let (out, events) = run(replies, vec![ok("frame-a"), ok("frame-b")]).await;
        let shows = events.iter().filter(|e| *e == "show").count() as u64;
        assert!(
            shows <= CAPTURE_TTL_MS / RESTORE_RETRY_DELAY_MS,
            "retried past the TTL: {shows}"
        );
        assert!(shows > 1, "{events:?}");
        let record = out.run.unwrap();
        assert_eq!(record.page_restored, Some(false));
        let restore = record.restore.unwrap();
        assert_eq!(restore["attempts"], json!(shows), "{restore}");
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
        for (probe, why) in [
            (
                fails("UI Bridge request timed out"),
                "the webview did not answer",
            ),
            (
                ok(json!({"success": true, "query": true, "hidden": null})),
                "differed before and after the probe's paint fence",
            ),
            (
                ok(json!({"success": false, "error": "x"})),
                "carries no hidden flag",
            ),
        ] {
            let (out, events) = run_with_probe(probe.clone(), vec![], vec![ok("frame-a")]).await;
            let err = out.frame_a.expect_err("unknown is not visible");
            assert!(err.contains("unknown"), "{probe:?}: {err}");
            assert!(err.contains(why), "{probe:?}: {err}");
            assert!(out.run.is_none(), "{probe:?}");
            assert_eq!(events, ["permits", "probe"], "{probe:?}");
        }
    }

    /// `hidden: false` counts only behind a painted fence: a restore writes its
    /// state at once and paints later, so a timed-out or missing fence is
    /// unknown and refuses before frame A. When the hook needs no fence
    /// (`not_needed`: no recent restore to paint, or dev controls off so nothing
    /// can have been hidden there), the arm proceeds.
    #[tokio::test]
    async fn a_probe_whose_paint_fence_is_unproven_refuses_before_frame_a() {
        for probe in [
            json!({"success": true, "query": true, "hidden": false, "paintFence": "timeout"}),
            json!({"success": true, "query": true, "hidden": false}),
        ] {
            let (out, events) =
                run_with_probe(ok(probe.clone()), vec![], vec![ok("frame-a")]).await;
            let err = out
                .frame_a
                .expect_err("an unpainted restore is not visible text");
            assert!(err.contains("painted visible is unknown"), "{probe}: {err}");
            assert!(out.run.is_none(), "{probe}");
            assert_eq!(events, ["permits", "probe"], "{probe}");
        }
        let not_needed =
            json!({"success": true, "query": true, "hidden": false, "paintFence": "not_needed"});
        let (out, events) = run_with_probe(
            ok(not_needed),
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        assert_eq!(out.frame_b, Ok("frame-b"));
        assert!(has(&events, "show"), "{events:?}");
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
        assert!(!aborted(&events), "{events:?}");
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
    /// restored and no frame B is returned. Frame A is refused too: the lost
    /// `wasHidden` is what would have shown another caller's hide landing after
    /// the probe, during frame A.
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
            let err = out
                .frame_a
                .expect_err("frame A may be under another caller's hide");
            assert!(err.contains("whose hide it is is unknown"), "{anon}: {err}");
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
        assert_eq!(
            events.last().map(String::as_str),
            Some("abort-restore answered")
        );
        assert!(!has(&events, "show"), "{events:?}");
    }

    #[tokio::test]
    async fn a_cancel_while_the_hide_is_in_flight_still_restores() {
        let events = run_until_stalled_then_cancel(vec![None], vec![ok("frame-a")]).await;
        // No reply yet: the guard's restore counts its TTL hold from the cancel.
        assert_eq!(
            events.last().map(String::as_str),
            Some("abort-restore unanswered")
        );
        assert!(has(&events, "hide"), "{events:?}");
    }

    #[tokio::test]
    async fn a_completed_arm_does_not_also_restore_from_its_guard() {
        let (_, events) = run(
            vec![ok(hid()), ok(restored(true))],
            vec![ok("frame-a"), ok("frame-b")],
        )
        .await;
        assert!(!aborted(&events), "{events:?}");
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
            guard.arm(
                move |held, permits, _| *k.lock().unwrap() = Some((held, permits)),
                tokio::time::Instant::now(),
            );
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
            guard.arm(
                move |_, _, _| r.store(true, std::sync::atomic::Ordering::SeqCst),
                tokio::time::Instant::now(),
            );
            guard.disarm();
        }
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(lock.try_lock().is_ok());
        assert_eq!(sem.available_permits(), 2);
    }

    /// A stand-in webview for the cancel path: answers each restore with the
    /// next of `replies` after `latency`, counting the restores it was sent.
    fn slow_webview(
        replies: Vec<Result<Value, String>>,
        latency: std::time::Duration,
    ) -> (
        Arc<std::sync::atomic::AtomicUsize>,
        impl Fn(&'static str, Value) -> BoxFut<Result<Value, String>> + Send + Sync + 'static,
    ) {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        let replies = Mutex::new(replies.into_iter());
        let send = move |_kind: &'static str, body: Value| {
            assert_eq!(
                body,
                json!({"hidden": false}),
                "the cancel path only restores"
            );
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let reply = replies
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| Err("no more replies scripted".into()));
            let fut: BoxFut<Result<Value, String>> = Box::pin(async move {
                tokio::time::sleep(latency).await;
                reply
            });
            fut
        };
        (count, send)
    }

    /// The cancel path's restore — what the guard spawns, [`restore_after_cancel`]
    /// — releases the permits and the lock only once the page is known
    /// restored: after a verified reply; after retrying a restore that never
    /// left the runner; and, when no restore answered `success: true`, only
    /// once the TTL has had time to restore the page, counted from the hide's
    /// reply, or from the cancel when no reply had come. A restore that was not
    /// verified hands the TTL-restore bumps the time left until the TTL fires.
    #[tokio::test(start_paused = true)]
    async fn the_cancel_restore_releases_the_permits_only_once_the_page_is_known_restored() {
        let ms = std::time::Duration::from_millis;
        let latency = ms(300);
        let busy = "UI Bridge temporarily unavailable (circuit breaker open)";
        let timed_out = "UI Bridge request timed out";
        let ttl_fired = CAPTURE_TTL_MS + TTL_BUMP_MARGIN_MS;
        struct Case {
            name: &'static str,
            replies: Vec<Result<Value, String>>,
            /// The hide's reply came 5 s before the cancel (else: not yet).
            answered: bool,
            /// Restores sent.
            restores: usize,
            /// Released this long after the cancel, in ms.
            released: u64,
            /// The delay handed to the TTL-restore bumps, if any.
            ttl_left: Option<u64>,
        }
        let cases = [
            Case {
                name: "verified",
                replies: vec![Ok(restored(true))],
                answered: true,
                restores: 1,
                released: 300,
                ttl_left: None,
            },
            Case {
                name: "never left, then verified",
                replies: vec![Err(busy.into()), Err(busy.into()), Ok(restored(true))],
                answered: true,
                restores: 3,
                released: 3 * 300 + 2 * RESTORE_RETRY_DELAY_MS,
                ttl_left: None,
            },
            Case {
                name: "failed after it may have been sent",
                replies: vec![Err(timed_out.into())],
                answered: true,
                restores: 1,
                released: ttl_fired - 5_000,
                ttl_left: Some(0),
            },
            Case {
                name: "answered, not verified",
                replies: vec![Ok(restored(false))],
                answered: true,
                restores: 1,
                released: 300,
                ttl_left: Some(ttl_fired - 5_000 - 300),
            },
            Case {
                name: "failed, cancelled before the hide's reply",
                replies: vec![Err(timed_out.into())],
                answered: false,
                restores: 1,
                released: ttl_fired,
                ttl_left: Some(0),
            },
        ];
        for Case {
            name,
            replies,
            answered,
            restores,
            released,
            ttl_left,
        } in cases
        {
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            let sem = Arc::new(tokio::sync::Semaphore::new(2));
            let permits = Arc::clone(&sem).acquire_many_owned(2).await.unwrap();
            let held = Arc::clone(&lock).lock_owned().await;
            let (sent, send) = slow_webview(replies, latency);
            let bumps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ttl_bumps_at: Arc<Mutex<Vec<u64>>> = Default::default();
            let (b, t) = (bumps.clone(), ttl_bumps_at.clone());
            let cancel = tokio::time::Instant::now();
            let times = HideTimes {
                sent: cancel - ms(6_000),
                answered: answered.then_some(cancel - ms(5_000)),
            };
            let task = tokio::spawn(restore_after_cancel(
                send,
                move || {
                    b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
                move |delay| t.lock().unwrap().push(delay),
                held,
                permits,
                times,
            ));
            tokio::time::sleep_until(cancel + ms(released - 1)).await;
            assert_eq!(sem.available_permits(), 0, "{name}: released early");
            assert!(lock.try_lock().is_err(), "{name}: lock released early");
            assert_eq!(bumps.load(std::sync::atomic::Ordering::SeqCst), 0, "{name}");
            tokio::time::sleep_until(cancel + ms(released + 1)).await;
            assert_eq!(sem.available_permits(), 2, "{name}: not released");
            assert!(lock.try_lock().is_ok(), "{name}: lock not released");
            assert_eq!(bumps.load(std::sync::atomic::Ordering::SeqCst), 1, "{name}");
            let restore = task.await.expect("the cancel restore panicked");
            assert_eq!(
                sent.load(std::sync::atomic::Ordering::SeqCst),
                restores,
                "{name}: {restore}"
            );
            assert_eq!(
                ttl_bumps_at.lock().unwrap().clone(),
                ttl_left.into_iter().collect::<Vec<_>>(),
                "{name}"
            );
        }
    }

    /// A cancel during the main path's own TTL hold (its restore failed) does
    /// not cut that hold short: the guard hands its restore the hide's sent
    /// and reply times, and the permits stay held until the TTL has fired,
    /// counted from the hide's reply — when the main path's hold would have
    /// ended too.
    ///
    /// The cancel is timed from the hide's reply, not from the call: the arm
    /// first waits on the process-wide [`pair_lock`], which a test running in
    /// parallel may hold, and on a paused clock a timeout around that wait can
    /// fire before the arm has hidden anything. The wait before the hide is
    /// therefore unbounded on purpose: a deadlock there hangs this test rather
    /// than failing it, which is the price of removing the race.
    ///
    /// The hide answers a second after it is sent, so the instant the guard
    /// hands over is checked to be the reply's, not the send's or the cancel's.
    #[tokio::test(start_paused = true)]
    async fn a_cancel_during_the_ttl_hold_keeps_the_permits_until_the_ttl() {
        use std::sync::atomic::Ordering::SeqCst;
        let ms = std::time::Duration::from_millis;
        let sem = Arc::new(tokio::sync::Semaphore::new(2));
        let handed: Arc<Mutex<Option<HideTimes>>> = Default::default();
        let (_, webview_send) = webview(vec![
            Ok(visible()),
            Ok(hid()),
            Err("UI Bridge request timed out".into()),
        ]);
        let hide_answered = Arc::new(tokio::sync::Notify::new());
        let replied_at: Arc<Mutex<Option<tokio::time::Instant>>> = Default::default();
        let answered_tx = Arc::clone(&hide_answered);
        let replied_tx = Arc::clone(&replied_at);
        let send = move |kind: &'static str, body: Value| {
            let is_hide = body == arm_hide_body();
            let reply = webview_send(kind, body);
            let answered = Arc::clone(&answered_tx);
            let replied = Arc::clone(&replied_tx);
            async move {
                let reply = reply.await;
                if is_hide {
                    // The arm holds the pair lock by now, so this wait is
                    // the hide's own latency and nothing else's.
                    tokio::time::sleep(ms(1_000)).await;
                    *replied.lock().unwrap() = Some(tokio::time::Instant::now());
                    answered.notify_one();
                }
                reply
            }
        };
        let s = Arc::clone(&sem);
        let acquire = move || async move {
            s.acquire_many_owned(2)
                .await
                .map_err(|e| format!("acquire: {e}"))
        };
        let capture = || std::future::ready(Ok::<&'static str, String>("frame"));
        let (guard_send_count, guard_send) =
            slow_webview(vec![Err("UI Bridge request timed out".into())], ms(0));
        let h = handed.clone();
        let abort = move |lock: PairLockGuard,
                          permits: tokio::sync::OwnedSemaphorePermit,
                          times: HideTimes| {
            *h.lock().unwrap() = Some(times);
            tokio::spawn(restore_after_cancel(
                guard_send,
                || {},
                |_| {},
                lock,
                permits,
                times,
            ));
        };
        let mut arm = Box::pin(capture_text_hidden_pair(
            send,
            acquire,
            capture,
            || {},
            |_| {},
            abort,
        ));
        tokio::select! {
            _ = &mut arm => panic!("the arm finished before its hide was answered"),
            _ = hide_answered.notified() => {}
        }
        assert!(
            tokio::time::timeout(ms(CAPTURE_TTL_MS / 2), &mut arm)
                .await
                .is_err(),
            "the arm was still in its TTL hold"
        );
        assert!(
            handed.lock().unwrap().is_none(),
            "the guard fires only on the cancel"
        );
        drop(arm);
        let times = handed.lock().unwrap().take().expect("the guard fired");
        let answered = replied_at.lock().unwrap().expect("the hide was answered");
        assert_eq!(
            times.answered,
            Some(answered),
            "the guard hands over the hide's reply instant"
        );
        assert_eq!(answered, times.sent + ms(1_000), "a second after the send");
        let ttl_fired = answered + ms(CAPTURE_TTL_MS + TTL_BUMP_MARGIN_MS);
        tokio::time::sleep_until(ttl_fired - ms(1)).await;
        assert_eq!(
            sem.available_permits(),
            0,
            "no capture may see the page before the TTL has fired"
        );
        tokio::time::sleep_until(ttl_fired + ms(1)).await;
        assert_eq!(sem.available_permits(), 2);
        assert_eq!(
            guard_send_count.load(SeqCst),
            1,
            "the guard restored once more"
        );
    }

    /// The TTL-restore bumps: one at the delay, and a second
    /// [`TTL_BUMP_LATE_EXTRA_MS`] later for a TTL restore that ran late.
    #[tokio::test(start_paused = true)]
    async fn the_ttl_bumps_fire_at_the_delay_and_again_late() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let t0 = tokio::time::Instant::now();
        let task = tokio::spawn(ttl_bumps(1_000, move || {
            c.fetch_add(1, SeqCst);
        }));
        let at = |ms: u64| tokio::time::sleep_until(t0 + std::time::Duration::from_millis(ms));
        at(999).await;
        assert_eq!(count.load(SeqCst), 0);
        at(1_001).await;
        assert_eq!(count.load(SeqCst), 1);
        at(1_000 + TTL_BUMP_LATE_EXTRA_MS - 1).await;
        assert_eq!(count.load(SeqCst), 1);
        at(1_000 + TTL_BUMP_LATE_EXTRA_MS + 1).await;
        assert_eq!(count.load(SeqCst), 2);
        task.await.expect("the bump task ran to its end");
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
                            json!({"success": true, "query": true, "hidden": was,
                                   "paintFence": "animation_frame"})
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
                    |_, _, _| {},
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
            assert!(!aborted(&events), "{error}: {events:?}");
            let err = out.frame_a.expect_err("no hook answer checked frame A");
            assert!(
                err.contains("hide request never left the runner"),
                "{error}: {err}"
            );
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

    /// The direct route's production call site, pinned by source: its TTL bump
    /// goes to the real scheduler. (The guard path's wiring — the abort
    /// closure spawning [`restore_after_cancel`] over the real request, bump
    /// and scheduler — is pinned in `vision_routes`'
    /// `the_arm_wiring_is_the_real_one`.)
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
