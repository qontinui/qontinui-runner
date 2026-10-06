//! Runner-hosted UI Bridge command-relay tab protocol.
//!
//! Implements the web-relay wire contract the SDK relay client
//! (`ui-bridge/packages/ui-bridge/src/relay/relay-client.ts`) and the
//! injected transport (`ui-bridge-inject` CLI → `injected/bootstrap.ts`)
//! speak, so a temp runner can host injected tabs without a prod relay:
//!
//!   1. `GET  /ui-bridge/commands/stream?tabId=` — SSE command delivery.
//!      First event is `{"type":"connected","tabId":...}`; subsequent events
//!      are queued command frames `{commandId, action, payload, timestamp}`.
//!      `: heartbeat` keep-alive comments every 15s.
//!   2. `POST /ui-bridge/commands` — the tab posts the result envelope
//!      `{commandId, success, result, tabId, error?}` (registered in
//!      `mod.rs::routes()`, chained onto the existing GET).
//!   3. `POST /ui-bridge/heartbeat` — tab registry upsert; response carries
//!      `data.tabRegistered` so the client can detect silent stream drops
//!      and force a reconnect.
//!   4. `GET  /ui-bridge/tabs` — registry listing; polled by
//!      `ui-bridge-headless`'s `waitForUiBridgeRegistration` (reads
//!      `body.data.tabs[].tabId`).
//!   5. `POST /ui-bridge/relay/dispatch` — runner-side command entry point:
//!      queue a command to a registered tab and await its result.
//!
//! Heartbeats are LENIENT about metadata: `registrationMetadata`
//! (`{userId, sessionId}`) is stored when present but not required. Plan:
//! `plans/2026-06-12-co-pilot-automation-ui-bridge-remediation.md` item 6(a).
//!
//! Identity is NOT lenient. Every tab is bound to the [`Principal`] that first
//! attached or heartbeat it (its `Origin`, or the digest of its
//! `X-UI-Bridge-Tab-Key`), and a `tabId` in a request is not evidence of who
//! sent it: a live holder is displaced only by its own principal or operator
//! trust (R1), an ended tab's id is reserved for its holder for the eviction
//! window (R5), and a result completes a command only when the poster is the
//! tab's principal, with no operator-trust exemption (R3). Plan:
//! `2026-09-17-ui-bridge-relay-registration-is-unauthenticated` Phase 2.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    response::Json,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use axum::Extension;

use crate::mcp::origin_guard::RequesterPrincipal;
use crate::mcp::relay_binding::{
    BindingMode, Principal, Refusal, RelayBinding, RelayState, BINDING_TOMBSTONE_MS, RULE_R1,
    RULE_R5, RULE_R8, RULE_R9_UNKEYED,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Tabs with no live SSE listener are evicted from the registry once their
/// last sign of life (`last_seen_ms`) is this old — inclusive: this is the
/// FIRST age at which a disconnected tab is dropped, not the last age at which
/// it survives (`evict_stale` retains while `age < STALE_TAB_EVICT_MS`, and
/// `eviction_fires_when_the_age_reaches_the_bound_not_after` pins it).
/// Eviction is lazy: it runs on the next registry read or upsert (listing,
/// heartbeat, stream connect, dispatch), not on a disconnect or a result.
/// Served to callers as `staleTabEvictMs` on `GET /ui-bridge/tabs`, beside
/// the `lastSeen` it is measured against. Heartbeats arrive every 10s;
/// 60s = 6 missed beats.
pub const STALE_TAB_EVICT_MS: u64 = 60_000;

/// Default await window for a dispatched command's result.
const DEFAULT_DISPATCH_TIMEOUT_MS: u64 = 10_000;
/// Cap on caller-supplied dispatch timeouts.
const MAX_DISPATCH_TIMEOUT_MS: u64 = 60_000;

/// SSE keep-alive comment interval (matches the web relay's 15s).
const SSE_KEEPALIVE_SECS: u64 = 15;

/// Heartbeat metadata keys copied into the tab record verbatim.
const HEARTBEAT_METADATA_KEYS: &[&str] = &[
    "url",
    "title",
    "visibility",
    "appId",
    "appName",
    "appType",
    "framework",
    "capabilities",
    "version",
];

/// The header a pinned injected tab sends to bind itself to a key rather than
/// to an origin (R9). Read by the runner, never issued by it.
const TAB_KEY_HEADER: &str = "x-ui-bridge-tab-key";

const ROUTE_STREAM: &str = "commands/stream";
const ROUTE_HEARTBEAT: &str = "heartbeat";
const ROUTE_RESULT: &str = "commands";

fn tab_key(headers: &HeaderMap) -> Option<&str> {
    headers.get(TAB_KEY_HEADER).and_then(|v| v.to_str().ok())
}

/// Command frame as delivered over the SSE stream. Field names match the
/// relay client's `QueuedCommand` (camelCase on the wire).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedCommand {
    pub command_id: String,
    pub action: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

/// Result envelope a tab POSTs back for a dispatched command.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub success: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

/// One tab an untargeted dispatch could have meant, with the origin its
/// attach was verified at (`None` for operator trust and a keyed tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabCandidate {
    pub id: String,
    pub verified_origin: Option<String>,
}

/// Why a dispatch could not produce a tab result.
#[derive(Debug)]
pub enum DispatchError {
    /// No tab currently holds a live SSE listener.
    NoTabConnected,
    /// No `tabId` was supplied and more than one tab is connected.
    AmbiguousTab(Vec<TabCandidate>),
    /// The named tab is unknown or has no live SSE listener.
    TabNotFound(String),
    /// The tab's listener channel closed before a result arrived.
    Disconnected(String),
    /// The tab never posted a result within the window.
    Timeout { command_id: String, timeout_ms: u64 },
}

struct Listener {
    conn_id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<QueuedCommand>,
}

struct TabRecord {
    registered_at_ms: u64,
    last_heartbeat_ms: Option<u64>,
    /// Last moment we positively observed the tab alive (heartbeat OR stream
    /// connect). Drives stale eviction for tabs that connected a stream but
    /// never heartbeat.
    last_seen_ms: u64,
    metadata: serde_json::Map<String, serde_json::Value>,
    listener: Option<Listener>,
    /// Who holds this tab id. Set by the first attach or heartbeat and moved
    /// only by an admitted displacement. `None` only for a principal-less
    /// (opaque-origin) claim admitted under `shadow` / `off`, which any
    /// principal may then claim.
    principal: Option<Principal>,
}

/// A dispatched command awaiting its result.
struct PendingTabCommand {
    tab_id: String,
    /// The tab's principal when the command was queued; the fallback for R3
    /// when the tab's record has since been evicted.
    principal: Option<Principal>,
    sender: tokio::sync::oneshot::Sender<CommandResult>,
}

#[derive(Default)]
struct RegistryInner {
    tabs: HashMap<String, TabRecord>,
    pending: HashMap<String, PendingTabCommand>,
}

/// Process-wide relay tab registry. One per runner (held on `ApiState`).
pub struct RelayRegistry {
    inner: Mutex<RegistryInner>,
    conn_seq: AtomicU64,
}

impl Default for RelayRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryInner::default()),
            conn_seq: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        // Lock poisoning would mean a panic mid-registry-op; the registry
        // holds no invariants that a half-applied op can corrupt beyond the
        // op itself, so recover rather than wedge every relay route.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// R1 / R5 / R9-unkeyed, then bind: create the record for `tab_id` or
    /// admit `principal` onto the existing one. The check and the write are one
    /// lock acquisition (`inner` is the held guard), so two concurrent claims
    /// can never both pass against the same prior holder.
    ///
    /// - A live holder is displaced only by its own principal or operator
    ///   trust (R1).
    /// - An ended tab's record is kept for `STALE_TAB_EVICT_MS`, which equals
    ///   the reservation window, so the record IS the R5 tombstone: nobody but
    ///   the holder (or operator trust) may take the id until it evicts.
    /// - One arm is not enforced by default: an UNKEYED browser re-attaching
    ///   from a different origin after the prior stream ended. A pinned
    ///   injected tab that predates the tab key legitimately crosses origins
    ///   (an OAuth hop), so that arm rides `active_binding` and is only
    ///   counted (`R9-unkeyed`) until Phase 4 graduates it.
    fn claim(
        inner: &mut RegistryInner,
        binding: &RelayBinding,
        principal: &Principal,
        route: &'static str,
        tab_id: &str,
        now: u64,
    ) -> Result<(), Refusal> {
        let checked = binding.config.binding != BindingMode::Off;
        let record = inner
            .tabs
            .entry(tab_id.to_string())
            .or_insert_with(|| TabRecord {
                registered_at_ms: now,
                last_heartbeat_ms: None,
                last_seen_ms: now,
                metadata: serde_json::Map::new(),
                listener: None,
                principal: None,
            });
        if checked {
            if let Some(holder) = record.principal.as_ref() {
                if !principal.may_displace(holder) {
                    let live = record.listener.is_some();
                    let unkeyed_hop = !live
                        && matches!(holder, Principal::Browser { .. })
                        && matches!(principal, Principal::Browser { .. });
                    if unkeyed_hop {
                        binding.meter(
                            binding.config.active_binding,
                            principal,
                            route,
                            Refusal::registration_held(RULE_R9_UNKEYED),
                        )?;
                    } else {
                        binding.meter(
                            binding.config.binding,
                            principal,
                            route,
                            Refusal::registration_held(if live { RULE_R1 } else { RULE_R5 }),
                        )?;
                    }
                }
            }
        }
        // Admitted. A principal-less claim never replaces a bound holder.
        match principal {
            Principal::Opaque { .. } => {}
            p => record.principal = Some(p.clone()),
        }
        Ok(())
    }

    /// Register (or replace) the SSE listener for `tab_id`. Returns the
    /// connection id (for drop-time deregistration) and the command receiver,
    /// or the [`Refusal`] when `principal` may not take this tab id.
    pub fn connect_stream(
        &self,
        binding: &RelayBinding,
        principal: &Principal,
        tab_id: &str,
    ) -> Result<(u64, tokio::sync::mpsc::UnboundedReceiver<QueuedCommand>), Refusal> {
        let conn_id = self.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let now = crate::util::time::now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        Self::claim(&mut inner, binding, principal, ROUTE_STREAM, tab_id, now)?;
        let record = inner
            .tabs
            .get_mut(tab_id)
            .expect("claim created or found this record under the same lock");
        record.last_seen_ms = now;
        record.listener = Some(Listener { conn_id, tx });
        Ok((conn_id, rx))
    }

    /// Deregister the SSE listener for `tab_id`, but only if it is still the
    /// connection identified by `conn_id` — a reconnect that already replaced
    /// the listener must not be torn down by the old stream's drop.
    pub fn disconnect_stream(&self, tab_id: &str, conn_id: u64) {
        let mut inner = self.lock();
        if let Some(record) = inner.tabs.get_mut(tab_id) {
            if record
                .listener
                .as_ref()
                .is_some_and(|l| l.conn_id == conn_id)
            {
                record.listener = None;
                record.last_seen_ms = crate::util::time::now_ms();
            }
        }
    }

    /// Upsert a tab from a heartbeat body. Returns whether the tab currently
    /// holds a live SSE listener (`tabRegistered` in the response — the
    /// client's silent-drop recovery signal).
    pub fn heartbeat(
        &self,
        binding: &RelayBinding,
        principal: &Principal,
        tab_id: &str,
        body: &serde_json::Value,
    ) -> Result<bool, Refusal> {
        let now = crate::util::time::now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        Self::claim(&mut inner, binding, principal, ROUTE_HEARTBEAT, tab_id, now)?;
        let record = inner
            .tabs
            .get_mut(tab_id)
            .expect("claim created or found this record under the same lock");
        record.last_heartbeat_ms = Some(now);
        record.last_seen_ms = now;
        for key in HEARTBEAT_METADATA_KEYS {
            if let Some(value) = body.get(*key) {
                if !value.is_null() {
                    record.metadata.insert((*key).to_string(), value.clone());
                }
            }
        }
        // Lenient per-user scoping: store the envelope when supplied.
        if let Some(meta) = body.get("registrationMetadata") {
            for key in ["userId", "sessionId"] {
                if let Some(value) = meta.get(key).and_then(|v| v.as_str()) {
                    if !value.trim().is_empty() {
                        record
                            .metadata
                            .insert(key.to_string(), serde_json::json!(value.trim()));
                    }
                }
            }
        }
        Ok(record.listener.is_some())
    }

    /// Snapshot the registry as JSON tab entries (stale tabs evicted first).
    /// Shape mirrors the web relay's `GET /tabs`: each entry carries `tabId`,
    /// flattened heartbeat metadata, `lastHeartbeat`, `lastSeen`, `connected`,
    /// and `isPrimary` (oldest registered tab).
    ///
    /// `lastSeen` is the quantity `staleTabEvictMs` bounds — a disconnected
    /// tab is dropped once `now - lastSeen` reaches it. `lastHeartbeat` is
    /// NOT that quantity and is null for a tab that only ever held a stream,
    /// so a caller reading the bound needs this field to use it.
    pub fn list_tabs(&self) -> Vec<serde_json::Value> {
        let now = crate::util::time::now_ms();
        let mut inner = self.lock();
        evict_stale(&mut inner, now);
        let primary = inner
            .tabs
            .iter()
            .min_by_key(|(id, t)| (t.registered_at_ms, (*id).clone()))
            .map(|(id, _)| id.clone());
        let mut tabs: Vec<(u64, serde_json::Value)> = inner
            .tabs
            .iter()
            .map(|(tab_id, record)| {
                let mut entry = record.metadata.clone();
                entry.insert("tabId".to_string(), serde_json::json!(tab_id));
                entry.insert(
                    "lastHeartbeat".to_string(),
                    serde_json::json!(record.last_heartbeat_ms),
                );
                entry.insert(
                    "lastSeen".to_string(),
                    serde_json::json!(record.last_seen_ms),
                );
                entry.insert(
                    "registeredAt".to_string(),
                    serde_json::json!(record.registered_at_ms),
                );
                entry.insert(
                    "connected".to_string(),
                    serde_json::json!(record.listener.is_some()),
                );
                // Provenance, from the verified header and never the body.
                entry.insert(
                    "verifiedOrigin".to_string(),
                    serde_json::json!(record.principal.as_ref().and_then(|p| p.verified_origin())),
                );
                entry.insert(
                    "principalClass".to_string(),
                    serde_json::json!(record.principal.as_ref().map(|p| p.class_str())),
                );
                entry.insert(
                    "isPrimary".to_string(),
                    serde_json::json!(primary.as_deref() == Some(tab_id.as_str())),
                );
                (record.registered_at_ms, serde_json::Value::Object(entry))
            })
            .collect();
        // Deterministic ordering: oldest first (primary leads).
        tabs.sort_by_key(|(registered_at, _)| *registered_at);
        tabs.into_iter().map(|(_, entry)| entry).collect()
    }

    /// Ids of tabs that currently hold a live SSE listener.
    pub fn connected_tab_ids(&self) -> Vec<String> {
        let mut inner = self.lock();
        evict_stale(&mut inner, crate::util::time::now_ms());
        let mut ids: Vec<String> = inner
            .tabs
            .iter()
            .filter(|(_, t)| t.listener.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Deliver a result envelope for `command_id` (R3). `Ok(false)` when no
    /// dispatch is awaiting that id (already timed out, or unknown).
    ///
    /// The result is accepted only from the tab the command was routed to:
    /// the body `tab_id` must be the recorded target AND the poster must be
    /// that tab's principal. There is NO operator-trust exemption — no agent
    /// ever posts a tab's result, and the exemption is exactly what a
    /// laundered (supervisor-proxied, Origin-stripped) POST would use. A
    /// refusal leaves the command pending, so the genuine tab can still answer.
    pub fn complete(
        &self,
        binding: &RelayBinding,
        principal: &Principal,
        tab_id: &str,
        command_id: &str,
        result: CommandResult,
    ) -> Result<bool, Refusal> {
        let mut inner = self.lock();
        let Some(pending) = inner.pending.get(command_id) else {
            return Ok(false);
        };
        if binding.config.binding != BindingMode::Off {
            let holder = inner
                .tabs
                .get(&pending.tab_id)
                .and_then(|t| t.principal.as_ref())
                .or(pending.principal.as_ref());
            let ours = tab_id == pending.tab_id && holder.is_none_or(|h| principal.same(h));
            if !ours {
                binding.meter(
                    binding.config.binding,
                    principal,
                    ROUTE_RESULT,
                    Refusal::command_not_yours(),
                )?;
            }
        }
        Ok(match inner.pending.remove(command_id) {
            Some(p) => p.sender.send(result).is_ok(),
            None => false,
        })
    }

    /// Queue a command to a registered tab and await its result envelope.
    ///
    /// Target resolution: an explicit `tab_id` must name a tab with a live
    /// listener; with no `tab_id`, exactly one connected tab must exist.
    /// Returns `(tab_id, command_id, result)` on success.
    pub async fn dispatch(
        &self,
        binding: &RelayBinding,
        tab_id: Option<&str>,
        action: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<(String, String, CommandResult), DispatchError> {
        let command_id = uuid::Uuid::new_v4().to_string();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // Resolve the target + enqueue while holding the lock; await outside.
        let target = {
            let mut inner = self.lock();
            evict_stale(&mut inner, crate::util::time::now_ms());
            let target = match tab_id {
                Some(id) => {
                    let record = inner
                        .tabs
                        .get(id)
                        .ok_or_else(|| DispatchError::TabNotFound(id.to_string()))?;
                    if record.listener.is_none() {
                        return Err(DispatchError::TabNotFound(id.to_string()));
                    }
                    id.to_string()
                }
                None => {
                    let now = crate::util::time::now_ms();
                    let connected: Vec<&String> = inner
                        .tabs
                        .iter()
                        .filter(|(_, t)| t.listener.is_some())
                        .map(|(id, _)| id)
                        .collect();
                    if connected.is_empty() {
                        return Err(DispatchError::NoTabConnected);
                    }
                    // R8: an untargeted dispatch may resolve only when exactly
                    // one tab is connected AND no tab under a different
                    // principal is, or was within the reservation window,
                    // connected. Otherwise a fresh foreign tab attaching while
                    // the real one reconnects would capture the command.
                    let sole = (connected.len() == 1).then(|| connected[0].clone());
                    let rival = sole.as_ref().is_some_and(|id| {
                        let mine = inner.tabs.get(id).and_then(|t| t.principal.as_ref());
                        inner.tabs.iter().any(|(other, t)| {
                            other != id
                                && now.saturating_sub(t.last_seen_ms) < BINDING_TOMBSTONE_MS as u64
                                && !matches!((mine, t.principal.as_ref()),
                                    (Some(a), Some(b)) if a.same(b))
                        })
                    });
                    if sole.is_some() && !rival {
                        sole.expect("checked is_some")
                    } else {
                        let mut candidates: Vec<TabCandidate> = inner
                            .tabs
                            .iter()
                            .filter(|(_, t)| {
                                t.listener.is_some()
                                    || now.saturating_sub(t.last_seen_ms)
                                        < BINDING_TOMBSTONE_MS as u64
                            })
                            .map(|(id, t)| TabCandidate {
                                id: id.clone(),
                                verified_origin: t
                                    .principal
                                    .as_ref()
                                    .and_then(|p| p.verified_origin()),
                            })
                            .collect();
                        candidates.sort_by(|a, b| a.id.cmp(&b.id));
                        let ambiguous_by_count = connected.len() > 1;
                        if ambiguous_by_count {
                            // Two connected tabs were ambiguous before R8 and
                            // stay so in every mode.
                            return Err(DispatchError::AmbiguousTab(candidates));
                        }
                        // Sole connected tab, rival tab seen recently: R8's
                        // new refusal, metered through the kill switch.
                        let principal = inner
                            .tabs
                            .get(connected[0])
                            .and_then(|t| t.principal.clone())
                            .unwrap_or(Principal::Opaque {
                                class: crate::mcp::origin_guard::OriginClass::Foreign,
                            });
                        binding
                            .meter(
                                binding.config.active_binding,
                                &principal,
                                "POST /ui-bridge/relay/dispatch",
                                Refusal::active_held(RULE_R8),
                            )
                            .map_err(|_| DispatchError::AmbiguousTab(candidates))?;
                        connected[0].clone()
                    }
                }
            };
            let principal = inner.tabs.get(&target).and_then(|t| t.principal.clone());
            inner.pending.insert(
                command_id.clone(),
                PendingTabCommand {
                    tab_id: target.clone(),
                    principal,
                    sender: result_tx,
                },
            );
            let command = QueuedCommand {
                command_id: command_id.clone(),
                action: action.to_string(),
                payload,
                timestamp: crate::util::time::now_ms(),
            };
            let record = inner
                .tabs
                .get_mut(&target)
                .expect("target resolved from this map under the same lock");
            let listener = record
                .listener
                .as_ref()
                .expect("listener checked under the same lock");
            if listener.tx.send(command).is_err() {
                // Receiver dropped (stream gone) — clean up and report.
                record.listener = None;
                inner.pending.remove(&command_id);
                return Err(DispatchError::TabNotFound(target));
            }
            target
        };

        match tokio::time::timeout(timeout, result_rx).await {
            Ok(Ok(result)) => Ok((target, command_id, result)),
            Ok(Err(_)) => Err(DispatchError::Disconnected(target)),
            Err(_) => {
                self.lock().pending.remove(&command_id);
                Err(DispatchError::Timeout {
                    command_id,
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    /// Test hook: run stale eviction as if the clock read `now`.
    #[cfg(test)]
    fn evict_stale_at(&self, now: u64) {
        evict_stale(&mut self.lock(), now);
    }
}

/// Remove tabs with no live SSE listener whose last sign of life is at least
/// [`STALE_TAB_EVICT_MS`] old — `age < bound` is retained, `age == bound` is
/// dropped, which is the boundary `CONTRACT.md` states for `staleTabEvictMs`.
/// Connected tabs are never evicted — the SSE drop guard handles their
/// lifecycle.
fn evict_stale(inner: &mut RegistryInner, now: u64) {
    inner.tabs.retain(|_, record| {
        record.listener.is_some() || now.saturating_sub(record.last_seen_ms) < STALE_TAB_EVICT_MS
    });
}

/// Drop guard owned by the SSE generator: deregisters this connection's
/// listener when the client goes away and axum drops the stream.
struct StreamGuard {
    registry: Arc<RelayRegistry>,
    tab_id: String,
    conn_id: u64,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.registry.disconnect_stream(&self.tab_id, self.conn_id);
        info!(
            "UI Bridge relay: SSE stream closed for tab {} (conn {})",
            self.tab_id, self.conn_id
        );
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /ui-bridge/commands/stream?tabId=
///
/// SSE command stream for a relay tab. Registers (or re-registers) the tab's
/// listener; a missing `tabId` gets a generated one, echoed in the initial
/// `connected` event. The drop guard deregisters the listener when the
/// client disconnects.
pub async fn ui_bridge_relay_command_stream_handler(
    State(state): State<RelayState>,
    requester: Option<Extension<RequesterPrincipal>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let principal = state
        .binding
        .principal(
            requester.as_ref().map(|e| &e.0),
            tab_key(&headers),
            ROUTE_STREAM,
        )
        .map_err(Refusal::into_http)?;
    let registry = state.ui_bridge_relay.clone();
    let tab_id = query
        .get("tabId")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // A refusal is a JSON body sent BEFORE the stream starts, so the victim
    // keeps its stream and `relay-client.ts` (which treats `!resp.ok` as
    // `scheduleReconnect()`) keeps retrying into a refusal counter.
    let (conn_id, mut rx) = registry
        .connect_stream(&state.binding, &principal, &tab_id)
        .map_err(Refusal::into_http)?;
    info!(
        "UI Bridge relay: SSE stream opened for tab {} (conn {})",
        tab_id, conn_id
    );

    let stream = async_stream::stream! {
        let _guard = StreamGuard {
            registry,
            tab_id: tab_id.clone(),
            conn_id,
        };
        yield Ok(Event::default()
            .data(serde_json::json!({ "type": "connected", "tabId": tab_id }).to_string()));
        while let Some(command) = rx.recv().await {
            match serde_json::to_string(&command) {
                Ok(json) => yield Ok(Event::default().data(json)),
                Err(e) => {
                    warn!("UI Bridge relay: failed to serialize command frame: {e}");
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(SSE_KEEPALIVE_SECS))
            .text("heartbeat"),
    ))
}

/// POST /ui-bridge/heartbeat
///
/// Relay tab registry upsert. LENIENT about metadata (unlike the strict web
/// relay): only `tabId` is required; `registrationMetadata` is stored when
/// present. Who may heartbeat a tab id is NOT lenient: the caller must be the
/// tab's principal (see the module doc). The
/// response's `data.tabRegistered` reports whether this tab currently holds
/// a live SSE listener — the client forces a stream reconnect on `false`.
pub async fn ui_bridge_relay_heartbeat_handler(
    State(state): State<RelayState>,
    requester: Option<Extension<RequesterPrincipal>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let Some(tab_id) = body
        .get("tabId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        let mut err = api_error("heartbeat requires a non-empty tabId field");
        err.code = Some("MISSING_TAB_ID".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    };
    let principal = state
        .binding
        .principal(
            requester.as_ref().map(|e| &e.0),
            tab_key(&headers),
            ROUTE_HEARTBEAT,
        )
        .map_err(Refusal::into_http)?;
    let tab_registered = state
        .ui_bridge_relay
        .heartbeat(&state.binding, &principal, tab_id, &body)
        .map_err(Refusal::into_http)?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "received": true,
        "tabRegistered": tab_registered,
    }))))
}

/// POST /ui-bridge/commands
///
/// Result ingestion: a relay tab posts the result envelope for a previously
/// streamed command. `matched: false` means no dispatch was awaiting that
/// `commandId` (it timed out, or the id is unknown) — not an error, the tab
/// fire-and-forgets these.
pub async fn ui_bridge_relay_command_result_handler(
    State(state): State<RelayState>,
    requester: Option<Extension<RequesterPrincipal>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let Some(command_id) = body
        .get("commandId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        let mut err = api_error("command result requires a non-empty commandId field");
        err.code = Some("MISSING_COMMAND_ID".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    };
    let result = CommandResult {
        success: body
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        result: body
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        error: body
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };
    let principal = state
        .binding
        .principal(
            requester.as_ref().map(|e| &e.0),
            tab_key(&headers),
            ROUTE_RESULT,
        )
        .map_err(Refusal::into_http)?;
    let posted_tab_id = body
        .get("tabId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let matched = state
        .ui_bridge_relay
        .complete(
            &state.binding,
            &principal,
            posted_tab_id,
            command_id,
            result,
        )
        .map_err(Refusal::into_http)?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "received": true,
        "matched": matched,
    }))))
}

/// GET /ui-bridge/tabs
///
/// List registered relay tabs. Response shape matches what
/// `ui-bridge-headless`'s `waitForUiBridgeRegistration` polls for:
/// `{ success, data: { tabs: [{ tabId, ... }] } }`.
pub async fn ui_bridge_relay_tabs_handler(
    State(state): State<RelayState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(tabs_response_body(
        state.ui_bridge_relay.list_tabs(),
    ))))
}

/// The `data` payload of `GET /ui-bridge/tabs`.
///
/// Split out of the handler when the handler took an `ApiState`, which owns a
/// `tauri::AppHandle` no test can build (it now takes `RelayState`, which
/// `relay_binding/tests.rs` drives over a real socket). These
/// key names are a WIRE CONTRACT other products read, and until this split
/// nothing in the repo pinned them — which is why the rename below could be made
/// safely, and equally why the next one would have gone unnoticed.
/// `tabs_body_pins_the_wire_contract` is that pin.
///
/// `staleTabEvictMs` is named for what it governs: how long a tab with NO live
/// listener is retained before eviction (see `STALE_TAB_EVICT_MS` and
/// `evict_stale`). A connected tab is never evicted on heartbeat age at all, so
/// "heartbeat" was never the right word for this bound. It ships beside
/// `tabs[].lastSeen`, the quantity it is measured against — the bound on its own
/// tells a caller nothing, and `lastHeartbeat` is null for a stream-only tab.
///
/// It was `staleHeartbeatMs`, which the SDK's relay also emitted — for a
/// DIFFERENT quantity (its non-destructive active-tab freshness window). The SDK
/// renamed its own field to `tabActiveWindowMs` in @qontinui/ui-bridge 0.26.0,
/// so keeping this name here would leave one key meaning two things across two
/// products, which is the exact trap that rename closed.
pub(super) fn tabs_response_body(tabs: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "count": tabs.len(),
        "tabs": tabs,
        "staleTabEvictMs": STALE_TAB_EVICT_MS,
    })
}

/// Request body for `POST /ui-bridge/relay/dispatch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayDispatchRequest {
    /// Target tab. Optional when exactly one tab is connected.
    #[serde(default, alias = "targetTabId")]
    pub tab_id: Option<String>,
    /// Relay action id (e.g. `discover`, `find`, `get_snapshot`) — the same
    /// action vocabulary `executeCommand` dispatches browser-side.
    pub action: String,
    /// Action payload, forwarded verbatim.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Result await window in ms (default 10000, capped at 60000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// POST /ui-bridge/relay/dispatch
///
/// Queue a command to a registered relay tab over its SSE stream and await
/// the result envelope the tab POSTs back to `/ui-bridge/commands`. This is
/// the runner-side entry point for driving injected/relay tabs (the analog
/// of the web relay's per-route `queueCommand` dispatch).
pub async fn ui_bridge_relay_dispatch_handler(
    State(state): State<RelayState>,
    Json(request): Json<RelayDispatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if request.action.trim().is_empty() {
        let mut err = api_error("dispatch requires a non-empty action field");
        err.code = Some("MISSING_ACTION".to_string());
        return Err((StatusCode::BAD_REQUEST, Json(err)));
    }
    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_DISPATCH_TIMEOUT_MS)
        .clamp(1, MAX_DISPATCH_TIMEOUT_MS);
    info!(
        "UI Bridge relay: dispatch '{}' to tab {:?} (timeout {}ms)",
        request.action, request.tab_id, timeout_ms
    );

    match state
        .ui_bridge_relay
        .dispatch(
            &state.binding,
            request.tab_id.as_deref(),
            &request.action,
            request.payload,
            Duration::from_millis(timeout_ms),
        )
        .await
    {
        Ok((tab_id, command_id, result)) => {
            let payload = serde_json::json!({
                "tabId": tab_id,
                "commandId": command_id,
                "result": result.result,
            });
            if result.success {
                Ok(Json(ApiResponse::success(payload)))
            } else {
                // The tab answered but reported failure — surface its error
                // verbatim with the result payload attached.
                let mut response: ApiResponse<serde_json::Value> = ApiResponse::error(
                    result
                        .error
                        .unwrap_or_else(|| "relay tab reported a failed command".to_string()),
                );
                response.data = Some(payload);
                Ok(Json(response))
            }
        }
        Err(DispatchError::NoTabConnected) => {
            let mut err = api_error(
                "no relay tab is connected — register one via the SSE command stream first",
            );
            err.code = Some("NO_TAB_CONNECTED".to_string());
            Err((StatusCode::SERVICE_UNAVAILABLE, Json(err)))
        }
        Err(DispatchError::AmbiguousTab(ids)) => {
            let mut err = api_error(format!(
                "multiple relay tabs are connected ({}) — supply tabId to pin the target",
                ids.iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            err.code = Some("AMBIGUOUS_TAB".to_string());
            err.suggestions = Some(
                ids.iter()
                    .map(|c| format!("retry with {{\"tabId\": \"{}\"}}", c.id))
                    .collect(),
            );
            err.hint = Some(serde_json::json!({
                "candidates": ids
                    .iter()
                    .map(|c| serde_json::json!({
                        "tabId": c.id,
                        "verifiedOrigin": c.verified_origin,
                    }))
                    .collect::<Vec<_>>(),
            }));
            Err((StatusCode::CONFLICT, Json(err)))
        }
        Err(DispatchError::TabNotFound(id)) => {
            // Structured envelope so the 404 is discriminable from an
            // unregistered route (see CONTRACT.md "404 discriminator").
            let mut err = api_error(format!(
                "relay tab '{id}' is not connected — discover live tabs via GET /ui-bridge/tabs"
            ));
            err.code = Some("TAB_NOT_FOUND".to_string());
            Err((StatusCode::NOT_FOUND, Json(err)))
        }
        Err(DispatchError::Disconnected(id)) => {
            let mut err = api_error(format!(
                "relay tab '{id}' disconnected before posting a result"
            ));
            err.code = Some("TAB_DISCONNECTED".to_string());
            Err((StatusCode::BAD_GATEWAY, Json(err)))
        }
        Err(DispatchError::Timeout {
            command_id,
            timeout_ms,
        }) => {
            let mut err = api_error(format!(
                "relay tab did not post a result for command {command_id} within {timeout_ms}ms"
            ));
            err.code = Some("TIMEOUT".to_string());
            Err((StatusCode::GATEWAY_TIMEOUT, Json(err)))
        }
    }
}

// ============================================================================
// Routes + manifest
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    // NOTE: POST /ui-bridge/commands (result ingestion,
    // `ui_bridge_relay_command_result_handler`) is registered in
    // `mod.rs::routes()`, chained onto the pre-existing GET for the
    // Tauri-invoke command listing — the path is shared across families.
    axum::Router::new()
        .route(
            "/ui-bridge/commands/stream",
            get(ui_bridge_relay_command_stream_handler),
        )
        // Relay-tab registry heartbeat (web-relay wire contract; the old
        // `receive_heartbeat` IPC forward never had a frontend handler).
        .route(
            "/ui-bridge/heartbeat",
            post(ui_bridge_relay_heartbeat_handler),
        )
        .route("/ui-bridge/tabs", get(ui_bridge_relay_tabs_handler))
        .route(
            "/ui-bridge/relay/dispatch",
            post(ui_bridge_relay_dispatch_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/commands/stream"),
        ("POST", "/ui-bridge/heartbeat"),
        ("GET", "/ui-bridge/tabs"),
        ("POST", "/ui-bridge/relay/dispatch"),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::relay_binding::BindingConfig;

    fn b() -> Arc<RelayBinding> {
        RelayBinding::new(BindingConfig::default())
    }

    /// The caller every legacy registry test stands in for: an agent / script
    /// with no `Origin`, i.e. operator trust.
    fn op() -> Principal {
        Principal::OperatorTrust {
            class: crate::mcp::origin_guard::OriginClass::NonBrowser,
        }
    }

    fn heartbeat_body(tab_id: &str) -> serde_json::Value {
        serde_json::json!({
            "tabId": tab_id,
            "url": "https://example.test/login",
            "title": "Example",
            "visibility": "visible",
            "appType": "injected",
            "appName": "example login",
            "capabilities": ["control", "discovery"],
            "version": "0.19.0",
        })
    }

    #[test]
    fn heartbeat_registers_tab_without_stream() {
        let registry = RelayRegistry::new();
        let tab_registered = registry
            .heartbeat(&b(), &op(), "tab-a", &heartbeat_body("tab-a"))
            .unwrap();
        assert!(!tab_registered, "no SSE listener yet");

        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        let tab = &tabs[0];
        assert_eq!(tab["tabId"], "tab-a");
        assert_eq!(tab["connected"], false);
        assert_eq!(tab["url"], "https://example.test/login");
        assert_eq!(tab["appType"], "injected");
        assert_eq!(tab["isPrimary"], true);
        assert!(tab["lastHeartbeat"].is_u64());
    }

    #[test]
    fn heartbeat_is_lenient_about_registration_metadata() {
        let registry = RelayRegistry::new();
        // No registrationMetadata at all — accepted (unlike the strict web relay).
        assert!(!registry
            .heartbeat(
                &b(),
                &op(),
                "tab-a",
                &serde_json::json!({ "tabId": "tab-a" })
            )
            .unwrap());
        // With metadata — stored and surfaced on the tab entry.
        registry
            .heartbeat(
                &b(),
                &op(),
                "tab-a",
                &serde_json::json!({
                    "tabId": "tab-a",
                    "registrationMetadata": { "userId": "user-1", "sessionId": "sess-1" },
                }),
            )
            .unwrap();
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["userId"], "user-1");
        assert_eq!(tabs[0]["sessionId"], "sess-1");
    }

    #[tokio::test]
    async fn connect_stream_flips_tab_registered_and_connected() {
        let registry = RelayRegistry::new();
        let (_conn_id, _rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        assert!(registry
            .heartbeat(&b(), &op(), "tab-a", &heartbeat_body("tab-a"))
            .unwrap());
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["connected"], true);
        assert_eq!(registry.connected_tab_ids(), vec!["tab-a".to_string()]);
    }

    #[tokio::test]
    async fn dispatch_delivers_command_and_returns_result() {
        let registry = Arc::new(RelayRegistry::new());
        let (_conn_id, mut rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();

        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = rx.recv().await.expect("command frame");
                assert_eq!(command.action, "discover");
                assert_eq!(command.payload["query"], "smoke");
                let matched = registry
                    .complete(
                        &b(),
                        &op(),
                        "tab-a",
                        &command.command_id,
                        CommandResult {
                            success: true,
                            result: serde_json::json!({ "elements": [{ "id": "button-run" }] }),
                            error: None,
                        },
                    )
                    .unwrap();
                assert!(matched, "dispatch should be awaiting this commandId");
            })
        };

        let (tab_id, command_id, result) = registry
            .dispatch(
                &b(),
                Some("tab-a"),
                "discover",
                serde_json::json!({ "query": "smoke" }),
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch result");
        assert_eq!(tab_id, "tab-a");
        assert!(!command_id.is_empty());
        assert!(result.success);
        assert_eq!(result.result["elements"][0]["id"], "button-run");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_without_tab_id_targets_sole_connected_tab() {
        let registry = Arc::new(RelayRegistry::new());
        // A heartbeat-only (not connected) tab must NOT make this ambiguous.
        registry
            .heartbeat(&b(), &op(), "tab-idle", &heartbeat_body("tab-idle"))
            .unwrap();
        let (_conn_id, mut rx) = registry.connect_stream(&b(), &op(), "tab-live").unwrap();

        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = rx.recv().await.expect("command frame");
                let _ = registry.complete(
                    &b(),
                    &op(),
                    "tab-live",
                    &command.command_id,
                    CommandResult {
                        success: true,
                        result: serde_json::Value::Null,
                        error: None,
                    },
                );
            })
        };

        let (tab_id, _, _) = registry
            .dispatch(
                &b(),
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch result");
        assert_eq!(tab_id, "tab-live");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_with_no_connected_tab_is_no_tab_connected() {
        let registry = RelayRegistry::new();
        registry
            .heartbeat(&b(), &op(), "tab-idle", &heartbeat_body("tab-idle"))
            .unwrap();
        let err = registry
            .dispatch(
                &b(),
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("no connected tab");
        assert!(matches!(err, DispatchError::NoTabConnected));
    }

    #[tokio::test]
    async fn dispatch_with_two_connected_tabs_is_ambiguous() {
        let registry = RelayRegistry::new();
        let (_c1, _rx1) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        let (_c2, _rx2) = registry.connect_stream(&b(), &op(), "tab-b").unwrap();
        let err = registry
            .dispatch(
                &b(),
                None,
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("ambiguous");
        match err {
            DispatchError::AmbiguousTab(ids) => {
                let ids: Vec<&str> = ids.iter().map(|c| c.id.as_str()).collect();
                assert_eq!(ids, vec!["tab-a", "tab-b"]);
            }
            other => panic!("expected AmbiguousTab, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_to_unknown_or_unconnected_tab_is_tab_not_found() {
        let registry = RelayRegistry::new();
        let err = registry
            .dispatch(
                &b(),
                Some("tab-missing"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("unknown tab");
        assert!(matches!(err, DispatchError::TabNotFound(ref id) if id == "tab-missing"));

        // Known via heartbeat but no live stream — pinned dispatch must NOT
        // silently target some other tab; it reports TAB_NOT_FOUND.
        registry
            .heartbeat(&b(), &op(), "tab-idle", &heartbeat_body("tab-idle"))
            .unwrap();
        let err = registry
            .dispatch(
                &b(),
                Some("tab-idle"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("unconnected tab");
        assert!(matches!(err, DispatchError::TabNotFound(ref id) if id == "tab-idle"));
    }

    #[tokio::test]
    async fn dispatch_times_out_and_cleans_pending() {
        let registry = RelayRegistry::new();
        let (_conn_id, mut rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        let err = registry
            .dispatch(
                &b(),
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_millis(50),
            )
            .await
            .expect_err("timeout");
        let command_id = match err {
            DispatchError::Timeout { command_id, .. } => command_id,
            other => panic!("expected Timeout, got {other:?}"),
        };
        // The frame still arrived on the stream...
        let frame = rx.recv().await.expect("command frame");
        assert_eq!(frame.command_id, command_id);
        // ...but the pending waiter is gone: a late result no longer matches.
        let matched = registry
            .complete(
                &b(),
                &op(),
                "tab-a",
                &command_id,
                CommandResult {
                    success: true,
                    result: serde_json::Value::Null,
                    error: None,
                },
            )
            .unwrap();
        assert!(!matched, "timed-out dispatch must remove its pending entry");
    }

    #[tokio::test]
    async fn dispatch_to_dropped_stream_is_tab_not_found() {
        let registry = RelayRegistry::new();
        let (_conn_id, rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        drop(rx); // client vanished without the guard firing yet
        let err = registry
            .dispatch(
                &b(),
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(1),
            )
            .await
            .expect_err("dead stream");
        assert!(matches!(err, DispatchError::TabNotFound(_)));
        // The dead listener was cleared as a side effect.
        assert!(registry.connected_tab_ids().is_empty());
    }

    #[tokio::test]
    async fn reconnect_replaces_listener_and_old_disconnect_is_ignored() {
        let registry = RelayRegistry::new();
        let (old_conn, _old_rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        let (_new_conn, mut new_rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();

        // The OLD stream's drop guard fires late — it must not tear down the
        // replacement listener.
        registry.disconnect_stream("tab-a", old_conn);
        assert_eq!(registry.connected_tab_ids(), vec!["tab-a".to_string()]);

        // Commands flow to the new listener.
        let registry = Arc::new(registry);
        let responder = {
            let registry = registry.clone();
            tokio::spawn(async move {
                let command = new_rx.recv().await.expect("command frame");
                let _ = registry.complete(
                    &b(),
                    &op(),
                    "tab-a",
                    &command.command_id,
                    CommandResult {
                        success: true,
                        result: serde_json::Value::Null,
                        error: None,
                    },
                );
            })
        };
        registry
            .dispatch(
                &b(),
                Some("tab-a"),
                "ping",
                serde_json::Value::Null,
                Duration::from_secs(5),
            )
            .await
            .expect("dispatch via replacement listener");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn matching_disconnect_deregisters_listener() {
        let registry = RelayRegistry::new();
        let (conn_id, _rx) = registry.connect_stream(&b(), &op(), "tab-a").unwrap();
        registry.disconnect_stream("tab-a", conn_id);
        assert!(registry.connected_tab_ids().is_empty());
        // The tab record survives (until stale eviction) — connected: false.
        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["connected"], false);
    }

    // ---- GET /ui-bridge/tabs wire contract -------------------------------
    //
    // The keys below are read by other products (the SDK relay client, the
    // `ui-bridge-inject` CLI, qontinui-web's co-pilot executor). Nothing pinned
    // them until #1392 renamed one of them and had to establish by grep that it
    // was safe. These tests are that pin, and they assert LITERALS rather than
    // the constants the implementation uses — asserting `STALE_TAB_EVICT_MS`
    // against a body built from `STALE_TAB_EVICT_MS` would be a tautology that
    // stays green through any rename or retuning.

    #[test]
    fn tabs_body_pins_the_wire_contract() {
        let body = tabs_response_body(vec![serde_json::json!({ "tabId": "tab-a" })]);

        assert_eq!(body["count"], 1);
        assert_eq!(body["tabs"][0]["tabId"], "tab-a");
        assert_eq!(body["staleTabEvictMs"], 60_000);

        // The retired name must not come back. It was OURS, for this field,
        // before #1392 renamed it to `staleTabEvictMs`. The SDK also used the
        // name once, for a DIFFERENT quantity, and renamed its own to
        // `tabActiveWindowMs` in 0.26.0 — so at the pinned release the name
        // belongs to neither product, and one key meaning two things across
        // two products is the trap #1392 closed.
        assert!(
            body.get("staleHeartbeatMs").is_none(),
            "staleHeartbeatMs must never be ours: it is our own retired name for \
             this field, and was separately the SDK's name for a different one"
        );
    }

    #[test]
    fn tabs_body_reports_an_empty_registry_as_zero() {
        let body = tabs_response_body(Vec::new());
        assert_eq!(body["count"], 0);
        assert_eq!(body["tabs"], serde_json::json!([]));
        // The bound is a property of the relay, not of its population: a caller
        // polling an empty registry still needs it to size its own timeout.
        assert_eq!(body["staleTabEvictMs"], 60_000);
    }

    #[test]
    fn list_tabs_exposes_the_quantity_eviction_measures() {
        let registry = RelayRegistry::new();

        // Heartbeat path: both clocks are set, and they agree.
        registry
            .heartbeat(&b(), &op(), "tab-beat", &heartbeat_body("tab-beat"))
            .unwrap();
        let beat = registry.list_tabs().remove(0);
        assert_eq!(beat["lastSeen"], beat["lastHeartbeat"]);

        // Stream-only path: the tab connected and dropped without ever
        // heartbeating, so `lastHeartbeat` is null — yet this is exactly the
        // tab eviction is about to act on. Without `lastSeen` a caller holding
        // `staleTabEvictMs` has nothing to subtract it from.
        let registry = RelayRegistry::new();
        let (conn_id, _rx) = registry
            .connect_stream(&b(), &op(), "tab-stream-only")
            .unwrap();
        registry.disconnect_stream("tab-stream-only", conn_id);

        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["connected"], false);
        assert!(tabs[0]["lastHeartbeat"].is_null());
        assert!(
            tabs[0]["lastSeen"].is_u64(),
            "a stream-only tab still has a last-seen clock: {}",
            tabs[0]
        );
    }

    #[test]
    fn eviction_fires_when_the_age_reaches_the_bound_not_after() {
        let registry = RelayRegistry::new();
        registry
            .heartbeat(&b(), &op(), "tab-a", &heartbeat_body("tab-a"))
            .unwrap();
        let last_seen = registry.list_tabs()[0]["lastSeen"]
            .as_u64()
            .expect("lastSeen is emitted");

        // One millisecond inside the window: retained.
        registry.evict_stale_at(last_seen + STALE_TAB_EVICT_MS - 1);
        assert_eq!(registry.list_tabs().len(), 1);

        // Exactly at the bound: gone. The comparison is `age < bound`, so
        // `staleTabEvictMs` is the first age at which a disconnected tab is
        // dropped — which is what a caller subtracting `lastSeen` needs to know.
        registry.evict_stale_at(last_seen + STALE_TAB_EVICT_MS);
        assert!(registry.list_tabs().is_empty());
    }

    #[test]
    fn stale_unconnected_tabs_are_evicted_connected_tabs_are_not() {
        let registry = RelayRegistry::new();
        registry
            .heartbeat(&b(), &op(), "tab-stale", &heartbeat_body("tab-stale"))
            .unwrap();
        let (_conn_id, _rx) = registry.connect_stream(&b(), &op(), "tab-live").unwrap();

        registry.evict_stale_at(crate::util::time::now_ms() + STALE_TAB_EVICT_MS + 1);
        let tabs = registry.list_tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0]["tabId"], "tab-live");
    }

    #[test]
    fn complete_with_unknown_command_id_is_unmatched() {
        let registry = RelayRegistry::new();
        assert!(!registry
            .complete(
                &b(),
                &op(),
                "tab-a",
                "no-such-command",
                CommandResult {
                    success: true,
                    result: serde_json::Value::Null,
                    error: None,
                },
            )
            .unwrap());
    }

    /// The SSE command frame must use the relay client's camelCase field
    /// names (`relay-client.ts` `QueuedCommand`): commandId, action, payload,
    /// timestamp.
    #[test]
    fn command_frame_serializes_to_relay_client_shape() {
        let frame = QueuedCommand {
            command_id: "cmd-1".to_string(),
            action: "discover".to_string(),
            payload: serde_json::json!({ "query": "smoke" }),
            timestamp: 1234,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["commandId"], "cmd-1");
        assert_eq!(json["action"], "discover");
        assert_eq!(json["payload"]["query"], "smoke");
        assert_eq!(json["timestamp"], 1234);
        assert!(
            json.get("command_id").is_none(),
            "must be camelCase on the wire"
        );
    }

    /// First-registered tab is primary; ordering is oldest-first.
    #[test]
    fn primary_is_oldest_registered_tab() {
        let registry = RelayRegistry::new();
        registry
            .heartbeat(&b(), &op(), "tab-first", &heartbeat_body("tab-first"))
            .unwrap();
        // Force a strictly later registered_at for the second tab.
        std::thread::sleep(std::time::Duration::from_millis(5));
        registry
            .heartbeat(&b(), &op(), "tab-second", &heartbeat_body("tab-second"))
            .unwrap();
        let tabs = registry.list_tabs();
        assert_eq!(tabs[0]["tabId"], "tab-first");
        assert_eq!(tabs[0]["isPrimary"], true);
        assert_eq!(tabs[1]["isPrimary"], false);
    }
}
