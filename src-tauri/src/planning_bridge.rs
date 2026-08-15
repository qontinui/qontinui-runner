//! Bridge between Rust workflow executor and Python HTN planning layer.
//!
//! Provides functions for requesting HTN plans and reporting outcomes.
//! The Python planner lives in `multistate.planning` and is called via
//! subprocess/IPC.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Serialized world state for the Python HTN planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnWorldState {
    pub active_states: Vec<String>,
    pub available_transitions: Vec<String>,
    pub element_visible: std::collections::HashMap<String, bool>,
    pub element_values: std::collections::HashMap<String, String>,
}

/// A single action in an HTN plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnAction {
    pub name: String,
    pub args: Vec<serde_json::Value>,
}

/// Outcome of plan execution, reported for meta-optimizer learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnPlanOutcome {
    pub plan_id: String,
    pub success: bool,
    pub steps_executed: u32,
    pub steps_succeeded: u32,
    pub replans: u32,
    pub total_time_ms: f64,
    pub error: Option<String>,
}

/// Configuration for HTN planning integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnConfig {
    /// Whether to attempt HTN planning before falling back to AI agent.
    pub enabled: bool,
    /// Maximum time for the entire HTN attempt (planning + execution) in ms.
    pub timeout_ms: u64,
    /// Maximum replans during execution.
    pub max_replans: u32,
    /// Python executable path (uses "python" if not set).
    pub python_path: Option<String>,
    /// UI Bridge URL for querying element state (e.g., "http://localhost:1420").
    /// If None, HTN runs in plan-only mode without actual GUI execution.
    pub ui_bridge_url: Option<String>,
    /// UI Bridge target type: "web", "desktop", "mobile".
    pub target_type: Option<String>,
    /// Optional path to a serialized StateManager JSON file.
    /// When provided, the planner uses the saved state machine; otherwise empty.
    pub state_machine_path: Option<String>,
    /// Optional path to a directory of HTN method JSON files.
    /// When provided, methods are loaded alongside the built-in generic methods.
    pub methods_directory: Option<String>,
}

impl Default for HtnConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Off by default until method library matures
            timeout_ms: 15000,
            max_replans: 5,
            python_path: None,
            ui_bridge_url: None,
            target_type: None,
            state_machine_path: None,
            methods_directory: None,
        }
    }
}

/// Report plan execution outcome for meta-optimizer learning.
///
/// Stores the outcome via structured logging. Database persistence will be
/// added when the meta-optimizer DB schema is extended for HTN-specific data.
pub fn report_plan_outcome(outcome: &HtnPlanOutcome) {
    info!(
        plan_id = %outcome.plan_id,
        success = outcome.success,
        steps_executed = outcome.steps_executed,
        steps_succeeded = outcome.steps_succeeded,
        replans = outcome.replans,
        total_time_ms = outcome.total_time_ms,
        "HTN plan outcome",
    );
    // TODO: Store in learning_outcomes table when meta-optimizer DB schema is extended
    // For now, structured logging captures the data for analysis
    debug!("HTN plan outcome details: {:?}", outcome);
}

/// Check if HTN planning should be attempted for the current context.
///
/// Returns `true` if HTN is enabled. World state is now built inside the
/// Python subprocess (via `execute_htn_attempt`), so we no longer require
/// pre-populated state from Rust.
pub fn should_attempt_htn(config: &HtnConfig) -> bool {
    config.enabled
}

/// The sibling repo checkouts whose `src/` trees the Python HTN CLI imports
/// from, in the order they are placed on `PYTHONPATH`.
const HTN_SRC_REPOS: [&str; 2] = ["qontinui", "multistate"];

/// The `<workspace-root>/<repo>/src` trees for the HTN CLI, split by whether
/// they actually exist on this machine.
struct HtnSrcTrees {
    /// Trees that exist on disk and belong on `PYTHONPATH`.
    present: Vec<PathBuf>,
    /// `(repo name, path)` for each tree that does not exist. Omitted from
    /// `PYTHONPATH` and reported by the caller — never handed to Python.
    missing: Vec<(&'static str, PathBuf)>,
}

/// Locate the HTN source trees under an injected workspace root.
///
/// These two paths used to be
/// `env!("CARGO_MANIFEST_DIR")/../../{qontinui,multistate}/src`. That is a
/// **compile-time** constant: the shipped binary carried the *build* machine's
/// source-tree location, invisible to any grep for a drive-letter literal, and
/// both paths were handed to Python with **no existence check**. On any other
/// host the runner silently put two nonexistent directories on `PYTHONPATH` and
/// the failure surfaced as an import error deep inside the planner, far from the
/// cause. (Plan `2026-08-04-remove-hardcoded-machine-paths-from-product-code`,
/// slice 5 Phase 7 — class 2.)
///
/// The root is injected rather than resolved here so the layout rule is
/// unit-testable against a synthetic tree; the resolution lives in
/// [`execute_htn_attempt`], through the crate's one door
/// [`crate::workspace_paths::workspace_root`].
fn htn_src_trees(root: Option<&Path>) -> HtnSrcTrees {
    let mut trees = HtnSrcTrees {
        present: Vec::new(),
        missing: Vec::new(),
    };
    let Some(root) = root else {
        // Nothing resolved, so nothing is *known* to be missing either — the
        // caller logs the unresolved root once instead of naming each tree.
        return trees;
    };
    for repo in HTN_SRC_REPOS {
        let dir = root.join(repo).join("src");
        if dir.is_dir() {
            trees.present.push(dir);
        } else {
            trees.missing.push((repo, dir));
        }
    }
    trees
}

/// Join the source trees that exist with any inherited `PYTHONPATH`, using the
/// platform separator. Pure: the ordering and the separator are the whole rule,
/// and a tree that does not exist is simply absent from the result.
fn htn_pythonpath(present: &[PathBuf], existing: Option<&str>) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut parts: Vec<String> = present.iter().map(|p| p.display().to_string()).collect();
    if let Some(existing) = existing.filter(|e| !e.is_empty()) {
        parts.push(existing.to_string());
    }
    parts.join(sep)
}

/// The `PYTHONPATH` override to hand the child, or `None` when there is nothing
/// to say — no resolvable root AND no inherited value.
///
/// The distinction matters: setting `PYTHONPATH=""` is not the same as leaving
/// it alone. The pre-Phase-7 code always set a non-empty value, so an empty one
/// is a new state, and it is a claim about the import path rather than the
/// absence of a claim. CPython happens to ignore it, but the rule this phase
/// enforces is "never hand the interpreter something bogus", so the call is
/// skipped outright.
fn htn_pythonpath_env(present: &[PathBuf], existing: Option<&str>) -> Option<String> {
    let composed = htn_pythonpath(present, existing);
    (!composed.is_empty()).then_some(composed)
}

/// Result of a full HTN plan-and-execute attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnExecutionResult {
    /// Whether planning found a viable plan.
    pub plan_found: bool,
    /// Whether plan execution succeeded (only meaningful if plan_found).
    pub execution_success: bool,
    /// Number of plan actions.
    pub plan_actions: u32,
    /// Number of steps that executed successfully.
    pub steps_succeeded: u32,
    /// Number of replans during execution.
    pub replans: u32,
    /// Total time in ms (planning + execution).
    pub total_time_ms: f64,
    /// Summary of what was done (for AI context in case of failure).
    pub summary: String,
    /// Error message if failed.
    pub error: Option<String>,
}

/// Attempt to plan AND execute an HTN fix for the given failure context.
///
/// This is a self-contained call that runs a Python script which:
/// 1. Initializes HAL and optionally connects to UI Bridge
/// 2. Snapshots the current world state
/// 3. Plans using all registered operators and methods
/// 4. Executes the plan with replanning on state divergence
/// 5. Returns the execution result as JSON
///
/// Returns `Ok(HtnExecutionResult)` with success/failure and details.
/// Returns `Err(String)` if the Python subprocess fails entirely.
pub async fn execute_htn_attempt(
    task_description: &str,
    config: &HtnConfig,
) -> Result<HtnExecutionResult, String> {
    // The Python HTN CLI imports from the `qontinui` and `multistate` sibling
    // checkouts. Resolve them under the workspace root and existence-check each
    // one, so a host that lacks them gets a named warning HERE rather than an
    // import error deep inside Python.
    let workspace_root = crate::workspace_paths::workspace_root();
    if workspace_root.is_none() {
        warn!(
            "HTN: no Qontinui workspace root resolved, so neither qontinui/src nor \
             multistate/src was added to PYTHONPATH — the planner will see only what \
             is already installed for this interpreter. Set $QONTINUI_ROOT to the \
             directory holding the repo checkouts."
        );
    }
    let src_trees = htn_src_trees(workspace_root.as_deref());
    for (repo, dir) in &src_trees.missing {
        warn!(
            "HTN: {repo}/src does not exist at {} — omitted from PYTHONPATH. Set \
             $QONTINUI_ROOT to the directory holding the repo checkouts if the \
             planner needs it.",
            dir.display()
        );
    }

    // Build stdin JSON config for the CLI
    let stdin_config = serde_json::json!({
        "task": task_description,
        "ui_bridge_url": config.ui_bridge_url,
        "target_type": config.target_type.as_deref().unwrap_or("web"),
        "state_machine_path": config.state_machine_path,
        "methods_directory": config.methods_directory,
        "timeout_ms": config.timeout_ms,
        "max_replans": config.max_replans,
    });
    let stdin_json = stdin_config.to_string();

    let python = config.python_path.as_deref().unwrap_or("python");
    debug!(
        "Running HTN via: {} -m qontinui.planning_integration",
        python
    );

    // Build PYTHONPATH from the src trees that actually exist, plus whatever the
    // interpreter already carries.
    let inherited = std::env::var("PYTHONPATH").ok();

    let mut command = crate::process_helpers::tokio_no_window(python);
    command
        .args(["-m", "qontinui.planning_integration"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Nothing to say ⇒ say nothing; see [`htn_pythonpath_env`].
    if let Some(pythonpath) = htn_pythonpath_env(&src_trees.present, inherited.as_deref()) {
        command.env("PYTHONPATH", pythonpath);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn Python HTN CLI: {}", e))?;

    // Write config to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        if let Err(e) = stdin.write_all(stdin_json.as_bytes()).await {
            let _ = child.kill().await;
            return Err(format!("Failed to write HTN config to stdin: {}", e));
        }
        // Dropping stdin closes it, signaling EOF to Python
        drop(stdin);
    }

    let output =
        match tokio::time::timeout(std::time::Duration::from_millis(config.timeout_ms), async {
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();

            // Read stdout and stderr concurrently BEFORE waiting,
            // to avoid deadlock when pipe buffers fill up.
            let (stdout_result, stderr_result) = tokio::join!(
                async {
                    let mut bytes = Vec::new();
                    if let Some(mut out) = stdout_handle {
                        tokio::io::AsyncReadExt::read_to_end(&mut out, &mut bytes)
                            .await
                            .ok();
                    }
                    bytes
                },
                async {
                    let mut bytes = Vec::new();
                    if let Some(mut err) = stderr_handle {
                        tokio::io::AsyncReadExt::read_to_end(&mut err, &mut bytes)
                            .await
                            .ok();
                    }
                    bytes
                },
            );

            // Now wait for the child to finish
            let status = child.wait().await?;

            Ok::<std::process::Output, std::io::Error>(std::process::Output {
                status,
                stdout: stdout_result,
                stderr: stderr_result,
            })
        })
        .await
        {
            Ok(result) => result.map_err(|e| format!("Python HTN CLI failed: {}", e))?,
            Err(_) => {
                let _ = child.kill().await;
                return Err("HTN execute attempt timed out".to_string());
            }
        };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("HTN CLI exited with error: {}", stderr);
        return Err(format!("HTN CLI failed: {}", stderr));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout_str
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");

    let result: HtnExecutionResult = serde_json::from_str(json_line).map_err(|e| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!(
            "Failed to parse HTN CLI output: {} (stdout: {}, stderr: {})",
            e, stdout_str, stderr,
        )
    })?;

    info!(
        "HTN result: plan_found={}, exec_success={}, actions={}, succeeded={}/{}, replans={}, time={:.1}ms",
        result.plan_found,
        result.execution_success,
        result.plan_actions,
        result.steps_succeeded,
        result.plan_actions,
        result.replans,
        result.total_time_ms,
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A synthetic workspace root holding only the source trees a test asks
    /// for — never this machine's layout and never the ambient environment, so
    /// the verdict holds on a fresh checkout and on a non-operator machine.
    ///
    /// pid + counter scoped because several worktrees run `cargo test` on this
    /// box concurrently; cleanup is a `Drop` guard so a failing assertion does
    /// not leak the tree. Same shape as `workspace_paths::tests::Fixture`.
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(repos: &[&str]) -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_planning_bridge_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for repo in repos {
            std::fs::create_dir_all(root.join(repo).join("src")).unwrap();
        }
        Fixture { root }
    }

    fn sep() -> &'static str {
        if cfg!(windows) {
            ";"
        } else {
            ":"
        }
    }

    /// The property the `CARGO_MANIFEST_DIR` version could not have: a source
    /// tree that is absent on this host is **omitted** from `PYTHONPATH`, so the
    /// planner never receives a path that does not exist. Before slice 5 Phase 7
    /// both paths were handed to Python unconditionally.
    #[test]
    fn a_missing_src_tree_is_omitted_from_pythonpath() {
        let f = fixture(&["qontinui"]);

        let trees = htn_src_trees(Some(&f.root));
        assert_eq!(trees.present, vec![f.root.join("qontinui").join("src")]);
        assert_eq!(
            trees.missing.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            vec!["multistate"],
            "the absent tree must be reported by name so the warning can name it"
        );

        let pythonpath = htn_pythonpath(&trees.present, None);
        assert!(
            pythonpath.contains(&f.root.join("qontinui").join("src").display().to_string()),
            "the tree that exists must be on the path: {pythonpath}"
        );
        assert!(
            !pythonpath.contains("multistate"),
            "the absent tree must not appear at all: {pythonpath}"
        );
        for part in pythonpath.split(sep()) {
            assert!(
                Path::new(part).is_dir(),
                "PYTHONPATH must never carry a nonexistent path: {part}"
            );
        }
    }

    /// The layout rule: `<workspace-root>/<repo>/src`, in declaration order,
    /// with the inherited `PYTHONPATH` appended behind them.
    #[test]
    fn both_trees_resolve_under_the_root_and_precede_the_inherited_path() {
        let f = fixture(&["qontinui", "multistate"]);

        let trees = htn_src_trees(Some(&f.root));
        assert!(trees.missing.is_empty());

        assert_eq!(
            htn_pythonpath(&trees.present, Some("pre-existing")),
            format!(
                "{}{sep}{}{sep}pre-existing",
                f.root.join("qontinui").join("src").display(),
                f.root.join("multistate").join("src").display(),
                sep = sep()
            )
        );
    }

    /// An unresolved workspace root omits BOTH trees rather than guessing. The
    /// caller logs that once; nothing is reported as individually "missing",
    /// because with no root there is no path to have looked at.
    #[test]
    fn an_unresolved_workspace_root_omits_both_trees() {
        let trees = htn_src_trees(None);
        assert!(trees.present.is_empty());
        assert!(trees.missing.is_empty());
        assert_eq!(htn_pythonpath(&trees.present, None), "");
        assert_eq!(
            htn_pythonpath(&trees.present, Some("pre-existing")),
            "pre-existing",
            "the interpreter's own PYTHONPATH must survive an unresolved root"
        );
    }

    /// Nothing to say ⇒ the variable is not set at all. An empty
    /// `PYTHONPATH=""` is a new state the pre-Phase-7 code never produced (it
    /// always set a non-empty value), and it is a claim about the import path
    /// rather than the absence of one.
    #[test]
    fn nothing_to_say_sets_no_pythonpath_at_all() {
        let trees = htn_src_trees(None);
        assert_eq!(htn_pythonpath_env(&trees.present, None), None);
        assert_eq!(htn_pythonpath_env(&trees.present, Some("")), None);

        // …but anything real to say is still said.
        assert_eq!(
            htn_pythonpath_env(&trees.present, Some("pre-existing")).as_deref(),
            Some("pre-existing")
        );
        let f = fixture(&["qontinui"]);
        let trees = htn_src_trees(Some(&f.root));
        let expected = f.root.join("qontinui").join("src").display().to_string();
        assert_eq!(htn_pythonpath_env(&trees.present, None), Some(expected));
    }

    /// An empty inherited `PYTHONPATH` contributes nothing — no dangling
    /// separator, which would read as an empty (i.e. cwd) path entry to Python.
    #[test]
    fn an_empty_inherited_pythonpath_is_not_appended() {
        let f = fixture(&["qontinui"]);
        let trees = htn_src_trees(Some(&f.root));
        assert_eq!(
            htn_pythonpath(&trees.present, Some("")),
            f.root.join("qontinui").join("src").display().to_string()
        );
    }
}
