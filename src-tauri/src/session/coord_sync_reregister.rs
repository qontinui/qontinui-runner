//! Re-creating a coord session row that coord answers `404 session_not_found`
//! for (plan
//! `2026-10-29-session-outbox-drops-the-finish-of-a-session-coord-never-registered`,
//! Phase 2, decisions D1 + D2).
//!
//! Every non-`started` lifecycle row (`state_change` / `closed` / `finished`)
//! and every `heartbeat` is a path-addressed `PATCH /sessions/:id`. Coord never
//! creates a row from a PATCH, so when the row is missing — its `started` row
//! was quarantined, held, trimmed, or refused, or coord lost it — the PATCH can
//! only ever answer 404. Before this module the drain ACK-dropped that 404 as a
//! permanent 4xx, so the session's `finished` mark (and its close) were gone
//! for good while the runner had reported the finish as `queued`.
//!
//! The recovery is the same shape as the gate-registration bootstrap in
//! `coord_sync` (`gate_registration_outcome` → `bootstrap_then_register`):
//! re-create the row from a RECORDED body, then retry the row once.
//!
//! ## The create-body sidecar (D2)
//!
//! `<outbox>.created.jsonl` holds the `POST /sessions` body of every session
//! the drain has SEEN a pending `started` row for, keyed by session id. It is
//! written on first sight — before hold or quarantine routing, so a `started`
//! row that is never sent still leaves its body here — with exactly the body
//! the `started` push would send (`coord_sync::rebuild_create_body`). It is not
//! a second outbox: it holds create bodies, not events, and nothing drains it.
//!
//! - **One writer**: [`Reregister`]. Lines are `{"op":"created",…}` (first
//!   sight; a later sighting of the same session appends nothing) and
//!   `{"op":"closed",…}` (coord ACKed the session's `closed` row).
//! - **Bound**: an entry is pruned [`PRUNE_AFTER_CLOSE`] (7 days) after its
//!   session's `closed` ACK, and the file holds at most [`MAX_SESSIONS`]
//!   entries (oldest closed first, then oldest recorded, are evicted past it).
//!   Pruning is lazy: on load, and whenever the append-only file has grown past
//!   twice its live entry count, at which point it is rewritten compacted
//!   (temp file + rename).
//! - A session registered before this file existed has no entry. That is
//!   [`ReregisterOutcome::NoBody`] by design — a body is never invented.
//!
//! ## The re-register (D1)
//!
//! [`reregister`] POSTs the recorded body to `/sessions` under the SAME id, so
//! the queued rows' path address stays valid. Coord answers a same-device,
//! same-tenant re-POST of an existing id with a 2xx (an idempotent replay), so
//! any 2xx is [`ReregisterOutcome::Created`]. A 409 means only
//! `session_id_collision` (another device or tenant holds the id) or
//! `harness_session_already_bound` — it is NOT the acquire-path conflict, and
//! the drain must never route it through `handle_conflict` (that opens the
//! ConflictModal and ACKs the row).
//!
//! At most one POST per session per [`REREGISTER_WINDOW`] (10 minutes),
//! whatever its outcome — a coord that keeps refusing is asked again on that
//! budget, not on every drain retry.
//!
//! ## Counters (`GET /health` `sessionOutbox`)
//!
//! - `orphanedSessions` `{id_collision|reregister_refused|no_body: n}` — one
//!   per re-register attempt that left the session without a coord row, so at
//!   most one per session per window.
//! - `reregistered` `{created: n}` — rows re-created.
//!
//! Both dense, `null` until the drain has been observed (UNKNOWN, not zero),
//! rendered beside Phase 1's `lifecycleDrops`.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use crate::auth::TenantScope;

/// At most one re-register `POST /sessions` per session in this window.
pub(crate) const REREGISTER_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A sidecar entry is pruned this long after its session's `closed` ACK.
pub(crate) const PRUNE_AFTER_CLOSE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Hard cap on sidecar entries. A create body is a few hundred bytes, so this
/// bounds the file at a few MiB even if no session ever closes.
pub(crate) const MAX_SESSIONS: usize = 10_000;

/// `<outbox>.created.jsonl` — the create-body sidecar beside the outbox.
pub(crate) fn created_path(outbox_path: &Path) -> PathBuf {
    let name = outbox_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session-outbox.jsonl".to_string());
    outbox_path.with_file_name(format!("{name}.created.jsonl"))
}

/// What one re-register `POST /sessions` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReregisterOutcome {
    /// Coord answered 2xx — created, or its idempotent same-device replay of a
    /// row that exists. The caller retries its row once.
    Created,
    /// Coord answered 409: the id is held by another device / tenant
    /// (`session_id_collision`) or the harness session is bound elsewhere
    /// (`harness_session_already_bound`). Carries coord's `error` code.
    Collision(String),
    /// Coord refused the create with another 4xx (400 `invalid_intent` /
    /// `unknown_device`, 403 `harness_session_id_requires_device_identity`).
    Refused { status: u16 },
    /// No create body is recorded for the session; nothing was sent.
    NoBody,
    /// Network, timeout, 5xx or 429 — coord did not decide.
    Transport(String),
}

/// [`Reregister::attempt`]'s answer: the budget refused the POST, or one ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// A re-register for this session already ran inside the window.
    BudgetSpent,
    Done(ReregisterOutcome),
}

/// A recorded create body and the tenant whose credential its `started` push
/// presented (`None` when that push resolved no owning tenant).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecordedBody {
    pub body: JsonValue,
    pub tenant_id: Option<Uuid>,
}

/// A snapshot of the re-register counters for the health block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReregisterCounts {
    pub id_collision: u64,
    pub reregister_refused: u64,
    pub no_body: u64,
    pub created: u64,
}

impl ReregisterCounts {
    /// `sessionOutbox.orphanedSessions`, dense.
    pub(crate) fn orphaned_json(&self) -> JsonValue {
        json!({
            "id_collision": self.id_collision,
            "reregister_refused": self.reregister_refused,
            "no_body": self.no_body,
        })
    }

    /// `sessionOutbox.reregistered`, dense.
    pub(crate) fn reregistered_json(&self) -> JsonValue {
        json!({ "created": self.created })
    }
}

/// POST the recorded create body to `{base}/sessions` under the same id.
///
/// The credential is the tenant the `started` push presented when one was
/// recorded, else `fallback_scope` (the scope the drain resolves for the row
/// being pushed). `recorded == None` is [`ReregisterOutcome::NoBody`] with no
/// request at all.
pub(crate) async fn reregister(
    http: &reqwest::Client,
    base: &str,
    session_id: Uuid,
    recorded: Option<RecordedBody>,
    fallback_scope: TenantScope,
) -> ReregisterOutcome {
    let Some(recorded) = recorded else {
        return ReregisterOutcome::NoBody;
    };
    let scope = recorded
        .tenant_id
        .map(TenantScope::Owned)
        .unwrap_or(fallback_scope);
    let url = format!("{}/sessions", base.trim_end_matches('/'));
    let resp =
        match crate::auth::attach_device_auth_for(http.post(&url).json(&recorded.body), scope)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                return ReregisterOutcome::Transport(format!(
                    "re-register POST /sessions for {session_id}: {}",
                    super::coord_sync::transport_error(&e)
                ))
            }
        };
    let status = resp.status();
    if status.is_success() {
        return ReregisterOutcome::Created;
    }
    let detail = resp.text().await.unwrap_or_default();
    if status == StatusCode::CONFLICT {
        let code = serde_json::from_str::<JsonValue>(&detail)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "conflict".to_string());
        return ReregisterOutcome::Collision(code);
    }
    if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
        let snippet: String = detail.chars().take(600).collect();
        tracing::warn!(
            session = %session_id,
            status = status.as_u16(),
            body = %snippet,
            "coord_sync: coord refused the re-register POST /sessions"
        );
        return ReregisterOutcome::Refused {
            status: status.as_u16(),
        };
    }
    // Leading status so the drain's failure classifier reads it (a 429 is
    // pacing and never counts toward quarantine; a 5xx is `server_error`).
    ReregisterOutcome::Transport(format!(
        "{status}: re-register POST /sessions for {session_id}: {detail}"
    ))
}

// ---------------------------------------------------------------------------
// The sidecar store
// ---------------------------------------------------------------------------

/// One line of `<outbox>.created.jsonl`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Line {
    Created {
        session_id: Uuid,
        body: JsonValue,
        #[serde(default)]
        tenant_id: Option<Uuid>,
        recorded_at: DateTime<Utc>,
    },
    Closed {
        session_id: Uuid,
        closed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
struct Entry {
    body: JsonValue,
    tenant_id: Option<Uuid>,
    recorded_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct Store {
    path: PathBuf,
    entries: HashMap<Uuid, Entry>,
    /// Lines in the file, so the append-only growth can trigger a compaction.
    lines: usize,
}

impl Store {
    fn load(path: PathBuf, now: DateTime<Utc>) -> Self {
        let mut store = Store {
            path,
            entries: HashMap::new(),
            lines: 0,
        };
        let text = match std::fs::read_to_string(&store.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
            Err(e) => {
                tracing::warn!(
                    path = %store.path.display(),
                    error = %e,
                    "coord_sync: create-body sidecar unreadable — re-registers start with no \
                     recorded bodies (a missing body is held, never invented)"
                );
                return store;
            }
        };
        let mut unparseable = 0usize;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            store.lines += 1;
            match serde_json::from_str::<Line>(line) {
                Ok(l) => store.apply(l),
                // A torn last line from a crash mid-append, most likely.
                Err(_) => unparseable += 1,
            }
        }
        if unparseable > 0 {
            tracing::warn!(
                path = %store.path.display(),
                unparseable,
                "coord_sync: skipped unparseable create-body sidecar lines"
            );
        }
        let pruned = store.prune(now);
        if pruned > 0 || unparseable > 0 || store.needs_compaction() {
            store.compact_logged();
        }
        store
    }

    fn apply(&mut self, line: Line) {
        match line {
            Line::Created {
                session_id,
                body,
                tenant_id,
                recorded_at,
            } => {
                // First sight wins; a duplicate line changes nothing.
                self.entries.entry(session_id).or_insert(Entry {
                    body,
                    tenant_id,
                    recorded_at,
                    closed_at: None,
                });
            }
            Line::Closed {
                session_id,
                closed_at,
            } => {
                if let Some(e) = self.entries.get_mut(&session_id) {
                    e.closed_at.get_or_insert(closed_at);
                }
            }
        }
    }

    /// Drop entries closed more than [`PRUNE_AFTER_CLOSE`] ago, then evict
    /// past [`MAX_SESSIONS`]. Returns how many were removed.
    fn prune(&mut self, now: DateTime<Utc>) -> usize {
        let before = self.entries.len();
        let horizon = chrono::Duration::seconds(PRUNE_AFTER_CLOSE.as_secs() as i64);
        self.entries.retain(|_, e| {
            e.closed_at
                .is_none_or(|c| now.signed_duration_since(c) <= horizon)
        });
        if self.entries.len() > MAX_SESSIONS {
            // Closed sessions go first (oldest close first), then the oldest
            // recorded open ones.
            let mut order: Vec<(Uuid, bool, DateTime<Utc>)> = self
                .entries
                .iter()
                .map(|(id, e)| {
                    (
                        *id,
                        e.closed_at.is_none(),
                        e.closed_at.unwrap_or(e.recorded_at),
                    )
                })
                .collect();
            order.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
            let excess = self.entries.len() - MAX_SESSIONS;
            for (id, _, _) in order.into_iter().take(excess) {
                self.entries.remove(&id);
            }
        }
        before - self.entries.len()
    }

    fn needs_compaction(&self) -> bool {
        self.lines > 2 * self.entries.len() + 64
    }

    fn append(&mut self, line: &Line) -> std::io::Result<()> {
        let mut buf = serde_json::to_string(line).map_err(std::io::Error::other)?;
        buf.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(buf.as_bytes())?;
        f.sync_data()?;
        self.lines += 1;
        Ok(())
    }

    /// Rewrite the file from the live entries (temp file + rename).
    fn compact(&mut self) -> std::io::Result<()> {
        let mut buf = String::new();
        let mut lines = 0usize;
        for (id, e) in &self.entries {
            let created = Line::Created {
                session_id: *id,
                body: e.body.clone(),
                tenant_id: e.tenant_id,
                recorded_at: e.recorded_at,
            };
            buf.push_str(&serde_json::to_string(&created).map_err(std::io::Error::other)?);
            buf.push('\n');
            lines += 1;
            if let Some(closed_at) = e.closed_at {
                let closed = Line::Closed {
                    session_id: *id,
                    closed_at,
                };
                buf.push_str(&serde_json::to_string(&closed).map_err(std::io::Error::other)?);
                buf.push('\n');
                lines += 1;
            }
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(buf.as_bytes())?;
            f.sync_data()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        self.lines = lines;
        Ok(())
    }

    fn compact_logged(&mut self) {
        if let Err(e) = self.compact() {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "coord_sync: create-body sidecar compaction failed — the append-only file \
                 keeps growing until the next attempt"
            );
        }
    }

    /// Append, then compact lazily once the file has outgrown its entries.
    fn append_logged(&mut self, line: &Line, now: DateTime<Utc>) {
        if let Err(e) = self.append(line) {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "coord_sync: create-body sidecar write failed — kept in memory only, so a \
                 re-register after a restart would find no body for this session"
            );
            return;
        }
        if self.entries.len() > MAX_SESSIONS || self.needs_compaction() {
            self.prune(now);
            self.compact_logged();
        }
    }
}

// ---------------------------------------------------------------------------
// The drain-facing handle
// ---------------------------------------------------------------------------

/// The re-register state one `CoordSync` drain owns: the create-body sidecar,
/// the per-session POST budget and the counters.
#[derive(Debug)]
pub(crate) struct Reregister {
    store: Mutex<Store>,
    /// When each session's last re-register POST went out. Pruned to the
    /// window on every budget check, so bounded by the sessions re-registered
    /// in the last [`REREGISTER_WINDOW`].
    last_attempt: Mutex<HashMap<Uuid, Instant>>,
    /// The budget window in milliseconds ([`REREGISTER_WINDOW`] unless a test
    /// overrides it).
    window_ms: AtomicU64,
    id_collision: AtomicU64,
    reregister_refused: AtomicU64,
    no_body: AtomicU64,
    created: AtomicU64,
}

impl Reregister {
    /// Open (or start) the sidecar beside the outbox at `outbox_path`.
    pub(crate) fn open(outbox_path: &Path) -> Self {
        Self {
            store: Mutex::new(Store::load(created_path(outbox_path), Utc::now())),
            last_attempt: Mutex::new(HashMap::new()),
            window_ms: AtomicU64::new(REREGISTER_WINDOW.as_millis() as u64),
            id_collision: AtomicU64::new(0),
            reregister_refused: AtomicU64::new(0),
            no_body: AtomicU64::new(0),
            created: AtomicU64::new(0),
        }
    }

    /// Test seam: shorten (or lengthen) the per-session POST budget window.
    #[cfg(test)]
    pub(crate) fn set_window(&self, window: Duration) {
        self.window_ms
            .store(window.as_millis() as u64, Ordering::Relaxed);
    }

    fn window(&self) -> Duration {
        Duration::from_millis(self.window_ms.load(Ordering::Relaxed))
    }

    /// Whether a create body is already recorded for `session_id`.
    pub(crate) fn has_body(&self, session_id: Uuid) -> bool {
        self.store
            .lock()
            .map(|s| s.entries.contains_key(&session_id))
            .unwrap_or(true)
    }

    /// Record `body` for `session_id` on first sight; a session already
    /// recorded is left untouched (no duplicate line).
    pub(crate) fn record_body(&self, session_id: Uuid, body: JsonValue, tenant_id: Option<Uuid>) {
        let Ok(mut store) = self.store.lock() else {
            return;
        };
        if store.entries.contains_key(&session_id) {
            return;
        }
        let now = Utc::now();
        store.entries.insert(
            session_id,
            Entry {
                body: body.clone(),
                tenant_id,
                recorded_at: now,
                closed_at: None,
            },
        );
        let line = Line::Created {
            session_id,
            body,
            tenant_id,
            recorded_at: now,
        };
        store.append_logged(&line, now);
    }

    /// Coord ACKed `session_id`'s `closed` row: start its prune clock.
    pub(crate) fn note_closed(&self, session_id: Uuid) {
        let Ok(mut store) = self.store.lock() else {
            return;
        };
        let now = Utc::now();
        match store.entries.get_mut(&session_id) {
            Some(e) if e.closed_at.is_none() => e.closed_at = Some(now),
            _ => return,
        }
        store.append_logged(
            &Line::Closed {
                session_id,
                closed_at: now,
            },
            now,
        );
    }

    /// The recorded create body for `session_id`, if any.
    pub(crate) fn recorded(&self, session_id: Uuid) -> Option<RecordedBody> {
        let store = self.store.lock().ok()?;
        store.entries.get(&session_id).map(|e| RecordedBody {
            body: e.body.clone(),
            tenant_id: e.tenant_id,
        })
    }

    /// Take the session's re-register budget: `true` (and the clock starts)
    /// unless a POST for it already went out inside the window.
    fn take_budget(&self, session_id: Uuid) -> bool {
        let window = self.window();
        let now = Instant::now();
        let Ok(mut last) = self.last_attempt.lock() else {
            return false;
        };
        last.retain(|_, at| now.saturating_duration_since(*at) < window);
        if last.contains_key(&session_id) {
            return false;
        }
        last.insert(session_id, now);
        true
    }

    /// One budgeted re-register of `session_id`: the budget check, the POST
    /// ([`reregister`]), and the counters. A [`ReregisterOutcome::NoBody`]
    /// spends the budget too, so a session with no body is counted at most
    /// once per window rather than on every drain retry.
    pub(crate) async fn attempt(
        &self,
        http: &reqwest::Client,
        base: &str,
        session_id: Uuid,
        fallback_scope: TenantScope,
    ) -> Attempt {
        if !self.take_budget(session_id) {
            return Attempt::BudgetSpent;
        }
        let recorded = self.recorded(session_id);
        let outcome = reregister(http, base, session_id, recorded, fallback_scope).await;
        match &outcome {
            ReregisterOutcome::Created => {
                self.created.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    coord_session = %session_id,
                    "coord_sync: coord had no row for this session (404 session_not_found) — \
                     re-created it from the recorded create body; retrying the row"
                );
            }
            ReregisterOutcome::Collision(code) => {
                self.id_collision.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    session = %session_id,
                    code = %code,
                    "coord_sync: re-register POST /sessions answered 409 — the id is held \
                     elsewhere; the session is ORPHANED and its lifecycle rows are held, not \
                     dropped (no conflict modal: this is not an acquire)"
                );
            }
            ReregisterOutcome::Refused { status } => {
                self.reregister_refused.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    session = %session_id,
                    status,
                    "coord_sync: coord refused to re-create the session row — its lifecycle \
                     rows are held and re-tried on the re-register budget"
                );
            }
            ReregisterOutcome::NoBody => {
                self.no_body.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    session = %session_id,
                    "coord_sync: coord has no row for this session and no create body is \
                     recorded for it (registered before the sidecar existed, or never seen \
                     pending) — its lifecycle rows are held, never posted with an invented body"
                );
            }
            ReregisterOutcome::Transport(e) => tracing::warn!(
                session = %session_id,
                error = %e,
                "coord_sync: re-register POST /sessions did not get an answer — retried on \
                 the session's backoff once the budget allows"
            ),
        }
        Attempt::Done(outcome)
    }

    /// A point-in-time copy of the counters.
    pub(crate) fn counts(&self) -> ReregisterCounts {
        ReregisterCounts {
            id_collision: self.id_collision.load(Ordering::Relaxed),
            reregister_refused: self.reregister_refused.load(Ordering::Relaxed),
            no_body: self.no_body.load(Ordering::Relaxed),
            created: self.created.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn created_path_sits_beside_the_outbox() {
        let p = created_path(Path::new("/x/session-outbox.jsonl"));
        assert_eq!(p, Path::new("/x/session-outbox.jsonl.created.jsonl"));
    }

    #[test]
    fn record_body_is_idempotent_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("outbox.jsonl");
        let r = Reregister::open(&outbox);
        let s = Uuid::new_v4();
        let t = Uuid::new_v4();
        r.record_body(s, json!({"id": s, "n": 1}), Some(t));
        r.record_body(s, json!({"id": s, "n": 2}), None);
        let text = std::fs::read_to_string(created_path(&outbox)).unwrap();
        assert_eq!(text.lines().count(), 1, "first sight only: {text}");

        let again = Reregister::open(&outbox);
        let got = again.recorded(s).expect("survives a reload");
        assert_eq!(got.body["n"], 1, "first sight wins");
        assert_eq!(got.tenant_id, Some(t));
        assert!(again.has_body(s));
        assert!(!again.has_body(Uuid::new_v4()));
    }

    #[test]
    fn a_session_closed_more_than_seven_days_ago_is_pruned_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = dir.path().join("outbox.jsonl");
        let path = created_path(&outbox);
        let (old, recent, open) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let t0 = now() - chrono::Duration::days(30);
        let mut text = String::new();
        for id in [old, recent, open] {
            let l = Line::Created {
                session_id: id,
                body: json!({"id": id}),
                tenant_id: None,
                recorded_at: t0,
            };
            text.push_str(&serde_json::to_string(&l).unwrap());
            text.push('\n');
        }
        for (id, at) in [
            (old, now() - chrono::Duration::days(8)),
            (recent, now() - chrono::Duration::days(6)),
        ] {
            let l = Line::Closed {
                session_id: id,
                closed_at: at,
            };
            text.push_str(&serde_json::to_string(&l).unwrap());
            text.push('\n');
        }
        text.push_str("{\"op\":\"created\",\"session_id\":"); // torn line
        std::fs::write(&path, text).unwrap();

        let r = Reregister::open(&outbox);
        assert!(!r.has_body(old), "closed 8 days ago: pruned");
        assert!(r.has_body(recent), "closed 6 days ago: kept");
        assert!(r.has_body(open), "never closed: kept");
        // The prune rewrote the file compacted.
        let reloaded = Reregister::open(&outbox);
        assert!(!reloaded.has_body(old));
        assert!(reloaded.has_body(recent) && reloaded.has_body(open));
    }

    #[test]
    fn the_store_is_capped_evicting_closed_sessions_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store {
            path: dir.path().join("c.jsonl"),
            entries: HashMap::new(),
            lines: 0,
        };
        let closed = Uuid::new_v4();
        let base = now() - chrono::Duration::days(1);
        store.entries.insert(
            closed,
            Entry {
                body: json!({}),
                tenant_id: None,
                recorded_at: base,
                closed_at: Some(now()),
            },
        );
        let mut oldest_open = None;
        for i in 0..MAX_SESSIONS {
            let id = Uuid::new_v4();
            if i == 0 {
                oldest_open = Some(id);
            }
            store.entries.insert(
                id,
                Entry {
                    body: json!({}),
                    tenant_id: None,
                    recorded_at: base + chrono::Duration::seconds(i as i64),
                    closed_at: None,
                },
            );
        }
        assert_eq!(store.prune(now()), 1);
        assert_eq!(store.entries.len(), MAX_SESSIONS);
        assert!(!store.entries.contains_key(&closed), "closed goes first");
        assert!(store.entries.contains_key(&oldest_open.unwrap()));
    }

    #[test]
    fn the_budget_allows_one_attempt_per_window() {
        let dir = tempfile::tempdir().unwrap();
        let r = Reregister::open(&dir.path().join("outbox.jsonl"));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(r.take_budget(a));
        assert!(!r.take_budget(a), "inside the window");
        assert!(r.take_budget(b), "per session");
        r.set_window(Duration::from_millis(0));
        assert!(r.take_budget(a), "a spent window re-grants it");
    }

    #[test]
    fn counters_render_dense() {
        let c = ReregisterCounts {
            id_collision: 1,
            reregister_refused: 0,
            no_body: 2,
            created: 3,
        };
        let o = c.orphaned_json();
        assert_eq!(o["id_collision"], 1);
        assert_eq!(o["reregister_refused"], 0, "zeros present");
        assert_eq!(o["no_body"], 2);
        assert_eq!(o.as_object().unwrap().len(), 3);
        assert_eq!(c.reregistered_json(), json!({"created": 3}));
    }
}
