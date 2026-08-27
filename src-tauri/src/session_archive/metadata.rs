//! Head/tail metadata extraction from a Claude Code JSONL transcript, and the
//! join back onto the runner's own session registry.
//!
//! ## Head/tail, not a full parse
//!
//! The corpus is 3.5 GB across 8,308 files (measured 2026-08-26; p50 310 KB,
//! p99 4 MB, max 7 MB). Every byte has to be READ regardless — the archive body
//! is the file, verbatim, and its digest covers all of it — but every byte does
//! **not** have to be `serde_json`-parsed. The head and tail windows carry
//! everything the head row needs: the first record establishes `cwd`,
//! `sessionId`, `gitBranch` and `version`; the `summary` record Claude Code
//! writes carries the title; the first and last human turns are the two prompts;
//! and the line count is the turn count, which `memchr` answers without parsing
//! anything.
//!
//! Records the windows cannot see are not guessed at. A transcript whose only
//! human turn sits in the middle yields `first_prompt` and no `last_prompt`
//! rather than a fabricated one — an absent field is left absent, and the web
//! upsert treats an omitted field as "leave it alone" rather than as NULL.
//!
//! ## What is NOT derived here
//!
//! `launch_command` is deliberately absent. The runner's registry does not
//! store one and the transcript never held one, so any value would be
//! reconstructed from the account wrapper and presented as recorded fact. The
//! web schema's omitted-means-untouched rule makes leaving it out the honest
//! option; the relaunch route already has `account_label` + `config_dir` +
//! `working_dir`, which is what a hand relaunch actually needs.
//!
//! `state = "abandoned"` is likewise never emitted. The scanner can distinguish
//! a session the registry calls open from one it calls closed, and can call a
//! long-untouched transcript with no registry row closed — but nothing on disk
//! distinguishes *abandoned* from *closed*, and inventing the difference would
//! put a guess in a lifecycle column.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How many lines at each end of a transcript are parsed for metadata.
///
/// Generous enough that the leading `summary` record, the session-start
/// preamble and the first real human turn are all inside the head window even
/// when a session opens with a long injected briefing, and that the closing
/// human turn is inside the tail window even after a burst of tool results.
const WINDOW_LINES: usize = 64;

/// Longest prompt excerpt recorded on the head row.
///
/// `first_prompt` / `last_prompt` exist to make a session recognisable in a
/// list and searchable by `?q=`; they are not the transcript, which is one
/// `GET /{id}/export` away and byte-verbatim.
const PROMPT_EXCERPT_CHARS: usize = 2000;

/// Metadata read out of one transcript file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptMetadata {
    /// The session id the transcript itself claims (`sessionId` on any
    /// record). Normally equal to the filename stem; when they disagree the
    /// FILENAME wins as the identity, because that is what `claude --resume`
    /// and every other reader in this tree keys on — the disagreement is
    /// reported instead.
    pub session_id_in_body: Option<String>,
    pub working_dir: Option<String>,
    pub git_branch: Option<String>,
    /// The Claude Code version that wrote the transcript.
    pub cli_version: Option<String>,
    /// Claude Code's own generated summary of the session, when it wrote one.
    pub ai_title: Option<String>,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    /// Non-empty lines — one JSONL record each.
    pub turn_count: usize,
    /// Lines the head/tail windows could not parse as JSON. Reported rather
    /// than hidden: a transcript with a torn tail is exactly the one whose
    /// metadata is least trustworthy.
    pub unparsable_window_lines: usize,
}

/// Split `raw` into logical lines without allocating a `String` for the whole
/// file, skipping blank ones.
fn line_ranges(raw: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in raw.iter().enumerate() {
        if *b == b'\n' {
            let mut end = i;
            if end > start && raw[end - 1] == b'\r' {
                end -= 1;
            }
            if end > start {
                out.push((start, end));
            }
            start = i + 1;
        }
    }
    if start < raw.len() {
        let mut end = raw.len();
        if end > start && raw[end - 1] == b'\r' {
            end -= 1;
        }
        if end > start {
            out.push((start, end));
        }
    }
    out
}

/// Flatten a record's `message.content` to text.
///
/// A string content is a plain human turn. An array content is a block list;
/// text blocks contribute their text and non-text blocks are summarised in
/// place (`[tool_use: Bash]`) rather than dropped, so the shape of the turn
/// survives into the excerpt.
fn flatten_content(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => parts.push(format!(
                        "[tool_use: {}]",
                        b.get("name").and_then(Value::as_str).unwrap_or("?")
                    )),
                    Some("tool_result") => parts.push("[tool_result]".to_string()),
                    Some(other) => parts.push(format!("[{other}]")),
                    None => {}
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

/// True for a record that represents something a HUMAN typed.
///
/// `type: "user"` alone is not enough: tool results come back as user records
/// too, and Claude Code marks injected context with `isMeta`. Both would make
/// `first_prompt` read as though the operator had typed a tool result.
fn is_human_turn(record: &Value) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if record.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return false;
    };
    match content {
        Value::String(_) => true,
        // An array whose blocks are ALL tool results is the transport's echo,
        // not a turn.
        Value::Array(blocks) => blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) != Some("tool_result")),
        _ => false,
    }
}

/// Truncate to [`PROMPT_EXCERPT_CHARS`] on a char boundary, appending an
/// explicit ellipsis so a reader can see the excerpt is one.
fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= PROMPT_EXCERPT_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(PROMPT_EXCERPT_CHARS).collect();
    format!("{cut}…")
}

/// Parse the head and tail windows of one transcript's bytes.
pub fn parse_transcript(raw: &[u8]) -> TranscriptMetadata {
    let lines = line_ranges(raw);
    let mut meta = TranscriptMetadata {
        turn_count: lines.len(),
        ..Default::default()
    };
    if lines.is_empty() {
        return meta;
    }

    let head_end = WINDOW_LINES.min(lines.len());
    let tail_start = lines.len().saturating_sub(WINDOW_LINES).max(head_end);

    let mut parse_window = |range: std::ops::Range<usize>, is_head: bool| {
        for idx in range {
            let (s, e) = lines[idx];
            let Ok(record) = serde_json::from_slice::<Value>(&raw[s..e]) else {
                meta.unparsable_window_lines += 1;
                continue;
            };

            // Identity + provenance: first sighting wins, so a resumed
            // session's later `cwd` change cannot rewrite where it started.
            if meta.session_id_in_body.is_none() {
                meta.session_id_in_body = record
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            if meta.working_dir.is_none() {
                meta.working_dir = record.get("cwd").and_then(Value::as_str).map(str::to_string);
            }
            if meta.git_branch.is_none() {
                meta.git_branch = record
                    .get("gitBranch")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
            if meta.cli_version.is_none() {
                meta.cli_version = record
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            if meta.ai_title.is_none() {
                if let Some(s) = record.get("summary").and_then(Value::as_str) {
                    meta.ai_title = Some(excerpt(s));
                }
            }

            if let Some(ts) = record.get("timestamp").and_then(Value::as_str) {
                if meta.started_at.is_none() {
                    meta.started_at = Some(ts.to_string());
                }
                // The tail window overwrites this repeatedly; the last write
                // is the last timestamped record in the file.
                meta.last_activity_at = Some(ts.to_string());
            }

            if is_human_turn(&record) {
                if let Some(text) = record
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(flatten_content)
                {
                    let text = excerpt(&text);
                    if !text.is_empty() {
                        if is_head && meta.first_prompt.is_none() {
                            meta.first_prompt = Some(text.clone());
                        }
                        meta.last_prompt = Some(text);
                    }
                }
            }
        }
    };

    parse_window(0..head_end, true);
    parse_window(tail_start..lines.len(), false);
    meta
}

/// The repo a session was working in, derived from its `cwd`.
///
/// The last path segment, which is the repo checkout's directory name for both
/// a primary checkout (`D:/qontinui-root/qontinui-web`) and a session worktree
/// (`…/agent-worktrees/<uuid>/qontinui-runner`). That is exactly the spelling
/// coord's own D2 predicate normalizes to
/// (`split_part(tr.repo, '/', 2)`), so the value feeds
/// [`super::tenancy::RepoTenantMap::candidates`] without a second convention.
///
/// A `cwd` one level INSIDE a checkout yields the subdirectory instead. That
/// is a known imprecision, and the honest one: the transcript records where
/// the session was, not where its repo root is, and walking the filesystem
/// looking for a `.git` would answer for the machine as it is today rather
/// than as it was when the session ran.
pub fn repo_from_working_dir(working_dir: Option<&str>) -> Option<String> {
    let wd = working_dir?.trim().trim_end_matches(['/', '\\']);
    if wd.is_empty() {
        return None;
    }
    let tail = wd.rsplit(['/', '\\']).next()?;
    if tail.is_empty() || tail.ends_with(':') {
        return None;
    }
    Some(tail.to_string())
}

// ===========================================================================
// The runner's own session registry
// ===========================================================================

/// The fields this backfill joins off `terminal-sessions.json`.
///
/// Read as loose JSON rather than through
/// `session::session_lifecycle_store::TerminalSessionRecord`: that type lives
/// in the BINARY crate and `qontinui-pr` cannot reach it. Reading the documented
/// camelCase keys of a file whose writer is one module away is a projection,
/// not a re-implementation — there is no logic here to drift, only field names,
/// and a renamed field shows up as an absent value rather than a wrong one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryRecord {
    pub account_label: Option<String>,
    pub account_wrapper: Option<String>,
    pub tenant_id: Option<String>,
    pub task_run_id: Option<String>,
    pub session_name: Option<String>,
    pub name_source: Option<String>,
    pub title: Option<String>,
    pub config_dir: Option<String>,
    pub working_dir: Option<String>,
    pub provider: Option<String>,
    pub restore_tier: Option<String>,
    /// `"open"` | `"closed"` as the runner recorded it.
    pub state: Option<String>,
    pub bypass_permissions: Option<bool>,
    pub opened_at_ms: Option<i64>,
    pub closed_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Load one `terminal-sessions.json` into a map keyed by `claudeSessionId`.
///
/// Two-stage parse, for the same reason the store's own loader uses one: a
/// single malformed record must not discard the whole registry. A missing file
/// is an empty map, not an error — a machine that never ran the runner still
/// has transcripts worth archiving.
pub fn load_registry_file(path: &Path) -> HashMap<String, RegistryRecord> {
    let mut out = HashMap::new();
    let Ok(bytes) = std::fs::read(path) else {
        return out;
    };
    let Ok(raw) = serde_json::from_slice::<HashMap<String, Value>>(&bytes) else {
        return out;
    };
    for (key, v) in raw {
        let id = s(&v, "claudeSessionId").unwrap_or(key);
        out.insert(
            id,
            RegistryRecord {
                account_label: s(&v, "accountLabel"),
                account_wrapper: s(&v, "accountWrapper"),
                tenant_id: s(&v, "tenantId"),
                task_run_id: s(&v, "taskRunId"),
                session_name: s(&v, "sessionName"),
                name_source: s(&v, "nameSource"),
                title: s(&v, "title"),
                config_dir: s(&v, "configDir"),
                working_dir: s(&v, "workingDir"),
                provider: s(&v, "provider"),
                restore_tier: s(&v, "restoreTier"),
                state: s(&v, "state"),
                bypass_permissions: v.get("bypassPermissions").and_then(Value::as_bool),
                opened_at_ms: v.get("openedAt").and_then(Value::as_i64),
                closed_at_ms: v.get("closedAt").and_then(Value::as_i64),
                last_seen_at_ms: v.get("lastSeenAt").and_then(Value::as_i64),
            },
        );
    }
    out
}

/// Every `terminal-sessions.json` on this machine: the primary runner's
/// unscoped file plus every named/temp secondary's instance-scoped one.
///
/// The instance directories are DISCOVERED (`instance-*` under
/// `~/.qontinui/runner/`) rather than enumerated, because their names are
/// per-spawn — a hard-coded list would archive the primary's sessions and
/// silently miss every secondary's.
pub fn registry_paths(runner_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![runner_dir.join("terminal-sessions.json")];
    if let Ok(entries) = std::fs::read_dir(runner_dir) {
        let mut scoped: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("instance-"))
            })
            .map(|p| p.join("terminal-sessions.json"))
            .collect();
        scoped.sort();
        out.extend(scoped);
    }
    out
}

/// Merge every discovered registry into one lookup.
///
/// First writer wins, and the primary is first — a secondary that reattached a
/// session the primary also knows must not overwrite the primary's account and
/// tenant binding.
pub fn load_all_registries(runner_dir: &Path) -> HashMap<String, RegistryRecord> {
    let mut merged: HashMap<String, RegistryRecord> = HashMap::new();
    for path in registry_paths(runner_dir) {
        for (k, v) in load_registry_file(&path) {
            merged.entry(k).or_insert(v);
        }
    }
    merged
}

/// `~/.qontinui/runner` — where the primary runner's registry lives and where
/// the `instance-<name>` subdirectories sit.
pub fn default_runner_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".qontinui")
        .join("runner")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsonl(lines: &[&str]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    #[test]
    fn head_and_tail_windows_carry_the_head_row_fields() {
        let raw = jsonl(&[
            r#"{"type":"summary","summary":"Fix the flaky merge train test"}"#,
            r#"{"type":"user","cwd":"D:/qontinui-root/qontinui-runner","sessionId":"abc","gitBranch":"main","version":"2.0.1","timestamp":"2026-08-26T10:00:00Z","message":{"role":"user","content":"start here"}}"#,
            r#"{"type":"assistant","timestamp":"2026-08-26T10:00:05Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
            r#"{"type":"user","timestamp":"2026-08-26T10:05:00Z","message":{"role":"user","content":"and now finish"}}"#,
        ]);
        let m = parse_transcript(&raw);
        assert_eq!(m.turn_count, 4);
        assert_eq!(m.session_id_in_body.as_deref(), Some("abc"));
        assert_eq!(m.working_dir.as_deref(), Some("D:/qontinui-root/qontinui-runner"));
        assert_eq!(m.git_branch.as_deref(), Some("main"));
        assert_eq!(m.cli_version.as_deref(), Some("2.0.1"));
        assert_eq!(m.ai_title.as_deref(), Some("Fix the flaky merge train test"));
        assert_eq!(m.first_prompt.as_deref(), Some("start here"));
        assert_eq!(m.last_prompt.as_deref(), Some("and now finish"));
        assert_eq!(m.started_at.as_deref(), Some("2026-08-26T10:00:00Z"));
        assert_eq!(m.last_activity_at.as_deref(), Some("2026-08-26T10:05:00Z"));
        assert_eq!(m.unparsable_window_lines, 0);
    }

    #[test]
    fn tool_results_and_meta_records_are_not_human_turns() {
        // Both come back as `type: "user"`. Treating either as a prompt makes
        // `first_prompt` read like the operator typed a tool result.
        let raw = jsonl(&[
            r#"{"type":"user","isMeta":true,"timestamp":"2026-08-26T10:00:00Z","message":{"role":"user","content":"<session briefing>"}}"#,
            r#"{"type":"user","timestamp":"2026-08-26T10:00:01Z","message":{"role":"user","content":[{"type":"tool_result","content":"exit 0"}]}}"#,
            r#"{"type":"user","timestamp":"2026-08-26T10:00:02Z","message":{"role":"user","content":"the real question"}}"#,
        ]);
        let m = parse_transcript(&raw);
        assert_eq!(m.first_prompt.as_deref(), Some("the real question"));
    }

    #[test]
    fn a_torn_line_is_counted_rather_than_hidden() {
        let raw = jsonl(&[
            r#"{"type":"user","sessionId":"abc","timestamp":"2026-08-26T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","message":{"role":"assist"#,
        ]);
        let m = parse_transcript(&raw);
        assert_eq!(m.turn_count, 2);
        assert_eq!(m.unparsable_window_lines, 1);
        assert_eq!(m.session_id_in_body.as_deref(), Some("abc"));
    }

    #[test]
    fn an_empty_transcript_yields_no_metadata_and_no_panic() {
        let m = parse_transcript(b"");
        assert_eq!(m, TranscriptMetadata::default());
        let m = parse_transcript(b"\n\n\r\n");
        assert_eq!(m.turn_count, 0);
    }

    #[test]
    fn crlf_lines_parse() {
        let raw = b"{\"type\":\"user\",\"sessionId\":\"abc\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\r\n".to_vec();
        let m = parse_transcript(&raw);
        assert_eq!(m.turn_count, 1);
        assert_eq!(m.first_prompt.as_deref(), Some("hi"));
    }

    #[test]
    fn a_middle_only_human_turn_is_not_fabricated_into_the_windows() {
        // 200 filler records with the single human turn at position 100 —
        // outside both windows. The metadata must simply not carry a prompt.
        let mut lines: Vec<String> = Vec::new();
        for i in 0..200 {
            if i == 100 {
                lines.push(
                    r#"{"type":"user","timestamp":"2026-08-26T10:00:00Z","message":{"role":"user","content":"buried"}}"#
                        .to_string(),
                );
            } else {
                lines.push(
                    r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"x"}]}}"#
                        .to_string(),
                );
            }
        }
        let raw = lines.join("\n").into_bytes();
        let m = parse_transcript(&raw);
        assert_eq!(m.turn_count, 200);
        assert_eq!(m.first_prompt, None);
        assert_eq!(m.last_prompt, None);
    }

    #[test]
    fn repo_is_the_checkout_directory_name_for_both_layouts() {
        assert_eq!(
            repo_from_working_dir(Some("D:/qontinui-root/qontinui-web")).as_deref(),
            Some("qontinui-web")
        );
        assert_eq!(
            repo_from_working_dir(Some(
                "D:/qontinui-root/agent-worktrees/01a03e5f-e7d2/qontinui-runner/"
            ))
            .as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(
            repo_from_working_dir(Some("C:\\Users\\jspin\\code\\thing")).as_deref(),
            Some("thing")
        );
        assert_eq!(repo_from_working_dir(None), None);
        assert_eq!(repo_from_working_dir(Some("   ")), None);
        assert_eq!(repo_from_working_dir(Some("D:/")), None);
    }

    #[test]
    fn the_registry_projection_reads_the_camel_case_keys_the_store_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("terminal-sessions.json");
        std::fs::write(
            &path,
            r#"{
              "sess-1": {
                "claudeSessionId": "sess-1",
                "accountLabel": "gmail",
                "accountWrapper": "clg",
                "tenantId": "0f1a4c1e-0000-0000-0000-000000000001",
                "sessionName": "08-26 backfill",
                "nameSource": "operator",
                "configDir": "C:/claude/.claude-gmail",
                "workingDir": "D:/qontinui-root/qontinui-runner",
                "provider": "claude",
                "restoreTier": "full",
                "state": "closed",
                "bypassPermissions": true,
                "openedAt": 1756200000000,
                "closedAt": 1756203600000,
                "lastSeenAt": 1756203500000
              },
              "broken": { "claudeSessionId": 42 }
            }"#,
        )
        .unwrap();
        let map = load_registry_file(&path);
        let rec = map.get("sess-1").expect("sess-1 present");
        assert_eq!(rec.account_label.as_deref(), Some("gmail"));
        assert_eq!(rec.state.as_deref(), Some("closed"));
        assert_eq!(rec.bypass_permissions, Some(true));
        assert_eq!(rec.closed_at_ms, Some(1756203600000));
        // The malformed row keeps its map key rather than discarding the file.
        assert!(map.contains_key("broken"));
    }

    #[test]
    fn a_missing_registry_is_an_empty_map_not_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_registry_file(&tmp.path().join("nope.json")).is_empty());
    }

    #[test]
    fn secondary_instances_are_discovered_and_the_primary_wins_a_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = tmp.path();
        std::fs::write(
            runner.join("terminal-sessions.json"),
            r#"{"s":{"claudeSessionId":"s","accountLabel":"gmail"}}"#,
        )
        .unwrap();
        let inst = runner.join("instance-temp-9880");
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(
            inst.join("terminal-sessions.json"),
            r#"{"s":{"claudeSessionId":"s","accountLabel":"hotmail"},"t":{"claudeSessionId":"t"}}"#,
        )
        .unwrap();

        assert_eq!(registry_paths(runner).len(), 2);
        let merged = load_all_registries(runner);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged.get("s").unwrap().account_label.as_deref(),
            Some("gmail"),
            "a secondary must not overwrite the primary's binding"
        );
    }
}
