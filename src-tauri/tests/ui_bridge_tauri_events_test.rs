//! Integration tests for UI Bridge tauri-events observation (Phase 2d).
//!
//! The qontinui-runner crate is primarily a binary — its library crate
//! (`qontinui_runner_lib`, see Cargo.toml) only exposes a small surface
//! (`accessibility`, `schema_export`) that does not include `event_system`.
//! Integration tests under `tests/` can therefore only reach crate-internal
//! items via subprocess invocation of the real bin-crate unit tests, or by
//! round-tripping against a live runner HTTP endpoint.
//!
//! These tests take the source-level / subprocess approach so they stay
//! hermetic in `cargo test` without requiring a running runner:
//!
//! 1. `all_event_names_source_lists_accessibility_events` — parse
//!    `src/event_system/types.rs` and assert the `ALL_EVENT_NAMES`
//!    constant literally declares the two new accessibility event names.
//!    This is the same guarantee the in-crate unit test
//!    `every_event_name_is_in_all_event_names` asserts dynamically, but
//!    it is visible to external tooling.
//! 2. `accessibility_app_event_variants_declared` — assert the two new
//!    `AppEvent` variants and their constructor helpers exist in
//!    `src/event_system/types.rs`, including the expected payload field
//!    names (`backend`, `target`, `node_count`, `duration_ms`). This
//!    pins the serde shape the SDK frontend listener depends on.
//! 3. `tauri_event_names_route_registered` — assert
//!    `src/mcp/sdk_client.rs` registers the
//!    `GET /ui-bridge/sdk/tauri-event-names` handler and returns an
//!    `event_names` field sourced from `ALL_EVENT_NAMES`.
//! 4. `accessibility_connect_emits_events` — assert
//!    `src/commands/accessibility.rs` emits the new events via the
//!    `AppEvent::accessibility_connected` / `accessibility_capture_complete`
//!    constructors, wiring the Connect flow through the emitter.
//! 5. `inner_event_system_unit_tests_pass` — run `cargo test -p
//!    qontinui-runner --bin qontinui-runner event_system::types::tests`
//!    in a subprocess to exercise the drift-guard unit test that
//!    validates `ALL_EVENT_NAMES` against every `AppEvent` variant's
//!    `event_name()` arm at runtime. This test is `#[ignore]` by default
//!    because it triggers a (cached) bin-crate compile; run with
//!    `cargo test --test ui_bridge_tauri_events_test -- --ignored`
//!    to include it.
//!
//! The user's end-to-end success criterion — click Connect in the
//! Accessibility Explorer and see `a11y-capture-complete` in a
//! change-buffer drain — is covered separately by the temp-runner
//! manual test phase; that path needs a live Tauri environment which
//! integration tests intentionally avoid.

use std::path::PathBuf;
use std::process::Command;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(rel: &str) -> String {
    let path = crate_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

// ---------------------------------------------------------------------------
// Test 1 — ALL_EVENT_NAMES lists the two new accessibility events.
// ---------------------------------------------------------------------------

#[test]
fn all_event_names_source_lists_accessibility_events() {
    let src = read_source("src/event_system/types.rs");

    // The constant itself must be declared and pub.
    assert!(
        src.contains("pub const ALL_EVENT_NAMES: &[&str]"),
        "ALL_EVENT_NAMES constant declaration missing from event_system/types.rs"
    );

    // Both new accessibility event channel names must be string-literals in
    // the source. The drift-guard unit test inside the module (see test 5)
    // asserts these align with the `event_name()` match arms.
    assert!(
        src.contains("\"a11y-connected\""),
        "ALL_EVENT_NAMES source missing \"a11y-connected\" literal"
    );
    assert!(
        src.contains("\"a11y-capture-complete\""),
        "ALL_EVENT_NAMES source missing \"a11y-capture-complete\" literal"
    );

    // Sanity: the constant's slice body should contain at least 30 distinct
    // quoted event names (we've declared 34 at time of writing). We count
    // lines of the form `    "name",` inside the constant body.
    let body = extract_between(
        &src,
        "pub const ALL_EVENT_NAMES: &[&str] = &[",
        "];",
    )
    .expect("ALL_EVENT_NAMES body not found");
    let quoted: Vec<&str> = body
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('"') && t.ends_with("\",")
        })
        .collect();
    assert!(
        quoted.len() >= 30,
        "ALL_EVENT_NAMES has only {} entries (expected >= 30)",
        quoted.len()
    );

    // No duplicate names.
    let mut sorted: Vec<&&str> = quoted.iter().collect();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        before,
        "ALL_EVENT_NAMES contains duplicate entries"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — AppEvent accessibility variants serialize to the expected shape.
// ---------------------------------------------------------------------------
//
// `AppEvent` uses `#[serde(tag = "event_type", content = "data")]`, so the
// serialized JSON shape is:
//   {
//     "event_type": "AccessibilityConnected",
//     "data": { "backend": "...", "target": "...", "timestamp": N }
//   }
//
// We can't instantiate `AppEvent` here without a library import, but we
// _can_ assert the struct definitions and constructors in source match the
// shape the frontend listener expects. The fallback live-runner path in
// test 5 exercises runtime behavior.

#[test]
fn accessibility_app_event_variants_declared() {
    let src = read_source("src/event_system/types.rs");

    // Variant declarations.
    assert!(
        src.contains("AccessibilityConnected {"),
        "AppEvent::AccessibilityConnected variant missing"
    );
    assert!(
        src.contains("AccessibilityCaptureComplete {"),
        "AppEvent::AccessibilityCaptureComplete variant missing"
    );

    // Payload fields the frontend depends on. Each field must appear inside
    // the variant block, not anywhere in the file.
    let connected_block = extract_between(&src, "AccessibilityConnected {", "},")
        .expect("AccessibilityConnected body");
    assert!(
        connected_block.contains("backend:"),
        "AccessibilityConnected missing `backend` field"
    );
    assert!(
        connected_block.contains("target:"),
        "AccessibilityConnected missing `target` field"
    );
    assert!(
        connected_block.contains("timestamp:"),
        "AccessibilityConnected missing `timestamp` field"
    );

    let capture_block = extract_between(&src, "AccessibilityCaptureComplete {", "},")
        .expect("AccessibilityCaptureComplete body");
    assert!(
        capture_block.contains("backend:"),
        "AccessibilityCaptureComplete missing `backend` field"
    );
    assert!(
        capture_block.contains("node_count:"),
        "AccessibilityCaptureComplete missing `node_count` field"
    );
    assert!(
        capture_block.contains("duration_ms:"),
        "AccessibilityCaptureComplete missing `duration_ms` field"
    );

    // `event_name()` match arms for the two variants.
    assert!(
        src.contains("AppEvent::AccessibilityConnected { .. } => \"a11y-connected\""),
        "event_name() arm for AccessibilityConnected missing"
    );
    assert!(
        src.contains(
            "AppEvent::AccessibilityCaptureComplete { .. } => \"a11y-capture-complete\""
        ),
        "event_name() arm for AccessibilityCaptureComplete missing"
    );

    // Convenience constructors.
    assert!(
        src.contains("pub fn accessibility_connected("),
        "constructor accessibility_connected() missing"
    );
    assert!(
        src.contains("pub fn accessibility_capture_complete("),
        "constructor accessibility_capture_complete() missing"
    );

    // Serde tag configuration — this is what makes the serialized JSON
    // shape predictable for the SDK listener. Changing this shape is a
    // breaking change; the assertion acts as a tripwire.
    assert!(
        src.contains("#[serde(tag = \"event_type\", content = \"data\")]"),
        "AppEvent serde tag/content attribute changed — SDK listener shape will break"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — GET /ui-bridge/sdk/tauri-event-names is registered and returns
// the `event_names` field sourced from ALL_EVENT_NAMES.
// ---------------------------------------------------------------------------

#[test]
fn tauri_event_names_route_registered() {
    let src = read_source("src/mcp/sdk_client.rs");

    assert!(
        src.contains("\"/ui-bridge/sdk/tauri-event-names\""),
        "sdk_client.rs does not register /ui-bridge/sdk/tauri-event-names"
    );
    assert!(
        src.contains("ALL_EVENT_NAMES"),
        "sdk_client.rs does not reference ALL_EVENT_NAMES \
         (handler must source its list from the canonical constant)"
    );
    assert!(
        src.contains("event_names"),
        "sdk_client.rs does not emit an `event_names` response field"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — accessibility.rs emits the two events through the event system.
// ---------------------------------------------------------------------------

#[test]
fn accessibility_connect_emits_events() {
    let src = read_source("src/commands/accessibility.rs");

    assert!(
        src.contains("accessibility_connected"),
        "accessibility.rs does not call AppEvent::accessibility_connected"
    );
    assert!(
        src.contains("accessibility_capture_complete"),
        "accessibility.rs does not call AppEvent::accessibility_capture_complete"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — drift-guard unit test runs green.
//
// Ignored by default: compiling the bin crate for this subprocess call
// is expensive (minutes cold, seconds incremental). Developers run
// `cargo test` on the crate as a whole in CI which already exercises
// this path. Keep this test as a documented entry point:
//
//   cargo test --test ui_bridge_tauri_events_test -- --ignored
// ---------------------------------------------------------------------------

#[test]
#[ignore = "slow: rebuilds bin crate — run with --ignored when you want runtime coverage"]
fn inner_event_system_unit_tests_pass() {
    let status = Command::new(env!("CARGO"))
        .current_dir(crate_root())
        .args([
            "test",
            "--bin",
            "qontinui-runner",
            "--",
            "event_system::types::tests::every_event_name_is_in_all_event_names",
            "--exact",
        ])
        .status()
        .expect("failed to spawn cargo test subprocess");
    assert!(
        status.success(),
        "inner event_system drift-guard unit test failed"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the substring between `start` and the next occurrence of `end`
/// after `start`. Returns `None` if either marker is missing.
fn extract_between<'a>(src: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let s = src.find(start)? + start.len();
    let rest = &src[s..];
    let e = rest.find(end)?;
    Some(&rest[..e])
}
