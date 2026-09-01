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

/// `/add-tests` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/add-tests.md` in this repository — edit it
/// there.
const ADD_TESTS: &str = include_str!("fleet_commands/add-tests.md");

/// `/add-types` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/add-types.md` in this repository — edit it
/// there.
const ADD_TYPES: &str = include_str!("fleet_commands/add-types.md");

/// `/analyze-automation` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/analyze-automation.md` in this repository — edit it
/// there.
const ANALYZE_AUTOMATION: &str = include_str!("fleet_commands/analyze-automation.md");

/// `/analyze-subagent` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/analyze-subagent.md` in this repository — edit it
/// there.
const ANALYZE_SUBAGENT: &str = include_str!("fleet_commands/analyze-subagent.md");

/// `/ask-operator` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/ask-operator.md` in this repository — edit it
/// there.
const ASK_OPERATOR: &str = include_str!("fleet_commands/ask-operator.md");

/// `/audit` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/audit.md` in this repository — edit it
/// there.
const AUDIT: &str = include_str!("fleet_commands/audit.md");

/// `/auto-fix` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/auto-fix.md` in this repository — edit it
/// there.
const AUTO_FIX: &str = include_str!("fleet_commands/auto-fix.md");

/// `/auto-improve` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/auto-improve.md` in this repository — edit it
/// there.
const AUTO_IMPROVE: &str = include_str!("fleet_commands/auto-improve.md");

/// `/auto-review` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/auto-review.md` in this repository — edit it
/// there.
const AUTO_REVIEW: &str = include_str!("fleet_commands/auto-review.md");

/// `/babysit-prs` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/babysit-prs.md` in this repository — edit it
/// there.
const BABYSIT_PRS: &str = include_str!("fleet_commands/babysit-prs.md");

/// `/clean-commit` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/clean-commit.md` in this repository — edit it
/// there.
const CLEAN_COMMIT: &str = include_str!("fleet_commands/clean-commit.md");

/// `/clean` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/clean.md` in this repository — edit it
/// there.
const CLEAN: &str = include_str!("fleet_commands/clean.md");

/// `/code-analyze` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/code-analyze.md` in this repository — edit it
/// there.
const CODE_ANALYZE: &str = include_str!("fleet_commands/code-analyze.md");

/// `/code-fix` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/code-fix.md` in this repository — edit it
/// there.
const CODE_FIX: &str = include_str!("fleet_commands/code-fix.md");

/// `/coordinate` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/coordinate.md` in this repository — edit it
/// there.
const COORDINATE: &str = include_str!("fleet_commands/coordinate.md");

/// `/create-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/create-plan.md` in this repository — edit it
/// there.
const CREATE_PLAN: &str = include_str!("fleet_commands/create-plan.md");

/// `/create-tutorial` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/create-tutorial.md` in this repository — edit it
/// there.
const CREATE_TUTORIAL: &str = include_str!("fleet_commands/create-tutorial.md");

/// `/debug-loop` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/debug-loop.md` in this repository — edit it
/// there.
const DEBUG_LOOP: &str = include_str!("fleet_commands/debug-loop.md");

/// `/debug` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/debug.md` in this repository — edit it
/// there.
const DEBUG: &str = include_str!("fleet_commands/debug.md");

/// `/find-debt` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/find-debt.md` in this repository — edit it
/// there.
const FIND_DEBT: &str = include_str!("fleet_commands/find-debt.md");

/// `/find-misplaced` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/find-misplaced.md` in this repository — edit it
/// there.
const FIND_MISPLACED: &str = include_str!("fleet_commands/find-misplaced.md");

/// `/fix` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/fix.md` in this repository — edit it
/// there.
const FIX: &str = include_str!("fleet_commands/fix.md");

/// `/implement-phase` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/implement-phase.md` in this repository — edit it
/// there.
const IMPLEMENT_PHASE: &str = include_str!("fleet_commands/implement-phase.md");

/// `/improve-all` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/improve-all.md` in this repository — edit it
/// there.
const IMPROVE_ALL: &str = include_str!("fleet_commands/improve-all.md");

/// `/manual-test-coord-loop` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/manual-test-coord-loop.md` in this repository — edit it
/// there.
const MANUAL_TEST_COORD_LOOP: &str = include_str!("fleet_commands/manual-test-coord-loop.md");

/// `/manual-test-coord` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/manual-test-coord.md` in this repository — edit it
/// there.
const MANUAL_TEST_COORD: &str = include_str!("fleet_commands/manual-test-coord.md");

/// `/manual-test-loop` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/manual-test-loop.md` in this repository — edit it
/// there.
const MANUAL_TEST_LOOP: &str = include_str!("fleet_commands/manual-test-loop.md");

/// `/manual-test` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/manual-test.md` in this repository — edit it
/// there.
const MANUAL_TEST: &str = include_str!("fleet_commands/manual-test.md");

/// `/merge-train-steward` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/merge-train-steward.md` in this repository — edit it
/// there.
const MERGE_TRAIN_STEWARD: &str = include_str!("fleet_commands/merge-train-steward.md");

/// `/mobile-dev` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/mobile-dev.md` in this repository — edit it
/// there.
const MOBILE_DEV: &str = include_str!("fleet_commands/mobile-dev.md");

/// `/mobile-verify` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/mobile-verify.md` in this repository — edit it
/// there.
const MOBILE_VERIFY: &str = include_str!("fleet_commands/mobile-verify.md");

/// `/mtc` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/mtc.md` in this repository — edit it
/// there.
const MTC: &str = include_str!("fleet_commands/mtc.md");

/// `/name` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/name.md` in this repository — edit it
/// there.
const NAME: &str = include_str!("fleet_commands/name.md");

/// `/next-steps` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/next-steps.md` in this repository — edit it
/// there.
const NEXT_STEPS: &str = include_str!("fleet_commands/next-steps.md");

/// `/organize-notes` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/organize-notes.md` in this repository — edit it
/// there.
const ORGANIZE_NOTES: &str = include_str!("fleet_commands/organize-notes.md");

/// `/publish-runner` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/publish-runner.md` in this repository — edit it
/// there.
const PUBLISH_RUNNER: &str = include_str!("fleet_commands/publish-runner.md");

/// `/pull-all` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/pull-all.md` in this repository — edit it
/// there.
const PULL_ALL: &str = include_str!("fleet_commands/pull-all.md");

/// `/pull-scoped` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/pull-scoped.md` in this repository — edit it
/// there.
const PULL_SCOPED: &str = include_str!("fleet_commands/pull-scoped.md");

/// `/pvi` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/pvi.md` in this repository — edit it
/// there.
const PVI: &str = include_str!("fleet_commands/pvi.md");

/// `/qa` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/qa.md` in this repository — edit it
/// there.
const QA: &str = include_str!("fleet_commands/qa.md");

/// `/recursive-automation` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/recursive-automation.md` in this repository — edit it
/// there.
const RECURSIVE_AUTOMATION: &str = include_str!("fleet_commands/recursive-automation.md");

/// `/refactor-srp` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/refactor-srp.md` in this repository — edit it
/// there.
const REFACTOR_SRP: &str = include_str!("fleet_commands/refactor-srp.md");

/// `/reflect-ui-bridge` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/reflect-ui-bridge.md` in this repository — edit it
/// there.
const REFLECT_UI_BRIDGE: &str = include_str!("fleet_commands/reflect-ui-bridge.md");

/// `/research-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/research-plan.md` in this repository — edit it
/// there.
const RESEARCH_PLAN: &str = include_str!("fleet_commands/research-plan.md");

/// `/resume-foreign` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/resume-foreign.md` in this repository — edit it
/// there.
const RESUME_FOREIGN: &str = include_str!("fleet_commands/resume-foreign.md");

/// `/review-before-code` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/review-before-code.md` in this repository — edit it
/// there.
const REVIEW_BEFORE_CODE: &str = include_str!("fleet_commands/review-before-code.md");

/// `/review-commit` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/review-commit.md` in this repository — edit it
/// there.
const REVIEW_COMMIT: &str = include_str!("fleet_commands/review-commit.md");

/// `/review-logs` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/review-logs.md` in this repository — edit it
/// there.
const REVIEW_LOGS: &str = include_str!("fleet_commands/review-logs.md");

/// `/review-plan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/review-plan.md` in this repository — edit it
/// there.
const REVIEW_PLAN: &str = include_str!("fleet_commands/review-plan.md");

/// `/review-plan-next-steps` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/review-plan-next-steps.md` in this repository — edit it
/// there.
const REVIEW_PLAN_NEXT_STEPS: &str = include_str!("fleet_commands/review-plan-next-steps.md");

/// `/rewind-session` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/rewind-session.md` in this repository — edit it
/// there.
const REWIND_SESSION: &str = include_str!("fleet_commands/rewind-session.md");

/// `/run-automation` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/run-automation.md` in this repository — edit it
/// there.
const RUN_AUTOMATION: &str = include_str!("fleet_commands/run-automation.md");

/// `/scout` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/scout.md` in this repository — edit it
/// there.
const SCOUT: &str = include_str!("fleet_commands/scout.md");

/// `/security-scan` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/security-scan.md` in this repository — edit it
/// there.
const SECURITY_SCAN: &str = include_str!("fleet_commands/security-scan.md");

/// `/summarize-session` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/summarize-session.md` in this repository — edit it
/// there.
const SUMMARIZE_SESSION: &str = include_str!("fleet_commands/summarize-session.md");

/// `/symbol-claims-warn` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/symbol-claims-warn.md` in this repository — edit it
/// there.
const SYMBOL_CLAIMS_WARN: &str = include_str!("fleet_commands/symbol-claims-warn.md");

/// `/test-ui-bridge` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/test-ui-bridge.md` in this repository — edit it
/// there.
const TEST_UI_BRIDGE: &str = include_str!("fleet_commands/test-ui-bridge.md");

/// `/ufix` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/ufix.md` in this repository — edit it
/// there.
const UFIX: &str = include_str!("fleet_commands/ufix.md");

/// `/ui-bridge` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/ui-bridge.md` in this repository — edit it
/// there.
const UI_BRIDGE: &str = include_str!("fleet_commands/ui-bridge.md");

/// `/unattended` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/unattended.md` in this repository — edit it
/// there.
const UNATTENDED: &str = include_str!("fleet_commands/unattended.md");

/// `/update-spec` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/update-spec.md` in this repository — edit it
/// there.
const UPDATE_SPEC: &str = include_str!("fleet_commands/update-spec.md");

/// `/validate` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/validate.md` in this repository — edit it
/// there.
const VALIDATE: &str = include_str!("fleet_commands/validate.md");

/// `/verify-plan-status` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/verify-plan-status.md` in this repository — edit it
/// there.
const VERIFY_PLAN_STATUS: &str = include_str!("fleet_commands/verify-plan-status.md");

/// `/verify-web` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/verify-web.md` in this repository — edit it
/// there.
const VERIFY_WEB: &str = include_str!("fleet_commands/verify-web.md");

/// `/vet-imp` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/vet-imp.md` in this repository — edit it
/// there.
const VET_IMP: &str = include_str!("fleet_commands/vet-imp.md");

/// `/workflow-runs` procedure, bundled into the binary. Canonical source:
/// `src-tauri/src/fleet_commands/workflow-runs.md` in this repository — edit it
/// there.
const WORKFLOW_RUNS: &str = include_str!("fleet_commands/workflow-runs.md");

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
    ("add-tests", ADD_TESTS),
    ("add-types", ADD_TYPES),
    ("analyze-automation", ANALYZE_AUTOMATION),
    ("analyze-subagent", ANALYZE_SUBAGENT),
    ("ask-operator", ASK_OPERATOR),
    ("audit", AUDIT),
    ("auto-fix", AUTO_FIX),
    ("auto-improve", AUTO_IMPROVE),
    ("auto-review", AUTO_REVIEW),
    ("babysit-prs", BABYSIT_PRS),
    ("clean-commit", CLEAN_COMMIT),
    ("clean", CLEAN),
    ("code-analyze", CODE_ANALYZE),
    ("code-fix", CODE_FIX),
    ("coordinate", COORDINATE),
    ("create-plan", CREATE_PLAN),
    ("create-tutorial", CREATE_TUTORIAL),
    ("debug-loop", DEBUG_LOOP),
    ("debug", DEBUG),
    ("find-debt", FIND_DEBT),
    ("find-misplaced", FIND_MISPLACED),
    ("fix", FIX),
    ("implement-phase", IMPLEMENT_PHASE),
    ("improve-all", IMPROVE_ALL),
    ("manual-test-coord-loop", MANUAL_TEST_COORD_LOOP),
    ("manual-test-coord", MANUAL_TEST_COORD),
    ("manual-test-loop", MANUAL_TEST_LOOP),
    ("manual-test", MANUAL_TEST),
    ("merge-train-steward", MERGE_TRAIN_STEWARD),
    ("mobile-dev", MOBILE_DEV),
    ("mobile-verify", MOBILE_VERIFY),
    ("mtc", MTC),
    ("name", NAME),
    ("next-steps", NEXT_STEPS),
    ("organize-notes", ORGANIZE_NOTES),
    ("publish-runner", PUBLISH_RUNNER),
    ("pull-all", PULL_ALL),
    ("pull-scoped", PULL_SCOPED),
    ("pvi", PVI),
    ("qa", QA),
    ("recursive-automation", RECURSIVE_AUTOMATION),
    ("refactor-srp", REFACTOR_SRP),
    ("reflect-ui-bridge", REFLECT_UI_BRIDGE),
    ("research-plan", RESEARCH_PLAN),
    ("resume-foreign", RESUME_FOREIGN),
    ("review-before-code", REVIEW_BEFORE_CODE),
    ("review-commit", REVIEW_COMMIT),
    ("review-logs", REVIEW_LOGS),
    ("review-plan", REVIEW_PLAN),
    ("review-plan-next-steps", REVIEW_PLAN_NEXT_STEPS),
    ("rewind-session", REWIND_SESSION),
    ("run-automation", RUN_AUTOMATION),
    ("scout", SCOUT),
    ("security-scan", SECURITY_SCAN),
    ("summarize-session", SUMMARIZE_SESSION),
    ("symbol-claims-warn", SYMBOL_CLAIMS_WARN),
    ("test-ui-bridge", TEST_UI_BRIDGE),
    ("ufix", UFIX),
    ("ui-bridge", UI_BRIDGE),
    ("unattended", UNATTENDED),
    ("update-spec", UPDATE_SPEC),
    ("validate", VALIDATE),
    ("verify-plan-status", VERIFY_PLAN_STATUS),
    ("verify-web", VERIFY_WEB),
    ("vet-imp", VET_IMP),
    ("workflow-runs", WORKFLOW_RUNS),
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
                     but never says a `gate_id` whose `initial_verdict_reason` says the \
                     predicate cannot be evaluated is a REGISTERED-BUT-NOT-USABLE gate. \
                     (The rule also tested `warnings[]` emptiness until 2026-08-31, when \
                     that half was narrowed away as over-broad.) Every \
                     registration path needs both rules, not just the file as a whole; add \
                     the Warnings-honesty bullet to this path in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
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

    /// The bundled commands must never acquire a genuinely OPERATOR-LOCAL
    /// absolute path (a Windows user profile, a specific machine's home
    /// directory). Mirrors `fleet_skills::tests::bundled_skills_have_no_operator_local_paths`
    /// one module over: these bodies ship to every fleet device the same way
    /// the embedded skills do, so a path rooted on one operator's machine is a
    /// dead pointer on every other one.
    ///
    /// `reflect-ui-bridge` is the one documented exception, for the same
    /// reason the skills test documents its own: it cites
    /// `C:/Users/<someone>/AppData/...` as the ANTI-pattern the command
    /// instructs a session never to hardcode, immediately followed by the
    /// env-resolved alternative — a citation of what not to do, not an
    /// instruction to read that path.
    #[test]
    fn staged_fleet_commands_have_no_operator_local_paths() {
        const FORBIDDEN: &[&str] = &["D:/qontinui-root", "D:\\qontinui-root", "C:/Users/", "/home/"];
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if *name == "reflect-ui-bridge" {
                continue;
            }
            checked += 1;
            for pat in FORBIDDEN {
                assert!(
                    !contents.contains(pat),
                    "bundled agent command {name} contains forbidden operator-local absolute \
                     path {pat:?} — a path rooted on one operator's machine is a dead pointer \
                     on every other fleet device; rewrite it in \
                     src-tauri/src/fleet_commands/{name}.md"
                );
            }
        }
        assert!(
            checked > 0,
            "every bundled command was excluded from this guard — either the bundle is empty \
             or the exclusion list swallowed it; check FLEET_COMMANDS and the exclusion above"
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
