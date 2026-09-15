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
//! (`install_effects_producer::post_session_open`), which is the runner's
//! start/resume signal: the shim's SessionStart hook posts it on both.
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

/// When session-open arrived for a watch — monotonic (window deadline and the
/// input comparison) and wall-clock (compared against transcript timestamps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAnchor {
    pub at: Instant,
    pub wall: SystemTime,
}

/// An [`OpenAnchor`] shared between a running watch and the registry, so a
/// LATER session-open for the same session can move it.
pub type SharedAnchor = std::sync::Arc<std::sync::Mutex<OpenAnchor>>;

/// Build a fresh [`SharedAnchor`].
pub fn shared_anchor(at: Instant, wall: SystemTime) -> SharedAnchor {
    std::sync::Arc::new(std::sync::Mutex::new(OpenAnchor { at, wall }))
}

fn read_anchor(anchor: &SharedAnchor) -> OpenAnchor {
    *anchor.lock().unwrap_or_else(|e| e.into_inner())
}

/// One registry entry: the claim's identity plus the anchor its watch reads.
struct ActiveWatch {
    claimed: Instant,
    anchor: SharedAnchor,
}

/// Process-global registry of ACTIVE watches, keyed by session id. One fresh
/// session posts `/control/session-open` twice — the identity shim BEFORE it
/// execs `claude`, the SessionStart hook later, from inside the hook batch —
/// and two watches over one transcript would emit the same detection twice.
static ACTIVE_WATCHES: std::sync::Mutex<Option<std::collections::HashMap<String, ActiveWatch>>> =
    std::sync::Mutex::new(None);

/// What [`WatchClaim::claim_or_reanchor`] did.
pub enum ClaimOutcome {
    /// No active watch: this caller owns the new one.
    New(WatchClaim),
    /// A watch was already active; its anchor was moved to this later
    /// session-open (and its deadline with it). The caller starts nothing.
    Reanchored,
}

/// An active watch's claim on its session id. Dropping it — when the watch
/// task finishes, or unwinds — releases the id.
pub struct WatchClaim {
    session_id: String,
    claimed: Instant,
    anchor: SharedAnchor,
}

impl WatchClaim {
    /// Claim `session_id` for one watch anchored at `open`, or — while another
    /// watch for it is still active — RE-ANCHOR that watch to `open` when it is
    /// later than its current anchor, extending its deadline. Dropping the later
    /// open instead would leave the window counted from the shim's pre-exec
    /// post, which can end before the hook batch the incident's turn followed.
    ///
    /// An entry whose anchor is older than the watch window plus a minute is
    /// treated as abandoned and replaced, so a registry entry can never
    /// suppress watches for a session forever.
    pub fn claim_or_reanchor(session_id: &str, open: OpenAnchor) -> ClaimOutcome {
        let mut guard = ACTIVE_WATCHES.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(Default::default);
        if let Some(active) = map.get(session_id) {
            let mut current = active.anchor.lock().unwrap_or_else(|e| e.into_inner());
            let live = open.at.saturating_duration_since(current.at)
                < PHANTOM_TURN_WATCH_WINDOW + Duration::from_secs(60);
            if live {
                if open.at > current.at {
                    *current = open;
                }
                return ClaimOutcome::Reanchored;
            }
        }
        let claimed = Instant::now();
        let anchor = std::sync::Arc::new(std::sync::Mutex::new(open));
        map.insert(
            session_id.to_string(),
            ActiveWatch {
                claimed,
                anchor: anchor.clone(),
            },
        );
        ClaimOutcome::New(Self {
            session_id: session_id.to_string(),
            claimed,
            anchor,
        })
    }

    /// The anchor this claim's watch must read (and later opens move).
    pub fn anchor(&self) -> SharedAnchor {
        self.anchor.clone()
    }
}

impl Drop for WatchClaim {
    fn drop(&mut self) {
        let mut guard = ACTIVE_WATCHES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = guard.as_mut() {
            // Only this claim's own entry: an abandoned claim that was replaced
            // must not release its successor.
            if map
                .get(&self.session_id)
                .is_some_and(|a| a.claimed == self.claimed)
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

/// Append-only reader over one transcript file: yields only complete lines
/// appended since the last read, starting from the file's length at creation
/// ([`Self::new`]) or from its first byte ([`Self::from_start`]).
///
/// The length baseline matters on `--resume`: the file already holds the whole
/// prior conversation, full of short typed turns that are cheapest never to
/// read. The timestamp filter in [`detect_phantom_turn`] is what makes a
/// from-start read safe when no baseline could be taken in time.
struct AppendTail {
    path: PathBuf,
    offset: u64,
}

impl AppendTail {
    fn new(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, offset }
    }

    fn from_start(path: PathBuf) -> Self {
        Self { path, offset: 0 }
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
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return Vec::new();
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

/// Where each transcript tail starts reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailStart {
    /// At the file's length when the watch is built — prior content is never
    /// read. For a watch built on the session-open route itself.
    CurrentEnd,
    /// At byte 0. For a watch built LATER (after account discovery), where a
    /// length baseline could swallow the very turn it exists to catch; prior
    /// conversation is then excluded by the turn-timestamp filter instead.
    Beginning,
}

/// One bounded watch after one `POST /control/session-open`.
pub struct SessionOpenWatch {
    pub session_id: String,
    pub terminal_id: String,
    /// One append-only reader per candidate transcript path.
    tails: Vec<AppendTail>,
    /// When session-open arrived. Shared with the registry: a later
    /// session-open for the same session moves it, and with it the deadline.
    anchor: SharedAnchor,
    pub window: Duration,
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
        anchor: SharedAnchor,
        window: Duration,
        poll: Duration,
    ) -> Self {
        let tail = match start {
            TailStart::CurrentEnd => AppendTail::new,
            TailStart::Beginning => AppendTail::from_start,
        };
        Self {
            session_id,
            terminal_id,
            tails: paths.into_iter().map(tail).collect(),
            anchor,
            window,
            poll,
        }
    }

    /// Poll `paths` every `poll` until the anchor plus `window` — re-read every
    /// tick, so a re-anchor extends it — feeding newly appended lines and a
    /// fresh `input_slots()` snapshot to [`detect_phantom_turn`]. Emits at most
    /// once, then stops; stops early on a clear typed turn; otherwise ends
    /// silently at the deadline. Returns the verdict it stopped on, for tests.
    ///
    /// `input_slots` returns `None` when the terminal cannot be resolved
    /// (closed, or not managed by this runner). The no-input half of the
    /// evidence is then UNKNOWN, so such a match is reported as the distinct
    /// info-level `phantom_turn_candidate_unresolved_terminal`, never as the
    /// `phantom_turn_detected` warn ([`detection_event`]).
    pub async fn run(mut self, input_slots: impl Fn() -> Option<PtyInputSlots>) -> Verdict {
        loop {
            let anchor = read_anchor(&self.anchor);
            let lines: Vec<String> = self
                .tails
                .iter_mut()
                .flat_map(|t| t.read_new_lines())
                .collect();
            if !lines.is_empty() {
                let slots = input_slots();
                let evidence = InputEvidence {
                    session_open_at: anchor.at,
                    session_open_wall: anchor.wall.into(),
                    latest: slots.as_ref().and_then(|s| s.latest()),
                };
                match detect_phantom_turn(lines.iter().map(String::as_str), evidence) {
                    Verdict::Phantom(turn) => {
                        self.emit_detection(&turn, slots.as_ref(), anchor);
                        return Verdict::Phantom(turn);
                    }
                    Verdict::ClearTypedTurn => return Verdict::ClearTypedTurn,
                    Verdict::Undecided => {}
                }
            }
            if Instant::now() >= read_anchor(&self.anchor).at + self.window {
                return Verdict::Undecided;
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    fn emit_detection(&self, turn: &PhantomTurn, slots: Option<&PtyInputSlots>, anchor: OpenAnchor) {
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
/// watch is still running re-anchors that watch instead of starting a second
/// ([`WatchClaim::claim_or_reanchor`]).
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
    // One watch per session: the shim (before exec) and the SessionStart hook
    // (later) both post session-open for a fresh session. The claim rides into
    // the task and is released when the watch ends.
    let claim = match WatchClaim::claim_or_reanchor(&session_id, open) {
        ClaimOutcome::New(claim) => claim,
        ClaimOutcome::Reanchored => {
            debug!(
                session_id = %session_id,
                terminal_id = %terminal_id,
                "phantom-turn watch re-anchored to a later session-open; no second watch"
            );
            return;
        }
    };
    let anchor = claim.anchor();
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
            anchor,
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
        // async worker, so it runs on the blocking pool. By the time it returns
        // a length baseline could already contain the phantom turn, so the
        // tails start at byte 0 and the turn-timestamp filter excludes the
        // prior conversation instead.
        None => {
            tokio::spawn(async move {
                let dirs = tokio::task::spawn_blocking(super::transcript::find_claude_config_dirs)
                    .await
                    .unwrap_or_default();
                let watch = build(dirs, TailStart::Beginning);
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
            user_line_at("please run the tests", "typed", Some("2026-09-14T08:01:00Z")),
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
        let stale = [user_line_at("is", "typed", Some("2026-09-15T09:59:58.099Z"))];
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

    /// One watch per session id while it is active: a LATER session-open
    /// re-anchors it (moving the deadline) instead of starting a second, an
    /// earlier one never moves it back, and the claim is released on drop.
    #[test]
    fn a_later_session_open_reanchors_the_one_active_watch() {
        let sid = format!("test-dedupe-{}", uuid::Uuid::new_v4());
        let first_open = anchor_now();
        let ClaimOutcome::New(first) = WatchClaim::claim_or_reanchor(&sid, first_open) else {
            panic!("first claim");
        };
        let later = OpenAnchor {
            at: first_open.at + Duration::from_secs(5),
            wall: first_open.wall + Duration::from_secs(5),
        };
        assert!(matches!(
            WatchClaim::claim_or_reanchor(&sid, later),
            ClaimOutcome::Reanchored
        ));
        assert_eq!(read_anchor(&first.anchor()), later, "moved to the later open");
        assert!(matches!(
            WatchClaim::claim_or_reanchor(&sid, first_open),
            ClaimOutcome::Reanchored
        ));
        assert_eq!(read_anchor(&first.anchor()), later, "never moved back");

        assert!(matches!(
            WatchClaim::claim_or_reanchor(&format!("{sid}-other"), anchor_now()),
            ClaimOutcome::New(_)
        ));
        drop(first);
        assert!(
            matches!(WatchClaim::claim_or_reanchor(&sid, anchor_now()), ClaimOutcome::New(_)),
            "released when the watch finishes"
        );
    }

    /// The running watch re-reads its anchor: moving it forward extends the
    /// deadline of the same task.
    #[tokio::test]
    async fn reanchoring_extends_the_running_watch_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let anchor = shared_anchor(started, SystemTime::now());
        let mover = anchor.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut a = mover.lock().unwrap();
            a.at += Duration::from_millis(400);
        });
        let verdict = SessionOpenWatch::new(
            "sid".into(),
            "tid".into(),
            vec![dir.path().join("missing.jsonl")],
            TailStart::CurrentEnd,
            anchor,
            Duration::from_millis(200),
            Duration::from_millis(10),
        )
        .run(|| None)
        .await;
        assert_eq!(verdict, Verdict::Undecided);
        assert!(
            started.elapsed() >= Duration::from_millis(550),
            "the deadline moved with the anchor: ended after {:?}",
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

    /// A watch built late (unknown account) reads from byte 0: prior
    /// conversation is excluded by timestamp, and a phantom that landed before
    /// the watch was even built is still caught.
    #[tokio::test]
    async fn a_from_start_watch_catches_a_turn_written_before_it_was_built() {
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
            TailStart::Beginning,
            shared_anchor(Instant::now(), open_wall),
            Duration::from_millis(300),
            Duration::from_millis(10),
        )
        .run(|| Some(PtyInputSlots::default()))
        .await;
        assert!(matches!(verdict, Verdict::Phantom(PhantomTurn { content_chars: 2, .. })), "{verdict:?}");
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
            shared_anchor(open, SystemTime::now()),
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
            shared_anchor(started, SystemTime::now()),
            Duration::from_millis(80),
            Duration::from_millis(10),
        )
        .run(|| None)
        .await;
        assert_eq!(verdict, Verdict::Undecided);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
