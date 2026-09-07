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
    /// binding. Expired rows are purged first. Past [`MAX_GRANTS`] the
    /// soonest-expiring UNBOUND row is evicted; a bound row is a live
    /// attachment, and when every row is bound the new grant is refused
    /// (logged) rather than knocking a live tab off — a flood of mints must
    /// not be a way to sever attachments. Returns `false` when refused.
    pub fn insert(&self, grant: AttachGrant, now: u64) -> bool {
        let Ok(mut map) = self.inner.lock() else {
            return false;
        };
        map.retain(|_, g| g.expires_at > now);
        if let Some(existing) = map.get_mut(&grant.grant_jti) {
            if existing.terminal_id.is_none() {
                existing.terminal_id = grant.terminal_id.clone();
            }
            existing.expires_at = grant.expires_at;
            return true;
        }
        if map.len() >= MAX_GRANTS {
            let victim = map
                .iter()
                .filter(|(_, g)| g.terminal_id.is_none())
                .min_by_key(|(_, g)| g.expires_at)
                .map(|(k, _)| k.clone());
            match victim {
                Some(victim) => {
                    warn!(
                        evicted = %victim,
                        cap = MAX_GRANTS,
                        "remote attach: grant table at capacity — evicting the soonest-expiring unbound grant"
                    );
                    map.remove(&victim);
                }
                None => {
                    warn!(
                        refused = %grant.grant_jti,
                        cap = MAX_GRANTS,
                        "remote attach: grant table at capacity and every row is a live binding — refusing the new grant"
                    );
                    return false;
                }
            }
        }
        map.insert(grant.grant_jti.clone(), grant);
        true
    }

    /// Admit or refuse one frame. Order: preference (a disabled device leaks
    /// nothing about which jtis it knows), then presence, then the source
    /// device, then expiry, then the terminal binding. `terminal_id` is the
    /// terminal the FRAME names; an unbound grant admits only a frame naming
    /// no terminal (i.e. the `terminal_attach` that will bind it).
    /// `source_device_id` is the device the FRAME claims to come from — see
    /// [`Self::lookup`] for how it is cross-checked.
    pub fn admit(
        &self,
        grant_jti: &str,
        source_device_id: Option<&str>,
        terminal_id: Option<&str>,
        preference: AcceptRemoteAttach,
        now: u64,
    ) -> Result<AttachGrant, AttachRefusal> {
        let grant = self.lookup(grant_jti, source_device_id, preference, now)?;
        match (grant.terminal_id.as_deref(), terminal_id) {
            (Some(bound), Some(named)) if bound == named => {}
            (Some(_), _) | (None, Some(_)) => return Err(AttachRefusal::TerminalMismatch),
            (None, None) => {}
        }
        Ok(grant)
    }

    /// Presence + source device + expiry + preference, WITHOUT the binding
    /// check — what `terminal_attach` needs, since it resolves the terminal
    /// itself and then binds (a re-attach after a relay drop names no
    /// terminal but the grant is already bound; `bind` settles whether they
    /// agree).
    ///
    /// Source cross-check (defense in depth behind the backend's own
    /// `attach_grant_wrong_source`): when the table row names a source
    /// device AND the frame names one AND they differ, the frame is refused
    /// as `attach_grant_unknown` — the same answer an unknown jti gets, so a
    /// broker forwarding jti J from the wrong device learns nothing about
    /// whether J exists here. Either side empty skips the check (an older
    /// coord or relay that does not carry the field must not lock the
    /// feature out). Checked before expiry for the same reason: a
    /// wrong-source frame never learns the jti was merely expired.
    pub fn lookup(
        &self,
        grant_jti: &str,
        source_device_id: Option<&str>,
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
        if let Some(claimed) = source_device_id.map(str::trim).filter(|s| !s.is_empty()) {
            let expected = grant.source_device_id.trim();
            if !expected.is_empty() && !expected.eq_ignore_ascii_case(claimed) {
                warn!(
                    grant_jti,
                    granted_source = expected,
                    frame_source = claimed,
                    "remote attach: frame's source device is not the one the grant was minted for — refused as unknown"
                );
                return Err(AttachRefusal::GrantUnknown);
            }
        }
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

    /// The jtis of every unexpired grant bound to `terminal_id` — the remote
    /// subscribers of that terminal, which the outbound flow gate consults
    /// per frame.
    pub fn grants_bound_to(&self, terminal_id: &str, now: u64) -> Vec<String> {
        self.inner
            .lock()
            .map(|map| {
                map.values()
                    .filter(|g| g.expires_at > now && g.terminal_id.as_deref() == Some(terminal_id))
                    .map(|g| g.grant_jti.clone())
                    .collect()
            })
            .unwrap_or_default()
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

// ---------------------------------------------------------------------------
// Target role — flow control across the wire (Phase 5)
// ---------------------------------------------------------------------------

/// What one `remote_terminal_flow` frame did to a subscriber's gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowTransition {
    /// Emission to this subscriber is now withheld.
    Paused,
    /// Emission resumed; `skipped` says whether any frame was withheld while
    /// paused — when true the target owes the source a ring resync so it can
    /// splice what it missed from its last offset.
    Resumed { skipped: bool },
    /// The frame changed nothing (same state twice, or an unknown grant
    /// resuming — nothing was ever withheld from it).
    Unchanged,
}

#[derive(Debug, Default, Clone, Copy)]
struct FlowState {
    paused: bool,
    skipped: bool,
}

/// Per-remote-subscriber emission gates on the TARGET, keyed by `grant_jti`.
///
/// This is the wire-side projection of the SOURCE's `EmissionGate`
/// (`terminal/session.rs`): the hysteresis — watermarks, ack accounting — runs
/// exactly once, on the source, whose local gate decides `paused` and sends it
/// here as `remote_terminal_flow`. The target holds no second policy; it
/// mirrors the flag, withholds that subscriber's `terminal_output` frames
/// while it is set, and remembers that it did so. The PTY read on the target
/// is never touched (the shipped invariant at `session.rs` — "gate emission,
/// never pause reads"): withheld bytes are still in the target's scrollback
/// ring, which the resume-time resync replays from.
#[derive(Default)]
pub struct RemoteFlowGates {
    inner: Mutex<HashMap<String, FlowState>>,
}

impl RemoteFlowGates {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one `remote_terminal_flow {paused}` for `grant_jti`.
    pub fn set_paused(&self, grant_jti: &str, paused: bool) -> FlowTransition {
        let Ok(mut map) = self.inner.lock() else {
            return FlowTransition::Unchanged;
        };
        if paused {
            let st = map.entry(grant_jti.to_string()).or_default();
            if st.paused {
                return FlowTransition::Unchanged;
            }
            st.paused = true;
            st.skipped = false;
            FlowTransition::Paused
        } else {
            match map.remove(grant_jti) {
                Some(st) if st.paused => FlowTransition::Resumed {
                    skipped: st.skipped,
                },
                _ => FlowTransition::Unchanged,
            }
        }
    }

    /// True when emission to `grant_jti` is currently withheld.
    pub fn is_paused(&self, grant_jti: &str) -> bool {
        self.inner
            .lock()
            .map(|m| m.get(grant_jti).is_some_and(|s| s.paused))
            .unwrap_or(false)
    }

    /// Record that a frame was withheld from a paused `grant_jti`.
    pub fn note_skipped(&self, grant_jti: &str) {
        if let Ok(mut map) = self.inner.lock() {
            if let Some(st) = map.get_mut(grant_jti) {
                if st.paused {
                    st.skipped = true;
                }
            }
        }
    }

    /// Forget a subscriber's gate (its grant detached, expired or was
    /// removed).
    pub fn remove(&self, grant_jti: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(grant_jti);
        }
    }

    /// Decide whether one `terminal_output` frame for a terminal should still
    /// go to the relay on behalf of its REMOTE subscribers, given their jtis:
    /// forwarded when at least one is not paused; every paused one is marked
    /// skipped. With no remote subscriber at all the answer is `false` — the
    /// caller's web-subscriber check is the other reason to forward.
    pub fn admit_remote_output(&self, grant_jtis: &[String]) -> bool {
        if grant_jtis.is_empty() {
            return false;
        }
        let Ok(mut map) = self.inner.lock() else {
            return true;
        };
        let mut any_open = false;
        for jti in grant_jtis {
            match map.get_mut(jti) {
                Some(st) if st.paused => st.skipped = true,
                _ => any_open = true,
            }
        }
        any_open
    }
}

static FLOW_GATES: OnceLock<RemoteFlowGates> = OnceLock::new();

/// The process-wide per-subscriber flow gates (target role).
pub fn flow_gates() -> &'static RemoteFlowGates {
    FLOW_GATES.get_or_init(RemoteFlowGates::new)
}

/// How much of the target ring an attach reply ships (Phase 5, lazy
/// scrollback). The rest stays fetchable through
/// `remote_terminal_buffer {from_offset, to_offset}` and is announced to the
/// source through `ring_start_offset` on the reply.
pub const REMOTE_ATTACH_TAIL_BYTES: usize = 64 * 1024;

/// The bounded tail of a ring snapshot `(data, start_offset)`: returns the
/// tail bytes and the absolute offset of the tail's first byte.
pub fn attach_tail(data: &[u8], start_offset: u64, cap: usize) -> (&[u8], u64) {
    if data.len() <= cap {
        return (data, start_offset);
    }
    let skip = data.len() - cap;
    (&data[skip..], start_offset.saturating_add(skip as u64))
}

/// Slice a ring snapshot `(data, start_offset)` to the absolute range
/// `[from, to)`, clamped to what the ring holds. Returns the bytes and the
/// absolute offset of the first returned byte (which is `max(from, start)`,
/// or the ring end when the range lies entirely outside it).
pub fn slice_ring(data: &[u8], start_offset: u64, from: Option<u64>, to: Option<u64>) -> (&[u8], u64) {
    let end_offset = start_offset.saturating_add(data.len() as u64);
    let lo = from.unwrap_or(start_offset).clamp(start_offset, end_offset);
    let hi = to.unwrap_or(end_offset).clamp(lo, end_offset);
    let a = (lo - start_offset) as usize;
    let b = (hi - start_offset) as usize;
    (&data[a..b], lo)
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
            .admit(
                &block.grant_jti,
                block.source_device_id.as_deref(),
                terminal_id,
                preference(),
                now,
            )
            .map(Some),
    }
}

/// Gate a target-side `terminal_resize` / `_close` / `_buffer` frame on its
/// `remote` block. `Some(frame)` is the typed refusal to send back — the
/// caller returns it WITHOUT touching the terminal; `None` means proceed
/// (either no `remote` block — the operator-web path — or an admitted
/// grant). Pure so the "refused means untouched" ordering is testable
/// without a `TerminalManager`.
pub fn refuse_remote_frame<P: FnOnce() -> AcceptRemoteAttach>(
    grants: &RemoteAttachGrants,
    preference: P,
    data: &Value,
    terminal_id: &str,
    now: u64,
) -> Option<Value> {
    match gate_remote_frame(grants, preference, data, Some(terminal_id), now) {
        Ok(_) => None,
        Err(refusal) => {
            warn!(
                terminal_id,
                code = refusal.code(),
                msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                "remote attach: refused frame"
            );
            Some(refusal_frame(refusal, data, Some(terminal_id)))
        }
    }
}

/// The decision half of `terminal_attach` (target role), pure over the grant
/// table and a terminal resolver so it is testable without a
/// `TerminalManager`. Parses the `remote` block, looks the grant up (source
/// device cross-checked), resolves the grant's session to a local terminal
/// through `resolve_terminal`, and binds the grant to it. `Ok` carries the
/// block, the grant, and whatever the resolver returned for the terminal;
/// `Err` is the exact frame to send back.
pub fn admit_terminal_attach<T, P, R>(
    grants: &RemoteAttachGrants,
    preference: P,
    data: &Value,
    now: u64,
    resolve_terminal: R,
) -> Result<(RemoteBlock, AttachGrant, String, T), Value>
where
    P: FnOnce() -> AcceptRemoteAttach,
    R: FnOnce(Uuid) -> Option<(String, T)>,
{
    let request_id = data.get("request_id").cloned().unwrap_or(Value::Null);
    let block = match parse_remote_block(data) {
        Some(Ok(block)) => block,
        Some(Err(())) => return Err(refusal_frame(AttachRefusal::GrantUnknown, data, None)),
        None => {
            return Err(json!({
                "type": "error",
                "code": "remote_block_required",
                "message": "terminal_attach carries no remote block — a remote attach is admitted only under a coord-minted grant",
                "request_id": request_id,
                "remote": remote_echo(data),
            }));
        }
    };

    let grant = match grants.lookup(
        &block.grant_jti,
        block.source_device_id.as_deref(),
        preference(),
        now,
    ) {
        Ok(grant) => grant,
        Err(refusal) => {
            warn!(
                grant_jti = %block.grant_jti,
                code = refusal.code(),
                "remote attach: terminal_attach refused"
            );
            return Err(refusal_frame(refusal, data, None));
        }
    };

    // The table row came from coord directly; the frame's session_id came
    // from the grant claim via the backend. They should agree — the table
    // wins, and a disagreement is logged.
    if let Some(named) = block.session_id {
        if named != grant.session_id {
            warn!(
                grant_jti = %block.grant_jti,
                table_session = %grant.session_id,
                frame_session = %named,
                "remote attach: frame names a different session than the grant — using the grant's"
            );
        }
    }

    let Some((terminal_id, terminal)) = resolve_terminal(grant.session_id) else {
        warn!(
            grant_jti = %block.grant_jti,
            session = %grant.session_id,
            "remote attach: no local terminal hosts that coord session"
        );
        return Err(json!({
            "type": "remote_terminal_error",
            "request_id": request_id,
            "grant_jti": block.grant_jti,
            "remote": remote_echo(data),
            "code": AttachRefusal::SessionNotLocal.code(),
            "message": AttachRefusal::SessionNotLocal.message(),
        }));
    };

    // Bind at first use; a re-attach must land on the same terminal.
    if !grants.bind(&block.grant_jti, &terminal_id) {
        return Err(refusal_frame(
            AttachRefusal::TerminalMismatch,
            data,
            Some(&terminal_id),
        ));
    }
    Ok((block, grant, terminal_id, terminal))
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

/// Depth of the outbound queue. It absorbs a BURST while the pump is busy;
/// it does not carry frames across a disconnect — the backend tears the
/// attachment down on every source-socket drop, so anything queued while
/// no connection was up is stale by the time the next one is, and
/// [`RemoteAttachClient::discard_backlog`] drops it before the reattaches
/// are queued. Keystrokes are small; 4096 is a burst bound, not throughput.
const OUTBOUND_QUEUE: usize = 4096;

/// How long `attach` waits for the target's reply before giving up.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-grant cap on output buffered between `remote_terminal_attached` and
/// `register_pane`. That window is one `TerminalManager::create_with_io`
/// long — milliseconds — so the cap only matters against a target that is
/// flooding; past it the oldest chunk goes, with a warning.
pub const PENDING_OUTPUT_CAP_BYTES: usize = 1 << 20;

/// Refusal codes that mean the attachment is GONE — the target or backend
/// will never route to this grant again — so the pane closes with the code.
/// Anything else (`attach_terminal_mismatch` during a re-bind window, a
/// stale frame refused after a reconnect, a code this build does not know)
/// is logged and the pane kept: the next reattach re-binds it.
const FATAL_REMOTE_ERROR_CODES: &[&str] = &[
    "attach_grant_unknown",
    "attach_grant_expired",
    "attach_grant_invalid",
    "attach_grant_wrong_source",
    "attach_grant_consumed",
    "session_not_local",
    "remote_attach_disabled",
    "listener_lost",
];

fn is_fatal_remote_error(code: &str) -> bool {
    FATAL_REMOTE_ERROR_CODES.contains(&code)
}

/// Output chunks for a grant whose pane is not registered yet.
#[derive(Default)]
struct PendingOutput {
    chunks: std::collections::VecDeque<Vec<u8>>,
    bytes: usize,
}

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
    /// Output that arrived between the `remote_terminal_attached` reply and
    /// `register_pane`, keyed by grant jti. Without it those frames were
    /// dropped ("output for no live pane") AND the pane's `remote_offset`
    /// fell behind the target's, so the next reattach splice skipped the
    /// wrong prefix.
    pending_output: Mutex<HashMap<String, PendingOutput>>,
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
            pending_output: Mutex::new(HashMap::new()),
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

    /// Drop every frame queued before this connection existed. The backend
    /// tore every attachment down when the previous socket dropped, so each
    /// queued input / resize / flow / detach would be refused as stale — and
    /// would go out BEFORE the `reattach:<jti>` frames
    /// [`Self::on_relay_connected`] queues. The pump calls this with the
    /// guard it holds for the connection's life, before the `connected` ack
    /// can queue the reattaches. Returns how many frames were discarded.
    pub fn discard_backlog(rx: &mut mpsc::Receiver<Value>) -> usize {
        let mut discarded = 0usize;
        while rx.try_recv().is_ok() {
            discarded += 1;
        }
        if discarded > 0 {
            info!(
                discarded,
                "remote attach: dropped outbound frames queued before this relay connection — the backend tore those attachments down; reattaching instead"
            );
        }
        discarded
    }

    /// Open a pending-output slot for `grant_jti`: from now until
    /// [`Self::register_pane`], `remote_terminal_output` for that jti is
    /// buffered instead of dropped.
    fn open_pending_output(&self, grant_jti: &str) {
        if let Ok(mut slots) = self.pending_output.lock() {
            slots.entry(grant_jti.to_string()).or_default();
        }
    }

    /// Buffer one decoded chunk if a slot is open. `false` when no slot.
    fn buffer_pending_output(&self, grant_jti: &str, bytes: Vec<u8>) -> bool {
        let Ok(mut slots) = self.pending_output.lock() else {
            return false;
        };
        let Some(slot) = slots.get_mut(grant_jti) else {
            return false;
        };
        slot.bytes += bytes.len();
        slot.chunks.push_back(bytes);
        while slot.bytes > PENDING_OUTPUT_CAP_BYTES && slot.chunks.len() > 1 {
            if let Some(oldest) = slot.chunks.pop_front() {
                slot.bytes -= oldest.len();
                warn!(
                    grant_jti,
                    dropped_bytes = oldest.len(),
                    cap = PENDING_OUTPUT_CAP_BYTES,
                    "remote attach: pre-registration output over cap — dropped the oldest chunk"
                );
            }
        }
        true
    }

    /// Close and return the slot's chunks (empty when none).
    fn take_pending_output(&self, grant_jti: &str) -> Vec<Vec<u8>> {
        self.pending_output
            .lock()
            .ok()
            .and_then(|mut slots| slots.remove(grant_jti))
            .map(|slot| slot.chunks.into_iter().collect())
            .unwrap_or_default()
    }

    /// Forget any pre-registration output for a grant whose pane will never
    /// be registered (the attach was answered but the tab failed to open).
    pub fn discard_pending_output(&self, grant_jti: &str) {
        let _ = self.take_pending_output(grant_jti);
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

    /// Phase 5 lazy scrollback: ask the target for the ring range
    /// `[from, to)` a pane did not receive at attach and wait for the
    /// `remote_terminal_buffer` that answers it. The bytes are returned to
    /// the caller — NOT spliced into the pane's stream, which is past them.
    pub async fn request_history(
        &self,
        pane: &RemotePaneIo,
        from: u64,
        to: u64,
        timeout: Duration,
    ) -> Result<AttachedReply, AttachError> {
        let request_id = format!("{HISTORY_PREFIX}{}", Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(request_id.clone(), tx);
        }
        let frame = json!({
            "type": "remote_terminal_buffer",
            "request_id": request_id,
            "grant_jti": pane.grant_jti(),
            "terminal_id": pane.terminal_id(),
            "from_offset": from,
            "to_offset": to,
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
                code: "history_canceled".to_string(),
                message: "the history request was dropped before a reply arrived".to_string(),
            }),
            Err(_elapsed) => {
                self.take_pending(&request_id);
                Err(AttachError {
                    code: "timeout".to_string(),
                    message: format!(
                        "no remote_terminal_buffer within {}s — the relay may be disconnected \
                         or the target offline",
                        timeout.as_secs()
                    ),
                })
            }
        }
    }

    /// The relay connection carrying every live pane dropped: write the
    /// in-band notice into each pane. Nothing closes — a drop is not an
    /// exit — and [`Self::on_relay_connected`] reattaches when it returns.
    pub fn on_relay_disconnected(&self) {
        let panes: Vec<Arc<RemotePaneIo>> = self
            .panes
            .lock()
            .map(|p| p.values().filter(|p| !p.is_finished()).cloned().collect())
            .unwrap_or_default();
        if panes.is_empty() {
            return;
        }
        info!(
            live_panes = panes.len(),
            "remote attach: relay disconnected — live remote panes wait for reconnect"
        );
        for pane in panes {
            pane.note_relay_lost();
        }
    }

    /// Register a live pane for inbound routing by `grant_jti`. Finished
    /// panes are swept on every insert. Output buffered since the
    /// `remote_terminal_attached` reply is then delivered — after the seed
    /// ring, which `RemotePaneIo::new` already queued.
    ///
    /// Residual: a chunk the target emitted after it bound the grant but
    /// before it snapshotted the ring, and which the socket delivered AFTER
    /// the `attached` reply, is in both the ring and this buffer and is
    /// shown twice. Output frames carry no offset, so it cannot be spliced
    /// out here; the window is the target's bind→snapshot gap, a few
    /// microseconds inside one handler.
    pub fn register_pane(&self, pane: Arc<RemotePaneIo>) {
        let jti = pane.grant_jti().to_string();
        if let Ok(mut panes) = self.panes.lock() {
            panes.retain(|_, p| !p.is_finished());
            panes.insert(jti.clone(), pane.clone());
        }
        let buffered = self.take_pending_output(&jti);
        if !buffered.is_empty() {
            debug!(
                grant_jti = %jti,
                chunks = buffered.len(),
                "remote attach: delivering output buffered before the pane registered"
            );
            for chunk in buffered {
                pane.push_output(&chunk);
            }
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
                            // Open the buffer BEFORE the waiter is woken:
                            // this arm and the output arm run on the same
                            // inbound loop, so no output frame for this jti
                            // can be routed between here and the pane's
                            // registration without finding the slot. Frames
                            // before the reply were already in the ring.
                            let jti = reply.grant_jti.clone();
                            self.open_pending_output(&jti);
                            if tx.send(Ok(reply)).is_err() {
                                // The waiter timed out first: nobody will
                                // register a pane for this grant.
                                self.discard_pending_output(&jti);
                            }
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
                let bytes = match data
                    .get("data")
                    .and_then(|v| v.as_str())
                    .map(|s| STANDARD.decode(s))
                {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(e)) => {
                        warn!("remote attach: undecodable output frame: {e}");
                        return true;
                    }
                    None => return true,
                };
                match self.pane(jti) {
                    Some(pane) => pane.push_output(&bytes),
                    None => {
                        if !self.buffer_pending_output(jti, bytes) {
                            debug!(grant_jti = jti, "remote attach: output for no live pane");
                        }
                    }
                }
                true
            }
            "remote_terminal_buffer" => {
                let Some(reply) = parse_attached(data) else {
                    warn!("remote attach: malformed remote_terminal_buffer frame: {data}");
                    return true;
                };
                // A history range the operator asked for resolves its waiter
                // (`request_history`) and is NOT spliced — the pane's stream
                // is already past it. Anything else — the target's resync
                // after a flow resume, an unsolicited ring — splices like a
                // reattach, from the last byte seen.
                if let Some(tx) = request_id
                    .filter(|rid| rid.starts_with(HISTORY_PREFIX))
                    .and_then(|rid| self.take_pending(rid))
                {
                    let _ = tx.send(Ok(reply));
                    return true;
                }
                if let Some(pane) = self.pane(&reply.grant_jti) {
                    pane.splice_replay(&reply.ring);
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
                    self.settle_pane_error(jti, &code, &message);
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
                    self.settle_pane_error(jti, code, message);
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

    /// An error frame for a live pane with no pending request to resolve:
    /// close the pane only for a code that means the attachment is gone
    /// (see [`FATAL_REMOTE_ERROR_CODES`]); otherwise keep it — one refused
    /// stale frame after a reconnect, or an `attach_terminal_mismatch`
    /// during a re-bind window, must not close a live tab with exit 1.
    fn settle_pane_error(&self, jti: &str, code: &str, message: &str) {
        if is_fatal_remote_error(code) {
            if let Some(pane) = self.pane(jti) {
                pane.mark_error(code, message);
            }
            self.drop_pane(jti);
        } else {
            warn!(
                grant_jti = jti,
                code,
                message,
                live_pane = self.pane(jti).is_some(),
                "remote attach: non-fatal remote error — pane kept; the next reattach re-binds it"
            );
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
const HISTORY_PREFIX: &str = "history:";

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
            history_start: data.get("ring_start_offset").and_then(|v| v.as_u64()),
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
            assert!(table.insert(grant(&format!("j{i}"), None, NOW + 100 + i as u64), NOW));
        }
        assert_eq!(table.len(), MAX_GRANTS);
        // j5 becomes a live attachment; it expires sooner than almost
        // everything else, which is exactly what must NOT get it evicted.
        assert!(table.bind("j5", "term-Q"));

        // One more evicts the soonest-expiring UNBOUND row (j0), not j5.
        assert!(table.insert(grant("overflow", None, NOW + 5000), NOW));
        assert_eq!(table.len(), MAX_GRANTS);
        assert!(matches!(
            table.admit("j0", None, None, AcceptRemoteAttach::Tenant, NOW),
            Err(AttachRefusal::GrantUnknown)
        ));
        assert!(table
            .admit("overflow", None, None, AcceptRemoteAttach::Tenant, NOW)
            .is_ok());

        // A flood of MAX_GRANTS further mints churns every unbound row and
        // never touches the bound one.
        for i in 0..MAX_GRANTS {
            assert!(table.insert(
                grant(&format!("flood{i}"), None, NOW + 7000 + i as u64),
                NOW
            ));
        }
        assert_eq!(table.len(), MAX_GRANTS);
        let live = table
            .admit("j5", None, Some("term-Q"), AcceptRemoteAttach::Tenant, NOW)
            .expect("a bound grant survives a 256-row flood");
        assert_eq!(live.terminal_id.as_deref(), Some("term-Q"));
        assert!(table.is_terminal_attached("term-Q", NOW));

        // Push + poll delivering the same jti: the binding survives and the
        // expiry is refreshed.
        table.insert(grant("j5", None, NOW + 6000), NOW);
        let g = table
            .admit("j5", None, Some("term-Q"), AcceptRemoteAttach::Tenant, NOW)
            .unwrap();
        assert_eq!(g.terminal_id.as_deref(), Some("term-Q"));
        assert_eq!(g.expires_at, NOW + 6000);

        // Expired rows are purged on insert.
        table.insert(grant("late", None, NOW + 1), NOW);
        table.insert(grant("later", None, NOW + 9000), NOW + 2);
        assert!(matches!(
            table.admit("late", None, None, AcceptRemoteAttach::Tenant, NOW + 2),
            Err(AttachRefusal::GrantUnknown)
        ));
    }

    /// When every row is a live binding there is no safe victim: the new
    /// grant is refused rather than a live tab severed.
    #[test]
    fn full_table_of_live_bindings_refuses_rather_than_evicts() {
        let table = RemoteAttachGrants::new();
        for i in 0..MAX_GRANTS {
            table.insert(
                grant(
                    &format!("j{i}"),
                    Some(&format!("t{i}")),
                    NOW + 100 + i as u64,
                ),
                NOW,
            );
        }
        assert_eq!(table.len(), MAX_GRANTS);
        assert!(!table.insert(grant("one-too-many", None, NOW + 5000), NOW));
        assert_eq!(table.len(), MAX_GRANTS);
        assert!(matches!(
            table.admit("one-too-many", None, None, AcceptRemoteAttach::Tenant, NOW),
            Err(AttachRefusal::GrantUnknown)
        ));
        for i in 0..MAX_GRANTS {
            assert!(
                table.is_terminal_attached(&format!("t{i}"), NOW),
                "t{i} still bound"
            );
        }
        // Once one binding is released (detach), the next mint fits again.
        table.unbind("j3");
        assert!(table.insert(grant("fits-now", None, NOW + 5000), NOW));
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
            .admit(
                "j1",
                None,
                Some("term-A"),
                AcceptRemoteAttach::SameUser,
                NOW
            )
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

    /// Finding 1 — defense in depth behind the backend's own source check:
    /// a frame forwarded for jti J from a device other than the one coord
    /// minted J for is refused, as `attach_grant_unknown` (never a code that
    /// confirms J exists), and never reaches the PTY.
    #[test]
    fn input_from_the_wrong_source_device_is_refused_as_unknown() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", Some("term-A"), NOW + 600), NOW);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "someone-else"}),
            )),
            NOW,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "attach_grant_unknown");
        assert!(
            sink.writes().is_empty(),
            "wrong-source input must not reach the PTY"
        );
        // Even an EXPIRED grant answers unknown to the wrong source — the
        // wrong device learns nothing about the jti.
        table.insert(grant("j2", Some("term-A"), NOW + 1), NOW);
        let reply = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(
                json!({"grant_jti": "j2", "source_device_id": "someone-else"}),
            )),
            NOW + 5,
        )
        .expect("a refusal frame");
        assert_eq!(reply["code"], "attach_grant_unknown");
        assert!(sink.writes().is_empty());

        // The right device (case-insensitively) is admitted.
        let admitted = apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "SRC-DEVICE"}),
            )),
            NOW,
        );
        assert!(admitted.is_none());
        assert_eq!(sink.writes().len(), 1);
    }

    /// The cross-check is skipped when EITHER side is empty: an older coord
    /// row with no source device, or a relay that does not forward the field.
    #[test]
    fn source_cross_check_is_skipped_when_either_side_is_empty() {
        let sink = RecordingSink::default();
        let table = RemoteAttachGrants::new();
        let mut no_source = grant("j1", Some("term-A"), NOW + 600);
        no_source.source_device_id = String::new();
        table.insert(no_source, NOW);
        table.insert(grant("j2", Some("term-A"), NOW + 600), NOW);
        // Table side empty: any claimed source is admitted.
        assert!(apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "whoever"})
            )),
            NOW,
        )
        .is_none());
        // Frame side absent or blank: admitted against a table row that has one.
        assert!(apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(json!({"grant_jti": "j2"}))),
            NOW,
        )
        .is_none());
        assert!(apply_terminal_input(
            &sink,
            &table,
            || AcceptRemoteAttach::Tenant,
            &input_frame(Some(json!({"grant_jti": "j2", "source_device_id": "  "}))),
            NOW,
        )
        .is_none());
        assert_eq!(sink.writes().len(), 3);
    }

    /// Finding 6 — `terminal_resize` / `_close` / `_buffer` with a `remote`
    /// block against an empty table answer a typed error. The relay handlers
    /// call this seam BEFORE `tm.get`, so a `Some` here is a frame that
    /// touched no session; a frame with no `remote` block passes through.
    #[test]
    fn resize_close_buffer_with_a_remote_block_are_refused_against_an_empty_table() {
        let table = RemoteAttachGrants::new();
        for msg_type in ["terminal_resize", "terminal_close", "terminal_buffer"] {
            let frame = json!({
                "type": msg_type,
                "terminal_id": "term-A",
                "request_id": format!("req-{msg_type}"),
                "cols": 80, "rows": 24,
                "remote": {"grant_jti": "nope", "source_device_id": "x"},
            });
            let reply = refuse_remote_frame(
                &table,
                || AcceptRemoteAttach::SameUser,
                &frame,
                "term-A",
                NOW,
            )
            .unwrap_or_else(|| panic!("{msg_type} must be refused"));
            assert_eq!(reply["type"], "error", "{msg_type}");
            assert_eq!(reply["code"], "attach_grant_unknown", "{msg_type}");
            assert_eq!(reply["request_id"], format!("req-{msg_type}"));
            assert_eq!(reply["terminal_id"], "term-A");
            assert_eq!(reply["grant_jti"], "nope");

            let legacy = json!({"type": msg_type, "terminal_id": "term-A"});
            assert!(
                refuse_remote_frame(&table, || AcceptRemoteAttach::Off, &legacy, "term-A", NOW)
                    .is_none(),
                "{msg_type} with no remote block is the operator-web path"
            );
        }
    }

    fn attach_frame(remote: Option<Value>) -> Value {
        let mut f =
            json!({"type": "terminal_attach", "request_id": "att-1", "cols": 80, "rows": 24});
        if let Some(r) = remote {
            f["remote"] = r;
        }
        f
    }

    /// Finding 6 — the `terminal_attach` decision seam (the handler itself
    /// needs a Tauri `AppHandle`, so the extracted predicate is what is
    /// tested): no `remote` block, unknown grant, wrong source, no local
    /// terminal, and a re-attach resolving to a different terminal than the
    /// grant is bound to.
    #[test]
    fn terminal_attach_seam_answers_each_refusal_and_binds_on_success() {
        let table = RemoteAttachGrants::new();
        table.insert(grant("j1", None, NOW + 600), NOW);
        let resolves_to = |t: &'static str| move |_sid: Uuid| Some((t.to_string(), ()));
        let none = |_sid: Uuid| -> Option<(String, ())> { None };

        // No remote block.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(None),
            NOW,
            resolves_to("term-A"),
        )
        .expect_err("remote_block_required");
        assert_eq!(err["type"], "error");
        assert_eq!(err["code"], "remote_block_required");
        assert_eq!(err["request_id"], "att-1");

        // Unknown grant.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(json!({"grant_jti": "ghost"}))),
            NOW,
            resolves_to("term-A"),
        )
        .expect_err("unknown");
        assert_eq!(err["code"], "attach_grant_unknown");

        // Wrong source device: unknown, and the grant stays unbound.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "intruder"}),
            )),
            NOW,
            resolves_to("term-A"),
        )
        .expect_err("wrong source");
        assert_eq!(err["code"], "attach_grant_unknown");
        assert!(!table.is_terminal_attached("term-A", NOW));

        // No local terminal hosts the session.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "src-device"}),
            )),
            NOW,
            none,
        )
        .expect_err("not local");
        assert_eq!(err["type"], "remote_terminal_error");
        assert_eq!(err["code"], "session_not_local");
        assert_eq!(err["grant_jti"], "j1");
        assert_eq!(err["request_id"], "att-1");

        // Success binds to the resolved terminal.
        let (block, admitted, terminal_id, ()) = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(
                json!({"grant_jti": "j1", "source_device_id": "src-device"}),
            )),
            NOW,
            resolves_to("term-A"),
        )
        .expect("admitted");
        assert_eq!(block.grant_jti, "j1");
        assert_eq!(admitted.grant_jti, "j1");
        assert_eq!(terminal_id, "term-A");
        assert!(table.is_terminal_attached("term-A", NOW));

        // Re-attach resolving to another terminal: bind mismatch.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
            resolves_to("term-B"),
        )
        .expect_err("mismatch");
        assert_eq!(err["code"], "attach_terminal_mismatch");
        assert_eq!(err["terminal_id"], "term-B");
        // Same terminal is idempotent.
        assert!(admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Tenant,
            &attach_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
            resolves_to("term-A"),
        )
        .is_ok());
        // Preference off refuses before anything else.
        let err = admit_terminal_attach(
            &table,
            || AcceptRemoteAttach::Off,
            &attach_frame(Some(json!({"grant_jti": "j1"}))),
            NOW,
            resolves_to("term-A"),
        )
        .expect_err("disabled");
        assert_eq!(err["code"], "remote_attach_disabled");
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
                history_start: None,
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

    // ---- Phase 5: flow gates, ring slicing, lazy history -----------------

    /// Pause → withheld frames mark `skipped` → resume reports it; a resume
    /// with nothing withheld, a second pause, and a resume of an unknown
    /// grant are all what they say.
    #[test]
    fn flow_gate_transitions_report_what_was_withheld() {
        let gates = RemoteFlowGates::new();
        assert_eq!(gates.set_paused("a", false), FlowTransition::Unchanged);
        assert_eq!(gates.set_paused("a", true), FlowTransition::Paused);
        assert_eq!(gates.set_paused("a", true), FlowTransition::Unchanged);
        assert!(gates.is_paused("a"));
        assert!(!gates.is_paused("b"));
        assert_eq!(
            gates.set_paused("a", false),
            FlowTransition::Resumed { skipped: false }
        );
        assert!(!gates.is_paused("a"));

        assert_eq!(gates.set_paused("a", true), FlowTransition::Paused);
        gates.note_skipped("a");
        gates.note_skipped("zzz"); // unknown: no entry is created
        assert!(!gates.is_paused("zzz"));
        assert_eq!(
            gates.set_paused("a", false),
            FlowTransition::Resumed { skipped: true }
        );
        // `remove` forgets a paused gate outright — a later resume is a no-op.
        assert_eq!(gates.set_paused("c", true), FlowTransition::Paused);
        gates.remove("c");
        assert_eq!(gates.set_paused("c", false), FlowTransition::Unchanged);
    }

    /// The outbound decision: forwarded while ANY bound subscriber is open,
    /// withheld (and remembered) once every one of them is paused, and never
    /// forwarded on remote grounds with no subscriber at all.
    #[test]
    fn remote_output_is_withheld_only_when_every_subscriber_is_paused() {
        let gates = RemoteFlowGates::new();
        let both = vec!["p".to_string(), "q".to_string()];
        assert!(!gates.admit_remote_output(&[]));
        assert!(gates.admit_remote_output(&both));
        gates.set_paused("p", true);
        assert!(gates.admit_remote_output(&both), "q is still open");
        gates.set_paused("q", true);
        assert!(!gates.admit_remote_output(&both));
        assert_eq!(
            gates.set_paused("p", false),
            FlowTransition::Resumed { skipped: true }
        );
        assert_eq!(
            gates.set_paused("q", false),
            FlowTransition::Resumed { skipped: true }
        );
    }

    /// `grants_bound_to` lists exactly the unexpired grants bound to the
    /// terminal, by jti.
    #[test]
    fn grants_bound_to_lists_live_subscribers_of_a_terminal() {
        let table = RemoteAttachGrants::new();
        table.insert(grant("live", Some("t1"), NOW + 100), NOW);
        table.insert(grant("other", Some("t2"), NOW + 100), NOW);
        table.insert(grant("unbound", None, NOW + 100), NOW);
        table.insert(grant("stale", Some("t1"), NOW - 1), NOW - 10);
        let mut jtis = table.grants_bound_to("t1", NOW);
        jtis.sort();
        assert_eq!(jtis, vec!["live".to_string()]);
        assert!(table.grants_bound_to("t9", NOW).is_empty());
    }

    /// Ring helpers: the attach tail keeps the LAST `cap` bytes with a
    /// corrected start offset; the range slice clamps to what the ring holds.
    #[test]
    fn attach_tail_and_slice_ring_keep_offsets_honest() {
        let data = b"0123456789";
        assert_eq!(attach_tail(data, 100, 4), (&b"6789"[..], 106));
        assert_eq!(attach_tail(data, 100, 10), (&data[..], 100));
        assert_eq!(attach_tail(data, 100, 64), (&data[..], 100));
        assert_eq!(
            slice_ring(data, 100, Some(102), Some(105)),
            (&b"234"[..], 102)
        );
        assert_eq!(slice_ring(data, 100, None, None), (&data[..], 100));
        // Below the ring: clamped up to the ring start.
        assert_eq!(slice_ring(data, 100, Some(5), Some(103)), (&b"012"[..], 100));
        // Past the ring: empty, anchored at the ring end.
        assert_eq!(slice_ring(data, 100, Some(500), None), (&b""[..], 110));
        // Inverted range: empty at `from`.
        assert_eq!(slice_ring(data, 100, Some(107), Some(103)), (&b""[..], 107));
    }

    /// The attach reply's `ring_start_offset` lands on the ring as
    /// `history_start`; a reply without it reads as `None`.
    #[test]
    fn attached_frame_carries_the_ring_start_when_present() {
        let mut f = attached_frame("r", "j", b"tail", 900);
        assert_eq!(parse_attached(&f).unwrap().ring.history_start, None);
        f["ring_start_offset"] = json!(100);
        assert_eq!(parse_attached(&f).unwrap().ring.history_start, Some(100));
    }

    /// Lazy history: the request goes out as `remote_terminal_buffer` with the
    /// range, the correlated reply resolves the waiter WITHOUT touching the
    /// pane's stream, and an uncorrelated buffer still splices.
    #[tokio::test]
    async fn history_request_resolves_without_splicing_the_pane() {
        let client = RemoteAttachClient::new();
        let pane = Arc::new(RemotePaneIo::new(
            "jti-h",
            "remote-term",
            "grant.jwt",
            client.sink(),
            80,
            24,
            AttachedRing {
                buffer: b"tail".to_vec(),
                start_offset: 1_000,
                total_bytes_produced: 1_004,
                history_start: Some(200),
            },
        ));
        client.register_pane(pane.clone());
        assert_eq!(pane.history_range(), Some((200, 1_000)));

        let fut = client.request_history(&pane, 200, 1_000, Duration::from_secs(5));
        tokio::pin!(fut);
        assert!(futures_util::poll!(fut.as_mut()).is_pending());
        let frame = client.lock_outbound().await.try_recv().unwrap();
        assert_eq!(frame["type"], "remote_terminal_buffer");
        assert_eq!(frame["grant_jti"], "jti-h");
        assert_eq!(frame["terminal_id"], "remote-term");
        assert_eq!(frame["from_offset"], 200);
        assert_eq!(frame["to_offset"], 1_000);
        let rid = frame["request_id"].as_str().unwrap().to_string();
        assert!(rid.starts_with(HISTORY_PREFIX));

        let mut reply = attached_frame(&rid, "jti-h", b"older", 200);
        reply["type"] = json!("remote_terminal_buffer");
        assert!(client.handle_inbound("remote_terminal_buffer", &reply));
        let got = fut.await.expect("history");
        assert_eq!(got.ring.buffer, b"older");
        assert_eq!(got.ring.start_offset, 200);
        // The pane's stream did not move: history is returned, not spliced.
        assert_eq!(pane.remote_offset(), 1_004);

        // An UNSOLICITED buffer (the target's flow-resume resync) splices.
        let mut resync = attached_frame("resync", "jti-h", b"tailMORE", 1_000);
        resync["type"] = json!("remote_terminal_buffer");
        assert!(client.handle_inbound("remote_terminal_buffer", &resync));
        assert_eq!(pane.remote_offset(), 1_008);
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

    fn new_pane(client: &RemoteAttachClient, jti: &str, seed: AttachedRing) -> Arc<RemotePaneIo> {
        Arc::new(RemotePaneIo::new(
            jti,
            "remote-term",
            "grant.jwt",
            client.sink(),
            80,
            24,
            seed,
        ))
    }

    fn read_all(pane: &Arc<RemotePaneIo>) -> Vec<u8> {
        let mut r = pane.reader().unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        out
    }

    /// Finding 2 — output that arrives between the `remote_terminal_attached`
    /// reply and `register_pane` is buffered, delivered after the seed ring,
    /// and counted in `remote_offset` so the next reattach splice is right.
    #[tokio::test]
    async fn output_delivered_before_register_pane_is_readable_after_it() {
        let client = RemoteAttachClient::new();
        let fut = client.attach("grant.jwt", 80, 24, Duration::from_secs(5));
        tokio::pin!(fut);
        assert!(futures_util::poll!(fut.as_mut()).is_pending());
        let frame = client.lock_outbound().await.try_recv().unwrap();
        let rid = frame["request_id"].as_str().unwrap().to_string();

        // Before the reply: no slot, the frame is dropped as before (it is
        // in the ring the reply carries).
        assert!(client.handle_inbound(
            "remote_terminal_output",
            &json!({"grant_jti": "jti-7", "data": STANDARD.encode(b"IGNORED")})
        ));
        assert!(client.handle_inbound(
            "remote_terminal_attached",
            &attached_frame(&rid, "jti-7", b"seed", 100)
        ));
        let reply = fut.await.expect("attached");

        // Between the reply and registration: buffered, not dropped.
        for chunk in [&b"abc"[..], &b"def"[..]] {
            assert!(client.handle_inbound(
                "remote_terminal_output",
                &json!({"grant_jti": "jti-7", "data": STANDARD.encode(chunk)})
            ));
        }
        let pane = new_pane(&client, "jti-7", reply.ring);
        assert_eq!(pane.remote_offset(), 104, "seed only, before registration");
        client.register_pane(pane.clone());
        assert_eq!(
            pane.remote_offset(),
            110,
            "buffered bytes advance the splice point"
        );
        assert!(
            client.pending_output.lock().unwrap().is_empty(),
            "the slot is consumed"
        );

        // A later frame routes straight to the pane.
        assert!(client.handle_inbound(
            "remote_terminal_output",
            &json!({"grant_jti": "jti-7", "data": STANDARD.encode(b"g")})
        ));
        pane.mark_exit(0);
        let pane2 = pane.clone();
        let bytes = tokio::task::spawn_blocking(move || read_all(&pane2))
            .await
            .unwrap();
        assert_eq!(bytes, b"seedabcdefg");
    }

    /// The pre-registration buffer is bounded: past the cap the oldest chunk
    /// goes, and a slot whose tab never opens is discarded.
    #[test]
    fn pending_output_is_capped_and_discardable() {
        let client = RemoteAttachClient::new();
        client.open_pending_output("jti-1");
        let big = vec![b'x'; PENDING_OUTPUT_CAP_BYTES / 2 + 1];
        assert!(client.buffer_pending_output("jti-1", b"first".to_vec()));
        assert!(client.buffer_pending_output("jti-1", big.clone()));
        assert!(client.buffer_pending_output("jti-1", big.clone()));
        let chunks = client.take_pending_output("jti-1");
        assert_eq!(chunks.len(), 1, "over cap: the two oldest were dropped");
        assert_eq!(chunks[0].len(), big.len());
        // No slot: not buffered.
        assert!(!client.buffer_pending_output("jti-1", b"late".to_vec()));
        client.open_pending_output("jti-2");
        client.discard_pending_output("jti-2");
        assert!(!client.buffer_pending_output("jti-2", b"late".to_vec()));
    }

    /// Finding 3 — frames queued while no connection was up are stale (the
    /// backend tore the attachment down); a new connection discards them so
    /// the first frame out is the `remote_terminal_attach` reattach.
    #[tokio::test]
    async fn reconnect_discards_the_stale_backlog_before_reattaching() {
        let client = RemoteAttachClient::new();
        let pane = new_pane(&client, "jti-1", AttachedRing::default());
        client.register_pane(pane.clone());
        // Queued while disconnected: input, resize, flow.
        for t in [
            "remote_terminal_input",
            "remote_terminal_resize",
            "remote_terminal_flow",
        ] {
            client
                .send(json!({"type": t, "grant_jti": "jti-1"}))
                .unwrap();
        }
        // The pump for the new connection: take the guard, drop the backlog,
        // hold the guard while the `connected` ack queues the reattaches.
        let mut rx = client.lock_outbound().await;
        assert_eq!(RemoteAttachClient::discard_backlog(&mut rx), 3);
        client.on_relay_connected();
        let first = rx.try_recv().expect("the reattach frame");
        assert_eq!(first["type"], "remote_terminal_attach");
        assert_eq!(first["request_id"], "reattach:jti-1");
        assert!(rx.try_recv().is_err(), "nothing stale follows");
        // Frames queued on the live connection flow as normal.
        client
            .send(json!({"type": "remote_terminal_input", "grant_jti": "jti-1"}))
            .unwrap();
        assert_eq!(rx.try_recv().unwrap()["type"], "remote_terminal_input");
    }

    /// Finding 4 — a `remote_terminal_error` for a live pane closes it only
    /// for a code that means the attachment is gone; `attach_terminal_mismatch`
    /// (a re-bind window) or an unknown code is logged and the pane kept.
    #[test]
    fn non_fatal_remote_errors_keep_the_pane() {
        let client = RemoteAttachClient::new();
        let pane = new_pane(&client, "jti-1", AttachedRing::default());
        client.register_pane(pane.clone());
        for (msg_type, rid, code) in [
            (
                "remote_terminal_error",
                Value::Null,
                "attach_terminal_mismatch",
            ),
            (
                "remote_terminal_error",
                Value::Null,
                "some_code_this_build_does_not_know",
            ),
            (
                "remote_terminal_error",
                json!("reattach:jti-1"),
                "attach_terminal_mismatch",
            ),
            ("error", json!("reattach:jti-1"), "attach_terminal_mismatch"),
        ] {
            let mut frame =
                json!({"type": msg_type, "grant_jti": "jti-1", "code": code, "message": "m"});
            if !rid.is_null() {
                frame["request_id"] = rid;
            }
            if msg_type == "error" {
                frame.as_object_mut().unwrap().remove("grant_jti");
            }
            assert!(
                client.handle_inbound(msg_type, &frame),
                "{msg_type}/{code} consumed"
            );
            assert!(
                client.pane("jti-1").is_some(),
                "{msg_type}/{code}: pane still registered"
            );
            assert!(!pane.is_finished(), "{msg_type}/{code}: pane still live");
        }
        assert!(is_fatal_remote_error("attach_grant_unknown"));
        assert!(is_fatal_remote_error("listener_lost"));
        assert!(!is_fatal_remote_error("attach_terminal_mismatch"));
        // A fatal code still closes it.
        assert!(client.handle_inbound(
            "remote_terminal_error",
            &json!({"grant_jti": "jti-1", "code": "attach_grant_consumed", "message": "gone"})
        ));
        assert!(pane.is_finished());
        assert!(client.pane("jti-1").is_none());
    }
}
