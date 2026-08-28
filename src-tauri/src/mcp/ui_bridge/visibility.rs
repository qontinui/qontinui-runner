//! `POST /ui-bridge/control/visibility` — the occlusion sweep.
//!
//! **What is covering what, page-wide and DIRECTED.** `page-health` and
//! `discover` both report per-element visibility, but neither answers the
//! question an operator or an autonomous tester actually asks after a layout
//! regression: "is any floating widget hiding something?". Answering it needs
//! the `occluder -> occluded` relation, not a per-element boolean.
//!
//! ## Contract
//!
//! This is a **runner-direct** port of the SDK handler
//! (`ui-bridge/packages/ui-bridge/src/server/handlers.ts:5279`,
//! `visibility`), declared in the SDK route table at
//! `ui-bridge/packages/ui-bridge/src/server/types.ts:1782`
//! (`{ method: 'POST', path: '/control/visibility', handler: 'visibility' }`).
//! Request knobs (`minRatio`, `includeExpected`) and the `VisibilityReport` /
//! `VisibilityOcclusionEntry` response shape (`types.ts:2413`/`2437`) are
//! matched field-for-field — a runner route answering with a *different*
//! shape than the SDK would be worse than the 404 it replaces, because
//! cross-surface tooling (the visual-audit / page-health skills) reads both
//! transports with one parser.
//!
//! Before this module the route was runner-side missing entirely, so the
//! Phase-2a drift test `mod.rs::sdk_manifest_routes_are_exposed_by_runner`
//! saw `POST /control/visibility` as SDK-declared-but-unexposed.
//!
//! ## Where the occlusion data comes from
//!
//! The SDK handler reads `state.occludedBy` / `state.occludedPct`, computed
//! by the registry's own `elementFromPoint` hit-test in
//! `core/registry.ts::computeVisibilityVerdict`. The hit-test is the stronger
//! of the two available signals — it observes what the compositor actually
//! painted, so it sees `clip-path`, transformed ancestors and scroll clipping
//! that a bounding-box model cannot derive. (The geometric z-order model
//! lives in `ui-bridge-auto`, which DEPENDS ON `ui-bridge`; importing it on
//! either side would make the two mutually dependent.)
//!
//! So the runner evaluates **in the webview**, against the SDK's own live
//! registry (`window.__UI_BRIDGE__.registry.getAllElements()`), and prefers
//! `getState().occludedBy` verbatim wherever the bundled SDK provides it —
//! zero drift once that lands in a published build.
//!
//! It cannot only do that, though. The blame fields are **not in the
//! `@qontinui/ui-bridge` build the runner currently bundles** (0.24.0's
//! `dist/` has `elementFromPoint` but no `occludedBy`/`occludedPct`; verified
//! against the installed package and against a live `/control/discover` on
//! this box). Sourcing exclusively from them would make every runner call
//! answer `verdict: "clear"` — a *false* "nothing is covered", which is the
//! failure mode this route exists to close. The injected routine therefore
//! carries a fallback arm that runs the same probe in the page: the same nine
//! sample points, the same 48px perimeter threshold, the same guard chain and
//! the same occluder-naming precedence as `computeVisibilityVerdict`
//! (`registry.ts:735`, `:757`, `:796`, `:826`). The hit-test still runs in
//! the DOM via `document.elementFromPoint` — nothing about stacking is
//! modelled in Rust — and the arm becomes dead weight, not a divergence, the
//! moment the registry ships the fields.
//!
//! Filtering, ranking and the verdict live in Rust
//! ([`build_visibility_report`]) so they are unit-testable without a webview.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::Json};
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

use super::helpers::{evaluate_js_expression_in_window, read_window_label};

/// Default `minRatio`: filters hairline overlaps. Mirrors
/// `params?.minRatio ?? 0.02` in the SDK handler (`handlers.ts:5281`).
const DEFAULT_MIN_RATIO: f64 = 0.02;

/// In-page occlusion probe.
///
/// Answers `{elementCount, candidates[]}` or `{error}`. Candidates are
/// UNFILTERED and UNSORTED — `minRatio`, ranking and the verdict are applied
/// in Rust so they can be tested without a browser.
///
/// `JSON.stringify` drops `undefined`, so an element with no `label` or no
/// text simply omits those keys — exactly as the SDK's `text: text ||
/// undefined` does.
const VISIBILITY_PROBE_JS: &str = r#"(() => {
  try {
    const bridge = window.__UI_BRIDGE__;
    const registry = bridge && bridge.registry;
    if (!registry || typeof registry.getAllElements !== 'function') {
      return JSON.stringify({ error: 'UI Bridge registry not available in this window' });
    }
    const all = registry.getAllElements() || [];

    // registry.ts:757 — below this size on BOTH axes only the centre is probed.
    const PERIMETER_MIN_PX = 48;
    // registry.ts:735 — centre first, then the inset perimeter.
    const POINTS = [
      [0.5, 0.5],
      [0.06, 0.06], [0.94, 0.06], [0.06, 0.94], [0.94, 0.94],
      [0.5, 0.06], [0.5, 0.94], [0.06, 0.5], [0.94, 0.5]
    ];

    // `classList(el)`, never raw `.className`: an SVG element's className is
    // an SVGAnimatedString, and the occluder is very often exactly that.
    const firstClass = (el) => {
      try {
        const cl = el.classList;
        if (cl && cl.length) return cl[0];
      } catch (e) { /* fall through */ }
      const raw = el.getAttribute && el.getAttribute('class');
      if (typeof raw === 'string') {
        const parts = raw.trim().split(/\s+/).filter(Boolean);
        if (parts.length) return parts[0];
      }
      return '';
    };

    // registry.ts:796 — prefer the registry id, because that is the name every
    // downstream report speaks; fall back to a DOM descriptor so an
    // unregistered overlay is still NAMED rather than an anonymous "something".
    const describeOccluder = (hit) => {
      const registered = hit.closest ? hit.closest('[data-ui-bridge-id]') : null;
      const id = registered && registered.getAttribute('data-ui-bridge-id');
      if (id) return id;
      const tag = (hit.tagName || '').toLowerCase();
      if (hit.id) return tag + '#' + hit.id;
      const cls = firstClass(hit);
      return cls ? tag + '.' + cls : tag;
    };

    // registry.ts:772 — guarded: jsdom does not implement elementFromPoint,
    // and an unguarded call would make the probe's absence a crash.
    const safeFromPoint = (x, y) => {
      try {
        return typeof document.elementFromPoint === 'function'
          ? document.elementFromPoint(x, y)
          : null;
      } catch (e) { return null; }
    };

    // registry.ts:826 computeVisibilityVerdict — guard chain then hit-test.
    const probe = (node) => {
      const rect = node.getBoundingClientRect();
      if (!rect || rect.width === 0 || rect.height === 0) return null;
      let style;
      try { style = window.getComputedStyle(node); } catch (e) { return null; }
      if (!style) return null;
      if (style.display === 'none') return null;
      if (style.visibility === 'hidden') return null;
      if (parseFloat(style.opacity) === 0) return null;
      const inViewport = rect.bottom > 0 && rect.right > 0
        && rect.top < window.innerHeight && rect.left < window.innerWidth;
      if (!inViewport) return null;

      const wide = rect.width >= PERIMETER_MIN_PX;
      const tall = rect.height >= PERIMETER_MIN_PX;
      const points = (wide || tall) ? POINTS : POINTS.slice(0, 1);

      let sampled = 0;
      let covered = 0;
      const blame = new Map();
      for (const p of points) {
        const px = rect.left + rect.width * p[0];
        const py = rect.top + rect.height * p[1];
        // A point outside the viewport tells us nothing — elementFromPoint
        // returns null there, which is not evidence of covering.
        if (px < 0 || px >= window.innerWidth || py < 0 || py >= window.innerHeight) continue;
        sampled++;
        const hit = safeFromPoint(px, py);
        if (hit === null) continue;
        if (hit === node || node.contains(hit)) continue;
        // An ANCESTOR at the sample point means our own box is transparent
        // there (padding, a gap between children) — not that we are covered.
        if (hit.contains(node)) continue;
        covered++;
        const who = describeOccluder(hit);
        blame.set(who, (blame.get(who) || 0) + 1);
      }
      if (sampled === 0 || covered === 0) return null;

      let occludedBy;
      let worst = 0;
      blame.forEach((n, who) => { if (n > worst) { worst = n; occludedBy = who; } });
      return { occludedBy: occludedBy, occludedPct: Math.round((covered / sampled) * 100) };
    };

    const candidates = [];
    for (const el of all) {
      let state = null;
      try { state = typeof el.getState === 'function' ? el.getState() : null; } catch (e) { state = null; }

      // Preferred arm: the registry's own hit-test verdict, used verbatim.
      let occludedBy = state && state.occludedBy;
      let occludedPct = state && state.occludedPct;

      if (!occludedBy) {
        const node = el.element;
        if (!node || typeof node.getBoundingClientRect !== 'function') continue;
        const found = probe(node);
        if (!found) continue;
        occludedBy = found.occludedBy;
        occludedPct = found.occludedPct;
      }
      if (!occludedBy) continue;

      // §4.6 redaction disposition: `textContent` is minted through
      // `scrubContentByVerdict` in getElementState, so a redacted element
      // arrives already scrubbed and nothing further happens here. Echoing
      // it is the point — "something is covered" is not actionable,
      // "the string `Zone 8` is covered" is.
      const text = state && typeof state.textContent === 'string' ? state.textContent.trim() : '';
      candidates.push({
        element: el.id,
        label: el.label,
        text: text || undefined,
        occludedBy: occludedBy,
        ratio: (occludedPct || 0) / 100,
        hidesText: text.length > 0
      });
    }

    return JSON.stringify({ elementCount: all.length, candidates: candidates });
  } catch (err) {
    return JSON.stringify({ error: String((err && err.message) || err) });
  }
})()"#;

/// Read `minRatio`, rejecting a non-numeric value BY NAME.
///
/// The SDK's `params?.minRatio ?? 0.02` would silently default a garbage
/// value; the runner's sibling handlers (e.g. `read-value`'s `all`) reject
/// by name instead, because a knob that was quietly dropped is exactly how a
/// caller ends up trusting a filter that never ran. Absent and `null` still
/// take the default — that is the contract, not a garbage value.
pub(super) fn validate_min_ratio(body: &serde_json::Value) -> Result<f64, String> {
    match body.get("minRatio") {
        None | Some(serde_json::Value::Null) => Ok(DEFAULT_MIN_RATIO),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (0.0..=1.0).contains(&n) => Ok(n),
            Some(n) => Err(format!(
                "'minRatio' must be a number between 0 and 1 (got {n})"
            )),
            None => Err("'minRatio' must be a number between 0 and 1".to_string()),
        },
    }
}

/// Read `includeExpected`, rejecting a non-boolean value by name.
pub(super) fn validate_include_expected(body: &serde_json::Value) -> Result<bool, String> {
    match body.get("includeExpected") {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(b)) => Ok(*b),
        Some(_) => Err("'includeExpected' must be a boolean".to_string()),
    }
}

/// Turn the probe's raw candidates into the SDK's `VisibilityReport`.
///
/// Pure: filtering by `minRatio`, stamping the two constant entry fields,
/// ranking and the verdict. Split out from the handler so the shape can be
/// asserted without a webview.
///
/// `isExpectedOverlay` is `false` on every entry and `source` is `hit-test`
/// on every entry — matching the SDK exactly. `includeExpected` is echoed and
/// filters nothing there either; the field is part of the shape so a consumer
/// merging results from `ui-bridge-auto`'s geometric arm can say which probe
/// found what.
pub(super) fn build_visibility_report(
    element_count: u64,
    candidates: &[serde_json::Value],
    min_ratio: f64,
    include_expected: bool,
) -> serde_json::Value {
    let mut occlusions: Vec<serde_json::Value> = candidates
        .iter()
        .filter_map(|c| {
            let element = c.get("element").and_then(|v| v.as_str())?;
            let occluded_by = c.get("occludedBy").and_then(|v| v.as_str())?;
            let ratio = c.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if ratio < min_ratio {
                return None;
            }
            let hides_text = c
                .get("hidesText")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut entry = serde_json::Map::new();
            entry.insert("element".into(), serde_json::json!(element));
            if let Some(label) = c.get("label").and_then(|v| v.as_str()) {
                entry.insert("label".into(), serde_json::json!(label));
            }
            if let Some(text) = c.get("text").and_then(|v| v.as_str()) {
                entry.insert("text".into(), serde_json::json!(text));
            }
            entry.insert("occludedBy".into(), serde_json::json!(occluded_by));
            entry.insert("ratio".into(), serde_json::json!(ratio));
            entry.insert("isExpectedOverlay".into(), serde_json::json!(false));
            entry.insert("hidesText".into(), serde_json::json!(hides_text));
            entry.insert("source".into(), serde_json::json!("hit-test"));
            Some(serde_json::Value::Object(entry))
        })
        .collect();

    // Worst first, and text-hiding occlusions outrank blank ones: a covered
    // label destroys information the reader cannot recover.
    occlusions.sort_by(|a, b| {
        let ah = a
            .get("hidesText")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let bh = b
            .get("hidesText")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        bh.cmp(&ah).then_with(|| {
            let ar = a.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let br = b.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.0);
            br.partial_cmp(&ar).unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // An empty list from a registry with no elements is UNKNOWN, not
    // "nothing is covered" — say which one this is.
    let verdict = if element_count == 0 {
        "unknown_empty_registry"
    } else if occlusions.is_empty() {
        "clear"
    } else {
        "occlusions_found"
    };

    serde_json::json!({
        "occlusions": occlusions,
        "elementCount": element_count,
        "minRatio": min_ratio,
        "includeExpected": include_expected,
        "verdict": verdict,
    })
}

/// POST /ui-bridge/control/visibility
///
/// Body (all optional): `{ "minRatio": 0.02, "includeExpected": false }`.
/// `windowLabel` additionally scopes the sweep to a pop-out window, the same
/// way every other in-page handler in this family accepts it; omit for the
/// main window.
pub async fn ui_bridge_visibility_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let body = body.map(|b| b.0).unwrap_or_else(|| serde_json::json!({}));

    let min_ratio = match validate_min_ratio(&body) {
        Ok(v) => v,
        Err(msg) => return Err((StatusCode::BAD_REQUEST, Json(api_error(msg)))),
    };
    let include_expected = match validate_include_expected(&body) {
        Ok(v) => v,
        Err(msg) => return Err((StatusCode::BAD_REQUEST, Json(api_error(msg)))),
    };

    info!("UI Bridge API: visibility sweep (minRatio={min_ratio})");

    let window_label = read_window_label(&body);
    let raw = evaluate_js_expression_in_window(&state, VISIBILITY_PROBE_JS, window_label)
        .await
        .map_err(|e| {
            error!("UI Bridge API: visibility probe failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))
        })?;

    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        error!("UI Bridge API: visibility probe returned unparseable JSON: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "visibility probe returned unparseable JSON: {e}"
            ))),
        )
    })?;

    // The probe reports its own failure rather than throwing, so an absent
    // registry surfaces as an error instead of a fabricated `clear` verdict.
    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(err.to_string())),
        ));
    }

    let element_count = parsed
        .get("elementCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let candidates = parsed
        .get("candidates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(Json(ApiResponse::success(build_visibility_report(
        element_count,
        &candidates,
        min_ratio,
        include_expected,
    ))))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route(
        "/ui-bridge/control/visibility",
        post(ui_bridge_visibility_handler),
    )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[("POST", "/ui-bridge/control/visibility")]
}

#[cfg(test)]
mod visibility_report_tests {
    //! Shape seam for `POST /control/visibility`. Asserts the report matches
    //! the SDK's `VisibilityReport` / `VisibilityOcclusionEntry`
    //! (`ui-bridge/packages/ui-bridge/src/server/types.ts:2413`) without
    //! needing an axum router or a live webview.

    use super::{build_visibility_report, validate_include_expected, validate_min_ratio};
    use serde_json::json;

    fn candidate(element: &str, ratio: f64, hides_text: bool) -> serde_json::Value {
        let mut v = json!({
            "element": element,
            "occludedBy": "svg.minimap",
            "ratio": ratio,
            "hidesText": hides_text,
        });
        if hides_text {
            v["text"] = json!("Zone 8: qontinui-web");
            v["label"] = json!("session name");
        }
        v
    }

    #[test]
    fn report_carries_every_contract_field() {
        let report = build_visibility_report(12, &[candidate("el-1", 0.4, true)], 0.02, false);

        assert_eq!(report["elementCount"], json!(12));
        assert_eq!(report["minRatio"], json!(0.02));
        assert_eq!(report["includeExpected"], json!(false));
        assert_eq!(report["verdict"], json!("occlusions_found"));

        let entry = &report["occlusions"][0];
        assert_eq!(entry["element"], json!("el-1"));
        assert_eq!(entry["label"], json!("session name"));
        assert_eq!(entry["text"], json!("Zone 8: qontinui-web"));
        assert_eq!(entry["occludedBy"], json!("svg.minimap"));
        assert_eq!(entry["ratio"], json!(0.4));
        assert_eq!(entry["isExpectedOverlay"], json!(false));
        assert_eq!(entry["hidesText"], json!(true));
        assert_eq!(entry["source"], json!("hit-test"));
    }

    #[test]
    fn label_and_text_are_omitted_rather_than_null() {
        let report = build_visibility_report(3, &[candidate("el-1", 0.5, false)], 0.02, false);
        let entry = report["occlusions"][0].as_object().unwrap();
        assert!(
            !entry.contains_key("label"),
            "label must be omitted, not null"
        );
        assert!(
            !entry.contains_key("text"),
            "text must be omitted, not null"
        );
    }

    #[test]
    fn min_ratio_filters_hairline_overlaps() {
        let cands = vec![
            candidate("hairline", 0.01, false),
            candidate("real", 0.30, false),
        ];
        let report = build_visibility_report(9, &cands, 0.02, false);
        assert_eq!(report["occlusions"].as_array().unwrap().len(), 1);
        assert_eq!(report["occlusions"][0]["element"], json!("real"));
    }

    #[test]
    fn text_hiding_occlusions_outrank_blank_ones_then_ratio() {
        let cands = vec![
            candidate("blank-big", 0.90, false),
            candidate("text-small", 0.10, true),
            candidate("text-big", 0.60, true),
        ];
        let report = build_visibility_report(9, &cands, 0.02, false);
        let order: Vec<&str> = report["occlusions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["element"].as_str().unwrap())
            .collect();
        assert_eq!(order, vec!["text-big", "text-small", "blank-big"]);
    }

    #[test]
    fn empty_registry_is_unknown_not_clear() {
        let report = build_visibility_report(0, &[], 0.02, false);
        assert_eq!(report["verdict"], json!("unknown_empty_registry"));
        assert_eq!(
            build_visibility_report(7, &[], 0.02, false)["verdict"],
            json!("clear")
        );
    }

    #[test]
    fn params_default_and_are_echoed() {
        assert_eq!(validate_min_ratio(&json!({})).unwrap(), 0.02);
        assert_eq!(
            validate_min_ratio(&json!({ "minRatio": null })).unwrap(),
            0.02
        );
        assert_eq!(
            validate_min_ratio(&json!({ "minRatio": 0.5 })).unwrap(),
            0.5
        );
        assert!(!validate_include_expected(&json!({})).unwrap());
        assert!(validate_include_expected(&json!({ "includeExpected": true })).unwrap());

        let report = build_visibility_report(1, &[], 0.5, true);
        assert_eq!(report["minRatio"], json!(0.5));
        assert_eq!(report["includeExpected"], json!(true));
    }

    #[test]
    fn bad_params_are_rejected_by_name() {
        assert!(validate_min_ratio(&json!({ "minRatio": "0.5" }))
            .unwrap_err()
            .contains("minRatio"));
        assert!(validate_min_ratio(&json!({ "minRatio": 5 }))
            .unwrap_err()
            .contains("minRatio"));
        assert!(
            validate_include_expected(&json!({ "includeExpected": "yes" }))
                .unwrap_err()
                .contains("includeExpected")
        );
    }
}
