//! Rescuing the main transcript's TAIL after an account migration (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
//! Phase 2b).
//!
//! [`super::copy_transcript`] copies `<sid>.jsonl` BEFORE the respawn and the
//! close, so the old writer outlives the copy: on 2026-10-22 it appended five
//! lines two minutes after the carry — four `queue-operation` lines and the
//! `<task-notification>` reporting that a background agent had FAILED on the
//! weekly limit, the only record that it had died. No other account held them.
//!
//! The resumed process owns the destination `<sid>.jsonl` from the moment it
//! starts, and has written its own `parentUuid` chain there; splicing the old
//! chain in would create a second leaf a later `--resume` may pick. So the tail
//! is NEVER appended to the destination transcript. Instead, once the old
//! writer is proven gone (the same gate [`super::delta_sync`] runs behind):
//! - the bytes past [`super::TranscriptCopy::transcript_offset`] are written
//!   verbatim to `<dst sidecar>/migration-tail/<from-account>-<unix-ms>.jsonl`
//!   — durable, discoverable, and never read by the CLI;
//! - [`scan_tail`] extracts the lines the resumed session needs to hear about
//!   (task notifications and operator-typed prompts), and [`catch_up_message`]
//!   renders them as ONE bounded, single-line message for the idle watcher.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::TranscriptCopy;

/// Directory under the destination's `<sid>/` the tail is written into.
pub(crate) const TAIL_DIR: &str = "migration-tail";

/// How many items [`catch_up_message`] lists before summarising the rest.
pub(crate) const MAX_LISTED_ITEMS: usize = 12;

/// Character bound on one listed item.
const MAX_ITEM_CHARS: usize = 200;

/// Read bound on a tail. The measured tail was 5 lines in ~2 minutes; this
/// only stops a pathological read.
const MAX_TAIL_BYTES: u64 = 16 * 1024 * 1024;

/// What the source transcript holds past the carry's offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TailRead {
    /// The bytes past the offset (possibly none). `truncated` when the read
    /// stopped at [`MAX_TAIL_BYTES`].
    Bytes { bytes: Vec<u8>, truncated: bool },
    /// The source is SHORTER than the offset the carry recorded: it was
    /// truncated or replaced, so which bytes are "new" is UNKNOWN.
    SourceShorter { len: u64, offset: u64 },
}

/// Read `src` from `offset` to EOF (bounded).
pub(crate) fn read_tail(src: &Path, offset: u64) -> Result<TailRead, String> {
    let mut file =
        std::fs::File::open(src).map_err(|e| format!("open {} failed: {e}", src.display()))?;
    let len = file
        .metadata()
        .map_err(|e| format!("stat {} failed: {e}", src.display()))?
        .len();
    if len < offset {
        return Ok(TailRead::SourceShorter { len, offset });
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("seek {} failed: {e}", src.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {} failed: {e}", src.display()))?;
    let truncated = len - offset > MAX_TAIL_BYTES;
    Ok(TailRead::Bytes { bytes, truncated })
}

/// One tail line the resumed session should hear about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatchUpItem {
    /// A `type: user` line carrying a `<task-notification>` — a background
    /// task or subagent finished (or failed) after the move.
    TaskNotification {
        task_id: Option<String>,
        status: Option<String>,
        summary: Option<String>,
    },
    /// A prompt the operator typed into the OLD pane after the move.
    OperatorPrompt { text: String },
}

/// What [`scan_tail`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TailScan {
    /// Non-blank lines in the tail.
    pub lines: usize,
    /// Lines that are not one JSON object — counted, never dropped silently.
    pub unparseable: usize,
    /// The lines worth delivering, in transcript order, duplicates dropped.
    pub items: Vec<CatchUpItem>,
}

/// Pure: classify every line of a tail. A `queue-operation` line names the
/// same notification its `user` line delivers, so only `user` lines count.
pub(crate) fn scan_tail(bytes: &[u8]) -> TailScan {
    let mut scan = TailScan::default();
    for raw in bytes.split(|b| *b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        scan.lines += 1;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) else {
            scan.unparseable += 1;
            continue;
        };
        if !value.is_object() {
            scan.unparseable += 1;
            continue;
        }
        if let Some(item) = catch_up_item(&value) {
            if !scan.items.contains(&item) {
                scan.items.push(item);
            }
        }
    }
    scan
}

/// The catch-up item one parsed transcript line carries, if any.
fn catch_up_item(line: &serde_json::Value) -> Option<CatchUpItem> {
    if line.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    if line.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let text = user_text(line.get("message")?.get("content")?)?;
    if text.contains("<task-notification>") {
        return Some(CatchUpItem::TaskNotification {
            task_id: tag(&text, "task-id"),
            status: tag(&text, "status"),
            summary: tag(&text, "summary"),
        });
    }
    let typed = line.get("promptSource").and_then(|s| s.as_str()) == Some("typed")
        || line
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(|k| k.as_str())
            == Some("human");
    let text = text.trim();
    (typed && !text.is_empty()).then(|| CatchUpItem::OperatorPrompt {
        text: text.to_string(),
    })
}

/// The text of a user message: a plain string, or the `text` blocks of a
/// block list (a `tool_result` list has none, so it yields `None`).
fn user_text(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// The trimmed text between `<name>` and `</name>`, when both are present.
fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let len = text.get(start..)?.find(&close)?;
    let value = text.get(start..start + len)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Collapse `s` to one line: control characters become spaces and runs of
/// whitespace one space. The message is typed into a PTY, where a newline or
/// an escape would act.
pub(crate) fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `s` on one line, bounded to `max` characters (an ellipsis marks a cut).
fn bounded(s: &str, max: usize) -> String {
    let line = one_line(s);
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

fn describe(item: &CatchUpItem) -> String {
    match item {
        CatchUpItem::TaskNotification {
            task_id,
            status,
            summary,
        } => {
            let head = format!(
                "task {} {}",
                task_id.as_deref().unwrap_or("?"),
                status.as_deref().unwrap_or("(no status)")
            );
            match summary {
                Some(summary) => bounded(&format!("{head}: {summary}"), MAX_ITEM_CHARS),
                None => bounded(&head, MAX_ITEM_CHARS),
            }
        }
        CatchUpItem::OperatorPrompt { text } => {
            format!("operator prompt \"{}\"", bounded(text, MAX_ITEM_CHARS))
        }
    }
}

/// Pure: the ONE message typed into the resumed pane, or `None` when the tail
/// holds nothing to deliver. Single line, control characters scrubbed, at most
/// [`MAX_LISTED_ITEMS`] items of at most [`MAX_ITEM_CHARS`] characters each.
pub(crate) fn catch_up_message(scan: &TailScan, tail_path: &Path) -> Option<String> {
    if scan.items.is_empty() {
        return None;
    }
    let listed: Vec<String> = scan
        .items
        .iter()
        .take(MAX_LISTED_ITEMS)
        .map(describe)
        .collect();
    let more = scan.items.len().saturating_sub(MAX_LISTED_ITEMS);
    let more = if more > 0 {
        format!("; and {more} more")
    } else {
        String::new()
    };
    let unparseable = if scan.unparseable > 0 {
        format!(" ({} tail line(s) could not be parsed)", scan.unparseable)
    } else {
        String::new()
    };
    Some(one_line(&format!(
        "{} message(s) reached the old account after the move: {}{more}.{unparseable} Full lines: {}.",
        scan.items.len(),
        listed.join("; "),
        tail_path.display()
    )))
}

/// The file-name label for the account a tail came from: the config dir's
/// last component (`C:\claude\.claude-lukas` → `claude-lukas`), restricted to
/// `[A-Za-z0-9._-]` and never starting with a dot.
pub(crate) fn account_file_label(config_dir: &str) -> String {
    let last = config_dir
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let label: String = last
        .trim_start_matches('.')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if label.is_empty() {
        "account".to_string()
    } else {
        label
    }
}

/// Write `bytes` verbatim to `<dst_sidecar>/migration-tail/<label>-<unix_ms>.jsonl`.
/// Never overwrites: an existing file of that name is an error.
pub(crate) fn write_tail(
    dst_sidecar: &Path,
    from_config_dir: &str,
    unix_ms: i64,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let dir = dst_sidecar.join(TAIL_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}-{unix_ms}.jsonl",
        account_file_label(from_config_dir)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(path)
}

/// What one [`rescue_tail`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TailRescue {
    /// The old writer appended nothing after the carry.
    Empty,
    /// The source is shorter than the carry's offset — UNKNOWN which lines
    /// are new, so nothing was written.
    SourceShorter { len: u64, offset: u64 },
    /// The tail was written to `path`.
    Rescued {
        path: PathBuf,
        bytes: usize,
        truncated: bool,
        scan: TailScan,
    },
}

impl TailRescue {
    /// Non-blank tail lines found (0 when nothing was rescued).
    pub(crate) fn lines(&self) -> usize {
        match self {
            Self::Rescued { scan, .. } => scan.lines,
            Self::Empty | Self::SourceShorter { .. } => 0,
        }
    }

    /// The catch-up message to deliver, if the tail holds anything to say.
    pub(crate) fn message(&self) -> Option<String> {
        match self {
            Self::Rescued { path, scan, .. } => catch_up_message(scan, path),
            Self::Empty | Self::SourceShorter { .. } => None,
        }
    }
}

/// Rescue the source `<sid>.jsonl`'s tail past `since.transcript_offset` into
/// the destination's `<sid>/migration-tail/`. Call only once the old writer is
/// proven gone. Never touches either `<sid>.jsonl`.
pub(crate) fn rescue_tail(
    src_config_dir: &str,
    claude_session_id: &str,
    since: &TranscriptCopy,
    now_ms: i64,
) -> Result<TailRescue, String> {
    super::require_session_uuid(claude_session_id)?;
    // The two paths the carry actually used (a long cwd's `<prefix>-<hash>`
    // dir included), never re-derived from the cwd.
    let src = since
        .src_session_dir
        .with_file_name(format!("{claude_session_id}.jsonl"));
    let (bytes, truncated) = match read_tail(&src, since.transcript_offset)? {
        TailRead::SourceShorter { len, offset } => {
            warn!(
                session = %claude_session_id,
                source = %src.display(),
                len,
                offset,
                "session tail rescue: the source transcript is SHORTER than the carry's offset — which lines are new is UNKNOWN; nothing written"
            );
            return Ok(TailRescue::SourceShorter { len, offset });
        }
        TailRead::Bytes { bytes, .. } if bytes.iter().all(u8::is_ascii_whitespace) => {
            return Ok(TailRescue::Empty);
        }
        TailRead::Bytes { bytes, truncated } => (bytes, truncated),
    };
    let dst_sidecar = &since.dst_session_dir;
    let path = write_tail(dst_sidecar, src_config_dir, now_ms, &bytes)
        .map_err(|e| format!("write the tail under {} failed: {e}", dst_sidecar.display()))?;
    let scan = scan_tail(&bytes);
    info!(
        session = %claude_session_id,
        path = %path.display(),
        bytes = bytes.len(),
        truncated,
        lines = scan.lines,
        unparseable = scan.unparseable,
        deliverable = scan.items.len(),
        "session tail rescue: the old account's writer appended after the carry — tail saved beside the session"
    );
    Ok(TailRescue::Rescued {
        path,
        bytes: bytes.len(),
        truncated,
        scan,
    })
}

#[cfg(test)]
#[path = "tail_tests.rs"]
mod tests;
