//! Fork D — the enforcement loop. The named claude-config skills in
//! [`qontinui_types::priorities_profile::EnforcementProfile`] (`code-reviewer`,
//! `security-scan`) run as post-generation gates between "generated" and "covered":
//! a finding whose category ∈ `block_on` (`security`) **blocks** (revision rejected,
//! finding fed back into the next codegen prompt); a finding outside `block_on` is
//! advisory (recorded, not fatal).
//!
//! ## Honesty boundary (structurally-present, NOT run in this crate's tests)
//!
//! This module builds the **hook + the interface** the orchestration loop drives, and a
//! deterministic, self-contained [`builtin_static_checks`] gate (a real, runnable
//! scan — no external skill process — that flags a small set of insecure patterns).
//! What it does NOT do here is *spawn the actual `code-reviewer` / `security-scan` skill
//! subprocesses*: those are claude-config skills invoked by the conductor/runner in the
//! live loop, not reachable as a unit-testable library call from this crate. So the
//! `run`-named skills are wired as an [`EnforcementGate`] trait the loop dispatches to,
//! with the built-in static gate as the one gate that genuinely runs here. The
//! integration with the real skills is flagged honestly as
//! **structurally-present-but-not-run** rather than faked.

use qontinui_types::priorities_profile::{EnforcementProfile, Profile};

use crate::scaffold::GeneratedBackend;

/// One finding from an enforcement gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The gate that produced it (e.g. `"security-scan"`, `"builtin-static"`).
    pub gate: String,
    /// The finding category (e.g. `"security"`, `"style"`). Compared against
    /// `EnforcementProfile.block_on` to decide fatal-vs-advisory.
    pub category: String,
    /// The relative file the finding is about.
    pub file: String,
    /// Human-readable description.
    pub detail: String,
}

/// The verdict of running every configured gate over a generated tree.
#[derive(Debug, Clone, Default)]
pub struct EnforcementReport {
    /// All findings, both blocking and advisory.
    pub findings: Vec<Finding>,
    /// Findings whose category ∈ `block_on` — these reject the revision.
    pub blocking: Vec<Finding>,
    /// Findings outside `block_on` — recorded, not fatal.
    pub advisory: Vec<Finding>,
    /// Gates named in `run` that were NOT executed here (the external skills) — listed
    /// so the caller can honestly report what's structurally-wired vs actually-run.
    pub deferred_gates: Vec<String>,
}

impl EnforcementReport {
    /// True iff no blocking finding — the revision may be admitted to the verify phase.
    pub fn passed(&self) -> bool {
        self.blocking.is_empty()
    }
}

/// A gate that can scan a generated tree. The built-in static gate implements this;
/// the external `code-reviewer` / `security-scan` skills are dispatched to by the loop
/// via the same interface (their adapters live in the runner, not this crate).
pub trait EnforcementGate {
    /// The gate's name (matched against `EnforcementProfile.run`).
    fn name(&self) -> &str;
    /// Scan the tree, returning findings.
    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding>;
}

/// Run the enforcement gates declared by the profile over a generated backend.
///
/// Only gates with an in-crate implementation actually run here (the built-in static
/// scan); any other gate named in `run` is recorded in `deferred_gates` so the report is
/// honest about what executed. `block_on` decides which findings are fatal.
pub fn run_enforcement(profile: &Profile, backend: &GeneratedBackend) -> EnforcementReport {
    let enforcement = profile.enforcement.clone().unwrap_or_default();
    run_enforcement_with(&enforcement, backend, &available_gates())
}

/// Lower-level entry: run an explicit gate set. Lets the loop inject real skill adapters.
pub fn run_enforcement_with(
    enforcement: &EnforcementProfile,
    backend: &GeneratedBackend,
    gates: &[Box<dyn EnforcementGate>],
) -> EnforcementReport {
    let mut report = EnforcementReport::default();

    for requested in &enforcement.run {
        match gates.iter().find(|g| g.name() == requested) {
            Some(gate) => {
                for f in gate.scan(backend) {
                    if enforcement.block_on.iter().any(|c| c == &f.category) {
                        report.blocking.push(f.clone());
                    } else {
                        report.advisory.push(f.clone());
                    }
                    report.findings.push(f);
                }
            }
            None => {
                // Named in `run` but no in-crate adapter — structurally wired, not run here.
                report.deferred_gates.push(requested.clone());
            }
        }
    }
    report
}

/// The gates this crate can actually run. The built-in static scan stands in as a real,
/// runnable `security`-category gate (so the loop's blocking path is exercised in tests)
/// AND is registered under the `security-scan` name so a profile that requests
/// `security-scan` gets a genuine scan rather than a deferral.
fn available_gates() -> Vec<Box<dyn EnforcementGate>> {
    vec![
        Box::new(BuiltinStaticGate {
            name: "security-scan".to_string(),
        }),
        Box::new(BuiltinStaticGate {
            name: "builtin-static".to_string(),
        }),
    ]
}

/// A deterministic, self-contained static scan: flags a small set of obviously-insecure
/// patterns in the generated python. Real and runnable (no external process) — it is the
/// gate that genuinely executes in this crate's tests, proving the blocking path works.
pub struct BuiltinStaticGate {
    name: String,
}

impl EnforcementGate for BuiltinStaticGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding> {
        builtin_static_checks(&self.name, backend)
    }
}

/// The actual checks. Flags: `eval(`/`exec(` usage, a hard-coded `password=` literal in
/// source, and `DEBUG = True`. Category `security` so it hits the `block_on` path.
pub fn builtin_static_checks(gate: &str, backend: &GeneratedBackend) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (rel, src) in &backend.files {
        if !rel.ends_with(".py") {
            continue;
        }
        for (needle, detail) in [
            ("eval(", "use of eval() on untrusted input is unsafe"),
            ("exec(", "use of exec() is unsafe"),
            ("password=\"", "hard-coded password literal in source"),
            (
                "DEBUG = True",
                "DEBUG=True leaks stack traces in production",
            ),
        ] {
            if src.contains(needle) {
                findings.push(Finding {
                    gate: gate.to_string(),
                    category: "security".to_string(),
                    file: rel.clone(),
                    detail: detail.to_string(),
                });
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn backend_with(files: &[(&str, &str)]) -> GeneratedBackend {
        let mut map = BTreeMap::new();
        for (k, v) in files {
            map.insert(k.to_string(), v.to_string());
        }
        GeneratedBackend {
            files: map,
            models: vec![],
            routes: vec![],
            openapi: serde_json::json!({}),
        }
    }

    #[test]
    fn clean_tree_passes() {
        let b = backend_with(&[("app/main.py", "def f():\n    return 1\n")]);
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["security-scan".to_string()],
                block_on: vec!["security".to_string()],
            },
            &b,
            &available_gates(),
        );
        assert!(
            report.passed(),
            "clean tree must pass: {:?}",
            report.findings
        );
    }

    #[test]
    fn insecure_pattern_blocks_when_security_blocks_on() {
        let b = backend_with(&[("app/api/x.py", "def h():\n    return eval(\"1+1\")\n")]);
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["security-scan".to_string()],
                block_on: vec!["security".to_string()],
            },
            &b,
            &available_gates(),
        );
        assert!(!report.passed(), "eval() must block");
        assert_eq!(report.blocking.len(), 1);
        assert_eq!(report.blocking[0].category, "security");
    }

    #[test]
    fn advisory_when_not_blocked_on() {
        let b = backend_with(&[("app/api/x.py", "def h():\n    return eval(\"1+1\")\n")]);
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["security-scan".to_string()],
                block_on: vec![], // nothing blocks → advisory only
            },
            &b,
            &available_gates(),
        );
        assert!(report.passed(), "no block_on → advisory only");
        assert_eq!(report.advisory.len(), 1);
    }

    #[test]
    fn unimplemented_skill_is_deferred_not_faked() {
        let b = backend_with(&[("app/main.py", "x = 1\n")]);
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["code-reviewer".to_string()],
                block_on: vec![],
            },
            &b,
            &available_gates(),
        );
        assert_eq!(report.deferred_gates, vec!["code-reviewer".to_string()]);
    }
}
