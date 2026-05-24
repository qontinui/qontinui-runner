//! Anthropic API authentication helper.
//!
//! Anthropic accepts two auth shapes on `api.anthropic.com/v1/messages`:
//!
//! 1. **API key** (`sk-ant-api...`): sent as `x-api-key: <token>`.
//! 2. **OAuth access token** (`sk-ant-oat...`): sent as
//!    `Authorization: Bearer <token>` PLUS `anthropic-beta: oauth-2025-04-20`.
//!
//! The earlier assumption that *"both can be sent via `x-api-key`"* (see the
//! old doc-comment in `claude_api_warm.rs`) is false for the account-usage
//! probe path and similar endpoints — OAuth tokens get a hard 401 unless the
//! Bearer + beta-header shape is used. This module centralises the dispatch so
//! every API call site routes correctly based on the token prefix.

/// Prefix that identifies an Anthropic OAuth access token.
pub(crate) const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

/// Anthropic beta header that must accompany OAuth-Bearer auth on
/// `api.anthropic.com/v1/messages`. Without it, OAuth tokens get a 401.
pub(crate) const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

/// Whether a raw token string is an OAuth access token (vs an API key).
pub(crate) fn is_oauth_token(token: &str) -> bool {
    token.starts_with(OAUTH_TOKEN_PREFIX)
}

/// Apply Anthropic auth headers to an async `reqwest::RequestBuilder`.
///
/// Dispatches by token prefix: `sk-ant-oat*` → `Authorization: Bearer` +
/// `anthropic-beta: oauth-2025-04-20`; anything else → `x-api-key`.
///
/// Callers stack their own `anthropic-version`, `anthropic-beta` (additional
/// betas, like prompt caching), and `content-type` headers — this helper only
/// owns the auth-related headers so it composes cleanly with the rest of the
/// request shape.
pub(crate) fn apply_async(
    builder: reqwest::RequestBuilder,
    token: &str,
) -> reqwest::RequestBuilder {
    if is_oauth_token(token) {
        builder
            .header("Authorization", format!("Bearer {}", token))
            .header("anthropic-beta", OAUTH_BETA_HEADER)
    } else {
        builder.header("x-api-key", token)
    }
}

/// Apply Anthropic auth headers to a blocking `reqwest::blocking::RequestBuilder`.
///
/// Mirror of [`apply_async`] for the blocking client used by the warm + cached
/// Claude API paths.
pub(crate) fn apply_blocking(
    builder: reqwest::blocking::RequestBuilder,
    token: &str,
) -> reqwest::blocking::RequestBuilder {
    if is_oauth_token(token) {
        builder
            .header("Authorization", format!("Bearer {}", token))
            .header("anthropic-beta", OAUTH_BETA_HEADER)
    } else {
        builder.header("x-api-key", token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_token_detection_matches_anthropic_prefix() {
        assert!(is_oauth_token("sk-ant-oat01-abc"));
        assert!(is_oauth_token("sk-ant-oat"));
        assert!(!is_oauth_token("sk-ant-api03-xyz"));
        assert!(!is_oauth_token(""));
        assert!(!is_oauth_token("Bearer sk-ant-oat01-x"));
    }

    /// Async builder dispatch: OAuth path emits Bearer + beta header,
    /// non-OAuth path emits x-api-key. We inspect the constructed `Request`
    /// directly so the test never touches the network.
    #[test]
    fn apply_async_routes_oauth_to_bearer() {
        let client = reqwest::Client::new();
        let req = apply_async(
            client.post("https://api.anthropic.com/v1/messages"),
            "sk-ant-oat01-deadbeef",
        )
        .build()
        .expect("build oauth request");

        let headers = req.headers();
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(auth, "Bearer sk-ant-oat01-deadbeef");

        let beta = headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(beta, OAUTH_BETA_HEADER);

        assert!(
            headers.get("x-api-key").is_none(),
            "OAuth path must NOT also set x-api-key"
        );
    }

    #[test]
    fn apply_async_routes_api_key_to_x_api_key() {
        let client = reqwest::Client::new();
        let req = apply_async(
            client.post("https://api.anthropic.com/v1/messages"),
            "sk-ant-api03-cafebabe",
        )
        .build()
        .expect("build api request");

        let headers = req.headers();
        let key = headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(key, "sk-ant-api03-cafebabe");

        assert!(
            headers.get("authorization").is_none(),
            "API-key path must NOT also set Authorization"
        );
        assert!(
            headers.get("anthropic-beta").is_none(),
            "API-key path must NOT add the OAuth beta header"
        );
    }

    #[test]
    fn apply_blocking_routes_oauth_to_bearer() {
        let client = reqwest::blocking::Client::new();
        let req = apply_blocking(
            client.post("https://api.anthropic.com/v1/messages"),
            "sk-ant-oat01-token",
        )
        .build()
        .expect("build oauth request");

        let headers = req.headers();
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer sk-ant-oat01-token")
        );
        assert_eq!(
            headers.get("anthropic-beta").and_then(|v| v.to_str().ok()),
            Some(OAUTH_BETA_HEADER)
        );
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn apply_blocking_routes_api_key_to_x_api_key() {
        let client = reqwest::blocking::Client::new();
        let req = apply_blocking(
            client.post("https://api.anthropic.com/v1/messages"),
            "sk-ant-api03-key",
        )
        .build()
        .expect("build api request");

        let headers = req.headers();
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("sk-ant-api03-key")
        );
        assert!(headers.get("authorization").is_none());
        assert!(headers.get("anthropic-beta").is_none());
    }
}
