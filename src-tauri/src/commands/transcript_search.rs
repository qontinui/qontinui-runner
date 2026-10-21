//! `transcript_search` — full-text search over Claude session transcripts, the
//! backend of the Session Manager's `text:"…"` query term (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 4).
//!
//! ## What it reads
//!
//! Paths come from [`DiskTranscriptIndex::transcript_files`] (the same config
//! dirs every other transcript reader discovers — no second walk of its own),
//! and text from [`transcript::read_transcript_file`] →
//! [`transcript::extract_text_from_messages`], so a hit means the words are in
//! the conversation as the transcript viewer renders it: user and assistant
//! text and plan content, not tool results or progress records.
//!
//! ## Matching
//!
//! The query is a list of terms, all of which must occur (AND). A `"quoted
//! phrase"` is one term, matched verbatim including its punctuation; anything
//! else splits on whitespace. Matching is ASCII-case-insensitive — ASCII
//! lowercasing keeps byte offsets stable, so a snippet is cut from the
//! original text at the hit. Each hit reports the total occurrence count of
//! every term and one snippet around the first term's first occurrence.
//!
//! ## Cost and honesty
//!
//! Extracted text is cached per transcript keyed by `(path, mtime, size)`, so a
//! repeat search re-parses only the files that changed; the cache is bounded
//! by bytes and evicts the oldest entries first. A search over 200 synthetic
//! transcripts (~140 KB of JSONL each, ~28 MB in all) completes within the
//! plan's 2 s budget, see `two_hundred_transcripts_search_within_the_budget`,
//! so the on-disk inverted index the plan's Risks section reserved for a > 2 s
//! measurement is not built.
//!
//! A scan is bounded in time. Every response states `{scanned, total,
//! truncated, elapsedMs}`, and a scan that stopped early (time budget), dropped
//! hits (limit) or could not read a file says so in `truncation` — and, when
//! the caller named sessions, lists the ones it did not scan in `notScanned`.
//! A capped scan is never presented as complete (served policy
//! `verification-and-evidence` `unknown-must-not-render-as-a-default`): the
//! frontend renders a session it did not scan as UNKNOWN for `text:`, never as
//! "no match".

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::warn;

use crate::session::reconcile::{DiskTranscriptIndex, TranscriptFile};
use crate::terminal::transcript;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Default number of hits returned when the caller names no limit.
pub const DEFAULT_LIMIT: usize = 200;
/// Hard ceiling on `limit`.
pub const MAX_LIMIT: usize = 2_000;
/// How long one scan may read before it stops and reports `time_budget`.
pub const SCAN_TIME_BUDGET: Duration = Duration::from_secs(8);
/// Backstop on the blocking task itself, measured off the async runtime
/// (`off_runtime::deadline`) so an exhausted blocking pool cannot hold the
/// command open forever.
const BLOCKING_WAIT: Duration = Duration::from_secs(15);
/// Bytes of extracted text the cache may hold, counting BOTH the original and
/// the lowercased copy of every entry — so this is the resident bound, not
/// half of it.
pub const TEXT_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
/// Snippet context before / after the hit, in bytes (rounded to char
/// boundaries).
const SNIPPET_BEFORE: usize = 80;
const SNIPPET_AFTER: usize = 140;

/// `truncation` reason: the scan ran out of [`SCAN_TIME_BUDGET`].
pub const TRUNCATED_TIME_BUDGET: &str = "time_budget";
/// `truncation` reason: more sessions matched than `limit`.
pub const TRUNCATED_LIMIT: &str = "limit";
/// `truncation` reason: at least one transcript could not be read.
pub const TRUNCATED_UNREADABLE: &str = "unreadable";
/// `truncation` reason: the blocking pool never ran the scan.
pub const TRUNCATED_NOT_RUN: &str = "not_run";

// ── Query ────────────────────────────────────────────────────────────────────

/// A parsed search: every term must occur. Terms are ASCII-lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub terms: Vec<String>,
}

/// Split `raw` into terms: `"quoted phrases"` verbatim (an unterminated quote
/// runs to the end), everything else on whitespace. Empty terms are dropped.
pub fn parse_search_query(raw: &str) -> SearchQuery {
    let mut terms = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix('"') {
            let (phrase, next) = after.split_once('"').unwrap_or((after, ""));
            if !phrase.trim().is_empty() {
                terms.push(phrase.to_ascii_lowercase());
            }
            rest = next;
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"')
                .unwrap_or(rest.len());
            // `end` is a char boundary (`find` returns one), so this never
            // falls back.
            let (word, next) = rest.split_at_checked(end).unwrap_or((rest, ""));
            terms.push(word.to_ascii_lowercase());
            rest = next;
        }
    }
    SearchQuery { terms }
}

// ── Text cache ───────────────────────────────────────────────────────────────

/// One transcript's extracted text, valid while its `(mtime, size)` holds.
#[derive(Debug, Clone)]
struct CachedText {
    mtime_ms: i64,
    size: u64,
    text: Arc<str>,
    lower: Arc<str>,
}

impl CachedText {
    fn bytes(&self) -> usize {
        self.text.len() + self.lower.len()
    }
}

/// Extracted text keyed by transcript path, bounded by
/// [`TextCache::budget_bytes`] and evicted oldest-inserted first.
#[derive(Debug)]
pub struct TextCache {
    entries: HashMap<PathBuf, CachedText>,
    order: VecDeque<PathBuf>,
    bytes: usize,
    budget_bytes: usize,
}

impl TextCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget_bytes,
        }
    }

    fn get(&self, file: &TranscriptFile) -> Option<CachedText> {
        self.entries
            .get(&file.path)
            .filter(|c| c.mtime_ms == file.mtime_ms && c.size == file.size)
            .cloned()
    }

    fn insert(&mut self, path: PathBuf, entry: CachedText) {
        if let Some(old) = self.entries.remove(&path) {
            self.bytes -= old.bytes();
            self.order.retain(|p| p != &path);
        }
        // A single text larger than the whole budget is served once, uncached.
        if entry.bytes() > self.budget_bytes {
            return;
        }
        self.bytes += entry.bytes();
        self.order.push_back(path.clone());
        self.entries.insert(path, entry);
        while self.bytes > self.budget_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.bytes -= gone.bytes();
            }
        }
    }

    /// Entries held — a test seam.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

static TEXT_CACHE: std::sync::OnceLock<Mutex<TextCache>> = std::sync::OnceLock::new();

fn global_cache() -> &'static Mutex<TextCache> {
    TEXT_CACHE.get_or_init(|| Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES)))
}

/// The text of `file`, from the cache when its `(mtime, size)` still holds,
/// else read, extracted and cached. `Err` when the file cannot be read.
fn text_for(file: &TranscriptFile, cache: &Mutex<TextCache>) -> Result<CachedText, String> {
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(file)) {
        return Ok(hit);
    }
    let messages = transcript::read_transcript_file(&file.path)?;
    let text = transcript::extract_text_from_messages(&messages);
    let lower = text.to_ascii_lowercase();
    let entry = CachedText {
        mtime_ms: file.mtime_ms,
        size: file.size,
        text: Arc::from(text),
        lower: Arc::from(lower),
    };
    if let Ok(mut c) = cache.lock() {
        c.insert(file.path.clone(), entry.clone());
    }
    Ok(entry)
}

// ── Matching ─────────────────────────────────────────────────────────────────

/// `(hit_count, first_hit_offset)` when every term occurs in `lower`.
fn match_text(lower: &str, query: &SearchQuery) -> Option<(usize, usize)> {
    let mut total = 0;
    let mut first = None;
    for (i, term) in query.terms.iter().enumerate() {
        let mut count = 0;
        for (pos, _) in lower.match_indices(term.as_str()) {
            if i == 0 && first.is_none() {
                first = Some(pos);
            }
            count += 1;
        }
        if count == 0 {
            return None;
        }
        total += count;
    }
    Some((total, first.unwrap_or(0)))
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// A one-line excerpt of `text` around byte `at`, whitespace collapsed, with
/// `…` where it was cut.
pub fn snippet_at(text: &str, at: usize, term_len: usize) -> String {
    let start = floor_boundary(text, at.saturating_sub(SNIPPET_BEFORE));
    let end = ceil_boundary(text, at + term_len + SNIPPET_AFTER);
    let body = text
        .get(start..end)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lead = if start > 0 { "…" } else { "" };
    let tail = if end < text.len() { "…" } else { "" };
    format!("{lead}{body}{tail}")
}

// ── Response ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchHit {
    pub session_id: String,
    pub config_dir: String,
    pub path: String,
    /// Occurrences of every term, summed.
    pub hit_count: usize,
    pub snippet: String,
    pub mtime_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSearchResponse {
    /// The terms the query parsed into (lowercased) — what was actually matched.
    pub terms: Vec<String>,
    /// Best first: most occurrences, then most recently modified. At most `limit`.
    pub hits: Vec<TranscriptSearchHit>,
    /// Transcripts that matched among those scanned (can exceed `hits.len()`).
    pub matched: usize,
    /// Transcripts whose text was searched.
    pub scanned: usize,
    /// Transcripts in scope (all on disk, or those of the named sessions).
    pub total: usize,
    /// Transcripts that could not be read — in scope, not scanned.
    pub failed: usize,
    /// True whenever the answer is not the complete one: see `truncation`.
    pub truncated: bool,
    /// Why: [`TRUNCATED_TIME_BUDGET`] | [`TRUNCATED_LIMIT`] |
    /// [`TRUNCATED_UNREADABLE`] | [`TRUNCATED_NOT_RUN`].
    pub truncation: Vec<String>,
    /// Named sessions with a transcript that was NOT searched (time budget,
    /// unreadable, or the scan never ran) — UNKNOWN for this query, never "no
    /// match".
    pub not_scanned: Vec<String>,
    /// Named sessions with no transcript on disk at all.
    pub missing: Vec<String>,
    pub elapsed_ms: u64,
}

/// Search options; `limit` is already clamped.
#[derive(Debug, Clone, Copy)]
pub struct SearchOptions {
    pub limit: usize,
    pub time_budget: Duration,
}

/// Search `files` (already scoped by the caller) for `query`. Pure apart from
/// the file reads behind `cache`; parallel across the machine's cores.
pub fn search_files(
    mut files: Vec<TranscriptFile>,
    query: &SearchQuery,
    opts: SearchOptions,
    cache: &Mutex<TextCache>,
) -> TranscriptSearchResponse {
    let started = Instant::now();
    let total = files.len();
    // Newest first, so a time-budget stop has searched what is most likely live.
    files.sort_by(|a, b| {
        b.mtime_ms
            .cmp(&a.mtime_ms)
            .then_with(|| a.path.cmp(&b.path))
    });

    enum Outcome {
        Hit(TranscriptSearchHit),
        Miss,
        Failed,
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::with_capacity(total));
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(total.max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if started.elapsed() >= opts.time_budget {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = files.get(i) else { break };
                let outcome = match text_for(file, cache) {
                    Err(e) => {
                        tracing::debug!("transcript_search: {:?} unreadable: {e}", file.path);
                        Outcome::Failed
                    }
                    Ok(text) => match match_text(&text.lower, query) {
                        None => Outcome::Miss,
                        Some((hit_count, at)) => Outcome::Hit(TranscriptSearchHit {
                            session_id: file.session_id.clone(),
                            config_dir: file.config_dir.to_string_lossy().into_owned(),
                            path: file.path.to_string_lossy().into_owned(),
                            hit_count,
                            snippet: snippet_at(
                                &text.text,
                                at,
                                query.terms.first().map_or(0, String::len),
                            ),
                            mtime_ms: file.mtime_ms,
                        }),
                    },
                };
                if let Ok(mut r) = results.lock() {
                    r.push((i, outcome));
                }
            });
        }
    });

    let results = results.into_inner().unwrap_or_default();
    let mut done = vec![false; total];
    let mut hits = Vec::new();
    let mut failed = 0;
    for (i, outcome) in results {
        match outcome {
            Outcome::Hit(h) => {
                done[i] = true;
                hits.push(h);
            }
            Outcome::Miss => done[i] = true,
            Outcome::Failed => failed += 1,
        }
    }
    let scanned = done.iter().filter(|d| **d).count();
    let unscanned = total - scanned;
    let mut truncation = Vec::new();
    if unscanned > failed {
        truncation.push(TRUNCATED_TIME_BUDGET.to_string());
    }
    if failed > 0 {
        truncation.push(TRUNCATED_UNREADABLE.to_string());
    }
    hits.sort_by(|a, b| {
        b.hit_count
            .cmp(&a.hit_count)
            .then_with(|| b.mtime_ms.cmp(&a.mtime_ms))
            .then_with(|| a.path.cmp(&b.path))
    });
    let matched = hits.len();
    if matched > opts.limit {
        hits.truncate(opts.limit);
        truncation.push(TRUNCATED_LIMIT.to_string());
    }
    // A session is scanned when every one of its transcripts was.
    let mut not_scanned: Vec<String> = files
        .iter()
        .zip(&done)
        .filter(|(_, d)| !**d)
        .map(|(f, _)| f.session_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    not_scanned.sort();
    TranscriptSearchResponse {
        terms: query.terms.clone(),
        hits,
        matched,
        scanned,
        total,
        failed,
        truncated: !truncation.is_empty(),
        truncation,
        not_scanned,
        missing: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// Restrict `files` to the named sessions. Returns the scoped files and the
/// named ids with no transcript at all.
pub fn scope_files(
    files: Vec<TranscriptFile>,
    session_keys: Option<&[String]>,
) -> (Vec<TranscriptFile>, Vec<String>) {
    let Some(keys) = session_keys else {
        return (files, Vec::new());
    };
    let wanted: HashSet<&str> = keys
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .collect();
    let scoped: Vec<TranscriptFile> = files
        .into_iter()
        .filter(|f| wanted.contains(f.session_id.as_str()))
        .collect();
    let present: HashSet<&str> = scoped.iter().map(|f| f.session_id.as_str()).collect();
    let mut missing: Vec<String> = wanted
        .iter()
        .filter(|k| !present.contains(*k))
        .map(|k| k.to_string())
        .collect();
    missing.sort();
    (scoped, missing)
}

/// The whole search over a given index: enumerate, scope, search. Blocking.
pub fn run_search(
    index: &DiskTranscriptIndex,
    query: &SearchQuery,
    session_keys: Option<&[String]>,
    opts: SearchOptions,
    cache: &Mutex<TextCache>,
) -> TranscriptSearchResponse {
    let (files, missing) = scope_files(index.transcript_files(), session_keys);
    let mut resp = search_files(files, query, opts, cache);
    resp.missing = missing;
    resp
}

/// `transcript_search(query, sessionKeys?, limit?)`.
///
/// `sessionKeys` restricts the scan to those Claude session ids (the Session
/// Manager passes the rows it is filtering); absent searches every transcript
/// on disk (the session search palette). `Err` only for an empty query, which
/// would otherwise "match" everything.
#[tauri::command]
pub async fn transcript_search(
    query: String,
    session_keys: Option<Vec<String>>,
    limit: Option<usize>,
) -> Result<TranscriptSearchResponse, String> {
    let parsed = parse_search_query(&query);
    if parsed.terms.is_empty() {
        return Err("empty_query".to_string());
    }
    let opts = SearchOptions {
        limit: limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        time_budget: SCAN_TIME_BUDGET,
    };
    let started = Instant::now();
    let terms = parsed.terms.clone();
    let requested = session_keys.clone();
    let task = spawn_blocking_tracked(move || {
        let index = DiskTranscriptIndex::discover();
        run_search(
            &index,
            &parsed,
            session_keys.as_deref(),
            opts,
            global_cache(),
        )
    });
    tokio::select! {
        joined = task => joined.map_err(|e| format!("transcript_search task failed: {e}")),
        _ = crate::off_runtime::deadline(BLOCKING_WAIT) => {
            warn!(
                "transcript_search: scan did not finish within {}s (blocking pool exhausted?) — \
                 answering not-run rather than holding the command open",
                BLOCKING_WAIT.as_secs()
            );
            Ok(not_run_response(
                terms,
                requested.as_deref(),
                started.elapsed(),
            ))
        }
    }
}

/// The answer when the blocking pool never ran the scan: truncated
/// `not_run`, and every requested session key listed in `not_scanned` — a
/// scoped search that did not run must never read as "no match" for the
/// sessions it named.
pub fn not_run_response(
    terms: Vec<String>,
    session_keys: Option<&[String]>,
    elapsed: Duration,
) -> TranscriptSearchResponse {
    let mut not_scanned: Vec<String> = session_keys
        .unwrap_or_default()
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    not_scanned.sort();
    TranscriptSearchResponse {
        terms,
        hits: Vec::new(),
        matched: 0,
        scanned: 0,
        total: 0,
        failed: 0,
        truncated: true,
        truncation: vec![TRUNCATED_NOT_RUN.to_string()],
        not_scanned,
        missing: Vec::new(),
        elapsed_ms: elapsed.as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user_line(uuid: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": "2026-10-13T10:00:00Z",
            "message": { "role": "user", "content": text },
        })
        .to_string()
    }

    fn assistant_line(uuid: &str, text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "timestamp": "2026-10-13T10:00:01Z",
            "message": {
                "role": "assistant",
                "model": "claude",
                "content": [{ "type": "text", "text": text }],
            },
        })
        .to_string()
    }

    /// Write one transcript `<config>/projects/<project>/<id>.jsonl`.
    fn write_transcript(config: &std::path::Path, project: &str, id: &str, lines: &[String]) {
        let dir = config.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join(format!("{id}.jsonl"))).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn opts() -> SearchOptions {
        SearchOptions {
            limit: DEFAULT_LIMIT,
            time_budget: SCAN_TIME_BUDGET,
        }
    }

    fn search(
        config: &std::path::Path,
        q: &str,
        keys: Option<&[String]>,
    ) -> TranscriptSearchResponse {
        let index = DiskTranscriptIndex::with_config_dirs(vec![config.to_path_buf()]);
        let cache = Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES));
        run_search(&index, &parse_search_query(q), keys, opts(), &cache)
    }

    #[test]
    fn query_splits_words_and_keeps_quoted_phrases_whole() {
        assert_eq!(
            parse_search_query(r#"Merge "train, stuck!" coord"#).terms,
            vec!["merge", "train, stuck!", "coord"]
        );
        assert_eq!(
            parse_search_query(r#"  "unterminated phrase"#).terms,
            vec!["unterminated phrase"]
        );
        assert!(parse_search_query("   ").terms.is_empty());
        assert!(parse_search_query(r#""""#).terms.is_empty());
    }

    #[test]
    fn a_phrase_spanning_punctuation_matches_verbatim_and_not_reordered() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(
            tmp.path(),
            "p",
            "s1",
            &[user_line("u1", "the merge train, stuck again")],
        );
        write_transcript(
            tmp.path(),
            "p",
            "s2",
            &[user_line("u1", "train merge stuck")],
        );
        let r = search(tmp.path(), r#""train, stuck""#, None);
        assert_eq!(
            r.hits
                .iter()
                .map(|h| h.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["s1"]
        );
        assert_eq!(r.scanned, 2);
        assert_eq!(r.total, 2);
        assert!(!r.truncated);
    }

    #[test]
    fn every_term_must_occur_and_case_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(
            tmp.path(),
            "p",
            "both",
            &[
                user_line("u", "Fix the Merge queue"),
                assistant_line("a", "TRAIN repaired; merge done"),
            ],
        );
        write_transcript(tmp.path(), "p", "one", &[user_line("u", "merge only")]);
        let r = search(tmp.path(), "merge train", None);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].session_id, "both");
        // merge ×2 + train ×1.
        assert_eq!(r.hits[0].hit_count, 3);
        assert!(
            r.hits[0].snippet.contains("Merge"),
            "snippet keeps original case: {}",
            r.hits[0].snippet
        );
        assert_eq!(r.matched, 1);
    }

    #[test]
    fn hits_sort_by_occurrences() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(tmp.path(), "p", "few", &[user_line("u", "coord")]);
        write_transcript(
            tmp.path(),
            "p",
            "many",
            &[user_line("u", "coord coord coord")],
        );
        let r = search(tmp.path(), "coord", None);
        assert_eq!(r.hits[0].session_id, "many");
        assert_eq!(r.hits[0].hit_count, 3);
    }

    #[test]
    fn tool_results_and_non_message_records_are_not_searched() {
        let tmp = tempfile::tempdir().unwrap();
        let tool_result = serde_json::json!({
            "type": "user", "uuid": "t",
            "message": { "content": [{ "type": "tool_result", "content": "secretword" }] },
        })
        .to_string();
        let progress = serde_json::json!({ "type": "progress", "data": "secretword" }).to_string();
        write_transcript(tmp.path(), "p", "s", &[tool_result, progress]);
        let r = search(tmp.path(), "secretword", None);
        assert!(r.hits.is_empty());
        assert_eq!(r.scanned, 1);
    }

    #[test]
    fn named_sessions_scope_the_scan_and_absent_ones_are_missing() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(tmp.path(), "p", "a", &[user_line("u", "needle")]);
        write_transcript(tmp.path(), "p", "b", &[user_line("u", "needle")]);
        let keys = vec!["a".to_string(), "ghost".to_string()];
        let r = search(tmp.path(), "needle", Some(&keys));
        assert_eq!(r.total, 1);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].session_id, "a");
        assert_eq!(r.missing, vec!["ghost"]);
        assert!(r.not_scanned.is_empty());
    }

    #[test]
    fn a_hit_limit_is_reported_as_truncation_with_the_true_match_count() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write_transcript(
                tmp.path(),
                "p",
                &format!("s{i}"),
                &[user_line("u", "needle")],
            );
        }
        let index = DiskTranscriptIndex::with_config_dirs(vec![tmp.path().to_path_buf()]);
        let cache = Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES));
        let r = run_search(
            &index,
            &parse_search_query("needle"),
            None,
            SearchOptions {
                limit: 2,
                time_budget: SCAN_TIME_BUDGET,
            },
            &cache,
        );
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.matched, 5);
        assert!(r.truncated);
        assert_eq!(r.truncation, vec![TRUNCATED_LIMIT]);
        assert_eq!(r.scanned, 5);
    }

    /// A zero time budget scans nothing — and says so: truncated, every named
    /// session listed as not scanned, never an empty "no matches" answer.
    #[test]
    fn an_exhausted_time_budget_is_truncated_and_lists_what_was_not_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(tmp.path(), "p", "a", &[user_line("u", "needle")]);
        write_transcript(tmp.path(), "p", "b", &[user_line("u", "other")]);
        let index = DiskTranscriptIndex::with_config_dirs(vec![tmp.path().to_path_buf()]);
        let cache = Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES));
        let keys = vec!["a".to_string(), "b".to_string()];
        let r = run_search(
            &index,
            &parse_search_query("needle"),
            Some(&keys),
            SearchOptions {
                limit: 10,
                time_budget: Duration::ZERO,
            },
            &cache,
        );
        assert_eq!(r.total, 2);
        assert_eq!(r.scanned, 0);
        assert!(r.truncated);
        assert_eq!(r.truncation, vec![TRUNCATED_TIME_BUDGET]);
        assert_eq!(r.not_scanned, vec!["a", "b"]);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_transcript_is_truncation_not_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(tmp.path(), "p", "ok", &[user_line("u", "needle")]);
        // A listed transcript `read_to_string` rejects: invalid UTF-8.
        let bad = tmp.path().join("projects").join("p").join("bad.jsonl");
        std::fs::write(&bad, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let keys = vec!["ok".to_string(), "bad".to_string()];
        let r = search(tmp.path(), "needle", Some(&keys));
        assert_eq!(r.total, 2);
        assert_eq!(r.scanned, 1);
        assert_eq!(r.failed, 1);
        assert!(r.truncated);
        assert_eq!(r.truncation, vec![TRUNCATED_UNREADABLE]);
        assert_eq!(r.not_scanned, vec!["bad"]);
    }

    /// A scan the blocking pool never ran names every requested session as
    /// not scanned, so none of them reads as "no match".
    #[test]
    fn a_scan_that_never_ran_lists_every_requested_session_as_not_scanned() {
        let keys = vec![
            "b".to_string(),
            "a".to_string(),
            " ".to_string(),
            "a".to_string(),
        ];
        let r = not_run_response(
            vec!["needle".to_string()],
            Some(&keys),
            Duration::from_secs(15),
        );
        assert!(r.truncated);
        assert_eq!(r.truncation, vec![TRUNCATED_NOT_RUN]);
        assert_eq!(r.not_scanned, vec!["a", "b"]);
        assert_eq!(r.scanned, 0);
        assert!(r.hits.is_empty());
        // Unscoped: nothing to name, but still truncated not_run.
        let unscoped = not_run_response(vec!["x".to_string()], None, Duration::ZERO);
        assert!(unscoped.truncated && unscoped.not_scanned.is_empty());
    }

    #[test]
    fn the_cache_is_keyed_by_mtime_and_size_and_rereads_a_changed_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_transcript(tmp.path(), "p", "s", &[user_line("u", "before")]);
        let index = DiskTranscriptIndex::with_config_dirs(vec![tmp.path().to_path_buf()]);
        let cache = Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES));
        let q = parse_search_query("after");
        assert!(run_search(&index, &q, None, opts(), &cache).hits.is_empty());
        assert_eq!(cache.lock().unwrap().len(), 1);
        // Same path, new content and size: the cached text must not answer.
        write_transcript(
            tmp.path(),
            "p",
            "s",
            &[user_line("u", "after, a longer line")],
        );
        assert_eq!(run_search(&index, &q, None, opts(), &cache).hits.len(), 1);
        assert_eq!(cache.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_cache_evicts_oldest_past_its_byte_budget() {
        let mut cache = TextCache::new(10);
        let entry = |s: &str| CachedText {
            mtime_ms: 1,
            size: 1,
            text: Arc::from(s),
            lower: Arc::from(s),
        };
        cache.insert(PathBuf::from("a"), entry("abc")); // 6 bytes
        cache.insert(PathBuf::from("b"), entry("de")); // 4 → 10
        assert_eq!(cache.len(), 2);
        cache.insert(PathBuf::from("c"), entry("f")); // 2 → evicts a
        assert_eq!(cache.len(), 2);
        assert!(!cache.entries.contains_key(&PathBuf::from("a")));
        // Larger than the whole budget: served, never cached.
        cache.insert(PathBuf::from("huge"), entry("0123456789"));
        assert!(!cache.entries.contains_key(&PathBuf::from("huge")));
    }

    #[test]
    fn snippets_cut_on_char_boundaries() {
        let text = format!("{}needle{}", "é".repeat(100), "ü".repeat(100));
        let at = text.find("needle").unwrap();
        let s = snippet_at(&text, at, 6);
        assert!(s.contains("needle"));
        assert!(s.starts_with('…') && s.ends_with('…'));
    }

    /// The plan's measured budget: 200 synthetic transcripts searched in < 2 s
    /// cold. Each is ~140 KB of JSONL (60 user/assistant turn pairs of ~1 KB
    /// text) — ~28 MB in all. The assertion is the budget itself.
    #[test]
    fn two_hundred_transcripts_search_within_the_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let filler = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(18);
        for i in 0..200 {
            let mut lines = Vec::with_capacity(121);
            for t in 0..60 {
                lines.push(user_line(&format!("u{t}"), &format!("{filler} turn {t}")));
                lines.push(assistant_line(
                    &format!("a{t}"),
                    &format!("{filler} reply {t}"),
                ));
            }
            if i % 10 == 0 {
                lines.push(user_line("needle", "the merge train is stuck"));
            }
            write_transcript(
                tmp.path(),
                &format!("proj-{}", i % 7),
                &format!("s{i:03}"),
                &lines,
            );
        }
        let index = DiskTranscriptIndex::with_config_dirs(vec![tmp.path().to_path_buf()]);
        let cache = Mutex::new(TextCache::new(TEXT_CACHE_BUDGET_BYTES));
        let q = parse_search_query(r#""merge train""#);

        let cold = Instant::now();
        let r = run_search(&index, &q, None, opts(), &cache);
        let cold_ms = cold.elapsed().as_millis();
        let warm = Instant::now();
        let r2 = run_search(&index, &q, None, opts(), &cache);
        let warm_ms = warm.elapsed().as_millis();
        eprintln!("transcript_search 200 transcripts: cold {cold_ms} ms, warm {warm_ms} ms");

        assert_eq!(r.total, 200);
        assert_eq!(r.scanned, 200);
        assert!(!r.truncated);
        assert_eq!(r.hits.len(), 20);
        assert_eq!(r2.hits, r.hits);
        assert!(
            cold_ms < 2_000,
            "cold search took {cold_ms} ms (budget 2000)"
        );
        assert!(
            warm_ms < 2_000,
            "warm search took {warm_ms} ms (budget 2000)"
        );
    }
}
