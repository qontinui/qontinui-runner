//! Per-session, per-model token usage read off live Claude Code transcripts.
//! Plan `2026-10-09-kpi-telemetry-and-dashboards` Phase 1 (runner half).
//!
//! ## What it produces
//!
//! For every Claude Code session the transcript watcher tails, a CUMULATIVE
//! total per model — input, output, cache-write and cache-read tokens, the
//! number of assistant API responses (`turn_count`), the first and last
//! response timestamps, and a cost ESTIMATE when the model is priced. The
//! [`crate::session::session_transcript_tailer::SessionTranscriptTailer`]
//! feeds it every watcher batch and ships the totals as a `usage_totals`
//! outbox row, which `coord_sync` drains to
//! `POST /coord/sessions/{claude_code_session_id}/usage` — an idempotent
//! upsert of cumulative totals per `(session, model)`.
//!
//! ## The counting rule, and the measurement behind it
//!
//! Claude Code writes ONE JSONL line per content block of an assistant
//! response, and every one of those lines repeats the response's
//! `message.usage`. Summing per line over-counts. Measured 2026-10-09 (plan
//! `2026-10-09-kpi-telemetry-and-dashboards` Phase 1, Step 0) with a scratch
//! script over three headless `stream-json` runs — Claude Code sessions
//! `2af9c80c-13ec-4ad2-b3ab-0085b25d17ad`, `d076a0a8-fb68-466e-b26c-32561f14dca5`
//! and `993d3c4f-1865-4c2d-9a66-b16e4cd848bb` — the sum over DISTINCT
//! `message.id`s of each transcript equalled that run's `result.usage` exactly
//! on all four fields, 3 of 3; the per-line sum matched none of them.
//!
//! Duplicate lines are NOT always identical, though. In subagent transcripts
//! (`<session>/subagents/agent-*.jsonl`) the repeated lines of one response
//! carry a GROWING `output_tokens` — streaming snapshots — while the other
//! three fields stay equal: across 3,239 multi-line responses, `output_tokens`
//! differed in 2,866, never decreased, and the last line always held the
//! maximum. So the rule is: **one entry per `message.id`, keeping the
//! per-field MAXIMUM** — equal to "last line wins" on the data, but
//! independent of the order lines are seen in, which matters because a
//! backfill read and a live batch can deliver the same response in either
//! order. First-seen-wins would under-count subagent output by 3x–4x.
//!
//! Because the rule is a max over a key, ingesting the same line twice is a
//! no-op. That is what lets this module re-read a file's history after a
//! runner restart (below) without coordinating with anything.
//!
//! ## Subagents roll up into the parent
//!
//! A subagent transcript lives at `<projects>/<parent-id>/subagents/agent-*.jsonl`.
//! The watcher tails it like any other `*.jsonl` (its watch is recursive) under
//! the file stem `agent-…`, which is no Claude Code session id — so
//! [`transcript_owner`] maps it back to `<parent-id>` and its usage lands in the
//! PARENT session's totals. Subagent and parent transcripts CAN share
//! `message.id`s — a forked subagent copies parent responses into its own
//! file — so the merge is safe only because both feed ONE map keyed on
//! `message.id` with the per-field max: a copied response is the same entry,
//! counted once, never added twice.
//!
//! A session first seen through a SUBAGENT file (its main transcript idle,
//! e.g. after a runner restart) reads the main transcript and every subagent
//! file on disk before that batch is applied, so its first totals are the
//! whole session's, never a subagent-only partial that would replace coord's
//! cumulative row.
//!
//! ## Cumulative across a restart: the backfill read
//!
//! The watcher starts most tails at EOF (startup discovery, a `Modify` for a
//! file it has never seen), and coord's upsert REPLACES a `(session, model)`
//! row with whatever totals arrive — so totals built only from bytes seen
//! since boot would overwrite a long session's real total with a small one.
//! Each file therefore carries a cursor: the first batch of a file this
//! process has not read reads `[0, batch start)` from disk first, and a batch
//! that starts above the cursor reads the gap. The first sighting of a parent
//! session also reads every subagent transcript already on disk for it. Only
//! usage is parsed and kept (a few dozen bytes per response); the bytes are
//! streamed, never held.
//!
//! That read happens once per file per process, and only for a file that is
//! actively being appended to — an idle transcript produces no batch.
//!
//! ## What is deliberately NOT counted
//!
//! - **Runner workflow transcripts.** A session whose main transcript carries
//!   the `queue-operation` marker is a runner-spawned headless run; its usage
//!   is the executor's to report from `result.usage`. The watcher already
//!   tears those tails down, and this module also excludes their subagent
//!   files (which the watcher does tail), so no partial row competes with the
//!   executor's for the same session.
//! - **`<synthetic>` responses** — client-side placeholders (an interrupted or
//!   errored turn), not API calls.
//!
//! ## Cost is an estimate or nothing
//!
//! Transcripts carry no reported cost. When [`crate::ai_pricing::get_pricing`]
//! prices the model, `cost_usd` is the cache-aware estimate and `cost_source`
//! is `"estimated"`; otherwise both are `null`. Never `0` for an unknown cost:
//! a zero reads as "free", which is a different, false, claim.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How often the tailer ships changed totals. Coarse on purpose: the totals
/// are cumulative, so a later row carries everything an earlier one did, and
/// a minute's lag is invisible to a daily KPI.
pub const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// A session untouched this long is dropped from memory (after a last flush
/// attempt). A later append re-reads its history from disk, so eviction loses
/// nothing but the cost of that read.
pub const USAGE_IDLE_EVICT: Duration = Duration::from_secs(6 * 60 * 60);

/// First delay before re-sending a session whose row coord answered 404;
/// doubles per attempt up to [`USAGE_RESEND_CAP`].
pub const USAGE_RESEND_BASE: Duration = Duration::from_secs(60);
/// Ceiling of the 404 re-send backoff.
pub const USAGE_RESEND_CAP: Duration = Duration::from_secs(30 * 60);
/// 404 re-sends per held session before giving up (a later change to its
/// totals still ships a new row).
pub const USAGE_RESEND_MAX_ATTEMPTS: u32 = 8;

/// The model id Claude Code stamps on client-side placeholder responses.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// How many leading lines of a main transcript the workflow check reads —
/// the same window `terminal::transcript::is_workflow_session` inspects.
const WORKFLOW_MARKER_LINES: usize = 5;

/// Cost provenance on the wire. Only `"estimated"` is produced here; coord's
/// contract also admits `"reported"`, which a transcript never carries.
pub const COST_SOURCE_ESTIMATED: &str = "estimated";

/// One assistant response's usage as one transcript line states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageObs {
    /// `message.id`, else `requestId`, else a digest of the line — the dedup key.
    pub key: String,
    pub model: String,
    /// `[input, output, cache_creation_input, cache_read_input]`.
    pub tokens: [i64; 4],
    /// The line's own `timestamp`, when it parses as RFC 3339.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
}

/// Parse one transcript line into a usage observation. `None` for anything
/// that is not an assistant line carrying `message.usage`, for a
/// `<synthetic>` response, and for malformed JSON.
pub fn parse_usage_line(line: &str) -> Option<UsageObs> {
    // Cheap prefilter: most lines (user turns, tool results, attachments) do
    // not mention usage at all, and this runs over whole files on a backfill.
    if !line.contains("\"usage\"") {
        return None;
    }
    let raw: RawLine = serde_json::from_str(line.trim_end()).ok()?;
    if raw.kind.as_deref() != Some("assistant") {
        return None;
    }
    let msg = raw.message?;
    let usage = msg.usage?;
    let model = msg
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    if model == SYNTHETIC_MODEL {
        return None;
    }
    let key = match msg.id.filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => match raw.request_id.filter(|s| !s.is_empty()) {
            Some(rid) => format!("req:{rid}"),
            None => {
                let digest = Sha256::digest(line.trim_end().as_bytes());
                format!("line:{}", hex::encode(&digest[..16]))
            }
        },
    };
    let at = raw
        .timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let n = |v: Option<i64>| v.unwrap_or(0).max(0);
    Some(UsageObs {
        key,
        model,
        tokens: [
            n(usage.input_tokens),
            n(usage.output_tokens),
            n(usage.cache_creation_input_tokens),
            n(usage.cache_read_input_tokens),
        ],
        at,
    })
}

/// Which Claude Code session a watched transcript's usage belongs to, and
/// whether it is a subagent transcript. The watcher keys a tail on the file
/// stem; for `<parent>/subagents/agent-*.jsonl` that stem is `agent-…`, so the
/// owner is the grandparent directory's name instead.
pub fn transcript_owner(watch_key: &str, path: &Path) -> (String, bool) {
    let parent_dir = path.parent();
    let is_subagent_dir = parent_dir
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("subagents");
    if is_subagent_dir && watch_key.starts_with("agent-") {
        if let Some(owner) = parent_dir
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            return (owner.to_string(), true);
        }
    }
    (watch_key.to_string(), false)
}

/// The subagent transcript directory of the session whose MAIN transcript is
/// `main_path` (`<projects>/<id>.jsonl` → `<projects>/<id>/subagents`).
fn subagent_dir(main_path: &Path, session: &str) -> Option<PathBuf> {
    Some(main_path.parent()?.join(session).join("subagents"))
}

/// The main transcript of `session` given one of its subagent transcripts
/// (`<projects>/<id>/subagents/agent-x.jsonl` → `<projects>/<id>.jsonl`).
fn main_transcript_of_subagent(sub_path: &Path, session: &str) -> Option<PathBuf> {
    Some(
        sub_path
            .parent()? // subagents
            .parent()? // <id>
            .parent()? // <projects>
            .join(format!("{session}.jsonl")),
    )
}

/// Totals for one model of one session — exactly one element of the wire
/// body's `models` array.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelUsageTotals {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    /// `None` (serialized `null`) when the model is unpriced — never `0`.
    pub cost_usd: Option<f64>,
    /// `"estimated"` exactly when `cost_usd` is set, else `null`.
    pub cost_source: Option<&'static str>,
    pub first_turn_at: Option<DateTime<Utc>>,
    pub last_turn_at: Option<DateTime<Utc>>,
    /// Distinct assistant API responses (`message.id`s) for this model.
    pub turn_count: i64,
}

/// One response after dedup: per-field max, earliest and latest line time.
#[derive(Debug, Clone)]
struct MessageUsage {
    model: String,
    tokens: [i64; 4],
    first_at: Option<DateTime<Utc>>,
    last_at: Option<DateTime<Utc>>,
}

fn min_at(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

fn max_at(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}

/// Everything known about one Claude Code session's usage.
#[derive(Debug, Default)]
struct SessionUsage {
    messages: HashMap<String, MessageUsage>,
    /// Per transcript file (main and subagents): the byte offset through which
    /// it has been ingested. Absent = never read by this process.
    cursors: HashMap<PathBuf, u64>,
    /// The subagent directory has been swept once, every read succeeding. A
    /// sweep with a failed read stays `false` and is retried by the next plan.
    subagents_swept: bool,
    /// When set, the next [`UsageLedger::dirty`] at or after this instant
    /// includes the session even if unchanged — coord answered 404 to its last
    /// row (it did not know the session yet). See [`UsageLedger::request_resend`].
    resend_at: Option<Instant>,
    /// 404 re-sends requested so far; capped at [`USAGE_RESEND_MAX_ATTEMPTS`].
    resend_attempts: u32,
    /// Bumped whenever an ingest changes a total.
    version: u64,
    /// The version last handed to the outbox.
    emitted_version: u64,
    last_touched: Option<Instant>,
}

impl SessionUsage {
    /// Fold one observation in. Returns whether any total changed.
    fn ingest(&mut self, obs: UsageObs) -> bool {
        match self.messages.get_mut(&obs.key) {
            None => {
                self.messages.insert(
                    obs.key,
                    MessageUsage {
                        model: obs.model,
                        tokens: obs.tokens,
                        first_at: obs.at,
                        last_at: obs.at,
                    },
                );
                true
            }
            Some(m) => {
                let mut changed = false;
                for (have, seen) in m.tokens.iter_mut().zip(obs.tokens) {
                    if seen > *have {
                        *have = seen;
                        changed = true;
                    }
                }
                let first = min_at(m.first_at, obs.at);
                let last = max_at(m.last_at, obs.at);
                changed |= first != m.first_at || last != m.last_at;
                m.first_at = first;
                m.last_at = last;
                changed
            }
        }
    }

    fn totals(&self) -> Vec<ModelUsageTotals> {
        let mut by_model: HashMap<&str, ModelUsageTotals> = HashMap::new();
        for m in self.messages.values() {
            let t = by_model
                .entry(m.model.as_str())
                .or_insert_with(|| ModelUsageTotals {
                    model: m.model.clone(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    cost_usd: None,
                    cost_source: None,
                    first_turn_at: None,
                    last_turn_at: None,
                    turn_count: 0,
                });
            t.input_tokens += m.tokens[0];
            t.output_tokens += m.tokens[1];
            t.cache_creation_input_tokens += m.tokens[2];
            t.cache_read_input_tokens += m.tokens[3];
            t.first_turn_at = min_at(t.first_turn_at, m.first_at);
            t.last_turn_at = max_at(t.last_turn_at, m.last_at);
            t.turn_count += 1;
        }
        let mut out: Vec<ModelUsageTotals> = by_model.into_values().collect();
        for t in &mut out {
            t.cost_usd = estimate_cost_usd(t);
            t.cost_source = t.cost_usd.map(|_| COST_SOURCE_ESTIMATED);
        }
        out.sort_by(|a, b| a.model.cmp(&b.model));
        out
    }
}

/// The cache-aware cost estimate, or `None` when the catalog does not price
/// the model. Gated on [`crate::ai_pricing::get_pricing`] first because the
/// cost function itself falls back to a family-blind GUESS for an unpriced
/// model, which this wire must not present as an estimate of this model.
fn estimate_cost_usd(t: &ModelUsageTotals) -> Option<f64> {
    crate::ai_pricing::get_pricing(&t.model)?;
    let u = |v: i64| u64::try_from(v).unwrap_or(0);
    Some(crate::ai_pricing::calculate_cost_usd_with_cache(
        u(t.input_tokens),
        u(t.output_tokens),
        u(t.cache_creation_input_tokens),
        u(t.cache_read_input_tokens),
        &t.model,
    ))
}

/// A byte range of one file to read before a batch can be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadRange {
    path: PathBuf,
    from: u64,
    /// `None` = to EOF.
    to: Option<u64>,
    /// A failed read of this range holds the batch (it would otherwise ship a
    /// partial total). `false` only for subagent-sweep reads, whose failure
    /// leaves the sweep pending instead.
    required: bool,
    /// Part of the subagent sweep.
    sweep: bool,
}

/// What a range read yielded: the observations and the offset just past the
/// last COMPLETE line consumed (a trailing fragment is left for later).
struct RangeRead {
    path: PathBuf,
    end: u64,
    obs: Vec<UsageObs>,
}

/// Stream `[from, to)` of `path` (to EOF when `to` is `None`), keeping only
/// usage observations. Lines are counted only when newline-terminated, so a
/// fragment the writer has not finished is neither parsed nor passed.
fn read_usage_range(path: &Path, from: u64, to: Option<u64>) -> std::io::Result<RangeRead> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let to = to.unwrap_or(len).min(len);
    if from >= to {
        return Ok(RangeRead {
            path: path.to_path_buf(),
            end: from,
            obs: Vec::new(),
        });
    }
    file.seek(SeekFrom::Start(from))?;
    let mut reader = BufReader::new(file.take(to - from));
    let mut buf = Vec::new();
    let mut pos = from;
    let mut obs = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        pos += n as u64;
        if let Some(o) = std::str::from_utf8(&buf).ok().and_then(parse_usage_line) {
            obs.push(o);
        }
    }
    Ok(RangeRead {
        path: path.to_path_buf(),
        end: pos,
        obs,
    })
}

/// Whether the main transcript at `path` is a runner workflow session
/// (`queue-operation` in its first lines). Unreadable = not one: the same
/// posture as the watcher's own sniff, which treats a not-yet-flushed file as
/// interactive and re-checks later.
fn is_workflow_transcript(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = String::new();
    for line in BufReader::new(f)
        .lines()
        .take(WORKFLOW_MARKER_LINES)
        .map_while(Result::ok)
    {
        head.push_str(&line);
        head.push('\n');
    }
    crate::terminal::transcript::is_workflow_session_marker(&head)
}

/// A session whose totals changed since they were last handed to the outbox.
#[derive(Debug, Clone, PartialEq)]
pub struct DirtyTotals {
    /// The Claude Code session id — the wire path key.
    pub claude_session_id: String,
    pub version: u64,
    pub models: Vec<ModelUsageTotals>,
}

/// Process-wide usage state, owned by the transcript tailer.
#[derive(Debug, Default)]
pub struct UsageLedger {
    sessions: Mutex<HashMap<String, SessionUsage>>,
    /// Sessions identified as runner workflow runs — never counted here —
    /// with when they were last seen, so [`Self::evict_idle`] bounds the set.
    workflow_sessions: Mutex<HashMap<String, Instant>>,
}

/// What [`UsageLedger::plan`] decided under the lock: the ranges known to be
/// needed, and whether a subagent sweep (a directory listing, done OUTSIDE the
/// lock) is owed.
struct Plan {
    reads: Vec<ReadRange>,
    /// The subagent directory to sweep, when the sweep is owed.
    sweep_dir: Option<PathBuf>,
    /// Cursors already held for that directory's files.
    sweep_cursors: HashMap<PathBuf, u64>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.sweep_dir.is_none()
    }
}

impl UsageLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionUsage>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn workflow(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.workflow_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `true` (and the entry refreshed) when `session` is a known workflow run.
    fn is_excluded(&self, session: &str) -> bool {
        match self.workflow().get_mut(session) {
            Some(seen) => {
                *seen = Instant::now();
                true
            }
            None => false,
        }
    }

    /// The main transcript of `session`, given the path a batch arrived on.
    fn main_path(session: &str, is_subagent: bool, path: &Path) -> Option<PathBuf> {
        if is_subagent {
            main_transcript_of_subagent(path, session)
        } else {
            Some(path.to_path_buf())
        }
    }

    /// Decide, under the ledger lock and WITHOUT touching the filesystem, what
    /// must be read before `path`'s batch at `file_start` can be applied.
    fn plan(&self, session: &str, is_subagent: bool, path: &Path, file_start: u64) -> Plan {
        let sessions = self.lock();
        let state = sessions.get(session);
        let cursor_of = |p: &Path| state.and_then(|s| s.cursors.get(p).copied());
        let mut reads = Vec::new();
        match cursor_of(path) {
            None if file_start > 0 => reads.push(ReadRange {
                path: path.to_path_buf(),
                from: 0,
                to: Some(file_start),
                required: true,
                sweep: false,
            }),
            Some(c) if c < file_start => reads.push(ReadRange {
                path: path.to_path_buf(),
                from: c,
                to: Some(file_start),
                required: true,
                sweep: false,
            }),
            _ => {}
        }
        let main = Self::main_path(session, is_subagent, path);
        // A session first reached through a subagent file reads its main
        // transcript whole, so no subagent-only partial is ever flushed.
        if is_subagent {
            // Cursor first: a session already reading its main transcript
            // never stats it under the lock (the steady state).
            if let Some(main) = main
                .as_ref()
                .filter(|m| cursor_of(m).is_none() && m.exists())
            {
                reads.push(ReadRange {
                    path: main.clone(),
                    from: 0,
                    to: None,
                    required: true,
                    sweep: false,
                });
            }
        }
        let swept = state.is_some_and(|s| s.subagents_swept);
        let sweep_dir = if swept {
            None
        } else {
            main.as_deref().and_then(|m| subagent_dir(m, session))
        };
        let sweep_cursors = match (&sweep_dir, state) {
            (Some(_), Some(s)) => s.cursors.clone(),
            _ => HashMap::new(),
        };
        Plan {
            reads,
            sweep_dir,
            sweep_cursors,
        }
    }

    /// Whether this batch is the first this process sees for `session`, which
    /// is when the workflow check runs.
    fn is_new_session(&self, session: &str) -> bool {
        !self.lock().contains_key(session)
    }

    /// Fold completed reads and the batch itself into `session`.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        session: &str,
        path: &Path,
        file_start: u64,
        appended: &str,
        reads: Vec<RangeRead>,
        sweep_complete: bool,
        now: Instant,
    ) {
        let batch: Vec<UsageObs> = appended.lines().filter_map(parse_usage_line).collect();
        let mut sessions = self.lock();
        let state = sessions.entry(session.to_string()).or_default();
        let mut changed = false;
        for read in reads {
            for o in read.obs {
                changed |= state.ingest(o);
            }
            let c = state.cursors.entry(read.path).or_insert(0);
            *c = (*c).max(read.end);
        }
        for o in batch {
            changed |= state.ingest(o);
        }
        let end = file_start + appended.len() as u64;
        let c = state.cursors.entry(path.to_path_buf()).or_insert(0);
        *c = (*c).max(end);
        if sweep_complete {
            state.subagents_swept = true;
        }
        if changed {
            state.version += 1;
        }
        state.last_touched = Some(now);
    }

    /// The NON-BLOCKING form of [`Self::observe`]: applies the batch and
    /// returns `true` when no file access is needed (the steady state — the
    /// batch starts at this file's cursor and the subagent sweep is done),
    /// else does NOTHING and returns `false`, and the caller runs
    /// [`Self::observe`] on a blocking thread. Never touches the filesystem.
    pub fn try_observe(&self, watch_key: &str, path: &Path, file_start: u64, appended: &str) -> bool {
        let (session, is_subagent) = transcript_owner(watch_key, path);
        if self.is_excluded(&session) {
            return true;
        }
        if self.is_new_session(&session) {
            // The workflow check reads the main transcript's head.
            return false;
        }
        if !self.plan(&session, is_subagent, path, file_start).is_empty() {
            return false;
        }
        self.apply(
            &session,
            path,
            file_start,
            appended,
            Vec::new(),
            false,
            Instant::now(),
        );
        true
    }

    /// Fold one watcher batch — `appended`, which begins at byte `file_start`
    /// of `path` — into its session's totals, first reading whatever of the
    /// session this process has not ingested yet: the file's own history, the
    /// main transcript when the session is first reached through a subagent
    /// file, and (once) every subagent transcript on disk. Synchronous file
    /// I/O: call it from a blocking context.
    ///
    /// A failed read of the file's own history or of the main transcript
    /// HOLDS the batch (nothing is applied, every cursor stays, the next batch
    /// retries): applying it would ship a partial total. A failed sweep read
    /// is logged, the rest is applied, and the sweep stays owed.
    pub fn observe(&self, watch_key: &str, path: &Path, file_start: u64, appended: &str) {
        let (session, is_subagent) = transcript_owner(watch_key, path);
        if self.is_excluded(&session) {
            return;
        }
        if self.is_new_session(&session) {
            let main = Self::main_path(&session, is_subagent, path);
            if main.as_deref().is_some_and(is_workflow_transcript) {
                tracing::debug!(
                    session = %session,
                    "usage_totals: runner workflow session — its usage is the executor's to \
                     report; not counted from the transcript"
                );
                self.workflow().insert(session, Instant::now());
                return;
            }
        }
        let Plan {
            mut reads,
            sweep_dir,
            sweep_cursors,
        } = self.plan(&session, is_subagent, path, file_start);
        // The directory listing happens here, outside the ledger lock.
        if let Some(dir) = sweep_dir.as_ref() {
            for sub in list_subagent_transcripts(dir) {
                let from = sweep_cursors.get(&sub).copied().unwrap_or(0);
                reads.push(ReadRange {
                    path: sub,
                    from,
                    to: None,
                    required: false,
                    sweep: true,
                });
            }
        }
        let mut done = Vec::with_capacity(reads.len());
        let mut sweep_complete = sweep_dir.is_some();
        for r in reads {
            match read_usage_range(&r.path, r.from, r.to) {
                Ok(read) => done.push(read),
                Err(e) => {
                    tracing::warn!(
                        session = %session,
                        path = %r.path.display(),
                        from = r.from,
                        error = %e,
                        "usage_totals: could not read transcript history — retried by a later \
                         batch"
                    );
                    if r.required {
                        // Hold the batch: see the doc comment.
                        return;
                    }
                    if r.sweep {
                        sweep_complete = false;
                    }
                }
            }
        }
        self.apply(
            &session,
            path,
            file_start,
            appended,
            done,
            sweep_complete,
            Instant::now(),
        );
    }

    /// Sessions whose totals changed since they were last marked emitted, or
    /// that coord asked to have re-sent ([`Self::request_resend`]).
    pub fn dirty(&self) -> Vec<DirtyTotals> {
        self.dirty_at(Instant::now())
    }

    /// [`Self::dirty`] as of `now` (a due re-send counts).
    pub fn dirty_at(&self, now: Instant) -> Vec<DirtyTotals> {
        self.lock()
            .iter()
            .filter(|(_, s)| {
                let resend_due = s.resend_at.is_some_and(|at| at <= now);
                (s.version != s.emitted_version || resend_due) && !s.messages.is_empty()
            })
            .map(|(id, s)| DirtyTotals {
                claude_session_id: id.clone(),
                version: s.version,
                models: s.totals(),
            })
            .collect()
    }

    /// The current totals of one session, whether or not they changed — the
    /// close path ships them unconditionally. `None` when nothing was counted.
    pub fn snapshot(&self, claude_session_id: &str) -> Option<DirtyTotals> {
        let sessions = self.lock();
        let s = sessions.get(claude_session_id)?;
        if s.messages.is_empty() {
            return None;
        }
        Some(DirtyTotals {
            claude_session_id: claude_session_id.to_string(),
            version: s.version,
            models: s.totals(),
        })
    }

    /// Record that `version` of a session's totals reached the outbox. A later
    /// version stays dirty.
    pub fn mark_emitted(&self, claude_session_id: &str, version: u64) {
        if let Some(s) = self.lock().get_mut(claude_session_id) {
            s.emitted_version = s.emitted_version.max(version);
            s.resend_at = None;
        }
    }

    /// Coord answered 404 to this session's last row (it did not know the
    /// session yet). Re-send the totals after a backoff — 60 s, doubling to
    /// 30 min — at most [`USAGE_RESEND_MAX_ATTEMPTS`] times per held session;
    /// past that the session is re-sent only when its totals change. A
    /// session not held needs nothing: its next append re-reads it and ships
    /// fresh totals.
    pub fn request_resend(&self, claude_session_id: &str, now: Instant) {
        if let Some(s) = self.lock().get_mut(claude_session_id) {
            if s.resend_attempts >= USAGE_RESEND_MAX_ATTEMPTS {
                return;
            }
            s.resend_attempts += 1;
            let factor = 1u32 << (s.resend_attempts - 1).min(10);
            s.resend_at = Some(now + std::cmp::min(USAGE_RESEND_BASE * factor, USAGE_RESEND_CAP));
        }
    }

    /// Forget a session (its coord row closed). A later append re-reads it.
    pub fn remove(&self, claude_session_id: &str) {
        self.lock().remove(claude_session_id);
    }

    /// Drop sessions (and remembered workflow runs) untouched for `idle`;
    /// returns how many sessions were dropped.
    pub fn evict_idle(&self, now: Instant, idle: Duration) -> usize {
        self.workflow()
            .retain(|_, seen| now.saturating_duration_since(*seen) < idle);
        let mut sessions = self.lock();
        let before = sessions.len();
        sessions.retain(|_, s| {
            s.last_touched
                .is_none_or(|t| now.saturating_duration_since(t) < idle)
        });
        before - sessions.len()
    }

    /// Sessions currently held — a diagnostic.
    pub fn session_count(&self) -> usize {
        self.lock().len()
    }

    /// Workflow runs currently remembered — a diagnostic.
    pub fn workflow_session_count(&self) -> usize {
        self.workflow().len()
    }
}

/// `agent-*.jsonl` files in a subagent directory, sorted. A missing
/// directory (no subagents yet) is an empty list.
fn list_subagent_transcripts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("agent-"))
        })
        .collect();
    out.sort();
    out
}

/// The `usage_totals` outbox payload: the Claude Code session id (the wire
/// PATH key — not the coord `sessions.id`, which is the outbox lane) and the
/// models array that is the request body verbatim.
pub fn usage_totals_payload(claude_session_id: &str, models: &[ModelUsageTotals]) -> serde_json::Value {
    serde_json::json!({
        "claude_code_session_id": claude_session_id,
        "models": models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A fixture assistant line. `out` varies to model streaming snapshots.
    fn line(id: &str, model: &str, usage: [i64; 4], ts: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "requestId": format!("req_{id}"),
            "message": {
                "id": id,
                "model": model,
                "role": "assistant",
                "content": [{"type": "text", "text": "x"}],
                "usage": {
                    "input_tokens": usage[0],
                    "output_tokens": usage[1],
                    "cache_creation_input_tokens": usage[2],
                    "cache_read_input_tokens": usage[3],
                    "service_tier": "standard"
                }
            }
        })
        .to_string()
            + "\n"
    }

    fn user_line() -> String {
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n".to_string()
    }

    fn one(totals: &[ModelUsageTotals], model: &str) -> ModelUsageTotals {
        totals
            .iter()
            .find(|t| t.model == model)
            .unwrap_or_else(|| panic!("no totals for {model}: {totals:?}"))
            .clone()
    }

    #[test]
    fn parse_skips_non_assistant_synthetic_and_malformed() {
        assert!(parse_usage_line(&user_line()).is_none());
        assert!(parse_usage_line("{\"usage\": not json").is_none());
        assert!(
            parse_usage_line(&line("m1", "<synthetic>", [0, 0, 0, 0], "2026-10-09T10:00:00Z"))
                .is_none()
        );
        let o = parse_usage_line(&line("m1", "claude-opus-5-5", [1, 2, 3, 4], "2026-10-09T10:00:00Z"))
            .unwrap();
        assert_eq!(o.key, "m1");
        assert_eq!(o.tokens, [1, 2, 3, 4]);
        assert_eq!(o.at.unwrap().to_rfc3339(), "2026-10-09T10:00:00+00:00");
    }

    #[test]
    fn parse_falls_back_to_request_id_then_a_line_digest() {
        let mut v: serde_json::Value = serde_json::from_str(&line(
            "m1",
            "claude-opus-5-5",
            [1, 1, 1, 1],
            "2026-10-09T10:00:00Z",
        ))
        .unwrap();
        v["message"].as_object_mut().unwrap().remove("id");
        let o = parse_usage_line(&v.to_string()).unwrap();
        assert_eq!(o.key, "req:req_m1");
        v.as_object_mut().unwrap().remove("requestId");
        let a = parse_usage_line(&v.to_string()).unwrap();
        let b = parse_usage_line(&v.to_string()).unwrap();
        assert!(a.key.starts_with("line:"));
        assert_eq!(a.key, b.key, "the digest key is stable, so a re-read dedups");
    }

    /// The measured rule: duplicate lines of one response count once, and the
    /// growing streaming `output_tokens` resolves to its maximum regardless of
    /// the order the lines are seen in.
    #[test]
    fn duplicate_message_lines_count_once_at_their_per_field_max() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let ts = "2026-10-09T10:00:00Z";
        // Later snapshot first, then an earlier one: max, not last-seen.
        let batch = [
            line("m1", "claude-opus-5-5", [10, 500, 100, 1000], ts),
            line("m1", "claude-opus-5-5", [10, 40, 100, 1000], ts),
            line("m1", "claude-opus-5-5", [10, 500, 100, 1000], ts),
            user_line(),
            line("m2", "claude-opus-5-5", [5, 7, 0, 2000], "2026-10-09T10:05:00Z"),
        ]
        .concat();
        std::fs::write(&p, &batch).unwrap();
        ledger.observe("s1", &p, 0, &batch);
        let t = one(&ledger.snapshot("s1").unwrap().models, "claude-opus-5-5");
        assert_eq!(t.input_tokens, 15);
        assert_eq!(t.output_tokens, 507);
        assert_eq!(t.cache_creation_input_tokens, 100);
        assert_eq!(t.cache_read_input_tokens, 3000);
        assert_eq!(t.turn_count, 2);
        assert_eq!(t.first_turn_at.unwrap().to_rfc3339(), "2026-10-09T10:00:00+00:00");
        assert_eq!(t.last_turn_at.unwrap().to_rfc3339(), "2026-10-09T10:05:00+00:00");

        // Re-ingesting the same bytes changes nothing (idempotent).
        let v = ledger.snapshot("s1").unwrap().version;
        ledger.observe("s1", &p, 0, &batch);
        assert_eq!(ledger.snapshot("s1").unwrap().version, v);
    }

    #[test]
    fn models_are_totalled_separately() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let batch = [
            line("a", "claude-opus-5-5", [1, 2, 3, 4], "2026-10-09T10:00:00Z"),
            line("b", "claude-haiku-4-5-20251001", [10, 20, 30, 40], "2026-10-09T10:01:00Z"),
            line("c", "claude-haiku-4-5-20251001", [1, 1, 1, 1], "2026-10-09T10:02:00Z"),
        ]
        .concat();
        std::fs::write(&p, &batch).unwrap();
        ledger.observe("s1", &p, 0, &batch);
        let models = ledger.snapshot("s1").unwrap().models;
        assert_eq!(models.len(), 2);
        let h = one(&models, "claude-haiku-4-5-20251001");
        assert_eq!((h.input_tokens, h.output_tokens, h.turn_count), (11, 21, 2));
        let o = one(&models, "claude-opus-5-5");
        assert_eq!((o.input_tokens, o.turn_count), (1, 1));
    }

    /// An unpriced model's cost stays null — never 0 — and a priced one is an
    /// estimate that says so.
    #[test]
    fn unknown_cost_stays_null_and_a_priced_model_is_marked_estimated() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let batch = [
            line("a", "some-unpriced-model-x", [100, 100, 0, 0], "2026-10-09T10:00:00Z"),
            line("b", "claude-haiku-4-5", [1000, 1000, 0, 0], "2026-10-09T10:00:00Z"),
        ]
        .concat();
        std::fs::write(&p, &batch).unwrap();
        ledger.observe("s1", &p, 0, &batch);
        let models = ledger.snapshot("s1").unwrap().models;
        let unpriced = one(&models, "some-unpriced-model-x");
        assert_eq!(unpriced.cost_usd, None);
        assert_eq!(unpriced.cost_source, None);
        let wire = serde_json::to_value(&unpriced).unwrap();
        assert!(wire["cost_usd"].is_null() && wire["cost_source"].is_null());

        let priced = one(&models, "claude-haiku-4-5");
        assert!(crate::ai_pricing::get_pricing("claude-haiku-4-5").is_some());
        assert!(priced.cost_usd.unwrap() > 0.0);
        assert_eq!(priced.cost_source, Some(COST_SOURCE_ESTIMATED));
    }

    /// Subagent transcripts roll into the PARENT session: those already on
    /// disk are swept on the parent's first batch, and a live subagent batch
    /// (watch key `agent-…`) lands on the parent too, deduped against the
    /// sweep.
    #[test]
    fn subagent_usage_merges_into_the_parent_session() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let parent = "11111111-2222-3333-4444-555555555555";
        let main = dir.path().join(format!("{parent}.jsonl"));
        let subdir = dir.path().join(parent).join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let sub1 = subdir.join("agent-aaa.jsonl");
        std::fs::write(
            &sub1,
            line("s1", "claude-opus-5-5", [1, 10, 0, 100], "2026-10-09T09:00:00Z")
                + &line("s1", "claude-opus-5-5", [1, 30, 0, 100], "2026-10-09T09:00:00Z"),
        )
        .unwrap();
        let mbatch = line("p1", "claude-opus-5-5", [2, 5, 0, 50], "2026-10-09T10:00:00Z");
        std::fs::write(&main, &mbatch).unwrap();

        ledger.observe(parent, &main, 0, &mbatch);
        let t = one(&ledger.snapshot(parent).unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.output_tokens, t.turn_count), (3, 35, 2));
        assert_eq!(t.first_turn_at.unwrap().to_rfc3339(), "2026-10-09T09:00:00+00:00");

        // A second subagent appears and is tailed live under its own stem.
        let sub2 = subdir.join("agent-bbb.jsonl");
        let sbatch = line("s2", "claude-haiku-4-5", [4, 4, 4, 4], "2026-10-09T10:10:00Z");
        std::fs::write(&sub2, &sbatch).unwrap();
        ledger.observe("agent-bbb", &sub2, 0, &sbatch);
        // The first subagent is also tailed live from 0: the sweep already
        // counted it, so nothing changes.
        let s1_bytes = std::fs::read_to_string(&sub1).unwrap();
        ledger.observe("agent-aaa", &sub1, 0, &s1_bytes);

        let models = ledger.snapshot(parent).unwrap().models;
        assert_eq!(one(&models, "claude-opus-5-5").output_tokens, 35);
        assert_eq!(one(&models, "claude-haiku-4-5").turn_count, 1);
        assert!(ledger.snapshot("agent-bbb").is_none(), "no row keyed on a subagent stem");
        assert_eq!(ledger.session_count(), 1);
    }

    /// A tail that starts mid-file (startup discovery tails from EOF) reads
    /// the history below it first, so the first totals are already cumulative.
    #[test]
    fn a_first_batch_mid_file_backfills_the_history_below_it() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let history = line("old", "claude-opus-5-5", [100, 100, 100, 100], "2026-10-08T10:00:00Z")
            + &user_line();
        let batch = line("new", "claude-opus-5-5", [1, 1, 1, 1], "2026-10-09T10:00:00Z");
        std::fs::write(&p, history.clone() + &batch).unwrap();
        let start = history.len() as u64;
        assert!(
            !ledger.try_observe("s1", &p, start, &batch),
            "a first sighting needs the file read — the non-blocking form must decline"
        );
        ledger.observe("s1", &p, start, &batch);
        let t = one(&ledger.snapshot("s1").unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.turn_count), (101, 2));
        assert_eq!(t.first_turn_at.unwrap().to_rfc3339(), "2026-10-08T10:00:00+00:00");

        // Steady state: the next batch starts at the cursor and needs no read.
        let next = line("next", "claude-opus-5-5", [1, 0, 0, 0], "2026-10-09T10:01:00Z");
        let at = start + batch.len() as u64;
        assert!(ledger.try_observe("s1", &p, at, &next));
        assert_eq!(
            one(&ledger.snapshot("s1").unwrap().models, "claude-opus-5-5").turn_count,
            3
        );
    }

    /// A gap between the cursor and a batch (a batch this ledger never saw)
    /// is read from the file.
    #[test]
    fn a_gap_above_the_cursor_is_read_from_the_file() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let a = line("a", "claude-opus-5-5", [1, 0, 0, 0], "2026-10-09T10:00:00Z");
        let b = line("b", "claude-opus-5-5", [2, 0, 0, 0], "2026-10-09T10:01:00Z");
        let c = line("c", "claude-opus-5-5", [4, 0, 0, 0], "2026-10-09T10:02:00Z");
        std::fs::write(&p, a.clone() + &b + &c).unwrap();
        ledger.observe("s1", &p, 0, &a);
        let c_start = (a.len() + b.len()) as u64;
        assert!(!ledger.try_observe("s1", &p, c_start, &c));
        ledger.observe("s1", &p, c_start, &c);
        let t = one(&ledger.snapshot("s1").unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.turn_count), (7, 3));
    }

    #[test]
    fn a_workflow_session_and_its_subagents_are_not_counted() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let parent = "wf-session";
        let main = dir.path().join(format!("{parent}.jsonl"));
        std::fs::write(
            &main,
            "{\"type\":\"queue-operation\",\"operation\":\"enqueue\"}\n".to_string()
                + &line("m", "claude-opus-5-5", [1, 1, 1, 1], "2026-10-09T10:00:00Z"),
        )
        .unwrap();
        let subdir = dir.path().join(parent).join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let sub = subdir.join("agent-x.jsonl");
        let sbatch = line("s", "claude-opus-5-5", [1, 1, 1, 1], "2026-10-09T10:00:00Z");
        std::fs::write(&sub, &sbatch).unwrap();
        ledger.observe("agent-x", &sub, 0, &sbatch);
        assert!(ledger.snapshot(parent).is_none());
        assert!(ledger.try_observe("agent-x", &sub, 0, &sbatch));
        assert_eq!(ledger.session_count(), 0);
    }

    #[test]
    fn dirty_tracks_versions_and_eviction_forgets_idle_sessions() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let a = line("a", "claude-opus-5-5", [1, 0, 0, 0], "2026-10-09T10:00:00Z");
        std::fs::write(&p, &a).unwrap();
        ledger.observe("s1", &p, 0, &a);
        let d = ledger.dirty();
        assert_eq!(d.len(), 1);
        ledger.mark_emitted("s1", d[0].version);
        assert!(ledger.dirty().is_empty());
        assert!(ledger.snapshot("s1").is_some(), "close still ships clean totals");

        let later = Instant::now() + Duration::from_secs(10);
        assert_eq!(ledger.evict_idle(later, Duration::from_secs(60)), 0);
        assert_eq!(ledger.evict_idle(later, Duration::from_secs(1)), 1);
        assert_eq!(ledger.session_count(), 0);
    }

    /// H1: a session first reached through a SUBAGENT file (its main
    /// transcript idle) reads the main transcript and the other subagents
    /// before that batch is applied — the first totals are never a
    /// subagent-only partial.
    #[test]
    fn a_session_first_seen_through_a_subagent_counts_the_whole_session() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let parent = "22222222-2222-3333-4444-555555555555";
        let main = dir.path().join(format!("{parent}.jsonl"));
        std::fs::write(
            &main,
            line("p1", "claude-opus-5-5", [100, 100, 0, 0], "2026-10-09T08:00:00Z"),
        )
        .unwrap();
        let subdir = dir.path().join(parent).join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(
            subdir.join("agent-old.jsonl"),
            line("o1", "claude-opus-5-5", [10, 10, 0, 0], "2026-10-09T09:00:00Z"),
        )
        .unwrap();
        let live = subdir.join("agent-live.jsonl");
        let batch = line("l1", "claude-opus-5-5", [1, 1, 0, 0], "2026-10-09T10:00:00Z");
        std::fs::write(&live, &batch).unwrap();

        assert!(!ledger.try_observe("agent-live", &live, 0, &batch));
        ledger.observe("agent-live", &live, 0, &batch);
        let t = one(&ledger.snapshot(parent).unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.turn_count), (111, 3));
        assert_eq!(t.first_turn_at.unwrap().to_rfc3339(), "2026-10-09T08:00:00+00:00");

        // The main transcript's own later batch (from its cursor) adds only
        // what is new.
        let more = line("p2", "claude-opus-5-5", [1000, 0, 0, 0], "2026-10-09T11:00:00Z");
        let at = std::fs::metadata(&main).unwrap().len();
        assert!(ledger.try_observe(parent, &main, at, &more));
        let t = one(&ledger.snapshot(parent).unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.turn_count), (1111, 4));
    }

    /// M2: a sweep read that fails leaves the sweep owed; the next batch
    /// retries it and picks the file up.
    #[cfg(unix)]
    #[test]
    fn a_failed_sweep_read_is_retried_by_the_next_batch() {
        use std::os::unix::fs::PermissionsExt;
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let parent = "33333333-2222-3333-4444-555555555555";
        let main = dir.path().join(format!("{parent}.jsonl"));
        let a = line("m1", "claude-opus-5-5", [1, 0, 0, 0], "2026-10-09T10:00:00Z");
        std::fs::write(&main, &a).unwrap();
        let subdir = dir.path().join(parent).join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let sub = subdir.join("agent-x.jsonl");
        std::fs::write(
            &sub,
            line("s1", "claude-opus-5-5", [10, 0, 0, 0], "2026-10-09T10:00:00Z"),
        )
        .unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&sub).is_ok() {
            // Running with privileges that ignore file modes: nothing to test.
            return;
        }
        ledger.observe(parent, &main, 0, &a);
        let t = one(&ledger.snapshot(parent).unwrap().models, "claude-opus-5-5");
        assert_eq!(t.input_tokens, 1, "the unreadable subagent is not yet counted");

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o644)).unwrap();
        let b = line("m2", "claude-opus-5-5", [100, 0, 0, 0], "2026-10-09T10:01:00Z");
        assert!(
            !ledger.try_observe(parent, &main, a.len() as u64, &b),
            "the sweep is still owed, so the batch needs the blocking path"
        );
        ledger.observe(parent, &main, a.len() as u64, &b);
        let t = one(&ledger.snapshot(parent).unwrap().models, "claude-opus-5-5");
        assert_eq!((t.input_tokens, t.turn_count), (111, 3));
    }

    #[test]
    fn a_404_resend_waits_out_its_backoff_and_is_capped() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let p = dir.path().join("s1.jsonl");
        let a = line("a", "claude-opus-5-5", [1, 0, 0, 0], "2026-10-09T10:00:00Z");
        std::fs::write(&p, &a).unwrap();
        ledger.observe("s1", &p, 0, &a);
        let d = ledger.dirty();
        ledger.mark_emitted("s1", d[0].version);
        assert!(ledger.dirty().is_empty());

        let t0 = Instant::now();
        ledger.request_resend("s1", t0);
        assert!(ledger.dirty_at(t0).is_empty(), "not before its backoff");
        let d = ledger.dirty_at(t0 + Duration::from_secs(60));
        assert_eq!(d.len(), 1, "due after 60 s");
        ledger.mark_emitted("s1", d[0].version);
        assert!(ledger.dirty_at(t0 + Duration::from_secs(3600)).is_empty());

        // The second attempt waits 120 s; attempts past the cap are ignored.
        ledger.request_resend("s1", t0);
        assert!(ledger.dirty_at(t0 + Duration::from_secs(119)).is_empty());
        assert_eq!(ledger.dirty_at(t0 + Duration::from_secs(120)).len(), 1);
        for _ in 0..USAGE_RESEND_MAX_ATTEMPTS {
            ledger.request_resend("s1", t0);
        }
        let d = ledger.dirty_at(t0 + USAGE_RESEND_CAP);
        assert_eq!(d.len(), 1, "the backoff is capped at 30 min");
        ledger.mark_emitted("s1", d[0].version);
        ledger.request_resend("s1", t0);
        assert!(
            ledger.dirty_at(t0 + Duration::from_secs(24 * 3600)).is_empty(),
            "no re-send past the attempt cap"
        );

        ledger.request_resend("never-seen", t0);
        assert_eq!(ledger.session_count(), 1, "a resend never creates a session");
    }

    #[test]
    fn remembered_workflow_runs_are_evicted_when_idle() {
        let ledger = UsageLedger::new();
        let dir = tempdir().unwrap();
        let main = dir.path().join("wf.jsonl");
        std::fs::write(&main, "{\"type\":\"queue-operation\"}\n").unwrap();
        ledger.observe("wf", &main, 0, "");
        assert_eq!(ledger.workflow_session_count(), 1);
        let later = Instant::now() + Duration::from_secs(10);
        ledger.evict_idle(later, Duration::from_secs(1));
        assert_eq!(ledger.workflow_session_count(), 0);
    }

    #[test]
    fn owner_of_a_subagent_transcript_is_the_grandparent_directory() {
        let p = Path::new("/c/projects/enc/abc-123/subagents/agent-x1.jsonl");
        assert_eq!(transcript_owner("agent-x1", p), ("abc-123".to_string(), true));
        let m = Path::new("/c/projects/enc/abc-123.jsonl");
        assert_eq!(transcript_owner("abc-123", m), ("abc-123".to_string(), false));
    }

    #[test]
    fn payload_carries_the_claude_session_id_and_the_wire_models() {
        let models = vec![ModelUsageTotals {
            model: "m".into(),
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 4,
            cost_usd: None,
            cost_source: None,
            first_turn_at: None,
            last_turn_at: None,
            turn_count: 1,
        }];
        let p = usage_totals_payload("csid", &models);
        assert_eq!(p["claude_code_session_id"], "csid");
        let m = &p["models"][0];
        for k in [
            "model",
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "cost_usd",
            "cost_source",
            "first_turn_at",
            "last_turn_at",
            "turn_count",
        ] {
            assert!(m.get(k).is_some(), "wire field {k} missing");
        }
    }
}
