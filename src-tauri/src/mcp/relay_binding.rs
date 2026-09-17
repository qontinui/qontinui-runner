//! Principal binding for the UI Bridge relay registrant routes.
//!
//! Plan `2026-09-17-ui-bridge-relay-registration-is-unauthenticated` (coord
//! finding `8f142485`; design-fork finding `bc060284`). The relay routes are on
//! `origin_guard::FOREIGN_ROUTES` because real registrants run under arbitrary
//! page origins, so the guard cannot answer WHO is registering. This module is
//! where that answer lives: a registration is bound to the principal the origin
//! guard already classified ([`RequesterPrincipal`]), and only that principal
//! (or operator trust) may displace, complete, heartbeat or deregister it.
//!
//! # Phase 0 (this state of the file): the seam, no behaviour change
//!
//! - [`RelayState`] — the slice of `ApiState` the relays need, extracted with
//!   `FromRef` so the relay handlers can be driven in tests without the
//!   `tauri::AppHandle` `ApiState` owns.
//! - [`BindingConfig`] / [`BindingMode`] — the two kill switches, read once at
//!   spawn by [`BindingConfig::from_env`] (the ONLY env reader; tests build a
//!   config directly and never set an env var).
//! - [`BindingCounters`] — per-rule `wouldRefuse` / `refused` counts, held on
//!   the one shared [`RelayBinding`] instance, never in a process global.
//!
//! Nothing here refuses anything yet. The rules (R1–R9, R-opaque) land in
//! Phases 1–3; `relay_binding/tests.rs` carries their acceptance tests,
//! `#[ignore]`d red until the phase that turns each one green.
//!
//! [`RequesterPrincipal`]: crate::mcp::origin_guard::RequesterPrincipal

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::FromRef;
use serde_json::{json, Value};

use super::app_dispatch::AppDispatcher;
use super::app_registry::AppRegistry;
use super::command_relay::CommandRelay;
use super::sdk_client::SdkConnectionManager;
use super::types::ApiState;
use super::ui_bridge::relay::RelayRegistry;
use super::ws_relay::WsConnectionManager;

/// Kill switch for R1–R5, R9 and R3's no-operator-trust-exemption clause.
pub const ENV_BINDING: &str = "QONTINUI_RUNNER_UIBRIDGE_BINDING";
/// Kill switch for the rules that change legitimate routing: R6, R8 and R9's
/// unkeyed cross-origin tab re-attach.
pub const ENV_ACTIVE_BINDING: &str = "QONTINUI_RUNNER_UIBRIDGE_ACTIVE_BINDING";

/// How one family of binding rules behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingMode {
    /// Refuse what the rule refuses (and count it as `refused`).
    Enforce,
    /// Compute the verdict, admit, log once per principal+rule, count
    /// `wouldRefuse`.
    Shadow,
    /// Today's behaviour: no verdict computed.
    Off,
}

impl BindingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Shadow => "shadow",
            Self::Off => "off",
        }
    }

    /// Parse an env value. Unset, empty or unrecognised → `default` (an
    /// unrecognised value says so).
    pub fn parse(raw: Option<&str>, default: Self, env_name: &str) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()) {
            None => default,
            Some(v) if v.is_empty() => default,
            Some(v) if v == "enforce" => Self::Enforce,
            Some(v) if v == "shadow" => Self::Shadow,
            Some(v) if v == "off" => Self::Off,
            Some(v) => {
                tracing::warn!(
                    value = %v,
                    default = default.as_str(),
                    "{env_name}: unrecognised value, using the default"
                );
                default
            }
        }
    }
}

/// The binding kill switches. Read once at spawn (never restart a runner to
/// apply one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingConfig {
    /// [`ENV_BINDING`]: R1–R5, R9, R3's no-exemption clause.
    pub binding: BindingMode,
    /// [`ENV_ACTIVE_BINDING`]: R6, R8, R9-unkeyed.
    pub active_binding: BindingMode,
}

impl Default for BindingConfig {
    /// The plan's graduated defaults: identity rules enforce, routing-changing
    /// rules shadow until Phase 4 graduation.
    fn default() -> Self {
        Self {
            binding: BindingMode::Enforce,
            active_binding: BindingMode::Shadow,
        }
    }
}

impl BindingConfig {
    /// Build from the raw values of [`ENV_BINDING`] and [`ENV_ACTIVE_BINDING`].
    pub fn from_values(binding: Option<&str>, active_binding: Option<&str>) -> Self {
        let d = Self::default();
        Self {
            binding: BindingMode::parse(binding, d.binding, ENV_BINDING),
            active_binding: BindingMode::parse(
                active_binding,
                d.active_binding,
                ENV_ACTIVE_BINDING,
            ),
        }
    }

    /// The production config: the process env, read now. The only env reader
    /// in this module.
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var(ENV_BINDING).ok().as_deref(),
            std::env::var(ENV_ACTIVE_BINDING).ok().as_deref(),
        )
    }
}

/// Per-rule outcome counts. Keys are rule ids (`R1` … `R9`, `R9-unkeyed`,
/// `R-opaque`).
#[derive(Debug, Default)]
pub struct BindingCounters {
    rules: Mutex<BTreeMap<&'static str, RuleCount>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RuleCount {
    pub would_refuse: u64,
    pub refused: u64,
}

// Phase 0 lands the counter store; the rules that record into it land in
// Phases 1–3, so outside tests nothing calls these yet.
#[cfg_attr(not(test), allow(dead_code))]
impl BindingCounters {
    /// Count one verdict for `rule`: `enforced` → `refused`, else `wouldRefuse`.
    pub fn record(&self, rule: &'static str, enforced: bool) {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let c = rules.entry(rule).or_default();
        if enforced {
            c.refused += 1;
        } else {
            c.would_refuse += 1;
        }
    }

    /// The counts for `rule` (zero when never recorded).
    pub fn get(&self, rule: &str) -> RuleCount {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        rules.get(rule).copied().unwrap_or_default()
    }

    fn rules_json(&self) -> Value {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = serde_json::Map::new();
        for (rule, c) in rules.iter() {
            out.insert(
                (*rule).to_string(),
                json!({ "wouldRefuse": c.would_refuse, "refused": c.refused }),
            );
        }
        Value::Object(out)
    }
}

/// Config plus counters: ONE instance per router, shared by every request as
/// an `Arc` on `ApiState` (and so on every [`RelayState`] extracted from it).
#[derive(Debug, Default)]
pub struct RelayBinding {
    pub config: BindingConfig,
    pub counters: BindingCounters,
}

#[cfg_attr(not(test), allow(dead_code))]
impl RelayBinding {
    pub fn new(config: BindingConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            counters: BindingCounters::default(),
        })
    }

    /// The `/health` `uiBridgeBinding` block (served from Phase 1).
    pub fn health_json(&self) -> Value {
        json!({
            "binding": self.config.binding.as_str(),
            "activeBinding": self.config.active_binding.as_str(),
            "bindingEnv": ENV_BINDING,
            "activeBindingEnv": ENV_ACTIVE_BINDING,
            "rules": self.counters.rules_json(),
        })
    }
}

/// What the UI Bridge relay handlers need, as `Arc` clones of `ApiState`'s
/// fields. Handlers take `State<RelayState>`; axum extracts it from
/// `Arc<ApiState>` through [`FromRef`], so production routing is unchanged,
/// while tests build one directly with [`RelayState::standalone`].
#[derive(Clone)]
pub struct RelayState {
    pub ws_connection_manager: Arc<WsConnectionManager>,
    pub ws_command_relay: Arc<CommandRelay>,
    pub app_registry: Arc<AppRegistry>,
    pub sdk_connection: Arc<tokio::sync::Mutex<SdkConnectionManager>>,
    pub ui_bridge_relay: Arc<RelayRegistry>,
    pub app_dispatcher: Arc<AppDispatcher>,
    // Read by the binding rules from Phase 1; in Phase 0 only the tests do.
    #[cfg_attr(not(test), allow(dead_code))]
    pub binding: Arc<RelayBinding>,
}

impl FromRef<Arc<ApiState>> for RelayState {
    fn from_ref(state: &Arc<ApiState>) -> Self {
        Self {
            ws_connection_manager: state.ws_connection_manager.clone(),
            ws_command_relay: state.ws_command_relay.clone(),
            app_registry: state.app_registry.clone(),
            sdk_connection: state.sdk_connection.clone(),
            ui_bridge_relay: state.ui_bridge_relay.clone(),
            app_dispatcher: state.app_dispatcher.clone(),
            binding: state.relay_binding.clone(),
        }
    }
}

impl RelayState {
    /// A fresh, self-contained relay state wired the way `create_router` wires
    /// `ApiState` (one registry, one WS manager, one command relay and one
    /// dispatcher over them), for tests.
    #[cfg(test)]
    pub fn standalone(config: BindingConfig) -> Self {
        let app_registry = AppRegistry::new();
        let ws_connection_manager = WsConnectionManager::new();
        let ws_command_relay = CommandRelay::new(ws_connection_manager.clone());
        let app_dispatcher = AppDispatcher::new(app_registry.clone(), ws_command_relay.clone());
        Self {
            ws_connection_manager,
            ws_command_relay,
            app_registry,
            sdk_connection: Arc::new(tokio::sync::Mutex::new(SdkConnectionManager::new())),
            ui_bridge_relay: Arc::new(RelayRegistry::new()),
            app_dispatcher,
            binding: RelayBinding::new(config),
        }
    }
}

#[cfg(test)]
mod tests;
