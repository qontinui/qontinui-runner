//! The operator's review of a session's code changes: which hunks they have
//! read, the notes they wrote on hunks, and the send door that turns attached
//! notes into a prompt.
//!
//! Plan `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
//! Phase 3 (Design decisions 2 and 3). The Terminal page's pure reducer
//! (`src/components/terminal/sessionReview.ts` `noteTransition`) is a
//! convenience for the UI; THIS module is the authority. [`transition`]
//! mirrors that reducer's edges exactly, and every write — a client event, a
//! send, a transcript confirmation, a terminal exit — goes through it and then
//! through a compare-and-set on the stored state, so no path can move a note
//! along an edge the lifecycle does not have.
//!
//! # Routes
//!
//! | Route | Effect |
//! |---|---|
//! | `GET /sessions/{id}/review` | read hunk keys + every note |
//! | `PUT /sessions/{id}/review/hunks` | mark hunks read / unread (idempotent) |
//! | `POST /sessions/{id}/review/notes` | create a `pending` note |
//! | `PATCH /sessions/{id}/review/notes/{note_id}` | `attach` / `detach` / `edit` / `discard` |
//! | `DELETE /sessions/{id}/review/notes/{note_id}` | `discard` |
//! | `POST /sessions/{id}/review/send` | deliver the composed text; notes → `submitted` |
//! | `POST /sessions/{id}/review/insert` | type the composed text with no CR; notes stay `attached` |
//!
//! `{id}` is the same key `GET /sessions/{id}/file-changes` takes: the
//! `claude` session id for a PTY tab (the transcript stem the tail records
//! touched files under — `tab.claudeSessionId` on the Terminal page), the
//! task-run id for a stream-json worker. The send TARGET is separate and
//! explicit, because a review session id is not a terminal id.
//!
//! # Delivery and confirmation
//!
//! The composed text is built client-side (`composeReviewPrompt`) and arrives
//! already carrying its `[review <8-hex>]` marker; the server extracts the
//! marker with the same rule as `parseMarker` ([`parse_marker`]). A PTY target
//! is written through [`crate::terminal::session::TerminalSession::submit_prompt`]
//! — the one choke point every inbound prompt uses — tagged
//! [`PtyWriteCaller::ReviewNotes`]; a task-run target through
//! [`crate::claude_session::worker_message::send_message_to_worker_via_handle`].
//! Notes move to `submitted` ONLY after that write returns Ok; a failed write
//! leaves them `attached` and returns the error.
//!
//! A write that returned is not a prompt that arrived. A note becomes
//! `confirmed` only when an operator prompt carrying its marker is observed in
//! a session transcript — [`observe_transcript_line`], called per line by the
//! transcript tail (`terminal/transcript_watcher.rs`), so a tail parked after
//! 600 s idle and revived on the next write is covered by the same call. When
//! the target terminal exits, its still-`submitted` notes settle to `unknown`
//! ([`settle_on_terminal_exit`]) — never to `confirmed` on a guess. A task-run
//! target settles the same way when its stream-json worker process ends with
//! no successor ([`settle_on_task_run_end`]). `unknown` is not terminal: a
//! later sighting of the marker is positive evidence the prompt arrived, so
//! `unknown → confirmed` is an edge.
//!
//! Two paths would otherwise strand a note at `submitted`. A target that exits
//! between the delivery and the `submitted` write has already run its exit
//! hook, which found nothing to settle — so [`send`] re-checks the target's
//! liveness AFTER that write and settles in-line ([`PromptDoor::target_live`]).
//! And a runner restart drops every exit hook on the floor — so
//! [`start_stranded_note_sweep`] settles, once at boot, every `submitted` note
//! whose target no live terminal or task run holds.
//!
//! Every mutation emits `session-review-changed` `{ "sessionId": … }` so the
//! page refreshes.
//!
//! # Known bound
//!
//! Confirmation reads the PTY transcript tail. A stream-json task run's
//! transcript is not tailed by it (the watcher tears workflow sessions down),
//! so a note sent to a `taskRunId` target stays `submitted` — which is true —
//! rather than being promoted on the strength of a stdin write, until the
//! worker's process ends, when it settles to `unknown`.

use std::collections::{BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::mcp::types::ApiState;
use crate::terminal::session::PtyWriteCaller;

// ===========================================================================
// Bounds
// ===========================================================================

/// Longest session id a route accepts.
const MAX_SESSION_ID_BYTES: usize = 256;
/// Longest hunk key (`<sha256hex>-<ordinal>` is ~70 bytes).
const MAX_HUNK_KEY_BYTES: usize = 256;
/// Longest file path.
const MAX_FILE_PATH_BYTES: usize = 4096;
/// Longest hunk header (`@@ -a,b +c,d @@ context`).
const MAX_HUNK_HEADER_BYTES: usize = 1024;
/// Longest stored excerpt (the composed prompt carries ≤ 12 lines of it).
const MAX_EXCERPT_BYTES: usize = 64 * 1024;
/// Longest note body.
const MAX_NOTE_BODY_BYTES: usize = 16 * 1024;
/// Most hunks one mark-read call may name.
const MAX_HUNKS_PER_MARK: usize = 2000;
/// Most notes one send may carry.
const MAX_NOTES_PER_SEND: usize = 100;
/// Longest composed text a send may deliver.
const MAX_SEND_TEXT_BYTES: usize = 256 * 1024;

// ===========================================================================
// The model
// ===========================================================================

/// Where a note is in its lifecycle — `ReviewNoteState` in `sessionReview.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteState {
    Pending,
    Attached,
    Submitted,
    Confirmed,
    Discarded,
    Unknown,
}

impl NoteState {
    /// Every state, in lifecycle order.
    pub const ALL: [NoteState; 6] = [
        NoteState::Pending,
        NoteState::Attached,
        NoteState::Submitted,
        NoteState::Confirmed,
        NoteState::Discarded,
        NoteState::Unknown,
    ];

    /// The stored / wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            NoteState::Pending => "pending",
            NoteState::Attached => "attached",
            NoteState::Submitted => "submitted",
            NoteState::Confirmed => "confirmed",
            NoteState::Discarded => "discarded",
            NoteState::Unknown => "unknown",
        }
    }

    /// Inverse of [`Self::as_str`].
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == raw)
    }
}

/// An event's name — `ReviewNoteEventType` in `sessionReview.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteEventKind {
    Attach,
    Detach,
    Discard,
    Edit,
    Submit,
    Confirm,
    SessionEnded,
}

impl NoteEventKind {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            NoteEventKind::Attach => "attach",
            NoteEventKind::Detach => "detach",
            NoteEventKind::Discard => "discard",
            NoteEventKind::Edit => "edit",
            NoteEventKind::Submit => "submit",
            NoteEventKind::Confirm => "confirm",
            NoteEventKind::SessionEnded => "sessionEnded",
        }
    }

    /// States this event may leave from — `LEGAL_FROM` in `sessionReview.ts`,
    /// edge for edge. Anything else is an illegal edge.
    pub fn legal_from(self) -> &'static [NoteState] {
        match self {
            NoteEventKind::Attach => &[NoteState::Pending],
            NoteEventKind::Detach => &[NoteState::Attached],
            NoteEventKind::Discard | NoteEventKind::Edit => {
                &[NoteState::Pending, NoteState::Attached]
            }
            NoteEventKind::Submit => &[NoteState::Attached],
            // `unknown → confirmed`: a marker sighting after the target ended
            // (or after a restart's sweep) is positive evidence it arrived.
            NoteEventKind::Confirm => &[NoteState::Submitted, NoteState::Unknown],
            NoteEventKind::SessionEnded => &[NoteState::Submitted],
        }
    }
}

/// Where a send delivers: `{ "terminalId": … }` or `{ "taskRunId": … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteTarget {
    /// A PTY tab, by terminal id.
    TerminalId(String),
    /// A stream-json worker, by task-run id.
    TaskRunId(String),
}

impl NoteTarget {
    /// The stored `target_kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            NoteTarget::TerminalId(_) => "terminal",
            NoteTarget::TaskRunId(_) => "task_run",
        }
    }

    /// The stored `target_id`.
    pub fn id(&self) -> &str {
        match self {
            NoteTarget::TerminalId(id) | NoteTarget::TaskRunId(id) => id,
        }
    }

    /// Inverse of [`Self::kind`] + [`Self::id`].
    pub fn from_parts(kind: &str, id: String) -> Option<Self> {
        match kind {
            "terminal" => Some(NoteTarget::TerminalId(id)),
            "task_run" => Some(NoteTarget::TaskRunId(id)),
            _ => None,
        }
    }
}

/// One note — `ReviewNote` in `sessionReview.ts`, plus where it was sent.
/// Timestamps are RFC 3339 (UTC, millisecond precision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNote {
    pub id: String,
    pub session_id: String,
    pub file_path: String,
    pub hunk_key: String,
    pub hunk_header: String,
    pub excerpt: String,
    pub body: String,
    pub state: NoteState,
    pub marker: Option<String>,
    pub created_at: String,
    pub submitted_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub target: Option<NoteTarget>,
}

/// One hunk the operator has read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadHunk {
    pub hunk_key: String,
    pub file_path: String,
    pub read_at: String,
}

/// A hunk named by a mark-read call.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HunkRef {
    pub hunk_key: String,
    pub file_path: String,
}

/// A lifecycle event, with what each one carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteEvent {
    Attach,
    Detach,
    Discard,
    Edit {
        body: String,
    },
    Submit {
        marker: String,
        at: String,
        target: NoteTarget,
    },
    Confirm {
        marker: String,
        at: String,
    },
    SessionEnded,
}

impl NoteEvent {
    /// This event's name.
    pub fn kind(&self) -> NoteEventKind {
        match self {
            NoteEvent::Attach => NoteEventKind::Attach,
            NoteEvent::Detach => NoteEventKind::Detach,
            NoteEvent::Discard => NoteEventKind::Discard,
            NoteEvent::Edit { .. } => NoteEventKind::Edit,
            NoteEvent::Submit { .. } => NoteEventKind::Submit,
            NoteEvent::Confirm { .. } => NoteEventKind::Confirm,
            NoteEvent::SessionEnded => NoteEventKind::SessionEnded,
        }
    }
}

/// Why [`transition`] refused — `NoteTransitionRefusal` in `sessionReview.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub from: NoteState,
    pub event: NoteEventKind,
    pub reason: String,
}

/// The lifecycle as a total function — `noteTransition` in `sessionReview.ts`,
/// and the authority it defers to. An illegal edge is a typed refusal.
pub fn transition(note: &ReviewNote, event: NoteEvent) -> Result<ReviewNote, Refusal> {
    let from = note.state;
    let kind = event.kind();
    let refuse = |reason: String| Refusal {
        from,
        event: kind,
        reason,
    };
    if !kind.legal_from().contains(&from) {
        let legal: Vec<&str> = kind.legal_from().iter().map(|s| s.as_str()).collect();
        return Err(refuse(format!(
            "\"{}\" is not a legal edge out of \"{}\" (legal from: {})",
            kind.as_str(),
            from.as_str(),
            legal.join(", ")
        )));
    }
    let mut next = note.clone();
    match event {
        NoteEvent::Attach => next.state = NoteState::Attached,
        NoteEvent::Detach => next.state = NoteState::Pending,
        NoteEvent::Discard => next.state = NoteState::Discarded,
        NoteEvent::Edit { body } => next.body = body,
        NoteEvent::Submit { marker, at, target } => {
            if !is_marker(&marker) {
                return Err(refuse(format!(
                    "\"{marker}\" is not an 8-hex review marker"
                )));
            }
            next.state = NoteState::Submitted;
            next.marker = Some(marker);
            next.submitted_at = Some(at);
            next.target = Some(target);
        }
        NoteEvent::Confirm { marker, at } => {
            // Confirmation is evidence the prompt THIS note went out in
            // arrived; a different marker is some other prompt.
            if note.marker.as_deref() != Some(marker.as_str()) {
                return Err(refuse(format!(
                    "observed marker \"{marker}\" is not this note's marker \"{}\"",
                    note.marker.as_deref().unwrap_or("<none>")
                )));
            }
            next.state = NoteState::Confirmed;
            next.confirmed_at = Some(at);
        }
        NoteEvent::SessionEnded => next.state = NoteState::Unknown,
    }
    Ok(next)
}

/// Exactly eight lowercase hex digits.
fn is_marker(candidate: &str) -> bool {
    candidate.len() == 8
        && candidate
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The first `[review <8-hex>]` marker anywhere in `text` — `parseMarker` in
/// `sessionReview.ts` (`/\[review ([0-9a-f]{8})\]/`), matched anywhere because
/// a transcript may prefix the prompt.
pub fn parse_marker(text: &str) -> Option<String> {
    const OPEN: &str = "[review ";
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = text.get(from..).and_then(|rest| rest.find(OPEN)) {
        let start = from + rel + OPEN.len();
        if let Some(candidate) = bytes.get(start..start + 9) {
            if let (Some(hex), Some(b']')) = (candidate.get(..8), candidate.get(8)) {
                if let Ok(hex) = std::str::from_utf8(hex) {
                    if is_marker(hex) {
                        return Some(hex.to_string());
                    }
                }
            }
        }
        // `[` is one byte, so this stays on a char boundary.
        from += rel + 1;
    }
    None
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ===========================================================================
// The store seam
// ===========================================================================

/// Persistence for reviews. Production is `PgDb`
/// (`database/pg/session_review.rs`); the tests use an in-memory store, so the
/// lifecycle and the send door are exercised without a database.
#[async_trait]
pub trait ReviewStore: Send + Sync {
    /// Every hunk `session_id` has marked read.
    async fn read_hunks(&self, session_id: &str) -> Result<Vec<ReadHunk>, String>;
    /// Mark `hunks` read (insert, keeping an existing `read_at`) or unread
    /// (delete). Returns how many rows actually changed.
    async fn set_hunks_read(
        &self,
        session_id: &str,
        hunks: &[HunkRef],
        read: bool,
    ) -> Result<usize, String>;
    /// Every note of `session_id`, oldest first.
    async fn list_notes(&self, session_id: &str) -> Result<Vec<ReviewNote>, String>;
    /// One note by id, whatever its session.
    async fn get_note(&self, note_id: &str) -> Result<Option<ReviewNote>, String>;
    /// Store a new note.
    async fn insert_note(&self, note: &ReviewNote) -> Result<(), String>;
    /// Compare-and-set: write `note`'s mutable fields only if the stored state
    /// is still `expected`. `false` means another writer moved it first.
    async fn replace_note_if(&self, note: &ReviewNote, expected: NoteState)
        -> Result<bool, String>;
    /// Every note carrying `marker`, in any state, whatever its session — the
    /// confirmation read (filtered to `submitted` / `unknown`) and the send's
    /// marker-reuse refusal.
    async fn notes_with_marker(&self, marker: &str) -> Result<Vec<ReviewNote>, String>;
    /// Every `submitted` note, whatever its session or target — the boot
    /// sweep's read.
    async fn submitted_notes(&self) -> Result<Vec<ReviewNote>, String>;
    /// `submitted` notes that were sent to `target`.
    async fn submitted_notes_for_target(
        &self,
        target: &NoteTarget,
    ) -> Result<Vec<ReviewNote>, String>;
}

// ===========================================================================
// The delivery seam
// ===========================================================================

/// Submit the text as a turn, or only type it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// Bracketed paste + CR — the notes go out.
    Submit,
    /// Bracketed paste, no CR — a draft the operator sends themselves.
    Insert,
}

/// What a delivery reported. `sanitized` / `bytes` come from the PTY choke
/// point's `SubmitPayload`; a task-run delivery has neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    pub sanitized: Option<bool>,
    pub bytes: Option<usize>,
}

/// Why a delivery did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// No live terminal / task-run session by that id.
    TargetNotFound(String),
    /// The target cannot do this mode (a task run has no draft to insert into).
    Unsupported(String),
    /// The write itself failed (an exited PTY, a refused task-run send) before
    /// any of the text reached the target.
    Failed(String),
    /// The write failed AFTER some or all of the text reached the target's
    /// input: the paste was written but the submitting CR was not, or the paste
    /// itself broke part-way. The text may be sitting in the TUI's input box.
    Partial(String),
}

/// Where composed text is delivered. Production is [`LiveDoor`].
#[async_trait]
pub trait PromptDoor: Send + Sync {
    async fn deliver(
        &self,
        target: &NoteTarget,
        text: &str,
        mode: SendMode,
    ) -> Result<Delivered, DeliveryError>;

    /// Can `target` still receive a turn? Read AFTER a send records
    /// `submitted`: a target that exited in between ran its exit hook before
    /// that write and settled nothing.
    fn target_live(&self, target: &NoteTarget) -> bool;
}

/// Whether `target` is live in this runner process: a terminal the manager
/// holds whose process has not exited, or a task run whose session is active.
///
/// The terminal half reads `is_alive`, which the waiter clears BEFORE it runs
/// the exit hook ([`settle_on_terminal_exit`]); the task-run half is the same
/// predicate [`settle_on_task_run_end`] uses, whose waiter force-closes the
/// session state before calling it. So whichever of "the hook" and "a send's
/// re-check" reads second sees the other's write.
pub fn target_is_live(app: &tauri::AppHandle, target: &NoteTarget) -> bool {
    use tauri::Manager;
    match target {
        NoteTarget::TerminalId(id) => app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
            .and_then(|tm| tm.get(id))
            .is_some_and(|session| session.is_alive()),
        NoteTarget::TaskRunId(id) => app
            .try_state::<Arc<crate::claude_session::SessionManager>>()
            .and_then(|sm| sm.get(id))
            .is_some_and(|session| session.state().is_active()),
    }
}

/// The production door: the PTY submit choke point for a terminal, the
/// in-process worker message primitive for a task run.
pub struct LiveDoor {
    state: Arc<ApiState>,
}

#[async_trait]
impl PromptDoor for LiveDoor {
    async fn deliver(
        &self,
        target: &NoteTarget,
        text: &str,
        mode: SendMode,
    ) -> Result<Delivered, DeliveryError> {
        match target {
            NoteTarget::TerminalId(id) => {
                let session = crate::mcp::terminals::get_terminal_manager(&self.state)
                    .get(id)
                    .ok_or_else(|| {
                        DeliveryError::TargetNotFound(format!("terminal not found: {id}"))
                    })?;
                let text = text.to_string();
                // `submit_prompt` sleeps between the paste and the CR; keep it
                // off the async executor.
                let payload = tokio::task::spawn_blocking(move || match mode {
                    SendMode::Submit => session.submit_prompt(&text, PtyWriteCaller::ReviewNotes),
                    SendMode::Insert => session.insert_prompt(&text, PtyWriteCaller::ReviewNotes),
                })
                .await
                .map_err(|e| DeliveryError::Partial(format!("terminal write panicked: {e}")))?
                .map_err(|e| {
                    // The choke point tags an error raised once the paste had
                    // started reaching the PTY; nothing else wrote a byte.
                    if e.starts_with(crate::terminal::session::PROMPT_PARTIALLY_WRITTEN) {
                        DeliveryError::Partial(e)
                    } else {
                        DeliveryError::Failed(e)
                    }
                })?;
                Ok(Delivered {
                    sanitized: Some(payload.sanitized),
                    bytes: Some(payload.bytes),
                })
            }
            NoteTarget::TaskRunId(id) => {
                if mode == SendMode::Insert {
                    return Err(DeliveryError::Unsupported(
                        "a task-run session has no input box to insert a draft into; send it"
                            .to_string(),
                    ));
                }
                if crate::drain::is_draining() {
                    return Err(DeliveryError::Failed(
                        "runner is draining for a planned restart — new messages are refused"
                            .to_string(),
                    ));
                }
                use tauri::Manager;
                let live = self
                    .state
                    .app_handle
                    .try_state::<Arc<crate::claude_session::SessionManager>>()
                    .is_some_and(|sm| sm.get(id).is_some());
                if !live {
                    return Err(DeliveryError::TargetNotFound(format!(
                        "no active session for task run {id}"
                    )));
                }
                crate::claude_session::worker_message::send_message_to_worker_via_handle(
                    &self.state.app_handle,
                    id,
                    text,
                )
                .await
                .map_err(DeliveryError::Failed)?;
                Ok(Delivered {
                    sanitized: None,
                    bytes: None,
                })
            }
        }
    }

    fn target_live(&self, target: &NoteTarget) -> bool {
        target_is_live(&self.state.app_handle, target)
    }
}

// ===========================================================================
// Marker sightings — the send/confirm race
// ===========================================================================

/// Markers recently seen in a transcript, so a confirmation that lands BEFORE
/// the send has recorded `submitted` is not lost.
///
/// The CR is written, `claude` writes the user record, and the tail can read
/// it before the send path's own `submitted` write commits; the confirmation
/// would then find no `submitted` note and the note would sit at `submitted`
/// forever. Each side therefore checks the other: the tail records the
/// sighting BEFORE it looks for `submitted` notes, and the send looks for a
/// sighting AFTER it writes `submitted` — whichever runs second sees the
/// other's write. Bounded in count and age.
pub struct MarkerSightings {
    seen: Mutex<VecDeque<(String, Instant)>>,
}

const SIGHTING_CAP: usize = 256;
const SIGHTING_TTL: Duration = Duration::from_secs(600);

impl MarkerSightings {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(VecDeque::new()),
        }
    }

    fn record(&self, marker: &str) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        seen.retain(|(_, at)| now.duration_since(*at) < SIGHTING_TTL);
        seen.push_back((marker.to_string(), now));
        while seen.len() > SIGHTING_CAP {
            seen.pop_front();
        }
    }

    fn seen(&self, marker: &str) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        seen.iter()
            .any(|(m, at)| m == marker && now.duration_since(*at) < SIGHTING_TTL)
    }
}

impl Default for MarkerSightings {
    fn default() -> Self {
        Self::new()
    }
}

static SIGHTINGS: LazyLock<MarkerSightings> = LazyLock::new(MarkerSightings::new);

/// Mutations that read-then-write `pending`/`attached` notes (client events,
/// sends) are serialized per session, so a detach cannot slip between a send's
/// validation and its delivery. Striped: a fixed set of locks, so nothing
/// grows with the number of sessions ever reviewed.
static SESSION_LOCKS: LazyLock<Vec<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| (0..32).map(|_| tokio::sync::Mutex::new(())).collect());

fn session_lock(session_id: &str) -> &'static tokio::sync::Mutex<()> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % SESSION_LOCKS.len();
    &SESSION_LOCKS[idx]
}

// ===========================================================================
// Typed refusals
// ===========================================================================

/// A route refusal: `{ "error", "code", "from"?, "event"?, "noteIds"? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub from: Option<NoteState>,
    pub event: Option<NoteEventKind>,
    pub note_ids: Vec<String>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            from: None,
            event: None,
            note_ids: Vec::new(),
        }
    }

    fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "store_error", message)
    }

    fn with_notes(mut self, ids: Vec<String>) -> Self {
        self.note_ids = ids;
        self
    }

    /// 409 for an edge the lifecycle does not have.
    fn refused(note_id: &str, refusal: Refusal) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "illegal_transition",
            message: refusal.reason,
            from: Some(refusal.from),
            event: Some(refusal.event),
            note_ids: vec![note_id.to_string()],
        }
    }

    fn note_not_found(session_id: &str, note_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "note_not_found",
            format!("no note {note_id} in session {session_id}"),
        )
        .with_notes(vec![note_id.to_string()])
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message, "code": self.code });
        if let Some(from) = self.from {
            body["from"] = json!(from.as_str());
        }
        if let Some(event) = self.event {
            body["event"] = json!(event.as_str());
        }
        if !self.note_ids.is_empty() {
            body["noteIds"] = json!(self.note_ids);
        }
        (self.status, Json(body)).into_response()
    }
}

fn check_len(field: &str, value: &str, max: usize, required: bool) -> Result<(), ApiError> {
    if required && value.trim().is_empty() {
        return Err(ApiError::invalid(
            "invalid_field",
            format!("{field} must be non-empty"),
        ));
    }
    if value.len() > max {
        return Err(ApiError::invalid(
            "field_too_large",
            format!("{field} is {} bytes; the bound is {max}", value.len()),
        ));
    }
    Ok(())
}

fn check_session_id(session_id: &str) -> Result<(), ApiError> {
    check_len("session id", session_id, MAX_SESSION_ID_BYTES, true)
}

// ===========================================================================
// Operations — the route bodies, over the two seams
// ===========================================================================

/// `GET /sessions/{id}/review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewState {
    pub session_id: String,
    pub read_hunks: Vec<ReadHunk>,
    pub notes: Vec<ReviewNote>,
}

pub async fn read_review(
    store: &dyn ReviewStore,
    session_id: &str,
) -> Result<ReviewState, ApiError> {
    check_session_id(session_id)?;
    Ok(ReviewState {
        session_id: session_id.to_string(),
        read_hunks: store
            .read_hunks(session_id)
            .await
            .map_err(ApiError::internal)?,
        notes: store
            .list_notes(session_id)
            .await
            .map_err(ApiError::internal)?,
    })
}

/// `PUT /sessions/{id}/review/hunks` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkHunksRequest {
    pub hunks: Vec<HunkRef>,
    pub read: bool,
}

/// `PUT /sessions/{id}/review/hunks` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkHunksResponse {
    pub session_id: String,
    pub read: bool,
    /// Rows that actually changed — `0` on a repeat of the same call.
    pub changed: usize,
}

pub async fn mark_hunks(
    store: &dyn ReviewStore,
    session_id: &str,
    req: MarkHunksRequest,
) -> Result<MarkHunksResponse, ApiError> {
    check_session_id(session_id)?;
    if req.hunks.len() > MAX_HUNKS_PER_MARK {
        return Err(ApiError::invalid(
            "too_many_hunks",
            format!(
                "{} hunks named; the bound is {MAX_HUNKS_PER_MARK} per call",
                req.hunks.len()
            ),
        ));
    }
    for h in &req.hunks {
        check_len("hunkKey", &h.hunk_key, MAX_HUNK_KEY_BYTES, true)?;
        check_len("filePath", &h.file_path, MAX_FILE_PATH_BYTES, true)?;
    }
    // A key named twice in one call is one hunk.
    let mut seen = BTreeSet::new();
    let hunks: Vec<HunkRef> = req
        .hunks
        .into_iter()
        .filter(|h| seen.insert(h.hunk_key.clone()))
        .collect();
    let changed = if hunks.is_empty() {
        0
    } else {
        store
            .set_hunks_read(session_id, &hunks, req.read)
            .await
            .map_err(ApiError::internal)?
    };
    Ok(MarkHunksResponse {
        session_id: session_id.to_string(),
        read: req.read,
        changed,
    })
}

/// `POST /sessions/{id}/review/notes` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateNoteRequest {
    pub file_path: String,
    pub hunk_key: String,
    #[serde(default)]
    pub hunk_header: String,
    #[serde(default)]
    pub excerpt: String,
    pub body: String,
}

pub async fn create_note(
    store: &dyn ReviewStore,
    session_id: &str,
    req: CreateNoteRequest,
) -> Result<ReviewNote, ApiError> {
    check_session_id(session_id)?;
    check_len("filePath", &req.file_path, MAX_FILE_PATH_BYTES, true)?;
    check_len("hunkKey", &req.hunk_key, MAX_HUNK_KEY_BYTES, true)?;
    check_len("hunkHeader", &req.hunk_header, MAX_HUNK_HEADER_BYTES, false)?;
    check_len("excerpt", &req.excerpt, MAX_EXCERPT_BYTES, false)?;
    check_len("body", &req.body, MAX_NOTE_BODY_BYTES, true)?;
    let note = ReviewNote {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        file_path: req.file_path,
        hunk_key: req.hunk_key,
        hunk_header: req.hunk_header,
        excerpt: req.excerpt,
        body: req.body,
        state: NoteState::Pending,
        marker: None,
        created_at: now_iso(),
        submitted_at: None,
        confirmed_at: None,
        target: None,
    };
    store.insert_note(&note).await.map_err(ApiError::internal)?;
    Ok(note)
}

/// `PATCH /sessions/{id}/review/notes/{note_id}` body:
/// `{ "type": "attach" | "detach" | "discard" | "edit", "body"?: string }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchNoteRequest {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub body: Option<String>,
}

/// The client-settable event a PATCH names. `submit`, `confirm` and
/// `sessionEnded` are the server's own edges — a client asserting them would
/// be claiming a delivery or an observation the server did not make.
fn client_event(req: PatchNoteRequest) -> Result<NoteEvent, ApiError> {
    match req.event_type.as_str() {
        "attach" => Ok(NoteEvent::Attach),
        "detach" => Ok(NoteEvent::Detach),
        "discard" => Ok(NoteEvent::Discard),
        "edit" => {
            let body = req
                .body
                .ok_or_else(|| ApiError::invalid("invalid_field", "an edit event needs a body"))?;
            check_len("body", &body, MAX_NOTE_BODY_BYTES, true)?;
            Ok(NoteEvent::Edit { body })
        }
        "submit" | "confirm" | "sessionEnded" => Err(ApiError::invalid(
            "event_not_client_settable",
            format!(
                "\"{}\" is the server's edge: submit happens through POST …/review/send, \
                 confirm when the marker is observed in the transcript, sessionEnded when \
                 the target exits",
                req.event_type
            ),
        )),
        other => Err(ApiError::invalid(
            "unknown_event",
            format!("\"{other}\" is not a note event (attach, detach, discard, edit)"),
        )),
    }
}

/// Apply one client event to one note of `session_id`.
pub async fn apply_client_event(
    store: &dyn ReviewStore,
    session_id: &str,
    note_id: &str,
    req: PatchNoteRequest,
) -> Result<ReviewNote, ApiError> {
    check_session_id(session_id)?;
    let event = client_event(req)?;
    let _guard = session_lock(session_id).lock().await;
    let note = store
        .get_note(note_id)
        .await
        .map_err(ApiError::internal)?
        .filter(|n| n.session_id == session_id)
        .ok_or_else(|| ApiError::note_not_found(session_id, note_id))?;
    let next = transition(&note, event).map_err(|r| ApiError::refused(note_id, r))?;
    if !store
        .replace_note_if(&next, note.state)
        .await
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "concurrent_update",
            format!(
                "note {note_id} left \"{}\" before this event applied; re-read it",
                note.state.as_str()
            ),
        )
        .with_notes(vec![note_id.to_string()]));
    }
    Ok(next)
}

/// `POST /sessions/{id}/review/send` and `…/insert` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendRequest {
    pub target: NoteTarget,
    pub note_ids: Vec<String>,
    /// The exact composed text, marker line included.
    pub text: String,
}

/// `POST /sessions/{id}/review/send` and `…/insert` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOutcome {
    pub session_id: String,
    pub marker: String,
    /// `true` for a send (notes went out), `false` for an insert (a draft).
    pub submitted: bool,
    /// Did the PTY choke point's neutralizer change the text? `null` for a
    /// task-run target, which has no such step.
    pub sanitized: Option<bool>,
    /// Bytes written to the PTY. `null` for a task-run target.
    pub bytes: Option<usize>,
    /// The sent notes as they now stand.
    pub notes: Vec<ReviewNote>,
}

/// Validate, deliver, and — for [`SendMode::Submit`], only after the write
/// returned Ok — move every note to `submitted`.
pub async fn send(
    store: &dyn ReviewStore,
    door: &dyn PromptDoor,
    sightings: &MarkerSightings,
    session_id: &str,
    req: SendRequest,
    mode: SendMode,
) -> Result<SendOutcome, ApiError> {
    check_session_id(session_id)?;
    check_len("target id", req.target.id(), MAX_SESSION_ID_BYTES, true)?;
    check_len("text", &req.text, MAX_SEND_TEXT_BYTES, true)?;
    let marker = parse_marker(&req.text).ok_or_else(|| {
        ApiError::invalid(
            "marker_missing",
            "the text carries no [review <8-hex>] marker; compose it with composeReviewPrompt",
        )
    })?;
    let mut seen = BTreeSet::new();
    let note_ids: Vec<String> = req
        .note_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if note_ids.is_empty() {
        return Err(ApiError::invalid(
            "no_notes",
            "a review send carries at least one attached note",
        ));
    }
    if note_ids.len() > MAX_NOTES_PER_SEND {
        return Err(ApiError::invalid(
            "too_many_notes",
            format!(
                "{} notes; the bound is {MAX_NOTES_PER_SEND}",
                note_ids.len()
            ),
        ));
    }

    let _guard = session_lock(session_id).lock().await;

    let mut notes = Vec::with_capacity(note_ids.len());
    let mut foreign = Vec::new();
    for id in &note_ids {
        match store.get_note(id).await.map_err(ApiError::internal)? {
            Some(n) if n.session_id == session_id => notes.push(n),
            _ => foreign.push(id.clone()),
        }
    }
    if !foreign.is_empty() {
        return Err(ApiError::invalid(
            "note_not_in_session",
            format!("these notes do not exist in session {session_id}"),
        )
        .with_notes(foreign));
    }
    // The notes recorded `submitted` must be the notes the text carries: a
    // note whose comment is not in the text would be claimed sent when it was
    // not. Checked in the form `composeReviewPrompt` writes a comment.
    let absent: Vec<String> = notes
        .iter()
        .filter(|n| !req.text.contains(&composed_comment(&n.body)))
        .map(|n| n.id.clone())
        .collect();
    if !absent.is_empty() {
        return Err(ApiError::invalid(
            "note_not_in_text",
            "the text does not carry these notes' comments; compose it with composeReviewPrompt \
             from the notes being sent",
        )
        .with_notes(absent));
    }
    // A marker is one send's identity: confirmation promotes every note that
    // carries it, so reusing one would let one prompt's arrival confirm
    // another's notes.
    let holders: Vec<String> = store
        .notes_with_marker(&marker)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|n| n.id)
        .collect();
    if !holders.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "marker_in_use",
            format!(
                "marker {marker} already identifies an earlier send; compose with a fresh marker"
            ),
        )
        .with_notes(holders));
    }
    // Every edge is validated BEFORE the write, so a send either delivers with
    // all of its notes legal or delivers nothing.
    let at = now_iso();
    let mut submitted = Vec::with_capacity(notes.len());
    for note in &notes {
        let next = transition(
            note,
            NoteEvent::Submit {
                marker: marker.clone(),
                at: at.clone(),
                target: req.target.clone(),
            },
        )
        .map_err(|r| ApiError::refused(&note.id, r))?;
        submitted.push(next);
    }

    let delivered = door
        .deliver(&req.target, &req.text, mode)
        .await
        .map_err(|e| match e {
            DeliveryError::TargetNotFound(m) => {
                ApiError::new(StatusCode::NOT_FOUND, "target_not_found", m)
            }
            DeliveryError::Unsupported(m) => ApiError::invalid("mode_unsupported", m),
            DeliveryError::Failed(m) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "delivery_failed",
                format!("{m} — nothing was written; the notes stay attached"),
            ),
            DeliveryError::Partial(m) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "delivery_partial",
                format!(
                    "{m} — the text may already be in the session's input box, unsent; \
                     the notes stay attached"
                ),
            ),
        })?;

    if mode == SendMode::Insert {
        return Ok(SendOutcome {
            session_id: session_id.to_string(),
            marker,
            submitted: false,
            sanitized: delivered.sanitized,
            bytes: delivered.bytes,
            notes,
        });
    }

    let mut unrecorded = Vec::new();
    for next in &submitted {
        match store.replace_note_if(next, NoteState::Attached).await {
            Ok(true) => {}
            Ok(false) => unrecorded.push(next.id.clone()),
            Err(e) => {
                warn!(
                    "session review: recording note {} submitted failed: {}",
                    next.id, e
                );
                unrecorded.push(next.id.clone());
            }
        }
    }
    if !unrecorded.is_empty() {
        // Not a failed send: the text went out. A client that read this as
        // one would send the same notes twice.
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "delivered_unrecorded",
            format!(
                "the prompt WAS delivered (marker {marker}) but these notes could not be \
                 recorded submitted — do not send them again"
            ),
        )
        .with_notes(unrecorded));
    }

    let mut notes = submitted;
    let mut refresh = false;
    if sightings.seen(&marker) {
        match confirm_marker(store, &marker, &now_iso()).await {
            Ok(_) => refresh = true,
            Err(e) => warn!("session review: confirming marker {} failed: {}", marker, e),
        }
    }
    // The exit-vs-send race: a target that exited between the delivery and the
    // `submitted` write ran its exit hook BEFORE that write, found nothing to
    // settle, and will not run again. Re-checked here, after the write, so one
    // of the two always sees the other.
    if !door.target_live(&req.target) {
        match settle_target_exit(store, &req.target).await {
            Ok(_) => refresh = true,
            Err(e) => warn!(
                "session review: settling notes for exited {} {} failed: {}",
                req.target.kind(),
                req.target.id(),
                e
            ),
        }
    }
    if refresh {
        for n in &mut notes {
            if let Ok(Some(fresh)) = store.get_note(&n.id).await {
                *n = fresh;
            }
        }
    }
    info!(
        session_id = %session_id,
        marker = %marker,
        notes = notes.len(),
        target = %req.target.kind(),
        "session review: notes submitted"
    );
    Ok(SendOutcome {
        session_id: session_id.to_string(),
        marker,
        submitted: true,
        sanitized: delivered.sanitized,
        bytes: delivered.bytes,
        notes,
    })
}

/// A note's comment as `composeReviewPrompt` writes it into the text:
/// trimmed (JS `String.prototype.trim`, which also strips U+FEFF), continuation
/// lines indented three spaces.
fn composed_comment(body: &str) -> String {
    body.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .split('\n')
        .collect::<Vec<_>>()
        .join("\n   ")
}

/// `submitted | unknown → confirmed` for every note carrying `marker`.
/// Returns the sessions whose notes changed.
pub async fn confirm_marker(
    store: &dyn ReviewStore,
    marker: &str,
    at: &str,
) -> Result<BTreeSet<String>, String> {
    let mut changed = BTreeSet::new();
    for note in store.notes_with_marker(marker).await? {
        if !NoteEventKind::Confirm.legal_from().contains(&note.state) {
            continue;
        }
        let event = NoteEvent::Confirm {
            marker: marker.to_string(),
            at: at.to_string(),
        };
        if let Ok(next) = transition(&note, event) {
            if store.replace_note_if(&next, note.state).await? {
                changed.insert(next.session_id);
            }
        }
    }
    Ok(changed)
}

/// An operator prompt was observed: record its marker's sighting, then
/// confirm the notes it carried. Returns the sessions whose notes changed.
pub async fn observe_operator_prompt(
    store: &dyn ReviewStore,
    sightings: &MarkerSightings,
    prompt_text: &str,
) -> Result<BTreeSet<String>, String> {
    let Some(marker) = parse_marker(prompt_text) else {
        return Ok(BTreeSet::new());
    };
    sightings.record(&marker);
    confirm_marker(store, &marker, &now_iso()).await
}

/// `submitted → unknown` for every note sent to `target`, which can no longer
/// receive it. Returns the sessions whose notes changed.
pub async fn settle_target_exit(
    store: &dyn ReviewStore,
    target: &NoteTarget,
) -> Result<BTreeSet<String>, String> {
    let mut changed = BTreeSet::new();
    for note in store.submitted_notes_for_target(target).await? {
        if let Ok(next) = transition(&note, NoteEvent::SessionEnded) {
            if store.replace_note_if(&next, NoteState::Submitted).await? {
                changed.insert(next.session_id);
            }
        }
    }
    Ok(changed)
}

/// `submitted → unknown` for every `submitted` note whose target `is_live`
/// says is gone — the boot sweep's body, over the store and a liveness seam.
/// Returns the sessions whose notes changed.
pub async fn settle_stranded(
    store: &dyn ReviewStore,
    is_live: &(dyn Fn(&NoteTarget) -> bool + Send + Sync),
) -> Result<BTreeSet<String>, String> {
    let mut changed = BTreeSet::new();
    for note in store.submitted_notes().await? {
        // A `submitted` note always records its target; one that somehow does
        // not has nowhere it could still arrive.
        if note.target.as_ref().is_some_and(is_live) {
            continue;
        }
        if let Ok(next) = transition(&note, NoteEvent::SessionEnded) {
            if store.replace_note_if(&next, NoteState::Submitted).await? {
                changed.insert(next.session_id);
            }
        }
    }
    Ok(changed)
}

// ===========================================================================
// Runtime hooks
// ===========================================================================

/// The UI refresh trigger: `session-review-changed` `{ "sessionId": … }`, on
/// the Tauri event bus and the shared broadcast channel.
pub fn emit_review_changed(app: &tauri::AppHandle, session_id: &str) {
    use tauri::{Emitter, Manager};
    if let Err(e) = app.emit("session-review-changed", json!({ "sessionId": session_id })) {
        warn!("session review: emit for {} failed: {}", session_id, e);
    }
    if let Some(app_state) = app.try_state::<Arc<crate::commands::AppState>>() {
        let _ = app_state.event_broadcast.send(json!({
            "type": "session-review-changed",
            "sessionId": session_id,
        }));
    }
}

/// Per-line hook for the transcript tail. Cheap for every line that cannot be
/// a confirmation: no JSON parse unless the raw line carries `[review `.
pub async fn observe_transcript_line(app: &tauri::AppHandle, store: &dyn ReviewStore, line: &str) {
    if !line.contains("[review ") {
        return;
    }
    let Some(prompt) = crate::terminal::transcript::operator_prompt_text_of_line(line) else {
        return;
    };
    match observe_operator_prompt(store, &SIGHTINGS, &prompt).await {
        Ok(sessions) => {
            for session_id in sessions {
                info!(session_id = %session_id, "session review: notes confirmed from the transcript");
                emit_review_changed(app, &session_id);
            }
        }
        Err(e) => warn!("session review: transcript confirmation failed: {}", e),
    }
}

/// Exit hook for a terminal's waiter thread: settle the notes sent into it
/// that were never seen arriving. Fire-and-forget; needs no caller runtime.
pub fn settle_on_terminal_exit(app: tauri::AppHandle, terminal_id: String) {
    spawn_settle(app, NoteTarget::TerminalId(terminal_id), "terminal exit");
}

/// End hook for a stream-json task-run worker's process-exit waiter
/// (`claude_session/session.rs`): its transcript is never tailed, so without
/// this a note sent to it would sit `submitted` forever. Settles to `unknown`
/// only when no live session holds the task-run id any more — a rate-limit
/// auto-restart re-registers a successor under the same id BEFORE the old
/// process's waiter gets here, and that successor can still receive the turn.
/// Fire-and-forget; needs no caller runtime.
pub fn settle_on_task_run_end(app: tauri::AppHandle, task_run_id: String) {
    use tauri::Manager;
    let successor_live = app
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .and_then(|sm| sm.get(&task_run_id))
        .is_some_and(|session| session.state().is_active());
    if !task_run_end_settles(successor_live) {
        info!(
            task_run_id = %task_run_id,
            "session review: worker process ended but a live successor holds the task run; notes left submitted"
        );
        return;
    }
    spawn_settle(app, NoteTarget::TaskRunId(task_run_id), "task-run end");
}

/// Whether a task-run worker's end settles its notes: only when no live
/// session took over the id.
fn task_run_end_settles(successor_live: bool) -> bool {
    !successor_live
}

/// How long the boot sweep waits before its first attempt, and between
/// attempts when the store is not reachable yet.
const STRANDED_SWEEP_DELAY: Duration = Duration::from_secs(15);
/// Attempts before the boot sweep gives up (the store never came up).
const STRANDED_SWEEP_ATTEMPTS: u32 = 8;

/// The boot sweep: once, settle every `submitted` note whose target is not
/// live in THIS process to `unknown`.
///
/// A runner restart loses every exit hook a note was waiting on: the terminal
/// it was sent into, and the stream-json worker, died with the old process,
/// and nothing will ever fire for them. Safe at any point after the terminal
/// and session managers are `.manage()`d — which is before Tauri's `setup`
/// runs, where this is started — because it never relies on those managers
/// being POPULATED: every terminal id is a fresh UUIDv4 per spawn
/// (`TerminalManager::create` / `create_with_io`) and nothing re-registers a
/// pre-restart terminal id, so a note's terminal is either live in this process
/// or gone for good. A note THIS process sent to a target that is still live is
/// left alone, and one to a target that has since exited is the exit hook's —
/// settling it here too is the same compare-and-set, so the two cannot disagree.
/// `unknown` is not terminal: a later marker sighting still confirms it.
pub fn start_stranded_note_sweep(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        for attempt in 1..=STRANDED_SWEEP_ATTEMPTS {
            tokio::time::sleep(STRANDED_SWEEP_DELAY).await;
            let Some(pg) = crate::database::pg::PgDb::try_global() else {
                continue;
            };
            let probe = app.clone();
            let is_live = move |target: &NoteTarget| target_is_live(&probe, target);
            match settle_stranded(&*pg, &is_live).await {
                Ok(sessions) => {
                    info!(
                        sessions = sessions.len(),
                        "session review: boot sweep settled stranded submitted notes"
                    );
                    for session_id in sessions {
                        emit_review_changed(&app, &session_id);
                    }
                    return;
                }
                Err(e) => warn!(
                    "session review: boot sweep attempt {}/{} failed: {}",
                    attempt, STRANDED_SWEEP_ATTEMPTS, e
                ),
            }
        }
        warn!("session review: boot sweep gave up; stranded submitted notes stay submitted");
    });
}

/// Settle `target`'s still-`submitted` notes to `unknown` off-thread, then
/// emit `session-review-changed` for each session that changed.
fn spawn_settle(app: tauri::AppHandle, target: NoteTarget, cause: &'static str) {
    let Some(pg) = crate::database::pg::PgDb::try_global() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        match settle_target_exit(&*pg, &target).await {
            Ok(sessions) => {
                for session_id in sessions {
                    info!(
                        session_id = %session_id,
                        target_kind = target.kind(),
                        target_id = %target.id(),
                        cause,
                        "session review: unconfirmed notes settled to unknown"
                    );
                    emit_review_changed(&app, &session_id);
                }
            }
            Err(e) => warn!(
                "session review: settling notes for {} {} ({}) failed: {}",
                target.kind(),
                target.id(),
                cause,
                e
            ),
        }
    });
}

// ===========================================================================
// Handlers
// ===========================================================================

type Reply<T> = Result<Json<T>, ApiError>;

async fn get_review_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Reply<ReviewState> {
    read_review(&*state.app_state.pg_db, &session_id)
        .await
        .map(Json)
}

async fn mark_hunks_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<MarkHunksRequest>,
) -> Reply<MarkHunksResponse> {
    let out = mark_hunks(&*state.app_state.pg_db, &session_id, req).await?;
    if out.changed > 0 {
        emit_review_changed(&state.app_handle, &session_id);
    }
    Ok(Json(out))
}

async fn create_note_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<CreateNoteRequest>,
) -> Result<(StatusCode, Json<ReviewNote>), ApiError> {
    let note = create_note(&*state.app_state.pg_db, &session_id, req).await?;
    emit_review_changed(&state.app_handle, &session_id);
    Ok((StatusCode::CREATED, Json(note)))
}

async fn patch_note_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath((session_id, note_id)): AxumPath<(String, String)>,
    Json(req): Json<PatchNoteRequest>,
) -> Reply<ReviewNote> {
    let note = apply_client_event(&*state.app_state.pg_db, &session_id, &note_id, req).await?;
    emit_review_changed(&state.app_handle, &session_id);
    Ok(Json(note))
}

async fn discard_note_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath((session_id, note_id)): AxumPath<(String, String)>,
) -> Reply<ReviewNote> {
    let req = PatchNoteRequest {
        event_type: "discard".to_string(),
        body: None,
    };
    let note = apply_client_event(&*state.app_state.pg_db, &session_id, &note_id, req).await?;
    emit_review_changed(&state.app_handle, &session_id);
    Ok(Json(note))
}

async fn deliver_handler(
    state: Arc<ApiState>,
    session_id: String,
    req: SendRequest,
    mode: SendMode,
) -> Reply<SendOutcome> {
    let door = LiveDoor {
        state: state.clone(),
    };
    let out = send(
        &*state.app_state.pg_db,
        &door,
        &SIGHTINGS,
        &session_id,
        req,
        mode,
    )
    .await?;
    if out.submitted {
        emit_review_changed(&state.app_handle, &session_id);
    }
    Ok(Json(out))
}

async fn send_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<SendRequest>,
) -> Reply<SendOutcome> {
    deliver_handler(state, session_id, req, SendMode::Submit).await
}

async fn insert_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<SendRequest>,
) -> Reply<SendOutcome> {
    deliver_handler(state, session_id, req, SendMode::Insert).await
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Avoid the v0.7-syntax false positive on `{name}` captures, mirroring
        // `mcp/snapshots.rs::routes`.
        .without_v07_checks()
        .route("/sessions/{session_id}/review", get(get_review_handler))
        .route(
            "/sessions/{session_id}/review/hunks",
            put(mark_hunks_handler),
        )
        .route(
            "/sessions/{session_id}/review/notes",
            post(create_note_handler),
        )
        .route(
            "/sessions/{session_id}/review/notes/{note_id}",
            patch(patch_note_handler).delete(discard_note_handler),
        )
        .route("/sessions/{session_id}/review/send", post(send_handler))
        .route("/sessions/{session_id}/review/insert", post(insert_handler))
}

/// Every `(method, path)` [`routes`] registers — pinned against `routes()` and
/// against the origin guard's door list by the tests below.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/sessions/{session_id}/review"),
        ("PUT", "/sessions/{session_id}/review/hunks"),
        ("POST", "/sessions/{session_id}/review/notes"),
        ("PATCH", "/sessions/{session_id}/review/notes/{note_id}"),
        ("DELETE", "/sessions/{session_id}/review/notes/{note_id}"),
        ("POST", "/sessions/{session_id}/review/send"),
        ("POST", "/sessions/{session_id}/review/insert"),
    ]
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The production store's contract, in memory.
    #[derive(Default)]
    struct MemoryStore {
        hunks: Mutex<BTreeMap<(String, String), ReadHunk>>,
        notes: Mutex<Vec<ReviewNote>>,
    }

    impl MemoryStore {
        fn note(&self, id: &str) -> ReviewNote {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .find(|n| n.id == id)
                .cloned()
                .expect("note exists")
        }
    }

    #[async_trait]
    impl ReviewStore for MemoryStore {
        async fn read_hunks(&self, session_id: &str) -> Result<Vec<ReadHunk>, String> {
            Ok(self
                .hunks
                .lock()
                .unwrap()
                .iter()
                .filter(|((s, _), _)| s == session_id)
                .map(|(_, h)| h.clone())
                .collect())
        }

        async fn set_hunks_read(
            &self,
            session_id: &str,
            hunks: &[HunkRef],
            read: bool,
        ) -> Result<usize, String> {
            let mut map = self.hunks.lock().unwrap();
            let mut changed = 0;
            for h in hunks {
                let key = (session_id.to_string(), h.hunk_key.clone());
                if read {
                    if let std::collections::btree_map::Entry::Vacant(slot) = map.entry(key) {
                        slot.insert(ReadHunk {
                            hunk_key: h.hunk_key.clone(),
                            file_path: h.file_path.clone(),
                            read_at: now_iso(),
                        });
                        changed += 1;
                    }
                } else if map.remove(&key).is_some() {
                    changed += 1;
                }
            }
            Ok(changed)
        }

        async fn list_notes(&self, session_id: &str) -> Result<Vec<ReviewNote>, String> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.session_id == session_id)
                .cloned()
                .collect())
        }

        async fn get_note(&self, note_id: &str) -> Result<Option<ReviewNote>, String> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .find(|n| n.id == note_id)
                .cloned())
        }

        async fn insert_note(&self, note: &ReviewNote) -> Result<(), String> {
            self.notes.lock().unwrap().push(note.clone());
            Ok(())
        }

        async fn replace_note_if(
            &self,
            note: &ReviewNote,
            expected: NoteState,
        ) -> Result<bool, String> {
            let mut notes = self.notes.lock().unwrap();
            match notes
                .iter_mut()
                .find(|n| n.id == note.id && n.state == expected)
            {
                Some(slot) => {
                    *slot = note.clone();
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        async fn notes_with_marker(&self, marker: &str) -> Result<Vec<ReviewNote>, String> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.marker.as_deref() == Some(marker))
                .cloned()
                .collect())
        }

        async fn submitted_notes(&self) -> Result<Vec<ReviewNote>, String> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.state == NoteState::Submitted)
                .cloned()
                .collect())
        }

        async fn submitted_notes_for_target(
            &self,
            target: &NoteTarget,
        ) -> Result<Vec<ReviewNote>, String> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.state == NoteState::Submitted && n.target.as_ref() == Some(target))
                .cloned()
                .collect())
        }
    }

    /// A door that answers with a fixed result and records every call.
    struct ScriptedDoor {
        result: Result<Delivered, DeliveryError>,
        calls: Mutex<Vec<(NoteTarget, String, SendMode)>>,
        /// What `target_live` answers — the target exiting mid-send is a
        /// `false` here.
        live: bool,
    }

    impl ScriptedDoor {
        fn ok() -> Self {
            Self::answering(Ok(Delivered {
                sanitized: Some(false),
                bytes: Some(42),
            }))
        }

        fn answering(result: Result<Delivered, DeliveryError>) -> Self {
            Self {
                result,
                calls: Mutex::new(Vec::new()),
                live: true,
            }
        }

        /// Delivers, but the target is gone by the time the send re-checks.
        fn ok_then_exited() -> Self {
            Self {
                live: false,
                ..Self::ok()
            }
        }

        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl PromptDoor for ScriptedDoor {
        async fn deliver(
            &self,
            target: &NoteTarget,
            text: &str,
            mode: SendMode,
        ) -> Result<Delivered, DeliveryError> {
            self.calls
                .lock()
                .unwrap()
                .push((target.clone(), text.to_string(), mode));
            self.result.clone()
        }

        fn target_live(&self, _target: &NoteTarget) -> bool {
            self.live
        }
    }

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";
    const MARKER: &str = "0a1b2c3d";

    fn terminal() -> NoteTarget {
        NoteTarget::TerminalId("term-1".to_string())
    }

    fn text() -> String {
        text_with(MARKER)
    }

    /// A composed text carrying `marker` and the test notes' comment, in
    /// `composeReviewPrompt`'s form.
    fn text_with(marker: &str) -> String {
        format!(
            "[review {marker}]\n\nReview notes on this session's changes (1 note):\n\n\
             1. src/lib.rs @@ -1,2 +1,3 @@\n```diff\n+added\n```\nComment: why this?"
        )
    }

    fn patch(event: &str) -> PatchNoteRequest {
        PatchNoteRequest {
            event_type: event.to_string(),
            body: None,
        }
    }

    async fn new_note(store: &MemoryStore, session: &str) -> ReviewNote {
        create_note(
            store,
            session,
            CreateNoteRequest {
                file_path: "src/lib.rs".to_string(),
                hunk_key: format!("{}-0", "ab".repeat(32)),
                hunk_header: "@@ -1,2 +1,3 @@".to_string(),
                excerpt: "+added".to_string(),
                body: "why this?".to_string(),
            },
        )
        .await
        .expect("create note")
    }

    async fn attached_note(store: &MemoryStore, session: &str) -> ReviewNote {
        let note = new_note(store, session).await;
        apply_client_event(store, session, &note.id, patch("attach"))
            .await
            .expect("attach")
    }

    fn send_req(ids: &[&str]) -> SendRequest {
        SendRequest {
            target: terminal(),
            note_ids: ids.iter().map(|s| s.to_string()).collect(),
            text: text(),
        }
    }

    fn note_in(state: NoteState) -> ReviewNote {
        ReviewNote {
            id: "n1".to_string(),
            session_id: SESSION.to_string(),
            file_path: "a.rs".to_string(),
            hunk_key: "k-0".to_string(),
            hunk_header: String::new(),
            excerpt: String::new(),
            body: "b".to_string(),
            state,
            marker: Some(MARKER.to_string()),
            created_at: now_iso(),
            submitted_at: None,
            confirmed_at: None,
            target: None,
        }
    }

    fn event_of(kind: NoteEventKind) -> NoteEvent {
        match kind {
            NoteEventKind::Attach => NoteEvent::Attach,
            NoteEventKind::Detach => NoteEvent::Detach,
            NoteEventKind::Discard => NoteEvent::Discard,
            NoteEventKind::Edit => NoteEvent::Edit {
                body: "x".to_string(),
            },
            NoteEventKind::Submit => NoteEvent::Submit {
                marker: MARKER.to_string(),
                at: now_iso(),
                target: terminal(),
            },
            NoteEventKind::Confirm => NoteEvent::Confirm {
                marker: MARKER.to_string(),
                at: now_iso(),
            },
            NoteEventKind::SessionEnded => NoteEvent::SessionEnded,
        }
    }

    const ALL_EVENTS: [NoteEventKind; 7] = [
        NoteEventKind::Attach,
        NoteEventKind::Detach,
        NoteEventKind::Discard,
        NoteEventKind::Edit,
        NoteEventKind::Submit,
        NoteEventKind::Confirm,
        NoteEventKind::SessionEnded,
    ];

    // ---- the lifecycle -------------------------------------------------

    /// The full edge table, spelled out literally rather than derived from
    /// `legal_from` — the TS reducer's `LEGAL_FROM`, which this must mirror.
    #[test]
    fn session_review_legal_edges_match_the_ts_reducer() {
        use NoteEventKind as E;
        use NoteState as S;
        let legal: &[(S, E, S)] = &[
            (S::Pending, E::Attach, S::Attached),
            (S::Attached, E::Detach, S::Pending),
            (S::Pending, E::Discard, S::Discarded),
            (S::Attached, E::Discard, S::Discarded),
            (S::Pending, E::Edit, S::Pending),
            (S::Attached, E::Edit, S::Attached),
            (S::Attached, E::Submit, S::Submitted),
            (S::Submitted, E::Confirm, S::Confirmed),
            (S::Unknown, E::Confirm, S::Confirmed),
            (S::Submitted, E::SessionEnded, S::Unknown),
        ];
        for from in NoteState::ALL {
            for event in ALL_EVENTS {
                let result = transition(&note_in(from), event_of(event));
                match legal.iter().find(|(f, e, _)| *f == from && *e == event) {
                    Some((_, _, to)) => {
                        assert_eq!(result.map(|n| n.state), Ok(*to), "{from:?} --{event:?}-->")
                    }
                    None => {
                        let Err(refusal) = result else {
                            panic!("{from:?} --{event:?}--> must be refused");
                        };
                        assert_eq!((refusal.from, refusal.event), (from, event));
                    }
                }
            }
        }
    }

    #[test]
    fn session_review_pending_cannot_jump_to_submitted() {
        let refusal = transition(
            &note_in(NoteState::Pending),
            event_of(NoteEventKind::Submit),
        )
        .expect_err("pending → submitted is not an edge");
        assert!(
            refusal.reason.contains("legal from: attached"),
            "{refusal:?}"
        );
    }

    #[test]
    fn session_review_submit_rejects_a_malformed_marker_and_confirm_a_foreign_one() {
        let bad = NoteEvent::Submit {
            marker: "ABCDEF12".to_string(),
            at: now_iso(),
            target: terminal(),
        };
        assert!(transition(&note_in(NoteState::Attached), bad).is_err());
        let foreign = NoteEvent::Confirm {
            marker: "ffffffff".to_string(),
            at: now_iso(),
        };
        assert!(transition(&note_in(NoteState::Submitted), foreign).is_err());
    }

    #[test]
    fn session_review_marker_parse_matches_the_ts_rule() {
        assert_eq!(parse_marker("[review 0a1b2c3d]"), Some(MARKER.to_string()));
        assert_eq!(
            parse_marker("prefix [review nothex!] then [review 0a1b2c3d] tail"),
            Some(MARKER.to_string()),
            "matched anywhere, first valid occurrence"
        );
        assert_eq!(parse_marker("[review 0A1B2C3D]"), None, "lowercase only");
        assert_eq!(parse_marker("[review 0a1b2c3]"), None, "exactly eight");
        assert_eq!(parse_marker("[review 0a1b2c3d"), None, "closing bracket");
        assert_eq!(parse_marker("é[review 0a1b2c3d]"), Some(MARKER.to_string()));
        assert_eq!(parse_marker("[review é"), None);
    }

    // ---- client events over the store ---------------------------------

    #[tokio::test]
    async fn session_review_client_events_are_revalidated_and_typed() {
        let store = MemoryStore::default();
        let note = new_note(&store, SESSION).await;
        assert_eq!(note.state, NoteState::Pending);

        // detach out of pending is not an edge: a typed 409 naming from/event.
        let err = apply_client_event(&store, SESSION, &note.id, patch("detach"))
            .await
            .expect_err("pending has no detach");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.code, "illegal_transition");
        assert_eq!(err.from, Some(NoteState::Pending));
        assert_eq!(err.event, Some(NoteEventKind::Detach));

        // A client may never assert a server edge.
        for server_edge in ["submit", "confirm", "sessionEnded"] {
            let err = apply_client_event(&store, SESSION, &note.id, patch(server_edge))
                .await
                .expect_err(server_edge);
            assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(err.code, "event_not_client_settable");
        }

        let edited = apply_client_event(
            &store,
            SESSION,
            &note.id,
            PatchNoteRequest {
                event_type: "edit".to_string(),
                body: Some("clearer".to_string()),
            },
        )
        .await
        .expect("edit");
        assert_eq!(
            (edited.body.as_str(), edited.state),
            ("clearer", NoteState::Pending)
        );

        let discarded = apply_client_event(&store, SESSION, &note.id, patch("discard"))
            .await
            .expect("discard");
        assert_eq!(discarded.state, NoteState::Discarded);
        let err = apply_client_event(&store, SESSION, &note.id, patch("attach"))
            .await
            .expect_err("discarded is final");
        assert_eq!(err.from, Some(NoteState::Discarded));
        assert_eq!(store.note(&note.id).state, NoteState::Discarded);
    }

    #[tokio::test]
    async fn session_review_wrong_session_note_is_refused() {
        let store = MemoryStore::default();
        let theirs = attached_note(&store, "other-session").await;

        let err = apply_client_event(&store, SESSION, &theirs.id, patch("detach"))
            .await
            .expect_err("a note of another session");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "note_not_found");

        let door = ScriptedDoor::ok();
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&theirs.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("sending another session's note");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code, "note_not_in_session");
        assert_eq!(err.note_ids, vec![theirs.id.clone()]);
        assert_eq!(door.calls(), 0, "nothing is delivered on a refusal");
        assert_eq!(store.note(&theirs.id).state, NoteState::Attached);
    }

    #[tokio::test]
    async fn session_review_mark_read_is_idempotent() {
        let store = MemoryStore::default();
        let req = |read| MarkHunksRequest {
            hunks: vec![
                HunkRef {
                    hunk_key: "k1-0".to_string(),
                    file_path: "a.rs".to_string(),
                },
                HunkRef {
                    hunk_key: "k1-1".to_string(),
                    file_path: "a.rs".to_string(),
                },
            ],
            read,
        };
        assert_eq!(
            mark_hunks(&store, SESSION, req(true))
                .await
                .unwrap()
                .changed,
            2
        );
        let first_read_at = read_review(&store, SESSION).await.unwrap().read_hunks;
        assert_eq!(
            mark_hunks(&store, SESSION, req(true))
                .await
                .unwrap()
                .changed,
            0
        );
        assert_eq!(
            read_review(&store, SESSION).await.unwrap().read_hunks,
            first_read_at,
            "a repeat keeps the first read_at"
        );
        assert_eq!(
            mark_hunks(&store, SESSION, req(false))
                .await
                .unwrap()
                .changed,
            2
        );
        assert_eq!(
            mark_hunks(&store, SESSION, req(false))
                .await
                .unwrap()
                .changed,
            0
        );
        assert!(read_review(&store, SESSION)
            .await
            .unwrap()
            .read_hunks
            .is_empty());
    }

    // ---- the send door ---------------------------------------------------

    #[tokio::test]
    async fn session_review_send_submits_only_after_the_write_returns() {
        let store = MemoryStore::default();
        let a = attached_note(&store, SESSION).await;
        let b = attached_note(&store, SESSION).await;
        let door = ScriptedDoor::ok();
        let out = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&a.id, &b.id, &a.id]),
            SendMode::Submit,
        )
        .await
        .expect("send");
        assert_eq!(door.calls(), 1);
        assert_eq!(out.marker, MARKER);
        assert!(out.submitted);
        assert_eq!(out.sanitized, Some(false));
        assert_eq!(out.notes.len(), 2, "a repeated id is one note");
        for id in [&a.id, &b.id] {
            let n = store.note(id);
            assert_eq!(n.state, NoteState::Submitted);
            assert_eq!(n.marker.as_deref(), Some(MARKER));
            assert_eq!(n.target, Some(terminal()));
            assert!(n.submitted_at.is_some());
        }
    }

    #[tokio::test]
    async fn session_review_send_to_dead_terminal_leaves_notes_attached() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        // What `submit_prompt`'s liveness gate returns for an exited PTY.
        let door = ScriptedDoor::answering(Err(DeliveryError::Failed(format!(
            "{}: terminal term-1 is not writable -- its process exited with code 0.",
            crate::terminal::session::TERMINAL_EXITED
        ))));
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("a dead terminal");
        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(err.code, "delivery_failed");
        assert!(err.message.contains("not writable"), "{err:?}");
        let stored = store.note(&note.id);
        assert_eq!(stored.state, NoteState::Attached);
        assert_eq!(stored.marker, None);

        let gone = ScriptedDoor::answering(Err(DeliveryError::TargetNotFound(
            "terminal not found: term-1".to_string(),
        )));
        let err = send(
            &store,
            &gone,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("a missing terminal");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(store.note(&note.id).state, NoteState::Attached);
    }

    #[tokio::test]
    async fn session_review_send_refuses_unattached_notes_and_markerless_text() {
        let store = MemoryStore::default();
        let pending = new_note(&store, SESSION).await;
        let door = ScriptedDoor::ok();
        let sightings = MarkerSightings::new();
        let err = send(
            &store,
            &door,
            &sightings,
            SESSION,
            send_req(&[&pending.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("pending note");
        assert_eq!(
            (err.status, err.code),
            (StatusCode::CONFLICT, "illegal_transition")
        );
        assert_eq!(err.from, Some(NoteState::Pending));
        assert_eq!(err.event, Some(NoteEventKind::Submit));

        let attached = attached_note(&store, SESSION).await;
        let mut req = send_req(&[&attached.id]);
        req.text = "no marker here".to_string();
        let err = send(&store, &door, &sightings, SESSION, req, SendMode::Submit)
            .await
            .expect_err("markerless");
        assert_eq!(err.code, "marker_missing");

        let err = send(
            &store,
            &door,
            &sightings,
            SESSION,
            send_req(&[]),
            SendMode::Submit,
        )
        .await
        .expect_err("no notes");
        assert_eq!(err.code, "no_notes");
        assert_eq!(door.calls(), 0);
    }

    #[tokio::test]
    async fn session_review_insert_leaves_notes_attached() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let door = ScriptedDoor::ok();
        let out = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Insert,
        )
        .await
        .expect("insert");
        assert!(!out.submitted);
        assert_eq!(door.calls.lock().unwrap()[0].2, SendMode::Insert);
        assert_eq!(store.note(&note.id).state, NoteState::Attached);
    }

    // ---- confirmation and exit ---------------------------------------------

    #[tokio::test]
    async fn session_review_marker_in_a_user_prompt_confirms() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let sightings = MarkerSightings::new();
        send(
            &store,
            &ScriptedDoor::ok(),
            &sightings,
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect("send");

        // A different marker proves nothing about this note.
        let changed = observe_operator_prompt(&store, &sightings, "[review ffffffff] other")
            .await
            .unwrap();
        assert!(changed.is_empty());
        assert_eq!(store.note(&note.id).state, NoteState::Submitted);

        let changed = observe_operator_prompt(&store, &sightings, &text())
            .await
            .unwrap();
        assert_eq!(changed, BTreeSet::from([SESSION.to_string()]));
        let n = store.note(&note.id);
        assert_eq!(n.state, NoteState::Confirmed);
        assert!(n.confirmed_at.is_some());

        // Seeing it again changes nothing.
        assert!(observe_operator_prompt(&store, &sightings, &text())
            .await
            .unwrap()
            .is_empty());
    }

    /// The race the sightings exist for: the transcript shows the marker
    /// BEFORE the send records `submitted`. The send must still confirm.
    #[tokio::test]
    async fn session_review_confirmation_seen_before_submit_is_not_lost() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let sightings = MarkerSightings::new();
        let early = observe_operator_prompt(&store, &sightings, &text())
            .await
            .unwrap();
        assert!(early.is_empty(), "nothing is submitted yet");
        let out = send(
            &store,
            &ScriptedDoor::ok(),
            &sightings,
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect("send");
        assert_eq!(out.notes[0].state, NoteState::Confirmed);
        assert_eq!(store.note(&note.id).state, NoteState::Confirmed);
    }

    #[tokio::test]
    async fn session_review_terminal_exit_settles_submitted_to_unknown() {
        let store = MemoryStore::default();
        let sent = attached_note(&store, SESSION).await;
        let still_attached = attached_note(&store, SESSION).await;
        send(
            &store,
            &ScriptedDoor::ok(),
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&sent.id]),
            SendMode::Submit,
        )
        .await
        .expect("send");

        // Another terminal exiting touches nothing.
        let other = NoteTarget::TerminalId("term-2".to_string());
        assert!(settle_target_exit(&store, &other).await.unwrap().is_empty());

        let changed = settle_target_exit(&store, &terminal()).await.unwrap();
        assert_eq!(changed, BTreeSet::from([SESSION.to_string()]));
        assert_eq!(store.note(&sent.id).state, NoteState::Unknown);
        assert_eq!(store.note(&still_attached.id).state, NoteState::Attached);

        // A late sighting is positive evidence it arrived: unknown → confirmed.
        let promoted = observe_operator_prompt(&store, &MarkerSightings::new(), &text())
            .await
            .unwrap();
        assert_eq!(promoted, BTreeSet::from([SESSION.to_string()]));
        let n = store.note(&sent.id);
        assert_eq!(n.state, NoteState::Confirmed);
        assert!(n.confirmed_at.is_some());
        assert_eq!(store.note(&still_attached.id).state, NoteState::Attached);
    }

    #[tokio::test]
    async fn session_review_task_run_end_settles_submitted_to_unknown() {
        let store = MemoryStore::default();
        let worker = NoteTarget::TaskRunId("run-1".to_string());
        let sent = attached_note(&store, SESSION).await;
        let to_terminal = attached_note(&store, SESSION).await;
        let mut req = send_req(&[&sent.id]);
        req.target = worker.clone();
        send(
            &store,
            &ScriptedDoor::ok(),
            &MarkerSightings::new(),
            SESSION,
            req,
            SendMode::Submit,
        )
        .await
        .expect("send to worker");
        assert_eq!(store.note(&sent.id).state, NoteState::Submitted);
        let mut to_term = send_req(&[&to_terminal.id]);
        to_term.text = text_with("feedf00d");
        send(
            &store,
            &ScriptedDoor::ok(),
            &MarkerSightings::new(),
            SESSION,
            to_term,
            SendMode::Submit,
        )
        .await
        .expect("send to terminal");

        // A different task run ending, or the terminal exiting, leaves the
        // worker's note alone; a terminal id equal to the run id is a
        // different target.
        let other = NoteTarget::TaskRunId("run-2".to_string());
        assert!(settle_target_exit(&store, &other).await.unwrap().is_empty());
        let same_id_terminal = NoteTarget::TerminalId("run-1".to_string());
        assert!(settle_target_exit(&store, &same_id_terminal)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(store.note(&sent.id).state, NoteState::Submitted);

        let changed = settle_target_exit(&store, &worker).await.unwrap();
        assert_eq!(changed, BTreeSet::from([SESSION.to_string()]));
        assert_eq!(store.note(&sent.id).state, NoteState::Unknown);
        assert_eq!(store.note(&to_terminal.id).state, NoteState::Submitted);
        // Idempotent: a second end settles nothing.
        assert!(settle_target_exit(&store, &worker)
            .await
            .unwrap()
            .is_empty());
    }

    // ---- send integrity ------------------------------------------------

    /// Every note recorded `submitted` must be in the text that went out; a
    /// marker identifies one send only.
    #[tokio::test]
    async fn session_review_send_refuses_absent_notes_and_a_reused_marker() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let door = ScriptedDoor::ok();

        let mut req = send_req(&[&note.id]);
        req.text = format!("[review {MARKER}]\n\nsomething else entirely");
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            req,
            SendMode::Submit,
        )
        .await
        .expect_err("the note's comment is not in the text");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code, "note_not_in_text");
        assert_eq!(err.note_ids, vec![note.id.clone()]);
        assert_eq!(door.calls(), 0);
        assert_eq!(store.note(&note.id).state, NoteState::Attached);

        // A multi-line comment is matched in its composed (indented) form.
        let multi = create_note(
            &store,
            SESSION,
            CreateNoteRequest {
                file_path: "src/lib.rs".to_string(),
                hunk_key: format!("{}-1", "cd".repeat(32)),
                hunk_header: "@@ -3 +3 @@".to_string(),
                excerpt: "-x".to_string(),
                body: "  line one\nline two \n".to_string(),
            },
        )
        .await
        .expect("create");
        apply_client_event(&store, SESSION, &multi.id, patch("attach"))
            .await
            .expect("attach");
        let mut req = send_req(&[&note.id, &multi.id]);
        req.text = format!(
            "{}\n\n2. src/lib.rs @@ -3 +3 @@\nComment: line one\n   line two",
            text()
        );
        send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            req,
            SendMode::Submit,
        )
        .await
        .expect("both comments are in the text");

        // The marker now identifies that send: a second send reusing it is a
        // conflict, naming the notes that already carry it.
        let again = attached_note(&store, SESSION).await;
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&again.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("marker reuse");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.code, "marker_in_use");
        assert_eq!(err.note_ids.len(), 2);
        assert_eq!(store.note(&again.id).state, NoteState::Attached);
    }

    /// A paste that landed without its CR is a distinct refusal: the text may
    /// be in the input box, and the notes are not recorded sent.
    #[tokio::test]
    async fn session_review_partial_delivery_is_its_own_code() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let door = ScriptedDoor::answering(Err(DeliveryError::Partial(format!(
            "{}: the paste was written but the submit enter failed: broken pipe",
            crate::terminal::session::PROMPT_PARTIALLY_WRITTEN
        ))));
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("a partial write");
        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(err.code, "delivery_partial");
        assert!(err.message.contains("input box"), "{err:?}");
        assert_eq!(store.note(&note.id).state, NoteState::Attached);
    }

    /// A store that refuses every compare-and-set: the prompt goes out and no
    /// note can be recorded.
    struct UnrecordingStore(MemoryStore);

    #[async_trait]
    impl ReviewStore for UnrecordingStore {
        async fn read_hunks(&self, s: &str) -> Result<Vec<ReadHunk>, String> {
            self.0.read_hunks(s).await
        }
        async fn set_hunks_read(&self, s: &str, h: &[HunkRef], r: bool) -> Result<usize, String> {
            self.0.set_hunks_read(s, h, r).await
        }
        async fn list_notes(&self, s: &str) -> Result<Vec<ReviewNote>, String> {
            self.0.list_notes(s).await
        }
        async fn get_note(&self, id: &str) -> Result<Option<ReviewNote>, String> {
            self.0.get_note(id).await
        }
        async fn insert_note(&self, n: &ReviewNote) -> Result<(), String> {
            self.0.insert_note(n).await
        }
        async fn replace_note_if(&self, _: &ReviewNote, _: NoteState) -> Result<bool, String> {
            Err("database went away".to_string())
        }
        async fn notes_with_marker(&self, m: &str) -> Result<Vec<ReviewNote>, String> {
            self.0.notes_with_marker(m).await
        }
        async fn submitted_notes(&self) -> Result<Vec<ReviewNote>, String> {
            self.0.submitted_notes().await
        }
        async fn submitted_notes_for_target(
            &self,
            t: &NoteTarget,
        ) -> Result<Vec<ReviewNote>, String> {
            self.0.submitted_notes_for_target(t).await
        }
    }

    /// Delivered but not recorded is NOT a failed send: its own code, so the
    /// page renders it as delivered and does not invite a second send.
    #[tokio::test]
    async fn session_review_delivered_but_unrecorded_is_not_a_failed_send() {
        let inner = MemoryStore::default();
        let note = attached_note(&inner, SESSION).await;
        let store = UnrecordingStore(inner);
        let door = ScriptedDoor::ok();
        let err = send(
            &store,
            &door,
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect_err("recording fails");
        assert_eq!(door.calls(), 1, "the prompt went out");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code, "delivered_unrecorded");
        assert_eq!(err.note_ids, vec![note.id.clone()]);
        assert!(err.message.contains("do not send them again"), "{err:?}");
    }

    // ---- stranded `submitted` notes --------------------------------------

    /// The target exits between the delivery and the `submitted` write: its
    /// exit hook already ran and found nothing, so the send settles in-line.
    #[tokio::test]
    async fn session_review_a_target_that_exits_mid_send_is_settled_by_the_send() {
        let store = MemoryStore::default();
        let note = attached_note(&store, SESSION).await;
        let out = send(
            &store,
            &ScriptedDoor::ok_then_exited(),
            &MarkerSightings::new(),
            SESSION,
            send_req(&[&note.id]),
            SendMode::Submit,
        )
        .await
        .expect("delivered");
        assert_eq!(out.notes[0].state, NoteState::Unknown);
        assert_eq!(store.note(&note.id).state, NoteState::Unknown);

        // Same race for a task-run target.
        let worker_note = attached_note(&store, SESSION).await;
        let mut req = send_req(&[&worker_note.id]);
        req.target = NoteTarget::TaskRunId("run-9".to_string());
        req.text = text_with("0badcafe");
        let out = send(
            &store,
            &ScriptedDoor::ok_then_exited(),
            &MarkerSightings::new(),
            SESSION,
            req,
            SendMode::Submit,
        )
        .await
        .expect("delivered to the worker");
        assert_eq!(out.notes[0].state, NoteState::Unknown);

        // A sighting that already happened wins over the exit: confirmed.
        let seen = attached_note(&store, SESSION).await;
        let sightings = MarkerSightings::new();
        let mut req = send_req(&[&seen.id]);
        req.text = text_with("5eed5eed");
        sightings.record("5eed5eed");
        let out = send(
            &store,
            &ScriptedDoor::ok_then_exited(),
            &sightings,
            SESSION,
            req,
            SendMode::Submit,
        )
        .await
        .expect("delivered and seen");
        assert_eq!(out.notes[0].state, NoteState::Confirmed);
    }

    /// The boot sweep settles exactly the `submitted` notes whose target is
    /// not live, and leaves every other note alone.
    #[tokio::test]
    async fn session_review_boot_sweep_settles_only_dead_targets() {
        let store = MemoryStore::default();
        let dead = attached_note(&store, SESSION).await;
        let live = attached_note(&store, SESSION).await;
        let untouched = attached_note(&store, SESSION).await;
        for (note, target, marker) in [
            (
                &dead,
                NoteTarget::TerminalId("old-term".to_string()),
                "000000aa",
            ),
            (
                &live,
                NoteTarget::TerminalId("new-term".to_string()),
                "000000bb",
            ),
        ] {
            let mut req = send_req(&[&note.id]);
            req.target = target;
            req.text = text_with(marker);
            send(
                &store,
                &ScriptedDoor::ok(),
                &MarkerSightings::new(),
                SESSION,
                req,
                SendMode::Submit,
            )
            .await
            .expect("send");
        }
        let is_live = |t: &NoteTarget| t.id() == "new-term";
        let changed = settle_stranded(&store, &is_live).await.unwrap();
        assert_eq!(changed, BTreeSet::from([SESSION.to_string()]));
        assert_eq!(store.note(&dead.id).state, NoteState::Unknown);
        assert_eq!(store.note(&live.id).state, NoteState::Submitted);
        assert_eq!(store.note(&untouched.id).state, NoteState::Attached);
        // Idempotent.
        assert!(settle_stranded(&store, &is_live).await.unwrap().is_empty());
    }

    #[test]
    fn session_review_task_run_end_defers_to_a_live_successor() {
        assert!(task_run_end_settles(false));
        assert!(!task_run_end_settles(true));
    }

    // ---- wiring ------------------------------------------------------------

    #[test]
    fn session_review_wire_shapes_are_camel_case() {
        let note = note_in(NoteState::Submitted);
        let v = serde_json::to_value(&note).unwrap();
        for key in [
            "id",
            "sessionId",
            "filePath",
            "hunkKey",
            "hunkHeader",
            "excerpt",
            "body",
            "state",
            "marker",
            "createdAt",
            "submittedAt",
            "confirmedAt",
            "target",
        ] {
            assert!(v.get(key).is_some(), "missing {key} in {v}");
        }
        assert_eq!(v["state"], "submitted");
        let req: SendRequest = serde_json::from_value(json!({
            "target": { "terminalId": "t" },
            "noteIds": ["a"],
            "text": "x",
        }))
        .unwrap();
        assert_eq!(req.target, NoteTarget::TerminalId("t".to_string()));
        let req: SendRequest = serde_json::from_value(json!({
            "target": { "taskRunId": "r" },
            "noteIds": [],
            "text": "x",
        }))
        .unwrap();
        assert_eq!(req.target, NoteTarget::TaskRunId("r".to_string()));
        assert_eq!(
            serde_json::to_value(NoteTarget::TaskRunId("r".to_string())).unwrap(),
            json!({ "taskRunId": "r" })
        );
    }

    /// Counted off the `routes()` FUNCTION BODY, as
    /// `terminals::route_entries_covers_every_route_registration_in_this_file`
    /// does, so a route added there without an entry here fails.
    #[test]
    fn session_review_route_entries_cover_every_registration() {
        let needle = format!(".{}(", "route");
        let fn_header = format!("pub fn {}() -> Router<Arc<ApiState>> {{", "routes");
        let src = include_str!("session_review.rs");
        let (_, body) = src
            .split_once(fn_header.as_str())
            .expect("routes() signature");
        let (body, _) = body.split_once("\n}").expect("routes() closes at column 0");
        let registered = body.matches(needle.as_str()).count();
        let mut paths: Vec<&str> = route_entries().iter().map(|(_, p)| *p).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), registered);
    }

    /// Every route reads or writes local source content or types into a PTY,
    /// so every one must be an origin-guard door.
    #[test]
    fn session_review_every_route_is_an_origin_guard_door() {
        for (method, path) in route_entries() {
            assert!(
                crate::mcp::origin_guard::is_credential_door(method, path),
                "{method} {path} is not in CREDENTIAL_DOORS"
            );
        }
    }
}
