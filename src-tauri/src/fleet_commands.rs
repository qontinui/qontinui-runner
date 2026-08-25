//! The agent command procedures this binary ships, and their provisioning into
//! a spawned session's working directory.
//!
//! `claude` discovers project slash commands from `<cwd>/.claude/commands/*.md`.
//! A gate-continuation session is spawned with a fresh worktree as its cwd, and
//! on most devices there is no `~/.claude/commands` at all, so the fleet
//! vet/implement procedures would be unresolvable. This module BUNDLES the
//! command procedures into the runner binary via `include_str!` and writes them
//! into the session cwd, so the commands resolve regardless of what (if
//! anything) is in the device's home dir.
//!
//! ## The `.md` files in `fleet_commands/` are the CANONICAL sources
//!
//! They are not staged copies of anything. They are ordinary files in this
//! public repository: edit them in place, review the change through a normal
//! pull request, and git history is the tamper record. There is no upstream to
//! re-sync from and no hash to re-pin — a diff in `git log` is the complete
//! account of how a shipped command body came to say what it says.
//!
//! Adding a command is adding a `.md` file next to them plus one line in
//! [`FLEET_COMMANDS`]. Nothing in this module or its consumers may assume the
//! bundle's size, in either direction.
//!
//! ### Where the bodies came from, once
//!
//! 74 of them were imported from `qontinui-claude-config/.claude/commands/` at
//! commit `0ecbd67` when `2026-08-20-fleet-served-agent-skills` Phase 6 filled
//! the bundle. That was a one-time import, not a sync: from here the rule above
//! applies to all of them equally, and there is no job that re-reads that repo.
//!
//! `vet-plan` and `implement-plan` were **not** re-imported. Both trees had
//! edited them independently — measured 2026-08-25, 60 runner-only and 112
//! config-only changed lines on `vet-plan`, 43 and 202 on `implement-plan` —
//! and the runner-only side is a series of reviewed fixes with an in-repo test
//! behind it ([`tests::every_registration_honesty_block_carries_the_warnings_rule`]
//! fails on the config-repo copy of `vet-plan`). Overwriting them would have
//! reverted those. Reconciling the two forks is a content judgement for the
//! operator, not something an embedding change gets to decide.
//!
//! ## Defaults, not the last word
//!
//! What is embedded here is the **default**. A signed-in account may override
//! any command by name, and `crate::agent_commands` resolves
//! `fresh fetch → disk cache → embedded default` before anything is written.
//! Because the default is compiled in, an unauthenticated, offline, or
//! first-run device still gets a working command set and the network is never
//! on the critical path.
//!
//! Because these bodies ship to every fleet device, they must stay free of any
//! one operator's absolute paths — see
//! [`tests::staged_fleet_text_has_no_operator_path_hardcodes`], and
//! [`crate::agent_skills::self_path`] for the shape-based gate that substring
//! list is only a floor for.
//!
//! ## The skills sibling
//!
//! [`crate::fleet_skills`] is this module for `.claude/skills/`. The two must
//! be provisioned from the same set of spawn paths; that is asserted, not
//! assumed.

use std::path::Path;

use tracing::{info, warn};

use crate::agent_commands::AgentCommandRegistry;

/// The embedded default commands, as `(name, body)`. `name` is the slash
/// command `claude` will expose and the filename stem written under
/// `.claude/commands/` (`vet-plan` -> `vet-plan.md` -> `/vet-plan`).
///
/// One line per `.md` file in `fleet_commands/`, sorted by name so a diff that
/// adds a command touches one line. **Never hardcode the length of this slice**
/// — see the module docs.
///
/// ## No underscore-prefixed unit is here, deliberately
///
/// The corpus carries copy-source specs (`_gate-registration`, `_loop-control`)
/// that other units paste from. They are `is_invocable = false` in the store,
/// and `GET /api/v1/agent-text-units`'s own `invocable_only` documentation
/// states the rule this bundle has to obey from the other side: *"a client that
/// PROVISIONS units to disk must pass true: a `_gate-registration.md` written
/// into `.claude/commands/` becomes an invocable slash command."* An embedded
/// default is provisioned to disk unconditionally, so shipping one here would
/// do exactly what the account layer is forbidden to do.
/// [`tests::no_embedded_command_is_a_copy_source_spec`] holds the line.
pub(crate) const FLEET_COMMANDS: &[(&str, &str)] = &[
    ("add-tests", include_str!("fleet_commands/add-tests.md")),
    ("add-types", include_str!("fleet_commands/add-types.md")),
    (
        "analyze-automation",
        include_str!("fleet_commands/analyze-automation.md"),
    ),
    (
        "analyze-subagent",
        include_str!("fleet_commands/analyze-subagent.md"),
    ),
    (
        "ask-operator",
        include_str!("fleet_commands/ask-operator.md"),
    ),
    ("audit", include_str!("fleet_commands/audit.md")),
    ("auto-fix", include_str!("fleet_commands/auto-fix.md")),
    (
        "auto-improve",
        include_str!("fleet_commands/auto-improve.md"),
    ),
    ("auto-review", include_str!("fleet_commands/auto-review.md")),
    ("babysit-prs", include_str!("fleet_commands/babysit-prs.md")),
    ("blocked", include_str!("fleet_commands/blocked.md")),
    (
        "build-mobile-aab",
        include_str!("fleet_commands/build-mobile-aab.md"),
    ),
    (
        "clean-commit",
        include_str!("fleet_commands/clean-commit.md"),
    ),
    ("clean", include_str!("fleet_commands/clean.md")),
    (
        "cleanup-steward",
        include_str!("fleet_commands/cleanup-steward.md"),
    ),
    (
        "code-analyze",
        include_str!("fleet_commands/code-analyze.md"),
    ),
    ("code-fix", include_str!("fleet_commands/code-fix.md")),
    ("coordinate", include_str!("fleet_commands/coordinate.md")),
    ("create-plan", include_str!("fleet_commands/create-plan.md")),
    (
        "create-tutorial",
        include_str!("fleet_commands/create-tutorial.md"),
    ),
    ("debug-loop", include_str!("fleet_commands/debug-loop.md")),
    ("debug", include_str!("fleet_commands/debug.md")),
    ("find-debt", include_str!("fleet_commands/find-debt.md")),
    (
        "find-misplaced",
        include_str!("fleet_commands/find-misplaced.md"),
    ),
    ("fix", include_str!("fleet_commands/fix.md")),
    ("gate-sweep", include_str!("fleet_commands/gate-sweep.md")),
    ("gate", include_str!("fleet_commands/gate.md")),
    (
        "implement-phase",
        include_str!("fleet_commands/implement-phase.md"),
    ),
    (
        "implement-plan",
        include_str!("fleet_commands/implement-plan.md"),
    ),
    ("improve-all", include_str!("fleet_commands/improve-all.md")),
    (
        "manual-test-coord-loop",
        include_str!("fleet_commands/manual-test-coord-loop.md"),
    ),
    (
        "manual-test-coord",
        include_str!("fleet_commands/manual-test-coord.md"),
    ),
    (
        "manual-test-loop",
        include_str!("fleet_commands/manual-test-loop.md"),
    ),
    ("manual-test", include_str!("fleet_commands/manual-test.md")),
    (
        "merge-train-steward",
        include_str!("fleet_commands/merge-train-steward.md"),
    ),
    ("mobile-dev", include_str!("fleet_commands/mobile-dev.md")),
    (
        "mobile-verify",
        include_str!("fleet_commands/mobile-verify.md"),
    ),
    ("mtc", include_str!("fleet_commands/mtc.md")),
    ("name", include_str!("fleet_commands/name.md")),
    ("next-steps", include_str!("fleet_commands/next-steps.md")),
    (
        "organize-notes",
        include_str!("fleet_commands/organize-notes.md"),
    ),
    ("plan-graph", include_str!("fleet_commands/plan-graph.md")),
    ("policy", include_str!("fleet_commands/policy.md")),
    (
        "publish-runner",
        include_str!("fleet_commands/publish-runner.md"),
    ),
    ("pull-all", include_str!("fleet_commands/pull-all.md")),
    ("pull-scoped", include_str!("fleet_commands/pull-scoped.md")),
    ("pvi", include_str!("fleet_commands/pvi.md")),
    ("qa", include_str!("fleet_commands/qa.md")),
    (
        "recursive-automation",
        include_str!("fleet_commands/recursive-automation.md"),
    ),
    (
        "refactor-srp",
        include_str!("fleet_commands/refactor-srp.md"),
    ),
    (
        "reflect-ui-bridge",
        include_str!("fleet_commands/reflect-ui-bridge.md"),
    ),
    (
        "research-plan",
        include_str!("fleet_commands/research-plan.md"),
    ),
    (
        "resume-foreign",
        include_str!("fleet_commands/resume-foreign.md"),
    ),
    (
        "review-before-code",
        include_str!("fleet_commands/review-before-code.md"),
    ),
    (
        "review-commit",
        include_str!("fleet_commands/review-commit.md"),
    ),
    ("review-logs", include_str!("fleet_commands/review-logs.md")),
    (
        "review-plan-next-steps",
        include_str!("fleet_commands/review-plan-next-steps.md"),
    ),
    ("review-plan", include_str!("fleet_commands/review-plan.md")),
    (
        "rewind-session",
        include_str!("fleet_commands/rewind-session.md"),
    ),
    (
        "run-automation",
        include_str!("fleet_commands/run-automation.md"),
    ),
    ("scout", include_str!("fleet_commands/scout.md")),
    (
        "security-scan",
        include_str!("fleet_commands/security-scan.md"),
    ),
    (
        "summarize-session",
        include_str!("fleet_commands/summarize-session.md"),
    ),
    (
        "symbol-claims-warn",
        include_str!("fleet_commands/symbol-claims-warn.md"),
    ),
    (
        "test-ui-bridge",
        include_str!("fleet_commands/test-ui-bridge.md"),
    ),
    ("ufix", include_str!("fleet_commands/ufix.md")),
    ("ui-bridge", include_str!("fleet_commands/ui-bridge.md")),
    ("unattended", include_str!("fleet_commands/unattended.md")),
    ("update-spec", include_str!("fleet_commands/update-spec.md")),
    ("validate", include_str!("fleet_commands/validate.md")),
    (
        "verify-plan-status",
        include_str!("fleet_commands/verify-plan-status.md"),
    ),
    ("verify-web", include_str!("fleet_commands/verify-web.md")),
    ("vet-imp", include_str!("fleet_commands/vet-imp.md")),
    ("vet-plan", include_str!("fleet_commands/vet-plan.md")),
    ("whereami", include_str!("fleet_commands/whereami.md")),
    (
        "workflow-runs",
        include_str!("fleet_commands/workflow-runs.md"),
    ),
];

/// Provision the resolved agent commands into `<workdir>/.claude/commands/` so
/// a `claude` session spawned with `workdir` as its cwd can resolve them as
/// PROJECT-scoped slash commands — even on a device with no
/// `~/.claude/commands`.
///
/// The set written is [`crate::agent_commands::resolve_registry`]'s output:
/// the account's overrides where it has any, the embedded defaults otherwise.
///
/// Fail-soft (mirrors `coord_mcp::provision_coord_mcp_for_session`): any IO
/// error is logged via `tracing::warn!` and swallowed — a provisioning failure
/// must never abort an otherwise-launchable spawn (the session simply lacks the
/// commands, the same state as before this feature). Resolution is fail-soft
/// too: a failed fetch, a rejected credential, a malformed override, or a
/// broken cache each degrade one step and warn, never propagate. Idempotent:
/// existing files are overwritten.
pub(crate) fn provision_fleet_commands_for_session(workdir: &str) {
    let root = Path::new(workdir);
    // Same refusal the skills provisioner applies, for the same reason and on
    // the same resolved target — see `fleet_skills::claude_dir_write_refusal`.
    //
    // This path is not hypothetical and it is not new: with `workdir` = the
    // workspace root, `<root>/.claude` is a SYMLINK into
    // `qontinui-claude-config/.claude/`, where the 78 command bodies are
    // TRACKED SOURCE. Measured on the operator box 2026-08-24:
    // `.claude/commands/vet-plan.md` and `implement-plan.md` were dirty and
    // byte-identical to this module's own `include_str!` copies, i.e. this
    // function had already overwritten the canonical source with the embedded
    // defaults (-352/+105 against HEAD). Writing account-fetched text over a
    // repo's tracked files is a data-loss bug wherever it happens; it also
    // dirties the worktree, and reclaim gate G1 never removes a dirty
    // worktree, which is how ~240/255 coord worktrees were once pinned.
    //
    // Before the fetch, not after: a target we will refuse to write is not
    // worth a network budget on the spawn path.
    if let Some(why) = crate::fleet_skills::claude_dir_write_refusal(root) {
        warn!(
            "fleet_commands: declining to provision agent commands into {} — {why}.              (Continuing spawn; the session resolves whatever commands its cwd already              has.)",
            root.join(".claude").display()
        );
        return;
    }

    let registry = crate::agent_commands::resolve_registry();
    let commands_dir = root.join(".claude").join("commands");
    match provision_fleet_commands_into(&commands_dir, &registry) {
        Ok(written) => {
            info!(
                "fleet_commands: provisioned {written} agent command(s) into {} \
                 ({} account override(s), {} embedded default(s) available)",
                commands_dir.display(),
                registry.override_count(),
                registry.builtin_count(),
            );
        }
        Err(e) => {
            warn!(
                "fleet_commands: failed to provision agent commands into {} \
                 (continuing spawn; the fleet slash commands may not resolve): {e}",
                commands_dir.display()
            );
        }
    }
}

/// Core of [`provision_fleet_commands_for_session`]: create `commands_dir` and
/// write every resolved command into it (overwrite/idempotent), returning the
/// count written. Split out so a unit test can drive it against a tempdir and
/// assert the result — mirroring how `provision_agent_definitions` factored out
/// its `_from_root` core.
fn provision_fleet_commands_into(
    commands_dir: &Path,
    registry: &AgentCommandRegistry,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(commands_dir)?;
    let mut written = 0usize;
    for command in registry.all() {
        let dst = commands_dir.join(command.file_name());
        std::fs::write(&dst, &command.body)?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The commands provisioner must refuse a target whose `.claude/` is
    /// tracked source, exactly as the skills provisioner does.
    ///
    /// This is a REGRESSION test for an overwrite that had already happened.
    /// Measured on the operator box 2026-08-24: with `workdir` = the workspace
    /// root, `<root>/.claude` is a symlink into `qontinui-claude-config/.claude/`,
    /// whose `commands/` holds 78 TRACKED files. `vet-plan.md` and
    /// `implement-plan.md` were dirty there and byte-identical to this module's
    /// own `include_str!` copies — this function had overwritten the canonical
    /// source with the embedded defaults, losing 247 net lines into the working
    /// tree. Fail-soft provisioning must never damage a repo it writes near.
    #[test]
    fn a_repo_that_tracks_dot_claude_is_refused_by_the_commands_provisioner() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let root = tmp.path();
        git(root, &["init", "--initial-branch=main"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "test"]);

        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        // Same NAME as a bundled default, so an unguarded provision overwrites
        // it rather than merely adding a sibling.
        std::fs::write(
            root.join(".claude/commands/vet-plan.md"),
            "# canonical fleet source
",
        )
        .unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "track .claude"]);

        provision_fleet_commands_for_session(&root.to_string_lossy());

        assert_eq!(
            std::fs::read_to_string(root.join(".claude/commands/vet-plan.md")).unwrap(),
            "# canonical fleet source
",
            "the commands provisioner overwrote TRACKED source"
        );
    }

    #[test]
    fn provisions_every_embedded_command_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        let registry = AgentCommandRegistry::new();

        let written = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert_eq!(
            written,
            FLEET_COMMANDS.len(),
            "should provision every embedded command"
        );

        // Every embedded default lands, byte-identically to what
        // `include_str!` embedded.
        for (name, body) in FLEET_COMMANDS {
            let path = commands_dir.join(format!("{name}.md"));
            assert!(path.exists(), "{name}.md should exist");
            let on_disk = std::fs::read_to_string(&path).expect("read command");
            assert!(!on_disk.is_empty(), "{name}.md should be non-empty");
            assert_eq!(
                &on_disk, body,
                "{name}.md must be written byte-identically to the embedded default"
            );
        }

        // Substrings verified present near the top of each bundled file
        // (the `# Vet Plan` / `# Implement Plan` H1 headings).
        let vet_body = std::fs::read_to_string(commands_dir.join("vet-plan.md")).unwrap();
        let implement_body =
            std::fs::read_to_string(commands_dir.join("implement-plan.md")).unwrap();
        assert!(
            vet_body.contains("Vet Plan"),
            "vet-plan.md should contain 'Vet Plan'"
        );
        assert!(
            implement_body.contains("Implement Plan"),
            "implement-plan.md should contain 'Implement Plan'"
        );
    }

    #[test]
    fn provision_is_idempotent_overwrite() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        let registry = AgentCommandRegistry::new();

        assert_eq!(
            provision_fleet_commands_into(&commands_dir, &registry).unwrap(),
            FLEET_COMMANDS.len()
        );
        // Second run over the same dir must succeed (overwrite, not error).
        assert_eq!(
            provision_fleet_commands_into(&commands_dir, &registry).unwrap(),
            FLEET_COMMANDS.len()
        );
    }

    /// An account override must land INSTEAD of the same-named default — one
    /// file per name, carrying the override's body.
    #[test]
    fn override_is_provisioned_in_place_of_the_default() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");

        let (name, default_body) = FLEET_COMMANDS[0];
        let mut registry = AgentCommandRegistry::new();
        registry.set_overrides(vec![qontinui_types::agent_commands::AgentCommand {
            id: "id-1".to_string(),
            organization_id: Some("org-1".to_string()),
            created_by_user_id: None,
            name: name.to_string(),
            body: "# my own procedure\n".to_string(),
            checksum: None,
            is_shared: false,
            current_version: 1,
            created_at: "2026-08-04T00:00:00Z".to_string(),
            updated_at: "2026-08-04T00:00:00Z".to_string(),
        }]);

        let written = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert_eq!(
            written,
            FLEET_COMMANDS.len(),
            "an override replaces a default; it does not add a file"
        );
        let on_disk = std::fs::read_to_string(commands_dir.join(format!("{name}.md"))).unwrap();
        assert_eq!(on_disk, "# my own procedure\n");
        assert_ne!(on_disk, default_body);
    }

    /// The gate-registration mechanics every bundled command that teaches
    /// registration must carry, as `(token, why it is load-bearing)`.
    ///
    /// These reached the bundle late (2026-08-08) and only after a lint in a
    /// DIFFERENT repository — `qontinui-claude-config`'s
    /// `lint-command-frontmatter.py` check #15b — was pointed at this tree by
    /// hand. That guard cannot hold the line here: it needs a
    /// `qontinui-claude-config` checkout to exist and be current, and no job in
    /// this repo's CI runs it. Since these bodies are what actually ship (see
    /// the module doc), the invariant is asserted where the files live.
    const GATE_REGISTRATION_MECHANICS: &[(&str, &str)] = &[
        (
            "gate_class",
            "decides WHO MAY CLEAR the gate (coord's per-tenant `gate_clearance` \
             matrix); a copy that omits it teaches sessions to register \
             unclassified gates, which is how the matrix stayed dark fleet-wide",
        ),
        (
            "initial_verdict_reason",
            "how a session tells a REGISTERED-BUT-NOT-USABLE gate from a usable \
             one; without it a returned `gate_id` reads as sufficient and an \
             unevaluable gate rots `open` with nothing escalating on it",
        ),
    ];

    /// Every bundled command that documents gate registration must teach the
    /// mechanics in [`GATE_REGISTRATION_MECHANICS`].
    ///
    /// Scoped by CONTENT, not by filename, so the guard covers whatever the
    /// bundle grows into — the module doc's "nothing may assume the bundle is
    /// two commands" applies to its tests too. A command that never registers a
    /// gate is simply not in scope.
    #[test]
    fn bundled_gate_registration_commands_teach_the_mechanics() {
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if !contents.contains("coord_register_gate") {
                continue;
            }
            checked += 1;
            for (token, why) in GATE_REGISTRATION_MECHANICS {
                assert!(
                    contents.contains(token),
                    "bundled agent command {name} documents gate registration but never \
                     mentions {token:?} — {why}. This file is provisioned into every \
                     spawned session and on a device with no qontinui-claude-config \
                     checkout it is the ONLY copy, so a mechanic missing here is a \
                     mechanic the fleet does not have; add it in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        assert!(
            checked > 0,
            "no bundled command mentions `coord_register_gate` — either the bundle lost \
             its gate-registration procedures or this guard's content probe went stale"
        );
    }

    /// Token presence is a floor, not proof of coverage: a file can mention a
    /// mechanic once and still leave a whole registration path teaching the
    /// superseded rule. That is exactly what happened — the 2026-08-08 carry
    /// satisfied a token-presence lint with a single mention while `vet-plan`'s
    /// flagged-items path still said a returned `gate_id` was the test.
    ///
    /// So assert it structurally: each "Masked-tool honesty" block that is
    /// about REGISTRATION must also carry the warnings rule. The two are the
    /// same class of false positive — a call that looks like it registered a
    /// gate and did not — and a path that teaches one without the other tells a
    /// session to report an unclearable gate as gated.
    ///
    /// Blocks are delimited by the next such bullet so an attest-side block
    /// (`coord_attest_gate`, which has no registration warnings) stays out of
    /// scope on its own content.
    #[test]
    fn every_registration_honesty_block_carries_the_warnings_rule() {
        const HONESTY: &str = "**Masked-tool honesty";
        for (name, contents) in FLEET_COMMANDS {
            let starts: Vec<usize> = contents.match_indices(HONESTY).map(|(i, _)| i).collect();
            for (n, &start) in starts.iter().enumerate() {
                let end = starts.get(n + 1).copied().unwrap_or(contents.len());
                let block = &contents[start..end];
                if !block.contains("coord_register_gate") {
                    continue; // attest-side, or some other honesty block
                }
                assert!(
                    block.contains("initial_verdict_reason"),
                    "bundled agent command {name}: the registration \"Masked-tool honesty\" \
                     block at byte {start} teaches that a returned `gate_id` is the test, \
                     but never says a `gate_id` carrying `warnings[]` / a \"cannot evaluate\" \
                     `initial_verdict_reason` is a REGISTERED-BUT-NOT-USABLE gate. Every \
                     registration path needs both rules, not just the file as a whole; add \
                     the Warnings-honesty bullet to this path in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
    }

    /// An operator-ABSOLUTE path into this workspace, in any of the three
    /// spellings a Windows box produces: drive (`D:/…`, `D:\…`), MSYS (`/d/…`)
    /// and WSL (`/mnt/d/…`). This is `lint-command-frontmatter.py` check #4's
    /// `OPERATOR_PATH_RE`, ported literal for literal — see
    /// [`staged_fleet_text_has_no_operator_path_hardcodes`].
    static OPERATOR_PATH_RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| {
            regex::Regex::new(r"(?i)(?:[A-Za-z]:[/\\]|/(?:mnt/)?[a-z]/)qontinui-root")
                .expect("operator-path regex")
        });

    /// Check #4's per-line opt-out, matched as the same literal. It marks a line
    /// that is *genuinely* operator-only — the mobile AAB flow's WSL mount and
    /// Play-Console paths are the whole of it in this corpus (7 lines in
    /// `build-mobile-aab.md`).
    const OPERATOR_LOCAL_MARKER: &str = "operator-local";

    /// The config-repo reach, unconditional and in both separator spellings
    /// (a Windows-authored body writes the second). This is arm A of
    /// [`crate::agent_skills::self_path`], kept here so it also covers the
    /// COMMANDS bundle, which that module does not scan.
    const CONFIG_REPO_REACH: &[&str] = &[
        "qontinui-claude-config/.claude",
        "qontinui-claude-config\\.claude",
    ];

    /// The bundled text must never carry an operator-absolute path into this
    /// workspace, or a reach into the operator's `qontinui-claude-config`
    /// checkout. Whatever is here ships to every fleet device, and a path that
    /// exists on one machine is a step that silently does nothing on the rest.
    ///
    /// ## This is check #4, ported — not a substring list of its own
    ///
    /// The list this test used to carry (`qontinui-dev-notes/plans`,
    /// `qontinui-root/plans`, `D:/qontinui-root`) was written when the bundle
    /// was two files, and it does not survive the real corpus in either
    /// direction. Measured 2026-08-25 over the 74 imported commands and 9
    /// skills:
    ///
    /// * It **over-matches**. `qontinui-dev-notes/plans` appears 9 times as an
    ///   ordinary repo-relative citation of a plan document
    ///   (`cleanup-steward.md` ×4, five `SKILL.md`s), which is exactly the
    ///   spelling `CLAUDE.md` itself uses. Failing those would make the guard
    ///   refuse working text, and a guard that cries wolf is a guard that gets
    ///   deleted.
    /// * It **under-matches**. It knows one of the three absolute spellings.
    ///   `/mnt/d/qontinui-root` and `D:\qontinui-root` — both live in
    ///   `build-mobile-aab.md` — match none of its patterns.
    ///
    /// `qontinui-claude-config`'s `lint-command-frontmatter.py` check #4
    /// already states this rule correctly for the tree these bodies came from:
    /// one shape regex over all three spellings, plus a per-line
    /// `operator-local` marker for the audited residuals. Porting it — the same
    /// arrangement, and the same reasoning, as
    /// [`crate::agent_skills::self_path`]'s port of check #22 — means a body
    /// that repo's lint accepts is one this bundle accepts, and vice versa.
    /// **When one changes, change both.**
    ///
    /// The shape rule is still only half the gate: it says nothing about
    /// whether a unit can reach *its own* files once provisioned. That is
    /// [`crate::agent_skills::self_path`], which runs over every embedded skill
    /// via [`crate::fleet_skills::tests::embedded_skills_reach_their_own_files`].
    #[test]
    fn staged_fleet_text_has_no_operator_path_hardcodes() {
        fn scan(unit: &str, path: &str, text: &str, failures: &mut Vec<String>) {
            for (n, line) in text.lines().enumerate() {
                if OPERATOR_PATH_RE.is_match(line) && !line.contains(OPERATOR_LOCAL_MARKER) {
                    failures.push(format!(
                        "{unit} {path}:{}: operator-absolute workspace path — rewrite it as \
                         `<workspace-root>/…` or derive the root at runtime, or append the \
                         `{OPERATOR_LOCAL_MARKER}` marker if the line is genuinely \
                         operator-only: {}",
                        n + 1,
                        line.trim()
                    ));
                }
                for pat in CONFIG_REPO_REACH {
                    if line.contains(pat) {
                        failures.push(format!(
                            "{unit} {path}:{}: reaches into a `qontinui-claude-config` \
                             checkout ({pat:?}) — a fleet device has none: {}",
                            n + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }

        let mut failures = Vec::new();
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            checked += 1;
            scan(
                "bundled agent command",
                &format!("src-tauri/src/fleet_commands/{name}.md"),
                contents,
                &mut failures,
            );
        }
        for skill in crate::fleet_skills::FLEET_SKILLS {
            for (path, text) in skill.files {
                checked += 1;
                scan(
                    "bundled agent skill",
                    &format!("src-tauri/src/fleet_skills/{}/{path}", skill.name),
                    text,
                    &mut failures,
                );
            }
        }
        assert!(
            failures.is_empty(),
            "{} operator-path violation(s) in the embedded bundles:\n{}",
            failures.len(),
            failures.join("\n")
        );
        assert!(
            checked > 0,
            "the guard scanned nothing — the embedded bundles have gone missing"
        );
    }

    /// **The guard must be able to fail.** A regex that quietly stops matching
    /// reads exactly like a clean corpus, and this guard's whole subject is a
    /// defect that already reads as clean. Samples are check #4's own
    /// known-bad/known-good pairs plus the two spellings this corpus actually
    /// contains.
    #[test]
    fn the_operator_path_guard_still_fires() {
        for bad in [
            r"cp /mnt/d/qontinui-root/qontinui-mobile/google-services.json .",
            r"adb install -r -d D:\qontinui-root\qontinui-mobile\build-output-signed.apk",
            r"read D:/qontinui-root/plans/2026-08-20-fleet-served-agent-skills.md",
            r"bash /d/qontinui-root/qontinui-claude-config/scripts/x.sh",
        ] {
            assert!(
                OPERATOR_PATH_RE.is_match(bad),
                "known-BAD sample not flagged: {bad}"
            );
        }
        for good in [
            r"- `<workspace-root>/qontinui-dev-notes/plans/2026-05-21-pr-merge-orchestrator-design.md` —",
            r"   `$QONTINUI_PLANS_DIR` when it resolves; otherwise they land in `qontinui-dev-notes/plans`,",
            r"`qontinui-claude-config/knowledge-base/qontinui-specific/coord-merge-train.md`",
            r"bash <path-to-this-skill-dir>/coord-revive.sh",
        ] {
            assert!(
                !OPERATOR_PATH_RE.is_match(good),
                "known-GOOD sample flagged: {good}"
            );
        }
        // And the marker is what exempts a line, not the pattern going away.
        let marked = r"cp /mnt/d/qontinui-root/x .  # operator-local: Play-Console flow";
        assert!(OPERATOR_PATH_RE.is_match(marked));
        assert!(marked.contains(OPERATOR_LOCAL_MARKER));
    }

    /// No embedded command may be an underscore-prefixed copy-source spec.
    ///
    /// `_gate-registration` and `_loop-control` are carried by the corpus for
    /// other units to paste from and are `is_invocable = false` in the store.
    /// `GET /api/v1/agent-text-units`'s `invocable_only` parameter documents the
    /// rule from the account side — *"a client that PROVISIONS units to disk
    /// must pass true"* — and an embedded default is provisioned
    /// unconditionally, so the bundle has to obey the same rule with no query
    /// parameter to lean on.
    #[test]
    fn no_embedded_command_is_a_copy_source_spec() {
        for (name, _) in FLEET_COMMANDS {
            assert!(
                !name.starts_with('_'),
                "embedded command {name:?} is a copy-source spec: writing it to \
                 .claude/commands/{name}.md makes the harness offer it as /{name}, which is \
                 precisely what `is_invocable = false` exists to prevent"
            );
        }
    }
}
