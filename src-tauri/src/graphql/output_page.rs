//! Byte-cursor pages over a task run's output log (`taskRunOutput`).
//!
//! Plan `2026-09-05-every-bounded-read-is-a-page-that-reads-as-a-corpus`,
//! Phase 5b. `taskRunOutput` used to take a caller-computed `offset` and
//! answer `hasMore = offset + limit < len`. The frontend computed the next
//! offset as `offset + content.length` — UTF-16 code units against a server
//! that slices BYTES — so any multi-byte character made the next page start
//! at the wrong place (or panic the byte slice mid-character).
//!
//! The read is now a walk over an IMMUTABLE key (plan D8): a byte position in
//! `task_runs.output_log`, which is append-only — its one write is
//! `output_log = output_log || $1` (pinned by
//! `tests::output_log_is_only_ever_appended`), so a byte prefix never changes
//! once written and a position keeps addressing the same text forever.
//!
//! The token reuses the shared codec's pieces it can: the
//! [`CursorScope`]/[`ScopeFingerprint`] scope binding (so a cursor minted for
//! one run is refused for another) and the `cursor_malformed` wording. Its
//! payload is a byte position rather than the shared codec's
//! `(timestamp, uuid)` keyset row, because a text position is not a row:
//!
//! ```json
//! {"v":1,"s":"runner.task_runs.output_log:byte,asc","p":<u64>,"f":"<64 hex>"}
//! ```
//!
//! encoded as unpadded urlsafe base64, decoded strictly.

use base64::Engine as _;
use qontinui_types::page::{
    BoundKind, BoundedReadMeta, CursorScope, ScopeFingerprint, SortKey, CURSOR_MAX_TOKEN_LEN,
    CURSOR_WIRE_VERSION,
};
use serde::{Deserialize, Serialize};

/// The page size, in bytes, when the caller passes no `limit`.
pub const OUTPUT_PAGE_DEFAULT_BYTES: i32 = 10_000;
/// The largest page a caller can ask for; `limit` is clamped to `1..=` this.
pub const OUTPUT_PAGE_MAX_BYTES: i32 = 1_048_576;

/// The output log's byte sequence, ascending. See the module doc for why the
/// key is immutable.
pub struct TaskRunOutputWalk;
impl SortKey for TaskRunOutputWalk {
    const ID: &'static str = "runner.task_runs.output_log:byte,asc";
}

/// The scope a run's output cursor is bound to.
pub fn output_scope(task_run_id: &str) -> ScopeFingerprint<TaskRunOutputWalk> {
    CursorScope::<TaskRunOutputWalk>::new()
        .opt_str("task_run_id", Some(task_run_id))
        .finish()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputCursorPayload {
    v: u8,
    s: String,
    p: u64,
    f: String,
}

/// A `cursor` this read did not mint. The one remedy is to restart the walk
/// without `cursor`, so every reason shares the code `cursor_malformed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputCursorError {
    reason: &'static str,
}

impl OutputCursorError {
    /// The stable machine code, shared with `qontinui_types::page::CursorError`.
    pub const CODE: &'static str = "cursor_malformed";

    /// The refusal text, in the fleet's standard wording, naming `cursor`.
    pub fn refusal(&self) -> String {
        format!(
            "invalid cursor — pass a `next_cursor` from a previous taskRunOutput response \
             verbatim, or omit `cursor` for the first page ({})",
            self.reason
        )
    }
}

fn refuse(reason: &'static str) -> OutputCursorError {
    OutputCursorError { reason }
}

/// Mint the token addressing byte position `pos` of the run's output.
pub fn encode_output_cursor(scope: &ScopeFingerprint<TaskRunOutputWalk>, pos: usize) -> String {
    let payload = OutputCursorPayload {
        v: CURSOR_WIRE_VERSION,
        s: TaskRunOutputWalk::ID.to_string(),
        p: pos as u64,
        f: scope.as_hex().to_string(),
    };
    // Four owned primitive fields: `to_vec` cannot fail, and the empty token
    // it would fall back to decodes as malformed rather than panicking.
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Decode a caller-supplied token STRICTLY against `scope` and the output it
/// is about to slice: a position past the end, or inside a multi-byte
/// character, is not one this read minted.
pub fn decode_output_cursor(
    scope: &ScopeFingerprint<TaskRunOutputWalk>,
    token: &str,
    output: &str,
) -> Result<usize, OutputCursorError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(refuse("the cursor is blank"));
    }
    if token.len() > CURSOR_MAX_TOKEN_LEN {
        return Err(refuse(
            "the cursor is longer than any token this read mints",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| refuse("the cursor is not unpadded urlsafe base64"))?;
    let payload: OutputCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| refuse("the cursor does not decode to a cursor payload"))?;
    if payload.v != CURSOR_WIRE_VERSION {
        return Err(refuse(
            "the cursor was minted by a different version of this read",
        ));
    }
    if payload.s != TaskRunOutputWalk::ID {
        return Err(refuse("the cursor was minted for a different sort order"));
    }
    if payload.f != scope.as_hex() {
        return Err(refuse(
            "the cursor was minted for a different task run; a cursor addresses a position in ONE output",
        ));
    }
    let pos =
        usize::try_from(payload.p).map_err(|_| refuse("the cursor's position is out of range"))?;
    if pos > output.len() || !output.is_char_boundary(pos) {
        return Err(refuse("the cursor's position is out of range"));
    }
    Ok(pos)
}

/// One page of the output: the text and its bounded-read envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputPage {
    pub content: String,
    pub meta: BoundedReadMeta,
}

/// Slice the page starting at byte `start` (a char boundary, as
/// [`decode_output_cursor`] guarantees) of at most `limit` bytes, never
/// splitting a character. A page always advances: a `limit` smaller than the
/// character at `start` returns that one character rather than an empty page
/// that would loop forever.
pub fn output_page(
    output: &str,
    start: usize,
    limit: i32,
    scope: &ScopeFingerprint<TaskRunOutputWalk>,
) -> OutputPage {
    let limit = limit.clamp(1, OUTPUT_PAGE_MAX_BYTES);
    let len = output.len();
    let start = start.min(len);
    let mut end = start.saturating_add(limit as usize).min(len);
    while end > start && !output.is_char_boundary(end) {
        end -= 1;
    }
    if end == start && start < len {
        end = start + 1;
        while end < len && !output.is_char_boundary(end) {
            end += 1;
        }
    }
    let content = output.get(start..end).unwrap_or_default().to_string();
    let shown = (end - start) as i64;
    let truncated = end < len;
    OutputPage {
        content,
        meta: BoundedReadMeta {
            count: shown,
            limit: i64::from(limit),
            shown,
            total: Some((len - start) as i64),
            truncated: Some(truncated),
            bound_kind: BoundKind::Exact,
            next_cursor: truncated.then(|| encode_output_cursor(scope, end)),
            available: true,
            filter_narrowed: None,
            enumerate_via: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a whole output page by page and return the pages' text.
    fn walk(output: &str, limit: i32) -> Vec<String> {
        let scope = output_scope("run-1");
        let mut pages = Vec::new();
        let mut start = 0usize;
        loop {
            let page = output_page(output, start, limit, &scope);
            assert_eq!(page.meta.total, Some((output.len() - start) as i64));
            assert_eq!(page.meta.shown, page.content.len() as i64);
            assert_eq!(page.meta.count, page.meta.shown);
            pages.push(page.content);
            match page.meta.next_cursor {
                Some(token) => {
                    assert_eq!(page.meta.truncated, Some(true));
                    start = decode_output_cursor(&scope, &token, output).expect("own token");
                }
                None => {
                    assert_eq!(page.meta.truncated, Some(false));
                    break;
                }
            }
            assert!(pages.len() < 10_000, "the walk must terminate");
        }
        pages
    }

    #[test]
    fn walking_to_exhaustion_reassembles_the_output_exactly() {
        let output = "héllo — wörld 🙂 ".repeat(40);
        for limit in [1, 2, 3, 5, 7, 64, 10_000] {
            let pages = walk(&output, limit);
            assert_eq!(pages.concat(), output, "limit {limit}");
            assert!(pages.iter().all(|p| !p.is_empty()), "every page advances");
        }
    }

    #[test]
    fn an_empty_output_is_one_complete_empty_page() {
        let page = output_page("", 0, 100, &output_scope("run-1"));
        assert_eq!(page.content, "");
        assert_eq!(page.meta.total, Some(0));
        assert_eq!(page.meta.truncated, Some(false));
        assert_eq!(page.meta.next_cursor, None);
    }

    #[test]
    fn limit_is_clamped_and_reported_as_applied() {
        let page = output_page("abc", 0, 0, &output_scope("r"));
        assert_eq!(page.meta.limit, 1);
        let page = output_page("abc", 0, i32::MAX, &output_scope("r"));
        assert_eq!(page.meta.limit, i64::from(OUTPUT_PAGE_MAX_BYTES));
    }

    #[test]
    fn a_cursor_for_another_run_or_mid_character_or_garbage_is_refused() {
        let output = "aé";
        let mine = output_scope("run-1");
        let token = encode_output_cursor(&mine, 1);
        assert_eq!(decode_output_cursor(&mine, &token, output), Ok(1));

        let other = output_scope("run-2");
        assert!(decode_output_cursor(&other, &token, output).is_err());
        // Byte 2 is inside `é`; byte 9 is past the end.
        for bad in [2usize, 9] {
            let token = encode_output_cursor(&mine, bad);
            assert!(
                decode_output_cursor(&mine, &token, output).is_err(),
                "pos {bad}"
            );
        }
        for garbage in ["", "!!", "eyJ2IjoxfQ", "a".repeat(5000).as_str()] {
            let err = decode_output_cursor(&mine, garbage, output).expect_err("garbage");
            assert!(err.refusal().contains("`cursor`"));
        }
        assert_eq!(OutputCursorError::CODE, "cursor_malformed");
    }

    /// Plan D8: the byte-position key is only immutable while every write to
    /// `output_log` APPENDS. Fails on any SQL that assigns it anything else.
    #[test]
    fn output_log_is_only_ever_appended() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // SQL keywords are upper case throughout `queries/` and the raw
        // statements in `src/`; a case-insensitive `set` would match Rust.
        let assign = regex::Regex::new(r"\bSET\b[^;]*?\boutput_log\s*=\s*([^,;]*)").expect("regex");
        let mut appends = 0usize;
        let mut offenders = Vec::new();
        for dir in ["queries", "src"] {
            scan(&root.join(dir), &mut |path, text| {
                for caps in assign.captures_iter(text) {
                    let rhs = caps[1].trim_start();
                    if rhs.to_ascii_lowercase().starts_with("output_log ||") {
                        appends += 1;
                    } else {
                        offenders.push(format!("{}: {}", path.display(), &caps[0]));
                    }
                }
            });
        }
        assert!(
            appends > 0,
            "the scan never saw the append — it is not reading the SQL"
        );
        assert!(
            offenders.is_empty(),
            "a write REPLACES output_log, so an output cursor would address different text: {offenders:#?}"
        );
    }

    fn scan(dir: &std::path::Path, f: &mut dyn FnMut(&std::path::Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan(&path, f);
            } else {
                // This file spells the needles as literals; skip it.
                let scanned = match path.extension().and_then(|e| e.to_str()) {
                    Some("sql") => true,
                    Some("rs") => !path.ends_with("output_page.rs"),
                    _ => false,
                };
                if !scanned {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    f(&path, &text);
                }
            }
        }
    }
}
