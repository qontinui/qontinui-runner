//! Reclaim-scoped worktree dirtiness.
//!
//! A worktree is "dirty" for reclaim purposes when it holds work that would
//! be LOST by removing it. Plain `git status --porcelain` non-emptiness is a
//! bad proxy for that, because the runner writes its own untracked
//! scaffolding into every agent worktree it allocates:
//!
//! | Path | Written by |
//! |---|---|
//! | `.claude/` | agent scaffolding (agents, settings) materialized per session |
//! | `.coord-mcp-status` | [`crate::coord_mcp`]'s degraded breadcrumb |
//! | `.mcp.json` | the coord-mcp proxy config written at worktree provisioning |
//!
//! None of those are operator work, and every one of them makes an otherwise
//! pristine worktree read as dirty forever. Since both the census
//! ([`super::census`]) and the reclaim executor ([`super::reclaim`]) refuse
//! to act on a dirty worktree, that self-inflicted dirtiness made agent
//! worktrees **permanently unreclaimable**.
//!
//! Measured on the operator's box 2026-07-28, over 5,322 backlog worktrees:
//! 1,696 were dirty from `.claude/` alone and a further 96 from
//! `.coord-mcp-status` / `.mcp.json` — **~34% false-dirty** — against exactly
//! ONE worktree holding genuine uncommitted work. Without this predicate,
//! arming reclaim would silently refuse a third of every future backlog.
//!
//! ## Scope — deliberately NOT applied to the canonical checkout
//!
//! [`super::census::compute_canonical_is_dirty`] and the shared-branch switch
//! guard in [`super`] ask a *different* question: "is it safe to move HEAD in
//! the operator's real checkout?" There, ANY uncommitted state matters,
//! including scaffolding, because a branch switch can carry or clobber it.
//! Those keep plain porcelain non-emptiness. This module is only for the
//! "is it safe to DELETE this disposable worktree?" question.
//!
//! ## Conservative by construction
//!
//! Only **untracked** (`??`) entries are ever ignored, and only when the path
//! is exactly a scaffolding entry or lives beneath a scaffolding directory. A
//! *tracked* modification is always dirty — including one inside `.claude/`
//! (e.g. ` M .claude/settings.json` in a repo that commits its agent config),
//! because that is a real edit to a real file. Anything unparseable is dirty.

/// Untracked paths the runner writes into worktrees it provisions. Matched
/// exactly, or as a directory prefix (`.claude/` covers `.claude/agents/x`).
pub(crate) const RUNNER_SCAFFOLDING: &[&str] = &[".claude", ".coord-mcp-status", ".mcp.json"];

/// Strip git's quoting from a porcelain path. git quotes paths containing
/// spaces or unusual bytes (`?? "a b.txt"`); ordinary paths are bare.
fn unquote(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path)
}

/// Is `path` runner-written scaffolding?
///
/// Matches the entry itself (`.mcp.json`) or anything beneath a scaffolding
/// directory (`.claude/agents/foo.md`, and the bare-directory form `.claude/`
/// git emits when the whole dir is untracked). Deliberately NOT a plain
/// `starts_with`, so a sibling like `.mcp.json.bak` stays dirty.
fn is_scaffolding_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/');
    RUNNER_SCAFFOLDING
        .iter()
        .any(|s| p == *s || p.starts_with(&format!("{s}/")))
}

/// Can this porcelain line be ignored when deciding reclaim-dirtiness?
fn is_ignorable_line(line: &str) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return true;
    }
    // ONLY untracked entries qualify. Tracked changes (" M ", "A  ", "R  ",
    // conflicts, …) are always real.
    match line.strip_prefix("?? ") {
        Some(rest) => is_scaffolding_path(unquote(rest.trim())),
        None => false,
    }
}

/// Reclaim-scoped dirtiness of a `git status --porcelain` payload.
///
/// `true` when at least one line represents work that removal would lose.
/// Pure — the unit-testable core of both call sites.
pub(crate) fn porcelain_is_dirty(porcelain: &str) -> bool {
    porcelain.lines().any(|l| !is_ignorable_line(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tree_is_not_dirty() {
        assert!(!porcelain_is_dirty(""));
        assert!(!porcelain_is_dirty("\n"));
        assert!(!porcelain_is_dirty("   \n\n"));
    }

    /// The whole point: each scaffolding path alone leaves the tree clean.
    #[test]
    fn runner_scaffolding_alone_is_not_dirty() {
        assert!(!porcelain_is_dirty("?? .claude/"));
        assert!(!porcelain_is_dirty("?? .claude/agents/reviewer.md"));
        assert!(!porcelain_is_dirty("?? .coord-mcp-status"));
        assert!(!porcelain_is_dirty("?? .mcp.json"));
        assert!(!porcelain_is_dirty(
            "?? .claude/\n?? .coord-mcp-status\n?? .mcp.json\n"
        ));
    }

    #[test]
    fn real_work_is_dirty() {
        assert!(porcelain_is_dirty(" M src/main.rs"));
        assert!(porcelain_is_dirty("?? src/new_file.rs"));
        assert!(porcelain_is_dirty("A  src/added.rs"));
        assert!(porcelain_is_dirty("R  old.rs -> new.rs"));
        assert!(porcelain_is_dirty("UU conflicted.rs"));
        assert!(porcelain_is_dirty("D  removed.rs"));
    }

    /// A TRACKED edit inside a scaffolding dir is real work — only untracked
    /// entries are ignorable. Guards the obvious over-broad implementation.
    #[test]
    fn tracked_change_under_scaffolding_is_still_dirty() {
        assert!(porcelain_is_dirty(" M .claude/settings.json"));
        assert!(porcelain_is_dirty("M  .mcp.json"));
        assert!(porcelain_is_dirty("A  .claude/agents/new.md"));
    }

    /// Prefix matching must not swallow siblings that merely share a prefix.
    #[test]
    fn scaffolding_prefix_does_not_over_match() {
        assert!(porcelain_is_dirty("?? .mcp.json.bak"));
        assert!(porcelain_is_dirty("?? .claudex/thing"));
        assert!(porcelain_is_dirty("?? .coord-mcp-status-old"));
        assert!(porcelain_is_dirty("?? src/.claude/nested.txt"));
    }

    #[test]
    fn scaffolding_mixed_with_real_work_is_dirty() {
        assert!(porcelain_is_dirty("?? .claude/\n M src/main.rs"));
        assert!(porcelain_is_dirty(" M src/main.rs\n?? .mcp.json"));
    }

    #[test]
    fn quoted_and_crlf_paths_are_handled() {
        assert!(!porcelain_is_dirty("?? \".coord-mcp-status\""));
        assert!(!porcelain_is_dirty("?? .claude/\r\n?? .mcp.json\r\n"));
        assert!(porcelain_is_dirty("?? \"src/a b.rs\""));
    }

    /// Windows-style separators appear in some git configurations.
    #[test]
    fn backslash_separators_are_normalized() {
        assert!(!porcelain_is_dirty("?? .claude\\agents\\x.md"));
    }

    /// Anything we cannot parse as an ignorable untracked entry is dirty —
    /// unknown shapes must never silently authorize a delete.
    #[test]
    fn unparseable_lines_are_dirty() {
        assert!(porcelain_is_dirty("garbage without status code"));
        assert!(porcelain_is_dirty("??no-space-after-code"));
    }
}
