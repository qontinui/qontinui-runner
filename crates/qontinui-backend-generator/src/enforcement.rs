//! Fork D — the enforcement loop. The named claude-config skills in
//! [`qontinui_types::priorities_profile::EnforcementProfile`] (`code-reviewer`,
//! `security-scan`) run as post-generation gates between "generated" and "covered":
//! a finding whose category ∈ `block_on` (`security`) **blocks** (revision rejected,
//! finding fed back into the next codegen prompt); a finding outside `block_on` is
//! advisory (recorded, not fatal).
//!
//! ## What actually runs here (no longer a static stand-in)
//!
//! The headline gate is [`SubprocessSecurityGate`]: it **materializes the generated
//! backend to a temp dir and runs a real security scanner subprocess on it**
//! (`bandit -r <dir> -f json` — the exact static analyzer the claude-config
//! `security-scan` skill invokes for Python projects: `poetry run bandit -r .`),
//! parses the scanner's JSON findings into the existing [`EnforcementReport`] shape
//! (severity → fatal-if-`block_on`), and removes the temp dir afterwards. So the
//! blocking path is exercised against a *real* scan of the *real* generated source, not
//! a hand-rolled pattern list.
//!
//! The [`EnforcementGate`] trait is retained so a deterministic test double
//! ([`BuiltinStaticGate`]) stays available for CI (which has no scanner) and so the
//! loop can inject alternative gates. `code-reviewer` is wired *structurally* as an
//! advisory gate ([`SubprocessReviewGate`]) — it is an LLM skill (`claude -p
//! "/code-reviewer"`), non-deterministic and slow, so it is dispatched only when
//! explicitly enabled and otherwise recorded in `deferred_gates`, honestly flagged.

use std::path::Path;
use std::process::Command;

use qontinui_types::completeness_verdict::{CoverageGap, GapReason, SpecSection};
use qontinui_types::functional_spec::SpecProvenance;
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
    /// Gates named in `run` that were NOT executed here (e.g. the LLM `code-reviewer`
    /// skill when not enabled) — listed so the caller can honestly report what is
    /// structurally-wired vs actually-run.
    pub deferred_gates: Vec<String>,
}

impl EnforcementReport {
    /// True iff no blocking finding — the revision may be admitted to the verify phase.
    pub fn passed(&self) -> bool {
        self.blocking.is_empty()
    }

    /// A single human-readable summary of the blocking findings, suitable for feeding
    /// back into the next codegen prompt so the model can fix the vulnerability.
    pub fn blocking_feedback(&self) -> String {
        if self.blocking.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "The previous generated backend was REJECTED by the security gate. Fix every \
             finding below and re-emit the handler bodies WITHOUT introducing them again:\n",
        );
        for f in &self.blocking {
            out.push_str(&format!("- [{}] {}: {}\n", f.gate, f.file, f.detail));
        }
        out
    }
}

/// A gate that can scan a generated tree. The real subprocess gate
/// ([`SubprocessSecurityGate`]) and the deterministic test double
/// ([`BuiltinStaticGate`]) both implement this; the LLM `code-reviewer` skill is
/// dispatched to via the same interface.
pub trait EnforcementGate {
    /// The gate's name (matched against `EnforcementProfile.run`).
    fn name(&self) -> &str;
    /// Scan the tree, returning findings.
    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding>;
}

/// Run the enforcement gates declared by the profile over a generated backend, using the
/// **real subprocess scanner** for `security-scan`.
///
/// Any gate named in `run` without an executing adapter (e.g. `code-reviewer` when the
/// LLM gate is not enabled) is recorded in `deferred_gates` so the report is honest about
/// what executed. `block_on` decides which findings are fatal.
pub fn run_enforcement(profile: &Profile, backend: &GeneratedBackend) -> EnforcementReport {
    let enforcement = profile.enforcement.clone().unwrap_or_default();
    run_enforcement_with(&enforcement, backend, &default_gates())
}

/// Lower-level entry: run an explicit gate set. Lets the loop (or a test) inject the real
/// subprocess gate or a deterministic double.
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
                // Named in `run` but no executing adapter — structurally wired, not run.
                report.deferred_gates.push(requested.clone());
            }
        }
    }
    report
}

/// The gates the live loop runs by default: the **real** subprocess security scanner
/// registered under the `security-scan` name. `code-reviewer` is intentionally NOT in
/// this default set (it is an LLM skill — see [`SubprocessReviewGate`]); it falls through
/// to `deferred_gates` unless the caller opts in via [`gates_with_review`].
pub fn default_gates() -> Vec<Box<dyn EnforcementGate>> {
    vec![Box::new(SubprocessSecurityGate::default())]
}

/// Default gates plus the advisory `code-reviewer` LLM gate, for callers that want it run.
pub fn gates_with_review(claude_bin: &str) -> Vec<Box<dyn EnforcementGate>> {
    vec![
        Box::new(SubprocessSecurityGate::default()),
        Box::new(SubprocessReviewGate {
            name: "code-reviewer".to_string(),
            claude_bin: claude_bin.to_string(),
        }),
    ]
}

// ===========================================================================
// The REAL gate: materialize → run scanner subprocess → parse → clean up
// ===========================================================================

/// A real security gate that runs an external scanner subprocess on the *materialized*
/// generated backend. Defaults to `bandit` (the static Python analyzer the claude-config
/// `security-scan` skill invokes: `poetry run bandit -r .`), invoked as
/// `bandit -r <dir> -f json`.
///
/// Registered under the `security-scan` name so a profile that requests `security-scan`
/// gets a genuine scan. The scan:
/// 1. writes the [`GeneratedBackend`] file tree to a fresh temp dir,
/// 2. runs the scanner subprocess on that dir,
/// 3. parses its JSON findings (severity-mapped) into [`Finding`]s with category
///    `security`,
/// 4. removes the temp dir.
pub struct SubprocessSecurityGate {
    /// The gate name (matched against `EnforcementProfile.run`). Default `security-scan`.
    pub name: String,
    /// The scanner executable. Default `bandit`.
    pub scanner_bin: String,
    /// Bandit test-ids to skip (e.g. `B101 assert_used`, which fires on every pytest
    /// `assert` and is NOT a production vulnerability). Default skips `B101`; the test
    /// suite is additionally excluded by path. Application-code vulnerabilities (SQLi,
    /// hardcoded secrets, etc.) are unaffected.
    pub skip_test_ids: Vec<String>,
    /// Glob fragments whose matching files are excluded from the scan (bandit `--exclude`).
    /// Default excludes the generated `tests/` tree — a security gate audits the
    /// *application* code it ships, not its own test asserts.
    pub exclude_globs: Vec<String>,
}

impl Default for SubprocessSecurityGate {
    fn default() -> Self {
        Self {
            name: "security-scan".to_string(),
            scanner_bin: "bandit".to_string(),
            skip_test_ids: vec!["B101".to_string()],
            exclude_globs: vec!["*/tests/*".to_string(), "*/conftest.py".to_string()],
        }
    }
}

impl EnforcementGate for SubprocessSecurityGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding> {
        match self.scan_real(backend) {
            Ok(findings) => findings,
            Err(e) => {
                // The scanner could not be run (not installed / spawn failure). Do NOT
                // fake a block and do NOT silently pass: surface the failure as a
                // diagnostic finding the caller must handle. It is categorized `security`
                // so a `block_on:[security]` profile treats an un-runnable scanner as a
                // blocker (fail closed), never a green light.
                vec![Finding {
                    gate: self.name.clone(),
                    category: "security".to_string(),
                    file: String::new(),
                    detail: format!("security scanner did not run: {e}"),
                }]
            }
        }
    }
}

impl SubprocessSecurityGate {
    /// Run the real scan, returning an error only when the scanner could not be invoked
    /// at all (so the caller can distinguish "scanner missing" from "scanner found
    /// nothing").
    fn scan_real(&self, backend: &GeneratedBackend) -> Result<Vec<Finding>, String> {
        let dir = TempDir::new("backgen-scan").map_err(|e| format!("temp dir: {e}"))?;
        backend
            .materialize_to(dir.path())
            .map_err(|e| format!("materialize: {e}"))?;

        // bandit -r <dir> -f json [--skip <ids>] [--exclude <globs>]. Exit code is
        // nonzero when findings exist (that is expected and NOT an error); we only treat a
        // missing executable / no-stdout as an invocation failure. Bandit writes the JSON
        // report to stdout. We skip B101 (pytest `assert`) and exclude the test tree so
        // the gate audits application code for real vulnerabilities, not test asserts.
        let mut cmd = Command::new(&self.scanner_bin);
        cmd.arg("-r").arg(dir.path()).arg("-f").arg("json");
        if !self.skip_test_ids.is_empty() {
            cmd.arg("--skip").arg(self.skip_test_ids.join(","));
        }
        if !self.exclude_globs.is_empty() {
            cmd.arg("--exclude").arg(self.exclude_globs.join(","));
        }
        let output = cmd
            .output()
            .map_err(|e| format!("spawn {}: {e}", self.scanner_bin))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Err(format!(
                "{} produced no JSON (stderr: {})",
                self.scanner_bin,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let findings = parse_bandit_json(&self.name, &stdout, dir.path())
            .map_err(|e| format!("parse {} output: {e}", self.scanner_bin))?;
        // dir is removed on drop.
        Ok(findings)
    }
}

/// Parse `bandit -f json` output into [`Finding`]s. Every bandit result becomes a
/// `security`-category finding; the path is relativized against the scanned root so the
/// finding's `file` matches the generated tree's relative path.
pub fn parse_bandit_json(gate: &str, json: &str, root: &Path) -> Result<Vec<Finding>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "no `results` array in bandit output".to_string())?;

    let mut findings = Vec::new();
    for r in results {
        let abs = r.get("filename").and_then(|f| f.as_str()).unwrap_or("");
        let rel = Path::new(abs)
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| abs.replace('\\', "/"));
        let test_id = r.get("test_id").and_then(|t| t.as_str()).unwrap_or("?");
        let test_name = r.get("test_name").and_then(|t| t.as_str()).unwrap_or("");
        let severity = r
            .get("issue_severity")
            .and_then(|s| s.as_str())
            .unwrap_or("UNDEFINED");
        let confidence = r
            .get("issue_confidence")
            .and_then(|s| s.as_str())
            .unwrap_or("UNDEFINED");
        let line = r.get("line_number").and_then(|l| l.as_u64()).unwrap_or(0);
        let text = r
            .get("issue_text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim();
        findings.push(Finding {
            gate: gate.to_string(),
            category: "security".to_string(),
            file: rel,
            detail: format!(
                "{test_id} {test_name} (severity {severity}, confidence {confidence}, line {line}): {text}"
            ),
        });
    }
    Ok(findings)
}

// ===========================================================================
// Advisory LLM gate (code-reviewer) — structurally wired, opt-in
// ===========================================================================

/// The `code-reviewer` claude-config skill, dispatched as an advisory subprocess
/// (`claude -p "/code-reviewer ..."`). Non-deterministic and slow, so it is only in the
/// gate set when the caller explicitly opts in ([`gates_with_review`]); otherwise the
/// gate name falls through to `deferred_gates`. Findings are category `review` (advisory
/// unless a profile blocks on `review`).
pub struct SubprocessReviewGate {
    pub name: String,
    pub claude_bin: String,
}

impl EnforcementGate for SubprocessReviewGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding> {
        let dir = match TempDir::new("backgen-review") {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        if backend.materialize_to(dir.path()).is_err() {
            return Vec::new();
        }
        let prompt = format!(
            "/code-reviewer Review the generated Python backend under {} for correctness, \
             security, and best-practice issues. List each issue on its own line prefixed \
             with FINDING:.",
            dir.path().display()
        );
        let output = Command::new(&self.claude_bin)
            .arg("-p")
            .arg(&prompt)
            .arg("--add-dir")
            .arg(dir.path())
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .filter_map(|l| l.trim().strip_prefix("FINDING:"))
            .map(|detail| Finding {
                gate: self.name.clone(),
                category: "review".to_string(),
                file: String::new(),
                detail: detail.trim().to_string(),
            })
            .collect()
    }
}

// ===========================================================================
// Feedback loop: blocker → re-dispatch CoverageGap
// ===========================================================================

/// Turn an unresolved blocking enforcement report into a [`CoverageGap`] work-list item,
/// so an enforcement blocker that survived the bounded codegen retries surfaces as a
/// **re-dispatch signal** (the reconciler re-generates the affected operation) rather
/// than a silent pass.
///
/// `op_ref` is the dotted spec ref of the operation whose handler is being gated
/// (e.g. `"operations.pairConfirm"`). Returns `None` when nothing blocks.
pub fn unresolved_blocker_gap(report: &EnforcementReport, op_ref: &str) -> Option<CoverageGap> {
    if report.passed() {
        return None;
    }
    let detail = report
        .blocking
        .iter()
        .map(|f| f.detail.clone())
        .collect::<Vec<_>>()
        .join("; ");
    Some(CoverageGap {
        r#ref: op_ref.to_string(),
        section: SpecSection::Operations,
        node_provenance: SpecProvenance::Observed,
        reason: GapReason::BehaviorMismatch,
        detail: Some(format!(
            "enforcement gate blocked after bounded retries: {detail}"
        )),
    })
}

/// The default bound on codegen feedback retries when a security blocker is found.
pub const DEFAULT_ENFORCEMENT_RETRIES: u32 = 2;

/// Drive the enforcement feedback loop over a single operation's generated handler.
///
/// `regenerate` is the codegen callback: given the previous report's blocking feedback
/// (empty on the first attempt) it must return a fresh [`GeneratedBackend`] revision. The
/// loop runs the configured gates after each attempt; a clean report admits the revision;
/// a blocking report feeds [`EnforcementReport::blocking_feedback`] into the next
/// `regenerate` call, up to `max_retries`. On exhaustion the final report is returned
/// still-blocking so the caller can call [`unresolved_blocker_gap`] — never a silent pass.
pub fn enforce_with_feedback<F>(
    enforcement: &EnforcementProfile,
    gates: &[Box<dyn EnforcementGate>],
    max_retries: u32,
    mut regenerate: F,
) -> (GeneratedBackend, EnforcementReport)
where
    F: FnMut(&str) -> GeneratedBackend,
{
    let mut feedback = String::new();
    let mut backend = regenerate(&feedback);
    let mut report = run_enforcement_with(enforcement, &backend, gates);

    let mut attempts = 0;
    while !report.passed() && attempts < max_retries {
        feedback = report.blocking_feedback();
        backend = regenerate(&feedback);
        report = run_enforcement_with(enforcement, &backend, gates);
        attempts += 1;
    }
    (backend, report)
}

// ===========================================================================
// Deterministic test double (CI has no scanner) + tiny temp-dir helper
// ===========================================================================

/// A deterministic, self-contained static scan, used as a **test double** so the
/// enforcement loop's blocking/advisory logic is unit-testable in CI (which has no
/// scanner subprocess). It implements [`EnforcementGate`] exactly like the real gate but
/// flags a fixed pattern set in-process. The *real* coverage of the scan path is the
/// `#[ignore]`d gated test in `tests/enforcement_subprocess.rs`, which runs the actual
/// `bandit` subprocess.
pub struct BuiltinStaticGate {
    pub name: String,
}

impl EnforcementGate for BuiltinStaticGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn scan(&self, backend: &GeneratedBackend) -> Vec<Finding> {
        builtin_static_checks(&self.name, backend)
    }
}

/// The double's checks: flags `eval(`/`exec(`, a hard-coded password literal, and
/// `DEBUG = True`. Category `security` so it hits the `block_on` path.
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

/// A minimal RAII temp directory (no external crate): creates a uniquely-named dir under
/// the OS temp dir and removes it (recursively) on drop. Enough for materialize→scan→clean.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{n}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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

    fn double_gates() -> Vec<Box<dyn EnforcementGate>> {
        vec![Box::new(BuiltinStaticGate {
            name: "security-scan".to_string(),
        })]
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
            &double_gates(),
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
            &double_gates(),
        );
        assert!(!report.passed(), "eval() must block");
        assert_eq!(report.blocking.len(), 1);
        assert_eq!(report.blocking[0].category, "security");
        assert!(report.blocking_feedback().contains("REJECTED"));
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
            &double_gates(),
        );
        assert!(report.passed(), "no block_on → advisory only");
        assert_eq!(report.advisory.len(), 1);
    }

    #[test]
    fn unrun_skill_is_deferred_not_faked() {
        let b = backend_with(&[("app/main.py", "x = 1\n")]);
        // code-reviewer is not in the default gate set → deferred, not faked.
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["code-reviewer".to_string()],
                block_on: vec![],
            },
            &b,
            &default_gates(),
        );
        assert_eq!(report.deferred_gates, vec!["code-reviewer".to_string()]);
    }

    #[test]
    fn unresolved_blocker_becomes_redispatch_gap() {
        let b = backend_with(&[("app/api/x.py", "def h():\n    return eval(\"1+1\")\n")]);
        let report = run_enforcement_with(
            &EnforcementProfile {
                run: vec!["security-scan".to_string()],
                block_on: vec!["security".to_string()],
            },
            &b,
            &double_gates(),
        );
        let gap = unresolved_blocker_gap(&report, "operations.pairConfirm")
            .expect("blocking report yields a gap");
        assert_eq!(gap.r#ref, "operations.pairConfirm");
        assert_eq!(gap.section, SpecSection::Operations);
        assert!(matches!(gap.reason, GapReason::BehaviorMismatch));
        assert!(unresolved_blocker_gap(&EnforcementReport::default(), "x").is_none());
    }

    #[test]
    fn feedback_loop_retries_then_recovers() {
        // First attempt is vulnerable; the second (after feedback) is clean.
        let enforcement = EnforcementProfile {
            run: vec!["security-scan".to_string()],
            block_on: vec!["security".to_string()],
        };
        let mut calls = 0;
        let (_, report) = enforce_with_feedback(
            &enforcement,
            &double_gates(),
            DEFAULT_ENFORCEMENT_RETRIES,
            |feedback| {
                calls += 1;
                if calls == 1 {
                    assert!(feedback.is_empty(), "first attempt has no feedback");
                    backend_with(&[("app/api/x.py", "def h():\n    return eval(\"1\")\n")])
                } else {
                    assert!(feedback.contains("REJECTED"), "retry gets feedback");
                    backend_with(&[("app/api/x.py", "def h():\n    return 1\n")])
                }
            },
        );
        assert!(report.passed(), "loop recovers after feedback");
        assert_eq!(calls, 2, "exactly one retry");
    }

    #[test]
    fn feedback_loop_exhausts_to_blocking_gap_never_silent() {
        // Always-vulnerable: the loop must end still-blocking (re-dispatch), not pass.
        let enforcement = EnforcementProfile {
            run: vec!["security-scan".to_string()],
            block_on: vec!["security".to_string()],
        };
        let mut calls = 0;
        let (_, report) = enforce_with_feedback(&enforcement, &double_gates(), 2, |_| {
            calls += 1;
            backend_with(&[("app/api/x.py", "def h():\n    return eval(\"1\")\n")])
        });
        assert!(!report.passed(), "exhausted loop must remain blocking");
        assert_eq!(calls, 3, "1 initial + 2 retries");
        assert!(unresolved_blocker_gap(&report, "operations.pairConfirm").is_some());
    }

    #[test]
    fn bandit_json_parses_into_security_findings() {
        // A representative bandit -f json payload (B608 SQL-injection + B105 password).
        let json = r#"{
          "results": [
            {"filename": "/tmp/scan/app/api/x.py", "test_id": "B608",
             "test_name": "hardcoded_sql_expressions", "issue_severity": "MEDIUM",
             "issue_confidence": "MEDIUM", "line_number": 7,
             "issue_text": "Possible SQL injection vector through string-based query construction."},
            {"filename": "/tmp/scan/app/api/x.py", "test_id": "B105",
             "test_name": "hardcoded_password_string", "issue_severity": "LOW",
             "issue_confidence": "MEDIUM", "line_number": 2,
             "issue_text": "Possible hardcoded password: 'hunter2'"}
          ]
        }"#;
        let findings = parse_bandit_json("security-scan", json, Path::new("/tmp/scan")).unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|f| f.category == "security"));
        assert_eq!(findings[0].file, "app/api/x.py");
        assert!(findings[0].detail.contains("B608"));
        assert!(findings[1].detail.contains("B105"));
    }

    #[test]
    fn missing_scanner_fails_closed_not_open() {
        // A scanner binary that cannot exist → the gate emits a blocking diagnostic
        // finding (fail closed), never a silent pass.
        let gate = SubprocessSecurityGate {
            scanner_bin: "definitely-not-a-real-scanner-xyz".to_string(),
            ..SubprocessSecurityGate::default()
        };
        let b = backend_with(&[("app/main.py", "x = 1\n")]);
        let findings = gate.scan(&b);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "security");
        assert!(findings[0].detail.contains("did not run"));
    }
}
