//! The single source of truth for the environment a launched agent session
//! inherits from the runner.
//!
//! ## Why this module exists
//!
//! Before it, `QONTINUI_SESSION_WORKTREES` was computed independently at every
//! launch surface (`commands::terminal` ×2, `commands::productivity` ×2,
//! `mcp::terminals`, `mcp::tauri_proxy`, `mcp::backend_relay`) — seven copies of
//! the same `ctx.session_worktrees_env_value().map(|v| vec![(KEY, v)])`
//! expression. The only genuinely shared piece was the *transport*:
//! `TerminalManager::create`'s `extra_env` parameter, which each surface
//! filled in itself.
//!
//! That shape has a silent failure mode: a launch surface added later inherits
//! nothing. It compiles, it spawns, and the session simply never sees the vars
//! — for the plan directories that means an agent writes plans into a *wrong*
//! directory rather than failing loudly, and the archive dir is not derivable
//! from anything the session can see. So the env contribution is centralized
//! here: every launch surface calls [`session_extra_env`], and adding a var is
//! a one-line change that reaches all of them at once.
//!
//! ## What is exported
//!
//! | Var | Source | Omitted when |
//! |---|---|---|
//! | `QONTINUI_SESSION_WORKTREES` | the launch's [`IsolatedEditContext`] | no context / no materialized worktrees |
//! | `QONTINUI_PLANS_DIR` | `QONTINUI_PLAN_ADAPTER_DIR` env → `PathSettings::plans_dir` | markdown-plan tier off |
//! | `QONTINUI_PLANS_ARCHIVE_DIR` | `PathSettings::plans_archive_dir` | no archive location configured |
//!
//! The two plan vars are a **pre-existing contract** this module finally
//! produces: `qontinui-stack/scripts/resolve-plan-deps.py` and the
//! `/plan-graph` skill already consume them. The names are therefore fixed.
//!
//! A var is emitted **only** when its value genuinely resolves; an unset value
//! is omitted rather than exported as an empty string (skills distinguish
//! "unset → use the documented fallback" from "set to nothing").

use super::isolated_edit::{IsolatedEditContext, SESSION_WORKTREES_ENV};
use crate::config_facade::get_setting;
use crate::settings::PathSettings;

/// Environment-variable name carrying the **active** plans directory onto a
/// launched agent session. Unset when the markdown-plan tier is off; consumers
/// that read it must state their own fallback rather than assume a path.
///
/// Pre-existing contract, not introduced here — `qontinui-stack/scripts/
/// resolve-plan-deps.py` and the `/plan-graph` skill already read it. The
/// runner is the producer that was missing.
pub const PLANS_DIR_ENV: &str = "QONTINUI_PLANS_DIR";

/// Environment-variable name carrying the plans **archive** directory onto a
/// launched agent session: an additional location plans may be read from or
/// archived into, per the user's own convention. The runner only carries the
/// value — whether a consumer treats it as a destination or as a legacy lookup
/// path is that consumer's business (the two existing consumers differ). Unset
/// when the setting is unset. Archiving is a file location, never a lifecycle
/// status.
pub const PLANS_ARCHIVE_DIR_ENV: &str = "QONTINUI_PLANS_ARCHIVE_DIR";

/// Normalize a configured directory string: a blank value is *unset*, never a
/// directory named `""`.
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

/// Assemble the env pairs from already-resolved values. Pure — the IO-free
/// core [`session_env`] is built on, and the unit under test for the
/// "every var present / every var omitted" drift guard.
fn build_session_env(
    session_worktrees: Option<String>,
    plans_dir: Option<String>,
    plans_archive_dir: Option<String>,
) -> Vec<(String, String)> {
    [
        (SESSION_WORKTREES_ENV, session_worktrees),
        (PLANS_DIR_ENV, plans_dir),
        (PLANS_ARCHIVE_DIR_ENV, plans_archive_dir),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|v| (key.to_string(), v)))
    .collect()
}

/// The full environment contribution for a session launched with `ctx` (the
/// launch's [`IsolatedEditContext`], or `None` for a shared-checkout launch).
///
/// Callers that build their own `Vec` (e.g. the backend-spawn path, which also
/// appends `CLAUDE_CONFIG_DIR` and the agent git identity) `extend` with this;
/// callers that pass `extra_env` straight through use [`session_extra_env`].
///
/// The plan-directory precedence is delegated to the adapter's
/// `plan_workunit_adapter::resolve_plans_dir`, so the reconcile scan and the
/// sessions it launches can never disagree about which directory is "the plans
/// dir". The archive dir has no env override: it has no legacy env var to stay
/// compatible with and is deliberately not derivable from the active dir (it
/// commonly lives in a different repo).
pub fn session_env(ctx: Option<&IsolatedEditContext>) -> Vec<(String, String)> {
    // One settings read for both directories — `get_setting` re-reads and
    // re-parses the whole settings file on every call.
    let paths = get_setting::<PathSettings>();
    build_session_env(
        ctx.and_then(|c| c.session_worktrees_env_value()),
        qontinui_runner_lib::plan_workunit_adapter::resolve_plans_dir(paths.plans_dir),
        non_blank(paths.plans_archive_dir),
    )
}

/// [`session_env`] in the shape `TerminalManager::create`'s `extra_env`
/// parameter takes: `None` rather than an empty vec when nothing resolves.
pub fn session_extra_env(ctx: Option<&IsolatedEditContext>) -> Option<Vec<(String, String)>> {
    let pairs = session_env(ctx);
    if pairs.is_empty() {
        None
    } else {
        Some(pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The drift guard D2a exists for: the shared helper carries BOTH plan-dir
    /// vars whenever they resolve. Every launch surface routes through
    /// `session_env`/`session_extra_env`, so a surface added later cannot ship
    /// sessions that silently lack the plan directories.
    #[test]
    fn emits_both_plan_dirs_when_configured() {
        let pairs = build_session_env(
            Some("qontinui-runner=/w/runner".to_string()),
            Some("/w/plans".to_string()),
            Some("/w/dev-notes/plans".to_string()),
        );

        assert_eq!(lookup(&pairs, PLANS_DIR_ENV), Some("/w/plans"));
        assert_eq!(
            lookup(&pairs, PLANS_ARCHIVE_DIR_ENV),
            Some("/w/dev-notes/plans")
        );
        assert_eq!(
            lookup(&pairs, SESSION_WORKTREES_ENV),
            Some("qontinui-runner=/w/runner")
        );
        assert_eq!(pairs.len(), 3, "exactly the three known vars");
    }

    #[test]
    fn omits_plan_dirs_when_unset() {
        let pairs = build_session_env(Some("qontinui-runner=/w/runner".to_string()), None, None);

        assert_eq!(lookup(&pairs, PLANS_DIR_ENV), None);
        assert_eq!(lookup(&pairs, PLANS_ARCHIVE_DIR_ENV), None);
        assert_eq!(
            lookup(&pairs, SESSION_WORKTREES_ENV),
            Some("qontinui-runner=/w/runner"),
            "the worktrees var is unaffected by the plan tier being off"
        );
        assert_eq!(pairs.len(), 1);
    }

    /// Never emit an empty-string env var: an unresolved value is an ABSENT
    /// key, which is what the skills' fallback clause keys off.
    #[test]
    fn nothing_resolved_yields_no_pairs() {
        assert!(build_session_env(None, None, None).is_empty());
    }

    /// The plan tier can be on for a shared-checkout launch (no worktrees).
    #[test]
    fn plan_dirs_survive_a_launch_with_no_worktrees() {
        let pairs = build_session_env(None, Some("/w/plans".to_string()), None);
        assert_eq!(lookup(&pairs, SESSION_WORKTREES_ENV), None);
        assert_eq!(lookup(&pairs, PLANS_DIR_ENV), Some("/w/plans"));
        assert_eq!(pairs.len(), 1);
    }

    /// A blank archive setting is treated as unset — never exported as `""`.
    #[test]
    fn blank_archive_setting_is_unset() {
        assert_eq!(non_blank(Some("   ".to_string())), None);
        assert_eq!(non_blank(Some(String::new())), None);
        assert_eq!(
            non_blank(Some("/w/dev-notes/plans".to_string())).as_deref(),
            Some("/w/dev-notes/plans")
        );
        assert!(build_session_env(None, None, non_blank(Some("   ".to_string()))).is_empty());
    }
}
