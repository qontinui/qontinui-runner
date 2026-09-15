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
    match carrier.as_ref().and_then(SystemPromptCarrier::policy_delivery) {
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
        assert_eq!(a, b, "a blank briefing composes the same bytes, so the same file");
    }

    /// Content-addressed: the name is the content hash, identical spawns share
    /// one file, different content never shares a name, and nothing else (no
    /// temp file) is left in the directory.
    #[test]
    fn composed_files_are_content_addressed_and_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let a = compose_spawn_prompt_in(tmp.path(), Some("BRIEF"), "BODY").unwrap();
        assert_eq!(
            a.file_name().unwrap().to_string_lossy(),
            spawn_prompt_file_name("BRIEF\n\nBODY")
        );
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("spawn-") && name.ends_with(".md"), "{name}");
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
            argv(&["claude", "--system-prompt", "x", "--append-system-prompt-file", "/x/spawn-1.md"]),
            argv(&["claude", "--system-prompt=x"]),
            argv(&["claude", "--system-prompt-file", "/t.md"]),
            argv(&["claude", "--system-prompt-file=/t.md", "--", "p"]),
        ] {
            assert!(argv_carries_replacement_prompt(&with), "{with:?}");
            assert_eq!(delivery_unless_replacement(Some(delivery.clone()), &with), None);
        }
        for without in [
            argv(&["claude", "--append-system-prompt-file", "/x/spawn-1.md", "--", "--system-prompt"]),
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
                (POLICY_DELIVERED_FILE_ENV.to_string(), "/x/spawn-2.md".to_string()),
            ]
        );
        assert_eq!(SystemPromptCarrier::Inline("b".into()).policy_delivery(), None);
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
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    const BASH_INTEGRATION: &str = include_str!("../../resources/shell-integration.bash");
    const ZSH_INTEGRATION: &str = include_str!("../../resources/shell-integration.zsh");
    const PS1_INTEGRATION: &str = include_str!("../../resources/shell-integration.ps1");
    const POLICY_HOOK: &str = include_str!("../../resources/session-restore/claude_policy_hook.sh");
    const IDENTITY_SHIM_BASH: &str = include_str!("../../resources/intercept/identity_shim.bash");

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The `claude` wrapper block of a posix integration script, without the
    /// OSC/prompt plumbing (which writes to `/dev/tty`).
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
        let tmp = tempfile::tempdir().unwrap();
        let out = fake_claude(tmp.path());
        let script = tmp.path().join("wrapper.sh");
        std::fs::write(&script, format!("{block}\nclaude {args}\n")).unwrap();
        let path = format!(
            "{}:{}",
            tmp.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let status = Command::new("bash")
            .arg(&script)
            .env_clear()
            .env("PATH", path)
            .env("QONTINUI_RUNNER_TERMINAL", "1")
            .envs(envs.iter().copied())
            .status()
            .expect("bash runs");
        assert!(status.success());
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
            ("--system-prompt-file=./s.md", vec!["--system-prompt-file=./s.md"]),
            ("-p hi --system-prompt-file ./s.md", vec!["-p", "hi", "--system-prompt-file", "./s.md"]),
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
    fn powershell_wrapper_uses_the_file_carrier_only_when_the_file_exists() {
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
                &["--append-system-prompt", "BRIEFING", "--append-system-prompt", "mine"]
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
                &["--append-system-prompt-file", &file_s, "-p", "--", "--append-system-prompt-file"]
            )
        );
        assert_eq!(
            run(&file_s, "--system-prompt mine"),
            record(
                "",
                "",
                &["--append-system-prompt-file", &file_s, "--system-prompt", "mine"]
            )
        );
    }

    /// Run the rendered bash identity shim as `claude <args>` with the marker
    /// pair set, against a fake real `claude` further down PATH.
    fn run_identity_shim(delivered_file: &str, args: &[&str]) -> String {
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
        let status = Command::new("bash")
            .arg(shim_dir.join("claude"))
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
            .env("QONTINUI_POLICY_DELIVERED_FILE", delivered_file)
            .status()
            .expect("the shim runs");
        assert!(status.success());
        std::fs::read_to_string(out).expect("the real claude ran")
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
            vec!["--append-system-prompt-file", composed, "--", "--system-prompt"],
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
            vec!["--append-system-prompt-file", composed, "--system-prompt", "x"],
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
        let out = cmd.output().expect("the hook runs");
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
        assert!(args.iter().all(|a| !a.contains("delivered_sha")), "{args:?}");
        let h = args.iter().position(|a| a == "-H").expect("a header flag");
        assert_eq!(args[h + 1], format!("X-Qontinui-Policy-Delivered-Sha: {SHA}"));

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
}
