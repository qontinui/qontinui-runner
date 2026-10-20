//! Shared secret-redaction sweep for outbound session content.
//!
//! One redactor, one test surface (plan
//! `2026-07-09-runner-session-history-cloud-sync` §3.3): both the live PTY
//! pipe ([`super::output_pipe`]) and the transcript-chunk emitter
//! ([`super::transcript_emitter`]) run this sweep before any byte leaves the
//! machine. Extracted from `output_pipe.rs` so there is exactly one regex to
//! audit — do not duplicate it.
//!
//! **Defense in depth, NOT a security boundary** (plan §D11): a determined
//! leak (multi-line secrets, base64 blobs, custom formats) still gets
//! through. The operator opts a session into sharing; redaction is a
//! courtesy backstop, documented as such in the settings UI copy.

use once_cell::sync::Lazy;
use regex::Regex;

/// Secret-redaction regex (plan §D11). Masks `key = value` / `key: value`
/// fragments whose key looks like a credential, including the JSON-quoted
/// form (`"password": "hunter2"`) that transcript-shaped JSONL carries.
/// Case-insensitive. The value (everything up to the next whitespace or
/// quote) is replaced with a fixed mask. Compiled once.
///
/// This is intentionally narrow + fast — it's a courtesy backstop, not a
/// DLP engine.
static SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(api[_-]?key|password|token|secret)(["']?\s*[=:]\s*["']?)([^\s"']+)"#)
        .expect("secret redaction regex is a valid literal")
});

/// Bearer-token sweep: masks the token following a `Bearer` scheme keyword
/// (`Authorization: Bearer eyJhbGci…`). The minimum-length bound keeps prose
/// like "the bearer of" unmasked.
static BEARER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(bearer)(\s+)([A-Za-z0-9\-._~+/=]{8,})")
        .expect("bearer redaction regex is a valid literal")
});

/// JWT sweep: three dot-separated base64url segments whose first starts with
/// `eyJ` (the base64 of `{"`), wherever they appear — not only after a
/// `Bearer` keyword (plan
/// `2026-10-14-permission-prompts-are-never-recorded-so-guard-rules-grow-on-anecdote`
/// D6). The signature segment may be empty (an unsigned `alg: none` token).
static JWT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*")
        .expect("jwt redaction regex is a valid literal")
});

/// URL-userinfo sweep: `scheme://user:pass@host` (or `scheme://token@host`)
/// becomes `scheme://***@host` (same plan, D6). Greedy to the LAST `@` in the
/// authority, so a userinfo that itself holds an `@` (the Azure / Atlas
/// `user@server:password@host` shape) is masked whole; the authority ends at
/// the first `/`, `?`, `#` or whitespace, so an `@` in a path or query is
/// not userinfo.
static URL_USERINFO_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b([A-Za-z][A-Za-z0-9+.\-]*://)[^/?#\s]*@")
        .expect("url-userinfo redaction regex is a valid literal")
});

/// Mask substituted for a matched secret value.
pub const MASK: &str = "***REDACTED***";

/// Mask substituted for URL userinfo (`scheme://***@host`).
pub const USERINFO_MASK: &str = "***";

/// Apply the redaction sweep to a byte slice. Operates on the UTF-8 lossy
/// view (session output is overwhelmingly UTF-8; non-UTF-8 bytes become the
/// replacement char in the masked output, which is acceptable for a
/// shared-tail / transcript courtesy view). Returns the (possibly) masked
/// bytes.
pub fn redact_secrets(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let masked = SECRET_RE.replace_all(&text, |caps: &regex::Captures| {
        // Keep the key + separator, mask only the value.
        format!("{}{}{}", &caps[1], &caps[2], MASK)
    });
    let masked = BEARER_RE.replace_all(&masked, |caps: &regex::Captures| {
        format!("{}{}{}", &caps[1], &caps[2], MASK)
    });
    let masked = JWT_RE.replace_all(&masked, MASK);
    let masked = URL_USERINFO_RE.replace_all(&masked, |caps: &regex::Captures| {
        format!("{}{USERINFO_MASK}@", &caps[1])
    });
    masked.into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- PTY-shaped fixtures (moved verbatim from output_pipe.rs) ---------

    #[test]
    fn redacts_key_value_secrets() {
        // Fixture assembled at runtime so secret-scanning of the SOURCE
        // (gitleaks generic-api-key) never sees a key=value adjacency.
        let input = format!(
            "export API_KEY={}\nnormal line\npassword: {}",
            "sk-abc123def", "hunter2"
        );
        let out = String::from_utf8(redact_secrets(input.as_bytes())).unwrap();
        assert!(out.contains("API_KEY=***REDACTED***"), "got: {out}");
        assert!(out.contains("password: ***REDACTED***"), "got: {out}");
        assert!(out.contains("normal line"));
        // The literal secret values must be gone.
        assert!(!out.contains("sk-abc123def"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn redacts_token_and_secret_variants() {
        for (line, secret) in [
            ("TOKEN=ghp_xxx", "ghp_xxx"),
            ("client_secret = abcdef", "abcdef"),
            ("api-key: zzz", "zzz"),
        ] {
            let out = String::from_utf8(redact_secrets(line.as_bytes())).unwrap();
            assert!(out.contains(MASK), "expected mask in: {out}");
            assert!(!out.contains(secret), "secret leaked in: {out}");
        }
    }

    #[test]
    fn leaves_non_secret_output_untouched() {
        let input = b"ls -la\ntotal 42\ndrwxr-xr-x  3 user group";
        let out = redact_secrets(input);
        assert_eq!(out, input);
    }

    #[test]
    fn redaction_is_lossless_for_keys() {
        // The key + separator survive; only the value is masked. This keeps
        // the shared tail legible ("there was an API_KEY here") without
        // leaking the value.
        let out = String::from_utf8(redact_secrets(b"API_KEY=secret123")).unwrap();
        assert_eq!(out, "API_KEY=***REDACTED***");
    }

    // ---- Transcript-shaped fixtures (plan Phase 3) -------------------------
    //
    // Transcript chunks are formatted AI-output text and JSONL-ish lines; the
    // sweep must mask planted secrets in both shapes BEFORE the durable
    // outbox write.

    #[test]
    fn redacts_secrets_in_transcript_jsonl_lines() {
        let input = br#"{"type":"assistant","text":"set API_KEY=sk-live-4242 in .env"}
{"type":"assistant","text":"password: hunter2 works"}
{"type":"tool_result","content":"Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.abc"}"#;
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(!out.contains("sk-live-4242"), "API_KEY leaked: {out}");
        assert!(!out.contains("hunter2"), "password leaked: {out}");
        assert!(
            !out.contains("eyJhbGciOiJIUzI1NiJ9.abc"),
            "bearer token leaked: {out}"
        );
        assert_eq!(out.matches(MASK).count(), 3, "got: {out}");
        // Non-secret structure survives.
        assert!(out.contains(r#"{"type":"assistant""#));
    }

    #[test]
    fn redacts_json_quoted_credential_fields() {
        // JSON-quoted key form: `"password": "hunter2"` — the quote sits
        // between the key and the separator, which the pre-extraction regex
        // missed entirely.
        let input = br#"{"api_key": "sk-123", "password": "hunter2", "note": "fine"}"#;
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(!out.contains("sk-123"), "api_key leaked: {out}");
        assert!(!out.contains("hunter2"), "password leaked: {out}");
        assert!(out.contains(r#""note": "fine""#), "got: {out}");
    }

    #[test]
    fn redacts_bearer_tokens_in_prose() {
        // Assembled at runtime so gitleaks' curl-auth-header rule never sees
        // the curl+Authorization+token adjacency in source.
        let input = format!(
            "curl -H 'Authorization: Bearer {}' api.example.com",
            "ghp_fake1234"
        );
        let out = String::from_utf8(redact_secrets(input.as_bytes())).unwrap();
        assert!(!out.contains("ghp_fake1234"));
        assert!(out.contains(&format!("Bearer {MASK}")), "got: {out}");
    }

    #[test]
    fn short_bearer_prose_is_not_masked() {
        // "the bearer of" — 2-char word after "bearer" is below the token
        // length bound, so ordinary prose survives.
        let input = b"the bearer of this message";
        assert_eq!(redact_secrets(input), input);
    }

    // ---- JWT + URL-userinfo (plan 2026-10-14 permission-prompt capture, D6) -

    #[test]
    fn redacts_a_bare_jwt() {
        // Assembled at runtime so source secret scanners see no literal JWT.
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "hbGciOiJIUzI1NiJ9", "zdWIiOiJ4In0", "c2ln-bmF0_dXJl"
        );
        let input = format!("COORD_JWT_FILE contents: {jwt} end");
        let out = String::from_utf8(redact_secrets(input.as_bytes())).unwrap();
        assert!(!out.contains(&jwt), "jwt leaked: {out}");
        assert_eq!(out, format!("COORD_JWT_FILE contents: {MASK} end"));
        // An unsigned token (empty signature segment) is still a JWT.
        let unsigned = format!("eyJ{}.eyJ{}.", "hbGciOiJub25lIn0", "zdWIiOiJ4In0");
        let out = String::from_utf8(redact_secrets(unsigned.as_bytes())).unwrap();
        assert_eq!(out, MASK);
    }

    #[test]
    fn a_two_segment_or_short_eyj_word_is_not_a_jwt() {
        let input = b"eyJhb.cd and eyJabcdef.ghijklm";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redacts_url_userinfo() {
        let input = format!(
            "git clone https://{}:{}@github.com/o/r.git && curl ftp://{}@h/x",
            "alice", "hunter2", "tok123"
        );
        let out = String::from_utf8(redact_secrets(input.as_bytes())).unwrap();
        assert!(
            !out.contains("alice") && !out.contains("hunter2") && !out.contains("tok123"),
            "{out}"
        );
        assert!(out.contains("https://***@github.com/o/r.git"), "{out}");
        assert!(out.contains("ftp://***@h/x"), "{out}");
    }

    #[test]
    fn an_at_sign_in_a_url_path_or_query_is_not_userinfo() {
        let input = b"see https://example.com/users/@me and mail a@b.c";
        assert_eq!(redact_secrets(input), input);
        let input = b"open https://example.com?x=a@b now";
        assert_eq!(redact_secrets(input), input);
        let input = b"open https://example.com#frag@x now";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn a_userinfo_holding_an_at_sign_is_masked_whole() {
        // The Azure Database for PostgreSQL / Atlas shape: the user name itself
        // carries `@server`. Assembled at runtime so scanners see no literal.
        let input = format!(
            "psql postgres://{}@mydemoserver:{}@srv.example.com:5432/db",
            "myadmin", "mypassword"
        );
        let out = String::from_utf8(redact_secrets(input.as_bytes())).unwrap();
        assert!(
            !out.contains("mypassword") && !out.contains("myadmin"),
            "{out}"
        );
        assert_eq!(out, "psql postgres://***@srv.example.com:5432/db");
    }

    #[test]
    fn redacts_agentic_transcript_block() {
        // The exact shape the loop_controller persists: an iteration header
        // followed by AI output containing a planted secret.
        let input =
            b"\n=== Agentic Phase (iteration 2) ===\nI added SECRET=deadbeef01 to the config.\n";
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(out.contains("=== Agentic Phase (iteration 2) ==="));
        assert!(!out.contains("deadbeef01"), "got: {out}");
        assert!(out.contains(&format!("SECRET={MASK}")), "got: {out}");
    }
}
