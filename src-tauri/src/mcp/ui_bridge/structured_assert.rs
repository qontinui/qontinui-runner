//! `POST /ui-bridge/control/assert`: declarative assertions over the runner's
//! own DOM — the request, the script builder, the verdict reader and the
//! handler. The route is registered in `capabilities.rs` `routes()`; this
//! family lives on its own (plan
//! 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route, D5).
//!
//! Two rules hold across the family:
//! - a request whose parameters do not name one assertion is REFUSED (400 or
//!   422), never defaulted into a different question (D4);
//! - an evaluation that yields no verdict is an ERROR status, never
//!   `passed: false` (D3), so a consumer can tell "the UI is wrong" from "the
//!   instrument is wrong".

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};

use super::helpers::direct_webview_evaluate_with_result;
use super::request::ui_bridge_request_sync;
use super::types::UiBridgeError;

/// How `query` finds elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QueryType {
    /// A CSS selector.
    #[default]
    Css,
    /// Elements whose trimmed text content equals `query`.
    Text,
    /// Elements whose text content contains `query`.
    TextContains,
}

/// What the assertion checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AssertionKind {
    Exists,
    NotExists,
    Count,
    IsVisible,
    HasText,
    HasClass,
    HasAttribute,
}

impl AssertionKind {
    /// The wire spelling (`rename_all = "camelCase"`), for messages.
    pub fn wire_name(self) -> &'static str {
        match self {
            AssertionKind::Exists => "exists",
            AssertionKind::NotExists => "notExists",
            AssertionKind::Count => "count",
            AssertionKind::IsVisible => "isVisible",
            AssertionKind::HasText => "hasText",
            AssertionKind::HasClass => "hasClass",
            AssertionKind::HasAttribute => "hasAttribute",
        }
    }
}

/// Request for a structured UI assertion.
///
/// Unknown keys and unknown `queryType` / `assertion` values are refused by
/// deserialization (422 naming them); a missing or wrong parameter for the
/// chosen assertion is refused by [`build_structured_assert_expression`] (400).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredAssertRequest {
    /// CSS selector or text to search for. Must be non-empty.
    pub query: String,
    /// How to search. Defaults to `css`.
    #[serde(default)]
    pub query_type: QueryType,
    /// What to assert.
    pub assertion: AssertionKind,
    /// `count`: a non-negative integer. `hasText` / `hasClass`: a non-empty
    /// string. `hasAttribute`: an optional string the attribute must equal.
    /// Refused on `exists`, `notExists` and `isVisible`.
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// `hasAttribute` only: the attribute name. Refused on every other arm.
    #[serde(default)]
    pub attribute: Option<String>,
}

/// Result of a structured assertion: served only when the page produced a
/// verdict.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertResult {
    pub passed: bool,
    pub assertion: AssertionKind,
    pub query: String,
    pub actual: serde_json::Value,
    pub expected: serde_json::Value,
    pub message: String,
}

/// A caller-supplied value as a complete JavaScript string literal, quotes
/// included, for splicing into the script the structured assert route
/// evaluates.
///
/// JSON encoding is the escape. It doubles every backslash, escapes the double
/// quote and escapes every C0 control character (CR and LF among them), so no
/// value can end the literal early or leave it unterminated. The two line
/// terminators JSON leaves raw, U+2028 and U+2029, are legal inside a
/// JavaScript string literal since ES2019, which made JSON string syntax a
/// subset of it. [`ui_bridge_expect_text_handler`](super::capabilities::ui_bridge_expect_text_handler)
/// encodes its search text the same way.
fn js_string_literal(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

/// A non-empty string `expected`, or the 400 message naming the arm.
fn required_text_expected(request: &StructuredAssertRequest) -> Result<&str, String> {
    let name = request.assertion.wire_name();
    let text = request
        .expected
        .as_ref()
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("assertion '{name}' requires a string `expected`"))?;
    if text.is_empty() {
        return Err(format!(
            "assertion '{name}' requires a non-empty `expected` (an empty one matches any element)"
        ));
    }
    Ok(text)
}

/// Build the expression `POST /ui-bridge/control/assert` evaluates in the page.
///
/// Pure, so unit tests pin the script without a page or an [`ApiState`]. Every
/// caller-supplied string (`query`, a string `expected`, `attribute`) enters
/// the script only through [`js_string_literal`]; `count`'s `expected` is
/// formatted from an unsigned integer. `Err` is the 400 message for a
/// parameter the chosen assertion does not take or cannot do without.
pub(crate) fn build_structured_assert_expression(
    request: &StructuredAssertRequest,
) -> Result<String, String> {
    let name = request.assertion.wire_name();
    if request.query.is_empty() {
        return Err("`query` must be non-empty".to_string());
    }
    if request.attribute.is_some() && request.assertion != AssertionKind::HasAttribute {
        return Err(format!(
            "assertion '{name}' takes no `attribute` (only 'hasAttribute' does)"
        ));
    }

    let query = js_string_literal(&request.query);
    let query_js = match request.query_type {
        QueryType::Css => format!("document.querySelectorAll({query})"),
        QueryType::Text => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.trim() === {query})"#
        ),
        QueryType::TextContains => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.includes({query}))"#
        ),
    };

    let assertion_js = match request.assertion {
        AssertionKind::Exists | AssertionKind::NotExists | AssertionKind::IsVisible
            if request.expected.is_some() =>
        {
            return Err(format!("assertion '{name}' takes no `expected`"));
        }
        AssertionKind::Exists => format!(
            r#"return JSON.stringify({{ found: ({query_js}).length, passed: ({query_js}).length > 0 }})"#
        ),
        AssertionKind::NotExists => format!(
            r#"return JSON.stringify({{ found: ({query_js}).length, passed: ({query_js}).length === 0 }})"#
        ),
        AssertionKind::Count => {
            let expected_value = request
                .expected
                .as_ref()
                .filter(|v| v.is_i64() || v.is_u64())
                .ok_or_else(|| "assertion 'count' requires an integer `expected`".to_string())?;
            let expected = expected_value.as_u64().ok_or_else(|| {
                "assertion 'count' requires a non-negative `expected` (a negative count can never match)"
                    .to_string()
            })?;
            format!(
                r#"return JSON.stringify({{ found: ({query_js}).length, passed: ({query_js}).length === {expected}, expected: {expected} }})"#
            )
        }
        AssertionKind::IsVisible => format!(
            r#"var els = {query_js}; var visible = Array.from(els).filter(function(el) {{ var r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).display !== 'none'; }}); return JSON.stringify({{ found: els.length, visible: visible.length, passed: visible.length > 0 }})"#
        ),
        AssertionKind::HasText => {
            let expected_text = js_string_literal(required_text_expected(request)?);
            format!(
                r#"var els = {query_js}; var texts = Array.from(els).map(el => el.textContent.trim()); var found = texts.some(t => t.includes({expected_text})); return JSON.stringify({{ found: els.length, texts: texts.slice(0, 3), passed: found }})"#
            )
        }
        AssertionKind::HasClass => {
            let expected_class = js_string_literal(required_text_expected(request)?);
            format!(
                r#"var els = {query_js}; var found = Array.from(els).some(el => el.classList.contains({expected_class})); var classes = Array.from(els).slice(0, 3).map(el => Array.from(el.classList).join(' ')); return JSON.stringify({{ found: els.length, classes: classes, passed: found }})"#
            )
        }
        AssertionKind::HasAttribute => {
            let attr = request
                .attribute
                .as_deref()
                .ok_or_else(|| "assertion 'hasAttribute' requires `attribute`".to_string())?;
            if attr.is_empty() {
                return Err("assertion 'hasAttribute' requires a non-empty `attribute`".to_string());
            }
            let check = match &request.expected {
                None => format!("el.hasAttribute({})", js_string_literal(attr)),
                Some(serde_json::Value::String(val)) => format!(
                    "el.getAttribute({}) === {}",
                    js_string_literal(attr),
                    js_string_literal(val)
                ),
                Some(_) => {
                    return Err(
                        "assertion 'hasAttribute' takes a string `expected` (or none, for a presence test)"
                            .to_string(),
                    );
                }
            };
            format!(
                r#"var els = {query_js}; var found = Array.from(els).some(el => {check}); return JSON.stringify({{ found: els.length, passed: found }})"#
            )
        }
    };

    // Every arm is a statement list ending in a top-level `return`: this
    // wrapper's value is what the page hands back. An arm that wraps itself in
    // its own IIFE computes the result and discards it, so the page returns
    // `undefined` (coord finding edfbb26a). The golden fixture and its vitest
    // execution oracle pin every arm's full script.
    Ok(format!("(() => {{ {assertion_js} }})()"))
}

/// The route's answer for a request [`build_structured_assert_expression`]
/// refused: 400, naming the parameter.
pub(crate) fn refuse_request(message: String) -> (StatusCode, Json<ApiResponse<()>>) {
    (StatusCode::BAD_REQUEST, Json(api_error(message)))
}

/// Which door evaluates a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvalDoor {
    /// The frontend's gated `page_evaluate` IPC handler.
    Ipc,
    /// The runner's direct webview eval, for a script the page gate refuses.
    Direct,
}

/// The door for `script`: direct when the page's evaluate gate (the runner's
/// Rust twin of `checkEvaluateBlocklist`) would refuse it, IPC otherwise.
///
/// The gate matches source text, string literals included, so a well-formed
/// `hasText` whose `expected` is "Connect WebSocket" trips it while carrying
/// no caller code. Sending such a script through the direct door is the
/// fallback every sibling route that builds a script from escaped caller
/// values already takes (`helpers.rs` `evaluate_js_expression`), and it is
/// safe under the same invariant: every caller value enters the script only
/// through [`js_string_literal`] (pinned by `structured_assert_script_tests`).
pub(crate) fn door_for(script: &str) -> EvalDoor {
    if super::page::evaluate_gate_match(script, false).is_some() {
        EvalDoor::Direct
    } else {
        EvalDoor::Ipc
    }
}

/// What one evaluation answered, by door.
#[derive(Debug)]
pub(crate) enum EvalOutcome {
    /// `ui_bridge_request_sync(.., "page_evaluate", ..)`'s answer, whole.
    Ipc(Result<serde_json::Value, String>),
    /// `direct_webview_evaluate_with_result`'s answer: the value as a string.
    Direct(Result<String, String>),
}

/// The verdict in a `page_evaluate` payload: `Ok((passed, parsed))` only when
/// `data.value` is a JSON string holding an object with a boolean `passed`.
/// Every other shape is `Err` naming what the page returned — no verdict,
/// never a fail.
pub(crate) fn read_assert_verdict(
    data: &serde_json::Value,
) -> Result<(bool, serde_json::Value), String> {
    let value = super::helpers::evaluate_ipc_value(data);
    let result_str = match value {
        Some(serde_json::Value::String(s)) => s.as_str(),
        Some(other) => {
            return Err(format!(
                "the page returned a non-string value ({}): {}",
                data.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("untyped"),
                other
            ));
        }
        None => {
            return Err(format!(
                "the page returned no value (type {})",
                data.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("absent")
            ));
        }
    };
    let parsed: serde_json::Value = serde_json::from_str(result_str)
        .map_err(|e| format!("unparseable result ({e}): {result_str}"))?;
    let passed = parsed
        .get("passed")
        .and_then(|p| p.as_bool())
        .ok_or_else(|| format!("result has no boolean `passed`: {result_str}"))?;
    Ok((passed, parsed))
}

/// The prefix `direct_webview_evaluate_with_result` puts on a script's own
/// throw, as opposed to its transport failures (timeout, no window, no port).
const DIRECT_EVAL_SCRIPT_ERROR_PREFIX: &str = "JS error: ";

/// The route's answer for one evaluation outcome. Pure: table-tested without a
/// page, and served by the diagnose client's stub so both see one mapping.
///
/// - IPC: the whole answer goes through `wrap_ipc_result`, so a transport
///   failure is 503 (frontend not ready) / 500, and an inner
///   `{success:false}` (a page-side throw or a gate refusal) is its typed 400.
/// - Direct: a script's own throw (`JS error: …`) is 400; any other failure is
///   classified like an IPC transport failure.
/// - A payload with no verdict ([`read_assert_verdict`]) is 500 naming it.
/// - Only a verdict is 200 with an [`AssertResult`].
pub(crate) fn structured_assert_response(
    request: &StructuredAssertRequest,
    outcome: EvalOutcome,
) -> Result<Json<ApiResponse<AssertResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let name = request.assertion.wire_name();
    let data = match outcome {
        EvalOutcome::Ipc(result) => {
            let Json(ok) = super::request::wrap_ipc_result(result)?;
            ok.data.unwrap_or(serde_json::Value::Null)
        }
        EvalOutcome::Direct(Ok(value)) => serde_json::json!({ "value": value }),
        EvalOutcome::Direct(Err(e)) if e.starts_with(DIRECT_EVAL_SCRIPT_ERROR_PREFIX) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error_detailed(
                    format!("Assertion '{name}' script threw: {e}"),
                    UiBridgeError::action_failed(e),
                )),
            ));
        }
        EvalOutcome::Direct(Err(e)) => {
            return match super::request::wrap_ipc_result(Err(e)) {
                Err(status_body) => Err(status_body),
                Ok(_) => unreachable!("wrap_ipc_result(Err) is always an error"),
            };
        }
    };

    let (passed, parsed) = read_assert_verdict(&data).map_err(|why| {
        let message = format!("Assertion '{name}' produced no verdict: {why}");
        warn!("UI Bridge API: structured assert: {message}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error_detailed(
                message.clone(),
                UiBridgeError::internal(message),
            )),
        )
    })?;

    let message = if passed {
        format!("Assertion '{name}' passed for '{}'", request.query)
    } else {
        format!(
            "Assertion '{name}' failed for '{}': {:?}",
            request.query, parsed
        )
    };
    Ok(Json(ApiResponse::success(AssertResult {
        passed,
        assertion: request.assertion,
        query: request.query.clone(),
        actual: parsed,
        expected: request.expected.clone().unwrap_or(serde_json::Value::Null),
        message,
    })))
}

/// POST /ui-bridge/control/assert
///
/// Evaluate a structured UI assertion without writing JavaScript. 200 carries
/// a verdict; a refused request is 400/422; an evaluation with no verdict is
/// a non-2xx with a typed code — never `passed: false`.
pub async fn ui_bridge_structured_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StructuredAssertRequest>,
) -> Result<Json<ApiResponse<AssertResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Structured assert ({} {:?} on '{}')",
        request.assertion.wire_name(),
        request.query_type,
        request.query
    );

    let full_expr = build_structured_assert_expression(&request).map_err(refuse_request)?;

    let outcome = match door_for(&full_expr) {
        EvalDoor::Ipc => {
            let payload = serde_json::json!({ "expression": full_expr });
            EvalOutcome::Ipc(ui_bridge_request_sync(&state, "page_evaluate", payload).await)
        }
        EvalDoor::Direct => EvalOutcome::Direct(
            direct_webview_evaluate_with_result(&state, &full_expr, None, false).await,
        ),
    };
    if let EvalOutcome::Ipc(Err(e)) | EvalOutcome::Direct(Err(e)) = &outcome {
        error!("UI Bridge API: structured assert evaluation failed: {}", e);
    }
    structured_assert_response(&request, outcome)
}

#[cfg(test)]
mod structured_assert_script_tests {
    //! `POST /ui-bridge/control/assert` splices caller values into a script
    //! that runs in the runner's own webview. These pin that every spliced
    //! value enters as a JS string literal it cannot end: the backslash
    //! doubled, the quote escaped, CR and LF escaped. Coord finding
    //! 5527320e: the route escaped the quote ONLY, so a backslash before a
    //! quote turned that quote into the end of the literal.

    use super::{build_structured_assert_expression, StructuredAssertRequest};
    use serde_json::json;

    /// One caller value, the literal it must become, and what that literal
    /// looks like when it ends early.
    struct Case {
        name: &'static str,
        /// The value as the caller sends it.
        value: &'static str,
        /// The JS string literal it must become, quotes included.
        escaped: &'static str,
        /// The literal ending early, ANCHORED at its opening quote. The bare
        /// tail of an early close (`\\'),fetch(`) also occurs inside a
        /// correctly escaped `\\\'`, so unanchored it cannot tell a broken
        /// escape from a sound one.
        early_close: &'static str,
    }

    const CASES: &[Case] = &[
        // The old escape turned `'` into `\'` and kept the backslash, so the
        // script read `'*\\'`: an escaped backslash, then a quote that ENDED
        // the literal, and the rest of the value ran as code.
        Case {
            name: "backslash before a single quote",
            value: r"*\'),fetch('/x'),document.querySelectorAll('*",
            escaped: r#""*\\'),fetch('/x'),document.querySelectorAll('*""#,
            early_close: r"'*\\'),fetch(",
        },
        // The same attack aimed at a double-quoted literal.
        Case {
            name: "backslash before a double quote",
            value: r#"*\"),fetch("/x"),document.querySelectorAll("*"#,
            escaped: r#""*\\\"),fetch(\"/x\"),document.querySelectorAll(\"*""#,
            early_close: r#""*\\"),fetch("#,
        },
        Case {
            name: "bare quotes",
            value: r#"say "hi" it's"#,
            escaped: r#""say \"hi\" it's""#,
            early_close: r#""say "hi"#,
        },
        // Unescaped, a trailing backslash escapes the closing quote, and the
        // literal runs on into the rest of the script.
        Case {
            name: "trailing backslash",
            value: r"div\",
            escaped: r#""div\\""#,
            early_close: r#""div\""#,
        },
        // A raw line break inside a JS string literal is a SyntaxError.
        Case {
            name: "line terminators",
            value: "a\r\nb",
            escaped: r#""a\r\nb""#,
            early_close: "\"a\r",
        },
    ];

    /// The script for a request body as a caller sends it: camelCase, through
    /// serde, so the test reads the field names the route actually receives.
    fn script(body: serde_json::Value) -> String {
        let request: StructuredAssertRequest =
            serde_json::from_value(body).expect("the request body deserializes");
        build_structured_assert_expression(&request).expect("a known query type and assertion")
    }

    /// Every case at ONE splice point. `before` and `after` are the script
    /// text on either side of the literal, so the escaped value is found where
    /// the route splices it and nowhere else counts. Names every failing case
    /// rather than stopping at the first.
    fn assert_each_case_spliced_escaped(
        script_for: impl Fn(&str) -> String,
        before: &str,
        after: &str,
    ) {
        let failures: Vec<String> = CASES
            .iter()
            .filter_map(|case| {
                let script = script_for(case.value);
                let escaped = case.escaped;
                let at_splice = format!("{before}{escaped}{after}");
                if !script.contains(&at_splice) {
                    Some(format!("{}: `{at_splice}` is not in `{script}`", case.name))
                } else if script.contains(case.early_close) {
                    Some(format!(
                        "{}: the literal ends early ({:?}) in `{script}`",
                        case.name, case.early_close
                    ))
                } else {
                    None
                }
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} of {} cases are not spliced as an escaped literal:\n{}",
            failures.len(),
            CASES.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn a_css_query_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": v, "queryType": "css", "assertion": "exists"})),
            "document.querySelectorAll(",
            ")",
        );
    }

    #[test]
    fn a_text_query_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": v, "queryType": "text", "assertion": "exists"})),
            "el.textContent.trim() === ",
            ")",
        );
    }

    #[test]
    fn a_text_contains_query_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": v, "queryType": "textContains", "assertion": "exists"})),
            "el.textContent.includes(",
            "))",
        );
    }

    #[test]
    fn a_has_text_expected_value_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": "p", "assertion": "hasText", "expected": v})),
            "t.includes(",
            ")",
        );
    }

    #[test]
    fn a_has_class_expected_value_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": "p", "assertion": "hasClass", "expected": v})),
            "el.classList.contains(",
            ")",
        );
    }

    /// `attribute` alone: the presence check.
    #[test]
    fn a_has_attribute_name_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| script(json!({"query": "p", "assertion": "hasAttribute", "attribute": v})),
            "el.hasAttribute(",
            ")",
        );
    }

    /// `attribute` beside an `expected` value: two literals in one check.
    /// Under the quote-only escape, a trailing backslash on the attribute
    /// escaped its closing quote, the expected value's opening quote ended
    /// the attribute literal instead, and the expected value ran as code.
    #[test]
    fn a_has_attribute_name_compared_to_a_value_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| {
                script(json!({
                    "query": "p",
                    "assertion": "hasAttribute",
                    "attribute": v,
                    "expected": "Tip",
                }))
            },
            "el.getAttribute(",
            r#") === "Tip""#,
        );
    }

    #[test]
    fn a_has_attribute_expected_value_is_spliced_as_an_escaped_literal() {
        assert_each_case_spliced_escaped(
            |v| {
                script(json!({
                    "query": "p",
                    "assertion": "hasAttribute",
                    "attribute": "title",
                    "expected": v,
                }))
            },
            r#"el.getAttribute("title") === "#,
            ");",
        );
    }
}

#[cfg(test)]
mod structured_assert_result_tests {
    //! What the route answers. The golden fixture pins each arm's full script
    //! (and its vitest oracle executes each one through the page's real
    //! evaluate gate and compiler); the tables below pin the refusals, the
    //! verdict reader, the status mapping and the door choice. Every expected
    //! value is a literal written here, never derived from the code under
    //! test.

    use super::{
        build_structured_assert_expression, door_for, read_assert_verdict,
        structured_assert_response, AssertionKind, EvalDoor, EvalOutcome, QueryType,
        StructuredAssertRequest,
    };
    use axum::http::StatusCode;
    use serde_json::json;

    /// The fixture both readers share (D2). Written by hand; never regenerate
    /// it from the builder.
    const GOLDEN: &str =
        include_str!("../../../../src/lib/__golden__/structured-assert-scripts.json");

    fn golden_cases() -> Vec<serde_json::Value> {
        let golden: serde_json::Value =
            serde_json::from_str(GOLDEN).expect("the golden fixture is JSON");
        golden["cases"]
            .as_array()
            .expect("the golden fixture has a `cases` array")
            .clone()
    }

    fn request(body: serde_json::Value) -> StructuredAssertRequest {
        serde_json::from_value(body).expect("the request body deserializes")
    }

    fn golden_request(name: &str) -> StructuredAssertRequest {
        let case = golden_cases()
            .into_iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no golden case `{name}`"));
        request(case["request"].clone())
    }

    fn golden_script(name: &str) -> String {
        golden_cases()
            .into_iter()
            .find(|c| c["name"] == name)
            .and_then(|c| c["script"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no golden script `{name}`"))
    }

    /// (a) The builder returns exactly the hand-written script for every case.
    #[test]
    fn every_case_builds_exactly_its_golden_script() {
        let cases = golden_cases();
        assert_eq!(cases.len(), 11, "the golden fixture pins eleven cases");
        let mut failures = Vec::new();
        for case in &cases {
            let name = case["name"].as_str().expect("a case name");
            let want = case["script"].as_str().expect("a case script");
            let req: StructuredAssertRequest = serde_json::from_value(case["request"].clone())
                .unwrap_or_else(|e| panic!("{name}: request does not deserialize: {e}"));
            match build_structured_assert_expression(&req) {
                Ok(got) if got == want => {}
                Ok(got) => failures.push(format!("{name}:\n  want {want}\n  got  {got}")),
                Err(e) => failures.push(format!("{name}: refused: {e}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// (b) An eighth assertion or a fourth query type cannot compile without
    /// naming its golden case here (no `_` arm), and every named case exists.
    #[test]
    fn the_golden_covers_every_arm_and_query_type() {
        fn case_for_assertion(kind: AssertionKind) -> &'static [&'static str] {
            match kind {
                AssertionKind::Exists => &["exists-css"],
                AssertionKind::NotExists => &["notExists-css"],
                AssertionKind::Count => &["count-css"],
                AssertionKind::IsVisible => &["isVisible-css"],
                AssertionKind::HasText => &["hasText-css", "hasText-gate-word"],
                AssertionKind::HasClass => &["hasClass-css"],
                // Both sub-arms: presence and equality.
                AssertionKind::HasAttribute => {
                    &["hasAttribute-present-css", "hasAttribute-equals-css"]
                }
            }
        }
        fn case_for_query_type(kind: QueryType) -> &'static str {
            match kind {
                QueryType::Css => "exists-css",
                QueryType::Text => "exists-text",
                QueryType::TextContains => "exists-textContains",
            }
        }
        let names: Vec<String> = golden_cases()
            .iter()
            .map(|c| c["name"].as_str().unwrap_or_default().to_string())
            .collect();
        let all_assertions = [
            AssertionKind::Exists,
            AssertionKind::NotExists,
            AssertionKind::Count,
            AssertionKind::IsVisible,
            AssertionKind::HasText,
            AssertionKind::HasClass,
            AssertionKind::HasAttribute,
        ];
        let all_query_types = [QueryType::Css, QueryType::Text, QueryType::TextContains];
        let mut missing = Vec::new();
        for kind in all_assertions {
            for name in case_for_assertion(kind) {
                if !names.iter().any(|n| n == name) {
                    missing.push(format!("{kind:?}: {name}"));
                }
                let req = golden_request(name);
                assert_eq!(req.assertion, kind, "{name} is not a {kind:?} case");
            }
        }
        for kind in all_query_types {
            let name = case_for_query_type(kind);
            if !names.iter().any(|n| n == name) {
                missing.push(format!("{kind:?}: {name}"));
            }
            assert_eq!(golden_request(name).query_type, kind, "{name}");
        }
        assert!(missing.is_empty(), "missing golden cases: {missing:?}");
    }

    /// (c) Every parameter the chosen assertion cannot do without, or does
    /// not take, is refused with a message naming it — never defaulted.
    #[test]
    fn a_missing_required_field_is_refused_not_defaulted() {
        let rows = [
            (
                "hasText no expected",
                json!({ "query": ".x", "assertion": "hasText" }),
                "assertion 'hasText' requires a string `expected`",
            ),
            (
                "hasClass no expected",
                json!({ "query": ".x", "assertion": "hasClass" }),
                "assertion 'hasClass' requires a string `expected`",
            ),
            (
                "count non-integer",
                json!({ "query": ".x", "assertion": "count", "expected": "2" }),
                "assertion 'count' requires an integer `expected`",
            ),
            (
                "count fractional",
                json!({ "query": ".x", "assertion": "count", "expected": 2.5 }),
                "assertion 'count' requires an integer `expected`",
            ),
            (
                "hasAttribute no attribute",
                json!({ "query": ".x", "assertion": "hasAttribute" }),
                "assertion 'hasAttribute' requires `attribute`",
            ),
            (
                "empty query",
                json!({ "query": "", "assertion": "exists" }),
                "`query` must be non-empty",
            ),
            (
                "negative count",
                json!({ "query": ".x", "assertion": "count", "expected": -1 }),
                "assertion 'count' requires a non-negative `expected` (a negative count can never match)",
            ),
            (
                "hasText empty expected",
                json!({ "query": ".x", "assertion": "hasText", "expected": "" }),
                "assertion 'hasText' requires a non-empty `expected` (an empty one matches any element)",
            ),
            (
                "hasClass empty expected",
                json!({ "query": ".x", "assertion": "hasClass", "expected": "" }),
                "assertion 'hasClass' requires a non-empty `expected` (an empty one matches any element)",
            ),
            (
                "hasAttribute non-string expected",
                json!({ "query": ".x", "assertion": "hasAttribute", "attribute": "title", "expected": 5 }),
                "assertion 'hasAttribute' takes a string `expected` (or none, for a presence test)",
            ),
            (
                "hasAttribute empty attribute",
                json!({ "query": ".x", "assertion": "hasAttribute", "attribute": "" }),
                "assertion 'hasAttribute' requires a non-empty `attribute`",
            ),
            (
                "exists given expected",
                json!({ "query": ".x", "assertion": "exists", "expected": 1 }),
                "assertion 'exists' takes no `expected`",
            ),
            (
                "notExists given expected",
                json!({ "query": ".x", "assertion": "notExists", "expected": true }),
                "assertion 'notExists' takes no `expected`",
            ),
            (
                "isVisible given expected",
                json!({ "query": ".x", "assertion": "isVisible", "expected": "yes" }),
                "assertion 'isVisible' takes no `expected`",
            ),
            (
                "exists given attribute",
                json!({ "query": ".x", "assertion": "exists", "attribute": "title" }),
                "assertion 'exists' takes no `attribute` (only 'hasAttribute' does)",
            ),
            (
                "hasText given attribute",
                json!({ "query": ".x", "assertion": "hasText", "expected": "a", "attribute": "title" }),
                "assertion 'hasText' takes no `attribute` (only 'hasAttribute' does)",
            ),
            (
                "count given attribute",
                json!({ "query": ".x", "assertion": "count", "expected": 1, "attribute": "title" }),
                "assertion 'count' takes no `attribute` (only 'hasAttribute' does)",
            ),
        ];
        let mut failures = Vec::new();
        for (name, body, want) in rows {
            match build_structured_assert_expression(&request(body)) {
                Err(got) if got == want => {}
                Err(got) => failures.push(format!("{name}: want `{want}`, got `{got}`")),
                Ok(script) => failures.push(format!("{name}: accepted as `{script}`")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_misspelled_key_is_refused_by_deserialization() {
        let rows = [
            (
                "queryTyp",
                json!({ "query": ".x", "assertion": "exists", "queryTyp": "text" }),
                "queryTyp",
            ),
            (
                "expectedText",
                json!({ "query": ".x", "assertion": "hasText", "expectedText": "a" }),
                "expectedText",
            ),
            (
                "query_type",
                json!({ "query": ".x", "assertion": "exists", "query_type": "text" }),
                "query_type",
            ),
            (
                "unknown queryType",
                json!({ "query": ".x", "assertion": "exists", "queryType": "xpath" }),
                "xpath",
            ),
            (
                "unknown assertion",
                json!({ "query": ".x", "assertion": "visible" }),
                "visible",
            ),
        ];
        let mut failures = Vec::new();
        for (name, body, named) in rows {
            match serde_json::from_value::<StructuredAssertRequest>(body) {
                Ok(r) => failures.push(format!("{name}: accepted as {r:?}")),
                Err(e) if e.to_string().contains(named) => {}
                Err(e) => failures.push(format!("{name}: refusal does not name `{named}`: {e}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// (d) Only a JSON string holding a boolean `passed` is a verdict.
    #[test]
    fn read_assert_verdict_reads_only_a_verdict() {
        assert_eq!(
            read_assert_verdict(&json!({ "value": "{\"passed\":true}", "type": "string" }))
                .map(|(p, _)| p),
            Ok(true)
        );
        assert_eq!(
            read_assert_verdict(&json!({ "value": "{\"found\":0,\"passed\":false}" }))
                .map(|(p, _)| p),
            Ok(false)
        );
        let undefined = read_assert_verdict(&json!({ "type": "undefined" }))
            .expect_err("undefined is no verdict");
        assert!(undefined.contains("undefined"), "{undefined}");
        for (name, data) in [
            ("scalar", json!({ "value": 5, "type": "scalar" })),
            ("not json", json!({ "value": "not json" })),
            ("no passed", json!({ "value": "{\"found\":1}" })),
            ("array", json!({ "value": "[1]" })),
            ("string passed", json!({ "value": "{\"passed\":\"true\"}" })),
        ] {
            assert!(
                read_assert_verdict(&data).is_err(),
                "{name} must be no verdict"
            );
        }
    }

    fn status_of(
        req: &StructuredAssertRequest,
        outcome: EvalOutcome,
    ) -> (StatusCode, serde_json::Value) {
        match structured_assert_response(req, outcome) {
            Ok(axum::Json(body)) => (StatusCode::OK, serde_json::to_value(body).unwrap()),
            Err((status, axum::Json(body))) => (status, serde_json::to_value(body).unwrap()),
        }
    }

    /// (e) Every evaluation outcome maps to its own status; only a verdict is
    /// a 200, and no failure is `passed: false`.
    #[test]
    fn every_evaluation_outcome_maps_to_its_status() {
        let req = golden_request("hasText-css");
        let rows: Vec<(&str, EvalOutcome, StatusCode)> = vec![
            (
                "frontend not ready",
                EvalOutcome::Ipc(Err(
                    r#"{"error":"frontend_not_ready","diagnostics":{}}"#.to_string()
                )),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "other transport error",
                EvalOutcome::Ipc(Err("connection reset by peer".to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "inner failure (page throw / gate refusal)",
                EvalOutcome::Ipc(Ok(json!({
                    "success": false,
                    "error": "Expression rejected: contains prohibited pattern (\\bWebSocket\\b)",
                }))),
                StatusCode::BAD_REQUEST,
            ),
            (
                "direct eval script throw",
                EvalOutcome::Direct(Err("JS error: x is not defined".to_string())),
                StatusCode::BAD_REQUEST,
            ),
            (
                "direct eval transport failure",
                EvalOutcome::Direct(Err("WebView window 'main' not found".to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "undefined",
                EvalOutcome::Ipc(Ok(json!({ "type": "undefined" }))),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "no boolean passed",
                EvalOutcome::Ipc(Ok(json!({ "value": "{\"found\":1}", "type": "string" }))),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "direct eval undefined",
                EvalOutcome::Direct(Ok("null".to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                "passed true",
                EvalOutcome::Ipc(Ok(
                    json!({ "value": "{\"found\":1,\"passed\":true}", "type": "string" }),
                )),
                StatusCode::OK,
            ),
            (
                "passed false",
                EvalOutcome::Ipc(Ok(
                    json!({ "value": "{\"found\":0,\"passed\":false}", "type": "string" }),
                )),
                StatusCode::OK,
            ),
            (
                "direct passed true",
                EvalOutcome::Direct(Ok("{\"found\":1,\"passed\":true}".to_string())),
                StatusCode::OK,
            ),
        ];
        let mut failures = Vec::new();
        for (name, outcome, want) in rows {
            let (got, body) = status_of(&req, outcome);
            if got != want {
                failures.push(format!("{name}: want {want}, got {got}: {body}"));
                continue;
            }
            if got == StatusCode::OK {
                if !body["data"]["passed"].is_boolean() {
                    failures.push(format!("{name}: 200 without a boolean verdict: {body}"));
                }
            } else {
                if body.get("data").is_some_and(|d| d.get("passed").is_some()) {
                    failures.push(format!("{name}: a failure carries `passed`: {body}"));
                }
                if !body["error_detail"]["code"].is_string()
                    && !body["errorDetail"]["code"].is_string()
                    && !body["code"].is_string()
                {
                    failures.push(format!("{name}: no typed code: {body}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// (f) A script the page gate refuses goes to the direct door; every other
    /// script goes through IPC.
    #[test]
    fn a_gate_refused_script_takes_the_direct_door() {
        assert_eq!(
            door_for(&golden_script("hasText-gate-word")),
            EvalDoor::Direct
        );
        for case in golden_cases() {
            let name = case["name"].as_str().unwrap_or_default();
            if name == "hasText-gate-word" {
                continue;
            }
            assert_eq!(
                door_for(case["script"].as_str().unwrap_or_default()),
                EvalDoor::Ipc,
                "{name}"
            );
        }
    }
}
