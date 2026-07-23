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

/// The ONLY production files allowed to reference `clear_interactive_signed_out`
/// — an ALLOWLIST, not a blocklist. Two are the definition sites (the
/// `SecureStorage` method and its `AuthManager` wrapper); the other three are
/// the explicit, user/agent-initiated pairing call sites. Any OTHER production
/// file that clears the marker is a regression.
///
/// This is inverted from the old static blocklist (`["pair.rs",
/// "mcp/device_jwt_refresher.rs"]`) on purpose: a blocklist only catches the two
/// modules someone thought to name, so a NEW background-refresher helper in a
/// fresh module could clear the marker undetected. A subset-of-allowlist check
/// fails closed — any new caller must be added here (and justified) before it
/// compiles green.
const CLEAR_ALLOWLIST: &[&str] = &[
    // Definition sites (plumbing, not a background-reachable clear).
    "secure_storage.rs",
    "auth.rs",
    // Explicit, user/agent-initiated credential acquisition (the only callers).
    "commands/auth.rs",
    "commands/web_integration.rs",
    "bin/qontinui_profile.rs",
];

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
fn only_allowlisted_sites_clear_the_interactive_signout_marker() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");

    // The allowlist must not rot: every named file must still exist.
    for rel in CLEAR_ALLOWLIST {
        assert!(
            src.join(rel).exists(),
            "allowlisted file src/{rel} no longer exists — update CLEAR_ALLOWLIST"
        );
    }
    let allow: std::collections::BTreeSet<&str> = CLEAR_ALLOWLIST.iter().copied().collect();

    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    files.sort();

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let rel = rel_path(&src, file);
        let text = read(file);
        if production_lines(&text)
            .iter()
            .any(|l| l.contains(CLEAR_CALL))
            && !allow.contains(rel.as_str())
        {
            offenders.push(rel);
        }
    }

    assert!(
        offenders.is_empty(),
        "\nThese files clear the interactive sign-out marker but are NOT on the\n\
         allowlist. Clearing the marker anywhere the BACKGROUND device-JWT refresher\n\
         can reach silently un-logs-out the operator minutes after they logged out —\n\
         the exact auto-logout behaviour the marker exists to prevent. The marker may\n\
         be cleared ONLY at an explicit, user/agent-initiated credential acquisition\n\
         (Cognito sign-in, pair-code redeem, CLI `device pair`).\n\n\
         If this is a genuinely explicit new call site, add it to CLEAR_ALLOWLIST in\n\
         this test WITH a justifying comment. If it is a background writer, remove the\n\
         clear.\n\n\
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
/// by brace depth, same approach as `coord_schema_authorship.rs`) and with
/// comments stripped.
///
/// Comment stripping matters because both directions match with `.contains()`:
/// a trailing `// … persist_pairing( …` or `// … clear_interactive_signed_out`
/// would otherwise false-satisfy the check (a comment mentioning the call reads
/// as making the call). Full-line comments (`//`-leading and `*`-leading
/// block-comment continuations) are dropped entirely; inline `/* … */` and
/// trailing `//` comments are stripped from otherwise-code lines.
fn production_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut pending_cfg_test = false;
    let mut cfg_test_active = false;
    let mut cfg_test_depth: i32 = 0;

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        // Whole-line comment: a `//` line/doc comment, or a `*`-leading
        // continuation line of a `/* … */` block comment (incl. the closing
        // `*/`).
        let is_comment_line = trimmed.starts_with("//") || trimmed.starts_with('*');

        if !is_comment_line && trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }

        if !cfg_test_active && !is_comment_line {
            out.push(strip_inline_comments(raw));
        }

        // Brace accounting stays on the raw line (unchanged from before), so
        // `#[cfg(test)]` region detection is not perturbed by comment stripping.
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

/// Remove single-line `/* … */` block comments and a trailing `// …` line
/// comment from a code line. Naive (does not account for `//` or `/*` inside
/// string literals) — sufficient for this file-level heuristic guard, and the
/// two matched tokens (`persist_pairing(`, `clear_interactive_signed_out`) never
/// appear inside a string literal in this codebase.
fn strip_inline_comments(line: &str) -> String {
    let mut s = line.to_string();
    // Strip single-line block comments first, so a `/* // */`-style construct
    // doesn't leave a spurious `//` behind.
    while let (Some(a), Some(b)) = (s.find("/*"), s.find("*/")) {
        if b >= a + 2 {
            s.replace_range(a..b + 2, " ");
        } else {
            break;
        }
    }
    if let Some(idx) = s.find("//") {
        s.truncate(idx);
    }
    s
}
