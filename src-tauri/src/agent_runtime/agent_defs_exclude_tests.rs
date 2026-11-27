//! Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
//! Phase 1: what the subagent-definitions pass writes into a session cwd is
//! excluded from the checkout's `git status` (one anchored `info/exclude` line
//! per definition, through `provision_exclude::exclude_written_paths`), and a
//! definition it keeps as somebody else's is never hidden.
//!
//! In a file of its own so an arming proof that reverts the production hunks
//! can rebuild these tests without the rest of `agent_runtime`'s `mod tests`.

#![cfg(test)]

use std::path::Path;

use super::{
    provision_agent_definitions_from_root, provision_agent_definitions_layers,
    provision_agent_definitions_retiring,
};
use crate::provision_guard::test_support::{git_add, git_init};

/// A qontinui-root whose claude-config checkout holds `defs` (the overlay's
/// source). A local copy of the `mod tests` helper of the same shape, so this
/// module builds on its own.
fn checkout_root_with(defs: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let src = root
        .path()
        .join("qontinui-claude-config")
        .join(".claude")
        .join("agents");
    std::fs::create_dir_all(&src).unwrap();
    for (name, body) in defs {
        std::fs::write(src.join(name), body).unwrap();
    }
    root
}

/// A retired definition: a body, and the `RETIRED_AGENTS`-shaped line that
/// retires it, built from that body's own git blob id so no test depends on a
/// blob this binary no longer carries. A local copy of the `mod tests` helper
/// of the same shape, so this module builds on its own.
fn retired_fixture() -> (&'static str, String) {
    let body = "---\nname: retired-agent\n---\n# a body this binary once shipped\n";
    let line = format!(
        "retired-agent.md 0000000000000000000000000000000000000000 {} \
         1111111111111111111111111111111111111111",
        crate::provenance::git_blob_id(body.as_bytes())
    );
    (body, line)
}

/// `git status --porcelain --untracked-files=all` of `dir`.
fn agent_defs_git_status(dir: &Path) -> String {
    let out = crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .expect("git status");
    assert!(out.status.success(), "git status should succeed");
    String::from_utf8(out.stdout).unwrap()
}

/// The `??` entries of [`agent_defs_git_status`], one per element.
fn agent_defs_untracked(dir: &Path) -> Vec<String> {
    agent_defs_git_status(dir)
        .lines()
        .filter(|l| l.starts_with("??"))
        .map(str::to_string)
        .collect()
}

/// The `info/exclude` of a repository [`git_init`]ed at `dir` (a primary
/// checkout, so its own `.git` is the common dir).
fn agent_defs_exclude(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".git").join("info").join("exclude")).unwrap_or_default()
}

/// The anchored exclude line the agents pass writes for definition `name`
/// in a session cwd at a checkout's root.
fn agent_def_line(name: &str) -> String {
    format!("/.claude/agents/{name}")
}

/// Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
/// Phase 1: what BOTH layers wrote — every embedded definition and a name
/// only the checkout overlay carries — gets its own anchored line in the
/// checkout's `info/exclude`, under the provisioned-path marker, with no
/// blanket `.claude/agents/` line; the checkout reads clean, and a second
/// pass leaves the exclude file byte-identical.
#[test]
fn provision_agent_defs_exclude_what_both_layers_wrote() {
    let embedded = crate::fleet_agents::embedded_agent_names();
    let overlay_only = "overlay-only.md";
    assert!(
        !embedded.iter().any(|n| n == overlay_only),
        "fixture: {overlay_only} must not be an embedded name, or the overlay half pins nothing"
    );
    let root = checkout_root_with(&[
        (overlay_only, "# PRIMARY overlay-only\n"),
        ("merge-specialist.md", "# PRIMARY merge-specialist\n"),
    ]);
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    let wt_cwd = wt.path().to_string_lossy().into_owned();

    let report = provision_agent_definitions_from_root(root.path(), &wt_cwd);
    assert_eq!(report.written, 2, "both overlay files are copied");

    let first = agent_defs_exclude(wt.path());
    let lines: Vec<&str> = first.lines().collect();
    for name in embedded.iter().map(String::as_str).chain([overlay_only]) {
        assert!(
            lines.contains(&agent_def_line(name).as_str()),
            "{name} has its own line: {first}"
        );
    }
    assert!(
        lines.contains(&crate::provision_exclude::PROVISIONED_EXCLUDE_MARKER),
        "{first}"
    );
    assert!(
        !lines.iter().any(|l| {
            let t = l.trim().trim_start_matches('/');
            t == ".claude/agents" || t == ".claude/agents/"
        }),
        "no blanket .claude/agents line: {first}"
    );
    assert_eq!(
        agent_defs_git_status(wt.path()),
        "",
        "nothing the pass wrote reads untracked"
    );

    provision_agent_definitions_from_root(root.path(), &wt_cwd);
    assert_eq!(
        agent_defs_exclude(wt.path()),
        first,
        "a second pass leaves the exclude file byte-identical"
    );
    assert_eq!(agent_defs_git_status(wt.path()), "");
}

/// Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
/// Phase 1: a definition a layer did NOT write gets no line, while every one
/// it did write gets one. An existing foreign, untracked `code-reviewer.md`
/// is kept by the floor, so it gets no line and stays visible. A tracked
/// definition the worktree deleted is rewritten by the overlay (its source
/// is untracked, so it is outside the versioning roster), but a tracked path
/// never gets a line either.
#[test]
fn provision_agent_defs_never_exclude_a_kept_definition() {
    let rewritten = "tracked-overlay.md";
    let root = checkout_root_with(&[(rewritten, "# PRIMARY tracked-overlay\n")]);
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    let dst = wt.path().join(".claude").join("agents");
    std::fs::create_dir_all(&dst).unwrap();
    // A person's own reviewer: no blob this runner ships, so the floor keeps it.
    let foreign = "---\nname: code-reviewer\n---\nthe operator's own reviewer\n";
    std::fs::write(dst.join("code-reviewer.md"), foreign).unwrap();
    // Tracked, then deleted from the working tree.
    std::fs::write(dst.join(rewritten), "# WORKTREE's own\n").unwrap();
    git_add(wt.path(), &dst.join(rewritten));
    std::fs::remove_file(dst.join(rewritten)).unwrap();
    let embedded = crate::fleet_agents::embedded_agent_names();
    assert!(
        embedded.iter().any(|n| n == "code-reviewer.md"),
        "fixture: code-reviewer.md must be an embedded name, or the floor keep pins nothing"
    );

    let wt_cwd = wt.path().to_string_lossy().into_owned();
    let report = provision_agent_definitions_from_root(root.path(), &wt_cwd);

    assert_eq!(
        std::fs::read_to_string(dst.join("code-reviewer.md")).unwrap(),
        foreign,
        "the foreign definition is kept"
    );
    assert_eq!(
        std::fs::read_to_string(dst.join(rewritten)).unwrap(),
        "# PRIMARY tracked-overlay\n",
        "the overlay rewrote the tracked-but-deleted definition"
    );
    assert_eq!(report.written, 1, "{:?}", report.skipped);

    let exclude = agent_defs_exclude(wt.path());
    let lines: Vec<&str> = exclude.lines().collect();
    for name in ["code-reviewer.md", rewritten] {
        assert!(
            !lines.contains(&agent_def_line(name).as_str()),
            "{name} was not written by the runner as an untracked file, so it gets no line: \
             {exclude}"
        );
    }
    for name in embedded.iter().filter(|n| *n != "code-reviewer.md") {
        assert!(
            lines.contains(&agent_def_line(name).as_str()),
            "{name} was written, so it has its own line: {exclude}"
        );
    }
    assert_eq!(
        agent_defs_untracked(wt.path()),
        vec!["?? .claude/agents/code-reviewer.md".to_string()],
        "the kept foreign definition is the only untracked entry"
    );
}

/// Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
/// Phase 1: with no workspace root (a published install's normal state) the
/// embedded floor is still written, and still excluded.
#[test]
fn provision_agent_defs_no_root_arm_excludes_the_floor() {
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    let unresolved =
        qontinui_types::paths::resolve_workspace_root(None, None, None, None, None, None)
            .require()
            .expect_err("no input names a workspace root");

    let report = provision_agent_definitions_layers(
        Err(unresolved),
        &wt.path().to_string_lossy(),
        crate::fleet_agents::RETIRED_AGENTS,
    );
    assert_eq!(report.rung, crate::capability_manifest::Rung::Unresolved);

    let exclude = agent_defs_exclude(wt.path());
    let lines: Vec<&str> = exclude.lines().collect();
    let embedded = crate::fleet_agents::embedded_agent_names();
    assert!(!embedded.is_empty(), "the bundle should not be empty");
    for name in &embedded {
        assert!(
            wt.path()
                .join(".claude")
                .join("agents")
                .join(name)
                .is_file(),
            "{name} is written"
        );
        assert!(
            lines.contains(&agent_def_line(name).as_str()),
            "{name} has its own line: {exclude}"
        );
    }
    assert_eq!(agent_defs_git_status(wt.path()), "");
}

/// Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
/// Phase 1: the exclude pass runs AFTER the retirement pass, so a retired
/// definition whose shipped copy is removed loses the line an earlier pass
/// wrote for it in the same pass. Beside it, the production table's
/// `test-generator.md` with an edited body is kept (locally modified) and
/// disowned, so its line goes too and the edit reads untracked.
#[test]
fn provision_agent_defs_prune_a_retired_definitions_line() {
    let (body, line) = retired_fixture();
    let production = crate::fleet_agents::RETIRED_AGENTS
        .iter()
        .copied()
        .find(|l| l.starts_with("test-generator.md "))
        .expect("the production table retires test-generator.md");
    let edited = "---\nname: test-generator\n---\n# edited in this workdir\n";
    let root = tempfile::tempdir().unwrap(); // no qontinui-claude-config
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    let dst = wt.path().join(".claude").join("agents");
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(dst.join("retired-agent.md"), body).unwrap();
    std::fs::write(dst.join("test-generator.md"), edited).unwrap();
    // The lines a pass wrote before either definition was retired.
    let exclude_path = wt.path().join(".git").join("info").join("exclude");
    let mut seeded = agent_defs_exclude(wt.path());
    seeded.push_str(&format!(
        "{}\n{}\n{}\n",
        crate::provision_exclude::PROVISIONED_EXCLUDE_MARKER,
        agent_def_line("retired-agent.md"),
        agent_def_line("test-generator.md"),
    ));
    // A `git init` with no template directory creates no `info/`.
    std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
    std::fs::write(&exclude_path, seeded).unwrap();
    assert_eq!(
        agent_defs_git_status(wt.path()),
        "",
        "fixture: both copies start hidden by their lines"
    );

    let wt_cwd = wt.path().to_string_lossy().into_owned();
    let report =
        provision_agent_definitions_retiring(root.path(), &wt_cwd, &[line.as_str(), production]);

    assert!(
        !dst.join("retired-agent.md").exists(),
        "the shipped retired copy is removed"
    );
    assert_eq!(report.removed.len(), 1, "{:?}", report.removed);
    assert_eq!(
        std::fs::read_to_string(dst.join("test-generator.md")).unwrap(),
        edited,
        "the edited retired copy is kept"
    );
    let exclude = agent_defs_exclude(wt.path());
    let lines: Vec<&str> = exclude.lines().collect();
    assert!(
        !lines.contains(&agent_def_line("retired-agent.md").as_str()),
        "the removed definition's line is pruned in the same pass: {exclude}"
    );
    assert!(
        !lines.contains(&agent_def_line("test-generator.md").as_str()),
        "the edited, retained copy's line is pruned: {exclude}"
    );
    for name in crate::fleet_agents::embedded_agent_names() {
        assert!(
            lines.contains(&agent_def_line(&name).as_str()),
            "{name} has its own line: {exclude}"
        );
    }
    assert_eq!(
        agent_defs_untracked(wt.path()),
        vec!["?? .claude/agents/test-generator.md".to_string()],
        "the edited copy reads untracked again"
    );
}

/// Plan `2026-11-26-runner-provisioned-subagent-definitions-dirty-every-tree-they-are-written-into`
/// Phase 1, D2: once a person edits a provisioned copy, the next pass keeps
/// it as theirs and DISOWNS it, so the line the first pass wrote is pruned
/// and the edit shows in `git status` rather than hiding behind it.
#[test]
fn provision_agent_defs_unexclude_an_edited_copy() {
    let root = tempfile::tempdir().unwrap(); // no qontinui-claude-config
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    let wt_cwd = wt.path().to_string_lossy().into_owned();
    let reviewer = wt
        .path()
        .join(".claude")
        .join("agents")
        .join("code-reviewer.md");

    provision_agent_definitions_from_root(root.path(), &wt_cwd);
    let first = agent_defs_exclude(wt.path());
    assert!(
        first
            .lines()
            .any(|l| l == agent_def_line("code-reviewer.md")),
        "pass 1 excludes the copy it wrote: {first}"
    );
    assert_eq!(agent_defs_git_status(wt.path()), "");

    let edit = "---\nname: code-reviewer\n---\nmy own edits\n";
    std::fs::write(&reviewer, edit).unwrap();
    provision_agent_definitions_from_root(root.path(), &wt_cwd);

    assert_eq!(
        std::fs::read_to_string(&reviewer).unwrap(),
        edit,
        "the edit is kept"
    );
    let exclude = agent_defs_exclude(wt.path());
    let lines: Vec<&str> = exclude.lines().collect();
    assert!(
        !lines.contains(&agent_def_line("code-reviewer.md").as_str()),
        "the edited copy's line is pruned: {exclude}"
    );
    for name in crate::fleet_agents::embedded_agent_names()
        .iter()
        .filter(|n| *n != "code-reviewer.md")
    {
        assert!(
            lines.contains(&agent_def_line(name).as_str()),
            "{name} keeps its line: {exclude}"
        );
    }
    assert_eq!(
        agent_defs_untracked(wt.path()),
        vec!["?? .claude/agents/code-reviewer.md".to_string()],
        "the edit is visible"
    );
}
