//! Every mounted POST route under `/ui-bridge` is CLASSIFIED for the journey
//! ledger (plan 2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time,
//! review item M3): either RECORDED — its handler calls the ledger — or
//! NOT_AN_ACTION with a one-line reason. A newly mounted POST route fails
//! [`every_mounted_post_route_is_classified`] until someone classifies it,
//! because a transport whose handler never calls the ledger writes no row and
//! fails nothing.
//!
//! axum 0.8 cannot enumerate a live router, so — like
//! `mcp::ui_bridge::manifest_drift_tests::manifest_matches_route_calls` — the
//! routes are read from the router registrations in the source (the route
//! registration calls and the dual-prefix macro). Comment lines are blanked
//! before scanning, so a registration quoted in prose is never counted.

use std::collections::{BTreeMap, BTreeSet};

/// POST routes whose handler records a journey edge.
const RECORDED: &[&str] = &[
    "/ui-bridge/ai/execute",
    "/ui-bridge/ai/execute-with-diff",
    "/ui-bridge/ai/fill-form",
    "/ui-bridge/ai/intents/execute",
    "/ui-bridge/ai/intents/execute-from-query",
    "/ui-bridge/ai/recovery/attempt",
    "/ui-bridge/batch",
    "/ui-bridge/control/action-plan",
    "/ui-bridge/control/actions/batch",
    "/ui-bridge/control/activate-tab/{tab_id}",
    "/ui-bridge/control/ai/execute-with-diff",
    "/ui-bridge/control/batch",
    "/ui-bridge/control/batch-actions",
    "/ui-bridge/control/batch-execute",
    "/ui-bridge/control/component/{id}/action/{action_id}",
    "/ui-bridge/control/element/{id}/action",
    "/ui-bridge/control/fill",
    "/ui-bridge/control/intent/{name}/execute",
    "/ui-bridge/control/intents/execute",
    "/ui-bridge/control/intents/execute-from-query",
    "/ui-bridge/control/key",
    "/ui-bridge/control/navigate-and-wait",
    "/ui-bridge/control/navigate-tab",
    "/ui-bridge/control/page/back",
    "/ui-bridge/control/page/click-by-selector",
    "/ui-bridge/control/page/click-by-text",
    "/ui-bridge/control/page/forward",
    "/ui-bridge/control/page/hard-refresh",
    "/ui-bridge/control/page/navigate",
    "/ui-bridge/control/page/navigate-to",
    "/ui-bridge/control/page/refresh",
    "/ui-bridge/control/page/scroll",
    "/ui-bridge/control/page/send-keys",
    "/ui-bridge/control/page/set-tab",
    "/ui-bridge/control/page/type-into",
    "/ui-bridge/control/redo",
    "/ui-bridge/control/states/navigate",
    "/ui-bridge/control/tab/activate",
    "/ui-bridge/control/transition/{id}/execute",
    "/ui-bridge/control/undo",
    "/ui-bridge/control/view/open",
    "/ui-bridge/control/with-diff",
    "/ui-bridge/control/workflow/{id}/run",
    "/ui-bridge/sdk/ai/execute",
    "/ui-bridge/sdk/ai/execute-with-diff",
    "/ui-bridge/sdk/ai/intents/execute",
    "/ui-bridge/sdk/ai/intents/execute-from-query",
    "/ui-bridge/sdk/ai/recovery/attempt",
    "/ui-bridge/sdk/component/{id}/action/{actionId}",
    "/ui-bridge/sdk/control/batch",
    "/ui-bridge/sdk/control/batch-action",
    "/ui-bridge/sdk/control/component/{id}/action/{actionId}",
    "/ui-bridge/sdk/control/element/{id}/action",
    "/ui-bridge/sdk/control/fill",
    "/ui-bridge/sdk/control/page/back",
    "/ui-bridge/sdk/control/page/click-by-selector",
    "/ui-bridge/sdk/control/page/click-by-text",
    "/ui-bridge/sdk/control/page/forward",
    "/ui-bridge/sdk/control/page/navigate",
    "/ui-bridge/sdk/control/page/navigate-to",
    "/ui-bridge/sdk/control/page/refresh",
    "/ui-bridge/sdk/control/page/scroll",
    "/ui-bridge/sdk/control/page/send-keys",
    "/ui-bridge/sdk/control/page/type-into",
    "/ui-bridge/sdk/control/redo",
    "/ui-bridge/sdk/control/states/navigate",
    "/ui-bridge/sdk/control/transition/{id}/execute",
    "/ui-bridge/sdk/control/undo",
    "/ui-bridge/sdk/control/workflow/{id}/run",
    "/ui-bridge/sdk/element/{id}/action",
    "/ui-bridge/sdk/execute-action-plan",
    "/ui-bridge/sdk/fill",
    "/ui-bridge/sdk/page/back",
    "/ui-bridge/sdk/page/click-by-selector",
    "/ui-bridge/sdk/page/click-by-text",
    "/ui-bridge/sdk/page/forward",
    "/ui-bridge/sdk/page/navigate",
    "/ui-bridge/sdk/page/navigate-to",
    "/ui-bridge/sdk/page/refresh",
    "/ui-bridge/sdk/page/send-keys",
    "/ui-bridge/sdk/page/type-into",
    "/ui-bridge/sdk/redo",
    "/ui-bridge/sdk/states/navigate",
    "/ui-bridge/sdk/transition/{id}/execute",
    "/ui-bridge/sdk/undo",
    "/ui-bridge/sdk/workflow/{id}/run",
];

const R00: &str = "analysis / capture of the page; acts on nothing";
const R01: &str = "change-tracking / bookkeeping read; acts on nothing";
const R02: &str = "waits for a condition; acts on nothing";
const R03: &str = "read-only lookup";
const R04: &str = "transport / connection management; acts on no page";
const R05: &str = "bridge diagnostics state; acts on no page";
const R06: &str = "assertion / verification; reads the page";
const R07: &str =
    "sets a declared-state flag in the SDK registry directly; no affordance is activated";
const R08: &str = "test-fixture route (debug builds); not a UI action";
const R09: &str = "codebase-integration tooling; touches source files, not the UI";
const R10: &str = "debug overlay / annotation; not an app affordance";
const R11: &str = "intent-registry maintenance; acts on no page";
const R12: &str =
    "arbitrary script evaluation, not a declared affordance; the journey records UI actions only";
const R13: &str = "generic command proxy carrying untyped commands; typed actions are recorded at their own routes (known gap: an action sent through this proxy is not recorded)";
const R14: &str = "writes the system clipboard; not an app affordance";
const R15: &str = "starts/stops the existing Python crawler (exploration.rs), not a Phase 3 explorer, and is itself no UI action; its clicks go through the RECORDED control routes and so land as run_kind=agent_action (known gap: crawler edges are mislabelled agent_action)";
const R15B: &str = "derives states from existing render logs (co-occurrence analysis, exploration.rs); acts on no page";
const R16: &str = "window lifecycle; ends the session rather than moving within it";
const R17: &str = "viewport configuration; not a UI affordance";
const R18: &str = "clears browser storage; not a UI affordance";
const R19: &str = "effect prediction; acts on nothing";

/// POST routes that are not UI actions, each with the reason.
const NOT_AN_ACTION: &[(&str, &str)] = &[
    ("/ui-bridge/ai/analyze/cross-app-compare", R00),
    ("/ui-bridge/ai/analyze/data", R00),
    ("/ui-bridge/ai/analyze/regions", R00),
    ("/ui-bridge/ai/analyze/structured-data", R00),
    ("/ui-bridge/ai/assert", R06),
    ("/ui-bridge/ai/assert-batch", R06),
    ("/ui-bridge/ai/assert/batch", R06),
    ("/ui-bridge/ai/bookmarks", R01),
    ("/ui-bridge/ai/change-buffer/disable", R01),
    ("/ui-bridge/ai/change-buffer/drain", R01),
    ("/ui-bridge/ai/change-buffer/enable", R01),
    ("/ui-bridge/ai/design-audit", R00),
    ("/ui-bridge/ai/expect", R06),
    ("/ui-bridge/ai/find", R03),
    ("/ui-bridge/ai/image-diff", R00),
    ("/ui-bridge/ai/intents/find", R11),
    ("/ui-bridge/ai/intents/register", R11),
    ("/ui-bridge/ai/network-probe", R00),
    ("/ui-bridge/ai/page-summary", R03),
    ("/ui-bridge/ai/scoped-diff", R01),
    ("/ui-bridge/ai/search", R03),
    ("/ui-bridge/ai/semantic-search", R03),
    ("/ui-bridge/ai/structured-changes", R01),
    ("/ui-bridge/ai/summarize-diff", R01),
    ("/ui-bridge/ai/wait-for", R02),
    ("/ui-bridge/ai/wait-for-change", R02),
    ("/ui-bridge/ai/wait-for-element", R02),
    ("/ui-bridge/ai/wait-for-element-condition", R02),
    ("/ui-bridge/ai/wait-for-idle", R02),
    ("/ui-bridge/ai/wait-for-navigation", R02),
    ("/ui-bridge/ai/wait-for-route", R02),
    ("/ui-bridge/ai/wait-for-route-change", R02),
    ("/ui-bridge/annotations/import", R10),
    ("/ui-bridge/apps/forward-device", R04),
    ("/ui-bridge/apps/register", R04),
    ("/ui-bridge/apps/scan", R04),
    ("/ui-bridge/apps/{app_id}/dispatch", R13),
    ("/ui-bridge/circuit-breaker/reset", R05),
    ("/ui-bridge/commands", R04),
    ("/ui-bridge/control/ai/bookmarks", R01),
    ("/ui-bridge/control/ai/change-buffer/disable", R01),
    ("/ui-bridge/control/ai/change-buffer/drain", R01),
    ("/ui-bridge/control/ai/change-buffer/enable", R01),
    ("/ui-bridge/control/ai/find", R03),
    ("/ui-bridge/control/ai/image-diff", R00),
    ("/ui-bridge/control/ai/scoped-diff", R01),
    ("/ui-bridge/control/ai/search", R03),
    ("/ui-bridge/control/ai/structured-changes", R01),
    ("/ui-bridge/control/ai/summarize-diff", R01),
    ("/ui-bridge/control/ai/wait-for-change", R02),
    ("/ui-bridge/control/annotation/{id}", R10),
    ("/ui-bridge/control/annotations", R10),
    ("/ui-bridge/control/annotations/import", R10),
    ("/ui-bridge/control/assert", R06),
    ("/ui-bridge/control/clear-storage", R18),
    ("/ui-bridge/control/clipboard", R14),
    ("/ui-bridge/control/clipboard/write", R14),
    (
        "/ui-bridge/control/component/{id}/action/{action_id}/predict",
        R19,
    ),
    ("/ui-bridge/control/console-errors/clear", R05),
    ("/ui-bridge/control/design/audit", R00),
    ("/ui-bridge/control/design/element/{id}/state-styles", R00),
    ("/ui-bridge/control/design/evaluate", R00),
    ("/ui-bridge/control/design/evaluate/baseline", R00),
    ("/ui-bridge/control/design/evaluate/diff", R00),
    ("/ui-bridge/control/design/responsive", R00),
    ("/ui-bridge/control/design/snapshot", R00),
    ("/ui-bridge/control/design/style-guide/clear", R00),
    ("/ui-bridge/control/design/style-guide/load", R00),
    ("/ui-bridge/control/discover", R03),
    ("/ui-bridge/control/element/{id}/assert", R06),
    ("/ui-bridge/control/element/{id}/expect", R06),
    ("/ui-bridge/control/elements/rank", R03),
    ("/ui-bridge/control/error-baselines/capture", R05),
    ("/ui-bridge/control/error-baselines/compare", R05),
    ("/ui-bridge/control/error-sessions/end", R05),
    ("/ui-bridge/control/error-sessions/start", R05),
    ("/ui-bridge/control/find", R03),
    ("/ui-bridge/control/forms/diff", R01),
    ("/ui-bridge/control/forms/snapshot", R01),
    ("/ui-bridge/control/intents", R11),
    ("/ui-bridge/control/intents/find", R11),
    ("/ui-bridge/control/network-requests/wait", R02),
    ("/ui-bridge/control/network/stubs", R05),
    ("/ui-bridge/control/network/verify-stub", R05),
    ("/ui-bridge/control/page-health", R00),
    ("/ui-bridge/control/page/close-request", R16),
    ("/ui-bridge/control/page/evaluate", R12),
    ("/ui-bridge/control/page/evaluate-batch", R12),
    ("/ui-bridge/control/page/evaluate-raw", R12),
    ("/ui-bridge/control/page/evaluate-safe", R12),
    ("/ui-bridge/control/page/find-by-text", R03),
    ("/ui-bridge/control/page/force-close", R16),
    ("/ui-bridge/control/page/read-value", R03),
    ("/ui-bridge/control/page/summary", R03),
    ("/ui-bridge/control/performance-entries/clear", R05),
    ("/ui-bridge/control/query-selector", R03),
    ("/ui-bridge/control/render-log", R01),
    ("/ui-bridge/control/sdk/spawn-headless", R04),
    ("/ui-bridge/control/spec/{id}/run", R06),
    ("/ui-bridge/control/state-group/{id}/activate", R07),
    ("/ui-bridge/control/state-group/{id}/deactivate", R07),
    ("/ui-bridge/control/state/{id}/activate", R07),
    ("/ui-bridge/control/state/{id}/deactivate", R07),
    ("/ui-bridge/control/states/find-path", R03),
    ("/ui-bridge/control/viewport-constraints", R17),
    ("/ui-bridge/control/visibility", R00),
    ("/ui-bridge/control/wait-for-app", R04),
    ("/ui-bridge/control/wait-for-element", R02),
    ("/ui-bridge/control/wait-for-element-stable", R02),
    ("/ui-bridge/control/wait-for-element-state", R02),
    ("/ui-bridge/control/wait-for-idle", R02),
    ("/ui-bridge/control/wait-for-idle/{signal}", R02),
    ("/ui-bridge/control/wait-for-navigation", R02),
    ("/ui-bridge/control/wait-for-route", R02),
    ("/ui-bridge/control/wait-for-route-change", R02),
    ("/ui-bridge/control/wait-for-targets", R02),
    ("/ui-bridge/debug/highlight/{id}", R10),
    ("/ui-bridge/design/audit", R00),
    ("/ui-bridge/design/element/{id}/state-styles", R00),
    ("/ui-bridge/design/evaluate", R00),
    ("/ui-bridge/design/evaluate/baseline", R00),
    ("/ui-bridge/design/evaluate/diff", R00),
    ("/ui-bridge/design/responsive", R00),
    ("/ui-bridge/design/snapshot", R00),
    ("/ui-bridge/design/style-guide/load", R00),
    ("/ui-bridge/devices/pair/confirm", R04),
    ("/ui-bridge/devices/pair/initiate", R04),
    ("/ui-bridge/devices/register-lan", R04),
    ("/ui-bridge/devices/{id}/connect", R04),
    ("/ui-bridge/devices/{id}/disconnect", R04),
    ("/ui-bridge/devices/{id}/transport/prefer", R04),
    ("/ui-bridge/discover-states", R15B),
    ("/ui-bridge/explore", R15),
    ("/ui-bridge/explore/stop", R15),
    ("/ui-bridge/headless/close", R04),
    ("/ui-bridge/headless/launch", R04),
    ("/ui-bridge/heartbeat", R04),
    ("/ui-bridge/integration/analyze", R09),
    ("/ui-bridge/integration/cache-architecture-spec", R09),
    ("/ui-bridge/integration/discover-pages", R09),
    ("/ui-bridge/integration/health-check", R09),
    ("/ui-bridge/integration/integrate", R09),
    ("/ui-bridge/integration/preview", R09),
    ("/ui-bridge/integration/read-file", R09),
    ("/ui-bridge/integration/read-page-source", R09),
    ("/ui-bridge/integration/update", R09),
    ("/ui-bridge/integration/write-hooks", R09),
    ("/ui-bridge/invoke/{command_name}", R13),
    ("/ui-bridge/ios/forward", R04),
    ("/ui-bridge/ipc-response", R04),
    ("/ui-bridge/observe/{command}", R01),
    ("/ui-bridge/pong", R04),
    ("/ui-bridge/relay/dispatch", R13),
    ("/ui-bridge/render-log", R01),
    ("/ui-bridge/render-log/snapshot", R01),
    ("/ui-bridge/sdk/ai/analyze/cross-app-compare", R00),
    ("/ui-bridge/sdk/ai/assert", R06),
    ("/ui-bridge/sdk/ai/assert/batch", R06),
    ("/ui-bridge/sdk/ai/bookmarks", R01),
    ("/ui-bridge/sdk/ai/change-buffer/disable", R01),
    ("/ui-bridge/sdk/ai/change-buffer/drain", R01),
    ("/ui-bridge/sdk/ai/change-buffer/enable", R01),
    ("/ui-bridge/sdk/ai/find", R03),
    ("/ui-bridge/sdk/ai/intents/find", R11),
    ("/ui-bridge/sdk/ai/intents/register", R11),
    ("/ui-bridge/sdk/ai/media/analyze", R00),
    ("/ui-bridge/sdk/ai/media/analyze/batch", R00),
    ("/ui-bridge/sdk/ai/media/analyze/page", R00),
    ("/ui-bridge/sdk/ai/media/audit/accessibility", R00),
    ("/ui-bridge/sdk/ai/media/audit/performance", R00),
    ("/ui-bridge/sdk/ai/media/compare", R00),
    ("/ui-bridge/sdk/ai/media/find", R03),
    ("/ui-bridge/sdk/ai/media/snapshot", R00),
    ("/ui-bridge/sdk/ai/scoped-diff", R01),
    ("/ui-bridge/sdk/ai/search", R03),
    ("/ui-bridge/sdk/ai/semantic-search", R03),
    ("/ui-bridge/sdk/ai/structured-changes", R01),
    ("/ui-bridge/sdk/ai/summarize-diff", R01),
    ("/ui-bridge/sdk/ai/wait-for-change", R02),
    ("/ui-bridge/sdk/annotations/import", R10),
    ("/ui-bridge/sdk/auto/assertScreenshot", R06),
    ("/ui-bridge/sdk/auto/assertText", R06),
    ("/ui-bridge/sdk/auto/captureBaseline", R00),
    ("/ui-bridge/sdk/auto/dismissAllHighlights", R00),
    ("/ui-bridge/sdk/auto/dismissHighlight", R00),
    ("/ui-bridge/sdk/auto/extractText", R00),
    ("/ui-bridge/sdk/auto/highlightElement", R10),
    ("/ui-bridge/sdk/auto/translateCoordinate", R00),
    ("/ui-bridge/sdk/clipboard", R14),
    ("/ui-bridge/sdk/connect", R04),
    ("/ui-bridge/sdk/console-errors/clear", R05),
    ("/ui-bridge/sdk/console/error-baselines/capture", R05),
    ("/ui-bridge/sdk/console/error-baselines/compare", R05),
    ("/ui-bridge/sdk/console/error-sessions/end", R05),
    ("/ui-bridge/sdk/console/error-sessions/start", R05),
    ("/ui-bridge/sdk/control/clipboard", R14),
    ("/ui-bridge/sdk/control/console-errors/clear", R05),
    ("/ui-bridge/sdk/control/discover", R03),
    ("/ui-bridge/sdk/control/error-baselines/capture", R05),
    ("/ui-bridge/sdk/control/error-baselines/compare", R05),
    ("/ui-bridge/sdk/control/error-sessions/end", R05),
    ("/ui-bridge/sdk/control/error-sessions/start", R05),
    ("/ui-bridge/sdk/control/find", R03),
    ("/ui-bridge/sdk/control/forms/diff", R01),
    ("/ui-bridge/sdk/control/forms/snapshot", R01),
    ("/ui-bridge/sdk/control/network-requests/wait", R02),
    ("/ui-bridge/sdk/control/page-evaluate", R12),
    ("/ui-bridge/sdk/control/page/find-by-text", R03),
    ("/ui-bridge/sdk/control/page/read-value", R03),
    ("/ui-bridge/sdk/control/performance-entries/clear", R05),
    ("/ui-bridge/sdk/control/query-selector", R03),
    ("/ui-bridge/sdk/control/state-group/{id}/activate", R07),
    ("/ui-bridge/sdk/control/state-group/{id}/deactivate", R07),
    ("/ui-bridge/sdk/control/state/{id}/activate", R07),
    ("/ui-bridge/sdk/control/state/{id}/deactivate", R07),
    ("/ui-bridge/sdk/control/states/find-path", R03),
    ("/ui-bridge/sdk/control/viewport", R17),
    ("/ui-bridge/sdk/control/wait-for-element", R02),
    ("/ui-bridge/sdk/control/wait-for-element-by-condition", R02),
    ("/ui-bridge/sdk/control/wait-for-element-registered", R02),
    ("/ui-bridge/sdk/control/wait-for-idle", R02),
    ("/ui-bridge/sdk/control/wait-for-idle/{signal}", R02),
    ("/ui-bridge/sdk/control/wait-for-route-change", R02),
    ("/ui-bridge/sdk/control/wait-for-targets", R02),
    ("/ui-bridge/sdk/debug/highlight/{id}", R10),
    ("/ui-bridge/sdk/design/audit", R00),
    ("/ui-bridge/sdk/design/element/{id}/state-styles", R00),
    ("/ui-bridge/sdk/design/evaluate", R00),
    ("/ui-bridge/sdk/design/evaluate/baseline", R00),
    ("/ui-bridge/sdk/design/evaluate/diff", R00),
    ("/ui-bridge/sdk/design/responsive", R00),
    ("/ui-bridge/sdk/design/snapshot", R00),
    ("/ui-bridge/sdk/design/style-guide/load", R00),
    ("/ui-bridge/sdk/diagnose-stuck", R05),
    ("/ui-bridge/sdk/disconnect", R04),
    ("/ui-bridge/sdk/discover", R03),
    ("/ui-bridge/sdk/discover-and-cache", R03),
    ("/ui-bridge/sdk/find", R03),
    ("/ui-bridge/sdk/forms/diff", R01),
    ("/ui-bridge/sdk/forms/snapshot", R01),
    ("/ui-bridge/sdk/heartbeat", R04),
    ("/ui-bridge/sdk/network-requests/wait", R02),
    ("/ui-bridge/sdk/page/find-by-text", R03),
    ("/ui-bridge/sdk/page/read-value", R03),
    ("/ui-bridge/sdk/performance-entries/clear", R05),
    ("/ui-bridge/sdk/render-log/snapshot", R01),
    ("/ui-bridge/sdk/state-group/{id}/activate", R07),
    ("/ui-bridge/sdk/state-group/{id}/deactivate", R07),
    ("/ui-bridge/sdk/state/{id}/activate", R07),
    ("/ui-bridge/sdk/state/{id}/deactivate", R07),
    ("/ui-bridge/sdk/states/find-path", R03),
    ("/ui-bridge/sdk/switch", R04),
    ("/ui-bridge/sdk/wait-for-idle", R02),
    ("/ui-bridge/sdk/wait-for-idle/{signal}", R02),
    ("/ui-bridge/sdk/wait-for-targets", R02),
    ("/ui-bridge/specs/verify-api", R06),
    ("/ui-bridge/tauri/invoke", R13),
    ("/ui-bridge/test/append-transcript-record", R08),
    ("/ui-bridge/test/clear-injected", R08),
    ("/ui-bridge/test/clear-lifecycle-store", R08),
    ("/ui-bridge/test/clear-sessions", R08),
    ("/ui-bridge/test/coord-mcp/seed-agent-token", R08),
    ("/ui-bridge/test/force-identity-evidence", R08),
    ("/ui-bridge/test/inject-errors", R08),
    ("/ui-bridge/test/inject-session", R08),
    ("/ui-bridge/test/list-lifecycle-open", R08),
    ("/ui-bridge/test/seed-error-scenario", R08),
    ("/ui-bridge/test/seed-lifecycle-store", R08),
    ("/ui-bridge/test/seed-terminal-scenario", R08),
    ("/ui-bridge/vision/analyze", R00),
    ("/ui-bridge/vision/annotate", R00),
    ("/ui-bridge/vision/assert", R06),
    ("/ui-bridge/vision/baseline", R00),
    ("/ui-bridge/vision/capture", R00),
    ("/ui-bridge/vision/describe", R00),
    ("/ui-bridge/vision/diff", R00),
    ("/ui-bridge/vision/extract", R00),
    ("/ui-bridge/vision/mutation-occurred", R00),
    ("/ui-bridge/vision/raw", R00),
];

/// The ledger entry points a RECORDED handler (or the wrapper it is) calls.
const LEDGER_CALLS: &[&str] = &[
    "record_control_result(",
    "record_sdk_result(",
    "record_sdk_navigation_result(",
    "record_action(",
    "record_diff(",
    "record_diff_result(",
    "record_sdk_component_action(",
    "record_sdk_batch(",
];

/// `src` with every line whose first non-blank characters are `//` (a line,
/// doc or inner-doc comment) blanked, line count preserved. A registration
/// quoted in a comment is prose, not a mounted route (B1).
fn strip_comment_lines(src: &str) -> String {
    src.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn comment_lines_are_not_scanned() {
    let src = "//! .route(\"/ui-bridge/x\", post(h))\n    /// doc\n        .route(\"/ui-bridge/y\", post(g))";
    let stripped = strip_comment_lines(src);
    assert!(!stripped.contains("/ui-bridge/x"));
    assert!(stripped.contains("/ui-bridge/y"));
    assert_eq!(stripped.lines().count(), src.lines().count());
}

fn rust_sources() -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path, strip_comment_lines(&text)));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

/// `(path → handler names)` for every POST route registered in the source.
fn mounted_post_routes(
    sources: &[(std::path::PathBuf, String)],
) -> BTreeMap<String, BTreeSet<String>> {
    let route_re =
        regex::Regex::new(r#"(?s)\.route\(\s*(?://[^\n]*\n\s*)*"(/ui-bridge/[^"]+)"\s*,"#)
            .unwrap_or_else(|e| panic!("route regex: {e}"));
    let method_re = regex::Regex::new(r"\b(get|post|put|delete|patch)\(\s*([A-Za-z0-9_:]+)")
        .unwrap_or_else(|e| panic!("method regex: {e}"));
    let dual_re = regex::Regex::new(
        r#"add_dual!\s*\(\s*[^,]+,\s*(get|post|put|delete|patch)\s*,\s*"([^"]+)"\s*,\s*([A-Za-z0-9_:]+)"#,
    )
    .unwrap_or_else(|e| panic!("add_dual regex: {e}"));
    let mut routes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (_, src) in sources {
        for cap in route_re.captures_iter(src) {
            let (Some(path), Some(whole)) = (cap.get(1), cap.get(0)) else {
                continue;
            };
            let body = src
                .get(whole.end()..(whole.end() + 400).min(src.len()))
                .or_else(|| src.get(whole.end()..))
                .unwrap_or_default();
            let end = body
                .find("\n        .route(")
                .or_else(|| body.find("\n    }"))
                .unwrap_or(body.len());
            for m in method_re.captures_iter(body.get(..end).unwrap_or(body)) {
                if m.get(1).map(|x| x.as_str()) == Some("post") {
                    if let Some(h) = m.get(2) {
                        routes.entry(path.as_str().to_string()).or_default().insert(
                            h.as_str()
                                .rsplit("::")
                                .next()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
            }
        }
        for cap in dual_re.captures_iter(src) {
            if cap.get(1).map(|x| x.as_str()) != Some("post") {
                continue;
            }
            let (Some(tail), Some(h)) = (cap.get(2), cap.get(3)) else {
                continue;
            };
            let handler = h
                .as_str()
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_string();
            for prefix in ["control", "ai"] {
                routes
                    .entry(format!("/ui-bridge/{prefix}/{}", tail.as_str()))
                    .or_default()
                    .insert(handler.clone());
            }
        }
    }
    routes
}

/// The body of the first fn named `name` (from its signature to the next
/// top-level `}`), searched across every source.
fn fn_body<'a>(sources: &'a [(std::path::PathBuf, String)], name: &str) -> Option<&'a str> {
    let sigs = [
        format!("\npub async fn {name}("),
        format!("\nasync fn {name}("),
        format!("\npub fn {name}("),
        format!("\nfn {name}("),
        format!("\npub(crate) async fn {name}("),
        format!("\npub(crate) fn {name}("),
    ];
    sources.iter().find_map(|(_, src)| {
        let start = sigs.iter().find_map(|s| src.find(s.as_str()))?;
        let rest = src.get(start + 1..)?;
        let end = rest.find("\n}\n").map(|i| i + 3).unwrap_or(rest.len());
        rest.get(..end)
    })
}

#[test]
fn every_mounted_post_route_is_classified() {
    let sources = rust_sources();
    let mounted = mounted_post_routes(&sources);
    assert!(
        mounted.len() > 300,
        "route scan found only {} POST routes — the scan is broken, not the routes",
        mounted.len()
    );
    let recorded: BTreeSet<&str> = RECORDED.iter().copied().collect();
    let denied: BTreeMap<&str, &str> = NOT_AN_ACTION.iter().copied().collect();

    let mut problems = Vec::new();
    for (path, handlers) in &mounted {
        match (recorded.contains(path.as_str()), denied.get(path.as_str())) {
            (true, Some(_)) => problems.push(format!("{path} is in BOTH lists")),
            (false, None) => problems.push(format!(
                "{path} (handler {handlers:?}) is mounted but UNCLASSIFIED — add it to \
                 RECORDED (and make its handler call the journey ledger) or to NOT_AN_ACTION \
                 with a one-line reason"
            )),
            (true, None) => {
                let calls_ledger = handlers.iter().any(|h| {
                    fn_body(&sources, h).is_some_and(|b| LEDGER_CALLS.iter().any(|c| b.contains(c)))
                });
                if !calls_ledger {
                    problems.push(format!(
                        "{path} is RECORDED but its handler {handlers:?} calls no journey ledger entry point"
                    ));
                }
            }
            (false, Some(reason)) => {
                if reason.trim().is_empty() {
                    problems.push(format!("{path} is NOT_AN_ACTION with no reason"));
                }
            }
        }
    }
    for path in recorded.iter().chain(denied.keys()) {
        if !mounted.contains_key(*path) {
            problems.push(format!(
                "{path} is classified but no longer mounted — remove it"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "journey route coverage:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_snapshot_routes_close_pending_edges() {
    let sources = rust_sources();
    for (handler, call) in [
        ("ui_bridge_get_snapshot_handler", "record_snapshot("),
        ("handle_snapshot", "record_journey_snapshot("),
        ("record_journey_snapshot", "record_snapshot("),
    ] {
        assert!(
            fn_body(&sources, handler).is_some_and(|b| b.contains(call)),
            "{handler} must call `{call}`"
        );
    }
}
