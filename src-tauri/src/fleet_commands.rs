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
//! ## The `.md` files in `fleet_commands/` are VENDORED copies
//!
//! The canonical source of every bundled command is
//! `qontinui-claude-config/.claude/commands/<name>.md`. The files here are
//! vendored copies of those, embedded so the binary carries a working command
//! set with no network and no sibling checkout. The direction was settled in
//! `qontinui-claude-config`'s favour when this runner de-forked its copies
//! (`dd1630ea0`); every later change arrives as a re-vendor commit carrying the
//! canonical bytes. Edit the canonical file, then re-vendor — an edit made only
//! here is a fork. The byte comparison that will police the copy against its
//! canonical file is a planned arm of `qontinui-claude-config`'s command lint
//! (plan `2026-09-13-a-served-command-body-carries-no-provenance-and-its-parity-gate-compares-tokens`,
//! Phases 1–3); until it gates, check #15b compares tokens, not bytes.
//!
//! ## Every written body states its provenance
//!
//! A body is written with ONE generated key at line 2 of its YAML frontmatter
//! (see [`with_provenance`]):
//!
//! ```text
//! qontinui-provenance: source=<builtin|served|disk_cache> canonical=qontinui-claude-config:.claude/commands/<name>.md blob=<sha1> runner_build=<RUNNER_BUILD_ID>
//! ```
//!
//! `blob` is the git blob id of the body bytes with the key excluded. For a
//! builtin it equals `git hash-object` of the VENDORED file this build
//! embedded — and of the canonical file only while the two are in byte parity.
//! After a fetch, `git -C qontinui-claude-config log --all --find-object=<blob>
//! -- .claude/commands/<name>.md` separates a stale copy (the blob is an older
//! canonical version) from a fork (no version of that file ever held it).
//! A provisioned file is checkable on its own — no sibling checkout, no runner —
//! by [`provenance_consistent`], or from a shell: when line 3 is `---` (a
//! prepended block) `tail -n +4 <file> | git hash-object --stdin`, otherwise
//! (the key was inserted into existing frontmatter) `sed 2d <file> | git
//! hash-object --stdin`; with no git at all, the sha1 of
//! `blob <byte-length>\0<body>`. The key is a write-time transform only: the
//! embedded consts and the files beside this module never carry it.
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

use crate::agent_commands::{AgentCommandRegistry, CommandSource};
use crate::capability_manifest::{self, CapabilityObservation, ProvisionReport};

/// `/vet-plan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/vet-plan.md` (canonical) — edit it
/// there, then re-vendor.
const VET_PLAN: &str = include_str!("fleet_commands/vet-plan.md");

/// `/implement-plan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/implement-plan.md` (canonical) — edit it
/// there, then re-vendor.
const IMPLEMENT_PLAN: &str = include_str!("fleet_commands/implement-plan.md");

/// `/policy` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/policy.md` (canonical) — edit it
/// there, then re-vendor.
///
/// One of the five COORD DOORS added to the bundle: the read door for the
/// fleet policy documents. Bundled rather than left to the account layer
/// because the override fetch itself needs a reachable backend — an agent that
/// cannot reach the network is exactly the agent that needs to read policy and
/// report why it is stuck.
const POLICY: &str = include_str!("fleet_commands/policy.md");

/// `/gate` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/gate.md` (canonical) — edit it
/// there, then re-vendor.
///
/// The transport-agnostic gate register/attest/withdraw door. Same reasoning
/// as [`POLICY`]: registering a gate is how a blocked agent makes its blocker
/// observable, so it must not itself depend on a healthy transport.
const GATE: &str = include_str!("fleet_commands/gate.md");

/// `/whereami` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/whereami.md` (canonical) — edit it
/// there, then re-vendor.
///
/// Reports session IDENTITY from `$QONTINUI_RUNNER_CONTEXT` (never a port
/// probe). Bundled because it answers "what am I running inside" — a question
/// whose answer must not depend on the thing being diagnosed.
const WHEREAMI: &str = include_str!("fleet_commands/whereami.md");

/// `/blocked` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/blocked.md` (canonical) — edit it
/// there, then re-vendor.
///
/// The session-close emit-on-block protocol. This is the LAST thing a stuck
/// session runs, so it is the one command least able to rely on a fetch having
/// succeeded earlier.
const BLOCKED: &str = include_str!("fleet_commands/blocked.md");

/// `/gate-sweep` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/gate-sweep.md` (canonical) — edit it
/// there, then re-vendor.
///
/// Reports open/closed gates. Bundled alongside [`GATE`] and [`BLOCKED`] so the
/// register/report pair is never half-present.
const GATE_SWEEP: &str = include_str!("fleet_commands/gate-sweep.md");

/// `/add-tests` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/add-tests.md` (canonical) — edit it
/// there, then re-vendor.
const ADD_TESTS: &str = include_str!("fleet_commands/add-tests.md");

/// `/add-types` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/add-types.md` (canonical) — edit it
/// there, then re-vendor.
const ADD_TYPES: &str = include_str!("fleet_commands/add-types.md");

/// `/analyze-automation` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/analyze-automation.md` (canonical) — edit it
/// there, then re-vendor.
const ANALYZE_AUTOMATION: &str = include_str!("fleet_commands/analyze-automation.md");

/// `/analyze-subagent` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/analyze-subagent.md` (canonical) — edit it
/// there, then re-vendor.
const ANALYZE_SUBAGENT: &str = include_str!("fleet_commands/analyze-subagent.md");

/// `/ask-operator` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/ask-operator.md` (canonical) — edit it
/// there, then re-vendor.
const ASK_OPERATOR: &str = include_str!("fleet_commands/ask-operator.md");

/// `/audit` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/audit.md` (canonical) — edit it
/// there, then re-vendor.
const AUDIT: &str = include_str!("fleet_commands/audit.md");

/// `/auto-fix` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/auto-fix.md` (canonical) — edit it
/// there, then re-vendor.
const AUTO_FIX: &str = include_str!("fleet_commands/auto-fix.md");

/// `/auto-improve` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/auto-improve.md` (canonical) — edit it
/// there, then re-vendor.
const AUTO_IMPROVE: &str = include_str!("fleet_commands/auto-improve.md");

/// `/auto-review` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/auto-review.md` (canonical) — edit it
/// there, then re-vendor.
const AUTO_REVIEW: &str = include_str!("fleet_commands/auto-review.md");

/// `/babysit-prs` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/babysit-prs.md` (canonical) — edit it
/// there, then re-vendor.
const BABYSIT_PRS: &str = include_str!("fleet_commands/babysit-prs.md");

/// `/clean-commit` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/clean-commit.md` (canonical) — edit it
/// there, then re-vendor.
const CLEAN_COMMIT: &str = include_str!("fleet_commands/clean-commit.md");

/// `/clean` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/clean.md` (canonical) — edit it
/// there, then re-vendor.
const CLEAN: &str = include_str!("fleet_commands/clean.md");

/// `/code-analyze` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/code-analyze.md` (canonical) — edit it
/// there, then re-vendor.
const CODE_ANALYZE: &str = include_str!("fleet_commands/code-analyze.md");

/// `/code-fix` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/code-fix.md` (canonical) — edit it
/// there, then re-vendor.
const CODE_FIX: &str = include_str!("fleet_commands/code-fix.md");

/// `/coordinate` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/coordinate.md` (canonical) — edit it
/// there, then re-vendor.
const COORDINATE: &str = include_str!("fleet_commands/coordinate.md");

/// `/create-plan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/create-plan.md` (canonical) — edit it
/// there, then re-vendor.
const CREATE_PLAN: &str = include_str!("fleet_commands/create-plan.md");

/// `/create-tutorial` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/create-tutorial.md` (canonical) — edit it
/// there, then re-vendor.
const CREATE_TUTORIAL: &str = include_str!("fleet_commands/create-tutorial.md");

/// `/debug-loop` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/debug-loop.md` (canonical) — edit it
/// there, then re-vendor.
const DEBUG_LOOP: &str = include_str!("fleet_commands/debug-loop.md");

/// `/debug` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/debug.md` (canonical) — edit it
/// there, then re-vendor.
const DEBUG: &str = include_str!("fleet_commands/debug.md");

/// `/find-debt` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/find-debt.md` (canonical) — edit it
/// there, then re-vendor.
const FIND_DEBT: &str = include_str!("fleet_commands/find-debt.md");

/// `/find-misplaced` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/find-misplaced.md` (canonical) — edit it
/// there, then re-vendor.
const FIND_MISPLACED: &str = include_str!("fleet_commands/find-misplaced.md");

/// `/fix` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/fix.md` (canonical) — edit it
/// there, then re-vendor.
const FIX: &str = include_str!("fleet_commands/fix.md");

/// `/implement-phase` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/implement-phase.md` (canonical) — edit it
/// there, then re-vendor.
const IMPLEMENT_PHASE: &str = include_str!("fleet_commands/implement-phase.md");

/// `/improve-all` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/improve-all.md` (canonical) — edit it
/// there, then re-vendor.
const IMPROVE_ALL: &str = include_str!("fleet_commands/improve-all.md");

/// `/manual-test-coord-loop` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/manual-test-coord-loop.md` (canonical) — edit it
/// there, then re-vendor.
const MANUAL_TEST_COORD_LOOP: &str = include_str!("fleet_commands/manual-test-coord-loop.md");

/// `/manual-test-coord` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/manual-test-coord.md` (canonical) — edit it
/// there, then re-vendor.
const MANUAL_TEST_COORD: &str = include_str!("fleet_commands/manual-test-coord.md");

/// `/manual-test-loop` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/manual-test-loop.md` (canonical) — edit it
/// there, then re-vendor.
const MANUAL_TEST_LOOP: &str = include_str!("fleet_commands/manual-test-loop.md");

/// `/manual-test` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/manual-test.md` (canonical) — edit it
/// there, then re-vendor.
const MANUAL_TEST: &str = include_str!("fleet_commands/manual-test.md");

/// `/merge-train-steward` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/merge-train-steward.md` (canonical) — edit it
/// there, then re-vendor.
const MERGE_TRAIN_STEWARD: &str = include_str!("fleet_commands/merge-train-steward.md");

/// `/mobile-dev` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/mobile-dev.md` (canonical) — edit it
/// there, then re-vendor.
const MOBILE_DEV: &str = include_str!("fleet_commands/mobile-dev.md");

/// `/mobile-verify` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/mobile-verify.md` (canonical) — edit it
/// there, then re-vendor.
const MOBILE_VERIFY: &str = include_str!("fleet_commands/mobile-verify.md");

/// `/mtc` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/mtc.md` (canonical) — edit it
/// there, then re-vendor.
const MTC: &str = include_str!("fleet_commands/mtc.md");

/// `/name` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/name.md` (canonical) — edit it
/// there, then re-vendor.
const NAME: &str = include_str!("fleet_commands/name.md");

/// `/next-steps` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/next-steps.md` (canonical) — edit it
/// there, then re-vendor.
const NEXT_STEPS: &str = include_str!("fleet_commands/next-steps.md");

/// `/organize-notes` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/organize-notes.md` (canonical) — edit it
/// there, then re-vendor.
const ORGANIZE_NOTES: &str = include_str!("fleet_commands/organize-notes.md");

/// `/publish-runner` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/publish-runner.md` (canonical) — edit it
/// there, then re-vendor.
const PUBLISH_RUNNER: &str = include_str!("fleet_commands/publish-runner.md");

/// `/pull-all` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/pull-all.md` (canonical) — edit it
/// there, then re-vendor.
const PULL_ALL: &str = include_str!("fleet_commands/pull-all.md");

/// `/pull-scoped` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/pull-scoped.md` (canonical) — edit it
/// there, then re-vendor.
const PULL_SCOPED: &str = include_str!("fleet_commands/pull-scoped.md");

/// `/pvi` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/pvi.md` (canonical) — edit it
/// there, then re-vendor.
const PVI: &str = include_str!("fleet_commands/pvi.md");

/// `/qa` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/qa.md` (canonical) — edit it
/// there, then re-vendor.
const QA: &str = include_str!("fleet_commands/qa.md");

/// `/recursive-automation` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/recursive-automation.md` (canonical) — edit it
/// there, then re-vendor.
const RECURSIVE_AUTOMATION: &str = include_str!("fleet_commands/recursive-automation.md");

/// `/refactor-srp` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/refactor-srp.md` (canonical) — edit it
/// there, then re-vendor.
const REFACTOR_SRP: &str = include_str!("fleet_commands/refactor-srp.md");

/// `/reflect-ui-bridge` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/reflect-ui-bridge.md` (canonical) — edit it
/// there, then re-vendor.
const REFLECT_UI_BRIDGE: &str = include_str!("fleet_commands/reflect-ui-bridge.md");

/// `/research-plan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/research-plan.md` (canonical) — edit it
/// there, then re-vendor.
const RESEARCH_PLAN: &str = include_str!("fleet_commands/research-plan.md");

/// `/resume-foreign` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/resume-foreign.md` (canonical) — edit it
/// there, then re-vendor.
const RESUME_FOREIGN: &str = include_str!("fleet_commands/resume-foreign.md");

/// `/review-before-code` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/review-before-code.md` (canonical) — edit it
/// there, then re-vendor.
const REVIEW_BEFORE_CODE: &str = include_str!("fleet_commands/review-before-code.md");

/// `/review-commit` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/review-commit.md` (canonical) — edit it
/// there, then re-vendor.
const REVIEW_COMMIT: &str = include_str!("fleet_commands/review-commit.md");

/// `/review-logs` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/review-logs.md` (canonical) — edit it
/// there, then re-vendor.
const REVIEW_LOGS: &str = include_str!("fleet_commands/review-logs.md");

/// `/review-plan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/review-plan.md` (canonical) — edit it
/// there, then re-vendor.
const REVIEW_PLAN: &str = include_str!("fleet_commands/review-plan.md");

/// `/review-plan-next-steps` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/review-plan-next-steps.md` (canonical) — edit it
/// there, then re-vendor.
const REVIEW_PLAN_NEXT_STEPS: &str = include_str!("fleet_commands/review-plan-next-steps.md");

/// `/rewind-session` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/rewind-session.md` (canonical) — edit it
/// there, then re-vendor.
const REWIND_SESSION: &str = include_str!("fleet_commands/rewind-session.md");

/// `/run-automation` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/run-automation.md` (canonical) — edit it
/// there, then re-vendor.
const RUN_AUTOMATION: &str = include_str!("fleet_commands/run-automation.md");

/// `/scout` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/scout.md` (canonical) — edit it
/// there, then re-vendor.
const SCOUT: &str = include_str!("fleet_commands/scout.md");

/// `/security-scan` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/security-scan.md` (canonical) — edit it
/// there, then re-vendor.
const SECURITY_SCAN: &str = include_str!("fleet_commands/security-scan.md");

/// `/summarize-session` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/summarize-session.md` (canonical) — edit it
/// there, then re-vendor.
const SUMMARIZE_SESSION: &str = include_str!("fleet_commands/summarize-session.md");

/// `/symbol-claims-warn` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/symbol-claims-warn.md` (canonical) — edit it
/// there, then re-vendor.
const SYMBOL_CLAIMS_WARN: &str = include_str!("fleet_commands/symbol-claims-warn.md");

/// `/test-ui-bridge` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/test-ui-bridge.md` (canonical) — edit it
/// there, then re-vendor.
const TEST_UI_BRIDGE: &str = include_str!("fleet_commands/test-ui-bridge.md");

/// `/ufix` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/ufix.md` (canonical) — edit it
/// there, then re-vendor.
const UFIX: &str = include_str!("fleet_commands/ufix.md");

/// `/ui-bridge` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/ui-bridge.md` (canonical) — edit it
/// there, then re-vendor.
const UI_BRIDGE: &str = include_str!("fleet_commands/ui-bridge.md");

/// `/unattended` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/unattended.md` (canonical) — edit it
/// there, then re-vendor.
const UNATTENDED: &str = include_str!("fleet_commands/unattended.md");

/// `/update-spec` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/update-spec.md` (canonical) — edit it
/// there, then re-vendor.
const UPDATE_SPEC: &str = include_str!("fleet_commands/update-spec.md");

/// `/validate` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/validate.md` (canonical) — edit it
/// there, then re-vendor.
const VALIDATE: &str = include_str!("fleet_commands/validate.md");

/// `/verify-plan-status` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/verify-plan-status.md` (canonical) — edit it
/// there, then re-vendor.
const VERIFY_PLAN_STATUS: &str = include_str!("fleet_commands/verify-plan-status.md");

/// `/verify-web` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/verify-web.md` (canonical) — edit it
/// there, then re-vendor.
const VERIFY_WEB: &str = include_str!("fleet_commands/verify-web.md");

/// `/vet-imp` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/vet-imp.md` (canonical) — edit it
/// there, then re-vendor.
const VET_IMP: &str = include_str!("fleet_commands/vet-imp.md");

/// `/workflow-runs` procedure, bundled into the binary. Vendored from
/// qontinui-claude-config `.claude/commands/workflow-runs.md` (canonical) — edit it
/// there, then re-vendor.
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

/// The YAML frontmatter key [`with_provenance`] writes at line 2 of every
/// provisioned command file.
pub(crate) const PROVENANCE_KEY: &str = "qontinui-provenance:";

/// The build identity stamped into `runner_build=` — the same compile-time
/// value `/health` reports as `buildId`.
const RUNNER_BUILD: &str = env!("RUNNER_BUILD_ID");

/// The parsed fields of one `qontinui-provenance:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvenanceLine {
    /// `builtin` / `served` / `disk_cache` — [`CommandSource::as_str`].
    pub source: String,
    /// `qontinui-claude-config:.claude/commands/<name>.md`.
    pub canonical: String,
    /// Lowercase 40-hex git blob id of the body, provenance excluded.
    pub blob: String,
    /// The `RUNNER_BUILD_ID` of the binary that wrote the file.
    pub runner_build: String,
}

impl ProvenanceLine {
    /// Parse the text after [`PROVENANCE_KEY`]. `None` unless all four fields
    /// are present.
    fn parse(value: &str) -> Option<Self> {
        let (mut source, mut canonical, mut blob, mut runner_build) = (None, None, None, None);
        for token in value.split_whitespace() {
            let (k, v) = token.split_once('=')?;
            let slot = match k {
                "source" => &mut source,
                "canonical" => &mut canonical,
                "blob" => &mut blob,
                "runner_build" => &mut runner_build,
                _ => continue,
            };
            *slot = Some(v.to_string());
        }
        Some(Self {
            source: source?,
            canonical: canonical?,
            blob: blob?,
            runner_build: runner_build?,
        })
    }
}

/// Why [`provenance_consistent`] refused a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProvenanceError {
    /// No well-formed `qontinui-provenance:` line at line 2 of a frontmatter
    /// block — the file carries no claim to check.
    Missing,
    /// The body no longer hashes to the blob the line recorded: it was edited
    /// after it was written, or the line was copied onto another body.
    BlobMismatch { recorded: String, actual: String },
}

/// Git blob id (`git hash-object`) of `bytes`, lowercase 40-hex.
fn git_blob_id(bytes: &[u8]) -> String {
    // Hashing an in-memory buffer cannot fail for the Blob type; the fallback
    // keeps this total rather than panicking inside a fail-soft provisioner.
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

/// The content of a line with its terminator (`\n` or `\r\n`) removed.
fn line_content(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .unwrap_or(line)
}

/// `text` split into its first line (terminator included) and the remainder.
fn split_first_line(text: &str) -> (&str, &str) {
    match text.find('\n') {
        Some(i) => text.split_at(i + 1),
        None => (text, ""),
    }
}

/// `body` with ONE generated `qontinui-provenance:` key placed at line 2, in
/// YAML frontmatter.
///
/// Placement, because YAML frontmatter is recognised only when it starts at
/// line 1 (the convention Claude Code's command loader follows) — nothing is ever put above an existing `---`:
/// - a body that opens a NON-EMPTY frontmatter block (`---\n` or `---\r\n`
///   followed by anything but a closing `---`) gets the key inserted as the
///   first line inside it, with the opener's own line ending;
/// - every other body gets a new `---\n<key>\n---\n` block prepended and is
///   otherwise unchanged. That includes a body opening an EMPTY block
///   (`---\n---\n`): inserting into it would produce bytes identical to a
///   prepended block over the empty block's remainder, and
///   [`strip_provenance`] could not tell the two apart.
///
/// `blob` is computed over `body` BEFORE the key is added, so it equals
/// `git hash-object` of the vendored file for an unmodified builtin (and of the
/// canonical file only while the two are in byte parity).
pub(crate) fn with_provenance(name: &str, body: &str, source: CommandSource) -> String {
    let key = format!(
        "{PROVENANCE_KEY} source={} canonical=qontinui-claude-config:.claude/commands/{name}.md \
         blob={} runner_build={RUNNER_BUILD}",
        source.as_str(),
        git_blob_id(body.as_bytes()),
    );
    let (first, rest) = split_first_line(body);
    let opens_block = first == "---\n" || first == "---\r\n";
    let (second, _) = split_first_line(rest);
    if opens_block && line_content(second) != "---" {
        let eol = if first.ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        format!("{first}{key}{eol}{rest}")
    } else {
        format!("---\n{key}\n---\n{body}")
    }
}

/// Undo [`with_provenance`]: remove exactly the one `qontinui-provenance:` line
/// at line 2 — and the frontmatter block around it when that block is then
/// empty (the one `with_provenance` created) — returning the parsed line and
/// the original body, byte-for-byte. `None` when line 2 is not a well-formed
/// provenance line inside a frontmatter opener.
pub(crate) fn strip_provenance(text: &str) -> Option<(ProvenanceLine, String)> {
    let (first, rest) = split_first_line(text);
    if first != "---\n" && first != "---\r\n" {
        return None;
    }
    let (key_line, after_key) = split_first_line(rest);
    let value = line_content(key_line).strip_prefix(PROVENANCE_KEY)?;
    let parsed = ProvenanceLine::parse(value)?;
    let (third, after_third) = split_first_line(after_key);
    let body = if line_content(third) == "---" {
        // The block holds nothing but the key: `with_provenance` created it.
        after_third.to_string()
    } else {
        format!("{first}{after_key}")
    };
    Some((parsed, body))
}

/// Check a provisioned command file against its own provenance line: strip the
/// line, re-hash what remains, and compare with the recorded `blob`. Needs
/// nothing but the file — no sibling checkout, no runner.
pub(crate) fn provenance_consistent(text: &str) -> Result<ProvenanceLine, ProvenanceError> {
    let (line, body) = strip_provenance(text).ok_or(ProvenanceError::Missing)?;
    let actual = git_blob_id(body.as_bytes());
    if actual == line.blob {
        Ok(line)
    } else {
        Err(ProvenanceError::BlobMismatch {
            recorded: line.blob,
            actual,
        })
    }
}

/// What a destination that ALREADY EXISTS says about itself, read back from its
/// own provenance line just before [`provision_fleet_commands_into`] overwrites
/// it.
///
/// This is the production reader of [`provenance_consistent`]. Without it the
/// whole reader half of the provenance feature — [`ProvenanceLine`],
/// [`ProvenanceError`], [`strip_provenance`], [`provenance_consistent`] — is
/// reachable only from tests, and the module doc's claim that a provisioned
/// file "is checkable on its own" holds only for a reader willing to hand-roll
/// the `git hash-object` pipeline that doc spells out.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Existing {
    /// A runner wrote it and nothing has touched it since: the body still
    /// hashes to the blob its own line records. Replacing it loses nothing, so
    /// this is the silent, overwhelmingly common case.
    Pristine,
    /// A runner wrote it and it was EDITED afterwards. The overwrite below
    /// discards that edit.
    Edited(Box<EditedFile>),
    /// No well-formed provenance line: a pre-provenance runner build wrote it,
    /// or something that is not this provisioner did.
    Unstamped,
}

/// The fields worth naming when an edited file is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditedFile {
    /// Which rung supplied the bytes that were written (`CommandSource`).
    source: String,
    /// The `RUNNER_BUILD_ID` of the build that wrote the file.
    runner_build: String,
    /// `qontinui-claude-config:.claude/commands/<name>.md` — where the edit
    /// SHOULD have gone.
    canonical: String,
    /// The blob the provenance line recorded when the file was written.
    recorded: String,
    /// What the body hashes to now.
    actual: String,
}

/// Classify an existing destination. `None` when there is nothing to classify —
/// the path does not exist, or is unreadable as UTF-8.
///
/// Fail-soft on purpose, exactly as `TrackedPaths::probe` is: every read error
/// resolves to `None`, i.e. to writing exactly as before. Reading a file back
/// is a logging concern and must never turn a provisioning pass into a failed
/// session spawn.
fn classify_existing(dst: &Path) -> Option<Existing> {
    let text = std::fs::read_to_string(dst).ok()?;
    match provenance_consistent(&text) {
        Ok(_) => Some(Existing::Pristine),
        Err(ProvenanceError::Missing) => Some(Existing::Unstamped),
        Err(ProvenanceError::BlobMismatch { recorded, actual }) => {
            // The error deliberately carries only the two blobs, so re-read the
            // line for the rest. It parsed once inside `provenance_consistent`,
            // so the fallback is unreachable; it keeps the classifier total
            // rather than panicking inside a fail-soft provisioner.
            let line = strip_provenance(&text)
                .map(|(line, _)| line)
                .unwrap_or_else(|| ProvenanceLine {
                    source: String::new(),
                    canonical: String::new(),
                    blob: String::new(),
                    runner_build: String::new(),
                });
            Some(Existing::Edited(Box::new(EditedFile {
                source: line.source,
                runner_build: line.runner_build,
                canonical: line.canonical,
                recorded,
                actual,
            })))
        }
    }
}

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

    // The registry's own row: WHICH of `resolve_registry`'s three arms answered.
    // Recorded before the write, because it is a fact about resolution rather
    // than about provisioning and holds even if every write below is skipped.
    let arm = registry.resolution_arm();
    crate::capability_manifest::record_observation(
        "agent_commands_registry",
        CapabilityObservation::from_command_source(arm).with_detail(format!(
            "CommandSource::{} — {} override(s) over {} embedded default(s)",
            arm.as_str(),
            registry.override_count(),
            registry.builtin_count(),
        )),
    );

    match provision_fleet_commands_into(&commands_dir, &registry) {
        Ok(report) => crate::capability_manifest::record_provision(workdir, report),
        Err(e) => {
            // The destination directory itself could not be created, so no unit
            // was even attempted. Still fail-soft — the spawn continues — but it
            // is now a ROW rather than only a log line.
            warn!(
                "fleet_commands: failed to provision agent commands into {} \
                 (continuing spawn; the fleet slash commands may not resolve): {e}",
                commands_dir.display()
            );
            let mut report = ProvisionReport::new(
                "fleet_commands",
                registry.all().len(),
                capability_manifest::Rung::Unresolved,
            )
            .with_destination(commands_dir.display().to_string());
            report.skip(
                commands_dir.display().to_string(),
                capability_manifest::SkipReason::WriteFailed(e.to_string()),
            );
            crate::capability_manifest::record_provision(workdir, report);
        }
    }
}

/// Core of [`provision_fleet_commands_for_session`]: create `commands_dir` and
/// write every resolved command into it, returning the counts. Split out so a
/// unit test can drive it against a tempdir and assert the result — mirroring
/// how `provision_agent_definitions` factored out its `_from_root` core.
///
/// Each body is written through [`with_provenance`], so the file on disk is
/// the resolved body plus one `qontinui-provenance:` frontmatter line.
///
/// Idempotent (a second pass over the same dir overwrites rather than errors),
/// with ONE exception: a destination that already exists AND is tracked in the
/// enclosing git repository is skipped, logged at `info!`, and counted in
/// [`ProvisionReport::skipped`] WITH its reason. Spawning a session with such a
/// checkout as its cwd
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
) -> std::io::Result<ProvisionReport> {
    std::fs::create_dir_all(commands_dir)?;
    let tracked = crate::provision_guard::TrackedPaths::probe(commands_dir);
    let resolved = registry.all();
    // The BODIES are the embedded defaults unless an override replaced one; the
    // rung of the account layer itself is the separate `agent_commands_registry`
    // row, which `provision_fleet_commands_for_session` records.
    let mut out = ProvisionReport::new(
        "fleet_commands",
        resolved.len(),
        capability_manifest::Rung::Embedded,
    )
    .with_destination(commands_dir.display().to_string());
    let mut edited = 0usize;
    for command in &resolved {
        let file_name = command.file_name();
        let dst = commands_dir.join(&file_name);
        if tracked.should_skip(&dst, Path::new(&file_name)) {
            info!(
                "fleet_commands: skipping {} — it is tracked by the enclosing git \
                 repository, and overwriting it would silently replace that repo's \
                 own content and dirty its tree",
                dst.display()
            );
            out.skip(file_name, capability_manifest::SkipReason::GitTracked);
            continue;
        }
        // The write below is still unconditional — this reads the OUTGOING file
        // only to say what is being lost. The tracked-file guard above covers
        // the case where the enclosing REPO owns the file; nothing covered the
        // case where a PERSON edited an untracked provisioned copy, and that
        // overwrite has been silent since this provisioner was written.
        match classify_existing(&dst) {
            Some(Existing::Edited(e)) => {
                edited += 1;
                warn!(
                    "fleet_commands: replacing {} — it was written by runner build \
                     {} from the {} body and EDITED afterwards (its provenance line \
                     records blob {}, its body now hashes to {}). That edit is being \
                     discarded. Edit {} instead, then re-vendor.",
                    dst.display(),
                    e.runner_build,
                    e.source,
                    e.recorded,
                    e.actual,
                    e.canonical,
                );
            }
            Some(Existing::Unstamped) => {
                info!(
                    "fleet_commands: replacing {} — it carries no provenance line, so \
                     a pre-provenance runner build or something other than this \
                     provisioner wrote it",
                    dst.display()
                );
            }
            // Pristine (a runner wrote it and nobody touched it) and absent are
            // the ordinary paths, and say nothing.
            Some(Existing::Pristine) | None => {}
        }
        std::fs::write(
            &dst,
            with_provenance(&command.name, &command.body, command.source),
        )?;
        out.record_written();
    }
    // Built after the loop rather than at construction so it can carry the
    // read-back count; a pass that clobbered nobody's edits reads as before.
    let mut detail = format!(
        "{} embedded default(s), {} account override(s)",
        registry.builtin_count(),
        registry.override_count()
    );
    if edited > 0 {
        detail.push_str(&format!(
            "; replaced {edited} hand-edited file(s) — see the warnings above"
        ));
    }
    out = out.with_detail(detail);
    // Nothing landed at all, so no rung answered for this session — a stated
    // outcome rather than a claim that the embedded floor delivered.
    if out.written == 0 {
        out.set_rung(capability_manifest::Rung::Unresolved);
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
        assert!(out.skipped.is_empty(), "nothing here is git-tracked");
        assert!(out.is_complete(), "a full pass must not read as degraded");

        // Every embedded default lands, byte-identically to what
        // `include_str!` embedded.
        for (name, body) in FLEET_COMMANDS {
            let path = commands_dir.join(format!("{name}.md"));
            assert!(path.exists(), "{name}.md should exist");
            let on_disk = std::fs::read_to_string(&path).expect("read command");
            assert!(!on_disk.is_empty(), "{name}.md should be non-empty");
            let (line, stripped) =
                strip_provenance(&on_disk).expect("every written body carries provenance");
            assert_eq!(
                &stripped, body,
                "{name}.md minus its provenance line must be byte-identical to the embedded default"
            );
            assert_eq!(line.source, "builtin");
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
        registry.set_overrides(
            vec![qontinui_types::agent_commands::AgentCommand {
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
            }],
            crate::agent_commands::CommandSource::Served,
        );

        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert_eq!(
            out.written,
            FLEET_COMMANDS.len(),
            "an override replaces a default; it does not add a file"
        );
        let on_disk = std::fs::read_to_string(commands_dir.join(format!("{name}.md"))).unwrap();
        let (_, on_disk) = strip_provenance(&on_disk).expect("provenance line");
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

        assert_eq!(out.skipped.len(), 1, "the tracked file should be skipped");
        // The REASON is the deliverable: a bare count is the same unreadable
        // signal the `warn!` this report replaces already was.
        assert_eq!(
            out.skipped[0].reason,
            crate::capability_manifest::SkipReason::GitTracked
        );
        assert_eq!(out.skipped[0].unit, format!("{tracked_name}.md"));
        assert!(out.is_degraded(), "a skipped unit means the pass degraded");
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
        assert_eq!(out.skipped.len(), FLEET_COMMANDS.len());
        assert!(
            out.skipped
                .iter()
                .all(|s| s.reason == crate::capability_manifest::SkipReason::GitTracked),
            "every skip must carry the reason it was skipped for"
        );
        // Nothing landed, so no rung answered — stated, not assumed.
        assert_eq!(out.rung, crate::capability_manifest::Rung::Unresolved);
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
        registry.set_overrides(
            vec![qontinui_types::agent_commands::AgentCommand {
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
            }],
            crate::agent_commands::CommandSource::Served,
        );

        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");

        assert_eq!(out.skipped.len(), 1);
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

        assert!(
            out.skipped.is_empty(),
            "nothing is tracked, so nothing is skipped"
        );
        assert_eq!(out.written, FLEET_COMMANDS.len());
        assert_eq!(
            &strip_provenance(&std::fs::read_to_string(&dst).unwrap())
                .expect("provenance line")
                .1,
            body,
            "an untracked destination is overwritten exactly as before"
        );
    }

    /// A provisioned command file, read back.
    fn provision_one(registry: &AgentCommandRegistry, name: &str) -> String {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        provision_fleet_commands_into(&commands_dir, registry).expect("provision");
        std::fs::read_to_string(commands_dir.join(format!("{name}.md"))).expect("read command")
    }

    fn embedded(name: &str) -> &'static str {
        FLEET_COMMANDS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, b)| *b)
            .unwrap_or_else(|| panic!("{name} is not bundled"))
    }

    fn override_registry(name: &str, body: &str, source: CommandSource) -> AgentCommandRegistry {
        let mut registry = AgentCommandRegistry::new();
        registry.set_overrides(
            vec![qontinui_types::agent_commands::AgentCommand {
                id: "id-1".to_string(),
                organization_id: Some("org-1".to_string()),
                created_by_user_id: None,
                name: name.to_string(),
                body: body.to_string(),
                checksum: None,
                is_shared: false,
                current_version: 1,
                created_at: "2026-08-04T00:00:00Z".to_string(),
                updated_at: "2026-08-04T00:00:00Z".to_string(),
            }],
            source,
        );
        registry
    }

    /// A body with no frontmatter gets a NEW block whose only line is the
    /// provenance key, and everything after the block is the body unchanged.
    #[test]
    fn a_plain_body_gets_a_new_frontmatter_block_holding_only_the_key() {
        let original = embedded("vet-plan");
        assert!(
            !original.starts_with("---"),
            "fixture must be frontmatter-free"
        );
        let on_disk = provision_one(&AgentCommandRegistry::new(), "vet-plan");

        let lines: Vec<&str> = on_disk.splitn(4, '\n').collect();
        assert_eq!(lines[0], "---");
        assert!(
            lines[1].starts_with(&format!("{PROVENANCE_KEY} source=builtin ")),
            "line 2 must be the provenance key, got {:?}",
            lines[1]
        );
        assert!(lines[1].contains("canonical=qontinui-claude-config:.claude/commands/vet-plan.md "));
        assert!(lines[1].ends_with(&format!(" runner_build={RUNNER_BUILD}")));
        assert_eq!(lines[2], "---");
        assert_eq!(
            lines[3], original,
            "the rest of the file is the body, unchanged"
        );
    }

    /// A body that already opens frontmatter keeps it parseable: `---` still at
    /// line 1, the key first inside the block, every original key after it and
    /// the closing `---` intact.
    #[test]
    fn a_frontmatter_body_keeps_its_keys_with_the_provenance_line_first() {
        let original = embedded("policy");
        assert!(
            original.starts_with("---\n"),
            "fixture must open frontmatter"
        );
        let on_disk = provision_one(&AgentCommandRegistry::new(), "policy");

        let mut lines = on_disk.lines();
        assert_eq!(lines.next(), Some("---"));
        assert!(lines.next().unwrap().starts_with(PROVENANCE_KEY));
        // Everything from the original's line 2 onward follows verbatim.
        let original_tail = original.split_once('\n').unwrap().1;
        let written_tail = on_disk
            .split_once('\n')
            .unwrap()
            .1
            .split_once('\n')
            .unwrap()
            .1;
        assert_eq!(written_tail, original_tail);
        // The original block's keys are still inside one frontmatter block.
        let block: Vec<&str> = on_disk
            .lines()
            .skip(1)
            .take_while(|l| *l != "---")
            .collect();
        assert!(block.iter().any(|l| l.starts_with("description:")));
        assert_eq!(
            on_disk.lines().filter(|l| *l == "---").count(),
            original.lines().filter(|l| *l == "---").count(),
            "no fence added or lost"
        );
    }

    /// `blob` is the git blob id of the body WITHOUT the key — for a builtin,
    /// the `git hash-object` of the vendored file itself.
    #[test]
    fn the_recorded_blob_is_the_git_blob_id_of_the_original_body() {
        // Known answer: `printf 'hello\n' | git hash-object --stdin`.
        assert_eq!(
            git_blob_id(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        for name in ["vet-plan", "policy"] {
            let on_disk = provision_one(&AgentCommandRegistry::new(), name);
            let (line, _) = strip_provenance(&on_disk).expect("provenance line");
            let expected =
                git2::Oid::hash_object(git2::ObjectType::Blob, embedded(name).as_bytes())
                    .unwrap()
                    .to_string();
            assert_eq!(line.blob, expected);
            assert_eq!(line.blob.len(), 40);
            assert!(line
                .blob
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            // The vendored file on disk, read independently of `include_str!`.
            let vendored = std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src")
                    .join("fleet_commands")
                    .join(format!("{name}.md")),
            )
            .expect("read vendored file");
            assert_eq!(line.blob, git_blob_id(&vendored));
        }
    }

    /// `strip_provenance` is the exact inverse of `with_provenance` for every
    /// shape — including CRLF frontmatter and the empty-block edge case whose
    /// insertion would be ambiguous.
    #[test]
    fn strip_provenance_round_trips_every_shape() {
        let mut shapes: Vec<String> = vec![
            "# plain\nbody\n".to_string(),
            "---\ndescription: x\n---\n# body\n".to_string(),
            "---\r\ndescription: x\r\nname: y\r\n---\r\n# body\r\n".to_string(),
            "---\n---\n# empty block\n".to_string(),
            "---\r\n---\r\n# empty CRLF block\n".to_string(),
            "---\n".to_string(),
            "---".to_string(),
            String::new(),
            "no trailing newline".to_string(),
        ];
        shapes.extend(FLEET_COMMANDS.iter().map(|(_, b)| b.to_string()));
        for body in &shapes {
            for source in [
                CommandSource::Builtin,
                CommandSource::Served,
                CommandSource::DiskCache,
            ] {
                let written = with_provenance("x", body, source);
                assert!(
                    written.starts_with("---"),
                    "frontmatter must start at line 1"
                );
                let (line, stripped) = strip_provenance(&written)
                    .unwrap_or_else(|| panic!("no provenance found in {written:?}"));
                assert_eq!(&stripped, body, "round trip of {body:?}");
                assert_eq!(line.source, source.as_str());
                assert_eq!(provenance_consistent(&written), Ok(line));
            }
        }
        // CRLF frontmatter keeps its own line endings on the inserted line.
        let crlf = with_provenance("x", "---\r\na: 1\r\n---\r\n", CommandSource::Builtin);
        assert!(crlf
            .split_once("\r\n")
            .unwrap()
            .1
            .starts_with(PROVENANCE_KEY));
        assert!(!crlf.contains("\n---\n"), "no LF-only fence introduced");
        // A file with no provenance line is not mistaken for one.
        assert_eq!(strip_provenance("---\ndescription: x\n---\n"), None);
        assert_eq!(strip_provenance("# plain\n"), None);
    }

    /// The honest negative, detectable inside ONE file: change a single byte of
    /// a provisioned body and the recorded blob no longer matches.
    #[test]
    fn a_tampered_body_is_detected_from_the_file_alone() {
        for name in ["vet-plan", "policy"] {
            let on_disk = provision_one(&AgentCommandRegistry::new(), name);
            assert!(
                provenance_consistent(&on_disk).is_ok(),
                "untampered is consistent"
            );

            // Flip one byte of the body, well past the frontmatter.
            let mut bytes = on_disk.clone().into_bytes();
            let i = bytes
                .iter()
                .rposition(|b| b.is_ascii_alphanumeric())
                .expect("body has an ascii letter");
            bytes[i] = if bytes[i] == b'x' { b'y' } else { b'x' };
            let tampered = String::from_utf8(bytes).expect("ascii flip stays utf-8");
            match provenance_consistent(&tampered) {
                Err(ProvenanceError::BlobMismatch { recorded, actual }) => {
                    assert_ne!(recorded, actual)
                }
                other => panic!("{name}: a one-byte tamper must be a mismatch, got {other:?}"),
            }
        }
        assert_eq!(
            provenance_consistent("# no provenance\n"),
            Err(ProvenanceError::Missing)
        );
    }

    /// `source=` names the layer that actually supplied the body.
    #[test]
    fn an_override_is_stamped_with_its_own_source() {
        let (name, _) = FLEET_COMMANDS[0];
        for source in [CommandSource::Served, CommandSource::DiskCache] {
            let registry = override_registry(name, "# my own procedure\n", source);
            let on_disk = provision_one(&registry, name);
            let (line, body) = strip_provenance(&on_disk).expect("provenance line");
            assert_eq!(line.source, source.as_str());
            assert_eq!(body, "# my own procedure\n");
            assert_eq!(line.blob, git_blob_id(b"# my own procedure\n"));
            assert_eq!(
                line.canonical,
                format!("qontinui-claude-config:.claude/commands/{name}.md")
            );
            // Commands the override did not touch still say builtin.
            let (other, _) = FLEET_COMMANDS[1];
            let (other_line, _) = strip_provenance(&provision_one(&registry, other)).unwrap();
            assert_eq!(other_line.source, "builtin");
        }
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

    /// A destination this provisioner wrote and NOBODY touched classifies as
    /// pristine, so the ordinary re-provision says nothing.
    #[test]
    fn a_reprovisioned_untouched_destination_classifies_as_pristine() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        let registry = AgentCommandRegistry::new();
        provision_fleet_commands_into(&commands_dir, &registry).expect("first pass");

        let (name, _) = FLEET_COMMANDS[0];
        let dst = commands_dir.join(format!("{name}.md"));
        assert_eq!(classify_existing(&dst), Some(Existing::Pristine));

        // And the second pass reports no clobbered edits.
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("second pass");
        assert_eq!(out.written, FLEET_COMMANDS.len());
        assert!(
            !out.detail.unwrap_or_default().contains("hand-edited"),
            "an untouched re-provision must not claim it replaced an edit"
        );
    }

    /// The case that was silent before: a provisioned file someone EDITED is
    /// still overwritten, but the pass now names it — and the report counts it.
    #[test]
    fn a_hand_edited_destination_is_reported_before_it_is_replaced() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        let registry = AgentCommandRegistry::new();
        provision_fleet_commands_into(&commands_dir, &registry).expect("first pass");

        let (name, _) = FLEET_COMMANDS[0];
        let dst = commands_dir.join(format!("{name}.md"));
        let written = std::fs::read_to_string(&dst).expect("read provisioned");
        let (line, _) = strip_provenance(&written).expect("provenance line");
        std::fs::write(&dst, format!("{written}\nhand-written addition\n")).expect("edit it");

        match classify_existing(&dst) {
            Some(Existing::Edited(e)) => {
                // The recorded blob is the one the file's own line carries, and
                // the actual differs precisely because it was edited.
                assert_eq!(e.recorded, line.blob);
                assert_ne!(e.actual, e.recorded);
                assert_eq!(e.source, "builtin");
                assert_eq!(e.runner_build, line.runner_build);
                assert_eq!(
                    e.canonical,
                    format!("qontinui-claude-config:.claude/commands/{name}.md"),
                    "the warning must point at the canonical file, not the vendored copy"
                );
            }
            other => panic!("an edited file must classify as Edited, got {other:?}"),
        }

        // The write itself is unchanged: the edit is still replaced, and the
        // file is the freshly provisioned body again.
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("second pass");
        assert_eq!(
            out.written,
            FLEET_COMMANDS.len(),
            "the overwrite still happens"
        );
        assert!(
            out.detail
                .as_deref()
                .unwrap_or_default()
                .contains("replaced 1 hand-edited file(s)"),
            "the report must count the clobbered edit, got {:?}",
            out.detail
        );
        assert_eq!(
            classify_existing(&dst),
            Some(Existing::Pristine),
            "after the replacing pass the file is pristine again"
        );
    }

    /// A file that carries no provenance line at all — a pre-provenance runner
    /// build, or something that is not this provisioner — is `Unstamped`, not
    /// mistaken for an edit.
    #[test]
    fn an_unstamped_destination_is_not_reported_as_an_edit() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands_dir = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).expect("mkdir");

        let (name, body) = FLEET_COMMANDS[0];
        let dst = commands_dir.join(format!("{name}.md"));
        std::fs::write(&dst, body).expect("write an unstamped body");
        assert_eq!(classify_existing(&dst), Some(Existing::Unstamped));

        let registry = AgentCommandRegistry::new();
        let out = provision_fleet_commands_into(&commands_dir, &registry).expect("provision");
        assert!(
            !out.detail.unwrap_or_default().contains("hand-edited"),
            "an unstamped file is not an edit of a runner-written one"
        );
    }

    /// Classification is fail-soft: an absent path is `None`, and `None` writes
    /// exactly as before. A missing destination is the normal first-pass case
    /// and must never read as a finding.
    #[test]
    fn an_absent_destination_classifies_as_nothing_at_all() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        assert_eq!(classify_existing(&tmp.path().join("not-there.md")), None);
    }

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
            // Collapsed for uniformity with the other positive prose guards.
            // Both current tokens are single words, so this is a no-op today;
            // it makes a multi-word mechanic added later wrap-proof by default.
            let normalised = collapse_ws(contents);
            for (token, why) in GATE_REGISTRATION_MECHANICS {
                assert!(
                    normalised.contains(&collapse_ws(token)),
                    "bundled agent command {name} documents gate registration but never \
                     mentions {token:?} — {why}. This file is provisioned into every \
                     spawned session and on a device with no qontinui-claude-config \
                     checkout it is the ONLY copy, so a mechanic missing here is a \
                     mechanic the fleet does not have; add it in \
                     qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
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
                     bullet to this path in qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
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
                     qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
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
    ///
    /// ## This guard is already wrap-proof — audited 2026-09-20, do not "fix" it
    ///
    /// It normalizes before matching (below), so its `WINDOW` and those
    /// measured margins are both in NORMALIZED space and a re-wrap moves
    /// neither. It is the prior art [`collapse_ws`] generalized from, not a
    /// residue awaiting the same treatment. The audit that established this
    /// swept every negative `contains` in this file: this one already
    /// normalizes; `staged_fleet_commands_have_no_plan_path_hardcodes` and
    /// `staged_fleet_commands_have_no_operator_local_paths` match filesystem
    /// paths, which contain no whitespace and have no window; and the CRLF
    /// fence check asserts line endings deliberately. So **no negative prose
    /// guard in this file is wrap-sensitive**, and a wrapped violation cannot
    /// escape one. Normalizing here on top of its own stripping would change
    /// nothing except to make the margins above harder to re-derive.
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
                 {NOT_USABLE_TEST}. Fix qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
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
                         the canonical body qontinui-claude-config/.claude/commands/{name}.md and \
                         re-vendor it here - editing the bundled copy alone is what reddens \
                         check #15c on qontinui-claude-config `main`. And if the demotion is \
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
    /// load-bearing once these files became the user-facing embedded
    /// defaults: whatever is vendored here ships to every fleet device.
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
                     device; fix it in qontinui-claude-config .claude/commands/{name}.md and re-vendor"
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
                        "  qontinui-claude-config/.claude/commands/{name}.md (bundled here as \
                         src-tauri/src/fleet_commands/{name}.md):{}: `{}` — `{verb}` has a \
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
        const FORBIDDEN: &[&str] = &[
            "D:/qontinui-root",
            "D:\\qontinui-root",
            "C:/Users/",
            "/home/",
        ];
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
                     qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
                );
            }
        }
        assert!(
            checked > 0,
            "every bundled command was excluded from this guard — either the bundle is empty \
             or the exclusion list swallowed it; check FLEET_COMMANDS and the exclusion above"
        );
    }

    /// Collapse every run of ASCII whitespace to a single space.
    ///
    /// The bundled bodies are hard-wrapped markdown at ~80 columns, while the
    /// prose these guards match is multi-word. A pure re-wrap — an edit that
    /// changes **no words at all** — therefore splits a phrase across a line
    /// break and a raw `contains` stops seeing it. That is not hypothetical:
    /// `qontinui-claude-config#1027` re-wrapped a paragraph of `vet-imp.md` and
    /// split three distinct phrases (the `IN PROGRESS` disposition anchor twice,
    /// the arm-table order `4, 3, 2, 1, 5, then 6`, and `route to closeout`), so
    /// a BYTE-FAITHFUL re-vendor of the canonical body fails guards here with
    /// messages describing removals that never happened.
    ///
    /// Applied to BOTH haystack and needle, this makes the positive prose
    /// guards match the rule rather than one particular line-wrapping of it.
    /// [`no_bundled_command_revives_the_retired_warnings_emptiness_test`] has
    /// done exactly this since it was written; these guards are catching up.
    ///
    /// **Whitespace only — markup is deliberately NOT stripped.** The wrapped
    /// citations carry their backticks on both sides of the break, so collapsing
    /// whitespace is sufficient. Stripping `` ` `` or `*` as well would widen
    /// matching beyond the invariant, admitting a body that says the words
    /// without marking them up as the anchor.
    fn collapse_ws(s: &str) -> String {
        s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The gate on [`collapse_ws`] — and its CONTROL.
    ///
    /// Clause (a) is the point of the change: a body whose phrase is split
    /// across a newline satisfies the guard. Clause (b) is what makes (a)
    /// meaningful — a body genuinely missing the phrase must still FAIL.
    /// **Without (b) this normalisation is indistinguishable from deleting the
    /// guards**, which is the one way a fix for a false failure becomes a
    /// silent false pass.
    ///
    /// Two bounds on what this test covers, stated so it is not over-read:
    ///
    /// - It exercises [`collapse_ws`] **in isolation**, never a guard site, so
    ///   it would not catch a site that collapsed only one side. No site is at
    ///   risk today — every needle is a source literal with single spaces and
    ///   no newlines, so collapsing the needle is a no-op there — but a needle
    ///   that ever gains a newline makes one-sided collapse a real bug this
    ///   test is blind to.
    /// - `split_ascii_whitespace` is ASCII-only: it does not collapse a
    ///   non-breaking space (`U+00A0`), a thin space, or `U+2028`. None of the
    ///   bundled bodies contains whitespace outside `[ \t\n\r]` today, so this
    ///   is latent — but an editor that inserts an NBSP re-arms the original
    ///   defect with the same misleading message.
    #[test]
    fn collapsing_whitespace_survives_a_rewrap_and_still_fails_a_real_removal() {
        // The real anchor, and the real wrap that hid it: `qontinui-claude-config`'s
        // `vet-imp.md` carries this citation broken after "CONDITIONALLY".
        let needle = collapse_ws("`IN PROGRESS` is CONDITIONALLY overwritable");

        let one_line = "see `/vet-plan` §5 (\"`IN PROGRESS` is CONDITIONALLY overwritable\") for\nthe disposition.\n";
        let rewrapped = "see `/vet-plan` §5 (\"`IN PROGRESS` is CONDITIONALLY\noverwritable\") for the disposition.\n";
        // Indentation after the break, i.e. a wrap inside a list item or a
        // blockquote — the run of whitespace is more than one character.
        let rewrapped_indented =
            "- see `/vet-plan` §5 (\"`IN PROGRESS` is CONDITIONALLY\n      overwritable\") for it.\n";

        for (label, body) in [
            ("unwrapped", one_line),
            ("rewrapped", rewrapped),
            ("rewrapped+indented", rewrapped_indented),
        ] {
            assert!(
                collapse_ws(body).contains(&needle),
                "(a) a {label} body carrying every word of the anchor must SATISFY the \
                 guard — a pure re-wrap changes no words and must not read as a removal"
            );
        }

        // (b) THE CONTROL. Each of these is genuinely missing something; none
        // may pass, or the guards above assert nothing at all.
        let genuinely_absent = [
            // the disposition removed outright
            (
                "phrase deleted",
                "see `/vet-plan` §5 for the disposition.\n",
            ),
            // a word dropped from the middle
            (
                "word dropped",
                "see (\"`IN PROGRESS` is overwritable\") for the disposition.\n",
            ),
            // the load-bearing qualifier inverted
            (
                "qualifier changed",
                "see (\"`IN PROGRESS` is FREELY overwritable\") for the disposition.\n",
            ),
            // right words, wrong order
            (
                "reordered",
                "see (\"overwritable CONDITIONALLY is `IN PROGRESS`\") here.\n",
            ),
            // A word split in half. Genuinely absent under a correct
            // implementation — and THE case that discriminates against the one
            // over-collapse mutation the rest of this block is blind to: a
            // `collapse_ws` that joined with "" instead of " " would fabricate
            // the word boundary and match here. Without this case every
            // assertion above passes under that mutation, and only the
            // `assert_eq!` below catches it — which reads like a redundant unit
            // check a later author may delete as noise.
            (
                "word split in half",
                "see (\"`IN PROGRESS` is CONDITIONALLY over writable\") here.\n",
            ),
        ];
        for (label, body) in genuinely_absent {
            assert!(
                !collapse_ws(body).contains(&needle),
                "(b) CONTROL FAILED for {label:?}: a body genuinely missing the anchor \
                 still matched, so whitespace collapsing has widened these guards into \
                 asserting nothing. Narrow it back — the control is what separates this \
                 change from deleting the guard"
            );
        }

        // The deliberate bound: whitespace only. Markup is NOT stripped, so a
        // body that says the words without marking them up as the anchor is
        // still out of scope — matching them would widen the guards past the
        // invariant they exist to assert.
        assert!(
            !collapse_ws("see IN PROGRESS is CONDITIONALLY overwritable here.\n").contains(&needle),
            "collapse_ws must not strip markup: the backticked anchor and the bare \
             words are different claims, and conflating them widens every guard that \
             matches an anchor"
        );
        assert_eq!(
            collapse_ws("a\t \n b  c\r\n"),
            "a b c",
            "every run of ASCII whitespace — tabs, CR, LF, multiple spaces — collapses \
             to exactly one space, and the result is trimmed at both ends"
        );
    }

    /// Content probe selecting the bundled commands that RESTATE `/vet-plan`'s
    /// delivery **arm table**, and so must carry its clauses.
    ///
    /// Scoped by CONTENT rather than by filename, for the same reason
    /// [`bundled_gate_registration_commands_teach_the_mechanics`] is — the
    /// module doc's "nothing may assume the bundle is two commands" applies to
    /// its tests too. Today this selects exactly `/vet-plan`, `/implement-plan`
    /// and `/vet-imp`; a fourth command that grows the guard is covered the day
    /// it does.
    const ARM_TABLE_READ: &str = "coord_work_unit_list_citations";

    /// Every door a bundled body may read delivery state through.
    ///
    /// **This is the half [`ARM_TABLE_READ`] used to do as well, and could
    /// not.** One constant was answering two different questions — *"does this
    /// body teach the delivery guard?"* and *"does this body restate the arm
    /// table?"* — which is fine only while every body that reads delivery reads
    /// it through the same tool. `verify-plan-status` does not: it reads
    /// delivery through `coord_query_delivery` / `GET
    /// /coord/twin/delivery/verdict`, and carries its own four-branch
    /// disposition table whose divergence from `/vet-plan`'s is deliberate and
    /// documented in its own body (*"Deliberate divergence, do not 'restore
    /// consistency'"* — `/vet-plan` may write only `VETTED`, while `SHIPPED` is
    /// that command's own state to write).
    ///
    /// So the clause loops keep asking the narrow question against
    /// [`ARM_TABLE_READ`], while the cross-check below admits `ARM_TABLE_READ`
    /// unconditionally and [`DIVERGENT_TABLE_DOOR`] only alongside a
    /// [`DECLARED_DIVERGENCE`]. This set is what the failure message
    /// ENUMERATES; it is not itself the predicate. Collapsing the two
    /// questions back together would force a choice between two wrong answers:
    /// writing a tool name into a body that does not use it purely to satisfy
    /// a token probe, or deleting a true and useful cross-reference.
    ///
    /// Both entries are const references rather than repeated literals, so the
    /// message cannot advertise a route the predicate does not honour — the
    /// hand-copied-rule defect `NOT_USABLE_TEST`'s doc comment records this
    /// file already paying for once.
    const DELIVERY_GUARD_DOORS: &[&str] = &[ARM_TABLE_READ, DIVERGENT_TABLE_DOOR];

    /// The one door a body may read delivery through WITHOUT restating the arm
    /// table — admitted only alongside [`DECLARED_DIVERGENCE`].
    const DIVERGENT_TABLE_DOOR: &str = "coord_query_delivery";

    /// What a body must DECLARE to cite the shared disposition section without
    /// restating `/vet-plan`'s arm table.
    ///
    /// [`DELIVERY_GUARD_DOORS`] alone would be an escape hatch: swapping
    /// `coord_work_unit_list_citations` for `coord_query_delivery` in any body
    /// would satisfy the cross-check while silently dropping that body out of
    /// both clause loops. Requiring the divergence to be STATED makes the broad
    /// door cost something — a body claiming its own table has to say so where
    /// a reader will see it, which `verify-plan-status` already does.
    /// **Known bound, recorded rather than papered over:** this is a prose key,
    /// so a re-wording ("Divergence is deliberate") breaks it and the body would
    /// have to restate the arm table or re-declare. A LONGER literal would be
    /// more fragile, not less; the durable fix is a machine-stable marker in the
    /// canonical qontinui-claude-config body (an HTML comment or a frontmatter
    /// key), which is a change to that repo and out of this guard's scope.
    /// Case is normalised away, and so is wrapping, so the two ways this file
    /// has already been bitten cannot re-arm on it.
    const DECLARED_DIVERGENCE: &str = "Deliberate divergence";

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
            if !contents.contains(ARM_TABLE_READ) {
                continue;
            }
            checked += 1;
            // Whitespace-collapsed, so a hard-wrap that splits a clause across a
            // line break is not read as the clause having been removed. Two of
            // these clauses were split by a pure re-wrap in
            // `qontinui-claude-config#1027`; ALL FOUR are multi-word and so
            // wrap-sensitive. See [`collapse_ws`].
            let haystack = collapse_ws(contents).to_lowercase();
            for (token, why) in IN_PROGRESS_DELIVERY_GUARD_CLAUSES {
                assert!(
                    haystack.contains(&collapse_ws(token).to_lowercase()),
                    "bundled agent command {name} teaches the `IN PROGRESS` delivery \
                     guard (it names {ARM_TABLE_READ}) but never mentions {token:?} — \
                     {why}. Without it this command's copy of the guard fails OPEN, and \
                     the failure is silent: the prose still reads complete. Add it in \
                     qontinui-claude-config .claude/commands/{name}.md (then re-vendor). \
                     Note the match is whitespace-insensitive, so a line break inside \
                     the phrase is NOT what this is reporting — the words are genuinely \
                     absent"
                );
            }
        }
        // A command can leave this guard's scope SILENTLY: drop the delivery
        // read from `implement-plan.md` while keeping its pointer sentence and
        // `checked` falls to 1 with every assertion above still green. So close
        // the loop from the other side — anything that CITES the shared section
        // must also name a delivery read. That is exactly the
        // "keep the two in sync" instruction the files state and could not
        // enforce, and unlike a `checked >= 2` floor it assumes nothing about
        // how many commands are in the bundle.
        //
        // The anchor is multi-word prose matched against hard-wrapped markdown,
        // so BOTH sides are whitespace-collapsed: a body whose citation is
        // wrapped is in scope exactly as one whose citation fits on a line.
        // Before that, `verify-plan-status` cited this anchor across a line
        // break and was therefore invisible here — outside the guard's scope
        // while still telling readers to apply the section's disposition, which
        // is verbatim the state this assertion's own message exists to prevent.
        //
        // ⚠️ The satisfier is DELIVERY_GUARD_DOORS **conditionally**, not
        // unconditionally, and the condition is the whole point. Closing this
        // loop worked originally because the satisfier and the clause-loop
        // SELECTOR were one constant: anything citing the anchor had to name
        // `coord_work_unit_list_citations`, which is exactly what puts it back
        // in the clause loop above. Splitting the constant catches the DROP
        // case but would let the SWAP case through — replace that one token
        // with `coord_query_delivery` and a body leaves both clause loops
        // silently, `checked` falls, and the `checked > 0` floor still passes.
        //
        // So a body qualifies on the broad door only while it PROVES it owns a
        // divergent disposition table of its own, by declaring the divergence
        // in as many words. That is what `verify-plan-status` genuinely does;
        // a body that merely swaps the token does not, and still fails here.
        for (anchor, _) in CROSS_COMMAND_SECTION_ANCHORS {
            let needle = collapse_ws(anchor);
            // Lowercased for the same reason the clause loop above is, and
            // stated there: capitalisation at a sentence start is not part of
            // the rule, and a guard that fired on it would be asserting prose
            // style. The declaration sits at the start of a bolded run in a
            // table cell today, so moving it mid-sentence must not fire this.
            let divergence = collapse_ws(DECLARED_DIVERGENCE).to_lowercase();
            for (name, contents) in FLEET_COMMANDS {
                let normalised = collapse_ws(contents);
                let restates_the_table = contents.contains(ARM_TABLE_READ);
                let owns_a_divergent_table = contents.contains(DIVERGENT_TABLE_DOOR)
                    && normalised.to_lowercase().contains(&divergence);
                assert!(
                    !normalised.contains(&needle) || restates_the_table || owns_a_divergent_table,
                    "bundled agent command {name} points readers at the {anchor:?} \
                     section but neither names {ARM_TABLE_READ} nor declares a \
                     divergent disposition table of its own ({DELIVERY_GUARD_DOORS:?} \
                     plus {DECLARED_DIVERGENCE:?}), so it has dropped out \
                     of this guard's scope while still telling a reader to apply that \
                     section's disposition — the clauses above stop being checked for \
                     it and nothing else notices. Restore the delivery read in \
                     the canonical body qontinui-claude-config/.claude/commands/{name}.md \
                     and re-vendor it here (editing the bundled copy alone is what \
                     reddens check #15c on qontinui-claude-config `main`), or remove the \
                     pointer"
                );
            }
        }
        assert!(
            checked > 0,
            "no bundled command mentions {ARM_TABLE_READ:?} — either the bundle lost its \
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
            let needle = collapse_ws(anchor);
            let mut defined = false;
            let mut citers: Vec<&&str> = Vec::new();
            for (name, contents) in FLEET_COMMANDS {
                // Whitespace-collapsed on both sides: a citation the author
                // hard-wrapped is still a citation, and reading it as absent is
                // what let this edge go unwatched. See [`collapse_ws`].
                let normalised = collapse_ws(contents);
                if !normalised.contains(&needle) {
                    continue;
                }
                if name == target {
                    // `# ` + the anchor matches the heading at ANY level and
                    // does NOT match a prose mention, so a heading renamed while
                    // the old wording survives elsewhere in the file still fails
                    // — the case that would otherwise dangle the pointer silently.
                    //
                    // ⚠️ RAW on purpose, unlike the citation scan above. A
                    // markdown heading cannot wrap, so collapsing buys nothing
                    // here and COSTS the strictness the comment claims: against
                    // a collapsed haystack a bare `#` ending any line (a closed
                    // ATX heading, a fenced block, a table cell) followed by a
                    // prose mention would satisfy it with the real heading gone.
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
                 restore the heading in the canonical body \
                 qontinui-claude-config/.claude/commands/{target}.md and re-vendor it \
                 here (editing the bundled copy alone is what reddens check #15c on \
                 qontinui-claude-config `main`) or \
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
    /// Same scope probe ([`ARM_TABLE_READ`]) and the same reasoning, so the two
    /// compose: that guard keeps the arms a reader would miss, this one keeps
    /// the arms a reader would not.
    #[test]
    fn bundled_delivery_guard_commands_name_the_clean_looking_unknown_arms() {
        let mut checked = 0usize;
        for (name, contents) in FLEET_COMMANDS {
            if !contents.contains(ARM_TABLE_READ) {
                continue;
            }
            checked += 1;
            // Collapsed for uniformity with its sibling guard. Every token here
            // is currently a single word, so this is a no-op today -- it is the
            // shape that matters: a multi-word arm added later is wrap-proof by
            // construction rather than by the next author remembering.
            let normalised = collapse_ws(contents);
            for (token, why) in CLEAN_LOOKING_UNKNOWN_ARMS {
                assert!(
                    normalised.contains(&collapse_ws(token)),
                    "bundled agent command {name} teaches the `IN PROGRESS` delivery \
                     guard (it names {ARM_TABLE_READ}) but never mentions {token:?} — \
                     {why}. A copy missing it fails OPEN on the one response shape that \
                     reads as a clean observation, and on a device with no \
                     qontinui-claude-config checkout this file is the ONLY copy; add it \
                     in qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
                );
            }
        }
        assert!(
            checked > 0,
            "no bundled command mentions {ARM_TABLE_READ:?} — either the bundle lost its \
             `IN PROGRESS` delivery guard entirely or this guard's content probe went \
             stale. Both need a human look; neither is a passing test"
        );
    }

    /// A command may not teach the do-not-overwrite lifecycle tokens and then
    /// OMIT `IN PROGRESS`.
    ///
    /// This is the original defect, encoded. The two guards above are scoped by
    /// [`ARM_TABLE_READ`], so they say nothing about a command that teaches the
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
            // Whitespace-collapsed: this phrase is multi-word, and a pure
            // re-wrap of the canonical body in qontinui-claude-config splits it
            // without changing a single word. See [`collapse_ws`].
            assert!(
                collapse_ws(contents).contains(&collapse_ws("is CONDITIONALLY overwritable")),
                "bundled agent command {name} teaches the do-not-overwrite lifecycle \
                 tokens but never says what to do with an `IN PROGRESS` stamp. That \
                 exact omission is what let a vet pass overwrite a plan whose work had \
                 already landed and then re-implement it; restore the \"`IN PROGRESS` is \
                 CONDITIONALLY overwritable\" disposition in \
                 qontinui-claude-config .claude/commands/{name}.md (then re-vendor)"
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
