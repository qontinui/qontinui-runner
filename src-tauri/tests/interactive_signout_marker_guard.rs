//! Regression gate: **every explicit pairing path must clear the interactive
//! sign-out marker, and no background credential writer may clear it.**
//!
//! ## Why this test exists
//!
//! `StoredTokens::interactive_signed_out` is what makes the autonomy-preserving
//! logout STICK. "Signed in?" is otherwise derived from credential PRESENCE
//! (`AuthManager::has_local_signed_in_session`), and that logout deliberately
//! keeps the Cognito session — and immediately re-mints a device JWT — so
//! without the marker the next `check_auth_status` would flip the operator
//! straight back to signed-in.
//!
//! That gives the marker two failure modes, in opposite directions, and this
//! gate covers both:
//!
//! 1. **A pairing path that forgets to clear it ⇒ LoginScreen dead-end.**
//!    `finalize_signed_in` is NOT the only way a session is established.
//!    `redeem_pair_code` (allowlisted over the UI-Bridge HTTP surface for
//!    agent-driven pairing) and the CLI `qontinui_profile device pair` both
//!    mint + persist a device JWT, promote to Tier 2 and bring the relay
//!    online. If they leave the marker set, `check_auth_status` short-circuits
//!    to `authenticated:false` FOREVER: the operator is dropped at the
//!    LoginScreen with a perfectly valid pairing, and the Settings pane where a
//!    pair code would be entered is unreachable behind that very gate.
//!
//! 2. **A background writer that clears it ⇒ silent un-logout.** The
//!    device-JWT refresher writes the same credential slots on every cycle
//!    (`store_tokens` / `store_oauth_tokens`). Clearing the marker from there —
//!    or from a shared writer like `pair::persist_pairing` that the refresher
//!    could later reach — would un-logout the operator minutes after they
//!    logged out, which is exactly the auto-logout behaviour this whole change
//!    removes.
//!
//! ## What is checked
//!
//! - Every production file that calls `persist_pairing(` (other than the module
//!   that defines it, `src/pair.rs`) must also call
//!   `clear_interactive_signed_out`.
//! - `pair::persist_pairing`'s own module and the device-JWT refresher must NOT
//!   call `clear_interactive_signed_out`.
//!
//! It is a file-level backstop, not a proof: it cannot tell you the clear
//! happens AFTER the pairing was persisted (that ordering is covered by the
//! unit tests in `auth.rs`'s `signed_in_verdict_tests` and by review). It does
//! make "a new pairing entry point silently forgot the marker" impossible to
//! merge unnoticed.

use std::path::{Path, PathBuf};

/// Defines `persist_pairing` itself; must stay marker-agnostic so the
/// background refresher can never clear the marker through it.
const PERSIST_PAIRING_DEFINITION: &str = "pair.rs";

/// Modules that must NEVER clear the marker (background credential writers).
const MUST_NOT_CLEAR: &[&str] = &["pair.rs", "mcp/device_jwt_refresher.rs"];

const CLEAR_CALL: &str = "clear_interactive_signed_out";
const PERSIST_CALL: &str = "persist_pairing(";

#[test]
fn every_pairing_path_clears_the_interactive_signout_marker() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    files.sort();

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let rel = rel_path(&src, file);
        if rel == PERSIST_PAIRING_DEFINITION {
            continue;
        }
        let text = read(file);
        let production = production_lines(&text);
        if production.iter().any(|l| l.contains(PERSIST_CALL))
            && !production.iter().any(|l| l.contains(CLEAR_CALL))
        {
            offenders.push(rel);
        }
    }

    assert!(
        offenders.is_empty(),
        "\nThese files persist a device pairing but never clear the interactive\n\
         sign-out marker. An operator who used the autonomy-preserving logout and\n\
         then re-paired through one of them is dead-ended at the LoginScreen with a\n\
         valid, relay-online pairing.\n\n\
         Add, AFTER the pairing has been persisted:\n\n    \
         if let Err(e) = AuthManager::new().clear_interactive_signed_out() {{ /* warn */ }}\n\n\
         Offending files:\n{}\n",
        offenders
            .iter()
            .map(|f| format!("    src/{f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn background_credential_writers_never_clear_the_marker() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    for rel in MUST_NOT_CLEAR {
        let path = src.join(rel);
        assert!(
            path.exists(),
            "guarded file src/{rel} no longer exists — update MUST_NOT_CLEAR"
        );
        let text = read(&path);
        if production_lines(&text)
            .iter()
            .any(|l| l.contains(CLEAR_CALL))
        {
            offenders.push((*rel).to_string());
        }
    }

    assert!(
        offenders.is_empty(),
        "\nThese modules are reached by the BACKGROUND device-JWT refresher, which\n\
         rewrites the credential slots on every cycle. Clearing the interactive\n\
         sign-out marker there silently un-logs-out the operator minutes after they\n\
         logged out — the exact auto-logout behaviour the marker exists to prevent.\n\
         Clear it at the explicit, user-initiated pairing call site instead.\n\n\
         Offending files:\n{}\n",
        offenders
            .iter()
            .map(|f| format!("    src/{f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rel_path(src: &Path, file: &Path) -> String {
    file.strip_prefix(src)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Recursively collect `*.rs` files under `dir`.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Lines that are production code: not inside a `#[cfg(test)]` region (tracked
/// by brace depth, same approach as `coord_schema_authorship.rs`) and not a
/// comment.
fn production_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut pending_cfg_test = false;
    let mut cfg_test_active = false;
    let mut cfg_test_depth: i32 = 0;

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        let is_comment = trimmed.starts_with("//") || trimmed.starts_with('*');

        if !is_comment && trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        if !cfg_test_active && !is_comment {
            out.push(raw);
        }

        let opens = raw.matches('{').count() as i32;
        let closes = raw.matches('}').count() as i32;
        if pending_cfg_test && opens > 0 {
            cfg_test_active = true;
            cfg_test_depth = depth;
            pending_cfg_test = false;
        }
        depth += opens - closes;
        if cfg_test_active && depth <= cfg_test_depth {
            cfg_test_active = false;
        }
    }
    out
}
