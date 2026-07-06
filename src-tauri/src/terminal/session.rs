//! TerminalSession — PTY lifecycle management for a single terminal instance.
//!
//! Spawns a shell via `portable-pty`, manages reader/writer threads,
//! and emits Tauri events for output and exit.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, oneshot};

use base64::{engine::general_purpose::STANDARD, Engine};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use tracing::{debug, info, warn};

use super::grid::{Grid, GridPerformer};
use super::interceptor::OutputInterceptor;
use super::types::{TerminalExitEvent, TerminalId, TerminalInfo};

/// Shell integration scripts embedded at compile time.
#[cfg(target_os = "windows")]
const PS1_INTEGRATION: &str = include_str!("../../resources/shell-integration.ps1");
#[cfg(not(target_os = "windows"))]
const BASH_INTEGRATION: &str = include_str!("../../resources/shell-integration.bash");
const ZSH_INTEGRATION: &str = include_str!("../../resources/shell-integration.zsh");

/// Write a shell integration script to a temp file, returning the path on success.
fn write_integration_script(content: &str, name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, content).ok()?;
    Some(path)
}

/// Write the zsh integration as `<dir>/.zshrc` and return `<dir>` for use as
/// `ZDOTDIR`. zsh has no `--rcfile`; pointing `ZDOTDIR` at a dir makes zsh
/// source `$ZDOTDIR/.zshrc` instead of `~/.zshrc` (our script re-sources the
/// user's real `~/.z*` files). Returns `None` so the caller can fall back to a
/// plain shell if the temp dir can't be created.
fn write_zsh_zdotdir(content: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("qontinui-zsh-integration");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".zshrc"), content).ok()?;
    Some(dir)
}

/// Maximum scrollback buffer capacity (1 MB).
const SCROLLBACK_CAPACITY: usize = 1_048_576;

/// Append a chunk of processed PTY output to a session's scrollback ring
/// buffer and bump its monotonic byte counter — the exact teeing step the
/// reader thread performs per read (see [`TerminalSession::spawn`]).
///
/// Extracted as a free function so it can be exercised in tests against a
/// real PTY without constructing a Tauri `AppHandle` (which the reader
/// thread otherwise needs only for event emission, orthogonal to buffer
/// state). `scrollback` and `total_produced` are this session's OWN Arcs —
/// the per-session isolation that [`TerminalSession::get_scrollback_buffer`]
/// reads back depends on each session holding distinct instances, which is
/// what the distinct-buffers regression test locks in.
///
/// Returns the chunk's absolute START offset in the session's output stream
/// (the counter value before this chunk) — the reader thread stamps it onto
/// the `terminal-output` event so the frontend can dedup a scrollback-ring
/// replay against concurrently-delivered live chunks by byte offset.
///
/// The counter bump happens INSIDE the ring lock so that a reader of
/// `(ring contents, total)` under the same lock sees a mutually consistent
/// pair — [`TerminalSession::get_scrollback_buffer`] relies on this to
/// compute an exact `end_offset` (off-by-one-chunk here would make the
/// frontend double-write or drop a chunk at the replay boundary).
fn tee_into_scrollback(
    scrollback: &Arc<Mutex<VecDeque<u8>>>,
    total_produced: &Arc<AtomicU64>,
    data: &[u8],
) -> u64 {
    if let Ok(mut sb) = scrollback.lock() {
        for &byte in data {
            if sb.len() >= SCROLLBACK_CAPACITY {
                sb.pop_front();
            }
            sb.push_back(byte);
        }
        total_produced.fetch_add(data.len() as u64, Ordering::Relaxed)
    } else {
        // Poisoned ring lock: the chunk was still produced — keep the
        // monotonic counter truthful even though buffering failed.
        total_produced.fetch_add(data.len() as u64, Ordering::Relaxed)
    }
}

/// Flow-control watermarks (bytes), mirroring VS Code's `FlowControlConstants`
/// (High=100000, Low=5000 chars). The reader pauses once the unacked gap
/// (`bytes_sent − bytes_acked`) exceeds [`FLOW_HIGH_WATERMARK`] and resumes
/// only once it drops back below [`FLOW_LOW_WATERMARK`] — hysteresis so we
/// don't thrash pause/resume one byte at a time. The frontend acks
/// render-completed bytes in ~5000-byte units (see `flowControl.ts`), so the
/// Low watermark matches one ack quantum.
const FLOW_HIGH_WATERMARK: u64 = 100_000;
const FLOW_LOW_WATERMARK: u64 = 5_000;

/// Byte cap for a held DEC-2026 (synchronized-output) frame: flush the
/// accumulated frame once it reaches this size even if `?2026l` hasn't
/// arrived, so a runaway/never-closed `?2026h` block can't buffer unbounded.
const SYNC_FLUSH_BYTE_CAP: usize = 256 * 1024;
/// Time cap for a held sync frame: flush once the block has been open this
/// long, so a slow producer mid-frame can't stall output past ~one frame.
const SYNC_FLUSH_TIME_CAP: std::time::Duration = std::time::Duration::from_millis(50);

/// Synchronized-output (DEC 2026) frame coalescer for the reader thread.
///
/// Full-screen TUIs (Claude Code) bracket each whole-frame redraw in
/// `?2026h … ?2026l`. Emitting per-PTY-read would split a frame across
/// several `terminal-output` events, so xterm paints partial frames —
/// mid-frame overdraw (symptom a). This accumulates the processed bytes of a
/// frame that spans multiple reads and emits the whole frame as ONE event so
/// xterm receives it in a single write (its RenderDebouncer then paints one
/// reflow). The common case — a whole frame in one read — never holds: the
/// parser's `sync_output` is already false post-advance, so [`Self::feed`]
/// emits immediately.
///
/// `feed` is called once per read with the post-interceptor `data`, that
/// data's stamped stream offset, and the live `sync_output` parser state
/// observed AFTER the data was advanced into the grid. The `emit` callback
/// receives `(payload, offset)` to broadcast as one event; `offset` is the
/// absolute stream offset of the payload's first byte (a correct replay
/// boundary — the first held byte's offset for a coalesced frame).
struct SyncFrameCoalescer {
    pending: Vec<u8>,
    pending_offset: u64,
    started_at: Option<std::time::Instant>,
}

impl SyncFrameCoalescer {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_offset: 0,
            started_at: None,
        }
    }

    /// Feed one read's processed `data` (stamped at `offset`) plus the live
    /// `in_sync` parser state observed right after advancing it into the grid.
    /// Emits zero or one coalesced event via `emit`.
    fn feed<F: FnMut(&[u8], u64)>(&mut self, data: &[u8], offset: u64, in_sync: bool, mut emit: F) {
        if in_sync {
            // Mid-frame: hold this chunk. Stamp the held frame's offset on the
            // FIRST held byte (a correct replay boundary).
            if self.pending.is_empty() {
                self.pending_offset = offset;
                self.started_at = Some(std::time::Instant::now());
            }
            self.pending.extend_from_slice(data);
            // Byte-cap a runaway/never-closed block.
            if self.pending.len() >= SYNC_FLUSH_BYTE_CAP {
                emit(&self.pending, self.pending_offset);
                self.pending.clear();
                self.started_at = None;
            }
        } else if self.pending.is_empty() {
            // Common case: no open frame, nothing held — emit immediately.
            emit(data, offset);
        } else {
            // The block just closed in this read (it carried `?2026l`): emit
            // the held prefix + this chunk as the single frame, stamped at the
            // held frame's start offset.
            self.pending.extend_from_slice(data);
            emit(&self.pending, self.pending_offset);
            self.pending.clear();
            self.started_at = None;
        }
    }

    /// Flush a held frame if it has been open at least [`SYNC_FLUSH_TIME_CAP`].
    /// Called before each (blocking) read so a slow producer mid-frame still
    /// flushes within ~one frame rather than waiting for the next byte.
    fn flush_if_timed_out<F: FnMut(&[u8], u64)>(&mut self, mut emit: F) {
        if let Some(started) = self.started_at {
            if started.elapsed() >= SYNC_FLUSH_TIME_CAP && !self.pending.is_empty() {
                emit(&self.pending, self.pending_offset);
                self.pending.clear();
                self.started_at = None;
            }
        }
    }

    /// Flush any remaining held frame (reader exit on EOF / read error) so the
    /// last frame of a never-closed block isn't lost.
    fn flush_remaining<F: FnMut(&[u8], u64)>(&mut self, mut emit: F) {
        if !self.pending.is_empty() {
            emit(&self.pending, self.pending_offset);
            self.pending.clear();
            self.started_at = None;
        }
    }
}

/// Tauri `terminal-output` wire shape: the shared `TerminalOutputEvent`
/// (qontinui-types) is `deny_unknown_fields`, so the extra `offset` rides on
/// this runner-local struct instead of forcing a schemas version bump. Only
/// the runner frontend consumes the Tauri event; the backend-relay broadcast
/// keeps its own offset-free payload.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputWire<'a> {
    terminal_id: &'a str,
    data: &'a str,
    /// Absolute byte offset of this chunk's first byte in the session's
    /// output stream (the value of `total_bytes_produced` before this chunk
    /// was teed). Lets the frontend dedup a scrollback-ring replay against
    /// live chunks — see `terminal_get_scrollback` and the frontend's
    /// `scrollbackReplay.ts`.
    offset: u64,
}

/// ANSI bracketed-paste begin marker.
const BRACKETED_PASTE_BEGIN: &[u8] = b"\x1b[200~";
/// ANSI bracketed-paste end marker.
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
/// Bare CR — the submit keystroke after the paste window closes.
/// Intentionally NOT `\r\n` (see [`TerminalSession::submit_prompt`]).
const SUBMIT_ENTER: &[u8] = b"\r";

/// Delay between the bracketed-paste block and the trailing submit keystroke.
///
/// **Why this exists:** the original `submit_prompt` wrote
/// `\x1b[200~<msg>\x1b[201~\r` as four `write_all`s under a single locked
/// writer with one final `flush`. From Claude Code's readline perspective
/// these landed in one read cycle — its bracketed-paste handler consumed
/// the begin/body/end and then ate the trailing `\r` as paste-tail bleed,
/// never submitting. Empirically reproduced on §6 E2E 2026-05-10
/// (`bracketed-paste-submit-doesnt-fully-submit.md`): brief visible in
/// the input area, no `❯` cursor at the end, no working indicators, task
/// stuck at `assigned`. A SEPARATE write of `\r` via `/terminals/{id}/write`
/// did submit, confirming the issue is timing / single-read-cycle
/// consumption, not the byte itself.
///
/// 150ms is enough for one Claude readline cycle on typical hardware.
/// Tunable: bump if E2E shows submits still racing.
const POST_PASTE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

/// Build the exact byte sequence [`TerminalSession::submit_prompt`] writes.
/// Exposed so tests (and the worker_session unit test) can assert the
/// submit framing without spinning up a real PTY.
pub(crate) fn build_submit_payload(message: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        BRACKETED_PASTE_BEGIN.len()
            + message.len()
            + BRACKETED_PASTE_END.len()
            + SUBMIT_ENTER.len(),
    );
    out.extend_from_slice(BRACKETED_PASTE_BEGIN);
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(BRACKETED_PASTE_END);
    out.extend_from_slice(SUBMIT_ENTER);
    out
}

/// Pure line-assembly core of the typed-input observer
/// ([`TerminalSession::observe_input`]): walk one chunk of raw keystroke
/// bytes, mutating the per-terminal line buffer, and return any lines this
/// chunk completed (CR/LF-submitted, non-blank). Heuristic by design:
/// backspace/DEL pop one char, printable ASCII accumulates, control bytes
/// and non-ASCII are dropped. Capped at 4 KiB so a pathological no-newline
/// stream (e.g. a huge paste) can't grow unbounded.
fn consume_input_bytes(buf: &mut String, data: &[u8]) -> Vec<String> {
    let mut completed: Vec<String> = Vec::new();
    for &b in data {
        match b {
            // CR or LF — submit the line.
            0x0D | 0x0A => {
                if !buf.trim().is_empty() {
                    completed.push(std::mem::take(buf));
                } else {
                    buf.clear();
                }
            }
            // Backspace / DEL — approximate edit handling.
            0x08 | 0x7F => {
                buf.pop();
            }
            // Printable ASCII (incl. space).
            0x20..=0x7E => buf.push(b as char),
            _ => {}
        }
    }
    if buf.len() > 4096 {
        buf.clear();
    }
    completed
}

/// A single PTY-backed terminal session.
pub struct TerminalSession {
    /// Unique identifier for this terminal.
    id: TerminalId,
    /// Display title. Mutated post-spawn by [`Self::set_title`] (Phase 2 of
    /// the bi-directional title sync — frontend OSC 0 observers in xterm.js
    /// call back via `terminal_set_title` Tauri command).
    title: Arc<Mutex<String>>,
    /// Working directory the shell was started in.
    working_dir: String,
    /// Which terminal page this session belongs to.
    page_id: String,
    /// Thread-safe writer to PTY stdin.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Handle to the PTY master (needed for resize).
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process PID.
    child_pid: Option<u32>,
    /// Current terminal dimensions (atomic for lock-free resize from &self).
    cols: AtomicU16,
    rows: AtomicU16,
    /// Whether the shell process is still alive.
    is_alive: Arc<AtomicBool>,
    /// Exit code (set when process exits).
    exit_code: Arc<Mutex<Option<i32>>>,
    /// Handle to the reader thread (for join on cleanup).
    reader_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Handle to the waiter thread (for join on cleanup).
    waiter_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Bytes received by the frontend (for flow control).
    bytes_sent: Arc<AtomicU64>,
    bytes_acked: Arc<AtomicU64>,
    /// Ring buffer of recent raw PTY output for reconnection.
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// Monotonic counter of all bytes ever produced by the PTY.
    total_bytes_produced: Arc<AtomicU64>,
    /// Unix timestamp in milliseconds when the session was created.
    created_at: u64,
    /// Broadcast channel for HTTP/SSE subscribers to receive base64-encoded output chunks.
    output_tx: broadcast::Sender<String>,
    /// Server-side cell grid produced by the VT parser tee in the reader thread.
    grid: Arc<Mutex<Grid>>,
    /// One-shot sender fired by the reader thread when it observes the
    /// first OSC 0 / OSC 2 title from the child process. Used by
    /// `spawn_worker_session` (Phase 1) to gate `Initializing → Ready` on
    /// readline visibility: Claude Code's CLI emits an OSC 0 title
    /// (`"✳ Claude Code"`) on startup, so this resolves ~150–300 ms after
    /// child spawn. Wrapped in `Mutex<Option<...>>` because the sender is
    /// consumed (`take()`'d) on first fire — subsequent OSC 0s do not
    /// re-fire.
    first_osc_title_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// One-shot receiver, taken at most once by
    /// [`Self::subscribe_first_osc_title`]. After the take, callers that
    /// ask again get `None` and should treat the worker as already ready
    /// (the OSC may have fired before they could subscribe, in which case
    /// the sender already consumed the slot above).
    first_osc_title_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    /// Coord-native session id, set after `register_external()` wires this
    /// terminal into the coordinator's session plane. `None` until wired;
    /// read by `terminal_close` so it can close the coord mirror.
    coord_session_id: Arc<Mutex<Option<uuid::Uuid>>>,
    /// R1 (session-lifecycle-cleanup) — best-effort hook invoked by the
    /// waiter thread the instant the backing PTY process exits. Wired by
    /// `terminal_create` alongside [`Self::set_coord_session_id`]: it
    /// closes the coord session mirror for `coord_session_id` so a process
    /// that dies (operator types `exit`, shell crashes, etc.) doesn't leave
    /// a ghost `active` row on the dashboard until coord's own stale→closed
    /// watcher reaps it (the runner no longer self-closes abandoned
    /// sessions; see coord_sync plan A3). Held in a shared
    /// slot so the already-spawned waiter thread can read it once the
    /// registration completes; cloned into the waiter at spawn time.
    /// Idempotent against the frontend `terminal_close` path because
    /// `SessionRegistry::close` is itself idempotent.
    on_exit: Arc<Mutex<Option<Box<dyn Fn(uuid::Uuid) + Send + Sync>>>>,
    /// Phase 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
    /// When the session declared edit intent on a registered repo and
    /// `worktree_mode_enabled()` was true at spawn time, this carries
    /// the allocated `IsolatedEditContext`. The context's `Drop` impl
    /// stops the heartbeat task and fires a best-effort claim release;
    /// `close()` clears the slot first so release fires before PTY
    /// teardown.
    isolated_edit_ctx:
        Arc<Mutex<Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>>>,
    /// App handle, retained so the typed-input observer
    /// ([`Self::observe_input`]) can dispatch its effects (coord warning
    /// event; lifecycle-store registration + bypass event of the typed
    /// claude resume sniff) off the PTY write path.
    /// `None` only in unit-test fixtures that don't drive a real app.
    app_handle: Option<AppHandle>,
    /// Accumulates printable keystroke bytes between line submits so
    /// completed lines can be matched by the typed-input consumers (L3
    /// branch-mutating-git warn; typed claude resume sniff). Drained on
    /// CR/LF; see [`consume_input_bytes`]. Cheap on the hot path.
    input_line_buf: Arc<Mutex<String>>,
}

impl TerminalSession {
    /// Spawn a new terminal session with a shell process.
    ///
    /// `command` is an optional program-and-args override (Decision 3 of
    /// `plans/2026-06-05-visible-gate-continuations-and-plan-ready-predicate.md`):
    /// when `Some([program, args…])` the session runs that program as its PTY
    /// child instead of the interactive shell — this is how the gate-continuation
    /// terminal branch launches the `claude` CLI directly with the prompt as
    /// argv. When `None` (every operator-opened / frontend terminal), the session
    /// falls back to [`Self::build_shell_command`] byte-for-byte, so the
    /// interactive-terminal path is untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: TerminalId,
        title: String,
        working_dir: String,
        page_id: String,
        cols: u16,
        rows: u16,
        app_handle: AppHandle,
        interceptor: Arc<OutputInterceptor>,
        command: Option<Vec<String>>,
        extra_env: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;

        // Build the PTY child command: an explicit program+args override
        // (Decision 3) when supplied, else the interactive shell.
        let mut cmd = Self::build_command_from(command);

        // Set working directory
        let cwd = if working_dir.is_empty() {
            dirs::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        } else {
            working_dir.clone()
        };
        cmd.cwd(&cwd);

        // Remove CLAUDECODE env var so Claude CLI works inside the terminal
        cmd.env_remove("CLAUDECODE");

        // Set TERM for proper color/capability support.
        // xterm.js is a full xterm-compatible terminal, so use xterm-256color on all
        // platforms. The previous "cygwin" setting on Windows caused issues with tools
        // like Claude Code that check TERM for capability detection.
        cmd.env("TERM", "xterm-256color");

        // Mark this terminal as running inside the Qontinui Runner so that tools
        // (e.g. Claude Code via the shell integration wrapper) can detect the context.
        cmd.env("QONTINUI_RUNNER_TERMINAL", "1");
        cmd.env(
            "QONTINUI_RUNNER_API_PORT",
            crate::mcp::types::get_mcp_api_port().to_string(),
        );

        // Phase 2c — caller-supplied launch env (e.g.
        // `QONTINUI_SESSION_WORKTREES`, the agent-agnostic pointer to every
        // materialized sibling worktree of this session). Set after the
        // built-in runner vars so a caller can intentionally override them.
        let caller_pinned_config_dir = extra_env
            .as_ref()
            .is_some_and(|env| env.iter().any(|(k, _)| k == "CLAUDE_CONFIG_DIR"));
        if let Some(env) = extra_env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        // ---- ALWAYS-ON session-restore identity seam (plan §3b) -------------
        // NOT gated by any flag — the out-of-box session-restore guarantee
        // (Principle 2) must apply to every user with zero setup. Generates a
        // per-terminal session UUID, injects it + the terminal id as env,
        // materializes the always-on `claude`/`gemini` identity shims and
        // prepends their dir to PATH, and records the session AUTHORITATIVELY at
        // spawn (zero transcript race — the §3b determinism mechanism). Runs
        // AFTER caller `extra_env` so the identity dir wins on PATH. Fail-open:
        // any failure injects nothing and the terminal still spawns.
        Self::apply_identity_seam(&mut cmd, &id, &app_handle, &cwd, &title, &page_id);

        // ---- Install-interception PATH-shim seam (plan §4 Phase 1) ----------
        // Behind the master flag `QONTINUI_INSTALL_INTERCEPT_ENABLED` (default
        // OFF — ships dark). SEPARATE from the always-on identity seam above:
        // they SHARE the materializer/seam/loopback plumbing but NOT the master
        // enable flag (robustness — the out-of-box guarantee cannot ride a
        // default-dark flag). When OFF this block is a no-op. When ON,
        // materialize the per-terminal install-shim bin dir and prepend it to
        // PATH + set the loopback port/mode so an agent that types `npm install …`
        // is transparently declared/observed. Fail-open: any materialize failure
        // injects nothing.
        Self::apply_install_intercept_env(&mut cmd, &id);

        // Set CLAUDE_CONFIG_DIR so Claude Code uses the resolved account
        // (multi-account support with auto-rotation on rate-limit) — UNLESS
        // the caller already pinned an account via `extra_env`. A caller pin
        // (backend continuation spawns, account-migration respawns) is a
        // deliberate per-session choice and must not be clobbered by the
        // process-global resolved dir, which may point at a different (or
        // freshly-exhausted) account.
        if !caller_pinned_config_dir {
            let ai_settings = crate::settings::get_ai_settings();
            if let Some(config_dir) =
                crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli)
            {
                cmd.env("CLAUDE_CONFIG_DIR", &config_dir);
            }
        }

        // Spawn the child process
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {}", e))?;

        let child_pid = child.process_id();
        info!(
            terminal_id = %id,
            pid = ?child_pid,
            cwd = %cwd,
            "Terminal session spawned"
        );

        // Assign to Windows Job Object for crash safety
        #[cfg(target_os = "windows")]
        if let Some(pid) = child_pid {
            Self::assign_to_job_object(pid);
        }

        // Get writer and master from the PTY pair
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {}", e))?;
        let writer = Arc::new(Mutex::new(writer));

        let is_alive = Arc::new(AtomicBool::new(true));
        let exit_code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let bytes_sent = Arc::new(AtomicU64::new(0));
        let bytes_acked = Arc::new(AtomicU64::new(0));
        let scrollback_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(SCROLLBACK_CAPACITY)));
        let total_bytes_produced = Arc::new(AtomicU64::new(0));
        let grid = Arc::new(Mutex::new(Grid::new(cols, rows)));
        let (output_tx, _) = broadcast::channel::<String>(256);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Phase 1: one-shot fired by the reader thread on the first OSC
        // 0/2 title transition. The session owns the receiver until a
        // caller (`spawn_worker_session`) takes it via
        // `subscribe_first_osc_title`; the reader owns the sender via
        // an Arc<Mutex<Option<...>>> slot and `take`s it on first fire.
        let (osc_title_tx, osc_title_rx) = oneshot::channel::<()>();
        let first_osc_title_tx = Arc::new(Mutex::new(Some(osc_title_tx)));
        let first_osc_title_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>> =
            Arc::new(Mutex::new(Some(osc_title_rx)));

        // Get a reader from the master PTY
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;

        // Spawn reader thread: reads PTY output → interceptor → scrollback + Tauri event
        let reader_id = id.clone();
        let reader_app = app_handle.clone();
        let reader_alive = is_alive.clone();
        let reader_bytes_sent = bytes_sent.clone();
        let reader_bytes_acked = bytes_acked.clone();
        let reader_scrollback = scrollback_buffer.clone();
        let reader_total_bytes = total_bytes_produced.clone();
        let reader_output_tx = output_tx.clone();
        let reader_grid = grid.clone();
        let reader_osc_title_tx = first_osc_title_tx.clone();
        let reader_handle = thread::Builder::new()
            .name(format!("terminal-reader-{}", &id))
            .spawn(move || {
                let mut parser = vte::Parser::new();
                let mut buf = [0u8; 8192];
                // Phase 4 — sync-output (DEC 2026) frame coalescing. While the
                // VT parser is inside a `?2026h … ?2026l` block (Claude's TUI
                // brackets each full-frame redraw this way), accumulate the
                // processed bytes here instead of emitting per-read. Once the
                // block closes — or a safety cap trips so a never-closed block
                // can't stall output — emit the whole frame as ONE
                // `terminal-output` event so xterm receives the frame in a
                // single write and its RenderDebouncer paints one reflow.
                //
                // The common case (a whole frame in one read) is unaffected:
                // `sync_output` is already false post-advance, so we emit
                // immediately with no holding. Only frames that span multiple
                // reads are held.
                let mut coalescer = SyncFrameCoalescer::new();
                // Flow-control hysteresis state: once the unacked gap crosses
                // the High watermark we stay paused until it falls back under
                // Low (see FLOW_HIGH_WATERMARK / FLOW_LOW_WATERMARK).
                let mut paused = false;

                // Emit one `terminal-output` event for `payload` stamped at
                // absolute `offset`, mirror it to SSE + the backend relay, and
                // advance the flow-control "sent" counter by the emitted length
                // (so backpressure tracks bytes actually delivered to the
                // frontend, not bytes still held in the coalescer).
                let emit_chunk = |payload: &[u8], offset: u64| {
                    let encoded = STANDARD.encode(payload);
                    // Broadcast to HTTP/SSE subscribers (ignore if no receivers)
                    let _ = reader_output_tx.send(encoded.clone());
                    let event = TerminalOutputWire {
                        terminal_id: &reader_id,
                        data: &encoded,
                        offset,
                    };
                    if let Err(e) = reader_app.emit("terminal-output", &event) {
                        warn!(
                            terminal_id = %reader_id,
                            error = %e,
                            "Failed to emit terminal output event"
                        );
                    }
                    // Broadcast to backend relay for remote mobile access
                    crate::event_system::broadcast_ws_notification(
                        &reader_app,
                        "terminal-output",
                        &serde_json::json!({
                            "terminal_id": &reader_id,
                            "data": &event.data,
                        }),
                    );
                    reader_bytes_sent.fetch_add(payload.len() as u64, Ordering::Relaxed);
                };

                loop {
                    if !reader_alive.load(Ordering::Relaxed) {
                        break;
                    }

                    // Flow control (Phase 3): char-count watermarks with
                    // hysteresis, mirroring VS Code. The gap is measured
                    // against EMITTED bytes (a held frame in the coalescer
                    // hasn't been sent yet), so a held frame doesn't trip
                    // backpressure on its own. Pause once the unacked gap
                    // exceeds High; resume only once it drops below Low — so a
                    // burst that overruns xterm's input buffer is throttled at
                    // the producer until the renderer's acks catch up.
                    let sent = reader_bytes_sent.load(Ordering::Relaxed);
                    let acked = reader_bytes_acked.load(Ordering::Relaxed);
                    let gap = sent.saturating_sub(acked);
                    if paused {
                        if gap > FLOW_LOW_WATERMARK {
                            thread::sleep(std::time::Duration::from_millis(10));
                            continue;
                        }
                        paused = false;
                    } else if gap > FLOW_HIGH_WATERMARK {
                        paused = true;
                        thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }

                    // Time-cap a held sync block: if the block has been open ≥
                    // SYNC_FLUSH_TIME_CAP, flush it now rather than waiting for
                    // the next read to return. (The read below blocks, so a
                    // slow PTY mid-frame could otherwise hold a frame past one
                    // frame interval until the next byte arrives.)
                    coalescer.flush_if_timed_out(&emit_chunk);

                    match reader.read(&mut buf) {
                        Ok(0) => {
                            debug!(terminal_id = %reader_id, "PTY reader got EOF");
                            break;
                        }
                        Ok(n) => {
                            let data = interceptor.process(&reader_id, &buf[..n]);

                            // Tee processed output into the per-session
                            // scrollback ring buffer + byte counter. Shared
                            // with the distinct-buffers regression test so
                            // both drive the identical teeing path. The
                            // returned start offset is stamped onto the
                            // event below for replay dedup.
                            let chunk_offset =
                                tee_into_scrollback(&reader_scrollback, &reader_total_bytes, &data);

                            // Tee through the VT parser into the per-session cell grid.
                            // Detect the first OSC 0/2 title transition by
                            // checking whether the grid's title became
                            // `Some` *during* this parser advance. The
                            // sender lives in an `Arc<Mutex<Option<...>>>`
                            // slot we drain on first fire — subsequent
                            // title changes don't re-fire. Worker dispatch
                            // gating in `spawn_worker_session` only needs
                            // the one-shot signal.
                            let title_was_none = reader_grid
                                .lock()
                                .ok()
                                .map(|g| g.title().is_none())
                                .unwrap_or(false);
                            if let Ok(mut g) = reader_grid.lock() {
                                let mut perf = GridPerformer::new(&mut g);
                                parser.advance(&mut perf, &data);
                            }
                            if title_was_none {
                                let title_is_now_some = reader_grid
                                    .lock()
                                    .ok()
                                    .map(|g| g.title().is_some())
                                    .unwrap_or(false);
                                if title_is_now_some {
                                    if let Ok(mut slot) = reader_osc_title_tx.lock() {
                                        if let Some(tx) = slot.take() {
                                            // Receiver may have been
                                            // dropped (caller didn't
                                            // subscribe). Ignore — the
                                            // sender simply discards the
                                            // signal.
                                            let _ = tx.send(());
                                        }
                                    }
                                }
                            }

                            // Sync-output-aware emit (Phase 4). Read the live
                            // DEC-2026 state straight after the advance above:
                            // if a `?2026h` is still open we're mid-frame, so
                            // accumulate and defer the emit; otherwise flush
                            // (any held prefix + this chunk) as one event.
                            let in_sync = reader_grid
                                .lock()
                                .ok()
                                .map(|g| g.sync_output())
                                .unwrap_or(false);

                            // Coalesce sync-output frames (Phase 4). The
                            // scrollback ring + total counter were already fed
                            // per-read above, so total-byte accounting stays
                            // correct whether or not the coalescer holds.
                            coalescer.feed(&data, chunk_offset, in_sync, &emit_chunk);
                        }
                        Err(e) => {
                            // On Windows, the PTY reader returns an error when the child exits
                            debug!(terminal_id = %reader_id, error = %e, "PTY read error (likely process exit)");
                            break;
                        }
                    }
                }
                // Flush any frame still held in a never-closed sync block so
                // the last frame isn't lost when the reader exits (EOF / read
                // error on child exit).
                coalescer.flush_remaining(&emit_chunk);
                debug!(terminal_id = %reader_id, "Reader thread exiting");
            })
            .map_err(|e| format!("Failed to spawn reader thread: {}", e))?;

        // R1 — shared slots the waiter thread reads on PTY exit so it can
        // close the coord session mirror immediately (vs. leaving it for
        // coord's stale→closed watcher; see coord_sync plan A3). Both are
        // populated AFTER spawn by
        // `terminal_create` once `register_external` returns the coord id,
        // so the waiter reads them at exit time rather than capturing a
        // value that isn't known yet at spawn.
        let coord_session_id: Arc<Mutex<Option<uuid::Uuid>>> = Arc::new(Mutex::new(None));
        let on_exit: Arc<Mutex<Option<Box<dyn Fn(uuid::Uuid) + Send + Sync>>>> =
            Arc::new(Mutex::new(None));

        // Spawn waiter thread: detects process exit
        let waiter_id = id.clone();
        let waiter_title = title.clone();
        let waiter_alive = is_alive.clone();
        let waiter_exit = exit_code.clone();
        // Retain a clone for the session struct (input-line warn hook)
        // before the original handle is moved into the waiter thread.
        let session_app_handle = app_handle.clone();
        let waiter_app = app_handle;
        let waiter_coord_session_id = coord_session_id.clone();
        let waiter_on_exit = on_exit.clone();
        let waiter_handle = thread::Builder::new()
            .name(format!("terminal-waiter-{}", &id))
            .spawn(move || {
                // portable-pty's child is not Send, so we must wait in the thread that has it
                let mut child = child;
                let status = child.wait();
                let code = match status {
                    Ok(exit) => {
                        // ExitStatus doesn't expose the code directly on all platforms
                        // via portable-pty. Use success() check.
                        if exit.success() {
                            Some(0)
                        } else {
                            // Try to get the exit code; fall back to 1 for non-zero
                            Some(1)
                        }
                    }
                    Err(e) => {
                        warn!(terminal_id = %waiter_id, error = %e, "Failed to wait on child process");
                        None
                    }
                };

                waiter_alive.store(false, Ordering::Relaxed);
                if let Ok(mut ec) = waiter_exit.lock() {
                    *ec = code;
                }

                info!(terminal_id = %waiter_id, exit_code = ?code, "Terminal process exited");

                let event = TerminalExitEvent {
                    terminal_id: waiter_id.clone(),
                    exit_code: code,
                };
                if let Err(e) = waiter_app.emit("terminal-exit", &event) {
                    warn!(
                        terminal_id = %waiter_id,
                        error = %e,
                        "Failed to emit terminal exit event"
                    );
                }
                // Broadcast to backend relay for remote mobile access
                crate::event_system::broadcast_ws_notification(
                    &waiter_app,
                    "terminal-exit",
                    &serde_json::json!({
                        "terminal_id": &waiter_id,
                        "exit_code": code,
                    }),
                );

                // Push notification for mobile (fire-and-forget)
                crate::commands::workflow_events::emit_terminal_exited(
                    &waiter_id,
                    &waiter_title,
                    code,
                );

                // R1 — close the coord session mirror the instant the PTY
                // process exits, instead of letting it linger `active`
                // until coord's own stale→closed watcher reaps it (the
                // runner no longer self-closes abandoned sessions; see
                // coord_sync plan A3). Read the coord id + hook
                // populated by `terminal_create` after registration; if the
                // terminal was never wired into coord (registration failed,
                // or close raced ahead via the frontend `terminal_close`
                // path which clears nothing here but is idempotent on the
                // registry side), there's simply nothing to do. The hook
                // calls `SessionRegistry::close_by_id`, which is idempotent
                // — a no-op when the session is already Closed.
                let coord_id = waiter_coord_session_id
                    .lock()
                    .ok()
                    .and_then(|slot| *slot);
                if let Some(coord_id) = coord_id {
                    if let Ok(slot) = waiter_on_exit.lock() {
                        if let Some(cb) = slot.as_ref() {
                            debug!(
                                terminal_id = %waiter_id,
                                coord_session = %coord_id,
                                "terminal exit — closing coord session mirror"
                            );
                            cb(coord_id);
                        }
                    }
                }

                // P1 (close-on-clean-exit): on a CLEAN exit only, close the
                // owning `term-N` pop-out window iff this was its last live
                // session. A non-zero / unknown exit stays visible (honesty).
                // Docked sessions (owner "main") and the main window are never
                // auto-closed. Best-effort + cheap; guarded inside the helper.
                if let Some(assignments) = tauri::Manager::try_state::<
                    std::sync::Arc<crate::window_assignments::WindowAssignments>,
                >(&waiter_app)
                {
                    crate::commands::terminal_windows::auto_close_owner_window_if_empty(
                        &waiter_app,
                        assignments.inner(),
                        &waiter_id,
                        code,
                    );
                }
            })
            .map_err(|e| format!("Failed to spawn waiter thread: {}", e))?;

        // Store the master for resize operations
        let master: Box<dyn MasterPty + Send> = pair.master;

        Ok(Self {
            id,
            title: Arc::new(Mutex::new(title)),
            working_dir: cwd,
            page_id,
            writer,
            master: Arc::new(Mutex::new(master)),
            child_pid,
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            is_alive,
            exit_code,
            reader_join: Mutex::new(Some(reader_handle)),
            waiter_join: Mutex::new(Some(waiter_handle)),
            bytes_sent,
            bytes_acked,
            scrollback_buffer,
            total_bytes_produced,
            created_at,
            output_tx,
            grid,
            first_osc_title_tx,
            first_osc_title_rx,
            coord_session_id,
            on_exit,
            isolated_edit_ctx: Arc::new(Mutex::new(None)),
            app_handle: Some(session_app_handle),
            input_line_buf: Arc::new(Mutex::new(String::new())),
        })
    }

    /// Build the PTY child [`CommandBuilder`] from an optional program+args
    /// override (Decision 3).
    ///
    /// - `Some([program, args…])` → run `program` directly with `args` as the
    ///   session's child. The gate-continuation terminal branch uses this to
    ///   launch `claude "<prompt>"` so the prompt is visible in scrollback and
    ///   the session behaves identically to the operator launching it (no
    ///   PTY-readiness race, no shell wrapping).
    /// - `Some([])` (empty) → no program to run; fall back to the shell so we
    ///   never spawn an empty command.
    /// - `None` → [`Self::build_shell_command`], the interactive-shell path
    ///   every operator-opened terminal takes. Back-compat by construction.
    fn build_command_from(command: Option<Vec<String>>) -> CommandBuilder {
        match command {
            Some(parts) if !parts.is_empty() => {
                let mut cmd = CommandBuilder::new(&parts[0]);
                for arg in &parts[1..] {
                    cmd.arg(arg);
                }
                cmd
            }
            _ => Self::build_shell_command(),
        }
    }

    /// Build the platform-appropriate shell command, injecting shell integration if possible.
    fn build_shell_command() -> CommandBuilder {
        #[cfg(target_os = "windows")]
        {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // Try to write and source the integration script. Fall back to plain -NoExit on failure.
            if let Some(script_path) =
                write_integration_script(PS1_INTEGRATION, "qontinui-shell-integration.ps1")
            {
                // -Command mode: dot-source the script then keep shell alive.
                // The script itself sources $PROFILE and sets up OSC 633 hooks.
                let source_cmd = format!(". '{}'", script_path.display());
                cmd.args(["-NoLogo", "-NoExit", "-Command", source_cmd.as_str()]);
            } else {
                cmd.args(["-NoLogo", "-NoExit"]);
            }
            cmd
        }
        #[cfg(not(target_os = "windows"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let shell_name = std::path::Path::new(&shell)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let mut cmd = CommandBuilder::new(&shell);
            // Shell integration is shell-specific. `--rcfile` is a BASH flag —
            // passing it to zsh (the macOS default) fails with
            // "no such option: rcfile", so each shell needs its own mechanism.
            match shell_name {
                "zsh" => {
                    // zsh: point ZDOTDIR at a dir whose `.zshrc` is our
                    // integration (it re-sources the user's ~/.z* files). If we
                    // can't write it, fall through to a plain zsh that sources
                    // ~/.zshrc normally (no OSC 633, but a working terminal).
                    if let Some(zdotdir) = write_zsh_zdotdir(ZSH_INTEGRATION) {
                        cmd.env("ZDOTDIR", zdotdir.to_string_lossy().as_ref());
                    }
                }
                // bash (and the empty/unknown-path default) keep the --rcfile
                // path, with --login as the no-script fallback.
                "bash" | "" => {
                    if let Some(script_path) = write_integration_script(
                        BASH_INTEGRATION,
                        "qontinui-shell-integration.bash",
                    ) {
                        cmd.arg("--rcfile");
                        cmd.arg(script_path.to_string_lossy().as_ref());
                    } else {
                        cmd.arg("--login");
                    }
                }
                // Any other shell (fish, etc.): launch it plainly — never pass
                // bash-only flags it would reject.
                _ => {}
            }
            cmd
        }
    }

    /// The canonical PATH env-var key for the PTY child process. Windows env is
    /// case-insensitive with undefined duplicate-key resolution, so a shim
    /// prepended under a different casing than the inherited key can be shadowed
    /// by the original. We therefore always set the SAME casing the OS itself
    /// uses (`Path` on Windows, `PATH` elsewhere) and remove every inherited
    /// case-variant first (see [`Self::set_child_path`]).
    const PATH_ENV_KEY: &'static str = if cfg!(windows) { "Path" } else { "PATH" };

    /// Set the child's path env var to `value` under the single canonical key,
    /// removing every case-variant first so the child has EXACTLY ONE path key.
    ///
    /// Why this matters on Windows: the runner's own env var is `Path`;
    /// `portable_pty`'s `CommandBuilder` seeds the child from the parent env and
    /// a naive `cmd.env("PATH", …)` used to be able to leave the child with both
    /// an inherited `Path` (no shim) and an added `PATH` (shimmed), letting the
    /// loader resolve tools via the original `Path` and bypass the identity
    /// shim. Removing all three variants then setting the OS-preferred casing
    /// guarantees the shim-prepended value is the one Windows actually reads.
    ///
    /// (portable_pty 0.8 folds env keys case-insensitively on Windows, so this
    /// is belt-and-suspenders there; the removal is a harmless no-op when no
    /// variant is present, and the fix is correct regardless of the library's
    /// internal folding.)
    fn set_child_path(cmd: &mut CommandBuilder, value: &str) {
        cmd.env_remove("PATH");
        cmd.env_remove("Path");
        cmd.env_remove("path");
        cmd.env(Self::PATH_ENV_KEY, value);
    }

    /// Extract an explicit `--session-id <id>` (or `--session-id=<id>`) from a
    /// built PTY child command's argv, if present. Used by the identity seam to
    /// adopt the id a runner-launched direct-command spawn actually carries as
    /// its authoritative pin — so the recorded id equals the id the session runs
    /// under (no phantom). Returns `None` for the interactive-shell path, whose
    /// argv is the shell program and carries no `--session-id`.
    fn explicit_session_id_from(cmd: &CommandBuilder) -> Option<String> {
        let argv = cmd.get_argv();
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            let s = arg.to_string_lossy();
            if let Some(rest) = s.strip_prefix("--session-id") {
                // Attached form: `--session-id=<id>`.
                if let Some(id) = rest.strip_prefix('=') {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
                // Space-separated form: `--session-id <id>`.
                if rest.is_empty() {
                    if let Some(next) = it.next() {
                        let id = next.to_string_lossy();
                        if !id.is_empty() {
                            return Some(id.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// Install-interception env-seam (plan §4 Phase 1). Behind the master flag
    /// `QONTINUI_INSTALL_INTERCEPT_ENABLED` (default OFF). When ON, materialize
    /// the per-terminal PATH-shim bin dir and mutate the child `cmd`:
    /// prepend the shim dir to `PATH`, set `QONTINUI_INSTALL_INTERCEPT_PORT`
    /// (the BOUND runner port) + `QONTINUI_INSTALL_INTERCEPT_MODE` (the
    /// runner-env-resolved `observe`/`gate` — plan §3 step 3; default observe).
    ///
    /// Fail-open: a materialize failure injects nothing (the terminal still
    /// spawns, un-shimmed). When OFF, this is a pure no-op — the child env is
    /// byte-identical to pre-interception.
    ///
    /// The prepended value is written under the OS-canonical path key via
    /// [`Self::set_child_path`], which first removes every inherited case-variant
    /// (`PATH`/`Path`/`path`) — on Windows the runner env var is `Path`, so
    /// setting a bare `"PATH"` could leave the original `Path` (un-shimmed) to
    /// win the loader's case-insensitive resolution.
    fn apply_install_intercept_env(cmd: &mut CommandBuilder, terminal_id: &str) {
        use crate::install_effects_producer::intercept::shim_materializer;

        if !shim_materializer::intercept_enabled() {
            return; // ships dark — byte-identical child env
        }
        let base_dir = std::env::temp_dir();
        let port = crate::install_effects_producer::intercept::bound_port();
        // The MODE the terminal gets is resolved from the runner's OWN env
        // (`QONTINUI_INSTALL_INTERCEPT_MODE`; default observe, `gate` enables
        // Phase-3 gating, anything else fails open to observe — plan §3 step 3).
        let mode = shim_materializer::resolve_mode();
        let seam = match shim_materializer::materialize(&base_dir, terminal_id, port, mode) {
            Some(s) => s,
            None => return, // fail-open: couldn't write shims ⇒ inject nothing
        };

        let current_path = std::env::var("PATH").ok();
        let new_path = shim_materializer::prepend_path(&seam.shim_dir, current_path.as_deref());
        Self::set_child_path(cmd, &new_path);
        cmd.env(shim_materializer::PORT_ENV, seam.port.to_string());
        cmd.env(shim_materializer::MODE_ENV, &seam.mode);
        // Tell the compiled `.exe` stub its own dir so its real-tool PATH scan
        // can skip it even if `current_exe()` is unavailable (plan §6 / §4
        // Phase 4 — the `qontinui-shim` stub reads this).
        cmd.env(
            "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR",
            seam.shim_dir.to_string_lossy().as_ref(),
        );
        info!(
            terminal_id = %terminal_id,
            shim_dir = %seam.shim_dir.display(),
            port = seam.port,
            mode = %seam.mode,
            "install-intercept: shim dir injected onto terminal PATH"
        );
    }

    /// Always-on session-restore identity seam (plan §3b). NOT gated by any
    /// flag — the out-of-box guarantee. For EVERY terminal:
    ///
    /// 1. Generate a per-terminal session UUID (`QONTINUI_PINNED_SESSION_ID`).
    /// 2. Inject it + `QONTINUI_TERMINAL_ID` + the bound loopback port into the
    ///    child env (the identity shim reads them to pin + confirm).
    /// 3. Materialize the always-on `claude`/`gemini` identity shims and prepend
    ///    their dir to `PATH` so a hand-started provider is pinned to the id.
    /// 4. Record the session AUTHORITATIVELY at spawn via
    ///    [`crate::commands::terminal::record_pinned_session_open`] — identity is
    ///    fixed with zero round-trip; the hook POST is confirmation/liveness
    ///    only (§3b determinism mechanism).
    ///
    /// Fail-open at every step: a materialize failure injects nothing (the
    /// terminal still spawns un-shimmed); a missing lifecycle store skips the
    /// record (the confirming hook still records via `/control/session-open`).
    fn apply_identity_seam(
        cmd: &mut CommandBuilder,
        terminal_id: &str,
        app_handle: &AppHandle,
        cwd: &str,
        title: &str,
        page_id: &str,
    ) {
        use crate::install_effects_producer::intercept::shim_materializer;
        use tauri::Manager;

        // 1. Pinned session id — the runner KNOWS it up front.
        //
        // If the caller supplied an explicit `--session-id <id>` in the PTY
        // child command (the gate-continuation / runner-launched direct-command
        // path builds `[claude, --session-id, <id>, …]`), ADOPT that id as the
        // authoritative pin instead of minting a fresh one. Otherwise the seam
        // would record a fresh uuid the session never runs under (the identity
        // shim's don't-double-pin passes the explicit id straight through), and
        // the caller's own capture-hint record would carry the REAL id — a
        // two-record split where the seam's row is a phantom. Recording the id
        // the command actually carries makes recorded id == run id == the id
        // that gets the SessionStart confirmation hook (plan Phase 2). When no
        // explicit id is present (the interactive-shell path), generate one.
        let pinned =
            Self::explicit_session_id_from(cmd).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // 2. Identity env — ALWAYS injected (zero-setup capture).
        cmd.env(shim_materializer::TERMINAL_ID_ENV, terminal_id);
        cmd.env(shim_materializer::PINNED_SESSION_ID_ENV, &pinned);
        // The identity shim's confirmation POST targets the runner loopback on
        // this port. Inject it even when install-interception is OFF so the
        // confirmation works out of the box (the var is shared but harmless when
        // install interception is dark — no install shims are on PATH then).
        let port = crate::install_effects_producer::intercept::bound_port();
        cmd.env(shim_materializer::PORT_ENV, port.to_string());

        // ---- Claude SessionStart hook delivery (plan §4, Phase 2) -----------
        // Materialize the bundled `--settings` hook into the runner's OWN
        // app-data dir (~/.qontinui/runner/session-restore/) — NEVER ~/.claude —
        // and inject its absolute path. The identity shim's `claude` wrapper
        // appends `--settings $that` (additive: Claude merges the hook on top of
        // any ~/.claude config WITHOUT writing to it). The SessionStart hook then
        // POSTs a confirmation/liveness signal to /control/session-open on
        // startup AND --resume. Fail-open: a materialize failure injects nothing
        // (identity still rides the spawn-time --session-id pin; only the
        // confirmation hook is absent).
        let hook_dir = crate::session::claude_hook::session_restore_dir();
        match crate::session::claude_hook::materialize(&hook_dir) {
            Some(settings_path) => {
                cmd.env(
                    crate::session::claude_hook::CLAUDE_SETTINGS_ENV,
                    settings_path.to_string_lossy().as_ref(),
                );
            }
            None => {
                // Hook delivery off for this terminal — identity is still pinned
                // + recorded at spawn below; only the confirmation hook is absent.
            }
        }

        // 3. Materialize the always-on identity shims + prepend their dir.
        let base_dir = std::env::temp_dir();
        match shim_materializer::materialize_identity(&base_dir, terminal_id) {
            Some(identity_dir) => {
                let current_path = std::env::var("PATH").ok();
                let new_path =
                    shim_materializer::prepend_path(&identity_dir, current_path.as_deref());
                Self::set_child_path(cmd, &new_path);
                // The identity shim reads this to skip its own dir in the
                // real-tool scan (reusing the install shim's env contract).
                cmd.env(
                    "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR",
                    identity_dir.to_string_lossy().as_ref(),
                );
                info!(
                    terminal_id = %terminal_id,
                    identity_dir = %identity_dir.display(),
                    "session-restore: always-on identity shim dir injected onto terminal PATH"
                );
            }
            None => {
                // Fail-open: env vars are still set so a directly-invoked
                // provider could still be pinned by a future absolute-path
                // wrapper, but no PATH shim this terminal.
            }
        }

        // 4. Record AUTHORITATIVELY at spawn (zero transcript race). The
        // lifecycle store is shared in Tauri state by main.rs; absent in test
        // fixtures / a boot window — then the confirming hook records instead.
        if let Some(store) = app_handle
            .try_state::<std::sync::Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        {
            crate::commands::terminal::record_pinned_session_open(
                store.inner(),
                pinned.clone(),
                terminal_id.to_string(),
                None, // config dir resolved by the hook / reconcile if needed
                cwd.to_string(),
                title.to_string(),
                page_id.to_string(),
                0,
                crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            );
            info!(
                terminal_id = %terminal_id,
                session_id = %pinned,
                "session-restore: session recorded authoritatively at spawn"
            );
        }
    }

    /// Assign a process to the Windows Job Object for crash safety.
    #[cfg(target_os = "windows")]
    fn assign_to_job_object(pid: u32) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

        unsafe {
            let handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
            if !handle.is_null() && !std::ptr::eq(handle, INVALID_HANDLE_VALUE as *mut _) {
                crate::job_object::assign_process_to_job(handle as _);
                CloseHandle(handle as _);
            }
        }
    }

    /// Write data (keystrokes) to the PTY stdin.
    ///
    /// This is the SINGLE funnel for terminal input — every write surface
    /// (Tauri `terminal_write`, HTTP `write_terminal_handler`, the WS input
    /// loop, transports, backend relay) routes through here, so the
    /// typed-input observer ([`Self::observe_input`]) covers every present
    /// and future write path. Observation happens AFTER the bytes hit the
    /// PTY and the writer lock is released — it never delays keystrokes.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        {
            let mut writer = self
                .writer
                .lock()
                .map_err(|e| format!("Writer lock poisoned: {}", e))?;
            writer
                .write_all(data)
                .map_err(|e| format!("Failed to write to PTY: {}", e))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        } // writer lock released before observation

        self.observe_input(data);
        Ok(())
    }

    /// Observe raw typed input bytes — feed them through the per-terminal
    /// input-line buffer ([`consume_input_bytes`]) and dispatch each
    /// completed (CR/LF-submitted) line to the typed-input consumers:
    ///
    /// 1. **L3 git-warn**: a branch-mutating git line kicks off a
    ///    best-effort coord lookup that emits a soft
    ///    `terminal-coord-warning` event when a PEER holds this repo's
    ///    worktree claim.
    /// 2. **Typed claude resume sniff** (#548 Phase 2): a
    ///    `claude … --resume <id>` / `--session-id <id>` line registers the
    ///    session in the durable lifecycle store and — for the bypass form —
    ///    emits the `terminal-bypass-permissions` event, mirroring the
    ///    spawn-argv sniff in `TerminalManager::create`.
    ///
    /// SOFT + CHEAP: never blocks, never affects the PTY write. Called from
    /// [`Self::write`] AFTER the bytes are written; all effects run on
    /// detached async tasks.
    fn observe_input(&self, data: &[u8]) {
        // Collect any completed lines this chunk produced. We hold the
        // buffer lock only for the cheap byte-walk, then release before
        // spawning the async effect tasks.
        let mut completed: Vec<String> = Vec::new();
        if let Ok(mut buf) = self.input_line_buf.lock() {
            completed = consume_input_bytes(&mut buf, data);
        }
        if completed.is_empty() {
            return;
        }

        let Some(app_handle) = self.app_handle.clone() else {
            return;
        };
        for line in completed {
            if let Some(parsed) = super::claude_resume_sniff::parse_typed_claude_resume(&line) {
                let title = self
                    .title
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_else(|e| e.into_inner().clone());
                super::claude_resume_sniff::spawn_register_typed_resume(
                    app_handle.clone(),
                    self.id.clone(),
                    self.working_dir.clone(),
                    self.page_id.clone(),
                    title,
                    parsed,
                );
            }
            if super::coord_warn::is_branch_mutating_git(&line) {
                super::coord_warn::spawn_check_and_warn(
                    app_handle.clone(),
                    self.id.clone(),
                    self.working_dir.clone(),
                    self.coord_session_id(),
                    line,
                );
            }
        }
    }

    /// Submit `message` to a Claude-style interactive prompt running in
    /// the PTY. The bytes are wrapped in ANSI bracketed-paste markers
    /// (`\x1b[200~` ... `\x1b[201~`) so the TUI treats the content as a
    /// single paste block, then a bare `\r` is written **outside** the
    /// paste window so the prompt actually submits.
    ///
    /// CR-LF (`\r\n`) is intentionally NOT used: bracketed-paste TUIs
    /// like Claude Code consume the `\n` as the closing newline of the
    /// paste rather than as the submit keystroke. Sending only `\r`
    /// after the end marker reliably triggers send. See Phase 6 §6
    /// remediation plan, Issue 1.
    pub fn submit_prompt(&self, message: &str) -> Result<(), String> {
        // Phase 1: write the bracketed-paste block, flush, release the
        // writer lock. The lock release lets concurrent reads on this
        // pty (output drain) interleave during the post-paste delay.
        {
            let mut writer = self
                .writer
                .lock()
                .map_err(|e| format!("Writer lock poisoned: {}", e))?;
            writer
                .write_all(BRACKETED_PASTE_BEGIN)
                .map_err(|e| format!("Failed to write paste begin: {}", e))?;
            writer
                .write_all(message.as_bytes())
                .map_err(|e| format!("Failed to write paste body: {}", e))?;
            writer
                .write_all(BRACKETED_PASTE_END)
                .map_err(|e| format!("Failed to write paste end: {}", e))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        } // writer lock released

        // Sleep so Claude Code's readline can fully process the paste
        // sequence BEFORE the submit byte arrives. Without this, the
        // bracketed-paste handler consumes the trailing CR as paste-tail
        // and never submits — see [`POST_PASTE_DELAY`] doc for the §6 E2E
        // reproduction. Sync sleep is acceptable here; callers
        // (`coordinator/act.rs::send_message_to_worker`) tolerate ~150ms
        // blocking on the multi-threaded tokio runtime. Move to
        // `tokio::task::spawn_blocking` if this ever goes hot-path.
        std::thread::sleep(POST_PASTE_DELAY);

        // Phase 2: bare CR (Enter) as a separate read cycle. Re-acquires
        // the writer lock; if the worker pty has been closed in the
        // meantime, this Err propagates cleanly.
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| format!("Writer lock poisoned: {}", e))?;
        writer
            .write_all(SUBMIT_ENTER)
            .map_err(|e| format!("Failed to write submit enter: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        Ok(())
    }

    /// Resize the PTY dimensions.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to resize PTY: {}", e))?;
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        if let Ok(mut g) = self.grid.lock() {
            g.resize(cols, rows);
        }
        Ok(())
    }

    /// Acknowledge bytes received by the frontend (flow control).
    pub fn ack(&self, bytes: u64) {
        self.bytes_acked.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Get terminal info for the frontend.
    pub fn info(&self) -> TerminalInfo {
        // Phase 2: title is now `Arc<Mutex<String>>`. Clone under the lock
        // so the returned snapshot doesn't observe a torn write from a
        // concurrent `set_title`.
        let title = self
            .title
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        TerminalInfo {
            id: self.id.clone(),
            title,
            pid: self.child_pid,
            cols: self.cols.load(Ordering::Relaxed),
            rows: self.rows.load(Ordering::Relaxed),
            working_dir: self.working_dir.clone(),
            is_alive: self.is_alive.load(Ordering::Relaxed),
            exit_code: self.exit_code.lock().ok().and_then(|ec| *ec),
            created_at: self.created_at,
            total_bytes_produced: self.total_bytes_produced.load(Ordering::Relaxed),
            page_id: self.page_id.clone(),
        }
    }

    /// Update the session's display title. Phase 2 of bi-directional
    /// title sync: the frontend's xterm.js `onTitleChange` handler relays
    /// observed OSC 0/2 titles back to the runner via the
    /// `terminal_set_title` Tauri command, which routes through
    /// `TerminalManager::set_title` to here. Subsequent `info()` calls
    /// (and `GET /terminals`) see the new value; other observers get the
    /// `terminal-title-changed` event emitted by the manager.
    pub fn set_title(&self, title: String) {
        match self.title.lock() {
            Ok(mut g) => *g = title,
            // Poisoned: recover and overwrite. Title is a pure-text field
            // with no invariants to preserve, so this is safe.
            Err(e) => *e.into_inner() = title,
        }
    }

    /// Take the one-shot receiver that resolves when the reader thread
    /// observes the first OSC 0/2 title from the child process. Returns
    /// `None` if a previous caller already took the receiver — callers
    /// should treat that as "already initialized" and skip the wait.
    /// `Some(rx)` resolves to `Ok(())` when the title lands, or
    /// `Err(oneshot::error::RecvError)` if the session closes before any
    /// OSC 0/2 arrives (caller should fall back to a timeout regardless).
    ///
    /// Used by `spawn_worker_session` (Phase 1) to gate
    /// `Initializing → Ready` on Claude CLI readline visibility — the CLI
    /// emits its OSC 0 title (`"✳ Claude Code"`) ~150–300 ms after
    /// startup, so the rx resolves well before the 8 s fallback timeout
    /// the dispatcher uses.
    pub fn subscribe_first_osc_title(&self) -> Option<oneshot::Receiver<()>> {
        self.first_osc_title_rx
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Get the scrollback buffer contents and the byte offset where the data starts.
    /// Returns `(data, start_offset)` where `start_offset = total_bytes_produced - data.len()`.
    ///
    /// `total` is read while HOLDING the ring lock: `tee_into_scrollback`
    /// bumps the counter inside the same lock, so the pair is mutually
    /// consistent — `start_offset + data.len()` is an exact replay boundary
    /// for offset-stamped `terminal-output` chunks (no chunk is ever half
    /// inside the snapshot).
    pub fn get_scrollback_buffer(&self) -> (Vec<u8>, u64) {
        let (data, total) = match self.scrollback_buffer.lock() {
            Ok(sb) => (
                sb.iter().copied().collect::<Vec<u8>>(),
                self.total_bytes_produced.load(Ordering::Relaxed),
            ),
            Err(_) => (
                Vec::new(),
                self.total_bytes_produced.load(Ordering::Relaxed),
            ),
        };
        let start_offset = total.saturating_sub(data.len() as u64);
        (data, start_offset)
    }

    /// Reset flow control counters so a reconnecting frontend doesn't hit backpressure.
    pub fn reset_flow_control(&self) {
        let sent = self.bytes_sent.load(Ordering::Relaxed);
        self.bytes_acked.store(sent, Ordering::Relaxed);
    }

    /// Subscribe to the terminal output broadcast channel.
    /// Returns a receiver that yields base64-encoded output chunks.
    pub fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.output_tx.subscribe()
    }

    /// Set the coord-native session id after `register_external()` wires
    /// this terminal into the coordinator's session plane.
    pub fn set_coord_session_id(&self, id: uuid::Uuid) {
        if let Ok(mut slot) = self.coord_session_id.lock() {
            *slot = Some(id);
        }
    }

    /// Read the coord-native session id, if one has been wired.
    pub fn coord_session_id(&self) -> Option<uuid::Uuid> {
        self.coord_session_id.lock().ok().and_then(|g| *g)
    }

    /// R1 — install the on-exit hook the waiter thread fires the instant
    /// the PTY process exits. `terminal_create` wires this (alongside
    /// [`Self::set_coord_session_id`]) to close the coord session mirror
    /// immediately rather than leaving it for coord's stale→closed watcher
    /// (the runner no longer self-closes abandoned sessions; coord_sync A3).
    /// The callback receives the coord session id and must be idempotent
    /// (it shares the close path with the frontend `terminal_close`
    /// command — `SessionRegistry::close_by_id` is already idempotent).
    pub fn set_on_exit(&self, hook: Box<dyn Fn(uuid::Uuid) + Send + Sync>) {
        if let Ok(mut slot) = self.on_exit.lock() {
            *slot = Some(hook);
        }
    }

    /// Phase 2 of the worktree-isolation plan — park the
    /// `IsolatedEditContext` returned by
    /// `agent_worktree::isolated_edit::acquire` so its heartbeat task
    /// + claim live as long as the terminal session. The slot is
    /// cleared in `close()`, which drops the context and fires
    /// best-effort claim release.
    pub fn set_isolated_edit_ctx(
        &self,
        ctx: crate::agent_worktree::isolated_edit::IsolatedEditContext,
    ) {
        if let Ok(mut slot) = self.isolated_edit_ctx.lock() {
            *slot = Some(ctx);
        }
    }

    /// Clone the per-session grid handle so callers can snapshot or read text.
    pub fn grid(&self) -> Arc<Mutex<Grid>> {
        self.grid.clone()
    }

    /// Check if the shell process is still alive.
    pub fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::Relaxed)
    }

    /// Kill the shell process and clean up threads.
    pub fn close(&self) {
        info!(terminal_id = %self.id, "Closing terminal session");
        self.is_alive.store(false, Ordering::Relaxed);

        // Phase 2 — drop the isolated edit context first so the
        // claim-release fire-and-forget posts ahead of the PTY teardown
        // (release uses tokio::spawn; running it before we drain threads
        // makes ordering observable in coord audit logs).
        if let Ok(mut slot) = self.isolated_edit_ctx.lock() {
            slot.take();
        }

        // Kill the child process via PID if still alive
        if let Some(pid) = self.child_pid {
            #[cfg(target_os = "windows")]
            {
                let _ = crate::process_helpers::no_window("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        // Drop the writer to signal EOF on stdin
        if let Ok(mut writer) = self.writer.lock() {
            drop(writer.flush());
        }

        // Drop the master PTY handle — this closes the OS pipe and unblocks the
        // reader thread which may be stuck in a blocking read() call.
        if let Ok(mut master) = self.master.lock() {
            // Replace with a placeholder so the Drop actually runs now.
            // MasterPty is trait-object-boxed, so we swap it out.
            let _dropped = std::mem::replace(&mut *master, create_noop_master());
        }

        // Join threads with a timeout so we never hang the UI
        if let Ok(mut handle) = self.reader_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "reader", &self.id);
            }
        }
        if let Ok(mut handle) = self.waiter_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "waiter", &self.id);
            }
        }

        // Install-interception cleanup (plan §4 Phase 4): remove this terminal's
        // per-terminal shim bin dir so it does not leak after the PTY child is
        // reaped. Best-effort + no-op when interception was never enabled (the
        // dir simply won't exist). The stale-sweep at the next materialize reaps
        // anything a crash left behind.
        crate::install_effects_producer::intercept::shim_materializer::cleanup(
            &std::env::temp_dir(),
            &self.id,
        );

        info!(terminal_id = %self.id, "Terminal session closed");
    }
}

/// Join a thread with a timeout, logging a warning if it doesn't finish in time.
fn join_with_timeout(handle: thread::JoinHandle<()>, name: &str, terminal_id: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_name = name.to_string();
    let tid = terminal_id.to_string();

    // Spawn a helper thread that joins and signals completion
    let _ = thread::Builder::new()
        .name(format!("join-{}-{}", thread_name, tid))
        .spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });

    // Wait up to 2 seconds
    if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
        warn!(
            terminal_id = %terminal_id,
            thread = %name,
            "Thread did not finish within 2s timeout — detaching"
        );
    }
}

/// Create a no-op MasterPty placeholder used when dropping the real master during close.
fn create_noop_master() -> Box<dyn MasterPty + Send> {
    Box::new(NoopMaster)
}

/// Minimal MasterPty that does nothing — used as a swap target during close().
struct NoopMaster;

impl MasterPty for NoopMaster {
    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize {
            rows: 0,
            cols: 0,
            pixel_width: 0,
            pixel_height: 0,
        })
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }
    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }
    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.is_alive.load(Ordering::Relaxed) {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory `Write` whose bytes can be inspected after the writes.
    /// Backed by an `Arc<Mutex<Vec<u8>>>` so the test can read it after
    /// the `TerminalSession` fakery has consumed the writer.
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut g = self.0.lock().unwrap();
            g.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Construct a minimum-viable `TerminalSession` whose `writer` field
    /// is a `CapturingWriter` and whose other fields are inert. The
    /// reader/waiter threads are NOT spawned — `submit_prompt` only
    /// touches `self.writer`, so this is enough.
    fn make_test_session(buf: Arc<Mutex<Vec<u8>>>) -> TerminalSession {
        let writer: Box<dyn Write + Send> = Box::new(CapturingWriter(buf));
        let (output_tx, _) = broadcast::channel::<String>(1);
        let (osc_title_tx, osc_title_rx) = oneshot::channel::<()>();
        TerminalSession {
            id: "test".to_string(),
            title: Arc::new(Mutex::new("test".to_string())),
            working_dir: ".".to_string(),
            page_id: "default".to_string(),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(create_noop_master())),
            child_pid: None,
            cols: AtomicU16::new(80),
            rows: AtomicU16::new(24),
            // Mark the session as already-dead so Drop doesn't try to
            // join nonexistent reader/waiter threads.
            is_alive: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(Mutex::new(None)),
            reader_join: Mutex::new(None),
            waiter_join: Mutex::new(None),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_acked: Arc::new(AtomicU64::new(0)),
            scrollback_buffer: Arc::new(Mutex::new(VecDeque::new())),
            total_bytes_produced: Arc::new(AtomicU64::new(0)),
            created_at: 0,
            output_tx,
            grid: Arc::new(Mutex::new(Grid::new(80, 24))),
            first_osc_title_tx: Arc::new(Mutex::new(Some(osc_title_tx))),
            first_osc_title_rx: Arc::new(Mutex::new(Some(osc_title_rx))),
            coord_session_id: Arc::new(Mutex::new(None)),
            on_exit: Arc::new(Mutex::new(None)),
            isolated_edit_ctx: Arc::new(Mutex::new(None)),
            // No real Tauri app in unit fixtures — the input-line warn
            // hook no-ops when this is `None`.
            app_handle: None,
            input_line_buf: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Drive a `SyncFrameCoalescer` across a sequence of `(data, offset,
    /// in_sync)` reads and return the emitted `(payload, offset)` events.
    fn run_coalescer(reads: &[(&[u8], u64, bool)]) -> Vec<(Vec<u8>, u64)> {
        let mut c = SyncFrameCoalescer::new();
        let emitted = Arc::new(Mutex::new(Vec::<(Vec<u8>, u64)>::new()));
        for (data, offset, in_sync) in reads {
            let sink = emitted.clone();
            c.feed(data, *offset, *in_sync, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        let sink = emitted.clone();
        c.flush_remaining(move |p, o| sink.lock().unwrap().push((p.to_vec(), o)));
        Arc::try_unwrap(emitted).unwrap().into_inner().unwrap()
    }

    #[test]
    fn whole_frame_in_one_read_emits_immediately() {
        // sync_output already false post-advance → no holding, one event.
        let events = run_coalescer(&[(b"\x1b[?2026hFRAME\x1b[?2026l", 0, false)]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, b"\x1b[?2026hFRAME\x1b[?2026l");
        assert_eq!(events[0].1, 0);
    }

    #[test]
    fn multi_read_sync_frame_coalesces_into_one_emit() {
        // A frame split across three reads: open (in_sync), middle (in_sync),
        // close (in_sync flips false in the read that carried `?2026l`).
        // Offsets are the absolute stream positions of each read.
        let events = run_coalescer(&[
            (b"\x1b[?2026hPART1", 10, true),
            (b"PART2", 23, true),
            (b"PART3\x1b[?2026l", 28, false),
        ]);
        assert_eq!(events.len(), 1, "the frame must coalesce into ONE event");
        assert_eq!(events[0].0, b"\x1b[?2026hPART1PART2PART3\x1b[?2026l");
        // Offset must be the FIRST held byte's offset (the replay boundary).
        assert_eq!(events[0].1, 10);
    }

    #[test]
    fn non_sync_chunks_each_emit_separately() {
        let events = run_coalescer(&[(b"alpha", 0, false), (b"beta", 5, false)]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (b"alpha".to_vec(), 0));
        assert_eq!(events[1], (b"beta".to_vec(), 5));
    }

    #[test]
    fn byte_cap_flushes_a_never_closed_sync_block() {
        let mut c = SyncFrameCoalescer::new();
        let emitted = Arc::new(Mutex::new(Vec::<(Vec<u8>, u64)>::new()));
        // First read opens the block and is under the cap.
        let big = vec![b'x'; SYNC_FLUSH_BYTE_CAP];
        {
            let sink = emitted.clone();
            c.feed(b"\x1b[?2026h", 0, true, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        // Second read pushes the held buffer past the byte cap → forced flush
        // even though `?2026l` never arrived.
        {
            let sink = emitted.clone();
            c.feed(&big, 8, true, move |p, o| {
                sink.lock().unwrap().push((p.to_vec(), o));
            });
        }
        let events = emitted.lock().unwrap();
        assert_eq!(events.len(), 1, "byte cap must force a flush");
        assert_eq!(events[0].1, 0, "flush keeps the first held byte's offset");
        assert!(events[0].0.len() >= SYNC_FLUSH_BYTE_CAP);
    }

    #[test]
    fn flush_remaining_emits_a_held_unclosed_frame_on_exit() {
        // Reader exits (EOF) mid-frame — the held prefix must still be emitted.
        let events = run_coalescer(&[(b"\x1b[?2026hHALF", 100, true)]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, b"\x1b[?2026hHALF");
        assert_eq!(events[0].1, 100);
    }

    #[test]
    fn build_command_from_override_uses_program_and_args() {
        // Decision 3: an explicit override runs that program with its args as
        // the PTY child (argv[0]=program, argv[1..]=args).
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "do the thing".to_string(),
        ]));
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["claude".to_string(), "do the thing".to_string()]);
    }

    #[test]
    fn build_command_from_program_only_has_no_extra_args() {
        let cmd = TerminalSession::build_command_from(Some(vec!["claude".to_string()]));
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["claude".to_string()]);
    }

    #[test]
    fn build_command_from_none_falls_back_to_shell() {
        // None → the interactive shell path (back-compat by construction). The
        // shell program is platform-specific, but it must NOT be `claude` and
        // must match what `build_shell_command` produces.
        let fallback = TerminalSession::build_command_from(None);
        let shell = TerminalSession::build_shell_command();
        assert_eq!(
            fallback.get_argv().first(),
            shell.get_argv().first(),
            "None override must fall back to the shell program"
        );
    }

    #[test]
    fn build_command_from_empty_vec_falls_back_to_shell() {
        // An empty override is meaningless (no program) → fall back to the
        // shell rather than spawning nothing.
        let fallback = TerminalSession::build_command_from(Some(vec![]));
        let shell = TerminalSession::build_shell_command();
        assert_eq!(fallback.get_argv().first(), shell.get_argv().first());
    }

    #[test]
    fn path_env_key_is_os_canonical_casing() {
        // Windows env is case-insensitive with undefined duplicate resolution,
        // so the child path key MUST be the OS-preferred casing (`Path`), not a
        // bare `PATH` that the inherited `Path` could shadow.
        #[cfg(windows)]
        assert_eq!(TerminalSession::PATH_ENV_KEY, "Path");
        #[cfg(not(windows))]
        assert_eq!(TerminalSession::PATH_ENV_KEY, "PATH");
    }

    #[test]
    fn set_child_path_leaves_exactly_one_shim_first_path_key() {
        // The env-injection invariant (plan Phase 1): after set_child_path the
        // child has EXACTLY ONE path key (any casing) and it carries the shim
        // dir as its FIRST segment. Seed a pre-existing case-variant to prove
        // the removal defeats the Windows `Path`-shadows-`PATH` collision.
        let mut cmd = CommandBuilder::new("dummy");
        cmd.env("Path", "C:\\real\\bin;C:\\windows");
        cmd.env("PATH", "/usr/bin:/bin");

        let sep = if cfg!(windows) { ';' } else { ':' };
        let shim_dir = "SHIM_DIR_MARKER";
        let existing = if cfg!(windows) {
            "C:\\real\\bin;C:\\windows"
        } else {
            "/usr/bin:/bin"
        };
        let new_path = format!("{shim_dir}{sep}{existing}");
        TerminalSession::set_child_path(&mut cmd, &new_path);

        // Exactly one path key survives (case-folded), and it is shim-first.
        let path_keys: Vec<(String, String)> = cmd
            .iter_full_env_as_str()
            .filter(|(k, _)| k.eq_ignore_ascii_case("path"))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            path_keys.len(),
            1,
            "expected exactly one path key, got {path_keys:?}"
        );
        let (key, value) = &path_keys[0];
        assert_eq!(key, TerminalSession::PATH_ENV_KEY, "canonical casing");
        assert!(
            value.starts_with(shim_dir),
            "shim dir must be the FIRST path segment; got {value:?}"
        );
        // The readback via the canonical key must also find the shim-first value
        // (guards against a stray un-removed variant winning).
        let read = cmd
            .get_env(TerminalSession::PATH_ENV_KEY)
            .map(|v| v.to_string_lossy().to_string());
        assert_eq!(read.as_deref(), Some(new_path.as_str()));
    }

    #[test]
    fn explicit_session_id_from_space_separated() {
        // The runner-launched direct-command path builds
        // `[claude, --session-id, <id>, …]`; the seam must adopt <id> as its
        // authoritative pin so recorded id == the id claude runs under.
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--session-id".to_string(),
            "abc-123".to_string(),
            "--".to_string(),
            "do the thing".to_string(),
        ]));
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_attached_form() {
        let cmd = TerminalSession::build_command_from(Some(vec![
            "claude".to_string(),
            "--session-id=xyz-789".to_string(),
        ]));
        assert_eq!(
            TerminalSession::explicit_session_id_from(&cmd),
            Some("xyz-789".to_string())
        );
    }

    #[test]
    fn explicit_session_id_from_none_for_interactive_shell() {
        // The interactive-shell path carries no --session-id → the seam mints a
        // fresh id (unit-tested here only that it returns None so the caller
        // falls back to Uuid::new_v4).
        let cmd = TerminalSession::build_command_from(None);
        assert_eq!(TerminalSession::explicit_session_id_from(&cmd), None);
    }

    #[test]
    fn build_submit_payload_emits_paste_markers_and_bare_cr() {
        let payload = build_submit_payload("hello");
        assert_eq!(payload, b"\x1b[200~hello\x1b[201~\r");
    }

    #[test]
    fn build_submit_payload_handles_empty_message() {
        let payload = build_submit_payload("");
        assert_eq!(payload, b"\x1b[200~\x1b[201~\r");
    }

    #[test]
    fn submit_prompt_writes_bracketed_paste_then_bare_cr() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("hello")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();
        assert_eq!(written, b"\x1b[200~hello\x1b[201~\r");
    }

    #[test]
    fn submit_prompt_does_not_emit_lf() {
        // Regression guard: the Phase 6 §6 bug was that send_user_message
        // sent CR-LF, and Claude Code's bracketed-paste handler ate the
        // LF as the paste-block terminator instead of submitting. The
        // submit byte must be a bare CR; no LF anywhere in the output.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session
            .submit_prompt("line1 line2")
            .expect("submit_prompt failed");
        let written = buf.lock().unwrap().clone();
        assert!(
            !written.contains(&b'\n'),
            "submit_prompt must not emit LF bytes; got {:?}",
            written
        );
    }

    #[test]
    fn set_title_updates_info() {
        // Phase 2: bi-directional title sync. set_title must be visible
        // via info() so subsequent /terminals reads see the new value.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf);
        assert_eq!(session.info().title, "test");
        session.set_title("✳ Claude Code".to_string());
        assert_eq!(session.info().title, "✳ Claude Code");
        // Idempotent overwrite.
        session.set_title("Worker 1".to_string());
        assert_eq!(session.info().title, "Worker 1");
    }

    // --- typed-input observer: line assembly + the write() funnel ----------

    #[test]
    fn consume_input_bytes_assembles_edits_and_caps_lines() {
        let mut buf = String::new();
        // Partial chunk — no completed line, buffer accumulates across calls.
        assert!(consume_input_bytes(&mut buf, b"claude --re").is_empty());
        assert_eq!(buf, "claude --re");
        let done = consume_input_bytes(&mut buf, b"sume abc\r");
        assert_eq!(done, vec!["claude --resume abc".to_string()]);
        assert!(buf.is_empty());
        // LF completes too; blank lines are not emitted.
        assert!(consume_input_bytes(&mut buf, b"   \n").is_empty());
        // 0x08 backspace and 0x7F DEL both pop the last char.
        let done = consume_input_bytes(&mut buf, b"claude -x\x08-resume\r");
        assert_eq!(done, vec!["claude --resume".to_string()]);
        let done = consume_input_bytes(&mut buf, b"git statuz\x7Fs\r");
        assert_eq!(done, vec!["git status".to_string()]);
        // Escape sequences / non-ASCII bytes are dropped, printables kept.
        let done = consume_input_bytes(&mut buf, b"ls\x1b\x01\xc3\xa9 -la\r");
        assert_eq!(done, vec!["ls -la".to_string()]);
        // > 4 KiB with no newline — buffer cleared, not unbounded growth.
        assert!(consume_input_bytes(&mut buf, &vec![b'a'; 5000]).is_empty());
        assert!(buf.is_empty(), "over-cap buffer must be cleared");
        // Still usable afterwards.
        let done = consume_input_bytes(&mut buf, b"echo hi\r");
        assert_eq!(done, vec!["echo hi".to_string()]);
    }

    #[test]
    fn write_funnels_input_into_the_observer() {
        // The observation funnel lives in `TerminalSession::write` itself —
        // ANY caller of write() feeds the input-line buffer, with no
        // per-caller observe call (the fixture has no app_handle, so effect
        // dispatch is skipped but line assembly still runs).
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf.clone());
        session.write(b"claude --re").expect("write failed");
        assert_eq!(
            session.input_line_buf.lock().unwrap().as_str(),
            "claude --re",
            "write() must feed the typed-input observer"
        );
        // Completing the line drains the buffer (the line was dispatched)...
        session.write(b"sume\r").expect("write failed");
        assert!(session.input_line_buf.lock().unwrap().is_empty());
        // ...and the PTY itself still received every byte, unchanged.
        assert_eq!(buf.lock().unwrap().as_slice(), b"claude --resume\r");
    }

    #[test]
    fn subscribe_first_osc_title_returns_some_once() {
        // The receiver can be taken at most once. Subsequent takes return
        // `None` — `spawn_worker_session` interprets that as "already
        // ready, skip the wait".
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let session = make_test_session(buf);
        assert!(session.subscribe_first_osc_title().is_some());
        assert!(session.subscribe_first_osc_title().is_none());
    }

    /// Spawn a real `portable-pty` child that prints `marker`, drive a
    /// reader loop that tees its output into `session`'s OWN scrollback +
    /// byte-counter Arcs via the production [`tee_into_scrollback`] path,
    /// and stop once the child has exited and its output is drained.
    ///
    /// This is the exact teeing the reader thread in
    /// [`TerminalSession::spawn`] performs per read — only the Tauri event
    /// emission (irrelevant to buffer state) is omitted, which is why the
    /// test can run without an `AppHandle`.
    fn drive_real_pty_into(session: &TerminalSession, marker: &str) {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        // Cross-platform single-shot echo: `cmd /C echo MARKER` on Windows,
        // `sh -c "echo MARKER"` elsewhere. Both exit immediately after
        // printing, so the reader loop terminates on EOF.
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg(format!("echo {}", marker));
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg(format!("echo {}", marker));
            c
        };
        cmd.env("TERM", "xterm-256color");

        let mut child = pair.slave.spawn_command(cmd).expect("spawn echo child");
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        // Drop the slave handle so the master sees EOF after the child exits.
        drop(pair.slave);

        let scrollback = session.scrollback_buffer.clone();
        let total = session.total_bytes_produced.clone();

        // Reader thread: tee everything into the session's own Arcs via the
        // production path until the PTY closes. This MUST be its own thread
        // (exactly like the production reader thread): on Windows/ConPTY,
        // `reader.read()` keeps blocking after the child exits — only
        // closing the MASTER unblocks it — so an inline read loop with a
        // between-reads deadline check hangs (it wedged the windows-latest
        // CI job for the full 90-minute timeout).
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tee_into_scrollback(&scrollback, &total, &buf[..n]);
                    }
                }
            }
        });

        // The echo child exits immediately after printing.
        let _ = child.wait();

        // Bounded poll until the marker lands in the session's scrollback
        // (the reader thread races the child's output in). On timeout we
        // fall through — the caller's assertions then fail with the actual
        // buffer content rather than hanging the test harness.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let (sb, _) = session.get_scrollback_buffer();
            if String::from_utf8_lossy(&sb).contains(marker) || std::time::Instant::now() > deadline
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Closing the master tears down the PTY (ClosePseudoConsole on
        // Windows), which unblocks the reader thread's pending `read()`.
        drop(pair.master);
        let _ = reader_thread.join();
    }

    /// REGRESSION (plan
    /// `2026-06-06-gate-continuation-terminal-visibility-and-execution`):
    /// a live observation showed `GET /terminals/{id}/buffer` returning
    /// byte-identical stale content for two DIFFERENT terminals. The read
    /// path proved sound and the defect did not reproduce, but no buffer
    /// or route-handler test existed. This locks in the core invariant the
    /// handler relies on: each session owns an independent scrollback
    /// buffer, so two sessions running distinct commands read back distinct
    /// content — never each other's, never byte-identical.
    ///
    /// Uses TWO real `portable-pty` children (cross-platform echo) feeding
    /// the production [`tee_into_scrollback`] path into each session's own
    /// Arcs, then asserts isolation through the real
    /// [`TerminalSession::get_scrollback_buffer`].
    #[test]
    fn scrollback_buffers_are_per_session_distinct() {
        const MARKER_A: &str = "QONTINUI_DISTINCT_MARKER_AAAAA";
        const MARKER_B: &str = "QONTINUI_DISTINCT_MARKER_BBBBB";

        let session_a = make_test_session(Arc::new(Mutex::new(Vec::new())));
        let session_b = make_test_session(Arc::new(Mutex::new(Vec::new())));

        // Confirm the two sessions hold DISTINCT buffer Arcs (the precise
        // aliasing the live defect would represent). If these ever pointed
        // at the same allocation, the assertions below would also fail —
        // this is the cheap, direct guard.
        assert!(
            !Arc::ptr_eq(&session_a.scrollback_buffer, &session_b.scrollback_buffer),
            "sessions must not share a scrollback buffer Arc"
        );

        drive_real_pty_into(&session_a, MARKER_A);
        drive_real_pty_into(&session_b, MARKER_B);

        let (buf_a, _) = session_a.get_scrollback_buffer();
        let (buf_b, _) = session_b.get_scrollback_buffer();
        let text_a = String::from_utf8_lossy(&buf_a);
        let text_b = String::from_utf8_lossy(&buf_b);

        // (a) A has its own marker and NOT B's.
        assert!(
            text_a.contains(MARKER_A),
            "session A buffer missing MARKER_A; got {:?}",
            text_a
        );
        assert!(
            !text_a.contains(MARKER_B),
            "session A buffer leaked MARKER_B; got {:?}",
            text_a
        );

        // (b) B has its own marker and NOT A's.
        assert!(
            text_b.contains(MARKER_B),
            "session B buffer missing MARKER_B; got {:?}",
            text_b
        );
        assert!(
            !text_b.contains(MARKER_A),
            "session B buffer leaked MARKER_A; got {:?}",
            text_b
        );

        // (c) The two buffers are not byte-identical — the exact stale-read
        // symptom from the live observation.
        assert_ne!(
            buf_a, buf_b,
            "two terminals returned byte-identical scrollback buffers"
        );
    }
}
