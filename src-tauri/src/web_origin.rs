//! The qontinui-web FRONTEND origin, as distinct from its API origin.
//!
//! Production is a **split** deployment: `https://api.qontinui.io` serves only
//! the FastAPI routes (`/api/v1/*`), while user-facing Next.js pages such as
//! `/login` and `/connect-runner` are served by the frontend at
//! `https://qontinui.io`. The API host answers those page paths with
//! `404 {"error":"NOT_FOUND"}`, so anything that opens a PAGE in the user's
//! browser must target the frontend origin, never the API base.
//!
//! Lives in the lib crate (not the runner binary's `api_config`) so both the
//! runner GUI and lib-crate bin targets like `qontinui_profile` reach ONE copy
//! of the derivation; `api_config` re-exports it.

/// Canonical Qontinui production web-frontend FQDN (the Next.js app).
///
/// The `api.` host has no login page and no `/connect-runner`, so any UI that
/// sends the user to a web page must target this origin, not
/// [`crate::profiles::PROD_API_BASE_URL`]. See [`derive_web_base_url`].
pub const PROD_WEB_BASE_URL: &str = "https://qontinui.io";

/// Environment variable naming the web-frontend origin explicitly, for the
/// deployments [`derive_web_base_url`] cannot infer — chiefly local dev, where
/// the backend listens on `:8000` and the Next.js frontend on `:3001`.
pub const WEB_FRONTEND_BASE_ENV: &str = "QONTINUI_WEB_FRONTEND_BASE";

/// Derive the user-facing web-frontend origin from an API `backend_url`.
///
/// Production splits the API (`https://api.qontinui.io`) from the web frontend
/// (`https://qontinui.io`) — the only difference is the leading `api.` host
/// label. When the backend host begins with `api.`, this strips that single
/// label (preserving scheme and port) to yield the frontend origin. For any
/// other host — localhost dev, a bare IP, or a unified deployment where one
/// origin serves both — the backend URL is returned unchanged (the
/// long-standing "same origin serves the SPA" fallback). Callers with their
/// own frontend override (the `web_base_url` setting, or
/// `$QONTINUI_WEB_FRONTEND_BASE` via [`resolve_web_frontend_base`]) apply it
/// before falling back to this derivation.
///
/// The returned value has no trailing slash and no path.
pub fn derive_web_base_url(backend_url: &str) -> String {
    let trimmed = backend_url.trim().trim_end_matches('/');
    // Canonical mapping: the production API host maps to the production web
    // origin. The general `api.`-stripping below also yields this, but pinning
    // it keeps the two constants coupled even if the web FQDN ever diverges
    // from a simple label strip.
    if trimmed == crate::profiles::PROD_API_BASE_URL {
        return PROD_WEB_BASE_URL.to_string();
    }
    let (scheme, rest) = match trimmed.split_once("://") {
        Some(parts) => parts,
        None => return trimmed.to_string(),
    };
    // Authority is everything up to the first '/'; a ':' splits off the port.
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    match host.strip_prefix("api.") {
        // Only rewrite when an `api.` label was actually present.
        Some(frontend_host) => match port {
            Some(p) => format!("{}://{}:{}", scheme, frontend_host, p),
            None => format!("{}://{}", scheme, frontend_host),
        },
        None => trimmed.to_string(),
    }
}

/// Which rung of [`resolve_web_frontend_base`] produced the frontend origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebFrontendSource {
    /// `$QONTINUI_WEB_FRONTEND_BASE` was set to a non-blank value.
    EnvOverride,
    /// Derived from the web API base by [`derive_web_base_url`]. Carries the
    /// API base it was derived from, so a wrong origin can be traced back to
    /// the API rung that chose it.
    DerivedFromApiBase { api_base: String },
}

/// Resolve the web-frontend origin a browser page should be opened on.
///
/// 1. `env` (`$QONTINUI_WEB_FRONTEND_BASE`) — an explicit override; blank or
///    slash-only counts as unset.
/// 2. Otherwise [`derive_web_base_url`] applied to `api_base` — the already
///    resolved API base, so every existing API-base knob (`$QONTINUI_WEB_BASE`,
///    a profile `api_url`, the production default) still steers the page too:
///    `https://api.qontinui.io` → `https://qontinui.io`.
///
/// Pure: the caller reads the environment and owns the output surface.
pub fn resolve_web_frontend_base(env: Option<&str>, api_base: &str) -> (String, WebFrontendSource) {
    if let Some(explicit) = env
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
    {
        return (explicit, WebFrontendSource::EnvOverride);
    }
    (
        derive_web_base_url(api_base),
        WebFrontendSource::DerivedFromApiBase {
            api_base: api_base.trim().trim_end_matches('/').to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_web_base_url_strips_prod_api_label() {
        // The headline case: api.qontinui.io (no login page) → qontinui.io.
        // Couples the two production constants through the pinned mapping.
        assert_eq!(
            derive_web_base_url(crate::profiles::PROD_API_BASE_URL),
            PROD_WEB_BASE_URL
        );
        assert_eq!(
            derive_web_base_url("https://api.qontinui.io"),
            "https://qontinui.io"
        );
        assert_eq!(
            derive_web_base_url("https://api.qontinui.io/"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_non_api_hosts() {
        // Localhost dev + unified deployments have no `api.` label to strip,
        // so the backend origin is returned unchanged (old fallback behavior).
        assert_eq!(
            derive_web_base_url("http://localhost:8000"),
            "http://localhost:8000"
        );
        assert_eq!(
            derive_web_base_url("http://127.0.0.1:8000/"),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            derive_web_base_url("https://qontinui.io"),
            "https://qontinui.io"
        );
    }

    #[test]
    fn derive_web_base_url_preserves_scheme_and_port() {
        assert_eq!(
            derive_web_base_url("http://api.example.test:8080"),
            "http://example.test:8080"
        );
        // A host that merely starts with the letters "api" but has no `api.`
        // label must NOT be rewritten.
        assert_eq!(
            derive_web_base_url("https://apiserver.example.test"),
            "https://apiserver.example.test"
        );
    }

    #[test]
    fn frontend_base_defaults_to_the_frontend_host_not_the_api_host() {
        // The 2026-10-18 defect: `device pair --browser` opened
        // https://api.qontinui.io/connect-runner, which 404s. With nothing
        // configured the page must open on the frontend origin.
        assert_eq!(
            resolve_web_frontend_base(None, "https://api.qontinui.io"),
            (
                "https://qontinui.io".to_string(),
                WebFrontendSource::DerivedFromApiBase {
                    api_base: "https://api.qontinui.io".to_string()
                }
            )
        );
    }

    #[test]
    fn frontend_base_env_override_wins_and_is_normalized() {
        assert_eq!(
            resolve_web_frontend_base(Some(" http://localhost:3001/ "), "http://127.0.0.1:8000"),
            (
                "http://localhost:3001".to_string(),
                WebFrontendSource::EnvOverride
            )
        );
    }

    #[test]
    fn frontend_base_ignores_a_blank_or_slash_only_override() {
        for blank in ["", "   ", "/", " // "] {
            assert_eq!(
                resolve_web_frontend_base(Some(blank), "https://api.staging.example/"),
                (
                    "https://staging.example".to_string(),
                    WebFrontendSource::DerivedFromApiBase {
                        api_base: "https://api.staging.example".to_string()
                    }
                ),
                "override {blank:?} must count as unset"
            );
        }
    }

    #[test]
    fn frontend_base_keeps_a_unified_api_origin() {
        // A unified deployment (one origin serves pages and API) keeps working:
        // there is no `api.` label to strip.
        assert_eq!(
            resolve_web_frontend_base(None, "http://127.0.0.1:8000").0,
            "http://127.0.0.1:8000"
        );
    }
}
