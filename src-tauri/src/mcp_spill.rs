//! MCP tool-output spill store — session-scoped, bounded, on-disk storage for
//! oversized `tools/call` result bodies (plan
//! `2026-08-20-runner-mcp-tool-output-spill`, Phase 2).
//!
//! # Why this lives in the LIB crate
//!
//! Exactly the reason [`crate::runner_breadcrumb`] and [`crate::intercept_core`]
//! were lifted here: the WRITER is `bin/wrappers_mcp.rs`, a second bin, and a
//! second bin cannot import from the runner bin's module tree (see this crate's
//! `lib.rs` header). The READER — today the same binary's `read_spilled_result`
//! tool, tomorrow anything else that needs a spilled body — must agree with the
//! writer on the on-disk shape byte for byte. One module ⇒ one schema ⇒ writer
//! and reader cannot drift.
//!
//! `session/local_store.rs`'s retention discipline is the model this module
//! follows, but that is a BIN module (`main.rs`) and so is readable as a
//! *pattern* only — it is not callable from here.
//!
//! # File format
//!
//! One file per spill at `<root>/<session>/<id>.spill`:
//!
//! ```text
//! {"schema":1,"id":"…","session":"…","tool":"…",…}\n
//! <body bytes, verbatim UTF-8>
//! ```
//!
//! A single-line JSON header followed by a newline and then the body exactly as
//! it would have gone on the wire. One file rather than a metadata sidecar plus
//! a body file, because two files cannot be published atomically together — a
//! crash between the two renames leaves a body with no header (unsweepable by
//! tool/session) or a header with no body (a dead pointer that *looks* live).
//! With one file [`crate::fs_atomic::atomic_write_owner_only`] makes the whole
//! record appear at once or not at all. `serde_json::to_string` never emits a
//! raw newline, so the first `\n` in the file is always the header terminator.
//!
//! # Bodies are NOT redacted here
//!
//! Settled in the plan's Risks section. `session/redact.rs` is a bin module and
//! its own doc forbids duplicating its regex; more to the point it guards
//! content leaving the machine and self-describes as "defense in depth, NOT a
//! security boundary". A spill file never leaves the box, and the unredacted
//! body was already going into the agent's context uncapped — spilling it does
//! not widen exposure. The correct control is the on-disk one, so the spill
//! directory and every spill file get the same owner-only treatment as the
//! encrypted token store ([`crate::fs_perms`]).
//!
//! # Diagnostics go to stderr, not `tracing`
//!
//! The only consumer today is a binary with no `tracing` subscriber installed,
//! where a `warn!` would evaporate. stderr is that binary's one diagnostic
//! channel (same reasoning as the Phase 1 metric line), and in the runner bin
//! stderr is captured into the dev logs, so a message is never silently lost.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version of the on-disk header. Bump only on a BREAKING shape change;
/// [`SpillStore::read`] refuses a record whose schema it does not know rather
/// than misparsing it.
pub const SPILL_SCHEMA: u32 = 1;

/// File extension for a spill record. The sweep only ever considers files with
/// this suffix, so `fs_atomic`'s in-flight temp files (`*.tmp.<pid>.<seq>.…`)
/// are never mistaken for records.
pub const SPILL_EXTENSION: &str = "spill";

/// Directory under the runner's app-data dir (`~/.qontinui/runner/`) that holds
/// every session's spills.
pub const SPILL_DIR_NAME: &str = "mcp-spill";

/// Total on-disk bound across ALL sessions' spills, enforced by
/// [`SpillStore::put`] before every write.
///
/// 64 MiB is the same number `session/local_store.rs` picked for the session
/// outbox, and for the same reason: it is orders of magnitude above a healthy
/// steady state (measured spill bodies are 90–300 KB, so this is ~200+ records)
/// while staying far below the disk-pressure threshold that has bitten this box
/// before. Spill files are a NEW disk consumer, which is why the bound ships
/// with the store rather than as a follow-up.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Age bound. A locator is only useful for as long as the conversation that was
/// handed it is still running, and an MCP server process lives exactly as long
/// as its AI client. A day is comfortably longer than any single session while
/// still guaranteeing that an abandoned session's bodies do not sit on disk
/// indefinitely.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Operator override for [`DEFAULT_MAX_TOTAL_BYTES`], in bytes. Present because
/// disk pressure on this box is a known operational hazard and shrinking the
/// cap must not require a rebuild. A value that does not parse as a `u64` is
/// ignored with a warning rather than silently treated as zero.
pub const MAX_TOTAL_BYTES_ENV: &str = "QONTINUI_MCP_SPILL_MAX_BYTES";

/// Env var naming the ambient Claude Code session — the same id the session
/// row records (`session/mod.rs::ambient_claude_code_session_id`) and the
/// `prepare-commit-msg` hook stamps as a git trailer. Using it means a spill
/// directory can be joined back to the conversation that produced it.
const SESSION_ID_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Longest session-directory name we will create. Session ids are UUIDs in
/// practice; the bound exists so a poisoned env var cannot produce a path the
/// filesystem rejects.
const MAX_SESSION_KEY_LEN: usize = 64;

/// Prefix on every diagnostic this module writes. Distinct from the MCP
/// server's own `[wrappers-mcp]` so retention events are greppable on their own.
const LOG_PREFIX: &str = "[mcp-spill]";

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// The header of one spilled body — everything a sweep or a retrieval needs
/// without reading the body itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillRecord {
    /// [`SPILL_SCHEMA`] at write time.
    pub schema: u32,
    /// Stable locator, unique across sessions. A UUIDv7 in simple (32 lowercase
    /// hex chars) form: filename-safe by construction, and time-ordered, so a
    /// directory listing sorts chronologically.
    pub id: String,
    /// Session that owns the spill — the directory name, and the join key back
    /// to the conversation.
    pub session: String,
    /// MCP tool whose result this was. Recorded so a sweep (or an operator) can
    /// attribute disk use to a tool rather than to "the MCP server".
    pub tool: String,
    /// Media type of the body as the writer understood it (`application/json`
    /// for a serialized wrapper result, `text/plain` for subagent output and
    /// error messages). Advisory: it describes the body, it does not constrain
    /// how a reader slices it.
    pub content_type: String,
    /// Length of the body in bytes — the TRUE size the preview stands in for.
    pub byte_len: u64,
    /// Unix epoch millis at write time.
    pub created_at_ms: i64,
    /// Whether the body was an error result (`isError: true` on the wire). Kept
    /// so a retrieval can say what it is handing back.
    pub is_error: bool,
}

/// A byte range of a spilled body, with both endpoints snapped to UTF-8
/// character boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillSlice {
    /// The record this slice came from.
    pub record: SpillRecord,
    /// Byte offset the slice actually starts at — the requested offset rounded
    /// DOWN to a character boundary.
    pub offset: u64,
    /// Byte offset to pass as the next request's `offset`. Equals
    /// `record.byte_len` when the slice reached the end of the body.
    pub next_offset: u64,
    /// The slice itself. Always whole characters.
    pub text: String,
}

impl SpillSlice {
    /// Whether this slice ran to the end of the body.
    pub fn is_final(&self) -> bool {
        self.next_offset >= self.record.byte_len
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// A session's spill directory, plus the retention policy that bounds it.
///
/// One store per process: the MCP server's lifetime IS the AI client session's
/// lifetime, so "the session" and "this process" are the same scope. That is
/// also why the store carries no lock — `put` is the only mutator and
/// [`crate::fs_atomic`] already makes concurrent writers to the same path safe.
#[derive(Debug)]
pub struct SpillStore {
    /// `<app-data>/mcp-spill/` — the parent of every session's directory. The
    /// retention sweep works over this whole tree, not just our own session,
    /// because an abandoned session cannot sweep itself.
    root: PathBuf,
    /// `<root>/<session>/`.
    session_dir: PathBuf,
    session: String,
    max_total_bytes: u64,
    max_age: Duration,
    /// Spills belonging to THIS store's session that retention deleted.
    ///
    /// **Non-zero means real loss**: a locator this process already handed to a
    /// model now resolves to nothing, which turns a truthful preview into a
    /// dead pointer — strictly worse than the truncation this design rejects.
    /// Every drop is also warned on stderr, never silent. Deletions of *other*
    /// sessions' spills are ordinary garbage collection and are logged but not
    /// counted here: this process never issued those locators, so nobody is
    /// holding them.
    dropped_own_session: AtomicU64,
}

impl SpillStore {
    /// Open (creating if needed) the spill directory for the ambient session
    /// under the runner's app-data dir, with the default bounds.
    pub fn open_default() -> io::Result<Self> {
        let root = default_root().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "spill store: home directory is unresolvable",
            )
        })?;
        Self::open(root, &ambient_session_key())
    }

    /// Open (creating if needed) `<root>/<session>/` with the default bounds,
    /// honouring [`MAX_TOTAL_BYTES_ENV`].
    pub fn open(root: PathBuf, session: &str) -> io::Result<Self> {
        Self::open_with_bounds(root, session, max_total_bytes_from_env(), DEFAULT_MAX_AGE)
    }

    /// Open with explicit bounds. Tests use this to exercise retention without
    /// writing 64 MiB.
    pub fn open_with_bounds(
        root: PathBuf,
        session: &str,
        max_total_bytes: u64,
        max_age: Duration,
    ) -> io::Result<Self> {
        let session = sanitize_session_key(session);
        let session_dir = root.join(&session);
        fs::create_dir_all(&session_dir)?;
        // Restrict the DIRECTORIES before anything is written into them, so a
        // body is never even briefly readable inside a permissive parent. Both
        // levels: the root because it is ours alone, the session dir because on
        // Unix `create_dir_all` applied the ambient umask to it.
        if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&root) {
            eprintln!("{LOG_PREFIX} could not restrict {}: {e}", root.display());
        }
        if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&session_dir) {
            eprintln!(
                "{LOG_PREFIX} could not restrict {}: {e}",
                session_dir.display()
            );
        }
        Ok(Self {
            root,
            session_dir,
            session,
            max_total_bytes,
            max_age,
            dropped_own_session: AtomicU64::new(0),
        })
    }

    /// The sanitized session key — also the directory name.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The directory this store writes into.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Count of this session's spills that retention has deleted. See
    /// [`SpillStore::dropped_own_session`](struct.SpillStore.html#structfield.dropped_own_session)
    /// — non-zero means at least one issued locator is now a dead pointer.
    pub fn dropped_own_session(&self) -> u64 {
        self.dropped_own_session.load(Ordering::Relaxed)
    }

    /// Persist `body` and return its record.
    ///
    /// Retention runs FIRST, budgeted for the incoming body, so the bound holds
    /// after the write rather than one write late. A body that is on its own
    /// larger than the total cap is still written: the cap bounds accumulation,
    /// and refusing the write would lose the very result the spill exists to
    /// preserve.
    pub fn put(
        &self,
        tool: &str,
        content_type: &str,
        is_error: bool,
        body: &str,
    ) -> io::Result<SpillRecord> {
        self.sweep(body.len() as u64);

        let record = SpillRecord {
            schema: SPILL_SCHEMA,
            id: Uuid::now_v7().simple().to_string(),
            session: self.session.clone(),
            tool: tool.to_string(),
            content_type: content_type.to_string(),
            byte_len: body.len() as u64,
            created_at_ms: now_ms(),
            is_error,
        };

        let header = serde_json::to_string(&record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut buf = Vec::with_capacity(header.len() + 1 + body.len());
        buf.extend_from_slice(header.as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(body.as_bytes());

        crate::fs_atomic::atomic_write_owner_only(&self.path_for(&record.id), &buf)?;
        Ok(record)
    }

    /// Read `len` bytes of the body identified by `id`, starting at `offset`.
    ///
    /// Both endpoints are snapped to UTF-8 character boundaries — the start
    /// backwards, the end forwards — so the returned text is always whole
    /// characters and [`SpillSlice::next_offset`] is always a legal next
    /// `offset`. A `len` of at least 1 therefore always makes progress, even
    /// when it lands mid-character.
    pub fn read(&self, id: &str, offset: u64, len: u64) -> io::Result<SpillSlice> {
        let id = validated_id(id)?;
        let path = self.path_for(id);
        let file = fs::File::open(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "no spill '{id}' in session '{}' — it was never written, or retention \
                         has already swept it",
                        self.session
                    ),
                )
            } else {
                e
            }
        })?;

        let mut reader = BufReader::new(file);
        let mut header = String::new();
        let header_bytes = reader.read_line(&mut header)?;
        if header_bytes == 0 || !header.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("spill '{id}' has no header line"),
            ));
        }
        let record: SpillRecord = serde_json::from_str(header.trim_end_matches('\n'))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if record.schema != SPILL_SCHEMA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "spill '{id}' has schema {} — this build understands {SPILL_SCHEMA}",
                    record.schema
                ),
            ));
        }

        let body_offset = header_bytes as u64;
        let total = record.byte_len;
        let mut file = reader.into_inner();
        let (offset, next_offset, text) =
            read_char_aligned(&mut file, body_offset, total, offset, len)?;
        Ok(SpillSlice {
            record,
            offset,
            next_offset,
            text,
        })
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.session_dir.join(format!("{id}.{SPILL_EXTENSION}"))
    }

    // -----------------------------------------------------------------------
    // Retention
    // -----------------------------------------------------------------------

    /// Enforce both bounds across every session's directory, budgeting
    /// `incoming` bytes for the write that is about to happen.
    ///
    /// Best-effort by design: a sweep that cannot read a directory must not
    /// stop a result from being preserved, so every failure is warned and
    /// stepped over rather than propagated.
    fn sweep(&self, incoming: u64) {
        let mut files = self.collect();

        // Age bound first — it is the cheaper, less destructive of the two, and
        // whatever it reclaims the byte bound then does not have to.
        let now = SystemTime::now();
        files.retain(|f| {
            let age = now.duration_since(f.modified).unwrap_or_default();
            if age > self.max_age {
                !self.drop_file(f, "age")
            } else {
                true
            }
        });

        // Byte bound: oldest first. Since ids are UUIDv7 and the sort key is
        // mtime, "oldest" is also "least likely to still be referenced" — and
        // dead sessions sort before the live one, so another session's garbage
        // is evicted before our own live locators.
        files.sort_by_key(|f| f.modified);
        let mut total = files
            .iter()
            .map(|f| f.bytes)
            .fold(0u64, |a, b| a.saturating_add(b))
            .saturating_add(incoming);
        for f in &files {
            if total <= self.max_total_bytes {
                break;
            }
            if self.drop_file(f, "bytes") {
                total = total.saturating_sub(f.bytes);
            }
        }

        self.prune_empty_session_dirs();
    }

    /// Every `.spill` file under the root, across all sessions.
    fn collect(&self) -> Vec<SpillFile> {
        let mut out = Vec::new();
        let session_dirs = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "{LOG_PREFIX} cannot list {} for retention: {e}",
                    self.root.display()
                );
                return out;
            }
        };
        for session_entry in session_dirs.flatten() {
            let session_path = session_entry.path();
            if !session_path.is_dir() {
                continue;
            }
            let own_session = session_path == self.session_dir;
            let files = match fs::read_dir(&session_path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!(
                        "{LOG_PREFIX} cannot list {} for retention: {e}",
                        session_path.display()
                    );
                    continue;
                }
            };
            for file_entry in files.flatten() {
                let path = file_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some(SPILL_EXTENSION) {
                    continue;
                }
                let meta = match file_entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                out.push(SpillFile {
                    path,
                    bytes: meta.len(),
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                    own_session,
                });
            }
        }
        out
    }

    /// Delete one spill, accounting for it honestly. Returns whether the file
    /// is actually gone.
    fn drop_file(&self, f: &SpillFile, reason: &str) -> bool {
        if let Err(e) = fs::remove_file(&f.path) {
            eprintln!(
                "{LOG_PREFIX} retention could not delete {} ({reason}): {e}",
                f.path.display()
            );
            return false;
        }
        if f.own_session {
            let n = self.dropped_own_session.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "{LOG_PREFIX} WARNING dropped OWN-SESSION spill {} ({} bytes, reason={reason}) — \
                 any locator already handed to the model for it is now a dead pointer \
                 (dropped_own_session={n})",
                f.path.display(),
                f.bytes
            );
        } else {
            eprintln!(
                "{LOG_PREFIX} retention dropped {} ({} bytes, reason={reason})",
                f.path.display(),
                f.bytes
            );
        }
        true
    }

    /// Remove other sessions' directories once their last spill is gone.
    /// Without this the root accrues one empty directory per session forever —
    /// a slower leak than the bodies, but a leak. Our own directory is left
    /// alone: this process is still writing into it.
    fn prune_empty_session_dirs(&self) {
        let entries = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || path == self.session_dir {
                continue;
            }
            let is_empty = match fs::read_dir(&path) {
                Ok(mut d) => d.next().is_none(),
                Err(_) => false,
            };
            if is_empty {
                let _ = fs::remove_dir(&path);
            }
        }
    }
}

/// One spill file as the retention sweep sees it.
#[derive(Debug)]
struct SpillFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    own_session: bool,
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// `~/.qontinui/runner/mcp-spill/` — the runner's established app-data dir (the
/// session outbox, `session-restore/` and the port breadcrumb all live in its
/// parent). `None` when the home dir is unresolvable.
pub fn default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("runner").join(SPILL_DIR_NAME))
}

/// The ambient session key, sanitized.
///
/// Falls back to `pid-<pid>` outside Claude Code. Pid reuse across runs can make
/// two unrelated processes share a directory; that is harmless — ids are unique
/// within it, and the only consequence is that retention may evict a neighbour's
/// record instead of its own. It is NOT worth a lock file to prevent.
pub fn ambient_session_key() -> String {
    std::env::var(SESSION_ID_ENV)
        .ok()
        .map(|s| sanitize_session_key(s.trim()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid-{}", std::process::id()))
}

/// Reduce an arbitrary string to a safe single path segment. Anything outside
/// `[A-Za-z0-9._-]` becomes `_`, so `..` and separators can never escape the
/// root, and the result is truncated to [`MAX_SESSION_KEY_LEN`].
fn sanitize_session_key(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_SESSION_KEY_LEN)
        .collect();
    // `.` and `..` are legal under the character rule but are not names.
    if out.chars().all(|c| c == '.') {
        out.clear();
    }
    out
}

/// A spill id is 32 lowercase hex characters — the simple form of the UUIDv7
/// [`SpillStore::put`] mints. Validating rather than sanitizing is deliberate:
/// the id reaches us from a model-authored tool argument, and anything that is
/// not exactly an id we issued must be rejected, not coerced into some
/// neighbouring path.
fn validated_id(id: &str) -> io::Result<&str> {
    let ok = id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(id)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{id}' is not a spill id (expected 32 lowercase hex characters)"),
        ))
    }
}

/// Read `[offset, offset + len)` of the body, snapping the start back and the
/// end forward to UTF-8 character boundaries.
///
/// Snapping the END *forward* rather than truncating is what guarantees
/// progress: a caller asking for one byte in the middle of a 3-byte character
/// gets that whole character back and a `next_offset` past it, instead of an
/// empty slice and the same offset forever.
fn read_char_aligned(
    file: &mut fs::File,
    body_offset: u64,
    total: u64,
    offset: u64,
    len: u64,
) -> io::Result<(u64, u64, String)> {
    let offset = offset.min(total);
    let end = offset.saturating_add(len).min(total);
    if offset == total {
        return Ok((total, total, String::new()));
    }

    // A UTF-8 character is at most 4 bytes, so 3 bytes of context on each side
    // is always enough to find the enclosing boundary.
    let back = offset.min(3);
    let forward = (total - end).min(3);
    let read_start = offset - back;
    let read_len = (end + forward - read_start) as usize;

    file.seek(SeekFrom::Start(body_offset + read_start))?;
    let mut buf = vec![0u8; read_len];
    file.read_exact(&mut buf)?;

    let mut start_idx = back as usize;
    while start_idx > 0 && is_continuation(buf[start_idx]) {
        start_idx -= 1;
    }
    let mut end_idx = (end - read_start) as usize;
    while end_idx < buf.len() && is_continuation(buf[end_idx]) {
        end_idx += 1;
    }

    let text = String::from_utf8(buf[start_idx..end_idx].to_vec())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((
        read_start + start_idx as u64,
        read_start + end_idx as u64,
        text,
    ))
}

/// Is this byte a UTF-8 continuation byte (`10xxxxxx`) — i.e. NOT a character
/// boundary?
fn is_continuation(b: u8) -> bool {
    (b & 0b1100_0000) == 0b1000_0000
}

fn max_total_bytes_from_env() -> u64 {
    match std::env::var(MAX_TOTAL_BYTES_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "{LOG_PREFIX} ignoring {MAX_TOTAL_BYTES_ENV}={raw:?} ({e}) — using default \
                     {DEFAULT_MAX_TOTAL_BYTES}"
                );
                DEFAULT_MAX_TOTAL_BYTES
            }
        },
        _ => DEFAULT_MAX_TOTAL_BYTES,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> SpillStore {
        SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-a",
            1_000_000,
            DEFAULT_MAX_AGE,
        )
        .expect("open")
    }

    #[test]
    fn put_then_read_round_trips_the_whole_body() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let body = "line one\nline two\nline three\n";
        let rec = s
            .put("wrapper_v0__export_code", "application/json", false, body)
            .unwrap();

        assert_eq!(rec.schema, SPILL_SCHEMA);
        assert_eq!(rec.byte_len, body.len() as u64);
        assert_eq!(rec.tool, "wrapper_v0__export_code");
        assert_eq!(rec.session, "sess-a");
        assert!(!rec.is_error);

        let slice = s.read(&rec.id, 0, body.len() as u64).unwrap();
        assert_eq!(slice.text, body);
        assert_eq!(slice.offset, 0);
        assert_eq!(slice.next_offset, body.len() as u64);
        assert!(slice.is_final());
    }

    #[test]
    fn spill_file_is_header_line_then_verbatim_body() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let body = "{\n  \"a\": 1\n}";
        let rec = s.put("t", "application/json", false, body).unwrap();

        let raw = fs::read(s.session_dir().join(format!("{}.spill", rec.id))).unwrap();
        let nl = raw.iter().position(|b| *b == b'\n').unwrap();
        let header: SpillRecord = serde_json::from_slice(&raw[..nl]).unwrap();
        assert_eq!(header, rec);
        // The body is byte-for-byte what was handed in, newlines included.
        assert_eq!(&raw[nl + 1..], body.as_bytes());
    }

    #[test]
    fn spill_read_is_ranged_and_chains_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let body: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let rec = s.put("t", "text/plain", false, &body).unwrap();

        let mut seen = String::new();
        let mut offset = 0u64;
        loop {
            let slice = s.read(&rec.id, offset, 64).unwrap();
            if slice.text.is_empty() {
                break;
            }
            seen.push_str(&slice.text);
            offset = slice.next_offset;
            if slice.is_final() {
                break;
            }
        }
        assert_eq!(seen, body);
    }

    #[test]
    fn spill_read_snaps_to_character_boundaries_and_always_advances() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        // Every character is 3 bytes, so almost every offset is mid-character.
        let body = "日本語テキスト".repeat(20);
        let rec = s.put("t", "text/plain", false, &body).unwrap();

        // Requesting one byte from a mid-character offset must still return a
        // whole character and move the cursor past it.
        let slice = s.read(&rec.id, 1, 1).unwrap();
        assert_eq!(slice.offset, 0);
        assert_eq!(slice.next_offset, 3);
        assert_eq!(slice.text, "日");

        // Walking the whole body one byte at a time must reassemble it exactly.
        let mut seen = String::new();
        let mut offset = 0u64;
        while offset < rec.byte_len {
            let slice = s.read(&rec.id, offset, 1).unwrap();
            assert!(slice.next_offset > offset, "read must make progress");
            seen.push_str(&slice.text);
            offset = slice.next_offset;
        }
        assert_eq!(seen, body);
    }

    #[test]
    fn spill_read_past_the_end_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let rec = s.put("t", "text/plain", false, "abc").unwrap();
        let slice = s.read(&rec.id, 99, 10).unwrap();
        assert_eq!(slice.text, "");
        assert_eq!(slice.offset, 3);
        assert!(slice.is_final());
    }

    #[test]
    fn spill_read_rejects_a_non_id_rather_than_coercing_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        for bad in [
            "../../etc/passwd",
            "",
            "ABCDEF01234567890123456789ABCDEF",
            "0123456789abcdef",
        ] {
            let err = s.read(bad, 0, 10).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "id {bad:?}");
        }
    }

    #[test]
    fn spill_read_of_a_swept_id_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let rec = s.put("t", "text/plain", false, "gone soon").unwrap();
        fs::remove_file(s.session_dir().join(format!("{}.spill", rec.id))).unwrap();
        let err = s.read(&rec.id, 0, 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("retention"));
    }

    #[test]
    fn spill_retention_evicts_oldest_first_and_counts_own_session_loss() {
        let dir = tempfile::tempdir().unwrap();
        // Room for ~2 bodies of 1 KiB plus their headers.
        let s = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-a",
            2_600,
            DEFAULT_MAX_AGE,
        )
        .unwrap();

        let body = "x".repeat(1024);
        let first = s.put("t", "text/plain", false, &body).unwrap();
        // mtime has 1-second granularity on some filesystems; stamp the order
        // explicitly so "oldest first" is what is being tested, not the clock.
        age_file(&s.session_dir().join(format!("{}.spill", first.id)), 300);
        let second = s.put("t", "text/plain", false, &body).unwrap();
        age_file(&s.session_dir().join(format!("{}.spill", second.id)), 200);

        assert_eq!(s.dropped_own_session(), 0);
        let third = s.put("t", "text/plain", false, &body).unwrap();

        assert_eq!(s.dropped_own_session(), 1, "one eviction to make room");
        assert_eq!(
            s.read(&first.id, 0, 10).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "the OLDEST must be the one evicted"
        );
        assert!(s.read(&second.id, 0, 10).is_ok());
        assert!(s.read(&third.id, 0, 10).is_ok());
    }

    #[test]
    fn spill_retention_evicts_by_age_and_prunes_the_dead_session_dir() {
        let dir = tempfile::tempdir().unwrap();
        let old = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-old",
            u64::MAX,
            Duration::from_secs(60),
        )
        .unwrap();
        let stale = old.put("t", "text/plain", false, "stale").unwrap();
        let stale_path = old.session_dir().join(format!("{}.spill", stale.id));
        age_file(&stale_path, 3600);

        let live = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-new",
            u64::MAX,
            Duration::from_secs(60),
        )
        .unwrap();
        live.put("t", "text/plain", false, "fresh").unwrap();

        assert!(!stale_path.exists(), "age bound must evict the stale spill");
        assert!(
            !dir.path().join("sess-old").exists(),
            "an emptied session dir must be pruned"
        );
        assert!(dir.path().join("sess-new").exists());
        // Another session's garbage is not this session's loss.
        assert_eq!(live.dropped_own_session(), 0);
    }

    #[test]
    fn spill_body_larger_than_the_cap_is_still_stored() {
        let dir = tempfile::tempdir().unwrap();
        let s =
            SpillStore::open_with_bounds(dir.path().to_path_buf(), "sess-a", 100, DEFAULT_MAX_AGE)
                .unwrap();
        let body = "y".repeat(4096);
        let rec = s.put("t", "text/plain", false, &body).unwrap();
        // The cap bounds ACCUMULATION; refusing this write would lose the very
        // result the spill exists to preserve.
        assert_eq!(s.read(&rec.id, 0, 4096).unwrap().text, body);
    }

    #[test]
    fn spill_session_keys_cannot_escape_the_root() {
        // `.` survives the character rule (session ids may legitimately carry
        // one), but every separator becomes `_`, so the result is always ONE
        // path segment — `.._.._evil` names a sibling, not a parent.
        assert_eq!(sanitize_session_key("../../evil"), ".._.._evil");
        assert_eq!(sanitize_session_key("a/b\\c"), "a_b_c");
        // The two names that ARE traversals are rejected outright, which sends
        // `ambient_session_key` to its `pid-<pid>` fallback.
        assert_eq!(sanitize_session_key(".."), "");
        assert_eq!(sanitize_session_key("."), "");
        assert_eq!(
            sanitize_session_key("0198f2c1-1a2b-7c3d-8e4f-5a6b7c8d9e0f"),
            "0198f2c1-1a2b-7c3d-8e4f-5a6b7c8d9e0f"
        );
        assert_eq!(sanitize_session_key(&"z".repeat(200)).len(), 64);
    }

    #[test]
    fn spill_error_bodies_record_that_they_were_errors() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let rec = s.put("t", "text/plain", true, "boom").unwrap();
        assert!(rec.is_error);
        assert!(s.read(&rec.id, 0, 4).unwrap().record.is_error);
    }

    #[test]
    fn spill_record_json_is_one_line() {
        // The file format depends on it: the first newline terminates the
        // header. `serde_json::to_string` must never pretty-print.
        let rec = SpillRecord {
            schema: SPILL_SCHEMA,
            id: "0".repeat(32),
            session: "s".into(),
            tool: "t".into(),
            content_type: "text/plain".into(),
            byte_len: 7,
            created_at_ms: 1,
            is_error: false,
        };
        let s = serde_json::to_string(&rec).unwrap();
        assert!(!s.contains('\n'));
        assert_eq!(serde_json::from_str::<SpillRecord>(&s).unwrap(), rec);
    }

    /// Backdate a file's mtime by `secs` so retention ordering is deterministic
    /// rather than dependent on filesystem timestamp granularity.
    fn age_file(path: &Path, secs: u64) {
        let when = SystemTime::now() - Duration::from_secs(secs);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(when)).unwrap();
    }
}
