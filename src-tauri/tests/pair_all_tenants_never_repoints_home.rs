//! Regression gate: **"Connect all my workspaces" never reaches the two
//! pairing doors that move this device's home tenant or rotate its machine
//! key.**
//!
//! ## Why this test exists
//!
//! Plan
//! `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`
//! Phase 3. There are two other in-app pairing paths, and both are wrong for a
//! multi-tenant heal:
//!
//! - qontinui-web `POST /api/v1/devices/pair-cli` (`pair_with_auth_token`,
//!   and the Cognito sign-in's `finalize_signed_in` leg) goes through coord's
//!   `record_pairing`, which REWRITES `coord.devices.tenant_id` — pairing N
//!   tenants one after another leaves home on whichever came last — and web
//!   rotates the device's single `dmk_` to it;
//! - `machine-credential/exchange` mints for the HOME tenant only.
//!
//! The attended multi-tenant flow exists precisely so neither happens. A
//! refactor that "reuses" one of them from `pair_all_tenants` would compile,
//! pass every unit test, and silently re-point home. This walks the source of
//! every function on the flow and refuses any mention of those doors.
//!
//! It is a code-line backstop in the style of
//! `interactive_signout_marker_guard.rs`, not a call-graph proof: the closure
//! of functions below is named explicitly, and the test fails if any of them
//! can no longer be found, so a rename cannot quietly empty the scan.

use std::path::{Path, PathBuf};

/// Tokens that name a home-repointing or key-rotating door, or a function
/// that reaches one.
const FORBIDDEN: &[&str] = &[
    "pair-cli",
    "pair_cli",
    "pair_with_auth_token",
    "machine-credential",
    "machine_credential",
    "exchange_device_machine_key",
    "finalize_signed_in",
    "cognito_sign_in",
    "pkce_login",
    "pair_via_browser(",
];

/// `(file under src/, fn name)` — every function the multi-tenant flow runs.
const FLOW: &[(&str, &str)] = &[
    ("commands/web_integration.rs", "pair_all_tenants"),
    ("commands/web_integration.rs", "default_pair_selection"),
    ("commands/web_integration.rs", "finish_explicit_pairing"),
    ("pair.rs", "pair_via_browser_multi"),
    ("pair.rs", "browser_pair_round_trip"),
    ("pair.rs", "pair_start_multi_request_body"),
    ("pair.rs", "pair_collect"),
    ("pair.rs", "pair_collect_request_body"),
    ("pair.rs", "parse_callback"),
    ("pair.rs", "persist_collected_pairings"),
    ("pair.rs", "persist_collected_pairings_with"),
    ("pair.rs", "persist_pairing_with"),
];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Drop `//` comments (line and doc) so prose naming a forbidden door — this
/// flow's own docs explain why it avoids `pair-cli` — cannot fail the scan.
fn code_only(src: &str) -> String {
    src.lines()
        .map(|l| match l.split_once("//") {
            // Keep `//` inside a string literal such as "http://…".
            Some((code, _)) if !code.contains('"') => code,
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of `fn <name>` (from its signature to the matching close brace).
fn fn_body(src: &str, name: &str) -> Option<String> {
    let needle = format!("fn {name}(");
    let needle_generic = format!("fn {name}<");
    let start = src.find(&needle).or_else(|| src.find(&needle_generic))?;
    // `find` returns char boundaries, and `{`/`}` are one byte, so every
    // `get` below lands on a boundary; `get` keeps it panic-free regardless.
    let tail = src.get(start..)?;
    let open = tail.find('{')?;
    let mut depth = 0usize;
    for (i, c) in tail.get(open..)?.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return tail.get(..=open + i).map(str::to_string);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn the_multi_tenant_pair_flow_never_names_a_home_repointing_door() {
    let mut violations = Vec::new();
    for (file, name) in FLOW {
        let path = src_root().join(file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let code = code_only(&src);
        let body = fn_body(&code, name)
            .unwrap_or_else(|| panic!("fn {name} not found in {file} — update FLOW"));
        for token in FORBIDDEN {
            if body.contains(token) {
                violations.push(format!("{file}::{name} mentions `{token}`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "the attended multi-tenant pair must not reach web pair-cli or \
         machine-credential/exchange:\n  {}",
        violations.join("\n  ")
    );
}

/// The flow sends no `home_tenant_id` from the in-app command: coord moves the
/// home pointer only when one is named, and "Connect all my workspaces" never
/// names one.
#[test]
fn pair_all_tenants_never_names_a_home_tenant() {
    let src = std::fs::read_to_string(src_root().join("commands/web_integration.rs")).unwrap();
    let body = fn_body(&code_only(&src), "pair_all_tenants").expect("pair_all_tenants");
    assert!(
        body.contains("pair_via_browser_multi(&coord_base, &for_blocking, None)"),
        "pair_all_tenants must call pair_via_browser_multi with home_tenant_id = None"
    );
}

/// The scanner itself: a forbidden token in CODE is caught, in a comment is not.
#[test]
fn the_scanner_sees_code_and_ignores_prose() {
    let src = "fn f() {\n    // pair-cli is avoided\n    let u = \"https://x/pair-cli\";\n}\n";
    let body = fn_body(&code_only(src), "f").unwrap();
    assert!(body.contains("pair-cli"), "a URL literal is code");
    let prose = "fn g() {\n    // pair-cli is avoided\n    let u = 1;\n}\n";
    assert!(!fn_body(&code_only(prose), "g")
        .unwrap()
        .contains("pair-cli"));
}
