//! Phantom-turn DETECTOR — notice, from the transcript, a suspiciously short
//! `typed` user turn right after a Claude session starts or resumes, when no
//! real input reached the PTY.
//!
//! **Why this exists.** After a runner rebuild, restored/resumed sessions got a
//! phantom two-character user turn — the literal text `is` — auto-submitted
//! before any real input. It landed 70–160 ms after the SessionStart hook batch
//! was recorded, and up to ~18 s after it when a slow hook stretched that phase.
//! Every runner write path was ruled out (plan
//! `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`); the
//! mis-parse is believed to live inside Claude Code's own hook-batch handling.
//! Proving that took hours of hand-correlating `qontinui-runner.log` against
//! transcripts. This module turns that correlation into one greppable
//! `phantom_turn_detected` warn line.
//!
//! **Detection, not prevention — stated plainly.** The runner is not the
//! injector, so there is nothing in its injection path to refuse. The watcher
//! reads the transcript AFTER the turn was written, and it never intercepts,
//! refuses or alters anything. A fired event means "this turn is shaped like
//! the incident and the runner saw no input that could explain it"; a silent
//! watcher means only that no such turn appeared inside the window.
//!
//! **Shape.** [`detect_phantom_turn`] is pure — JSONL lines plus the terminal's
//! input evidence in, a [`Verdict`] out — so every rule is unit-tested
//! without a PTY or a filesystem. [`SessionOpenWatch`] is the thin
//! bounded poll around it, spawned by `POST /control/session-open`
//! (`install_effects_producer::post_session_open`): the identity shim posts the
//! start signal (before it execs `claude`), and the SessionStart hook posts
//! start/resume.
//!
//! **Input evidence** is Phase 2's per-session slot
//! ([`crate::terminal::session::PtyInputSlots`]): the last `write` and the last
//! `submit_prompt` into that terminal, with their caller tags and instants.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use tracing::{debug, info, warn};

use super::session::{PtyInputObservation, PtyInputSlots};

/// How long after session-open the watcher keeps reading the transcript.
///
/// Sized to the incident, not to the happy path: the phantom turn landed
/// 70–160 ms after the hook batch on an ordinary start, but ~18 s after it when
/// `capability-doctor-boot.sh` timed out and stretched the hook phase — and
/// `/control/session-open` is itself one of those hooks, so it can fire long
/// before the batch ends. 25 s covers the stretched case with margin, while a
/// hard bound keeps one task per session-open from becoming a resident watcher.
/// The plan records these figures as one box on one day; tune against real
/// `phantom_turn_detected` data rather than treating the value as a guarantee.
pub const PHANTOM_TURN_WATCH_WINDOW: Duration = Duration::from_secs(25);

/// How often the watcher reads newly appended transcript bytes. It need not be
/// finer than the 70 ms low end of the observed gap: elapsed time comes from
/// the turn's own `timestamp` when it has one, so the tick only bounds how late
/// the warn is emitted, not how precisely the gap is measured.
pub const PHANTOM_TURN_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A `typed` user turn whose trimmed text is at most this many characters is
/// "implausibly short". The incident's turn was `is` (2). A legitimate
/// three-character first prompt (`hi?`, `yes`) also matches, which is why the
/// no-real-input condition — not length — carries the detector's precision.
pub const PHANTOM_TURN_MAX_CHARS: usize = 3;

/// The `promptSource` value a transcript gives a turn Claude Code believes was
/// typed at its prompt. Real transcripts carry `typed`, `system` (task
/// notifications and similar) and `suggestion_accepted`; only `typed` is the
/// incident's shape.
const PROMPT_SOURCE_TYPED: &str = "typed";

/// How far before session-open a turn's own `timestamp` may be and still be
/// judged. Covers clock granularity and the few ms between Claude Code writing a
/// turn and the runner stamping session-open; anything older is prior
/// conversation — notably the history `--fork-session` copies into a brand-new
/// transcript file, which the append baseline cannot exclude because the file
/// did not exist when the watch started.
pub const PHANTOM_TURN_TIMESTAMP_SLACK: Duration = Duration::from_secs(2);

/// One session-open moment — monotonic (compared against PTY input instants)
/// and wall-clock (compared against transcript timestamps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAnchor {
    pub at: Instant,
    pub wall: SystemTime,
}

/// The longest gap between a session's first open and a later one that still
/// PAIRS them into one watch (the shim's pre-exec post and the SessionStart
/// hook's post for the same launch). A later open past it, or one preceded by
/// PTY input since the first, is a NEW launch in that pane — a quit and
/// `claude --resume` — and gets its own watch and its own evidence anchor.
pub const PHANTOM_TURN_PAIRING_INTERVAL: Duration = Duration::from_secs(30);

/// A watch's timing, split in two because the two uses want DIFFERENT opens:
///
/// - `evidence` is the EARLIEST paired session-open. "Real input since open"
///   and the turn-timestamp filter are judged against it: keystrokes typed
///   after the shim's pre-exec open are real input even if the hook's open
///   lands later, and a phantom stamped between the two opens must not be
///   filtered out as history just because the hook's post was handled late.
/// - `latest_open` is the LATEST paired session-open; the deadline runs from it
///   ([`WatchState::deadline`]), so a later open extends the watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchTiming {
    pub evidence: OpenAnchor,
    pub latest_open: Instant,
}

/// Everything a running watch and the registry share about one watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchState {
    pub timing: WatchTiming,
    /// The watch window the deadline is computed from.
    pub window: Duration,
    /// The watch has decided to stop (a verdict, or its deadline). A later open
    /// must start a new watch rather than extend one that is ending.
    pub ending: bool,
    /// A newer watch replaced this one in the registry (another terminal, or a
    /// new launch in the same pane). The watch stops at its next tick without a
    /// verdict, so it can neither double-report nor blame the wrong terminal.
    pub superseded: bool,
}

impl WatchState {
    /// When the watch ends: the latest paired open plus the window, but never
    /// later than the evidence anchor plus TWICE the window — a stream of
    /// paired opens cannot keep one watch alive indefinitely.
    pub fn deadline(&self) -> Instant {
        std::cmp::min(
            self.timing.latest_open + self.window,
            self.timing.evidence.at + 2 * self.window,
        )
    }
}

/// [`WatchState`] shared between a running watch and the registry.
pub type SharedTiming = std::sync::Arc<std::sync::Mutex<WatchState>>;

/// A fresh [`SharedTiming`] for a watch whose only open so far is `open`, with
/// the default window ([`SessionOpenWatch::new`] sets the watch's own).
pub fn shared_timing(open: OpenAnchor) -> SharedTiming {
    std::sync::Arc::new(std::sync::Mutex::new(WatchState {
        timing: WatchTiming {
            evidence: open,
            latest_open: open.at,
        },
        window: PHANTOM_TURN_WATCH_WINDOW,
        ending: false,
        superseded: false,
    }))
}

fn lock_state(shared: &SharedTiming) -> std::sync::MutexGuard<'_, WatchState> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn read_timing(shared: &SharedTiming) -> WatchTiming {
    lock_state(shared).timing
}

/// One registry entry: the terminal it watches and the state its watch reads.
/// The `Arc` itself is the claim's identity.
struct ActiveWatch {
    terminal_id: String,
    shared: SharedTiming,
}

/// Process-global registry of ACTIVE watches, keyed by session id. One fresh
/// session posts `/control/session-open` twice — the identity shim BEFORE it
/// execs `claude`, the SessionStart hook later, from inside the hook batch —
/// and two watches over one transcript would emit the same detection twice.
static ACTIVE_WATCHES: std::sync::Mutex<Option<std::collections::HashMap<String, ActiveWatch>>> =
    std::sync::Mutex::new(None);

/// What [`WatchClaim::claim_or_extend`] did.
pub enum ClaimOutcome {
    /// No pairable watch: this caller owns a new one (any previous watch for
    /// the session was marked superseded).
    New(WatchClaim),
    /// A live watch for the same session AND terminal paired with this open;
    /// its deadline now runs from it (its evidence anchor stays the earliest
    /// open). The caller starts nothing.
    Extended,
}

/// An active watch's claim on its session id. Dropping it — when the watch
/// task finishes, or unwinds — releases the id, unless a newer watch has
/// replaced it.
pub struct WatchClaim {
    session_id: String,
    shared: SharedTiming,
}

impl WatchClaim {
    /// Claim `session_id` for a watch of `terminal_id` opened at `open`, or
    /// EXTEND the live watch it pairs with. A later open pairs only when ALL of:
    ///
    /// - it is for the same terminal (another terminal means the session moved
    ///   panes, and the old watch's input evidence is the wrong terminal's);
    /// - the existing watch is not ending, not superseded, and its deadline has
    ///   not passed (a watch about to return must not silently absorb it);
    /// - it arrives within [`PHANTOM_TURN_PAIRING_INTERVAL`] of the evidence
    ///   anchor;
    /// - `last_input_at`, the terminal's latest PTY input, is not between the
    ///   two opens (input there means the pane was used between two launches).
    ///
    /// Pairing moves the deadline to this open ([`WatchState::deadline`]) and
    /// keeps the earlier open as the evidence anchor. Anything else starts a new
    /// watch anchored at `open` and marks the old one superseded, so it stops
    /// without a verdict; the old claim's drop then leaves the replacement in
    /// place (`Arc::ptr_eq` in `Drop`).
    pub fn claim_or_extend(
        session_id: &str,
        terminal_id: &str,
        open: OpenAnchor,
        last_input_at: Option<Instant>,
        window: Duration,
    ) -> ClaimOutcome {
        let mut guard = ACTIVE_WATCHES.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(Default::default);
        if let Some(active) = map.get(session_id) {
            let mut state = lock_state(&active.shared);
            let first = state.timing.evidence.at;
            let (lo, hi) = if open.at < first {
                (open.at, first)
            } else {
                (first, open.at)
            };
            let pairs = active.terminal_id == terminal_id
                && !state.ending
                && !state.superseded
                && Instant::now() < state.deadline()
                && hi.saturating_duration_since(lo) <= PHANTOM_TURN_PAIRING_INTERVAL
                && !last_input_at.is_some_and(|t| t > lo && t <= hi);
            if pairs {
                if open.at > state.timing.latest_open {
                    state.timing.latest_open = open.at;
                }
                if open.at < state.timing.evidence.at {
                    state.timing.evidence = open;
                }
                return ClaimOutcome::Extended;
            }
            state.superseded = true;
        }
        let shared = shared_timing(open);
        lock_state(&shared).window = window;
        map.insert(
            session_id.to_string(),
            ActiveWatch {
                terminal_id: terminal_id.to_string(),
                shared: shared.clone(),
            },
        );
        ClaimOutcome::New(Self {
            session_id: session_id.to_string(),
            shared,
        })
    }

    /// The state this claim's watch must read (and later opens extend).
    pub fn timing(&self) -> SharedTiming {
        self.shared.clone()
    }
}

impl Drop for WatchClaim {
    fn drop(&mut self) {
        let mut guard = ACTIVE_WATCHES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = guard.as_mut() {
            // Only this claim's own entry: a replaced claim must not release its
            // successor. Identity is the shared state itself.
            if map
                .get(&self.session_id)
                .is_some_and(|a| std::sync::Arc::ptr_eq(&a.shared, &self.shared))
            {
                map.remove(&self.session_id);
            }
        }
    }
}

/// One user turn read out of a transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserTurn {
    prompt_source: Option<String>,
    /// Trimmed text length in chars.
    text_chars: usize,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Parse one transcript JSONL line as a user TEXT turn, or `None`.
///
/// Not a user text turn: any non-`user` record, a malformed line, an `isMeta`
/// record (caveats and hook output Claude Code stores as user-role metadata),
/// a sidechain (subagent) record, and a user record whose content carries no
/// text at all (a `tool_result` array, an image-only paste).
fn parse_user_turn(line: &str) -> Option<UserTurn> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    let flag = |k: &str| v.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
    if flag("isMeta") || flag("isSidechain") {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text: String = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if texts.is_empty() {
                return None;
            }
            texts.concat()
        }
        _ => return None,
    };
    let text_chars = text.trim().chars().count();
    if text_chars == 0 {
        return None;
    }
    Some(UserTurn {
        prompt_source: v
            .get("promptSource")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        text_chars,
        timestamp: v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
    })
}

/// What the runner knows about input into the session's terminal, as of the
/// moment the lines were read.
#[derive(Debug, Clone, Copy)]
pub struct InputEvidence<'a> {
    /// When `POST /control/session-open` arrived.
    pub session_open_at: Instant,
    /// The same moment on the wall clock, compared against each turn's own
    /// transcript `timestamp` ([`PHANTOM_TURN_TIMESTAMP_SLACK`]).
    pub session_open_wall: chrono::DateTime<chrono::Utc>,
    /// The terminal's latest PTY input event, if any — `None` also when the
    /// terminal could not be resolved (see [`SessionOpenWatch`]).
    pub latest: Option<&'a PtyInputObservation>,
}

impl InputEvidence<'_> {
    /// Did a real input event reach the PTY at or after session-open?
    ///
    /// Input BEFORE session-open does not count, deliberately with no
    /// lookback: a restored pane is resumed by a `claude --resume <id>` line
    /// written into its shell moments before Claude starts, and a lookback
    /// long enough to cover keyboard typeahead would also swallow that write —
    /// masking exactly the population the incident hit. The accepted cost is a
    /// false positive when an operator types a ≤3-char prompt during Claude's
    /// startup; it is a warn line, never an action.
    fn real_input_since_open(&self) -> bool {
        self.latest
            .is_some_and(|obs| obs.at >= self.session_open_at)
    }
}

/// A detected phantom-shaped turn. Carries the length, never the content: the
/// turn text is session data and does not belong in the runner log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantomTurn {
    pub content_chars: usize,
    /// The turn's own transcript timestamp, when it parsed.
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// What [`detect_phantom_turn`] concluded over one batch of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A phantom-shaped turn: short, `typed`, no real input since open.
    Phantom(PhantomTurn),
    /// A `typed` turn was seen and it is not phantom-shaped. The watcher stops:
    /// the incident's turn preceded any real prompt, so once a real typed turn
    /// has landed there is nothing left inside the pattern to watch for.
    ClearTypedTurn,
    /// No `typed` user text turn in these lines; keep watching.
    Undecided,
    /// Returned only by [`SessionOpenWatch::run`], never by
    /// [`detect_phantom_turn`]: a newer watch replaced this one, so it stopped
    /// without judging or emitting anything.
    Superseded,
}

/// The pure detector. Walks `lines` in order and returns on the first `typed`
/// user text turn written after session-open: [`Verdict::Phantom`] when it is
/// at most [`PHANTOM_TURN_MAX_CHARS`] trimmed chars AND `input` shows no real
/// input since session-open, else [`Verdict::ClearTypedTurn`]. Non-`typed`
/// turns (`system`, `suggestion_accepted`, absent) are skipped however short:
/// those sources are not the incident's shape.
///
/// A turn is judged only when its own `timestamp` parses and is no earlier
/// than session-open minus [`PHANTOM_TURN_TIMESTAMP_SLACK`]. An older turn is
/// prior conversation (a `--fork-session` copies the whole history into a new
/// file); an unstamped one cannot be placed in time, and a detector whose
/// precision rests on timing does not guess.
pub fn detect_phantom_turn<'l>(
    lines: impl IntoIterator<Item = &'l str>,
    input: InputEvidence<'_>,
) -> Verdict {
    for line in lines {
        let Some(turn) = parse_user_turn(line) else {
            continue;
        };
        if turn.prompt_source.as_deref() != Some(PROMPT_SOURCE_TYPED) {
            continue;
        }
        let slack = chrono::Duration::from_std(PHANTOM_TURN_TIMESTAMP_SLACK)
            .unwrap_or_else(|_| chrono::Duration::seconds(2));
        if !turn
            .timestamp
            .is_some_and(|ts| ts >= input.session_open_wall - slack)
        {
            continue;
        }
        if turn.text_chars <= PHANTOM_TURN_MAX_CHARS && !input.real_input_since_open() {
            return Verdict::Phantom(PhantomTurn {
                content_chars: turn.text_chars,
                timestamp: turn.timestamp,
            });
        }
        return Verdict::ClearTypedTurn;
    }
    Verdict::Undecided
}

/// Every transcript path this session could be written at: each candidate
/// config dir crossed with each candidate project cwd. More than one because
/// the hook may omit `config_dir` (then every account home on the box is a
/// candidate) and the recorded working dir can differ in spelling from the
/// hook's raw cwd (MSYS normalization on Windows). Deduplicated, order kept.
pub fn candidate_transcript_paths(
    config_dirs: &[PathBuf],
    cwds: &[String],
    session_id: &str,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in config_dirs {
        for cwd in cwds.iter().filter(|c| !c.trim().is_empty()) {
            let p = super::transcript::session_transcript_path(dir, cwd, session_id);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// How far back from a transcript's current end a [`TailStart::RecentTail`]
/// reader starts. Transcripts of long sessions run to many MB; the turn this
/// module looks for landed seconds after session-open, so only the recent end
/// of the file can hold it. 512 KiB is hundreds of ordinary records.
pub const PHANTOM_TURN_TAIL_LOOKBACK_BYTES: u64 = 512 * 1024;

/// Append-only reader over one transcript file: yields only complete lines
/// appended since the last read, starting from the file's length at creation
/// ([`Self::new`]) or [`PHANTOM_TURN_TAIL_LOOKBACK_BYTES`] before it
/// ([`Self::from_recent`]).
///
/// The length baseline matters on `--resume`: the file already holds the whole
/// prior conversation, full of short typed turns that are cheapest never to
/// read. The timestamp filter in [`detect_phantom_turn`] is what makes a
/// look-back read safe when no baseline could be taken in time.
struct AppendTail {
    path: PathBuf,
    offset: u64,
    /// The offset landed mid-file, probably mid-line: discard bytes up to the
    /// first newline on the next read, so no torn record is ever parsed.
    skip_to_line_start: bool,
}

impl AppendTail {
    fn new(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            offset,
            skip_to_line_start: false,
        }
    }

    fn from_recent(path: PathBuf) -> Self {
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let offset = len.saturating_sub(PHANTOM_TURN_TAIL_LOOKBACK_BYTES);
        // The offset is already at a line start exactly when the byte before
        // it is a newline; only otherwise is there a torn first record to skip.
        // A byte that cannot be read is treated as torn (the safe direction).
        let skip_to_line_start = offset > 0 && byte_at(&path, offset - 1) != Some(b'\n');
        Self {
            path,
            offset,
            skip_to_line_start,
        }
    }

    /// New complete lines since the last call. A trailing partial line is left
    /// for the next call (the offset advances only past the last `\n`). A file
    /// shorter than the offset was replaced, so it is re-read from the start.
    fn read_new_lines(&mut self) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.skip_to_line_start = false;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut owned = Vec::new();
        if f.read_to_end(&mut owned).is_err() {
            return Vec::new();
        }
        let mut buf = &owned[..];
        if self.skip_to_line_start {
            let Some(first_nl) = buf.iter().position(|b| *b == b'\n') else {
                return Vec::new();
            };
            self.offset += (first_nl + 1) as u64;
            self.skip_to_line_start = false;
            buf = &buf[first_nl + 1..];
        }
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
            return Vec::new();
        };
        self.offset += (last_nl + 1) as u64;
        String::from_utf8_lossy(&buf[..last_nl])
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// The single byte at `pos` in the file at `path`, if it can be read.
fn byte_at(path: &std::path::Path, pos: u64) -> Option<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(pos)).ok()?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b).ok()?;
    Some(b[0])
}

/// Where each transcript tail starts reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailStart {
    /// At the file's length when the watch is built — prior content is never
    /// read. For a watch built on the session-open route itself.
    CurrentEnd,
    /// [`PHANTOM_TURN_TAIL_LOOKBACK_BYTES`] before the end, at a line start.
    /// For a watch built LATER (after account discovery), where a length
    /// baseline could swallow the very turn it exists to catch; prior
    /// conversation inside the look-back is excluded by the turn-timestamp
    /// filter instead.
    RecentTail,
}

/// One bounded watch after one `POST /control/session-open`.
pub struct SessionOpenWatch {
    pub session_id: String,
    pub terminal_id: String,
    /// One append-only reader per candidate transcript path.
    tails: Vec<AppendTail>,
    /// The shared [`WatchState`]: timing a paired later open extends, and the
    /// supersession flag a replacing watch sets.
    timing: SharedTiming,
    pub poll: Duration,
}

impl SessionOpenWatch {
    /// Build the watch, placing each path's tail per `start`.
    ///
    /// With [`TailStart::CurrentEnd`] the baseline is the one timing-sensitive
    /// step: on `--resume` the file already exists, and anything appended before
    /// the baseline is never judged. The incident's turn landed as little as
    /// 70 ms after the hook batch, so callers take that baseline as close to
    /// session-open as they can — see [`spawn_watch`].
    pub fn new(
        session_id: String,
        terminal_id: String,
        paths: Vec<PathBuf>,
        start: TailStart,
        timing: SharedTiming,
        window: Duration,
        poll: Duration,
    ) -> Self {
        let tail = match start {
            TailStart::CurrentEnd => AppendTail::new,
            TailStart::RecentTail => AppendTail::from_recent,
        };
        lock_state(&timing).window = window;
        Self {
            session_id,
            terminal_id,
            tails: paths.into_iter().map(tail).collect(),
            timing,
            poll,
        }
    }

    /// Poll `paths` every `poll` until [`WatchState::deadline`] — re-read every
    /// tick, so a paired later open extends it — feeding newly appended lines,
    /// the EARLIEST open as the evidence anchor, and a fresh `input_slots()`
    /// snapshot to [`detect_phantom_turn`]. File reads run on the blocking
    /// pool, never on an async worker. Emits at most
    /// once, then stops; stops early on a clear typed turn; otherwise ends
    /// silently at the deadline. A superseded watch returns
    /// [`Verdict::Superseded`] at its next tick without judging or emitting.
    /// Returns the verdict it stopped on, for tests.
    ///
    /// `input_slots` returns `None` when the terminal cannot be resolved
    /// (closed, or not managed by this runner). The no-input half of the
    /// evidence is then UNKNOWN, so such a match is reported as the distinct
    /// info-level `phantom_turn_candidate_unresolved_terminal`, never as the
    /// `phantom_turn_detected` warn ([`detection_event`]).
    pub async fn run(mut self, input_slots: impl Fn() -> Option<PtyInputSlots>) -> Verdict {
        loop {
            let mut tails = std::mem::take(&mut self.tails);
            let Ok((tails, lines)) = tokio::task::spawn_blocking(move || {
                let lines: Vec<String> =
                    tails.iter_mut().flat_map(|t| t.read_new_lines()).collect();
                (tails, lines)
            })
            .await
            else {
                lock_state(&self.timing).ending = true;
                return Verdict::Undecided;
            };
            self.tails = tails;
            let anchor = {
                let state = lock_state(&self.timing);
                if state.superseded {
                    return Verdict::Superseded;
                }
                state.timing.evidence
            };
            if !lines.is_empty() {
                let slots = input_slots();
                let evidence = InputEvidence {
                    session_open_at: anchor.at,
                    session_open_wall: anchor.wall.into(),
                    latest: slots.as_ref().and_then(|s| s.latest()),
                };
                match detect_phantom_turn(lines.iter().map(String::as_str), evidence) {
                    Verdict::Phantom(turn) => {
                        {
                            let mut state = lock_state(&self.timing);
                            if state.superseded {
                                return Verdict::Superseded;
                            }
                            state.ending = true;
                        }
                        self.emit_detection(&turn, slots.as_ref(), anchor);
                        return Verdict::Phantom(turn);
                    }
                    Verdict::ClearTypedTurn => {
                        let mut state = lock_state(&self.timing);
                        if state.superseded {
                            return Verdict::Superseded;
                        }
                        state.ending = true;
                        return Verdict::ClearTypedTurn;
                    }
                    Verdict::Undecided | Verdict::Superseded => {}
                }
            }
            {
                // Decided under the same lock `claim_or_extend` extends under,
                // so an open either lands before this check (and extends) or
                // sees `ending` (and starts a new watch) — never lost between.
                let mut state = lock_state(&self.timing);
                if state.superseded {
                    return Verdict::Superseded;
                }
                if Instant::now() >= state.deadline() {
                    state.ending = true;
                    return Verdict::Undecided;
                }
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    fn emit_detection(
        &self,
        turn: &PhantomTurn,
        slots: Option<&PtyInputSlots>,
        anchor: OpenAnchor,
    ) {
        let latest = slots.and_then(|s| s.latest());
        let last_write_caller = latest
            .map(|o| o.caller.tag().into_owned())
            .unwrap_or_else(|| "none".to_string());
        // -1 when there was no input at all. Always >= 0 otherwise: a detection
        // implies the latest input predates session-open.
        let last_write_ms_before_open = latest
            .map(|o| anchor.at.saturating_duration_since(o.at).as_millis() as i64)
            .unwrap_or(-1);
        let elapsed = elapsed_ms_since_open(turn, anchor.at, anchor.wall);
        match detection_event(slots.is_some()) {
            DetectionEvent::Detected => warn!(
                event = "phantom_turn_detected",
                session_id = %self.session_id,
                terminal_id = %self.terminal_id,
                content_chars = turn.content_chars,
                elapsed_ms_since_session_open = elapsed,
                terminal_resolved = true,
                last_write_caller = %last_write_caller,
                last_write_ms_before_open,
                "phantom_turn_detected: a {}-char typed user turn appeared after session-open \
                 with no PTY input since; detection only, the turn was not altered",
                turn.content_chars,
            ),
            DetectionEvent::CandidateUnresolvedTerminal => info!(
                event = "phantom_turn_candidate_unresolved_terminal",
                session_id = %self.session_id,
                terminal_id = %self.terminal_id,
                content_chars = turn.content_chars,
                elapsed_ms_since_session_open = elapsed,
                terminal_resolved = false,
                "phantom_turn_candidate_unresolved_terminal: a {}-char typed user turn appeared \
                 after session-open, but the terminal could not be resolved, so whether any PTY \
                 input preceded it is UNKNOWN; not reported as a detection",
                turn.content_chars,
            ),
        }
    }
}

/// Which event a phantom-shaped match is reported as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionEvent {
    /// `phantom_turn_detected` (warn): the terminal resolved and showed no input.
    Detected,
    /// `phantom_turn_candidate_unresolved_terminal` (info): no input evidence
    /// could be read, so the match is a candidate, not a detection.
    CandidateUnresolvedTerminal,
}

/// The event for a phantom-shaped match, by whether the terminal resolved.
pub fn detection_event(terminal_resolved: bool) -> DetectionEvent {
    if terminal_resolved {
        DetectionEvent::Detected
    } else {
        DetectionEvent::CandidateUnresolvedTerminal
    }
}

/// Milliseconds from session-open to the turn: from the turn's own transcript
/// timestamp when it has one (poll-tick independent), else to now.
fn elapsed_ms_since_open(
    turn: &PhantomTurn,
    session_open_at: Instant,
    session_open_wall: SystemTime,
) -> i64 {
    let open: chrono::DateTime<chrono::Utc> = session_open_wall.into();
    match turn.timestamp {
        Some(ts) => (ts - open).num_milliseconds(),
        None => session_open_at.elapsed().as_millis() as i64,
    }
}

/// Spawn the watch for one confirmed session-open. Fire-and-forget: the
/// session-open route must never wait on it.
///
/// `config_dir` / `cwds` come from the request and the lifecycle record; when
/// no config dir is known every account home on the box is a candidate. With
/// no cwd at all there is no transcript path to derive, so the watch is
/// skipped with an info line saying so. A session-open for a session whose
/// watch of the same terminal is still running extends that watch instead of
/// starting a second ([`WatchClaim::claim_or_extend`]).
pub fn spawn_watch(
    app_handle: tauri::AppHandle,
    session_id: String,
    terminal_id: String,
    config_dir: Option<String>,
    cwds: Vec<String>,
) {
    let open = OpenAnchor {
        at: Instant::now(),
        wall: SystemTime::now(),
    };
    if cwds.iter().all(|c| c.trim().is_empty()) {
        info!(
            session_id = %session_id,
            terminal_id = %terminal_id,
            "phantom-turn watch skipped: no cwd known for the session, so no transcript path"
        );
        return;
    }
    // One watch per launch: the shim (before exec) and the SessionStart hook
    // (later) both post session-open for a fresh session, and pair; a later
    // launch replaces the watch. The claim rides into the task and is released
    // when the watch ends.
    // The terminal's latest PTY input, which decides whether this open pairs
    // with an earlier one for the same launch.
    let last_input_at = {
        use tauri::Manager;
        app_handle
            .try_state::<std::sync::Arc<super::TerminalManager>>()
            .and_then(|tm| tm.get(&terminal_id))
            .and_then(|s| s.last_input().latest().map(|o| o.at))
    };
    let claim = match WatchClaim::claim_or_extend(
        &session_id,
        &terminal_id,
        open,
        last_input_at,
        PHANTOM_TURN_WATCH_WINDOW,
    ) {
        ClaimOutcome::New(claim) => claim,
        ClaimOutcome::Extended => {
            debug!(
                session_id = %session_id,
                terminal_id = %terminal_id,
                "phantom-turn watch extended by a paired session-open; no second watch"
            );
            return;
        }
    };
    let timing = claim.timing();
    let tid = terminal_id.clone();
    let input_slots = move || {
        use tauri::Manager;
        app_handle
            .try_state::<std::sync::Arc<super::TerminalManager>>()
            .and_then(|tm| tm.get(&tid))
            .map(|s| s.last_input())
    };
    let build = move |config_dirs: Vec<PathBuf>, start: TailStart| {
        let paths = candidate_transcript_paths(&config_dirs, &cwds, &session_id);
        info!(
            session_id = %session_id,
            terminal_id = %terminal_id,
            candidate_paths = paths.len(),
            window_ms = PHANTOM_TURN_WATCH_WINDOW.as_millis() as u64,
            "phantom-turn watch started after session-open"
        );
        SessionOpenWatch::new(
            session_id,
            terminal_id,
            paths,
            start,
            timing,
            PHANTOM_TURN_WATCH_WINDOW,
            PHANTOM_TURN_POLL_INTERVAL,
        )
    };
    match config_dir.filter(|d| !d.trim().is_empty()) {
        // The common case: the account is known, so the path set is one or two
        // `stat`s and the baseline is taken here, on the route, before the
        // phantom turn can land and be swallowed into it.
        Some(dir) => {
            let watch = build(vec![PathBuf::from(dir)], TailStart::CurrentEnd);
            tokio::spawn(async move {
                let verdict = watch.run(input_slots).await;
                drop(claim);
                verdict
            });
        }
        // Account unknown: discovery walks every account home on the box — a
        // blocking directory walk that belongs on neither the route nor an
        // async worker, so it runs on the blocking pool — together with the
        // tail construction, whose per-path `stat`s and boundary reads are file
        // I/O as well. By the time it returns a length baseline could already
        // contain the phantom turn, so the tails start a bounded look-back
        // before the end and the turn-timestamp filter excludes the prior
        // conversation instead.
        None => {
            tokio::spawn(async move {
                let Ok(watch) = tokio::task::spawn_blocking(move || {
                    build(
                        super::transcript::find_claude_config_dirs(),
                        TailStart::RecentTail,
                    )
                })
                .await
                else {
                    lock_state(&claim.timing()).ending = true;
                    return Verdict::Undecided;
                };
                let verdict = watch.run(input_slots).await;
                drop(claim);
                verdict
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::session::PtyWriteCaller;

    fn user_line(text: &str, source: &str) -> String {
        serde_json::json!({
            "type": "user",
            "promptSource": source,
            "timestamp": "2026-09-15T10:00:00.120Z",
            "message": {"role": "user", "content": text},
        })
        .to_string()
    }

    const HOOK_LINE: &str = r#"{"type":"attachment","attachment":{"type":"hook_success","hookName":"SessionStart:resume"}}"#;

    /// Session-open on the wall clock, 20 ms before the fixture turns'
    /// `timestamp` (`10:00:00.120Z`).
    fn open_wall() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-15T10:00:00.100Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn no_input(open: Instant) -> InputEvidence<'static> {
        InputEvidence {
            session_open_at: open,
            session_open_wall: open_wall(),
            latest: None,
        }
    }

    fn user_line_at(text: &str, source: &str, timestamp: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "type": "user",
            "promptSource": source,
            "message": {"role": "user", "content": text},
        });
        if let Some(ts) = timestamp {
            v["timestamp"] = serde_json::Value::String(ts.to_string());
        }
        v.to_string()
    }

    /// `--fork-session` copies the whole prior conversation into a NEW
    /// transcript file, so the append baseline (0 for a new file) cannot
    /// exclude it. Turns stamped before session-open (minus the slack) are
    /// history and never judged; unstamped turns are skipped; a turn inside the
    /// slack, or after open, still is.
    #[test]
    fn turns_older_than_session_open_or_unstamped_are_never_judged() {
        let open = Instant::now();
        let forked_history = [
            user_line_at("is", "typed", Some("2026-09-14T08:00:00Z")),
            user_line_at(
                "please run the tests",
                "typed",
                Some("2026-09-14T08:01:00Z"),
            ),
            user_line_at("ok", "typed", Some("2026-09-15T10:00:00.099Z")),
            user_line_at("is", "typed", None),
            user_line_at("is", "typed", Some("not a time")),
        ];
        // 10:00:00.099 is 1 ms before open — inside the 2 s slack, so judged.
        assert!(matches!(
            detect_phantom_turn(forked_history.iter().map(String::as_str), no_input(open)),
            Verdict::Phantom(PhantomTurn {
                content_chars: 2,
                ..
            })
        ));
        // Without that one, everything is history or unstamped: undecided.
        let history_only = [
            &forked_history[0],
            &forked_history[1],
            &forked_history[3],
            &forked_history[4],
        ];
        assert_eq!(
            detect_phantom_turn(history_only.iter().map(|s| s.as_str()), no_input(open)),
            Verdict::Undecided
        );
        // Just past the slack (open − 2.001 s): history.
        let stale = [user_line_at(
            "is",
            "typed",
            Some("2026-09-15T09:59:58.099Z"),
        )];
        assert_eq!(
            detect_phantom_turn(stale.iter().map(String::as_str), no_input(open)),
            Verdict::Undecided
        );
    }

    fn anchor_now() -> OpenAnchor {
        OpenAnchor {
            at: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    fn claim(sid: &str, tid: &str, open: OpenAnchor, input: Option<Instant>) -> ClaimOutcome {
        WatchClaim::claim_or_extend(sid, tid, open, input, PHANTOM_TURN_WATCH_WINDOW)
    }

    fn open_after(first: OpenAnchor, d: Duration) -> OpenAnchor {
        OpenAnchor {
            at: first.at + d,
            wall: first.wall + d,
        }
    }

    /// A paired later open (same terminal, within the pairing interval, no
    /// input between) extends the deadline only, keeping the earliest evidence;
    /// the claim is released on drop.
    #[test]
    fn a_paired_session_open_extends_the_deadline_but_keeps_the_earliest_evidence() {
        let sid = format!("test-dedupe-{}", uuid::Uuid::new_v4());
        let first_open = anchor_now();
        let ClaimOutcome::New(first) = claim(&sid, "t1", first_open, None) else {
            panic!("first claim");
        };
        let later = open_after(first_open, Duration::from_secs(5));
        assert!(matches!(
            claim(&sid, "t1", later, None),
            ClaimOutcome::Extended
        ));
        assert_eq!(
            read_timing(&first.timing()),
            WatchTiming {
                evidence: first_open,
                latest_open: later.at
            },
            "deadline moved, evidence kept"
        );

        assert!(matches!(
            claim(&format!("{sid}-other"), "t1", anchor_now(), None),
            ClaimOutcome::New(_)
        ));
        drop(first);
        assert!(
            matches!(claim(&sid, "t1", anchor_now(), None), ClaimOutcome::New(_)),
            "released when the watch finishes"
        );
    }

    /// A later open from a DIFFERENT terminal replaces the watch, marks the old
    /// one superseded, and the old claim's drop leaves the replacement.
    #[test]
    fn a_session_open_from_another_terminal_replaces_and_supersedes_the_watch() {
        let sid = format!("test-replace-{}", uuid::Uuid::new_v4());
        let ClaimOutcome::New(old) = claim(&sid, "t1", anchor_now(), None) else {
            panic!("first claim");
        };
        let ClaimOutcome::New(new) = claim(&sid, "t2", anchor_now(), None) else {
            panic!("another terminal must get its own watch");
        };
        assert!(lock_state(&old.timing()).superseded);
        assert!(!lock_state(&new.timing()).superseded);
        drop(old);
        assert!(
            matches!(
                claim(&sid, "t2", anchor_now(), None),
                ClaimOutcome::Extended
            ),
            "the replacement survives the old claim's drop"
        );
        drop(new);
        assert!(matches!(
            claim(&sid, "t2", anchor_now(), None),
            ClaimOutcome::New(_)
        ));
    }

    /// A quit and `claude --resume` in the same pane is its OWN launch: a later
    /// open past the pairing interval, or one preceded by PTY input since the
    /// first open, starts a fresh watch anchored at its own open.
    #[test]
    fn a_later_launch_in_the_same_pane_gets_a_fresh_evidence_anchor() {
        // Past the pairing interval.
        let sid = format!("test-relaunch-{}", uuid::Uuid::new_v4());
        let first_open = anchor_now();
        let ClaimOutcome::New(first) = claim(&sid, "t1", first_open, None) else {
            panic!("first claim");
        };
        let late = open_after(
            first_open,
            PHANTOM_TURN_PAIRING_INTERVAL + Duration::from_secs(1),
        );
        let ClaimOutcome::New(second) = claim(&sid, "t1", late, None) else {
            panic!("a launch past the pairing interval is a new watch");
        };
        assert_eq!(read_timing(&second.timing()).evidence, late);
        assert!(lock_state(&first.timing()).superseded);
        drop((first, second));

        // Within the interval, but the pane saw input between the opens.
        let sid = format!("test-relaunch-input-{}", uuid::Uuid::new_v4());
        let first_open = anchor_now();
        let ClaimOutcome::New(first) = claim(&sid, "t1", first_open, None) else {
            panic!("first claim");
        };
        let resumed = open_after(first_open, Duration::from_secs(4));
        let typed = first_open.at + Duration::from_secs(2);
        let ClaimOutcome::New(second) = claim(&sid, "t1", resumed, Some(typed)) else {
            panic!("input between the opens means a new launch");
        };
        assert_eq!(read_timing(&second.timing()).evidence, resumed);
        assert!(lock_state(&first.timing()).superseded);
        // Input BEFORE the first open does not break pairing.
        let sid = format!("test-pair-prior-input-{}", uuid::Uuid::new_v4());
        let first_open = anchor_now();
        let ClaimOutcome::New(_held) = claim(&sid, "t1", first_open, None) else {
            panic!("first claim");
        };
        let prior = first_open
            .at
            .checked_sub(Duration::from_millis(1))
            .unwrap_or(first_open.at);
        assert!(matches!(
            claim(
                &sid,
                "t1",
                open_after(first_open, Duration::from_secs(1)),
                Some(prior)
            ),
            ClaimOutcome::Extended
        ));
    }

    /// An entry that is ending, or whose deadline has passed, never absorbs a
    /// later open: a new watch starts.
    #[test]
    fn an_ending_or_expired_watch_is_not_extended() {
        let sid = format!("test-ending-{}", uuid::Uuid::new_v4());
        let ClaimOutcome::New(first) = claim(&sid, "t1", anchor_now(), None) else {
            panic!("first claim");
        };
        lock_state(&first.timing()).ending = true;
        assert!(matches!(
            claim(&sid, "t1", anchor_now(), None),
            ClaimOutcome::New(_)
        ));

        let sid = format!("test-expired-{}", uuid::Uuid::new_v4());
        let ClaimOutcome::New(_short) =
            WatchClaim::claim_or_extend(&sid, "t1", anchor_now(), None, Duration::from_millis(1))
        else {
            panic!("first claim");
        };
        std::thread::sleep(Duration::from_millis(5));
        assert!(matches!(
            claim(&sid, "t1", anchor_now(), None),
            ClaimOutcome::New(_)
        ));
    }

    /// The deadline is the latest open plus the window, capped at the evidence
    /// anchor plus twice the window.
    #[test]
    fn the_watch_deadline_is_capped_at_twice_the_window() {
        let t = Instant::now();
        let w = Duration::from_secs(25);
        let state = |latest: Duration| WatchState {
            timing: WatchTiming {
                evidence: OpenAnchor {
                    at: t,
                    wall: SystemTime::now(),
                },
                latest_open: t + latest,
            },
            window: w,
            ending: false,
            superseded: false,
        };
        assert_eq!(state(Duration::ZERO).deadline(), t + w);
        assert_eq!(
            state(Duration::from_secs(10)).deadline(),
            t + Duration::from_secs(35)
        );
        assert_eq!(state(Duration::from_secs(40)).deadline(), t + 2 * w);
    }

    /// A superseded watch yields no verdict — even with a phantom-shaped turn
    /// sitting in its transcript.
    #[tokio::test]
    async fn a_superseded_watch_stops_without_a_verdict() {
        let sid = format!("test-superseded-{}", uuid::Uuid::new_v4());
        let ClaimOutcome::New(old) = claim(&sid, "t1", anchor_now(), None) else {
            panic!("first claim");
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "").unwrap();
        let watch = SessionOpenWatch::new(
            sid.clone(),
            "t1".into(),
            vec![path.clone()],
            TailStart::CurrentEnd,
            old.timing(),
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        let ClaimOutcome::New(_new) = claim(&sid, "t2", anchor_now(), None) else {
            panic!("replacement");
        };
        let now = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            &path,
            format!("{}\n", user_line_at("is", "typed", Some(&now))),
        )
        .unwrap();
        let started = Instant::now();
        let verdict = watch.run(|| Some(PtyInputSlots::default())).await;
        assert_eq!(verdict, Verdict::Superseded);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Run one watch over `line` (appended after the watch is built) with the
    /// timing a paired two-open launch leaves behind.
    async fn run_two_open_watch(second_open_after: Duration, line: String) -> Verdict {
        let sid = format!("test-two-open-{}", uuid::Uuid::new_v4());
        let first = anchor_now();
        let ClaimOutcome::New(held) = claim(&sid, "t1", first, None) else {
            panic!("first claim");
        };
        assert!(matches!(
            claim(&sid, "t1", open_after(first, second_open_after), None),
            ClaimOutcome::Extended
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "").unwrap();
        let watch = SessionOpenWatch::new(
            sid,
            "t1".into(),
            vec![path.clone()],
            TailStart::CurrentEnd,
            held.timing(),
            Duration::from_millis(300),
            Duration::from_millis(10),
        );
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let verdict = watch.run(|| Some(PtyInputSlots::default())).await;
        drop(held);
        verdict
    }

    /// A slow second open (3 s after the first) does not push the timestamp
    /// filter past a phantom stamped between the two opens.
    #[tokio::test]
    async fn a_slow_second_open_does_not_skip_a_phantom_between_the_opens() {
        let between = (chrono::Utc::now() + chrono::Duration::milliseconds(500)).to_rfc3339();
        let verdict = run_two_open_watch(
            Duration::from_secs(3),
            user_line_at("is", "typed", Some(&between)),
        )
        .await;
        assert!(
            matches!(
                verdict,
                Verdict::Phantom(PhantomTurn {
                    content_chars: 2,
                    ..
                })
            ),
            "{verdict:?}"
        );
    }

    /// Input judged against the EARLIEST paired open: a keystroke after the
    /// first open counts as real input, so a short typed turn does not fire.
    #[test]
    fn input_after_the_evidence_anchor_counts_as_real_input() {
        let first = Instant::now();
        let obs = PtyInputObservation {
            caller: PtyWriteCaller::TauriTerminalWrite,
            at: first + Duration::from_millis(500),
            bytes: 2,
        };
        let now = chrono::Utc::now();
        let evidence = InputEvidence {
            session_open_at: first,
            session_open_wall: now,
            latest: Some(&obs),
        };
        let line = user_line_at("ok", "typed", Some(&now.to_rfc3339()));
        assert_eq!(
            detect_phantom_turn([line.as_str()], evidence),
            Verdict::ClearTypedTurn
        );
    }

    /// The running watch re-reads its state: a later open extends the deadline
    /// of the same task.
    #[tokio::test]
    async fn a_later_open_extends_the_running_watch_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let timing = shared_timing(OpenAnchor {
            at: started,
            wall: SystemTime::now(),
        });
        let mover = timing.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            lock_state(&mover).timing.latest_open += Duration::from_millis(150);
        });
        let verdict = SessionOpenWatch::new(
            "sid".into(),
            "tid".into(),
            vec![dir.path().join("missing.jsonl")],
            TailStart::CurrentEnd,
            timing,
            Duration::from_millis(200),
            Duration::from_millis(10),
        )
        .run(|| None)
        .await;
        assert_eq!(verdict, Verdict::Undecided);
        assert!(
            started.elapsed() >= Duration::from_millis(340),
            "the deadline moved with the latest open: ended after {:?}",
            started.elapsed()
        );
    }

    /// An unresolved terminal makes the no-input evidence UNKNOWN: the match is
    /// a candidate (info), never the `phantom_turn_detected` warn.
    #[test]
    fn an_unresolved_terminal_is_a_candidate_not_a_detection() {
        assert_eq!(detection_event(true), DetectionEvent::Detected);
        assert_eq!(
            detection_event(false),
            DetectionEvent::CandidateUnresolvedTerminal
        );
    }

    /// A watch built late (unknown account) reads a bounded look-back: prior
    /// conversation in it is excluded by timestamp, and a phantom that landed
    /// before the watch was even built is still caught.
    #[tokio::test]
    async fn a_recent_tail_watch_catches_a_turn_written_before_it_was_built() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let open_wall = SystemTime::now();
        let now = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                user_line_at("no", "typed", Some("2026-01-01T00:00:00Z")),
                user_line_at("is", "typed", Some(&now))
            ),
        )
        .unwrap();
        let verdict = SessionOpenWatch::new(
            "sid".into(),
            "tid".into(),
            vec![path],
            TailStart::RecentTail,
            shared_timing(OpenAnchor {
                at: Instant::now(),
                wall: open_wall,
            }),
            Duration::from_millis(300),
            Duration::from_millis(10),
        )
        .run(|| Some(PtyInputSlots::default()))
        .await;
        assert!(
            matches!(
                verdict,
                Verdict::Phantom(PhantomTurn {
                    content_chars: 2,
                    ..
                })
            ),
            "{verdict:?}"
        );
    }

    /// When the look-back offset lands exactly on a line start (the byte before
    /// it is a newline), that first line is whole and is NOT skipped.
    #[test]
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn a_recent_tail_on_a_line_boundary_keeps_the_first_line() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aligned.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "older-record").unwrap();
        // Exactly PHANTOM_TURN_TAIL_LOOKBACK_BYTES of 1024-byte lines.
        let lines = (PHANTOM_TURN_TAIL_LOOKBACK_BYTES / 1024) as usize;
        for i in 0..lines {
            let head = format!("line-{i:04}-");
            writeln!(f, "{head}{}", "y".repeat(1023 - head.len())).unwrap();
        }
        drop(f);
        let mut tail = AppendTail::from_recent(path);
        assert!(!tail.skip_to_line_start);
        let got = tail.read_new_lines();
        assert_eq!(got.len(), lines);
        assert!(got[0].starts_with("line-0000-"), "{}", &got[0][..12]);
    }

    /// A multi-MB transcript is not read whole: the look-back starts within
    /// the last [`PHANTOM_TURN_TAIL_LOOKBACK_BYTES`], skips the torn first
    /// line, and yields only complete records — the newest one included.
    #[test]
    fn a_recent_tail_reads_only_whole_lines_of_the_last_512_kib() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let old = user_line_at(&"x".repeat(900), "typed", Some("2026-01-01T00:00:00Z"));
        let total = 3 * 1024 * 1024 / (old.len() + 1) + 1;
        for _ in 0..total {
            writeln!(f, "{old}").unwrap();
        }
        let now = chrono::Utc::now().to_rfc3339();
        writeln!(f, "{}", user_line_at("is", "typed", Some(&now))).unwrap();
        drop(f);
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len > 3 * 1024 * 1024);

        let mut tail = AppendTail::from_recent(path);
        assert_eq!(tail.offset, len - PHANTOM_TURN_TAIL_LOOKBACK_BYTES);
        let lines = tail.read_new_lines();
        assert!(
            !lines.is_empty() && lines.len() < total,
            "{} lines",
            lines.len()
        );
        assert!(
            lines
                .iter()
                .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()),
            "a torn record was yielded"
        );
        assert!(lines.last().unwrap().contains("\"is\""));
        assert_eq!(tail.offset, len, "consumed to the end");
        assert!(tail.read_new_lines().is_empty());
    }

    /// The incident's shape: hook output, then a 2-char `typed` `is` with no
    /// PTY input since session-open.
    #[test]
    fn phantom_shaped_turn_fires() {
        let open = Instant::now();
        let lines = [HOOK_LINE.to_string(), user_line("is", "typed")];
        let verdict = detect_phantom_turn(lines.iter().map(String::as_str), no_input(open));
        match verdict {
            Verdict::Phantom(t) => {
                assert_eq!(t.content_chars, 2);
                assert!(t.timestamp.is_some());
            }
            other => panic!("expected Phantom, got {other:?}"),
        }
    }

    /// A normal first turn — full-length — does not fire, and ends the watch.
    #[test]
    fn a_normal_first_turn_does_not_fire() {
        let open = Instant::now();
        let lines = [
            HOOK_LINE.to_string(),
            user_line("please run the test suite", "typed"),
        ];
        assert_eq!(
            detect_phantom_turn(lines.iter().map(String::as_str), no_input(open)),
            Verdict::ClearTypedTurn
        );
    }

    /// A short turn the operator really typed: the PTY saw a write after
    /// session-open, so the length alone is not suspicious.
    #[test]
    fn a_short_turn_with_matching_real_input_does_not_fire() {
        let open = Instant::now();
        let obs = PtyInputObservation {
            caller: PtyWriteCaller::TauriTerminalWrite,
            at: open + Duration::from_millis(50),
            bytes: 1,
        };
        let evidence = InputEvidence {
            session_open_at: open,
            session_open_wall: open_wall(),
            latest: Some(&obs),
        };
        let lines = [user_line("ok", "typed")];
        assert_eq!(
            detect_phantom_turn(lines.iter().map(String::as_str), evidence),
            Verdict::ClearTypedTurn
        );
    }

    /// Input from BEFORE session-open (the `claude --resume` line typed into a
    /// restored pane's shell) does not explain a turn that appeared after it.
    #[test]
    fn input_before_session_open_does_not_mask_a_phantom() {
        let obs = PtyInputObservation {
            caller: PtyWriteCaller::TauriTerminalWrite,
            at: Instant::now(),
            bytes: 30,
        };
        let open = obs.at + Duration::from_millis(900);
        let evidence = InputEvidence {
            session_open_at: open,
            session_open_wall: open_wall(),
            latest: Some(&obs),
        };
        let lines = [user_line("is", "typed")];
        assert!(matches!(
            detect_phantom_turn(lines.iter().map(String::as_str), evidence),
            Verdict::Phantom(_)
        ));
    }

    /// `system` (and any non-`typed`) short turns are not the incident's shape.
    #[test]
    fn a_system_prompt_source_short_turn_does_not_fire() {
        let open = Instant::now();
        for source in ["system", "suggestion_accepted"] {
            let lines = [user_line("is", source)];
            assert_eq!(
                detect_phantom_turn(lines.iter().map(String::as_str), no_input(open)),
                Verdict::Undecided,
                "source {source}"
            );
        }
    }

    /// Records that are user-role but not a typed text turn are skipped:
    /// `isMeta`, sidechains, tool results, and the threshold boundary.
    #[test]
    fn non_text_and_boundary_records_are_classified_correctly() {
        let open = Instant::now();
        let meta =
            r#"{"type":"user","isMeta":true,"promptSource":"typed","message":{"content":"is"}}"#;
        let side = r#"{"type":"user","isSidechain":true,"promptSource":"typed","message":{"content":"is"}}"#;
        let tool = r#"{"type":"user","promptSource":"typed","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        assert_eq!(
            detect_phantom_turn([meta, side, tool, "not json"], no_input(open)),
            Verdict::Undecided
        );
        // Array-form text content, at the threshold (3) and one past it (4).
        let three = r#"{"type":"user","promptSource":"typed","timestamp":"2026-09-15T10:00:00.120Z","message":{"content":[{"type":"text","text":" yes "}]}}"#;
        assert!(matches!(
            detect_phantom_turn([three], no_input(open)),
            Verdict::Phantom(PhantomTurn {
                content_chars: 3,
                ..
            })
        ));
        assert_eq!(
            detect_phantom_turn([user_line("four", "typed").as_str()], no_input(open)),
            Verdict::ClearTypedTurn
        );
    }

    #[test]
    fn candidate_paths_cross_dirs_and_cwds_without_duplicates() {
        let dirs = vec![PathBuf::from("/c1"), PathBuf::from("/c2")];
        let cwds = vec![
            "/w/repo".to_string(),
            "/w/repo".to_string(),
            " ".to_string(),
        ];
        let paths = candidate_transcript_paths(&dirs, &cwds, "sid");
        assert_eq!(paths.len(), 2);
        assert!(paths[0].starts_with("/c1") && paths[0].ends_with("sid.jsonl"));
        assert!(paths[1].starts_with("/c2"));
    }

    /// The watcher end to end over a real file: the prior conversation already
    /// on disk (a short typed turn a resume must NOT re-judge) is skipped by
    /// the baseline, and the phantom appended after open is caught.
    #[tokio::test]
    async fn watcher_skips_prior_content_and_catches_an_appended_phantom() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, format!("{}\n", user_line("no", "typed"))).unwrap();

        let open = Instant::now();
        let writer_path = path.clone();
        let appender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&writer_path)
                .unwrap();
            // Written in two halves so the partial-line rule is exercised.
            // Stamped NOW, i.e. after session-open, as a live turn is.
            let now = chrono::Utc::now().to_rfc3339();
            let line = user_line_at("is", "typed", Some(&now));
            let (a, b) = line.split_at(line.len() / 2);
            f.write_all(format!("{HOOK_LINE}\n{a}").as_bytes()).unwrap();
            f.flush().unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            f.write_all(format!("{b}\n").as_bytes()).unwrap();
        });
        let verdict = SessionOpenWatch::new(
            "sid".into(),
            "tid".into(),
            vec![path],
            TailStart::CurrentEnd,
            shared_timing(OpenAnchor {
                at: open,
                wall: SystemTime::now(),
            }),
            Duration::from_secs(3),
            Duration::from_millis(10),
        )
        .run(|| Some(PtyInputSlots::default()))
        .await;
        appender.await.unwrap();
        assert!(
            matches!(
                verdict,
                Verdict::Phantom(PhantomTurn {
                    content_chars: 2,
                    ..
                })
            ),
            "got {verdict:?}"
        );
    }

    /// Bounded: with nothing appended the watcher returns at the window.
    #[tokio::test]
    async fn watcher_ends_at_the_window_when_nothing_is_appended() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let verdict = SessionOpenWatch::new(
            "sid".into(),
            "tid".into(),
            vec![dir.path().join("missing.jsonl")],
            TailStart::CurrentEnd,
            shared_timing(OpenAnchor {
                at: started,
                wall: SystemTime::now(),
            }),
            Duration::from_millis(80),
            Duration::from_millis(10),
        )
        .run(|| None)
        .await;
        assert_eq!(verdict, Verdict::Undecided);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
