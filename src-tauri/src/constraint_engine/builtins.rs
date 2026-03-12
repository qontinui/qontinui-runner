//! Built-in constraint definitions.
//!
//! These are always-available constraints that can be enabled/disabled per workflow.
//! They cover common safety checks that apply to most development workflows.

use super::types::{Constraint, ConstraintCheck, ConstraintSeverity};

/// Get all built-in constraints.
///
/// Returns constraints that are enabled by default. Callers can override
/// the `enabled` flag per constraint to customize behavior.
pub fn builtin_constraints() -> Vec<Constraint> {
    vec![
        secret_detection(),
        debug_statement_detection(),
        env_file_detection(),
    ]
}

/// Detect potential secrets, API keys, and credentials in modified files.
fn secret_detection() -> Constraint {
    Constraint {
        id: "builtin:no-secrets".to_string(),
        name: "No Secrets in Code".to_string(),
        description: "Prevents committing hardcoded secrets, API keys, passwords, \
            and tokens. Use environment variables or a secrets manager instead."
            .to_string(),
        check: ConstraintCheck::GrepForbidden {
            // Match common secret patterns:
            // - API_KEY = "..." or api_key: "..."
            // - SECRET_KEY, PASSWORD, TOKEN, PRIVATE_KEY with assignment
            // - Bearer tokens
            // - AWS access keys (AKIA...)
            // Exclude: comments, variable references ($VAR), and common false positives
            pattern: r#"(?i)(?:api[_-]?key|secret[_-]?key|password|passwd|token|private[_-]?key|access[_-]?key)\s*[:=]\s*["'][^"'$\{]{8,}["']|AKIA[0-9A-Z]{16}|Bearer\s+[A-Za-z0-9\-._~+/]{20,}"#.to_string(),
            file_glob: None,
        },
        severity: ConstraintSeverity::Block,
        enabled: true,
    }
}

/// Detect debug/development-only statements that shouldn't be committed.
fn debug_statement_detection() -> Constraint {
    Constraint {
        id: "builtin:no-debug-statements".to_string(),
        name: "No Debug Statements".to_string(),
        description: "Detects common debug statements (console.log with debug markers, \
            dbg! macro, print debugging) that should be removed before committing."
            .to_string(),
        check: ConstraintCheck::GrepForbidden {
            // Match explicit debug markers, not all console.log/println:
            // - console.log("DEBUG", "TODO", "FIXME", "HACK", "XXX")
            // - dbg!(...) in Rust
            // - breakpoint() in Python
            // - debugger; in JavaScript
            pattern: r#"(?:console\.log\s*\(\s*["'](?:DEBUG|TODO|FIXME|HACK|XXX))|(?:^[^/]*\bdbg!\s*\()|(?:^\s*debugger\s*;)|(?:^\s*breakpoint\s*\(\s*\))"#.to_string(),
            file_glob: None,
        },
        severity: ConstraintSeverity::Warn,
        enabled: true,
    }
}

/// Detect .env files being modified (they should never be committed).
fn env_file_detection() -> Constraint {
    Constraint {
        id: "builtin:no-env-files".to_string(),
        name: "No .env File Modifications".to_string(),
        description: "Prevents modifying .env files which may contain secrets. \
            Use .env.example for templates."
            .to_string(),
        check: ConstraintCheck::FileScope {
            // This is inverted — we use FileScope with an empty allowed list,
            // and the evaluator checks specifically for .env files.
            // The evaluator has special handling for this constraint ID.
            allowed_paths: vec![],
        },
        severity: ConstraintSeverity::Block,
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtins_all_have_ids() {
        let constraints = builtin_constraints();
        assert!(!constraints.is_empty());
        for c in &constraints {
            assert!(
                c.id.starts_with("builtin:"),
                "ID should start with 'builtin:': {}",
                c.id
            );
            assert!(!c.name.is_empty());
            assert!(!c.description.is_empty());
        }
    }

    #[test]
    fn test_builtins_all_enabled_by_default() {
        let constraints = builtin_constraints();
        for c in &constraints {
            assert!(
                c.enabled,
                "Built-in '{}' should be enabled by default",
                c.id
            );
        }
    }
}
