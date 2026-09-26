//! The spawn-time SYSTEM-PROMPT carrier — how the runner's briefing and the
//! tenant's policy body reach a `claude` process at launch (plan
//! `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`, Phase 1
//! and its "Carrier correction").
//!
//! ## Why this exists
//!
//! Until this module the `policy/session-protocol` body (~11 KB) rode the
//! `SessionStart` hook's `additionalContext` on every start, resume and
//! compact ([`crate::mcp::policy_context`]). That hook-output batch is the
//! boundary a phantom auto-submitted `is` turn was localized to, and the policy
//! render was the single largest thing crossing it. The body now rides the
//! system prompt instead, which Claude Code applies at process start with no
//! session action — the same STRUCTURAL, unconditional delivery the
//! 2026-08-08 plan fought for, on a different wire.
//!
//! ## ONE composed file, never two flags
//!
//! Claude Code refuses `--append-system-prompt` and
//! `--append-system-prompt-file` together (`Error: Cannot use both …`, verified
//! against the Claude Code CLI in use when this landed), and every seam that delivers the hook already passes the
//! briefing inline. So a spawn composes ONE file — the
//! [`crate::terminal::runner_context`] briefing, a blank line, then the policy
//! body — and passes it via `--append-system-prompt-file` INSTEAD of the inline
//! flag ([`SystemPromptCarrier`]). A file keeps the ~11 KB off argv (the
//! `CreateProcessW` 32767-char ceiling) and off the env block (~32 KB), and
//! `runner_context()` itself is untouched, so its "never policy content"
//! contract and the `QONTINUI_RUNNER_CONTEXT` env var `/whereami` reads both
//! hold.
//!
//! Claude Code also treats a missing file as a FATAL start (`Append system
//! prompt file not found`). Every fall-back here therefore degrades to the
//! inline briefing — the pre-plan behaviour — and never to a flag naming a file
//! that might not exist.
//!
//! ## Where the body comes from: an on-disk per-tenant cache
//!
//! Spawn seams must not touch coord. The body is read from
//! `policy_body.<tenant-id>.md` in [`claude_hook::session_restore_dir`], which
//! the policy-context ROUTE writes atomically after each successful fetch
//! ([`write_policy_body_cache`]). A background poller was rejected: coord
//! inserts a `coord.session_policy_reads` row for EVERY agent-door read, so a
//! 45 s refresher would flood the compliance table with session-less rows. The
//! cache is on disk, so sessions restored right after a runner restart still get
//! the body at spawn.
//!
//! The directory is machine-global and shared by every runner instance, and a
//! prompt must never cross tenants — hence the tenant id in the file NAME
//! (the rule [`claude_hook::session_restore_dir`]'s own docs set for anything
//! whose content varies per process). No resolvable tenant ⇒ no cache is
//! written or read, and the spawn falls back to the inline briefing; there is
//! deliberately no unscoped fallback name.
//!
//! ## The delivered-SHA marker (the honesty half)
//!
//! A spawn that used the file carrier exports [`POLICY_DELIVERED_SHA_ENV`] on
//! the `claude` process: [`policy_body_sha`] of the exact body bytes it
//! composed. The bundled policy hook forwards it, and the route sends only a
//! short confirmation when it equals the hash of the body it JUST fetched;
//! anything else — no marker, a stale spawn-time copy, a cold cache — gets the
//! full body as before. File existence is never evidence: a session launched
//! before the cache existed has no marker, whatever is on disk now.
//!
//! The marker travels with [`POLICY_DELIVERED_FILE_ENV`], the composed file's
//! path, so the identity shims and shell wrappers can keep it ONLY for a
//! `claude` whose argv passes exactly that file. A nested `claude` inheriting
//! both — bare, inline, or with an `--append-system-prompt-file` of its own —
//! loses both.
//!
//! ## Spawn-file lifetime
//!
//! Composed files live in [`SPAWN_PROMPTS_DIR`], named by the hash of their
//! content, so identical spawns share one file and a busy runner writes each
//! distinct prompt once. They are pruned by AGE only (older than
//! [`SPAWN_PROMPT_MAX_AGE`]), never by a liveness guess; reusing a file
//! refreshes its mtime. A pruned file costs an interactive pane nothing worse
//! than the inline fall-back: the shell wrapper checks existence before
//! choosing the file flag.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

use crate::session::claude_hook;

/// Claude Code's inline system-prompt flag — the carrier every seam used
/// before this module.
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";

/// Claude Code's file system-prompt flag. Mutually exclusive with
/// [`APPEND_SYSTEM_PROMPT_FLAG`]; a missing path is a fatal start.
pub const APPEND_SYSTEM_PROMPT_FILE_FLAG: &str = "--append-system-prompt-file";

/// Env var carrying the absolute path of an interactive pane's composed
/// spawn-prompt file. The `resources/shell-integration.{bash,zsh,ps1}`
/// wrappers pass it via [`APPEND_SYSTEM_PROMPT_FILE_FLAG`] when the file
/// exists, and fall back to the inline `QONTINUI_RUNNER_CONTEXT` otherwise.
pub const RUNNER_CONTEXT_FILE_ENV: &str = "QONTINUI_RUNNER_CONTEXT_FILE";

/// Env var carrying [`policy_body_sha`] of the policy body a `claude` process
/// received in its system prompt. Set ONLY on a child that was actually given
/// the file carrier; the shell wrappers and identity shims blank it for any
/// child that was not. Forwarded by `claude_policy_hook.sh` to the
/// policy-context route, which trusts it only when it equals the hash of the
/// body it just fetched.
pub const POLICY_DELIVERED_SHA_ENV: &str = "QONTINUI_POLICY_DELIVERED_SHA";

/// Env var carrying the absolute path of the composed file whose policy body
/// [`POLICY_DELIVERED_SHA_ENV`] names. Set and blanked together with the SHA.
/// The identity shims and shell wrappers keep the pair for a `claude` only when
/// its `--append-system-prompt-file` argument (`--flag path` or `--flag=path`)
/// equals this path; any other launch drops both, so a nested `claude` with a
/// prompt file of its own is never vouched for.
pub const POLICY_DELIVERED_FILE_ENV: &str = "QONTINUI_POLICY_DELIVERED_FILE";

/// Subdirectory of [`claude_hook::session_restore_dir`] holding the composed
/// per-spawn files.
pub const SPAWN_PROMPTS_DIR: &str = "spawn-prompts";

/// A composed spawn file older than this is pruned. Age, not liveness: a pane
/// open longer than this merely falls back to the inline briefing.
pub const SPAWN_PROMPT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Minimum spacing between prune passes, so a burst of spawns costs one
/// directory walk rather than one each.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// File-name prefix of the per-tenant policy body cache.
const POLICY_BODY_PREFIX: &str = "policy_body.";

/// When this process last pruned [`SPAWN_PROMPTS_DIR`].
static LAST_PRUNE: Mutex<Option<Instant>> = Mutex::new(None);

// ===========================================================================
// Hash (the ONE definition the spawn side and the route share)
// ===========================================================================

/// Lower-case hex SHA-256 of the policy body bytes.
///
/// The single hash definition behind [`POLICY_DELIVERED_SHA_ENV`]: the spawn
/// side hashes the bytes it composed, the route hashes
/// [`crate::mcp::policy_context::render_policy_body`] of what it fetched, and
/// the two are the same function over the same bytes by construction.
pub fn policy_body_sha(body: &str) -> String {
    hex::encode(Sha256::digest(body.as_bytes()))
}

// ===========================================================================
// Per-tenant policy body cache
// ===========================================================================

/// The cache path for `tenant` under `base_dir`
/// (`policy_body.<tenant-id>.md`). THE one definition of that name.
pub fn policy_body_cache_path(base_dir: &Path, tenant: &uuid::Uuid) -> PathBuf {
    base_dir.join(format!("{POLICY_BODY_PREFIX}{tenant}.md"))
}

/// Write `bytes` to `path` by temp-file + rename, so a concurrent reader sees
/// either the old file or the new one and never a torn one, and `fsync` the
/// data before the rename — for a file that is REPLACED in place (the
/// per-tenant cache), where a crash must not leave a renamed-but-empty file.
///
/// The temp file sits in the SAME directory (a rename across volumes is not
/// atomic) and carries the pid plus a random suffix, so two runner instances
/// sharing the machine-global dir never write through the same temp name.
/// `std::fs::rename` replaces an existing destination on Windows as well.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, bytes, true)
}

/// [`write_atomic`] with the `fsync` optional. Content-addressed, write-once
/// spawn files skip it: a torn file after a crash is simply re-written by the
/// next spawn that finds it wrong-sized, and a sync per spawn is pure cost.
fn write_atomic_with(path: &Path, bytes: &[u8], sync: bool) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent dir")
    })?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
        Ok(())
    })();
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Persist the rendered policy body for `tenant`, atomically.
///
/// Skips the write when the file already holds exactly these bytes — the
/// common case, since the route fires on every session start and the body
/// changes only when coord versions it. Returns the cache path.
pub fn write_policy_body_cache(
    base_dir: &Path,
    tenant: &uuid::Uuid,
    body: &str,
) -> std::io::Result<PathBuf> {
    let path = policy_body_cache_path(base_dir, tenant);
    if std::fs::read(&path).is_ok_and(|existing| existing == body.as_bytes()) {
        return Ok(path);
    }
    write_atomic(&path, body.as_bytes())?;
    Ok(path)
}

/// Read the cached policy body for `tenant`. `None` when absent, unreadable,
/// or empty — an empty body is not a policy.
pub fn read_policy_body_cache(base_dir: &Path, tenant: &uuid::Uuid) -> Option<String> {
    std::fs::read_to_string(policy_body_cache_path(base_dir, tenant))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

// ===========================================================================
// Composer + pruning
// ===========================================================================

/// Compose one spawn file in `base_dir/`[`SPAWN_PROMPTS_DIR`]: `briefing`, a
/// blank line, then `body` — or `body` alone when there is no briefing (the
/// account-migration `--resume` respawn passes none).
///
/// The file is CONTENT-ADDRESSED — `spawn-<first 16 hex of sha256(content)>.md`
/// ([`spawn_prompt_file_name`]) — so the name can never point at different
/// bytes: a later spawn cannot rewrite the prompt an earlier pane is about to
/// read, and identical spawns (every pane of one briefing and one policy
/// version) share one file. An existing file holding exactly these bytes is
/// reused and its mtime refreshed, so the age prune never removes a file a
/// spawn just handed out; otherwise it is written by temp + rename, without
/// `fsync`.
/// Opportunistically prunes old files ([`maybe_prune`]).
pub fn compose_spawn_prompt_in(
    base_dir: &Path,
    briefing: Option<&str>,
    body: &str,
) -> std::io::Result<PathBuf> {
    let dir = base_dir.join(SPAWN_PROMPTS_DIR);
    maybe_prune(&dir);
    let mut content = String::with_capacity(briefing.map_or(0, str::len) + body.len() + 2);
    if let Some(b) = briefing.filter(|b| !b.trim().is_empty()) {
        content.push_str(b.trim_end());
        content.push_str("\n\n");
    }
    content.push_str(body);
    let path = dir.join(spawn_prompt_file_name(&content));
    if !reuse_spawn_prompt(&path, content.as_bytes()) {
        write_atomic_with(&path, content.as_bytes(), false)?;
    }
    Ok(path)
}

/// The content-addressed file name for a composed spawn prompt.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub fn spawn_prompt_file_name(content: &str) -> String {
    let digest = hex::encode(Sha256::digest(content.as_bytes()));
    format!("spawn-{}.md", &digest[..16])
}

/// Reuse an already-composed file: `true` when `path` is a regular file whose
/// bytes EQUAL `expected` and its mtime was refreshed to now. Anything else — no
/// file, a torn or tampered one (same length, different bytes included), an
/// mtime that cannot be set — is `false`, and the caller rewrites it. The name
/// is only a hash prefix of what SHOULD be inside; the bytes are what `claude`
/// reads and what the delivered-SHA marker vouches for. Refreshing the mtime is
/// what keeps the age prune from deleting a file between this spawn handing out
/// its path and `claude` reading it.
fn reuse_spawn_prompt(path: &Path, expected: &[u8]) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != expected.len() as u64 {
        return false;
    }
    if !std::fs::read(path).is_ok_and(|existing| existing == expected) {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(SystemTime::now()))
        .is_ok()
}

/// Prune at most once per [`PRUNE_INTERVAL`] per process.
fn maybe_prune(dir: &Path) {
    let due = match LAST_PRUNE.lock() {
        Ok(mut last) => {
            let due = last.is_none_or(|t| t.elapsed() >= PRUNE_INTERVAL);
            if due {
                *last = Some(Instant::now());
            }
            due
        }
        Err(_) => false,
    };
    if due {
        let removed = prune_spawn_prompts_in(dir, SPAWN_PROMPT_MAX_AGE, SystemTime::now());
        if removed > 0 {
            tracing::debug!(removed, dir = %dir.display(), "spawn-prompt: pruned aged composed files");
        }
    }
}

/// Delete regular files in `dir` whose mtime is older than `max_age` as of
/// `now` — a file touched within `max_age` is never removed, which is what the
/// reuse path's mtime refresh and the shell wrappers' pre-exec `touch` rely on
/// to keep a live pane's file alive. Returns how many were removed. Every error is swallowed: pruning is
/// housekeeping, and a file it cannot judge is left alone. `now` is a
/// parameter so the age arithmetic is testable without sleeping.
pub fn prune_spawn_prompts_in(dir: &Path, max_age: Duration, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let aged = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if aged && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ===========================================================================
// The carrier
// ===========================================================================

/// How a spawn delivers its system prompt. Exactly one flag, never both —
/// Claude Code refuses the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPromptCarrier {
    /// Today's `--append-system-prompt <text>`: the briefing alone, no policy
    /// body, no marker. The fall-back whenever a composed file is unavailable.
    Inline(String),
    /// `--append-system-prompt-file <path>`: a composed file whose policy body
    /// hashes to `policy_sha`.
    File { path: PathBuf, policy_sha: String },
}

impl SystemPromptCarrier {
    /// The argv pair this carrier contributes.
    pub fn argv(&self) -> Vec<String> {
        match self {
            SystemPromptCarrier::Inline(text) => {
                vec![APPEND_SYSTEM_PROMPT_FLAG.to_string(), text.clone()]
            }
            SystemPromptCarrier::File { path, .. } => vec![
                APPEND_SYSTEM_PROMPT_FILE_FLAG.to_string(),
                path.to_string_lossy().into_owned(),
            ],
        }
    }

    /// The [`POLICY_DELIVERED_SHA_ENV`] value for the child — `Some` only for
    /// the file carrier, because only it delivered a policy body.
    pub fn policy_sha(&self) -> Option<&str> {
        match self {
            SystemPromptCarrier::Inline(_) => None,
            SystemPromptCarrier::File { policy_sha, .. } => Some(policy_sha),
        }
    }

    /// The marker pair for the child — `Some` only for the file carrier. The
    /// one constructor of [`PolicyDelivery`] a spawn seam uses, so the SHA and
    /// the path always name the same composed file the argv passes.
    pub fn policy_delivery(&self) -> Option<PolicyDelivery> {
        match self {
            SystemPromptCarrier::Inline(_) => None,
            SystemPromptCarrier::File { path, policy_sha } => Some(PolicyDelivery {
                sha: policy_sha.clone(),
                file: path.to_string_lossy().into_owned(),
            }),
        }
    }
}

/// Is `token` a REPLACEMENT system-prompt flag (`--system-prompt` or
/// `--system-prompt-file`, either spelling)?
pub fn is_replacement_prompt_flag(token: &str) -> bool {
    let name = token.split_once('=').map_or(token, |(name, _)| name);
    name == "--system-prompt" || name == "--system-prompt-file"
}

/// Does `argv` carry a replacement system-prompt flag ahead of its `--`
/// terminator?
///
/// THE one rule every path applies to the delivered-policy marker: whether
/// Claude Code still applies an `--append-system-prompt-file` beside a
/// replacement prompt is not behaviourally verified, so a child whose effective
/// argv carries one never receives the marker, and the policy hook serves the
/// full body. The direct-exec seams ([`delivery_unless_replacement`]), the
/// identity shims and the shell wrappers all implement it.
pub fn argv_carries_replacement_prompt(argv: &[String]) -> bool {
    argv.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| is_replacement_prompt_flag(a))
}

/// `delivery`, unless the spawn's final rendered `argv` carries a replacement
/// prompt (an operator launch template can add one) — see
/// [`argv_carries_replacement_prompt`].
pub fn delivery_unless_replacement(
    delivery: Option<PolicyDelivery>,
    argv: &[String],
) -> Option<PolicyDelivery> {
    delivery.filter(|_| !argv_carries_replacement_prompt(argv))
}

/// The delivered-policy marker for one `claude` child: [`POLICY_DELIVERED_SHA_ENV`]
/// and [`POLICY_DELIVERED_FILE_ENV`], always set (or blanked) together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDelivery {
    /// [`policy_body_sha`] of the body the composed file carries.
    pub sha: String,
    /// The composed file's path, exactly as the argv passes it.
    pub file: String,
}

/// The two marker env pairs for a direct-exec child: the delivery's values, or
/// BOTH blank when there is none — blank, never omitted, so a marker inherited
/// from the runner's own environment cannot vouch for a delivery this child
/// never received.
pub fn policy_delivery_env(delivery: Option<&PolicyDelivery>) -> [(String, String); 2] {
    [
        (
            POLICY_DELIVERED_SHA_ENV.to_string(),
            delivery.map(|d| d.sha.clone()).unwrap_or_default(),
        ),
        (
            POLICY_DELIVERED_FILE_ENV.to_string(),
            delivery.map(|d| d.file.clone()).unwrap_or_default(),
        ),
    ]
}

/// Resolve the carrier for a spawn from the live state: the tenant's cached
/// body (via [`crate::mcp::policy_context::spawn_policy_body`], which applies
/// the injection flag and the tenant scoping) and the real
/// [`claude_hook::session_restore_dir`].
///
/// Local file I/O only — never a coord call. `None` means "no system-prompt
/// flag at all" (no briefing AND no body).
pub fn resolve_system_prompt_carrier(briefing: Option<String>) -> Option<SystemPromptCarrier> {
    let body = crate::mcp::policy_context::spawn_policy_body();
    resolve_system_prompt_carrier_in(
        &claude_hook::session_restore_dir(),
        briefing,
        body.as_deref(),
    )
}

/// [`resolve_system_prompt_carrier`] over an explicit base dir and body — the
/// unit-test surface.
///
/// - body present ⇒ compose; a written file ⇒ [`SystemPromptCarrier::File`],
///   a write failure ⇒ the inline briefing (logged at `warn`);
/// - no body ⇒ the inline briefing;
/// - no briefing and no usable file ⇒ `None`.
pub fn resolve_system_prompt_carrier_in(
    base_dir: &Path,
    briefing: Option<String>,
    body: Option<&str>,
) -> Option<SystemPromptCarrier> {
    let briefing = briefing.filter(|b| !b.trim().is_empty());
    if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
        match compose_spawn_prompt_in(base_dir, briefing.as_deref(), body) {
            Ok(path) => {
                return Some(SystemPromptCarrier::File {
                    path,
                    policy_sha: policy_body_sha(body),
                })
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dir = %base_dir.join(SPAWN_PROMPTS_DIR).display(),
                    "spawn-prompt: composed file write failed — falling back to the inline \
                     briefing (the policy body arrives via the SessionStart hook instead)"
                );
            }
        }
    }
    briefing.map(SystemPromptCarrier::Inline)
}

/// The env a SHELL pane's child needs for `briefing`: `(name, Some(value))` to
/// set, `(name, None)` to remove.
///
/// The shell wrapper launches `claude` later, so the pane gets the composed
/// file's PATH (as [`RUNNER_CONTEXT_FILE_ENV`], the wrapper's input, and as
/// [`POLICY_DELIVERED_FILE_ENV`], the shims' match target) plus the SHA; with
/// no file all three are REMOVED, so a value inherited from the runner's own
/// environment can never vouch for a delivery that did not happen.
/// `QONTINUI_RUNNER_CONTEXT` is set by the caller as before — it is the
/// wrapper's fall-back and `/whereami`'s source.
pub fn shell_pane_prompt_env(briefing: &str) -> [(&'static str, Option<String>); 3] {
    shell_pane_prompt_env_from(resolve_system_prompt_carrier(Some(briefing.to_string())))
}

/// [`shell_pane_prompt_env`] over an already-resolved carrier (pure).
fn shell_pane_prompt_env_from(
    carrier: Option<SystemPromptCarrier>,
) -> [(&'static str, Option<String>); 3] {
    match carrier
        .as_ref()
        .and_then(SystemPromptCarrier::policy_delivery)
    {
        Some(PolicyDelivery { sha, file }) => [
            (RUNNER_CONTEXT_FILE_ENV, Some(file.clone())),
            (POLICY_DELIVERED_SHA_ENV, Some(sha)),
            (POLICY_DELIVERED_FILE_ENV, Some(file)),
        ],
        None => [
            (RUNNER_CONTEXT_FILE_ENV, None),
            (POLICY_DELIVERED_SHA_ENV, None),
            (POLICY_DELIVERED_FILE_ENV, None),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant() -> uuid::Uuid {
        uuid::Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap()
    }

    // ── Hash ────────────────────────────────────────────────────────────

    #[test]
    fn policy_body_sha_is_lowercase_hex_sha256_of_the_exact_bytes() {
        // Known vector: sha256("abc").
        assert_eq!(
            policy_body_sha("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Byte-exact: a trailing newline is a different body.
        assert_ne!(policy_body_sha("abc"), policy_body_sha("abc\n"));
    }

    // ── Per-tenant cache ────────────────────────────────────────────────

    #[test]
    fn the_cache_is_named_per_tenant_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let other = uuid::Uuid::new_v4();
        let path = write_policy_body_cache(tmp.path(), &tenant(), "body v6").unwrap();
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "policy_body.11111111-2222-4333-8444-555555555555.md"
        );
        assert_eq!(
            read_policy_body_cache(tmp.path(), &tenant()).as_deref(),
            Some("body v6")
        );
        // A prompt never crosses tenants: another tenant reads nothing.
        assert_eq!(read_policy_body_cache(tmp.path(), &other), None);
    }

    #[test]
    fn the_cache_write_replaces_atomically_and_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_policy_body_cache(tmp.path(), &tenant(), "v6").unwrap();
        write_policy_body_cache(tmp.path(), &tenant(), "v7").unwrap();
        // Identical bytes: a no-op that still succeeds.
        write_policy_body_cache(tmp.path(), &tenant(), "v7").unwrap();
        assert_eq!(
            read_policy_body_cache(tmp.path(), &tenant()).as_deref(),
            Some("v7")
        );
        let names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "no stray temp file survives: {names:?}");
    }

    #[test]
    fn an_empty_cache_file_is_not_a_policy() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(policy_body_cache_path(tmp.path(), &tenant()), "  \n").unwrap();
        assert_eq!(read_policy_body_cache(tmp.path(), &tenant()), None);
    }

    #[test]
    fn write_atomic_fails_cleanly_when_the_destination_is_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("occupied");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("keep"), "x").unwrap();
        assert!(write_atomic(&dest, b"bytes").is_err());
        let stray: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            stray.is_empty(),
            "the temp file is removed on a failed rename"
        );
    }

    // ── Composer ────────────────────────────────────────────────────────

    #[test]
    fn compose_puts_the_briefing_then_a_blank_line_then_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let path = compose_spawn_prompt_in(tmp.path(), Some("BRIEFING\n"), "BODY").unwrap();
        assert_eq!(path.parent().unwrap(), tmp.path().join(SPAWN_PROMPTS_DIR));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "BRIEFING\n\nBODY");
    }

    #[test]
    fn compose_without_a_briefing_is_the_body_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let a = compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        let b = compose_spawn_prompt_in(tmp.path(), Some("  "), "BODY").unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "BODY");
        assert_eq!(
            a, b,
            "a blank briefing composes the same bytes, so the same file"
        );
    }

    /// Content-addressed: the name is the content hash, identical spawns share
    /// one file, different content never shares a name, and nothing else (no
    /// temp file) is left in the directory.
    #[test]
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn composed_files_are_content_addressed_and_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let a = compose_spawn_prompt_in(tmp.path(), Some("BRIEF"), "BODY").unwrap();
        assert_eq!(
            a.file_name().unwrap().to_string_lossy(),
            spawn_prompt_file_name("BRIEF\n\nBODY")
        );
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("spawn-") && name.ends_with(".md"),
            "{name}"
        );
        assert_eq!(name.len(), "spawn-".len() + 16 + ".md".len(), "{name}");
        assert!(name["spawn-".len()..name.len() - 3]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));

        let again = compose_spawn_prompt_in(tmp.path(), Some("BRIEF"), "BODY").unwrap();
        assert_eq!(a, again, "identical content reuses the file");
        let other = compose_spawn_prompt_in(tmp.path(), Some("BRIEF"), "BODY v7").unwrap();
        assert_ne!(a, other);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "BRIEF\n\nBODY v7");

        let names: Vec<String> = std::fs::read_dir(tmp.path().join(SPAWN_PROMPTS_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temp files, no duplicates: {names:?}");
    }

    /// Reuse refreshes the mtime (so the age prune cannot delete a file a spawn
    /// just handed out), and a torn file of the wrong size is rewritten.
    #[test]
    fn reusing_a_composed_file_refreshes_its_mtime_and_repairs_a_torn_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        let old = SystemTime::now() - Duration::from_secs(6 * 24 * 60 * 60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        let refreshed = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(
            refreshed > old + Duration::from_secs(60),
            "mtime was not refreshed on reuse"
        );

        std::fs::write(&path, "BO").unwrap();
        compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "BODY");

        // Same length, different bytes: a length check alone would reuse it.
        std::fs::write(&path, "EVIL").unwrap();
        compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "BODY");
    }

    // ── Pruning ─────────────────────────────────────────────────────────

    #[test]
    fn prune_removes_only_files_older_than_the_max_age() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("spawn-old.md");
        std::fs::write(&file, "x").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let now = SystemTime::now();
        // Inside the window: kept.
        assert_eq!(
            prune_spawn_prompts_in(tmp.path(), SPAWN_PROMPT_MAX_AGE, now),
            0
        );
        assert!(file.exists());

        // Eight days on: the file is aged out; the directory is never touched.
        let later = now + Duration::from_secs(8 * 24 * 60 * 60);
        assert_eq!(
            prune_spawn_prompts_in(tmp.path(), SPAWN_PROMPT_MAX_AGE, later),
            1
        );
        assert!(!file.exists());
        assert!(tmp.path().join("subdir").exists());
    }

    #[test]
    fn prune_of_a_missing_dir_is_a_quiet_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            prune_spawn_prompts_in(
                &tmp.path().join("absent"),
                SPAWN_PROMPT_MAX_AGE,
                SystemTime::now()
            ),
            0
        );
    }

    // ── Carrier resolution ──────────────────────────────────────────────

    #[test]
    fn a_body_yields_the_file_carrier_with_the_bodys_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let carrier =
            resolve_system_prompt_carrier_in(tmp.path(), Some("BRIEF".into()), Some("BODY"))
                .unwrap();
        let SystemPromptCarrier::File { path, policy_sha } = &carrier else {
            panic!("expected the file carrier, got {carrier:?}");
        };
        assert!(path.exists(), "the flag must never name a missing file");
        assert_eq!(policy_sha, &policy_body_sha("BODY"));
        assert_eq!(carrier.policy_sha(), Some(policy_sha.as_str()));
        assert_eq!(
            carrier.argv(),
            vec![
                APPEND_SYSTEM_PROMPT_FILE_FLAG.to_string(),
                path.to_string_lossy().into_owned()
            ]
        );
    }

    #[test]
    fn no_body_yields_the_inline_briefing_and_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let carrier = resolve_system_prompt_carrier_in(tmp.path(), Some("BRIEF".into()), None);
        assert_eq!(carrier, Some(SystemPromptCarrier::Inline("BRIEF".into())));
        let carrier = carrier.unwrap();
        assert_eq!(carrier.policy_sha(), None);
        assert_eq!(
            carrier.argv(),
            vec![APPEND_SYSTEM_PROMPT_FLAG.to_string(), "BRIEF".to_string()]
        );
        assert!(
            !tmp.path().join(SPAWN_PROMPTS_DIR).exists(),
            "nothing is composed without a body"
        );
        assert_eq!(
            resolve_system_prompt_carrier_in(tmp.path(), None, None),
            None
        );
        assert_eq!(
            resolve_system_prompt_carrier_in(tmp.path(), None, Some(" ")),
            None
        );
    }

    #[test]
    fn a_failed_compose_falls_back_to_inline_never_to_a_dangling_file_flag() {
        let tmp = tempfile::tempdir().unwrap();
        // Occupy the spawn-prompts dir name with a FILE so the write fails.
        std::fs::write(tmp.path().join(SPAWN_PROMPTS_DIR), "not a dir").unwrap();
        assert_eq!(
            resolve_system_prompt_carrier_in(tmp.path(), Some("BRIEF".into()), Some("BODY")),
            Some(SystemPromptCarrier::Inline("BRIEF".into()))
        );
        // No briefing to fall back to (the account-migration respawn): no flag.
        assert_eq!(
            resolve_system_prompt_carrier_in(tmp.path(), None, Some("BODY")),
            None
        );
    }

    #[test]
    fn a_shell_pane_exports_the_file_and_marker_only_for_the_file_carrier() {
        let file = SystemPromptCarrier::File {
            path: PathBuf::from("/x/spawn-1.md"),
            policy_sha: "ab".repeat(32),
        };
        assert_eq!(
            shell_pane_prompt_env_from(Some(file)),
            [
                (RUNNER_CONTEXT_FILE_ENV, Some("/x/spawn-1.md".to_string())),
                (POLICY_DELIVERED_SHA_ENV, Some("ab".repeat(32))),
                (POLICY_DELIVERED_FILE_ENV, Some("/x/spawn-1.md".to_string())),
            ]
        );
        // Inline or nothing: all REMOVED, so an inherited marker cannot vouch.
        for carrier in [Some(SystemPromptCarrier::Inline("b".into())), None] {
            assert_eq!(
                shell_pane_prompt_env_from(carrier),
                [
                    (RUNNER_CONTEXT_FILE_ENV, None),
                    (POLICY_DELIVERED_SHA_ENV, None),
                    (POLICY_DELIVERED_FILE_ENV, None),
                ]
            );
        }
    }

    /// The uniform replacement-prompt rule: a replacement flag before `--`
    /// (either spelling) withholds the marker; after `--` it is prompt text.
    #[test]
    fn a_replacement_prompt_in_the_argv_withholds_the_delivery() {
        let delivery = PolicyDelivery {
            sha: "ab".repeat(32),
            file: "/x/spawn-1.md".to_string(),
        };
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for with in [
            argv(&[
                "claude",
                "--system-prompt",
                "x",
                "--append-system-prompt-file",
                "/x/spawn-1.md",
            ]),
            argv(&["claude", "--system-prompt=x"]),
            argv(&["claude", "--system-prompt-file", "/t.md"]),
            argv(&["claude", "--system-prompt-file=/t.md", "--", "p"]),
        ] {
            assert!(argv_carries_replacement_prompt(&with), "{with:?}");
            assert_eq!(
                delivery_unless_replacement(Some(delivery.clone()), &with),
                None
            );
        }
        for without in [
            argv(&[
                "claude",
                "--append-system-prompt-file",
                "/x/spawn-1.md",
                "--",
                "--system-prompt",
            ]),
            argv(&["claude", "--system-prompts", "x"]),
            argv(&["claude"]),
        ] {
            assert!(!argv_carries_replacement_prompt(&without), "{without:?}");
            assert_eq!(
                delivery_unless_replacement(Some(delivery.clone()), &without),
                Some(delivery.clone())
            );
        }
    }

    /// A direct-exec child gets the SHA and the exact argv path together, or
    /// both blank — never omitted.
    #[test]
    fn policy_delivery_env_sets_both_or_blanks_both() {
        let file = SystemPromptCarrier::File {
            path: PathBuf::from("/x/spawn-2.md"),
            policy_sha: "cd".repeat(32),
        };
        let delivery = file.policy_delivery().unwrap();
        assert_eq!(delivery.file, file.argv()[1], "the path the argv passes");
        assert_eq!(
            policy_delivery_env(Some(&delivery)),
            [
                (POLICY_DELIVERED_SHA_ENV.to_string(), "cd".repeat(32)),
                (
                    POLICY_DELIVERED_FILE_ENV.to_string(),
                    "/x/spawn-2.md".to_string()
                ),
            ]
        );
        assert_eq!(
            SystemPromptCarrier::Inline("b".into()).policy_delivery(),
            None
        );
        assert_eq!(
            policy_delivery_env(None),
            [
                (POLICY_DELIVERED_SHA_ENV.to_string(), String::new()),
                (POLICY_DELIVERED_FILE_ENV.to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn the_env_names_are_the_documented_contract() {
        // The shell wrappers, the identity shims and the hook script spell
        // these literally.
        assert_eq!(RUNNER_CONTEXT_FILE_ENV, "QONTINUI_RUNNER_CONTEXT_FILE");
        assert_eq!(POLICY_DELIVERED_SHA_ENV, "QONTINUI_POLICY_DELIVERED_SHA");
        assert_eq!(POLICY_DELIVERED_FILE_ENV, "QONTINUI_POLICY_DELIVERED_FILE");
    }
}

/// The shell-side halves of the carrier contract, exercised for real: the
/// `shell-integration` wrappers that pick the flag and the bundled policy hook
/// that forwards the marker. Unix-only (they drive `bash`); the PowerShell
/// wrapper is exercised only when `pwsh` is on PATH.
#[cfg(all(test, unix))]
mod script_tests {
    use crate::process_helpers::output_with_timeout;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    /// Every shell these tests run is a handful of lines that redirects into a
    /// tempdir and exits. A bound this loose can only be hit by a script that
    /// has genuinely hung — and a hung shell must FAIL the test rather than
    /// park the test thread (and CI) forever, which is exactly what a bare
    /// `.status()` / `.output()` does. Routed through
    /// [`crate::process_helpers::output_with_timeout`], so expiry kills the
    /// whole process tree and returns an error.
    const SCRIPT_BUDGET: Duration = Duration::from_secs(30);

    /// Serialises "write an executable, then spawn". `Command` forks, and a
    /// forked child inherits every fd open at that moment — including another
    /// thread's still-open write handle on the script IT is about to run,
    /// which then fails to start with ETXTBSY. Cheap: these are sub-second
    /// shell runs.
    static EXE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`EXE_LOCK`], surviving a poisoned mutex so one failing test
    /// reports its own assertion instead of cascading into the rest.
    fn exe_guard() -> std::sync::MutexGuard<'static, ()> {
        EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    const BASH_INTEGRATION: &str = include_str!("../../resources/shell-integration.bash");
    const ZSH_INTEGRATION: &str = include_str!("../../resources/shell-integration.zsh");
    const PS1_INTEGRATION: &str = include_str!("../../resources/shell-integration.ps1");
    const POLICY_HOOK: &str = include_str!("../../resources/session-restore/claude_policy_hook.sh");
    const IDENTITY_SHIM_BASH: &str = include_str!("../../resources/intercept/identity_shim.bash");

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The `claude` wrapper block of a posix integration script, without the
    /// OSC/prompt plumbing (which writes to `/dev/tty`).
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn wrapper_block(script: &str) -> &str {
        let start = script
            .find("# ── Claude Code runner context")
            .expect("the wrapper block is present");
        let rest = &script[start..];
        let end = rest.find("\nfi\n").expect("the wrapper block closes") + 4;
        &rest[..end]
    }

    fn write_exe(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A fake `claude` on PATH that records its argv (one per line) and the
    /// marker pair it inherited.
    fn fake_claude(dir: &Path) -> std::path::PathBuf {
        let out = dir.join("claude-invocation.txt");
        write_exe(
            &dir.join("claude"),
            &format!(
                "#!/usr/bin/env bash\n{{ printf 'SHA=[%s]\\n' \"${{QONTINUI_POLICY_DELIVERED_SHA:-}}\"; \
                 printf 'FILE=[%s]\\n' \"${{QONTINUI_POLICY_DELIVERED_FILE:-}}\"; \
                 for a in \"$@\"; do printf 'ARG=%s\\n' \"$a\"; done; }} > '{}'\n",
                out.display()
            ),
        );
        out
    }

    /// Run a posix wrapper block under bash with `envs`, then `claude <args>`.
    fn run_wrapper(block: &str, envs: &[(&str, &str)], args: &str) -> String {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        let out = fake_claude(tmp.path());
        let script = tmp.path().join("wrapper.sh");
        std::fs::write(&script, format!("{block}\nclaude {args}\n")).unwrap();
        let path = format!(
            "{}:{}",
            tmp.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(&script)
            .env_clear()
            .env("PATH", path)
            .env("QONTINUI_RUNNER_TERMINAL", "1")
            .envs(envs.iter().copied());
        let out_res = output_with_timeout(cmd, SCRIPT_BUDGET).expect("bash runs inside its budget");
        // `output_with_timeout` PIPES stderr, where `.status()` inherited it — so
        // the script's own diagnosis reaches the failing assertion only if we
        // carry it here. Without this a broken wrapper reports `assertion failed`
        // and nothing else.
        assert!(
            out_res.status.success(),
            "wrapper script failed: {}",
            String::from_utf8_lossy(&out_res.stderr)
        );
        std::fs::read_to_string(out).expect("the fake claude ran")
    }

    /// The expected fake-claude record: the marker pair, then each argv entry.
    fn record(sha: &str, file: &str, args: &[&str]) -> String {
        let mut out = format!("SHA=[{sha}]\nFILE=[{file}]\n");
        for a in args {
            out.push_str(&format!("ARG={a}\n"));
        }
        out
    }

    fn assert_wrapper_contract(block: &str, label: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("spawn-1.md");
        std::fs::write(&file, "BRIEFING\n\nBODY").unwrap();
        let file_s = file.to_string_lossy().into_owned();
        let pane_env = [
            ("QONTINUI_RUNNER_CONTEXT", "BRIEFING"),
            ("QONTINUI_RUNNER_CONTEXT_FILE", file_s.as_str()),
            ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
            ("QONTINUI_POLICY_DELIVERED_FILE", file_s.as_str()),
        ];

        // File present: the file flag INSTEAD of the inline one; marker kept.
        // The file is touched first, so an old live pane re-arms it against
        // the age prune.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(6 * 24 * 60 * 60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(
            run_wrapper(block, &pane_env, "-p hi"),
            record(
                SHA,
                &file_s,
                &["--append-system-prompt-file", &file_s, "-p", "hi"]
            ),
            "{label}: file carrier"
        );
        let touched = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert!(
            touched > old + std::time::Duration::from_secs(60),
            "{label}: the wrapper must touch the composed file before exec"
        );

        // File pruned/missing: inline fall-back, marker BLANKED.
        assert_eq!(
            run_wrapper(
                block,
                &[
                    ("QONTINUI_RUNNER_CONTEXT", "BRIEFING"),
                    ("QONTINUI_RUNNER_CONTEXT_FILE", "/nonexistent/spawn-x.md"),
                    ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
                    ("QONTINUI_POLICY_DELIVERED_FILE", "/nonexistent/spawn-x.md"),
                ],
                "-p hi",
            ),
            record("", "", &["--append-system-prompt", "BRIEFING", "-p", "hi"]),
            "{label}: inline fall-back"
        );

        // Caller inline flag(s) only: our briefing JOINS as another inline flag
        // (never the file — Claude Code refuses that pair); marker blanked.
        for (args, tail) in [
            (
                "--append-system-prompt mine",
                vec!["--append-system-prompt", "mine"],
            ),
            (
                "--append-system-prompt=mine -p hi",
                vec!["--append-system-prompt=mine", "-p", "hi"],
            ),
            (
                "--append-system-prompt a --append-system-prompt b",
                vec!["--append-system-prompt", "a", "--append-system-prompt", "b"],
            ),
        ] {
            let mut expect = vec!["--append-system-prompt", "BRIEFING"];
            expect.extend(tail);
            assert_eq!(
                run_wrapper(block, &pane_env, args),
                record("", "", &expect),
                "{label}: caller inline prompt {args:?}"
            );
        }

        // A caller `--system-prompt[-file]` STARTS beside the append file flag
        // (verified against the Claude Code CLI in use when this landed), so ours is still passed — but whether its
        // content is still applied is unverified, so the marker is BLANKED and
        // the hook serves the full body.
        for (args, tail) in [
            ("--system-prompt mine", vec!["--system-prompt", "mine"]),
            (
                "--system-prompt-file=./s.md",
                vec!["--system-prompt-file=./s.md"],
            ),
            (
                "-p hi --system-prompt-file ./s.md",
                vec!["-p", "hi", "--system-prompt-file", "./s.md"],
            ),
        ] {
            let mut expect = vec!["--append-system-prompt-file", file_s.as_str()];
            expect.extend(tail);
            assert_eq!(
                run_wrapper(block, &pane_env, args),
                record("", "", &expect),
                "{label}: caller replacement prompt {args:?}"
            );
        }

        // A prompt flag spelled AFTER `--` is positional prompt text: ours is
        // passed and the marker kept.
        for (args, tail) in [
            (
                "-p -- --append-system-prompt-file",
                vec!["-p", "--", "--append-system-prompt-file"],
            ),
            (
                "-- --append-system-prompt x",
                vec!["--", "--append-system-prompt", "x"],
            ),
        ] {
            let mut expect = vec!["--append-system-prompt-file", file_s.as_str()];
            expect.extend(tail);
            assert_eq!(
                run_wrapper(block, &pane_env, args),
                record(SHA, &file_s, &expect),
                "{label}: not a caller-owned append prompt {args:?}"
            );
        }

        // A caller-owned append FILE suppresses ours entirely.
        for (args, argv) in [
            (
                "--append-system-prompt-file ./eval.md",
                vec!["--append-system-prompt-file", "./eval.md"],
            ),
            (
                "--append-system-prompt-file=./eval.md",
                vec!["--append-system-prompt-file=./eval.md"],
            ),
            (
                "--append-system-prompt x --append-system-prompt-file ./eval.md",
                vec![
                    "--append-system-prompt",
                    "x",
                    "--append-system-prompt-file",
                    "./eval.md",
                ],
            ),
        ] {
            assert_eq!(
                run_wrapper(block, &pane_env, args),
                record("", "", &argv),
                "{label}: caller-owned prompt {args:?}"
            );
        }

        // Nothing at all: bare launch, no marker.
        assert_eq!(
            run_wrapper(
                block,
                &[
                    ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
                    ("QONTINUI_POLICY_DELIVERED_FILE", "/x.md")
                ],
                "-p hi"
            ),
            record("", "", &["-p", "hi"]),
            "{label}: no briefing"
        );
    }

    #[test]
    fn bash_wrapper_uses_the_file_carrier_only_when_the_file_exists() {
        assert_wrapper_contract(wrapper_block(BASH_INTEGRATION), "bash");
    }

    /// The zsh block is the bash block's twin and uses only syntax both shells
    /// share, so it is driven under bash here (no zsh on the CI image). This
    /// proves its LOGIC; its zsh-specific parse is covered by the shared syntax.
    #[test]
    fn zsh_wrapper_logic_matches_the_bash_twin() {
        assert_wrapper_contract(wrapper_block(ZSH_INTEGRATION), "zsh");
    }

    #[test]
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn powershell_wrapper_uses_the_file_carrier_only_when_the_file_exists() {
        let _serial = exe_guard();
        if Command::new("pwsh").arg("-v").output().is_err() {
            eprintln!("skipping: pwsh not on PATH");
            return;
        }
        let start = PS1_INTEGRATION
            .find("# ── Claude Code runner context")
            .unwrap();
        let rest = &PS1_INTEGRATION[start..];
        let end = rest.find("\n# Intercept PSReadLine").unwrap();
        let block = &rest[..end];

        let tmp = tempfile::tempdir().unwrap();
        let out = fake_claude(tmp.path());
        let file = tmp.path().join("spawn-1.md");
        std::fs::write(&file, "BODY").unwrap();
        let path = format!(
            "{}:{}",
            tmp.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let file_s = file.to_string_lossy().into_owned();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(6 * 24 * 60 * 60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let run = |ctx_file: &str, args: &str| {
            let script = tmp.path().join("w.ps1");
            std::fs::write(&script, format!("{block}\nclaude {args}\n")).unwrap();
            let output = Command::new("pwsh")
                .args(["-NoProfile", "-NonInteractive", "-File"])
                .arg(&script)
                .env("PATH", &path)
                .env("QONTINUI_RUNNER_TERMINAL", "1")
                .env("QONTINUI_RUNNER_CONTEXT", "BRIEFING")
                .env("QONTINUI_RUNNER_CONTEXT_FILE", ctx_file)
                .env("QONTINUI_POLICY_DELIVERED_SHA", SHA)
                .env("QONTINUI_POLICY_DELIVERED_FILE", ctx_file)
                .output()
                .expect("pwsh runs");
            assert!(output.status.success());
            // Fail-open and SILENT: a missing composed file must not leak a
            // red non-terminating error into the operator's pane.
            assert!(
                output.stderr.is_empty(),
                "{args}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::fs::read_to_string(&out).unwrap()
        };
        assert_eq!(
            run(&file_s, "-p hi"),
            record(
                SHA,
                &file_s,
                &["--append-system-prompt-file", &file_s, "-p", "hi"]
            )
        );
        assert!(
            std::fs::metadata(&file).unwrap().modified().unwrap()
                > old + std::time::Duration::from_secs(60),
            "the ps1 wrapper must touch the composed file before exec"
        );
        assert_eq!(
            run("/nonexistent/spawn-x.md", "-p hi"),
            record("", "", &["--append-system-prompt", "BRIEFING", "-p", "hi"])
        );
        assert_eq!(
            run(&file_s, "--append-system-prompt mine"),
            record(
                "",
                "",
                &[
                    "--append-system-prompt",
                    "BRIEFING",
                    "--append-system-prompt",
                    "mine"
                ]
            )
        );
        assert_eq!(
            run(&file_s, "--append-system-prompt-file ./eval.md"),
            record("", "", &["--append-system-prompt-file", "./eval.md"])
        );
        // A quoted `--` reaches $args, and the scan stops there.
        assert_eq!(
            run(&file_s, "-p '--' --append-system-prompt-file"),
            record(
                SHA,
                &file_s,
                &[
                    "--append-system-prompt-file",
                    &file_s,
                    "-p",
                    "--",
                    "--append-system-prompt-file"
                ]
            )
        );
        assert_eq!(
            run(&file_s, "--system-prompt mine"),
            record(
                "",
                "",
                &[
                    "--append-system-prompt-file",
                    &file_s,
                    "--system-prompt",
                    "mine"
                ]
            )
        );
    }

    /// Run the rendered bash identity shim as `claude <args>` with the marker
    /// pair set, against a fake real `claude` further down PATH.
    fn run_identity_shim(delivered_file: &str, args: &[&str]) -> String {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("shim");
        let real_dir = tmp.path().join("real");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        let out = fake_claude(&real_dir);
        let rendered = IDENTITY_SHIM_BASH
            .replace("@@TOOL@@", "claude")
            .replace("@@SHIM_DIR@@", &shim_dir.to_string_lossy());
        write_exe(&shim_dir.join("claude"), &rendered);
        let mut cmd = Command::new("bash");
        cmd.arg(shim_dir.join("claude"))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    shim_dir.display(),
                    real_dir.display()
                ),
            )
            .env("QONTINUI_POLICY_DELIVERED_SHA", SHA)
            .env("QONTINUI_POLICY_DELIVERED_FILE", delivered_file);
        let shim_out =
            output_with_timeout(cmd, SCRIPT_BUDGET).expect("the shim runs inside its budget");
        // Same reason as `run_wrapper`: stderr is piped now, so surface it in the
        // assertion rather than discarding the shim's own error text.
        assert!(
            shim_out.status.success(),
            "identity shim failed: {}",
            String::from_utf8_lossy(&shim_out.stderr)
        );
        std::fs::read_to_string(out).expect("the real claude ran")
    }

    /// A NESTED `claude` (recursion guard set) must not inherit the parent
    /// terminal's coord-mcp key variables — the bash identity shim unsets every
    /// `QONTINUI_COORD_MCP_NONCE_<K>` / `QONTINUI_COORD_MCP_CREDENTIAL_<K>` in
    /// its pass-through, and nothing else (plan
    /// `2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages`).
    #[test]
    fn bash_identity_shim_nested_passthrough_drops_the_terminal_key_vars() {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("shim");
        let real_dir = tmp.path().join("real");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        let out = real_dir.join("env.txt");
        write_exe(
            &real_dir.join("claude"),
            &format!(
                "#!/usr/bin/env bash
env | grep -E '^(QONTINUI_COORD_MCP_|KEEP_ME)' | sort > '{}'
",
                out.display()
            ),
        );
        let rendered = IDENTITY_SHIM_BASH
            .replace("@@TOOL@@", "claude")
            .replace("@@SHIM_DIR@@", &shim_dir.to_string_lossy());
        write_exe(&shim_dir.join("claude"), &rendered);
        let nonce_var = crate::coord_mcp_config::terminal_nonce_env_name("/w/a");
        let cred_var = crate::coord_mcp_config::terminal_credential_env_name("/w/a");
        let mut cmd = Command::new("bash");
        cmd.arg(shim_dir.join("claude"))
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    shim_dir.display(),
                    real_dir.display()
                ),
            )
            .env("QONTINUI_INSTALL_INTERCEPT_GUARD", "1")
            .env(&nonce_var, "parent-terminal-nonce")
            .env(&cred_var, "/parent/cred.json")
            .env("KEEP_ME", "1");
        let shim_out = crate::process_helpers::output_with_timeout(cmd, SCRIPT_BUDGET)
            .expect("the shim runs inside its budget");
        assert!(
            shim_out.status.success(),
            "identity shim failed: {}",
            String::from_utf8_lossy(&shim_out.stderr)
        );
        let got = std::fs::read_to_string(out).expect("the real claude ran");
        assert_eq!(got, "KEEP_ME=1\n", "only the unrelated var survives: {got}");
    }

    /// The bash identity shim keeps the marker pair ONLY for a launch that
    /// passes the composed file itself (either spelling). A nested `claude`
    /// with its own prompt file, a bare one, or an inline one loses both.
    #[test]
    fn bash_identity_shim_keeps_the_marker_only_for_the_exact_composed_file() {
        let composed = "/rt/spawn-prompts/spawn-0123456789abcdef.md";
        let flag_attached = format!("--append-system-prompt-file={composed}");
        for args in [
            vec!["--append-system-prompt-file", composed, "-p", "hi"],
            vec![flag_attached.as_str(), "-p", "hi"],
            // A replacement flag spelled after `--` is prompt text.
            vec![
                "--append-system-prompt-file",
                composed,
                "--",
                "--system-prompt",
            ],
        ] {
            let got = run_identity_shim(composed, &args);
            assert!(
                got.starts_with(&format!("SHA=[{SHA}]\nFILE=[{composed}]\n")),
                "{args:?}: {got}"
            );
        }
        for args in [
            vec!["-p", "--append-system-prompt-file", "./eval.md"],
            vec!["--append-system-prompt-file=./eval.md"],
            vec!["--append-system-prompt-file", "./eval.md", composed],
            vec!["--append-system-prompt", "x"],
            vec!["-p", "hi"],
            // The composed file beside a REPLACEMENT prompt: withheld.
            vec![
                "--append-system-prompt-file",
                composed,
                "--system-prompt",
                "x",
            ],
            vec!["--system-prompt-file=./s.md", flag_attached.as_str()],
        ] {
            let got = run_identity_shim(composed, &args);
            assert!(got.starts_with("SHA=[]\nFILE=[]\n"), "{args:?}: {got}");
        }
        // No inherited file: nothing to match, the SHA is dropped.
        let got = run_identity_shim("", &["--append-system-prompt-file", composed]);
        assert!(got.starts_with("SHA=[]\n"), "{got}");
    }

    /// Run the bundled policy hook against a fake `curl` that echoes every
    /// argument it was given, one per line, and return those lines.
    fn hook_curl_args(sha: Option<&str>) -> Vec<String> {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        write_exe(
            &tmp.path().join("curl"),
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
        );
        let hook = tmp.path().join("claude_policy_hook.sh");
        write_exe(&hook, POLICY_HOOK);
        let mut cmd = Command::new("bash");
        cmd.arg(&hook)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
            .env("QONTINUI_RUNNER_API_PORT", "9876")
            .env("QONTINUI_TERMINAL_ID", "term-1")
            .stdin(std::process::Stdio::null());
        if let Some(sha) = sha {
            cmd.env("QONTINUI_POLICY_DELIVERED_SHA", sha);
        }
        let out = output_with_timeout(cmd, SCRIPT_BUDGET).expect("the hook runs inside its budget");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The marker rides a request HEADER (the runner's trace log records
    /// request URIs), never the query string, and only when well formed.
    #[test]
    fn the_policy_hook_forwards_only_a_well_formed_delivered_sha_in_a_header() {
        const URL: &str = "http://127.0.0.1:9876/sessions/term-1/policy-context";
        let args = hook_curl_args(Some(SHA));
        assert_eq!(args.last().map(String::as_str), Some(URL), "{args:?}");
        assert!(
            args.iter().all(|a| !a.contains("delivered_sha")),
            "{args:?}"
        );
        let h = args.iter().position(|a| a == "-H").expect("a header flag");
        assert_eq!(
            args[h + 1],
            format!("X-Qontinui-Policy-Delivered-Sha: {SHA}")
        );

        let args = hook_curl_args(None);
        assert_eq!(args.last().map(String::as_str), Some(URL));
        assert!(!args.iter().any(|a| a == "-H"), "{args:?}");
        // Empty (the blanked marker), short, or non-hex: not forwarded.
        for bad in ["", "abc", &"z".repeat(64), &format!("{SHA}&x=1")] {
            let args = hook_curl_args(Some(bad));
            assert_eq!(args.last().map(String::as_str), Some(URL), "{bad:?}");
            assert!(!args.iter().any(|a| a == "-H"), "{bad:?}: {args:?}");
        }
    }

    // ── The policy hook's PAYLOAD parse ─────────────────────────────────────
    //
    // Plan `2026-09-21-policy-body-still-crosses-the-sessionstart-boundary`,
    // Phase 3. The property that actually failed in production is NOT the one
    // the plan's Phase 1 gate named — "a carrier in the argv implies a
    // matching marker in the child env" held on the unfixed tree and would
    // have passed it. What failed is one link further on:
    //
    //   A HOOK INVOCATION CARRYING A MARKER MUST ALSO CARRY THE SOURCE IT
    //   WAS GIVEN.
    //
    // `claude_policy_hook.sh` resolved its JSON parser with a bare
    // `command -v python`. On a box with `python3` and no `python` — every
    // modern Linux — both extractions returned "" SILENTLY, so the hook built
    // a URL with no `source=` and no `claude_session_id=` while still sending
    // a valid `X-Qontinui-Policy-Delivered-Sha` header. The route then read
    // `raw_source = None`, `source_permits_confirmation(None)` was false, and
    // it served the FULL ~11 KB body — the exact outcome the marker exists to
    // avoid. 98 such injections on 2026-09-20 alone.
    //
    // These run the REAL bundled script. A route-level test cannot see this
    // class: the route was correct the whole time while production was wrong.

    const SESSION_HOOK: &str =
        include_str!("../../resources/session-restore/claude_session_hook.sh");
    const STOP_HOOK: &str = include_str!("../../resources/session-restore/claude_stop_hook.sh");
    const PRECOMPACT_HOOK: &str =
        include_str!("../../resources/session-restore/claude_precompact_hook.sh");

    /// Every rung of the cascade, exercised unconditionally. `jq` is rung ONE
    /// -- the rung most production boxes actually run -- so skipping it when the
    /// host lacks it reports green about something that never executed.
    const ALL_RUNGS: [&[&str]; 3] = [&["jq"], &["python3"], &[]];

    const SESSION_ID: &str = "c4095874-264a-4ce7-9c7f-7d6c9b1748ba";
    const BARE_URL: &str = "http://127.0.0.1:9876/sessions/term-1/policy-context";

    /// A production-shaped `SessionStart` payload.
    fn session_start_payload(source: &str) -> String {
        format!(
            "{{\"session_id\":\"{SESSION_ID}\",\"transcript_path\":\"/t/x.jsonl\",\
             \"cwd\":\"/w\",\"hook_event_name\":\"SessionStart\",\"source\":\"{source}\"}}"
        )
    }

    /// Resolve a program on the HOST's PATH, so a test can place the real
    /// thing — or deliberately NOT place it — on a stripped PATH of its own.
    fn which_host(name: &str) -> Option<std::path::PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }

    /// A PATH holding ONLY `bash`, a fake `curl`, and the named interpreters.
    ///
    /// Built from scratch rather than prepended to the host's, so "`python` is
    /// absent" is a FACT ABOUT THE RUN instead of a hope about the machine —
    /// which is the whole point of the decisive case below.
    fn isolated_bin(
        tmp: &Path,
        interpreters: &[&str],
        curl_body: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let bin = tmp.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let bash = which_host("bash").expect("bash is on PATH (this module drives bash)");
        std::os::unix::fs::symlink(&bash, bin.join("bash")).unwrap();
        for name in interpreters {
            let real = which_host(name)
                .unwrap_or_else(|| panic!("`{name}` must be on PATH to exercise its rung"));
            std::os::unix::fs::symlink(&real, bin.join(name)).unwrap();
        }
        // The shebang is the RESOLVED bash, not `/usr/bin/env bash`: on the
        // stripped PATH below `env` could not look `bash` up.
        write_exe(
            &bin.join("curl"),
            &format!("#!{}\n{curl_body}", bash.display()),
        );
        (bin, bash)
    }

    /// One bundled-hook run: the fake `curl`'s argv, and the hook's own
    /// stderr — the channel a degrade must announce itself on.
    struct HookRun {
        curl_args: Vec<String>,
        stderr: String,
    }

    impl HookRun {
        /// The request URL: the first `http://` argument, NOT the last one.
        /// The GET hook puts the URL last, but the two POSTing hooks end with
        /// `--data-binary @-`, so `last()` silently answered `@-` for them and
        /// made their assertions unfalsifiable-looking rather than true.
        fn url(&self) -> &str {
            self.curl_args
                .iter()
                .find(|a| a.starts_with("http://"))
                .map(String::as_str)
                .unwrap_or("")
        }
        fn has_header(&self, h: &str) -> bool {
            self.curl_args.iter().any(|a| a == h)
        }
    }

    /// Run the bundled policy hook with `payload` on stdin, a delivered-sha
    /// marker, and ONLY `interpreters` reachable as JSON parsers.
    /// Run ANY bundled hook against a fake `curl` that records its argv to a
    /// file, with `payload` on stdin and only `interpreters` reachable.
    ///
    /// Recording to a FILE rather than stdout is what lets one harness serve
    /// all three: the session and precompact hooks send curl's stdout to
    /// `/dev/null`, so a stdout-echoing fake would be invisible for them.
    fn run_hook_argv(
        script: &str,
        name: &str,
        payload: &str,
        envs: &[(&str, &str)],
        interpreters: &[&str],
    ) -> HookRun {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        let argv = tmp.path().join("curl-argv.txt");
        let (bin, bash) = isolated_bin(
            tmp.path(),
            interpreters,
            &format!(
                "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n",
                argv.display()
            ),
        );

        let hook = tmp.path().join(name);
        write_exe(&hook, script);
        let payload_file = tmp.path().join("payload.json");
        std::fs::write(&payload_file, payload).unwrap();

        // Redirect the payload in from a FILE rather than piping it: it keeps
        // the run inside `output_with_timeout`'s plain `Output` shape. The
        // un-EOF'd case gets its own fifo-based test below, because a file
        // redirect always reaches EOF immediately and so could never catch a
        // hook that blocks forever on its drain.
        let mut cmd = Command::new(&bash);
        cmd.arg("-c")
            .arg(format!(
                // `bash <file>`, never `exec <file>`: execing the script makes
                // it ETXTBSY whenever another test thread's fork is still
                // holding the write fd this thread just closed, which is a
                // flake in the harness that reads as a hook failure.
                "exec '{}' '{}' < '{}'",
                bash.display(),
                hook.display(),
                payload_file.display()
            ))
            .env_clear()
            .env("PATH", bin.display().to_string())
            .envs(envs.iter().copied());
        let res = output_with_timeout(cmd, SCRIPT_BUDGET).expect("the hook runs inside its budget");
        // A hook that fails a session is worse than the bug it was fixing.
        assert!(
            res.status.success(),
            "{name} must never fail a session: {}",
            String::from_utf8_lossy(&res.stderr)
        );
        HookRun {
            curl_args: std::fs::read_to_string(&argv)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect(),
            stderr: String::from_utf8_lossy(&res.stderr).into_owned(),
        }
    }

    fn run_policy_hook(payload: &str, sha: Option<&str>, interpreters: &[&str]) -> HookRun {
        let mut envs = vec![
            ("QONTINUI_RUNNER_API_PORT", "9876"),
            ("QONTINUI_TERMINAL_ID", "term-1"),
        ];
        if let Some(sha) = sha {
            envs.push(("QONTINUI_POLICY_DELIVERED_SHA", sha));
        }
        run_hook_argv(
            POLICY_HOOK,
            "claude_policy_hook.sh",
            payload,
            &envs,
            interpreters,
        )
    }

    /// The session hook, addressed the way the runner addresses it. No
    /// `QONTINUI_TERMINAL_ID`: this hook reads the id out of the PAYLOAD, and
    /// that is the branch with no coverage before now.
    fn run_session_hook(payload: &str, interpreters: &[&str]) -> HookRun {
        run_hook_argv(
            SESSION_HOOK,
            "claude_session_hook.sh",
            payload,
            &[("QONTINUI_INSTALL_INTERCEPT_PORT", "9876")],
            interpreters,
        )
    }

    /// The precompact hook with NO terminal id, for the same reason.
    fn run_precompact_hook(payload: &str, interpreters: &[&str]) -> HookRun {
        run_hook_argv(
            PRECOMPACT_HOOK,
            "claude_precompact_hook.sh",
            payload,
            &[("QONTINUI_RUNNER_API_PORT", "9876")],
            interpreters,
        )
    }

    /// The POST body a hook handed to `curl -d`.
    fn posted_body(run: &HookRun) -> String {
        let i = run
            .curl_args
            .iter()
            .position(|a| a == "-d")
            .unwrap_or_else(|| panic!("no -d in {:?}", run.curl_args));
        run.curl_args[i + 1].clone()
    }

    /// Assert the run carried the whole triple: source, attribution, marker.
    fn assert_full_context(run: &HookRun, source: &str) {
        assert!(
            run.url().contains(&format!("source={source}")),
            "the `source` must survive the payload parse: {:?}",
            run.curl_args
        );
        assert!(
            run.url()
                .contains(&format!("claude_session_id={SESSION_ID}")),
            "the read must stay attributable: {:?}",
            run.curl_args
        );
        assert!(
            run.has_header(&format!("X-Qontinui-Policy-Delivered-Sha: {SHA}")),
            "the marker must ride along: {:?}",
            run.curl_args
        );
        assert!(
            run.stderr.is_empty(),
            "a healthy parse warns about nothing: {}",
            run.stderr
        );
    }

    /// **THE DECISIVE CASE.** `python3` on PATH, `python` NOT — the shape of
    /// every Linux box in this fleet, and of the box the defect was found on.
    ///
    /// Revert the interpreter cascade to a bare `command -v python` and this
    /// is the test that fails: the URL comes back as the bare
    /// `…/policy-context` with the marker header still attached, which is
    /// byte-for-byte the shape of all 98 defect injections logged 2026-09-20.
    #[test]
    fn the_policy_hook_forwards_the_source_when_only_python3_is_on_path() {
        let run = run_policy_hook(&session_start_payload("startup"), Some(SHA), &["python3"]);
        assert_full_context(&run, "startup");
    }

    /// The cascade's LAST rung: no JSON parser on the box at all. The source
    /// must still arrive, because the fallback parses in bash itself — so
    /// "no interpreter" stops being a way to lose the source rather than
    /// merely becoming one interpreter name less likely.
    #[test]
    fn the_policy_hook_forwards_the_source_with_no_json_interpreter_at_all() {
        let run = run_policy_hook(&session_start_payload("compact"), Some(SHA), &[]);
        assert_full_context(&run, "compact");
    }

    /// The `jq` rung, where the box has one. Skipped rather than failed on a
    /// host without `jq`: it is genuinely optional, and the two cases above
    /// already cover the property on every host.
    #[test]
    fn the_policy_hook_forwards_the_source_over_the_jq_rung() {
        // NOT skipped when `jq` is missing. `jq` is rung ONE of the cascade —
        // the rung most production boxes actually execute — so a green run
        // that silently never exercised it is the `watcher-honesty` shape:
        // the report says "passed" about something it did not do. `isolated_bin`
        // panics with a message naming the missing program.
        let run = run_policy_hook(&session_start_payload("resume"), Some(SHA), &["jq"]);
        assert_full_context(&run, "resume");
    }

    /// A payload the cascade cannot read must DIAGNOSE rather than degrade
    /// silently. The silent degrade is the deeper defect: `printf ''` turned
    /// "I could not parse the payload" into "the payload said nothing", which
    /// is what made a missing `source` indistinguishable from a real one for
    /// five days and 98 injections.
    #[test]
    fn the_policy_hook_diagnoses_an_unreadable_payload_without_breaking_the_session() {
        let run = run_policy_hook("{\"session_id\": ", Some(SHA), &["python3"]);
        // It degrades to the bare URL ...
        assert_eq!(run.url(), BARE_URL, "{:?}", run.curl_args);
        // ... but SAYS SO, naming the consequence, on the channel the runner
        // captures into its log.
        assert!(run.stderr.contains("no 'source'"), "{}", run.stderr);
        assert!(
            run.stderr.contains("FULL policy body"),
            "the diagnostic must name the consequence, not just the fault: {}",
            run.stderr
        );
        assert!(run.stderr.contains("no 'session_id'"), "{}", run.stderr);
        // And the injection still happens: the session is never blocked.
        assert!(
            run.has_header(&format!("X-Qontinui-Policy-Delivered-Sha: {SHA}")),
            "{:?}",
            run.curl_args
        );
    }

    /// An EMPTY payload is an honest UNKNOWN, not a parse failure — a hook
    /// invoked with no stdin has nothing to report and must not cry wolf.
    /// Without this split the diagnostic would fire on every such run and be
    /// tuned out, which is how a real signal gets lost.
    #[test]
    fn the_policy_hook_is_silent_when_there_is_no_payload_at_all() {
        let run = run_policy_hook("", Some(SHA), &["python3"]);
        assert_eq!(run.url(), BARE_URL, "{:?}", run.curl_args);
        assert!(
            run.stderr.is_empty(),
            "an absent payload is UNKNOWN, not a fault: {}",
            run.stderr
        );
    }

    /// Run the bundled Stop hook against a fake `curl` answering `verdict`,
    /// and return what the hook printed on stdout.
    fn run_stop_hook(verdict: &str, interpreters: &[&str]) -> String {
        let _serial = exe_guard();
        let tmp = tempfile::tempdir().unwrap();
        let (bin, bash) = isolated_bin(
            tmp.path(),
            interpreters,
            // Drain the piped payload with a BUILTIN (`cat` is not on this
            // PATH), then answer as the runner's verdict endpoint would.
            &format!("IFS= read -r -d '' _ 2>/dev/null || true\nprintf '%s' '{verdict}'\n"),
        );
        let hook = tmp.path().join("claude_stop_hook.sh");
        write_exe(&hook, STOP_HOOK);
        let payload_file = tmp.path().join("payload.json");
        std::fs::write(
            &payload_file,
            format!("{{\"session_id\":\"{SESSION_ID}\",\"stop_hook_active\":false}}"),
        )
        .unwrap();
        let mut cmd = Command::new(&bash);
        cmd.arg("-c")
            .arg(format!(
                // `bash <file>`, never `exec <file>`: execing the script makes
                // it ETXTBSY whenever another test thread's fork is still
                // holding the write fd this thread just closed, which is a
                // flake in the harness that reads as a hook failure.
                "exec '{}' '{}' < '{}'",
                bash.display(),
                hook.display(),
                payload_file.display()
            ))
            .env_clear()
            .env("PATH", bin.display().to_string())
            .env("QONTINUI_RUNNER_API_PORT", "9876")
            .env("QONTINUI_TERMINAL_ID", "term-1")
            .env("QONTINUI_STOP_HOOK_CONTINUATION", "on");
        let res = output_with_timeout(cmd, SCRIPT_BUDGET).expect("the hook runs inside its budget");
        assert!(
            res.status.success(),
            "the stop hook must never trap a session: {}",
            String::from_utf8_lossy(&res.stderr)
        );
        String::from_utf8_lossy(&res.stdout).into_owned()
    }

    /// The Stop hook's verdict mapping, over EVERY rung of the cascade.
    ///
    /// Its `command -v python || exit 0` killed the whole hook — verdict
    /// request included — on this box, so the mapping below had never run
    /// here at all. The shell rung hand-builds the JSON envelope, which is
    /// the riskiest new code in this repair; it is safe only because the
    /// capture class excludes `"` and `\`, and that is what this pins.
    #[test]
    fn the_stop_hook_maps_a_block_verdict_on_every_parser_rung() {
        for rung in ALL_RUNGS {
            let out = run_stop_hook(
                r#"{"decision":"block","prompt":"Do the follow-ups."}"#,
                rung,
            );
            assert!(out.contains("\"decision\""), "{rung:?}: {out}");
            assert!(out.contains("block"), "{rung:?}: {out}");
            assert!(out.contains("Do the follow-ups."), "{rung:?}: {out}");

            // An `allow` must produce NO output — the stop proceeds. A
            // substring test for "block" would wrongly fire on a body that
            // merely mentions it, so the shell rung matches the KEY.
            let out = run_stop_hook(r#"{"decision":"allow","prompt":"never block"}"#, rung);
            assert!(
                out.is_empty(),
                "{rung:?}: an allow must emit nothing: {out}"
            );
        }
    }

    /// The static twin of the behavioural tests above: no bundled hook may
    /// reach for an interpreter without offering `python3` AND a rung that
    /// needs no interpreter at all. This is what catches a reintroduction in
    /// the two hooks whose behaviour is not otherwise exercised here.
    #[test]
    fn every_bundled_hook_resolves_python3_and_a_parser_free_rung() {
        for (name, body) in [
            ("claude_policy_hook.sh", POLICY_HOOK),
            ("claude_session_hook.sh", SESSION_HOOK),
            ("claude_stop_hook.sh", STOP_HOOK),
            ("claude_precompact_hook.sh", PRECOMPACT_HOOK),
        ] {
            // Strip comments FIRST. Every literal below also appears in the
            // prose explaining the defect, so a grep over the raw file is
            // satisfied by the explanation of the fix rather than the fix —
            // it passed on a tree whose code had been gutted.
            let body: String = body
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n");
            let body = body.as_str();
            if !body.contains("python") {
                continue;
            }
            assert!(
                body.contains("python3"),
                "{name}: reaches for `python` but never `python3` — the exact defect \
                 plan 2026-09-21-policy-body-still-crosses-the-sessionstart-boundary fixed"
            );
            assert!(
                body.contains("qh_parser"),
                "{name}: no resolved-once parser cascade, so a missing interpreter \
                 can still silently lose a field"
            );
            assert!(
                body.contains("BASH_REMATCH"),
                "{name}: no interpreter-free rung — `no parser on the box` must not be \
                 a way to lose a field"
            );
        }
    }

    /// **F1 — the rungs must never disagree, and the bash rung must never
    /// FORGE.** This is the property the first cut of this fix claimed and did
    /// not have: a regex sees a flat byte stream and takes the LEFTMOST match,
    /// while `json.load` and `jq` take the TOP-LEVEL key and the LAST
    /// duplicate. On a payload whose true top-level `source` is `clear` but
    /// which carries an earlier nested `source: startup`, the leftmost read
    /// returns `startup` — which passes the four-literal check, lets the route
    /// reach the SHA comparison, and serves the SHORT body to a session whose
    /// snapshot reset is unverified. That is the honesty property inverted, in
    /// exactly the direction this plan exists to protect.
    ///
    /// The rung now declines an ambiguous document, so each rung either AGREES
    /// with the structured ones or returns nothing. Losing a value is safe;
    /// forging one is not.
    #[test]
    fn no_parser_rung_ever_disagrees_about_the_source() {
        let uuid = SESSION_ID;
        for (label, payload, structured) in [
            (
                "nested object before the real key",
                format!("{{\"session_id\":\"{uuid}\",\"meta\":{{\"source\":\"startup\"}},\"source\":\"clear\"}}"),
                "clear",
            ),
            (
                "nested inside an array",
                format!("{{\"session_id\":\"{uuid}\",\"items\":[{{\"source\":\"startup\"}}],\"source\":\"clear\"}}"),
                "clear",
            ),
            (
                "duplicate top-level keys (last wins in JSON)",
                format!("{{\"session_id\":\"{uuid}\",\"source\":\"resume\",\"source\":\"startup\"}}"),
                "startup",
            ),
        ] {
            // The structured rungs agree with each other and with JSON.
            for rung in [["python3"], ["jq"]] {
                let run = run_policy_hook(&payload, Some(SHA), &rung);
                assert!(
                    run.url().contains(&format!("source={structured}")),
                    "{label} on {rung:?}: expected source={structured}, got {}",
                    run.url()
                );
            }
            // The interpreter-free rung DECLINES rather than answering
            // differently. `source=` absent means the route serves the full
            // body — the safe direction.
            let run = run_policy_hook(&payload, Some(SHA), &[]);
            assert!(
                !run.url().contains("source="),
                "{label}: the shell rung must DECLINE an ambiguous payload, not \
                 answer from the leftmost match — got {}",
                run.url()
            );
        }

        // NESTED-ONLY: the key exists ONLY below the top level, so it occurs
        // exactly ONCE and an occurrence count waves it through. JSON says the
        // top-level key is ABSENT, and both structured rungs return empty —
        // so a rung that answers here does not merely disagree, it invents a
        // value out of a payload that has none. For `source` that value was
        // `startup`, which passes the four-literal gate and makes the route
        // serve the SHORT confirmation to a session that asked for no such
        // thing, with no diagnostic, because the rung believed it read
        // something. Strictly worse than the defect this plan started from.
        //
        // This is the shape the first two cuts of the fix missed: every
        // payload in the loop above carries a top-level twin ALONGSIDE the
        // nested key, so all of them trip the occurrence count and none of
        // them reaches this branch.
        for (label, payload) in [
            (
                "nested only",
                format!("{{\"session_id\":\"{uuid}\",\"meta\":{{\"source\":\"startup\"}}}}"),
            ),
            (
                "array-nested only",
                format!("{{\"session_id\":\"{uuid}\",\"items\":[{{\"source\":\"startup\"}}]}}"),
            ),
            (
                "deep-nested only",
                format!("{{\"session_id\":\"{uuid}\",\"a\":{{\"b\":{{\"source\":\"startup\"}}}}}}"),
            ),
            (
                "nested only, spaced",
                format!(
                    "{{ \"session_id\" : \"{uuid}\" , \"meta\" : {{ \"source\" : \"startup\" }} }}"
                ),
            ),
        ] {
            for rung in ALL_RUNGS {
                let run = run_policy_hook(&payload, Some(SHA), rung);
                assert!(
                    !run.url().contains("source="),
                    "{label} on {rung:?}: JSON has NO top-level `source`, so no rung \
                     may supply one — got {}",
                    run.url()
                );
            }
        }

        // NON-OBJECT ROOT. A key nested inside a TOP-LEVEL ARRAY is nested at
        // NO extra `{`, so the brace test alone reads it as top-level:
        // `[{"source":"clear"}]` has exactly one `{` and one `}`. The bracket
        // clause had been covering this by accident, so deleting it without a
        // root check reintroduced forgery — of the honesty-sensitive value, in
        // a document JSON says has no such key at all.
        for (label, payload) in [
            ("array root", "[{\"source\":\"clear\"}]".to_string()),
            (
                "array root, later object",
                format!("[1,2,{{\"session_id\":\"{uuid}\",\"source\":\"clear\"}}]"),
            ),
            ("string root", "\"just a string\"".to_string()),
        ] {
            for rung in ALL_RUNGS {
                let run = run_policy_hook(&payload, Some(SHA), rung);
                assert!(
                    !run.url().contains("source="),
                    "{label} on {rung:?}: the root is not an object, so JSON has no \
                     top-level key and no rung may supply one — got {}",
                    run.url()
                );
            }
        }

        // BRACKETS ARE NOT NESTING. A `[` cannot put a key below the top level
        // without an extra `{`, so the brace test already covers every nesting
        // shape — while declining on a bracket returns a no-interpreter box to
        // the ORIGINAL defect (full body AND a NULL-session, unattributable
        // read) for something as ordinary as a `cwd` with a bracket in it.
        for (label, payload) in [
            (
                "bracket in cwd",
                format!("{{\"session_id\":\"{uuid}\",\"cwd\":\"/home/u/proj[old]\",\"source\":\"startup\"}}"),
            ),
            (
                "top-level array value",
                format!("{{\"session_id\":\"{uuid}\",\"tags\":[\"a\",\"b\"],\"source\":\"startup\"}}"),
            ),
        ] {
            for rung in ALL_RUNGS {
                let run = run_policy_hook(&payload, Some(SHA), rung);
                assert!(
                    run.url().contains("source=startup"),
                    "{label} on {rung:?}: a bracket is not nesting and must not \
                     cost the rung its answer — got {}",
                    run.url()
                );
            }
        }

        // Control: an unambiguous payload is still read by every rung, so the
        // decline above is about ambiguity and not a rung that stopped working.
        for rung in ALL_RUNGS {
            let run = run_policy_hook(&session_start_payload("startup"), Some(SHA), &rung);
            assert!(
                run.url().contains("source=startup"),
                "{rung:?}: {}",
                run.url()
            );
        }
    }

    /// **F4 — a value that becomes a URL PATH SEGMENT is constrained.**
    /// `claude_session_id` was shape-checked because it rides a query string;
    /// the segment had no check at all, so a crafted `session_id` re-pointed
    /// the request at a different loopback route. Worse for the hooks that
    /// `POST --data-binary @-`, which would carry the payload there with it.
    #[test]
    fn a_crafted_session_id_cannot_re_point_the_request_at_another_route() {
        let hostile = "x/../../control/session-open?z=";
        let payload = format!("{{\"session_id\":\"{hostile}\",\"source\":\"startup\"}}");
        // No QONTINUI_TERMINAL_ID, so the payload id is what addresses the route.
        let run = run_hook_argv(
            POLICY_HOOK,
            "claude_policy_hook.sh",
            &payload,
            &[("QONTINUI_RUNNER_API_PORT", "9876")],
            &["python3"],
        );
        assert!(
            run.curl_args.is_empty(),
            "the hook must not issue a request at all rather than address a \
             route the id chose: {:?}",
            run.curl_args
        );
        // Control: a canonical id still addresses the intended route.
        let run = run_hook_argv(
            POLICY_HOOK,
            "claude_policy_hook.sh",
            &session_start_payload("startup"),
            &[("QONTINUI_RUNNER_API_PORT", "9876")],
            &["python3"],
        );
        assert_eq!(
            run.url(),
            &format!(
                "http://127.0.0.1:9876/sessions/{SESSION_ID}/policy-context?source=startup&claude_session_id={SESSION_ID}"
            )
        );
    }

    /// **F7 — the session hook had no behavioural test at all**, so the static
    /// grep was the only thing holding its rung, and that grep is satisfied by
    /// the literals appearing in a COMMENT. Every rung must read the payload.
    #[test]
    fn the_session_hook_reads_the_payload_on_every_parser_rung() {
        for rung in ALL_RUNGS {
            let run = run_session_hook(&session_start_payload("resume"), &rung);
            let body = posted_body(&run);
            assert!(
                body.contains(&format!("\"session_id\":\"{SESSION_ID}\"")),
                "{rung:?}: {body}"
            );
            assert!(body.contains("\"source\":\"resume\""), "{rung:?}: {body}");
            assert!(run.stderr.is_empty(), "{rung:?}: {}", run.stderr);
        }
    }

    /// **F2 — the session hook DEFAULTS `source` to `startup`, and that is a
    /// fabrication.** A `resume` whose source could not be read was recorded
    /// as a startup with nothing said — "a missing source indistinguishable
    /// from a real one", the very defect this plan removes, still live one
    /// hook over. The default stays (the route's contract expects a known
    /// label) but it must announce itself.
    #[test]
    fn the_session_hook_omits_a_source_it_could_not_read_rather_than_inventing_one() {
        let run = run_session_hook(
            &format!("{{\"session_id\":\"{SESSION_ID}\"}}"),
            &["python3"],
        );
        let body = posted_body(&run);
        // NOT `"source":"startup"`. Warning about a fabrication while still
        // sending it leaves every downstream consumer seeing a real startup,
        // so the plan's own thesis stayed unmet one hook over. The route
        // accepts an absent source (`#[serde(default)] Option<String>`) and
        // already renders it distinctly, so omitting costs nothing.
        assert!(
            !body.contains("\"source\""),
            "an unreadable source must be OMITTED, not invented: {body}"
        );
        assert!(
            body.contains(&format!("\"session_id\":\"{SESSION_ID}\"")),
            "{body}"
        );
        assert!(
            run.stderr.contains("OMITTING the field"),
            "and it must say so: {}",
            run.stderr
        );
        // A source it CAN read is still sent.
        let run = run_session_hook(&session_start_payload("resume"), &["python3"]);
        assert!(posted_body(&run).contains("\"source\":\"resume\""));
    }

    /// **F5 — every hand-built JSON field is escaped.** Note the inversion:
    /// the interpreter-free rung CANNOT produce a value needing this, while
    /// `jq` and `python3` can. So this is a test about the rungs that parse
    /// correctly.
    #[test]
    fn the_session_hook_escapes_what_it_interpolates_into_json() {
        for rung in [["python3"], ["jq"]] {
            let run = run_session_hook("{\"session_id\":\"ab\\\"c\",\"source\":\"resume\"}", &rung);
            let body = posted_body(&run);
            assert!(body.contains(r#""session_id":"ab\"c""#), "{rung:?}: {body}");
            // Still parseable: the quote did not terminate the field.
            assert!(body.ends_with('}'), "{rung:?}: {body}");
        }
    }

    /// **F7 — the precompact hook had no behavioural test either.**
    #[test]
    fn the_precompact_hook_reads_the_payload_on_every_parser_rung() {
        for rung in ALL_RUNGS {
            let run = run_precompact_hook(&session_start_payload("startup"), &rung);
            assert!(
                run.url()
                    .contains(&format!("/sessions/{SESSION_ID}/context-low")),
                "{rung:?}: {:?}",
                run.curl_args
            );
        }
    }

    /// **F7 — the stop hook's PAYLOAD-side session id never ran**, because the
    /// other stop-hook test always sets `QONTINUI_TERMINAL_ID` and that branch
    /// short-circuits it.
    #[test]
    fn the_stop_hook_reads_the_session_id_from_the_payload_when_there_is_no_terminal_id() {
        for rung in ALL_RUNGS {
            let run = run_hook_argv(
                STOP_HOOK,
                "claude_stop_hook.sh",
                &format!("{{\"session_id\":\"{SESSION_ID}\",\"stop_hook_active\":false}}"),
                &[
                    ("QONTINUI_RUNNER_API_PORT", "9876"),
                    ("QONTINUI_STOP_HOOK_CONTINUATION", "on"),
                ],
                &rung,
            );
            assert!(
                run.url()
                    .contains(&format!("/sessions/{SESSION_ID}/continuation-verdict")),
                "{rung:?}: {:?}",
                run.curl_args
            );
        }
    }

    /// **F3 — a hook must not BLOCK on a stdin that never reaches EOF.**
    /// `cat` does exactly that: measured on one SessionStart event, the policy
    /// hook returned in 2 s on its `read -t 1` while the session hook was
    /// still alive at 6 s, wedging session start rather than degrading it.
    ///
    /// The file redirect every other test uses always reaches EOF instantly,
    /// so it can never catch this — hence the fifo, with a writer held open
    /// well past the budget. A hook that blocks makes `output_with_timeout`
    /// expire, which is the failure.
    #[test]
    fn no_hook_blocks_on_a_stdin_that_never_reaches_eof() {
        const WEDGE_BUDGET: Duration = Duration::from_secs(12);
        let _serial = exe_guard();
        for (name, script) in [
            ("claude_policy_hook.sh", POLICY_HOOK),
            ("claude_session_hook.sh", SESSION_HOOK),
            ("claude_precompact_hook.sh", PRECOMPACT_HOOK),
            ("claude_stop_hook.sh", STOP_HOOK),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let (bin, bash) = isolated_bin(
                tmp.path(),
                &["python3"],
                "exit 0
",
            );
            let hook = tmp.path().join(name);
            write_exe(&hook, script);
            let fifo = tmp.path().join("stdin.fifo");

            let mut cmd = Command::new(&bash);
            cmd.arg("-c")
                .arg(format!(
                    // The writer's own output goes to /dev/null so it cannot
                    // hold the pipe we are reading open past the hook's exit.
                    "mkfifo '{f}'; {{ exec 9>'{f}'; sleep 30; }} >/dev/null 2>&1 & exec '{b}' '{h}' < '{f}'",
                    b = bash.display(),
                    f = fifo.display(),
                    h = hook.display()
                ))
                .env_clear()
                .env("PATH", format!("{}:{}", bin.display(), "/usr/bin:/bin"))
                .env("QONTINUI_RUNNER_API_PORT", "9876")
                .env("QONTINUI_INSTALL_INTERCEPT_PORT", "9876")
                .env("QONTINUI_TERMINAL_ID", "term-1")
                .env("QONTINUI_STOP_HOOK_CONTINUATION", "on");
            assert!(
                output_with_timeout(cmd, WEDGE_BUDGET).is_ok(),
                "{name} BLOCKED on a stdin that never reaches EOF — that wedges \
                 the session instead of degrading it"
            );
        }
    }

    /// The Stop hook is the ONE place a hook hand-builds JSON that Claude then
    /// parses, so a rung that mangles a value corrupts the envelope rather
    /// than merely losing it. A prompt carrying a JSON escape must fall back
    /// to the constant reason on every rung — and whatever comes out must
    /// actually BE JSON.
    ///
    /// This is what pins the capture class: widen it to `[^"]*` and the shell
    /// rung captures `say \` from `"say \"hi\" now"`, emitting a trailing
    /// backslash that makes the envelope unparseable.
    #[test]
    fn the_stop_hook_never_emits_a_corrupt_envelope_for_an_escaped_prompt() {
        for rung in ALL_RUNGS {
            let out = run_stop_hook(r#"{"decision":"block","prompt":"say \"hi\" now"}"#, rung);
            let parsed: serde_json::Value = serde_json::from_str(out.trim())
                .unwrap_or_else(|e| panic!("{rung:?}: not JSON ({e}): {out}"));
            assert_eq!(parsed["decision"], "block", "{rung:?}: {out}");
            let reason = parsed["reason"].as_str().expect("a reason string");
            // Either the rung read the prompt exactly, or it declined to the
            // constant. Never a half-read one.
            assert!(
                reason == r#"say "hi" now"# || reason.starts_with("Are there follow-ups?"),
                "{rung:?}: mangled reason {reason:?}"
            );
        }
    }

    /// **The path guard belongs to EVERY hook, on BOTH arms.** The shipped
    /// version tested only the policy hook against a hostile payload id, so
    /// gutting the guard in the stop and precompact hooks survived the whole
    /// suite — and the precompact hook's environment arm had no guard at all,
    /// on the one hook that `POST --data-binary @-`s the entire payload.
    #[test]
    fn no_hook_lets_a_crafted_id_re_point_the_request_from_either_arm() {
        const HOSTILE: &str = "x/../../control/session-open?z=";
        // `..` alone is the subtle one: the id alphabet PERMITS `.`, so a dot
        // segment passed a guard whose own comment claimed it did not — and
        // curl collapses the segment before sending, which takes the request
        // off `/sessions/<id>/` entirely rather than degrading it.
        // A bare `.` collapses exactly as `..` does — verified against a real
        // listener: `/sessions/./policy-context` arrives as
        // `GET /sessions/policy-context`. Both leave `/sessions/<id>/`.
        for hostile in [HOSTILE, ".", "..", "./x", "../..", "a/b"] {
            for (name, script, port_env) in [
                (
                    "claude_policy_hook.sh",
                    POLICY_HOOK,
                    "QONTINUI_RUNNER_API_PORT",
                ),
                (
                    "claude_precompact_hook.sh",
                    PRECOMPACT_HOOK,
                    "QONTINUI_RUNNER_API_PORT",
                ),
                ("claude_stop_hook.sh", STOP_HOOK, "QONTINUI_RUNNER_API_PORT"),
            ] {
                // ARM 1: the id arrives in the ENVIRONMENT.
                let run = run_hook_argv(
                    script,
                    name,
                    "{}",
                    &[
                        (port_env, "9876"),
                        ("QONTINUI_TERMINAL_ID", hostile),
                        ("QONTINUI_STOP_HOOK_CONTINUATION", "on"),
                    ],
                    &["python3"],
                );
                assert!(
                    run.url().is_empty(),
                    "{name} env arm sent a request for {hostile:?}: {:?}",
                    run.curl_args
                );

                // ARM 2: the id arrives in the PAYLOAD.
                let run = run_hook_argv(
                    script,
                    name,
                    &format!("{{\"session_id\":\"{hostile}\"}}"),
                    &[
                        (port_env, "9876"),
                        ("QONTINUI_STOP_HOOK_CONTINUATION", "on"),
                    ],
                    &["python3"],
                );
                assert!(
                    run.url().is_empty(),
                    "{name} payload arm sent a request for {hostile:?}: {:?}",
                    run.curl_args
                );
            }
        }
    }

    /// Control characters cannot appear RAW in a JSON string, so a newline or
    /// tab in `$PWD` or a payload id emitted a body the route rejects — and
    /// `|| true` on the curl swallowed the rejection, so the record simply
    /// never appeared. A silent degrade rather than a forge, but silent is the
    /// thing this plan is about.
    #[test]
    fn the_session_hook_escapes_control_characters_it_cannot_send_raw() {
        let run = run_hook_argv(
            SESSION_HOOK,
            "claude_session_hook.sh",
            &session_start_payload("resume"),
            &[
                ("QONTINUI_INSTALL_INTERCEPT_PORT", "9876"),
                // MUST carry a byte outside `\n\r\t`. Those three have their own
                // short-escape lines ABOVE the general loop, so a vector of
                // only tab and newline passes with the loop deleted — which is
                // exactly the defect this test exists to pin.
                ("QONTINUI_TERMINAL_ID", "t\t1\nx\u{1}y\u{1b}z\u{7}"),
            ],
            &["python3"],
        );
        let body = posted_body(&run);
        // Escaped, never dropped: dropping COLLIDES — `cle\u{1}ar` would
        // become `clear`, turning an unparseable label into a real one.
        assert!(body.contains(r"\u0001"), "C0 must be \\u-escaped: {body}");
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {body}"));
        assert_eq!(parsed["terminal_id"], "t\t1\nx\u{1}y\u{1b}z\u{7}", "{body}");
    }

    /// Find a locale whose COLLATION reorders the C0 range in an ERE, so the
    /// test below is not vacuous. Panics when the host has none.
    ///
    /// This exists because the whole hook suite runs under `.env_clear()` —
    /// i.e. the C locale, the ONE locale where the bug it guards is
    /// invisible. That made the suite structurally blind to a class, not
    /// merely short a vector, so widening the byte vector could never have
    /// caught it.
    fn c0_reordering_locale(bash: &Path) -> String {
        // BOUNDED like every other spawn in this module: `locale -a` is a
        // one-shot read, but an unbounded `.output()` parks the calling thread
        // forever if it hangs, and this one runs in CI. The loop below already
        // went through `output_with_timeout`; this call was the one that did
        // not (caught by `scripts/check_untimed_subprocess.py`).
        let mut listing = Command::new("locale");
        listing.arg("-a");
        let listed = output_with_timeout(listing, SCRIPT_BUDGET);
        let names: Vec<String> = listed
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for name in names {
            let n = name.trim().to_string();
            if n.is_empty() || n == "C" || n == "POSIX" || n.starts_with("C.") {
                continue;
            }
            let mut cmd = Command::new(bash);
            cmd.arg("-c")
                .arg("c=$(printf '\\x0b'); if [[ $c =~ [$'\\x01'-$'\\x1f'] ]]; then echo IN; else echo OUT; fi")
                .env("LC_ALL", &n);
            if let Ok(out) = output_with_timeout(cmd, SCRIPT_BUDGET) {
                if String::from_utf8_lossy(&out.stdout).trim() == "OUT" {
                    return n;
                }
            }
        }
        panic!(
            "no locale on this host reorders the C0 range in an ERE, so the VT/FF \
             regression cannot be exercised here — install a UTF-8 locale such as \
             en_US.UTF-8 rather than letting this test pass vacuously"
        );
    }

    /// **VT (0x0B) and FF (0x0C) leak RAW under a reordering locale.** The C0
    /// sweep is an ERE, and `shopt globasciiranges` — which keeps a GLOB range
    /// ASCII-ordered everywhere — does not apply to `=~`; bash hands the range
    /// to `regcomp`, which honours `LC_COLLATE`. Under `en_US.UTF-8` the range
    /// excludes 09 0a 0b 0c 0d, and tab/LF/CR have their own escapes above, so
    /// exactly VT and FF escape the sweep, the route rejects the body, and
    /// `|| true` swallows it — the record never appears.
    ///
    /// The predecessor of that loop was a GLOB and had no such hole, so this
    /// is a regression the ERE conversion introduced.
    #[test]
    fn the_session_hook_escapes_c0_under_a_locale_that_reorders_the_range() {
        let bash = which_host("bash").expect("bash is on PATH");
        let locale = c0_reordering_locale(&bash);
        // VT and FF first: those are the two the reordering actually frees.
        const HOSTILE: &str = "a\u{b}b\u{c}c\u{1}d\u{1b}e";
        let run = run_hook_argv(
            SESSION_HOOK,
            "claude_session_hook.sh",
            &session_start_payload("resume"),
            &[
                ("QONTINUI_INSTALL_INTERCEPT_PORT", "9876"),
                ("LC_ALL", locale.as_str()),
                ("LANG", locale.as_str()),
                ("QONTINUI_TERMINAL_ID", HOSTILE),
            ],
            &["python3"],
        );
        let body = posted_body(&run);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| {
            panic!("under {locale} the hook emitted invalid JSON ({e}): {body}")
        });
        assert_eq!(parsed["terminal_id"], HOSTILE, "under {locale}: {body}");
    }
}
