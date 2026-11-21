//! `POST /ui-bridge/control/assert`: declarative assertions over the runner's
//! own DOM — the request, the script builder, and the handler. The route is
//! registered in `capabilities.rs` `routes()`; this family lives on its own
//! (plan 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route, D5).

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::request::ui_bridge_request_sync;

// ============================================================================
// Structured Assert (declarative assertions over DOM)
// ============================================================================

/// Request for a structured UI assertion.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredAssertRequest {
    /// CSS selector or text to search for
    pub query: String,
    /// How to search: "css" (CSS selector), "text" (text content match), "textContains" (substring)
    #[serde(default = "default_query_type")]
    pub query_type: String,
    /// What to assert: "exists", "notExists", "count", "hasText", "hasClass", "isVisible", "hasAttribute"
    pub assertion: String,
    /// Expected value (for count, hasText, hasClass, hasAttribute assertions)
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Attribute name (for hasAttribute assertion)
    #[serde(default)]
    pub attribute: Option<String>,
}

fn default_query_type() -> String {
    "css".to_string()
}

/// Result of a structured assertion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertResult {
    pub passed: bool,
    pub assertion: String,
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
/// subset of it. [`ui_bridge_expect_text_handler`](super::capabilities::ui_bridge_expect_text_handler) encodes its search text the
/// same way.
fn js_string_literal(value: &str) -> String {
    serde_json::Value::from(value).to_string()
}

/// Build the expression `POST /ui-bridge/control/assert` evaluates in the page.
///
/// Pure, so unit tests pin the script without a page or an [`ApiState`]. Every
/// caller-supplied string (`query`, a string `expected`, `attribute`) enters
/// the script only through [`js_string_literal`]; `count`'s `expected` is
/// formatted from an integer. `Err` is the 400 message for an unknown
/// `query_type` or `assertion`.
fn build_structured_assert_expression(request: &StructuredAssertRequest) -> Result<String, String> {
    // Build the JS expression for this assertion
    let query_js = match request.query_type.as_str() {
        "css" => format!(
            "document.querySelectorAll({})",
            js_string_literal(&request.query)
        ),
        "text" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.trim() === {})"#,
            js_string_literal(&request.query)
        ),
        "textContains" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.includes({}))"#,
            js_string_literal(&request.query)
        ),
        _ => {
            return Err(format!(
                "Invalid query_type '{}'. Use: css, text, textContains",
                request.query_type
            ));
        }
    };

    let assertion_js = match request.assertion.as_str() {
        "exists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length > 0 }})"#,
            query_js, query_js
        ),
        "notExists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === 0 }})"#,
            query_js, query_js
        ),
        "count" => {
            let expected = request
                .expected
                .as_ref()
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "assertion 'count' requires an integer `expected`".to_string())?;
            format!(
                r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === {}, expected: {} }})"#,
                query_js, query_js, expected, expected
            )
        }
        "isVisible" => {
            format!(
                r#"var els = {}; var visible = Array.from(els).filter(function(el) {{ var r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).display !== 'none'; }}); return JSON.stringify({{ found: els.length, visible: visible.length, passed: visible.length > 0 }})"#,
                query_js
            )
        }
        "hasText" => {
            let expected_text = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "assertion 'hasText' requires a string `expected`".to_string())?;
            format!(
                r#"var els = {}; var texts = Array.from(els).map(el => el.textContent.trim()); var found = texts.some(t => t.includes({})); return JSON.stringify({{ found: els.length, texts: texts.slice(0, 3), passed: found }})"#,
                query_js,
                js_string_literal(expected_text)
            )
        }
        "hasClass" => {
            let expected_class = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "assertion 'hasClass' requires a string `expected`".to_string())?;
            format!(
                r#"var els = {}; var found = Array.from(els).some(el => el.classList.contains({})); var classes = Array.from(els).slice(0, 3).map(el => Array.from(el.classList).join(' ')); return JSON.stringify({{ found: els.length, classes: classes, passed: found }})"#,
                query_js,
                js_string_literal(expected_class)
            )
        }
        "hasAttribute" => {
            let attr = request
                .attribute
                .as_deref()
                .ok_or_else(|| "assertion 'hasAttribute' requires `attribute`".to_string())?;
            let expected_val = request.expected.as_ref().and_then(|v| v.as_str());
            let check = if let Some(val) = expected_val {
                format!(
                    "el.getAttribute({}) === {}",
                    js_string_literal(attr),
                    js_string_literal(val)
                )
            } else {
                format!("el.hasAttribute({})", js_string_literal(attr))
            };
            format!(
                r#"var els = {}; var found = Array.from(els).some(el => {}); return JSON.stringify({{ found: els.length, passed: found }})"#,
                query_js, check
            )
        }
        _ => {
            return Err(format!(
                "Invalid assertion '{}'. Use: exists, notExists, count, isVisible, hasText, hasClass, hasAttribute",
                request.assertion
            ));
        }
    };

    // Every arm is a statement list ending in a top-level `return`: this
    // wrapper's value is what the page hands back. An arm that wraps itself in
    // its own IIFE computes the result and discards it, so the page returns
    // `undefined` and the route reports `passed: false` for an element that
    // matches (coord finding edfbb26a: hasText, hasClass and hasAttribute).
    Ok(format!("(() => {{ {assertion_js} }})()"))
}

/// POST /ui-bridge/control/assert
///
/// Evaluate a structured UI assertion without writing JavaScript.
/// Supports CSS selectors and text search with common assertion types.
/// Returns a structured pass/fail result.
pub async fn ui_bridge_structured_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StructuredAssertRequest>,
) -> Result<Json<ApiResponse<AssertResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Structured assert ({} {} on '{}')",
        request.assertion, request.query_type, request.query
    );

    let full_expr = build_structured_assert_expression(&request)
        .map_err(|message| (StatusCode::BAD_REQUEST, Json(api_error(message))))?;

    // Use the safe evaluate path
    let payload = serde_json::json!({ "expression": full_expr });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            // No verdict is an ERROR, never `passed: false`: a script that
            // produced no JSON string with a boolean `passed` says nothing
            // about the UI under test.
            let no_verdict = |why: &str| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "Assertion '{}' produced no verdict: {}",
                        request.assertion, why
                    ))),
                )
            };
            let result_str = super::helpers::evaluate_ipc_value(&data)
                .and_then(|v| v.as_str())
                .ok_or_else(|| no_verdict("the page returned no string value"))?;
            let parsed: serde_json::Value = serde_json::from_str(result_str)
                .map_err(|e| no_verdict(&format!("unparseable result ({})", e)))?;
            let passed = parsed
                .get("passed")
                .and_then(|p| p.as_bool())
                .ok_or_else(|| no_verdict("result has no boolean `passed`"))?;

            let result = AssertResult {
                passed,
                assertion: request.assertion.clone(),
                query: request.query.clone(),
                actual: parsed.clone(),
                expected: request.expected.clone().unwrap_or(serde_json::Value::Null),
                message: if passed {
                    format!(
                        "Assertion '{}' passed for '{}'",
                        request.assertion, request.query
                    )
                } else {
                    format!(
                        "Assertion '{}' failed for '{}': {:?}",
                        request.assertion, request.query, parsed
                    )
                },
            };

            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            error!("UI Bridge API: structured assert evaluation failed: {}", e);
            match super::request::wrap_ipc_result(Err(e)) {
                Err(status_body) => Err(status_body),
                Ok(_) => unreachable!("wrap_ipc_result(Err) is always an error"),
            }
        }
    }
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
    //! The assert route returns the value of its outer
    //! `(() => { … })()` wrapper. Each arm must therefore END in a top-level
    //! `return`; an arm that is itself an IIFE has its result discarded, the
    //! page returns `undefined`, and the route reports `passed: false` even
    //! when the element matches. Coord finding edfbb26a: hasText, hasClass and
    //! hasAttribute were each such an IIFE, so they could never pass.

    use super::{build_structured_assert_expression, StructuredAssertRequest};
    use serde_json::json;

    const WRAPPER_OPEN: &str = "(() => { ";
    const WRAPPER_CLOSE: &str = " })()";

    /// Every assertion the route accepts, as a caller would send it.
    fn every_assertion() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("exists", json!({ "query": ".x", "assertion": "exists" })),
            (
                "notExists",
                json!({ "query": ".x", "assertion": "notExists" }),
            ),
            (
                "count",
                json!({ "query": ".x", "assertion": "count", "expected": 2 }),
            ),
            (
                "isVisible",
                json!({ "query": ".x", "assertion": "isVisible" }),
            ),
            (
                "hasText",
                json!({ "query": ".x", "assertion": "hasText", "expected": "Save" }),
            ),
            (
                "hasClass",
                json!({ "query": ".x", "assertion": "hasClass", "expected": "active" }),
            ),
            (
                "hasAttribute",
                json!({ "query": ".x", "assertion": "hasAttribute", "attribute": "title" }),
            ),
            (
                "hasAttribute with a value",
                json!({ "query": ".x", "assertion": "hasAttribute", "attribute": "title", "expected": "Tip" }),
            ),
        ]
    }

    #[test]
    fn every_assertion_arm_returns_its_result_from_the_outer_wrapper() {
        let mut failures = Vec::new();
        for (name, body) in every_assertion() {
            let request: StructuredAssertRequest =
                serde_json::from_value(body).expect("the request body deserializes");
            let script = build_structured_assert_expression(&request)
                .expect("a known query type and assertion");
            let Some(arm) = script
                .strip_prefix(WRAPPER_OPEN)
                .and_then(|rest| rest.strip_suffix(WRAPPER_CLOSE))
            else {
                failures.push(format!(
                    "{name}: not wrapped in `(() => {{ … }})()`: {script}"
                ));
                continue;
            };
            // An arm that opens with `(` is an expression statement, typically
            // its own IIFE, whose value the wrapper throws away.
            if arm.trim_start().starts_with('(') {
                failures.push(format!(
                    "{name}: the arm is an expression whose value is discarded: {arm}"
                ));
            }
            // The arm's last statement is the `return` the wrapper hands back.
            let last_statement = arm
                .trim_end()
                .trim_end_matches(';')
                .rsplit("; ")
                .next()
                .unwrap_or("");
            if !last_statement.starts_with("return JSON.stringify(") {
                failures.push(format!(
                    "{name}: the arm does not end in `return JSON.stringify(…)`: {arm}"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_missing_required_field_is_refused_not_defaulted() {
        for (name, body) in [
            (
                "hasText no expected",
                json!({ "query": ".x", "assertion": "hasText" }),
            ),
            (
                "hasClass no expected",
                json!({ "query": ".x", "assertion": "hasClass" }),
            ),
            (
                "count non-integer",
                json!({ "query": ".x", "assertion": "count", "expected": "2" }),
            ),
            (
                "hasAttribute no attribute",
                json!({ "query": ".x", "assertion": "hasAttribute" }),
            ),
        ] {
            let request: StructuredAssertRequest =
                serde_json::from_value(body).expect("deserializes");
            assert!(
                build_structured_assert_expression(&request).is_err(),
                "{name} must be refused"
            );
        }
    }

    #[test]
    fn a_misspelled_key_is_refused_by_deserialization() {
        let r = serde_json::from_value::<StructuredAssertRequest>(
            json!({ "query": ".x", "assertion": "exists", "queryTyp": "text" }),
        );
        assert!(r.is_err());
    }
}
