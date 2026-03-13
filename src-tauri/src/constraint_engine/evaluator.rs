//! Constraint evaluation engine.
//!
//! Evaluates constraints against modified files after the agentic phase.
//! Returns structured results that the loop controller converts into
//! convergence actions for the AI.

use std::path::{Path, PathBuf};

use regex::Regex;
use tracing::{debug, info, warn};

use super::builtins::builtin_constraints;
use super::types::*;
use crate::str_utils::truncate_str;
use crate::unified_workflow_executor::convergence::ConvergenceAction;

/// The constraint engine. Evaluates constraints against the working directory
/// after the AI makes changes.
pub struct ConstraintEngine {
    /// All registered constraints (builtins + user-defined).
    constraints: Vec<Constraint>,
    /// Project root directory for resolving file paths.
    project_root: Option<PathBuf>,
}

impl ConstraintEngine {
    /// Create a new constraint engine with built-in constraints.
    pub fn new() -> Self {
        Self {
            constraints: builtin_constraints(),
            project_root: None,
        }
    }

    /// Create a constraint engine from a loaded config.
    ///
    /// Applies builtin overrides (enable/disable) and adds custom constraints
    /// from the TOML config file. This is the preferred constructor when a
    /// project has a `constraints.toml` file.
    pub fn from_config(loaded: &super::config::LoadedConfig) -> Self {
        let mut engine = Self::new();

        // Apply builtin overrides
        for (suffix, enabled) in &loaded.builtin_overrides {
            let full_id = format!("builtin:{}", suffix);
            for c in &mut engine.constraints {
                if c.id == full_id {
                    c.enabled = *enabled;
                    info!(
                        "CONSTRAINT-ENGINE: Config override: {} → {}",
                        full_id,
                        if *enabled { "enabled" } else { "disabled" },
                    );
                }
            }
        }

        // Add custom constraints
        if !loaded.custom_constraints.is_empty() {
            info!(
                "CONSTRAINT-ENGINE: Adding {} custom constraint(s) from config",
                loaded.custom_constraints.len(),
            );
            engine.constraints.extend(loaded.custom_constraints.clone());
        }

        engine
    }

    /// Set the project root directory.
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }

    /// Add user-defined constraints.
    pub fn with_constraints(mut self, constraints: Vec<Constraint>) -> Self {
        self.constraints.extend(constraints);
        self
    }

    /// Get a reference to all registered constraints.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Disable a constraint by ID.
    pub fn disable_constraint(&mut self, id: &str) {
        for c in &mut self.constraints {
            if c.id == id {
                c.enabled = false;
            }
        }
    }

    /// Enable a constraint by ID.
    pub fn enable_constraint(&mut self, id: &str) {
        for c in &mut self.constraints {
            if c.id == id {
                c.enabled = true;
            }
        }
    }

    /// Apply AI-discovered constraint proposals.
    ///
    /// New constraints are added to the engine for evaluation in subsequent
    /// iterations. Builtin overrides are applied immediately. Returns the
    /// number of proposals applied.
    pub fn apply_proposals(&mut self, proposals: &[super::proposals::ConstraintProposal]) -> usize {
        let mut applied = 0;

        for proposal in proposals {
            match proposal {
                super::proposals::ConstraintProposal::NewConstraint(constraint) => {
                    // Skip if we already have a constraint with this ID
                    if self.constraints.iter().any(|c| c.id == constraint.id) {
                        debug!(
                            "CONSTRAINT-ENGINE: Skipping duplicate proposal '{}'",
                            constraint.id,
                        );
                        continue;
                    }
                    info!(
                        "CONSTRAINT-ENGINE: Adding AI-proposed constraint '{}' ({:?})",
                        constraint.id, constraint.severity,
                    );
                    self.constraints.push(constraint.clone());
                    applied += 1;
                }
                super::proposals::ConstraintProposal::BuiltinOverride {
                    builtin_suffix,
                    enabled,
                    reason,
                } => {
                    let full_id = format!("builtin:{}", builtin_suffix);
                    let mut found = false;
                    for c in &mut self.constraints {
                        if c.id == full_id {
                            c.enabled = *enabled;
                            found = true;
                            info!(
                                "CONSTRAINT-ENGINE: AI-proposed override: {} → {} (reason: {})",
                                full_id,
                                if *enabled { "enabled" } else { "disabled" },
                                reason,
                            );
                        }
                    }
                    if found {
                        applied += 1;
                    } else {
                        warn!(
                            "CONSTRAINT-ENGINE: AI proposed override for unknown builtin '{}'",
                            full_id,
                        );
                    }
                }
            }
        }

        applied
    }

    /// Evaluate all enabled constraints against the given modified files.
    ///
    /// # Arguments
    /// * `modified_files` - Paths of files modified in the agentic phase
    ///
    /// # Returns
    /// A vector of constraint results (one per enabled constraint).
    pub fn evaluate(&self, modified_files: &[String]) -> Vec<ConstraintResult> {
        if modified_files.is_empty() {
            debug!("CONSTRAINT-ENGINE: No modified files to check");
            return Vec::new();
        }

        let enabled: Vec<&Constraint> = self.constraints.iter().filter(|c| c.enabled).collect();

        if enabled.is_empty() {
            debug!("CONSTRAINT-ENGINE: No enabled constraints");
            return Vec::new();
        }

        info!(
            "CONSTRAINT-ENGINE: Evaluating {} constraint(s) against {} modified file(s)",
            enabled.len(),
            modified_files.len(),
        );

        let mut results = Vec::new();

        for constraint in &enabled {
            let result = self.evaluate_single(constraint, modified_files);
            if !result.passed {
                info!(
                    "CONSTRAINT-ENGINE: Violation in '{}' ({:?}) — {} violation(s)",
                    constraint.name,
                    constraint.severity,
                    result.violations.len(),
                );
            }
            results.push(result);
        }

        results
    }

    /// Evaluate a single constraint.
    fn evaluate_single(
        &self,
        constraint: &Constraint,
        modified_files: &[String],
    ) -> ConstraintResult {
        let violations = match &constraint.check {
            ConstraintCheck::GrepForbidden { pattern, file_glob } => {
                self.check_grep_forbidden(pattern, file_glob.as_deref(), modified_files)
            }
            ConstraintCheck::GrepRequired { pattern, file_glob } => {
                self.check_grep_required(pattern, file_glob.as_deref(), modified_files)
            }
            ConstraintCheck::FileScope { allowed_paths } => {
                // Special case for env file detection
                if constraint.id == "builtin:no-env-files" {
                    self.check_env_files(modified_files)
                } else {
                    self.check_file_scope(allowed_paths, modified_files)
                }
            }
            ConstraintCheck::Command {
                cmd,
                cwd,
                timeout_secs,
            } => self.check_command(cmd, cwd.as_deref(), *timeout_secs),
        };

        ConstraintResult {
            constraint_id: constraint.id.clone(),
            constraint_name: constraint.name.clone(),
            passed: violations.is_empty(),
            severity: constraint.severity,
            violations,
        }
    }

    /// Check for forbidden patterns in modified files.
    fn check_grep_forbidden(
        &self,
        pattern: &str,
        file_glob: Option<&str>,
        modified_files: &[String],
    ) -> Vec<ConstraintViolation> {
        let regex = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "CONSTRAINT-ENGINE: Invalid regex pattern '{}': {}",
                    pattern, e
                );
                return vec![ConstraintViolation {
                    file: None,
                    line: None,
                    detail: format!("Invalid constraint pattern: {}", e),
                }];
            }
        };

        let glob_pattern = file_glob.and_then(|g| glob::Pattern::new(g).ok());
        let mut violations = Vec::new();

        for file_path in modified_files {
            // Apply glob filter if specified
            if let Some(ref glob) = glob_pattern {
                if !glob.matches(file_path) {
                    continue;
                }
            }

            // Skip binary files and common non-text files
            if is_likely_binary(file_path) {
                continue;
            }

            // Resolve to absolute path
            let abs_path = self.resolve_path(file_path);

            // Read file contents
            let contents = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue, // File may have been deleted or moved
            };

            // Search for forbidden pattern
            for (line_num, line) in contents.lines().enumerate() {
                if regex.is_match(line) {
                    violations.push(ConstraintViolation {
                        file: Some(file_path.clone()),
                        line: Some((line_num + 1) as u32),
                        detail: format!(
                            "Forbidden pattern found: `{}`",
                            truncate_str(line.trim(), 120),
                        ),
                    });
                }
            }
        }

        violations
    }

    /// Check that required patterns exist in modified files.
    fn check_grep_required(
        &self,
        pattern: &str,
        file_glob: Option<&str>,
        modified_files: &[String],
    ) -> Vec<ConstraintViolation> {
        let regex = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "CONSTRAINT-ENGINE: Invalid regex pattern '{}': {}",
                    pattern, e
                );
                return vec![ConstraintViolation {
                    file: None,
                    line: None,
                    detail: format!("Invalid constraint pattern: {}", e),
                }];
            }
        };

        let glob_pattern = file_glob.and_then(|g| glob::Pattern::new(g).ok());
        let mut violations = Vec::new();

        for file_path in modified_files {
            if let Some(ref glob) = glob_pattern {
                if !glob.matches(file_path) {
                    continue;
                }
            }

            if is_likely_binary(file_path) {
                continue;
            }

            let abs_path = self.resolve_path(file_path);
            let contents = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if !regex.is_match(&contents) {
                violations.push(ConstraintViolation {
                    file: Some(file_path.clone()),
                    line: None,
                    detail: format!("Required pattern not found: `{}`", pattern),
                });
            }
        }

        violations
    }

    /// Check that modified files are within allowed directories.
    fn check_file_scope(
        &self,
        allowed_paths: &[String],
        modified_files: &[String],
    ) -> Vec<ConstraintViolation> {
        if allowed_paths.is_empty() {
            return Vec::new(); // No scope restriction
        }

        let mut violations = Vec::new();

        for file_path in modified_files {
            let normalized = file_path.replace('\\', "/");
            let in_scope = allowed_paths
                .iter()
                .any(|prefix| normalized.starts_with(prefix));

            if !in_scope {
                violations.push(ConstraintViolation {
                    file: Some(file_path.clone()),
                    line: None,
                    detail: format!(
                        "File is outside allowed scope. Allowed: {}",
                        allowed_paths.join(", "),
                    ),
                });
            }
        }

        violations
    }

    /// Special check for .env files in the modified file list.
    fn check_env_files(&self, modified_files: &[String]) -> Vec<ConstraintViolation> {
        let mut violations = Vec::new();

        for file_path in modified_files {
            let normalized = file_path.replace('\\', "/");
            let filename = Path::new(&normalized)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            // Match .env, .env.local, .env.production, etc.
            // But NOT .env.example or .env.template
            if filename.starts_with(".env")
                && !filename.contains("example")
                && !filename.contains("template")
                && !filename.contains("sample")
            {
                violations.push(ConstraintViolation {
                    file: Some(file_path.clone()),
                    line: None,
                    detail: "Environment file modified — these often contain secrets and should not be committed. Use .env.example for templates.".to_string(),
                });
            }
        }

        violations
    }

    /// Run a command and check exit code, with timeout.
    fn check_command(
        &self,
        cmd: &str,
        cwd: Option<&str>,
        timeout_secs: u64,
    ) -> Vec<ConstraintViolation> {
        let working_dir = match (cwd, &self.project_root) {
            (Some(relative), Some(root)) => root.join(relative),
            (None, Some(root)) => root.clone(),
            _ => std::env::current_dir().unwrap_or_default(),
        };

        // Parse command (simple split — doesn't handle quotes, but sufficient for
        // basic commands like "cargo check" or "npm run lint")
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return vec![ConstraintViolation {
                file: None,
                line: None,
                detail: "Empty constraint command".to_string(),
            }];
        }

        let mut child = match std::process::Command::new(parts[0])
            .args(&parts[1..])
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                return vec![ConstraintViolation {
                    file: None,
                    line: None,
                    detail: format!("Command `{}` failed to execute: {}", cmd, e),
                }];
            }
        };

        // Wait with timeout
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let start = std::time::Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Process exited — read output
                    if status.success() {
                        return Vec::new();
                    }
                    let stderr = child
                        .stderr
                        .take()
                        .and_then(|mut s| {
                            let mut buf = String::new();
                            std::io::Read::read_to_string(&mut s, &mut buf).ok()?;
                            Some(buf)
                        })
                        .unwrap_or_default();
                    return vec![ConstraintViolation {
                        file: None,
                        line: None,
                        detail: format!(
                            "Command `{}` failed (exit {}): {}",
                            cmd,
                            status.code().unwrap_or(-1),
                            truncate_str(stderr.trim(), 500),
                        ),
                    }];
                }
                Ok(None) => {
                    // Still running — check timeout
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return vec![ConstraintViolation {
                            file: None,
                            line: None,
                            detail: format!("Command `{}` timed out after {}s", cmd, timeout_secs,),
                        }];
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    return vec![ConstraintViolation {
                        file: None,
                        line: None,
                        detail: format!("Command `{}` failed to execute: {}", cmd, e),
                    }];
                }
            }
        }
    }

    /// Resolve a file path relative to the project root.
    fn resolve_path(&self, file_path: &str) -> PathBuf {
        let path = PathBuf::from(file_path);
        if path.is_absolute() {
            path
        } else if let Some(ref root) = self.project_root {
            root.join(file_path)
        } else {
            path
        }
    }

    /// Generate a prompt section describing active constraints.
    ///
    /// This is injected into the AI's context so it knows what constraints
    /// are enforced before making changes (proactive, not reactive).
    /// Returns empty string if no constraints are enabled.
    pub fn constraints_prompt(&self) -> String {
        let enabled: Vec<&Constraint> = self.constraints.iter().filter(|c| c.enabled).collect();

        if enabled.is_empty() {
            return String::new();
        }

        let mut lines = vec![
            "## Active Constraints".to_string(),
            String::new(),
            "The following constraints are enforced on your changes:".to_string(),
            String::new(),
        ];

        for c in &enabled {
            let severity_label = match c.severity {
                ConstraintSeverity::Block => "BLOCK",
                ConstraintSeverity::Warn => "WARN",
                ConstraintSeverity::Log => "LOG",
            };
            lines.push(format!(
                "- **{}** (`{}`, {}): {}",
                c.name, c.id, severity_label, c.description,
            ));
        }

        lines.push(String::new());
        lines.push(
            "BLOCK constraints will reject your changes. Fix violations before proceeding."
                .to_string(),
        );

        lines.join("\n")
    }

    /// Convert constraint results into convergence actions for the loop controller.
    ///
    /// This bridges the constraint engine with the convergence system — violations
    /// become context injections that the AI receives in the next iteration.
    pub fn results_to_actions(results: &[ConstraintResult]) -> Vec<ConvergenceAction> {
        let blocking: Vec<&ConstraintResult> = results
            .iter()
            .filter(|r| !r.passed && r.severity == ConstraintSeverity::Block)
            .collect();

        let warnings: Vec<&ConstraintResult> = results
            .iter()
            .filter(|r| !r.passed && r.severity == ConstraintSeverity::Warn)
            .collect();

        let mut actions = Vec::new();

        if !blocking.is_empty() {
            let mut context = String::from("### Constraint Violations (BLOCKING)\n\n");
            context.push_str(
                "The following constraints were violated by your changes. \
                You MUST fix these before proceeding:\n\n",
            );

            for result in &blocking {
                context.push_str(&format!(
                    "#### {} (`{}`)\n",
                    result.constraint_name, result.constraint_id
                ));
                for violation in &result.violations {
                    if let Some(ref file) = violation.file {
                        if let Some(line) = violation.line {
                            context.push_str(&format!(
                                "- `{}:{}` — {}\n",
                                file, line, violation.detail
                            ));
                        } else {
                            context.push_str(&format!("- `{}` — {}\n", file, violation.detail));
                        }
                    } else {
                        context.push_str(&format!("- {}\n", violation.detail));
                    }
                }
                context.push('\n');
            }

            actions.push(ConvergenceAction::InjectContext {
                context,
                priority: 0, // Highest priority — shown first
            });
        }

        if !warnings.is_empty() {
            let mut context = String::from("### Constraint Warnings\n\n");
            context.push_str(
                "The following constraints have warnings. These don't block execution \
                but should be addressed:\n\n",
            );

            for result in &warnings {
                context.push_str(&format!(
                    "#### {} (`{}`)\n",
                    result.constraint_name, result.constraint_id
                ));
                for violation in &result.violations {
                    if let Some(ref file) = violation.file {
                        if let Some(line) = violation.line {
                            context.push_str(&format!(
                                "- `{}:{}` — {}\n",
                                file, line, violation.detail
                            ));
                        } else {
                            context.push_str(&format!("- `{}` — {}\n", file, violation.detail));
                        }
                    } else {
                        context.push_str(&format!("- {}\n", violation.detail));
                    }
                }
                context.push('\n');
            }

            actions.push(ConvergenceAction::InjectContext {
                context,
                priority: 1,
            });
        }

        actions
    }

    /// Check if any blocking violations exist in the results.
    pub fn has_blocking_violations(results: &[ConstraintResult]) -> bool {
        results
            .iter()
            .any(|r| !r.passed && r.severity == ConstraintSeverity::Block)
    }

    /// Get a summary line for logging.
    pub fn summarize_results(results: &[ConstraintResult]) -> String {
        let total = results.len();
        let passed = results.iter().filter(|r| r.passed).count();
        let blocked = results
            .iter()
            .filter(|r| !r.passed && r.severity == ConstraintSeverity::Block)
            .count();
        let warned = results
            .iter()
            .filter(|r| !r.passed && r.severity == ConstraintSeverity::Warn)
            .count();

        format!(
            "{}/{} passed, {} blocking, {} warnings",
            passed, total, blocked, warned,
        )
    }
}

/// Check if a file path looks like a binary file.
fn is_likely_binary(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    matches!(
        ext.to_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "ico"
            | "svg"
            | "webp"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "otf"
            | "zip"
            | "gz"
            | "tar"
            | "bz2"
            | "7z"
            | "rar"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "mp3"
            | "mp4"
            | "wav"
            | "avi"
            | "mov"
            | "wasm"
            | "pyc"
            | "pyo"
            | "class"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_file(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        // Return the relative path as it would appear in modified_files
        name.to_string()
    }

    #[test]
    fn test_grep_forbidden_detects_secrets() {
        let dir = setup_test_dir();
        let file = write_file(
            dir.path(),
            "config.py",
            r#"
API_KEY = "sk-1234567890abcdef"
DATABASE_URL = "postgres://localhost/db"
"#,
        );

        let engine = ConstraintEngine::new().with_project_root(dir.path().to_path_buf());
        let results = engine.evaluate(&[file]);

        // The secret detection constraint should fire
        let secret_result = results
            .iter()
            .find(|r| r.constraint_id == "builtin:no-secrets");
        assert!(secret_result.is_some(), "Secret detection should run");
        let result = secret_result.unwrap();
        assert!(!result.passed, "Should detect the API_KEY secret");
        assert!(!result.violations.is_empty());
    }

    #[test]
    fn test_grep_forbidden_ignores_clean_files() {
        let dir = setup_test_dir();
        let file = write_file(
            dir.path(),
            "main.rs",
            r#"
fn main() {
    let config = Config::from_env();
    println!("Starting server");
}
"#,
        );

        let engine = ConstraintEngine::new().with_project_root(dir.path().to_path_buf());
        let results = engine.evaluate(&[file]);

        let secret_result = results
            .iter()
            .find(|r| r.constraint_id == "builtin:no-secrets");
        assert!(secret_result.is_some());
        assert!(secret_result.unwrap().passed, "Clean file should pass");
    }

    #[test]
    fn test_env_file_detection() {
        let dir = setup_test_dir();
        let _file = write_file(dir.path(), ".env", "SECRET=value\n");

        let engine = ConstraintEngine::new().with_project_root(dir.path().to_path_buf());
        let results = engine.evaluate(&[".env".to_string()]);

        let env_result = results
            .iter()
            .find(|r| r.constraint_id == "builtin:no-env-files");
        assert!(env_result.is_some());
        assert!(!env_result.unwrap().passed, ".env should be flagged");
    }

    #[test]
    fn test_env_example_allowed() {
        let dir = setup_test_dir();
        let _file = write_file(dir.path(), ".env.example", "SECRET=\n");

        let engine = ConstraintEngine::new().with_project_root(dir.path().to_path_buf());
        let results = engine.evaluate(&[".env.example".to_string()]);

        let env_result = results
            .iter()
            .find(|r| r.constraint_id == "builtin:no-env-files");
        assert!(env_result.is_some());
        assert!(env_result.unwrap().passed, ".env.example should be allowed");
    }

    #[test]
    fn test_file_scope_check() {
        let constraint = Constraint {
            id: "test:scope".to_string(),
            name: "Test Scope".to_string(),
            description: "Only allow src/ changes".to_string(),
            check: ConstraintCheck::FileScope {
                allowed_paths: vec!["src/".to_string(), "tests/".to_string()],
            },
            severity: ConstraintSeverity::Block,
            enabled: true,
        };

        let engine = ConstraintEngine {
            constraints: vec![constraint],
            project_root: None,
        };

        let results = engine.evaluate(&[
            "src/main.rs".to_string(),
            "tests/test_main.rs".to_string(),
            "scripts/deploy.sh".to_string(), // Out of scope
        ]);

        let scope_result = results.iter().find(|r| r.constraint_id == "test:scope");
        assert!(scope_result.is_some());
        let result = scope_result.unwrap();
        assert!(
            !result.passed,
            "Should fail because scripts/ is out of scope"
        );
        assert_eq!(result.violations.len(), 1);
        assert!(result.violations[0]
            .detail
            .contains("outside allowed scope"));
    }

    #[test]
    fn test_results_to_actions_blocking() {
        let results = vec![ConstraintResult {
            constraint_id: "test".to_string(),
            constraint_name: "Test Constraint".to_string(),
            passed: false,
            severity: ConstraintSeverity::Block,
            violations: vec![ConstraintViolation {
                file: Some("src/auth.rs".to_string()),
                line: Some(42),
                detail: "Secret found".to_string(),
            }],
        }];

        let actions = ConstraintEngine::results_to_actions(&results);
        assert!(!actions.is_empty());
        assert!(actions.iter().any(|a| matches!(
            a,
            ConvergenceAction::InjectContext { context, priority: 0 }
            if context.contains("BLOCKING") && context.contains("src/auth.rs:42")
        )));
    }

    #[test]
    fn test_results_to_actions_warnings() {
        let results = vec![ConstraintResult {
            constraint_id: "test".to_string(),
            constraint_name: "Test Constraint".to_string(),
            passed: false,
            severity: ConstraintSeverity::Warn,
            violations: vec![ConstraintViolation {
                file: Some("src/main.rs".to_string()),
                line: None,
                detail: "Debug statement found".to_string(),
            }],
        }];

        let actions = ConstraintEngine::results_to_actions(&results);
        assert!(actions
            .iter()
            .any(|a| matches!(a, ConvergenceAction::InjectContext { priority: 1, .. })));
    }

    #[test]
    fn test_disable_constraint() {
        let dir = setup_test_dir();
        let file = write_file(
            dir.path(),
            "config.py",
            r#"API_KEY = "sk-1234567890abcdef""#,
        );

        let mut engine = ConstraintEngine::new().with_project_root(dir.path().to_path_buf());
        engine.disable_constraint("builtin:no-secrets");

        let results = engine.evaluate(&[file]);

        let secret_result = results
            .iter()
            .find(|r| r.constraint_id == "builtin:no-secrets");
        assert!(
            secret_result.is_none(),
            "Disabled constraint should not run"
        );
    }

    #[test]
    fn test_empty_modified_files() {
        let engine = ConstraintEngine::new();
        let results = engine.evaluate(&[]);
        assert!(results.is_empty(), "No files → no results");
    }

    #[test]
    fn test_binary_files_skipped() {
        assert!(is_likely_binary("image.png"));
        assert!(is_likely_binary("font.woff2"));
        assert!(is_likely_binary("archive.zip"));
        assert!(!is_likely_binary("main.rs"));
        assert!(!is_likely_binary("config.toml"));
        assert!(!is_likely_binary("README.md"));
    }

    #[test]
    fn test_summarize_results() {
        let results = vec![
            ConstraintResult {
                constraint_id: "a".to_string(),
                constraint_name: "A".to_string(),
                passed: true,
                severity: ConstraintSeverity::Block,
                violations: vec![],
            },
            ConstraintResult {
                constraint_id: "b".to_string(),
                constraint_name: "B".to_string(),
                passed: false,
                severity: ConstraintSeverity::Block,
                violations: vec![ConstraintViolation {
                    file: None,
                    line: None,
                    detail: "fail".to_string(),
                }],
            },
            ConstraintResult {
                constraint_id: "c".to_string(),
                constraint_name: "C".to_string(),
                passed: false,
                severity: ConstraintSeverity::Warn,
                violations: vec![ConstraintViolation {
                    file: None,
                    line: None,
                    detail: "warn".to_string(),
                }],
            },
        ];

        let summary = ConstraintEngine::summarize_results(&results);
        assert_eq!(summary, "1/3 passed, 1 blocking, 1 warnings");
    }

    #[test]
    fn test_constraints_prompt_with_enabled() {
        let engine = ConstraintEngine::new();
        let prompt = engine.constraints_prompt();
        // Default engine has builtins enabled — should produce a non-empty prompt
        assert!(
            !prompt.is_empty(),
            "Should produce prompt for enabled builtins"
        );
        assert!(prompt.contains("## Active Constraints"));
        assert!(prompt.contains("BLOCK"));
    }

    #[test]
    fn test_constraints_prompt_empty_when_all_disabled() {
        let engine = ConstraintEngine {
            constraints: vec![Constraint {
                id: "test:disabled".to_string(),
                name: "Disabled".to_string(),
                description: "A disabled constraint".to_string(),
                check: ConstraintCheck::GrepForbidden {
                    pattern: "TODO".to_string(),
                    file_glob: None,
                },
                severity: ConstraintSeverity::Warn,
                enabled: false,
            }],
            project_root: None,
        };
        let prompt = engine.constraints_prompt();
        assert!(
            prompt.is_empty(),
            "Should be empty when all constraints are disabled"
        );
    }

    #[test]
    fn test_constraints_prompt_includes_severity_and_id() {
        let engine = ConstraintEngine {
            constraints: vec![
                Constraint {
                    id: "builtin:no-secrets".to_string(),
                    name: "No Secrets".to_string(),
                    description: "Do not introduce API keys".to_string(),
                    check: ConstraintCheck::GrepForbidden {
                        pattern: "sk-".to_string(),
                        file_glob: None,
                    },
                    severity: ConstraintSeverity::Block,
                    enabled: true,
                },
                Constraint {
                    id: "project:no-todos".to_string(),
                    name: "No TODOs".to_string(),
                    description: "Remove TODO markers".to_string(),
                    check: ConstraintCheck::GrepForbidden {
                        pattern: "TODO".to_string(),
                        file_glob: None,
                    },
                    severity: ConstraintSeverity::Warn,
                    enabled: true,
                },
            ],
            project_root: None,
        };
        let prompt = engine.constraints_prompt();
        assert!(prompt.contains("No Secrets"));
        assert!(prompt.contains("`builtin:no-secrets`"));
        assert!(prompt.contains("BLOCK"));
        assert!(prompt.contains("No TODOs"));
        assert!(prompt.contains("`project:no-todos`"));
        assert!(prompt.contains("WARN"));
        assert!(prompt.contains("BLOCK constraints will reject"));
    }
}
