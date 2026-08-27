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

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version of the on-disk header. Bump only on a BREAKING shape change;
/// [`SpillStore::read`] refuses a record whose schema it does not know rather
/// than misparsing it.
pub const SPILL_SCHEMA: u32 = 1;

/// File extension for a spill record. The record arms of the sweep only ever
/// consider files with this suffix, so `fs_atomic`'s in-flight temp files
/// (`*.tmp.<pid>.<seq>.…`) are never mistaken for records — they are swept by
/// their own arm instead, keyed on [`TEMP_INFIX`].
pub const SPILL_EXTENSION: &str = "spill";

/// The substring that identifies an `fs_atomic` temp file for one of OUR
/// records.
///
/// [`crate::fs_atomic::atomic_write_owner_only`] names its temp
/// `{target_file_name}.tmp.{pid}.{seq}.{nanos}`, and our target file name is
/// always `<id>.spill` — so our temps, and only ours, contain `.spill.tmp.`.
/// Matching the writer's actual pattern rather than a generic `*.tmp` is what
/// keeps the sweep from deleting a file some other component happens to leave
/// under the root. `temp_infix_matches_fs_atomics_naming` pins it against
/// [`SPILL_EXTENSION`].
const TEMP_INFIX: &str = ".spill.tmp.";

/// Directory under the runner's app-data dir (`~/.qontinui/runner/`) that holds
/// every session's spills.
pub const SPILL_DIR_NAME: &str = "mcp-spill";

/// On-disk bound on the spills THIS PROCESS issued, enforced by
/// [`SpillStore::put`] before every write.
///
/// **Per-process, not root-wide.** Every `wrappers_mcp` bounds the records it
/// handed out and nothing else; several of them share one root as a matter of
/// course. That is what makes [`SpillStore::enforce_byte_bound`]'s target
/// reachable at all — see its doc for the root-wide budget this replaced and
/// the three ordinary situations that switched it off.
///
/// **The arithmetic, so a reader does not have to derive the fleet-wide
/// ceiling.** It is `cap × (servers that wrote within `max_age`)` — the live
/// ones plus any that have since exited, until [`DEFAULT_MAX_AGE`] reclaims the
/// leavers. At 8 MiB:
///
/// | servers within `max_age` | root-wide ceiling |
/// |---|---|
/// | 5 — this box's ordinary concurrency | 40 MiB |
/// | 20 | 160 MiB |
/// | 42 | 336 MiB — the scale of the 355 MB outbox that filled this disk |
///
/// and every one of those servers has to produce ~27 oversized results to reach
/// its own cap in the first place. Measured spill bodies are 90–300 KB, so
/// 8 MiB holds ~27 of the largest and ~90 of the smallest; against
/// `wrappers_mcp`'s 32 KiB spill threshold it is 256 of the smallest body that
/// can spill at all. A single 300 KB spill is 3.6% of it.
///
/// 64 MiB — `session/local_store.rs`'s number — was inherited here by mistake.
/// That is one GLOBAL outbox, so its cap *is* the ceiling; this one gets
/// multiplied by every concurrent session, and five sessions at 64 MiB is
/// 320 MiB — the field incident itself, not a margin below it.
pub const DEFAULT_MAX_OWN_BYTES: u64 = 8 * 1024 * 1024;

/// Age bound. A locator is only useful for as long as the conversation that was
/// handed it is still running, and an MCP server process lives exactly as long
/// as its AI client. A day is comfortably longer than any single session while
/// still guaranteeing that an abandoned session's bodies do not sit on disk
/// indefinitely.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Operator override for [`DEFAULT_MAX_OWN_BYTES`], in bytes. Present because
/// disk pressure on this box is a known operational hazard and shrinking the
/// cap must not require a rebuild.
///
/// **Monotonic: a smaller value always evicts more.** Not a coincidence — it
/// falls out of the budget being per-process, because the bytes measured
/// against the cap are exactly the bytes eviction is allowed to delete, so
/// lowering the cap can only enlarge the overage the sweep works off. The
/// root-wide budget this replaced inverted the knob: a smaller cap made a
/// neighbour's live bytes more likely to exceed the budget on their own, at
/// which point the arm deleted NOTHING — the single lever an operator has for
/// disk pressure switching enforcement off, and taking with it even the bytes
/// this process was entitled to reclaim.
///
/// A value that does not parse as a `u64` is ignored with a warning rather than
/// silently treated as zero.
pub const MAX_OWN_BYTES_ENV: &str = "QONTINUI_MCP_SPILL_MAX_BYTES";

/// Env var naming the ambient Claude Code session — the same id the session
/// row records (`session/mod.rs::ambient_claude_code_session_id`) and the
/// `prepare-commit-msg` hook stamps as a git trailer. Using it means a spill
/// directory can be joined back to the conversation that produced it.
const SESSION_ID_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Longest session-directory name we will create. Session ids are UUIDs in
/// practice; the bound exists so a poisoned env var cannot produce a path the
/// filesystem rejects.
const MAX_SESSION_KEY_LEN: usize = 64;

/// Hard ceiling on the header line [`SpillStore::read`] will buffer.
///
/// A real header is a single JSON object of eight short fields — a few hundred
/// bytes, and bounded by [`MAX_SESSION_KEY_LEN`], [`MAX_TOOL_LEN`] and
/// [`MAX_CONTENT_TYPE_LEN`]. 8 KiB is far above any of them and far below the
/// bodies this store holds, which is the separation that matters: it exists so
/// that a record with no newline in it cannot be read into memory in its
/// entirety before being rejected.
const MAX_HEADER_BYTES: u64 = 8 * 1024;

/// Longest [`SpillRecord::tool`] a header will carry.
///
/// **`tool` is the one header field that reaches [`SpillStore::put`] from
/// outside this process.** On `wrappers_mcp`'s unknown-tool arm it is the raw
/// `name` off the JSON-RPC frame, so a client picks its length. Unbounded, a
/// 40 KB name broke the format in two directions at once:
///
/// - the header line went past [`MAX_HEADER_BYTES`], so [`SpillStore::read`]
///   refused the record FOREVER — a locator that is a dead pointer from the
///   moment it is issued, and invisible to
///   [`SpillStore::dropped_own_session`] because nothing ever deleted it; and
/// - the preview interpolates the field (`wrappers_mcp::spill_preview`), so
///   the inline stand-in grew LARGER than the body it replaced — the cap
///   defeated on the very path that enforces it.
///
/// 256 bytes is an order of magnitude above any real tool name
/// (`wrapper_v0__export_code` is 23) and still leaves the worst-case header far
/// under the read bound — which the `const _: () = assert!` below checks at
/// COMPILE time rather than trusting this paragraph. A writer and a reader that
/// disagree about a bound they share is exactly the drift this module's
/// one-schema design exists to prevent.
pub const MAX_TOOL_LEN: usize = 256;

/// Longest [`SpillRecord::content_type`] a header will carry.
///
/// Every call site passes a literal today, so this is not reachable the way
/// [`MAX_TOOL_LEN`] is — but `wrappers_mcp::dispatch_read_spill` re-spills a
/// body whose `content_type` it read back OFF DISK, which puts the field one
/// hop from being as caller-controlled as `tool`. Bounding it is what makes the
/// header bound provable instead of argued.
const MAX_CONTENT_TYPE_LEN: usize = 128;

/// What one byte of a header field can cost once `serde_json` escapes it: a
/// control character becomes `\u00XX`, six bytes for one. [`MAX_SESSION_KEY_LEN`]
/// is exempt — [`sanitize_session_key`] has already reduced that field to
/// `[A-Za-z0-9._-]`, none of which escapes.
const JSON_ESCAPE_WORST_CASE: usize = 6;

/// The header's own weight with everything variable removed: the eight keys,
/// the punctuation, the 32-character id, and the two numbers at their widest
/// (`u64::MAX`, `i64::MIN`). Measured at ~192 bytes; 256 leaves room for a
/// field added later without silently eating the margin below.
const MAX_HEADER_FIXED_BYTES: usize = 256;

/// The writer's bounds must fit what the reader will buffer.
///
/// This is the invariant [`MAX_TOOL_LEN`] exists for, stated where the compiler
/// can enforce it: raising a field bound (or adding a field) past what
/// [`SpillStore::read`] accepts would otherwise mint records that are
/// unreadable from the instant they are written, and nothing at runtime would
/// say so.
const _: () = assert!(
    MAX_HEADER_FIXED_BYTES
        + MAX_SESSION_KEY_LEN
        + JSON_ESCAPE_WORST_CASE * (MAX_TOOL_LEN + MAX_CONTENT_TYPE_LEN)
        < MAX_HEADER_BYTES as usize
);

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
    /// attribute disk use to a tool rather than to "the MCP server". Bounded by
    /// [`MAX_TOOL_LEN`] at the writer, because on the unknown-tool arm this is
    /// a string the caller chose.
    pub tool: String,
    /// Media type of the body as the writer understood it (`application/json`
    /// for a serialized wrapper result, `text/plain` for subagent output and
    /// error messages). Advisory: it describes the body, it does not constrain
    /// how a reader slices it. Bounded by [`MAX_CONTENT_TYPE_LEN`].
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
/// One store per process. The one piece of mutable state it carries is
/// [`SpillStore::issued`] — the ids THIS process published — behind a mutex;
/// nothing else needs one, because `put` is the only mutator and
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
    /// Bytes THIS PROCESS's own issued records may occupy — see
    /// [`DEFAULT_MAX_OWN_BYTES`]. Nothing else under the root is measured
    /// against it, and nothing else is ever deleted to satisfy it.
    max_own_bytes: u64,
    max_age: Duration,
    /// Every spill id THIS process has published — the exact set of locators it
    /// has handed to a model.
    ///
    /// **Directory identity is not process identity.** A session directory is
    /// named for [`ambient_session_key`], and more than one `wrappers_mcp` can
    /// legitimately land on the same name: a resumed conversation keeps its
    /// `CLAUDE_CODE_SESSION_ID`, a restarted server reopens the same directory,
    /// and the `pid-<pid>` fallback repeats whenever a pid comes round again.
    /// So "in our directory" answers a different question from "we issued it",
    /// and it is the second one both the honesty counter and the eviction rule
    /// actually mean. Keyed on ids because that is what a locator IS; a few
    /// hundred 32-byte strings a day is not a memory concern.
    ///
    /// **The value is the ISSUE SEQUENCE, and it is what makes "oldest first"
    /// true.** Membership alone was not enough: the byte bound orders its
    /// candidates by filesystem mtime, and mtime cannot separate two records
    /// written inside one clock tick — see
    /// [`SpillStore::evictable_oldest_first`]. Nothing else on disk can. The
    /// header's `created_at_ms` is millisecond resolution, and a UUIDv7 id's
    /// ordered prefix is the same millisecond with a random tail, so both tie
    /// exactly where mtime does. This counter is the only exact record of the
    /// order this process handed locators out, and it is free to keep.
    issued: Mutex<HashMap<String, u64>>,
    /// Hands out the next [`SpillStore::issued`] sequence number. Monotone for
    /// the life of the process; never reused, and never reset by eviction (a
    /// dropped record stays issued — that is what keeps the honesty counter
    /// honest).
    next_issue_seq: AtomicU64,
    /// Spills THIS PROCESS issued that retention then deleted.
    ///
    /// **Non-zero means real loss**: a locator this process already handed to a
    /// model now resolves to nothing, which turns a truthful preview into a
    /// dead pointer — strictly worse than the truncation this design rejects.
    /// Every drop is also warned on stderr, never silent, and
    /// [`SpillStore::read`] consults the counter so a `NotFound` can say WHICH
    /// of its possible answers this one is.
    ///
    /// **The counter is honest by construction, because the byte bound both
    /// measures and evicts exactly the records this process issued** — see
    /// [`SpillStore::enforce_byte_bound`]. Several `wrappers_mcp` servers share
    /// one root; that is the ordinary case, not an edge. Evicting another
    /// server's record would manufacture exactly this dead pointer inside a
    /// process that cannot count it, cannot warn about it, and whose model then
    /// simply gets a bare `NotFound` — the deletion would be logged as ordinary
    /// GC in the wrong process's stderr. Reclaiming anyone else's disk is
    /// therefore left to the age bound, the one arm entitled to assume that
    /// nobody is still holding the locator.
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
    /// honouring [`MAX_OWN_BYTES_ENV`].
    pub fn open(root: PathBuf, session: &str) -> io::Result<Self> {
        Self::open_with_bounds(root, session, max_own_bytes_from_env(), DEFAULT_MAX_AGE)
    }

    /// Open with explicit bounds. Tests use this to exercise retention without
    /// writing 8 MiB.
    pub fn open_with_bounds(
        root: PathBuf,
        session: &str,
        max_own_bytes: u64,
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
            max_own_bytes,
            max_age,
            issued: Mutex::new(HashMap::new()),
            next_issue_seq: AtomicU64::new(0),
            dropped_own_session: AtomicU64::new(0),
        })
    }

    /// The ids this process has published, each mapped to its issue sequence.
    /// A poisoned lock still holds usable data — the guarded value is a plain
    /// `HashMap` and no writer can leave it half-updated — and refusing to read
    /// it would blind the honesty counter for the rest of the process's life,
    /// so the poison is stepped over rather than propagated.
    fn issued(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        self.issued.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sanitized session key — also the directory name.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The directory this store writes into.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// The bytes this process's own spills may occupy. Exposed so the server
    /// can state it at startup: the fleet-wide ceiling is this number times the
    /// number of servers running, and [`MAX_OWN_BYTES_ENV`] can move it, so an
    /// operator who cannot see the effective value cannot compute the ceiling
    /// either.
    pub fn max_own_bytes(&self) -> u64 {
        self.max_own_bytes
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
    /// larger than the cap is still written: the cap bounds accumulation, and
    /// refusing the write would lose the very result the spill exists to
    /// preserve.
    ///
    /// `tool` and `content_type` are bounded HERE, at the writer, rather than
    /// trusted from the caller — see [`MAX_TOOL_LEN`] for what an unbounded
    /// `tool` did to both the header and the preview. The body is not bounded:
    /// storing it whole is the entire point.
    pub fn put(
        &self,
        tool: &str,
        content_type: &str,
        is_error: bool,
        body: &str,
    ) -> io::Result<SpillRecord> {
        self.sweep(body.len() as u64);
        self.ensure_session_dir()?;

        let record = SpillRecord {
            schema: SPILL_SCHEMA,
            id: Uuid::now_v7().simple().to_string(),
            session: self.session.clone(),
            tool: bounded_tool_name(tool).into_owned(),
            content_type: truncate_on_char_boundary(content_type, MAX_CONTENT_TYPE_LEN)
                .into_owned(),
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
        // Only a record that actually reached disk is a locator this process
        // will hand out, so registration happens after the write, not before:
        // the map has to mean "issued", not "attempted". The sequence is taken
        // here rather than at record-construction time for the same reason —
        // it must count locators handed out, not writes attempted.
        let seq = self.next_issue_seq.fetch_add(1, Ordering::Relaxed);
        self.issued().insert(record.id.clone(), seq);
        Ok(record)
    }

    /// Make sure this store's session directory exists, recreating it if
    /// anything removed it since [`SpillStore::open`].
    ///
    /// Not paranoia, and not merely defensive. `open` is the ONLY other place
    /// that creates this directory, and
    /// [`crate::fs_atomic::atomic_write_owner_only`] does not create its
    /// parent — so a directory that disappears once makes every later
    /// oversized result in this process fail to spill and go back to the model
    /// whole and uncapped, permanently, silently reinstating exactly the
    /// problem this store exists to solve. A concurrent server's retention
    /// sweep is one way that happens (see
    /// [`SpillStore::prune_idle_session_dirs`], which now guards against it
    /// from the other side); an operator clearing disk is another. Recreating
    /// here makes the store self-heal against ALL of them rather than against
    /// one enumerated cause.
    fn ensure_session_dir(&self) -> io::Result<()> {
        if self.session_dir.is_dir() {
            return Ok(());
        }
        fs::create_dir_all(&self.session_dir)?;
        // A recreated directory carries the ambient umask / inherited ACL, so
        // it gets the same owner-only hardening `open` applies — BOTH levels,
        // because `create_dir_all` recreates `<root>` too when the whole tree
        // was removed, and hardening only the leaf would leave every session's
        // directory NAME (the join key back to a conversation) world-readable.
        // The bodies themselves are safe either way — `atomic_write_owner_only`
        // writes them `0600` — so this closes a naming leak, not a content one.
        if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&self.root) {
            eprintln!(
                "{LOG_PREFIX} could not restrict {}: {e}",
                self.root.display()
            );
        }
        if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&self.session_dir) {
            eprintln!(
                "{LOG_PREFIX} could not restrict {}: {e}",
                self.session_dir.display()
            );
        }
        Ok(())
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
                io::Error::new(io::ErrorKind::NotFound, self.not_found_message(id))
            } else {
                e
            }
        })?;

        let mut reader = BufReader::new(file);
        let mut header = String::new();
        // Bounded read: a corrupt or truncated record with no newline in it at
        // all would otherwise make `read_line` pull the ENTIRE file into this
        // `String` — a body the whole store exists to keep out of memory —
        // only for the check below to reject it. `Take` stops at a length no
        // legitimate header can reach, and a header that hits the bound has no
        // trailing newline, so it falls into the same rejection.
        let header_bytes = (&mut reader)
            .take(MAX_HEADER_BYTES)
            .read_line(&mut header)?;
        if header_bytes == 0 || !header.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("spill '{id}' has no header line in its first {MAX_HEADER_BYTES} bytes"),
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

    /// Explain a missing record using what this store actually knows.
    ///
    /// "It was never written, or retention swept it" is an ambiguity the store
    /// can resolve for itself, from two facts it owns: whether it ever ISSUED
    /// this id ([`SpillStore::issued`]) and how many of its own it has since
    /// lost ([`SpillStore::dropped_own_session`]). Both are needed. The counter
    /// alone used to be read as covering every record in our directory, so a
    /// stale id from an earlier server under the same session key was answered
    /// with "retention dropped it" — the same class of lie the counter exists
    /// to prevent, pointed the other way. Saying which answer this is, and only
    /// when the store can actually tell, is the point of keeping either fact —
    /// a number nobody reads is not a safeguard, and one that is read wrongly
    /// is worse than none.
    fn not_found_message(&self, id: &str) -> String {
        let dropped = self.dropped_own_session();
        let issued = self.issued().contains_key(id);
        match (issued, dropped) {
            (true, 1..) => format!(
                "no spill '{id}' in session '{}' — this server issued that locator and retention \
                 has already dropped {dropped} of its own spills, so this was most likely one of \
                 them",
                self.session
            ),
            (true, 0) => format!(
                "no spill '{id}' in session '{}' — this server issued that locator and retention \
                 here has dropped none of its own spills, so the record was removed out of band",
                self.session
            ),
            (false, 1..) => format!(
                "no spill '{id}' in session '{}' — this server never issued that locator (the \
                 session key is shared with any earlier server that used it), and retention here \
                 has dropped {dropped} of its OWN spills, none of them this one",
                self.session
            ),
            (false, 0) => format!(
                "no spill '{id}' in session '{}' — this server never issued that locator and \
                 retention here has dropped none of its own spills, so it was never written by \
                 this process (a stale id from another session, or a record removed out of band)",
                self.session
            ),
        }
    }

    // -----------------------------------------------------------------------
    // Retention
    // -----------------------------------------------------------------------

    /// Enforce both bounds, budgeting `incoming` bytes for the write that is
    /// about to happen.
    ///
    /// The two arms have deliberately different reach. The age bound works
    /// across every session's directory — it is the ONLY arm that touches
    /// anything this process did not issue, and it is entitled to, because past
    /// `max_age` the conversation that was handed the locator is long over and
    /// there is nobody left to hand a dead pointer to. The byte bound works on
    /// this process's own records alone (see
    /// [`SpillStore::enforce_byte_bound`]). Age runs first: it is the cheaper,
    /// less destructive of the two, and whatever it reclaims of ours the byte
    /// bound then does not have to.
    ///
    /// Best-effort by design: a sweep that cannot read a directory must not
    /// stop a result from being preserved, so every failure is warned and
    /// stepped over rather than propagated.
    fn sweep(&self, incoming: u64) {
        let scan = self.collect();
        let now = SystemTime::now();
        let mut files = scan.files;

        files.retain(|f| {
            let age = now.duration_since(f.modified).unwrap_or_default();
            if age > self.max_age {
                !self.drop_file(f, f.kind.age_reason())
            } else {
                true
            }
        });

        self.enforce_byte_bound(&files, incoming);
        self.prune_idle_session_dirs(&scan.dirs, now);
    }

    /// The records THIS PROCESS issued, in the order
    /// [`SpillStore::enforce_byte_bound`] must evict them: oldest first, ties
    /// broken by the order the locators were actually handed out.
    ///
    /// **mtime cannot order these on its own, and a stable sort hides it.**
    /// `modified` comes from the filesystem, and an OS stamps inode times from
    /// a coarse clock — writes inside one tick get an identical timestamp
    /// whatever precision the filesystem stores. Sorting on `modified` alone is
    /// a *stable* sort, so every such tie silently fell through to the order
    /// [`SpillStore::collect`] happened to walk the directory in, and
    /// `fs::read_dir` guarantees no order at all (on ext4 with `dir_index` it
    /// is filename-hash order over random UUIDv7 ids). "Oldest first" then
    /// means "arbitrary", and the arm evicts a locator NEWER than one it
    /// spares — the one likelier to still be live in the model's context, and
    /// so the more expensive dead pointer of the two.
    ///
    /// Nothing on disk fixes this. `SpillRecord::created_at_ms` is millisecond
    /// resolution and the ordered prefix of a UUIDv7 id is the same millisecond
    /// with a random tail, so both tie exactly where mtime does. The only exact
    /// answer is in memory: [`SpillStore::issued`]'s sequence, which is why
    /// that map stores one.
    ///
    /// mtime stays PRIMARY so behaviour is unchanged wherever it already
    /// separates two records — including for a record whose mtime moved after
    /// it was issued. The sequence only decides ties, which is precisely where
    /// the old key had nothing to say.
    ///
    /// Filtering before sorting is also what keeps `issue_seq`'s `u64::MAX`
    /// sentinel out of the comparator: a record that is not ours has no
    /// sequence, and is neither counted nor evictable
    /// ([`SpillFile::ours`]).
    ///
    /// Kept as its own function so the ordering can be tested directly, without
    /// a filesystem and without depending on a clock tick to produce the tie —
    /// the failure mode above is unreachable from a test that writes real files
    /// and hopes.
    fn evictable_oldest_first(files: &[SpillFile]) -> Vec<&SpillFile> {
        let mut ours: Vec<&SpillFile> = files.iter().filter(|f| f.ours).collect();
        ours.sort_by_key(|f| (f.modified, f.issue_seq));
        ours
    }

    /// Enforce the own-bytes bound, budgeting `incoming` for the write that is
    /// about to happen.
    ///
    /// **The rule: the records THIS PROCESS issued are both what is MEASURED
    /// and what is EVICTED.** Nothing else under the root is either — not a
    /// neighbour's records, live or stale, not a temp. Oldest first within that
    /// set.
    ///
    /// That one sentence is the whole design, and the fact that the measured
    /// set and the candidate set are the *same* set is what buys the property
    /// that matters: **the target is reachable by construction.** Evicting
    /// everything we own takes our total to zero, which is under any budget. So
    /// there is nothing for a refusal guard to refuse, and no arithmetic in
    /// which the arm can decide it is beaten.
    ///
    /// The rule this replaced measured every byte under the root against a cap
    /// only our own records could pay — a category error, and one whose failure
    /// mode was fleet-normal rather than exotic. Five concurrent sessions each
    /// hold about a fifth of the root, so each computes "unevictable ≈ 0.8 ×
    /// total"; once the root passes the cap EVERY server refuses at once and
    /// the root is bounded by `max_age` alone. A restart reproduced it
    /// single-handed, because [`SpillStore::issued`] is in-memory: a resumed
    /// conversation's own predecessor's records are foreign-and-live to it. And
    /// [`MAX_OWN_BYTES_ENV`] inverted — lowering the cap made the refusal more
    /// likely, so the one lever for disk pressure turned enforcement off.
    ///
    /// Before that it was "oldest first across every session", on the reasoning
    /// that mtime order approximates liveness order. It does not. Several
    /// `wrappers_mcp` servers share this root as a matter of course, and
    /// oldest-first *systematically prefers* the longest-running live session's
    /// records: they are the oldest files on disk precisely because that
    /// session is still going. Evicting one destroys a locator another process
    /// has already handed to its model, in a process that cannot count the loss
    /// ([`SpillStore::dropped_own_session`] stays zero there), does not warn
    /// about it (the warning lands in OUR stderr, labelled as ordinary GC), and
    /// whose model then gets a bare `NotFound` — the exact dead pointer the
    /// counter exists to make impossible. "Issued by us" rather than "in our
    /// directory" is what makes the protection hold even when two servers share
    /// a session key — see [`SpillStore::issued`].
    ///
    /// **What the per-process budget costs, stated plainly.** No process
    /// reclaims another's disk here, so a server that has exited keeps its
    /// bytes until the age arm takes them, and the root is bounded at
    /// `max_own_bytes × (servers that wrote within max_age)` rather than at one
    /// flat number. That is a ceiling — predictable, and computed for the
    /// default in [`DEFAULT_MAX_OWN_BYTES`] — where the root-wide cap was a
    /// number that read like one and was not enforced. Exceeding a disk budget
    /// is recoverable and observable; an uncounted dead pointer is a silent lie
    /// told to a model.
    fn enforce_byte_bound(&self, files: &[SpillFile], incoming: u64) {
        if incoming >= self.max_own_bytes {
            // A body larger than the whole cap is stored anyway — `put`'s doc
            // says why. Budgeting for it leaves no budget at all (and would
            // underflow the subtraction below), so the loop would empty this
            // process's store for a target the write itself puts back out of
            // reach the moment it lands. The cap bounds ACCUMULATION; a single
            // write that exceeds it on its own is outside what it can express,
            // and the age arm has already run.
            eprintln!(
                "{LOG_PREFIX} incoming body is {incoming} bytes against a {} byte cap — skipping \
                 byte-bound eviction rather than emptying this server's own store for a target \
                 the write itself would immediately exceed",
                self.max_own_bytes
            );
            return;
        }
        // Budget for the incoming write by shrinking the target, not by adding
        // it to the total: the two are equivalent arithmetic, but a target
        // that stays constant is one the loop can actually be shown to reach.
        let budget = self.max_own_bytes - incoming;
        // `SpillFile::ours` is the entire predicate, on both sides of the
        // ledger: these are the bytes counted, and these are the files
        // deletable. See this function's doc for why they must be one set.
        let ours = Self::evictable_oldest_first(files);
        let mut total = ours
            .iter()
            .map(|f| f.bytes)
            .fold(0u64, |a, b| a.saturating_add(b));
        if total <= budget {
            return;
        }

        for f in ours {
            if total <= budget {
                return;
            }
            if self.drop_file(f, "bytes") {
                total = total.saturating_sub(f.bytes);
            }
        }
        if total > budget {
            // Reachable only when a deletion FAILED. Evicting every record we
            // issued takes the measured total to zero, so a remainder here is a
            // filesystem error, never a policy decision — worth saying rather
            // than assuming, since the two want different responses.
            eprintln!(
                "{LOG_PREFIX} {total} bytes of our own remain against a {budget} byte budget \
                 after evicting every record this server issued — some deletion above failed; \
                 its error is logged with the file that would not go"
            );
        }
    }

    /// Every spill record and every stray `fs_atomic` temp under the root,
    /// across all sessions, plus each session directory's mtime as it stood
    /// BEFORE this sweep touched anything.
    ///
    /// The pre-sweep mtimes are what [`SpillStore::prune_idle_session_dirs`]
    /// judges idleness by; reading them here rather than at prune time is what
    /// stops our own deletions from looking like somebody else's activity.
    fn collect(&self) -> SweepScan {
        let mut scan = SweepScan::default();
        // Held for the whole walk: nothing inside it locks, and `put` — the
        // only writer — runs on the same thread as the sweep it triggers.
        let issued = self.issued();
        let session_dirs = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "{LOG_PREFIX} cannot list {} for retention: {e}",
                    self.root.display()
                );
                return scan;
            }
        };
        for session_entry in session_dirs.flatten() {
            let session_path = session_entry.path();
            if !session_path.is_dir() {
                continue;
            }
            let own_session = session_path == self.session_dir;
            scan.dirs.push(SessionDir {
                modified: session_entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH),
                path: session_path.clone(),
                own_session,
            });
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
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                let kind = if path.extension().and_then(|e| e.to_str()) == Some(SPILL_EXTENSION) {
                    SpillKind::Record
                } else if name.contains(TEMP_INFIX) {
                    SpillKind::Temp
                } else {
                    continue;
                };
                let meta = match file_entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                // A record's file stem IS its id (`path_for`), so this asks
                // exactly the question the eviction rule and the honesty
                // counter mean: did WE hand this locator out? A temp has no
                // published id and so is never ours by this test — which is
                // correct, since nothing was ever issued for it. A hit also
                // carries WHEN we handed it out, which is what orders eviction
                // once mtime has run out of resolution.
                let issue_seq = if kind == SpillKind::Record {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|id| issued.get(id).copied())
                } else {
                    None
                };
                scan.files.push(SpillFile {
                    path,
                    bytes: meta.len(),
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                    ours: issue_seq.is_some(),
                    issue_seq: issue_seq.unwrap_or(u64::MAX),
                    kind,
                });
            }
        }
        scan
    }

    /// Delete one file, accounting for it honestly. Returns whether it is
    /// actually gone.
    ///
    /// Only a record THIS PROCESS issued counts as loss. A temp of ours was
    /// never published, and a record in our directory written by an earlier
    /// server under the same session key was never issued BY US — counting
    /// either would make the honesty counter, and the `NotFound` message that
    /// cites it, lie in the other direction.
    fn drop_file(&self, f: &SpillFile, reason: &str) -> bool {
        if let Err(e) = fs::remove_file(&f.path) {
            eprintln!(
                "{LOG_PREFIX} retention could not delete {} ({reason}): {e}",
                f.path.display()
            );
            return false;
        }
        if f.ours {
            let n = self.dropped_own_session.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "{LOG_PREFIX} WARNING dropped a spill THIS SERVER ISSUED {} ({} bytes, \
                 reason={reason}) — any locator already handed to the model for it is now a dead \
                 pointer (dropped_own_session={n})",
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

    /// Remove another session's directory once its last spill is gone AND it
    /// has been idle for longer than `max_age`. Without this the root accrues
    /// one empty directory per session forever — a slower leak than the
    /// bodies, but a leak. Our own directory is always left alone: this
    /// process is still writing into it.
    ///
    /// **The idle test is not a refinement, it is the whole safety property.**
    /// Every session's directory is created when its server starts and holds
    /// no `.spill` file until that session's first oversized result — so
    /// "empty" is the NORMAL state of a perfectly live neighbour, not evidence
    /// of a dead one. Pruning on emptiness alone deleted a running process's
    /// directory out from under it, and since
    /// [`crate::fs_atomic::atomic_write_owner_only`] does not create its
    /// parent, every subsequent spill in that process failed and its oversized
    /// bodies went back to the model whole — the sweep silently reinstating
    /// the exact problem the store exists to fix.
    ///
    /// A directory's mtime moves whenever a child is added or removed, so
    /// "untouched for `max_age`" is the one honest signal that nobody is
    /// writing there. The mtimes come from [`SpillStore::collect`], i.e. from
    /// before this sweep deleted anything, so a directory we have just emptied
    /// ourselves is still judged on the activity that preceded us.
    /// ([`SpillStore::ensure_session_dir`] is the other half of the fix: a
    /// prune that is nonetheless wrong is now survivable rather than terminal.)
    ///
    /// A neighbour that has just passed its own `ensure_session_dir` but has
    /// not yet created a temp is still racing us between the emptiness check
    /// and the `remove_dir` below, and no ordering here closes that: the
    /// filesystem offers no "remove if still empty". It is left as a race
    /// deliberately, because the whole cost of losing it is bounded and
    /// self-healing — that neighbour's one in-flight `put` fails, returns its
    /// body whole (loud, truthful, uncapped for one result), and its next `put`
    /// recreates the directory. The alternative, a lock file under the root, is
    /// a new failure mode of its own for a race this narrow.
    fn prune_idle_session_dirs(&self, dirs: &[SessionDir], now: SystemTime) {
        for dir in dirs {
            if dir.own_session {
                continue;
            }
            if now.duration_since(dir.modified).unwrap_or_default() <= self.max_age {
                continue;
            }
            let is_empty = match fs::read_dir(&dir.path) {
                Ok(mut d) => d.next().is_none(),
                Err(_) => false,
            };
            if is_empty {
                let _ = fs::remove_dir(&dir.path);
            }
        }
    }
}

/// What the retention sweep found under the root in one pass.
#[derive(Debug, Default)]
struct SweepScan {
    files: Vec<SpillFile>,
    dirs: Vec<SessionDir>,
}

/// One session directory as the sweep first saw it.
#[derive(Debug)]
struct SessionDir {
    path: PathBuf,
    /// mtime read BEFORE the sweep deleted anything — see
    /// [`SpillStore::prune_idle_session_dirs`].
    modified: SystemTime,
    own_session: bool,
}

/// What kind of file the sweep is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpillKind {
    /// A published `<id>.spill` record. May still be referenced by a locator.
    Record,
    /// An `fs_atomic` temp (see [`TEMP_INFIX`]). Either a write happening right
    /// now, or the full-body-sized remains of one a process kill interrupted —
    /// `atomic_write_owner_only` cleans up only on a returned `Err`, so a
    /// killed process leaves its temp behind. Nothing else in this module can
    /// see such a file, which is how they used to accumulate without bound:
    /// invisible to both retention arms AND pinning their dead session's
    /// directory against pruning.
    Temp,
}

impl SpillKind {
    /// Reason string for an age-bound deletion, so the two classes are
    /// greppable apart in the log.
    fn age_reason(self) -> &'static str {
        match self {
            SpillKind::Record => "age",
            SpillKind::Temp => "orphan-temp",
        }
    }
}

/// One file under the spill root as the retention sweep sees it.
#[derive(Debug)]
struct SpillFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    /// A published record whose id THIS process issued — not merely a file in
    /// our session directory. See [`SpillStore::issued`] for why the two are
    /// different questions.
    ///
    /// **This flag IS the byte bound**, both halves of it: what it marks is
    /// what counts against `max_own_bytes` and what may be deleted to get back
    /// under it ([`SpillStore::enforce_byte_bound`]). [`SpillStore::collect`]
    /// can only set it on a [`SpillKind::Record`] — a temp has no published id
    /// — so temps and every foreign record fall outside both halves together,
    /// which is exactly the symmetry that keeps the bound's target reachable.
    ours: bool,
    /// This record's position in the sequence of locators THIS process handed
    /// out ([`SpillStore::issued`]), or `u64::MAX` when it is not ours.
    ///
    /// The sentinel is never compared: [`SpillStore::evictable_oldest_first`]
    /// filters on `ours` before it sorts, so only real sequences reach the
    /// comparator. `u64::MAX` rather than `0` so that a future reader who sorts
    /// without filtering gets foreign records last instead of evicted first.
    issue_seq: u64,
    kind: SpillKind,
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
/// Falls back to `pid-<pid>` outside Claude Code. **A key is not a process
/// identity and never was.** Three ordinary things put two `wrappers_mcp`
/// processes on one directory: a resumed conversation keeps its
/// `CLAUDE_CODE_SESSION_ID`, a restarted server reopens the same key, and a
/// reused pid repeats the fallback.
///
/// That is survivable, but not for the reason this doc used to give. It claimed
/// a collision was harmless because "retention may evict a neighbour's record
/// instead of its own" — which under the current eviction rule is exactly
/// backwards: reading a stranger's records as our own would make them
/// *maximally* evictable (the neighbour protection would not apply) and would
/// count their loss as ours. What actually makes a collision harmless is that
/// neither the eviction rule nor the honesty counter keys on the directory at
/// all: both key on [`SpillStore::issued`], the ids this process published. A
/// colliding stranger's records are therefore treated exactly like any other
/// neighbour's — not counted against our cap, not evictable by the byte arm,
/// not counted as our loss, reclaimed by the age arm alone — and ids stay
/// unique within the directory regardless. Still NOT worth a lock file to
/// prevent.
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

/// Bound a tool name to what a header — and therefore a preview — may carry.
///
/// **Public because writer and reader must not disagree on a bound they
/// share.** [`SpillStore::put`] applies it to [`SpillRecord::tool`], and
/// `wrappers_mcp` applies the same function to the same string on the paths
/// that render it WITHOUT going through a record: the unknown-tool error
/// message, and the `tool=` field of the stderr metric lines. Two copies of the
/// rule would drift; one function cannot.
pub fn bounded_tool_name(tool: &str) -> Cow<'_, str> {
    truncate_on_char_boundary(tool, MAX_TOOL_LEN)
}

/// Cut `value` to at most `max` BYTES on a character boundary, marking the cut.
///
/// Marked rather than silently shortened, for the reason the preview is marked
/// partial: a reader must never have to infer from length alone that it is
/// holding a fragment. The ellipsis is counted inside `max`, so the result is
/// never longer than asked for — which is what the header bound rests on.
fn truncate_on_char_boundary(value: &str, max: usize) -> Cow<'_, str> {
    /// Three bytes, and reserved out of `max` rather than added to it.
    const ELLIPSIS: &str = "…";
    if value.len() <= max {
        return Cow::Borrowed(value);
    }
    let mut cut = max.saturating_sub(ELLIPSIS.len());
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    Cow::Owned(format!("{}{ELLIPSIS}", &value[..cut]))
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

fn max_own_bytes_from_env() -> u64 {
    match std::env::var(MAX_OWN_BYTES_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "{LOG_PREFIX} ignoring {MAX_OWN_BYTES_ENV}={raw:?} ({e}) — using default \
                     {DEFAULT_MAX_OWN_BYTES}"
                );
                DEFAULT_MAX_OWN_BYTES
            }
        },
        _ => DEFAULT_MAX_OWN_BYTES,
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
    fn spill_eviction_order_is_issue_order_when_mtimes_tie() {
        // The defect this pins: `sort_by_key(|f| f.modified)` is STABLE, so
        // every mtime tie fell through to whatever order `collect` happened to
        // walk the directory in — and `fs::read_dir` promises none. On CI that
        // made the byte bound evict a NEWER record than one it spared, and it
        // reddened `main` at random through the `wrappers_mcp` test that reads
        // the resulting error message
        // (`read_spill_says_when_retention_is_the_reason_a_locator_died`,
        // run 33021749396 attempt 2, 2026-08-27).
        //
        // Asserting it here rather than through the filesystem is the whole
        // point. A test that writes three real files can only produce the tie
        // when the clock cooperates, so it detects the bug at whatever rate the
        // host's timestamp granularity allows — which is exactly the flake. One
        // shared `modified` and a REVERSED input order makes it deterministic:
        // under the old key a stable sort returns the input order unchanged, so
        // this fails every time; under the fix it returns issue order.
        let tied = SystemTime::now();
        let mk = |seq: u64, ours: bool| SpillFile {
            path: PathBuf::from(format!("{seq}.spill")),
            bytes: 1_204,
            modified: tied,
            ours,
            issue_seq: if ours { seq } else { u64::MAX },
            kind: SpillKind::Record,
        };

        // Reversed on purpose — newest issued first.
        let files = vec![mk(2, true), mk(1, true), mk(0, true)];
        let order: Vec<u64> = SpillStore::evictable_oldest_first(&files)
            .iter()
            .map(|f| f.issue_seq)
            .collect();
        assert_eq!(
            order,
            vec![0, 1, 2],
            "with mtimes tied, eviction order must be the order the locators \
             were issued — not the order the directory was walked in"
        );

        // And a record we did not issue is neither counted nor evictable, so it
        // must not reach the comparator at all (its `issue_seq` is a sentinel).
        let with_foreign = vec![mk(1, true), mk(0, false), mk(0, true)];
        let kept = SpillStore::evictable_oldest_first(&with_foreign);
        assert_eq!(kept.len(), 2, "a foreign record is not evictable");
        assert!(
            kept.iter().all(|f| f.ours),
            "the sentinel seq must never sort a foreign record into the queue"
        );
    }

    #[test]
    fn spill_byte_bound_evicts_the_first_issued_when_every_mtime_is_equal() {
        // The end-to-end half of the test above: same property, but through
        // `put` and the real sweep, so a future refactor that stops threading
        // the issue sequence into `collect` is caught even if it leaves
        // `evictable_oldest_first` itself correct.
        let dir = tempfile::tempdir().unwrap();
        let s = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-tie",
            2_600,
            DEFAULT_MAX_AGE,
        )
        .unwrap();

        let body = "x".repeat(1024);
        let first = s.put("t", "text/plain", false, &body).unwrap();
        let second = s.put("t", "text/plain", false, &body).unwrap();
        assert_eq!(s.dropped_own_session(), 0, "two records still fit");

        // Collapse the clock: give both records the SAME mtime, which is what
        // a fast host produces on its own and what the old ordering could not
        // resolve. Unlike `age_file`'s staggered stamps, this does not hand the
        // sort the answer — it removes it.
        let tied = SystemTime::now() - Duration::from_secs(60);
        for id in [&first.id, &second.id] {
            filetime::set_file_mtime(
                s.session_dir().join(format!("{id}.spill")),
                filetime::FileTime::from_system_time(tied),
            )
            .unwrap();
        }

        let third = s.put("t", "text/plain", false, &body).unwrap();

        assert_eq!(s.dropped_own_session(), 1, "one eviction to make room");
        assert_eq!(
            s.read(&first.id, 0, 10).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "the FIRST-ISSUED must be the one evicted, even with mtimes tied"
        );
        assert!(s.read(&second.id, 0, 10).is_ok());
        assert!(s.read(&third.id, 0, 10).is_ok());
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
        // The DIRECTORY has to look dead too, not just its contents: an idle
        // directory is what the prune keys on now, because an empty one is the
        // normal state of a live neighbour that has not spilled yet.
        age_file(old.session_dir(), 3600);

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
    fn spill_retention_spares_a_live_neighbour_that_has_not_spilled_yet() {
        // The ordinary case on this box: two `wrappers_mcp` servers, one root.
        // Session B has opened but has not produced an oversized result yet —
        // the state EVERY session is in until its first spill — so its
        // directory is empty. A's sweep must not read that as a dead session.
        let dir = tempfile::tempdir().unwrap();
        let b = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-b",
            u64::MAX,
            Duration::from_secs(60),
        )
        .unwrap();
        let a = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-a",
            u64::MAX,
            Duration::from_secs(60),
        )
        .unwrap();

        a.put("t", "text/plain", false, "a's first spill").unwrap();

        assert!(
            b.session_dir().is_dir(),
            "a live neighbour's directory must survive another session's sweep"
        );
        // And B is still able to spill — the property the directory stands for.
        let rec = b.put("t", "text/plain", false, "b's first spill").unwrap();
        assert_eq!(b.read(&rec.id, 0, 64).unwrap().text, "b's first spill");
    }

    #[test]
    fn spill_put_recreates_a_session_dir_deleted_underneath_it() {
        // Whatever removed it — another server's sweep, an operator clearing
        // disk — the store must heal instead of silently spilling nothing for
        // the rest of the process's life. `atomic_write_owner_only` does not
        // create its parent, so without this `put` fails `NotFound` forever.
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        fs::remove_dir_all(s.session_dir()).unwrap();

        let rec = s
            .put("t", "text/plain", false, "after the deletion")
            .unwrap();
        assert!(s.session_dir().is_dir());
        assert_eq!(s.read(&rec.id, 0, 64).unwrap().text, "after the deletion");
    }

    #[test]
    fn spill_byte_bound_evicts_our_own_before_a_live_neighbours() {
        // mtime order is not liveness order: the neighbour's record here is the
        // OLDEST file on disk, which is exactly what a long-running live
        // session looks like. Evicting it would leave a dead pointer in a
        // process that never learns of it — and it would not even help, since
        // its bytes are not measured against our cap in the first place.
        let dir = tempfile::tempdir().unwrap();
        let body = "x".repeat(1024);

        let b = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-b",
            u64::MAX,
            DEFAULT_MAX_AGE,
        )
        .unwrap();
        let neighbour = b.put("t", "text/plain", false, &body).unwrap();
        age_file(
            &b.session_dir().join(format!("{}.spill", neighbour.id)),
            300,
        );

        // Room for ~2 bodies of 1 KiB plus their headers, of OUR OWN.
        let a = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-a",
            2_600,
            DEFAULT_MAX_AGE,
        )
        .unwrap();
        let first = a.put("t", "text/plain", false, &body).unwrap();
        age_file(&a.session_dir().join(format!("{}.spill", first.id)), 200);
        let second = a.put("t", "text/plain", false, &body).unwrap();
        age_file(&a.session_dir().join(format!("{}.spill", second.id)), 100);
        let third = a.put("t", "text/plain", false, &body).unwrap();

        assert!(
            b.read(&neighbour.id, 0, 16).is_ok(),
            "a live neighbour's record is not ours to evict, however old it is"
        );
        assert_eq!(
            a.read(&first.id, 0, 16).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "we make room out of our OWN oldest instead"
        );
        assert!(a.read(&second.id, 0, 16).is_ok());
        assert!(a.read(&third.id, 0, 16).is_ok());
        assert_eq!(
            a.dropped_own_session(),
            1,
            "and the loss is counted, which is the whole point of restricting the rule"
        );
        // A counted loss also changes what a later read says about the id.
        let msg = a.read(&first.id, 0, 16).unwrap_err().to_string();
        assert!(
            msg.contains("dropped 1"),
            "read must cite the counter: {msg}"
        );
    }

    #[test]
    fn spill_a_body_over_the_whole_cap_does_not_empty_every_session() {
        // `total = sum + incoming` can never fall below the cap when `incoming`
        // alone exceeds it, so the byte loop used to run to exhaustion and
        // delete every record in every session — one oversized body wiping the
        // multi-session store.
        let dir = tempfile::tempdir().unwrap();
        let b = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-b",
            u64::MAX,
            DEFAULT_MAX_AGE,
        )
        .unwrap();
        let neighbour = b.put("t", "text/plain", false, "neighbour's body").unwrap();

        let a =
            SpillStore::open_with_bounds(dir.path().to_path_buf(), "sess-a", 100, DEFAULT_MAX_AGE)
                .unwrap();
        let ours = a.put("t", "text/plain", false, "our earlier body").unwrap();

        let huge = "y".repeat(4096);
        let rec = a.put("t", "text/plain", false, &huge).unwrap();

        assert_eq!(a.read(&rec.id, 0, 4096).unwrap().text, huge);
        assert!(
            b.read(&neighbour.id, 0, 64).is_ok(),
            "the neighbour's store must survive our oversized write"
        );
        assert!(
            a.read(&ours.id, 0, 64).is_ok(),
            "so must our own earlier records"
        );
        assert_eq!(a.dropped_own_session(), 0);
    }

    #[test]
    fn spill_byte_bound_holds_our_own_store_whatever_neighbours_hold() {
        // The property the per-process budget exists to guarantee, tested
        // against the case the root-wide budget could not survive: a live
        // neighbour holding many times A's whole cap in records younger than
        // `max_age`. Measuring the root against a per-process cap made
        // `unevictable > budget` true on every one of A's `put`s, so the byte
        // bound refused to run and A's store grew to whatever `max_age`
        // allowed — the fleet-normal shape, not an edge: five sessions sharing
        // this root each see four fifths of it as unevictable, and a restarted
        // server sees its OWN predecessor's records that way too.
        let dir = tempfile::tempdir().unwrap();
        let body = "x".repeat(1024);

        let b = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-b",
            u64::MAX,
            DEFAULT_MAX_AGE,
        )
        .unwrap();
        let neighbours: Vec<SpillRecord> = (0..40)
            .map(|_| b.put("t", "text/plain", false, &body).unwrap())
            .collect();
        let neighbour_bytes = session_bytes(b.session_dir());
        assert!(
            neighbour_bytes > 3_000 * 10,
            "the neighbour must dwarf A's cap for this to test anything"
        );

        let a = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-a",
            3_000,
            DEFAULT_MAX_AGE,
        )
        .unwrap();
        let ours: Vec<SpillRecord> = (0..12)
            .map(|_| a.put("t", "text/plain", false, &body).unwrap())
            .collect();

        // Bounded by OUR cap, regardless of what is next door. The budget is
        // expressed in BODY bytes, so one record's header is the honest slack
        // to allow on top of it.
        let own_bytes = session_bytes(a.session_dir());
        assert!(
            own_bytes <= 3_000 + MAX_HEADER_BYTES,
            "our own store is {own_bytes} bytes against a 3000 byte cap"
        );
        assert!(
            a.dropped_own_session() > 0,
            "and it is bounded by ENFORCEMENT, not by having written too little"
        );
        // The newest locator — the one a model is most likely still holding —
        // is what the oldest were spent on.
        assert!(a.read(&ours[ours.len() - 1].id, 0, 16).is_ok());

        // None of it was paid for out of the neighbour's store.
        for rec in &neighbours {
            assert!(
                b.read(&rec.id, 0, 16).is_ok(),
                "a live neighbour's record is never evicted to satisfy our cap"
            );
        }
        assert_eq!(session_bytes(b.session_dir()), neighbour_bytes);
    }

    #[test]
    fn spill_a_smaller_cap_evicts_more_not_less() {
        // `QONTINUI_MCP_SPILL_MAX_BYTES` is the only lever an operator has for
        // disk pressure without a rebuild, and the root-wide budget inverted
        // it: a smaller cap made the neighbour's live bytes more likely to
        // exceed the budget on their own, at which point the arm evicted
        // NOTHING — including the bytes this process was entitled to reclaim.
        // The neighbour below is what produced that; it must make no difference
        // now.
        fn run(cap: u64) -> (u64, u64) {
            let dir = tempfile::tempdir().unwrap();
            let body = "x".repeat(1024);
            let b = SpillStore::open_with_bounds(
                dir.path().to_path_buf(),
                "sess-b",
                u64::MAX,
                DEFAULT_MAX_AGE,
            )
            .unwrap();
            for _ in 0..30 {
                b.put("t", "text/plain", false, &body).unwrap();
            }
            let a = SpillStore::open_with_bounds(
                dir.path().to_path_buf(),
                "sess-a",
                cap,
                DEFAULT_MAX_AGE,
            )
            .unwrap();
            for _ in 0..10 {
                a.put("t", "text/plain", false, &body).unwrap();
            }
            (a.dropped_own_session(), session_bytes(a.session_dir()))
        }

        let (dropped_roomy, bytes_roomy) = run(8_000);
        let (dropped_tight, bytes_tight) = run(2_000);
        assert!(
            dropped_tight > dropped_roomy,
            "lowering the cap must evict MORE, not less: {dropped_tight} vs {dropped_roomy}"
        );
        assert!(
            bytes_tight < bytes_roomy,
            "and must leave LESS on disk: {bytes_tight} vs {bytes_roomy}"
        );
    }

    #[test]
    fn spill_a_caller_supplied_tool_name_cannot_mint_an_unreadable_record() {
        // `wrappers_mcp`'s unknown-tool arm spills an error body whose `tool`
        // is the raw `name` off the JSON-RPC frame, so a 40 KB name is one
        // `tools/call` away. Unbounded it pushed the header past
        // `MAX_HEADER_BYTES`, and `read` then refused the record FOREVER — a
        // locator that was a dead pointer the moment it was issued, and
        // invisible to `dropped_own_session` because nothing had deleted it.
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let huge = "z".repeat(40 * 1024);
        let rec = s.put(&huge, "application/json", true, "the body").unwrap();

        assert!(
            rec.tool.len() <= MAX_TOOL_LEN,
            "tool is {} bytes",
            rec.tool.len()
        );
        assert!(
            rec.tool.starts_with("zzz") && rec.tool.ends_with('…'),
            "a truncated field must say it was truncated: {}",
            rec.tool
        );
        assert_eq!(s.read(&rec.id, 0, 64).unwrap().text, "the body");
        assert_eq!(s.read(&rec.id, 0, 64).unwrap().record.tool, rec.tool);

        // The runtime half of the `const _: () = assert!` beside the constants:
        // every bounded field at its widest, in its most expensive form — a
        // control character costs six bytes once `serde_json` escapes it — must
        // still leave the header inside what `read` will buffer.
        let widest = SpillRecord {
            schema: SPILL_SCHEMA,
            id: "0".repeat(32),
            session: "z".repeat(MAX_SESSION_KEY_LEN),
            tool: "\u{1}".repeat(MAX_TOOL_LEN),
            content_type: "\u{1}".repeat(MAX_CONTENT_TYPE_LEN),
            byte_len: u64::MAX,
            created_at_ms: i64::MIN,
            is_error: true,
        };
        let header = serde_json::to_string(&widest).unwrap();
        assert!(
            (header.len() as u64) < MAX_HEADER_BYTES,
            "worst-case header is {} bytes against a {MAX_HEADER_BYTES} byte read bound",
            header.len()
        );
    }

    #[test]
    fn spill_retention_counts_and_evicts_only_what_this_process_issued() {
        // Two servers, one session key: a resumed conversation keeps its
        // `CLAUDE_CODE_SESSION_ID`, a restarted server reopens the directory,
        // and a `pid-<pid>` fallback repeats when a pid comes round again.
        // Directory identity calls the earlier server's records ours. They are
        // not — and reading them that way made retention destroy a stranger's
        // still-live locator AND report the loss as this process's own, the
        // exact lie the counter exists to prevent.
        let dir = tempfile::tempdir().unwrap();
        let max_age = Duration::from_secs(60);
        let body = "x".repeat(1024);

        let earlier =
            SpillStore::open_with_bounds(dir.path().to_path_buf(), "sess-a", u64::MAX, max_age)
                .unwrap();
        let live = earlier.put("t", "text/plain", false, &body).unwrap();
        let stale = earlier.put("t", "text/plain", false, &body).unwrap();
        let live_path = earlier.session_dir().join(format!("{}.spill", live.id));
        let stale_path = earlier.session_dir().join(format!("{}.spill", stale.id));
        // Old enough to be the oldest file under the root, young enough to be
        // alive — the shape that makes an mtime-ordered rule pick it first.
        age_file(&live_path, 30);
        age_file(&stale_path, 3600);

        // Room for ~2 files of OUR OWN. This process opens the SAME key.
        let s = SpillStore::open_with_bounds(dir.path().to_path_buf(), "sess-a", 2_600, max_age)
            .unwrap();
        let first = s.put("t", "text/plain", false, &body).unwrap();
        age_file(&s.session_dir().join(format!("{}.spill", first.id)), 20);

        assert!(
            !stale_path.exists(),
            "the age arm reclaims it, as it should"
        );
        assert_eq!(
            s.dropped_own_session(),
            0,
            "but sweeping a record this process never issued is not this process's loss"
        );

        let second = s.put("t", "text/plain", false, &body).unwrap();
        age_file(&s.session_dir().join(format!("{}.spill", second.id)), 15);
        let third = s.put("t", "text/plain", false, &body).unwrap();
        assert!(
            live_path.exists(),
            "a live stranger on our session key is a neighbour: neither counted against our cap \
             nor evictable to satisfy it"
        );
        assert_eq!(
            s.read(&first.id, 0, 16).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "we make room out of what we actually issued"
        );
        assert!(s.read(&second.id, 0, 16).is_ok());
        assert!(s.read(&third.id, 0, 16).is_ok());
        assert_eq!(s.dropped_own_session(), 1);

        // And the message must not blame our retention for an id we never
        // handed out — the same lie, pointed the other way.
        let msg = s.read(&stale.id, 0, 16).unwrap_err().to_string();
        assert!(msg.contains("never issued"), "{msg}");
    }

    #[test]
    fn spill_orphaned_temp_files_are_swept_and_stop_pinning_the_dir() {
        // A process killed mid-write leaves a full-body-sized temp behind:
        // `atomic_write_owner_only` cleans up only on a returned `Err`. Neither
        // retention arm could see it, and it pinned its dead session's
        // directory against pruning forever.
        let dir = tempfile::tempdir().unwrap();
        let dead = dir.path().join("sess-dead");
        fs::create_dir_all(&dead).unwrap();
        let orphan = dead.join(format!(
            "{}.spill.tmp.4242.0.1700000000000000000",
            "0".repeat(32)
        ));
        fs::write(&orphan, "y".repeat(4096)).unwrap();
        age_file(&orphan, 3600);
        age_file(&dead, 3600);

        // A temp belonging to a write happening RIGHT NOW must be left alone —
        // deleting it would corrupt a concurrent server's in-flight spill.
        let busy = dir.path().join("sess-busy");
        fs::create_dir_all(&busy).unwrap();
        let in_flight = busy.join(format!(
            "{}.spill.tmp.4243.0.1700000000000000001",
            "1".repeat(32)
        ));
        fs::write(&in_flight, "still being written").unwrap();

        let live = SpillStore::open_with_bounds(
            dir.path().to_path_buf(),
            "sess-live",
            u64::MAX,
            Duration::from_secs(60),
        )
        .unwrap();
        // One of OUR OWN, from an earlier run that shared this session key. It
        // is swept like any other, but it was never published, so it must not
        // register as a lost locator.
        let ours = live.session_dir().join(format!(
            "{}.spill.tmp.4244.0.1700000000000000002",
            "2".repeat(32)
        ));
        fs::write(&ours, "interrupted").unwrap();
        age_file(&ours, 3600);

        live.put("t", "text/plain", false, "fresh").unwrap();

        assert!(!ours.exists(), "our own aged temp must be swept too");
        assert_eq!(
            live.dropped_own_session(),
            0,
            "a temp was never published, so sweeping one is not a lost locator"
        );
        assert!(!orphan.exists(), "an aged temp must be swept");
        assert!(
            !dead.exists(),
            "and with it gone the dead session's dir prunes"
        );
        assert!(in_flight.exists(), "an in-flight temp must be left alone");
        assert!(busy.exists());
    }

    #[test]
    fn spill_temp_infix_matches_fs_atomics_naming() {
        // `atomic_write_owner_only` names its temp `{file_name}.tmp.{pid}.{seq}.{nanos}`
        // and our file name is always `<id>.spill`. If either half moves, the
        // sweep stops seeing temps — silently, since nothing else looks at them.
        assert_eq!(TEMP_INFIX, format!(".{SPILL_EXTENSION}.tmp."));
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let target = s.path_for(&"0".repeat(32));
        let name = target.file_name().unwrap().to_str().unwrap();
        assert!(format!("{name}.tmp.1234.0.99").contains(TEMP_INFIX));
    }

    #[test]
    fn spill_read_of_a_headerless_record_does_not_buffer_the_body() {
        // A record with no newline at all must be rejected on the bound, not
        // after pulling the entire body into a `String`.
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        let id = "0".repeat(32);
        fs::write(s.path_for(&id), "z".repeat(MAX_HEADER_BYTES as usize * 4)).unwrap();
        let err = s.read(&id, 0, 16).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string()
                .contains(&format!("first {MAX_HEADER_BYTES} bytes")),
            "the message must name the bound the read stopped at: {err}"
        );
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

    /// Backdate a file's — or a directory's — mtime by `secs` so retention
    /// ordering is deterministic rather than dependent on filesystem timestamp
    /// granularity. Directories matter as much as files now: the prune keys on
    /// a session directory's own idleness.
    fn age_file(path: &Path, secs: u64) {
        let when = SystemTime::now() - Duration::from_secs(secs);
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(when)).unwrap();
    }

    /// Bytes of every published record in one session directory — the quantity
    /// the byte bound is now defined over, measured from the outside rather
    /// than from the store's own accounting.
    fn session_bytes(dir: &Path) -> u64 {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(SPILL_EXTENSION))
            .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
            .sum()
    }
}
