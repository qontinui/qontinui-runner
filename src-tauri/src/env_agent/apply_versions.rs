//! The `versions` half of the local apply (plan
//! `2026-07-02-devenv-copy-canonical-config-phase2-agent-apply`, P2b slice 3).
//!
//! This is the slice the operator's OQ#4 **YES** authorizes: a developer's
//! runner may install toolchain versions **on that developer's own box**, given
//! local confirm, dry-run default, and a hard no-op when no version manager is
//! detected. Apply authority never leaves the machine that owns the machine.
//!
//! ## The surface is three keys
//!
//! After web #808 / runner #818 derived-key filtering, `versions` has exactly
//! three actionable keys — **`node`, `python`, `rustc`** — the only ones that
//! describe the BOX rather than the source tree the capturing binary was built
//! from. `runner_crate_version`, `node_package_version`, `node_package_name`,
//! `python_constraint`, `tauri` and the whole `node_dep_*` prefix are not:
//! they describe the source tree, and no apply can move them. The server marks
//! them derived and [`super::pull::SectionPlan::actionable`] already drops
//! them; [`APPLIABLE_TOOLS`] is the second, independent gate.
//!
//! Every one of those non-appliable keys changed **provenance** in slice 5
//! Phase 8 of plan `2026-08-04-remove-hardcoded-machine-paths-from-product-code`,
//! because each was previously reached from `env!("CARGO_MANIFEST_DIR")` — a
//! compile-time constant naming a path that only existed on the machine the
//! binary was compiled on. **No classification changed**: they still describe
//! the build and the source trees, not the box, and no apply can move them.
//! Exactly one key changed VALUE:
//!
//! | key | new provenance | value |
//! |-----|----------------|-------|
//! | `runner_crate_version` | `env!("CARGO_PKG_VERSION")` | unchanged |
//! | `tauri` | the `tauri::VERSION` constant | **CHANGED** |
//! | `node_package_name`, `node_package_version`, `node_dep_*` | `<workspace-root>/qontinui-runner/package.json` | unchanged |
//! | `python_constraint` | `<workspace-root>/qontinui-web/backend/pyproject.toml` | unchanged |
//!
//! `tauri` is the one value change: it now reports the concrete version of the
//! crate this binary is LINKED against instead of the declared range (`"2.5"`)
//! parsed out of the build host's manifest — strictly better, and a one-time
//! move on that key as runners rebuild.
//!
//! The `node_*` keys are called out because they nearly did change value. The
//! old walk's doc claimed it preferred the `qontinui-web` frontend manifest, but
//! that branch was unreachable — the walk probed `dir/package.json` first and hit
//! the runner's own on its second step. Anchoring at the frontend would have
//! silently retargeted five keys at a sibling repo, leaving `node_package_name`
//! describing a different package from the `runner_crate_version` beside it.
//! `super::collectors::workspace_package_json` therefore anchors at
//! `<workspace-root>/qontinui-runner/package.json`.
//!
//! This is not a general installer. It is a three-key problem.
//!
//! ## Detect-then-drive, fail-safe
//!
//! Chosen over the two alternatives by `[policy: design-tradeoff-ranking]`:
//!
//! - **Pinfile-only** (`.nvmrc`, `rust-toolchain.toml`, `.python-version`) fails
//!   at rank 1, capability: writing `.nvmrc` does not change what `node
//!   --version` answers — nvm needs an explicit `nvm use` — so it cannot deliver
//!   "drift clears", which is this plan's whole deliverable.
//! - **Package-manager shell-out** fails at rank 3, robustness: elevation-
//!   requiring on Windows and not idempotent.
//!
//! So: detect the box's own manager, drive it, and — **hard requirement** — when
//! no supported manager is detected, report only and change nothing. Not a
//! best-effort fallback, not a package-manager attempt. That is the same
//! "unrecognized input falls to the inert branch" idiom already proven twice in
//! this feature (unknown policy string → `ReportOnly`; unknown key → not
//! derived) and once more in slice 2 (a key outside the fixed set → skipped).
//!
//! ## Verify by re-probing, never by self-report
//!
//! After driving a manager this module re-runs the SAME probe the capture runs
//! ([`super::collectors::probe_tool_version`]) in the SAME declared scope root
//! ([`super::collectors::probe_scope`], slice 1) and reports whether the
//! observed version actually moved. That matters because several real
//! configurations swallow a successful-looking drive — a `rust-toolchain.toml`
//! in the scope root outranks `rustup default`, and an fnm/nvm default only
//! reaches a process with shell integration. Those cases must read as "drove
//! the manager, the observed version did not move", not as success.
//!
//! The re-probe is a LOCAL check, not the plan's oracle. The oracle is still the
//! twin's drift clearing after `env capture` re-reports — this only stops the
//! apply from claiming a convergence it cannot see.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::apply::{AppliedChange, SectionApply, SectionStatus, SkipRecord};
use super::collectors::{self, ProbeScope};
use super::pull::{Change, SectionPlan};

/// The envelope section this module applies.
pub const VERSIONS_SECTION: &str = "versions";

/// How long one manager step may take. Toolchain installs are genuinely slow
/// (a rustup toolchain is hundreds of MB), and this runs only under an explicit
/// `--confirm`, so the budget is generous — but bounded, so a manager waiting on
/// a prompt cannot wedge the CLI forever.
const DRIVE_BUDGET: Duration = Duration::from_secs(20 * 60);

/// Budget for the post-drive re-probe. Far shorter: by then the toolchain is
/// installed, and a shim that takes 30s to answer `--version` is itself broken.
const VERIFY_BUDGET: Duration = Duration::from_secs(30);

/// A toolchain whose OBSERVED version this runner can move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Node,
    Python,
    Rustc,
}

/// The three actionable `versions` keys — the complete surface of this slice.
pub const APPLIABLE_TOOLS: &[Tool] = &[Tool::Node, Tool::Python, Tool::Rustc];

impl Tool {
    /// The `versions` section key.
    pub fn key(self) -> &'static str {
        match self {
            Tool::Node => "node",
            Tool::Python => "python",
            Tool::Rustc => "rustc",
        }
    }

    /// The command the capture probes with — and therefore the command the
    /// verification must re-probe with.
    pub fn command(self) -> &'static str {
        match self {
            Tool::Node => "node",
            Tool::Python => "python",
            Tool::Rustc => "rustc",
        }
    }

    /// Resolve a section key to a tool. Anything else is not appliable.
    pub fn from_key(key: &str) -> Option<Self> {
        APPLIABLE_TOOLS.iter().copied().find(|t| t.key() == key)
    }

    /// Extract the bare version out of a `<cmd> --version` line.
    ///
    /// The captured values are the raw first line of the probe, which differs
    /// per tool: `v24.14.0` (node), `Python 3.13.13`, `rustc 1.95.0 (hash
    /// date)`. A manager needs the bare version, and the verification compares
    /// PARSED versions so an incidental difference in the trailing build
    /// metadata is not read as a failure to converge.
    pub fn parse_version(self, reported: &str) -> Option<String> {
        let reported = reported.trim();
        let raw = match self {
            Tool::Node => reported.trim_start_matches(['v', 'V']),
            // `Python 3.13.13`, and (stderr on very old builds) the same shape.
            Tool::Python => reported.split_whitespace().nth(1)?,
            // `rustc 1.95.0 (hash 2026-01-01)`
            Tool::Rustc => reported.split_whitespace().nth(1)?,
        };
        let raw = raw.trim();
        // Guard against a probe line that parsed structurally but carries no
        // version — driving a manager to install `""` would be worse than not
        // acting.
        if raw.is_empty() || !raw.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        Some(raw.to_string())
    }
}

/// The capture-provenance key `collect_versions` emits: which KIND of scope the
/// toolchain probes ran in (`default` / `declared` / `inherited`).
pub const PROBE_SCOPE_KEY: &str = "probe_scope_kind";

/// Why canonical's toolchain numbers cannot be compared with this box's.
///
/// This closes the residual slice 1a left open and handed to slice 3: after
/// #936 each box is self-consistent across runs, but two boxes that measured
/// DIFFERENT scopes still had their `node`/`python`/`rustc` compared as
/// like-for-like. Since "drift clears" is this slice's success oracle, acting
/// on that comparison would mean installing a version that was observed
/// somewhere else — converging a number while diverging the thing it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeMismatch {
    /// Both sides reported a scope kind and they differ.
    Differs { local: String, canonical: String },
    /// Canonical reports a scope kind and this box does not.
    LocalMissing { canonical: String },
    /// This box reports one and canonical does not — canonical was captured by
    /// a runner predating scope provenance.
    CanonicalMissing,
}

impl ScopeMismatch {
    /// Operator-facing reason, used verbatim as the skip reason.
    pub fn reason(&self) -> String {
        match self {
            Self::Differs { local, canonical } => format!(
                "canonical measured its toolchain in a `{canonical}` scope and this box measures \
                 a `{local}` one — the two numbers do not describe the same thing, so installing \
                 canonical's would converge the reading while diverging the box. Align the scopes \
                 (`env scope-root`) first."
            ),
            Self::LocalMissing { canonical } => format!(
                "canonical reports a `{canonical}` capture scope but this box reports none, so \
                 the two toolchain readings cannot be shown comparable."
            ),
            Self::CanonicalMissing => "canonical was captured by a runner that predates capture \
                 scope provenance, so its toolchain readings cannot be shown comparable with \
                 this box's — re-run `env capture` on the canonical machine."
                .to_string(),
        }
    }
}

/// Decide whether canonical's `versions` numbers are comparable with this
/// box's, from the section DIFF. **Pure — unit-tested.**
///
/// Reads `section.changes` rather than [`SectionPlan::actionable`] on purpose:
/// the provenance key is server-classified as derived (it is reported, never an
/// apply action), so `actionable()` correctly filters it out — but this check
/// needs to SEE it. The absence of a change for the key means both sides
/// reported the same value, which is the comparable case.
pub fn scope_mismatch(section: &SectionPlan) -> Option<ScopeMismatch> {
    section.changes.iter().find_map(|c| match c {
        Change::Differs {
            key,
            local,
            canonical,
        } if key == PROBE_SCOPE_KEY => Some(ScopeMismatch::Differs {
            local: local.clone(),
            canonical: canonical.clone(),
        }),
        Change::Missing { key, canonical } if key == PROBE_SCOPE_KEY => {
            Some(ScopeMismatch::LocalMissing {
                canonical: canonical.clone(),
            })
        }
        Change::Extra { key, .. } if key == PROBE_SCOPE_KEY => {
            Some(ScopeMismatch::CanonicalMissing)
        }
        _ => None,
    })
}

/// A version manager this runner knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// Shim-based and near-universal — the easy one.
    Rustup,
    /// Shim-based; `volta install node@x` sets the user default immediately.
    Volta,
    Fnm,
    /// POSIX `nvm` is a shell FUNCTION, not an executable — it can only be
    /// driven through a login-ish `bash -c '. nvm.sh && …'`.
    NvmPosix,
    /// nvm-windows ships a real `nvm.exe`.
    NvmWindows,
    /// Covers pyenv and pyenv-win, which share this command surface.
    Pyenv,
}

impl Manager {
    pub fn label(self) -> &'static str {
        match self {
            Manager::Rustup => "rustup",
            Manager::Volta => "volta",
            Manager::Fnm => "fnm",
            Manager::NvmPosix => "nvm",
            Manager::NvmWindows => "nvm-windows",
            Manager::Pyenv => "pyenv",
        }
    }

    /// The executable whose presence means "detected". `None` for
    /// [`Manager::NvmPosix`], which is a shell function and is detected by
    /// `$NVM_DIR/nvm.sh` instead.
    fn detect_command(self) -> Option<&'static str> {
        match self {
            Manager::Rustup => Some("rustup"),
            Manager::Volta => Some("volta"),
            Manager::Fnm => Some("fnm"),
            Manager::NvmPosix => None,
            Manager::NvmWindows => Some("nvm"),
            Manager::Pyenv => Some("pyenv"),
        }
    }
}

/// Managers to try for a tool, in priority order. Shim-based managers come
/// first: they change what a freshly-spawned `<cmd> --version` answers without
/// needing shell integration, which is exactly what the drift oracle measures.
pub fn candidates(tool: Tool) -> Vec<Manager> {
    match tool {
        Tool::Rustc => vec![Manager::Rustup],
        Tool::Node => {
            if cfg!(windows) {
                // `nvm` on Windows is nvm-windows (a real .exe); the POSIX shell
                // function does not exist there.
                vec![Manager::Volta, Manager::Fnm, Manager::NvmWindows]
            } else {
                vec![Manager::Volta, Manager::Fnm, Manager::NvmPosix]
            }
        }
        Tool::Python => vec![Manager::Pyenv],
    }
}

/// One command the drive will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: String,
    pub args: Vec<String>,
}

impl Step {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        }
    }

    /// Shell-ish rendering for the preview. Not executed — the steps are spawned
    /// argv-style, never through a shell.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// The command sequence that drives `manager` to `version`. **Pure —
/// unit-tested.**
///
/// `scope_is_default` says whether the declared capture scope is the box's
/// default (the home directory) rather than a specific project tree. It only
/// affects rustup, which can express both: changing the machine's DEFAULT
/// toolchain is right when the probe measures the default, and a directory
/// OVERRIDE is the smaller change when the probe measures one tree. Choosing the
/// narrower lever where one exists keeps the blast radius matched to what the
/// operator actually declared.
pub fn drive_steps(manager: Manager, version: &str, scope_is_default: bool) -> Vec<Step> {
    match manager {
        Manager::Rustup => {
            let mut steps = vec![Step::new(
                "rustup",
                &["toolchain", "install", version, "--profile", "minimal"],
            )];
            if scope_is_default {
                steps.push(Step::new("rustup", &["default", version]));
            } else {
                steps.push(Step::new("rustup", &["override", "set", version]));
            }
            steps
        }
        Manager::Volta => vec![Step::new("volta", &["install", &format!("node@{version}")])],
        Manager::Fnm => vec![
            Step::new("fnm", &["install", version]),
            Step::new("fnm", &["default", version]),
        ],
        Manager::NvmPosix => vec![Step::new(
            "bash",
            &[
                "-lc",
                &format!(
                    // `nvm` is a shell function: source it, then drive it. The
                    // `alias default` is what makes a NEW shell (and therefore
                    // the next capture probe) see the version.
                    ". \"${{NVM_DIR:-$HOME/.nvm}}/nvm.sh\" && nvm install {version} \
                     && nvm alias default {version}"
                ),
            ],
        )],
        Manager::NvmWindows => vec![
            Step::new("nvm", &["install", version]),
            Step::new("nvm", &["use", version]),
        ],
        Manager::Pyenv => vec![
            // `--skip-existing` makes a re-run idempotent instead of an error.
            Step::new("pyenv", &["install", "--skip-existing", version]),
            Step::new("pyenv", &["global", version]),
        ],
    }
}

/// One toolchain this apply would move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionAction {
    pub tool: Tool,
    /// The box's currently observed value, as the capture reports it.
    pub local: Option<String>,
    /// Canonical's observed value, as the capture reports it.
    pub canonical: String,
    /// The bare version parsed out of `canonical`, i.e. what the manager is
    /// driven to.
    pub target_version: String,
    pub manager: Manager,
    pub steps: Vec<Step>,
}

/// The plan for the `versions` section. **Pure to compute.**
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionsPlan {
    pub actions: Vec<VersionAction>,
    pub skipped: Vec<SkipRecord>,
    pub notes: Vec<String>,
}

/// Detect which manager (if any) is available for a tool. Injected as a closure
/// by [`plan_versions`] so the planner is hermetic in tests — real detection
/// spawns processes, which a unit test must not depend on.
pub type Detector<'a> = &'a dyn Fn(Tool) -> Option<Manager>;

/// Plan the `versions` apply. **Pure** given a detector — no host mutation, no
/// process spawned by this function itself.
///
/// Only [`SectionPlan::actionable`] changes are considered, so a non-applyable
/// policy, an `Extra` local key and every repo-derived key are excluded before
/// this sees them. [`Tool::from_key`] then narrows to the three observed keys.
///
/// Keys the capture could not MEASURE are excluded upstream too, and are
/// re-surfaced here as NOTES — see the loop below. They produce no action by
/// design; they must not produce silence.
pub fn plan_versions(
    section: &SectionPlan,
    detect: Detector<'_>,
    scope: &ProbeScope,
) -> VersionsPlan {
    let mut plan = VersionsPlan::default();
    let scope_is_default = scope_is_default(scope);

    // Comparability gate. When the two boxes measured different scopes, EVERY
    // toolchain key in this section is unsound to act on — so the whole section
    // reports and changes nothing, rather than each key failing separately for
    // a reason that is really one fact about the pair of captures.
    if let Some(mismatch) = scope_mismatch(section) {
        let reason = mismatch.reason();
        for change in section.actionable() {
            if let Some(tool) = Tool::from_key(change.key()) {
                plan.skipped.push(SkipRecord {
                    key: tool.key().to_string(),
                    reason: reason.clone(),
                });
            }
        }
        plan.notes.push(reason);
        plan.skipped.sort_by(|a, b| a.key.cmp(&b.key));
        return plan;
    }

    for change in section.actionable() {
        let (key, local, canonical) = match change {
            Change::Missing { key, canonical } => (key.as_str(), None, canonical.as_str()),
            Change::Differs {
                key,
                local,
                canonical,
            } => (key.as_str(), Some(local.clone()), canonical.as_str()),
            Change::Extra { .. } => continue,
        };

        let Some(tool) = Tool::from_key(key) else {
            plan.skipped.push(SkipRecord {
                key: key.to_string(),
                reason: "not an observed toolchain version — nothing to install".to_string(),
            });
            continue;
        };

        let Some(target_version) = tool.parse_version(canonical) else {
            plan.skipped.push(SkipRecord {
                key: key.to_string(),
                reason: format!("cannot read a version out of canonical's {canonical:?}"),
            });
            continue;
        };

        // THE fail-safe. No supported manager ⇒ report only, change nothing.
        // Not a best-effort fallback, not a package-manager attempt.
        let Some(manager) = detect(tool) else {
            let tried: Vec<&str> = candidates(tool).iter().map(|m| m.label()).collect();
            plan.skipped.push(SkipRecord {
                key: key.to_string(),
                reason: format!(
                    "no supported version manager detected (looked for {}) — reporting only, \
                     nothing changed. Install one, or move {key} to {target_version} by hand.",
                    tried.join(", ")
                ),
            });
            continue;
        };

        plan.actions.push(VersionAction {
            tool,
            local,
            canonical: canonical.to_string(),
            target_version: target_version.clone(),
            manager,
            steps: drive_steps(manager, &target_version, scope_is_default),
        });
    }

    // Keys this box never READ. `actionable()` filtered them out upstream, which
    // is correct — but "filtered upstream" is exactly how a change becomes
    // invisible, and an empty action list with nothing beside it renders as
    // `versions [nothing to do]`. That contradicts this section's own rule
    // (`SkipRecord`: "Surfaced rather than dropped: a silently-ignored change
    // reads as 'nothing to do'"). Notes cost no action, so they are the right
    // channel: the key is reported, and still never installed.
    for key in &section.unknown_keys {
        plan.notes.push(format!(
            "`{key}` could NOT be measured on this box (its `--version` probe exceeded the capture \
             budget twice), so there is nothing here to compare against canonical. It is NOT being \
             treated as missing and NOT being installed — installing over a version nobody read is \
             the failure this refusal exists to prevent. Re-run `env capture` once the box is \
             quiet, or fix the shim if it never answers."
        ));
    }

    if !plan.actions.is_empty() && !scope_is_default {
        plan.notes.push(format!(
            "probes measure the declared scope root ({}), so the apply targets that tree rather \
             than the box's default toolchain",
            scope
                .root
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "unset".to_string()),
        ));
    }

    plan.actions.sort_by_key(|a| a.tool.key());
    plan.skipped.sort_by(|a, b| a.key.cmp(&b.key));
    plan
}

/// True when the declared capture scope is the box's DEFAULT (the home
/// directory, or nothing at all) rather than a specific project tree.
fn scope_is_default(scope: &ProbeScope) -> bool {
    match (&scope.root, dirs::home_dir()) {
        (Some(root), Some(home)) => root == &home,
        // No resolved root at all ⇒ the probe inherits, which is as close to
        // "the default" as this box gets.
        (None, _) => true,
        (Some(_), None) => false,
    }
}

// ============================================================================
// Execution
// ============================================================================

/// What actually happened for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveOutcome {
    /// The manager ran and the re-probe shows the observed version moved.
    Converged { observed: String },
    /// The manager ran without error, but the re-probe still reports something
    /// else. Reported honestly rather than as success — see the module docs for
    /// the configurations that cause this.
    DidNotConverge { observed: Option<String> },
    /// A step failed. Later steps are NOT attempted.
    Failed { step: String, detail: String },
}

/// Run one action's steps in `cwd`, then verify by re-probing.
fn drive(action: &VersionAction, cwd: Option<&Path>) -> DriveOutcome {
    for step in &action.steps {
        match run_step(step, cwd) {
            Ok(()) => {}
            Err(detail) => {
                return DriveOutcome::Failed {
                    step: step.display(),
                    detail,
                }
            }
        }
    }

    let observed = collectors::probe_tool_version(action.tool.command(), cwd, VERIFY_BUDGET);
    let converged = observed
        .as_deref()
        .and_then(|o| action.tool.parse_version(o))
        .map(|v| v == action.target_version)
        .unwrap_or(false);

    match (converged, observed) {
        (true, Some(observed)) => DriveOutcome::Converged { observed },
        (_, observed) => DriveOutcome::DidNotConverge { observed },
    }
}

/// Spawn one step with a bounded wall clock, capturing output. Returns the tail
/// of stderr/stdout on failure so the operator sees WHY, not just that it broke.
fn run_step(step: &Step, cwd: Option<&Path>) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let mut command = crate::process_helpers::no_window(&step.program);
    command
        .args(&step.args)
        // No stdin: a manager that decides to prompt must fail on EOF rather
        // than block a CLI the operator is watching.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", step.program))?;

    let deadline = Instant::now() + DRIVE_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut detail = String::new();
                if let Some(mut se) = child.stderr.take() {
                    let _ = se.read_to_string(&mut detail);
                }
                if detail.trim().is_empty() {
                    if let Some(mut so) = child.stdout.take() {
                        let _ = so.read_to_string(&mut detail);
                    }
                }
                return Err(format!("exited {status}: {}", tail(&detail)));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", DRIVE_BUDGET.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("waiting on {}: {e}", step.program)),
        }
    }
}

/// Last few non-empty lines of a process's output, for an error message.
fn tail(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(3);
    lines[start..].join(" | ")
}

/// Detect a manager for real, by asking each candidate for its own version.
/// Deliberately a `--version` spawn rather than a PATH scan: a manager that
/// cannot answer `--version` cannot be driven either.
fn detect_real(tool: Tool) -> Option<Manager> {
    for manager in candidates(tool) {
        let detected = match manager.detect_command() {
            Some(cmd) => {
                collectors::probe_tool_version(cmd, None, Duration::from_secs(5)).is_some()
            }
            // POSIX nvm is a shell function — its script's existence is the
            // signal.
            None => nvm_posix_script().is_some(),
        };
        if detected {
            return Some(manager);
        }
    }
    None
}

/// Path of POSIX nvm's `nvm.sh` (`$NVM_DIR`, else `~/.nvm`), when it exists.
fn nvm_posix_script() -> Option<PathBuf> {
    let dir = std::env::var("NVM_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".nvm")))?;
    let script = dir.join("nvm.sh");
    script.is_file().then_some(script)
}

// ============================================================================
// Driver entry point
// ============================================================================

/// Plan — and, with `confirm`, perform — the `versions` apply for one section.
pub fn apply_section(section: &SectionPlan, confirm: bool) -> SectionApply {
    apply_section_with(section, &detect_real, confirm)
}

/// [`apply_section`] with an injectable detector, so the planning half is
/// testable without a version manager on the machine running the tests.
pub fn apply_section_with(
    section: &SectionPlan,
    detect: Detector<'_>,
    confirm: bool,
) -> SectionApply {
    let name = section.section.as_str();
    let scope = collectors::probe_scope();
    let plan = plan_versions(section, detect, &scope);

    let target = scope
        .root
        .as_ref()
        .map(|p| format!("scope root {}", p.display()))
        .unwrap_or_else(|| "the inherited working directory".to_string());

    let mut out = SectionApply {
        section: name.to_string(),
        status: SectionStatus::NothingToDo,
        target: Some(target),
        changes: Vec::new(),
        skipped: plan.skipped,
        notes: plan.notes,
    };

    if plan.actions.is_empty() {
        // `NothingToDo` is a POSITIVE claim about the box, and it is unsound
        // over a key nobody read. The notes above already carry the detail;
        // the STATUS has to stop saying the opposite, or the one-line section
        // summary (`versions [nothing to do]`) contradicts them.
        if !section.unknown_keys.is_empty() {
            out.status = SectionStatus::Blocked(format!(
                "{} key(s) in this section could not be measured on this box, so nothing here can \
                 be called settled — see the notes below. Nothing was changed.",
                section.unknown_keys.len(),
            ));
        }
        return out;
    }

    if !confirm {
        out.status = SectionStatus::Planned;
        out.changes = plan.actions.iter().map(planned_change).collect();
        return out;
    }

    let mut applied_any = false;
    for action in &plan.actions {
        match drive(action, scope.root.as_deref()) {
            DriveOutcome::Converged { observed } => {
                applied_any = true;
                out.changes.push(AppliedChange {
                    key: action.tool.key().to_string(),
                    from: action.local.clone(),
                    to: observed,
                    detail: Some(format!("via {}", action.manager.label())),
                });
            }
            // Neither of the remaining outcomes is a change: nothing observable
            // moved, so nothing goes in `changes` (which is what the local audit
            // log records). They become skips carrying the real reason.
            DriveOutcome::DidNotConverge { observed } => {
                out.skipped.push(SkipRecord {
                    key: action.tool.key().to_string(),
                    reason: format!(
                        "{} ran, but the probe still reports {} — a pinfile or shell-integrated \
                         default in this scope outranks it. Nothing here is being claimed as \
                         converged.",
                        action.manager.label(),
                        observed.as_deref().unwrap_or("nothing"),
                    ),
                });
            }
            DriveOutcome::Failed { step, detail } => {
                out.skipped.push(SkipRecord {
                    key: action.tool.key().to_string(),
                    reason: format!("`{step}` failed — {detail}"),
                });
            }
        }
    }

    out.status = if applied_any {
        SectionStatus::Applied
    } else {
        // Every action was attempted and none moved the observed version. That
        // is not "nothing to do" — say so.
        SectionStatus::Blocked(
            "every detected manager ran without moving the observed version — see the reasons \
             above"
                .to_string(),
        )
    };
    out.skipped.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// The dry-run rendering of an action. Version strings are not secrets, so
/// nothing is redacted — but the shape matches [`AppliedChange`] exactly, so the
/// same renderers serve both halves.
fn planned_change(action: &VersionAction) -> AppliedChange {
    let steps: Vec<String> = action.steps.iter().map(Step::display).collect();
    AppliedChange {
        key: action.tool.key().to_string(),
        from: action.local.clone(),
        to: action.canonical.clone(),
        detail: Some(format!(
            "via {}: {}",
            action.manager.label(),
            steps.join(" && ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_agent::pull::{compute_plan, CanonicalConfig};
    use serde_json::{json, Map, Value};

    fn versions_section(canonical: Value, local: Value, derived: Value) -> SectionPlan {
        versions_section_with_unknown(canonical, local, derived, json!([]))
    }

    /// [`versions_section`] plus the LOCAL capture's unmeasured-key list — the
    /// keys whose `--version` probe blew the capture budget, which are reported
    /// but must never reach a `VersionAction`.
    fn versions_section_with_unknown(
        canonical: Value,
        local: Value,
        derived: Value,
        unknown: Value,
    ) -> SectionPlan {
        let cfg = CanonicalConfig {
            canonical_machine_id: Some("canon".to_string()),
            canonical_machine_name: Some("spaceship".to_string()),
            schema_version: None,
            captured_at: None,
            sections: json!({ "versions": canonical })
                .as_object()
                .unwrap()
                .clone(),
            section_policy: json!({ "versions": "applyable" })
                .as_object()
                .unwrap()
                .clone(),
            derived_keys: json!({ "versions": derived }).as_object().unwrap().clone(),
        };
        let local_sections = json!({ "versions": local });
        let local_unknown = json!({ "versions": unknown });
        compute_plan(
            &cfg,
            local_sections.as_object().unwrap(),
            local_unknown.as_object().unwrap(),
            "env-1",
            "this-box",
        )
        .sections
        .into_iter()
        .find(|s| s.section == VERSIONS_SECTION)
        .expect("versions section planned")
    }

    /// A scope that is NOT the home directory, so `scope_is_default` is false.
    fn project_scope() -> ProbeScope {
        ProbeScope {
            root: Some(PathBuf::from(if cfg!(windows) {
                "C:\\some\\project"
            } else {
                "/some/project"
            })),
            rejected: None,
            kind: collectors::ProbeScopeKind::Declared,
        }
    }

    fn home_scope() -> ProbeScope {
        ProbeScope {
            root: dirs::home_dir(),
            rejected: None,
            kind: collectors::ProbeScopeKind::Default,
        }
    }

    fn no_manager(_: Tool) -> Option<Manager> {
        None
    }

    // ------------------------------------------------------------------
    // Version parsing — each tool reports a different shape
    // ------------------------------------------------------------------

    #[test]
    fn parses_each_tools_reported_shape() {
        assert_eq!(
            Tool::Node.parse_version("v24.14.0").as_deref(),
            Some("24.14.0")
        );
        assert_eq!(
            Tool::Python.parse_version("Python 3.13.13").as_deref(),
            Some("3.13.13")
        );
        assert_eq!(
            Tool::Rustc
                .parse_version("rustc 1.95.0 (abcdef012 2026-01-05)")
                .as_deref(),
            Some("1.95.0")
        );
    }

    /// A line that parses structurally but carries no version must yield None —
    /// driving a manager to install `""` would be worse than not acting.
    #[test]
    fn refuses_a_probe_line_with_no_version() {
        assert_eq!(Tool::Rustc.parse_version("rustc"), None);
        assert_eq!(Tool::Python.parse_version("Python"), None);
        assert_eq!(Tool::Node.parse_version("v"), None);
        assert_eq!(Tool::Node.parse_version(""), None);
        assert_eq!(Tool::Rustc.parse_version("error: no toolchain"), None);
    }

    /// Verification compares PARSED versions, so differing build metadata on
    /// the same release is not read as a failure to converge.
    #[test]
    fn parsed_versions_ignore_trailing_build_metadata() {
        let a = Tool::Rustc.parse_version("rustc 1.95.0 (aaaa 2026-01-05)");
        let b = Tool::Rustc.parse_version("rustc 1.95.0 (bbbb 2026-01-06)");
        assert_eq!(a, b);
    }

    // ------------------------------------------------------------------
    // Scope comparability — the residual slice 1a handed to this slice
    // ------------------------------------------------------------------

    /// Two boxes that measured DIFFERENT scopes are not measuring the same
    /// quantity, so no toolchain key in the section may be acted on. Installing
    /// canonical's number would converge the READING while diverging the box.
    #[test]
    fn a_scope_mismatch_refuses_the_whole_section() {
        let section = versions_section(
            json!({"node": "v24.14.0", "rustc": "rustc 1.95.0 (a b)",
                   "probe_scope_kind": "declared"}),
            json!({"node": "v20.9.0", "rustc": "rustc 1.80.0 (c d)",
                   "probe_scope_kind": "default"}),
            json!(["probe_scope_kind"]),
        );
        assert_eq!(
            scope_mismatch(&section),
            Some(ScopeMismatch::Differs {
                local: "default".to_string(),
                canonical: "declared".to_string(),
            })
        );
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        assert!(plan.actions.is_empty(), "got {:?}", plan.actions);
        // Both toolchain keys are reported with the reason — not silently gone.
        let keys: Vec<&str> = plan.skipped.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["node", "rustc"]);
        assert!(plan.skipped[0]
            .reason
            .contains("do not describe the same thing"));
        assert_eq!(plan.notes.len(), 1);
    }

    /// The gate reads the raw diff, NOT `actionable()`. The provenance key is
    /// server-classified as derived — so `actionable()` correctly filters it
    /// out — and a gate built on `actionable()` would therefore never fire.
    /// This is the regression that would silently reopen the residual.
    #[test]
    fn the_gate_sees_a_key_that_actionable_filters_out() {
        let section = versions_section(
            json!({"node": "v24.14.0", "probe_scope_kind": "declared"}),
            json!({"node": "v20.9.0", "probe_scope_kind": "default"}),
            json!(["probe_scope_kind"]),
        );
        // Derived ⇒ absent from actionable()…
        let actionable: Vec<&str> = section.actionable().iter().map(|c| c.key()).collect();
        assert_eq!(actionable, vec!["node"]);
        // …but present in the raw diff, which is what the gate reads.
        assert!(scope_mismatch(&section).is_some());
    }

    /// Matching scopes are the comparable case and must NOT be gated — this is
    /// the ordinary fleet configuration (every box on its default toolchain).
    #[test]
    fn matching_scopes_do_not_gate_the_apply() {
        let section = versions_section(
            json!({"node": "v24.14.0", "probe_scope_kind": "default"}),
            json!({"node": "v20.9.0", "probe_scope_kind": "default"}),
            json!(["probe_scope_kind"]),
        );
        // Identical values ⇒ no Change at all for the key ⇒ comparable.
        assert_eq!(scope_mismatch(&section), None);
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].tool, Tool::Node);
    }

    /// A canonical captured by a runner predating scope provenance cannot be
    /// shown comparable, so it is refused with an actionable instruction rather
    /// than assumed compatible.
    #[test]
    fn canonical_without_provenance_is_refused_with_an_instruction() {
        let section = versions_section(
            json!({"node": "v24.14.0"}),
            json!({"node": "v20.9.0", "probe_scope_kind": "default"}),
            json!([]),
        );
        assert_eq!(
            scope_mismatch(&section),
            Some(ScopeMismatch::CanonicalMissing)
        );
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        assert!(plan.actions.is_empty());
        assert!(plan.skipped[0].reason.contains("re-run `env capture`"));
    }

    /// …and the mirror case: canonical reports a scope this box does not.
    #[test]
    fn local_without_provenance_is_refused() {
        let section = versions_section(
            json!({"node": "v24.14.0", "probe_scope_kind": "default"}),
            json!({"node": "v20.9.0"}),
            json!(["probe_scope_kind"]),
        );
        assert_eq!(
            scope_mismatch(&section),
            Some(ScopeMismatch::LocalMissing {
                canonical: "default".to_string()
            })
        );
        assert!(
            plan_versions(&section, &|_| Some(Manager::Volta), &home_scope())
                .actions
                .is_empty()
        );
    }

    // ------------------------------------------------------------------
    // The fail-safe — the load-bearing requirement of this slice
    // ------------------------------------------------------------------

    /// **The hard requirement.** No supported manager detected ⇒ report only,
    /// change nothing. Not a best-effort fallback, not a package-manager
    /// attempt.
    #[test]
    fn no_detected_manager_is_a_hard_no_op() {
        let section = versions_section(
            json!({"node": "v24.14.0", "python": "Python 3.13.13", "rustc": "rustc 1.95.0 (a b)"}),
            json!({"node": "v20.9.0", "python": "Python 3.12.1", "rustc": "rustc 1.80.0 (c d)"}),
            json!([]),
        );
        let plan = plan_versions(&section, &no_manager, &home_scope());
        assert!(plan.actions.is_empty(), "got {:?}", plan.actions);
        assert_eq!(plan.skipped.len(), 3);
        for skip in &plan.skipped {
            assert!(
                skip.reason
                    .contains("no supported version manager detected"),
                "got {}",
                skip.reason
            );
            assert!(skip.reason.contains("nothing changed"));
        }
    }

    /// …and it holds through the driver entry point with `--confirm` given: an
    /// undetectable manager must still produce zero changes.
    #[test]
    fn no_detected_manager_yields_no_changes_even_with_confirm() {
        let section = versions_section(
            json!({"node": "v24.14.0"}),
            json!({"node": "v20.9.0"}),
            json!([]),
        );
        let out = apply_section_with(&section, &no_manager, true);
        assert!(out.changes.is_empty());
        assert_eq!(out.status, SectionStatus::NothingToDo);
        assert_eq!(out.skipped.len(), 1);
    }

    // ------------------------------------------------------------------
    // The three-key surface
    // ------------------------------------------------------------------

    /// Repo-derived keys are dropped upstream by `actionable()`; a non-derived
    /// key that still is not one of the three is dropped HERE. Two independent
    /// gates, because the section is server-marked applyable as a whole.
    #[test]
    fn only_the_three_observed_keys_are_appliable() {
        let section = versions_section(
            json!({
                "node": "v24.14.0",
                "runner_crate_version": "1.0.9",
                "some_future_key": "9",
            }),
            json!({
                "node": "v20.9.0",
                "runner_crate_version": "1.0.5",
                "some_future_key": "8",
            }),
            json!(["runner_crate_version"]),
        );
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        let keys: Vec<&str> = plan.actions.iter().map(|a| a.tool.key()).collect();
        assert_eq!(keys, vec!["node"]);
        // The derived key never reached this module at all…
        assert!(!plan.skipped.iter().any(|s| s.key == "runner_crate_version"));
        // …and the unknown one is reported, not silently dropped.
        let unknown = plan
            .skipped
            .iter()
            .find(|s| s.key == "some_future_key")
            .expect("unknown key reported");
        assert!(unknown.reason.contains("not an observed toolchain version"));
    }

    /// The end of the chain the fix cuts: a key the capture could not MEASURE
    /// (its `--version` probe blew the 3s budget) must never become a real
    /// install. `actionable()` filters it upstream, so it never reaches
    /// `Tool::from_key` — and a measured sibling in the same section must still
    /// install, so the suppression cannot be section-wide.
    #[test]
    fn an_unmeasured_key_never_becomes_an_install_action() {
        let section = versions_section_with_unknown(
            json!({"rustc": "rustc 1.82.0 (f6e511eec 2024-10-15)", "node": "v24.14.0"}),
            // Neither captured locally; only `rustc` timed out.
            json!({}),
            json!([]),
            json!(["rustc"]),
        );
        assert!(section.is_unknown("rustc"));
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        let keys: Vec<&str> = plan.actions.iter().map(|a| a.tool.key()).collect();
        assert_eq!(
            keys,
            vec!["node"],
            "only the genuinely-absent tool may be installed"
        );
        // Filtered upstream by `actionable()`, so it never reaches
        // `Tool::from_key` and must not appear as a skip reason invented here.
        assert!(!plan.skipped.iter().any(|s| s.key == "rustc"));
        // …but "filtered upstream" is how a change becomes invisible. It IS
        // surfaced, on the notes channel, which costs no action.
        let note = plan
            .notes
            .iter()
            .find(|n| n.contains("rustc"))
            .expect("an unmeasured key must be surfaced, not silently dropped");
        assert!(note.contains("could NOT be measured"), "{note}");
        assert!(note.contains("NOT being installed"), "{note}");
    }

    /// The section-level consequence: with every change unmeasured there are no
    /// actions and no skips, and the status must NOT be `NothingToDo`. That
    /// label is a positive claim about the box, and `env apply` printing
    /// `versions [nothing to do]` about keys it never read is the same silence
    /// `SkipRecord`'s own doc exists to forbid.
    #[test]
    fn an_all_unmeasured_section_is_not_reported_as_nothing_to_do() {
        let section = versions_section_with_unknown(
            json!({"rustc": "rustc 1.82.0 (f6e511eec 2024-10-15)"}),
            json!({}),
            json!([]),
            json!(["rustc"]),
        );
        let out = apply_section_with(&section, &|_| Some(Manager::Rustup), false);
        assert!(out.changes.is_empty());
        assert!(out.skipped.is_empty());
        assert_ne!(
            out.status,
            SectionStatus::NothingToDo,
            "an unread key is not 'nothing to do'"
        );
        match &out.status {
            SectionStatus::Blocked(why) => {
                assert!(why.contains("could not be measured"), "{why}");
                assert!(why.contains("Nothing was changed"), "{why}");
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
        assert!(out.notes.iter().any(|n| n.contains("rustc")));

        // And the rendered report says so rather than going quiet.
        let text = super::super::apply::render_report(&super::super::apply::ApplyReport {
            environment_id: "env-1".to_string(),
            canonical_machine_name: Some("spaceship".to_string()),
            is_canonical_self: false,
            confirmed: false,
            sections: vec![out],
        });
        assert!(!text.contains("nothing to do"), "{text}");
        assert!(text.contains("could NOT be measured"), "{text}");
    }

    /// …and a section with a real action alongside an unmeasured key still
    /// plans that action. The surfacing must not become a section-wide brake.
    #[test]
    fn an_unmeasured_key_does_not_block_its_measured_sibling() {
        let section = versions_section_with_unknown(
            json!({"rustc": "rustc 1.82.0 (f6e511eec 2024-10-15)", "node": "v24.14.0"}),
            json!({}),
            json!([]),
            json!(["rustc"]),
        );
        let out = apply_section_with(&section, &|_| Some(Manager::Volta), false);
        assert_eq!(out.status, SectionStatus::Planned);
        let keys: Vec<&str> = out.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["node"]);
        assert!(out.notes.iter().any(|n| n.contains("rustc")));
    }

    /// An unparseable canonical value is refused rather than guessed at.
    #[test]
    fn unparseable_canonical_version_is_skipped() {
        let section = versions_section(
            json!({"node": "not a version"}),
            json!({"node": "v20.9.0"}),
            json!([]),
        );
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &home_scope());
        assert!(plan.actions.is_empty());
        assert!(plan.skipped[0].reason.contains("cannot read a version"));
    }

    // ------------------------------------------------------------------
    // drive_steps — pure command construction
    // ------------------------------------------------------------------

    /// rustup gets the narrower lever when the probe measures a project tree,
    /// and the machine default when it measures the box's default.
    #[test]
    fn rustup_uses_default_for_the_default_scope_and_override_for_a_tree() {
        let default_scope = drive_steps(Manager::Rustup, "1.95.0", true);
        assert_eq!(default_scope[0].args[0], "toolchain");
        assert_eq!(default_scope[1].args, vec!["default", "1.95.0"]);

        let tree = drive_steps(Manager::Rustup, "1.95.0", false);
        assert_eq!(tree[1].args, vec!["override", "set", "1.95.0"]);
    }

    #[test]
    fn node_and_python_managers_build_their_documented_commands() {
        assert_eq!(
            drive_steps(Manager::Volta, "24.14.0", true)[0].display(),
            "volta install node@24.14.0"
        );
        let fnm = drive_steps(Manager::Fnm, "24.14.0", true);
        assert_eq!(fnm[0].display(), "fnm install 24.14.0");
        assert_eq!(fnm[1].display(), "fnm default 24.14.0");

        let pyenv = drive_steps(Manager::Pyenv, "3.13.13", true);
        assert_eq!(pyenv[0].display(), "pyenv install --skip-existing 3.13.13");
        assert_eq!(pyenv[1].display(), "pyenv global 3.13.13");
    }

    /// POSIX nvm is a shell function, so it must be sourced — and the `alias
    /// default` is what makes a NEWLY SPAWNED probe (i.e. the next capture) see
    /// the version at all.
    #[test]
    fn posix_nvm_sources_the_script_and_sets_the_default_alias() {
        let steps = drive_steps(Manager::NvmPosix, "24.14.0", true);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].program, "bash");
        let script = &steps[0].args[1];
        assert!(script.contains("nvm.sh"), "{script}");
        assert!(script.contains("nvm install 24.14.0"), "{script}");
        assert!(script.contains("nvm alias default 24.14.0"), "{script}");
    }

    /// Every candidate list is non-empty and every manager builds at least one
    /// step, so no tool can silently detect a manager it cannot drive.
    #[test]
    fn every_candidate_manager_is_drivable() {
        for tool in APPLIABLE_TOOLS {
            let cands = candidates(*tool);
            assert!(!cands.is_empty(), "{:?} has no candidates", tool);
            for manager in cands {
                assert!(
                    !drive_steps(manager, "1.2.3", true).is_empty(),
                    "{manager:?} builds no steps"
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Preview
    // ------------------------------------------------------------------

    /// A dry run must expose the exact commands it would run, and must not
    /// report the section as applied.
    #[test]
    fn dry_run_previews_the_commands_and_changes_nothing() {
        let section = versions_section(
            json!({"node": "v24.14.0"}),
            json!({"node": "v20.9.0"}),
            json!([]),
        );
        let out = apply_section_with(&section, &|_| Some(Manager::Volta), false);
        assert_eq!(out.status, SectionStatus::Planned);
        assert_eq!(out.changes.len(), 1);
        let change = &out.changes[0];
        assert_eq!(change.key, "node");
        assert_eq!(change.from.as_deref(), Some("v20.9.0"));
        assert_eq!(change.to, "v24.14.0");
        assert!(change
            .detail
            .as_deref()
            .unwrap()
            .contains("volta install node@24.14.0"));
    }

    /// A declared project scope is called out, because it changes what the
    /// apply targets (that tree, not the box's default toolchain).
    #[test]
    fn a_project_scope_is_noted_in_the_plan() {
        let section = versions_section(
            json!({"node": "v24.14.0"}),
            json!({"node": "v20.9.0"}),
            json!([]),
        );
        let plan = plan_versions(&section, &|_| Some(Manager::Volta), &project_scope());
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.notes[0].contains("declared scope root"));
        // And the rustup lever would follow suit.
        assert!(!scope_is_default(&project_scope()));
    }

    #[test]
    fn nothing_actionable_is_nothing_to_do() {
        let section = versions_section(
            json!({"node": "v24.14.0"}),
            json!({"node": "v24.14.0"}),
            json!([]),
        );
        let out = apply_section_with(&section, &|_| Some(Manager::Volta), true);
        assert_eq!(out.status, SectionStatus::NothingToDo);
        assert!(out.changes.is_empty());
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn tail_keeps_the_last_lines_only() {
        assert_eq!(tail("a\n\nb\nc\nd\n"), "b | c | d");
        assert_eq!(tail(""), "");
    }
}
