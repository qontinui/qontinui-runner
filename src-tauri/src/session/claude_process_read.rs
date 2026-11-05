//! Which `claude` runs under a pane — THE process read, shared by the resume
//! door (which VERIFIES a resume it just spawned) and the session ledger (which
//! decides whether a session is BACK after a restart).
//!
//! Plan `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`,
//! Phase 1 (D2): *a session is back only when a process read shows its
//! `claude --resume <id>` running in a pane this boot created or adopted.* The
//! door already answered exactly that question for its own spawns
//! ([`judge`] over [`read_claude_procs`]); the ledger asks it of every pane
//! still bound to an `open` row, so the reader lives here, once.
//!
//! ## An unread pane is UNKNOWN, never "no claude"
//!
//! Every way the read can fail is a [`PaneReadError`], kept apart from an
//! empty `Ok(vec![])` (a pane read successfully with no `claude` in it). The
//! platform snapshot degrades to an EMPTY table on every failure (a `/proc`
//! read error, a timed-out WMI probe), and a machine running this code always
//! has at least one process, so an empty table is
//! [`PaneReadError::TableUnreadable`].

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::process_capture::process_tree as pt;
use crate::terminal::TerminalManager;

/// One live `claude` process as read from the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeProc {
    pub pid: u32,
    pub cmdline: String,
    /// `None` = this platform could not say (never a guess).
    pub cwd: Option<String>,
    /// `CLAUDE_CONFIG_DIR`; `None` = this platform could not say.
    pub config_dir: Option<String>,
}

/// What reading the process table showed about a resumed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessEvidence {
    pub terminal_id: String,
    pub pid: Option<u32>,
    pub cmdline: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub cwd: Option<String>,
    /// `null` = this platform could not say (never a guess).
    pub claude_config_dir: Option<String>,
}

/// Why a pane's processes could not be read. Each one is UNKNOWN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneReadError {
    /// The terminal manager does not hold the pane.
    PaneAbsent,
    /// The pane has no child pid to walk from.
    NoChildPid,
    /// The process table could not be read (the snapshot came back empty).
    TableUnreadable,
    /// A `claude` was found under the pane and its command line could not be
    /// read (a WMI timeout or spawn error on Windows, an unreadable
    /// `/proc/<pid>/cmdline` on Linux), so which session it holds is unknown.
    CmdlineUnreadable,
}

/// One pane's read: the `claude` processes in its subtree, or why there is no
/// answer.
pub type PaneRead = Result<Vec<ClaudeProc>, PaneReadError>;

/// What a session's `claude` must look like for a read to count as it.
/// `None` for a directory = the row recorded none, so it is not compared.
#[derive(Debug, Clone, Copy)]
pub struct ResumeExpectation<'a> {
    pub session_id: &'a str,
    pub working_dir: Option<&'a str>,
    pub config_dir: Option<&'a str>,
}

/// `Ok` only when exactly one `claude` holding the session id is present and
/// every fact the platform could read agrees with the expectation.
/// `Err(None)` = not there (yet); `Err(Some(reason))` = present but wrong.
pub fn judge(
    procs: &[ClaudeProc],
    want: &ResumeExpectation<'_>,
    terminal_id: &str,
) -> Result<ProcessEvidence, Option<String>> {
    let matching: Vec<&ClaudeProc> = procs
        .iter()
        .filter(|p| {
            pt::parse_session_id_from_cmdline(&p.cmdline).as_deref() == Some(want.session_id)
        })
        .collect();
    let p = match matching.as_slice() {
        [] => return Err(None),
        [one] => *one,
        many => return Err(Some(format!("multiple_resume_processes({})", many.len()))),
    };
    if let (Some(cwd), Some(want_cwd)) = (&p.cwd, want.working_dir) {
        if !same_path(cwd, want_cwd) {
            return Err(Some(format!("cwd_mismatch(want {want_cwd}, got {cwd})")));
        }
    }
    if let (Some(dir), Some(want_dir)) = (&p.config_dir, want.config_dir) {
        if !same_path(dir, want_dir) {
            return Err(Some(format!(
                "config_dir_mismatch(want {want_dir}, got {dir})"
            )));
        }
    }
    Ok(ProcessEvidence {
        terminal_id: terminal_id.to_string(),
        pid: Some(p.pid),
        cmdline: Some(p.cmdline.clone()),
        cwd: p.cwd.clone(),
        claude_config_dir: p.config_dir.clone(),
    })
}

/// Path equality tolerant of trailing separators, `\` vs `/`, and symlinks
/// where both sides resolve. Case-insensitive only where the filesystem is
/// (Windows, macOS): on Linux `/Work` and `/work` are different directories,
/// and calling them equal would pass a resume pinned to the wrong tree.
pub fn same_path(a: &str, b: &str) -> bool {
    same_path_with(a, b, cfg!(any(windows, target_os = "macos")))
}

fn same_path_with(a: &str, b: &str, case_insensitive: bool) -> bool {
    let norm = |s: &str| {
        let canon = std::fs::canonicalize(s)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| s.to_string());
        let canon = canon.replace('\\', "/").trim_end_matches('/').to_string();
        if case_insensitive {
            canon.to_lowercase()
        } else {
            canon
        }
    };
    norm(a) == norm(b)
}

/// `CLAUDE_CONFIG_DIR` of a live process. Linux only (`/proc/<pid>/environ`);
/// elsewhere `None`, which is "could not say", never a match.
#[cfg(target_os = "linux")]
fn config_dir_of_pid(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    bytes
        .split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CLAUDE_CONFIG_DIR=").map(str::to_string))
        .filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn config_dir_of_pid(_pid: u32) -> Option<String> {
    None
}

/// The process table, or [`PaneReadError::TableUnreadable`] when the snapshot
/// came back empty (see the module docs).
pub async fn snapshot() -> Result<pt::ProcessSnapshot, PaneReadError> {
    let snap = pt::snapshot_process_table_public().await;
    if snap.names.is_empty() {
        return Err(PaneReadError::TableUnreadable);
    }
    Ok(snap)
}

/// The `claude` processes under each `(key, root pid)`, from ONE snapshot.
async fn claude_procs_under(
    roots: &[(String, u32)],
) -> Result<HashMap<String, PaneRead>, PaneReadError> {
    let snap = snapshot().await?;
    let per_root: Vec<(String, Vec<u32>)> = roots
        .iter()
        .map(|(key, root)| {
            (
                key.clone(),
                pt::claude_pids_in_inclusive_subtree(*root, &snap),
            )
        })
        .collect();
    let all: Vec<u32> = per_root
        .iter()
        .flat_map(|(_, p)| p.iter().copied())
        .collect();
    let cmdlines = pt::command_lines_for_pids(&all).await;
    let cwds = pt::working_directories_for_pids(&all).await;
    Ok(per_root
        .into_iter()
        .map(|(key, pids)| {
            (
                key,
                pane_read_of(&pids, &cmdlines, &cwds, config_dir_of_pid),
            )
        })
        .collect())
}

/// PURE: one pane's read from the per-pid facts. A `claude` pid with no
/// command line makes the whole pane [`PaneReadError::CmdlineUnreadable`]:
/// dropping it would read a running `claude` as "no claude here". A cwd the
/// kernel marks ` (deleted)` (Linux, a removed directory) is compared as the
/// path it names.
fn pane_read_of(
    pids: &[u32],
    cmdlines: &HashMap<u32, String>,
    cwds: &HashMap<u32, String>,
    config_dir_of: impl Fn(u32) -> Option<String>,
) -> PaneRead {
    pids.iter()
        .map(|&pid| {
            let cmdline = cmdlines
                .get(&pid)
                .ok_or(PaneReadError::CmdlineUnreadable)?
                .clone();
            Ok(ClaudeProc {
                pid,
                cmdline,
                cwd: cwds
                    .get(&pid)
                    .map(|c| without_deleted_suffix(c).to_string()),
                config_dir: config_dir_of(pid),
            })
        })
        .collect()
}

/// `/proc/<pid>/cwd` of a directory removed since reads `<path> (deleted)`.
fn without_deleted_suffix(cwd: &str) -> &str {
    cwd.strip_suffix(" (deleted)").unwrap_or(cwd)
}

/// Read every pane in `terminal_ids` from one snapshot. Every id gets an
/// entry; a pane that cannot be read carries its [`PaneReadError`].
pub async fn read_panes(
    tm: &TerminalManager,
    terminal_ids: impl IntoIterator<Item = String>,
) -> HashMap<String, PaneRead> {
    let mut out: HashMap<String, PaneRead> = HashMap::new();
    let mut roots: Vec<(String, u32)> = Vec::new();
    for id in terminal_ids {
        match tm.get(&id) {
            None => {
                out.insert(id, Err(PaneReadError::PaneAbsent));
            }
            Some(session) => match session.child_pid() {
                None => {
                    out.insert(id, Err(PaneReadError::NoChildPid));
                }
                Some(pid) => roots.push((id, pid)),
            },
        }
    }
    if roots.is_empty() {
        return out;
    }
    match claude_procs_under(&roots).await {
        Ok(read) => out.extend(read),
        Err(e) => out.extend(roots.into_iter().map(|(id, _)| (id, Err(e)))),
    }
    out
}

/// Read the `claude` processes under one pane.
pub async fn read_claude_procs(tm: &Arc<TerminalManager>, terminal_id: &str) -> PaneRead {
    read_panes(tm, [terminal_id.to_string()])
        .await
        .remove(terminal_id)
        .unwrap_or(Err(PaneReadError::PaneAbsent))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b9f6c1e-1111-4222-8333-444455556666";

    fn want() -> ResumeExpectation<'static> {
        ResumeExpectation {
            session_id: ID,
            working_dir: Some("/work"),
            config_dir: Some("/cfg"),
        }
    }

    fn proc(cmd: &str, cwd: Option<&str>, cfg: Option<&str>) -> ClaudeProc {
        ClaudeProc {
            pid: 42,
            cmdline: cmd.to_string(),
            cwd: cwd.map(String::from),
            config_dir: cfg.map(String::from),
        }
    }

    #[test]
    fn path_case_folds_only_where_the_filesystem_does() {
        assert!(same_path_with("/Work/Proj", "/work/proj", true));
        assert!(!same_path_with("/Work/Proj", "/work/proj", false));
        assert!(same_path_with("/work/proj/", "/work/proj", false));
        assert_eq!(
            same_path("/Work/Proj", "/work/proj"),
            cfg!(any(windows, target_os = "macos"))
        );
    }

    #[test]
    fn judge_requires_exactly_one_matching_resume_process() {
        let good = format!("/bin/claude --permission-mode bypassPermissions --resume {ID}");
        // Not there yet: keep polling.
        assert_eq!(judge(&[], &want(), "t").unwrap_err(), None);
        let other = "/bin/claude --resume 11111111-1111-4111-8111-111111111111";
        assert_eq!(
            judge(&[proc(other, None, None)], &want(), "t").unwrap_err(),
            None
        );
        // One match, unreadable cwd/config: accepted, fields stay null.
        let ev = judge(&[proc(&good, None, None)], &want(), "t").unwrap();
        assert_eq!(
            (ev.pid, ev.cwd, ev.claude_config_dir),
            (Some(42), None, None)
        );
        // Two matches: a double spawn is a failure, not a success.
        let two = [proc(&good, None, None), proc(&good, None, None)];
        assert_eq!(
            judge(&two, &want(), "t").unwrap_err(),
            Some("multiple_resume_processes(2)".to_string())
        );
    }

    #[test]
    fn judge_fails_on_a_wrong_cwd_or_config_dir_but_not_on_unknown() {
        let good = format!("claude --resume {ID}");
        let bad_cwd = judge(&[proc(&good, Some("/elsewhere"), None)], &want(), "t");
        assert!(bad_cwd.unwrap_err().unwrap().starts_with("cwd_mismatch"));
        let bad_cfg = judge(&[proc(&good, Some("/work"), Some("/other"))], &want(), "t");
        assert!(bad_cfg
            .unwrap_err()
            .unwrap()
            .starts_with("config_dir_mismatch"));
        assert!(judge(&[proc(&good, Some("/work/"), Some("/cfg"))], &want(), "t").is_ok());
    }

    /// A `claude` whose command line could not be read is UNKNOWN, never
    /// dropped: dropping it read a running `claude` as an empty pane.
    #[test]
    fn a_claude_pid_with_no_cmdline_makes_the_pane_unreadable() {
        let cmd: HashMap<u32, String> = [(1, format!("claude --resume {ID}"))].into();
        let none = HashMap::new();
        assert_eq!(
            pane_read_of(&[1, 2], &cmd, &none, |_| None),
            Err(PaneReadError::CmdlineUnreadable)
        );
        // Windows' WMI timeout: an empty command-line map for found pids.
        assert_eq!(
            pane_read_of(&[1], &none, &none, |_| None),
            Err(PaneReadError::CmdlineUnreadable)
        );
        // No claude pid at all is a real, readable, empty pane.
        assert_eq!(pane_read_of(&[], &none, &none, |_| None), Ok(Vec::new()));
        let one = pane_read_of(&[1], &cmd, &none, |_| Some("/cfg".to_string())).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].config_dir.as_deref(), Some("/cfg"));
    }

    /// Linux marks the cwd of a removed directory ` (deleted)`; the path it
    /// names is what the row recorded, so the suffix must not fail the match.
    #[test]
    fn a_deleted_cwd_is_compared_as_the_path_it_names() {
        let cmd: HashMap<u32, String> = [(1, format!("claude --resume {ID}"))].into();
        let cwd: HashMap<u32, String> = [(1, "/work (deleted)".to_string())].into();
        let procs = pane_read_of(&[1], &cmd, &cwd, |_| None).unwrap();
        assert_eq!(procs[0].cwd.as_deref(), Some("/work"));
        assert!(judge(&procs, &want(), "t").is_ok());
        assert_eq!(without_deleted_suffix("/work"), "/work");
    }

    /// A row that recorded no directory is not compared on it: the read's own
    /// value is evidence, the absent expectation is not a mismatch.
    #[test]
    fn judge_skips_a_directory_the_row_never_recorded() {
        let good = format!("claude --resume {ID}");
        let bare = ResumeExpectation {
            session_id: ID,
            working_dir: None,
            config_dir: None,
        };
        assert!(judge(&[proc(&good, Some("/anywhere"), Some("/any"))], &bare, "t").is_ok());
    }
}
