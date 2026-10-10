//! Model gateway: one place that decides where every model call goes.
//!
//! An install may declare a **model gateway**, a base URL that fronts the
//! Anthropic Messages API (the tenant's own gateway, or a vendor-run one).
//! The decision record `gateway-tenants-route-every-model-call-through-the-gateway`
//! says that once a gateway is declared, every model call goes through it:
//!
//! - **Spawned `claude` sessions** reach the model through the gateway. They run
//!   under a runner-owned `CLAUDE_CONFIG_DIR` ([`session_config_dir`]) whose
//!   `settings.json` carries the `apiKeyHelper` and the routing env
//!   (`ANTHROPIC_BASE_URL`, `ANTHROPIC_CUSTOM_HEADERS`). Every child-spawn seam
//!   applies [`live_child_env_plan`] last, which pins that dir and strips every
//!   env-borne credential and provider switch that could route around it.
//! - **The runner's own direct calls** (the `ai_provider` API paths, the
//!   connection test, the knowledge summarizer) take the gateway's base URL and
//!   headers instead of the vendor host ([`ModelCall`]).
//! - **Subscription accounts and account rotation are off.** The account
//!   picker, rate-limit rotation, usage probes and account migration all stand
//!   down while [`gateway_declared`] holds, and the non-Anthropic providers
//!   (Gemini, pi, OpenAI-compatible) are refused ([`provider_refusal`]).
//! - **The gateway host is allowed by the security profile** and the vendor
//!   host is dropped from it
//!   ([`crate::security::PolicyEngine::resolve_for_runtime`]).
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
//! prints the key on stdout. Claude Code runs it for spawned sessions; the
//! runner runs it for its own direct calls and caches the result for a TTL
//! ([`ModelGatewaySettings::api_key_helper_ttl_secs`], default 5 minutes). A
//! helper is REQUIRED unless the declaration sets `network_auth: true`, which
//! says the gateway authenticates by network position or mTLS. Extra headers are
//! for routing only: credential-shaped names are refused
//! ([`ModelGatewaySettings::validate`]).
//!
//! ## Fail closed: the three gateway states
//!
//! [`state`] is tri-state, and the third state is the point:
//!
//! - `NotDeclared` — no gateway; nothing here applies.
//! - `Declared` — `settings.json` declares one (valid or not).
//! - `Unknown` — the runner cannot tell: `settings.json` is unreadable, or it
//!   declares no gateway while the **sticky marker** (a runner-owned file
//!   written whenever a gateway is saved or materialized, and deleted ONLY when
//!   `save_model_gateway` clears the gateway) says this install had one. That is
//!   the shape of a settings reset to defaults.
//!
//! `Unknown` and an invalid `Declared` both resolve to [`Resolution::Unresolved`],
//! which behaves like a gateway that cannot be reached: [`ModelCall::resolve`]
//! errors, [`gateway_declared`] is true (so every subscription path stands
//! down), headless spawns are refused ([`spawn_refusal`]), and any other session
//! runs under the gateway config dir with vendor credentials stripped, an
//! unroutable base URL and a key helper that fails. Nothing falls back to the
//! vendor host.
//!
//! ## Limits (documented, not enforced here)
//!
//! Claude Code merges settings in precedence order, and a repository's own
//! `.claude/settings.json` / `.claude/settings.local.json` and the machine's
//! managed settings rank ABOVE the user-level settings the runner provisions in
//! the gateway config dir. Such a layer can still set `apiKeyHelper`, `env`
//! (`ANTHROPIC_BASE_URL`) or permissions. Governing those layers is the
//! tenant's managed-settings job; the runner cannot override them.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, warn};

/// The vendor endpoint used when no gateway is declared.
pub const VENDOR_API_BASE: &str = "https://api.anthropic.com";

/// The vendor API host, dropped from the security allow-list under a gateway.
pub const VENDOR_API_HOST: &str = "api.anthropic.com";

/// The Messages API path, appended to whichever base the call resolves to.
pub const MESSAGES_PATH: &str = "/v1/messages";

/// Env var Claude Code reads for its API base URL.
pub const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// Env var Claude Code reads for extra request headers (`Name: Value` lines).
pub const CUSTOM_HEADERS_ENV: &str = "ANTHROPIC_CUSTOM_HEADERS";

/// Env var that turns off Claude Code's non-essential traffic (telemetry, error
/// reporting, auto-update checks) — none of which goes through the gateway.
pub const NONESSENTIAL_TRAFFIC_ENV: &str = "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC";

/// Env var Claude Code reads for how long it caches an `apiKeyHelper` result.
pub const HELPER_TTL_ENV: &str = "CLAUDE_CODE_API_KEY_HELPER_TTL_MS";

/// Base URL an UNRESOLVED gateway session is pointed at. `.invalid` is reserved
/// (RFC 2606) and never resolves, so nothing — and no credential — leaves.
pub const UNRESOLVED_BASE_URL: &str = "https://model-gateway-unresolved.invalid";

/// Key helper an UNRESOLVED gateway session gets: it always fails, so the CLI
/// never authenticates (and never offers a subscription login in its place).
/// Parses under both `sh -c` and `cmd /C`.
pub const UNRESOLVED_KEY_HELPER: &str = "echo model-gateway-unresolved 1>&2 && exit 1";

/// Env-borne credentials Claude Code would prefer over `apiKeyHelper`. Each one
/// is stripped from a gateway session's child, so a key inherited from the
/// runner's environment can neither bypass the helper nor travel to the gateway.
pub const SHADOWING_CREDENTIAL_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// Env that switches Claude Code to another provider (or another provider's
/// endpoint) — each would route a gateway session around the gateway.
pub const PROVIDER_SWITCH_ENV: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
];

/// Header names refused in [`ModelGatewaySettings::headers`] (exact,
/// case-insensitive). Credentials ride the api-key-helper, never plaintext
/// settings.
const CREDENTIAL_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "cookie",
    "set-cookie",
];

/// Header-name SUFFIXES refused for the same reason (`x-auth-token`,
/// `x-client-secret`, `x-service-api-key`, …).
const CREDENTIAL_HEADER_SUFFIXES: &[&str] = &["-token", "-secret", "-api-key", "-password"];

/// How long the runner waits for the api-key-helper before giving up.
const API_KEY_HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// Default cache TTL for the helper's key on the runner's own calls.
pub const DEFAULT_HELPER_TTL_SECS: u64 = 300;

/// Subdirectory of the runner's config dir that holds the gateway state.
const GATEWAY_SUBDIR: &str = "model-gateway";

/// The gateway sessions' `CLAUDE_CONFIG_DIR`, under [`GATEWAY_SUBDIR`].
const SESSION_CONFIG_LEAF: &str = "claude-config";

/// The sticky "this install had a gateway" marker, under [`GATEWAY_SUBDIR`].
const MARKER_FILE: &str = "last-known-gateway.json";

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
    /// tags). Credential-shaped names are refused; see the module doc.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Shell command that prints the gateway API key on stdout. Required unless
    /// [`Self::network_auth`] is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_helper: Option<String>,
    /// `true` declares that the gateway authenticates by network position or
    /// mTLS, so no key helper is needed. Explicit on purpose: a gateway saved
    /// without a helper is otherwise refused, because a missing helper is far
    /// more often an omission than a design.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub network_auth: bool,
    /// How long the runner caches the helper's key for its own calls, and the
    /// TTL handed to Claude Code (`CLAUDE_CODE_API_KEY_HELPER_TTL_MS`). `None`
    /// means [`DEFAULT_HELPER_TTL_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_helper_ttl_secs: Option<u64>,
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
            network_auth: self.network_auth,
            api_key_helper_ttl_secs: self.api_key_helper_ttl_secs,
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
        if n.api_key_helper.is_none() && !n.network_auth {
            return Err(
                "model gateway needs an api-key-helper command; set `network_auth: true` only \
                 when the gateway authenticates by network position or mTLS"
                    .to_string(),
            );
        }
        if n.api_key_helper_ttl_secs == Some(0) {
            return Err("model gateway api_key_helper_ttl_secs must be at least 1".to_string());
        }

        let mut headers = Vec::with_capacity(n.headers.len());
        for (name, value) in &n.headers {
            if is_credential_header(name) {
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
            helper_ttl: Duration::from_secs(
                n.api_key_helper_ttl_secs.unwrap_or(DEFAULT_HELPER_TTL_SECS),
            ),
        }))
    }
}

fn is_credential_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CREDENTIAL_HEADER_NAMES.contains(&lower.as_str())
        || CREDENTIAL_HEADER_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
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
    helper_ttl: Duration,
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

    /// How long the helper's key is cached.
    pub fn helper_ttl(&self) -> Duration {
        self.helper_ttl
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

    /// The helper's key, from the TTL cache or a fresh helper run. `Ok(None)`
    /// when no helper is configured (`network_auth`). Blocking: async callers
    /// wrap it in a blocking task.
    pub fn api_key(&self) -> Result<Option<String>, String> {
        match self.api_key_helper.as_deref() {
            None => Ok(None),
            Some(cmd) => cached_helper_key(cmd, self.helper_ttl, || {
                run_api_key_helper(cmd, API_KEY_HELPER_TIMEOUT)
            })
            .map(Some),
        }
    }

    /// The `settings.json` written into the gateway sessions' config dir. It
    /// carries the helper and the routing env — the ONLY place the gateway URL
    /// reaches a session (see [`child_env_plan`]).
    pub fn session_settings_json(&self) -> serde_json::Value {
        let mut env = base_session_env();
        env.insert(BASE_URL_ENV.to_string(), self.base_url.clone().into());
        if let Some(h) = self.custom_headers_env() {
            env.insert(CUSTOM_HEADERS_ENV.to_string(), h.into());
        }
        if self.api_key_helper.is_some() {
            env.insert(
                HELPER_TTL_ENV.to_string(),
                self.helper_ttl.as_millis().to_string().into(),
            );
        }
        let mut root = serde_json::Map::new();
        if let Some(helper) = &self.api_key_helper {
            root.insert("apiKeyHelper".to_string(), helper.clone().into());
        }
        root.insert("env".to_string(), serde_json::Value::Object(env));
        serde_json::Value::Object(root)
    }
}

/// Env every gateway session's `settings.json` carries, valid or unresolved:
/// non-essential traffic off, and every provider switch forced off (a settings
/// layer cannot unset a variable, so `"0"` is the strongest it can say).
fn base_session_env() -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    env.insert(NONESSENTIAL_TRAFFIC_ENV.to_string(), "1".into());
    for var in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        env.insert(var.to_string(), "0".into());
    }
    env
}

/// The `settings.json` an UNRESOLVED gateway session gets: an unroutable base
/// URL and a key helper that always fails.
pub fn unresolved_session_settings_json() -> serde_json::Value {
    let mut env = base_session_env();
    env.insert(BASE_URL_ENV.to_string(), UNRESOLVED_BASE_URL.into());
    let mut root = serde_json::Map::new();
    root.insert("apiKeyHelper".to_string(), UNRESOLVED_KEY_HELPER.into());
    root.insert("env".to_string(), serde_json::Value::Object(env));
    serde_json::Value::Object(root)
}

/// Helper-key cache: command line → (fetched at, key). Errors are never cached.
static HELPER_KEY_CACHE: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);

/// The cached key for `cmd` if younger than `ttl`, else `fetch()` (cached on
/// success).
pub(crate) fn cached_helper_key(
    cmd: &str,
    ttl: Duration,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if let Ok(cache) = HELPER_KEY_CACHE.lock() {
        if let Some((at, key)) = cache.as_ref().and_then(|m| m.get(cmd)) {
            if at.elapsed() < ttl {
                return Ok(key.clone());
            }
        }
    }
    let key = fetch()?;
    if let Ok(mut cache) = HELPER_KEY_CACHE.lock() {
        cache
            .get_or_insert_with(HashMap::new)
            .insert(cmd.to_string(), (Instant::now(), key.clone()));
    }
    Ok(key)
}

/// Drop every cached helper key (a gateway save may change the helper's
/// meaning without changing its command line).
pub fn clear_helper_key_cache() {
    if let Ok(mut cache) = HELPER_KEY_CACHE.lock() {
        *cache = None;
    }
}

/// Run `cmd` through the platform shell and return its trimmed stdout.
///
/// The command line comes from the operator's own settings, the same trust
/// level as Claude Code's `apiKeyHelper`. Bounded by `timeout`: a helper that
/// hangs is killed and reported, never waited on forever. stderr is drained (a
/// chatty helper cannot block on a full pipe) and discarded — a failing helper
/// may print the very secret it was meant to hand over.
pub(crate) fn run_api_key_helper(cmd: &str, timeout: Duration) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut c = crate::process_helpers::no_window("cmd");
        // `raw_arg`: hand cmd.exe the operator's line verbatim instead of
        // letting Rust's MSVC-style quoting mangle its quotes.
        c.arg("/C").raw_arg(cmd);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut command = {
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
    let mut stderr = child.stderr.take();
    let drainer = std::thread::spawn(move || {
        if let Some(err) = stderr.as_mut() {
            let _ = std::io::copy(err, &mut std::io::sink());
        }
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
    let _ = drainer.join();
    if !status.success() {
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

/// What the runner knows about this install's gateway. See "Fail closed" in
/// the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayState {
    NotDeclared,
    Declared(ModelGatewaySettings),
    Unknown(String),
}

/// Classify the gateway state from a settings read. Pure, so the unreadable and
/// reset-to-defaults cases are testable without touching the live config dir.
pub fn classify_state(
    settings_readable: bool,
    read_error: Option<&str>,
    declaration: &ModelGatewaySettings,
    marker_present: bool,
) -> GatewayState {
    if !settings_readable {
        return GatewayState::Unknown(format!(
            "settings.json could not be read ({}), so whether this install routes model \
             calls through a gateway is unknown; failing closed",
            read_error.unwrap_or("unknown error")
        ));
    }
    if declaration.is_declared() {
        return GatewayState::Declared(declaration.clone());
    }
    if marker_present {
        return GatewayState::Unknown(
            "settings.json declares no model gateway, but this install recorded one (a settings \
             reset?); failing closed until the gateway is saved again or explicitly cleared in \
             the AI settings"
                .to_string(),
        );
    }
    GatewayState::NotDeclared
}

/// The live gateway state.
pub fn state() -> GatewayState {
    let loaded = crate::settings::read_settings_from_disk();
    let marker = gateway_dir()
        .map(|d| marker_present_at(&d))
        .unwrap_or(false);
    classify_state(
        loaded.is_authoritative(),
        loaded.error.as_deref(),
        &loaded.settings.model_gateway,
        marker,
    )
}

/// What a caller should do about the gateway right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No gateway: the vendor route and subscription accounts apply.
    NoGateway,
    /// A valid gateway: route through it.
    Gateway(ModelGateway),
    /// A gateway that cannot be resolved (invalid, or state unknown): refuse
    /// model calls, never fall back to the vendor host.
    Unresolved(String),
}

impl GatewayState {
    pub fn resolution(&self) -> Resolution {
        match self {
            GatewayState::NotDeclared => Resolution::NoGateway,
            GatewayState::Unknown(reason) => Resolution::Unresolved(reason.clone()),
            GatewayState::Declared(s) => match s.resolve() {
                Ok(Some(g)) => Resolution::Gateway(g),
                Ok(None) => Resolution::NoGateway,
                Err(e) => Resolution::Unresolved(e),
            },
        }
    }
}

/// The live [`Resolution`].
pub fn resolution() -> Resolution {
    state().resolution()
}

/// The live declaration from `settings.json` (an unreadable file reads as the
/// default, empty declaration — use [`state`] for decisions).
///
/// Read through the read-only settings loader: this is consulted at every spawn
/// seam and inside diagnostics, and `load_settings` is a writer door.
pub fn current_settings() -> ModelGatewaySettings {
    crate::settings::read_settings_from_disk()
        .settings
        .model_gateway
}

/// Whether a gateway is declared OR its state is unknown — the predicate every
/// "subscription accounts and rotation are off" gate reads. Fail closed: only a
/// positive `NotDeclared` lets a subscription path run.
pub fn gateway_declared() -> bool {
    !matches!(state(), GatewayState::NotDeclared)
}

/// The live, validated gateway. `Ok(None)` when none is declared; `Err` when one
/// is declared but cannot be resolved, or the state is unknown.
pub fn current() -> Result<Option<ModelGateway>, String> {
    match resolution() {
        Resolution::NoGateway => Ok(None),
        Resolution::Gateway(g) => Ok(Some(g)),
        Resolution::Unresolved(e) => Err(e),
    }
}

/// The refusal a headless spawn returns while the gateway is unresolved, or
/// `None` when a spawn may proceed.
pub fn spawn_refusal() -> Option<String> {
    match resolution() {
        Resolution::Unresolved(reason) => Some(format!(
            "refusing to start a claude session: the model gateway cannot be resolved ({reason})"
        )),
        _ => None,
    }
}

/// The refusal for a provider that cannot route through the Anthropic-shaped
/// gateway (review M5): Gemini (API or CLI), pi and OpenAI-compatible. `None`
/// when the provider may run.
pub fn provider_refusal(provider: &crate::settings::AiProvider) -> Option<String> {
    use crate::settings::AiProvider;
    let blocked = matches!(
        provider,
        AiProvider::GeminiApi
            | AiProvider::GeminiCli
            | AiProvider::PiCli
            | AiProvider::OpenAiCompatible
    );
    if blocked && gateway_declared() {
        Some(format!(
            "the {provider:?} provider is refused while a model gateway is configured: every \
             model call must go through the gateway, and this provider does not speak its \
             protocol. Switch the AI provider to Claude CLI or Claude API, or clear the gateway."
        ))
    } else {
        None
    }
}

/// The runner-owned gateway dir (`<config dir>/model-gateway`).
fn gateway_dir() -> Result<PathBuf, String> {
    let (dir, _source) = crate::settings::resolve_config_dir()?;
    Ok(dir.join(GATEWAY_SUBDIR))
}

/// The `CLAUDE_CONFIG_DIR` every gateway session runs under: a runner-owned dir
/// beside `settings.json`, so each install (primary, secondary, temp) has its
/// own. It holds no subscription credentials by construction.
pub fn session_config_dir() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join(SESSION_CONFIG_LEAF))
}

pub(crate) fn marker_present_at(gateway_dir: &Path) -> bool {
    gateway_dir.join(MARKER_FILE).exists()
}

pub(crate) fn write_marker_at(gateway_dir: &Path, gateway: &ModelGateway) -> Result<(), String> {
    std::fs::create_dir_all(gateway_dir)
        .map_err(|e| format!("create {}: {e}", gateway_dir.display()))?;
    let body = serde_json::json!({ "base_url": gateway.base_url(), "host": gateway.host() });
    let path = gateway_dir.join(MARKER_FILE);
    let text = body.to_string();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn clear_marker_at(gateway_dir: &Path) -> Result<(), String> {
    let path = gateway_dir.join(MARKER_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Record a just-saved declaration in the sticky marker: written for a valid
/// gateway, deleted for a cleared one. Called ONLY by the save path — the one
/// place an operator says "no gateway" on purpose.
pub fn record_saved_declaration(saved: &ModelGatewaySettings) -> Result<(), String> {
    clear_helper_key_cache();
    let dir = gateway_dir()?;
    match saved.resolve()? {
        Some(gateway) => write_marker_at(&dir, &gateway),
        None => clear_marker_at(&dir),
    }
}

/// Write the gateway sessions' config dir for `resolution`: `settings.json`
/// (helper and routing env, or the unresolved trap) and a `.claude.json` that
/// marks onboarding done. A valid gateway also refreshes the sticky marker.
pub fn materialize_session_config(resolution: &Resolution) -> Result<PathBuf, String> {
    let gdir = gateway_dir()?;
    let dir = gdir.join(SESSION_CONFIG_LEAF);
    let settings = match resolution {
        Resolution::NoGateway => return Ok(dir),
        Resolution::Gateway(g) => {
            write_marker_at(&gdir, g)?;
            g.session_settings_json()
        }
        Resolution::Unresolved(_) => unresolved_session_settings_json(),
    };
    materialize_session_config_at(&dir, &settings)?;
    Ok(dir)
}

pub(crate) fn materialize_session_config_at(
    dir: &Path,
    settings: &serde_json::Value,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let settings_path = dir.join("settings.json");
    let body = serde_json::to_string_pretty(settings)
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
/// is declared. Materializes the dir as a side effect. An unresolved gateway
/// still returns the dir (holding the unresolved trap), so the session runs
/// with no subscription account.
pub fn session_config_dir_override() -> Option<String> {
    session_config_dir_for(&resolution())
}

fn session_config_dir_for(resolution: &Resolution) -> Option<String> {
    if matches!(resolution, Resolution::NoGateway) {
        return None;
    }
    if let Err(e) = materialize_session_config(resolution) {
        error!("model gateway: could not write the session config dir: {e}");
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

/// Build the [`ChildEnvPlan`] for a resolution and its session config dir.
///
/// A VALID gateway's URL and headers are deliberately NOT put in the process
/// env (review H1): they ride the gateway config dir's `settings.json`, which
/// only a session running under that dir loads. An inherited copy is stripped.
/// So a command typed with some other `CLAUDE_CONFIG_DIR` can never pair that
/// dir's subscription token with the gateway URL. An UNRESOLVED gateway's
/// unroutable URL IS put in the env, because sending a request nowhere is the
/// point.
pub fn child_env_plan_for(
    resolution: &Resolution,
    session_config_dir: Option<&str>,
) -> ChildEnvPlan {
    if matches!(resolution, Resolution::NoGateway) {
        return ChildEnvPlan::default();
    }
    let mut plan = ChildEnvPlan::default();
    plan.remove.extend(
        SHADOWING_CREDENTIAL_ENV
            .iter()
            .chain(PROVIDER_SWITCH_ENV)
            .map(|s| s.to_string()),
    );
    plan.remove.push(CUSTOM_HEADERS_ENV.to_string());
    if let Some(dir) = session_config_dir {
        plan.set
            .push(("CLAUDE_CONFIG_DIR".to_string(), dir.to_string()));
    }
    plan.set
        .push((NONESSENTIAL_TRAFFIC_ENV.to_string(), "1".to_string()));
    match resolution {
        Resolution::Unresolved(_) => plan
            .set
            .push((BASE_URL_ENV.to_string(), UNRESOLVED_BASE_URL.to_string())),
        _ => plan.remove.push(BASE_URL_ENV.to_string()),
    }
    plan
}

/// [`child_env_plan_for`] over a declaration (tests and callers that hold
/// settings rather than a resolution).
pub fn child_env_plan(
    settings: &ModelGatewaySettings,
    session_config_dir: Option<&str>,
) -> ChildEnvPlan {
    let resolution = classify_state(true, None, settings, false).resolution();
    child_env_plan_for(&resolution, session_config_dir)
}

/// The [`ChildEnvPlan`] for the live state. Applied by the three
/// `crate::terminal::scrub_credential_env_*` wrappers, which run last at every
/// spawn seam, so the gateway config dir overrides any subscription-account pin
/// set earlier in the seam. Empty when no gateway is declared.
pub(crate) fn live_child_env_plan() -> ChildEnvPlan {
    let resolution = resolution();
    if matches!(resolution, Resolution::NoGateway) {
        return ChildEnvPlan::default();
    }
    let dir = session_config_dir_for(&resolution);
    child_env_plan_for(&resolution, dir.as_deref())
}

// ---------------------------------------------------------------------------
// Direct model calls
// ---------------------------------------------------------------------------

/// Where one direct model call goes, plus the credential it carries.
///
/// Every runner-side call to the Messages API builds its request through this
/// type, so the vendor host appears in exactly one place ([`VENDOR_API_BASE`]).
/// `Debug` is hand-written: it never prints a credential.
#[derive(Clone)]
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

impl std::fmt::Debug for ModelCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelCall::Vendor { .. } => f
                .debug_struct("Vendor")
                .field("token", &"<redacted>")
                .finish(),
            ModelCall::Gateway { gateway, key } => f
                .debug_struct("Gateway")
                .field("base_url", &gateway.base_url())
                .field("key", &key.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// Blocking client for gateway calls: redirects are NOT followed (review L6),
/// so a gateway answer cannot bounce the key to another host.
fn gateway_blocking_client() -> &'static reqwest::blocking::Client {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|e| {
                warn!("model gateway: blocking client builder failed ({e}); using a bare client");
                reqwest::blocking::Client::new()
            })
    })
}

/// Async twin of [`gateway_blocking_client`].
fn gateway_async_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|e| {
                warn!("model gateway: async client builder failed ({e}); using a bare client");
                reqwest::Client::new()
            })
    })
}

impl ModelCall {
    /// Resolve the call for the live state. `vendor_token` is consulted only
    /// when no gateway is declared. A gateway call never sends a vendor or
    /// subscription credential; an unresolved gateway refuses.
    pub fn resolve(vendor_token: impl FnOnce() -> Result<String, String>) -> Result<Self, String> {
        Self::resolve_for(&resolution(), vendor_token)
    }

    /// [`Self::resolve`] over an explicit declaration (tests, callers that
    /// already hold settings).
    pub fn resolve_with(
        settings: &ModelGatewaySettings,
        vendor_token: impl FnOnce() -> Result<String, String>,
    ) -> Result<Self, String> {
        Self::resolve_for(
            &classify_state(true, None, settings, false).resolution(),
            vendor_token,
        )
    }

    /// [`Self::resolve`] over an explicit [`Resolution`].
    pub fn resolve_for(
        resolution: &Resolution,
        vendor_token: impl FnOnce() -> Result<String, String>,
    ) -> Result<Self, String> {
        match resolution {
            Resolution::NoGateway => Ok(ModelCall::Vendor {
                token: vendor_token()?,
            }),
            Resolution::Gateway(gateway) => {
                let key = gateway.api_key()?;
                Ok(ModelCall::Gateway {
                    gateway: gateway.clone(),
                    key,
                })
            }
            Resolution::Unresolved(reason) => Err(format!(
                "model call refused: the model gateway cannot be resolved ({reason})"
            )),
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
    /// Callers add `anthropic-version`, betas, content type and body. A vendor
    /// call uses the caller's `client`; a gateway call uses the runner's
    /// no-redirect gateway client.
    pub fn post_blocking(
        &self,
        client: &reqwest::blocking::Client,
        path: &str,
    ) -> reqwest::blocking::RequestBuilder {
        match self {
            ModelCall::Vendor { token } => crate::ai_provider::anthropic_auth::apply_blocking(
                client.post(self.url(path)),
                token,
            ),
            ModelCall::Gateway { gateway, key } => {
                let mut b = gateway_blocking_client().post(self.url(path));
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
        match self {
            ModelCall::Vendor { token } => {
                crate::ai_provider::anthropic_auth::apply_async(client.post(self.url(path)), token)
            }
            ModelCall::Gateway { gateway, key } => {
                let mut b = gateway_async_client().post(self.url(path));
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

    /// Whether a 429 on this call may mark a subscription account rate-limited
    /// (review L5): only a vendor call spends a subscription account's quota.
    pub fn may_mark_account_rate_limited(&self) -> bool {
        !self.is_gateway()
    }
}

/// The refusal a subscription-account path returns while a gateway is declared.
pub const SUBSCRIPTION_OFF_REASON: &str =
    "a model gateway is configured (or its state is unknown): subscription accounts and account \
     rotation are off";

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
        let reply =
            serde_json::json!({"content": [{"type": "text", "text": reply_text}]}).to_string();
        one_shot_server(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            reply.len(),
            reply
        ))
    }

    /// One-shot HTTP server answering with `raw_response` verbatim.
    pub(crate) fn one_shot_server(
        raw_response: String,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
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
            stream.write_all(raw_response.as_bytes()).unwrap();
            lines
        });
        (format!("http://{addr}"), handle)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{one_shot_messages_server, one_shot_server};
    use super::*;

    /// A declaration with `network_auth` set, so it resolves without a helper
    /// unless a test says otherwise.
    fn decl(base: &str) -> ModelGatewaySettings {
        ModelGatewaySettings {
            base_url: Some(base.to_string()),
            network_auth: true,
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
        assert_eq!(v["env"][NONESSENTIAL_TRAFFIC_ENV], "1");
        assert_eq!(v["env"]["CLAUDE_CODE_USE_BEDROCK"], "0");
        assert_eq!(v["env"][HELPER_TTL_ENV], "300000");

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
        let g = s.resolve().unwrap().unwrap().session_settings_json();
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

    /// Review M4: gateway sessions shed every provider switch that could route
    /// around the gateway, and turn off non-essential traffic.
    #[test]
    fn child_env_plan_strips_provider_switches_and_disables_nonessential_traffic() {
        let mut s = decl("https://gw.example.com");
        s.api_key_helper = Some("echo k".into());
        let plan = child_env_plan(&s, Some("/cfg"));
        for var in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_BEDROCK_BASE_URL",
            "ANTHROPIC_VERTEX_BASE_URL",
            "ANTHROPIC_FOUNDRY_BASE_URL",
        ] {
            assert!(plan.remove.iter().any(|r| r == var), "{var} not stripped");
        }
        assert!(plan
            .set
            .iter()
            .any(|(k, v)| k == "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" && v == "1"));
    }

    /// Review H1 (defence in depth): a VALID gateway's routing env rides the
    /// gateway config dir's settings.json, not the process env — so a typed
    /// `CLAUDE_CONFIG_DIR=<subscription dir>` override cannot pair a
    /// subscription token with the gateway URL. The inherited copies are
    /// stripped.
    #[test]
    fn child_env_plan_keeps_the_gateway_url_out_of_the_process_env() {
        let mut s = decl("https://gw.example.com");
        s.api_key_helper = Some("echo k".into());
        s.headers.insert("X-Tenant".into(), "t1".into());
        let plan = child_env_plan(&s, Some("/cfg"));
        assert!(!plan
            .set
            .iter()
            .any(|(k, _)| k == BASE_URL_ENV || k == CUSTOM_HEADERS_ENV));
        assert!(plan.remove.iter().any(|r| r == BASE_URL_ENV));
        assert!(plan.remove.iter().any(|r| r == CUSTOM_HEADERS_ENV));
    }

    /// Review M4: a gateway must name an api-key-helper unless it explicitly
    /// declares network authentication.
    #[test]
    fn helper_is_required_unless_network_auth_is_declared() {
        let mut s = decl("https://gw.example.com");
        s.network_auth = false;
        let err = s.resolve().unwrap_err();
        assert!(err.contains("api-key-helper"), "{err}");
        s.network_auth = true;
        assert!(s.resolve().unwrap().is_some());
        s.network_auth = false;
        s.api_key_helper = Some("echo k".into());
        assert!(s.resolve().unwrap().is_some());
    }

    /// Review L4: the credential-header denylist covers the common secret
    /// carriers, not only the three Anthropic ones.
    #[test]
    fn extended_credential_headers_are_refused() {
        for name in [
            "api-key",
            "X-Goog-Api-Key",
            "Cookie",
            "X-Auth-Token",
            "X-Client-Secret",
        ] {
            let mut s = decl("https://gw.example.com");
            s.network_auth = true;
            s.headers.insert(name.to_string(), "v".to_string());
            assert!(s.resolve().is_err(), "{name} accepted");
        }
    }

    /// Review L3: no credential appears in a ModelCall's Debug output.
    #[test]
    fn model_call_debug_redacts_credentials() {
        let vendor = ModelCall::Vendor {
            token: "sk-ant-api03-SECRET".into(),
        };
        assert!(!format!("{vendor:?}").contains("SECRET"));
        let mut s = decl("https://gw.example.com");
        s.network_auth = true;
        let gw = ModelCall::Gateway {
            gateway: s.resolve().unwrap().unwrap(),
            key: Some("gw-SECRET".into()),
        };
        assert!(!format!("{gw:?}").contains("SECRET"));
    }

    /// Review H2: an unreadable settings.json is UNKNOWN, never "no gateway".
    #[test]
    fn unparseable_settings_fail_closed() {
        let state = classify_state(
            false,
            Some("expected value at line 1"),
            &Default::default(),
            false,
        );
        let GatewayState::Unknown(reason) = &state else {
            panic!("unreadable settings must be Unknown, got {state:?}");
        };
        assert!(reason.contains("expected value at line 1"), "{reason}");
        assert!(matches!(state.resolution(), Resolution::Unresolved(_)));
        // A model call refuses rather than falling back to the vendor host.
        let err = ModelCall::resolve_for(&state.resolution(), || Ok("vendor".into())).unwrap_err();
        assert!(err.contains("cannot be resolved"), "{err}");
    }

    /// Review H2: settings reset to defaults while the sticky marker says this
    /// install had a gateway is UNKNOWN, not "no gateway".
    #[test]
    fn reset_to_defaults_with_the_marker_fails_closed() {
        let state = classify_state(true, None, &ModelGatewaySettings::default(), true);
        assert!(matches!(state, GatewayState::Unknown(_)), "{state:?}");
        let plan = child_env_plan_for(&state.resolution(), Some("/cfg"));
        assert!(plan
            .set
            .iter()
            .any(|(k, v)| k == BASE_URL_ENV && v == UNRESOLVED_BASE_URL));
        assert!(plan.remove.iter().any(|r| r == "ANTHROPIC_API_KEY"));
        // Without the marker, the same settings are positively "no gateway".
        assert_eq!(
            classify_state(true, None, &ModelGatewaySettings::default(), false),
            GatewayState::NotDeclared
        );
    }

    /// The sticky marker is written for a valid gateway and removed only by an
    /// explicit clear.
    #[test]
    fn marker_is_written_and_cleared_explicitly() {
        let tmp = tempfile::tempdir().unwrap();
        let gdir = tmp.path().join("model-gateway");
        assert!(!marker_present_at(&gdir));
        let g = decl("https://gw.example.com").resolve().unwrap().unwrap();
        write_marker_at(&gdir, &g).unwrap();
        assert!(marker_present_at(&gdir));
        clear_marker_at(&gdir).unwrap();
        assert!(!marker_present_at(&gdir));
        // Clearing an absent marker is not an error.
        clear_marker_at(&gdir).unwrap();
    }

    /// An unresolved session gets an unroutable URL and a helper that fails.
    #[test]
    fn unresolved_session_settings_trap_the_session() {
        let v = unresolved_session_settings_json();
        assert_eq!(v["env"][BASE_URL_ENV], UNRESOLVED_BASE_URL);
        assert_eq!(v["apiKeyHelper"], UNRESOLVED_KEY_HELPER);
        assert!(UNRESOLVED_BASE_URL.contains(".invalid"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn the_unresolved_key_helper_always_fails() {
        assert!(run_api_key_helper(UNRESOLVED_KEY_HELPER, Duration::from_secs(5)).is_err());
    }

    /// Review L2: the helper's key is cached for the TTL; errors are not.
    #[test]
    fn helper_key_is_cached_for_the_ttl_and_errors_are_not() {
        let cmd = "test-helper-cache-unique-cmd";
        let mut calls = 0;
        let mut fetch = |out: Result<String, String>| {
            calls += 1;
            out
        };
        assert!(
            cached_helper_key(cmd, Duration::from_secs(60), || fetch(Err("boom".into()))).is_err()
        );
        assert_eq!(
            cached_helper_key(cmd, Duration::from_secs(60), || fetch(Ok("k1".into()))).unwrap(),
            "k1"
        );
        // Within the TTL: served from cache, fetch not called.
        assert_eq!(
            cached_helper_key(cmd, Duration::from_secs(60), || panic!("refetched")).unwrap(),
            "k1"
        );
        assert_eq!(calls, 2);
        // A zero TTL refetches.
        assert_eq!(
            cached_helper_key(cmd, Duration::ZERO, || Ok("k2".into())).unwrap(),
            "k2"
        );
        assert!(decl("https://gw.example.com")
            .clone()
            .tap_ttl(Some(0))
            .resolve()
            .is_err());
    }

    /// Review L5: only a vendor call may mark a subscription account limited.
    #[test]
    fn only_vendor_calls_mark_accounts_rate_limited() {
        let vendor = ModelCall::Vendor { token: "t".into() };
        assert!(vendor.may_mark_account_rate_limited());
        let gw = ModelCall::resolve_with(&decl("https://gw.example.com"), || {
            panic!("vendor token read")
        })
        .unwrap();
        assert!(!gw.may_mark_account_rate_limited());
    }

    /// Review L6: a gateway answer's redirect is not followed, so the key is
    /// never re-sent to wherever the redirect points.
    #[test]
    fn gateway_calls_do_not_follow_redirects() {
        let (base, server) = one_shot_server(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:1/elsewhere\r\n\
             content-length: 0\r\nconnection: close\r\n\r\n"
                .to_string(),
        );
        let mut s = decl(&base);
        s.network_auth = true;
        let call = ModelCall::resolve_with(&s, || panic!("vendor token read")).unwrap();
        let resp = call
            .post_blocking(&reqwest::blocking::Client::new(), MESSAGES_PATH)
            .body("{}")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 307);
        server.join().unwrap();
    }

    trait TapTtl {
        fn tap_ttl(self, ttl: Option<u64>) -> Self;
    }
    impl TapTtl for ModelGatewaySettings {
        fn tap_ttl(mut self, ttl: Option<u64>) -> Self {
            self.api_key_helper_ttl_secs = ttl;
            self
        }
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
        // The URL and headers ride the dir's settings.json, not the env.
        assert_eq!(get(BASE_URL_ENV), None);
        assert_eq!(get(CUSTOM_HEADERS_ENV), None);
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
        // Never the declared (invalid) URL, never the vendor: nowhere.
        assert_eq!(base.as_deref(), Some(UNRESOLVED_BASE_URL));
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
