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
//! against v2.1.272), and every seam that delivers the hook already passes the
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
//! ## Spawn-file lifetime
//!
//! Composed files live in [`SPAWN_PROMPTS_DIR`] and are pruned by AGE only
//! (older than [`SPAWN_PROMPT_MAX_AGE`]), never by a liveness guess. A pruned
//! file costs an interactive pane nothing worse than the inline fall-back: the
//! shell wrapper checks existence before choosing the file flag.

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
/// either the old file or the new one and never a torn one.
///
/// The temp file sits in the SAME directory (a rename across volumes is not
/// atomic) and carries the pid plus a random suffix, so two runner instances
/// sharing the machine-global dir never write through the same temp name.
/// `std::fs::rename` replaces an existing destination on Windows as well.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
        f.sync_all()
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
/// Each spawn gets a UNIQUE file, never a shared one: a pane launches `claude`
/// long after the terminal spawned, and a shared name would let a later spawn
/// rewrite the prompt an earlier pane is about to read. Opportunistically
/// prunes old files ([`maybe_prune`]).
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
    let path = dir.join(format!("spawn-{}.md", uuid::Uuid::new_v4().simple()));
    write_atomic(&path, content.as_bytes())?;
    Ok(path)
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
/// `now`. Returns how many were removed. Every error is swallowed: pruning is
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
/// file's PATH plus the SHA; with no file both are REMOVED, so a value
/// inherited from the runner's own environment can never vouch for a delivery
/// that did not happen. `QONTINUI_RUNNER_CONTEXT` is set by the caller as
/// before — it is the wrapper's fall-back and `/whereami`'s source.
pub fn shell_pane_prompt_env(briefing: &str) -> [(&'static str, Option<String>); 2] {
    shell_pane_prompt_env_from(resolve_system_prompt_carrier(Some(briefing.to_string())))
}

/// [`shell_pane_prompt_env`] over an already-resolved carrier (pure).
fn shell_pane_prompt_env_from(
    carrier: Option<SystemPromptCarrier>,
) -> [(&'static str, Option<String>); 2] {
    match carrier {
        Some(SystemPromptCarrier::File { path, policy_sha }) => [
            (
                RUNNER_CONTEXT_FILE_ENV,
                Some(path.to_string_lossy().into_owned()),
            ),
            (POLICY_DELIVERED_SHA_ENV, Some(policy_sha)),
        ],
        _ => [
            (RUNNER_CONTEXT_FILE_ENV, None),
            (POLICY_DELIVERED_SHA_ENV, None),
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
    fn compose_without_a_briefing_is_the_body_alone_and_names_are_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let a = compose_spawn_prompt_in(tmp.path(), None, "BODY").unwrap();
        let b = compose_spawn_prompt_in(tmp.path(), Some("  "), "BODY").unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "BODY");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "BODY");
        assert_ne!(a, b, "every spawn gets its own file");
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
    fn a_shell_pane_exports_both_vars_only_for_the_file_carrier() {
        let file = SystemPromptCarrier::File {
            path: PathBuf::from("/x/spawn-1.md"),
            policy_sha: "ab".repeat(32),
        };
        assert_eq!(
            shell_pane_prompt_env_from(Some(file)),
            [
                (RUNNER_CONTEXT_FILE_ENV, Some("/x/spawn-1.md".to_string())),
                (POLICY_DELIVERED_SHA_ENV, Some("ab".repeat(32))),
            ]
        );
        // Inline or nothing: both REMOVED, so an inherited marker cannot vouch.
        for carrier in [Some(SystemPromptCarrier::Inline("b".into())), None] {
            assert_eq!(
                shell_pane_prompt_env_from(carrier),
                [
                    (RUNNER_CONTEXT_FILE_ENV, None),
                    (POLICY_DELIVERED_SHA_ENV, None)
                ]
            );
        }
    }

    #[test]
    fn the_env_names_are_the_documented_contract() {
        // The shell wrappers, the identity shims and the hook script spell
        // these literally.
        assert_eq!(RUNNER_CONTEXT_FILE_ENV, "QONTINUI_RUNNER_CONTEXT_FILE");
        assert_eq!(POLICY_DELIVERED_SHA_ENV, "QONTINUI_POLICY_DELIVERED_SHA");
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
    /// marker it inherited.
    fn fake_claude(dir: &Path) -> std::path::PathBuf {
        let out = dir.join("claude-invocation.txt");
        write_exe(
            &dir.join("claude"),
            &format!(
                "#!/usr/bin/env bash\n{{ printf 'SHA=[%s]\\n' \"${{QONTINUI_POLICY_DELIVERED_SHA:-}}\"; \
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

    fn assert_wrapper_contract(block: &str, label: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("spawn-1.md");
        std::fs::write(&file, "BRIEFING\n\nBODY").unwrap();
        let file_s = file.to_string_lossy().into_owned();

        // File present: the file flag INSTEAD of the inline one; marker kept.
        let got = run_wrapper(
            block,
            &[
                ("QONTINUI_RUNNER_CONTEXT", "BRIEFING"),
                ("QONTINUI_RUNNER_CONTEXT_FILE", &file_s),
                ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
            ],
            "-p hi",
        );
        assert_eq!(
            got,
            format!("SHA=[{SHA}]\nARG=--append-system-prompt-file\nARG={file_s}\nARG=-p\nARG=hi\n"),
            "{label}: file carrier"
        );

        // File pruned/missing: inline fall-back, marker BLANKED.
        let got = run_wrapper(
            block,
            &[
                ("QONTINUI_RUNNER_CONTEXT", "BRIEFING"),
                ("QONTINUI_RUNNER_CONTEXT_FILE", "/nonexistent/spawn-x.md"),
                ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
            ],
            "-p hi",
        );
        assert_eq!(
            got, "SHA=[]\nARG=--append-system-prompt\nARG=BRIEFING\nARG=-p\nARG=hi\n",
            "{label}: inline fall-back"
        );

        // The caller brought their own system prompt: untouched, marker blanked.
        let got = run_wrapper(
            block,
            &[
                ("QONTINUI_RUNNER_CONTEXT", "BRIEFING"),
                ("QONTINUI_RUNNER_CONTEXT_FILE", &file_s),
                ("QONTINUI_POLICY_DELIVERED_SHA", SHA),
            ],
            "--append-system-prompt mine",
        );
        assert_eq!(
            got, "SHA=[]\nARG=--append-system-prompt\nARG=mine\n",
            "{label}: caller-owned prompt"
        );

        // Nothing at all: bare launch, no marker.
        let got = run_wrapper(block, &[("QONTINUI_POLICY_DELIVERED_SHA", SHA)], "-p hi");
        assert_eq!(got, "SHA=[]\nARG=-p\nARG=hi\n", "{label}: no briefing");
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
        let run = |ctx_file: &str| {
            let script = tmp.path().join("w.ps1");
            std::fs::write(&script, format!("{block}\nclaude -p hi\n")).unwrap();
            let status = Command::new("pwsh")
                .args(["-NoProfile", "-NonInteractive", "-File"])
                .arg(&script)
                .env("PATH", &path)
                .env("QONTINUI_RUNNER_TERMINAL", "1")
                .env("QONTINUI_RUNNER_CONTEXT", "BRIEFING")
                .env("QONTINUI_RUNNER_CONTEXT_FILE", ctx_file)
                .env("QONTINUI_POLICY_DELIVERED_SHA", SHA)
                .status()
                .expect("pwsh runs");
            assert!(status.success());
            std::fs::read_to_string(&out).unwrap()
        };
        let file_s = file.to_string_lossy().into_owned();
        assert_eq!(
            run(&file_s),
            format!("SHA=[{SHA}]\nARG=--append-system-prompt-file\nARG={file_s}\nARG=-p\nARG=hi\n")
        );
        assert_eq!(
            run("/nonexistent/spawn-x.md"),
            "SHA=[]\nARG=--append-system-prompt\nARG=BRIEFING\nARG=-p\nARG=hi\n"
        );
    }

    /// Run the bundled policy hook against a fake `curl` that echoes the URL it
    /// was asked to fetch, and return that URL.
    fn hook_url(sha: Option<&str>) -> String {
        let tmp = tempfile::tempdir().unwrap();
        write_exe(
            &tmp.path().join("curl"),
            "#!/usr/bin/env bash\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '%s' \"$last\"\n",
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
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn the_policy_hook_forwards_only_a_well_formed_delivered_sha() {
        assert_eq!(
            hook_url(Some(SHA)),
            format!("http://127.0.0.1:9876/sessions/term-1/policy-context?delivered_sha={SHA}")
        );
        assert_eq!(
            hook_url(None),
            "http://127.0.0.1:9876/sessions/term-1/policy-context"
        );
        // Empty (the blanked marker), short, or non-hex: not forwarded.
        for bad in ["", "abc", &"z".repeat(64), &format!("{SHA}&x=1")] {
            assert_eq!(
                hook_url(Some(bad)),
                "http://127.0.0.1:9876/sessions/term-1/policy-context",
                "{bad:?}"
            );
        }
    }
}
