//! TerminalManager — registry of active terminal sessions.
//!
//! Follows the same HashMap-based manager pattern as `claude_session::SessionManager`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use tracing::{debug, error, info, warn};

use super::interceptor::OutputInterceptor;
use super::session::TerminalSession;
use super::types::{TerminalId, TerminalInfo};
use crate::claude_session::SessionManager;

/// Manages all active terminal sessions.
pub struct TerminalManager {
    sessions: Mutex<HashMap<TerminalId, Arc<TerminalSession>>>,
    interceptor: Arc<OutputInterceptor>,
}

/// Whether opening a terminal in `dir` should pre-accept Claude's workspace
/// trust for it.
///
/// Trust is what stops a directory's `.claude/settings.json` hooks and
/// `.mcp.json` servers from auto-executing, so granting it is a real decision —
/// and creating a terminal is NOT by itself a request to make that decision for
/// an arbitrary directory. Without this gate, opening a plain shell anywhere on
/// the machine would permanently mark that directory trusted in every account,
/// including the operator's own later interactive sessions.
///
/// The population this feature exists for is the fleet's own checkouts: the
/// workspace root and everything beneath it, which is where every managed repo
/// and every allocated agent worktree lives. Anything outside it keeps the
/// dialog, which is the correct default for a directory nobody has vouched for.
fn workspace_trust_is_in_scope(dir: &str) -> bool {
    let Some(root) = crate::mcp::shared::current_project_path() else {
        // No workspace root resolved: we cannot say the directory is ours, so
        // we do not grant trust for it.
        return false;
    };
    is_within(dir, &root)
}

/// Containment test on canonicalized paths, so `..` segments and Windows
/// short/verbatim spellings cannot walk out of the root. Case-insensitive on
/// Windows, where the filesystem is.
fn is_within(dir: &str, root: &str) -> bool {
    let canon = |p: &str| {
        std::fs::canonicalize(p)
            .ok()
            .map(|c| c.to_string_lossy().replace('\\', "/"))
    };
    let (Some(d), Some(r)) = (canon(dir), canon(root)) else {
        return false;
    };
    #[cfg(windows)]
    let (d, r) = (d.to_lowercase(), r.to_lowercase());
    let r_prefix = if r.ends_with('/') {
        r.clone()
    } else {
        format!("{r}/")
    };
    d == r || d.starts_with(&r_prefix)
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
    ///
    /// `resource_override` is forwarded verbatim to [`TerminalSession::spawn`]'s
    /// spawn-time resource gate. It is a required parameter rather than a
    /// defaulted one on purpose: every spawn surface has to state whether it is
    /// an attended spawn (an operator who can be shown a dialog and asked) or an
    /// unattended one, and a default would let the next surface inherit an
    /// answer nobody chose. See [`crate::resource_guard`].
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
        resource_override: bool,
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

        // Pre-accept the Claude workspace-trust dialog for this directory before
        // anything can be typed into the PTY. This is the drift-proof seam for
        // it: several launch surfaces have the FRONTEND compose the
        // `CLAUDE_CONFIG_DIR=… claude …` line and type it in, so the launch-spec
        // builder is not a complete chokepoint, but every one of them first
        // creates a terminal here. Idempotent and best-effort — see
        // `claude_session::workspace_trust`.
        if workspace_trust_is_in_scope(&working_dir) {
            crate::claude_session::workspace_trust::ensure_workspace_trusted(
                &working_dir,
                crate::claude_session::workspace_trust::TrustTargets::EveryKnownAccount,
            );
        }

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
            resource_override,
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

        // D1: the same bypass fact, made DURABLE on the session's lifecycle
        // record. The event below is process-lifetime UI state; the record has
        // to answer "did this session run with permissions bypassed?" after a
        // restart, and that is a safety-relevant question.
        //
        // Stamped for BOTH answers, not only `true`: the runner built this
        // spawn's argv, so `false` here is knowledge ("this spawn carried no
        // bypass flag"), not absence. Reserving `None` for genuine absence is
        // what keeps "unknown" and "not bypassed" distinguishable on the
        // record. A later typed `claude --dangerously-skip-permissions` in the
        // same PTY re-stamps `true` from the resume sniff (last write wins).
        //
        // The record already exists: `TerminalSession::spawn` records the
        // pre-pinned session synchronously through the identity seam before it
        // returns.
        {
            use tauri::Manager;
            if let Some(store) = emitter
                .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
            {
                store.update_identity_by_terminal(
                    &info.id,
                    &crate::session::session_lifecycle_store::SessionIdentityUpdate {
                        bypass_permissions: Some(bypass_permissions),
                        ..Default::default()
                    },
                );
            }
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
        // Durable-title sync (plan 2026-07-03-runner-session-tracking-drift-
        // and-guardrails Phase 3 item 4): mirror the rename into the lifecycle
        // registry so a restart restores the pane under its CURRENT name, not
        // the spawn-time one. No-ops cleanly when the terminal has no open
        // record (plain shell never recorded) or the store isn't managed.
        {
            use tauri::Manager;
            if let Some(store) = app_handle
                .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
            {
                store.update_title_by_terminal(id, &title);
            }
        }
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

    /// Move a terminal session onto a different page and emit a
    /// `terminal-page-changed` Tauri event so other webview windows (and the
    /// backend relay's WS subscribers) re-mount the tab under its new page.
    ///
    /// Phase 5 of the runner-API account-selection plan
    /// (`2026-07-18-runner-api-account-selection.md`): the
    /// `POST /terminals/{id}/move` surface routes here. Mirrors
    /// [`Self::set_title`] — same lookup / mutate / durable-flush / double-emit
    /// shape. The move is also flushed to the lifecycle registry so a restart
    /// restores the pane on its CURRENT page, not the spawn-time one.
    pub fn set_page(
        &self,
        id: &str,
        page_id: String,
        app_handle: &AppHandle,
    ) -> Result<(), String> {
        let session = self
            .get(id)
            .ok_or_else(|| format!("Terminal session not found: {}", id))?;
        session.set_page(page_id.clone());
        // Durable-page sync: mirror the move into the lifecycle registry so a
        // restart restores the pane under its CURRENT page. No-ops cleanly when
        // the terminal has no open record or the store isn't managed.
        {
            use tauri::Manager;
            if let Some(store) = app_handle
                .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
            {
                store.update_page_by_terminal(id, &page_id);
            }
        }
        let payload = serde_json::json!({ "id": id, "pageId": page_id });
        if let Err(e) = app_handle.emit("terminal-page-changed", &payload) {
            error!("Failed to emit terminal-page-changed: {}", e);
        }
        // Mirror to the backend WS relay so remote viewers stay consistent
        // (same pattern as set_title's terminal-title-changed mirror).
        crate::event_system::broadcast_ws_notification(
            app_handle,
            "terminal-page-changed",
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
    /// Coordinator's `project.tasks.assigned_session_id` join) use to
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

    /// Get the number of active sessions, with a poisoned lock rendered as `0`.
    ///
    /// Its one caller numbers a default tab title (`Terminal N`), where a `0`
    /// on a poisoned lock costs a duplicate title and nothing else. Anything
    /// that REPORTS or DECIDES on this number must use [`Self::count_checked`]
    /// and handle the `None` — same split, for the same reason, as
    /// `health_monitor::get_thread_count` vs `thread_count_reading`.
    pub fn count(&self) -> usize {
        self.count_checked().unwrap_or(0)
    }

    /// The number of active sessions, or `None` when the registry could not be
    /// read (a poisoned lock — some other thread panicked while holding it).
    ///
    /// **`None`, never `Some(0)`.** This is the number
    /// `fleet::resource_sample` publishes as `active_terminal_sessions`, and a
    /// fabricated zero there is the worst possible lie: a runner wedged under
    /// ~130 concurrent sessions (2026-08-29) is exactly the box most likely to
    /// have panicked a thread inside this mutex, and it would report itself to
    /// the fleet dashboard as *maximally idle* at the moment it was maximally
    /// loaded. NULL says "this device did not answer", which is true and
    /// rankable; `0` says "this device is free", which is neither.
    pub fn count_checked(&self) -> Option<usize> {
        match self.sessions.lock() {
            Ok(s) => Some(s.len()),
            Err(e) => {
                error!("Sessions lock poisoned: {}", e);
                None
            }
        }
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

    /// Close all terminal sessions, under a GLOBAL deadline. Called on app
    /// shutdown.
    ///
    /// Plan `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 2
    /// step 5. This used to be a bare `for` loop with no cap of any kind, while
    /// each `session.close()` costs a blocking `taskkill /F /T` plus two 2 s
    /// thread joins plus a recursive `remove_dir_all` — i.e. ≈ N × 4 s of
    /// unbounded, `O(terminals)` work that ran inline on the native UI thread.
    ///
    /// Past `deadline` the remaining sessions get a kill-only teardown: the
    /// child process tree still dies (that is the part whose omission would
    /// leak processes), while the thread joins and the shim-dir sweep are
    /// skipped. The imminent `process::exit` reclaims the rest.
    pub fn close_all(&self, deadline: std::time::Instant) {
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
        let mut degraded = 0usize;
        for session in &sessions {
            if std::time::Instant::now() >= deadline {
                session.close_kill_only();
                degraded += 1;
            } else {
                session.close_with_deadline(Some(deadline));
            }
        }
        if degraded > 0 {
            warn!(
                "close_all hit its global deadline — {} of {} terminal session(s) got a \
                 kill-only teardown",
                degraded,
                sessions.len()
            );
        }
        info!("All terminal sessions closed");
    }

    /// Get a reference to the output interceptor (for adding hooks).
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
///
/// `pub(crate)` so the typed-input resume sniff
/// ([`super::claude_resume_sniff`], #548 Phase 2) reuses the SAME matcher —
/// one definition of "bypass-implying command" for spawn-argv and typed paths.
pub(crate) fn command_implies_bypass_permissions(argv: &[String]) -> bool {
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

#[cfg(test)]
mod workspace_trust_scope_tests {
    use super::is_within;

    /// The workspace root itself, and anything beneath it, is in scope — this
    /// is the population the pre-trust exists for (managed repos and allocated
    /// agent worktrees all live here).
    #[test]
    fn the_root_and_its_descendants_are_in_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let nested = tmp.path().join("repo").join("src");
        std::fs::create_dir_all(&nested).unwrap();

        assert!(is_within(&root, &root));
        assert!(is_within(&nested.to_string_lossy(), &root));
    }

    /// A directory outside the workspace keeps the dialog. Granting trust there
    /// would arm that directory's hooks and MCP servers in every account, for a
    /// decision nobody made.
    #[test]
    fn a_sibling_outside_the_root_is_not_in_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        assert!(!is_within(
            &outside.to_string_lossy(),
            &root.to_string_lossy()
        ));
    }

    /// A prefix that is not a path boundary must not count: `.../workspace-evil`
    /// is not inside `.../workspace`.
    #[test]
    fn a_name_prefix_is_not_containment() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        let evil = tmp.path().join("workspace-evil");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&evil).unwrap();

        assert!(!is_within(&evil.to_string_lossy(), &root.to_string_lossy()));
    }

    /// `..` cannot walk out of the root, because both sides are canonicalized.
    #[test]
    fn dotdot_cannot_escape_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let escape = root.join("..").join("elsewhere");
        assert!(!is_within(
            &escape.to_string_lossy(),
            &root.to_string_lossy()
        ));
    }
}
