//! SDK↔runner WIRE-SHAPE pins — field names, response shapes, status
//! commitments.
//!
//! Plan `2026-09-09-runner-ui-bridge-0-24-to-0-26-across-the-rust-ts-boundary`,
//! Phase 2.
//!
//! # What this module is NOT
//!
//! It is **not** a second route-path pin. Two of those already exist and this
//! module deliberately duplicates neither:
//!
//! * `relay::tests::tabs_body_pins_the_wire_contract` — pins what the runner
//!   EMITS on `GET /ui-bridge/tabs`.
//! * `manifest_drift_tests::sdk_manifest_routes_are_exposed_by_runner`
//!   (`mod.rs`) — pins the route PATHS the runner exposes against the SDK's
//!   own `UI_BRIDGE_ROUTES`.
//!
//! What neither of them covers, and what this module exists for, is the layer
//! underneath a path: the **field names, the response shape, the verdict
//! vocabulary and the status-code commitments** the Rust side depends on. A
//! route can stay at the same path while every key inside it is renamed, and
//! until this file nothing in the repo would have gone red.
//!
//! # Why now
//!
//! `@qontinui/ui-bridge` is pinned at `^0.24.0` here and `origin/main` is
//! `0.26.0`. Diffing `UI_BRIDGE_ROUTES` across that span is one addition and
//! zero removals — `POST /control/visibility` — which the runner ALREADY
//! serves from Rust independently (`screenshots::ui_bridge_visibility_handler`).
//! So the path layer is already safe and the shape layer is where the bump can
//! actually hurt: two independent implementations of one contract, drifting
//! field by field with nothing comparing them.
//!
//! # The assertions are LITERALS, on purpose
//!
//! Every expected value below is spelled out as a literal rather than read
//! from the constant the implementation uses. Asserting
//! `STALE_TAB_EVICT_MS == STALE_TAB_EVICT_MS` is a tautology that survives any
//! rename; asserting `60_000` does not. Same reasoning as the comment above
//! `tabs_body_pins_the_wire_contract`, restated because it is the whole design
//! of this file.

#![cfg(test)]

use std::io::Write;
use std::path::PathBuf;

use super::relay::tabs_response_body;
use super::screenshots::{build_visibility_report, VisibilityRequest};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One `discover` element carrying the occlusion fields
/// `build_visibility_report` reads out of `state`.
fn occluded_element(
    id: &str,
    occluded_by: &str,
    occluded_pct: f64,
    text: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "label": "Save",
        "state": {
            "occludedBy": occluded_by,
            "occludedPct": occluded_pct,
            "textContent": text,
        }
    })
}

/// Read a first-party source file out of this crate, for the source-scrape
/// pins below. Scraping is the same technique `manifest_drift_tests` uses, and
/// it is here for the same reason: the thing being pinned (a status code, a
/// default) lives in a handler that takes an `ApiState`, which owns a
/// `tauri::AppHandle` no unit test can build.
fn crate_source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("first-party source {} unreadable: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// POST /control/visibility — REQUEST field names
// ---------------------------------------------------------------------------

/// `VisibilityRequest` carries `#[serde(rename_all = "camelCase")]`, so the
/// wire names are `minRatio` / `includeExpected` — NOT the Rust field names.
/// A dropped or edited `rename_all` would leave the struct compiling, the
/// route answering `200`, and every caller's parameters silently ignored,
/// because both fields are `#[serde(default)]`. That failure mode is invisible
/// from the outside: the sweep just quietly runs at the default ratio.
#[test]
fn visibility_request_deserializes_the_sdk_camelcase_names() {
    let req: VisibilityRequest =
        serde_json::from_str(r#"{"minRatio":0.5,"includeExpected":true}"#).expect("camelCase body");
    assert_eq!(
        req.min_ratio,
        Some(0.5),
        "`minRatio` is the SDK's request key (visibility-report.ts \
         BuildVisibilityReportInput); the Rust field is min_ratio and only \
         rename_all=camelCase bridges the two"
    );
    assert_eq!(
        req.include_expected,
        Some(true),
        "`includeExpected` is the SDK's request key; renaming it here makes \
         every caller's flag a silent no-op rather than an error"
    );

    // The snake_case spellings are NOT accepted. Pinned as an explicit
    // negative because `#[serde(default)]` turns an unrecognised key into
    // `None` rather than a 4xx — so if the rename were ever dropped, a
    // camelCase caller would degrade silently in exactly this shape.
    let snake: VisibilityRequest =
        serde_json::from_str(r#"{"min_ratio":0.5,"include_expected":true}"#)
            .expect("unknown keys are ignored, not rejected");
    assert_eq!(
        snake.min_ratio, None,
        "snake_case must NOT be a second accepted spelling — one wire name per \
         quantity, or the two implementations drift without anything failing"
    );
    assert_eq!(snake.include_expected, None, "same, for include_expected");

    // A bodyless POST is accepted. The SDK's manifest declares this route
    // `bodyRequired: true` (types.ts UI_BRIDGE_ROUTES); the runner is
    // deliberately MORE permissive, and that asymmetry is pinned so nobody
    // "fixes" it into a 4xx that breaks the runner's own callers.
    let empty: VisibilityRequest = serde_json::from_str("{}").expect("empty object");
    assert_eq!(empty.min_ratio, None);
    assert_eq!(empty.include_expected, None);
}

// ---------------------------------------------------------------------------
// POST /control/visibility — RESPONSE field names
// ---------------------------------------------------------------------------

/// One occlusion entry, field for field against the SDK's
/// `VisibilityOcclusionEntry` (`types.ts`). The Rust twin builds this by hand
/// in `occlusion_entry`, so nothing but this test compares the two vocabularies.
#[test]
fn occlusion_entry_pins_the_sdk_entry_field_names() {
    let report = build_visibility_report(
        &[occluded_element("btn-save", "modal-1", 40.0, "Save")],
        0.02,
        false,
    );
    let entry = &report["occlusions"][0];

    // The relation is DIRECTED and the direction is the whole value: swap
    // these two names and the report says the wrong element is unreadable.
    assert_eq!(
        entry["element"], "btn-save",
        "`element` is the COVERED element in the SDK's directed relation"
    );
    assert_eq!(
        entry["occludedBy"], "modal-1",
        "`occludedBy` is the element ON TOP; the pair is meaningless if either \
         name moves"
    );
    assert_eq!(
        entry["ratio"], 0.4,
        "`ratio` is 0..1 while the registry reports `occludedPct` 0..100 — the \
         /100 conversion is part of the contract, and a unit slip here reads \
         as 'everything is occluded'"
    );
    assert_eq!(
        entry["isExpectedOverlay"], false,
        "`isExpectedOverlay` is the SDK's key for 'the occluder is a tracked \
         modal, not a bug'; see the KNOWN DIVERGENCE test below for why the \
         Rust twin always says false"
    );
    assert_eq!(
        entry["hidesText"], true,
        "`hidesText` drives the SDK's sort order — text-hiding occlusions rank \
         above blank ones"
    );
    assert_eq!(
        entry["source"], "hit-test",
        "`source` is the SDK union 'geometry' | 'hit-test'; this arm is always \
         hit-test because the geometric probe lives in ui-bridge-auto and is \
         not consulted here"
    );
    assert_eq!(
        entry["label"], "Save",
        "`label` is optional and camelCase-free"
    );
    assert_eq!(
        entry["text"], "Save",
        "`text` is the covered element's text"
    );

    // Negative pins. serde is not involved here — the entry is hand-built with
    // `json!` — so a snake_case slip would be a plain typo that no compiler
    // catches and every SDK-side consumer reads as a missing field.
    for absent in [
        "occluded_by",
        "is_expected_overlay",
        "hides_text",
        "elementId",
    ] {
        assert!(
            entry.get(absent).is_none(),
            "`{absent}` must never appear: the SDK reads camelCase, and one \
             quantity under two spellings is the drift this file exists to stop"
        );
    }
}

/// The report-level keys, against the SDK's `VisibilityReport` interface.
///
/// This test also records the two directions in which the Rust twin and the
/// SDK 0.26.0 report genuinely DIFFER, so the difference is a documented fact
/// rather than something a reader has to rediscover by diffing two languages.
#[test]
fn visibility_report_pins_the_sdk_report_level_keys() {
    let report = build_visibility_report(
        &[occluded_element("btn-save", "modal-1", 40.0, "Save")],
        0.02,
        true,
    );

    assert!(
        report["occlusions"].is_array(),
        "`occlusions` is the SDK's entry list"
    );
    assert_eq!(
        report["elementCount"], 1,
        "`elementCount` is the size of the swept population, not of the result \
         — it is what separates 'clear' from 'nothing was looked at'"
    );
    assert_eq!(
        report["minRatio"], 0.02,
        "`minRatio` is echoed so a caller can tell which threshold produced \
         this list"
    );
    assert_eq!(
        report["includeExpected"], true,
        "`includeExpected` is echoed; see the KNOWN DIVERGENCE test for what \
         the echo does and does not promise"
    );
    assert_eq!(
        report["verdict"], "occlusions_found",
        "`verdict` is the SDK's three-variant union — see the verdict test"
    );

    // Runner-only, and deliberately OUTSIDE the verdict union so no SDK
    // consumer breaks on an unknown value.
    assert_eq!(
        report["occlusionDataObserved"], true,
        "`occlusionDataObserved` is the runner's own advisory field: a `clear` \
         verdict from a webview whose bundled SDK emits no occlusion data at \
         all is 'nothing OBSERVED', not 'nothing covered'"
    );

    // DIVERGENCE, recorded: the SDK 0.26.0 report carries two more keys that
    // the Rust twin does not emit, because both are outputs of the modal-stack
    // classifier the Rust side has no equivalent of. A consumer written
    // against the SDK interface will find them `undefined` here. Pinned as an
    // explicit absence so the day someone implements the classifier, this test
    // reddens and the pin is updated on purpose rather than by accident.
    for sdk_only in ["expectedOverlayDetection", "expectedOverlaysFiltered"] {
        assert!(
            report.get(sdk_only).is_none(),
            "`{sdk_only}` is an SDK-0.26.0 report key with no Rust twin. If \
             this is now red, the modal-stack classifier landed here — update \
             this pin and the divergence note deliberately"
        );
    }

    for absent in [
        "element_count",
        "min_ratio",
        "include_expected",
        "occlusion_data_observed",
    ] {
        assert!(
            report.get(absent).is_none(),
            "`{absent}`: the report is camelCase on the wire, without exception"
        );
    }
}

/// The verdict vocabulary is a CLOSED three-variant union shared with the SDK.
/// A fourth value, or a renamed one, is an unhandled case in every consumer's
/// switch — which is why all three arms are exercised here rather than one.
#[test]
fn visibility_verdict_union_is_exactly_the_sdk_three() {
    // Empty registry: UNKNOWN, never "clear". This is the absence-is-not-zero
    // distinction, and collapsing it into `clear` is the exact misreading the
    // variant exists to prevent.
    let empty = build_visibility_report(&[], 0.02, false);
    assert_eq!(
        empty["verdict"], "unknown_empty_registry",
        "an empty element list is UNKNOWN — nothing was swept, so nothing can \
         be declared clear"
    );
    assert_eq!(empty["elementCount"], 0);

    // Swept, nothing covered.
    let clear = build_visibility_report(
        &[serde_json::json!({ "id": "btn-save", "state": {} })],
        0.02,
        false,
    );
    assert_eq!(
        clear["verdict"], "clear",
        "a non-empty sweep with no entries is `clear`"
    );
    assert_eq!(
        clear["occlusionDataObserved"], false,
        "no element carried any occlusion field, so `clear` here is 'not \
         observed' — the advisory field is what says so"
    );

    // Swept, something covered.
    let found = build_visibility_report(
        &[occluded_element("btn-save", "modal-1", 40.0, "Save")],
        0.02,
        false,
    );
    assert_eq!(found["verdict"], "occlusions_found");
}

/// `minRatio` is a `<` filter, and its boundary is shared with the SDK
/// (`if (ratio < minRatio) continue`). Off-by-one here means the two
/// implementations disagree about the same page at exactly the threshold a
/// caller tuned.
#[test]
fn visibility_min_ratio_is_a_strict_below_filter() {
    let elements = [occluded_element("btn-save", "modal-1", 40.0, "Save")];

    // ratio == minRatio: KEPT.
    let at = build_visibility_report(&elements, 0.4, false);
    assert_eq!(
        at["occlusions"].as_array().map(Vec::len),
        Some(1),
        "the filter is `ratio < minRatio`, so an occlusion exactly AT the \
         threshold is reported — matching the SDK's own comparison"
    );

    // ratio just above minRatio: DROPPED.
    let above = build_visibility_report(&elements, 0.400_001, false);
    assert_eq!(
        above["occlusions"].as_array().map(Vec::len),
        Some(0),
        "below-threshold hairline overlaps are dropped, not reported at ratio 0"
    );
    assert_eq!(
        above["verdict"], "clear",
        "a fully-filtered list is `clear`, not `unknown_empty_registry` — the \
         registry was not empty"
    );
}

/// KNOWN CROSS-IMPLEMENTATION DIVERGENCE, pinned so it is a recorded fact.
///
/// The Rust twin hardcodes `isExpectedOverlay: false` and ECHOES
/// `includeExpected` without ever acting on it. The SDK fixed exactly this in
/// `fc6838e` (`packages/ui-bridge/src/server/visibility-report.ts`): it
/// classifies each occluder against the snapshot's modal stack and, when
/// `includeExpected` is false, DROPS the expected ones and counts them in
/// `expectedOverlaysFiltered`.
///
/// So on identical input the two implementations can return different lists.
/// That is not a bug this phase fixes; it is a divergence this phase makes
/// visible. **If this test goes red because the Rust side now filters, the fix
/// landed — update the pin, and drop the SDK-only-keys assertion above with it.**
#[test]
fn include_expected_is_parsed_and_echoed_but_never_applied() {
    let elements = [occluded_element("btn-save", "modal-1", 40.0, "Save")];

    let excluded = build_visibility_report(&elements, 0.02, false);
    let included = build_visibility_report(&elements, 0.02, true);

    assert_eq!(
        excluded["occlusions"], included["occlusions"],
        "includeExpected changes NOTHING in the Rust twin — the two lists are \
         identical. The SDK's fc6838e filters here; that divergence is the \
         point of this pin"
    );
    assert_eq!(excluded["includeExpected"], false, "echoed verbatim");
    assert_eq!(included["includeExpected"], true, "echoed verbatim");
    assert_eq!(
        included["occlusions"][0]["isExpectedOverlay"], false,
        "always false on this side: classifying a tracked modal needs an \
         overlay registry the Rust twin has no access to, so it reports \
         honestly rather than guessing"
    );
}

// ---------------------------------------------------------------------------
// POST /control/visibility — the commitments a unit test cannot reach
// ---------------------------------------------------------------------------

/// Status-code and default-value commitments of `ui_bridge_visibility_handler`.
///
/// Scraped from source rather than exercised, because the handler takes an
/// `ApiState` that owns a `tauri::AppHandle` no test can construct. A reader
/// would otherwise have to INFER these from the handler body — which is
/// precisely the inference this phase was asked to remove.
#[test]
fn visibility_handler_pins_its_status_and_default_commitments() {
    let src = crate_source("src/mcp/ui_bridge/screenshots.rs");
    let start = src
        .find("pub async fn ui_bridge_visibility_handler")
        .expect("ui_bridge_visibility_handler not found — has it been renamed?");
    // The handler ends where the next item begins; the routes block follows it.
    let end = src[start..]
        .find("\n// =====")
        .map(|off| start + off)
        .unwrap_or(src.len());
    let body = &src[start..end];

    assert!(
        body.contains("body: Option<Json<VisibilityRequest>>"),
        "the body is OPTIONAL: a bodyless POST /control/visibility must sweep \
         at the defaults, not answer 415/422. The SDK manifest declares this \
         route bodyRequired:true and the runner is deliberately more \
         permissive — pinned so nobody tightens it into a break"
    );
    assert!(
        body.contains("req.min_ratio.unwrap_or(0.02)"),
        "0.02 is the SDK's DEFAULT_VISIBILITY_MIN_RATIO \
         (visibility-report.ts). Two implementations of one route defaulting \
         to different thresholds is a silent disagreement about the same page"
    );
    assert!(
        body.contains("req.include_expected.unwrap_or(false)"),
        "the SDK defaults includeExpected to false; a `true` default here \
         would change which occlusions a default call reports"
    );
    assert!(
        body.contains("StatusCode::INTERNAL_SERVER_ERROR"),
        "a webview that cannot answer `discover` is a 500. That is a server \
         fault, and SDK clients retry 5xx"
    );
    for four_xx in [
        "StatusCode::BAD_REQUEST",
        "StatusCode::NOT_FOUND",
        "StatusCode::UNPROCESSABLE_ENTITY",
        "StatusCode::SERVICE_UNAVAILABLE",
    ] {
        assert!(
            !body.contains(four_xx),
            "{four_xx} must not appear in this handler: the only failure arm \
             is an unreachable webview, and reporting it as caller error makes \
             an SDK client stop retrying a fault it did not cause"
        );
    }
}

// ---------------------------------------------------------------------------
// GET /ui-bridge/tabs — the SDK names the runner must NOT collide with
// ---------------------------------------------------------------------------

/// `tabs_body_pins_the_wire_contract` (relay.rs) already pins what the runner
/// EMITS. This pins what it must never emit: two SDK-side field names for
/// DIFFERENT quantities, either of which would be read by an SDK consumer as
/// its own field.
#[test]
fn tabs_body_must_not_collide_with_sdk_relay_field_names() {
    let body = tabs_response_body(vec![serde_json::json!({ "tabId": "tab-a" })]);

    assert_eq!(
        body["staleTabEvictMs"], 60_000,
        "the eviction bound a poller subtracts from `lastSeen`; asserted as a \
         literal so a retune has to come here rather than sliding through"
    );

    assert!(
        body.get("staleHeartbeatMs").is_none(),
        "staleHeartbeatMs is the SDK's key for another quantity — never ours"
    );
    assert!(
        body.get("tabActiveWindowMs").is_none(),
        "tabActiveWindowMs is the SDK CommandRelay's own field \
         (server/command-relay.ts, default 30_000): the window within which a \
         heartbeat still counts the tab ACTIVE. Ours is an EVICTION deadline, \
         a different quantity on a different clock — emitting this name would \
         hand an SDK consumer 60_000 where it expects 30_000 and read as a \
         tab that never goes stale"
    );
}

/// `GET /ui-bridge/tabs` has NO error arm — an empty registry is a `200`.
///
/// Scraped for the same reason as the visibility handler above (the handler
/// takes an `ApiState`), and what it pins is an ABSENCE. That absence is the
/// load-bearing half of the contract for a poller: `ui-bridge-headless`'s
/// `waitForUiBridgeRegistration` loops on this route while a tab is still
/// connecting, so "no tabs yet" MUST arrive as a 200 whose body says `count: 0`
/// — never as a 404 or a 503. A status-coded empty is indistinguishable from a
/// dead relay, and a poller that cannot tell them apart gives up on a bridge
/// that was merely still coming up.
#[test]
fn tabs_route_is_infallible_and_reports_empty_in_the_body() {
    let src = crate_source("src/mcp/ui_bridge/relay.rs");
    let start = src
        .find("pub async fn ui_bridge_relay_tabs_handler")
        .expect("ui_bridge_relay_tabs_handler not found — has it been renamed?");
    let end = src[start..]
        .find("\n}\n")
        .map(|off| start + off)
        .unwrap_or(src.len());
    let body_src = &src[start..end];

    // Sanity floor first: prove the slice actually contains the handler, so a
    // scrape that silently selected nothing cannot pass the absence check below.
    assert!(
        body_src.contains("ApiResponse::success"),
        "the scraped slice does not contain the handler body — the extraction \
         found nothing, and an absence check over nothing is a vacuous green"
    );
    assert!(
        !body_src.contains("StatusCode::"),
        "GET /ui-bridge/tabs must keep NO error arm: an empty registry is a 200 \
         with count 0, because a poller cannot distinguish a status-coded empty \
         from a relay that is down"
    );
}

// ---------------------------------------------------------------------------
// The vacuous-green guard
// ---------------------------------------------------------------------------

/// Where the SDK's `UI_BRIDGE_ROUTES` is expected to be, relative to
/// `src-tauri/`. The literal is duplicated from
/// `manifest_drift_tests::sdk_manifest_routes_are_exposed_by_runner` ON
/// PURPOSE: if that test's path moves and this one's does not, one of the two
/// goes red, which is the only way a path convention gets noticed at all.
const SDK_TYPES_RELATIVE: &str = "../../ui-bridge/packages/ui-bridge/src/server/types.ts";

/// Override for the resolved path — a worktree does not always sit next to a
/// ui-bridge checkout at the conventional depth.
const SDK_TYPES_PATH_ENV: &str = "QONTINUI_UI_BRIDGE_SDK_TYPES";

/// Declares the absence. Set it and this test records an UNKNOWN instead of
/// failing; leave it unset and an absent sibling is a RED test.
const SDK_ABSENT_DECLARED_ENV: &str = "QONTINUI_UI_BRIDGE_SDK_ABSENT";

/// The SDK checkout must be PRESENT and PARSEABLE, or this test says so.
///
/// # Why this test exists
///
/// `sdk_manifest_routes_are_exposed_by_runner` (`mod.rs`) `return`s when the
/// SDK's `types.ts` is unreadable. That is a GREEN TEST THAT ASSERTED NOTHING:
/// the one gate comparing the SDK's route list to the runner's passes hardest
/// exactly when it has no data. `.qontinui/ci.toml` names the hazard in its own
/// words — *"WITHOUT this checkout the test takes its 'file unreadable, skip'
/// branch and silently passes, so SDK-vs-runner drift would go unflagged — a
/// vacuous green"* — and declares the sibling to prevent it. Both CI lanes
/// check the sibling out, so in CI the skip branch should never be taken; this
/// test is what makes that "should" observable instead of assumed.
///
/// It is a SEPARATE test rather than an edit to the skip branch because a peer
/// holds unpushed commits on `mod.rs`, and this phase must not collide with
/// them. The effect is the same and arguably better: the skip is no longer the
/// only signal, and this signal is impossible to reach accidentally.
///
/// # The three outcomes
///
/// * present and parseable → PASS, having actually checked something;
/// * absent/unparseable with no declaration → **FAIL**, naming the path;
/// * absent with `QONTINUI_UI_BRIDGE_SDK_ABSENT=1` → a recorded UNKNOWN on
///   stderr, and pass. An absence has to be DECLARED. That is the whole
///   difference from the silent skip: unknown must never render as a default.
#[test]
fn sdk_sibling_checkout_is_present_and_parseable() {
    let path = match std::env::var(SDK_TYPES_PATH_ENV) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SDK_TYPES_RELATIVE),
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            let declared = std::env::var(SDK_ABSENT_DECLARED_ENV).unwrap_or_default() == "1";
            assert!(
                declared,
                "UI BRIDGE SDK CHECKOUT MISSING: {} ({e}).\n\
                 This is NOT a skippable condition. \
                 `manifest_drift_tests::sdk_manifest_routes_are_exposed_by_runner` \
                 silently passes without this file, so an absent sibling turns \
                 the only SDK-vs-runner drift gate into a vacuous green.\n\
                 Fix it: check out qontinui/ui-bridge as declared in \
                 .qontinui/ci.toml, or point {SDK_TYPES_PATH_ENV} at an \
                 existing checkout.\n\
                 To DECLARE the absence instead (and record an UNKNOWN rather \
                 than a verdict), set {SDK_ABSENT_DECLARED_ENV}=1.",
                path.display()
            );
            // NOT `eprintln!`. `cargo test` installs an output capture that
            // swallows the `print!` family for a PASSING test, so an
            // `eprintln!` here would record the UNKNOWN into a buffer nobody
            // ever reads — a silent skip wearing a different hat, which is
            // exactly what this test exists to abolish. Writing the process's
            // stderr handle directly bypasses that capture, so the UNKNOWN
            // lands in the run's output whether or not `--nocapture` was given.
            let _ = writeln!(
                std::io::stderr(),
                "UNKNOWN sdk_sibling_checkout_is_present_and_parseable: {} unreadable ({e}); \
                 absence DECLARED via {SDK_ABSENT_DECLARED_ENV}=1. SDK-vs-runner route drift \
                 is UNVERIFIED in this run — not verified-clean.",
                path.display()
            );
            return;
        }
    };

    let array_start = src
        .find("UI_BRIDGE_ROUTES")
        .map(|i| i + "UI_BRIDGE_ROUTES".len())
        .unwrap_or_else(|| {
            panic!(
                "{} is readable but declares no UI_BRIDGE_ROUTES — the SDK's \
                 route manifest has moved or been renamed, and the drift gate \
                 in mod.rs is now scanning a file that cannot answer it",
                path.display()
            )
        });
    let array_body = &src[array_start..];

    let route_count = array_body.matches("path: '").count();
    assert!(
        route_count > 100,
        "only {route_count} route entries found in {} — a parse that finds \
         almost nothing is the same vacuous green as a missing file, reached \
         a different way",
        path.display()
    );

    // The single addition across 0.24.0 -> 0.26.0, and the one route in the
    // diff the runner serves from Rust rather than from the SDK bundle. If it
    // is gone, this repo's independent Rust twin is now answering a path the
    // SDK no longer declares.
    assert!(
        array_body.contains("path: '/control/visibility'"),
        "the SDK no longer declares POST /control/visibility, which the runner \
         serves from its own Rust twin (screenshots::ui_bridge_visibility_handler). \
         Either the route was renamed SDK-side or the runner is now the only \
         implementation — both need a decision, not a green test"
    );
}
