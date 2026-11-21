//! The directories a session's transcript names, and whether it proves the
//! session called no write-capable tool — the transcript source of the
//! `finished_close` custody gather (plan
//! `2026-11-20-triage-sessions-custody-reads-the-sessions-recorded-trees-not-its-cwd`,
//! D1, D2 and D6).
//!
//! ## What is read
//!
//! * The session's main transcript `<project>/<id>.jsonl`: the NEWEST copy, by
//!   the locator the close guard's revival arm already uses
//!   ([`super::transcript_facts::find_copies`] and
//!   [`super::transcript_facts::newest_copy`], which order copies by
//!   [`crate::agent_worktree::custody::order_by_newest_turn`]). When several
//!   copies cannot be ranked, every copy is read and the proof does not hold.
//! * Every subagent sidechain under `<project>/<id>/subagents/`, at any depth
//!   down to [`SIDECHAIN_MAX_DEPTH`] (a workflow's agents sit one level down,
//!   in `subagents/workflows/<run>/`). The sidechains beside EVERY copy are
//!   read. An account migration copies the main transcript, and nothing
//!   establishes that it copies the `<id>/subagents` tree beside it.
//!
//! Only structured JSONL fields are read, never prose and never shell text.
//! From each record that is the record's `cwd`; the parent directory of each
//! write-tool target (`file_path` of `Edit`, `Write` and `MultiEdit`,
//! `notebook_path` of `NotebookEdit`; a relative target is taken against the
//! record's own `cwd`); and the name of every `tool_use`. The directories are
//! returned unresolved. The custody gather resolves each one to the worktree
//! holding it, and one in no worktree contributes nothing.
//!
//! ## What the read establishes (D2)
//!
//! [`TranscriptScan::read`] says whether every file was read to its end with
//! every line parsed as a record ([`TranscriptRead::incomplete`]), and names
//! the first `tool_use` of a tool outside [`READ_ONLY_TOOLS`]
//! ([`TranscriptRead::write_call`]). The no-write proof holds only when there
//! is neither. So `Bash`, `PowerShell`, `Edit`, `Write`, `MultiEdit`,
//! `NotebookEdit`, `Agent`/`Task` (a subagent can write), every MCP tool and
//! every tool this list does not know all break it. A `Bash`
//! write into a tree the session never entered leaves no path here. That is
//! why any `Bash` call denies the empty-set pass, rather than the scan parsing
//! the command (the plan's Risks).
//!
//! ## Bounds and cache
//!
//! One scan reads at most [`SCAN_MAX_BYTES`] of files it has not cached, and
//! stops at [`SCAN_DEADLINE`]. A scan cut short by either is incomplete, which
//! the custody gate reads as UNKNOWN, never as a pass. A file read to its end
//! is cached on its `(len, mtime)` [`FileStamp`], so a finished session, whose
//! transcript no longer changes, is read once rather than on every executor
//! tick. A cached file costs nothing against the byte bound.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::Deserialize;

use super::transcript_facts::{find_copies, newest_copy};
use super::transcript_last_turn::FileStamp;
use qontinui_runner_lib::wind_down::TranscriptRead;

/// The tools that cannot change a file in a worktree. A `tool_use` of any
/// other name breaks the no-write proof. That includes the write tools D2
/// names, `Skill` and `SlashCommand` (a skill can run in a subagent),
/// `EnterWorktree`, every `mcp__*` tool, and any tool added after this list.
pub const READ_ONLY_TOOLS: [&str; 14] = [
    "Read",
    "Glob",
    "Grep",
    "LS",
    "NotebookRead",
    "WebFetch",
    "WebSearch",
    "ToolSearch",
    // Writes Claude Code's own todo list under its config dir, never a
    // worktree.
    "TodoWrite",
    "AskUserQuestion",
    "ExitPlanMode",
    // Reads a background shell's output. Starting that shell was a `Bash`
    // call, which already breaks the proof.
    "BashOutput",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
];

/// Is `tool` on [`READ_ONLY_TOOLS`]? The one predicate the proof reads.
pub fn is_read_only_tool(tool: &str) -> bool {
    READ_ONLY_TOOLS.contains(&tool)
}

/// The write tools whose input names the file they write, and the input key
/// that carries it.
const WRITE_TARGETS: [(&str, &str); 4] = [
    ("Edit", "file_path"),
    ("Write", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

/// Bytes one scan may read from files it has not cached. Transcripts of
/// 143 MB exist on a fleet box.
pub const SCAN_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// How long one scan may run.
pub const SCAN_DEADLINE: Duration = Duration::from_secs(20);

/// How deep under `<id>/subagents/` sidechains are looked for.
pub const SIDECHAIN_MAX_DEPTH: usize = 4;

/// What one scan of a session's transcript established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptScan {
    /// Every distinct directory the transcript names: record `cwd`s and the
    /// parent directory of each write-tool target. They are unresolved and
    /// may no longer exist.
    pub dirs: Vec<PathBuf>,
    /// Whether the whole transcript, main and sidechains, was read, and the
    /// first `tool_use` outside [`READ_ONLY_TOOLS`] it holds.
    pub read: TranscriptRead,
}

impl TranscriptScan {
    /// A scan that read nothing, for the reason given.
    pub fn unread(why: impl Into<String>) -> Self {
        Self {
            dirs: Vec::new(),
            read: TranscriptRead::unread(why),
        }
    }
}

/// What one transcript file holds.
#[derive(Debug, Clone, Default)]
struct FileScan {
    dirs: BTreeSet<PathBuf>,
    /// The first `tool_use` outside [`READ_ONLY_TOOLS`].
    write_call: Option<String>,
    /// Why the file does not count as read whole.
    incomplete: Option<String>,
    /// Bytes consumed.
    bytes: u64,
    /// The read reached the end of the file without an I/O error or the
    /// deadline, so the result holds for as long as the file's stamp does.
    reached_end: bool,
}

/// One transcript line, reduced to the fields the scan reads. serde skips
/// every other field without buffering it, a tool result's body included.
#[derive(Deserialize)]
struct Record {
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<Blocks>,
}

/// A message's `content`: its blocks when it is an array, none when it is a
/// string or null. Any other shape fails the line, which leaves the file
/// incomplete.
struct Blocks(Vec<Block>);

impl<'de> Deserialize<'de> for Blocks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BlocksVisitor;
        impl<'de> Visitor<'de> for BlocksVisitor {
            type Value = Blocks;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a message content string or block array")
            }
            fn visit_str<E: serde::de::Error>(self, _text: &str) -> Result<Blocks, E> {
                Ok(Blocks(Vec::new()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Blocks, E> {
                Ok(Blocks(Vec::new()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Blocks, A::Error> {
                let mut blocks = Vec::new();
                while let Some(block) = seq.next_element::<Block>()? {
                    blocks.push(block);
                }
                Ok(Blocks(blocks))
            }
        }
        deserializer.deserialize_any(BlocksVisitor)
    }
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
    input: Option<ToolInput>,
}

#[derive(Deserialize)]
struct ToolInput {
    file_path: Option<String>,
    notebook_path: Option<String>,
}

impl ToolInput {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "file_path" => self.file_path.as_deref(),
            "notebook_path" => self.notebook_path.as_deref(),
            _ => None,
        }
    }
}

impl FileScan {
    fn note_incomplete(&mut self, why: String) {
        self.incomplete.get_or_insert(why);
    }

    /// Fold one parsed record in.
    fn fold(&mut self, record: Record, label: &str, line: usize) {
        let cwd = record
            .cwd
            .map(|c| PathBuf::from(c.trim()))
            .filter(|c| !c.as_os_str().is_empty());
        match &cwd {
            Some(c) if c.is_absolute() => {
                self.dirs.insert(c.clone());
            }
            Some(c) => self.note_incomplete(format!(
                "{label} line {line} names a relative cwd {}",
                c.display()
            )),
            None => {}
        }
        let blocks = record
            .message
            .and_then(|m| m.content)
            .map_or_else(Vec::new, |b| b.0);
        for block in blocks {
            if block.kind.as_deref() != Some("tool_use") {
                continue;
            }
            let name = block.name.unwrap_or_default();
            if self.write_call.is_none() && !is_read_only_tool(&name) {
                self.write_call = Some(format!("{label} line {line} calls {name:?}"));
            }
            let Some(target) = WRITE_TARGETS
                .iter()
                .find(|(tool, _)| *tool == name)
                .and_then(|(_, key)| block.input.as_ref()?.get(key))
                .map(|t| PathBuf::from(t.trim()))
                .filter(|t| !t.as_os_str().is_empty())
            else {
                continue;
            };
            let absolute = if target.is_absolute() {
                Some(target.clone())
            } else {
                cwd.as_ref()
                    .filter(|c| c.is_absolute())
                    .map(|c| c.join(&target))
            };
            match absolute.as_deref().and_then(Path::parent) {
                Some(dir) => {
                    self.dirs.insert(dir.to_path_buf());
                }
                None => self.note_incomplete(format!(
                    "{label} line {line}: the {name} target {} cannot be located",
                    target.display()
                )),
            }
        }
    }
}

/// Scan one transcript stream to its end, or to `deadline`. Pure apart from
/// the clock. A line that is not a record is noted and the scan goes on, so
/// the directories on later lines are still collected.
fn scan_records<R: BufRead>(mut reader: R, deadline: Instant, label: &str) -> FileScan {
    let mut scan = FileScan::default();
    let mut line = Vec::new();
    let mut number = 0usize;
    loop {
        if Instant::now() >= deadline {
            scan.note_incomplete(format!(
                "{label}: the scan ran out of time after line {number}"
            ));
            return scan;
        }
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(n) => scan.bytes += n as u64,
            Err(e) => {
                scan.note_incomplete(format!("{label} could not be read: {e}"));
                return scan;
            }
        }
        number += 1;
        let text = line.trim_ascii();
        if text.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Record>(text) {
            Ok(record) => scan.fold(record, label, number),
            Err(e) => scan.note_incomplete(format!("{label} line {number} is not a record: {e}")),
        }
    }
    scan.reached_end = true;
    scan
}

type Cache = HashMap<PathBuf, (FileStamp, FileScan)>;

fn cache() -> &'static Mutex<Cache> {
    static CELL: OnceLock<Mutex<Cache>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Hard ceiling on cached files. Past it the cache is cleared.
const CACHE_CAP: usize = 4096;

/// Scan one file, from the cache when its stamp is unchanged. `remaining` is
/// the byte budget left, and it is debited by what was read.
fn scan_file(path: &Path, remaining: &mut u64, deadline: Instant) -> FileScan {
    let label = path.display().to_string();
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            let mut scan = FileScan::default();
            scan.note_incomplete(format!("{label} could not be read: {e}"));
            return scan;
        }
    };
    let stamp = FileStamp::of(&meta);
    if let Ok(guard) = cache().lock() {
        if let Some((cached, scan)) = guard.get(path) {
            if *cached == stamp {
                return scan.clone();
            }
        }
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => {
            let mut scan = FileScan::default();
            scan.note_incomplete(format!("{label} could not be opened: {e}"));
            return scan;
        }
    };
    let left = *remaining;
    let mut scan = scan_records(BufReader::new(file.take(left)), deadline, &label);
    *remaining = left.saturating_sub(scan.bytes);
    if stamp.len > left {
        // The cut is the reason, whatever its torn last line also reported.
        scan.reached_end = false;
        scan.incomplete = Some(format!(
            "{label} is {} bytes, past the {left} bytes left of the scan's byte bound",
            stamp.len
        ));
        return scan;
    }
    // Stat again after the read: a file written meanwhile was not read as
    // stamped, so the result is used but not cached.
    let unchanged = std::fs::metadata(path).is_ok_and(|m| FileStamp::of(&m) == stamp);
    if scan.reached_end && unchanged {
        if let Ok(mut guard) = cache().lock() {
            if guard.len() >= CACHE_CAP {
                guard.clear();
            }
            guard.insert(path.to_path_buf(), (stamp, scan.clone()));
        }
    }
    scan
}

/// Every `*.jsonl` under `dir`, down to `depth` levels. A `dir` that does not
/// exist holds none. `Err` when a directory that exists could not be listed.
fn sidechain_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} could not be listed: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("{} listing broke off: {e}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            if depth == 0 {
                return Err(format!(
                    "{} is nested deeper than {SIDECHAIN_MAX_DEPTH} levels under subagents/",
                    path.display()
                ));
            }
            sidechain_files(&path, depth - 1, out)?;
        } else if path.extension().is_some_and(|x| x == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scan `session_id`'s transcript under `roots`: the newest copy's main file
/// and the sidechains beside every copy, within `max_bytes` and `deadline`.
pub fn scan_session_in(
    roots: &[PathBuf],
    session_id: &str,
    max_bytes: u64,
    deadline: Duration,
) -> TranscriptScan {
    let deadline = Instant::now() + deadline;
    let copies = match find_copies(roots, session_id) {
        Ok(copies) => copies,
        Err(e) => return TranscriptScan::unread(e),
    };
    if copies.is_empty() {
        return TranscriptScan::unread(format!(
            "no transcript for session {session_id} under any of {} account root(s)",
            roots.len()
        ));
    }
    let mut failures: Vec<String> = Vec::new();
    let mut files: Vec<PathBuf> = match newest_copy(copies.clone()) {
        Ok(newest) => newest.into_iter().map(|c| c.path).collect(),
        Err(e) => {
            failures.push(e);
            copies.iter().map(|c| c.path.clone()).collect()
        }
    };
    for copy in &copies {
        let subagents = copy.path.with_extension("").join("subagents");
        if let Err(e) = sidechain_files(&subagents, SIDECHAIN_MAX_DEPTH, &mut files) {
            failures.push(e);
        }
    }

    let mut remaining = max_bytes;
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut write_call: Option<String> = None;
    for file in &files {
        let scan = scan_file(file, &mut remaining, deadline);
        dirs.extend(scan.dirs);
        if write_call.is_none() {
            write_call = scan.write_call;
        }
        failures.extend(scan.incomplete);
    }
    TranscriptScan {
        dirs: dirs.into_iter().collect(),
        read: TranscriptRead {
            incomplete: failures.into_iter().next(),
            write_call,
        },
    }
}

/// [`scan_session_in`] over every Claude config root this machine has, under
/// the shipped bounds.
pub fn scan_session(session_id: &str) -> TranscriptScan {
    scan_session_in(
        &crate::terminal::transcript::find_claude_config_dirs(),
        session_id,
        SCAN_MAX_BYTES,
        SCAN_DEADLINE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    /// An assistant record at `cwd` calling `tool` with `input`.
    fn call(cwd: &str, tool: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant", "cwd": cwd, "timestamp": "2026-11-20T10:00:00.000Z",
            "message": {"role": "assistant", "stop_reason": "tool_use",
                        "content": [{"type": "tool_use", "id": "u1", "name": tool, "input": input}]},
        })
        .to_string()
    }

    /// A user record at `cwd`: a prompt, or a tool result.
    fn said(cwd: &str, content: serde_json::Value) -> String {
        serde_json::json!({
            "type": "user", "cwd": cwd, "timestamp": "2026-11-20T10:00:01.000Z",
            "message": {"role": "user", "content": content},
        })
        .to_string()
    }

    fn scan(lines: &[String]) -> FileScan {
        let text = lines.join("\n");
        scan_records(
            text.as_bytes(),
            Instant::now() + Duration::from_secs(60),
            "t.jsonl",
        )
    }

    fn dirs(scan: &FileScan) -> Vec<&str> {
        scan.dirs.iter().map(|d| d.to_str().unwrap()).collect()
    }

    /// The `<root>/projects/<p>/<sid>.jsonl` path of one copy.
    fn main_path(root: &Path, project: &str, sid: &str) -> PathBuf {
        root.join("projects")
            .join(project)
            .join(format!("{sid}.jsonl"))
    }

    #[test]
    fn the_allowlist_never_admits_a_tool_that_can_write() {
        for tool in [
            "Bash",
            "PowerShell",
            "Edit",
            "Write",
            "MultiEdit",
            "NotebookEdit",
            "Agent",
            "Task",
            "Skill",
            "SlashCommand",
            "EnterWorktree",
            "mcp__coord__coord_register_gate",
        ] {
            assert!(!is_read_only_tool(tool), "{tool}");
        }
    }

    #[test]
    fn cwds_and_write_targets_are_collected_and_read_only_tools_prove_no_write() {
        let read_only = scan(&[
            said("/ws", serde_json::json!("look around")),
            call(
                "/ws",
                "Read",
                serde_json::json!({"file_path": "/ws/wt/a/src/x.rs"}),
            ),
            said(
                "/ws/wt/b",
                serde_json::json!([{"type": "tool_result", "tool_use_id": "u1",
                                    "content": [{"type": "text", "text": "big"}]}]),
            ),
            "".to_string(),
        ]);
        // A Read target names no tree: only the records' cwds do.
        assert_eq!(dirs(&read_only), vec!["/ws", "/ws/wt/b"]);
        assert_eq!(read_only.write_call, None);
        assert_eq!(read_only.incomplete, None);
        assert!(read_only.reached_end);

        let wrote = scan(&[
            call(
                "/ws",
                "Edit",
                serde_json::json!({"file_path": "/ws/wt/a/src/x.rs", "old_string": "a"}),
            ),
            call(
                "/ws",
                "NotebookEdit",
                serde_json::json!({"notebook_path": "/ws/wt/n/nb.ipynb"}),
            ),
            // A relative target is taken against the record's own cwd.
            call(
                "/ws/wt/c",
                "Write",
                serde_json::json!({"file_path": "docs/y.md", "content": "z"}),
            ),
        ]);
        assert_eq!(
            dirs(&wrote),
            vec![
                "/ws",
                "/ws/wt/a/src",
                "/ws/wt/c",
                "/ws/wt/c/docs",
                "/ws/wt/n"
            ]
        );
        assert_eq!(
            wrote.write_call.as_deref(),
            Some("t.jsonl line 1 calls \"Edit\"")
        );
    }

    #[test]
    fn any_tool_off_the_allowlist_breaks_the_proof() {
        for tool in ["Bash", "Agent", "Task", "mcp__x__y", "SomeFutureTool", ""] {
            let s = scan(&[call("/ws", tool, serde_json::json!({}))]);
            assert!(s.write_call.is_some(), "{tool:?}");
        }
    }

    #[test]
    fn a_line_that_is_not_a_record_is_incomplete_and_later_lines_still_count() {
        let s = scan(&[
            "{cut off".to_string(),
            said("/ws/wt/a", serde_json::json!("go")),
        ]);
        assert!(s.incomplete.as_deref().unwrap().contains("line 1"));
        assert_eq!(dirs(&s), vec!["/ws/wt/a"]);
        // A content shape the scan does not know is not skipped in silence.
        let odd = scan(&[serde_json::json!({"cwd": "/ws", "message": {"content": 7}}).to_string()]);
        assert!(odd.incomplete.is_some());
    }

    #[test]
    fn a_passed_deadline_is_incomplete() {
        let line = said("/ws", serde_json::json!("go"));
        let s = scan_records(line.as_bytes(), Instant::now(), "t.jsonl");
        assert!(s.incomplete.as_deref().unwrap().contains("ran out of time"));
        assert!(!s.reached_end);
    }

    #[test]
    fn the_newest_copy_and_every_sidechain_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "8c0e5a52-0000-4000-8000-000000000001";
        let root = tmp.path().join(".claude-a");
        let main = main_path(&root, "p1", sid);
        write(&main, &[said("/ws", serde_json::json!("go"))]);
        let roots = vec![root.clone()];
        let clean = scan_session_in(&roots, sid, SCAN_MAX_BYTES, SCAN_DEADLINE);
        assert_eq!(clean.dirs, vec![PathBuf::from("/ws")]);
        assert_eq!(clean.read.no_write_proof(), Ok(()));

        // A subagent's edit, and a workflow agent one level further down.
        let subagents = main.with_extension("").join("subagents");
        write(
            &subagents.join("agent-a1.jsonl"),
            &[call(
                "/ws",
                "Edit",
                serde_json::json!({"file_path": "/ws/wt/a/x.rs"}),
            )],
        );
        write(
            &subagents
                .join("workflows")
                .join("wf_1")
                .join("agent-a2.jsonl"),
            &[said("/ws/wt/b", serde_json::json!("go"))],
        );
        let both = scan_session_in(&roots, sid, SCAN_MAX_BYTES, SCAN_DEADLINE);
        assert_eq!(
            both.dirs,
            vec![
                PathBuf::from("/ws"),
                PathBuf::from("/ws/wt/a"),
                PathBuf::from("/ws/wt/b")
            ]
        );
        assert_eq!(both.read.incomplete, None);
        assert!(both.read.write_call.unwrap().contains("\"Edit\""));
    }

    #[test]
    fn no_copy_a_bound_or_an_unrankable_copy_never_proves_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "8c0e5a52-0000-4000-8000-000000000002";
        let a = tmp.path().join(".claude-a");
        let b = tmp.path().join(".claude-b");
        let none = scan_session_in(&[a.clone()], sid, SCAN_MAX_BYTES, SCAN_DEADLINE);
        assert!(none.read.incomplete.unwrap().contains("no transcript"));

        write(
            &main_path(&a, "p1", sid),
            &[said("/ws/wt/a", serde_json::json!("go"))],
        );
        let cut = scan_session_in(&[a.clone()], sid, 4, SCAN_DEADLINE);
        assert!(cut.read.incomplete.unwrap().contains("byte bound"));

        // A second copy that holds no turn cannot be ranked: every copy is
        // read, and the proof does not hold.
        write(
            &main_path(&b, "p2", sid),
            &[serde_json::json!({"type": "permission-mode", "cwd": "/ws/wt/b"}).to_string()],
        );
        let unranked = scan_session_in(&[a, b], sid, SCAN_MAX_BYTES, SCAN_DEADLINE);
        assert!(unranked.read.incomplete.is_some());
        assert_eq!(
            unranked.dirs,
            vec![PathBuf::from("/ws/wt/a"), PathBuf::from("/ws/wt/b")]
        );
    }

    /// The cache must never serve a stale "no write": a file the session
    /// wrote to since is read again.
    #[test]
    fn a_changed_file_is_read_again_and_not_served_from_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "8c0e5a52-0000-4000-8000-000000000003";
        let root = tmp.path().join(".claude-a");
        let main = main_path(&root, "p1", sid);
        let roots = vec![root];
        write(&main, &[said("/ws", serde_json::json!("go"))]);
        assert_eq!(
            scan_session_in(&roots, sid, SCAN_MAX_BYTES, SCAN_DEADLINE)
                .read
                .no_write_proof(),
            Ok(())
        );
        write(
            &main,
            &[
                said("/ws", serde_json::json!("go")),
                call("/ws", "Bash", serde_json::json!({"command": "true"})),
            ],
        );
        assert!(scan_session_in(&roots, sid, SCAN_MAX_BYTES, SCAN_DEADLINE)
            .read
            .write_call
            .unwrap()
            .contains("\"Bash\""));
    }
}
