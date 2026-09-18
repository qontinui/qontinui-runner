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
//! # The seam
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
//! # Phase 1 (this state of the file)
//!
//! [`Principal`], [`Principal::same`], [`Refusal`], the tombstone map and
//! [`RelayBinding::meter`] land here, and the WebSocket relay plus the app
//! registry consult them (R1, R2, R3-WS, R4, R5, R-opaque).
//! [`RelayBinding::health_json`] is served by the production `/health`
//! handler. The HTTP relay tabs (R1/R3/R5 for tabs, R9) are Phase 2, and the
//! active-connection rules (R6, R7, R8) are Phase 3; `relay_binding/tests.rs`
//! carries their acceptance tests, `#[ignore]`d red until then.
//!
//! [`RequesterPrincipal`]: crate::mcp::origin_guard::RequesterPrincipal

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use axum::extract::FromRef;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::app_dispatch::AppDispatcher;
use super::app_registry::AppRegistry;
use super::command_relay::CommandRelay;
use super::origin_guard::{NormOrigin, OriginClass, RequesterPrincipal};
use super::sdk_client::SdkConnectionManager;
use super::types::{api_error, ApiResponse, ApiState};
use super::ui_bridge::relay::RelayRegistry;
use super::ws_relay::WsConnectionManager;

// ---------------------------------------------------------------------------
// Denial payloads (security-surface content trigger #6)
// ---------------------------------------------------------------------------

/// A live `appId` / `tabId` is held by a different principal (R1, R2, R5).
pub const CODE_REGISTRATION_HELD: &str = "UIB_REGISTRATION_HELD";
/// A browser registration declared an origin, `baseUrl` or transport its
/// header origin does not support (R4).
pub const CODE_ORIGIN_MISMATCH: &str = "UIB_ORIGIN_MISMATCH";
/// A browser-class request with no parseable, non-`null` `Origin` and no tab
/// key: no principal at all (R-opaque).
pub const CODE_OPAQUE_ORIGIN: &str = "UIB_OPAQUE_ORIGIN";
/// A result was posted by someone other than the connection or tab the
/// command was routed to (R3).
pub const CODE_COMMAND_NOT_YOURS: &str = "UIB_COMMAND_NOT_YOURS";

pub const RULE_R1: &str = "R1";
pub const RULE_R2: &str = "R2";
pub const RULE_R3: &str = "R3";
pub const RULE_R4: &str = "R4";
pub const RULE_R5: &str = "R5";
pub const RULE_OPAQUE: &str = "R-opaque";

/// How long a browser principal's released `appId` / `tabId` stays reserved
/// for it (R5). Closes the reload race: a tab reloads, and an attacker
/// polling `/ui-bridge/apps/registered` must not get the id first.
pub const BINDING_TOMBSTONE_MS: i64 = 60_000;

/// Ceiling on the tombstone map, so a page spraying fresh ids cannot grow it
/// without bound. Expired entries go first, then the soonest to expire.
const MAX_TOMBSTONES: usize = 1024;

/// How many `(rule, class, route)` tuples `/health` reports.
const MAX_RECENT_TUPLES: usize = 20;

/// Ceiling on the "log this WARN once per principal+rule" set.
const MAX_LOGGED_SIGHTINGS: usize = 512;

/// A refused relay operation: the code, the rule that refused it, and one
/// sentence. Deliberately carries NO holder origin and NO holder class — the
/// holder's existence is already public through `GET /ui-bridge/apps/registered`
/// and `GET /ui-bridge/tabs`, and the refusal reveals nothing beyond that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub rule: &'static str,
    pub message: &'static str,
}

impl Refusal {
    pub const fn registration_held(rule: &'static str) -> Self {
        Self {
            code: CODE_REGISTRATION_HELD,
            rule,
            message: "This appId is held by a different principal; only its holder or an operator-trust caller may claim or release it",
        }
    }

    pub const fn origin_mismatch(message: &'static str) -> Self {
        Self {
            code: CODE_ORIGIN_MISMATCH,
            rule: RULE_R4,
            message,
        }
    }

    pub const fn opaque_origin() -> Self {
        Self {
            code: CODE_OPAQUE_ORIGIN,
            rule: RULE_OPAQUE,
            message: "A browser request with no parseable, non-null Origin has no principal and cannot register on this route",
        }
    }

    pub const fn command_not_yours() -> Self {
        Self {
            code: CODE_COMMAND_NOT_YOURS,
            rule: RULE_R3,
            message: "This command was routed to a different connection or tab",
        }
    }

    /// `409` for "someone else holds it", `403` for "you are not who you say".
    pub fn status(&self) -> StatusCode {
        match self.code {
            CODE_REGISTRATION_HELD => StatusCode::CONFLICT,
            _ => StatusCode::FORBIDDEN,
        }
    }

    /// The HTTP shape: the canonical error envelope with `code` set, so a
    /// caller reads `body.code` the way it does for every other refusal.
    pub fn into_http(self) -> (StatusCode, Json<ApiResponse<()>>) {
        let status = self.status();
        let mut body = api_error(format!("{} [{}]", self.message, self.rule));
        body.code = Some(self.code.to_string());
        (status, Json(body))
    }

    /// The WebSocket shape. `LiveSessionTransport.handleMessage` already maps
    /// an `ack ok:false` to a `WrapperTransportError` carrying this `code`, so
    /// no client change is needed to surface it.
    pub fn ws_ack(&self) -> Value {
        json!({
            "type": "ack",
            "ok": false,
            "error": { "code": self.code, "message": self.message, "rule": self.rule },
        })
    }
}

// ---------------------------------------------------------------------------
// Principal
// ---------------------------------------------------------------------------

/// WHO made a relay request, derived from the headers the origin guard already
/// classified. This is the whole of design B: an `appId` in a frame stops
/// being evidence of identity, and no registrant has to hold a new credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// `NonBrowser` (agents, curl, Node wrappers) or `FirstParty` (the runner's
    /// own webview). Both already hold full local API trust, so both may
    /// displace anything.
    OperatorTrust { class: OriginClass },
    /// A browser page, identified by the `Origin` header it cannot forge.
    Browser {
        class: OriginClass,
        origin: NormOrigin,
    },
    /// R9: a tab that presented `X-UI-Bridge-Tab-Key`. Bound to the key's
    /// digest whatever its origin, because a pinned injected tab crosses
    /// origins by design. Consulted by the tab routes in Phase 2.
    TabKey { digest: String },
    /// A browser-class request with no usable `Origin`. It has NO principal:
    /// it is `same` as nothing, not even another `Opaque`. Reached only under
    /// `shadow` / `off`, where R-opaque does not refuse.
    Opaque { class: OriginClass },
}

impl Principal {
    /// Resolve from the guard's classification, optionally with a tab key.
    ///
    /// `None` (no guard extension at all) is operator trust: the guard is
    /// applied to the whole router, so the only way to reach a handler without
    /// it is a caller that never crossed a browser boundary.
    pub fn resolve(
        requester: Option<&RequesterPrincipal>,
        tab_key: Option<&str>,
    ) -> Result<Self, Refusal> {
        let Some(r) = requester else {
            return Ok(Self::OperatorTrust {
                class: OriginClass::NonBrowser,
            });
        };
        match r.class {
            OriginClass::NonBrowser | OriginClass::FirstParty => {
                Ok(Self::OperatorTrust { class: r.class })
            }
            OriginClass::Trusted | OriginClass::Extension | OriginClass::Foreign => {
                if let Some(key) = tab_key.map(str::trim).filter(|k| !k.is_empty()) {
                    return Ok(Self::TabKey {
                        digest: key_digest(key),
                    });
                }
                match &r.origin {
                    Some(o) => Ok(Self::Browser {
                        class: r.class,
                        origin: o.clone(),
                    }),
                    None => Err(Refusal::opaque_origin()),
                }
            }
        }
    }

    pub fn is_operator_trust(&self) -> bool {
        matches!(self, Self::OperatorTrust { .. })
    }

    /// The header origin this principal was verified at, `None` for operator
    /// trust (which sends none) and for a keyed or opaque principal.
    pub fn verified_origin(&self) -> Option<String> {
        match self {
            Self::Browser { origin, .. } => Some(origin.as_origin_string()),
            _ => None,
        }
    }

    /// The class name served as `principalClass` and logged in the `/health`
    /// tuples.
    pub fn class_str(&self) -> &'static str {
        match self {
            Self::OperatorTrust { class } | Self::Browser { class, .. } => class.as_str(),
            Self::TabKey { .. } => "tab_key",
            Self::Opaque { class } => class.as_str(),
        }
    }

    /// Principal equality — the one function every rule compares with.
    ///
    /// Operator trust is one principal. Browsers compare as `NormOrigin` with
    /// the loopback aliases folded together. A keyed tab compares by key
    /// digest only, whatever the origin. An `Opaque` principal matches
    /// nothing, including another `Opaque`: two attacker pages with no origin
    /// must not share a claim.
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::OperatorTrust { .. }, Self::OperatorTrust { .. }) => true,
            (Self::Browser { origin: a, .. }, Self::Browser { origin: b, .. }) => {
                a.same_principal(b)
            }
            (Self::TabKey { digest: a }, Self::TabKey { digest: b }) => a == b,
            _ => false,
        }
    }

    /// May `self` displace, release or otherwise act on a claim held by
    /// `holder`? Operator trust always may — it already holds every door.
    pub fn may_displace(&self, holder: &Self) -> bool {
        self.is_operator_trust() || self.same(holder)
    }

    /// A stable key for the "log once per principal+rule" set.
    fn log_key(&self) -> String {
        match self {
            Self::OperatorTrust { class } => format!("operator:{}", class.as_str()),
            Self::Browser { class, origin } => {
                format!("{}:{}", class.as_str(), origin.as_origin_string())
            }
            Self::TabKey { digest } => format!("tabkey:{}", &digest[..16.min(digest.len())]),
            Self::Opaque { class } => format!("opaque:{}", class.as_str()),
        }
    }
}

/// `sha256(key)`, hex. The runner never mints a tab key — the injector
/// generates it — and never stores the key itself, only this digest.
pub fn key_digest(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

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

/// A released browser-principal id, reserved for its previous holder until
/// `expires_at_ms` (R5).
#[derive(Debug, Clone)]
struct Tombstone {
    holder: Principal,
    expires_at_ms: i64,
}

/// Config, counters and the tombstone map: ONE instance per router, shared by
/// every request as an `Arc` on `ApiState` (and so on every [`RelayState`]
/// extracted from it).
#[derive(Debug, Default)]
pub struct RelayBinding {
    pub config: BindingConfig,
    pub counters: BindingCounters,
    /// `"app:<id>"` / `"tab:<id>"` → the principal that just released it.
    tombstones: Mutex<HashMap<String, Tombstone>>,
    /// The last [`MAX_RECENT_TUPLES`] `(rule, class, route)` triples, for
    /// `/health`. No origin: for a Foreign requester that would name the
    /// other sites that reached this runner.
    recent: Mutex<VecDeque<(&'static str, &'static str, &'static str)>>,
    /// "principal+rule already logged" — one WARN per pair, not per request.
    logged: Mutex<HashSet<String>>,
}

impl RelayBinding {
    pub fn new(config: BindingConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            counters: BindingCounters::default(),
            tombstones: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            logged: Mutex::new(HashSet::new()),
        })
    }

    /// Apply `mode` to a computed refusal.
    ///
    /// - `Enforce` → counted as `refused`, returned as `Err`.
    /// - `Shadow` → counted as `wouldRefuse`, logged once per principal+rule,
    ///   and `Ok`: the caller proceeds exactly as it does today.
    /// - `Off` → nothing; callers skip the check entirely, this arm is only
    ///   defensive.
    pub fn meter(
        &self,
        mode: BindingMode,
        principal: &Principal,
        route: &'static str,
        refusal: Refusal,
    ) -> Result<(), Refusal> {
        match mode {
            BindingMode::Off => Ok(()),
            BindingMode::Enforce => {
                self.counters.record(refusal.rule, true);
                self.push_recent(refusal.rule, principal.class_str(), route);
                Err(refusal)
            }
            BindingMode::Shadow => {
                self.counters.record(refusal.rule, false);
                self.push_recent(refusal.rule, principal.class_str(), route);
                if self.first_sighting(principal, refusal.rule) {
                    tracing::warn!(
                        class = principal.class_str(),
                        rule = refusal.rule,
                        code = refusal.code,
                        route = route,
                        "ui-bridge binding (shadow): would refuse where the kill switch enforces (logged once per principal+rule; counted on /health uiBridgeBinding)"
                    );
                }
                Ok(())
            }
        }
    }

    /// Resolve the caller's principal, metering R-opaque through the kill
    /// switch: `enforce` refuses, `shadow` counts and admits with a
    /// principal-less [`Principal::Opaque`], `off` admits silently.
    pub fn principal(
        &self,
        requester: Option<&RequesterPrincipal>,
        tab_key: Option<&str>,
        route: &'static str,
    ) -> Result<Principal, Refusal> {
        match Principal::resolve(requester, tab_key) {
            Ok(p) => Ok(p),
            Err(refusal) => {
                let opaque = Principal::Opaque {
                    class: requester.map(|r| r.class).unwrap_or(OriginClass::Foreign),
                };
                self.meter(self.config.binding, &opaque, route, refusal)?;
                Ok(opaque)
            }
        }
    }

    fn push_recent(&self, rule: &'static str, class: &'static str, route: &'static str) {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() >= MAX_RECENT_TUPLES {
            recent.pop_front();
        }
        recent.push_back((rule, class, route));
    }

    fn first_sighting(&self, principal: &Principal, rule: &'static str) -> bool {
        let mut logged = self.logged.lock().unwrap_or_else(|e| e.into_inner());
        if logged.len() >= MAX_LOGGED_SIGHTINGS {
            // Bounded: past the ceiling every sighting logs rather than
            // growing the set. Noisy beats unbounded.
            return true;
        }
        logged.insert(format!("{}|{}", principal.log_key(), rule))
    }

    // -- Tombstones (R5) -------------------------------------------------

    /// Reserve `key` for `holder` for [`BINDING_TOMBSTONE_MS`]. Operator-trust
    /// releases are NOT tombstoned: an agent's id is free the moment it lets go.
    pub fn tombstone(&self, key: String, holder: &Principal) {
        if holder.is_operator_trust() {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut map = self.tombstones.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, t| t.expires_at_ms > now);
        if map.len() >= MAX_TOMBSTONES {
            // Drop the one closest to expiry so a spray of fresh ids cannot
            // grow the map without bound.
            if let Some(soonest) = map
                .iter()
                .min_by_key(|(_, t)| t.expires_at_ms)
                .map(|(k, _)| k.clone())
            {
                map.remove(&soonest);
            }
        }
        map.insert(
            key,
            Tombstone {
                holder: holder.clone(),
                expires_at_ms: now + BINDING_TOMBSTONE_MS,
            },
        );
    }

    /// The principal still holding `key`'s tombstone, if it has not expired.
    pub fn tombstone_holder(&self, key: &str) -> Option<Principal> {
        let now = chrono::Utc::now().timestamp_millis();
        let map = self.tombstones.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key)
            .filter(|t| t.expires_at_ms > now)
            .map(|t| t.holder.clone())
    }

    /// Drop `key`'s tombstone — called when the id is claimed again.
    pub fn clear_tombstone(&self, key: &str) {
        let mut map = self.tombstones.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(key);
    }

    /// Drop every expired tombstone. Driven by `app_registry::spawn_sweeper`'s
    /// existing tick rather than a second timer. Returns how many went.
    pub fn sweep_tombstones(&self) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let mut map = self.tombstones.lock().unwrap_or_else(|e| e.into_inner());
        let before = map.len();
        map.retain(|_, t| t.expires_at_ms > now);
        before - map.len()
    }

    /// The `/health` `uiBridgeBinding` block, served by the production
    /// handler (`mcp_api::health`). Phase 4's graduation of R6, R8 and
    /// R9-unkeyed is decided from these counters, so they have to be readable
    /// by an operator, not only by the test harness.
    pub fn health_json(&self) -> Value {
        let recent: Vec<Value> = self
            .recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(rule, class, route)| json!({ "rule": rule, "class": class, "route": route }))
            .collect();
        json!({
            "binding": self.config.binding.as_str(),
            "activeBinding": self.config.active_binding.as_str(),
            "bindingEnv": ENV_BINDING,
            "activeBindingEnv": ENV_ACTIVE_BINDING,
            "tombstoneMs": BINDING_TOMBSTONE_MS,
            "tombstones": self
                .tombstones
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            "rules": self.counters.rules_json(),
            "recent": recent,
        })
    }
}

/// The tombstone key for an `appId`. Tab ids get `tab:` in Phase 2, so the two
/// namespaces can never collide.
pub fn app_tombstone_key(app_id: &str) -> String {
    format!("app:{app_id}")
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
    /// The binding config, counters and tombstones every rule reads.
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
