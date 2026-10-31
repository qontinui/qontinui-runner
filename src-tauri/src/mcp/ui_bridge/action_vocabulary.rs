//! Action-verb vocabulary for the `/control/element/{id}/action` gates.
//!
//! An agent that names a verb the element does not accept (`press` on the
//! terminal command bar, `click` on a native element that advertises `press`)
//! used to get the GLOBAL built-in list back and no suggestion, so it had to
//! guess the element's own vocabulary. This module answers instead with:
//!
//!   * the element's own accepted verbs,
//!   * a "did you mean" — the first synonym candidate the element ACTUALLY
//!     accepts (so a suggestion is never a verb the element would also
//!     reject), and
//!   * a params hint for the suggested verb (`sendKeys` → `{"keys": [...]}`,
//!     `type` → `{"text": "…"}`), because the right verb with the wrong params
//!     shape is the next 400.
//!
//! The synonym table is one literal map whose candidate lists name both the
//! web verb (`click`) and the native verb (`press`); the per-element accepted
//! filter chooses between them. The ui-bridge SDK carries the same table in
//! TypeScript (`packages/ui-bridge/src/control/actionVocabulary.ts`); each
//! copy's literal tests pin its entries (plan
//! `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`, D3).

use serde_json::{json, Value};

use super::types::{RecoveryHint, UiBridgeError, UiBridgeErrorCode};

/// Requested verb → ordered candidate verbs. Lookup is case-insensitive, so
/// each key appears once in lower or canonical form. The suggestion is the
/// first candidate the element accepts.
pub(crate) const ACTION_VERB_SYNONYMS: &[(&str, &[&str])] = &[
    // Key presses. `press` keeps its native meaning wherever the element
    // advertises `press` itself: an exact accepted match is never corrected.
    ("press", &["sendKeys", "click"]),
    ("keypress", &["sendKeys", "click"]),
    ("presskey", &["sendKeys", "click"]),
    ("key", &["sendKeys", "click"]),
    ("keydown", &["sendKeys", "click"]),
    // Text entry.
    ("fill", &["type"]),
    ("input", &["type"]),
    ("enter", &["type"]),
    ("write", &["type"]),
    ("typetext", &["type"]),
    // Activation: web `click`, native `press` (the ui-bridge-native tap verb).
    ("tap", &["click", "press"]),
    ("activate", &["click", "press"]),
    ("click", &["press"]),
    // Mouse-button variants.
    ("rightclick", &["rightClick"]),
    ("contextmenu", &["rightClick"]),
    ("middleclick", &["middleClick"]),
    ("dblclick", &["doubleClick"]),
    // Pointer / scroll.
    ("mouseover", &["hover"]),
    ("mouseenter", &["hover"]),
    ("scrollto", &["scrollIntoView"]),
];

/// The verb an agent most likely meant, chosen from `accepted` only.
///
/// Order:
///   1. `requested` is itself accepted (exact) → `None`; it is never corrected.
///   2. A case-only difference (`sendkeys` → `sendKeys`).
///   3. The first [`ACTION_VERB_SYNONYMS`] candidate present in `accepted`.
///
/// Returns `None` when nothing the element accepts fits.
pub(crate) fn suggest_action<S: AsRef<str>>(requested: &str, accepted: &[S]) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() || accepted.iter().any(|a| a.as_ref() == requested) {
        return None;
    }
    if let Some(case_fix) = accepted
        .iter()
        .map(AsRef::as_ref)
        .find(|a| a.eq_ignore_ascii_case(requested))
    {
        return Some(case_fix.to_string());
    }
    let (_, candidates) = ACTION_VERB_SYNONYMS
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(requested))?;
    candidates
        .iter()
        .find(|c| accepted.iter().any(|a| a.as_ref() == **c))
        .map(|c| (*c).to_string())
}

/// A params literal for `verb`, or `None` when the verb takes none worth
/// naming. For `sendKeys` the key the caller already named (`params.key`, or a
/// string `params.keys`) is carried over, so `press {key:"Escape"}` is hinted
/// as `sendKeys {keys:[{key:"Escape"}]}`. The descriptor form is used because
/// every SDK version accepts it (bare key-name strings are newer).
pub(crate) fn params_hint(verb: &str, requested_params: Option<&Value>) -> Option<Value> {
    match verb {
        "sendKeys" => {
            let key = requested_params
                .and_then(|p| {
                    p.get("key")
                        .and_then(Value::as_str)
                        .or_else(|| p.get("keys").and_then(Value::as_str))
                })
                .filter(|k| !k.trim().is_empty())
                .unwrap_or("Enter");
            Some(json!({ "keys": [{ "key": key }] }))
        }
        "type" => Some(json!({ "text": "…" })),
        "select" | "setValue" => Some(json!({ "value": "…" })),
        _ => None,
    }
}

/// A resolved "did you mean" plus its params hint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActionSuggestion {
    pub verb: String,
    pub params_hint: Option<Value>,
}

impl ActionSuggestion {
    /// Resolve the suggestion for `requested` against `accepted`.
    pub(crate) fn resolve<S: AsRef<str>>(
        requested: &str,
        accepted: &[S],
        requested_params: Option<&Value>,
    ) -> Option<Self> {
        let verb = suggest_action(requested, accepted)?;
        let params_hint = params_hint(&verb, requested_params);
        Some(Self { verb, params_hint })
    }

    /// ` Did you mean 'sendKeys'? (params: {"keys":[{"key":"Enter"}]})`
    pub(crate) fn message_suffix(&self) -> String {
        match &self.params_hint {
            Some(hint) => format!(" Did you mean '{}'? (params: {})", self.verb, hint),
            None => format!(" Did you mean '{}'?", self.verb),
        }
    }

    /// Add `didYouMean` (and `paramsHint` when there is one) to an error
    /// context object.
    pub(crate) fn attach_to(&self, context: &mut Value) {
        if let Some(obj) = context.as_object_mut() {
            obj.insert("didYouMean".to_string(), Value::String(self.verb.clone()));
            if let Some(hint) = &self.params_hint {
                obj.insert("paramsHint".to_string(), hint.clone());
            }
        }
    }
}

/// The deferred unknown-action gate's decision, as a pure function so the
/// handler's 400 body is testable without IPC.
///
/// Rejects only when the verb is not a built-in, not a registered custom
/// action, and not in the element's own advertised list — so a verb the
/// element advertises is accepted here, and a rejected verb can never appear
/// in the accepted list the error names. `None` advertised list (probe failed
/// or element advertises nothing) → never rejects: a rejection is not
/// manufactured from missing metadata.
pub(crate) fn deferred_unknown_action_error(
    requested: &str,
    element_id: &str,
    action_is_builtin: bool,
    action_is_registered_custom: bool,
    element_supported_actions: Option<&[String]>,
    requested_params: Option<&Value>,
) -> Option<UiBridgeError> {
    if action_is_builtin || action_is_registered_custom {
        return None;
    }
    let accepted = element_supported_actions?;
    if accepted.iter().any(|a| a == requested) {
        return None;
    }
    Some(UiBridgeError::invalid_action(
        requested,
        Some(element_id),
        accepted,
        requested_params,
    ))
}

/// The pre-IPC `ACTION_NOT_SUPPORTED` body: a known verb the element does not
/// advertise. Carries the element's list plus the same `didYouMean` /
/// `paramsHint` the unknown-verb gate gives.
pub(crate) fn action_not_supported_error(
    requested: &str,
    element_id: &str,
    supported: &[String],
    requested_params: Option<&Value>,
) -> UiBridgeError {
    let suggestion = ActionSuggestion::resolve(requested, supported, requested_params);
    let mut message = format!(
        "Action '{}' is not supported by element '{}'. Element advertises: {:?}.",
        requested, element_id, supported
    );
    let mut context = json!({
        "code": "ACTION_NOT_SUPPORTED",
        "supported_actions": supported,
        "requested_action": requested,
    });
    if let Some(s) = &suggestion {
        message.push_str(&s.message_suffix());
        s.attach_to(&mut context);
    }
    UiBridgeError {
        code: UiBridgeErrorCode::ActionNotSupported,
        message,
        recovery: Some(RecoveryHint::Unrecoverable),
        context: Some(context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::api_error_detailed;
    use crate::mcp::ui_bridge::elements::{
        extract_custom_actions, extract_supported_actions, validate_action_name,
    };

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The terminal command-bar input's advertised list, as measured
    /// (`CommandBar.tsx` `COMMAND_BAR_INPUT_ACTIONS`).
    const TERMINAL_INPUT_ACTIONS: &[&str] = &[
        "focus",
        "blur",
        "hover",
        "scroll",
        "scrollIntoView",
        "click",
        "hoverClick",
        "type",
        "clear",
        "sendKeys",
    ];

    // ── table literals ────────────────────────────────────────────────────

    #[test]
    fn table_entries_are_literal() {
        let lookup = |k: &str| {
            ACTION_VERB_SYNONYMS
                .iter()
                .find(|(from, _)| *from == k)
                .map(|(_, c)| c.to_vec())
                .unwrap_or_else(|| panic!("table must carry '{k}'"))
        };
        assert_eq!(lookup("press"), vec!["sendKeys", "click"]);
        assert_eq!(lookup("keypress"), vec!["sendKeys", "click"]);
        assert_eq!(lookup("presskey"), vec!["sendKeys", "click"]);
        assert_eq!(lookup("key"), vec!["sendKeys", "click"]);
        assert_eq!(lookup("keydown"), vec!["sendKeys", "click"]);
        for k in ["fill", "input", "enter", "write", "typetext"] {
            assert_eq!(lookup(k), vec!["type"], "{k}");
        }
        assert_eq!(lookup("tap"), vec!["click", "press"]);
        assert_eq!(lookup("activate"), vec!["click", "press"]);
        assert_eq!(lookup("click"), vec!["press"]);
        assert_eq!(lookup("rightclick"), vec!["rightClick"]);
        assert_eq!(lookup("contextmenu"), vec!["rightClick"]);
        assert_eq!(lookup("middleclick"), vec!["middleClick"]);
        assert_eq!(lookup("dblclick"), vec!["doubleClick"]);
        assert_eq!(lookup("mouseover"), vec!["hover"]);
        assert_eq!(lookup("mouseenter"), vec!["hover"]);
        assert_eq!(lookup("scrollto"), vec!["scrollIntoView"]);
    }

    #[test]
    fn table_keys_are_unique_case_insensitively() {
        for (i, (a, _)) in ACTION_VERB_SYNONYMS.iter().enumerate() {
            for (b, _) in &ACTION_VERB_SYNONYMS[i + 1..] {
                assert!(!a.eq_ignore_ascii_case(b), "duplicate key '{a}' / '{b}'");
            }
        }
    }

    // ── suggest_action ────────────────────────────────────────────────────

    #[test]
    fn press_suggests_send_keys_when_accepted() {
        let accepted = list(TERMINAL_INPUT_ACTIONS);
        assert_eq!(
            suggest_action("press", &accepted).as_deref(),
            Some("sendKeys")
        );
    }

    #[test]
    fn press_suggests_click_when_only_click_accepted() {
        let accepted = list(&["click", "focus"]);
        assert_eq!(suggest_action("press", &accepted).as_deref(), Some("click"));
    }

    #[test]
    fn keypress_suggests_send_keys() {
        let accepted = list(&["click", "sendKeys"]);
        assert_eq!(
            suggest_action("keypress", &accepted).as_deref(),
            Some("sendKeys")
        );
        assert_eq!(
            suggest_action("keyPress", &accepted).as_deref(),
            Some("sendKeys")
        );
        assert_eq!(
            suggest_action("keyDown", &accepted).as_deref(),
            Some("sendKeys")
        );
    }

    #[test]
    fn no_suggestion_when_no_candidate_is_accepted() {
        let accepted = list(&["focus", "blur"]);
        assert_eq!(suggest_action("press", &accepted), None);
        assert_eq!(suggest_action("slap", &accepted), None);
        assert_eq!(suggest_action("rightclick", &accepted), None);
    }

    #[test]
    fn case_only_fix() {
        let accepted = list(TERMINAL_INPUT_ACTIONS);
        assert_eq!(
            suggest_action("sendkeys", &accepted).as_deref(),
            Some("sendKeys")
        );
        assert_eq!(
            suggest_action("SCROLLINTOVIEW", &accepted).as_deref(),
            Some("scrollIntoView")
        );
        // A case-only fix wins over the synonym table: `Click` on an element
        // accepting both `click` and `press` means `click`.
        let both = list(&["click", "press"]);
        assert_eq!(suggest_action("Click", &both).as_deref(), Some("click"));
    }

    #[test]
    fn exact_accepted_match_is_never_corrected() {
        let native = list(&["press", "focus"]);
        assert_eq!(suggest_action("press", &native), None);
        let web = list(&["click"]);
        assert_eq!(suggest_action("click", &web), None);
    }

    #[test]
    fn native_and_web_activation_verbs() {
        assert_eq!(
            suggest_action("click", &list(&["press"])).as_deref(),
            Some("press")
        );
        assert_eq!(
            suggest_action("tap", &list(&["press"])).as_deref(),
            Some("press")
        );
        assert_eq!(
            suggest_action("tap", &list(&["click", "press"])).as_deref(),
            Some("click")
        );
        assert_eq!(
            suggest_action("contextMenu", &list(&["rightClick"])).as_deref(),
            Some("rightClick")
        );
        assert_eq!(
            suggest_action("middleclick", &list(&["middleClick"])).as_deref(),
            Some("middleClick")
        );
        assert_eq!(
            suggest_action("dblClick", &list(&["doubleClick"])).as_deref(),
            Some("doubleClick")
        );
        assert_eq!(
            suggest_action("fill", &list(&["type"])).as_deref(),
            Some("type")
        );
        assert_eq!(
            suggest_action("scrollTo", &list(&["scrollIntoView"])).as_deref(),
            Some("scrollIntoView")
        );
    }

    #[test]
    fn suggestion_is_always_an_accepted_verb() {
        let accepted = list(TERMINAL_INPUT_ACTIONS);
        for (from, _) in ACTION_VERB_SYNONYMS {
            if let Some(s) = suggest_action(from, &accepted) {
                assert!(accepted.contains(&s), "'{from}' suggested unaccepted '{s}'");
            }
        }
    }

    // ── params hints ──────────────────────────────────────────────────────

    #[test]
    fn params_hints_are_literal() {
        assert_eq!(
            params_hint("sendKeys", None),
            Some(json!({ "keys": [{ "key": "Enter" }] }))
        );
        assert_eq!(
            params_hint("sendKeys", Some(&json!({ "key": "Escape" }))),
            Some(json!({ "keys": [{ "key": "Escape" }] }))
        );
        assert_eq!(params_hint("type", None), Some(json!({ "text": "…" })));
        assert_eq!(params_hint("click", None), None);
    }

    // ── gate decisions ────────────────────────────────────────────────────

    #[test]
    fn a_verb_the_element_advertises_passes_the_deferred_gate() {
        let advertised = list(&["focus", "pasteText"]);
        assert!(deferred_unknown_action_error(
            "pasteText",
            "el",
            false,
            false,
            Some(&advertised),
            None
        )
        .is_none());
    }

    #[test]
    fn missing_metadata_never_rejects() {
        assert!(deferred_unknown_action_error("press", "el", false, false, None, None).is_none());
    }

    #[test]
    fn a_rejected_verb_never_appears_in_the_accepted_list() {
        let advertised = list(TERMINAL_INPUT_ACTIONS);
        for verb in ["press", "slap", "keydown", "rightclick", "fill"] {
            let err =
                deferred_unknown_action_error(verb, "el", false, false, Some(&advertised), None)
                    .unwrap_or_else(|| panic!("{verb} must be rejected"));
            let ctx = err.context.expect("context");
            let named: Vec<&str> = ctx["supportedActions"]
                .as_array()
                .expect("supportedActions array")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert!(
                !named.contains(&verb),
                "rejected '{verb}' listed as accepted"
            );
            if let Some(dym) = ctx.get("didYouMean").and_then(Value::as_str) {
                assert!(
                    named.contains(&dym),
                    "didYouMean '{dym}' not in accepted list"
                );
            }
        }
    }

    #[test]
    fn action_not_supported_carries_did_you_mean() {
        let err =
            action_not_supported_error("click", "native-btn", &list(&["press", "focus"]), None);
        assert!(matches!(err.code, UiBridgeErrorCode::ActionNotSupported));
        let ctx = err.context.as_ref().expect("context");
        assert_eq!(ctx["didYouMean"], json!("press"));
        assert_eq!(ctx["requested_action"], json!("click"));
        assert!(
            err.message.contains("Did you mean 'press'?"),
            "{}",
            err.message
        );
        // No candidate accepted → no didYouMean key at all.
        let none = action_not_supported_error("drag", "b", &list(&["focus"]), None);
        assert!(none.context.unwrap().get("didYouMean").is_none());
    }

    /// Handler-level: the 400 body `execute_action` returns for
    /// `{"action":"press","params":{"key":"Enter"}}` against a fake element
    /// advertising the measured terminal-input list, built through the same
    /// steps the handler runs (advertised-list extraction, built-in check,
    /// deferred gate, `api_error_detailed`).
    #[test]
    fn press_on_terminal_input_400_body() {
        let element = json!({
            "id": "terminal.command-bar-input",
            "actions": TERMINAL_INPUT_ACTIONS,
        });
        let params = json!({ "key": "Enter" });
        let advertised = extract_supported_actions(&element);
        assert!(extract_custom_actions(&element).is_none());
        let is_builtin = validate_action_name("press").is_ok();
        let detail = deferred_unknown_action_error(
            "press",
            "terminal.command-bar-input",
            is_builtin,
            false,
            advertised.as_deref(),
            Some(&params),
        )
        .expect("press must be rejected on the terminal input");
        let body = serde_json::to_value(api_error_detailed(detail.message.clone(), detail))
            .expect("serializes");

        assert_eq!(body["success"], json!(false));
        let ed = &body["error_detail"];
        assert_eq!(ed["code"], json!("INVALID_REQUEST"));
        let ctx = &ed["context"];
        assert_eq!(ctx["action"], json!("press"));
        assert_eq!(ctx["elementId"], json!("terminal.command-bar-input"));
        assert_eq!(ctx["supportedActions"], json!(TERMINAL_INPUT_ACTIONS));
        assert_eq!(ctx["didYouMean"], json!("sendKeys"));
        assert_eq!(ctx["paramsHint"], json!({ "keys": [{ "key": "Enter" }] }));
        assert_eq!(
            body["error"],
            json!(
                "Unknown action 'press' for element 'terminal.command-bar-input'. \
                 This element accepts: focus, blur, hover, scroll, scrollIntoView, click, \
                 hoverClick, type, clear, sendKeys. Did you mean 'sendKeys'? \
                 (params: {\"keys\":[{\"key\":\"Enter\"}]})"
            )
        );
    }
}
