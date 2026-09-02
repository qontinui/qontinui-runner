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
//! bundle is two commands.
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
//! [`tests::staged_fleet_commands_have_no_plan_path_hardcodes`].

use std::path::Path;

use tracing::{info, warn};

use crate::agent_commands::AgentCommandRegistry;

/// `/vet-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/vet-plan.md` in this repository — edit it
/// there.
const VET_PLAN: &str = include_str!("fleet_commands/vet-plan.md");

/// `/implement-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/implement-plan.md` in this repository — edit
/// it there.
const IMPLEMENT_PLAN: &str = include_str!("fleet_commands/implement-plan.md");

/// `/policy` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/policy.md` in this repository — edit it there.
///
/// One of the five COORD DOORS added to the bundle: the read door for the
/// fleet policy documents. Bundled rather than left to the account layer
/// because the override fetch itself needs a reachable backend — an agent that
/// cannot reach the network is exactly the agent that needs to read policy and
/// report why it is stuck.
const POLICY: &str = include_str!("fleet_commands/policy.md");

/// `/gate` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/gate.md` in this repository — edit it there.
///
/// The transport-agnostic gate register/attest/withdraw door. Same reasoning
/// as [`POLICY`]: registering a gate is how a blocked agent makes its blocker
/// observable, so it must not itself depend on a healthy transport.
const GATE: &str = include_str!("fleet_commands/gate.md");

/// `/whereami` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/whereami.md` in this repository — edit it
/// there.
///
/// Reports session IDENTITY from `$QONTINUI_RUNNER_CONTEXT` (never a port
/// probe). Bundled because it answers "what am I running inside" — a question
/// whose answer must not depend on the thing being diagnosed.
const WHEREAMI: &str = include_str!("fleet_commands/whereami.md");

/// `/blocked` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/blocked.md` in this repository — edit it
/// there.
///
/// The session-close emit-on-block protocol. This is the LAST thing a stuck
/// session runs, so it is the one command least able to rely on a fetch having
/// succeeded earlier.
const BLOCKED: &str = include_str!("fleet_commands/blocked.md");

/// `/gate-sweep` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/gate-sweep.md` in this repository — edit it
/// there.
///
/// Reports open/closed gates. Bundled alongside [`GATE`] and [`BLOCKED`] so the
/// register/report pair is never half-present.
const GATE_SWEEP: &str = include_str!("fleet_commands/gate-sweep.md");

/// The embedded default commands, as `(name, body)`. `name` is the slash
/// command `claude` will expose and the filename stem written under
/// `.claude/commands/` (`vet-plan` -> `vet-plan.md` -> `/vet-plan`).
pub(crate) const FLEET_COMMANDS: &[(&str, &str)] = &[
    ("vet-plan", VET_PLAN),
    ("implement-plan", IMPLEMENT_PLAN),
    ("policy", POLICY),
    ("gate", GATE),
    ("whereami", WHEREAMI),
    ("blocked", BLOCKED),
    ("gate-sweep", GATE_SWEEP),
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
/// broken cache each degrade one step and warn, never propagate.
///
/// Idempotent, and existing files are overwritten — EXCEPT where the
/// destination is already tracked by the enclosing git repository, which is
/// skipped (see [`provision_fleet_commands_into`] and
/// [`crate::provision_guard`]).
pub(crate) fn provision_fleet_commands_for_session(workdir: &str) {
    let registry = crate::agent_commands::resolve_registry();
    let commands_dir = Path::new(workdir).join(".claude").join("commands");
    match provision_fleet_commands_into(&commands_dir, &registry) {
        Ok(Provisioned { written, skipped }) => {
            info!(
                "fleet_commands: provisioned {written} agent command(s) into {} \
                 ({skipped} skipped as git-tracked; \
                 {} account override(s), {} embedded default(s) available)",
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

/// Outcome of one provisioning pass: how many command files were written, and
/// how many were left alone because the destination is tracked by the enclosing
/// git repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Provisioned {
    /// Files written (overwriting whatever was there).
    pub(crate) written: usize,
    /// Files NOT written because the destination exists and is git-tracked.
    pub(crate) skipped: usize,
}

/// Core of [`provision_fleet_commands_for_session`]: create `commands_dir` and
/// write every resolved command into it, returning the counts. Split out so a
/// unit test can drive it against a tempdir and assert the result — mirroring
/// how `provision_agent_definitions` factored out its `_from_root` core.
///
/// Idempotent (a second pass over the same dir overwrites rather than errors),
/// with ONE exception: a destination that already exists AND is tracked in the
/// enclosing git repository is skipped, logged at `info!`, and counted in
/// [`Provisioned::skipped`]. Spawning a session with such a checkout as its cwd
/// used to replace the repo's own content with the binary's embedded copy and
/// leave the tree dirty.
///
/// **This is not only the narrow two-file case.** `qontinui-dev-notes` tracks
/// exactly `vet-plan.md` + `implement-plan.md`, but `qontinui-claude-config`
/// tracks all seven — so a session spawned there provisions ZERO commands, and
/// a tracked file outranks an account override. The measured table, and why
/// that is the intended outcome, are in [`crate::provision_guard`]'s module doc.
///
/// **Fail-soft, and this is a hard requirement.** The tracked probe
/// ([`crate::provision_guard::TrackedPaths::probe`]) resolves EVERY failure — an
/// unreadable or absent git dir, no `git` binary, any non-zero exit, and a `git`
/// that hangs — to "nothing tracked", i.e. to writing exactly as before. A
/// skipped write must never become an aborted spawn, and a failed or slow probe
/// must never become one either. The probe runs ONCE per pass, not once per
/// file, so this costs one process spawn rather than seven.
fn provision_fleet_commands_into(
    commands_dir: &Path,
    registry: &AgentCommandRegistry,
) -> std::io::Result<Provisioned> {
    std::fs::create_dir_all(commands_dir)?;
    let tracked = crate::provision_guard::TrackedPaths::probe(commands_dir);
    let mut out = Provisioned::default();
    for command in registry.all() {
        let file_name = command.file_name();
        let dst = commands_dir.join(&file_name);
        if tracked.should_skip(&dst, Path::new(&file_name)) {
            info!(
                "fleet_commands: skipping {} — it is tracked by the enclosing git \
                 repository, and overwriting it would silently replace that repo's \
                 own content and dirty its tree",
                dst.display()
            );
            out.skipped += 1;
            continue;
        }
        std::fs::write(&dst, &command.body)?;
        out.written += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_every_embedded_command_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        let registry = AgentCommandRegistry::new();

        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert_eq!(
            out.written,
            FLEET_COMMANDS.len(),
            "should provision every embedded command"
        );
        assert_eq!(out.skipped, 0, "nothing here is git-tracked");

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
            provision_fleet_commands_into(&commands_dir, &registry)
                .unwrap()
                .written,
            FLEET_COMMANDS.len()
        );
        // Second run over the same dir must succeed (overwrite, not error).
        assert_eq!(
            provision_fleet_commands_into(&commands_dir, &registry)
                .unwrap()
                .written,
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

        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert_eq!(
            out.written,
            FLEET_COMMANDS.len(),
            "an override replaces a default; it does not add a file"
        );
        let on_disk = std::fs::read_to_string(commands_dir.join(format!("{name}.md"))).unwrap();
        assert_eq!(on_disk, "# my own procedure\n");
        assert_ne!(on_disk, default_body);
    }

    /// A destination that is TRACKED by the enclosing git repository must be
    /// left alone. `qontinui-claude-config` and `qontinui-dev-notes` both track
    /// exactly `.claude/commands/vet-plan.md` and `implement-plan.md`; spawning
    /// a session with either as its cwd used to replace the repo's own content
    /// with this binary's embedded copy and leave the tree dirty.
    #[test]
    fn a_git_tracked_destination_is_skipped_not_clobbered() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        // One command file is the repo's own, tracked content.
        let (tracked_name, _) = FLEET_COMMANDS[0];
        let tracked = commands_dir.join(format!("{tracked_name}.md"));
        std::fs::write(&tracked, b"# the repo's own body\n").unwrap();
        crate::provision_guard::test_support::git_add(tmp.path(), &tracked);

        let registry = AgentCommandRegistry::new();
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");

        assert_eq!(out.skipped, 1, "the tracked file should be skipped");
        assert_eq!(
            out.written,
            FLEET_COMMANDS.len() - 1,
            "every OTHER command should still be written"
        );
        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "# the repo's own body\n",
            "a tracked destination must keep the repo's content, not the embedded copy"
        );
    }

    /// The blast radius the module doc names: a checkout that tracks EVERY
    /// bundled command provisions zero of them. `qontinui-claude-config` is
    /// exactly that repo (measured 2026-08-30: it tracks all seven), so this is
    /// the shipped behaviour there, not a corner case — and it is the one the
    /// single-file test above cannot show.
    #[test]
    fn a_checkout_tracking_every_command_provisions_none_of_them() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        for (name, _) in FLEET_COMMANDS {
            let dst = commands_dir.join(format!("{name}.md"));
            std::fs::write(&dst, format!("# repo body for {name}\n")).unwrap();
            crate::provision_guard::test_support::git_add(tmp.path(), &dst);
        }

        let registry = AgentCommandRegistry::new();
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");

        assert_eq!(out.written, 0, "every destination is tracked");
        assert_eq!(out.skipped, FLEET_COMMANDS.len());
        for (name, _) in FLEET_COMMANDS {
            assert_eq!(
                std::fs::read_to_string(commands_dir.join(format!("{name}.md"))).unwrap(),
                format!("# repo body for {name}\n")
            );
        }
    }

    /// A tracked destination outranks an ACCOUNT OVERRIDE too. The override
    /// layer resolves first (`fresh fetch -> disk cache -> embedded default`),
    /// and this guard then skips whatever won — so in a tracked checkout the
    /// override is inert. That follows from the rule (an override written over
    /// tracked content dirties the tree exactly as a default would), but nothing
    /// in the log line says "an override lost", so it is pinned here.
    #[test]
    fn a_tracked_destination_outranks_an_account_override() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        let (name, _) = FLEET_COMMANDS[0];
        let dst = commands_dir.join(format!("{name}.md"));
        std::fs::write(&dst, b"# the repo's own body\n").unwrap();
        crate::provision_guard::test_support::git_add(tmp.path(), &dst);

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

        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");

        assert_eq!(out.skipped, 1);
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "# the repo's own body\n",
            "the override must not be written over tracked content either"
        );
    }

    /// The untracked arm: same repo, same directory, but the file is not in the
    /// index — so the pre-existing overwrite behaviour is unchanged. This is the
    /// arm that keeps a fresh agent worktree (nothing tracked under `.claude/`)
    /// fully provisioned.
    #[test]
    fn an_untracked_destination_inside_a_repo_is_still_written() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        let (name, body) = FLEET_COMMANDS[0];
        let dst = commands_dir.join(format!("{name}.md"));
        std::fs::write(&dst, b"stale, untracked\n").unwrap();
        // Deliberately NOT `git add`ed.

        let registry = AgentCommandRegistry::new();
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");

        assert_eq!(out.skipped, 0, "nothing is tracked, so nothing is skipped");
        assert_eq!(out.written, FLEET_COMMANDS.len());
        assert_eq!(
            &std::fs::read_to_string(&dst).unwrap(),
            body,
            "an untracked destination is overwritten exactly as before"
        );
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

    /// The ONE statement of the REGISTERED-BUT-NOT-USABLE test, interpolated
    /// into the assertion messages below so the rule is written down once.
    ///
    /// The `qontinui-claude-config` twin of these guards learned this the hard
    /// way on 2026-08-31: the rule had been hand-copied into three runtime
    /// sites of `lint-registration-warnings-honesty.py`, the narrowing updated
    /// one of them, and the check then fired and instructed the author to write
    /// back the exact test the narrowing had just removed. That was closed by
    /// `qontinui-claude-config#531`, which states the rule once as a constant
    /// interpolated into all three sites. This is the same mechanism on the
    /// Rust side: the guards below cite the rule from here, so no copy of it
    /// can age separately from another.
    const NOT_USABLE_TEST: &str = "a returned `gate_id` is REGISTERED-BUT-NOT-USABLE when \
         `initial_verdict_reason` says the predicate cannot be evaluated, or when \
         `initial_verdict` is a terminal state it can never clear from (`misconfigured` / \
         `failed`). A non-empty `warnings[]` is NOT that signal - read the warnings, do not \
         count them; that half of the rule was narrowed away as over-broad on 2026-08-31";

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
    ///
    /// SCOPE, stated so it is not mistaken for whole-bundle coverage: only
    /// `vet-plan` (twice) and `implement-plan` state the rule inside a
    /// "Masked-tool honesty" block, so exactly three blocks are in scope here.
    /// `blocked` and `gate` carry the rule outside such a block and are covered
    /// instead by
    /// [`tests::no_bundled_command_revives_the_retired_warnings_emptiness_test`],
    /// which is file-scoped over the whole bundle.
    #[test]
    fn every_registration_honesty_block_carries_the_warnings_rule() {
        const HONESTY: &str = "**Masked-tool honesty";
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            let starts: Vec<usize> = contents.match_indices(HONESTY).map(|(i, _)| i).collect();
            for (n, &start) in starts.iter().enumerate() {
                let end = starts.get(n + 1).copied().unwrap_or(contents.len());
                let block = &contents[start..end];
                if !block.contains("coord_register_gate") {
                    continue; // attest-side, or some other honesty block
                }
                checked += 1;
                assert!(
                    block.contains("initial_verdict_reason"),
                    "bundled agent command {name}: the registration \"Masked-tool honesty\" \
                     block at byte {start} teaches that a returned `gate_id` is the test, \
                     but never states the discriminator that tells a usable gate from an \
                     unusable one. The rule: {NOT_USABLE_TEST}. Every registration path \
                     needs it, not just the file as a whole; add the Warnings-honesty \
                     bullet to this path in src-tauri/src/fleet_commands/{name}.md"
                );
                // The SECOND arm of the narrowed rule. Keyed on the terminal
                // states rather than on an `initial_verdict` token, which would
                // be vacuous: `initial_verdict_reason` contains that token as a
                // substring, so the assertion above already satisfies it and a
                // block teaching only the first arm would still pass.
                assert!(
                    block.contains("misconfigured"),
                    "bundled agent command {name}: the registration \"Masked-tool honesty\" \
                     block at byte {start} states the `initial_verdict_reason` arm but not \
                     the terminal-`initial_verdict` arm, so it teaches a session to treat a \
                     gate born `misconfigured` / `failed` as live and wait on something that \
                     can never clear. The rule: {NOT_USABLE_TEST}. Add the missing arm in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        // Non-vacuity floor, matching the sibling guard above. Without it a
        // drift in the HONESTY marker silently reduces this guard to scanning
        // zero blocks and passing.
        assert!(
            checked > 0,
            "no bundled command has a registration \"Masked-tool honesty\" block - either \
             the bundle lost its gate-registration procedures or this guard's {HONESTY:?} \
             marker went stale and it is now passing vacuously"
        );
    }

    /// The retired half of the rule must not come back.
    ///
    /// Until 2026-08-31 the rule read "a non-empty `warnings[]` **or** a
    /// 'cannot evaluate' `initial_verdict_reason` means
    /// REGISTERED-BUT-NOT-USABLE". The `warnings[]` half was narrowed away as
    /// over-broad: coord emits informational warnings freely, so counting them
    /// told sessions to withdraw and re-register gates coord was evaluating
    /// normally (`qontinui-runner#1245`, measured against live gates that
    /// carried warnings and were `verdict: open`).
    ///
    /// **Nothing guarded that narrowing, which is why this guard exists.** The
    /// two guards above key on the PRESENCE of `initial_verdict_reason` - a
    /// token the retired wording carries just as the corrected wording does.
    /// Replaying the pre-narrowing bodies (`546e9e024^`) through both
    /// predicates passes them clean, so a regression to the retired rule was
    /// invisible to this repo's CI. That is the same defect class the guard
    /// above documents from 2026-08-08: a token-presence floor cannot tell a
    /// corrected path from a superseded one.
    ///
    /// Detection is by PROXIMITY on normalized text, which separates the two
    /// wordings with a wide measured margin: in the retired bodies every
    /// "non-empty warnings" mention is followed by its not-usable verdict
    /// within 67-81 characters (five sites across four files), while in the
    /// corrected bodies no such mention has a verdict within 400. `WINDOW` sits
    /// between the two.
    #[test]
    fn no_bundled_command_revives_the_retired_warnings_emptiness_test() {
        // Characters after a "non-empty warnings" mention within which a
        // not-usable verdict means the mention is ASSERTING the retired test
        // rather than demoting it. Measured margin: retired 67-81, corrected
        // none within 400.
        const WINDOW: usize = 200;
        const TRIGGER: &str = "non-empty warnings";
        const VERDICTS: &[&str] = &[
            "registered-but-not-usable",
            "not a registered gate",
            "can never clear",
        ];
        // The retired section heading, which asserts the same test without
        // using the trigger phrase. Superseded by "a `gate_id` with a DEAD
        // VERDICT is not a registered gate".
        const RETIRED_HEADING: &str = "gate_id with warnings is not a registered gate";

        for (name, contents) in FLEET_COMMANDS {
            // Strip the markdown emphasis and collapse whitespace so the rule
            // is matched as prose rather than as one particular line-wrapping
            // of it.
            let stripped: String = contents
                .chars()
                .filter(|c| *c != '*' && *c != '`')
                .collect();
            let norm = stripped
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();

            assert!(
                !norm.contains(RETIRED_HEADING),
                "bundled agent command {name} revives the RETIRED gate-warnings heading \
                 (\"a `gate_id` with WARNINGS is not a registered gate\"). The rule is now: \
                 {NOT_USABLE_TEST}. Fix src-tauri/src/fleet_commands/{name}.md"
            );

            let chars: Vec<char> = norm.chars().collect();
            let trigger: Vec<char> = TRIGGER.chars().collect();
            for i in 0..chars.len().saturating_sub(trigger.len()) {
                if chars[i..i + trigger.len()] != trigger[..] {
                    continue;
                }
                let from = i + trigger.len();
                let to = (from + WINDOW).min(chars.len());
                let window: String = chars[from..to].iter().collect();
                for verdict in VERDICTS {
                    assert!(
                        !window.contains(verdict),
                        "bundled agent command {name} revives the RETIRED half of the \
                         gate-warnings rule: a {TRIGGER:?} mention is followed within \
                         {WINDOW} characters by the not-usable verdict {verdict:?}, which is \
                         the superseded wording that COUNTS warnings instead of reading \
                         them. The rule is now: {NOT_USABLE_TEST}. Fix \
                         src-tauri/src/fleet_commands/{name}.md - and if the demotion is \
                         genuinely being restated next to a verdict, reword it rather than \
                         widen this guard. Offending window: {window:?}"
                    );
                }
            }
        }
    }

    /// The bundled commands must never acquire the operator's absolute plan
    /// paths. Scope to the specific hardcode patterns that were neutralized —
    /// NOT bare `qontinui-dev-notes`, which legitimately appears as a repo name.
    ///
    /// This guard is independent of where the bodies come from, and it got MORE
    /// load-bearing once these files became the canonical, user-facing
    /// defaults: whatever is here ships to every fleet device.
    #[test]
    fn staged_fleet_commands_have_no_plan_path_hardcodes() {
        const FORBIDDEN: &[&str] = &[
            "qontinui-dev-notes/plans",
            "qontinui-root/plans",
            "D:/qontinui-root",
        ];
        for (name, contents) in FLEET_COMMANDS {
            for pat in FORBIDDEN {
                assert!(
                    !contents.contains(pat),
                    "bundled agent command {name} contains forbidden plan-path hardcode \
                     {pat:?} — an operator-local absolute path must never ship to a fleet \
                     device; rewrite it in src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
    }

    /// Gate verbs coord serves in operator/agent PAIRS: each has a
    /// device-authed twin one path segment away, under an `/agent/` INFIX —
    /// `POST /coord/gates/:gate_id/agent/<verb>`. The unprefixed
    /// `POST /coord/gates/:gate_id/<verb>` is the OPERATOR door and answers a
    /// device or agent JWT `401 operator context missing; SSO required`.
    ///
    /// Ported from `qontinui-claude-config`'s `scripts/lint-gate-route-tier.py`
    /// (check #21), which cannot hold the line here: it needs a
    /// `qontinui-claude-config` checkout to exist and be current, and no job in
    /// this repo's CI runs it. Same reasoning as
    /// [`bundled_gate_registration_commands_teach_the_mechanics`] — these bodies
    /// are what actually ship, so the invariant is asserted where the files
    /// live.
    const OPERATOR_TIER_TWIN_VERBS: &[&str] = &[
        "reject",
        "reopen",
        "mute",
        "unmute",
        "snooze",
        "continuation-cancel",
        "force-clear",
        "audience",
    ];

    /// Same-line exemption marker, spelled exactly as the Python linter spells
    /// it so a body can carry one marker that satisfies both guards.
    const ROUTE_TIER_ALLOW_MARKER: &str = "lint-gate-route-tier: allow";

    /// True when line `line` earns an exemption for a candidate on it.
    ///
    /// **SAME LINE ONLY.** The Python original tried the preceding line too and
    /// the docstring records why that was reverted: these bodies say "operator"
    /// dozens of times per section, so an unrelated sentence above a genuine
    /// defect exempted it. A prose case that genuinely spans lines carries the
    /// explicit marker instead — an intentional exemption should be legible as
    /// one.
    fn route_tier_line_is_exempt(line: &str) -> bool {
        if line.contains(ROUTE_TIER_ALLOW_MARKER) {
            return true;
        }
        // Case-insensitive WHOLE-WORD `operator`: a line documenting the
        // operator's own door, or contrasting the two tiers.
        let lower = line.to_ascii_lowercase();
        lower.match_indices("operator").any(|(i, _)| {
            let before_ok = i == 0
                || !lower.as_bytes()[i - 1].is_ascii_alphanumeric()
                    && lower.as_bytes()[i - 1] != b'_';
            let end = i + "operator".len();
            let after_ok = end >= lower.len()
                || !lower.as_bytes()[end].is_ascii_alphanumeric() && lower.as_bytes()[end] != b'_';
            before_ok && after_ok
        })
    }

    /// No bundled command may instruct an agent to POST an operator-tier gate
    /// verb that HAS a device-authed twin.
    ///
    /// On 2026-08-20 gate `7902e457` spawned a redundant terminal against a plan
    /// already stamped SHIPPED, because the shipped bodies told agents to POST
    /// the operator `continuation-cancel` route and documented the resulting 401
    /// as EXPECTED. The agent twin had existed for over a week. A whole session
    /// was burned re-doing finished work.
    ///
    /// ## Limitation — read before trusting a green run
    ///
    /// This guards the ROUTE-TIER CLASS ONLY. It is purely textual and keys on
    /// route literals, so it does NOT detect:
    ///
    /// - bundle staleness in general — a body may be arbitrarily far behind
    ///   `qontinui-claude-config`'s canonical copy and still pass;
    /// - a prose claim about reachability ("the cancel below stays
    ///   operator-only") sitting near a correct route — that is the belief that
    ///   actually cost the session, and it needs a reader;
    /// - coord adding a NEW twin no bundled body mentions — this test does not
    ///   read coord's `routes.rs`, deliberately, since a cross-repo checkout
    ///   would give it a second way to go stale;
    /// - the SKILL bundle. This scans [`FLEET_COMMANDS`] only, while
    ///   `crate::fleet_skills` ships `.claude/skills/**` on the same "on a device
    ///   with no checkout this is the only copy" argument, so the same defect
    ///   class is unguarded there. Measured 2026-08-30: zero operator-tier route
    ///   literals across the 13 embedded skill files, so this is latent rather
    ///   than live — but the `correct_uses > 0` staleness floor below says
    ///   nothing about the half of the bundle it never reads.
    ///
    /// The correct-usage counter below exists because a guard that has stopped
    /// matching reports clean forever.
    #[test]
    fn bundled_commands_never_send_agents_to_operator_tier_gate_routes() {
        let verb_alt = OPERATOR_TIER_TWIN_VERBS
            .iter()
            .map(|v| regex::escape(v))
            .collect::<Vec<_>>()
            .join("|");
        // `/coord/gates/<anchor>/<twin verb>`.
        //
        // **What actually discriminates the twin form is the ANCHOR CLASS.**
        // `[^/\s`]+` cannot cross a `/`, so the verb must sit in the segment
        // immediately after the gate id; `/coord/gates/<id>/agent/mute` fails to
        // match because `agent` is not a twin verb and there is no second
        // `/coord/gates/` to re-anchor on.
        //
        // The Python original then layers a `(?<!/agent)` lookbehind covering
        // exactly one residual case — a gate id literally spelled `agent`
        // (`/coord/gates/agent/mute`). Rust's `regex` crate has NO lookbehind,
        // so that guard is implemented explicitly below by rejecting an anchor
        // capture equal to `agent`. BOTH are preserved: loosening the anchor
        // class would make the `agent`-anchor rejection load-bearing.
        let candidate = regex::Regex::new(&format!(
            r"/coord/gates/(?P<anchor>[^/\s`]+)/(?P<verb>{verb_alt})\b"
        ))
        .expect("route-tier candidate pattern compiles");
        let correct_twin = regex::Regex::new(r"/coord/gates/[^/\s`]+/agent/")
            .expect("route-tier twin pattern compiles");

        // Self-test: a guard that has stopped matching reports clean forever.
        assert!(
            candidate.is_match("POST $COORD_HTTP_URL/coord/gates/<gate_id>/continuation-cancel"),
            "route-tier pattern no longer matches the defect shape it exists for"
        );
        assert!(
            !candidate.is_match("POST $COORD_HTTP_URL/coord/gates/<gate_id>/agent/mute"),
            "route-tier pattern flags the CORRECT twin form"
        );
        for verb in ["approve", "attest", "withdraw", "continuation-consumed"] {
            assert!(
                !candidate.is_match(&format!("POST /coord/gates/<id>/{verb}")),
                "route-tier pattern flags {verb:?}, a verb with no `/agent/` twin"
            );
        }
        // The one case the explicit `agent`-anchor rejection covers.
        {
            let m = candidate
                .captures("POST /coord/gates/agent/mute")
                .expect("anchor class matches a gate id spelled `agent`");
            assert_eq!(
                &m["anchor"], "agent",
                "the /agent/-anchor guard has rotted: it no longer sees this shape"
            );
        }
        assert!(
            route_tier_line_is_exempt("| Operator | `POST /coord/gates/:id/mute` |"),
            "operator-context exemption stopped working"
        );
        assert!(
            route_tier_line_is_exempt(&format!(
                "POST /coord/gates/:id/mute <!-- {ROUTE_TIER_ALLOW_MARKER} -->"
            )),
            "allow-marker exemption stopped working"
        );
        assert!(
            !route_tier_line_is_exempt("POST /coord/gates/:id/mute"),
            "exemption fires on a line that earns no exemption"
        );
        assert!(
            !route_tier_line_is_exempt("cooperatorship"),
            "the `operator` exemption is whole-word, not a substring match"
        );

        let mut violations: Vec<String> = Vec::new();
        let mut correct_uses = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            for (idx, line) in contents.lines().enumerate() {
                correct_uses += correct_twin.find_iter(line).count();
                for caps in candidate.captures_iter(line) {
                    // The Python `(?<!/agent)` lookbehind, spelled out: a gate
                    // id literally `agent` means the line already reads
                    // `/coord/gates/agent/<verb>`, i.e. the twin form.
                    if &caps["anchor"] == "agent" {
                        continue;
                    }
                    if route_tier_line_is_exempt(line) {
                        continue;
                    }
                    let verb = &caps["verb"];
                    violations.push(format!(
                        "  src-tauri/src/fleet_commands/{name}.md:{}: `{}` — `{verb}` has a \
                         device-authed twin. An agent must POST \
                         /coord/gates/<id>/agent/{verb}; the unprefixed route is the \
                         operator's and answers an agent 401. If this line IS documenting \
                         the operator door, say 'operator' on it (or add \
                         '{ROUTE_TIER_ALLOW_MARKER}').",
                        idx + 1,
                        &caps[0],
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "bundled agent command(s) send AGENTS to operator-tier gate routes — the agent \
             twin sits one path segment away, and the 401 it avoids has already cost a real \
             session (gate 7902e457, 2026-08-20). {} violation(s):\n{}",
            violations.len(),
            violations.join("\n")
        );
        assert!(
            correct_uses > 0,
            "no bundled command uses the `/coord/gates/<id>/agent/` twin form at all — this \
             guard is matching nothing, which reads as clean forever; re-derive the route \
             model before trusting it"
        );
    }

    /// Content probe selecting the bundled commands that teach the
    /// `IN PROGRESS` delivery guard: a command teaches it iff it names the read
    /// the whole guard is built on.
    ///
    /// Scoped by CONTENT rather than by filename, for the same reason
    /// [`bundled_gate_registration_commands_teach_the_mechanics`] is — the
    /// module doc's "nothing may assume the bundle is two commands" applies to
    /// its tests too. Today this selects exactly `/vet-plan` and
    /// `/implement-plan` and nothing else in the bundle; a third command that
    /// grows the guard is covered the day it does.
    const DELIVERY_READ: &str = "coord_work_unit_list_citations";

    /// The fail-closed clauses of the `IN PROGRESS` delivery guard, as
    /// `(token, why it is load-bearing)`.
    ///
    /// Every one of these was added by a LATER REVIEW ROUND than the one that
    /// shipped the guard, and each closed a hole that read as complete prose
    /// until someone traced one concrete response shape through it. Nothing but
    /// a reader's care has held them in place since — and `implement-plan.md`'s
    /// own "keep the two in sync" instruction had no enforcement at all, which
    /// is the gap this test closes.
    ///
    /// Matched case-insensitively, unlike its case-sensitive neighbour above.
    /// These tokens are prose, and the two bodies do differ on sentence
    /// position (`Arm 6 is the DEFAULT` where the table introduces it, `arm 6
    /// is the DEFAULT` mid-sentence where the other file cites it). Both files
    /// happen to also carry a lowercase occurrence today, so this is defensive
    /// rather than currently load-bearing — but capitalisation at a sentence
    /// start is not part of the rule, and a guard that fired on it would be
    /// asserting prose style instead of the invariant.
    const IN_PROGRESS_DELIVERY_GUARD_CLAUSES: &[(&str, &str)] = &[
        (
            "4, 3, 2, 1, 5, then 6",
            "the arm table's EVALUATION ORDER. Several responses match more than one \
             row, and the conclusive, permissive arms are 1 and 5 — so a reader taking \
             the table top-down reaches \"proceed to vet\" before ever reaching the \
             UNKNOWN arms, which turns a degraded read into a confident observation of \
             not-delivered",
        ),
        (
            "arm 6",
            "the fail-closed DEFAULT arm. Written first as \"anything else\" on arm 5, it \
             put coord being down, a dead transport, and the superset route's degraded \
             200 all onto \"a clean, complete observation of not delivered -> proceed\" \
             — the exact inversion of `verification-and-evidence` \
             `unknown-must-not-render-as-a-default`, applied to the fleet's \
             highest-base-rate failure",
        ),
        (
            "unidentified default",
            "the STOP for an `IN PROGRESS` stamp carrying no session marker, or one that \
             cannot be positively attributed to the reading session. Without it an \
             unmarked stamp — hand-written, operator-written, or predating the marker \
             convention — matches no case and falls back to overwrite, which is \
             verbatim the regression the section opens by forbidding",
        ),
        (
            "route to closeout",
            "arm 1's terminal disposition — the one arm that stops a run whose work has \
             ALREADY LANDED. Without it a shipped plan is re-vetted and its phase agents \
             re-run against `main`, which is how PR #479 came to be built against work \
             PR #468 had already merged",
        ),
    ];

    /// Every bundled command that teaches the `IN PROGRESS` delivery guard must
    /// carry all of [`IN_PROGRESS_DELIVERY_GUARD_CLAUSES`].
    ///
    /// These bodies are what actually ship (see the module doc): on a device
    /// with no `qontinui-claude-config` checkout they are the ONLY copy, so a
    /// clause missing here is a clause the fleet does not have.
    #[test]
    fn bundled_delivery_guard_commands_carry_the_fail_closed_clauses() {
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if !contents.contains(DELIVERY_READ) {
                continue;
            }
            checked += 1;
            let haystack = contents.to_lowercase();
            for (token, why) in IN_PROGRESS_DELIVERY_GUARD_CLAUSES {
                assert!(
                    haystack.contains(&token.to_lowercase()),
                    "bundled agent command {name} teaches the `IN PROGRESS` delivery \
                     guard (it names {DELIVERY_READ}) but never mentions {token:?} — \
                     {why}. Without it this command's copy of the guard fails OPEN, and \
                     the failure is silent: the prose still reads complete. Add it in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        // A command can leave this guard's scope SILENTLY: drop the delivery
        // read from `implement-plan.md` while keeping its pointer sentence and
        // `checked` falls to 1 with every assertion above still green. So close
        // the loop from the other side — anything that CITES the shared section
        // must also name the read that section is built on. That is exactly the
        // "keep the two in sync" instruction the files state and could not
        // enforce, and unlike a `checked >= 2` floor it assumes nothing about
        // how many commands are in the bundle.
        for (anchor, _) in CROSS_COMMAND_SECTION_ANCHORS {
            for (name, contents) in FLEET_COMMANDS {
                assert!(
                    !contents.contains(anchor) || contents.contains(DELIVERY_READ),
                    "bundled agent command {name} points readers at the {anchor:?} \
                     section but no longer names {DELIVERY_READ}, so it has dropped out \
                     of this guard's scope while still telling a reader to apply that \
                     section's disposition — the clauses above stop being checked for \
                     it and nothing else notices. Restore the delivery read in \
                     src-tauri/src/fleet_commands/{name}.md, or remove the pointer"
                );
            }
        }
        assert!(
            checked > 0,
            "no bundled command mentions {DELIVERY_READ:?} — either the bundle lost its \
             `IN PROGRESS` delivery guard entirely or this guard's content probe went \
             stale. Both need a human look; neither is a passing test"
        );
    }

    /// Cross-command section pointers, as `(anchor text, the command that
    /// defines it)`.
    ///
    /// `/implement-plan` does not restate the disposition table — it points at
    /// `/vet-plan`'s section by heading text. That is the deliberate design
    /// (the section was made explicitly shared across commands rather than
    /// duplicated), which makes the pointer a wiring edge like any other.
    const CROSS_COMMAND_SECTION_ANCHORS: &[(&str, &str)] =
        &[("`IN PROGRESS` is CONDITIONALLY overwritable", "vet-plan")];

    /// A bundled command's pointer at another bundled command's section must
    /// resolve inside the bundle.
    ///
    /// Renaming the heading would leave the pointer dangling with nothing
    /// failing: the reader who follows it finds no such section and falls back
    /// to the pre-guard behaviour, which is overwrite. On a device with no
    /// `qontinui-claude-config` checkout there is no second place the reader
    /// could resolve it from.
    #[test]
    fn cross_command_section_pointers_resolve_within_the_bundle() {
        assert!(
            !CROSS_COMMAND_SECTION_ANCHORS.is_empty(),
            "CROSS_COMMAND_SECTION_ANCHORS is empty, so this test asserts nothing. The \
             bundle's cross-command pointers did not stop existing; the table did"
        );
        for (anchor, target) in CROSS_COMMAND_SECTION_ANCHORS {
            let mut defined = false;
            let mut citers: Vec<&&str> = Vec::new();
            for (name, contents) in FLEET_COMMANDS {
                if !contents.contains(anchor) {
                    continue;
                }
                if name == target {
                    // `# ` + the anchor matches the heading at ANY level and
                    // does NOT match a prose mention, so a heading renamed while
                    // the old wording survives elsewhere in the file still fails
                    // — the case that would otherwise dangle the pointer silently.
                    defined = contents.contains(&format!("# {anchor}"));
                } else {
                    citers.push(name);
                }
            }
            // Checked first: with no citer left there is no edge to dangle, and
            // the honest failure is that this guard went stale — not that the
            // target dropped a heading nobody points at any more.
            assert!(
                !citers.is_empty(),
                "no bundled command cites {anchor:?}, so this guard is watching an edge \
                 that no longer exists. Drop the row from \
                 CROSS_COMMAND_SECTION_ANCHORS, or restore the citation that was lost"
            );
            assert!(
                defined,
                "bundled agent command(s) {citers:?} point readers at {target}'s \
                 {anchor:?} section, but {target} no longer contains that text. Either \
                 restore the heading in src-tauri/src/fleet_commands/{target}.md or \
                 update every citation of it — a dangling pointer here drops the reader \
                 back to the behaviour the section exists to forbid"
            );
        }
    }

    /// The two UNKNOWN arms that a degraded delivery read makes LOOK CLEAN, as
    /// `(token, why it is load-bearing)`.
    ///
    /// [`IN_PROGRESS_DELIVERY_GUARD_CLAUSES`] covers the arms whose absence a
    /// reader would notice: the evaluation order, the fail-closed default, the
    /// unidentified-stamp STOP, arm 1's closeout route. These two are different
    /// in kind — **neither is error-shaped**. Both answer `200` with a
    /// parseable `delivery` and no `citations_error`, so arm 6's enumeration
    /// (errors, unparseable or non-2xx bodies, an absent `delivery`, a dead
    /// transport) does not reach them, and a copy carrying arm 6 alone still
    /// scores a degraded window as arm 5 — "a clean, complete observation of
    /// not delivered".
    ///
    /// They were the last thing to reach the copies: `implement-plan.md`
    /// carried the arm order and arm 6 but named neither of these until the
    /// follow-up that added this guard.
    const CLEAN_LOOKING_UNKNOWN_ARMS: &[(&str, &str)] = &[
        (
            "evidence_complete",
            "arm 2's only discriminator, and it must NOT be keyed on `shipped`: the two \
             derive independently (`shipped = inputs.delivered`, `evidence_complete = \
             evidence_gaps.is_empty()`) and the merged-predicate-degraded gap is \
             unit-independent, so `shipped: true` with `evidence_complete: false` is \
             reachable and falls through to the permissive arm without it",
        ),
        (
            "merged_degraded_reason",
            "arm 3, evaluated ahead of every arm but 4. It sits BESIDE `delivery` and is \
             present even when the verdict could not be derived at all, so while it is \
             set every citation's `merged: false` is UNKNOWN rather than an observation \
             — and nothing about the response looks like an error",
        ),
        (
            "unknown-must-not-render-as-a-default",
            "the served `verification-and-evidence` clause both arms exist to satisfy. A \
             copy that drops the citation keeps the arms but loses the reason, which is \
             what invites the next editor to collapse them back into `not shipped`",
        ),
    ];

    /// Every command in the scope of
    /// [`bundled_delivery_guard_commands_carry_the_fail_closed_clauses`] must
    /// also name [`CLEAN_LOOKING_UNKNOWN_ARMS`].
    ///
    /// Same scope probe ([`DELIVERY_READ`]) and the same reasoning, so the two
    /// compose: that guard keeps the arms a reader would miss, this one keeps
    /// the arms a reader would not.
    #[test]
    fn bundled_delivery_guard_commands_name_the_clean_looking_unknown_arms() {
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if !contents.contains(DELIVERY_READ) {
                continue;
            }
            checked += 1;
            for (token, why) in CLEAN_LOOKING_UNKNOWN_ARMS {
                assert!(
                    contents.contains(token),
                    "bundled agent command {name} teaches the `IN PROGRESS` delivery \
                     guard (it names {DELIVERY_READ}) but never mentions {token:?} — \
                     {why}. A copy missing it fails OPEN on the one response shape that \
                     reads as a clean observation, and on a device with no \
                     qontinui-claude-config checkout this file is the ONLY copy; add it \
                     in src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        assert!(
            checked > 0,
            "no bundled command mentions {DELIVERY_READ:?} — either the bundle lost its \
             `IN PROGRESS` delivery guard entirely or this guard's content probe went \
             stale. Both need a human look; neither is a passing test"
        );
    }

    /// A command may not teach the do-not-overwrite lifecycle tokens and then
    /// OMIT `IN PROGRESS`.
    ///
    /// This is the original defect, encoded. The two guards above are scoped by
    /// [`DELIVERY_READ`], so they say nothing about a command that teaches the
    /// lifecycle list while carrying no delivery guard AT ALL — which is
    /// exactly the state `/vet-plan` was in: `SHIPPED` / `SUPERSEDED` /
    /// `OBSOLETE` listed as protected, `IN PROGRESS` simply absent, and a vet
    /// pass free to re-stamp a plan whose work had landed, satisfy its own
    /// VETTED gate with the stamp it had just written, and re-run phase agents
    /// against `main`.
    ///
    /// The probe is the CO-OCCURRENCE of two of the trio rather than one token:
    /// the two spellings in the bundle punctuate the list differently
    /// (`` `SHIPPED` / `SUPERSEDED` / `OBSOLETE` `` against `` `SHIPPED`,
    /// `SUPERSEDED` or `OBSOLETE` ``), so no single literal matches both, and
    /// `SUPERSEDED` alone is an ordinary English word a later command could use
    /// in prose having nothing to do with a plan stamp.
    #[test]
    fn bundled_lifecycle_commands_dispose_of_in_progress() {
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if !(contents.contains("SUPERSEDED") && contents.contains("OBSOLETE")) {
                continue; // not a command that disposes of a plan lifecycle stamp
            }
            checked += 1;
            assert!(
                contents.contains("is CONDITIONALLY overwritable"),
                "bundled agent command {name} teaches the do-not-overwrite lifecycle \
                 tokens but never says what to do with an `IN PROGRESS` stamp. That \
                 exact omission is what let a vet pass overwrite a plan whose work had \
                 already landed and then re-implement it; restore the \"`IN PROGRESS` is \
                 CONDITIONALLY overwritable\" disposition in \
                 src-tauri/src/fleet_commands/{name}.md"
            );
        }
        assert!(
            checked > 0,
            "no bundled command mentions both `SUPERSEDED` and `OBSOLETE` — either the \
             bundle lost its plan-lifecycle procedures or this guard's content probe \
             went stale"
        );
    }
}
