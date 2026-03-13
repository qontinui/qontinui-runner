//! Constraint configuration loading.
//!
//! Loads constraint definitions from a TOML configuration file
//! (`constraints.toml`) in the project root. This allows projects to define
//! custom constraints and override built-in constraint settings.
//!
//! ## Config file format
//!
//! ```toml
//! # Override built-in constraint settings
//! [builtins]
//! no-secrets = true           # enabled by default
//! no-debug-statements = false # disable for this project
//! no-env-files = true         # enabled by default
//!
//! # Resource limits (optional)
//! [resources]
//! max_wall_time_secs = 1800       # 30 minute timeout
//! max_files_modified = 20         # scope creep guard
//! max_agentic_time_ms = 300000    # 5 minute AI budget
//! warning_threshold = 0.75        # warn at 75%
//!
//! # Custom constraints
//! [[constraint]]
//! id = "project:no-todos"
//! name = "No TODO markers"
//! description = "Remove all TODO/FIXME markers before committing"
//! severity = "warn"
//!
//! [constraint.check]
//! type = "grep_forbidden"
//! pattern = "(?i)\\b(TODO|FIXME|HACK|XXX)\\b"
//!
//! [[constraint]]
//! id = "project:src-scope"
//! name = "Source file scope"
//! description = "Only modify files in src/ and tests/"
//! severity = "block"
//!
//! [constraint.check]
//! type = "file_scope"
//! allowed_paths = ["src/", "tests/", "config/"]
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, info, warn};

use super::types::{Constraint, ConstraintCheck, ConstraintSeverity};
use crate::unified_workflow_executor::convergence::ResourceLimits;

/// The top-level TOML configuration structure.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ConfigFile {
    /// Override built-in constraint enable/disable.
    /// Key: builtin suffix (e.g., "no-secrets"), Value: enabled.
    builtins: HashMap<String, bool>,

    /// Resource limits.
    resources: Option<ResourceConfig>,

    /// Custom constraint definitions.
    #[serde(default)]
    constraint: Vec<ConstraintConfig>,
}

/// Resource limits from the config file.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResourceConfig {
    max_wall_time_secs: Option<u64>,
    max_files_modified: Option<u32>,
    max_agentic_time_ms: Option<u64>,
    warning_threshold: Option<f64>,
}

/// A constraint definition in the TOML config.
#[derive(Debug, Deserialize)]
struct ConstraintConfig {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_severity")]
    severity: String,
    check: CheckConfig,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_severity() -> String {
    "warn".to_string()
}

fn default_true() -> bool {
    true
}

/// A check definition in the TOML config.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CheckConfig {
    GrepForbidden {
        pattern: String,
        #[serde(default)]
        file_glob: Option<String>,
    },
    GrepRequired {
        pattern: String,
        #[serde(default)]
        file_glob: Option<String>,
    },
    FileScope {
        allowed_paths: Vec<String>,
    },
    Command {
        cmd: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
}

fn default_timeout() -> u64 {
    30
}

/// Result of loading a constraint config file.
pub struct LoadedConfig {
    /// Builtin overrides: map of builtin suffix → enabled.
    pub builtin_overrides: HashMap<String, bool>,
    /// Custom constraints from the config file.
    pub custom_constraints: Vec<Constraint>,
    /// Resource limits from the config file.
    pub resource_limits: Option<ResourceLimits>,
    /// Non-fatal warnings encountered during parsing (e.g., skipped constraints).
    pub warnings: Vec<String>,
}

/// The config file name to search for.
pub const CONFIG_FILENAME: &str = "constraints.toml";

/// Search for and load a constraints config file.
///
/// Searches for `constraints.toml` starting from `search_dir` and walking
/// up to the filesystem root. Returns the first config found, or `None`
/// if no config file exists.
///
/// This allows projects to place the config at any level (project root,
/// monorepo root, etc.) and have it automatically discovered.
pub fn load_config(search_dir: &Path) -> Option<LoadedConfig> {
    let config_path = find_config_file(search_dir)?;
    info!(
        "CONSTRAINT-CONFIG: Found config at {}",
        config_path.display()
    );
    load_config_from_path(&config_path)
}

/// Load a constraint config from a specific file path.
pub fn load_config_from_path(path: &Path) -> Option<LoadedConfig> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "CONSTRAINT-CONFIG: Failed to read {}: {}",
                path.display(),
                e
            );
            return None;
        }
    };

    parse_config(&content, path)
}

/// Parse a TOML config string into a `LoadedConfig`.
fn parse_config(content: &str, source_path: &Path) -> Option<LoadedConfig> {
    let config: ConfigFile = match toml::from_str(content) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "CONSTRAINT-CONFIG: Failed to parse {}: {}",
                source_path.display(),
                e
            );
            return None;
        }
    };

    // Convert builtin overrides
    let builtin_overrides = config.builtins;

    // Convert custom constraints
    let custom_constraints: Vec<Constraint> = config
        .constraint
        .into_iter()
        .filter_map(|cc| convert_constraint(cc, source_path))
        .collect();

    // Convert resource limits
    let resource_limits = config.resources.map(|rc| ResourceLimits {
        max_wall_time_secs: rc.max_wall_time_secs,
        max_files_modified: rc.max_files_modified,
        max_agentic_time_ms: rc.max_agentic_time_ms,
        warning_threshold: rc.warning_threshold.unwrap_or(0.75),
    });

    info!(
        "CONSTRAINT-CONFIG: Loaded {} builtin override(s), {} custom constraint(s), resources={}",
        builtin_overrides.len(),
        custom_constraints.len(),
        if resource_limits.is_some() {
            "configured"
        } else {
            "defaults"
        },
    );

    Some(LoadedConfig {
        builtin_overrides,
        custom_constraints,
        resource_limits,
        warnings: vec![],
    })
}

/// Convert a TOML constraint config into a `Constraint`.
fn convert_constraint(cc: ConstraintConfig, source: &Path) -> Option<Constraint> {
    let severity = match cc.severity.as_str() {
        "block" => ConstraintSeverity::Block,
        "warn" => ConstraintSeverity::Warn,
        "log" => ConstraintSeverity::Log,
        other => {
            warn!(
                "CONSTRAINT-CONFIG: Unknown severity '{}' in constraint '{}' ({}), using 'warn'",
                other,
                cc.id,
                source.display()
            );
            ConstraintSeverity::Warn
        }
    };

    let check = match cc.check {
        CheckConfig::GrepForbidden { pattern, file_glob } => {
            // Validate regex
            if let Err(e) = regex::Regex::new(&pattern) {
                warn!(
                    "CONSTRAINT-CONFIG: Invalid regex in constraint '{}': {}",
                    cc.id, e
                );
                return None;
            }
            ConstraintCheck::GrepForbidden { pattern, file_glob }
        }
        CheckConfig::GrepRequired { pattern, file_glob } => {
            if let Err(e) = regex::Regex::new(&pattern) {
                warn!(
                    "CONSTRAINT-CONFIG: Invalid regex in constraint '{}': {}",
                    cc.id, e
                );
                return None;
            }
            ConstraintCheck::GrepRequired { pattern, file_glob }
        }
        CheckConfig::FileScope { allowed_paths } => ConstraintCheck::FileScope { allowed_paths },
        CheckConfig::Command {
            cmd,
            cwd,
            timeout_secs,
        } => ConstraintCheck::Command {
            cmd,
            cwd,
            timeout_secs,
        },
    };

    Some(Constraint {
        id: cc.id,
        name: cc.name,
        description: cc.description,
        check,
        severity,
        enabled: cc.enabled,
    })
}

/// Parse a TOML config string, returning structured validation results.
///
/// Unlike `parse_config` (internal), this returns errors as strings instead of
/// logging them, making it suitable for the HTTP validation endpoint.
pub fn parse_config_str(content: &str) -> Result<LoadedConfig, Vec<String>> {
    let config: ConfigFile = match toml::from_str(content) {
        Ok(c) => c,
        Err(e) => {
            return Err(vec![format!("TOML parse error: {}", e)]);
        }
    };

    let mut errors = Vec::new();

    // Convert builtin overrides
    let builtin_overrides = config.builtins;

    // Convert custom constraints, collecting errors
    let mut custom_constraints = Vec::new();
    for cc in config.constraint {
        let source = Path::new("<input>");
        match convert_constraint(cc, source) {
            Some(c) => custom_constraints.push(c),
            None => {
                errors.push(
                    "A constraint was skipped due to invalid configuration (e.g., bad regex)"
                        .to_string(),
                );
            }
        }
    }

    // Convert resource limits
    let resource_limits = config.resources.map(|rc| ResourceLimits {
        max_wall_time_secs: rc.max_wall_time_secs,
        max_files_modified: rc.max_files_modified,
        max_agentic_time_ms: rc.max_agentic_time_ms,
        warning_threshold: rc.warning_threshold.unwrap_or(0.75),
    });

    Ok(LoadedConfig {
        builtin_overrides,
        custom_constraints,
        resource_limits,
        warnings: errors,
    })
}

/// Search for a config file starting from `dir` and walking up.
///
/// Checks for `constraints.toml` at each level, first in the directory
/// itself, then inside `.qontinui/`. Returns the first match found.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let candidate = current.join(CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }

        // Also check for .qontinui/constraints.toml
        let dotdir_candidate = current.join(".qontinui").join(CONFIG_FILENAME);
        if dotdir_candidate.is_file() {
            return Some(dotdir_candidate);
        }

        if !current.pop() {
            break;
        }
    }
    debug!(
        "CONSTRAINT-CONFIG: No {} found searching from {}",
        CONFIG_FILENAME,
        dir.display()
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[builtins]
no-debug-statements = false
"#;
        let config =
            parse_config(toml, Path::new("test.toml")).expect("Should parse minimal config");
        assert_eq!(
            config.builtin_overrides.get("no-debug-statements"),
            Some(&false)
        );
        assert!(config.custom_constraints.is_empty());
        assert!(config.resource_limits.is_none());
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[builtins]
no-secrets = true
no-debug-statements = false

[resources]
max_wall_time_secs = 1800
max_files_modified = 20
warning_threshold = 0.8

[[constraint]]
id = "project:no-todos"
name = "No TODO markers"
description = "Remove TODO markers"
severity = "warn"

[constraint.check]
type = "grep_forbidden"
pattern = "(?i)\\bTODO\\b"

[[constraint]]
id = "project:src-scope"
name = "Source scope"
description = "Only src/ and tests/"
severity = "block"

[constraint.check]
type = "file_scope"
allowed_paths = ["src/", "tests/"]
"#;
        let config = parse_config(toml, Path::new("test.toml")).expect("Should parse full config");

        // Builtin overrides
        assert_eq!(config.builtin_overrides.get("no-secrets"), Some(&true));
        assert_eq!(
            config.builtin_overrides.get("no-debug-statements"),
            Some(&false)
        );

        // Custom constraints
        assert_eq!(config.custom_constraints.len(), 2);
        assert_eq!(config.custom_constraints[0].id, "project:no-todos");
        assert_eq!(
            config.custom_constraints[0].severity,
            ConstraintSeverity::Warn
        );
        assert_eq!(config.custom_constraints[1].id, "project:src-scope");
        assert_eq!(
            config.custom_constraints[1].severity,
            ConstraintSeverity::Block
        );

        // Resource limits
        let limits = config.resource_limits.expect("Should have resource limits");
        assert_eq!(limits.max_wall_time_secs, Some(1800));
        assert_eq!(limits.max_files_modified, Some(20));
        assert_eq!(limits.warning_threshold, 0.8);
    }

    #[test]
    fn test_parse_empty_config() {
        let config = parse_config("", Path::new("test.toml")).expect("Empty config should parse");
        assert!(config.builtin_overrides.is_empty());
        assert!(config.custom_constraints.is_empty());
        assert!(config.resource_limits.is_none());
    }

    #[test]
    fn test_parse_invalid_regex_skips_constraint() {
        let toml = r#"
[[constraint]]
id = "bad-regex"
name = "Bad Regex"
severity = "warn"

[constraint.check]
type = "grep_forbidden"
pattern = "[invalid("
"#;
        let config = parse_config(toml, Path::new("test.toml")).expect("Should parse");
        assert!(
            config.custom_constraints.is_empty(),
            "Invalid regex should skip the constraint"
        );
    }

    #[test]
    fn test_parse_unknown_severity_defaults_to_warn() {
        let toml = r#"
[[constraint]]
id = "test"
name = "Test"
severity = "potato"

[constraint.check]
type = "file_scope"
allowed_paths = ["src/"]
"#;
        let config = parse_config(toml, Path::new("test.toml")).expect("Should parse");
        assert_eq!(config.custom_constraints.len(), 1);
        assert_eq!(
            config.custom_constraints[0].severity,
            ConstraintSeverity::Warn
        );
    }

    #[test]
    fn test_parse_command_constraint() {
        let toml = r#"
[[constraint]]
id = "project:compile-check"
name = "Quick compile"
description = "Verify code compiles"
severity = "block"

[constraint.check]
type = "command"
cmd = "cargo check"
timeout_secs = 60
"#;
        let config = parse_config(toml, Path::new("test.toml")).expect("Should parse");
        assert_eq!(config.custom_constraints.len(), 1);
        let c = &config.custom_constraints[0];
        assert!(matches!(
            &c.check,
            ConstraintCheck::Command { cmd, timeout_secs: 60, .. } if cmd == "cargo check"
        ));
    }

    #[test]
    fn test_parse_grep_required_constraint() {
        let toml = r#"
[[constraint]]
id = "project:license-header"
name = "License header"
description = "All Rust files must have a license header"
severity = "warn"

[constraint.check]
type = "grep_required"
pattern = "// Copyright"
file_glob = "*.rs"
"#;
        let config = parse_config(toml, Path::new("test.toml")).expect("Should parse");
        assert_eq!(config.custom_constraints.len(), 1);
        assert!(matches!(
            &config.custom_constraints[0].check,
            ConstraintCheck::GrepRequired { pattern, file_glob: Some(glob) }
            if pattern == "// Copyright" && glob == "*.rs"
        ));
    }

    #[test]
    fn test_find_config_file() {
        let dir = tempfile::tempdir().unwrap();
        // No config file → None
        assert!(find_config_file(dir.path()).is_none());

        // Create config file
        std::fs::write(dir.path().join(CONFIG_FILENAME), "[builtins]\n").unwrap();
        assert!(find_config_file(dir.path()).is_some());
    }

    #[test]
    fn test_find_config_in_dotdir() {
        let dir = tempfile::tempdir().unwrap();
        let dotdir = dir.path().join(".qontinui");
        std::fs::create_dir_all(&dotdir).unwrap();
        std::fs::write(dotdir.join(CONFIG_FILENAME), "[builtins]\n").unwrap();

        let found = find_config_file(dir.path());
        assert!(found.is_some());
        assert!(found.unwrap().to_string_lossy().contains(".qontinui"));
    }

    #[test]
    fn test_find_config_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&subdir).unwrap();

        // Config at the root
        std::fs::write(dir.path().join(CONFIG_FILENAME), "[builtins]\n").unwrap();

        let found = find_config_file(&subdir);
        assert!(found.is_some());
    }
}
