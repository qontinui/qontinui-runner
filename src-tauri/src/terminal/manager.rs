//! TerminalManager — registry of active terminal sessions.
//!
//! Follows the same HashMap-based manager pattern as `claude_session::SessionManager`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use tracing::{debug, error, info};

use super::interceptor::OutputInterceptor;
use super::session::TerminalSession;
use super::types::{TerminalId, TerminalInfo};
use crate::claude_session::SessionManager;

/// Manages all active terminal sessions.
pub struct TerminalManager {
    sessions: Mutex<HashMap<TerminalId, Arc<TerminalSession>>>,
    interceptor: Arc<OutputInterceptor>,
}

impl TerminalManager {
    /// Create a new terminal manager with an empty interceptor pipeline.
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            interceptor: Arc::new(OutputInterceptor::new()),
        }
    }

    /// Create a new terminal session, returning its info.
    ///
    /// `command` is an optional program+args override (Decision 3) forwarded to
    /// [`TerminalSession::spawn`]: `Some([program, args…])` runs that program as
    /// the PTY child (the gate-continuation terminal branch launches `claude`
    /// this way); `None` keeps the interactive-shell behavior every
    /// operator-opened terminal uses.
    ///
    /// `extra_env` (Phase 2c) is an optional set of `(key, value)` env vars
    /// exported onto the PTY child in addition to the built-in runner vars —
    /// the launch surfaces use it to carry `QONTINUI_SESSION_WORKTREES` (the
    /// agent-agnostic pointer to every materialized sibling worktree).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        title: Option<String>,
        working_dir: Option<String>,
        page_id: Option<String>,
        cols: Option<u16>,
        rows: Option<u16>,
        app_handle: AppHandle,
        command: Option<Vec<String>>,
        extra_env: Option<Vec<(String, String)>>,
    ) -> Result<TerminalInfo, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let title = title.unwrap_or_else(|| format!("Terminal {}", self.count() + 1));
        let working_dir = working_dir
            .filter(|d| !d.is_empty())
            .or_else(|| {
                // Default to workspace root (parent of qontinui-runner)
                crate::mcp::shared::current_project_path()
            })
            .unwrap_or_default();
        let page_id = page_id.unwrap_or_else(|| "default".to_string());
        let cols = cols.unwrap_or(120);
        let rows = rows.unwrap_or(30);

        // Bypass-aware needs-input detection (plan
        // `2026-06-07-runner-continuation-defer-and-phantom-needs-input.md`,
        // Defect B): command-sniff whether this PTY child is a Claude session
        // launched with permissions bypassed. The runner-spawned gate
        // continuation injects `--dangerously-skip-permissions`
        // (`agent_runtime.rs:1309`); operator resumes use
        // `--permission-mode bypassPermissions`. A bypass session can never
        // await *tool* approval, so the frontend must skip approval-shaped
        // patterns for it (the 12-min `rm -rf` phantom, 2026-06-07). We carry
        // this as a dedicated `terminal-bypass-permissions` Tauri event keyed
        // by terminal id rather than a new `TerminalInfo` field, because
        // `TerminalInfo` is the cross-repo wire schema
        // (`qontinui-schemas`, `#[schemars(deny_unknown_fields)]`, published
        // `@qontinui/shared-types`) and a runner-local UI hint does not belong
        // on the wire contract.
        let bypass_permissions = command
            .as_deref()
            .is_some_and(command_implies_bypass_permissions);

        let emitter = app_handle.clone();
        let session = TerminalSession::spawn(
            id.clone(),
            title,
            working_dir,
            page_id,
            cols,
            rows,
            app_handle,
            self.interceptor.clone(),
            command,
            extra_env,
        )?;

        let info = session.info();
        let session = Arc::new(session);

        let mut sessions = self
            .sessions
            .lock()
            .map_err(|e| format!("Sessions lock poisoned: {}", e))?;
        sessions.insert(id, session);

        // Notify frontend so externally-created terminals get a UI tab
        if let Err(e) = emitter.emit("terminal-created", &info) {
            error!("Failed to emit terminal-created: {}", e);
        }

        // Bypass-aware needs-input hint — emitted only when the spawn command
        // implies bypassed permissions. Emitted AFTER `terminal-created`; the
        // frontend listener buffers a bypass mark whose tab record hasn't
        // landed yet (mirrors the existing worker-registered race buffer), so
        // either delivery order is safe.
        if bypass_permissions {
            if let Err(e) = emitter.emit(
                "terminal-bypass-permissions",
                serde_json::json!({ "id": info.id }),
            ) {
                error!("Failed to emit terminal-bypass-permissions: {}", e);
            }
        }

        Ok(info)
    }

    /// Get a terminal session by ID.
    pub fn get(&self, id: &str) -> Option<Arc<TerminalSession>> {
        self.sessions.lock().ok().and_then(|s| s.get(id).cloned())
    }

    /// Update the title of a terminal session and emit a
    /// `terminal-title-changed` Tauri event so other webview windows
    /// (and the backend relay's WS subscribers) stay in sync.
    ///
    /// Phase 2 of the bi-directional title sync (plan
    /// `2026-05-11-runner-dispatch-and-terminal-ux-fixes-plan.md`): the
    /// frontend's xterm.js `onTitleChange` callback now invokes
    /// `terminal_set_title` so backend `/terminals` titles match what the
    /// user sees in the UI. Without this, `TerminalSession.title` was
    /// frozen at spawn time and drifted from the OSC 0 title the child
    /// emits at runtime.
    pub fn set_title(&self, id: &str, title: String, app_handle: &AppHandle) -> Result<(), String> {
        let session = self
            .get(id)
            .ok_or_else(|| format!("Terminal session not found: {}", id))?;
        session.set_title(title.clone());
        let payload = serde_json::json!({ "id": id, "title": title });
        if let Err(e) = app_handle.emit("terminal-title-changed", &payload) {
            error!("Failed to emit terminal-title-changed: {}", e);
        }
        // Mirror to the backend WS relay so remote mobile viewers stay
        // consistent (same pattern as the reader thread's terminal-output
        // mirror in session.rs).
        crate::event_system::broadcast_ws_notification(
            app_handle,
            "terminal-title-changed",
            &payload,
        );
        Ok(())
    }

    /// Like [`Self::set_title`], but silently drops the update when the
    /// terminal is registered with a `WorkerSession` in `session_manager`.
    /// Used by the `terminal_set_title` Tauri command to pin `Worker N`
    /// tab titles against OSC 0 drift from the embedded Claude CLI.
    ///
    /// Why: PR #98 (`0b61fa70a`, `feat(coord): worker readiness gate +
    /// terminal UX fixes from coord soak`) made title sync bidirectional,
    /// so OSC 0 titles emitted by Claude inside a worker pty overwrite
    /// both the in-memory `TerminalSession.title` AND the UI tab strip
    /// via `ZoneGrid::onTitleChange → invoke("terminal_set_title")`.
    /// `Worker N` is the at-a-glance identifier operators (and the
    /// Coordinator's `coord.tasks.assigned_session_id` join) use to
    /// triage workers — preserving it requires gating the post-spawn
    /// title mutations from worker-backed ptys.
    ///
    /// Non-worker terminals (manual `POST /terminals`, dashboard launch
    /// buttons, etc.) keep the existing OSC 0 follow-the-child behaviour
    /// through the unguarded [`Self::set_title`] path. Direct internal
    /// callers (e.g. a future "rename tab from settings" UI) should
    /// continue to invoke `set_title` directly — the gate applies only
    /// to the OSC 0 echo path.
    pub fn set_title_unless_worker(
        &self,
        session_manager: &SessionManager,
        id: &str,
        title: String,
        app_handle: &AppHandle,
    ) -> Result<(), String> {
        if session_manager.find_worker_by_terminal_id(id).is_some() {
            debug!(
                terminal_id = id,
                title = %title,
                "dropping OSC 0 title update for worker-backed terminal"
            );
            return Ok(());
        }
        self.set_title(id, title, app_handle)
    }

    /// Remove and close a terminal session.
    pub fn close(&self, id: &str) -> Result<(), String> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|e| format!("Sessions lock poisoned: {}", e))?;
            sessions.remove(id)
        };

        if let Some(session) = session {
            session.close();
            Ok(())
        } else {
            Err(format!("Terminal session not found: {}", id))
        }
    }

    /// List all terminal sessions with their info, sorted by creation time.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                return Vec::new();
            }
        };

        let mut infos: Vec<TerminalInfo> = sessions.values().map(|s| s.info()).collect();
        infos.sort_by_key(|info| info.created_at);
        infos
    }

    /// Get the number of active sessions.
    pub fn count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Return the `(cols, rows)` of the largest currently-registered
    /// terminal — "largest" measured by `cols * rows` cell count. Falls
    /// back to `(120, 30)` (the historical `create()` default) when no
    /// sessions exist.
    ///
    /// Phase 4 of the 2026-05-11 dispatch-fix plan: worker tabs spawned
    /// into a zone where another tab is currently visible mount under
    /// `display: none`. xterm.js's fit-addon then can't measure the
    /// container, so the backend PTY stays at the 120×30 default until
    /// the user activates the tab. Pre-sizing the new PTY to the
    /// dominant zone dims means Coordinator dispatch lands on a PTY
    /// matching the eventual visible size (Claude doesn't have to wrap
    /// twice). Fallback dims match what `create()` would have used on
    /// `None, None`, so behaviour is identical when there's no signal to
    /// crib from.
    pub fn dominant_zone_dims(&self) -> (u16, u16) {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned in dominant_zone_dims: {}", e);
                return (120, 30);
            }
        };
        let mut best: Option<(u16, u16, u32)> = None;
        for sess in sessions.values() {
            let info = sess.info();
            let area = (info.cols as u32).saturating_mul(info.rows as u32);
            if area == 0 {
                continue;
            }
            if best.map(|(_, _, a)| area > a).unwrap_or(true) {
                best = Some((info.cols, info.rows, area));
            }
        }
        best.map(|(c, r, _)| (c, r)).unwrap_or((120, 30))
    }

    /// Snapshot of `(session_id, Arc<TerminalSession>)` pairs for
    /// callers that need to iterate every active session — e.g. the
    /// cross-session grid search endpoint. Sorted by creation time
    /// for stable output ordering.
    pub fn sessions_snapshot(&self) -> Vec<(String, Arc<TerminalSession>)> {
        let sessions = match self.sessions.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                return Vec::new();
            }
        };
        let mut pairs: Vec<(String, Arc<TerminalSession>)> = sessions
            .iter()
            .map(|(id, sess)| (id.clone(), sess.clone()))
            .collect();
        pairs.sort_by_key(|(_, sess)| sess.info().created_at);
        pairs
    }

    /// Close all terminal sessions. Called on app shutdown.
    pub fn close_all(&self) {
        let sessions: Vec<Arc<TerminalSession>> = {
            match self.sessions.lock() {
                Ok(mut s) => s.drain().map(|(_, v)| v).collect(),
                Err(e) => {
                    error!("Sessions lock poisoned during close_all: {}", e);
                    return;
                }
            }
        };

        if sessions.is_empty() {
            return;
        }

        info!("Closing {} terminal session(s)", sessions.len());
        for session in &sessions {
            session.close();
        }
        info!("All terminal sessions closed");
    }

    /// Get a reference to the output interceptor (for adding hooks).
    #[allow(dead_code)]
    pub fn interceptor(&self) -> &Arc<OutputInterceptor> {
        &self.interceptor
    }
}

/// Whether a spawn command line implies a Claude session running with tool
/// permissions bypassed — i.e. one that can never legitimately stall on a
/// tool-approval prompt.
///
/// True iff the argv contains either:
/// - `--dangerously-skip-permissions` (the runner-spawned gate-continuation
///   form, `agent_runtime.rs`), or
/// - `--permission-mode bypassPermissions` (the operator-resume form). The two
///   tokens may be a single joined arg (`--permission-mode=bypassPermissions`)
///   or two adjacent args (`--permission-mode`, `bypassPermissions`); both are
///   handled. The whole command is joined and substring-matched so a flag
///   embedded inside a shell wrapper string (e.g.
///   `CLAUDE_CONFIG_DIR=… claude --permission-mode bypassPermissions`) is still
///   caught.
fn command_implies_bypass_permissions(argv: &[String]) -> bool {
    let joined = argv.join(" ");
    joined.contains("--dangerously-skip-permissions")
        || joined.contains("--permission-mode bypassPermissions")
        || joined.contains("--permission-mode=bypassPermissions")
}

#[cfg(test)]
mod tests {
    use super::command_implies_bypass_permissions;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn dangerously_skip_permissions_implies_bypass() {
        // Runner-spawned gate continuation (agent_runtime.rs:1309).
        assert!(command_implies_bypass_permissions(&argv(&[
            "claude",
            "--dangerously-skip-permissions",
            "--add-dir=/some/worktree",
            "run /implement-plan",
        ])));
    }

    #[test]
    fn permission_mode_bypass_two_args_implies_bypass() {
        // Operator resume — two adjacent args.
        assert!(command_implies_bypass_permissions(&argv(&[
            "claude",
            "--permission-mode",
            "bypassPermissions",
            "--resume",
            "abc123",
        ])));
    }

    #[test]
    fn permission_mode_bypass_joined_arg_implies_bypass() {
        // Operator resume threaded through a shell-string command (the form
        // the frontend injects: `CLAUDE_CONFIG_DIR=… claude --permission-mode
        // bypassPermissions …`) lands as one arg.
        assert!(command_implies_bypass_permissions(&argv(&[
            "CLAUDE_CONFIG_DIR=/cfg claude --permission-mode bypassPermissions --resume abc",
        ])));
        // …and the `=`-joined variant.
        assert!(command_implies_bypass_permissions(&argv(&[
            "claude",
            "--permission-mode=bypassPermissions",
        ])));
    }

    #[test]
    fn plain_shell_or_default_claude_is_not_bypass() {
        // Interactive shell — no flag.
        assert!(!command_implies_bypass_permissions(&argv(&["bash", "-l"])));
        // A Claude resume WITHOUT bypass (e.g. default/ask mode) must not be
        // treated as bypass — approval prompts there are real.
        assert!(!command_implies_bypass_permissions(&argv(&[
            "claude",
            "--permission-mode",
            "default",
            "--resume",
            "abc",
        ])));
        // The substring "bypassPermissions" alone (without the flag) does not
        // count — guards against a worktree path or prompt mentioning it.
        assert!(!command_implies_bypass_permissions(&argv(&[
            "claude",
            "echo bypassPermissions is a mode",
        ])));
    }
}
