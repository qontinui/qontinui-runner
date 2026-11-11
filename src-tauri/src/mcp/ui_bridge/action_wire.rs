//! The one place the runner spells its element-action wire convention.
//!
//! Every hop that carries an element action from an HTTP body to the
//! frontend's `execute_action` IPC handler (or to the same handler's twin on
//! an SDK app) builds its payload here, so a field the SDK's
//! `ControlActionRequest` grows reaches the receiver without a change to each
//! hop. The class this module closes is the one named in
//! `plans/2026-08-25-ui-bridge-request-path-loses-fields-structurally.md` and
//! its successor
//! `plans/2026-11-11-ui-bridge-request-path-loses-fields-successor-runner-twins-and-non-recurrence.md`:
//! a hop that rebuilds the request field by field drops every field added
//! after it was written.
//!
//! Hops that use it: the batch endpoints, the GraphQL mutation, the SDK
//! element route's IPC fallback, navigate-and-wait and the workflow step
//! executor's `element_action` step.

use serde_json::{json, Map, Value};

use super::request::{target_window_payload, TARGET_WINDOW_FIELD};

/// Step keys that address the *step* rather than the action it carries.
///
/// Used by [`step_action_payload`] to decide what to lift into the action
/// envelope. Everything NOT listed here rides along, so a field added to the
/// SDK action grammar reaches the frontend without a change to this file (the
/// frontend forwards the whole envelope to the SDK, `toControlActionRequest`).
pub(crate) const STEP_RESERVED_KEYS: &[&str] = &["type", "elementId", "element_id", "label"];

/// Why a request carries no usable action name. Both arms render as the typed
/// code [`ACTION_MISSING_CODE`] and dispatch nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionMissing {
    /// What was found where the action name belongs.
    found: &'static str,
}

/// The typed code of the refusal a request with no action name gets.
pub(crate) const ACTION_MISSING_CODE: &str = "action_missing";

impl ActionMissing {
    fn of(value: Option<&Value>) -> Self {
        let found = match value {
            None => "no `action` key",
            Some(Value::Null) => "null",
            Some(Value::Bool(_)) => "a boolean",
            Some(Value::Number(_)) => "a number",
            Some(Value::String(_)) => "an empty string",
            Some(Value::Array(_)) => "an array",
            Some(Value::Object(_)) => "an object",
        };
        Self { found }
    }

    /// The human-readable refusal, naming the field and what it held.
    pub(crate) fn message(&self) -> String {
        format!(
            "{ACTION_MISSING_CODE}: the request names no action; `action` must be a non-empty string (found {}). Nothing was dispatched.",
            self.found
        )
    }

    /// The refusal as an HTTP error envelope carrying the typed `code`, for
    /// routes whose error arm is `ApiResponse<()>` (navigate-and-wait).
    pub(crate) fn error_response(&self) -> crate::mcp::types::ApiResponse<()> {
        crate::mcp::types::ApiResponse {
            success: false,
            data: None,
            error: Some(self.message()),
            error_detail: None,
            hint: None,
            code: Some(ACTION_MISSING_CODE.to_string()),
            suggestions: None,
        }
    }

    /// The refusal as an action response (`success: false` plus the typed
    /// `code`), for hops that answer per step or per request.
    pub(crate) fn response(&self) -> Value {
        json!({
            "success": false,
            "code": ACTION_MISSING_CODE,
            "error": self.message(),
        })
    }
}

/// The action name of an action request, or the typed refusal.
///
/// A request with no action name is refused, never converted into a click: a
/// click is not what the caller asked for, and a missing name is a malformed
/// request (`/control/batch` documents `action` as required). The one default
/// the API documents is navigate-and-wait's, and it lives on that route's own
/// request type.
pub(crate) fn require_action_name(request: &Value) -> Result<String, ActionMissing> {
    match request.get("action") {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        other => Err(ActionMissing::of(other)),
    }
}

/// Build the `execute_action` IPC payload for an element action.
///
/// **This is the one place the element-action wire convention is spelled.**
/// The frontend handler (`src/hooks/ui-bridge-events/useControlEvents.ts`,
/// `case "execute_action"`) destructures `{ elementId, action }` and rejects
/// the request outright when either is missing. It then normalizes `action` as
/// *either* a bare string *or* the whole envelope
/// `{ action, params, waitOptions, ... }` and forwards that envelope to the
/// SDK (`toControlActionRequest`). A sibling `params` at the top level of the
/// payload is never read by anything.
///
/// Hand-rolling this payload is therefore a trap with two distinct failure
/// modes, and both were live defects fixed alongside this helper
/// (`plans/2026-08-25-ui-bridge-request-path-loses-fields-structurally.md`):
///
/// 1. Emitting `id` instead of `elementId` fails **every** request with
///    `"elementId and action are required"` — a hard, total failure.
/// 2. Flattening the envelope to a bare action string silently drops `params`,
///    `waitOptions` and every opt-in field. The request still looks well-formed
///    to the receiver, so it succeeds *wrongly*: a `type` action types nothing.
///
/// So the envelope is forwarded **by identity**, never rebuilt field-by-field.
///
/// The one deliberate exception is `windowLabel`, hoisted out of the envelope
/// to the payload root below — it is declared on the SDK envelope but consumed
/// at the root, so forwarding it verbatim is precisely what makes it inert.
/// A bare-string envelope passes through untouched (the frontend normalizes it).
///
/// An envelope with no usable action NAME (an object whose `action` is absent,
/// null, a number or empty; or a null, boolean, number or array envelope) is
/// refused with [`ActionMissing`]. It used to default to `click`.
pub(crate) fn element_action_payload(
    element_id: &str,
    action: Value,
) -> Result<Value, ActionMissing> {
    let mut action = match action {
        // The SDK proxy-fallback shape. The frontend turns `"click"` into
        // `{ action: "click" }` itself, so pass it through rather than
        // second-guessing it here.
        Value::String(s) if !s.trim().is_empty() => Value::String(s),
        Value::String(_) => return Err(ActionMissing::of(Some(&Value::String(String::new())))),
        Value::Object(obj) => {
            // Refuse on "no usable action NAME", not on "key absent": a
            // present-but-non-string `action` (a `null` from a serializer
            // emitting an absent optional, say) is as nameless as a missing one.
            match obj.get("action") {
                Some(Value::String(s)) if !s.trim().is_empty() => {}
                other => return Err(ActionMissing::of(other)),
            }
            Value::Object(obj)
        }
        // null / bool / number / array cannot carry an action name.
        other => return Err(ActionMissing::of(Some(&other))),
    };

    // `windowLabel` is DECLARED on the SDK's `ControlActionRequest` envelope
    // ("carried for transports that forward the request bag verbatim"), but it
    // is consumed at the payload ROOT by `split_target_window` — so an envelope
    // that merely carries it routes nothing and the step silently runs against
    // the main window. Forwarding the envelope whole is what makes that
    // reachable, so hoist it here, at the single chokepoint, rather than
    // leaving the one declared envelope field that does nothing.
    let window_label = action
        .as_object_mut()
        .and_then(|obj| obj.remove(TARGET_WINDOW_FIELD))
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty());

    Ok(target_window_payload(
        json!({
            "elementId": element_id,
            "action": action,
        }),
        window_label.as_deref(),
    ))
}

/// Build the `execute_action` payload for one batch step, in either grammar.
///
/// The batch endpoints disagree about how a step carries its action — which is
/// the drift this whole helper family exists to absorb — so both are accepted:
///
/// * **nested** (`{"elementId": "x", "action": {"action": "type", "params": {...}}}`),
///   used by `/control/batch-actions`' SDK caller. The envelope is forwarded by
///   identity.
/// * **flat** (`{"type": "action", "element_id": "x", "action": "type", "params": {...}}`),
///   used by `/control/batch-execute` and documented for `/control/batch`. Every
///   non-reserved key is lifted into the envelope, so `params`, `waitOptions`,
///   `expectChange` and any later opt-in ride along instead of being dropped by
///   a hand-maintained field list.
///
/// A nested envelope wins over a flat sibling of the same name; flat keys only
/// fill gaps. Accepting both matters because the runner's own capabilities
/// manifest documents `/control/batch-actions` steps as FLAT while the SDK type
/// is nested — so a caller following either is served, instead of one of them
/// silently losing its params.
///
/// The element id is read from `elementId` **or** `element_id`: the two
/// capability batch endpoints disagreed on the casing (one read each), so both
/// spellings are accepted rather than breaking whichever callers exist. Each
/// arm is checked for a *string*, so a `null` under the preferred spelling
/// falls through to the other rather than resolving to `""`.
pub(crate) fn step_action_payload(step: &Value) -> Result<Value, ActionMissing> {
    let element_id = step
        .get("elementId")
        .and_then(|v| v.as_str())
        .or_else(|| step.get("element_id").and_then(|v| v.as_str()))
        .unwrap_or_default();

    // Base: a nested `action` envelope if the step carries one.
    let mut envelope = match step.get("action") {
        Some(Value::Object(nested)) => nested.clone(),
        _ => Map::new(),
    };

    if let Some(obj) = step.as_object() {
        for (key, value) in obj {
            if STEP_RESERVED_KEYS.contains(&key.as_str()) {
                continue;
            }
            // An explicitly nested envelope wins over a flat sibling of the
            // same name; flat keys only fill gaps.
            if key == "action" && value.is_object() {
                continue;
            }
            envelope.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    element_action_payload(element_id, Value::Object(envelope))
}

/// The IPC payload for `POST /ui-bridge/sdk/element/{id}/action` when no SDK
/// app is connected and the action falls back to the runner's own webview.
///
/// The route's body IS the SDK's `ControlActionRequest`, and its WebSocket and
/// HTTP arms already forward it whole, so this arm must too. It used to rebuild
/// `{action, params, waitOptions, verifyEffect}` and drop every other field of
/// the request (`captureAfter`, `retryOptions`, `requestId`, `waitAfter*`,
/// `fromSnapshotId`, `includeResolutionAlternates`, `windowLabel`).
pub(crate) fn sdk_fallback_payload(element_id: &str, body: &Value) -> Result<Value, ActionMissing> {
    element_action_payload(element_id, body.clone())
}

/// The IPC payload for `POST /ui-bridge/control/navigate-and-wait`.
///
/// The route's own keys (`elementId`, `waitForStableMs`, `timeoutMs`) are typed
/// on the request and never reach the envelope. `action` and `params` are the
/// typed action half, and `rest` is every other key of the body, forwarded as
/// part of the action envelope. This is the additive shape of
/// `bookmarks.rs::with_diff_single_payload`.
pub(crate) fn navigate_and_wait_payload(
    element_id: &str,
    action: &str,
    params: Option<&Value>,
    rest: &Map<String, Value>,
) -> Result<Value, ActionMissing> {
    let mut envelope = rest.clone();
    envelope.insert("action".to_string(), Value::String(action.to_string()));
    envelope.insert(
        "params".to_string(),
        params.cloned().unwrap_or_else(|| json!({})),
    );
    element_action_payload(element_id, Value::Object(envelope))
}

/// Stamp navigate-and-wait's `windowLabel` (if the body carried one) onto a
/// follow-up IPC payload (`discover`, `get_snapshot`). The action runs in the
/// addressed pop-out window, so the stability poll and the snapshot have to
/// read the same window, not the main one.
pub(crate) fn navigate_and_wait_window_payload(base: Value, rest: &Map<String, Value>) -> Value {
    let label = rest
        .get(TARGET_WINDOW_FIELD)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    target_window_payload(base, label)
}

/// The route and body a workflow `element_action` step sends, built in one
/// place so the step posts exactly this and nothing else.
pub(crate) fn element_action_request(
    base_url: &str,
    snapshot_target: Option<&str>,
    element_id: &str,
    target: &Value,
) -> Result<(String, Value), String> {
    Ok((
        element_action_endpoint(base_url, snapshot_target, element_id)?,
        step_element_action_body(target),
    ))
}

/// The route a workflow `element_action` step posts to, for the snapshot target
/// the step names. Mirrors the snapshot arm and `component_action_endpoint`
/// (`step_executor/handlers/ui_bridge.rs`): `control` (the default) is the
/// runner's own UI route, `sdk` is the SDK's route through the runner, and
/// `proxy:PORT` is a proxied app. An element id taken from one target's
/// snapshot has to act on that same target.
///
/// The path is the SINGULAR `element/{id}/action`. The plural
/// `elements/{id}/action` the step used to post to is served by nothing.
pub(crate) fn element_action_endpoint(
    base_url: &str,
    snapshot_target: Option<&str>,
    element_id: &str,
) -> Result<String, String> {
    let path = format!("element/{}/action", urlencoding::encode(element_id));
    let base = base_url.trim_end_matches('/');
    match snapshot_target {
        None | Some("control") => Ok(format!("{base}/control/{path}")),
        Some("sdk") => Ok(format!("{base}/sdk/{path}")),
        Some(t) => match t.strip_prefix("proxy:") {
            // Parse the port: raw text here would let `proxy:80@host` turn
            // `127.0.0.1:80` into userinfo and send this state-changing POST to
            // `host`, past the network policy (which checks only raw_url).
            Some(port) => {
                let port: u16 = port.parse().map_err(|_| {
                    format!("Invalid proxy port in snapshot target '{t}': expected proxy:PORT")
                })?;
                Ok(format!(
                    "http://127.0.0.1:{port}/__ui-bridge/control/{path}"
                ))
            }
            None => Err(format!(
                "Unknown snapshot target: '{t}'. Use 'control', 'sdk', or 'proxy:PORT'"
            )),
        },
    }
}

/// The step-addressing key of a workflow `element_action` target: it picks the
/// element, and is not part of the action request.
const ELEMENT_ACTION_TARGET_ADDRESSING_KEYS: &[&str] = &["criteria"];

/// The request body a workflow `element_action` step posts: the step's target
/// minus its step-addressing key, forwarded whole. It used to rebuild
/// `{action, params}` and drop every other field of the target.
pub(crate) fn step_element_action_body(target: &Value) -> Value {
    let mut body = target.clone();
    if let Some(obj) = body.as_object_mut() {
        for key in ELEMENT_ACTION_TARGET_ADDRESSING_KEYS {
            obj.remove(*key);
        }
    }
    body
}

/// Regression tests for the element-action wire convention.
///
/// These exist because the convention was violated at four independent sites
/// and the violations were invisible to the compiler: every one of them built
/// an untyped `serde_json::json!` payload, so no amount of widening a DTO
/// would have caught them. The assertions below are deliberately written
/// against the *frontend handler's* reading of the payload
/// (`useControlEvents.ts` `case "execute_action"`), which is the actual
/// contract:
///   * it destructures `{ elementId, action }` — `id` is not a synonym;
///   * it reads `params` / `waitOptions` off the `action` envelope only.
#[cfg(test)]
mod element_action_payload_tests {
    use super::{target_window_payload, TARGET_WINDOW_FIELD};
    use serde_json::{json, Value};

    fn element_action_payload(id: &str, action: Value) -> Value {
        super::element_action_payload(id, action).expect("payload names an action")
    }
    fn step_action_payload(step: &Value) -> Value {
        super::step_action_payload(step).expect("step names an action")
    }

    #[test]
    fn payload_uses_element_id_key_never_id() {
        let payload = element_action_payload("btn-1", json!({"action": "click"}));

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1"),
            "frontend destructures `elementId`; anything else fails the request \
             with 'elementId and action are required'"
        );
        assert!(
            payload.get("id").is_none(),
            "`id` was the exact key that broke every batch step — it must not reappear"
        );
    }

    #[test]
    fn params_live_inside_the_action_envelope_not_as_a_sibling() {
        let payload = element_action_payload(
            "input-1",
            json!({"action": "type", "params": {"text": "hi"}}),
        );

        assert!(
            payload.get("params").is_none(),
            "a top-level `params` sibling is never read by the frontend"
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi"),
            "params must arrive nested in the action envelope"
        );
    }

    #[test]
    fn bare_string_action_passes_through_untouched() {
        // The SDK proxy-fallback shape. The frontend normalizes it itself.
        let payload = element_action_payload("btn-1", json!("click"));
        assert_eq!(
            payload.get("action").and_then(|v| v.as_str()),
            Some("click")
        );
    }


    /// The headline regression: a batch step must round-trip EVERY field.
    ///
    /// This is the structural assertion the class needs — it carries an
    /// `unknownFutureOptIn` key that no code in this repo knows about. If a
    /// later change reintroduces field-by-field rebuilding, that key vanishes
    /// and this test fails, which is exactly the signal that was missing when
    /// four sites drifted.
    #[test]
    fn batch_step_round_trips_every_field() {
        let step = json!({
            "label": "fill the search box",
            "elementId": "search-input",
            "action": {
                "action": "type",
                "params": {"text": "qontinui"},
                "waitOptions": {"visible": true, "timeout": 2000},
                "expectChange": true,
                "fromSnapshotId": "ubs2_a_b_c_d",
                "verifyEffect": {"mode": "strict"},
                "unknownFutureOptIn": {"nested": ["value"]}
            }
        });

        let payload = step_action_payload(&step);

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("search-input")
        );
        // The envelope must arrive by identity — not a rebuilt subset of it.
        assert_eq!(
            payload.get("action"),
            step.get("action"),
            "the action envelope must be forwarded whole, field-for-field"
        );
        // Spelled out, so a failure names the field that got dropped.
        for field in [
            "params",
            "waitOptions",
            "expectChange",
            "fromSnapshotId",
            "verifyEffect",
            "unknownFutureOptIn",
        ] {
            assert!(
                payload.pointer(&format!("/action/{field}")).is_some(),
                "batch step dropped `{field}` on the way to the frontend"
            );
        }
        // `label` addresses the step, not the action — it should not leak in.
        assert!(payload.pointer("/action/label").is_none());
    }

    #[test]
    fn flat_step_lifts_every_non_reserved_field_into_the_envelope() {
        let step = json!({
            "type": "action",
            "elementId": "btn-1",
            "label": "submit it",
            "action": "click",
            "params": {"button": "left"},
            "waitOptions": {"enabled": true},
            "expectChange": true,
            "unknownFutureOptIn": 42
        });

        let payload = step_action_payload(&step);

        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert!(payload.get("id").is_none());
        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("click")
        );
        for field in [
            "params",
            "waitOptions",
            "expectChange",
            "unknownFutureOptIn",
        ] {
            assert!(
                payload.pointer(&format!("/action/{field}")).is_some(),
                "flat step dropped `{field}` — the exact loss that made \
                 `waitOptions` unreachable on batch-execute"
            );
        }
        // Step-addressing keys must not be mistaken for action fields.
        for reserved in ["type", "label", "elementId", "element_id"] {
            assert!(
                payload.pointer(&format!("/action/{reserved}")).is_none(),
                "`{reserved}` addresses the step, not the action"
            );
        }
    }

    #[test]
    fn flat_step_accepts_both_element_id_spellings() {
        // The two capability batch endpoints had drifted: one read `element_id`,
        // the other `elementId`. Both must resolve.
        let snake = step_action_payload(&json!({"element_id": "btn-1", "action": "click"}));
        let camel = step_action_payload(&json!({"elementId": "btn-1", "action": "click"}));

        assert_eq!(
            snake.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert_eq!(
            camel.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
        assert_eq!(snake, camel);
    }

    #[test]
    fn flat_step_accepts_an_already_nested_action_envelope() {
        // The two batch grammars have drifted before, so the flat lifter also
        // takes the nested shape rather than forwarding an object as the
        // action *name* (which is never valid).
        let payload = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "type", "params": {"text": "hi"}, "waitOptions": {"visible": true}}
        }));

        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("type")
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi")
        );
        assert!(payload.pointer("/action/waitOptions").is_some());
    }

    #[test]
    fn nested_envelope_wins_over_a_flat_sibling_of_the_same_name() {
        let payload = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "type", "params": {"text": "nested"}},
            "params": {"text": "flat"}
        }));

        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("nested"),
            "an explicit envelope must not be overwritten by a flat sibling"
        );
    }


    #[test]
    fn flat_params_survive_a_bare_string_action_on_a_batch_step() {
        // The runner's own capabilities manifest documents batch-actions steps
        // as FLAT (`{elementId, action, params}`) while the SDK type is nested.
        // A caller following the manifest must not silently lose its params.
        let payload = step_action_payload(&json!({
            "elementId": "input-1",
            "action": "type",
            "params": {"text": "hi"}
        }));

        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("type")
        );
        assert_eq!(
            payload
                .pointer("/action/params/text")
                .and_then(|v| v.as_str()),
            Some("hi"),
            "flat params on a bare-string action step must reach the envelope"
        );
    }

    #[test]
    fn a_null_preferred_element_id_falls_through_to_the_other_spelling() {
        let payload = step_action_payload(
            &json!({"elementId": null, "element_id": "btn-1", "action": "click"}),
        );
        assert_eq!(
            payload.get("elementId").and_then(|v| v.as_str()),
            Some("btn-1")
        );
    }

    #[test]
    fn window_label_is_hoisted_to_the_payload_root_from_either_grammar() {
        // `windowLabel` is declared on the SDK action envelope but consumed at
        // the payload ROOT. Left in the envelope it routes nothing — the step
        // silently runs against the main window.
        let flat = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": "click",
            "windowLabel": "term-1"
        }));
        let nested = step_action_payload(&json!({
            "elementId": "btn-1",
            "action": {"action": "click", "windowLabel": "term-1"}
        }));

        for (name, payload) in [("flat", &flat), ("nested", &nested)] {
            assert_eq!(
                payload.get(TARGET_WINDOW_FIELD).and_then(|v| v.as_str()),
                Some("term-1"),
                "{name} step must route to the addressed window"
            );
            assert!(
                payload.pointer("/action/windowLabel").is_none(),
                "{name} step left `windowLabel` in the envelope, where nothing reads it"
            );
        }
    }

    #[test]
    fn a_main_or_absent_window_label_leaves_the_payload_unrouted() {
        // `target_window_payload` no-ops for main/empty, so the single-window
        // default payload stays byte-identical.
        for step in [
            json!({"elementId": "btn-1", "action": "click"}),
            json!({"elementId": "btn-1", "action": "click", "windowLabel": "main"}),
            json!({"elementId": "btn-1", "action": "click", "windowLabel": ""}),
        ] {
            let payload = step_action_payload(&step);
            assert!(
                payload.get(TARGET_WINDOW_FIELD).is_none(),
                "no routing field should be stamped for {step}"
            );
            assert_eq!(
                payload.pointer("/action/action").and_then(|v| v.as_str()),
                Some("click")
            );
        }
    }

    #[test]
    fn window_label_still_composes_onto_an_action_payload() {
        // `target_window_payload` stamps the routing field onto the payload
        // root; the action envelope must be left alone by it.
        let payload = target_window_payload(
            element_action_payload("btn-1", json!({"action": "click"})),
            Some("term-1"),
        );
        assert_eq!(
            payload.get(TARGET_WINDOW_FIELD).and_then(|v| v.as_str()),
            Some("term-1")
        );
        assert_eq!(
            payload.pointer("/action/action").and_then(|v| v.as_str()),
            Some("click")
        );
    }
}


/// Test support shared by every hop's round-trip table.
///
/// The key list is a LITERAL, not the SDK's list and not a constant of the
/// implementation: a field the SDK grows later is a red test only once someone
/// lists it here (Phase 9 of the plan pins this list against the SDK's).
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{json, Value};

    /// The 13 keys of the SDK's `ControlActionRequest` plus one the runner
    /// cannot know about.
    #[allow(dead_code)]
    pub(crate) const CONTROL_ACTION_REQUEST_KEYS: [&str; 14] = [
        "action",
        "params",
        "waitOptions",
        "windowLabel",
        "requestId",
        "captureAfter",
        "retryOptions",
        "waitAfter",
        "waitAfterTimeout",
        "waitAfterMinStable",
        "verifyEffect",
        "fromSnapshotId",
        "includeResolutionAlternates",
        "unknownFutureOptIn",
    ];

    /// A value for `key` that no default could produce by accident.
    pub(crate) fn sample_value(key: &str) -> Value {
        match key {
            "action" => json!("type"),
            "params" => json!({"text": "hi"}),
            "waitOptions" => json!({"visible": true, "timeout": 2000}),
            "windowLabel" => json!("term-1"),
            "requestId" => json!("req-7"),
            "captureAfter" => json!({"screenshot": true}),
            "retryOptions" => json!({"maxAttempts": 3}),
            "waitAfter" => json!({"stable": true}),
            "waitAfterTimeout" => json!(1234),
            "waitAfterMinStable" => json!(321),
            "verifyEffect" => json!({"mode": "strict"}),
            "fromSnapshotId" => json!("ubs2_a_b_c_d"),
            "includeResolutionAlternates" => json!(true),
            "unknownFutureOptIn" => json!({"nested": ["value"]}),
            other => panic!("no sample value for `{other}`"),
        }
    }

    /// Expand to one `#[test]` per key, each calling `$check("<key>")`.
    macro_rules! one_test_per_control_field {
        ($check:path) => {
            one_test_per_control_field!(@each $check;
                action => "action",
                params => "params",
                wait_options => "waitOptions",
                window_label => "windowLabel",
                request_id => "requestId",
                capture_after => "captureAfter",
                retry_options => "retryOptions",
                wait_after => "waitAfter",
                wait_after_timeout => "waitAfterTimeout",
                wait_after_min_stable => "waitAfterMinStable",
                verify_effect => "verifyEffect",
                from_snapshot_id => "fromSnapshotId",
                include_resolution_alternates => "includeResolutionAlternates",
                unknown_future_opt_in => "unknownFutureOptIn",
            );
        };
        (@each $check:path; $($name:ident => $key:literal),+ $(,)?) => {
            $(
                #[test]
                fn $name() {
                    $check($key);
                }
            )+
        };
    }
    pub(crate) use one_test_per_control_field;

    /// Assert that `key` (sent as `sample_value(key)`) reached `payload`, the
    /// IPC `execute_action` payload: at the payload root for `windowLabel`
    /// (routing consumes it there), inside the `action` envelope otherwise.
    pub(crate) fn assert_reached_ipc_payload(hop: &str, key: &str, payload: &Value) {
        let want = sample_value(key);
        if key == "windowLabel" {
            assert_eq!(
                payload.get("windowLabel"),
                Some(&want),
                "{hop}: `windowLabel` must be at the payload root, got {payload}"
            );
            assert!(
                payload.pointer("/action/windowLabel").is_none(),
                "{hop}: `windowLabel` left in the envelope, where nothing reads it: {payload}"
            );
        } else {
            assert_eq!(
                payload.pointer(&format!("/action/{key}")),
                Some(&want),
                "{hop}: `{key}` did not reach the action envelope: {payload}"
            );
            // The payload root's own `action` is the envelope itself.
            assert!(
                key == "action" || payload.get(key).is_none(),
                "{hop}: `{key}` also leaked to the payload root: {payload}"
            );
        }
    }
}

#[cfg(test)]
mod element_action_hop_tests {
    use super::test_support::{
        assert_reached_ipc_payload, one_test_per_control_field, sample_value,
    };
    use super::*;
    use serde_json::json;

    /// The batch endpoints' hop: a nested step envelope forwarded by identity.
    fn batch_step_check(key: &str) {
        let mut envelope = serde_json::Map::new();
        envelope.insert("action".into(), json!("type"));
        envelope.insert(key.into(), sample_value(key));
        let step = json!({"elementId": "e1", "action": envelope});
        assert_reached_ipc_payload(
            "batch step",
            key,
            &step_action_payload(&step).expect("step names an action"),
        );
    }
    mod batch_step {
        use super::*;
        one_test_per_control_field!(batch_step_check);
    }

    /// Probe 4: the path the step executor builds is a route the runner serves.
    #[test]
    fn step_element_action_path_is_in_the_route_manifest() {
        let url = element_action_endpoint("http://127.0.0.1:9876/ui-bridge", None, "ID").unwrap();
        let path = url
            .strip_prefix("http://127.0.0.1:9876")
            .expect("path under the runner origin")
            .replace("/ID/", "/{id}/");
        assert_eq!(path, "/ui-bridge/control/element/{id}/action");
        assert!(
            super::super::route_manifest()
                .iter()
                .any(|(m, p)| *m == "POST" && *p == path),
            "POST {path} is not served: the step would post to a route nothing serves"
        );
    }

    #[test]
    fn step_element_action_endpoint_follows_the_snapshot_target() {
        let base = "http://127.0.0.1:9876/ui-bridge/";
        assert_eq!(
            element_action_endpoint(base, Some("control"), "btn-1").unwrap(),
            "http://127.0.0.1:9876/ui-bridge/control/element/btn-1/action"
        );
        assert_eq!(
            element_action_endpoint(base, Some("sdk"), "btn-1").unwrap(),
            "http://127.0.0.1:9876/ui-bridge/sdk/element/btn-1/action"
        );
        assert_eq!(
            element_action_endpoint(base, Some("proxy:3001"), "btn-1").unwrap(),
            "http://127.0.0.1:3001/__ui-bridge/control/element/btn-1/action"
        );
    }

    #[test]
    fn step_element_action_endpoint_refuses_a_bad_target_and_a_bad_port() {
        let base = "http://127.0.0.1:9876/ui-bridge";
        assert!(element_action_endpoint(base, Some("proxy:80@evil"), "b")
            .unwrap_err()
            .contains("Invalid proxy port"));
        assert!(element_action_endpoint(base, Some("nope"), "b")
            .unwrap_err()
            .contains("Unknown snapshot target"));
    }

    #[test]
    fn step_element_action_endpoint_encodes_the_element_id() {
        let url = element_action_endpoint("http://h/ui-bridge", None, "a/b c").unwrap();
        assert_eq!(url, "http://h/ui-bridge/control/element/a%2Fb%20c/action");
    }
}

/// Phase 3: a request with no action name is refused, never clicked.
#[cfg(test)]
mod action_missing_tests {
    use super::*;
    use serde_json::json;

    fn assert_refused(what: &str, got: Result<Value, ActionMissing>) {
        let err = got.expect_err(what);
        assert!(
            err.message().starts_with("action_missing:"),
            "{what}: message must lead with the typed code, got {}",
            err.message()
        );
        let response = err.response();
        assert_eq!(response["code"], json!("action_missing"), "{what}");
        assert_eq!(response["success"], json!(false), "{what}");
    }

    #[test]
    fn element_action_payload_refuses_an_envelope_with_no_action_name() {
        assert_refused("empty object", element_action_payload("e1", json!({})));
        assert_refused(
            "null action",
            element_action_payload("e1", json!({"action": null})),
        );
        assert_refused(
            "numeric action",
            element_action_payload("e1", json!({"action": 42})),
        );
        assert_refused(
            "nested envelope with no name",
            element_action_payload("e1", json!({"action": {"params": {"text": "hi"}}})),
        );
        assert_refused(
            "empty-string action",
            element_action_payload("e1", json!({"action": ""})),
        );
    }

    #[test]
    fn element_action_payload_refuses_a_non_object_non_string_envelope() {
        for (what, envelope) in [
            ("null", json!(null)),
            ("boolean", json!(true)),
            ("number", json!(7)),
            ("array", json!([])),
            ("empty string", json!("")),
        ] {
            assert_refused(what, element_action_payload("e1", envelope));
        }
    }

    #[test]
    fn step_action_payload_refuses_a_step_with_no_action_name() {
        assert_refused("no action key", step_action_payload(&json!({"elementId": "e1"})));
        assert_refused(
            "null action",
            step_action_payload(&json!({"elementId": "e1", "action": null})),
        );
        assert_refused(
            "numeric action",
            step_action_payload(&json!({"elementId": "e1", "action": 42})),
        );
        assert_refused(
            "nested envelope with no name",
            step_action_payload(&json!({"elementId": "e1", "action": {"params": {}}})),
        );
        assert_refused(
            "flat step with params only",
            step_action_payload(&json!({"elementId": "e1", "params": {"text": "hi"}})),
        );
        assert_refused(
            "array action",
            step_action_payload(&json!({"elementId": "e1", "action": []})),
        );
    }

    #[test]
    fn sdk_fallback_payload_refuses_a_body_with_no_action_name() {
        for body in [
            json!({}),
            json!({"action": null}),
            json!({"action": 42}),
            json!({"params": {"text": "hi"}}),
        ] {
            assert_refused("sdk fallback", sdk_fallback_payload("e1", &body));
        }
    }

    #[test]
    fn require_action_name_refuses_what_the_sdk_handler_and_the_step_executor_read() {
        for target in [
            json!({}),
            json!({"action": null}),
            json!({"action": 42}),
            json!({"action": {"action": "click"}}),
            json!({"action": "  "}),
        ] {
            let err = require_action_name(&target).expect_err("no usable name");
            assert!(err.message().contains("`action`"), "{target}: {}", err.message());
        }
        assert_eq!(
            require_action_name(&json!({"action": "type"})).as_deref(),
            Ok("type")
        );
    }

    #[test]
    fn navigate_and_wait_payload_refuses_an_empty_action() {
        assert_refused(
            "empty",
            navigate_and_wait_payload("e1", "", None, &serde_json::Map::new()),
        );
    }
}

#[cfg(test)]
mod action_missing_message_window_and_step_request_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_action_missing_message_is_one_exact_sentence() {
        let err = require_action_name(&json!({"action": null})).unwrap_err();
        assert_eq!(
            err.message(),
            "action_missing: the request names no action; `action` must be a non-empty string (found null). Nothing was dispatched."
        );
        let err = require_action_name(&json!({})).unwrap_err();
        assert_eq!(
            err.message(),
            "action_missing: the request names no action; `action` must be a non-empty string (found no `action` key). Nothing was dispatched."
        );
    }

    #[test]
    fn the_http_error_envelope_carries_the_typed_code() {
        let env = require_action_name(&json!({})).unwrap_err().error_response();
        assert!(!env.success);
        assert_eq!(env.code.as_deref(), Some("action_missing"));
        assert!(env.error.unwrap().starts_with("action_missing:"));
    }

    #[test]
    fn navigate_and_wait_follow_up_reads_target_the_actions_window() {
        let mut rest = Map::new();
        rest.insert("windowLabel".into(), json!("term-1"));
        for base in [json!({"interactive_only": false}), json!({})] {
            let p = navigate_and_wait_window_payload(base, &rest);
            assert_eq!(p["windowLabel"], json!("term-1"), "{p}");
        }
        // The action payload itself carries it at the root too.
        let action = navigate_and_wait_payload("e1", "click", None, &rest).unwrap();
        assert_eq!(action["windowLabel"], json!("term-1"));
    }

    #[test]
    fn navigate_and_wait_follow_up_reads_stay_unrouted_for_main_or_absent() {
        let mut main = Map::new();
        main.insert("windowLabel".into(), json!("main"));
        let mut empty = Map::new();
        empty.insert("windowLabel".into(), json!(""));
        for rest in [Map::new(), main, empty] {
            let p = navigate_and_wait_window_payload(json!({"x": 1}), &rest);
            assert!(p.get("windowLabel").is_none(), "{p}");
        }
    }

    #[test]
    fn the_step_request_is_the_singular_route_and_the_whole_target() {
        let target = json!({
            "criteria": {"textContent": "Save"},
            "action": "type",
            "params": {"text": "hi"},
            "verifyEffect": {"mode": "strict"},
            "unknownFutureOptIn": {"nested": ["value"]}
        });
        let (url, body) = element_action_request(
            "http://127.0.0.1:9876/ui-bridge",
            Some("sdk"),
            "e1",
            &target,
        )
        .unwrap();
        assert_eq!(url, "http://127.0.0.1:9876/ui-bridge/sdk/element/e1/action");
        assert_eq!(
            body,
            json!({
                "action": "type",
                "params": {"text": "hi"},
                "verifyEffect": {"mode": "strict"},
                "unknownFutureOptIn": {"nested": ["value"]}
            })
        );
        assert!(!url.contains("/elements/"));
    }
}
