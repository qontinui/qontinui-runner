//! Quality Gate - Composite threshold checker
//!
//! Runs multiple analyzers and checks results against configurable thresholds.
//! This is a meta-analyzer that aggregates results from other analyzers.

use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{
    CheckDefinition, CheckIssue, CheckStructuredOutput, CheckSummary, IssueSeverity,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Quality gate thresholds (can be configured via check command)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateConfig {
    /// Maximum number of circular dependencies allowed
    #[serde(default = "default_max_circular_deps")]
    pub max_circular_deps: u32,
    /// Maximum number of god classes allowed
    #[serde(default = "default_max_god_classes")]
    pub max_god_classes: u32,
    /// Maximum number of critical security issues allowed
    #[serde(default)]
    pub max_security_critical: u32,
    /// Maximum number of high-severity security issues allowed
    #[serde(default)]
    pub max_security_high: u32,
    /// Minimum type coverage percentage required
    #[serde(default = "default_min_type_coverage")]
    pub min_type_coverage: f64,
    /// Maximum cyclomatic complexity allowed per function
    #[serde(default = "default_max_complexity")]
    pub max_complexity: u32,
    /// Maximum coupling (dependencies) per module
    #[serde(default = "default_max_coupling")]
    pub max_coupling: u32,
}

fn default_max_circular_deps() -> u32 {
    0
}
fn default_max_god_classes() -> u32 {
    5
}
fn default_min_type_coverage() -> f64 {
    80.0
}
fn default_max_complexity() -> u32 {
    15
}
fn default_max_coupling() -> u32 {
    10
}

impl Default for QualityGateConfig {
    fn default() -> Self {
        Self {
            max_circular_deps: default_max_circular_deps(),
            max_god_classes: default_max_god_classes(),
            max_security_critical: 0,
            max_security_high: 0,
            min_type_coverage: default_min_type_coverage(),
            max_complexity: default_max_complexity(),
            max_coupling: default_max_coupling(),
        }
    }
}

impl QualityGateConfig {
    /// Parse config from a JSON string (from check_def.command)
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map_err(|e| format!("Failed to parse quality gate config: {}", e))
    }

    /// Parse config from command string
    /// Supports formats:
    /// - JSON: {"max_circular_deps": 0, "max_god_classes": 5, ...}
    /// - Key=value pairs: max_circular_deps=0 max_god_classes=5 ...
    pub fn from_command(command: &str) -> Self {
        let command = command.trim();

        // Try JSON first
        if command.starts_with('{') {
            match Self::from_json(command) {
                Ok(config) => return config,
                Err(e) => {
                    warn!("Failed to parse quality gate config as JSON: {}", e);
                }
            }
        }

        // Try key=value format
        let mut config = Self::default();
        for part in command.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                match key {
                    "max_circular_deps" => {
                        if let Ok(v) = value.parse() {
                            config.max_circular_deps = v;
                        }
                    }
                    "max_god_classes" => {
                        if let Ok(v) = value.parse() {
                            config.max_god_classes = v;
                        }
                    }
                    "max_security_critical" => {
                        if let Ok(v) = value.parse() {
                            config.max_security_critical = v;
                        }
                    }
                    "max_security_high" => {
                        if let Ok(v) = value.parse() {
                            config.max_security_high = v;
                        }
                    }
                    "min_type_coverage" => {
                        if let Ok(v) = value.parse() {
                            config.min_type_coverage = v;
                        }
                    }
                    "max_complexity" => {
                        if let Ok(v) = value.parse() {
                            config.max_complexity = v;
                        }
                    }
                    "max_coupling" => {
                        if let Ok(v) = value.parse() {
                            config.max_coupling = v;
                        }
                    }
                    _ => {
                        debug!("Unknown quality gate config key: {}", key);
                    }
                }
            }
        }

        config
    }
}

/// Individual gate check result
#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub name: String,
    pub passed: bool,
    pub actual_value: serde_json::Value,
    pub threshold: serde_json::Value,
    pub message: String,
}

/// Result of running all quality gates
#[derive(Debug)]
struct QualityGateResults {
    gates: Vec<GateResult>,
    issues: Vec<CheckIssue>,
}

impl QualityGateResults {
    fn new() -> Self {
        Self {
            gates: Vec::new(),
            issues: Vec::new(),
        }
    }

    fn add_gate(
        &mut self,
        name: &str,
        passed: bool,
        actual: impl Into<serde_json::Value>,
        threshold: impl Into<serde_json::Value>,
        message: String,
        working_dir: &str,
        code: &str,
    ) {
        let actual_value = actual.into();
        let threshold_value = threshold.into();

        self.gates.push(GateResult {
            name: name.to_string(),
            passed,
            actual_value: actual_value.clone(),
            threshold: threshold_value.clone(),
            message: message.clone(),
        });

        if !passed {
            self.issues.push(CheckIssue {
                file: working_dir.to_string(),
                line: None,
                column: None,
                end_line: None,
                end_column: None,
                code: Some(code.to_string()),
                message,
                severity: IssueSeverity::Error,
                fixable: false,
                fix_applied: None,
            });
        }
    }

    fn gates_passed(&self) -> u32 {
        self.gates.iter().filter(|g| g.passed).count() as u32
    }

    fn gates_failed(&self) -> u32 {
        self.gates.iter().filter(|g| !g.passed).count() as u32
    }

    fn total_gates(&self) -> u32 {
        self.gates.len() as u32
    }
}

/// Analyze using quality gates
///
/// Runs multiple code quality analyzers and checks their results against
/// configurable thresholds. Returns a single aggregated result.
pub fn analyze(working_dir: &str, check_def: &CheckDefinition) -> Result<ParsedOutput, String> {
    info!("Running quality gate analysis on: {}", working_dir);

    // Parse config from command if provided, otherwise use defaults
    let config = check_def
        .command
        .as_ref()
        .map(|cmd| QualityGateConfig::from_command(cmd))
        .unwrap_or_default();

    debug!("Quality gate config: {:?}", config);

    let mut results = QualityGateResults::new();

    // Run each analyzer and check against thresholds
    run_circular_deps_gate(&mut results, working_dir, &config);
    run_god_class_gate(&mut results, working_dir, &config);
    run_security_gate(&mut results, working_dir, &config);
    run_coupling_gate(&mut results, working_dir, &config);
    run_type_coverage_gate(&mut results, working_dir, &config);
    run_complexity_gate(&mut results, working_dir, &config);

    // Build the final result
    let gates_passed = results.gates_passed();
    let gates_failed = results.gates_failed();
    let total_gates = results.total_gates();
    let issues_found = results.issues.len() as u32;

    info!(
        "Quality gate complete: {}/{} gates passed",
        gates_passed, total_gates
    );

    // Build tool_data with detailed gate results
    let mut tool_data = HashMap::new();
    tool_data.insert("gates_passed".to_string(), serde_json::json!(gates_passed));
    tool_data.insert("gates_failed".to_string(), serde_json::json!(gates_failed));
    tool_data.insert("total_gates".to_string(), serde_json::json!(total_gates));
    tool_data.insert(
        "pass_rate".to_string(),
        serde_json::json!(if total_gates > 0 {
            (gates_passed as f64 / total_gates as f64) * 100.0
        } else {
            100.0
        }),
    );
    tool_data.insert(
        "gate_results".to_string(),
        serde_json::to_value(&results.gates).unwrap_or_default(),
    );
    tool_data.insert(
        "config".to_string(),
        serde_json::to_value(&config).unwrap_or_default(),
    );

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: total_gates,
        structured_output: CheckStructuredOutput {
            issues: results.issues,
            summary: Some(CheckSummary {
                total_files: total_gates,
                files_with_issues: gates_failed,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), gates_failed),
                    ("passed".to_string(), gates_passed),
                ]),
            }),
            tool_data,
        },
    })
}

/// Run circular dependencies gate
fn run_circular_deps_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running circular dependencies check");

    match super::super::python::circular_deps::analyze(working_dir) {
        Ok(result) => {
            let count = result.issues_found;
            let passed = count <= config.max_circular_deps;
            let message = if passed {
                format!(
                    "Circular dependencies: {} (max: {})",
                    count, config.max_circular_deps
                )
            } else {
                format!(
                    "Circular dependencies exceeded: {} found (max: {})",
                    count, config.max_circular_deps
                )
            };

            results.add_gate(
                "circular_deps",
                passed,
                count,
                config.max_circular_deps,
                message,
                working_dir,
                "QG001",
            );
        }
        Err(e) => {
            debug!("Circular dependencies analyzer not available: {}", e);
            // Skip this gate if analyzer is not available
        }
    }
}

/// Run god class gate
fn run_god_class_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running god class check");

    match super::super::python::god_class::analyze_with_defaults(working_dir) {
        Ok(result) => {
            let count = result.issues_found;
            let passed = count <= config.max_god_classes;
            let message = if passed {
                format!("God classes: {} (max: {})", count, config.max_god_classes)
            } else {
                format!(
                    "God classes exceeded: {} found (max: {})",
                    count, config.max_god_classes
                )
            };

            results.add_gate(
                "god_class",
                passed,
                count,
                config.max_god_classes,
                message,
                working_dir,
                "QG002",
            );
        }
        Err(e) => {
            debug!("God class analyzer not available: {}", e);
        }
    }
}

/// Run security gate (checks both critical and high severity)
fn run_security_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running security scan");

    match super::super::security::scan::analyze_with_defaults(working_dir) {
        Ok(result) => {
            // Count issues by severity from the issues list
            // Critical issues have severity Error and code containing "critical"
            let critical_count = result
                .structured_output
                .issues
                .iter()
                .filter(|i| {
                    i.severity == IssueSeverity::Error
                        && i.code
                            .as_ref()
                            .map(|c| c.to_lowercase().contains("critical"))
                            .unwrap_or(false)
                })
                .count() as u32;

            // High issues are other Error severity issues
            let high_count = result
                .structured_output
                .issues
                .iter()
                .filter(|i| {
                    i.severity == IssueSeverity::Error
                        && !i
                            .code
                            .as_ref()
                            .map(|c| c.to_lowercase().contains("critical"))
                            .unwrap_or(false)
                })
                .count() as u32;

            // Check critical security issues
            let critical_passed = critical_count <= config.max_security_critical;
            let critical_message = if critical_passed {
                format!(
                    "Critical security issues: {} (max: {})",
                    critical_count, config.max_security_critical
                )
            } else {
                format!(
                    "Critical security issues exceeded: {} found (max: {})",
                    critical_count, config.max_security_critical
                )
            };

            results.add_gate(
                "security_critical",
                critical_passed,
                critical_count,
                config.max_security_critical,
                critical_message,
                working_dir,
                "QG003",
            );

            // Check high-severity security issues
            let high_passed = high_count <= config.max_security_high;
            let high_message = if high_passed {
                format!(
                    "High-severity security issues: {} (max: {})",
                    high_count, config.max_security_high
                )
            } else {
                format!(
                    "High-severity security issues exceeded: {} found (max: {})",
                    high_count, config.max_security_high
                )
            };

            results.add_gate(
                "security_high",
                high_passed,
                high_count,
                config.max_security_high,
                high_message,
                working_dir,
                "QG004",
            );
        }
        Err(e) => {
            debug!("Security scanner not available: {}", e);
        }
    }
}

/// Run coupling gate
fn run_coupling_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running coupling check");

    match super::super::python::coupling::analyze_with_defaults(working_dir) {
        Ok(result) => {
            // Get the max coupling value from tool_data
            let max_coupling = result
                .structured_output
                .tool_data
                .get("max_coupling")
                .and_then(|v| v.as_u64())
                .unwrap_or(result.issues_found as u64) as u32;

            let passed = max_coupling <= config.max_coupling;
            let message = if passed {
                format!(
                    "Max coupling: {} (max: {})",
                    max_coupling, config.max_coupling
                )
            } else {
                format!(
                    "Coupling exceeded: {} dependencies (max: {})",
                    max_coupling, config.max_coupling
                )
            };

            results.add_gate(
                "coupling",
                passed,
                max_coupling,
                config.max_coupling,
                message,
                working_dir,
                "QG005",
            );
        }
        Err(e) => {
            debug!("Coupling analyzer not available: {}", e);
        }
    }
}

/// Run type coverage gate
fn run_type_coverage_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running type coverage check");

    match super::super::python::type_coverage::analyze_with_defaults(working_dir) {
        Ok(result) => {
            // Get coverage percentage from tool_data
            let coverage = result
                .structured_output
                .tool_data
                .get("coverage_percent")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            let passed = coverage >= config.min_type_coverage;
            let message = if passed {
                format!(
                    "Type coverage: {:.1}% (min: {:.1}%)",
                    coverage, config.min_type_coverage
                )
            } else {
                format!(
                    "Type coverage below threshold: {:.1}% (min: {:.1}%)",
                    coverage, config.min_type_coverage
                )
            };

            results.add_gate(
                "type_coverage",
                passed,
                coverage,
                config.min_type_coverage,
                message,
                working_dir,
                "QG006",
            );
        }
        Err(e) => {
            debug!("Type coverage analyzer not available: {}", e);
        }
    }
}

/// Run complexity gate
fn run_complexity_gate(
    results: &mut QualityGateResults,
    working_dir: &str,
    config: &QualityGateConfig,
) {
    debug!("Running complexity check");

    // Try Rust complexity analyzer first, then fall back to Python SRP
    let complexity_result =
        super::super::rust_analyzer::complexity::analyze_with_defaults(working_dir)
            .or_else(|_| super::super::python::srp::analyze_with_defaults(working_dir));

    match complexity_result {
        Ok(result) => {
            // Get max complexity from tool_data
            let max_complexity = result
                .structured_output
                .tool_data
                .get("max_complexity")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            let passed = max_complexity <= config.max_complexity;
            let message = if passed {
                format!(
                    "Max complexity: {} (max: {})",
                    max_complexity, config.max_complexity
                )
            } else {
                format!(
                    "Complexity exceeded: {} (max: {})",
                    max_complexity, config.max_complexity
                )
            };

            results.add_gate(
                "complexity",
                passed,
                max_complexity,
                config.max_complexity,
                message,
                working_dir,
                "QG007",
            );
        }
        Err(e) => {
            debug!("Complexity analyzer not available: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = QualityGateConfig::default();
        assert_eq!(config.max_circular_deps, 0);
        assert_eq!(config.max_god_classes, 5);
        assert_eq!(config.max_security_critical, 0);
        assert_eq!(config.max_security_high, 0);
        assert!((config.min_type_coverage - 80.0).abs() < 0.01);
        assert_eq!(config.max_complexity, 15);
        assert_eq!(config.max_coupling, 10);
    }

    #[test]
    fn test_config_from_json() {
        let json = r#"{"max_circular_deps": 2, "max_god_classes": 10, "min_type_coverage": 90.0}"#;
        let config = QualityGateConfig::from_json(json).unwrap();
        assert_eq!(config.max_circular_deps, 2);
        assert_eq!(config.max_god_classes, 10);
        assert!((config.min_type_coverage - 90.0).abs() < 0.01);
        // Defaults for unspecified fields
        assert_eq!(config.max_security_critical, 0);
    }

    #[test]
    fn test_config_from_command_key_value() {
        let command = "max_circular_deps=3 max_god_classes=8 min_type_coverage=75.5";
        let config = QualityGateConfig::from_command(command);
        assert_eq!(config.max_circular_deps, 3);
        assert_eq!(config.max_god_classes, 8);
        assert!((config.min_type_coverage - 75.5).abs() < 0.01);
    }

    #[test]
    fn test_config_from_command_json() {
        let command = r#"{"max_circular_deps": 1, "max_complexity": 20}"#;
        let config = QualityGateConfig::from_command(command);
        assert_eq!(config.max_circular_deps, 1);
        assert_eq!(config.max_complexity, 20);
    }

    #[test]
    fn test_gate_results_counting() {
        let mut results = QualityGateResults::new();

        results.add_gate("test1", true, 0, 5, "Passed".to_string(), "/test", "T001");
        results.add_gate("test2", false, 10, 5, "Failed".to_string(), "/test", "T002");
        results.add_gate("test3", true, 3, 5, "Passed".to_string(), "/test", "T003");

        assert_eq!(results.gates_passed(), 2);
        assert_eq!(results.gates_failed(), 1);
        assert_eq!(results.total_gates(), 3);
        assert_eq!(results.issues.len(), 1); // Only failed gates create issues
    }
}
