//! Fleet-portable provisioning of the `/vet-plan` and `/implement-plan`
//! project slash commands into a spawned session's working directory.
//!
//! `claude` discovers project slash commands from `<cwd>/.claude/commands/*.md`.
//! A gate-continuation session is spawned with a fresh worktree as its cwd; on a
//! non-operator device there is no `~/.claude/commands` and no
//! `qontinui-claude-config` checkout, so the fleet vet/implement procedures would
//! be unresolvable. This module BUNDLES the two command procedures into the
//! runner binary via `include_str!` and writes them into the session cwd, so the
//! commands resolve regardless of what (if anything) is in the device's home dir.
//!
//! This is the fleet-portable sibling of `provision_agent_definitions` in
//! `agent_runtime.rs` (which copies subagent defs from a `qontinui-claude-config`
//! checkout). Where that path requires an operator checkout, this one carries the
//! assets in the binary — see that function's doc comment for the broader
//! fleet-portability rationale.

use std::path::Path;

use tracing::{info, warn};

/// `/vet-plan` procedure, bundled into the binary. Source of truth:
/// `qontinui-claude-config/.claude/commands/vet-plan.md`, staged here at build time.
const VET_PLAN: &str = include_str!("fleet_commands/vet-plan.md");

/// `/implement-plan` procedure, bundled into the binary. Source of truth:
/// `qontinui-claude-config/.claude/commands/implement-plan.md`, staged here.
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
/// no `~/.claude/commands` and no `qontinui-claude-config` checkout.
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
}
