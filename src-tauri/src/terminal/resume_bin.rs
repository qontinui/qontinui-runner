//! The resume seam's async binary pre-step: resolve the ABSOLUTE `claude`
//! before the sync [`super::account_migration::spawn_resumed_pane`] runs, and
//! never fall back to the bare name.
//!
//! Plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`
//! D3.1 / Phase 5. [`crate::agent_runtime::resolve_claude_bin`] falls back to
//! the bare `claude` when nothing resolves, and Claude Code's own updater
//! reinstalls the npm package often enough that the binary is absent for a real
//! share of wall time. A respawn that resolves inside that window used to get
//! the bare name; the PTY child then found the identity shim first on its own
//! `PATH`, the shim's 10 s wait missed too, and the pane exited 127 (five of
//! sixty migrations on merytshost, every one a lost session).
//!
//! So a miss here is a MISS: the resolver is retried on
//! [`crate::claude_cli_spawn::RETRY_DELAYS`], each attempt off the async worker
//! (`spawn_blocking`, because the resolver stats the filesystem), and a
//! persistent miss is a typed error the callers already handle as a failed
//! spawn. Three shapes count as a miss:
//!
//! - the bare [`crate::agent_runtime::claude_bin_path`] came back — the PATH
//!   walk found nothing, or `launchable_or_bare` refused a non-executable file;
//! - the resolved file is npm's PLACEHOLDER STUB. The package's `install.cjs`
//!   copies the native binary over a `bin/claude.exe` placeholder; between the
//!   two, `claude` exists, is executable, prints "claude native binary not
//!   installed" and exits. `install.cjs` itself tells the stub from the real
//!   binary by size (`< 4096` bytes; the real one is ~290 MB), and so does
//!   [`judge_resolved`] — but only for the package's own binary
//!   (`…/@anthropic-ai/claude-code/bin/claude.exe`, the `bin` field's target,
//!   reached through the symlink npm installs). A small pnpm / asdf / mise shim
//!   or a wrapper script is a valid launcher, not the stub;
//! - the file's size could not be read at all (it vanished between the walk
//!   and the stat).
//!
//! An explicit absolute `QONTINUI_CLAUDE_BIN` override is the operator's (or a
//! test fixture's) choice and is passed through unjudged.
//!
//! **The CLI-update flag seam.** The runner-owned CLI updater
//! ([`crate::claude_cli_update`]) holds an in-progress flag for the length of
//! each install. [`await_cli_update_quiescent`] waits on it before every
//! attempt [`resolve_with_retry`] makes, so a resume never resolves into an
//! install the runner itself is running. The wait is bounded by
//! [`CLI_UPDATE_WAIT`]; past it the resolve's own retry schedule decides.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

/// The size below which a resolved `claude` is npm's placeholder stub rather
/// than the native binary — the same bound the package's own `install.cjs`
/// uses (`statSync(dest).size < 4096`).
pub(crate) const PLACEHOLDER_STUB_MAX_BYTES: u64 = 4096;

/// Why one resolution attempt did not yield a launchable binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveMiss {
    /// The resolver fell back to the bare name.
    Bare,
    /// The resolved file is npm's placeholder stub.
    PlaceholderStub { path: String, len: u64 },
    /// The resolved file's size could not be read.
    Unreadable { path: String },
}

impl ResolveMiss {
    /// The kind token, in the `claude_cli_unavailable:<kind>` grammar.
    fn kind(&self) -> &'static str {
        match self {
            Self::Bare => "not_found",
            Self::PlaceholderStub { .. } => "placeholder_stub",
            Self::Unreadable { .. } => "unreadable",
        }
    }
}

impl std::fmt::Display for ResolveMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bare => write!(f, "no `claude` resolved on the runner's PATH"),
            Self::PlaceholderStub { path, len } => write!(
                f,
                "{path} is npm's {len}-byte placeholder stub (an install is mid-flight)"
            ),
            Self::Unreadable { path } => write!(f, "{path} could not be stat'ed"),
        }
    }
}

/// The package-relative path of the native binary npm's `bin` field points at
/// (`"bin": { "claude": "bin/claude.exe" }`) — the one file `install.cjs`
/// copies over its placeholder. Only a resolved `claude` whose CANONICAL path
/// ends here can be the stub.
const PACKAGE_BINARY_TAIL: [&str; 4] = ["@anthropic-ai", "claude-code", "bin", "claude.exe"];

/// Is `canonical` the npm package's own native binary (the file the
/// placeholder stub sits at mid-install)? A pnpm / asdf / mise shim or a
/// user's wrapper script is a different, legitimately small file.
pub(crate) fn is_npm_package_binary(canonical: &Path) -> bool {
    let names: Vec<&std::ffi::OsStr> = canonical
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect();
    names.len() >= PACKAGE_BINARY_TAIL.len()
        && names[names.len() - PACKAGE_BINARY_TAIL.len()..]
            .iter()
            .zip(PACKAGE_BINARY_TAIL)
            .all(|(have, want)| *have == std::ffi::OsStr::new(want))
}

/// Pure: is `resolved` a usable argv head for the respawn?
///
/// `bare` is [`crate::agent_runtime::claude_bin_path`]; `target` is the
/// resolved file's CANONICAL path (symlinks followed) and size, `None` when it
/// could not be read. The size test applies only when that canonical path is
/// the npm package's own binary ([`is_npm_package_binary`]): a small launcher
/// anywhere else is a shim or wrapper, not the stub.
pub(crate) fn judge_resolved(
    resolved: &str,
    bare: &str,
    target: Option<(&Path, u64)>,
) -> Result<(), ResolveMiss> {
    // An absolute override is launched as-is (the resolver does the same).
    if Path::new(bare).is_absolute() {
        return Ok(());
    }
    if resolved == bare {
        return Err(ResolveMiss::Bare);
    }
    match target {
        None => Err(ResolveMiss::Unreadable {
            path: resolved.to_string(),
        }),
        Some((canonical, len))
            if len < PLACEHOLDER_STUB_MAX_BYTES && is_npm_package_binary(canonical) =>
        {
            Err(ResolveMiss::PlaceholderStub {
                path: resolved.to_string(),
                len,
            })
        }
        Some(_) => Ok(()),
    }
}

/// The resolve-and-wait loop, with its effects injected so it is testable
/// without `PATH`: before EVERY attempt it awaits `await_update` (the CLI-update
/// flag seam), then `resolve_once`; between attempts it sleeps `delays[i]`.
/// `delays.len() + 1` attempts in all. A persistent miss is an error naming the
/// last miss, never the bare name.
pub(crate) async fn resolve_with_retry<W, WF, R, RF>(
    mut await_update: W,
    mut resolve_once: R,
    delays: &[Duration],
) -> Result<String, String>
where
    W: FnMut() -> WF,
    WF: Future<Output = ()>,
    R: FnMut() -> RF,
    RF: Future<Output = Result<String, ResolveMiss>>,
{
    let mut last = ResolveMiss::Bare;
    for attempt in 0..=delays.len() {
        if let Some(d) = attempt.checked_sub(1).and_then(|i| delays.get(i)) {
            tokio::time::sleep(*d).await;
        }
        await_update().await;
        match resolve_once().await {
            Ok(path) => return Ok(path),
            Err(miss) => {
                tracing::info!(
                    attempt = attempt + 1,
                    miss = %miss,
                    "resume-bin: claude did not resolve — retrying after the next delay"
                );
                last = miss;
            }
        }
    }
    Err(format!(
        "{}{} — {last}, after {} attempts; the resume is refused rather than \
         launched through the bare name",
        crate::claude_cli_spawn::CLI_UNAVAILABLE_HEAD,
        last.kind(),
        delays.len() + 1
    ))
}

/// How long a resume waits out a runner-owned CLI install before resolving
/// anyway. An install that outlives it is logged, and the resolve's retry
/// schedule then applies as it would to any other miss.
const CLI_UPDATE_WAIT: Duration = Duration::from_secs(120);

/// Returns once no runner-owned CLI install is in progress, or after
/// [`CLI_UPDATE_WAIT`].
async fn await_cli_update_quiescent() {
    use crate::claude_cli_update::{wait_if_updating, UpdateWait};
    match wait_if_updating(CLI_UPDATE_WAIT).await {
        UpdateWait::NotUpdating => {}
        UpdateWait::Finished(waited) => {
            tracing::info!(?waited, "resume-bin: waited out a runner CLI install");
        }
        UpdateWait::TimedOut(waited) => {
            tracing::warn!(
                ?waited,
                "resume-bin: a runner CLI install is still running — resolving anyway"
            );
        }
    }
}

/// One resolution attempt, off the async worker.
async fn resolve_once_blocking() -> Result<String, ResolveMiss> {
    tokio::task::spawn_blocking(|| {
        let resolved =
            crate::agent_runtime::launchable_or_bare(crate::agent_runtime::resolve_claude_bin());
        let bare = crate::agent_runtime::claude_bin_path();
        let target = std::fs::canonicalize(&resolved)
            .ok()
            .and_then(|c| std::fs::metadata(&c).ok().map(|m| (c, m.len())));
        judge_resolved(
            &resolved,
            &bare,
            target.as_ref().map(|(c, len)| (c.as_path(), *len)),
        )
        .map(|()| resolved)
    })
    .await
    .unwrap_or(Err(ResolveMiss::Bare))
}

/// The absolute, launchable `claude` for a `--resume` respawn, or a typed
/// `claude_cli_unavailable:<kind>` error. Never the bare name.
pub(crate) async fn resolve_resume_bin() -> Result<String, String> {
    resolve_with_retry(
        await_cli_update_quiescent,
        resolve_once_blocking,
        &crate::claude_cli_spawn::RETRY_DELAYS,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const REAL: u64 = 290_532_720;
    const PKG_BIN: &str = "/n/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe";

    fn at(p: &str, len: u64) -> Option<(&Path, u64)> {
        Some((Path::new(p), len))
    }

    #[test]
    fn judge_accepts_a_real_sized_absolute_binary() {
        assert_eq!(
            judge_resolved("/n/bin/claude", "claude", at(PKG_BIN, REAL)),
            Ok(())
        );
    }

    #[test]
    fn judge_refuses_the_bare_name() {
        assert_eq!(
            judge_resolved("claude", "claude", None),
            Err(ResolveMiss::Bare)
        );
    }

    /// Phase 1(c)'s hazard: the stub is executable and present, so only its
    /// size tells it apart — at the package's own binary path.
    #[test]
    fn judge_refuses_npms_placeholder_stub() {
        assert_eq!(
            judge_resolved("/n/bin/claude", "claude", at(PKG_BIN, 600)),
            Err(ResolveMiss::PlaceholderStub {
                path: "/n/bin/claude".into(),
                len: 600
            })
        );
        assert_eq!(
            judge_resolved(
                "/n/bin/claude",
                "claude",
                at(PKG_BIN, PLACEHOLDER_STUB_MAX_BYTES)
            ),
            Ok(()),
            "the bound is install.cjs's own `< 4096`"
        );
    }

    /// Review finding 4: a small launcher that is NOT the package's binary — a
    /// pnpm / asdf / mise shim, a user's wrapper script — is accepted.
    #[test]
    fn judge_accepts_a_small_shim_or_wrapper_that_is_not_the_package_binary() {
        for shim in [
            "/home/u/.local/share/pnpm/claude",
            "/home/u/.asdf/shims/claude",
            "/home/u/.local/share/mise/shims/claude",
            "/home/u/bin/claude",
            "/n/lib/node_modules/@anthropic-ai/claude-code/cli-wrapper.sh",
        ] {
            assert_eq!(
                judge_resolved(shim, "claude", at(shim, 600)),
                Ok(()),
                "{shim}"
            );
        }
    }

    #[test]
    fn the_package_binary_is_recognised_only_by_its_full_tail() {
        assert!(is_npm_package_binary(Path::new(PKG_BIN)));
        assert!(!is_npm_package_binary(Path::new(
            "/x/claude-code/bin/claude.exe"
        )));
        assert!(!is_npm_package_binary(Path::new(
            "/n/lib/node_modules/@anthropic-ai/other/bin/claude.exe"
        )));
    }

    #[test]
    fn judge_refuses_an_unstatable_file() {
        assert!(matches!(
            judge_resolved("/n/bin/claude", "claude", None),
            Err(ResolveMiss::Unreadable { .. })
        ));
    }

    #[test]
    fn judge_passes_an_absolute_override_through() {
        assert_eq!(
            judge_resolved(
                "/fixtures/mock_claude",
                "/fixtures/mock_claude",
                at("/fixtures/mock_claude", 10)
            ),
            Ok(())
        );
    }

    /// miss, miss, hit → the absolute path, after awaiting the update seam
    /// before every attempt.
    #[tokio::test(start_paused = true)]
    async fn resolve_miss_miss_hit_returns_the_absolute_path() {
        let calls = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicUsize::new(0));
        let (c, w) = (calls.clone(), waits.clone());
        let got = resolve_with_retry(
            move || {
                w.fetch_add(1, Ordering::SeqCst);
                async {}
            },
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                async move {
                    match n {
                        0 => Err(ResolveMiss::Bare),
                        1 => Err(ResolveMiss::PlaceholderStub {
                            path: "/n/bin/claude".into(),
                            len: 600,
                        }),
                        _ => Ok("/n/bin/claude".to_string()),
                    }
                }
            },
            &crate::claude_cli_spawn::RETRY_DELAYS,
        )
        .await;
        assert_eq!(got.as_deref(), Ok("/n/bin/claude"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            waits.load(Ordering::SeqCst),
            3,
            "the update seam is awaited before every attempt"
        );
    }

    /// A persistent miss is a typed error — never the bare name.
    #[tokio::test(start_paused = true)]
    async fn resolve_persistent_miss_is_a_typed_error_not_the_bare_name() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let got = resolve_with_retry(
            || async {},
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Err(ResolveMiss::Bare) }
            },
            &crate::claude_cli_spawn::RETRY_DELAYS,
        )
        .await;
        let err = got.expect_err("a persistent miss must not resolve");
        assert!(
            err.starts_with("claude_cli_unavailable:not_found"),
            "typed head: {err}"
        );
        assert_ne!(err, "claude");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            crate::claude_cli_spawn::RETRY_DELAYS.len() + 1
        );
    }
}
