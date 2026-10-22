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
//! injected runtime always does; the runner's webview opts in only when
//! `import.meta.env.DEV` is true, i.e. under a Vite dev server. Every runner —
//! the published one and every dev or temp runner alike — runs in exe mode, so
//! this route answers the typed refusal from the runner's own webview. It is
//! mounted anyway because the SDK manifest-parity test requires every SDK route
//! to be exposed. The gate is not a security boundary: hiding text adds no
//! capability beyond the runner's existing `page_evaluate` / eval surface.
//!
//! Validation mirrors the SDK's `validateTextVisibilityRequest` for an early
//! 400; the SDK copy is the authority. Unlike the media override, there is no
//! page-side wait to fit inside a transport budget: the SDK's hide and restore
//! are synchronous, so one body serves every transport arm.
//!
//! [`capture_text_hidden`] is the runner arm of plan step 2: hide, capture
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

/// The TTL the runner arm ([`capture_text_hidden`]) hides with. Long enough for
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
pub(crate) async fn direct_set<S, F>(body: Value, send: S) -> HandlerResult
where
    S: FnOnce(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
{
    validate_text_visibility_body(&body).map_err(bad_request)?;
    info!(
        "UI Bridge API: text visibility hidden={}",
        body["hidden"].as_bool().unwrap_or_default()
    );
    let result = send(REQUEST_TYPE, body).await;
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
    direct_set(body, |kind, body| async move {
        ui_bridge_request_sync(&state, kind, body).await
    })
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
// The runner arm (plan step 2): hide, capture frame B, restore
// ============================================================================

/// What the runner arm did to the page, reported beside frame B so a caller
/// can tell "no frame B because the hide was refused" from "frame B present"
/// and can prove the page ended restored.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextVisibilityRun {
    /// What the webview answered to the hide: the SDK's own result
    /// (`{success, hidden, changed, coverage, ttlMs, …}`) or its refusal
    /// (`{success: false, code, reason, error}` — `capability_unavailable_in_build`
    /// on every exe-mode runner), or `{success: false, error}` when the request
    /// never reached the SDK.
    pub hide: Value,
    /// What the webview answered to the restore, the SDK's `{hidden: false,
    /// restore: {verified, …}}`. `None` when no restore was sent: the hide
    /// failed, or the text was already hidden by another caller (`changed:
    /// false`) — then that caller's hide is left to it, and the TTL this hide
    /// re-armed ([`CAPTURE_TTL_MS`]) restores the page by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<Value>,
    /// True only when a restore was sent and the SDK verified it
    /// (`restore.verified: true`); false when it was sent and not verified or
    /// failed; `None` when none was sent (see `restore`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_restored: Option<bool>,
}

/// The outcome of [`capture_text_hidden`]: frame B when it was captured, else
/// why not, and the hide/restore record either way.
#[derive(Debug)]
pub struct TextHiddenCapture<Fr> {
    pub frame: Result<Fr, String>,
    pub run: TextVisibilityRun,
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

/// Hide every glyph in the runner's own webview, capture frame B, restore.
///
/// `send` is the request to the webview (`ui_bridge_request_sync`), called at
/// most twice — the hide and the restore; `capture` takes one frame of the
/// runner window. Frame B is taken only after the webview confirmed the hide
/// (`success: true, hidden: true`): the hook answers after a paint, so the
/// capture sees the hidden frame. The restore is sent whenever THIS call hid
/// the text, whether or not the capture succeeded; a restore that fails or is
/// not verified is reported in [`TextVisibilityRun::page_restored`], and frame
/// B is still returned, because what it shows was fixed when it was taken.
pub(crate) async fn capture_text_hidden<S, F, C, CF, Fr>(
    send: S,
    capture: C,
) -> TextHiddenCapture<Fr>
where
    S: Fn(&'static str, Value) -> F,
    F: Future<Output = Result<Value, String>>,
    C: FnOnce() -> CF,
    CF: Future<Output = Result<Fr, String>>,
{
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
        let error = hide_failure_message(&hide);
        return TextHiddenCapture {
            frame: Err(error),
            run: TextVisibilityRun {
                hide,
                restore: None,
                page_restored: None,
            },
        };
    }
    // `changed: false` means the text was already hidden, by a caller this one
    // must not undo. Only an explicit `false` reads that way: a reply that
    // omits it is treated as this call's own hide, and restored.
    let hid_it = hide.get("changed").and_then(Value::as_bool) != Some(false);

    let frame = capture().await;

    let (restore, page_restored) = if hid_it {
        let restore = match send(REQUEST_TYPE, serde_json::json!({"hidden": false})).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({"success": false, "error": e}),
        };
        let verified = is_true(&restore, "success")
            && restore.get("hidden").and_then(Value::as_bool) == Some(false)
            && restore
                .get("restore")
                .is_some_and(|r| is_true(r, "verified"));
        if !verified {
            warn!("vision: the runner webview's text restore was not verified: {restore}");
        }
        (Some(restore), Some(verified))
    } else {
        (None, None)
    };

    let frame = frame.map_err(|e| format!("frame B (text hidden) capture failed: {e}"));
    TextHiddenCapture {
        frame,
        run: TextVisibilityRun {
            hide,
            restore,
            page_restored,
        },
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
        let (status, Json(body)) = direct_set(json!({"hidden": "yes"}), send)
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
        let out = direct_set(json!({"hidden": true, "ttlMs": 5_000}), send)
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
        let (status, Json(body)) = direct_set(json!({"hidden": true}), send)
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

    // ── the runner arm: hide, capture frame B, restore ────────────────────

    fn hid() -> Value {
        json!({"success": true, "hidden": true, "changed": true, "ttlMs": CAPTURE_TTL_MS})
    }

    fn restored(verified: bool) -> Value {
        json!({"success": true, "hidden": false, "changed": true,
               "restore": {"trigger": "request", "verified": verified}})
    }

    /// A capture that records that it ran, and when relative to the requests.
    fn capture_after(
        sent: &Sent,
        result: Result<&'static str, String>,
    ) -> impl FnOnce() -> std::future::Ready<Result<&'static str, String>> {
        let sent = sent.clone();
        move || {
            sent.lock().unwrap().push(("<capture>", Value::Null));
            std::future::ready(result)
        }
    }

    #[tokio::test]
    async fn the_arm_hides_then_captures_then_restores() {
        let (sent, send) = webview(vec![Ok(hid()), Ok(restored(true))]);
        let cap = capture_after(&sent, Ok("frame-b"));
        let out = capture_text_hidden(send, cap).await;
        assert_eq!(out.frame, Ok("frame-b"));
        assert_eq!(
            *sent.lock().unwrap(),
            vec![
                (
                    REQUEST_TYPE,
                    json!({"hidden": true, "ttlMs": CAPTURE_TTL_MS})
                ),
                ("<capture>", Value::Null),
                (REQUEST_TYPE, json!({"hidden": false})),
            ]
        );
        assert_eq!(out.run.page_restored, Some(true));
        assert_eq!(out.run.hide, hid());
    }

    #[tokio::test]
    async fn a_refused_hide_captures_nothing_and_names_the_code() {
        let refusal = json!({"success": false, "code": "capability_unavailable_in_build",
                             "error": "dev control"});
        let (sent, send) = webview(vec![Ok(refusal.clone())]);
        let cap = capture_after(&sent, Ok("frame-b"));
        let out = capture_text_hidden(send, cap).await;
        let err = out.frame.expect_err("no frame B without a hide");
        assert!(err.contains("capability_unavailable_in_build"), "{err}");
        assert_eq!(sent.lock().unwrap().len(), 1, "no capture, no restore");
        assert_eq!(out.run.hide, refusal);
        assert_eq!(out.run.restore, None);
        assert_eq!(out.run.page_restored, None);
    }

    #[tokio::test]
    async fn an_unreachable_webview_is_a_failed_hide_not_a_frame() {
        let (_sent, send) = webview(vec![Err("UI Bridge request timed out".into())]);
        let out = capture_text_hidden(send, || std::future::ready(Ok("frame-b"))).await;
        assert!(out.frame.unwrap_err().contains("timed out"));
        assert_eq!(out.run.hide["success"], json!(false));
    }

    #[tokio::test]
    async fn a_failed_capture_still_restores_the_page() {
        let (sent, send) = webview(vec![Ok(hid()), Ok(restored(true))]);
        let cap = capture_after(&sent, Err("Runner window not found".into()));
        let out = capture_text_hidden(send, cap).await;
        let err = out.frame.expect_err("the capture failed");
        assert!(
            err.contains("frame B (text hidden) capture failed"),
            "{err}"
        );
        assert!(err.contains("Runner window not found"), "{err}");
        assert_eq!(
            sent.lock().unwrap().last().cloned(),
            Some((REQUEST_TYPE, json!({"hidden": false}))),
            "the restore is sent even though the capture failed"
        );
        assert_eq!(out.run.page_restored, Some(true));
    }

    #[tokio::test]
    async fn an_unverified_or_failed_restore_is_reported_and_frame_b_kept() {
        for reply in [
            Ok(restored(false)),
            Ok(json!({"success": false, "error": "STATE_SCHEMA_MISMATCH"})),
            Err("transport".to_string()),
        ] {
            let (_sent, send) = webview(vec![Ok(hid()), reply.clone()]);
            let out = capture_text_hidden(send, || std::future::ready(Ok("frame-b"))).await;
            assert_eq!(out.frame, Ok("frame-b"), "{reply:?}");
            assert_eq!(out.run.page_restored, Some(false), "{reply:?}");
            assert!(out.run.restore.is_some(), "{reply:?}");
        }
    }

    #[tokio::test]
    async fn text_another_caller_hid_is_left_to_it() {
        let already = json!({"success": true, "hidden": true, "changed": false});
        let (sent, send) = webview(vec![Ok(already)]);
        let out = capture_text_hidden(send, || std::future::ready(Ok("frame-b"))).await;
        assert_eq!(out.frame, Ok("frame-b"));
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "no restore of a hide this call did not make"
        );
        assert_eq!(out.run.restore, None);
        assert_eq!(out.run.page_restored, None);
    }

    #[test]
    fn the_run_record_serializes_camel_case_and_omits_what_was_not_sent() {
        let run = TextVisibilityRun {
            hide: json!({"success": false}),
            restore: None,
            page_restored: None,
        };
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            json!({"hide": {"success": false}})
        );
        let run = TextVisibilityRun {
            hide: hid(),
            restore: Some(restored(true)),
            page_restored: Some(true),
        };
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["pageRestored"], json!(true));
        assert_eq!(v["restore"]["restore"]["verified"], json!(true));
    }
}
