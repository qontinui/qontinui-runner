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
//!   * a params hint for the suggested verb (`sendKeys` → `{"keys": ["Enter"]}`,
//!     `type` → `{"text": "…"}`), because the right verb with the wrong params
//!     shape is the next 400.
//!
//! The synonym table is one literal map whose candidate lists name both the
//! web verb (`click`) and the native verb (`press`); the per-element accepted
//! filter chooses between them. The ui-bridge SDK carries TypeScript copies
//! (`packages/ui-bridge/src/control/actionVocabulary.ts` and its
//! `ui-bridge-native` twin; plan
//! `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`, D3 /
//! Phase 8), whose `foldVerb` matches `fold_verb`. No test spans the copies;
//! each pins its own entries as literals, so keep them in sync.

use serde_json::{json, Value};

use super::types::{RecoveryHint, UiBridgeError, UiBridgeErrorCode};

/// A verb reduced to what spelling cannot change: ASCII lowercase, no `_` or
/// `-`. `scroll_into_view`, `scroll-into-view` and `SCROLLINTOVIEW` all fold to
/// `scrollintoview`, because the runner's own built-in list carries snake-case
/// aliases and an agent writing `send_keys` plainly means `sendKeys`.
fn fold_verb(verb: &str) -> String {
    verb.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Requested verb → ordered candidate verbs. Lookup goes through [`fold_verb`],
/// so each key is written once, in its folded form. The suggestion is the first
/// candidate the element accepts.
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
    // Built-in aliases (`SUPPORTED_ACTION_NAMES`) the SDK does not infer into an
    // element's list. They pass the unknown-verb gate and stop at
    // `ACTION_NOT_SUPPORTED`, so without these the 400 for `double` would name no
    // suggestion.
    ("double", &["doubleClick"]),
    ("right", &["rightClick"]),
    ("middle", &["middleClick"]),
    // Pointer / scroll.
    ("mouseover", &["hover"]),
    ("mouseenter", &["hover"]),
    ("scrollto", &["scrollIntoView"]),
];

/// The verb an agent most likely meant, chosen from `accepted` only.
///
/// Order:
///   1. `requested` is itself accepted (exact) → `None`; it is never corrected.
///   2. A spelling-only difference (`sendkeys`, `send_keys` or ` sendKeys ` →
///      `sendKeys`).
///   3. The first [`ACTION_VERB_SYNONYMS`] candidate present in `accepted`.
///
/// Returns `None` when nothing the element accepts fits.
pub(crate) fn suggest_action<S: AsRef<str>>(requested: &str, accepted: &[S]) -> Option<String> {
    if requested.trim().is_empty() || accepted.iter().any(|a| a.as_ref() == requested) {
        return None;
    }
    let folded = fold_verb(requested.trim());
    if let Some(spelling_fix) = accepted
        .iter()
        .map(AsRef::as_ref)
        .find(|a| fold_verb(a) == folded)
    {
        return Some(spelling_fix.to_string());
    }
    let (_, candidates) = ACTION_VERB_SYNONYMS
        .iter()
        .find(|(from, _)| *from == folded)?;
    candidates
        .iter()
        .find(|c| accepted.iter().any(|a| a.as_ref() == **c))
        .map(|c| (*c).to_string())
}

/// Absent, `null`, or an empty string, array or object: the caller named nothing.
fn is_unnamed(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// A string `sendKeys` could take as a key. A lone space is the space key; any
/// other whitespace or a control character means text (`ls\r`), which is `type`'s
/// job and is not reliably a key name to either consumer.
fn is_key_token(token: &str) -> bool {
    token == " "
        || (!token.is_empty() && !token.chars().any(|c| c.is_whitespace() || c.is_control()))
}

/// `enter` → `Enter`: the SDK validates a bare key name case-sensitively, while
/// the terminal pane folds case, so the canonical spelling serves both. Any other
/// name passes through for `sendKeys` to resolve or refuse.
fn canonical_key_name(key: &str) -> &str {
    const NAMES: &[&str] = &[
        "Enter",
        "Escape",
        "Tab",
        "Backspace",
        "Delete",
        "Insert",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
    ];
    NAMES
        .iter()
        .find(|name| name.eq_ignore_ascii_case(key))
        .copied()
        .unwrap_or(key)
}

/// An object of the modifier flags `sendKeys` reads. The SDK ignores an unknown
/// name and reads `"false"` as true, so it would press the key with the wrong
/// modifiers. The terminal pane refuses both.
fn is_modifier_set(value: &Value) -> bool {
    value.as_object().is_some_and(|flags| {
        flags.iter().all(|(name, flag)| {
            matches!(name.as_str(), "ctrl" | "shift" | "alt" | "meta") && flag.is_boolean()
        })
    })
}

/// One well-formed entry of a `keys` array: a key token, or a descriptor with a
/// non-empty `key` and, if it has `modifiers`, a set of them.
fn is_key_entry(entry: &Value) -> bool {
    match entry {
        Value::String(token) => is_key_token(token),
        Value::Object(descriptor) => {
            descriptor
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| !key.is_empty())
                && descriptor.get("modifiers").is_none_or(is_modifier_set)
        }
        _ => false,
    }
}

/// The `sendKeys` params for what the caller already named, or `None` when no
/// hint can carry it faithfully. Copying a hint has to press the key the caller
/// asked for, so a request that cannot be restated gets no hint at all rather
/// than one naming a different key.
///
///   * Any non-empty field beyond `key`, `keys` and `modifiers`
///     (`{"value":"Escape"}`, `{"key":"c","ctrl":true}`): no hint, since it may
///     change which key is meant.
///   * Nothing named: `{"keys":["Enter"]}`, the shape with an example key.
///   * An array `keys`: kept as sent, when every entry is well formed.
///   * One key (`key`, or a string `keys`): a bare string, the form
///     `normalizeKeyDescriptors` aliases and validates. As a descriptor, `esc`
///     or `ctrl+Enter` would dispatch one key press literally so named and report
///     success. A terminal pane has no combo grammar, so it refuses a bare
///     `ctrl+c` hint, but loudly and before writing any bytes.
///   * A single character beside a set of `modifiers`: a descriptor, the form
///     that takes both and needs no alias. A longer name would be dispatched
///     unresolved, for the reason above, so it gets no hint.
///   * Anything else (both `key` and `keys`, `modifiers` beside an array or with
///     no key, a malformed value): no hint.
///
/// A name neither consumer knows (`hello`) still gets a hint. `sendKeys` refuses
/// it loudly, which is the answer the caller needs.
fn send_keys_hint(params: Option<&Value>) -> Option<Value> {
    let named = |field: &str| {
        params
            .and_then(|p| p.get(field))
            .filter(|value| !is_unnamed(value))
    };
    let names_only_keys = match params {
        None => true,
        Some(Value::Object(fields)) => fields.iter().all(|(name, value)| {
            matches!(name.as_str(), "key" | "keys" | "modifiers") || is_unnamed(value)
        }),
        Some(other) => is_unnamed(other),
    };
    if !names_only_keys {
        return None;
    }
    let modifiers = named("modifiers");
    let keys = match (named("key"), named("keys")) {
        (None, None) if modifiers.is_none() => vec![json!("Enter")],
        (None, Some(Value::Array(entries)))
            if modifiers.is_none() && entries.iter().all(is_key_entry) =>
        {
            entries.clone()
        }
        (Some(Value::String(key)), None) | (None, Some(Value::String(key)))
            if is_key_token(key) =>
        {
            match modifiers {
                None => vec![json!(canonical_key_name(key))],
                Some(modifiers) if is_modifier_set(modifiers) && key.chars().count() == 1 => {
                    vec![json!({ "key": key, "modifiers": modifiers })]
                }
                Some(_) => return None,
            }
        }
        _ => return None,
    };
    Some(json!({ "keys": keys }))
}

/// A params literal for `verb`, or `None` when the verb takes none worth naming
/// or, for `sendKeys`, when [`send_keys_hint`] cannot restate the request.
pub(crate) fn params_hint(verb: &str, requested_params: Option<&Value>) -> Option<Value> {
    match verb {
        "sendKeys" => send_keys_hint(requested_params),
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

    /// ` Did you mean 'sendKeys'? (params: {"keys":["Enter"]})`
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
/// or the element carries no action fields) → never rejects: a rejection is not
/// manufactured from missing metadata. An explicit empty list is metadata, and
/// rejects.
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
        element_id,
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
        extract_custom_actions, extract_supported_actions, is_builtin_action,
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
        assert_eq!(lookup("double"), vec!["doubleClick"]);
        assert_eq!(lookup("right"), vec!["rightClick"]);
        assert_eq!(lookup("middle"), vec!["middleClick"]);
        assert_eq!(lookup("mouseover"), vec!["hover"]);
        assert_eq!(lookup("mouseenter"), vec!["hover"]);
        assert_eq!(lookup("scrollto"), vec!["scrollIntoView"]);
    }

    #[test]
    fn table_keys_are_folded_and_unique() {
        for (i, (a, _)) in ACTION_VERB_SYNONYMS.iter().enumerate() {
            // An unfolded key could never match: lookup compares folded forms.
            assert_eq!(fold_verb(a), *a, "key '{a}' is not in folded form");
            for (b, _) in &ACTION_VERB_SYNONYMS[i + 1..] {
                assert_ne!(a, b, "duplicate key '{a}'");
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
    fn separator_only_fix() {
        let accepted = list(TERMINAL_INPUT_ACTIONS);
        assert_eq!(
            suggest_action("send_keys", &accepted).as_deref(),
            Some("sendKeys")
        );
        assert_eq!(
            suggest_action("send-keys", &accepted).as_deref(),
            Some("sendKeys")
        );
        assert_eq!(
            suggest_action("scroll_into_view", &accepted).as_deref(),
            Some("scrollIntoView")
        );
        assert_eq!(
            suggest_action("double_click", &list(&["doubleClick"])).as_deref(),
            Some("doubleClick")
        );
        assert_eq!(
            suggest_action("right-click", &list(&["rightClick"])).as_deref(),
            Some("rightClick")
        );
    }

    #[test]
    fn table_lookup_ignores_separators() {
        let accepted = list(TERMINAL_INPUT_ACTIONS);
        for verb in ["key_press", "press_key", "key-down"] {
            assert_eq!(
                suggest_action(verb, &accepted).as_deref(),
                Some("sendKeys"),
                "{verb}"
            );
        }
        assert_eq!(
            suggest_action("type_text", &accepted).as_deref(),
            Some("type")
        );
        assert_eq!(
            suggest_action("context_menu", &list(&["rightClick"])).as_deref(),
            Some("rightClick")
        );
        assert_eq!(
            suggest_action("scroll_to", &accepted).as_deref(),
            Some("scrollIntoView")
        );
    }

    #[test]
    fn builtin_aliases_suggest_the_canonical_verb() {
        assert_eq!(
            suggest_action("double", &list(&["doubleClick"])).as_deref(),
            Some("doubleClick")
        );
        assert_eq!(
            suggest_action("right", &list(&["rightClick"])).as_deref(),
            Some("rightClick")
        );
        assert_eq!(
            suggest_action("middle", &list(&["middleClick"])).as_deref(),
            Some("middleClick")
        );
    }

    #[test]
    fn surrounding_whitespace_is_a_spelling_fix() {
        // ` click ` passes as a built-in and stops at `ACTION_NOT_SUPPORTED`,
        // whose list holds `click`, so naming it is the whole answer.
        assert_eq!(
            suggest_action(" click ", &list(&["click"])).as_deref(),
            Some("click")
        );
        assert_eq!(suggest_action("   ", &list(&["click"])), None);
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
            Some(json!({ "keys": ["Enter"] }))
        );
        assert_eq!(
            params_hint("sendKeys", Some(&json!({ "key": "Escape" }))),
            Some(json!({ "keys": ["Escape"] }))
        );
        assert_eq!(params_hint("type", None), Some(json!({ "text": "…" })));
        assert_eq!(params_hint("click", None), None);
    }

    fn keys_hint(params: Value) -> Option<Value> {
        params_hint("sendKeys", Some(&params))
    }

    #[test]
    fn send_keys_hint_carries_the_callers_key() {
        // The array forms `sendKeys` itself takes are kept as sent.
        assert_eq!(
            keys_hint(json!({ "keys": ["Escape", "Tab"] })),
            Some(json!({ "keys": ["Escape", "Tab"] }))
        );
        let descriptor = json!({ "keys": [{ "key": "c", "modifiers": { "ctrl": true } }] });
        assert_eq!(keys_hint(descriptor.clone()), Some(descriptor.clone()));
        assert_eq!(
            keys_hint(json!({ "keys": "Escape" })),
            Some(json!({ "keys": ["Escape"] }))
        );
        // Bare strings, so the SDK resolves the alias and parses the combo.
        assert_eq!(
            keys_hint(json!({ "key": "esc" })),
            Some(json!({ "keys": ["esc"] }))
        );
        // The SDK validates case-sensitively, so a named key is canonicalized.
        assert_eq!(
            keys_hint(json!({ "key": "enter" })),
            Some(json!({ "keys": ["Enter"] }))
        );
        assert_eq!(
            keys_hint(json!({ "keys": "ARROWDOWN" })),
            Some(json!({ "keys": ["ArrowDown"] }))
        );
        assert_eq!(
            keys_hint(json!({ "key": "ctrl+Enter" })),
            Some(json!({ "keys": ["ctrl+Enter"] }))
        );
        // Modifiers beside the key need the descriptor form.
        assert_eq!(
            keys_hint(json!({ "key": "c", "modifiers": { "ctrl": true } })),
            Some(descriptor)
        );
        assert_eq!(
            keys_hint(json!({ "key": "+", "modifiers": { "ctrl": true } })),
            Some(json!({ "keys": [{ "key": "+", "modifiers": { "ctrl": true } }] }))
        );
        // The space key and the plus key are keys.
        assert_eq!(
            keys_hint(json!({ "key": " " })),
            Some(json!({ "keys": [" "] }))
        );
        assert_eq!(
            keys_hint(json!({ "key": "+" })),
            Some(json!({ "keys": ["+"] }))
        );
    }

    #[test]
    fn an_empty_field_does_not_hide_the_one_that_names_the_key() {
        let escape = Some(json!({ "keys": ["Escape"] }));
        assert_eq!(keys_hint(json!({ "key": "Escape", "keys": [] })), escape);
        assert_eq!(keys_hint(json!({ "key": "", "keys": "Escape" })), escape);
        assert_eq!(
            keys_hint(json!({ "key": "Escape", "modifiers": {} })),
            escape
        );
    }

    #[test]
    fn send_keys_hint_names_enter_only_when_no_key_was_named() {
        let enter = Some(json!({ "keys": ["Enter"] }));
        for params in [
            json!({}),
            json!({ "key": "" }),
            json!({ "keys": [] }),
            json!({ "key": null }),
            json!({ "modifiers": {} }),
        ] {
            assert_eq!(keys_hint(params.clone()), enter, "{params}");
        }
    }

    /// Each of these names something a hint cannot restate. A hint would press
    /// a different key, or be the next 400, so there must be none.
    #[test]
    fn a_request_that_cannot_be_restated_gets_no_hint() {
        for params in [
            // A modifier that would be dropped.
            json!({ "key": "c", "modifiers": ["ctrl"] }),
            json!({ "key": "c", "modifiers": "ctrl" }),
            json!({ "key": "c", "modifiers": { "Ctrl": true } }),
            json!({ "key": "c", "modifiers": { "ctrl": "false" } }),
            json!({ "keys": ["c"], "modifiers": { "ctrl": true } }),
            json!({ "modifiers": { "ctrl": true } }),
            json!({ "keys": [{ "key": "c", "modifiers": "ctrl" }] }),
            // A descriptor would not resolve a longer name, or parse a combo.
            json!({ "key": "esc", "modifiers": { "shift": true } }),
            json!({ "key": "ctrl+Enter", "modifiers": { "shift": true } }),
            // A field this hint does not read may change which key is meant.
            json!({ "value": "Escape" }),
            json!({ "text": "hello" }),
            json!({ "key": "c", "ctrl": true }),
            json!({ "key": "Escape", "value": "Tab" }),
            // Text, not a key.
            json!({ "keys": "ls\r" }),
            json!({ "key": "two words" }),
            // Two different answers to which key.
            json!({ "key": "Escape", "keys": ["Tab"] }),
            json!({ "key": "Escape", "keys": "Tab" }),
            // Not a key.
            json!({ "key": "   " }),
            json!({ "key": 13 }),
            json!({ "keys": [13] }),
            json!({ "keys": [{}] }),
            json!({ "keys": [""] }),
            json!({ "keys": 13 }),
        ] {
            assert_eq!(keys_hint(params.clone()), None, "{params}");
        }
    }

    #[test]
    fn no_hint_still_names_the_verb() {
        let suggestion = ActionSuggestion::resolve(
            "press",
            &list(TERMINAL_INPUT_ACTIONS),
            Some(&json!({ "key": "c", "modifiers": "ctrl" })),
        )
        .expect("sendKeys is accepted");
        assert_eq!(suggestion.verb, "sendKeys");
        assert_eq!(suggestion.params_hint, None);
        assert_eq!(suggestion.message_suffix(), " Did you mean 'sendKeys'?");
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
    fn an_element_advertising_nothing_is_named_as_such() {
        let err = deferred_unknown_action_error("press", "el", false, false, Some(&[]), None)
            .expect("an explicit empty list is metadata, so it rejects");
        assert_eq!(
            err.message,
            "Unknown action 'press' for element 'el'. This element advertises no actions."
        );
        let ctx = err.context.expect("context");
        assert_eq!(ctx["supportedActions"], json!([]));
        assert!(ctx.get("didYouMean").is_none());
    }

    #[test]
    fn unknown_action_error_is_scoped_to_the_element() {
        let err = deferred_unknown_action_error(
            "slap",
            "el",
            false,
            false,
            Some(&list(&["focus"])),
            None,
        )
        .expect("slap must be rejected");
        assert!(matches!(err.code, UiBridgeErrorCode::InvalidRequest));
        assert_eq!(
            err.message,
            "Unknown action 'slap' for element 'el'. This element accepts: focus."
        );
        let ctx = err.context.expect("context");
        assert_eq!(ctx["action"], json!("slap"));
        assert_eq!(ctx["elementId"], json!("el"));
        assert_eq!(ctx["supportedActions"], json!(["focus"]));
    }

    /// A built-in alias passes the unknown-verb gate and stops at
    /// `ACTION_NOT_SUPPORTED`, because elements advertise the canonical name.
    #[test]
    fn action_not_supported_names_the_canonical_form_of_an_alias() {
        let advertised = list(&["focus", "scrollIntoView", "doubleClick"]);
        for (alias, canonical) in [
            ("scroll_into_view", "scrollIntoView"),
            ("double_click", "doubleClick"),
            ("double", "doubleClick"),
        ] {
            let err = action_not_supported_error(alias, "el", &advertised, None);
            let ctx = err.context.as_ref().expect("context");
            assert_eq!(ctx["didYouMean"], json!(canonical), "{alias}");
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
        let is_builtin = is_builtin_action("press");
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
        assert_eq!(ctx["paramsHint"], json!({ "keys": ["Enter"] }));
        assert_eq!(
            body["error"],
            json!(
                "Unknown action 'press' for element 'terminal.command-bar-input'. \
                 This element accepts: focus, blur, hover, scroll, scrollIntoView, click, \
                 hoverClick, type, clear, sendKeys. Did you mean 'sendKeys'? \
                 (params: {\"keys\":[\"Enter\"]})"
            )
        );
    }
}
