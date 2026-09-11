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

use crate::settings::{AcceptRemoteAttach, AcceptRemoteCreate};
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

    /// True when this jti names a row in the ATTACH table, expired or not.
    ///
    /// Used by the create gate, and only there: a jti coord minted as an
    /// attach grant must never buy a PTY spawn, however the block reaching
    /// this machine was stamped. Expired rows count — a jti that was an attach
    /// grant does not become a create grant by ageing.
    pub fn contains(&self, grant_jti: &str) -> bool {
        self.inner
            .lock()
            .map(|map| map.contains_key(grant_jti))
            .unwrap_or(false)
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
// Target role — CREATE grants (plan
// `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase 3b)
// ---------------------------------------------------------------------------

/// One create grant as this runner knows it, learned from COORD — the
/// `create_request` directive or the `GET /sessions/create-requests` catch-up
/// poll — and never from the frame that presents it.
///
/// **That provenance is the whole point.** Before Phase 3b the target held no
/// create-grant table at all: it read `remote.kind == "create"` off the frame
/// the web relay forwarded and spawned a PTY, so the relay alone decided
/// whether a coord grant existed. A relay defect or a compromised relay then
/// spawned PTYs here. A create grant has no session and no terminal, so the
/// row is just the jti, the source device it was minted for, and its expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateGrant {
    pub grant_jti: String,
    /// The source device coord minted the grant for. Empty when the feed did
    /// not carry one (an older coord) — the cross-check then skips, exactly as
    /// the attach table's does.
    pub source_device_id: String,
    /// Unix seconds.
    pub expires_at: u64,
}

/// The in-memory create-grant table (target role). The twin of
/// [`RemoteAttachGrants`], minus everything a create has no room for: no
/// terminal binding (the terminal is what the grant will create) and no
/// session (none exists yet).
///
/// **Single-use, and single-use against the FEED as well as against a replay.**
/// [`Self::consume`] removes the row it returns: a create grant buys ONE spawn.
/// But removal alone is not enough, because the row's SOURCE is coord's pending
/// list, and coord's list keeps serving a grant until its `consumed_at` is set:
/// the 60 s catch-up poll would re-insert the jti this process just spent and
/// the "single use" would be a per-minute allowance. So a spent jti is also
/// remembered in [`Self::spent`] and [`Self::insert`] refuses to resurrect it.
///
/// The tombstone is the belt; coord's `POST …/create-requests/{jti}/consume` is
/// the braces, and both are needed. The tombstone alone dies with the process;
/// coord's flip alone leaves the window between the local spend and the flip.
///
/// The relay's Redis `SET NX` claim is neither: it is RELEASED when the create
/// completes, while the grant JWT stays valid for the rest of its 15 minutes.
#[derive(Default)]
pub struct RemoteCreateGrants {
    inner: Mutex<HashMap<String, CreateGrant>>,
    /// jtis this process has already spent, and their `expires_at`. Kept until
    /// the grant's own expiry — past that coord will not serve it either, so
    /// the tombstone has nothing left to guard and is purged with the rest.
    spent: Mutex<HashMap<String, u64>>,
}

impl RemoteCreateGrants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a grant coord published. Idempotent on `grant_jti` (push and
    /// poll both deliver the same row); expired rows are purged first; past
    /// [`MAX_GRANTS`] the soonest-expiring row is evicted. No row here is ever
    /// "live" the way a bound attach row is — a create grant is consumed the
    /// moment it is used — so eviction has no attachment to sever and never
    /// refuses. Returns `false` only when the lock is poisoned.
    pub fn insert(&self, grant: CreateGrant, now: u64) -> bool {
        // A jti this process already spent is NEVER re-admitted, however it
        // arrives. Coord keeps serving a grant until its `consumed_at` is set,
        // and the catch-up poll runs every 60 s, so without this the single-use
        // property degrades into "once per poll interval".
        if self.is_spent(&grant.grant_jti, now) {
            warn!(
                grant_jti = %grant.grant_jti,
                "remote create: refusing to re-record a grant this device already spent \\
                 (coord has not marked it consumed yet)"
            );
            return false;
        }
        let Ok(mut map) = self.inner.lock() else {
            return false;
        };
        map.retain(|_, g| g.expires_at > now);
        if let Some(existing) = map.get_mut(&grant.grant_jti) {
            existing.expires_at = grant.expires_at;
            if existing.source_device_id.trim().is_empty() {
                existing.source_device_id = grant.source_device_id;
            }
            return true;
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
                    "remote create: grant table at capacity — evicting the soonest-expiring grant"
                );
                map.remove(&victim);
            }
        }
        map.insert(grant.grant_jti.clone(), grant);
        true
    }

    /// Presence + source device + expiry, WITHOUT consuming. The read a test
    /// or a diagnostic wants; the admission path calls [`Self::consume`].
    ///
    /// Order, and the reason for it, is [`RemoteAttachGrants::lookup`]'s: a
    /// source-device mismatch answers the SAME refusal as an unknown jti, so a
    /// broker replaying jti J from the wrong device learns nothing about
    /// whether J exists here; and it is checked BEFORE expiry so a wrong-source
    /// frame never learns the jti was merely expired.
    pub fn lookup(
        &self,
        grant_jti: &str,
        source_device_id: Option<&str>,
        now: u64,
    ) -> Result<CreateGrant, CreateRefusal> {
        self.resolve(grant_jti, source_device_id, now, false)
    }

    /// [`Self::lookup`] and REMOVE the row, **atomically** — the admission
    /// path. See the type docs for why a create grant is single-use.
    ///
    /// One lock spans the check and the removal, deliberately: a check that
    /// released the lock before removing would let two concurrent frames
    /// carrying the same jti both pass and both spawn, which is precisely the
    /// double-spend this table exists to prevent. Exactly one caller can see
    /// `Ok` for a given jti.
    pub fn consume(
        &self,
        grant_jti: &str,
        source_device_id: Option<&str>,
        now: u64,
    ) -> Result<CreateGrant, CreateRefusal> {
        self.resolve(grant_jti, source_device_id, now, true)
    }

    /// The one implementation behind [`Self::lookup`] and [`Self::consume`],
    /// so the two can never drift into checking different things — and so the
    /// consume is one critical section rather than two.
    ///
    /// A POISONED lock answers [`CreateRefusal::GrantUnknown`]: the table's
    /// contents are then unknowable, and "unknown" is the fail-closed reading.
    fn resolve(
        &self,
        grant_jti: &str,
        source_device_id: Option<&str>,
        now: u64,
        take: bool,
    ) -> Result<CreateGrant, CreateRefusal> {
        // A zero clock means [`now_epoch_secs`] could not read the system time.
        // Every `expires_at <= 0` comparison is then false, i.e. every grant
        // reads as unexpired — fail-OPEN on an expiry check. Refuse instead.
        if now == 0 {
            warn!(
                grant_jti,
                "remote create: refused — this device cannot read its clock, so no grant's \
                 expiry can be evaluated"
            );
            return Err(CreateRefusal::GrantUnknown);
        }
        if self.is_spent(grant_jti, now) {
            return Err(CreateRefusal::GrantUnknown);
        }
        let mut map = self.inner.lock().map_err(|_| CreateRefusal::GrantUnknown)?;
        let Some(grant) = map.get(grant_jti) else {
            return Err(CreateRefusal::GrantUnknown);
        };
        // The source binding is REQUIRED, not merely checked when offered.
        //
        // The relay is the party this whole check defends against, and it is
        // the party that stamps `remote.source_device_id`. A "check it if
        // present" rule is therefore opt-out BY THE ADVERSARY: omit the key and
        // the binding evaporates. Coord's ledger column is `UUID NOT NULL` and
        // its directive always carries the field, so a grant whose row names a
        // source can only meet a frame that names one too — unless something
        // stripped it.
        let expected = grant.source_device_id.trim();
        if !expected.is_empty() {
            match source_device_id.map(str::trim).filter(|s| !s.is_empty()) {
                Some(claimed) if expected.eq_ignore_ascii_case(claimed) => {}
                claimed => {
                    warn!(
                        grant_jti,
                        granted_source = expected,
                        frame_source = claimed.unwrap_or("<absent>"),
                        "remote create: the frame's source device is not the one the grant was \
                         minted for (or names none at all) — refused as unknown"
                    );
                    return Err(CreateRefusal::GrantUnknown);
                }
            }
        }
        if grant.expires_at <= now {
            map.remove(grant_jti);
            return Err(CreateRefusal::GrantExpired);
        }
        let grant = grant.clone();
        if take {
            map.remove(grant_jti);
            drop(map);
            self.mark_spent(&grant, now);
        }
        Ok(grant)
    }

    /// True when this process already spent `grant_jti` and the grant has not
    /// yet expired. Expired tombstones are purged on the way past: coord stops
    /// serving an expired grant too, so there is nothing left to guard.
    ///
    /// A poisoned tombstone lock answers `true` — fail CLOSED. The cost is
    /// refusing creates on a process whose lock is already broken; the
    /// alternative is admitting a spent grant.
    fn is_spent(&self, grant_jti: &str, now: u64) -> bool {
        let Ok(mut spent) = self.spent.lock() else {
            return true;
        };
        spent.retain(|_, exp| *exp > now);
        spent.contains_key(grant_jti)
    }

    fn mark_spent(&self, grant: &CreateGrant, now: u64) {
        if let Ok(mut spent) = self.spent.lock() {
            spent.retain(|_, exp| *exp > now);
            spent.insert(grant.grant_jti.clone(), grant.expires_at);
        }
    }

    /// True when this jti names a row here, expired or not. The mirror of
    /// [`RemoteAttachGrants::contains`], and used for the same kind of
    /// cross-check.
    pub fn contains(&self, grant_jti: &str) -> bool {
        self.inner
            .lock()
            .map(|map| map.contains_key(grant_jti))
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
        if let Ok(mut spent) = self.spent.lock() {
            spent.retain(|_, exp| *exp > now);
        }
    }

    /// How many spent-jti tombstones are live — a diagnostic, and what the
    /// resurrection tests assert against.
    pub fn spent_len(&self, now: u64) -> usize {
        self.spent
            .lock()
            .map(|mut s| {
                s.retain(|_, exp| *exp > now);
                s.len()
            })
            .unwrap_or(0)
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
pub fn slice_ring(
    data: &[u8],
    start_offset: u64,
    from: Option<u64>,
    to: Option<u64>,
) -> (&[u8], u64) {
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

static CREATE_GRANTS: OnceLock<RemoteCreateGrants> = OnceLock::new();

/// The process-wide CREATE grant table — deliberately a SECOND table rather
/// than a `kind` column on the attach one. The two capabilities have disjoint
/// lifecycles (an attach row is bound and re-bound and lives to `exp`; a create
/// row is consumed by its one use) and disjoint shapes, and keeping them apart
/// is what makes `attach_grants.contains(jti)` a meaningful cross-check.
pub fn create_grants() -> &'static RemoteCreateGrants {
    CREATE_GRANTS.get_or_init(RemoteCreateGrants::new)
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Which capability the grant behind a `remote` block claims.
///
/// The web relay stamps `kind` from the grant's own `sub_type` — `attach_grant`
/// → [`RemoteGrantKind::Attach`], `create_grant` → [`RemoteGrantKind::Create`]
/// — and the two are NOT interchangeable here: an attach grant drives a PTY
/// the operator already opened, a create grant spawns one. Anything the relay
/// did not stamp `create` (absent, misspelled, a non-string) reads as
/// [`RemoteGrantKind::Attach`], the narrower of the two, so an unrecognised
/// value can only ever lose capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteGrantKind {
    #[default]
    Attach,
    Create,
}

impl RemoteGrantKind {
    /// The wire spelling the relay stamps.
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteGrantKind::Attach => "attach",
            RemoteGrantKind::Create => "create",
        }
    }
}

/// The `remote` block the web relay attaches to a forwarded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBlock {
    pub grant_jti: String,
    pub source_device_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub terminal_id: Option<String>,
    /// What the grant behind this block is for. Fails closed to
    /// [`RemoteGrantKind::Attach`] — see [`RemoteGrantKind`].
    pub kind: RemoteGrantKind,
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
        kind: match obj.get("kind").and_then(|v| v.as_str()).map(str::trim) {
            Some("create") => RemoteGrantKind::Create,
            _ => RemoteGrantKind::Attach,
        },
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

// ---------------------------------------------------------------------------
// Target role — remote CREATE (plan
// `2026-09-11-headless-runner-parity-from-a-headed-runner`, D2)
// ---------------------------------------------------------------------------

/// **D2, in one sentence: the TARGET chooses the working directory.**
///
/// A remote `terminal_create` spawns a PTY on THIS machine and can allocate a
/// worktree and take a coord claim, so neither the directory nor the repo may
/// come off the wire. The caller may express a PREFERENCE; this device answers
/// it out of its own configuration ([`CreateTargets`], built from
/// `settings.remote_create`), and a preference that is not a member of that
/// set is REFUSED — never quietly redirected to something else, because a
/// silent redirect teaches a caller that its value was honoured.
///
/// Concretely, the value this device spawns in is ALWAYS a string that came
/// out of [`CreateTargets`]; a caller-supplied string is only ever compared,
/// never used. That is what makes path trickery (`..`, a symlink, a differing
/// spelling of the same directory) uninteresting here: the worst a matching
/// string can achieve is the directory the operator already listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRoot {
    /// The label a caller may name in `working_dir_key`.
    pub key: String,
    /// The directory this device actually spawns in.
    pub path: String,
}

/// What this device is willing to spawn a remote terminal in, and under which
/// edit intents. Resolved on the target, per create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateTargets {
    /// In order; the FIRST is what a create naming no directory lands in.
    /// Empty means this device resolved nowhere to spawn — a refusal, not a
    /// fallback to the process cwd.
    pub roots: Vec<CreateRoot>,
    /// The `intent_repo` values a remote create may declare. Empty means NONE.
    pub repos: Vec<String>,
}

impl CreateTargets {
    pub fn keys(&self) -> Vec<String> {
        self.roots.iter().map(|r| r.key.clone()).collect()
    }
}

/// Why a remote `terminal_create` was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRefusal {
    Disabled,
    GrantRequired,
    /// This device holds no live create grant with that jti — including the
    /// case where it holds one minted for a DIFFERENT source device, which
    /// answers the same code deliberately (see [`RemoteCreateGrants::lookup`]).
    /// **The refusal the relay-trust hole became.** Reaching it means the relay
    /// forwarded a `terminal_create` whose grant coord never told this device
    /// about; before Phase 3b that frame spawned a PTY.
    GrantUnknown,
    /// The jti is one coord minted for this device, and it has expired.
    GrantExpired,
    NoTargetDirectory,
    WorkingDirNotAllowed,
    IntentRepoNotAllowed,
}

impl CreateRefusal {
    pub fn code(self) -> &'static str {
        match self {
            CreateRefusal::Disabled => "remote_create_disabled",
            CreateRefusal::GrantRequired => "remote_create_grant_required",
            CreateRefusal::GrantUnknown => "remote_create_grant_unknown",
            CreateRefusal::GrantExpired => "remote_create_grant_expired",
            CreateRefusal::NoTargetDirectory => "remote_create_no_target_directory",
            CreateRefusal::WorkingDirNotAllowed => "remote_create_working_dir_not_allowed",
            CreateRefusal::IntentRepoNotAllowed => "remote_create_intent_repo_not_allowed",
        }
    }

    /// The refusal text. [`CreateRefusal::Disabled`] NAMES the preference and
    /// says how to turn it on: the default is `off`, so without that sentence
    /// the first thing every operator meets reads as a bug rather than as a
    /// decision.
    pub fn message(self) -> &'static str {
        match self {
            CreateRefusal::Disabled => {
                "this device does not accept remote terminal creation — it is off by default; \
                 set `remote_create.accept_remote_create` to `same_user` or `tenant` in the \
                 runner's settings.json to allow it"
            }
            CreateRefusal::GrantRequired => {
                "terminal_create is admitted only under a coord-minted CREATE grant — an attach \
                 grant does not authorise spawning a terminal"
            }
            CreateRefusal::GrantUnknown => {
                "no live create grant with that jti on this device — coord did not tell this \
                 device about it, and this device does not take the relay's word for a grant"
            }
            CreateRefusal::GrantExpired => "the create grant has expired",
            CreateRefusal::NoTargetDirectory => {
                "this device resolved no directory to spawn a remote terminal in — list one in \
                 `remote_create.allowed_working_dirs`, or set `paths.workspace_root`"
            }
            CreateRefusal::WorkingDirNotAllowed => {
                "the requested working directory is not one this device offers for remote \
                 creation — name one of `allowed_working_dir_keys`"
            }
            CreateRefusal::IntentRepoNotAllowed => {
                "the requested intent_repo is not one this device offers for remote creation — \
                 list it in `remote_create.allowed_intent_repos`"
            }
        }
    }
}

/// Normalize a directory string for COMPARISON only: trim, `\` → `/`, drop a
/// trailing separator. Never used to build the path that is spawned in.
fn normalize_dir(raw: &str) -> String {
    let unified = raw.trim().replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    if trimmed.is_empty() {
        unified
    } else {
        trimmed.to_string()
    }
}

/// Resolve the directory a remote create spawns in — always one of
/// `targets.roots`, never the caller's string.
///
/// - No preference → the first root (this device's default).
/// - `working_dir_key` → exact key match, else [`CreateRefusal::WorkingDirNotAllowed`].
/// - `working_dir` → admitted only when it normalizes to a root's own path,
///   and even then the ROOT's spelling is returned. A `..` segment is refused
///   outright rather than resolved.
///
/// `working_dir_key` wins when both are present; they cannot disagree
/// usefully, and preferring the label keeps the path form from becoming the
/// interesting one.
pub fn resolve_create_working_dir(
    targets: &CreateTargets,
    requested_key: Option<&str>,
    requested_dir: Option<&str>,
) -> Result<String, CreateRefusal> {
    let Some(default_root) = targets.roots.first() else {
        return Err(CreateRefusal::NoTargetDirectory);
    };
    if let Some(key) = requested_key.map(str::trim).filter(|k| !k.is_empty()) {
        return targets
            .roots
            .iter()
            .find(|r| r.key.trim() == key)
            .map(|r| r.path.clone())
            .ok_or(CreateRefusal::WorkingDirNotAllowed);
    }
    if let Some(dir) = requested_dir.map(str::trim).filter(|d| !d.is_empty()) {
        let wanted = normalize_dir(dir);
        if wanted.split('/').any(|seg| seg == "..") {
            return Err(CreateRefusal::WorkingDirNotAllowed);
        }
        return targets
            .roots
            .iter()
            .find(|r| normalize_dir(&r.path) == wanted)
            .map(|r| r.path.clone())
            .ok_or(CreateRefusal::WorkingDirNotAllowed);
    }
    Ok(default_root.path.clone())
}

/// Resolve a remote create's `intent_repo` against this device's allowlist.
/// `Ok(None)` = none declared (and none is DERIVED for a remote create — the
/// local path's `working_dir`-derived intent is deliberately not reachable
/// from the wire, or an allowlisted directory that happens to be a checkout
/// would allocate a worktree nobody asked for).
pub fn resolve_create_intent_repo(
    targets: &CreateTargets,
    requested: Option<&str>,
) -> Result<Option<String>, CreateRefusal> {
    let Some(requested) = requested.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    targets
        .repos
        .iter()
        .find(|r| r.trim() == requested)
        .cloned()
        .map(Some)
        .ok_or(CreateRefusal::IntentRepoNotAllowed)
}

/// A remote create this device admitted: the block it came under, and the
/// TARGET-RESOLVED spawn parameters the handler must use in place of anything
/// the frame carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedCreate {
    pub block: RemoteBlock,
    pub working_dir: String,
    pub intent_repo: Option<String>,
}

/// The typed refusal frame for a remote create. Carries the allowlist the
/// caller missed (keys and repos), so a source can offer the operator a choice
/// instead of guessing paths.
pub fn create_refusal_frame(
    refusal: CreateRefusal,
    data: &Value,
    targets: Option<&CreateTargets>,
) -> Value {
    let remote = remote_echo(data);
    let mut frame = json!({
        "type": "error",
        "code": refusal.code(),
        "message": refusal.message(),
        "request_id": data.get("request_id").cloned().unwrap_or(Value::Null),
        "grant_jti": remote["grant_jti"].clone(),
        "remote": remote,
    });
    if let Some(targets) = targets {
        frame["allowed_working_dir_keys"] = json!(targets.keys());
        frame["allowed_intent_repos"] = json!(targets.repos);
    }
    frame
}

/// Gate one inbound `terminal_create`.
///
/// `Ok(None)` — the frame carries no `remote` block: the operator-web / mobile
/// path, untouched, caller-chosen `working_dir` and all. `Ok(Some(admitted))`
/// — a remote create this device authorises, whose `working_dir` and
/// `intent_repo` the caller did NOT choose. `Err(frame)` — the typed refusal
/// to send back, having touched no terminal.
///
/// Order: block → kind → preference → attach-table cross-check → **this
/// device's own create-grant table** → directory → repo.
///
/// The kind check is first among the refusals because it closes the hole the
/// gate was first written for: `terminal_create` used to be reachable by
/// anything holding an ATTACH grant. The create-grant lookup is the Phase 3b
/// addition and closes the deeper one: every check above it reads fields the
/// RELAY stamped, so together they only ever established *"the relay says a
/// create grant backs this"*. [`RemoteCreateGrants::consume`] asks a table this
/// device built from COORD's own directive and catch-up poll, so a frame whose
/// jti coord never minted for this device is refused however the relay stamped
/// it — which is the property a relay bug or compromise must not be able to
/// spend.
///
/// It is ALSO where the grant is spent — but at the END, not at that lookup:
/// see the two comments in the body. One grant buys one spawn, and no refusal
/// burns one.
pub fn admit_terminal_create<P, T>(
    attach_grants: &RemoteAttachGrants,
    create_grants: &RemoteCreateGrants,
    preference: P,
    targets: T,
    data: &Value,
    now: u64,
) -> Result<Option<AdmittedCreate>, Value>
where
    P: FnOnce() -> AcceptRemoteCreate,
    T: FnOnce() -> CreateTargets,
{
    let block = match parse_remote_block(data) {
        None => return Ok(None),
        Some(Err(())) => {
            return Err(create_refusal_frame(
                CreateRefusal::GrantRequired,
                data,
                None,
            ))
        }
        Some(Ok(block)) => block,
    };

    if block.kind != RemoteGrantKind::Create {
        warn!(
            grant_jti = %block.grant_jti,
            kind = block.kind.as_str(),
            "remote create: refused — the grant behind this frame is not a create grant"
        );
        return Err(create_refusal_frame(
            CreateRefusal::GrantRequired,
            data,
            None,
        ));
    }

    if preference() == AcceptRemoteCreate::Off {
        warn!(
            grant_jti = %block.grant_jti,
            "remote create: refused — accept_remote_create is off on this device"
        );
        return Err(create_refusal_frame(CreateRefusal::Disabled, data, None));
    }

    // A jti coord minted as an ATTACH grant, presented as a create. The relay
    // verifies `sub_type` itself, so this fires only on a broker defect or a
    // forged block — which is exactly when the PTY owner's own check is the
    // one that matters.
    if attach_grants.contains(&block.grant_jti) {
        warn!(
            grant_jti = %block.grant_jti,
            "remote create: refused — this jti is an ATTACH grant on this device"
        );
        return Err(create_refusal_frame(
            CreateRefusal::GrantRequired,
            data,
            None,
        ));
    }

    // THE INDEPENDENT CHECK. Everything above this line reads fields the RELAY
    // stamped; this reads a table COORD filled — the `create_request` directive
    // and the `GET /sessions/create-requests` catch-up poll. A jti coord never
    // minted for this device is refused here no matter how the frame was
    // labelled, which is the whole of what Phase 3b bought.
    //
    // It is a LOOKUP, not yet a consume, and it sits BEFORE the directory
    // resolution below on purpose: the refusal frames from there echo this
    // device's `allowed_working_dir_keys` and `allowed_intent_repos`, and an
    // unauthorised caller must not be able to enumerate them by guessing.
    if let Err(refusal) =
        create_grants.lookup(&block.grant_jti, block.source_device_id.as_deref(), now)
    {
        warn!(
            grant_jti = %block.grant_jti,
            code = refusal.code(),
            known_grants = create_grants.len(),
            "remote create: refused — this device holds no live create grant with that jti \
             (the relay's word is not evidence of one)"
        );
        // `None` targets: no allowlist in the body. See above.
        return Err(create_refusal_frame(refusal, data, None));
    }

    let targets = targets();
    let working_dir = resolve_create_working_dir(
        &targets,
        data.get("working_dir_key").and_then(|v| v.as_str()),
        data.get("working_dir").and_then(|v| v.as_str()),
    )
    .map_err(|refusal| {
        warn!(
            grant_jti = %block.grant_jti,
            code = refusal.code(),
            requested_key = data.get("working_dir_key").and_then(|v| v.as_str()).unwrap_or(""),
            requested_dir = data.get("working_dir").and_then(|v| v.as_str()).unwrap_or(""),
            "remote create: refused the requested working directory"
        );
        create_refusal_frame(refusal, data, Some(&targets))
    })?;

    let intent_repo =
        resolve_create_intent_repo(&targets, data.get("intent_repo").and_then(|v| v.as_str()))
            .map_err(|refusal| {
                warn!(
                    grant_jti = %block.grant_jti,
                    code = refusal.code(),
                    requested_repo = data.get("intent_repo").and_then(|v| v.as_str()).unwrap_or(""),
                    "remote create: refused the requested intent_repo"
                );
                create_refusal_frame(refusal, data, Some(&targets))
            })?;

    // SPEND the grant, and only now: a grant is consumed exactly when a spawn
    // is authorised, so a caller who named a directory this device does not
    // offer has not burned a capability they never got to use. The re-check is
    // not redundant — `lookup` above ran before the two resolutions, and this
    // is what makes the admission single-use.
    if let Err(refusal) =
        create_grants.consume(&block.grant_jti, block.source_device_id.as_deref(), now)
    {
        warn!(
            grant_jti = %block.grant_jti,
            code = refusal.code(),
            "remote create: refused at the spend — the grant went away between the check and \
             the spend (a concurrent use, or it expired)"
        );
        return Err(create_refusal_frame(refusal, data, None));
    }

    info!(
        grant_jti = %block.grant_jti,
        working_dir = %working_dir,
        intent_repo = ?intent_repo,
        "remote create: admitted — spawning in a directory THIS device resolved"
    );
    Ok(Some(AdmittedCreate {
        block,
        working_dir,
        intent_repo,
    }))
}

/// Is this create refusal worth ONE coord re-read before it is returned?
///
/// Exactly one is: [`CreateRefusal::GrantUnknown`]. Coord publishes the
/// `create_request` directive before it answers the source's mint, but the
/// directive rides NATS while the frame rides the source's HTTP round-trip and
/// the relay socket — two paths with no ordering between them, so a LEGITIMATE
/// create can arrive microseconds before the directive that authorises it. A
/// single on-demand `GET /sessions/create-requests` removes that race; the 60 s
/// catch-up poll alone would turn it into a minute-long flake.
///
/// **Every other refusal is final**, and that is the point of spelling this as
/// a predicate rather than as an `if` in the handler:
///
/// * `Disabled` — the device's own dial; coord has nothing to add.
/// * `GrantRequired` — the block is not a create block at all.
/// * `GrantExpired` — coord's list cannot un-expire it.
/// * the two allowlist refusals — decided entirely from local config.
///
/// Re-reading on any of those would be a coord round-trip per refused frame,
/// which is a denial-of-service lever pointed at coord.
pub fn refusal_warrants_a_coord_reread(code: &str) -> bool {
    code == CreateRefusal::GrantUnknown.code()
}

/// Minimum gap between two on-demand coord re-reads, whatever jti asked.
///
/// The re-read is `await`ed on the backend relay's read loop, which is serial:
/// every one of them stalls terminal input, output and every other relay frame
/// for as long as the coord GET takes (up to its 10 s timeout). Unthrottled,
/// that is a liveness lever a compromised relay can pull at will by forwarding
/// a stream of creates carrying invented jtis.
///
/// Three seconds is chosen against the thing being raced — the gap between
/// coord publishing the `create_request` directive and the relay's frame
/// arriving, which is milliseconds — not against the 900 s grant life. A
/// legitimate create that loses the race is served by the first re-read; a
/// flood gets at most one coord round-trip per window and is otherwise refused
/// from memory at no cost.
pub const CREATE_REREAD_COOLDOWN_SECS: u64 = 3;

/// Per-jti + global throttle for the on-demand re-read. `(last_reread_at,
/// jtis already re-read and still unknown)`.
static CREATE_REREAD_STATE: OnceLock<Mutex<(u64, HashMap<String, u64>)>> = OnceLock::new();

fn create_reread_state() -> &'static Mutex<(u64, HashMap<String, u64>)> {
    CREATE_REREAD_STATE.get_or_init(|| Mutex::new((0, HashMap::new())))
}

/// Should this unknown-jti refusal buy a coord re-read *right now*?
///
/// Two independent brakes, and a caller must clear BOTH:
///
/// * **once per jti** — a jti coord has already been asked about, and did not
///   know, is never asked about again while it could still be live. Retrying it
///   cannot produce a different answer for the same reason the first read
///   didn't.
/// * **once per [`CREATE_REREAD_COOLDOWN_SECS`]** — a flood of DISTINCT jtis
///   would otherwise slip past the per-jti brake, which is exactly the shape an
///   attacker would choose.
///
/// Calling this RECORDS the attempt, so it is not a pure predicate and must be
/// called once per decision. `grant_expires_hint` bounds how long the per-jti
/// memory is kept; a create grant lives 15 minutes, so an hour is a generous
/// upper bound for a jti nobody can name an expiry for.
pub fn claim_create_reread(grant_jti: &str, now: u64) -> bool {
    const UNKNOWN_JTI_MEMORY_SECS: u64 = 3600;
    let Ok(mut state) = create_reread_state().lock() else {
        // A poisoned throttle must not become an unthrottled door.
        return false;
    };
    let (last, asked) = &mut *state;
    asked.retain(|_, at| now.saturating_sub(*at) < UNKNOWN_JTI_MEMORY_SECS);
    if asked.contains_key(grant_jti) {
        return false;
    }
    if now.saturating_sub(*last) < CREATE_REREAD_COOLDOWN_SECS {
        return false;
    }
    *last = now;
    asked.insert(grant_jti.to_string(), now);
    true
}

/// Forget that `grant_jti` was re-read — called when the re-read FOUND it, so
/// the memory holds only jtis coord genuinely did not know. Without this a
/// legitimate grant that lost the race would occupy a slot for an hour.
pub fn clear_create_reread(grant_jti: &str) {
    if let Ok(mut state) = create_reread_state().lock() {
        state.1.remove(grant_jti);
    }
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
/// `register_pane`. That window is one oneshot hop plus `RemotePaneIo::new` —
/// the relay's inbound thread waking the command thread, not the
/// `create_with_io` spawn, which happens AFTER the pane is registered — so it
/// is sub-millisecond and the cap only matters against a target that is
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
    /// An exit or fatal error that arrived while the slot was open — i.e.
    /// between the target's `remote_terminal_attached` reply and
    /// [`RemoteTerminalClient::register_pane`]. Applied to the pane as soon as
    /// it registers.
    ///
    /// The window is the one the output buffer already exists for: from
    /// `open_pending_output`, on the relay's inbound thread as the `attached`
    /// reply is parsed, to `register_pane` on the command thread — one oneshot
    /// hop plus `RemotePaneIo::new`. (It is NOT the `create_with_io` spawn:
    /// `commands::remote_attach` registers the pane BEFORE that call, so an
    /// exit during the spawn finds a live pane and settles normally.)
    ///
    /// Without this, a remote process that exits inside that window is dropped
    /// on the floor:
    /// `mark_exit` never runs, so `PaneIo::wait` never settles and the output
    /// channel is never closed. The tab then stays `isAlive: true` against a
    /// dead remote forever — `detachedRemoteTabs` never lists it, so the UI
    /// never even offers Reattach.
    settled: Option<PendingSettlement>,
}

/// A terminal settlement that arrived before its pane existed.
#[derive(Debug, Clone)]
enum PendingSettlement {
    Exit { code: i32 },
    Error { code: String, message: String },
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

    /// Record a settlement (exit / fatal error) against an OPEN slot. `false`
    /// when there is no slot, which is the caller's signal that the frame
    /// names no pane and no pending registration, and so is genuinely stray.
    ///
    /// First settlement wins: a target that sends a fatal error and then an
    /// exit for the same grant should surface the error, which is the more
    /// specific of the two.
    fn buffer_pending_settlement(&self, grant_jti: &str, settled: PendingSettlement) -> bool {
        let Ok(mut slots) = self.pending_output.lock() else {
            return false;
        };
        let Some(slot) = slots.get_mut(grant_jti) else {
            return false;
        };
        if slot.settled.is_none() {
            slot.settled = Some(settled);
        }
        true
    }

    /// Close and return the slot's chunks (empty when none).
    fn take_pending_output(&self, grant_jti: &str) -> (Vec<Vec<u8>>, Option<PendingSettlement>) {
        self.pending_output
            .lock()
            .ok()
            .and_then(|mut slots| slots.remove(grant_jti))
            .map(|slot| (slot.chunks.into_iter().collect(), slot.settled))
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
        let (buffered, settled) = self.take_pending_output(&jti);
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
        // A settlement that arrived inside the registration window, applied
        // AFTER its output so the operator sees the last bytes before the
        // pane closes. Without this the pane never settles and the tab stays
        // alive against a dead remote — see `PendingOutput::settled`.
        if let Some(settled) = settled {
            match settled {
                PendingSettlement::Exit { code } => {
                    info!(
                        grant_jti = %jti,
                        code,
                        "remote attach: applying the exit that arrived before registration"
                    );
                    pane.mark_exit(code);
                }
                PendingSettlement::Error { code, message } => {
                    warn!(
                        grant_jti = %jti,
                        code = %code,
                        "remote attach: applying the fatal error that arrived before registration"
                    );
                    pane.mark_error(&code, &message);
                }
            }
            self.drop_pane(&jti);
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
                    self.drop_pane(jti);
                } else if self.buffer_pending_settlement(jti, PendingSettlement::Exit { code }) {
                    // The pane is mid-registration: hold the exit and let
                    // `register_pane` apply it. Do NOT drop the slot here —
                    // that would discard the settlement we just recorded.
                    debug!(
                        grant_jti = jti,
                        code, "remote attach: exit before the pane registered — buffered"
                    );
                } else {
                    self.drop_pane(jti);
                }
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
                self.drop_pane(jti);
            } else if self.buffer_pending_settlement(
                jti,
                PendingSettlement::Error {
                    code: code.to_string(),
                    message: message.to_string(),
                },
            ) {
                debug!(
                    grant_jti = jti,
                    code, "remote attach: fatal error before the pane registered — buffered"
                );
            } else {
                self.drop_pane(jti);
            }
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
        assert_eq!(
            slice_ring(data, 100, Some(5), Some(103)),
            (&b"012"[..], 100)
        );
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
        let (chunks, settled) = client.take_pending_output("jti-1");
        assert!(settled.is_none());
        assert_eq!(chunks.len(), 1, "over cap: the two oldest were dropped");
        assert_eq!(chunks[0].len(), big.len());
        // No slot: not buffered.
        assert!(!client.buffer_pending_output("jti-1", b"late".to_vec()));
        client.open_pending_output("jti-2");
        client.discard_pending_output("jti-2");
        assert!(!client.buffer_pending_output("jti-2", b"late".to_vec()));
    }

    /// A settlement that lands inside the registration window must survive to
    /// the pane, or the tab never settles: `mark_exit` never runs, `PaneIo::wait`
    /// never returns, the output channel is never closed, and the UI holds a
    /// live tab against a dead remote that it will not even offer to reattach.
    #[test]
    fn an_exit_before_registration_is_buffered_and_applied() {
        let client = RemoteAttachClient::new();
        client.open_pending_output("jti-1");
        assert!(client.buffer_pending_output("jti-1", b"bye\n".to_vec()));
        assert!(client.buffer_pending_settlement("jti-1", PendingSettlement::Exit { code: 3 }));

        let (chunks, settled) = client.take_pending_output("jti-1");
        assert_eq!(
            chunks,
            vec![b"bye\n".to_vec()],
            "output still delivered first"
        );
        match settled {
            Some(PendingSettlement::Exit { code }) => assert_eq!(code, 3),
            other => panic!("expected a buffered exit, got {other:?}"),
        }
    }

    /// A fatal error in the same window settles the pane too, and it wins over
    /// a later exit for the same grant: it is the more specific of the two.
    #[test]
    fn a_fatal_error_before_registration_wins_over_a_later_exit() {
        let client = RemoteAttachClient::new();
        client.open_pending_output("jti-1");
        assert!(client.buffer_pending_settlement(
            "jti-1",
            PendingSettlement::Error {
                code: "attach_grant_expired".to_string(),
                message: "grant expired".to_string(),
            },
        ));
        assert!(client.buffer_pending_settlement("jti-1", PendingSettlement::Exit { code: 0 }));
        match client.take_pending_output("jti-1").1 {
            Some(PendingSettlement::Error { code, .. }) => assert_eq!(code, "attach_grant_expired"),
            other => panic!("first settlement should win, got {other:?}"),
        }
    }

    /// With no open slot there is nothing mid-registration, so the frame is
    /// genuinely stray and the caller must be told rather than silently
    /// buffering into nowhere.
    #[test]
    fn a_settlement_with_no_open_slot_is_refused() {
        let client = RemoteAttachClient::new();
        assert!(!client.buffer_pending_settlement("nobody", PendingSettlement::Exit { code: 0 }));
    }

    /// L1, end to end through `register_pane`: a settlement recorded while the
    /// pane was mid-registration must actually reach the pane, and the pane
    /// must then be finished. The three tests above pin the buffer; this pins
    /// the delivery, which is the half the tab's liveness depends on.
    #[test]
    fn register_pane_applies_a_settlement_that_arrived_before_it() {
        use crate::terminal::remote_pane_io::{AttachedRing, RemoteFrameSink};

        #[derive(Default)]
        struct NullSink;
        impl RemoteFrameSink for NullSink {
            fn send_frame(&self, _f: serde_json::Value) -> Result<(), String> {
                Ok(())
            }
        }

        let client = RemoteAttachClient::new();
        let sink: Arc<dyn RemoteFrameSink> = Arc::new(NullSink);
        let pane = Arc::new(RemotePaneIo::new(
            "jti-1",
            "term-9",
            "grant.jwt",
            sink,
            80,
            24,
            AttachedRing::default(),
        ));

        // The relay thread opened the slot, then the exit landed — both before
        // the command thread got as far as registering the pane.
        client.open_pending_output("jti-1");
        assert!(client.buffer_pending_output("jti-1", b"last words\n".to_vec()));
        assert!(client.buffer_pending_settlement("jti-1", PendingSettlement::Exit { code: 7 }));
        assert!(!pane.is_finished(), "not settled before registration");

        client.register_pane(pane.clone());

        assert!(
            pane.is_finished(),
            "the buffered exit never reached the pane — `wait` would block \
             forever and the tab would stay alive against a dead remote"
        );
        // The output buffered before the settlement still got there first.
        let out = read_pane_to_end(&pane);
        assert!(
            out.windows(10).any(|w| w == b"last words"),
            "buffered output must be delivered before the settlement closes the channel"
        );
    }

    /// Drain a settled pane's reader to EOF.
    fn read_pane_to_end(pane: &Arc<RemotePaneIo>) -> Vec<u8> {
        use std::io::Read;
        let mut r = pane.reader().expect("reader");
        let mut out = Vec::new();
        let _ = r.read_to_end(&mut out);
        out
    }

    /// R1: a reattach must ship from where the source actually stopped, not a
    /// blind tail. `slice_ring` from `have` is what makes `splice_replay`'s
    /// loss test truthful — with the bounded tail a reconnect after more than
    /// the tail had been produced looked exactly like a rolled ring.
    #[test]
    fn reattach_ships_from_have_offset_so_no_false_loss_is_reported() {
        // Ring holds [100, 110); the source already has through 105.
        let data = b"0123456789";
        let (bytes, start) = slice_ring(data, 100, Some(105), None);
        assert_eq!(start, 105, "start must be what the source has, not a tail");
        assert_eq!(bytes, b"56789");
        // `splice_replay`'s loss test is `have < start`. Equal means no marker.
        assert_eq!(
            start, 105,
            "have == start, so splice_replay reports no loss"
        );
        // The bounded tail an unfixed target would have shipped for the SAME
        // reconnect: it starts at 106, above the source's 105, so splice_replay
        // would report a byte lost that the ring plainly still holds.
        let (_, tail_start) = attach_tail(data, 100, 4);
        assert!(
            tail_start > 105,
            "the tail arm is exactly the false-loss shape this fix removes"
        );

        // A genuinely rolled ring still reports the TRUE loss, not a padded one.
        let (bytes, start) = slice_ring(data, 100, Some(80), None);
        assert_eq!(start, 100, "clamped up to the ring, so loss = 100 - 80");
        assert_eq!(bytes, &data[..]);

        // A fresh attach (no have_offset) keeps the bounded tail.
        let (tail, tstart) = attach_tail(data, 100, 4);
        assert_eq!(tail, b"6789");
        assert_eq!(tstart, 106);
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

#[cfg(test)]
mod create_gate_tests {
    //! D2 — **the TARGET chooses the working directory.**
    //!
    //! These are the pin on the hole review round 1 found and closed:
    //! `terminal_create` spawns a PTY, and it used to be reachable under an
    //! attach grant with a caller-chosen `working_dir`. Every test here is
    //! written so that it FAILS against an implementation that trusts the
    //! caller — if `admit_terminal_create` ever returns the frame's own
    //! `working_dir` or `intent_repo`, these go red rather than silently
    //! admitting the frame.

    use super::*;
    use serde_json::json;

    const TARGET_ROOT: &str = "/home/agent/qontinui-root";
    const OTHER_ROOT: &str = "/home/agent/scratch";

    fn targets() -> CreateTargets {
        CreateTargets {
            roots: vec![
                CreateRoot {
                    key: "workspace_root".to_string(),
                    path: TARGET_ROOT.to_string(),
                },
                CreateRoot {
                    key: "scratch".to_string(),
                    path: OTHER_ROOT.to_string(),
                },
            ],
            repos: vec!["qontinui-runner".to_string()],
        }
    }

    /// A frame under a CREATE grant. `extra` carries whatever the caller is
    /// trying to choose.
    fn create_frame(extra: Value) -> Value {
        let mut frame = json!({
            "type": "terminal_create",
            "request_id": "req-1",
            "remote": {
                "grant_jti": "jti-create-1",
                "source_device_id": "dev-source",
                "kind": "create",
            },
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                frame[k] = v.clone();
            }
        }
        frame
    }

    fn empty_grants() -> RemoteAttachGrants {
        RemoteAttachGrants::new()
    }

    /// `NOW` is any fixed instant; the fixture grant below outlives it.
    const NOW: u64 = 1_700_000_000;

    /// A create-grant table holding exactly the grant [`create_frame`]
    /// presents — i.e. coord told this device about it. Every test that
    /// expects an ADMISSION uses this; the ones that must be refused for
    /// some OTHER reason use it too, so the refusal they assert is the one
    /// they name and not an incidental unknown-grant.
    fn known_create_grants() -> RemoteCreateGrants {
        let table = RemoteCreateGrants::new();
        assert!(table.insert(
            CreateGrant {
                grant_jti: "jti-create-1".to_string(),
                source_device_id: "dev-source".to_string(),
                expires_at: NOW + 900,
            },
            NOW,
        ));
        table
    }

    fn admit(data: &Value) -> Result<Option<AdmittedCreate>, Value> {
        admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::SameUser,
            targets,
            data,
            NOW,
        )
    }

    // ------------------------------------------------------------------
    // THE discriminating test.
    // ------------------------------------------------------------------

    /// A create naming a directory outside the target-resolved set is
    /// REFUSED — not silently redirected to the default, and above all not
    /// honoured.
    ///
    /// This is the test that fails against a caller-chosen-cwd
    /// implementation: such an implementation returns
    /// `Ok(Some(AdmittedCreate { working_dir: "/etc", .. }))`, and both
    /// assertions below reject that — the first because it is not an `Err`,
    /// and (were the refusal ever weakened into a redirect) the second
    /// because `/etc` is not in `CreateTargets`.
    #[test]
    fn a_working_dir_outside_the_targets_set_is_refused() {
        for requested in [
            "/etc",
            "/",
            "/home/agent",                      // a PARENT of a member
            "/home/agent/qontinui-root/secret", // a CHILD of a member
            "/home/agent/qontinui-root/../../../etc",
            "../../etc",
            "C:/Windows/System32",
        ] {
            let frame = create_frame(json!({ "working_dir": requested }));
            let err = admit(&frame).expect_err(&format!(
                "a remote create naming {requested:?} must be REFUSED — the target chooses the \
                 working directory, a caller only ever indexes into the set it offers"
            ));
            assert_eq!(
                err["code"], "remote_create_working_dir_not_allowed",
                "refusal must name WHY, for {requested:?}"
            );
            // The refusal carries the choice the caller actually has.
            assert_eq!(
                err["allowed_working_dir_keys"],
                json!(["workspace_root", "scratch"])
            );
        }
    }

    /// The same property stated positively: whatever is admitted, the
    /// directory spawned in is a string that came out of `CreateTargets` —
    /// never one off the wire.
    #[test]
    fn the_admitted_working_dir_is_always_a_member_of_the_targets_set() {
        let admitted_defaults = admit(&create_frame(json!({})))
            .expect("no preference is admitted")
            .expect("a remote block means a remote create");
        assert_eq!(admitted_defaults.working_dir, TARGET_ROOT);

        let admitted_key = admit(&create_frame(json!({ "working_dir_key": "scratch" })))
            .expect("a member key is admitted")
            .expect("a remote block means a remote create");
        assert_eq!(admitted_key.working_dir, OTHER_ROOT);

        // An exact-path preference is admitted, and what comes back is the
        // TARGET's spelling of that root.
        let admitted_path = admit(&create_frame(json!({ "working_dir": OTHER_ROOT })))
            .expect("an exact member path is admitted")
            .expect("a remote block means a remote create");
        assert_eq!(admitted_path.working_dir, OTHER_ROOT);

        for admitted in [admitted_defaults, admitted_key, admitted_path] {
            assert!(
                targets()
                    .roots
                    .iter()
                    .any(|root| root.path == admitted.working_dir),
                "the spawn directory must be one the TARGET offered"
            );
        }
    }

    /// A key the target does not offer is refused rather than falling back to
    /// the default — a silent fallback would teach a caller its key worked.
    #[test]
    fn an_unknown_working_dir_key_is_refused_not_defaulted() {
        let err = admit(&create_frame(json!({ "working_dir_key": "wherever" })))
            .expect_err("an unknown key must be refused");
        assert_eq!(err["code"], "remote_create_working_dir_not_allowed");
    }

    /// With no root at all this device refuses rather than spawning in the
    /// runner process's cwd.
    #[test]
    fn no_resolved_root_refuses_rather_than_guessing() {
        let err = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::SameUser,
            CreateTargets::default,
            &create_frame(json!({})),
            NOW,
        )
        .expect_err("nowhere to spawn must refuse");
        assert_eq!(err["code"], "remote_create_no_target_directory");
    }

    // ------------------------------------------------------------------
    // `intent_repo` — the same shape, and the reason it matters: it
    // ALLOCATES A WORKTREE and TAKES A COORD CLAIM.
    // ------------------------------------------------------------------

    /// The discriminating test for the second caller-chosen value. A trusting
    /// implementation returns `intent_repo: Some("qontinui-web")` and fails
    /// here.
    #[test]
    fn an_intent_repo_outside_the_targets_set_is_refused() {
        for requested in ["qontinui-web", "qontinui/qontinui-runner", "../escape"] {
            let err =
                admit(&create_frame(json!({ "intent_repo": requested }))).expect_err(&format!(
                    "a remote create declaring intent_repo {requested:?} must be REFUSED — it \
                     allocates a worktree and takes a coord claim"
                ));
            assert_eq!(err["code"], "remote_create_intent_repo_not_allowed");
            assert_eq!(err["allowed_intent_repos"], json!(["qontinui-runner"]));
        }
    }

    #[test]
    fn an_allowlisted_intent_repo_is_admitted_in_the_targets_own_spelling() {
        let admitted = admit(&create_frame(json!({ "intent_repo": "qontinui-runner" })))
            .expect("an allowlisted repo is admitted")
            .expect("a remote block means a remote create");
        assert_eq!(admitted.intent_repo.as_deref(), Some("qontinui-runner"));
    }

    /// The default configuration lists NO repo, so a remote create allocates
    /// no worktree and takes no claim until an operator says otherwise.
    #[test]
    fn the_default_allowlist_admits_no_intent_repo_at_all() {
        let no_repos = || CreateTargets {
            roots: targets().roots,
            repos: Vec::new(),
        };
        let err = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::SameUser,
            no_repos,
            &create_frame(json!({ "intent_repo": "qontinui-runner" })),
            NOW,
        )
        .expect_err("an empty allowlist admits nothing");
        assert_eq!(err["code"], "remote_create_intent_repo_not_allowed");

        // …and a create declaring none still works, with no intent.
        let admitted = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::SameUser,
            no_repos,
            &create_frame(json!({})),
            NOW,
        )
        .expect("no declared intent is fine")
        .expect("a remote block means a remote create");
        assert_eq!(admitted.intent_repo, None);
    }

    // ------------------------------------------------------------------
    // Who may create at all.
    // ------------------------------------------------------------------

    /// The preference is OFF by default, and the refusal says so and says how
    /// to change it — an OFF default that does not explain itself reads as a
    /// bug.
    #[test]
    fn the_default_preference_refuses_and_names_itself() {
        let err = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            AcceptRemoteCreate::default,
            targets,
            &create_frame(json!({})),
            NOW,
        )
        .expect_err("accept_remote_create defaults to off");
        assert_eq!(err["code"], "remote_create_disabled");
        let message = err["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("accept_remote_create"),
            "the refusal must NAME the preference: {message}"
        );
        assert!(
            message.contains("same_user") && message.contains("tenant"),
            "the refusal must say how to enable it: {message}"
        );
    }

    /// An ATTACH grant does not buy a spawn — at the PTY owner, not only at
    /// the relay.
    #[test]
    fn an_attach_grant_cannot_create() {
        let mut frame = create_frame(json!({}));
        frame["remote"]["kind"] = json!("attach");
        let err = admit(&frame).expect_err("an attach grant must not create");
        assert_eq!(err["code"], "remote_create_grant_required");

        // …and so does a block that names no kind at all (an older relay, or
        // one that forgot to stamp it): absent reads as `attach`.
        let mut unstamped = create_frame(json!({}));
        unstamped["remote"]
            .as_object_mut()
            .unwrap()
            .remove("kind")
            .expect("the fixture stamps a kind");
        let err = admit(&unstamped).expect_err("an unstamped block must not create");
        assert_eq!(err["code"], "remote_create_grant_required");
    }

    /// A jti this device holds as an ATTACH grant is refused even when the
    /// block claims `create` — the broker is not the last word here.
    #[test]
    fn a_jti_known_as_an_attach_grant_cannot_create() {
        let grants = RemoteAttachGrants::new();
        assert!(grants.insert(
            AttachGrant {
                grant_jti: "jti-create-1".to_string(),
                source_device_id: "dev-source".to_string(),
                session_id: Uuid::from_u128(3),
                terminal_id: None,
                expires_at: u64::MAX,
            },
            0,
        ));
        let err = admit_terminal_create(
            &grants,
            &known_create_grants(),
            || AcceptRemoteCreate::SameUser,
            targets,
            &create_frame(json!({})),
            NOW,
        )
        .expect_err("an attach jti stamped `create` must still be refused");
        assert_eq!(err["code"], "remote_create_grant_required");
    }

    // ------------------------------------------------------------------
    // THE Phase 3b discriminating pair — the target verifies for itself.
    // ------------------------------------------------------------------

    /// **The test this phase exists for.** The frame here is a
    /// PERFECTLY WELL-FORMED relay-forwarded create: `kind: "create"`, a
    /// source device, the preference ON, a directory this device offers, an
    /// allowlisted repo, and no jti in the attach table. Every check the
    /// pre-Phase-3b gate performed PASSES. It is refused anyway, because
    /// coord never told this device about that jti.
    ///
    /// **How this discriminates.** A trust-the-relay implementation — the one
    /// that shipped — returns `Ok(Some(AdmittedCreate { .. }))` here and
    /// spawns a PTY, so `expect_err` fails. The positive control below runs
    /// the SAME frame against a table that holds the grant and asserts an
    /// admission, so the refusal cannot be bought by breaking the gate
    /// outright: an implementation that refuses everything fails that one.
    /// The pair together is the property — *the target's own grant table, and
    /// nothing the relay stamped, decides.*
    #[test]
    fn a_create_whose_jti_this_device_never_learned_is_refused() {
        let unknown = RemoteCreateGrants::new();
        assert!(unknown.is_empty());
        let frame = create_frame(json!({
            "working_dir_key": "workspace_root",
            "intent_repo": "qontinui-runner",
        }));
        let err = admit_terminal_create(
            &empty_grants(),
            &unknown,
            || AcceptRemoteCreate::Tenant,
            targets,
            &frame,
            NOW,
        )
        .expect_err(
            "the relay forwarded a well-formed create, but coord never told this device about \
             that jti — the PTY owner must refuse it",
        );
        assert_eq!(err["code"], "remote_create_grant_unknown");
        let message = err["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("no live create grant"),
            "the refusal must say what is missing: {message}"
        );
    }

    /// The positive control for the test above: the identical frame, against a
    /// table that HOLDS the grant, is admitted. Without this, "refused" would
    /// be satisfiable by a gate that refuses everything.
    #[test]
    fn the_same_frame_is_admitted_once_coord_has_told_this_device() {
        let frame = create_frame(json!({
            "working_dir_key": "workspace_root",
            "intent_repo": "qontinui-runner",
        }));
        let admitted = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::Tenant,
            targets,
            &frame,
            NOW,
        )
        .expect("a jti coord told this device about is admitted")
        .expect("a remote block means a remote create");
        assert_eq!(admitted.working_dir, TARGET_ROOT);
        assert_eq!(admitted.intent_repo.as_deref(), Some("qontinui-runner"));
    }

    /// A grant minted for a DIFFERENT source device is refused, and with the
    /// SAME code an unknown jti gets — so a broker replaying jti J from the
    /// wrong device learns nothing about whether J exists here.
    ///
    /// **And the binding is REQUIRED, not merely checked when offered.** The
    /// relay is the party this check defends against and the party that stamps
    /// the field, so a check-if-present rule is opt-out by the adversary:
    /// omitting the key would evaporate the binding. Both the wrong value and
    /// the absent one are asserted, and the absent one is the case an attacker
    /// would actually choose.
    #[test]
    fn a_grant_minted_for_another_source_device_is_refused_as_unknown() {
        for mutate in [
            (|f: &mut Value| f["remote"]["source_device_id"] = json!("dev-somebody-else"))
                as fn(&mut Value),
            |f: &mut Value| {
                f["remote"]
                    .as_object_mut()
                    .unwrap()
                    .remove("source_device_id")
                    .expect("the fixture stamps a source device");
            },
            |f: &mut Value| f["remote"]["source_device_id"] = json!("   "),
            |f: &mut Value| f["remote"]["source_device_id"] = json!(null),
        ] {
            let table = known_create_grants();
            let mut frame = create_frame(json!({}));
            mutate(&mut frame);
            let err = admit_terminal_create(
                &empty_grants(),
                &table,
                || AcceptRemoteCreate::Tenant,
                targets,
                &frame,
                NOW,
            )
            .expect_err("the grant was not minted for the source this frame names (or names none)");
            assert_eq!(err["code"], "remote_create_grant_unknown", "{frame}");
            // …and the row survives, so the rightful source can still use it: a
            // wrong-source probe must not be a way to burn someone's grant.
            assert!(table.contains("jti-create-1"), "{frame}");
        }

        // Case-insensitive on the UUID's hex, which is the only tolerance.
        let table = known_create_grants();
        let mut frame = create_frame(json!({}));
        frame["remote"]["source_device_id"] = json!("DEV-SOURCE");
        assert!(admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &frame,
            NOW,
        )
        .is_ok());
    }

    /// **A spent jti is never resurrected by the feed.** Coord keeps serving a
    /// grant in its pending list until `consumed_at` is set, and the catch-up
    /// poll runs every 60 s — so without a tombstone the "single use" would be
    /// an allowance of one per poll interval, which is not single use at all.
    ///
    /// This fails against an implementation that only removes the row on
    /// consume: `insert` would take the re-delivered grant and the second
    /// admission would succeed.
    #[test]
    fn a_spent_jti_cannot_be_re_recorded_by_the_catch_up_feed() {
        let table = known_create_grants();
        assert!(admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &create_frame(json!({})),
            NOW,
        )
        .expect("the first use is admitted")
        .is_some());
        assert!(table.is_empty());
        assert_eq!(table.spent_len(NOW), 1);

        // Coord's poll re-delivers the row — it has not marked it consumed yet.
        assert!(
            !table.insert(
                CreateGrant {
                    grant_jti: "jti-create-1".to_string(),
                    source_device_id: "dev-source".to_string(),
                    expires_at: NOW + 900,
                },
                NOW,
            ),
            "a spent jti must not be re-recorded"
        );
        assert!(table.is_empty(), "and it must not be in the table");

        let err = admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &create_frame(json!({})),
            NOW,
        )
        .expect_err("a re-delivered spent grant must not spawn a second PTY");
        assert_eq!(err["code"], "remote_create_grant_unknown");

        // The tombstone expires with the grant — past that coord stops serving
        // it too, so there is nothing left to guard.
        assert_eq!(table.spent_len(NOW + 901), 0);
    }

    /// **An unreadable clock fails CLOSED.** `now_epoch_secs()` answers `0`
    /// when the system time cannot be read, and every `expires_at <= 0` is
    /// false — i.e. every grant would read as unexpired. Refuse instead.
    #[test]
    fn a_zero_clock_refuses_rather_than_reading_every_grant_as_live() {
        let table = known_create_grants();
        let err = admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &create_frame(json!({})),
            0,
        )
        .expect_err("a device that cannot read its clock cannot evaluate an expiry");
        assert_eq!(err["code"], "remote_create_grant_unknown");
        assert!(table.contains("jti-create-1"), "and nothing was burned");
    }

    /// An expired grant is refused with its OWN code — the source learns to
    /// mint a fresh one rather than chasing a phantom unknown-jti.
    #[test]
    fn an_expired_create_grant_is_refused_as_expired() {
        let err = admit_terminal_create(
            &empty_grants(),
            &known_create_grants(),
            || AcceptRemoteCreate::Tenant,
            targets,
            &create_frame(json!({})),
            NOW + 901,
        )
        .expect_err("a grant past its exp must not spawn");
        assert_eq!(err["code"], "remote_create_grant_expired");
    }

    /// **A create grant buys ONE spawn.** The row is consumed on admission, so
    /// a replayed frame — the relay's Redis claim bypassed, or the same frame
    /// delivered twice — is refused the second time.
    #[test]
    fn a_create_grant_is_single_use_at_the_target() {
        // Concurrency half: the consume is ONE critical section, so two frames
        // carrying the same jti at once cannot both see `Ok`.
        {
            let table = std::sync::Arc::new(known_create_grants());
            let wins: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
            std::thread::scope(|s| {
                for _ in 0..8 {
                    let table = table.clone();
                    let wins = wins.clone();
                    s.spawn(move || {
                        if table
                            .consume("jti-create-1", Some("dev-source"), NOW)
                            .is_ok()
                        {
                            wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(
                wins.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "exactly one concurrent consumer may win a create grant"
            );
        }

        let table = known_create_grants();
        let frame = create_frame(json!({}));
        let first = admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &frame,
            NOW,
        )
        .expect("the first use is admitted");
        assert!(first.is_some());
        assert!(table.is_empty(), "admission must consume the grant");

        let err = admit_terminal_create(
            &empty_grants(),
            &table,
            || AcceptRemoteCreate::Tenant,
            targets,
            &frame,
            NOW,
        )
        .expect_err("a replay of an admitted create must not spawn a second PTY");
        assert_eq!(err["code"], "remote_create_grant_unknown");
    }

    /// **A grant is consumed exactly when a spawn is authorised.** No refusal
    /// burns one — not the switched-off device, not a directory this device
    /// does not offer, not a repo outside the allowlist. Otherwise a typo in
    /// `working_dir_key` would cost the source a capability it never spent.
    #[test]
    fn no_refusal_consumes_the_grant() {
        for (label, pref, frame) in [
            (
                "preference off",
                AcceptRemoteCreate::Off,
                create_frame(json!({})),
            ),
            (
                "unoffered directory",
                AcceptRemoteCreate::Tenant,
                create_frame(json!({ "working_dir_key": "wherever" })),
            ),
            (
                "unallowlisted repo",
                AcceptRemoteCreate::Tenant,
                create_frame(json!({ "intent_repo": "qontinui-web" })),
            ),
        ] {
            let table = known_create_grants();
            assert!(
                admit_terminal_create(&empty_grants(), &table, move || pref, targets, &frame, NOW,)
                    .is_err(),
                "{label} must refuse"
            );
            assert!(
                table.contains("jti-create-1"),
                "{label} must not burn the grant"
            );
        }
    }

    /// EXACTLY ONE refusal buys a coord re-read. Widening this would point a
    /// coord round-trip-per-refused-frame lever at coord; narrowing it to none
    /// would reinstate the directive/frame race as a 60 s flake.
    #[test]
    fn only_an_unknown_grant_warrants_a_coord_reread() {
        assert!(refusal_warrants_a_coord_reread(
            CreateRefusal::GrantUnknown.code()
        ));
        for final_refusal in [
            CreateRefusal::Disabled,
            CreateRefusal::GrantRequired,
            CreateRefusal::GrantExpired,
            CreateRefusal::NoTargetDirectory,
            CreateRefusal::WorkingDirNotAllowed,
            CreateRefusal::IntentRepoNotAllowed,
        ] {
            assert!(
                !refusal_warrants_a_coord_reread(final_refusal.code()),
                "{final_refusal:?} is final — coord has nothing to add"
            );
        }
        // An unrecognised code is final too: an unknown refusal must not be a
        // way to make this device call coord.
        assert!(!refusal_warrants_a_coord_reread("something_else"));
        assert!(!refusal_warrants_a_coord_reread(""));
    }

    /// **The re-read is throttled on two independent axes**, because it is
    /// `await`ed on the relay's SERIAL read loop: every one stalls terminal
    /// input, output and every other frame for up to the coord timeout. A
    /// compromised relay forwarding invented jtis must not be able to hold the
    /// runner's whole relay hostage.
    ///
    /// The global `OnceLock` state is process-wide, so this test uses jtis
    /// nothing else touches and asserts relative behaviour rather than an
    /// absolute first call.
    #[test]
    fn the_coord_reread_is_throttled_per_jti_and_per_window() {
        let base = 9_000_000u64;
        let tag = "throttle-test";

        // One per window: the first distinct jti in a window wins, the next
        // does not, and the window reopens after the cooldown.
        assert!(claim_create_reread(&format!("{tag}-a"), base));
        assert!(
            !claim_create_reread(&format!("{tag}-b"), base),
            "a DIFFERENT jti inside the cooldown must not slip past — a flood of distinct jtis \
             is the shape an attacker would choose"
        );
        assert!(claim_create_reread(
            &format!("{tag}-b"),
            base + CREATE_REREAD_COOLDOWN_SECS
        ));

        // Once per jti: `-a` is remembered as asked-and-unknown, so it buys no
        // second round-trip for as long as the grant it names could still be
        // live. A create grant lives 900 s; the memory is an hour, so this is
        // well inside it. (The memory is deliberately BOUNDED rather than
        // forever — an unbounded set is a leak — and past the bound the jti it
        // names is long expired, so re-asking about it is harmless and the
        // global cooldown still caps the rate.)
        assert!(
            !claim_create_reread(&format!("{tag}-a"), base + 900),
            "a jti coord already said it did not know must not be asked again while the grant \
             it names could still be live"
        );

        // …unless the re-read FOUND it, which clears the memory: a legitimate
        // grant that lost the directive race must not hold a slot for an hour.
        clear_create_reread(&format!("{tag}-a"));
        assert!(claim_create_reread(&format!("{tag}-a"), base + 900));

        // The cooldown is sized against the directive race (milliseconds), not
        // against the grant's 900 s life.
        assert!(CREATE_REREAD_COOLDOWN_SECS > 0 && CREATE_REREAD_COOLDOWN_SECS < 60);
    }

    /// An unauthorised caller must not learn this device's directory and repo
    /// allowlists. The grant lookup therefore runs BEFORE the two resolutions,
    /// whose refusal bodies echo them — so an unknown jti gets a bare refusal.
    #[test]
    fn an_unknown_grant_refusal_does_not_leak_the_allowlists() {
        let err = admit_terminal_create(
            &empty_grants(),
            &RemoteCreateGrants::new(),
            || AcceptRemoteCreate::Tenant,
            targets,
            &create_frame(json!({ "working_dir_key": "wherever" })),
            NOW,
        )
        .expect_err("an unknown jti must be refused");
        assert_eq!(err["code"], "remote_create_grant_unknown");
        assert!(
            err.get("allowed_working_dir_keys").is_none(),
            "a caller with no grant must not enumerate this device's directories: {err}"
        );
        assert!(err.get("allowed_intent_repos").is_none(), "{err}");
    }

    /// A frame with NO `remote` block is the operator-web / mobile path: the
    /// gate returns `None` and the handler keeps its caller-chosen
    /// `working_dir`. This is the arm that must NOT be tightened — doing so
    /// would break every local terminal.
    #[test]
    fn a_local_create_is_untouched() {
        let local = json!({
            "type": "terminal_create",
            "working_dir": "/anywhere/at/all",
            "intent_repo": "qontinui-web",
        });
        let admitted = admit(&local).expect("a local create is never refused here");
        assert!(admitted.is_none(), "a local create resolves nothing");
    }

    /// A malformed block is a block: refused, never read as absent.
    #[test]
    fn a_malformed_remote_block_is_refused() {
        for block in [json!({}), json!({ "grant_jti": "" }), json!(7)] {
            let mut frame = create_frame(json!({}));
            frame["remote"] = block.clone();
            let err = admit(&frame).expect_err("a malformed block must not create");
            assert_eq!(err["code"], "remote_create_grant_required", "{block}");
        }
    }

    // ------------------------------------------------------------------
    // The resolver itself.
    // ------------------------------------------------------------------

    #[test]
    fn resolve_working_dir_tolerates_spelling_but_not_substitution() {
        let targets = targets();
        // A trailing separator and a backslash spelling of a member still
        // resolve — to the TARGET's own string.
        for spelling in [
            "/home/agent/scratch/",
            "  /home/agent/scratch  ",
            "\\home\\agent\\scratch",
        ] {
            assert_eq!(
                resolve_create_working_dir(&targets, None, Some(spelling)),
                Ok(OTHER_ROOT.to_string()),
                "{spelling}"
            );
        }
        // Case is NOT normalized: a differing case is a refusal, because a
        // refusal is the safe answer and a redirect is not.
        assert_eq!(
            resolve_create_working_dir(&targets, None, Some("/home/agent/SCRATCH")),
            Err(CreateRefusal::WorkingDirNotAllowed)
        );
    }

    #[test]
    fn the_key_wins_over_the_path_when_both_are_named() {
        let targets = targets();
        assert_eq!(
            resolve_create_working_dir(&targets, Some("scratch"), Some(TARGET_ROOT)),
            Ok(OTHER_ROOT.to_string())
        );
        assert_eq!(
            resolve_create_working_dir(&targets, Some("nope"), Some(TARGET_ROOT)),
            Err(CreateRefusal::WorkingDirNotAllowed),
            "a bad key is refused rather than falling through to the path"
        );
    }

    #[test]
    fn parse_remote_block_reads_the_kind_and_fails_closed() {
        let kind_of = |v: Value| match parse_remote_block(&v) {
            Some(Ok(block)) => block.kind,
            other => panic!("expected a parsed block, got {other:?}"),
        };
        assert_eq!(
            kind_of(json!({ "remote": { "grant_jti": "j", "kind": "create" } })),
            RemoteGrantKind::Create
        );
        for spelling in [
            json!("attach"),
            json!("CREATE"),
            json!("nonsense"),
            json!(3),
        ] {
            assert_eq!(
                kind_of(json!({ "remote": { "grant_jti": "j", "kind": spelling } })),
                RemoteGrantKind::Attach,
                "{spelling} must read as the NARROWER kind"
            );
        }
        assert_eq!(
            kind_of(json!({ "remote": { "grant_jti": "j" } })),
            RemoteGrantKind::Attach
        );
    }
}
