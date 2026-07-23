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

/// LF-normalized SHA-256 of each staged fleet command file, checked in so that
/// a silent edit — or a re-stage that drifts from the claude-config source of
/// truth — fails `cargo test fleet_commands` instead of shipping quietly to
/// every fleet device. The existing provisioning tests only assert non-empty +
/// an H1 heading, so they would pass against fully-drifted or half-neutralized
/// files; this hash is what actually pins the content.
///
/// The claude-config repo is not reliably reachable from runner CI, so drift is
/// caught by these hashes rather than by diffing against the source live.
///
/// ## Re-staging (run when the claude-config source legitimately changes)
///
/// From a workspace root holding both checkouts:
///
/// ```sh
/// cp qontinui-claude-config/.claude/commands/implement-plan.md \
///    qontinui-runner/src-tauri/src/fleet_commands/implement-plan.md
/// cp qontinui-claude-config/.claude/commands/vet-plan.md \
///    qontinui-runner/src-tauri/src/fleet_commands/vet-plan.md
/// cd qontinui-runner/src-tauri/src/fleet_commands
/// for f in implement-plan.md vet-plan.md; do
///   printf '%s  %s\n' "$(tr -d '\r' < "$f" | sha256sum | cut -d' ' -f1)" "$f"
/// done
/// ```
///
/// Paste the printed hashes below. The hash is taken over content with CR bytes
/// stripped (`*.md` is `text eol=lf` in `.gitattributes`), so it is stable
/// regardless of the checkout's line-ending policy or the build host's
/// `core.autocrlf`.
const STAGED_COMMAND_HASHES: &[(&str, &str)] = &[
    (
        "implement-plan.md",
        "fc9c0a82309e9489ed23ac2e99374e37eec3c57db6e4f8074b942c82e273e34c",
    ),
    (
        "vet-plan.md",
        "8ee11c8a284a62f1fbd59ce81d43155720168edb035dccd704200223f03c2a51",
    ),
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

    /// LF-normalized SHA-256 hex of `s` — CR bytes stripped so the hash matches
    /// regardless of whether `include_str!` embedded LF or CRLF for this build.
    fn lf_sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        let normalized = s.replace('\r', "");
        Sha256::digest(normalized.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The drift guard: each staged fleet command must match its checked-in
    /// LF-normalized hash. A mismatch means the bundled copy was edited (or the
    /// claude-config source changed) without re-staging — see the re-stage
    /// procedure documented on `STAGED_COMMAND_HASHES`.
    #[test]
    fn staged_fleet_commands_match_checked_in_hashes() {
        assert_eq!(
            STAGED_COMMAND_HASHES.len(),
            FLEET_COMMANDS.len(),
            "every staged fleet command needs exactly one checked-in hash"
        );
        for (name, contents) in FLEET_COMMANDS {
            let expected = STAGED_COMMAND_HASHES
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no checked-in hash for staged fleet command {name}"))
                .1;
            let actual = lf_sha256_hex(contents);
            assert_eq!(
                actual, expected,
                "staged fleet command {name} drifted from its checked-in hash — it was \
                 edited without re-staging from the claude-config source of truth (or the \
                 source changed and the hash was not updated). See the re-stage procedure \
                 documented on STAGED_COMMAND_HASHES."
            );
        }
    }

    /// The staged copies must never re-acquire the operator's absolute plan
    /// paths. Scope to the specific hardcode patterns Phase 3 neutralized — NOT
    /// bare `qontinui-dev-notes`, which legitimately appears as a repo name.
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
                    "staged fleet command {name} contains forbidden plan-path hardcode {pat:?} \
                     — neutralize it in the claude-config source and re-stage"
                );
            }
        }
    }
}
