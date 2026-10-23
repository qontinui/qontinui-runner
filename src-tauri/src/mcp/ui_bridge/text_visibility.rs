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
pub(crate) async fn direct_set<S, F, B>(body: Value, send: S, bump: B) -> HandlerResult
where
    S: FnOnce(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
    B: Fn(),
{
    validate_text_visibility_body(&body).map_err(bad_request)?;
    info!(
        "UI Bridge API: text visibility hidden={}",
        body["hidden"].as_bool().unwrap_or_default()
    );
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
    direct_set(
        body,
        |kind, body| async move { ui_bridge_request_sync(&state, kind, body).await },
        move || super::vision_routes::bump_mutation_id(&bump_state),
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
    /// (`changed: false`) — then that caller's hide is left to it, and the TTL
    /// this hide re-armed ([`CAPTURE_TTL_MS`]) restores the page by itself.
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
/// second's frame B — and both would measure the wrong pixels.
static PAIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Sends the restore if dropped while armed: the runner arm's future cancelled
/// (the HTTP client went away) or a panic unwinding through it after a
/// confirmed hide. Without it the page stays hidden until the SDK's TTL.
struct RestoreOnDrop<R: FnOnce()> {
    restore: Option<R>,
}

impl<R: FnOnce()> RestoreOnDrop<R> {
    fn disarm(mut self) {
        self.restore = None;
    }
}

impl<R: FnOnce()> Drop for RestoreOnDrop<R> {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            warn!("vision: the runner arm ended before its restore; restoring the webview's text");
            restore();
        }
    }
}

/// Frame A, then every glyph in the runner's own webview hidden, frame B, and
/// the page restored — under one lock, with one capture permit held throughout.
///
/// - `send` is the request to the webview (`ui_bridge_request_sync`): the hide
///   and the restore.
/// - `acquire` takes the capture permit BEFORE anything is hidden, so the page
///   is never left hidden while this call queues for a capture slot.
/// - `capture` takes one frame of the runner window, the permit already held;
///   it is called for frame A and, only after the webview confirmed the hide
///   (`success: true, hidden: true`; the hook answers after a paint), for
///   frame B.
/// - `bump` bumps the vision mutation id: before the hide and after the restore.
/// - `abort_restore` restores the page from a drop guard if this future is
///   cancelled or unwinds between the hide and the restore; it must not await
///   (it spawns).
///
/// The restore is sent whenever THIS call hid the text, whether or not frame B
/// was captured. A restore that fails or is not verified is reported in
/// [`TextVisibilityRun::page_restored`] and frame B is kept: what it shows was
/// fixed when it was taken. A restore answering `changed: false` means the
/// SDK's TTL had already restored the page, at an unknown moment that may
/// precede frame B, so frame B is discarded.
pub(crate) async fn capture_text_hidden_pair<S, F, A, AF, P, C, CF, Fr, B, R>(
    send: S,
    acquire: A,
    capture: C,
    bump: B,
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
    R: FnOnce(),
{
    let _serialized = PAIR_LOCK.lock().await;
    let not_taken = |why: &str| Err(format!("frame B was not taken: {why}"));
    let _permit = match acquire().await {
        Ok(p) => p,
        Err(e) => {
            return TextHiddenPair {
                frame_a: Err(format!("capture permit: {e}")),
                frame_b: not_taken("no capture permit"),
                run: None,
            }
        }
    };
    let frame_a = capture().await;
    if frame_a.is_err() {
        // Altering the page for a frame B nothing can be paired with buys nothing.
        return TextHiddenPair {
            frame_a,
            frame_b: not_taken("frame A could not be captured"),
            run: None,
        };
    }

    bump();
    let hide = match send(
        REQUEST_TYPE,
        serde_json::json!({"hidden": true, "ttlMs": CAPTURE_TTL_MS}),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => serde_json::json!({"success": false, "error": e}),
    };
    if !(is_true(&hide, "success") && is_true(&hide, "hidden")) {
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
    let warning = (hide.get("paintFence").and_then(Value::as_str) == Some("timeout")).then(|| {
        "the hide's paint fence timed out (the webview painted no frame within it), so \
         frame B may have been composited before the glyphs went transparent"
            .to_string()
    });
    // `changed: false` means the text was already hidden, by a caller this one
    // must not undo. Only an explicit `false` reads that way: a reply that
    // omits it is treated as this call's own hide, and restored.
    let hid_it = hide.get("changed").and_then(Value::as_bool) != Some(false);
    let guard = hid_it.then(|| RestoreOnDrop {
        restore: Some(abort_restore),
    });

    let mut frame_b = capture()
        .await
        .map_err(|e| format!("frame B (text hidden) capture failed: {e}"));

    let (restore, page_restored) = if hid_it {
        let restore = match send(REQUEST_TYPE, serde_json::json!({"hidden": false})).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({"success": false, "error": e}),
        };
        if let Some(guard) = guard {
            guard.disarm();
        }
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
        (Some(restore), Some(verified))
    } else {
        (None, None)
    };

    TextHiddenPair {
        frame_a,
        frame_b,
        run: Some(TextVisibilityRun {
            hide,
            restore,
            page_restored,
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
        let (status, Json(body)) = direct_set(json!({"hidden": "yes"}), send, || {})
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
        let out = direct_set(json!({"hidden": true, "ttlMs": 5_000}), send, || {})
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
        let (status, Json(body)) = direct_set(json!({"hidden": true}), send, || {})
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
        direct_set(json!({"hidden": true}), send, bump)
            .await
            .expect("forwarded");
        let kinds: Vec<&str> = log.lock().unwrap().iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, ["<bump>", REQUEST_TYPE, "<bump>"]);
    }

    // ── the runner arm: frame A, hide, frame B, restore ───────────────────

    type Events = Arc<Mutex<Vec<String>>>;

    fn hid() -> Value {
        json!({"success": true, "hidden": true, "changed": true, "ttlMs": CAPTURE_TTL_MS})
    }

    fn restored(verified: bool) -> Value {
        json!({"success": true, "hidden": false, "changed": true,
               "restore": {"trigger": "request", "verified": verified}})
    }

    type BoxFut<T> = std::pin::Pin<Box<dyn Future<Output = T> + Send>>;

    /// One scripted run of the arm: the webview answers `replies` in turn, the
    /// captures answer `frames` in turn, and every step lands in `events`.
    struct Script {
        events: Events,
        replies: Vec<Result<Value, String>>,
        frames: Vec<Result<&'static str, String>>,
    }

    impl Script {
        fn new(
            replies: Vec<Result<Value, String>>,
            frames: Vec<Result<&'static str, String>>,
        ) -> Self {
            Self {
                events: Default::default(),
                replies,
                frames,
            }
        }

        async fn run(self) -> (TextHiddenPair<&'static str>, Vec<String>) {
            let ev = self.events.clone();
            let out = run_scripted(self.events, self.replies, self.frames).await;
            let events = ev.lock().unwrap().clone();
            (out, events)
        }
    }

    fn run_scripted(
        events: Events,
        replies: Vec<Result<Value, String>>,
        frames: Vec<Result<&'static str, String>>,
    ) -> BoxFut<TextHiddenPair<&'static str>> {
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        let frames = Arc::new(Mutex::new(frames.into_iter()));
        let (e1, e2, e3, e4, e5) = (
            events.clone(),
            events.clone(),
            events.clone(),
            events.clone(),
            events,
        );
        let send = move |_kind: &'static str, body: Value| {
            let hidden = body["hidden"].as_bool().unwrap_or_default();
            e1.lock()
                .unwrap()
                .push(if hidden { "hide" } else { "show" }.into());
            let reply = replies
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| Err("no more replies scripted".into()));
            std::future::ready(reply)
        };
        let acquire = move || {
            e2.lock().unwrap().push("permit".into());
            std::future::ready(Ok::<(), String>(()))
        };
        let capture = move || {
            e3.lock().unwrap().push("capture".into());
            let next = frames.lock().unwrap().next();
            let fut: BoxFut<Result<&'static str, String>> = match next {
                Some(f) => Box::pin(std::future::ready(f)),
                // Out of scripted frames: a capture that never finishes.
                None => Box::pin(std::future::pending()),
            };
            fut
        };
        let bump = move || e4.lock().unwrap().push("bump".into());
        let abort = move || e5.lock().unwrap().push("abort-restore".into());
        Box::pin(capture_text_hidden_pair(
            send, acquire, capture, bump, abort,
        ))
    }

    #[tokio::test]
    async fn the_arm_takes_the_permit_then_a_then_hides_then_b_then_restores() {
        let (out, events) = Script::new(
            vec![Ok(hid()), Ok(restored(true))],
            vec![Ok("frame-a"), Ok("frame-b")],
        )
        .run()
        .await;
        assert_eq!(out.frame_a, Ok("frame-a"));
        assert_eq!(out.frame_b, Ok("frame-b"));
        assert_eq!(
            events,
            ["permit", "capture", "bump", "hide", "capture", "show", "bump"],
            "the permit before the hide; a bump before the hide and after the restore"
        );
        let run = out.run.expect("a hide was sent");
        assert_eq!(run.page_restored, Some(true));
        assert_eq!(run.hide, hid());
        assert_eq!(run.warning, None);
    }

    #[tokio::test]
    async fn no_frame_a_means_nothing_is_hidden() {
        let (out, events) = Script::new(vec![], vec![Err("Runner window not found".into())])
            .run()
            .await;
        assert!(out.frame_a.is_err());
        assert!(out
            .frame_b
            .unwrap_err()
            .contains("frame A could not be captured"));
        assert!(out.run.is_none());
        assert_eq!(events, ["permit", "capture"]);
    }

    #[tokio::test]
    async fn a_refused_hide_captures_no_frame_b_and_names_the_code() {
        let refusal = json!({"success": false, "code": "capability_unavailable_in_build",
                             "error": "dev control"});
        let (out, events) = Script::new(vec![Ok(refusal.clone())], vec![Ok("frame-a")])
            .run()
            .await;
        let err = out.frame_b.expect_err("no frame B without a hide");
        assert!(err.contains("capability_unavailable_in_build"), "{err}");
        assert_eq!(events, ["permit", "capture", "bump", "hide"]);
        let run = out.run.expect("the hide was sent");
        assert_eq!(run.hide, refusal);
        assert_eq!(run.restore, None);
    }

    #[tokio::test]
    async fn an_unreachable_webview_is_a_failed_hide_not_a_frame() {
        let (out, _) = Script::new(
            vec![Err("UI Bridge request timed out".into())],
            vec![Ok("frame-a")],
        )
        .run()
        .await;
        assert!(out.frame_b.unwrap_err().contains("timed out"));
        assert_eq!(out.run.unwrap().hide["success"], json!(false));
    }

    #[tokio::test]
    async fn a_failed_frame_b_still_restores_the_page() {
        let (out, events) = Script::new(
            vec![Ok(hid()), Ok(restored(true))],
            vec![Ok("frame-a"), Err("capture backend failed".into())],
        )
        .run()
        .await;
        let err = out.frame_b.expect_err("the capture failed");
        assert!(
            err.contains("frame B (text hidden) capture failed"),
            "{err}"
        );
        assert_eq!(events.last().map(String::as_str), Some("bump"));
        assert!(events.contains(&"show".to_string()), "{events:?}");
        assert_eq!(out.run.unwrap().page_restored, Some(true));
    }

    #[tokio::test]
    async fn an_unverified_or_failed_restore_is_reported_and_frame_b_kept() {
        for reply in [
            Ok(restored(false)),
            Ok(json!({"success": false, "error": "STATE_SCHEMA_MISMATCH"})),
            Err("transport".to_string()),
        ] {
            let (out, _) = Script::new(
                vec![Ok(hid()), reply.clone()],
                vec![Ok("frame-a"), Ok("frame-b")],
            )
            .run()
            .await;
            assert_eq!(out.frame_b, Ok("frame-b"), "{reply:?}");
            assert_eq!(out.run.unwrap().page_restored, Some(false), "{reply:?}");
        }
    }

    #[tokio::test]
    async fn a_restore_the_ttl_already_made_discards_frame_b() {
        let late = json!({"success": true, "hidden": false, "changed": false,
                          "restore": {"trigger": "ttl", "verified": true}});
        let (out, _) = Script::new(
            vec![Ok(hid()), Ok(late)],
            vec![Ok("frame-a"), Ok("frame-b")],
        )
        .run()
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
        let (out, _) = Script::new(
            vec![Ok(fenced), Ok(restored(true))],
            vec![Ok("frame-a"), Ok("frame-b")],
        )
        .run()
        .await;
        assert_eq!(out.frame_b, Ok("frame-b"));
        let warning = out.run.unwrap().warning.expect("a warning");
        assert!(warning.contains("paint fence timed out"), "{warning}");
    }

    #[tokio::test]
    async fn text_another_caller_hid_is_left_to_it() {
        let already = json!({"success": true, "hidden": true, "changed": false});
        let (out, events) = Script::new(vec![Ok(already)], vec![Ok("frame-a"), Ok("frame-b")])
            .run()
            .await;
        assert_eq!(out.frame_b, Ok("frame-b"));
        assert!(!events.contains(&"show".to_string()), "{events:?}");
        assert_eq!(out.run.unwrap().restore, None);
    }

    #[tokio::test]
    async fn a_cancelled_arm_restores_the_page_from_its_guard() {
        let events: Events = Default::default();
        // Frame B never finishes; the caller gives up while text is hidden.
        let fut = run_scripted(
            events.clone(),
            vec![Ok(hid()), Ok(restored(true))],
            vec![Ok("frame-a")],
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), fut)
                .await
                .is_err(),
            "frame B was scripted never to finish"
        );
        let events = events.lock().unwrap().clone();
        assert_eq!(
            events,
            [
                "permit",
                "capture",
                "bump",
                "hide",
                "capture",
                "abort-restore"
            ]
        );
    }

    #[tokio::test]
    async fn a_completed_arm_does_not_also_restore_from_its_guard() {
        let (_, events) = Script::new(
            vec![Ok(hid()), Ok(restored(true))],
            vec![Ok("frame-a"), Ok("frame-b")],
        )
        .run()
        .await;
        assert!(!events.contains(&"abort-restore".to_string()), "{events:?}");
    }

    /// Two arms at once against one webview whose frames show whether its text
    /// is hidden: each must see its own frame A with text and frame B without.
    #[tokio::test]
    async fn concurrent_arms_do_not_interleave() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let hidden = Arc::new(AtomicBool::new(false));
        let arm = |hidden: Arc<AtomicBool>| async move {
            let h1 = hidden.clone();
            let send = move |_kind: &'static str, body: Value| {
                let h = h1.clone();
                async move {
                    tokio::task::yield_now().await;
                    let want = body["hidden"].as_bool().unwrap_or_default();
                    let was = h.swap(want, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(if want {
                        json!({"success": true, "hidden": true, "changed": !was})
                    } else {
                        json!({"success": true, "hidden": false, "changed": was,
                               "restore": {"verified": true}})
                    })
                }
            };
            let h2 = hidden.clone();
            let capture = move || {
                let h = h2.clone();
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
                || {},
            )
            .await
        };
        let (one, two) = tokio::join!(arm(hidden.clone()), arm(hidden.clone()));
        for out in [one, two] {
            assert_eq!(out.frame_a, Ok(false), "frame A must show the text");
            assert_eq!(out.frame_b, Ok(true), "frame B must be the hidden page");
        }
        assert!(!hidden.load(Ordering::SeqCst), "the page ends restored");
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
