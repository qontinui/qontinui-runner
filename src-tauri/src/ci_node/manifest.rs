//! `.qontinui/ci.toml` manifest — the ONLY source of executed commands
//! (plan §4.1: coord's dispatch carries no commands; the manifest is read
//! from the checked-out tree at the dispatched SHA and validated strictly).

use serde::Deserialize;
use std::collections::BTreeMap;

/// Default per-step timeout (1h) when a step does not specify one.
pub(crate) const DEFAULT_STEP_TIMEOUT_SECS: u64 = 3_600;
/// Hard cap on any step's timeout (2h) — a larger value is a validation
/// error, not a clamp, so authors see the limit instead of silently losing
/// budget.
pub(crate) const MAX_STEP_TIMEOUT_SECS: u64 = 7_200;

/// Env vars a step may set. Deliberately a tight allowlist of build-tuning
/// knobs: nothing PATH/CARGO_HOME-class that could redirect binary or
/// toolchain resolution on the host.
const ENV_ALLOWLIST: &[&str] = &[
    "RUSTFLAGS",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_TERM_COLOR",
    "NODE_OPTIONS",
    "NODE_ENV",
    "CI",
    "QONTINUI_DISABLE_KEYCHAIN",
];

/// Shell metacharacters banned from command argv tokens. Commands are
/// executed as argv (never a shell string), but on Windows a `.cmd` shim
/// (pnpm) requires a `cmd.exe /C` respawn where these would become live —
/// banning them keeps that fallback injection-safe.
const ARGV_BANNED_CHARS: &[char] = &['&', '|', '<', '>', '^', '"', '\n', '\r', '\0', '%'];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiManifest {
    /// Manifest schema version — must be exactly 1.
    pub version: u32,
    pub steps: Vec<CiStep>,
    #[serde(default)]
    pub limits: CiLimits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiStep {
    pub name: String,
    /// Argv vector — `["cargo", "test"]`, NEVER a shell string.
    pub command: Vec<String>,
    /// Extra env for this step. Keys must be in the allowlist.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-step timeout; default [`DEFAULT_STEP_TIMEOUT_SECS`], max
    /// [`MAX_STEP_TIMEOUT_SECS`].
    pub timeout_secs: Option<u64>,
    /// Repo-relative working dir (e.g. `src-tauri`). Must not escape the
    /// worktree — validated structurally here AND canonicalize+prefix-checked
    /// at execution time.
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiLimits {
    /// `CARGO_BUILD_JOBS` exported to every step (default 1 — this
    /// workspace OOMs above that; the limit wins over a step's own env).
    pub cargo_build_jobs: Option<u32>,
}

impl CiStep {
    pub(crate) fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_STEP_TIMEOUT_SECS)
    }
}

impl CiLimits {
    pub(crate) fn effective_cargo_build_jobs(&self) -> u32 {
        self.cargo_build_jobs.unwrap_or(1).max(1)
    }
}

/// Parse + validate a manifest body. All failures are `Err(String)` with an
/// author-actionable message (they end up in the dispatch's log tail).
pub(crate) fn parse_and_validate(text: &str) -> Result<CiManifest, String> {
    let manifest: CiManifest =
        toml::from_str(text).map_err(|e| format!("ci.toml parse error: {e}"))?;
    if manifest.version != 1 {
        return Err(format!(
            "ci.toml version {} unsupported (this runner supports version = 1)",
            manifest.version
        ));
    }
    if manifest.steps.is_empty() {
        return Err("ci.toml has no [[steps]] — nothing to run".to_string());
    }
    for (i, step) in manifest.steps.iter().enumerate() {
        let label = if step.name.trim().is_empty() {
            format!("steps[{i}]")
        } else {
            format!("step '{}'", step.name)
        };
        if step.name.trim().is_empty() {
            return Err(format!("{label}: name must be non-empty"));
        }
        if step.command.is_empty() || step.command[0].trim().is_empty() {
            return Err(format!("{label}: command must be a non-empty argv array"));
        }
        for token in &step.command {
            if token.contains(ARGV_BANNED_CHARS) {
                return Err(format!(
                    "{label}: command token {token:?} contains a banned shell metacharacter"
                ));
            }
        }
        for key in step.env.keys() {
            if !ENV_ALLOWLIST.contains(&key.as_str()) {
                return Err(format!(
                    "{label}: env var {key:?} is not allowlisted (allowed: {ENV_ALLOWLIST:?})"
                ));
            }
        }
        if let Some(t) = step.timeout_secs {
            if t == 0 || t > MAX_STEP_TIMEOUT_SECS {
                return Err(format!(
                    "{label}: timeout_secs {t} out of range (1..={MAX_STEP_TIMEOUT_SECS})"
                ));
            }
        }
        if let Some(wd) = &step.working_dir {
            validate_working_dir(wd).map_err(|e| format!("{label}: {e}"))?;
        }
    }
    Ok(manifest)
}

/// Structural check that a working_dir stays inside the worktree: relative,
/// no parent/root/prefix components. Execution additionally canonicalizes
/// and prefix-checks against the real worktree path.
fn validate_working_dir(wd: &str) -> Result<(), String> {
    let path = std::path::Path::new(wd);
    // The `:` check rejects Windows drive-qualified paths (`C:\x`, `C:x`)
    // even when this code runs on a non-Windows host (cross-platform tests).
    if path.is_absolute() || wd.starts_with('/') || wd.starts_with('\\') || wd.contains(':') {
        return Err(format!("working_dir {wd:?} must be repo-relative"));
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "working_dir {wd:?} must not contain parent/root components"
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
version = 1

[limits]
cargo_build_jobs = 1

[[steps]]
name = "fmt"
command = ["cargo", "fmt", "--", "--check"]
working_dir = "src-tauri"

[[steps]]
name = "test"
command = ["cargo", "test"]
working_dir = "src-tauri"
env = { QONTINUI_DISABLE_KEYCHAIN = "1" }
timeout_secs = 7200
"#;

    #[test]
    fn valid_manifest_parses() {
        let m = parse_and_validate(VALID).expect("valid manifest must parse");
        assert_eq!(m.version, 1);
        assert_eq!(m.steps.len(), 2);
        assert_eq!(m.limits.effective_cargo_build_jobs(), 1);
        assert_eq!(
            m.steps[0].effective_timeout_secs(),
            DEFAULT_STEP_TIMEOUT_SECS
        );
        assert_eq!(m.steps[1].effective_timeout_secs(), 7200);
    }

    #[test]
    fn empty_steps_rejected() {
        let err = parse_and_validate("version = 1\nsteps = []\n").unwrap_err();
        assert!(err.contains("no [[steps]]"), "got: {err}");
    }

    #[test]
    fn wrong_version_rejected() {
        let err =
            parse_and_validate("version = 2\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n")
                .unwrap_err();
        assert!(err.contains("version 2 unsupported"), "got: {err}");
    }

    #[test]
    fn unknown_fields_rejected() {
        let err = parse_and_validate(
            "version = 1\nsurprise = true\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nshell = true\n",
        )
        .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
    }

    #[test]
    fn non_array_command_rejected() {
        // A shell-string command is a TYPE error under the argv contract.
        let err =
            parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = \"cargo test\"\n")
                .unwrap_err();
        assert!(err.contains("parse error"), "got: {err}");
    }

    #[test]
    fn empty_command_rejected() {
        let err =
            parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = []\n").unwrap_err();
        assert!(err.contains("non-empty argv"), "got: {err}");
    }

    #[test]
    fn shell_metacharacters_in_argv_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"cargo\", \"test\", \"&& del /f\"]\n",
        )
        .unwrap_err();
        assert!(err.contains("banned shell metacharacter"), "got: {err}");
    }

    #[test]
    fn env_outside_allowlist_rejected() {
        for key in ["PATH", "CARGO_HOME", "RUSTUP_HOME", "LD_PRELOAD"] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nenv = {{ {key} = \"evil\" }}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(err.contains("not allowlisted"), "{key}: got {err}");
        }
    }

    #[test]
    fn oversize_and_zero_timeouts_rejected() {
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\ntimeout_secs = 7201\n",
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
        let err = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\ntimeout_secs = 0\n",
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    #[test]
    fn working_dir_escape_rejected() {
        for wd in ["../sibling", "a/../../b", "/abs", "\\abs", "C:\\abs"] {
            let text = format!(
                "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nworking_dir = {wd:?}\n"
            );
            let err = parse_and_validate(&text).unwrap_err();
            assert!(
                err.contains("working_dir"),
                "{wd}: expected working_dir rejection, got {err}"
            );
        }
        // A benign nested relative dir passes.
        let ok = parse_and_validate(
            "version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\nworking_dir = \"src-tauri\"\n",
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn limits_default_to_one_job() {
        let m = parse_and_validate("version = 1\n[[steps]]\nname = \"x\"\ncommand = [\"true\"]\n")
            .unwrap();
        assert_eq!(m.limits.effective_cargo_build_jobs(), 1);
    }
}
