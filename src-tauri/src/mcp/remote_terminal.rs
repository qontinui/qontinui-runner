//! Remote terminal attach — the relay-side machinery for BOTH roles of plan
//! `2026-08-31-remote-session-tabs-in-runner-terminal` Phase 3c (D6).
//!
//! # Target role — the grant table is the last word
//!
//! A keystroke from another machine reaches this runner's PTY only under a
//! coord-minted **attach grant**: coord publishes `attach_request` on this
//! device's own `qontinui.sessions.<tenant>.<device>.attach_request` subject
//! (and serves the same rows from `GET /sessions/attach-requests` for the
//! catch-up poll), and the web relay forwards the source's frames with a
//! `remote {source_device_id, grant_jti, …}` block attached. Before any
//! `terminal_*` handler in `backend_relay.rs` acts on a frame that carries a
//! `remote` block it calls [`gate_remote_frame`], which admits the frame only
//! when the jti is in [`RemoteAttachGrants`], unexpired, bound to the very
//! terminal the frame names, and the device preference is not `off`. A frame
//! with NO `remote` block is the existing operator-web path and is not
//! touched. The PTY owner enforces; a bug or compromise in the broker cannot
//! type into a session coord did not authorise.
//!
//! # Source role — one client per process, one socket
//!
//! [`RemoteAttachClient`] is the source side's routing table: it owns the
//! outbound queue every [`RemotePaneIo`] writes into (drained onto the backend
//! socket by the relay's connected loop), correlates `remote_terminal_attach`
//! requests with their `remote_terminal_attached` replies, and routes inbound
//! `remote_terminal_output` / `_exit` / `_error` / `_buffer` frames to the
//! pane holding that `grant_jti`. On a relay reconnect it re-presents every
//! live pane's grant and splices the returned ring from the last byte seen.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::settings::AcceptRemoteAttach;
use crate::terminal::remote_pane_io::{AttachedRing, RemoteFrameSink, RemotePaneIo};

// ---------------------------------------------------------------------------
// Target role — grants
// ---------------------------------------------------------------------------

/// Upper bound on live grants held in memory. Grants live 15 minutes and a
/// device is attached to by a handful of peers at most; the cap is a leak
/// guard against a misbehaving publisher, not a capacity plan.
pub const MAX_GRANTS: usize = 256;

/// One attach grant as this runner knows it: the coord directive's fields
/// plus the terminal the grant was bound to at first use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachGrant {
    pub grant_jti: String,
    pub source_device_id: String,
    pub session_id: Uuid,
    /// `None` until the first `terminal_attach` binds it.
    pub terminal_id: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
}

/// Why a `remote` frame was refused. `code()` is the wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRefusal {
    GrantUnknown,
    GrantExpired,
    TerminalMismatch,
    Disabled,
    SessionNotLocal,
}

impl AttachRefusal {
    pub fn code(self) -> &'static str {
        match self {
            AttachRefusal::GrantUnknown => "attach_grant_unknown",
            AttachRefusal::GrantExpired => "attach_grant_expired",
            AttachRefusal::TerminalMismatch => "attach_terminal_mismatch",
            AttachRefusal::Disabled => "remote_attach_disabled",
            AttachRefusal::SessionNotLocal => "session_not_local",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            AttachRefusal::GrantUnknown => "no live attach grant with that jti on this device",
            AttachRefusal::GrantExpired => "the attach grant has expired",
            AttachRefusal::TerminalMismatch => {
                "the attach grant is not bound to the terminal this frame names"
            }
            AttachRefusal::Disabled => "this device does not accept remote attach",
            AttachRefusal::SessionNotLocal => "no local terminal hosts that coord session",
        }
    }
}

/// The in-memory grant table (target role).
#[derive(Default)]
pub struct RemoteAttachGrants {
    inner: Mutex<HashMap<String, AttachGrant>>,
}

impl RemoteAttachGrants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a grant coord published. Idempotent on `grant_jti`: a re-insert
    /// (push + poll both delivering the same row) keeps an existing terminal
    /// binding. Expired rows are purged first; past [`MAX_GRANTS`] the
    /// soonest-expiring row is evicted.
    pub fn insert(&self, grant: AttachGrant, now: u64) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        map.retain(|_, g| g.expires_at > now);
        if let Some(existing) = map.get_mut(&grant.grant_jti) {
            if existing.terminal_id.is_none() {
                existing.terminal_id = grant.terminal_id.clone();
            }
            existing.expires_at = grant.expires_at;
            return;
        }
        if map.len() >= MAX_GRANTS {
            if let Some(victim) = map
                .iter()
                .min_by_key(|(_, g)| g.expires_at)
                .map(|(k, _)| k.clone())
            {
                warn!(
                    evicted = %victim,
                    cap = MAX_GRANTS,
                    "remote attach: grant table at capacity — evicting the soonest-expiring grant"
                );
                map.remove(&victim);
            }
        }
        map.insert(grant.grant_jti.clone(), grant);
    }

    /// Admit or refuse one frame. Order: preference (a disabled device leaks
    /// nothing about which jtis it knows), then presence, then expiry, then
    /// the terminal binding. `terminal_id` is the terminal the FRAME names;
    /// an unbound grant admits only a frame naming no terminal (i.e. the
    /// `terminal_attach` that will bind it).
    pub fn admit(
        &self,
        grant_jti: &str,
        terminal_id: Option<&str>,
        preference: AcceptRemoteAttach,
        now: u64,
    ) -> Result<AttachGrant, AttachRefusal> {
        let grant = self.lookup(grant_jti, preference, now)?;
        match (grant.terminal_id.as_deref(), terminal_id) {
            (Some(bound), Some(named)) if bound == named => {}
            (Some(_), _) | (None, Some(_)) => return Err(AttachRefusal::TerminalMismatch),
            (None, None) => {}
        }
        Ok(grant)
    }

    /// Presence + expiry + preference, WITHOUT the binding check — what
    /// `terminal_attach` needs, since it resolves the terminal itself and
    /// then binds (a re-attach after a relay drop names no terminal but the
    /// grant is already bound; `bind` settles whether they agree).
    pub fn lookup(
        &self,
        grant_jti: &str,
        preference: AcceptRemoteAttach,
        now: u64,
    ) -> Result<AttachGrant, AttachRefusal> {
        if preference == AcceptRemoteAttach::Off {
            return Err(AttachRefusal::Disabled);
        }
        let mut map = self.inner.lock().map_err(|_| AttachRefusal::GrantUnknown)?;
        let Some(grant) = map.get(grant_jti) else {
            return Err(AttachRefusal::GrantUnknown);
        };
        if grant.expires_at <= now {
            map.remove(grant_jti);
            return Err(AttachRefusal::GrantExpired);
        }
        Ok(grant.clone())
    }

    /// Bind an admitted, unbound grant to the terminal it will drive. Returns
    /// `false` when the grant is gone or already bound elsewhere.
    pub fn bind(&self, grant_jti: &str, terminal_id: &str) -> bool {
        let Ok(mut map) = self.inner.lock() else {
            return false;
        };
        match map.get_mut(grant_jti) {
            Some(g) => match g.terminal_id.as_deref() {
                None => {
                    g.terminal_id = Some(terminal_id.to_string());
                    true
                }
                Some(bound) => bound == terminal_id,
            },
            None => false,
        }
    }

    pub fn remove(&self, grant_jti: &str) -> Option<AttachGrant> {
        self.inner.lock().ok()?.remove(grant_jti)
    }

    /// Clear a grant's terminal binding, keeping the row until it expires.
    /// This is what `terminal_detach` does rather than [`Self::remove`]: the
    /// backend tears the attachment down on every SOURCE socket drop — a
    /// relay blip included — and the source re-presents the SAME grant on
    /// reconnect. Coord's grant is still a live capability until `exp`, and
    /// the backend re-verifies it and the presenting device on every
    /// `remote_terminal_attach`, so a re-attach under it binds afresh here;
    /// a removed row would answer `attach_grant_unknown` instead. Returns the
    /// terminal it was bound to, `None` when unknown or unbound.
    pub fn unbind(&self, grant_jti: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()?
            .get_mut(grant_jti)?
            .terminal_id
            .take()
    }

    /// True when some unexpired grant is bound to `terminal_id` — the
    /// outbound relay forwards that terminal's output even with no web
    /// subscriber, because the backend routes it to the attached source.
    pub fn is_terminal_attached(&self, terminal_id: &str, now: u64) -> bool {
        self.inner
            .lock()
            .map(|map| {
                map.values()
                    .any(|g| g.expires_at > now && g.terminal_id.as_deref() == Some(terminal_id))
            })
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn purge_expired(&self, now: u64) {
        if let Ok(mut map) = self.inner.lock() {
            map.retain(|_, g| g.expires_at > now);
        }
    }
}

static GRANTS: OnceLock<RemoteAttachGrants> = OnceLock::new();

/// The process-wide grant table.
pub fn grants() -> &'static RemoteAttachGrants {
    GRANTS.get_or_init(RemoteAttachGrants::new)
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `remote` block the web relay attaches to a forwarded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBlock {
    pub grant_jti: String,
    pub source_device_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub terminal_id: Option<String>,
}

/// `None` when the frame carries no `remote` block (the operator-web path);
/// `Some(Err(()))` when it carries one with no usable `grant_jti` — which is
/// refused as an unknown grant, never treated as absent.
pub fn parse_remote_block(data: &Value) -> Option<Result<RemoteBlock, ()>> {
    let block = data.get("remote")?;
    if block.is_null() {
        return None;
    }
    let Some(obj) = block.as_object() else {
        return Some(Err(()));
    };
    let grant_jti = match obj.get("grant_jti").and_then(|v| v.as_str()) {
        Some(j) if !j.trim().is_empty() => j.trim().to_string(),
        _ => return Some(Err(())),
    };
    Some(Ok(RemoteBlock {
        grant_jti,
        source_device_id: obj
            .get("source_device_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        session_id: obj
            .get("session_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok()),
        terminal_id: obj
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }))
}

/// Gate one inbound target-side frame. `Ok(None)` = no `remote` block, act as
/// today. `Ok(Some(grant))` = a remote frame coord authorised for
/// `terminal_id`. `Err` = refuse, do NOT touch the terminal.
///
/// `preference` is a thunk, evaluated only when a `remote` block is present:
/// the operator-web path must not pay a settings read per keystroke.
pub fn gate_remote_frame<P: FnOnce() -> AcceptRemoteAttach>(
    grants: &RemoteAttachGrants,
    preference: P,
    data: &Value,
    terminal_id: Option<&str>,
    now: u64,
) -> Result<Option<AttachGrant>, AttachRefusal> {
    match parse_remote_block(data) {
        None => Ok(None),
        Some(Err(())) => {
            if preference() == AcceptRemoteAttach::Off {
                Err(AttachRefusal::Disabled)
            } else {
                Err(AttachRefusal::GrantUnknown)
            }
        }
        Some(Ok(block)) => grants
            .admit(&block.grant_jti, terminal_id, preference(), now)
            .map(Some),
    }
}

/// The `remote` echo the backend routes a target-side reply by: the frame's
/// own `source_device_id` + `grant_jti` (whatever it carried — a malformed
/// block echoes nulls, which the backend cannot route but can log).
pub fn remote_echo(data: &Value) -> Value {
    let block = data.get("remote").and_then(|r| r.as_object());
    let pick = |k: &str| {
        block
            .and_then(|b| b.get(k))
            .and_then(|v| v.as_str())
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null)
    };
    json!({
        "source_device_id": pick("source_device_id"),
        "grant_jti": pick("grant_jti"),
    })
}

/// The typed refusal frame the target sends back for `data`. Echoes the
/// frame's `request_id`, `terminal_id` (as resolved by the caller), and its
/// `remote {source_device_id, grant_jti}` block plus a top-level `grant_jti`
/// — the keys the web relay routes a remote-only reply by.
pub fn refusal_frame(refusal: AttachRefusal, data: &Value, terminal_id: Option<&str>) -> Value {
    let remote = remote_echo(data);
    json!({
        "type": "error",
        "code": refusal.code(),
        "message": refusal.message(),
        "request_id": data.get("request_id").cloned().unwrap_or(Value::Null),
        "terminal_id": terminal_id,
        "grant_jti": remote["grant_jti"].clone(),
        "remote": remote,
    })
}

/// The PTY-write side of `terminal_input`, abstracted so the gate can be
/// tested against a recorder that proves a refused frame never reaches it.
pub trait TerminalInputSink {
    /// Write raw bytes into the named terminal. `Err` for an unknown terminal
    /// or a dead PTY.
    fn write_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), String>;
}

impl TerminalInputSink for crate::terminal::TerminalManager {
    fn write_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), String> {
        match self.get(terminal_id) {
            Some(session) => session.write(bytes),
            None => Err(format!("Terminal not found: {terminal_id}")),
        }
    }
}

/// `terminal_input`, gated. Behaviour for a frame with no `remote` block is
/// the relay's pre-existing one: decode, write, warn on failure, no reply.
pub fn apply_terminal_input<S: TerminalInputSink, P: FnOnce() -> AcceptRemoteAttach>(
    sink: &S,
    grants: &RemoteAttachGrants,
    preference: P,
    data: &Value,
    now: u64,
) -> Option<Value> {
    let terminal_id = data
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let input_data = data.get("data").and_then(|v| v.as_str()).unwrap_or("");

    if let Err(refusal) = gate_remote_frame(grants, preference, data, Some(terminal_id), now) {
        warn!(
            terminal_id,
            code = refusal.code(),
            "remote attach: refused terminal_input"
        );
        return Some(refusal_frame(refusal, data, Some(terminal_id)));
    }

    match STANDARD.decode(input_data) {
        Ok(bytes) => {
            if let Err(e) = sink.write_input(terminal_id, &bytes) {
                warn!("Relay: failed to write to terminal {}: {}", terminal_id, e);
            }
        }
        Err(e) => {
            warn!("Invalid base64 terminal input: {}", e);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Source role — the client
// ---------------------------------------------------------------------------

/// Frames queued for the backend socket while no connection is up. Keystrokes
/// are small; this bounds a long outage rather than sizing throughput.
const OUTBOUND_QUEUE: usize = 4096;

/// How long `attach` waits for the target's reply before giving up.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);

/// A parsed `remote_terminal_attached` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedReply {
    pub grant_jti: String,
    pub terminal_id: String,
    pub ring: AttachedRing,
}

/// A typed attach failure — the code is what the picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remote_attach:{}: {}", self.code, self.message)
    }
}

type PendingAttach = oneshot::Sender<Result<AttachedReply, AttachError>>;

/// The source side's routing table and outbound queue.
pub struct RemoteAttachClient {
    out_tx: mpsc::Sender<Value>,
    out_rx: tokio::sync::Mutex<mpsc::Receiver<Value>>,
    panes: Mutex<HashMap<String, Arc<RemotePaneIo>>>,
    pending: Mutex<HashMap<String, PendingAttach>>,
}

static CLIENT: OnceLock<RemoteAttachClient> = OnceLock::new();

/// The process-wide client.
pub fn client() -> &'static RemoteAttachClient {
    CLIENT.get_or_init(RemoteAttachClient::new)
}

impl Default for RemoteAttachClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteAttachClient {
    pub fn new() -> Self {
        let (out_tx, out_rx) = mpsc::channel(OUTBOUND_QUEUE);
        Self {
            out_tx,
            out_rx: tokio::sync::Mutex::new(out_rx),
            panes: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The sink a new [`RemotePaneIo`] writes into.
    pub fn sink(&self) -> Arc<dyn RemoteFrameSink> {
        Arc::new(self.out_tx.clone())
    }

    /// Queue one frame for the socket.
    pub fn send(&self, frame: Value) -> Result<(), String> {
        self.out_tx.send_frame(frame)
    }

    /// Exclusive access to the outbound queue for the life of one relay
    /// connection. The relay's pump holds the guard while it drains; when the
    /// connection ends the guard drops and the next connection takes over.
    pub async fn lock_outbound(&self) -> tokio::sync::MutexGuard<'_, mpsc::Receiver<Value>> {
        self.out_rx.lock().await
    }

    /// Present `grant` to the target through the relay and wait for the
    /// target's ring. Typed errors: the backend's `error {code}` refusals,
    /// the target's `remote_terminal_error`, and a local `timeout`.
    pub async fn attach(
        &self,
        grant: &str,
        cols: u16,
        rows: u16,
        timeout: Duration,
    ) -> Result<AttachedReply, AttachError> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(request_id.clone(), tx);
        }
        let frame = json!({
            "type": "remote_terminal_attach",
            "request_id": request_id,
            "grant": grant,
            "cols": cols,
            "rows": rows,
        });
        if let Err(e) = self.send(frame) {
            self.take_pending(&request_id);
            return Err(AttachError {
                code: "relay_unavailable".to_string(),
                message: e,
            });
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_canceled)) => Err(AttachError {
                code: "attach_canceled".to_string(),
                message: "the attach request was dropped before a reply arrived".to_string(),
            }),
            Err(_elapsed) => {
                self.take_pending(&request_id);
                Err(AttachError {
                    code: "timeout".to_string(),
                    message: format!(
                        "no remote_terminal_attached within {}s — the relay may be \
                         disconnected or the target offline",
                        timeout.as_secs()
                    ),
                })
            }
        }
    }

    fn take_pending(&self, request_id: &str) -> Option<PendingAttach> {
        self.pending.lock().ok()?.remove(request_id)
    }

    /// Register a live pane for inbound routing by `grant_jti`. Finished
    /// panes are swept on every insert.
    pub fn register_pane(&self, pane: Arc<RemotePaneIo>) {
        if let Ok(mut panes) = self.panes.lock() {
            panes.retain(|_, p| !p.is_finished());
            panes.insert(pane.grant_jti().to_string(), pane);
        }
    }

    pub fn pane(&self, grant_jti: &str) -> Option<Arc<RemotePaneIo>> {
        self.panes.lock().ok()?.get(grant_jti).cloned()
    }

    pub fn live_pane_count(&self) -> usize {
        self.panes
            .lock()
            .map(|p| p.values().filter(|p| !p.is_finished()).count())
            .unwrap_or(0)
    }

    fn drop_pane(&self, grant_jti: &str) {
        if let Ok(mut panes) = self.panes.lock() {
            panes.remove(grant_jti);
        }
    }

    /// Route one inbound frame. Returns `true` when this client consumed it.
    /// Handles `remote_terminal_attached|output|exit|buffer|error` and a
    /// generic `error` whose `request_id` names a pending attach.
    pub fn handle_inbound(&self, msg_type: &str, data: &Value) -> bool {
        let request_id = data.get("request_id").and_then(|v| v.as_str());
        let grant_jti = data.get("grant_jti").and_then(|v| v.as_str());
        match msg_type {
            "remote_terminal_attached" => {
                let Some(reply) = parse_attached(data) else {
                    warn!("remote attach: malformed remote_terminal_attached frame: {data}");
                    return true;
                };
                match request_id {
                    Some(rid) if rid.starts_with(REATTACH_PREFIX) => {
                        if let Some(pane) = self.pane(&reply.grant_jti) {
                            pane.splice_replay(&reply.ring);
                            info!(
                                grant_jti = %reply.grant_jti,
                                terminal_id = %reply.terminal_id,
                                "remote attach: reattached after relay reconnect"
                            );
                        }
                    }
                    Some(rid) => match self.take_pending(rid) {
                        Some(tx) => {
                            let _ = tx.send(Ok(reply));
                        }
                        None => warn!(
                            request_id = rid,
                            "remote attach: remote_terminal_attached with no pending request"
                        ),
                    },
                    None => warn!("remote attach: remote_terminal_attached without request_id"),
                }
                true
            }
            "remote_terminal_output" => {
                let Some(jti) = grant_jti else {
                    return true;
                };
                let Some(pane) = self.pane(jti) else {
                    debug!(grant_jti = jti, "remote attach: output for no live pane");
                    return true;
                };
                match data
                    .get("data")
                    .and_then(|v| v.as_str())
                    .map(|s| STANDARD.decode(s))
                {
                    Some(Ok(bytes)) => pane.push_output(&bytes),
                    Some(Err(e)) => warn!("remote attach: undecodable output frame: {e}"),
                    None => {}
                }
                true
            }
            "remote_terminal_buffer" => {
                // A ring the target sent in answer to an explicit buffer
                // request: splice like a reattach.
                if let (Some(jti), Some(reply)) = (grant_jti, parse_attached(data)) {
                    if let Some(pane) = self.pane(jti) {
                        pane.splice_replay(&reply.ring);
                    }
                }
                true
            }
            "remote_terminal_exit" => {
                let Some(jti) = grant_jti else {
                    return true;
                };
                let code = data
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .map(|c| c as i32)
                    .unwrap_or(0);
                if let Some(pane) = self.pane(jti) {
                    pane.mark_exit(code);
                }
                self.drop_pane(jti);
                true
            }
            "remote_terminal_error" => {
                let code = data
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("remote_terminal_error")
                    .to_string();
                let message = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(tx) = request_id.and_then(|rid| self.take_pending(rid)) {
                    let _ = tx.send(Err(AttachError { code, message }));
                    return true;
                }
                // A refused RE-attach names the pane in its request id.
                let jti = grant_jti
                    .or_else(|| request_id.and_then(|rid| rid.strip_prefix(REATTACH_PREFIX)));
                if let Some(jti) = jti {
                    if let Some(pane) = self.pane(jti) {
                        pane.mark_error(&code, &message);
                    }
                    self.drop_pane(jti);
                }
                true
            }
            "error" => {
                // The backend's own refusal of a remote_terminal_attach
                // (attach_grant_invalid / _expired / _wrong_source) arrives as
                // a bare `error` correlated by request_id. A refused RE-attach
                // (`reattach:<jti>`) has no pending slot: the pane it names is
                // closed with the typed code rather than left hanging with a
                // target that no longer routes to it.
                if let Some(jti) = request_id.and_then(|rid| rid.strip_prefix(REATTACH_PREFIX)) {
                    let code = data.get("code").and_then(|v| v.as_str()).unwrap_or("error");
                    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(pane) = self.pane(jti) {
                        pane.mark_error(code, message);
                    }
                    self.drop_pane(jti);
                    return true;
                }
                let Some(tx) = request_id.and_then(|rid| self.take_pending(rid)) else {
                    return false;
                };
                let code = data
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                let message = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _ = tx.send(Err(AttachError { code, message }));
                true
            }
            _ => false,
        }
    }

    /// The relay (re)connected: re-present every live pane's grant so the
    /// target re-binds and returns its ring, which
    /// [`Self::handle_inbound`] splices from the last byte seen.
    pub fn on_relay_connected(&self) {
        let panes: Vec<Arc<RemotePaneIo>> = self
            .panes
            .lock()
            .map(|p| p.values().filter(|p| !p.is_finished()).cloned().collect())
            .unwrap_or_default();
        if panes.is_empty() {
            return;
        }
        info!(
            live_panes = self.live_pane_count(),
            "remote attach: relay connected — re-presenting grants for live remote panes"
        );
        for pane in panes {
            let rid = format!("{REATTACH_PREFIX}{}", pane.grant_jti());
            if let Err(e) = self.send(pane.reattach_frame(&rid)) {
                warn!(
                    grant_jti = %pane.grant_jti(),
                    terminal_id = %pane.terminal_id(),
                    error = %e,
                    "remote attach: reattach frame not queued"
                );
            }
        }
    }
}

const REATTACH_PREFIX: &str = "reattach:";

/// Parse the shared shape of `remote_terminal_attached` / `remote_terminal_buffer`.
fn parse_attached(data: &Value) -> Option<AttachedReply> {
    let grant_jti = data.get("grant_jti")?.as_str()?.to_string();
    let terminal_id = data.get("terminal_id")?.as_str()?.to_string();
    let raw = data
        .get("buffer")
        .or_else(|| data.get("data"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let buffer = STANDARD.decode(raw).ok()?;
    Some(AttachedReply {
        grant_jti,
        terminal_id,
        ring: AttachedRing {
            buffer,
            start_offset: data
                .get("start_offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_bytes_produced: data
                .get("total_bytes_produced")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::pane_io::PaneIo;
    use std::sync::Mutex as StdMutex;

    const NOW: u64 = 1_700_000_000;

    fn grant(jti: &str, terminal: Option<&str>, expires_at: u64) -> AttachGrant {
        AttachGrant {
            grant_jti: jti.to_string(),
            source_device_id: "src-device".to_string(),
            session_id: Uuid::from_u128(7),
            terminal_id: terminal.map(str::to_string),
            expires_at,
        }
    }

    /// Records every PTY write; `Err` for terminals not in `known`.
    #[derive(Default)]
    struct RecordingSink {
        writes: StdMutex<Vec<(String, Vec<u8>)>>,
    }

    impl TerminalInputSink for RecordingSink {
        fn write_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), String> {
            self.writes
                .lock()
                .unwrap()
                .push((terminal_id.to_string(), bytes.to_vec()));
            Ok(())
        }
    }

    impl RecordingSink {
        fn writes(&self) -> Vec<(String, Vec<u8>)> {
            self.writes.lock().unwrap().clone()
        }
    }

    fn input_frame(remote: Option<Value>) -> Value {
        let mut f = json!({
            "type": "terminal_input",
            "terminal_id": "term-A",
            "data": STANDARD.encode(b"rm -rf /\r"),
            "request_id": "req-1",
        });
        if let Some(r) = remote {
            f["remote"] = r;
        }
        f
    }

    // ---- target-side enforcement ------------------------------------------

    #[test]
    fn input_with_unknown_grant_is_refused_and_never_written() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "nope", "source_device_id": "x"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["type"], "error");
        assert_eq!(reply["code"], "attach_grant_unknown");
        assert_eq!(reply["request_id"], "req-1");
        assert_eq!(reply["terminal_id"], "term-A");
        // The web relay routes a remote-only reply by these two keys.
        assert_eq!(reply["grant_jti"], "nope");
        assert_eq!(reply["remote"]["grant_jti"], "nope");
        assert_eq!(reply["remote"]["source_device_id"], "x");
        assert!(
            sink.writes().is_empty(),
            "refused input must not reach the PTY"
        );
    }

    #[test]
    fn input_with_expired_grant_is_refused_and_the_grant_is_dropped() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-A"), NOW - 1), NOW - 10);
        assert_eq!(table.len(), 1);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "attach_grant_expired");
        assert!(sink.writes().is_empty());
        assert!(
            table.is_empty(),
            "an expired grant is purged on first touch"
        );
    }

    #[test]
    fn input_on_a_grant_bound_to_another_terminal_is_refused() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-B"), NOW + 600), NOW);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "attach_terminal_mismatch");
        assert!(sink.writes().is_empty());
    }

    #[test]
    fn input_on_an_unbound_grant_is_refused_until_attach_binds_it() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", None, NOW + 600), NOW);
        let refused = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(refused["code"], "attach_terminal_mismatch");
        assert!(sink.writes().is_empty());

        assert!(table.bind("j1", "term-A"));
        let admitted = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        );
        assert!(admitted.is_none(), "an admitted input sends no reply");
        assert_eq!(
            sink.writes(),
            vec![("term-A".to_string(), b"rm -rf /\r".to_vec())]
        );
        // Re-binding to a different terminal is refused; same terminal is idempotent.
        assert!(!table.bind("j1", "term-Z"));
        assert!(table.bind("j1", "term-A"));
    }

    #[test]
    fn valid_grant_writes_to_the_pty() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-A"), NOW + 600), NOW);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "src-device"}),
            )),
            NOW,
        );
        assert!(reply.is_none());
        assert_eq!(sink.writes().len(), 1);
        assert_eq!(sink.writes()[0].0, "term-A");
    }

    #[test]
    fn frame_with_no_remote_block_is_the_unchanged_operator_path() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new(); // empty: nothing to admit against
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Off, // even with the preference OFF
            &input_frame(None),
            NOW,
        );
        assert!(reply.is_none(), "no reply frame on the legacy path");
        assert_eq!(sink.writes().len(), 1, "the legacy path still writes");
        // `remote: null` is the same as absent.
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Off,
            &input_frame(Some(Value::Null)),
            NOW,
        );
        assert!(reply.is_none());
        assert_eq!(sink.writes().len(), 2);
    }

    #[test]
    fn preference_off_refuses_even_a_valid_grant() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-A"), NOW + 600), NOW);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Off,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "remote_attach_disabled");
        assert!(sink.writes().is_empty());
        // And a malformed block under `off` also reads as disabled, not unknown.
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Off,
            &input_frame(Some(json!({"source_device_id": "x"}))),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "remote_attach_disabled");
    }

    #[test]
    fn malformed_remote_block_is_refused_not_treated_as_absent() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        for bad in [
            json!({}),
            json!({"grant_jti": ""}),
            json!("string"),
            json!(42),
        ] {
            let reply = apply_terminal_input(
                &sink,
                &table,
                || AcceptRemoteAttach::SameUser,
                &input_frame(Some(bad.clone())),
                NOW,
            )
            .unwrap_or_else(|| panic!("{bad} must be refused"));
            assert_eq!(reply["code"], "attach_grant_unknown", "{bad}");
        }
        assert!(sink.writes().is_empty());
    }

    #[test]
    fn grant_table_is_bounded_and_keeps_bindings_on_reinsert() {
        let table = RemoteAttachGrants::new();
        for i in 0..MAX_GRANTS {
            table.insert(grant(&format!("j{i}"), None, NOW + 100 + i as u64), NOW);
        }
        assert_eq!(table.len(), MAX_GRANTS);
        // One more evicts the soonest-expiring (j0).
        table.insert(grant("overflow", None, NOW + 5000), NOW);
        assert_eq!(table.len(), MAX_GRANTS);
        assert!(matches!(
            table.admit("j0", None, AcceptRemoteAttach::Tenant, NOW),
            Err(AttachRefusal::GrantUnknown)
        ));
        assert!(table
            .admit("overflow", None, AcceptRemoteAttach::Tenant, NOW)
            .is_ok());

        // Push + poll delivering the same jti: the binding survives.
        assert!(table.bind("overflow", "term-Q"));
        table.insert(grant("overflow", None, NOW + 6000), NOW);
        let g = table
            .admit("overflow", Some("term-Q"), AcceptRemoteAttach::Tenant, NOW)
            .unwrap();
        assert_eq!(g.terminal_id.as_deref(), Some("term-Q"));
        assert_eq!(g.expires_at, NOW + 6000);

        // Expired rows are purged on insert.
        table.insert(grant("late", None, NOW + 1), NOW);
        table.insert(grant("later", None, NOW + 9000), NOW + 2);
        assert!(matches!(
            table.admit("late", None, AcceptRemoteAttach::Tenant, NOW + 2),
            Err(AttachRefusal::GrantUnknown)
        ));
    }

    #[test]
    fn attached_terminals_are_visible_to_the_outbound_gate() {
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-A"), NOW + 600), NOW);
        table.insert(grant("j2", None, NOW + 600), NOW);
        assert!(table.is_terminal_attached("term-A", NOW));
        assert!(!table.is_terminal_attached("term-B", NOW));
        assert!(
            !table.is_terminal_attached("term-A", NOW + 601),
            "expired binding"
        );
        table.remove("j1");
        assert!(!table.is_terminal_attached("term-A", NOW));
    }

    /// `terminal_detach` unbinds rather than removes: the grant is still a
    /// live capability until `exp`, so the same jti re-attaches (rebinding
    /// to the resolved terminal) after a relay blip — while input under the
    /// unbound grant is refused in between, and the outbound gate no longer
    /// sees the terminal as attached.
    #[test]
    fn detach_unbinds_and_the_same_grant_rebinds() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", None, NOW + 600), NOW);
        assert!(table.bind("j1", "term-A"));
        assert!(table.is_terminal_attached("term-A", NOW));

        assert_eq!(table.unbind("j1").as_deref(), Some("term-A"));
        assert_eq!(table.len(), 1, "the row survives the detach");
        assert!(!table.is_terminal_attached("term-A", NOW));
        let refused = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::SameUser,
            &input_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
        )
        .expect("refused while unbound");
        assert_eq!(refused["code"], "attach_terminal_mismatch");
        assert!(sink.writes().is_empty());

        // Re-attach: binds again (to whatever terminal the session resolves to).
        assert!(table.bind("j1", "term-A"));
        assert!(table
            .admit("j1", Some("term-A"), AcceptRemoteAttach::SameUser, NOW)
            .is_ok());
        // Unknown / already-unbound rows answer None.
        assert_eq!(table.unbind("nope"), None);
        table.unbind("j1");
        assert_eq!(table.unbind("j1"), None);
    }

    #[test]
    fn refusal_codes_are_the_contract_spellings() {
        assert_eq!(AttachRefusal::GrantUnknown.code(), "attach_grant_unknown");
        assert_eq!(AttachRefusal::GrantExpired.code(), "attach_grant_expired");
        assert_eq!(
            AttachRefusal::TerminalMismatch.code(),
            "attach_terminal_mismatch"
        );
        assert_eq!(AttachRefusal::Disabled.code(), "remote_attach_disabled");
        assert_eq!(AttachRefusal::SessionNotLocal.code(), "session_not_local");
    }

    // ---- source-side client -----------------------------------------------

    fn attached_frame(request_id: &str, jti: &str, buf: &[u8], start: u64) -> Value {
        json!({
            "type": "remote_terminal_attached",
            "request_id": request_id,
            "grant_jti": jti,
            "terminal_id": "remote-term",
            "buffer": STANDARD.encode(buf),
            "start_offset": start,
            "total_bytes_produced": start + buf.len() as u64,
        })
    }

    /// attach → the outbound queue carries `remote_terminal_attach`; the
    /// matching `remote_terminal_attached` resolves it with the decoded ring.
    #[tokio::test]
    async fn attach_round_trips_through_the_outbound_queue() {
        let client = RemoteAttachClient::new();
        let fut = client.attach("grant.jwt", 120, 40, Duration::from_secs(5));
        tokio::pin!(fut);
        // Poll once so the frame is queued and the pending entry registered.
        assert!(
            futures_util::poll!(fut.as_mut()).is_pending(),
            "attach must wait for the reply"
        );
        let frame = client
            .lock_outbound()
            .await
            .try_recv()
            .expect("the attach frame was queued");
        assert_eq!(frame["type"], "remote_terminal_attach");
        assert_eq!(frame["grant"], "grant.jwt");
        assert_eq!(frame["cols"], 120);
        assert_eq!(frame["rows"], 40);
        let rid = frame["request_id"].as_str().unwrap().to_string();

        assert!(client.handle_inbound(
            "remote_terminal_attached",
            &attached_frame(&rid, "jti-9", b"ring bytes", 42)
        ));
        let reply = fut.await.expect("attached");
        assert_eq!(reply.grant_jti, "jti-9");
        assert_eq!(reply.terminal_id, "remote-term");
        assert_eq!(reply.ring.buffer, b"ring bytes");
        assert_eq!(reply.ring.start_offset, 42);
        assert_eq!(reply.ring.total_bytes_produced, 52);
    }

    /// A backend `error {request_id, code}` and a target
    /// `remote_terminal_error {request_id, code}` both resolve a pending
    /// attach with the typed code; an unrelated `error` is not consumed.
    #[tokio::test]
    async fn attach_refusals_resolve_with_typed_codes() {
        let client = RemoteAttachClient::new();
        for (msg_type, code) in [
            ("error", "attach_grant_wrong_source"),
            ("remote_terminal_error", "session_not_local"),
        ] {
            let fut = client.attach("g", 80, 24, Duration::from_secs(5));
            tokio::pin!(fut);
            assert!(futures_util::poll!(fut.as_mut()).is_pending());
            let frame = client.lock_outbound().await.try_recv().unwrap();
            let rid = frame["request_id"].as_str().unwrap().to_string();
            assert!(client.handle_inbound(
                msg_type,
                &json!({"type": msg_type, "request_id": rid, "code": code, "message": "no"})
            ));
            let err = fut.await.expect_err("refused");
            assert_eq!(err.code, code);
        }
        assert!(
            !client.handle_inbound("error", &json!({"type": "error", "request_id": "ghost"})),
            "an error for no pending request is left to the relay's warn arm"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn attach_times_out_with_a_typed_error_and_clears_the_pending_slot() {
        let client = RemoteAttachClient::new();
        let err = client
            .attach("g", 80, 24, Duration::from_millis(50))
            .await
            .expect_err("timeout");
        assert_eq!(err.code, "timeout");
        assert!(client.pending.lock().unwrap().is_empty());
    }

    /// Inbound output / exit / error frames reach the pane registered under
    /// their jti; exit and error unregister it.
    #[tokio::test]
    async fn inbound_frames_route_to_the_registered_pane() {
        let client = RemoteAttachClient::new();
        let pane = Arc::new(RemotePaneIo::new(
            "jti-1",
            "remote-term",
            "grant.jwt",
            client.sink(),
            80,
            24,
            AttachedRing::default(),
        ));
        client.register_pane(pane.clone());
        assert_eq!(client.live_pane_count(), 1);
        let reader = pane.reader().unwrap();

        assert!(client.handle_inbound(
            "remote_terminal_output",
            &json!({"grant_jti": "jti-1", "terminal_id": "remote-term", "data": STANDARD.encode(b"abc")})
        ));
        assert!(client.handle_inbound(
            "remote_terminal_output",
            &json!({"grant_jti": "other", "terminal_id": "x", "data": STANDARD.encode(b"ZZZ")})
        ));
        assert!(client.handle_inbound(
            "remote_terminal_exit",
            &json!({"grant_jti": "jti-1", "terminal_id": "remote-term", "exit_code": 3})
        ));
        let bytes = tokio::task::spawn_blocking(move || {
            let mut r = reader;
            let mut out = Vec::new();
            r.read_to_end(&mut out).unwrap();
            out
        })
        .await
        .unwrap();
        assert_eq!(bytes, b"abc", "another jti's output must not leak in");
        assert_eq!(pane.wait(), Ok(3));
        assert!(client.pane("jti-1").is_none(), "exit unregisters the pane");

        // Error path.
        let pane2 = Arc::new(RemotePaneIo::new(
            "jti-2",
            "remote-term",
            "grant.jwt",
            client.sink(),
            80,
            24,
            AttachedRing::default(),
        ));
        client.register_pane(pane2.clone());
        assert!(client.handle_inbound(
            "remote_terminal_error",
            &json!({"grant_jti": "jti-2", "terminal_id": "remote-term", "code": "attach_grant_expired", "message": "gone"})
        ));
        assert_eq!(
            pane2.wait(),
            Ok(crate::terminal::remote_pane_io::ERROR_EXIT_CODE)
        );
        assert!(client.pane("jti-2").is_none());
        assert!(!client.handle_inbound("something_else", &json!({})));
    }

    /// A relay reconnect re-presents every live pane's grant, and the reply
    /// (correlated by the `reattach:` prefix) is spliced from the last byte
    /// seen rather than replayed whole.
    #[tokio::test]
    async fn reconnect_represents_grants_and_splices_the_ring() {
        let client = RemoteAttachClient::new();
        let pane = Arc::new(RemotePaneIo::new(
            "jti-1",
            "remote-term",
            "grant.jwt",
            client.sink(),
            80,
            24,
            AttachedRing {
                buffer: b"0123456789".to_vec(),
                start_offset: 0,
                total_bytes_produced: 10,
            },
        ));
        client.register_pane(pane.clone());
        let reader = pane.reader().unwrap();

        client.on_relay_connected();
        let frame = client.lock_outbound().await.try_recv().unwrap();
        assert_eq!(frame["type"], "remote_terminal_attach");
        assert_eq!(frame["request_id"], "reattach:jti-1");
        assert_eq!(frame["grant"], "grant.jwt");

        assert!(client.handle_inbound(
            "remote_terminal_attached",
            &attached_frame("reattach:jti-1", "jti-1", b"56789ABCDE", 5)
        ));
        pane.mark_exit(0);
        let bytes = tokio::task::spawn_blocking(move || {
            let mut r = reader;
            let mut out = Vec::new();
            r.read_to_end(&mut out).unwrap();
            out
        })
        .await
        .unwrap();
        assert_eq!(bytes, b"0123456789ABCDE");
    }

    /// A refused RE-attach — the backend's bare `error` or the target's
    /// `remote_terminal_error`, correlated only by the `reattach:<jti>`
    /// request id — closes the pane it names with the typed code instead of
    /// leaving a tab open that nothing routes to any more.
    #[tokio::test]
    async fn refused_reattach_closes_the_pane() {
        for (msg_type, code) in [
            ("error", "attach_grant_expired"),
            ("remote_terminal_error", "session_not_local"),
        ] {
            let client = RemoteAttachClient::new();
            let pane = Arc::new(RemotePaneIo::new(
                "jti-1",
                "remote-term",
                "grant.jwt",
                client.sink(),
                80,
                24,
                AttachedRing::default(),
            ));
            client.register_pane(pane.clone());
            client.on_relay_connected();
            let frame = client.lock_outbound().await.try_recv().unwrap();
            assert_eq!(frame["request_id"], "reattach:jti-1");

            assert!(
                client.handle_inbound(
                    msg_type,
                    &json!({"type": msg_type, "request_id": "reattach:jti-1", "code": code, "message": "gone"})
                ),
                "{msg_type} for a reattach id is consumed"
            );
            assert!(pane.is_finished(), "{msg_type}: pane closed");
            assert_eq!(
                pane.wait(),
                Ok(crate::terminal::remote_pane_io::ERROR_EXIT_CODE)
            );
            assert!(client.pane("jti-1").is_none(), "{msg_type}: pane dropped");
        }
    }
}
