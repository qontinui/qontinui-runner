//! Fleet-portable provisioning of the `/vet-plan` and `/implement-plan`
//! project slash commands into a spawned session's working directory.
//!
//! `claude` discovers project slash commands from `<cwd>/.claude/commands/*.md`.
//! A gate-continuation session is spawned with a fresh worktree as its cwd, and
//! on most devices there is no `~/.claude/commands` at all, so the fleet
//! vet/implement procedures would be unresolvable. This module BUNDLES the two
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
//! Because these bodies ship to every fleet device, they must stay free of any
//! one operator's absolute paths — see
//! [`tests::staged_fleet_commands_have_no_plan_path_hardcodes`].

use std::path::Path;

use tracing::{info, warn};

/// `/vet-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/vet-plan.md` in this repository — edit it
/// there.
const VET_PLAN: &str = include_str!("fleet_commands/vet-plan.md");

/// `/implement-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/implement-plan.md` in this repository — edit
/// it there.
const IMPLEMENT_PLAN: &str = include_str!("fleet_commands/implement-plan.md");

/// The command files to provision, as `(filename, contents)`. The filename is
/// the slash-command name `claude` will expose (e.g. `vet-plan.md` -> `/vet-plan`).
const FLEET_COMMANDS: &[(&str, &str)] = &[
    ("vet-plan.md", VET_PLAN),
    ("implement-plan.md", IMPLEMENT_PLAN),
];

/// Provision the bundled fleet command procedures into `<workdir>/.claude/commands/`
/// so a `claude` session spawned with `workdir` as its cwd can resolve `/vet-plan`
/// and `/implement-plan` as PROJECT-scoped slash commands — even on a device with
/// no `~/.claude/commands`.
///
/// Fail-soft (mirrors `coord_mcp::provision_coord_mcp_for_session`): any IO error
/// is logged via `tracing::warn!` and swallowed — a provisioning failure must
/// never abort an otherwise-launchable spawn (the session simply lacks the
/// commands, the same state as before this feature). Idempotent: existing files
/// are overwritten.
pub(crate) fn provision_fleet_commands_for_session(workdir: &str) {
    let commands_dir = Path::new(workdir).join(".claude").join("commands");
    match provision_fleet_commands_into(&commands_dir) {
        Ok(written) => {
            info!(
                "fleet_commands: provisioned {written} fleet command(s) into {}",
                commands_dir.display()
            );
        }
        Err(e) => {
            warn!(
                "fleet_commands: failed to provision fleet commands into {} \
                 (continuing spawn; /vet-plan and /implement-plan may not resolve): {e}",
                commands_dir.display()
            );
        }
    }
}

/// Core of [`provision_fleet_commands_for_session`]: create `commands_dir` and
/// write every bundled command file into it (overwrite/idempotent), returning the
/// count written. Split out so a unit test can drive it against a tempdir and
/// assert the result — mirroring how `provision_agent_definitions` factored out
/// its `_from_root` core.
fn provision_fleet_commands_into(commands_dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(commands_dir)?;
    let mut written = 0usize;
    for (name, contents) in FLEET_COMMANDS {
        let dst = commands_dir.join(name);
        std::fs::write(&dst, contents)?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_both_fleet_commands_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");

        let written = provision_fleet_commands_into(&commands_dir).expect("provision");
        assert_eq!(written, 2, "should provision exactly two fleet commands");

        let vet = commands_dir.join("vet-plan.md");
        let implement = commands_dir.join("implement-plan.md");
        assert!(vet.exists(), "vet-plan.md should exist");
        assert!(implement.exists(), "implement-plan.md should exist");

        let vet_body = std::fs::read_to_string(&vet).expect("read vet-plan.md");
        let implement_body = std::fs::read_to_string(&implement).expect("read implement-plan.md");
        assert!(!vet_body.is_empty(), "vet-plan.md should be non-empty");
        assert!(
            !implement_body.is_empty(),
            "implement-plan.md should be non-empty"
        );

        // Substrings verified present near the top of each staged file
        // (the `# Vet Plan` / `# Implement Plan` H1 headings).
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

        assert_eq!(provision_fleet_commands_into(&commands_dir).unwrap(), 2);
        // Second run over the same dir must succeed (overwrite, not error).
        assert_eq!(provision_fleet_commands_into(&commands_dir).unwrap(), 2);
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
                    "bundled fleet command {name} contains forbidden plan-path hardcode \
                     {pat:?} — an operator-local absolute path must never ship to a fleet \
                     device; rewrite it in src-tauri/src/fleet_commands/{name}"
                );
            }
        }
    }
}
