//! Exposure guard: only allow-listed modules touch the operator's OAuth slot.
//!
//! The OAuth slot holds the signed-in operator's Cognito tokens
//! (`get_oauth_access_token`, `get_oauth_id_token`, `get_oauth_refresh_token`),
//! and `cognito_bearer` / `ensure_fresh_cognito_bearer` / `refresh_cognito_bearer`
//! are the doors that hand that material to a caller. A UI-Bridge-reachable path
//! (an `/ui-bridge/invoke` arm, a Tauri-proxy arm, the observe tier, an HTTP
//! route) must never return it. This test is the source scan that keeps a NEW
//! reader from appearing unreviewed: any `src-tauri/src/**/*.rs` file outside
//! the allow-list that mentions one of the tokens in code fails the test.
//!
//! Plan `2026-10-28-one-session-lifecycle-authority`, Phase 16 (b) / R0b-2a.
//! The operator twin of Phase 16 (c) enters here as one new allow-listed reader.
//!
//! Lexical only: whole-word match on non-comment lines. A green run proves no
//! scanned code line outside the list names the tokens; the printed counts and
//! the floors below distinguish "scanned nothing" from "found nothing".

use std::fs;
use std::path::{Path, PathBuf};

const TOKENS: &[&str] = &[
    "get_oauth_access_token",
    "get_oauth_id_token",
    "get_oauth_refresh_token",
    "cognito_bearer",
    "ensure_fresh_cognito_bearer",
    "refresh_cognito_bearer",
];

/// Paths relative to `src-tauri/src`. `auth.rs` / `secure_storage.rs` define and
/// store the slot (not readers); the rest are today's internal readers.
const ALLOWED: &[&str] = &[
    "auth.rs",
    "secure_storage.rs",
    "commands/auth.rs",
    "commands/setup_wizard.rs",
    "commands/new_project.rs",
    "mcp/device_jwt_refresher.rs",
    "mcp/sdk_client.rs",
    "mcp/relay_routable.rs",
    "mcp/ui_bridge/gated_flow.rs",
];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let p = entry.expect("entry").path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The first token named as a whole word on a non-comment line, if any.
fn hit(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("//") {
        return None;
    }
    let code = line.split("//").next().unwrap_or(line);
    let bytes = code.as_bytes();
    for tok in TOKENS {
        for (s, _) in code.match_indices(tok) {
            let e = s + tok.len();
            let before = s == 0 || !is_ident(bytes[s - 1]);
            let after = e >= bytes.len() || !is_ident(bytes[e]);
            if before && after {
                return Some(tok);
            }
        }
    }
    None
}

#[test]
fn only_allow_listed_modules_touch_the_oauth_slot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root, &mut files);
    assert!(files.len() > 100, "scanned only {} files", files.len());

    let mut violations = Vec::new();
    let mut allowed_hits = 0usize;
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let allowed = ALLOWED.contains(&rel.as_str());
        let text = fs::read_to_string(f).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            if let Some(tok) = hit(line) {
                if allowed {
                    allowed_hits += 1;
                } else {
                    violations.push(format!("{rel}:{} names `{tok}`", n + 1));
                }
            }
        }
    }
    println!("scanned={} allowed_hits={allowed_hits}", files.len());
    assert!(
        allowed_hits > 0,
        "allow-listed readers matched nothing: scan is vacuous"
    );
    assert!(
        violations.is_empty(),
        "OAuth-slot material named outside the allow-list (add a reviewed reader to ALLOWED only \
         if it can never be returned over a UI-Bridge-reachable path):\n{}",
        violations.join("\n")
    );
}

#[test]
fn matcher_distinguishes_words_and_comments() {
    assert!(hit("let t = get_oauth_id_token();").is_some());
    assert!(hit("    // get_oauth_id_token").is_none());
    assert!(hit("my_cognito_bearer_x()").is_none());
    assert!(hit("x(); // cognito_bearer").is_none());
}
