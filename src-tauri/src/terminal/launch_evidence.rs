//! Launch evidence for a terminal pane whose Claude launch failed, and the
//! per-pane read of the shim's launch-failure record.
//!
//! Plan `2026-11-20-terminal-launch-failure-analyze-button`, Phase 2. The
//! "Analyze launch" button captures the failed pane into one local file and
//! opens an analyzer session whose first turn is
//! `/analyze-launch-failure <that file>`. This module owns the capture:
//!
//! - [`capture_launch_evidence`] writes
//!   `~/.qontinui/launch-evidence/<terminal_id>-<utc yyyymmddThhmmssZ>.md`
//!   (mode `0600` off Windows — the scrollback may hold secrets, D6), keeps
//!   only the newest [`KEEP_NEWEST`] files in that directory, and provisions the
//!   fleet commands into the pane's cwd so the analyzer pane opened there can
//!   resolve `/analyze-launch-failure` (D2).
//! - [`read_launch_failure`] serves the in-memory
//!   [`LaunchFailureRegistry`](super::launch_failure::LaunchFailureRegistry)
//!   entry through a serializable view. An absent entry is `recorded: false` —
//!   "nothing was reported", never "the launch succeeded".
//!
//! Both are reached from a Tauri command (`commands/terminal.rs`) and an HTTP
//! twin (`mcp/terminals.rs`).

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;

use super::launch_failure::LaunchFailure;
use super::{strip_ansi, TerminalManager};

/// The directory under `~/.qontinui` the evidence files are written to.
pub const EVIDENCE_DIR_NAME: &str = "launch-evidence";

/// How much ANSI-stripped scrollback an evidence file keeps: the last 256 KiB.
pub const SCROLLBACK_TAIL_BYTES: usize = 256 * 1024;

/// How many evidence files survive a capture; older ones are deleted.
pub const KEEP_NEWEST: usize = 50;

/// What the file records when the caller did not know the launch command —
/// the operator typed `claude …` by hand, so no launch record exists.
pub const UNKNOWN_COMMAND: &str = "UNKNOWN (typed by hand)";

/// The slash command the analyzer pane's first turn names. Its presence in the
/// cwd after provisioning is what `commands_provisioned` reports.
pub const ANALYZER_COMMAND_FILE: &str = "analyze-launch-failure.md";

/// What a capture answers: where the file is, the cwd the analyzer should open
/// in, and whether `/analyze-launch-failure` now resolves from that cwd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchEvidence {
    pub path: String,
    pub cwd: Option<String>,
    pub commands_provisioned: bool,
}

/// Why a capture produced no file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// No live terminal has this id.
    TerminalNotFound(String),
    /// `~/.qontinui` could not be resolved (no home directory).
    NoEvidenceDir,
    /// Creating the directory or writing the file failed.
    Io(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TerminalNotFound(id) => write!(f, "Terminal not found: {id}"),
            Self::NoEvidenceDir => write!(
                f,
                "launch evidence: no ~/.qontinui directory resolves (no home directory)"
            ),
            Self::Io(e) => write!(f, "launch evidence: write failed: {e}"),
        }
    }
}

/// The pane's PTY state at capture time. The SHELL's state, not Claude's: a
/// failed `claude` launch normally leaves the shell alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneState {
    pub alive: bool,
    pub exit_code: Option<i32>,
}

impl PaneState {
    fn render(&self) -> String {
        match (self.alive, self.exit_code) {
            (true, _) => "alive (the pane's shell is running)".to_string(),
            (false, Some(code)) => format!("exited (code {code})"),
            (false, None) => "exited (code UNKNOWN)".to_string(),
        }
    }
}

/// Everything one evidence file states, gathered before it is rendered.
#[derive(Debug, Clone)]
pub struct EvidenceRecord<'a> {
    pub terminal_id: &'a str,
    pub captured_at: DateTime<Utc>,
    pub launch_command: Option<&'a str>,
    /// The account and where it came from (`"caller"` / `"session-lifecycle record"`).
    pub account: Option<(&'a str, &'a str)>,
    pub pinned_session_id: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub failure: Option<&'a LaunchFailure>,
    pub pane: PaneState,
    pub build_id: &'a str,
    pub git_sha: &'a str,
    /// Raw PTY bytes from the scrollback ring.
    pub scrollback: &'a [u8],
}

/// `Some(trimmed)` for a non-blank string, `None` otherwise — a blank field from
/// a JSON body or the UI is "not given", never an empty value to record.
fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// The last [`SCROLLBACK_TAIL_BYTES`] of the ANSI-stripped scrollback, cut on a
/// char boundary, and whether anything was cut. Stripped BEFORE the cut, so a
/// cut can never land inside an escape sequence and leave half of it behind.
pub fn scrollback_tail(raw: &[u8]) -> (String, bool) {
    let text = strip_ansi(&String::from_utf8_lossy(raw));
    if text.len() <= SCROLLBACK_TAIL_BYTES {
        return (text, false);
    }
    let mut start = text.len() - SCROLLBACK_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text.get(start..).unwrap_or_default().to_string(), true)
}

/// A backtick fence longer than any backtick run in `text`, so the scrollback
/// can never close its own code block.
fn fence_for(text: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

fn rfc3339_ms(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

/// Render one evidence file. Every field is present in every file; an unknown
/// value is written as such, never omitted.
pub fn render_evidence(r: &EvidenceRecord<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Launch evidence: terminal {}\n\n", r.terminal_id));
    out.push_str(&format!(
        "- captured_at: {}\n",
        r.captured_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    let command = non_blank(r.launch_command).unwrap_or(UNKNOWN_COMMAND);
    out.push_str(&format!("- launch_command: {command}\n"));
    match r.account {
        Some((dir, source)) => {
            out.push_str(&format!("- account: {dir}\n"));
            out.push_str(&format!("- account_source: {source}\n"));
        }
        None => {
            out.push_str("- account: UNKNOWN (not recorded)\n");
            out.push_str("- account_source: none\n");
        }
    }
    out.push_str(&format!(
        "- pinned_session_id: {}\n",
        non_blank(r.pinned_session_id).unwrap_or("none")
    ));
    out.push_str(&format!(
        "- cwd: {}\n",
        non_blank(r.cwd).unwrap_or("UNKNOWN (no live working directory)")
    ));
    out.push_str(&format!("- pane: {}\n", r.pane.render()));
    out.push_str(&format!(
        "- runner: buildId={} gitSha={}\n",
        r.build_id, r.git_sha
    ));

    out.push_str("\n## Shim launch-failure record\n\n");
    match r.failure {
        Some(f) => {
            out.push_str(&format!("- kind: {}\n", f.event));
            out.push_str(&format!("- detail: {}\n", f.detail));
            out.push_str(&format!("- settings: {}\n", f.settings));
            out.push_str(&format!("- user_chose_session: {}\n", f.user_chose_session));
            out.push_str(&format!("- at: {} (unix ms {})\n", rfc3339_ms(f.at_ms), f.at_ms));
        }
        None => out.push_str("none recorded\n"),
    }

    let (tail, truncated) = scrollback_tail(r.scrollback);
    out.push_str("\n## Scrollback (ANSI-stripped");
    if truncated {
        out.push_str(&format!(", last {SCROLLBACK_TAIL_BYTES} bytes"));
    }
    out.push_str(")\n\n");
    let fence = fence_for(&tail);
    out.push_str(&format!("{fence}text\n{tail}"));
    if !tail.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{fence}\n"));
    out
}

/// `<terminal_id>-<yyyymmddThhmmssZ>.md`, with every character outside
/// `[A-Za-z0-9_-]` in the id replaced by `_` so an id can never escape the
/// directory.
pub fn evidence_file_name(terminal_id: &str, at: DateTime<Utc>) -> String {
    let id: String = terminal_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{id}-{}.md", at.format("%Y%m%dT%H%M%SZ"))
}

/// Create `path` exclusively, owner-only (`0600`) off Windows.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Write `body` to `dir/file_name`, never overwriting: a second capture of the
/// same terminal in the same second gets a `-2`, `-3`, … suffix.
pub fn write_evidence_file(dir: &Path, file_name: &str, body: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    for n in 1u32..=100 {
        let path = if n == 1 {
            dir.join(file_name)
        } else {
            dir.join(format!("{stem}-{n}.md"))
        };
        match create_private(&path) {
            Ok(mut f) => {
                f.write_all(body.as_bytes())?;
                f.sync_all()?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("100 evidence files named {stem}* already exist"),
    ))
}

/// Delete every `.md` file in `dir` except the newest `keep` (by modification
/// time, ties broken by name). Returns how many were deleted. Anything that is
/// not a regular `.md` file is left alone.
pub fn prune_evidence_dir(dir: &Path, keep: usize) -> std::io::Result<usize> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if !meta.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        files.push((mtime, path));
    }
    files.sort_by(|a, b| b.cmp(a));
    let mut deleted = 0;
    for (_, path) in files.into_iter().skip(keep) {
        match std::fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(e) => tracing::warn!(
                path = %path.display(),
                "launch evidence: retention could not delete an old capture: {e}"
            ),
        }
    }
    Ok(deleted)
}

/// Render, write and prune into `dir`, then provision through `provision`.
/// The settings- and app-free core of [`capture_launch_evidence`].
pub fn capture_into(
    dir: &Path,
    record: &EvidenceRecord<'_>,
    provision: impl FnOnce(&str) -> bool,
) -> Result<LaunchEvidence, CaptureError> {
    let body = render_evidence(record);
    let name = evidence_file_name(record.terminal_id, record.captured_at);
    let path =
        write_evidence_file(dir, &name, &body).map_err(|e| CaptureError::Io(e.to_string()))?;
    if let Err(e) = prune_evidence_dir(dir, KEEP_NEWEST) {
        tracing::warn!(dir = %dir.display(), "launch evidence: retention pass failed: {e}");
    }
    let cwd = non_blank(record.cwd).map(str::to_string);
    let commands_provisioned = cwd.as_deref().is_some_and(provision);
    Ok(LaunchEvidence {
        path: path.display().to_string(),
        cwd,
        commands_provisioned,
    })
}

/// Provision the fleet commands into `cwd` through the guarded operator-cwd
/// chokepoint, and report whether `/analyze-launch-failure` resolves from it
/// afterwards. A repo-authored `.claude/` is skipped by that chokepoint; it
/// still counts as provisioned when it already carries the command.
fn provision_analyzer_command(cwd: &str) -> bool {
    crate::agent_worktree::isolated_edit::provision_fleet_for_cwd(cwd);
    Path::new(cwd)
        .join(".claude")
        .join("commands")
        .join(ANALYZER_COMMAND_FILE)
        .is_file()
}

/// The account the pane was spawned on, from the session-lifecycle store's
/// sticky record — NOT re-resolved through the roster, because a logged-out
/// account is evidence here, not a reason to drop it.
fn recorded_account(terminal_id: &str) -> Option<String> {
    use std::sync::Arc;
    use tauri::Manager;

    let app = crate::tauri_app_handle::current()?;
    let store =
        app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()?;
    store
        .find_open_by_terminal(terminal_id)?
        .config_dir
        .filter(|d| !d.trim().is_empty())
}

/// Capture the evidence file for `terminal_id` (see the module docs). Blocking:
/// provisioning probes git and resolves the command registry, so async callers
/// run this on a blocking thread.
pub fn capture_launch_evidence(
    terminal_manager: &TerminalManager,
    terminal_id: &str,
    launch_command: Option<String>,
    config_dir: Option<String>,
    pinned_session_id: Option<String>,
) -> Result<LaunchEvidence, CaptureError> {
    let session = terminal_manager
        .get(terminal_id)
        .ok_or_else(|| CaptureError::TerminalNotFound(terminal_id.to_string()))?;
    let dir = qontinui_runner_lib::ambient::qontinui_dir()
        .ok_or(CaptureError::NoEvidenceDir)?
        .join(EVIDENCE_DIR_NAME);

    let (scrollback, _) = session.get_scrollback_buffer();
    let pane = PaneState {
        alive: session.is_alive(),
        exit_code: session.exit_code(),
    };
    let cwd = super::context_watcher::inherited_context(terminal_id).cwd;
    let now = Utc::now();
    let failure = super::launch_failure::registry().get(terminal_id, now.timestamp_millis());
    let caller_account = non_blank(config_dir.as_deref()).map(str::to_string);
    let stored_account = if caller_account.is_none() {
        recorded_account(terminal_id)
    } else {
        None
    };
    let account = match (&caller_account, &stored_account) {
        (Some(a), _) => Some((a.as_str(), "caller")),
        (None, Some(a)) => Some((a.as_str(), "session-lifecycle record")),
        (None, None) => None,
    };

    let record = EvidenceRecord {
        terminal_id,
        captured_at: now,
        launch_command: launch_command.as_deref(),
        account,
        pinned_session_id: pinned_session_id.as_deref(),
        cwd: cwd.as_deref(),
        failure: failure.as_ref(),
        pane,
        build_id: env!("RUNNER_BUILD_ID"),
        git_sha: env!("QONTINUI_GIT_SHA"),
        scrollback: &scrollback,
    };
    capture_into(&dir, &record, provision_analyzer_command)
}

/// The serializable view of one terminal's launch-failure record.
/// `{"recorded": false}` when the registry holds nothing for it (never
/// reported, or older than the registry's TTL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFailureView {
    pub recorded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_chose_session: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
}

impl LaunchFailureView {
    pub fn from_failure(failure: Option<&LaunchFailure>) -> Self {
        match failure {
            Some(f) => Self {
                recorded: true,
                kind: Some(f.event.clone()),
                detail: Some(f.detail.clone()),
                settings: Some(f.settings),
                user_chose_session: Some(f.user_chose_session),
                at_ms: Some(f.at_ms),
            },
            None => Self {
                recorded: false,
                kind: None,
                detail: None,
                settings: None,
                user_chose_session: None,
                at_ms: None,
            },
        }
    }
}

/// This process's launch-failure record for `terminal_id`, as a view.
pub fn read_launch_failure(terminal_id: &str) -> LaunchFailureView {
    let now_ms = Utc::now().timestamp_millis();
    LaunchFailureView::from_failure(
        super::launch_failure::registry()
            .get(terminal_id, now_ms)
            .as_ref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT_MS: i64 = 1_795_169_700_000; // 2026-11-20T10:15:00Z

    fn at() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(AT_MS).single().unwrap()
    }

    fn failure() -> LaunchFailure {
        LaunchFailure::from_beacon(
            "resolve-failed",
            "claude not found after 20 tries settings=true",
            AT_MS - 5_000,
        )
        .unwrap()
    }

    fn record<'a>(
        command: Option<&'a str>,
        failure: Option<&'a LaunchFailure>,
        scrollback: &'a [u8],
    ) -> EvidenceRecord<'a> {
        EvidenceRecord {
            terminal_id: "t1",
            captured_at: at(),
            launch_command: command,
            account: Some(("/h/.claude-x", "caller")),
            pinned_session_id: Some("abc"),
            cwd: Some("/work/repo"),
            failure,
            pane: PaneState {
                alive: true,
                exit_code: None,
            },
            build_id: "B1",
            git_sha: "deadbeef",
            scrollback,
        }
    }

    #[test]
    fn evidence_file_layout_is_exact() {
        let f = failure();
        let body = render_evidence(&record(
            Some("CLAUDE_CONFIG_DIR=\"/h/.claude-x\" claude --session-id abc"),
            Some(&f),
            b"$ claude\r\n\x1b[31mclaude: Text file busy\x1b[0m\r\n",
        ));
        assert_eq!(
            body,
            "# Launch evidence: terminal t1\n\
             \n\
             - captured_at: 2026-11-20T10:15:00Z\n\
             - launch_command: CLAUDE_CONFIG_DIR=\"/h/.claude-x\" claude --session-id abc\n\
             - account: /h/.claude-x\n\
             - account_source: caller\n\
             - pinned_session_id: abc\n\
             - cwd: /work/repo\n\
             - pane: alive (the pane's shell is running)\n\
             - runner: buildId=B1 gitSha=deadbeef\n\
             \n\
             ## Shim launch-failure record\n\
             \n\
             - kind: resolve-failed\n\
             - detail: claude not found after 20 tries settings=true\n\
             - settings: true\n\
             - user_chose_session: false\n\
             - at: 2026-11-20T10:14:55.000Z (unix ms 1795169695000)\n\
             \n\
             ## Scrollback (ANSI-stripped)\n\
             \n\
             ```text\n\
             $ claude\r\nclaude: Text file busy\r\n\
             ```\n"
        );
    }

    #[test]
    fn evidence_file_name_is_terminal_id_and_utc_stamp() {
        assert_eq!(evidence_file_name("t1", at()), "t1-20261120T101500Z.md");
        assert_eq!(
            evidence_file_name("../x y", at()),
            "___x_y-20261120T101500Z.md"
        );
    }

    #[test]
    fn missing_command_is_unknown_typed_by_hand() {
        for command in [None, Some(""), Some("  ")] {
            let body = render_evidence(&record(command, None, b""));
            assert!(
                body.contains("\n- launch_command: UNKNOWN (typed by hand)\n"),
                "{body}"
            );
        }
    }

    #[test]
    fn registry_entry_absent_is_none_recorded() {
        let body = render_evidence(&record(Some("claude"), None, b""));
        assert!(
            body.contains("## Shim launch-failure record\n\nnone recorded\n"),
            "{body}"
        );
        assert!(!body.contains("- kind:"));
    }

    #[test]
    fn registry_entry_present_is_rendered() {
        let f = failure();
        let body = render_evidence(&record(Some("claude"), Some(&f), b""));
        assert!(body.contains("\n- kind: resolve-failed\n"), "{body}");
        assert!(!body.contains("none recorded"));
    }

    #[test]
    fn scrollback_keeps_exactly_the_last_256_kib() {
        let mut raw = vec![b'a'; 10];
        raw.extend(vec![b'b'; SCROLLBACK_TAIL_BYTES]);
        let (tail, truncated) = scrollback_tail(&raw);
        assert!(truncated);
        assert_eq!(tail.len(), 262_144);
        assert!(tail.bytes().all(|b| b == b'b'));

        let (short, cut) = scrollback_tail(b"hello");
        assert_eq!((short.as_str(), cut), ("hello", false));

        let body = render_evidence(&record(Some("claude"), None, &raw));
        assert!(body.contains("## Scrollback (ANSI-stripped, last 262144 bytes)\n"));
    }

    #[test]
    fn scrollback_cut_lands_on_a_char_boundary() {
        // '€' is three bytes and 262144 ≡ 1 (mod 3), so 87382 of them (262146
        // bytes) put the raw cut at byte 2 — inside the first char. The cut
        // moves forward to byte 3, keeping 87381 whole chars.
        let raw = "€".repeat(87_382).into_bytes();
        let (tail, truncated) = scrollback_tail(&raw);
        assert!(truncated);
        assert_eq!(tail.len(), 262_143);
        assert!(tail.chars().all(|c| c == '€'));
    }

    #[test]
    fn a_backtick_fence_in_scrollback_cannot_close_the_block() {
        let body = render_evidence(&record(Some("claude"), None, b"```\nboom\n"));
        assert!(body.contains("\n````text\n```\nboom\n````\n"), "{body}");
    }

    #[cfg(unix)]
    #[test]
    fn evidence_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("launch-evidence");
        let out = capture_into(&dir, &record(Some("claude"), None, b"x"), |_| true).unwrap();
        let mode = std::fs::metadata(&out.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn capture_writes_the_named_file_and_reports_cwd_and_provisioning() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("launch-evidence");
        let mut seen = None;
        let out = capture_into(&dir, &record(Some("claude"), None, b"x"), |cwd| {
            seen = Some(cwd.to_string());
            true
        })
        .unwrap();
        assert_eq!(
            out,
            LaunchEvidence {
                path: dir.join("t1-20261120T101500Z.md").display().to_string(),
                cwd: Some("/work/repo".to_string()),
                commands_provisioned: true,
            }
        );
        assert_eq!(seen.as_deref(), Some("/work/repo"));

        // Same terminal, same second: a second file, never an overwrite.
        let again = capture_into(&dir, &record(Some("claude"), None, b"y"), |_| false).unwrap();
        assert_eq!(
            again.path,
            dir.join("t1-20261120T101500Z-2.md").display().to_string()
        );
        assert!(!again.commands_provisioned);
    }

    #[test]
    fn unknown_cwd_is_never_provisioned() {
        let td = tempfile::tempdir().unwrap();
        let mut rec = record(Some("claude"), None, b"x");
        rec.cwd = None;
        let out = capture_into(td.path(), &rec, |_| panic!("no cwd to provision")).unwrap();
        assert_eq!(out.cwd, None);
        assert!(!out.commands_provisioned);
    }

    #[test]
    fn retention_keeps_the_newest_50() {
        let td = tempfile::tempdir().unwrap();
        let base = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        for i in 0..55u64 {
            let p = td.path().join(format!("t{i:02}-x.md"));
            let f = std::fs::File::create(&p).unwrap();
            f.set_modified(base + std::time::Duration::from_secs(i))
                .unwrap();
        }
        std::fs::write(td.path().join("keep.txt"), b"not evidence").unwrap();

        assert_eq!(prune_evidence_dir(td.path(), KEEP_NEWEST).unwrap(), 5);
        let mut left: Vec<String> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), 51);
        assert_eq!(left.first().map(String::as_str), Some("keep.txt"));
        assert_eq!(left.get(1).map(String::as_str), Some("t05-x.md"));
        assert_eq!(left.last().map(String::as_str), Some("t54-x.md"));
    }

    #[test]
    fn launch_failure_view_serializes_recorded_and_not() {
        assert_eq!(
            serde_json::to_value(LaunchFailureView::from_failure(None)).unwrap(),
            serde_json::json!({"recorded": false})
        );
        let f = failure();
        assert_eq!(
            serde_json::to_value(LaunchFailureView::from_failure(Some(&f))).unwrap(),
            serde_json::json!({
                "recorded": true,
                "kind": "resolve-failed",
                "detail": "claude not found after 20 tries settings=true",
                "settings": true,
                "userChoseSession": false,
                "atMs": 1_795_169_695_000_i64,
            })
        );
    }

    #[test]
    fn launch_evidence_serializes_camel_case() {
        let ev = LaunchEvidence {
            path: "/p.md".to_string(),
            cwd: None,
            commands_provisioned: false,
        };
        assert_eq!(
            serde_json::to_value(ev).unwrap(),
            serde_json::json!({"path": "/p.md", "cwd": null, "commandsProvisioned": false})
        );
    }
}
