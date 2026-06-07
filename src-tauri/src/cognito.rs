//! Cognito Hosted-UI sign-in via RFC 8252 (OAuth 2.0 for Native Apps).
//!
//! Phase 5 of the unified-Cognito-identity plan. The runner is a **public**
//! OAuth client (no secret) using the Authorization Code + PKCE flow against
//! the Cognito Hosted UI at `https://auth.qontinui.io`.
//!
//! ## Flow (`pkce_login`)
//!
//! 1. Generate a high-entropy `code_verifier` (RFC 7636 §4.1, 43–128
//!    unreserved chars), its `code_challenge = base64url(SHA256(verifier))`,
//!    and a random anti-CSRF `state`.
//! 2. Bind a loopback HTTP server on the **fixed** `127.0.0.1:53682`. Cognito
//!    does exact `redirect_uri` matching, so the port is fixed (not the
//!    OS-assigned `:0` used by the coord browser-pair in `pair.rs`); the two
//!    registered callbacks are `http://localhost:53682/auth/callback` and
//!    `http://127.0.0.1:53682/auth/callback`.
//! 3. Open the system browser to `/oauth2/authorize?...` and wait for the
//!    redirect to `/auth/callback?code=...&state=...`.
//! 4. Validate `state`, exchange `code` + `code_verifier` at `/oauth2/token`
//!    for `{access_token, id_token, refresh_token, expires_in}`.
//!
//! This module reuses the loopback + system-browser pattern from
//! [`crate::pair::pair_via_browser`] but talks to Cognito directly rather than
//! to coord. The captured tokens are **not** persisted here — the caller
//! (`commands::auth`) stores them in the distinct oauth slots and then binds
//! the coord device-JWT to the Cognito user.

use serde::Deserialize;

// ============================================================================
// Cognito constants — runner public client (PKCE, no secret).
// ============================================================================

/// Cognito Hosted-UI base (authorize + token endpoints hang off this).
pub const COGNITO_BASE: &str = "https://auth.qontinui.io";
/// Authorize endpoint.
pub const COGNITO_AUTHORIZE_URL: &str = "https://auth.qontinui.io/oauth2/authorize";
/// Token endpoint.
pub const COGNITO_TOKEN_URL: &str = "https://auth.qontinui.io/oauth2/token";
/// Runner app client id — PUBLIC client, no secret, PKCE.
pub const COGNITO_CLIENT_ID: &str = "67f2a1a0cmgileob23lniud5t7";
/// Fixed loopback callback port. Cognito does exact `redirect_uri` matching,
/// so this MUST equal the registered callback's port (do NOT use `:0`).
pub const COGNITO_CALLBACK_PORT: u16 = 53682;
/// Registered redirect URI. `localhost` (not `127.0.0.1`) form is used for the
/// authorize request; both forms are registered in Cognito so the loopback
/// server — bound to `127.0.0.1` — still receives the redirect.
pub const COGNITO_REDIRECT_URI: &str = "http://localhost:53682/auth/callback";
/// OAuth scopes requested.
pub const COGNITO_SCOPE: &str = "openid email profile";
/// Region-scoped Cognito IDP endpoint used for the direct `InitiateAuth`
/// (USER_PASSWORD_AUTH) flow — the no-browser, credential-driven sign-in.
/// Distinct from the Hosted-UI token endpoint (`COGNITO_TOKEN_URL`).
pub const COGNITO_IDP_URL: &str = "https://cognito-idp.us-east-1.amazonaws.com/";

// ============================================================================
// Wire types.
// ============================================================================

/// Successful `/oauth2/token` response (authorization_code or refresh_token
/// grant). On a refresh grant Cognito omits `refresh_token` (the existing one
/// stays valid), hence `Option`.
#[derive(Debug, Clone, Deserialize)]
pub struct CognitoTokenResponse {
    pub access_token: String,
    pub id_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    #[serde(default)]
    #[allow(dead_code)]
    pub token_type: Option<String>,
}

/// The bundle returned to the caller after a successful PKCE login: the raw
/// tokens plus the Cognito `sub` (decoded from the id token) used as the
/// `X-Qontinui-User-Id` when binding the device JWT.
#[derive(Debug, Clone)]
pub struct CognitoLoginResult {
    pub access_token: String,
    pub id_token: String,
    pub refresh_token: String,
    /// Absolute unix-seconds expiry (now + expires_in).
    pub expires_at: i64,
    /// `sub` claim from the id token (the Cognito user id).
    pub sub: String,
    /// `email` claim from the id token, if present.
    pub email: Option<String>,
}

// ============================================================================
// PKCE helpers (RFC 7636).
// ============================================================================

/// Unreserved characters per RFC 3986 §2.3 — the legal alphabet for a PKCE
/// `code_verifier`.
const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Generate a 64-char (RFC 7636 allows 43–128) high-entropy `code_verifier`
/// from the unreserved alphabet using a CSPRNG.
pub fn generate_code_verifier() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..64)
        .map(|_| {
            let idx = rng.random_range(0..UNRESERVED.len());
            UNRESERVED[idx] as char
        })
        .collect()
}

/// `code_challenge = base64url-no-pad(SHA256(code_verifier))` (S256 method).
pub fn code_challenge_s256(verifier: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Random URL-safe `state` nonce (anti-CSRF).
pub fn generate_state() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the `/oauth2/authorize` URL for the PKCE flow.
///
/// When `identity_provider` is `Some`, an `&identity_provider=<Provider>` param
/// is appended so the Cognito Hosted UI jumps straight into that federated IdP
/// (e.g. `Google`, `MicrosoftEntra`, `GitHub`) rather than showing its own
/// chooser. When `None`, the param is omitted and behaviour is unchanged (the
/// native email/password + chooser screen).
pub fn build_authorize_url(
    state: &str,
    code_challenge: &str,
    identity_provider: Option<&str>,
) -> String {
    let mut url = format!(
        "{COGNITO_AUTHORIZE_URL}?response_type=code&client_id={client_id}\
         &redirect_uri={redirect}&scope={scope}&state={state}\
         &code_challenge={challenge}&code_challenge_method=S256",
        client_id = COGNITO_CLIENT_ID,
        redirect = urlencoding::encode(COGNITO_REDIRECT_URI),
        scope = urlencoding::encode(COGNITO_SCOPE),
        state = urlencoding::encode(state),
        challenge = code_challenge,
    );
    if let Some(provider) = identity_provider {
        if !provider.is_empty() {
            url.push_str("&identity_provider=");
            url.push_str(&urlencoding::encode(provider));
        }
    }
    url
}

/// Decode an unverified string claim from a JWT payload (the id token). We do
/// NOT verify the signature — the token was just received over TLS directly
/// from Cognito's token endpoint; the web backend / coord re-validate on use.
pub(crate) fn jwt_claim(token: &str, claim: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut parts = token.splitn(3, '.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get(claim).and_then(|v| v.as_str()).map(String::from)
}

// ============================================================================
// Callback capture.
// ============================================================================

/// Query params Cognito appends to the loopback redirect. On the error path
/// Cognito sends `error` / `error_description` instead of `code`, so both
/// `code` and `state` are optional and validated by the handler.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CognitoCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

// ============================================================================
// PKCE login (browser + loopback) — blocking.
// ============================================================================

/// Bind the loopback callback port with `SO_REUSEADDR` so a lingering
/// `TIME_WAIT` socket from a just-completed sign-in never blocks a fresh
/// rebind on Windows (`os error 10048`). Mirrors `mcp_api::try_bind_port`.
fn bind_loopback_reuseaddr(port: u16) -> std::io::Result<std::net::TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], port)).into())?;
    socket.listen(128)?;
    Ok(socket.into())
}

/// Run the full RFC-8252 PKCE login: open the system browser, capture the
/// loopback redirect, validate `state`, and exchange the code at the token
/// endpoint. Blocking (mirrors `pair::pair_via_browser`); call via
/// `tokio::task::spawn_blocking` from async contexts.
///
/// `identity_provider` selects a federated IdP to jump straight into (one of
/// the Cognito provider names — `Google`, `MicrosoftEntra`, `GitHub`); `None`
/// preserves the existing native email/chooser screen.
pub fn pkce_login(identity_provider: Option<&str>) -> Result<CognitoLoginResult, String> {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let code_verifier = generate_code_verifier();
    let code_challenge = code_challenge_s256(&code_verifier);
    let state = generate_state();

    // Bind the FIXED loopback port. Cognito exact-matches redirect_uri, so we
    // cannot use an OS-assigned port here. If the port is busy, surface a
    // clear, actionable error rather than silently falling back.
    //
    // Bind via socket2 with SO_REUSEADDR so a lingering TIME_WAIT socket from a
    // just-completed sign-in (the previous flow's loopback server tears down on
    // success, but the OS may hold the 4-tuple in TIME_WAIT briefly) never
    // blocks a fresh rebind with `os error 10048`.
    let std_listener = bind_loopback_reuseaddr(COGNITO_CALLBACK_PORT).map_err(|e| {
        format!(
            "could not bind loopback callback on 127.0.0.1:{COGNITO_CALLBACK_PORT} ({e}). \
             Another sign-in may be in progress, or another app is holding the port. \
             Close it and try again."
        )
    })?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking failed: {e}"))?;

    let received: Arc<Mutex<Option<Result<String, String>>>> = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    let state_expected = state.clone();

    // Graceful-shutdown signal: the route handler fires this the instant it has
    // written a result (code OR error) to `received`, so `axum::serve` returns
    // and the listener — hence port 53682 — is fully torn down before
    // `pkce_login` returns. Without this the server would keep LISTENing after
    // the code was captured, leaking the fixed port and making the NEXT sign-in
    // fail to bind with `os error 10048` (BUG 1).
    //
    // A oneshot can only be sent once and `Sender::send` consumes it, so it
    // lives in a shared `Mutex<Option<_>>` the (potentially re-entrant) handler
    // takes-and-sends exactly once.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> =
        Arc::new(Mutex::new(Some(shutdown_tx)));
    let shutdown_tx_route = shutdown_tx.clone();

    let (server_done_tx, server_done_rx) = std::sync::mpsc::channel::<()>();
    let _server_handle = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime build failed: {e}"))?;
        rt.block_on(async move {
            use axum::{extract::Query, response::Html, routing::get, Router};
            let state_for_route = state_expected.clone();
            let received_for_route = received_clone.clone();
            // Fire the graceful-shutdown signal exactly once (idempotent across
            // duplicate callback hits).
            let signal_shutdown =
                move |tx: &Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>| {
                    if let Some(tx) = tx.lock().expect("mutex").take() {
                        let _ = tx.send(());
                    }
                };
            let app = Router::new().route(
                "/auth/callback",
                get(move |Query(q): Query<CognitoCallbackQuery>| {
                    let state_for_route = state_for_route.clone();
                    let received_for_route = received_for_route.clone();
                    let shutdown_tx_route = shutdown_tx_route.clone();
                    let signal_shutdown = signal_shutdown.clone();
                    async move {
                        // Error path: Cognito reported a problem.
                        if let Some(err) = q.error {
                            let desc = q.error_description.unwrap_or_default();
                            *received_for_route.lock().expect("mutex") =
                                Some(Err(format!("Cognito returned error `{err}`: {desc}")));
                            signal_shutdown(&shutdown_tx_route);
                            return Html(
                                "<h1>Sign-in failed</h1><p>You can close this tab and try \
                                 again.</p>"
                                    .to_string(),
                            );
                        }
                        // Anti-CSRF: state must match.
                        if q.state.as_deref() != Some(state_for_route.as_str()) {
                            *received_for_route.lock().expect("mutex") = Some(Err(
                                "callback state did not match the pending sign-in".to_string(),
                            ));
                            signal_shutdown(&shutdown_tx_route);
                            return Html(
                                "<h1>State mismatch</h1><p>The sign-in could not be verified. \
                                 Close this tab and try again.</p>"
                                    .to_string(),
                            );
                        }
                        match q.code {
                            Some(code) if !code.is_empty() => {
                                *received_for_route.lock().expect("mutex") = Some(Ok(code));
                                signal_shutdown(&shutdown_tx_route);
                                Html(
                                    "<h1>&#10003; Signed in to Qontinui</h1>\
                                     <p>You can close this tab and return to the runner.</p>\
                                     <script>setTimeout(()=>window.close(),2000);</script>"
                                        .to_string(),
                                )
                            }
                            _ => {
                                *received_for_route.lock().expect("mutex") =
                                    Some(Err("callback carried no authorization code".to_string()));
                                signal_shutdown(&shutdown_tx_route);
                                Html(
                                    "<h1>Sign-in failed</h1><p>No authorization code was \
                                     returned. Close this tab and try again.</p>"
                                        .to_string(),
                                )
                            }
                        }
                    }
                }),
            );
            let tokio_listener = tokio::net::TcpListener::from_std(std_listener)
                .map_err(|e| format!("tokio listener wrap failed: {e}"))?;
            // Serve until EITHER the handler signals shutdown (code/error
            // captured) OR the 5-minute timeout elapses. `with_graceful_shutdown`
            // stops accepting + drains in-flight responses (so the "Signed in"
            // page is fully delivered to the browser) and THEN drops the
            // listener, releasing port 53682.
            let serve = axum::serve(tokio_listener, app).with_graceful_shutdown(async move {
                tokio::select! {
                    _ = shutdown_rx => {}
                    _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                }
            });
            serve.await.map_err(|e| format!("axum serve failed: {e}"))?;
            // The listener is now dropped (port released). Wake the polling
            // main thread so it stops waiting the moment the port is free.
            let _ = server_done_tx.send(());
            Ok(())
        })
    });

    let authorize_url = build_authorize_url(&state, &code_challenge, identity_provider);
    tracing::info!("opening browser for Cognito sign-in (callback {COGNITO_REDIRECT_URI})");
    if let Err(e) = open::that(&authorize_url) {
        tracing::warn!(
            "failed to open browser ({e}); user must open this URL manually: {authorize_url}"
        );
    }

    // Poll the slot up to 5 minutes. On capture we DON'T return immediately —
    // we first wait for the server thread to finish its graceful shutdown (it
    // sends on `server_done_rx` only AFTER the listener is dropped) so port
    // 53682 is provably free before `pkce_login` returns. This holds on BOTH
    // the success and the error path, so an immediate retry can always rebind.
    let deadline = Instant::now() + Duration::from_secs(300);
    let captured: Result<String, String> = loop {
        if let Some(result) = received.lock().expect("mutex").take() {
            break result;
        }
        if server_done_rx.try_recv().is_ok() {
            // Server stopped without a captured result — only happens on the
            // internal 300s graceful-shutdown timeout (no browser callback).
            break match received.lock().expect("mutex").take() {
                Some(result) => result,
                None => Err(
                    "sign-in timed out after 5 minutes waiting for the browser callback"
                        .to_string(),
                ),
            };
        }
        if Instant::now() >= deadline {
            break Err(
                "sign-in timed out after 5 minutes waiting for the browser callback".to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    // Wait (bounded) for the loopback server to fully tear down so the port is
    // released regardless of success/error — the handler fires graceful
    // shutdown the instant it writes `received`, so this is near-instant.
    let teardown_deadline = Instant::now() + Duration::from_secs(5);
    while server_done_rx.try_recv().is_err() && Instant::now() < teardown_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let code = captured?;

    // Exchange the authorization code for tokens.
    let token_resp = exchange_code(&code, &code_verifier)?;
    let expires_at = chrono::Utc::now().timestamp() + token_resp.expires_in;
    let refresh_token = token_resp.refresh_token.ok_or_else(|| {
        "Cognito token response omitted refresh_token — the runner client must be configured \
         to issue refresh tokens"
            .to_string()
    })?;
    let sub = jwt_claim(&token_resp.id_token, "sub")
        .ok_or_else(|| "id token has no `sub` claim — cannot identify the user".to_string())?;
    let email = jwt_claim(&token_resp.id_token, "email");

    Ok(CognitoLoginResult {
        access_token: token_resp.access_token,
        id_token: token_resp.id_token,
        refresh_token,
        expires_at,
        sub,
        email,
    })
}

// ============================================================================
// Direct credential login (`InitiateAuth` USER_PASSWORD_AUTH) — no browser.
// ============================================================================

/// `AuthenticationResult` block from a Cognito `InitiateAuth` /
/// `RespondToAuthChallenge` success response. Field names are Cognito's
/// PascalCase (the IDP JSON protocol), distinct from the Hosted-UI
/// `/oauth2/token` snake_case shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CognitoAuthenticationResult {
    pub access_token: String,
    pub id_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: i64,
}

/// Top-level `InitiateAuth` success envelope.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InitiateAuthResponse {
    #[serde(default)]
    authentication_result: Option<CognitoAuthenticationResult>,
}

/// Sign in directly with email + password via Cognito `InitiateAuth`
/// USER_PASSWORD_AUTH — no system browser, no loopback redirect. Yields the
/// same `CognitoLoginResult` bundle as [`pkce_login`] so the post-auth chain is
/// identical.
///
/// The password is NEVER logged. On a Cognito error (e.g.
/// `NotAuthorizedException`, `UserNotConfirmedException`,
/// `PasswordResetRequiredException`) a clean human-readable `Err(message)` is
/// returned. Blocking; call via `tokio::task::spawn_blocking`.
///
/// Requires the runner client (`COGNITO_CLIENT_ID`) to have
/// `ALLOW_USER_PASSWORD_AUTH` enabled in its Cognito app-client config.
pub fn password_login(email: &str, password: &str) -> Result<CognitoLoginResult, String> {
    let result = initiate_auth_user_password(email, password)?;

    let expires_at = chrono::Utc::now().timestamp() + result.expires_in;
    let refresh_token = result.refresh_token.ok_or_else(|| {
        "Cognito InitiateAuth response omitted RefreshToken — the runner client must be \
         configured to issue refresh tokens"
            .to_string()
    })?;
    let sub = jwt_claim(&result.id_token, "sub")
        .ok_or_else(|| "id token has no `sub` claim — cannot identify the user".to_string())?;
    let claim_email = jwt_claim(&result.id_token, "email");

    Ok(CognitoLoginResult {
        access_token: result.access_token,
        id_token: result.id_token,
        refresh_token,
        expires_at,
        sub,
        email: claim_email,
    })
}

/// POST the raw `InitiateAuth` USER_PASSWORD_AUTH request to the region IDP
/// endpoint and surface Cognito error codes as clean messages. Blocking.
fn initiate_auth_user_password(
    email: &str,
    password: &str,
) -> Result<CognitoAuthenticationResult, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;

    let body = serde_json::json!({
        "AuthFlow": "USER_PASSWORD_AUTH",
        "ClientId": COGNITO_CLIENT_ID,
        "AuthParameters": {
            "USERNAME": email,
            "PASSWORD": password,
        }
    });

    let resp = client
        .post(COGNITO_IDP_URL)
        .header(
            "X-Amz-Target",
            "AWSCognitoIdentityProviderService.InitiateAuth",
        )
        .header("Content-Type", "application/x-amz-json-1.1")
        .body(serde_json::to_vec(&body).map_err(|e| format!("serialize InitiateAuth: {e}"))?)
        .send()
        .map_err(|e| format!("POST InitiateAuth failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("read InitiateAuth response: {e}"))?;

    if !status.is_success() {
        return Err(humanize_cognito_error(&text, status.as_u16()));
    }

    let parsed: InitiateAuthResponse = serde_json::from_str(&text)
        .map_err(|e| format!("decode InitiateAuth response failed: {e}"))?;

    parsed.authentication_result.ok_or_else(|| {
        // A 200 without AuthenticationResult means Cognito returned a challenge
        // (e.g. NEW_PASSWORD_REQUIRED / MFA) which this direct flow can't drive.
        "Cognito requires an additional sign-in step (a challenge such as a new password \
         or MFA) that this credential sign-in cannot complete. Use the browser sign-in instead."
            .to_string()
    })
}

/// Turn a Cognito IDP JSON error body into a clean user-facing message. The
/// IDP returns `{"__type":"NotAuthorizedException","message":"..."}`.
fn humanize_cognito_error(body: &str, http_status: u16) -> String {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return format!(
                "Cognito sign-in failed (HTTP {http_status}): {}",
                body.chars().take(200).collect::<String>()
            )
        }
    };
    // `__type` is like "NotAuthorizedException" or
    // "com.amazon.coral...#NotAuthorizedException".
    let code = json
        .get("__type")
        .and_then(|v| v.as_str())
        .map(|s| s.rsplit('#').next().unwrap_or(s))
        .unwrap_or("UnknownError");
    let msg = json
        .get("message")
        .or_else(|| json.get("Message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match code {
        "NotAuthorizedException" => "Incorrect email or password.".to_string(),
        "UserNotFoundException" => "No account found for that email.".to_string(),
        "UserNotConfirmedException" => {
            "Your account is not confirmed yet. Check your email for a verification link."
                .to_string()
        }
        "PasswordResetRequiredException" => {
            "A password reset is required. Reset your password, then sign in again.".to_string()
        }
        "InvalidParameterException" => {
            if msg.is_empty() {
                "Invalid sign-in parameters.".to_string()
            } else {
                format!("Invalid sign-in: {msg}")
            }
        }
        "TooManyRequestsException" | "LimitExceededException" => {
            "Too many sign-in attempts. Wait a moment and try again.".to_string()
        }
        other => {
            if msg.is_empty() {
                format!("Cognito sign-in failed ({other}).")
            } else {
                format!("Cognito sign-in failed ({other}): {msg}")
            }
        }
    }
}

/// POST the authorization_code grant to `/oauth2/token`. Blocking.
fn exchange_code(code: &str, code_verifier: &str) -> Result<CognitoTokenResponse, String> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", COGNITO_CLIENT_ID),
        ("code", code),
        ("redirect_uri", COGNITO_REDIRECT_URI),
        ("code_verifier", code_verifier),
    ];
    post_token_form(&form)
}

/// Exchange a refresh token for a fresh access + id token. Cognito does NOT
/// return a new refresh_token on this grant; the caller keeps the existing
/// one. Blocking; call via `spawn_blocking`.
pub fn refresh_tokens(refresh_token: &str) -> Result<CognitoTokenResponse, String> {
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", COGNITO_CLIENT_ID),
        ("refresh_token", refresh_token),
    ];
    post_token_form(&form)
}

/// Shared form POST to the token endpoint with status/error surfacing.
fn post_token_form(form: &[(&str, &str)]) -> Result<CognitoTokenResponse, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;
    let resp = client
        .post(COGNITO_TOKEN_URL)
        .form(form)
        .send()
        .map_err(|e| format!("POST {COGNITO_TOKEN_URL} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .unwrap_or_else(|_| "<unreadable body>".to_string());
        return Err(format!(
            "token endpoint returned HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    resp.json::<CognitoTokenResponse>()
        .map_err(|e| format!("decode token response failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_verifier_is_unreserved_and_right_length() {
        let v = generate_code_verifier();
        assert_eq!(v.len(), 64, "verifier must be 64 chars (within 43..=128)");
        assert!(
            v.bytes().all(|b| UNRESERVED.contains(&b)),
            "verifier must use only RFC 3986 unreserved chars"
        );
    }

    #[test]
    fn code_verifiers_are_unique() {
        // Astronomically unlikely to collide; guards against a constant.
        assert_ne!(generate_code_verifier(), generate_code_verifier());
    }

    #[test]
    fn code_challenge_matches_rfc7636_test_vector() {
        // RFC 7636 Appendix B canonical vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = code_challenge_s256(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn state_is_url_safe_and_nonempty() {
        let s = generate_state();
        assert!(!s.is_empty());
        assert!(s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn authorize_url_has_required_params() {
        let url = build_authorize_url("st-ate", "chal-lenge", None);
        assert!(url.starts_with(COGNITO_AUTHORIZE_URL));
        assert!(url.contains("response_type=code"));
        assert!(url.contains(&format!("client_id={COGNITO_CLIENT_ID}")));
        assert!(url.contains("code_challenge=chal-lenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st-ate"));
        // redirect_uri must be percent-encoded.
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A53682%2Fauth%2Fcallback"));
        // scope is space-separated → encoded as %20.
        assert!(url.contains("scope=openid%20email%20profile"));
        // No provider → no identity_provider param (native chooser path).
        assert!(!url.contains("identity_provider"));
    }

    #[test]
    fn authorize_url_appends_identity_provider_when_set() {
        let url = build_authorize_url("st-ate", "chal-lenge", Some("Google"));
        assert!(url.contains("&identity_provider=Google"));

        // Microsoft Entra provider name is threaded verbatim.
        let url = build_authorize_url("st-ate", "chal-lenge", Some("MicrosoftEntra"));
        assert!(url.contains("&identity_provider=MicrosoftEntra"));

        // An empty provider is treated as "unset" → param omitted.
        let url = build_authorize_url("st-ate", "chal-lenge", Some(""));
        assert!(!url.contains("identity_provider"));
    }

    #[test]
    fn jwt_claim_extracts_sub_and_email() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload =
            URL_SAFE_NO_PAD.encode(br#"{"sub":"user-123","email":"a@b.co","token_use":"id"}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_claim(&token, "sub").as_deref(), Some("user-123"));
        assert_eq!(jwt_claim(&token, "email").as_deref(), Some("a@b.co"));
        assert_eq!(jwt_claim(&token, "missing"), None);
        assert_eq!(jwt_claim("not-a-jwt", "sub"), None);
    }

    #[test]
    fn token_response_decodes_with_and_without_refresh_token() {
        // authorization_code grant: includes refresh_token.
        let with: CognitoTokenResponse = serde_json::from_str(
            r#"{"access_token":"a","id_token":"i","refresh_token":"r","expires_in":3600,"token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(with.refresh_token.as_deref(), Some("r"));
        assert_eq!(with.expires_in, 3600);

        // refresh_token grant: Cognito omits refresh_token.
        let without: CognitoTokenResponse = serde_json::from_str(
            r#"{"access_token":"a2","id_token":"i2","expires_in":3600,"token_type":"Bearer"}"#,
        )
        .unwrap();
        assert!(without.refresh_token.is_none());
    }

    #[test]
    fn initiate_auth_response_decodes_pascalcase() {
        let resp: InitiateAuthResponse = serde_json::from_str(
            r#"{"AuthenticationResult":{"AccessToken":"at","IdToken":"it","RefreshToken":"rt","ExpiresIn":3600,"TokenType":"Bearer"}}"#,
        )
        .unwrap();
        let ar = resp.authentication_result.expect("has result");
        assert_eq!(ar.access_token, "at");
        assert_eq!(ar.id_token, "it");
        assert_eq!(ar.refresh_token.as_deref(), Some("rt"));
        assert_eq!(ar.expires_in, 3600);
    }

    #[test]
    fn initiate_auth_response_handles_challenge_without_result() {
        // A challenge (e.g. NEW_PASSWORD_REQUIRED) carries no AuthenticationResult.
        let resp: InitiateAuthResponse =
            serde_json::from_str(r#"{"ChallengeName":"NEW_PASSWORD_REQUIRED","Session":"abc"}"#)
                .unwrap();
        assert!(resp.authentication_result.is_none());
    }

    #[test]
    fn humanize_cognito_error_maps_known_codes() {
        assert_eq!(
            humanize_cognito_error(r#"{"__type":"NotAuthorizedException","message":"x"}"#, 400),
            "Incorrect email or password."
        );
        // Namespaced __type still resolves to the bare code.
        assert_eq!(
            humanize_cognito_error(
                r#"{"__type":"com.amazon.coral.service#UserNotFoundException","message":"x"}"#,
                400
            ),
            "No account found for that email."
        );
        assert!(
            humanize_cognito_error(r#"{"__type":"UserNotConfirmedException"}"#, 400)
                .contains("not confirmed")
        );
        // Unknown code falls back to a generic message including the code.
        assert!(
            humanize_cognito_error(r#"{"__type":"SomethingElse"}"#, 400).contains("SomethingElse")
        );
        // Non-JSON body doesn't panic.
        assert!(humanize_cognito_error("not json", 500).contains("500"));
    }
}
