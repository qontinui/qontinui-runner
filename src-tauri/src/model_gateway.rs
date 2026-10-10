//! Model gateway: one place that decides where every model call goes.
//!
//! An install may declare a **model gateway**, a base URL that fronts the
//! Anthropic Messages API (the tenant's own gateway, or a vendor-run one).
//! The decision record `gateway-tenants-route-every-model-call-through-the-gateway`
//! says that once a gateway is declared, every model call goes through it:
//!
//! - **Spawned `claude` sessions** reach the model through the gateway. They get
//!   `ANTHROPIC_BASE_URL` plus an `apiKeyHelper`. Both are carried by a
//!   runner-owned `CLAUDE_CONFIG_DIR` ([`session_config_dir`]) and by env applied
//!   at every child-spawn seam ([`live_child_env_plan`]).
//! - **The runner's own direct calls** (the `ai_provider` API paths, the
//!   connection test, the knowledge summarizer) take the gateway's base URL and
//!   headers instead of the vendor host ([`ModelCall`]).
//! - **Subscription accounts and account rotation are off.** Sessions run under
//!   the gateway config dir, never a subscription account's dir. The account
//!   picker, rate-limit rotation, usage probes and account migration all stand
//!   down while [`gateway_declared`] holds.
//! - **The gateway host is allowed by the security profile.** A restrictive
//!   `AllowList` network profile gets the host appended to its
//!   `allowed_domains` ([`crate::security::PolicyEngine::resolve_for_runtime`]).
//!
//! An install with no gateway keeps the runner's rule `feedback_no_anthropic_api`
//! unchanged. Spawned sessions use the operator's `claude` subscription and
//! nothing here applies.
//!
//! ## Secrets
//!
//! The gateway credential is never stored by the runner and never placed on a
//! command line. It comes from the operator's **api-key-helper command**, the
//! same contract as Claude Code's `apiKeyHelper` setting: a shell command that
//! prints the key on stdout. Claude Code runs it for spawned sessions. The
//! runner runs it for its own direct calls ([`ModelGateway::api_key`]). Extra
//! headers are for routing, such as a tenant or project tag. `authorization`
//! and `x-api-key` are refused there, so a secret cannot end up in plaintext
//! `settings.json` ([`ModelGatewaySettings::validate`]).
//!
//! ## Fail closed
//!
//! A declared gateway that does not validate (only reachable by hand-editing
//! `settings.json`, because the save path validates) never falls back to the
//! vendor host. Direct calls refuse with the validation error, and sessions are
//! still pointed at the declared base URL and the gateway config dir. They then
//! fail visibly instead of quietly spending a subscription.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, warn};

/// The vendor endpoint used when no gateway is declared.
pub const VENDOR_API_BASE: &str = "https://api.anthropic.com";

/// The Messages API path, appended to whichever base the call resolves to.
pub const MESSAGES_PATH: &str = "/v1/messages";

/// Env var Claude Code reads for its API base URL.
pub const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// Env var Claude Code reads for extra request headers (`Name: Value` lines).
pub const CUSTOM_HEADERS_ENV: &str = "ANTHROPIC_CUSTOM_HEADERS";

/// Env-borne credentials Claude Code would prefer over `apiKeyHelper`. Each one
/// is stripped from a gateway session's child, so a key inherited from the
/// runner's environment can neither bypass the helper nor travel to the gateway.
pub const SHADOWING_CREDENTIAL_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// Header names refused in [`ModelGatewaySettings::headers`]. Credentials ride
/// the api-key-helper, never plaintext settings.
const CREDENTIAL_HEADER_NAMES: &[&str] = &["authorization", "x-api-key", "proxy-authorization"];

/// How long the runner waits for the api-key-helper before giving up.
const API_KEY_HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// Subdirectory of the runner's config dir that holds the gateway sessions'
/// `CLAUDE_CONFIG_DIR`.
const SESSION_CONFIG_SUBDIR: &str = "model-gateway/claude-config";

/// Persisted gateway declaration (`settings.json` key `model_gateway`).
///
/// Empty `base_url` means no gateway. Every other field is ignored then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelGatewaySettings {
    /// Base URL that fronts the Messages API, such as
    /// `https://llm-gateway.example.com/anthropic`. The runner appends
    /// `/v1/messages`, exactly as Claude Code does with `ANTHROPIC_BASE_URL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Extra non-secret headers sent on every model call (routing or tenant
    /// tags). Credential headers are refused; see the module doc.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Shell command that prints the gateway API key on stdout. Optional: a
    /// gateway that authenticates by network position or mTLS needs none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_helper: Option<String>,
}

impl ModelGatewaySettings {
    /// Whether a gateway is declared (a non-blank `base_url`), valid or not.
    pub fn is_declared(&self) -> bool {
        self.base_url
            .as_deref()
            .is_some_and(|u| !u.trim().is_empty())
    }

    /// Normalize blank fields to `None`/absent and trim values. Applied on save
    /// so `settings.json` carries one canonical spelling.
    pub fn normalized(&self) -> Self {
        let trim_opt = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            base_url: trim_opt(&self.base_url).map(|u| u.trim_end_matches('/').to_string()),
            headers: self
                .headers
                .iter()
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .filter(|(k, _)| !k.is_empty())
                .collect(),
            api_key_helper: trim_opt(&self.api_key_helper),
        }
    }

    /// Validate a declaration. `Ok(())` for "no gateway declared" too.
    pub fn validate(&self) -> Result<(), String> {
        self.resolve().map(|_| ())
    }

    /// Resolve into a usable [`ModelGateway`]. `Ok(None)` when none is
    /// declared, `Err` when one is declared but invalid.
    pub fn resolve(&self) -> Result<Option<ModelGateway>, String> {
        let n = self.normalized();
        let Some(raw) = n.base_url.as_deref() else {
            return Ok(None);
        };
        let url = url::Url::parse(raw)
            .map_err(|e| format!("model gateway base URL '{raw}' does not parse: {e}"))?;
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("model gateway base URL '{raw}' has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        match url.scheme() {
            "https" => {}
            "http" if is_loopback_host(&host) => {}
            "http" => {
                return Err(format!(
                    "model gateway base URL '{raw}' uses plain http to a non-loopback host; \
                     use https (plain http is accepted for localhost only)"
                ))
            }
            other => {
                return Err(format!(
                    "model gateway base URL '{raw}' has unsupported scheme '{other}'"
                ))
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(
                "model gateway base URL must not carry credentials; use the api-key-helper"
                    .to_string(),
            );
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(format!(
                "model gateway base URL '{raw}' must not carry a query or fragment"
            ));
        }

        let mut headers = Vec::with_capacity(n.headers.len());
        for (name, value) in &n.headers {
            let lower = name.to_ascii_lowercase();
            if CREDENTIAL_HEADER_NAMES.contains(&lower.as_str()) {
                return Err(format!(
                    "model gateway header '{name}' carries a credential; configure the \
                     api-key-helper instead of storing a secret in settings"
                ));
            }
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("model gateway header name '{name}' is invalid: {e}"))?;
            if value.contains(['\r', '\n']) {
                return Err(format!(
                    "model gateway header '{name}' value contains a line break"
                ));
            }
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| format!("model gateway header '{name}' value is invalid: {e}"))?;
            headers.push((name.clone(), value.clone()));
        }

        Ok(Some(ModelGateway {
            base_url: raw.to_string(),
            host,
            headers,
            api_key_helper: n.api_key_helper,
        }))
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A validated gateway declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGateway {
    base_url: String,
    host: String,
    headers: Vec<(String, String)>,
    api_key_helper: Option<String>,
}

impl ModelGateway {
    /// Base URL with no trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Lowercased host, for the security profile's `allowed_domains`.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The configured api-key-helper command, if any.
    pub fn api_key_helper(&self) -> Option<&str> {
        self.api_key_helper.as_deref()
    }

    /// Absolute URL for an API path such as [`MESSAGES_PATH`].
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// The `ANTHROPIC_CUSTOM_HEADERS` value (`Name: Value` per line), or `None`
    /// when no extra headers are configured.
    pub fn custom_headers_env(&self) -> Option<String> {
        if self.headers.is_empty() {
            return None;
        }
        Some(
            self.headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Run the api-key-helper and return the key it prints. `Ok(None)` when no
    /// helper is configured. Blocking: async callers wrap it in a blocking task.
    pub fn api_key(&self) -> Result<Option<String>, String> {
        match self.api_key_helper.as_deref() {
            None => Ok(None),
            Some(cmd) => run_api_key_helper(cmd, API_KEY_HELPER_TIMEOUT).map(Some),
        }
    }

    /// The `settings.json` written into the gateway sessions' config dir. It
    /// carries the helper and the routing env, so a `claude` started from that
    /// dir reaches the gateway even when nothing else in its env does.
    pub fn session_settings_json(&self) -> serde_json::Value {
        let mut env = serde_json::Map::new();
        env.insert(BASE_URL_ENV.to_string(), self.base_url.clone().into());
        if let Some(h) = self.custom_headers_env() {
            env.insert(CUSTOM_HEADERS_ENV.to_string(), h.into());
        }
        let mut root = serde_json::Map::new();
        if let Some(helper) = &self.api_key_helper {
            root.insert("apiKeyHelper".to_string(), helper.clone().into());
        }
        root.insert("env".to_string(), serde_json::Value::Object(env));
        serde_json::Value::Object(root)
    }
}

/// Run `cmd` through the platform shell and return its trimmed stdout.
///
/// The command line comes from the operator's own settings, the same trust
/// level as Claude Code's `apiKeyHelper`. Bounded by `timeout`: a helper that
/// hangs is killed and reported, never waited on forever.
pub(crate) fn run_api_key_helper(cmd: &str, timeout: Duration) -> Result<String, String> {
    let mut command = if cfg!(target_os = "windows") {
        let mut c = crate::process_helpers::no_window("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = crate::process_helpers::no_window("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
        command.env_remove(name);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("model gateway api-key-helper failed to start: {e}"))?;

    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "model gateway api-key-helper did not finish within {}s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("model gateway api-key-helper wait failed: {e}")),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        // stderr is deliberately not echoed: a failing helper may print the
        // very secret it was meant to hand over.
        return Err(format!("model gateway api-key-helper exited with {status}"));
    }
    let key = out.trim().to_string();
    if key.is_empty() {
        return Err("model gateway api-key-helper printed nothing".to_string());
    }
    Ok(key)
}

// ---------------------------------------------------------------------------
// Live state (reads settings)
// ---------------------------------------------------------------------------

/// The live declaration from `settings.json`.
///
/// Read through the read-only settings loader: this is consulted at every spawn
/// seam and inside diagnostics (the config report resolves the account dir
/// through it), and `load_settings` is a writer door that may persist
/// migrations. An unreadable file reads as "no gateway declared".
pub fn current_settings() -> ModelGatewaySettings {
    crate::settings::read_settings_from_disk()
        .settings
        .model_gateway
}

/// Whether this install declares a gateway (valid or not). The single predicate
/// every "subscription accounts and rotation are off" gate reads.
pub fn gateway_declared() -> bool {
    current_settings().is_declared()
}

/// The live, validated gateway. `Ok(None)` when none is declared.
pub fn current() -> Result<Option<ModelGateway>, String> {
    current_settings().resolve()
}

/// The gateway host to allow in the security profile, if a valid gateway is
/// declared.
pub fn current_host() -> Option<String> {
    current().ok().flatten().map(|g| g.host().to_string())
}

/// The `CLAUDE_CONFIG_DIR` every gateway session runs under: a runner-owned dir
/// beside `settings.json`, so each install (primary, secondary, temp) has its
/// own. It holds no subscription credentials by construction.
pub fn session_config_dir() -> Result<PathBuf, String> {
    let (dir, _source) = crate::settings::resolve_config_dir()?;
    Ok(dir.join(SESSION_CONFIG_SUBDIR))
}

/// Write the gateway sessions' config dir: `settings.json` (helper and routing
/// env) and a `.claude.json` that marks onboarding done. `settings.json` is
/// rewritten only when its content changed. `.claude.json` is created once and
/// then left to Claude Code, which keeps its own state there.
pub fn materialize_session_config(gateway: &ModelGateway) -> Result<PathBuf, String> {
    let dir = session_config_dir()?;
    materialize_session_config_at(&dir, gateway)?;
    Ok(dir)
}

pub(crate) fn materialize_session_config_at(
    dir: &std::path::Path,
    gateway: &ModelGateway,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let settings_path = dir.join("settings.json");
    let body = serde_json::to_string_pretty(&gateway.session_settings_json())
        .map_err(|e| format!("serialize gateway session settings: {e}"))?;
    let current = std::fs::read_to_string(&settings_path).ok();
    if current.as_deref() != Some(body.as_str()) {
        std::fs::write(&settings_path, &body)
            .map_err(|e| format!("write {}: {e}", settings_path.display()))?;
    }
    let state_path = dir.join(".claude.json");
    if !state_path.exists() {
        std::fs::write(&state_path, "{\n  \"hasCompletedOnboarding\": true\n}\n")
            .map_err(|e| format!("write {}: {e}", state_path.display()))?;
    }
    Ok(())
}

/// The config dir a gateway session must run under, or `None` when no gateway
/// is declared. Materializes the dir as a side effect. A declared but invalid
/// gateway still returns the dir path, so the session runs with no
/// subscription account (see "Fail closed" in the module doc).
pub fn session_config_dir_override() -> Option<String> {
    let settings = current_settings();
    if !settings.is_declared() {
        return None;
    }
    if let Ok(Some(gateway)) = settings.resolve() {
        if let Err(e) = materialize_session_config(&gateway) {
            error!("model gateway: could not write the session config dir: {e}");
        }
    }
    match session_config_dir() {
        Ok(dir) => Some(dir.to_string_lossy().into_owned()),
        Err(e) => {
            error!("model gateway: could not resolve the session config dir: {e}");
            None
        }
    }
}

/// The env a child process gets on a gateway install: what to set and what to
/// remove. Pure over its inputs, so every seam and every test computes the same
/// plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildEnvPlan {
    pub set: Vec<(String, String)>,
    pub remove: Vec<String>,
}

/// Build the [`ChildEnvPlan`] for a declaration and a resolved session config
/// dir. No gateway declared gives an empty plan.
pub fn child_env_plan(
    settings: &ModelGatewaySettings,
    session_config_dir: Option<&str>,
) -> ChildEnvPlan {
    if !settings.is_declared() {
        return ChildEnvPlan::default();
    }
    let mut plan = ChildEnvPlan {
        remove: SHADOWING_CREDENTIAL_ENV
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    if let Some(dir) = session_config_dir {
        plan.set
            .push(("CLAUDE_CONFIG_DIR".to_string(), dir.to_string()));
    }
    match settings.resolve() {
        Ok(Some(gateway)) => {
            plan.set
                .push((BASE_URL_ENV.to_string(), gateway.base_url().to_string()));
            match gateway.custom_headers_env() {
                Some(h) => plan.set.push((CUSTOM_HEADERS_ENV.to_string(), h)),
                None => plan.remove.push(CUSTOM_HEADERS_ENV.to_string()),
            }
        }
        // Fail closed: point the session at what was declared, never at the
        // vendor host.
        _ => {
            let raw = settings.normalized().base_url.unwrap_or_default();
            plan.set.push((BASE_URL_ENV.to_string(), raw));
            plan.remove.push(CUSTOM_HEADERS_ENV.to_string());
        }
    }
    plan
}

/// The [`ChildEnvPlan`] for the live settings. Applied by the three
/// `crate::terminal::scrub_credential_env_*` wrappers, which run last at every
/// spawn seam, so the gateway config dir overrides any subscription-account pin
/// set earlier in the seam. Empty when no gateway is declared.
pub(crate) fn live_child_env_plan() -> ChildEnvPlan {
    let settings = current_settings();
    if !settings.is_declared() {
        return ChildEnvPlan::default();
    }
    let dir = session_config_dir_override();
    child_env_plan(&settings, dir.as_deref())
}

// ---------------------------------------------------------------------------
// Direct model calls
// ---------------------------------------------------------------------------

/// Where one direct model call goes, plus the credential it carries.
///
/// Every runner-side call to the Messages API builds its request through this
/// type, so the vendor host appears in exactly one place ([`VENDOR_API_BASE`]).
#[derive(Debug, Clone)]
pub enum ModelCall {
    /// No gateway declared: the vendor host, with the vendor token dispatched
    /// by `ai_provider::anthropic_auth` (API key or OAuth bearer).
    Vendor { token: String },
    /// A gateway is declared: its base URL and headers, and the helper's key
    /// (when a helper is configured) sent the way Claude Code sends an
    /// `apiKeyHelper` key, as both `x-api-key` and `Authorization: Bearer`.
    Gateway {
        gateway: ModelGateway,
        key: Option<String>,
    },
}

impl ModelCall {
    /// Resolve the call for the live settings. `vendor_token` is consulted only
    /// when no gateway is declared. A gateway call never sends a vendor or
    /// subscription credential.
    pub fn resolve(vendor_token: impl FnOnce() -> Result<String, String>) -> Result<Self, String> {
        Self::resolve_with(&current_settings(), vendor_token)
    }

    /// [`Self::resolve`] over an explicit declaration (tests, callers that
    /// already hold settings).
    pub fn resolve_with(
        settings: &ModelGatewaySettings,
        vendor_token: impl FnOnce() -> Result<String, String>,
    ) -> Result<Self, String> {
        match settings.resolve()? {
            None => Ok(ModelCall::Vendor {
                token: vendor_token()?,
            }),
            Some(gateway) => {
                let key = gateway.api_key()?;
                Ok(ModelCall::Gateway { gateway, key })
            }
        }
    }

    /// Whether this call goes through a gateway.
    pub fn is_gateway(&self) -> bool {
        matches!(self, ModelCall::Gateway { .. })
    }

    /// Short label for logs.
    pub fn route_label(&self) -> &'static str {
        match self {
            ModelCall::Vendor { .. } => "vendor",
            ModelCall::Gateway { .. } => "gateway",
        }
    }

    /// Absolute URL for `path`.
    pub fn url(&self, path: &str) -> String {
        match self {
            ModelCall::Vendor { .. } => format!("{VENDOR_API_BASE}{path}"),
            ModelCall::Gateway { gateway, .. } => gateway.endpoint(path),
        }
    }

    /// A blocking `POST` to `path` with routing and auth headers applied.
    /// Callers add `anthropic-version`, betas, content type and body.
    pub fn post_blocking(
        &self,
        client: &reqwest::blocking::Client,
        path: &str,
    ) -> reqwest::blocking::RequestBuilder {
        let builder = client.post(self.url(path));
        match self {
            ModelCall::Vendor { token } => {
                crate::ai_provider::anthropic_auth::apply_blocking(builder, token)
            }
            ModelCall::Gateway { gateway, key } => {
                let mut b = builder;
                for (k, v) in &gateway.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                if let Some(key) = key {
                    b = b
                        .header("x-api-key", key.as_str())
                        .header("Authorization", format!("Bearer {key}"));
                }
                b
            }
        }
    }

    /// Async twin of [`Self::post_blocking`].
    pub fn post_async(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        let builder = client.post(self.url(path));
        match self {
            ModelCall::Vendor { token } => {
                crate::ai_provider::anthropic_auth::apply_async(builder, token)
            }
            ModelCall::Gateway { gateway, key } => {
                let mut b = builder;
                for (k, v) in &gateway.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                if let Some(key) = key {
                    b = b
                        .header("x-api-key", key.as_str())
                        .header("Authorization", format!("Bearer {key}"));
                }
                b
            }
        }
    }
}

/// The refusal a subscription-account path returns while a gateway is declared.
pub const SUBSCRIPTION_OFF_REASON: &str =
    "a model gateway is configured: subscription accounts and account rotation are off";

/// Log once per call site that a subscription-account path stood down.
pub fn note_subscription_path_off(site: &str) {
    warn!(site, "{SUBSCRIPTION_OFF_REASON}");
}

/// Test support shared with the call sites' own routing tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// One-shot HTTP server on loopback: accepts a single request, records its
    /// request line and headers, and answers with a Messages-API-shaped body
    /// whose text is `reply_text`. Returns the base URL and a handle yielding
    /// the recorded lines.
    pub(crate) fn one_shot_messages_server(
        reply_text: &str,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let reply =
            serde_json::json!({"content": [{"type": "text", "text": reply_text}]}).to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
                lines.push(line);
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.len(),
                reply
            )
            .unwrap();
            lines
        });
        (format!("http://{addr}"), handle)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::one_shot_messages_server;
    use super::*;

    fn decl(base: &str) -> ModelGatewaySettings {
        ModelGatewaySettings {
            base_url: Some(base.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn blank_base_url_is_no_gateway() {
        assert!(!ModelGatewaySettings::default().is_declared());
        assert!(!decl("   ").is_declared());
        assert_eq!(decl("  ").resolve(), Ok(None));
    }

    #[test]
    fn resolve_normalizes_and_extracts_host() {
        let g = decl(" https://LLM.Example.com/anthropic/ ")
            .resolve()
            .unwrap()
            .unwrap();
        assert_eq!(g.base_url(), "https://LLM.Example.com/anthropic");
        assert_eq!(g.host(), "llm.example.com");
        assert_eq!(
            g.endpoint(MESSAGES_PATH),
            "https://LLM.Example.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn plain_http_only_for_loopback() {
        assert!(decl("http://127.0.0.1:8080").resolve().is_ok());
        assert!(decl("http://localhost:8080").resolve().is_ok());
        assert!(decl("http://[::1]:8080").resolve().is_ok());
        let err = decl("http://gateway.example.com").resolve().unwrap_err();
        assert!(err.contains("https"), "{err}");
        assert!(decl("ftp://gateway.example.com").resolve().is_err());
        assert!(decl("not a url").resolve().is_err());
    }

    #[test]
    fn url_credentials_query_and_fragment_are_refused() {
        assert!(decl("https://user:pw@gw.example.com").resolve().is_err());
        assert!(decl("https://gw.example.com?key=1").resolve().is_err());
        assert!(decl("https://gw.example.com#x").resolve().is_err());
    }

    #[test]
    fn credential_headers_are_refused() {
        for name in [
            "Authorization",
            "x-api-key",
            "X-API-KEY",
            "Proxy-Authorization",
        ] {
            let mut s = decl("https://gw.example.com");
            s.headers.insert(name.to_string(), "secret".to_string());
            let err = s.resolve().unwrap_err();
            assert!(err.contains("api-key-helper"), "{name}: {err}");
        }
    }

    #[test]
    fn header_values_with_line_breaks_are_refused() {
        let mut s = decl("https://gw.example.com");
        s.headers
            .insert("X-Tenant".to_string(), "a\nInjected: b".to_string());
        assert!(s.resolve().is_err());
        let mut s = decl("https://gw.example.com");
        s.headers.insert("Bad Name".to_string(), "v".to_string());
        assert!(s.resolve().is_err());
    }

    #[test]
    fn custom_headers_env_is_one_line_per_header() {
        let mut s = decl("https://gw.example.com");
        s.headers.insert("X-Tenant".into(), "t1".into());
        s.headers.insert("X-Project".into(), "p1".into());
        let g = s.resolve().unwrap().unwrap();
        assert_eq!(
            g.custom_headers_env().as_deref(),
            Some("X-Project: p1\nX-Tenant: t1")
        );
        assert_eq!(
            decl("https://gw.example.com")
                .resolve()
                .unwrap()
                .unwrap()
                .custom_headers_env(),
            None
        );
    }

    #[test]
    fn session_settings_carry_helper_and_routing_env() {
        let mut s = decl("https://gw.example.com/a");
        s.api_key_helper = Some("/opt/bin/get-key".into());
        s.headers.insert("X-Tenant".into(), "t1".into());
        let v = s.resolve().unwrap().unwrap().session_settings_json();
        assert_eq!(v["apiKeyHelper"], "/opt/bin/get-key");
        assert_eq!(v["env"][BASE_URL_ENV], "https://gw.example.com/a");
        assert_eq!(v["env"][CUSTOM_HEADERS_ENV], "X-Tenant: t1");

        let v = decl("https://gw.example.com")
            .resolve()
            .unwrap()
            .unwrap()
            .session_settings_json();
        assert!(v.get("apiKeyHelper").is_none());
    }

    #[test]
    fn materialize_writes_settings_and_onboarding_state_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cfg");
        let mut s = decl("https://gw.example.com");
        s.api_key_helper = Some("echo k".into());
        let g = s.resolve().unwrap().unwrap();
        materialize_session_config_at(&dir, &g).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(written["apiKeyHelper"], "echo k");
        // Claude Code owns .claude.json after creation: not overwritten.
        std::fs::write(dir.join(".claude.json"), "{\"claude\":\"state\"}").unwrap();
        materialize_session_config_at(&dir, &g).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".claude.json")).unwrap(),
            "{\"claude\":\"state\"}"
        );
    }

    #[test]
    fn child_env_plan_empty_without_gateway() {
        assert_eq!(
            child_env_plan(&ModelGatewaySettings::default(), Some("/x")),
            ChildEnvPlan::default()
        );
    }

    #[test]
    fn child_env_plan_routes_session_through_gateway() {
        let mut s = decl("https://gw.example.com");
        s.headers.insert("X-Tenant".into(), "t1".into());
        let plan = child_env_plan(&s, Some("/cfg/model-gateway/claude-config"));
        let get = |k: &str| {
            plan.set
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(
            get("CLAUDE_CONFIG_DIR"),
            Some("/cfg/model-gateway/claude-config")
        );
        assert_eq!(get(BASE_URL_ENV), Some("https://gw.example.com"));
        assert_eq!(get(CUSTOM_HEADERS_ENV), Some("X-Tenant: t1"));
        for cred in SHADOWING_CREDENTIAL_ENV {
            assert!(plan.remove.iter().any(|r| r == cred), "{cred} not removed");
        }
        // No secret value is ever placed in the env plan.
        assert!(plan.set.iter().all(|(k, _)| !k.contains("API_KEY")));
    }

    #[test]
    fn child_env_plan_fails_closed_on_invalid_declaration() {
        let s = decl("http://gateway.example.com");
        assert!(s.resolve().is_err());
        let plan = child_env_plan(&s, Some("/cfg"));
        let base = plan
            .set
            .iter()
            .find(|(k, _)| k == BASE_URL_ENV)
            .map(|(_, v)| v.clone());
        assert_eq!(base.as_deref(), Some("http://gateway.example.com"));
        assert!(plan
            .set
            .iter()
            .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == "/cfg"));
        assert!(plan.remove.iter().any(|r| r == "ANTHROPIC_API_KEY"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn api_key_helper_output_is_trimmed() {
        assert_eq!(
            run_api_key_helper("printf '  key-123\\n'", Duration::from_secs(5)).unwrap(),
            "key-123"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn api_key_helper_failures_are_errors_without_stderr() {
        let err =
            run_api_key_helper("echo sk-leaked 1>&2; exit 3", Duration::from_secs(5)).unwrap_err();
        assert!(!err.contains("sk-leaked"), "{err}");
        assert!(run_api_key_helper("true", Duration::from_secs(5)).is_err());
        let err = run_api_key_helper("sleep 5", Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
    }

    #[test]
    fn vendor_call_uses_vendor_host_and_token() {
        let call = ModelCall::resolve_with(&ModelGatewaySettings::default(), || {
            Ok("sk-ant-api03-x".into())
        })
        .unwrap();
        assert!(!call.is_gateway());
        assert_eq!(
            call.url(MESSAGES_PATH),
            "https://api.anthropic.com/v1/messages"
        );
        let req = call
            .post_blocking(&reqwest::blocking::Client::new(), MESSAGES_PATH)
            .build()
            .unwrap();
        assert_eq!(req.headers()["x-api-key"], "sk-ant-api03-x");
    }

    #[test]
    fn gateway_call_never_consults_the_vendor_token() {
        let call = ModelCall::resolve_with(&decl("https://gw.example.com"), || {
            panic!("vendor token must not be read for a gateway call")
        })
        .unwrap();
        assert!(call.is_gateway());
        let req = call
            .post_async(&reqwest::Client::new(), MESSAGES_PATH)
            .build()
            .unwrap();
        assert_eq!(req.url().as_str(), "https://gw.example.com/v1/messages");
        assert!(req.headers().get("x-api-key").is_none());
        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn invalid_gateway_refuses_instead_of_falling_back() {
        let err =
            ModelCall::resolve_with(&decl("http://gw.example.com"), || Ok("t".into())).unwrap_err();
        assert!(err.contains("https"), "{err}");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn gateway_call_reaches_the_configured_server_with_headers_and_helper_key() {
        let (base, server) = one_shot_messages_server("routed");
        let mut s = decl(&format!("{base}/anthropic"));
        s.headers.insert("X-Tenant".into(), "t1".into());
        s.api_key_helper = Some("echo gw-key-1".into());
        let call = ModelCall::resolve_with(&s, || panic!("vendor token read")).unwrap();
        let resp = call
            .post_blocking(&reqwest::blocking::Client::new(), MESSAGES_PATH)
            .header("anthropic-version", "2023-06-01")
            .json(&serde_json::json!({"model":"m","max_tokens":1,"messages":[]}))
            .send()
            .unwrap();
        assert!(resp.status().is_success());
        let lines = server.join().unwrap();
        assert_eq!(lines[0], "POST /anthropic/v1/messages HTTP/1.1");
        let has = |want: &str| lines.iter().any(|l| l.eq_ignore_ascii_case(want));
        assert!(has("x-tenant: t1"), "{lines:?}");
        assert!(has("x-api-key: gw-key-1"), "{lines:?}");
        assert!(has("authorization: Bearer gw-key-1"), "{lines:?}");
    }
}
