//! Browser-login token callback (Phase 3G-web-polish).
//!
//! Exposes `GET /auth/runner-token-callback` — the landing URL the
//! qontinui-web `/connect-runner` page redirects the user's browser to
//! after they confirm token creation. The handler pulls the pending flow
//! by its `state` from [`crate::server_mode::TokenFlowStore`], captures
//! the `token` from the query string, applies the new
//! `WebIntegrationSettings` via
//! [`crate::commands::web_integration::apply_web_integration_settings`]
//! (which triggers re-registration), and returns an HTML page the
//! browser displays.
//!
//! # Security
//!
//! - The endpoint is **unauthenticated** by design: a browser redirect
//!   carries no cookies or auth header to this localhost URL. What
//!   guards it is the one-shot pending `state` — without a valid pending
//!   flow, every hit returns 404.
//! - `consume()` clears the slot on match, so a replay from the browser's
//!   forward cache returns 404.
//! - Token values never appear in logs.
//! - All responses are HTML so the user's browser shows useful feedback
//!   instead of a raw JSON error.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::Deserialize;
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::settings::WebIntegrationSettings;

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route(
        "/auth/runner-token-callback",
        get(runner_token_callback_handler),
    )
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub state: String,
    pub token: String,
    /// Token record ID returned by `POST /api/v1/runners/tokens`.
    /// Not strictly required — we just log it so operators can trace the
    /// created token back through the web admin. Defaulting to `None`
    /// keeps us forward-compatible if the web side stops sending it.
    #[serde(default)]
    pub token_id: Option<String>,
}

/// Render an HTML page with the given status and body. Always uses
/// `text/html; charset=utf-8` so browsers render instead of downloading.
fn html_response(status: StatusCode, body: String) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    (status, headers, body)
}

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html>
  <head>
    <title>Runner Connected</title>
    <meta charset="utf-8" />
    <style>
      body {
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica,
          Arial, sans-serif;
        max-width: 480px;
        margin: 80px auto;
        text-align: center;
        color: #1f2937;
      }
      h1 {
        color: #16a34a;
        font-size: 28px;
        margin-bottom: 12px;
      }
      p {
        color: #4b5563;
        font-size: 15px;
        line-height: 1.5;
      }
      .runner-name {
        display: inline-block;
        padding: 2px 8px;
        margin: 0 2px;
        border-radius: 4px;
        background: #f3f4f6;
        font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas,
          "Liberation Mono", "Courier New", monospace;
        font-size: 13px;
      }
      .muted {
        color: #9ca3af;
        font-size: 13px;
        margin-top: 32px;
      }
    </style>
  </head>
  <body>
    <h1>&#10003; Runner connected</h1>
    <p>
      The runner <span class="runner-name">__RUNNER_NAME__</span> is now
      registered with qontinui-web.
    </p>
    <p>You can close this tab.</p>
    <p class="muted">This window will close automatically in 3 seconds&#8230;</p>
    <script>
      setTimeout(function () {
        window.close();
      }, 3000);
    </script>
  </body>
</html>
"#;

const NOT_FOUND_HTML: &str = r#"<!DOCTYPE html>
<html>
  <head>
    <title>Connect Runner &mdash; expired</title>
    <meta charset="utf-8" />
    <style>
      body {
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica,
          Arial, sans-serif;
        max-width: 480px;
        margin: 80px auto;
        text-align: center;
        color: #1f2937;
      }
      h1 {
        color: #b45309;
        font-size: 24px;
      }
      p {
        color: #4b5563;
        font-size: 15px;
        line-height: 1.5;
      }
    </style>
  </head>
  <body>
    <h1>Token flow expired or invalid</h1>
    <p>
      The pending login flow is no longer valid. Please retry from the runner's
      Settings &rarr; Web Integration panel.
    </p>
  </body>
</html>
"#;

fn error_html(message: &str) -> String {
    // Very small template — we intentionally don't pull in a template
    // engine for a one-off error page. `message` is server-built, not
    // user-supplied, so raw inlining is safe.
    format!(
        r#"<!DOCTYPE html>
<html>
  <head>
    <title>Connect Runner &mdash; error</title>
    <meta charset="utf-8" />
    <style>
      body {{
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica,
          Arial, sans-serif;
        max-width: 520px;
        margin: 80px auto;
        text-align: center;
        color: #1f2937;
      }}
      h1 {{
        color: #b91c1c;
        font-size: 24px;
      }}
      pre {{
        text-align: left;
        background: #f3f4f6;
        padding: 12px 16px;
        border-radius: 6px;
        font-size: 13px;
        overflow-x: auto;
      }}
    </style>
  </head>
  <body>
    <h1>Could not save runner token</h1>
    <p>{}</p>
    <p>Please retry from the runner's Settings.</p>
  </body>
</html>
"#,
        // Minimal HTML escaping — this message comes from our own code,
        // but be defensive in case a setting save error ever includes an
        // attacker-controlled substring.
        html_escape(message)
    )
}

/// Minimal HTML escaping — only the five characters that matter inside a
/// text node. Avoids pulling in a full HTML escape crate for this tiny use.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Handler for `GET /auth/runner-token-callback`.
async fn runner_token_callback_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<CallbackQuery>,
) -> axum::response::Response {
    // Validate pending flow. `consume` is one-shot: a match clears the
    // slot so a browser-cache replay returns 404.
    let pending = match state.app_state.token_flow.consume(&params.state) {
        Some(flow) => flow,
        None => {
            warn!(
                "runner_token_callback: no matching pending flow (state prefix={})",
                params.state.chars().take(8).collect::<String>()
            );
            return html_response(StatusCode::NOT_FOUND, NOT_FOUND_HTML.to_string())
                .into_response();
        }
    };

    // Guard against an empty token — shouldn't happen but the web side
    // could misbehave. We treat it as an error rather than saving a blank
    // token (which would clear the existing integration).
    if params.token.trim().is_empty() {
        warn!("runner_token_callback: received empty token value in callback");
        return html_response(
            StatusCode::BAD_REQUEST,
            error_html("The web backend returned an empty token."),
        )
        .into_response();
    }

    // Apply the new settings. We start from the currently-persisted
    // settings so fields the user configured out-of-band (e.g., enabled
    // toggle toggled manually elsewhere) are preserved, then overlay the
    // backend_url from the pending flow + the new token + enabled=true.
    let mut new_settings = crate::settings::load_settings().web_integration.clone();
    new_settings.backend_url = pending.backend_url.clone();
    new_settings.runner_token = params.token.clone();
    new_settings.enabled = true;

    // `apply_web_integration_settings` persists + hot-reloads + emits.
    let runner_name_for_display = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "runner".to_string());

    match apply_integration(&state, new_settings).await {
        Ok(()) => {
            info!(
                "runner_token_callback: applied new runner token (token_id={:?}, backend={})",
                params.token_id, pending.backend_url
            );

            // Runner-tier-decoupling Phase 5: promote runner to Tier 2 on
            // successful token receipt. The token-storage step above is
            // the must-succeed part; the steps below are nice-to-have —
            // log and continue on individual failures so a stuck side-
            // effect can't leave the user without a working integration.
            promote_to_tier_2(&params.token);

            // Kick the unified relay so it picks up the new tier + token
            // without waiting for the next reconnect cycle.
            tokio::spawn(async {
                crate::mcp::backend_relay::commands::kick_cloud_relay().await;
            });

            let body =
                SUCCESS_HTML.replace("__RUNNER_NAME__", &html_escape(&runner_name_for_display));
            html_response(StatusCode::OK, body).into_response()
        }
        Err(e) => {
            warn!(
                "runner_token_callback: failed to apply integration settings: {}",
                e
            );
            html_response(StatusCode::INTERNAL_SERVER_ERROR, error_html(&e)).into_response()
        }
    }
}

/// Promote the runner to Tier 2 (`QontinuiAccount`) and mark setup as
/// complete. Best-effort: any individual write that fails is logged and
/// skipped so a single side-effect failure can't poison the sign-in flow.
///
/// `token` is the qontinui-web-issued runner token. The current token
/// format is opaque (`qontinui_runner_<random>` per
/// `qontinui-web/backend/app/models/runner_token.py`), so the `sub`-claim
/// extraction is a no-op — kept as a forward-compatible path for when/if
/// the web side issues JWT-shaped tokens that carry the user id directly.
fn promote_to_tier_2(token: &str) {
    let mut s = crate::settings::load_settings();
    let mut mutated = false;

    if s.tier != crate::settings::RunnerTier::QontinuiAccount {
        s.tier = crate::settings::RunnerTier::QontinuiAccount;
        s.tier_initialized = true;
        mutated = true;
    }

    if !s.setup_completed {
        // The user reached this code path by completing the browser-side
        // pairing flow — that's a strictly stronger signal than the
        // wizard's "Finish" button, so call setup done if it isn't yet.
        s.setup_completed = true;
        mutated = true;
    }

    if s.qontinui_user_id.is_none() {
        if let Some(sub) = decode_jwt_sub(token) {
            s.qontinui_user_id = Some(sub);
            mutated = true;
        } else {
            info!(
                "runner_token_callback: token is opaque (not a JWT) — \
                 leaving qontinui_user_id unset; web-side relay handshake \
                 will populate user identity"
            );
        }
    }

    if mutated {
        if crate::instance::is_secondary() {
            warn!("promote_to_tier_2: secondary runner — applying in-memory only, skipping save_settings");
        } else if let Err(e) = crate::settings::save_settings(&s) {
            warn!(
                "runner_token_callback: failed to persist tier/setup_completed: {}",
                e
            );
        }
    }
}

/// Decode a JWT's `sub` claim without verifying signatures.
///
/// Returns `None` for non-JWTs (including the opaque
/// `qontinui_runner_<random>` runner-token format), malformed payloads, or
/// missing `sub` fields. Signature verification is intentionally skipped —
/// we just stored this token after a successful loopback handshake; the
/// only consumer of the extracted `sub` here is a display/log field on
/// the runner side.
fn decode_jwt_sub(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    use base64::Engine;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload
        .get("sub")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

async fn apply_integration(
    state: &Arc<ApiState>,
    new_settings: WebIntegrationSettings,
) -> Result<(), String> {
    crate::commands::web_integration::apply_web_integration_settings(
        state.app_state.as_ref(),
        &state.app_handle,
        new_settings,
    )
    .await
}
