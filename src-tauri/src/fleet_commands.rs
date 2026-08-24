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

/// `/vet-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/vet-plan.md` in this repository — edit it
/// there.
const VET_PLAN: &str = include_str!("fleet_commands/vet-plan.md");

/// `/implement-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/implement-plan.md` in this repository — edit
/// it there.
const IMPLEMENT_PLAN: &str = include_str!("fleet_commands/implement-plan.md");

/// The embedded default commands, as `(name, body)`. `name` is the slash
/// command `claude` will expose and the filename stem written under
/// `.claude/commands/` (`vet-plan` -> `vet-plan.md` -> `/vet-plan`).
pub(crate) const FLEET_COMMANDS: &[(&str, &str)] =
    &[("vet-plan", VET_PLAN), ("implement-plan", IMPLEMENT_PLAN)];

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
    let registry = crate::agent_commands::resolve_registry();
    let commands_dir = Path::new(workdir).join(".claude").join("commands");
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

    /// The bundled text must never acquire the operator's absolute plan paths
    /// or a reach into the operator's `qontinui-claude-config` checkout. Scope
    /// to the specific hardcode patterns that were neutralized — NOT bare
    /// `qontinui-dev-notes`, which legitimately appears as a repo name.
    ///
    /// This guard is independent of where the bodies come from, and it got MORE
    /// load-bearing once these files became the canonical, user-facing
    /// defaults: whatever is here ships to every fleet device.
    ///
    /// ## This list is a FLOOR, not the gate
    ///
    /// A substring list only catches the spellings someone thought of.
    /// Measured 2026-08-24 over the four real hardcode sites in
    /// `coord-pr-label/SKILL.md`: `qontinui-claude-config/.claude` catches
    /// **one** of them, because the other three wore an elided spelling
    /// (`.../coord-pr-label/set-label.sh`) that expands to the same config-repo
    /// path while containing neither token. The actual gate is shape-based and
    /// lives in [`crate::agent_skills::self_path`], which asks whether a unit
    /// can reach its own files at all once provisioned. Keep this list — it is
    /// cheap and it covers the plan-path patterns that shape rule says nothing
    /// about — but do not mistake it for coverage.
    #[test]
    fn staged_fleet_text_has_no_operator_path_hardcodes() {
        const FORBIDDEN: &[&str] = &[
            "qontinui-dev-notes/plans",
            "qontinui-root/plans",
            "D:/qontinui-root",
            // The config-repo reach. Both separator spellings, because a
            // Windows-authored body writes the second.
            "qontinui-claude-config/.claude",
            "qontinui-claude-config\\.claude",
        ];
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            checked += 1;
            for pat in FORBIDDEN {
                assert!(
                    !contents.contains(pat),
                    "bundled agent command {name} contains forbidden operator-path hardcode \
                     {pat:?} — an operator-local absolute path must never ship to a fleet \
                     device; rewrite it in src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        // The same floor over the embedded SKILLS bundle. Vacuous while Phase 6
        // has not filled it (see `crate::fleet_skills::FLEET_SKILLS`), and
        // written so it stops being vacuous the moment it is.
        for skill in crate::fleet_skills::FLEET_SKILLS {
            for (path, text) in skill.files {
                checked += 1;
                for pat in FORBIDDEN {
                    assert!(
                        !text.contains(pat),
                        "bundled agent skill {}/{path} contains forbidden operator-path \
                         hardcode {pat:?}",
                        skill.name
                    );
                }
            }
        }
        assert!(
            checked > 0,
            "the guard scanned nothing — the embedded bundles have gone missing"
        );
    }
}
