//! Package-manager resolution for the install-effects producer:
//! token parsing (with the explicit `pipenv` rejection), autodetect from a
//! repo checkout, and install-argv construction.
//!
//! Ecosystem policy (plan decision 4): npm/pnpm + cargo + pip are first-class;
//! yarn/poetry are accepted-but-degraded (valid coord variants, passed
//! through). `pipenv` is NOT a coord variant — it is rejected explicitly
//! rather than mislabeled as pip.

use std::path::Path;
use std::process::Command;

use super::registry_creds::RegistryEnv;
use super::types::{PackageManager, PackageSpecInput};
use crate::pm_detect::pm_command;

/// Error resolving the package manager (a 400 to the route caller).
#[derive(Debug, thiserror::Error)]
pub enum PmError {
    #[error(
        "unsupported package_manager '{0}': coord install-effect signatures \
         cover npm, yarn, pnpm, cargo, pip, poetry. 'pipenv' is NOT a coord \
         variant — it is rejected rather than mislabeled as pip; use pip with \
         a requirements file instead"
    )]
    Unsupported(String),
    #[error("could not autodetect a package manager in '{0}' — pass `package_manager` explicitly (one of: npm, yarn, pnpm, cargo, pip, poetry)")]
    Undetectable(String),
}

/// Parse an explicit `package_manager` token into the coord enum. Rejects
/// `pipenv` (and any other non-coord token) with [`PmError::Unsupported`].
pub fn parse_token(token: &str) -> Result<PackageManager, PmError> {
    match token.trim().to_ascii_lowercase().as_str() {
        "npm" => Ok(PackageManager::Npm),
        "yarn" => Ok(PackageManager::Yarn),
        "pnpm" => Ok(PackageManager::Pnpm),
        "cargo" => Ok(PackageManager::Cargo),
        "pip" => Ok(PackageManager::Pip),
        "poetry" => Ok(PackageManager::Poetry),
        other => Err(PmError::Unsupported(other.to_string())),
    }
}

/// Autodetect the package manager from a repo checkout by sniffing
/// lockfiles/manifests. Order is deterministic and lockfile-first (a lockfile
/// is the strongest signal of which tool owns the repo):
/// `pnpm-lock.yaml` → pnpm, `yarn.lock` → yarn, `package-lock.json` → npm,
/// any `package.json` → npm (default Node), `Cargo.toml` → cargo,
/// `poetry.lock`/`pyproject.toml[tool.poetry]` → poetry, `requirements*.txt`/
/// `setup.py`/`pyproject.toml` → pip.
pub fn autodetect(repo_path: &Path) -> Result<PackageManager, PmError> {
    let has = |name: &str| repo_path.join(name).exists();

    // ---- Node — lockfile first, then a bare package.json --------------------
    if has("pnpm-lock.yaml") {
        return Ok(PackageManager::Pnpm);
    }
    if has("yarn.lock") {
        return Ok(PackageManager::Yarn);
    }
    if has("package-lock.json") || has("npm-shrinkwrap.json") {
        return Ok(PackageManager::Npm);
    }
    if has("package.json") {
        return Ok(PackageManager::Npm);
    }

    // ---- Rust ---------------------------------------------------------------
    if has("Cargo.toml") {
        return Ok(PackageManager::Cargo);
    }

    // ---- Python — poetry vs pip --------------------------------------------
    if has("poetry.lock") || pyproject_is_poetry(repo_path) {
        return Ok(PackageManager::Poetry);
    }
    if has("requirements.txt")
        || has("requirements-dev.txt")
        || has("setup.py")
        || has("setup.cfg")
        || has("pyproject.toml")
    {
        return Ok(PackageManager::Pip);
    }

    Err(PmError::Undetectable(repo_path.display().to_string()))
}

/// True iff `pyproject.toml` declares a `[tool.poetry]` table (poetry-managed
/// rather than PEP-621/pip). Best-effort substring sniff — a false negative
/// just degrades to pip, which is the safe default.
fn pyproject_is_poetry(repo_path: &Path) -> bool {
    let p = repo_path.join("pyproject.toml");
    match std::fs::read_to_string(p) {
        Ok(text) => text.contains("[tool.poetry]"),
        Err(_) => false,
    }
}

/// Build the install subprocess command (program + args) for `pm` in
/// `repo_path`. Uses [`crate::pm_detect::pm_command`] so Windows `.cmd` shims
/// resolve and `CREATE_NO_WINDOW` is applied. An empty `packages` set is a
/// lockfile-only refresh (`npm install`, `cargo update`, `pip install -r
/// requirements.txt` is NOT assumed — pip with no packages is a no-op-ish
/// `pip install`, left to the caller).
///
/// Returns the [`Command`] plus the argv as `Vec<String>` for the response's
/// audit trail.
pub fn build_install_command(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[PackageSpecInput],
    dev: bool,
    env: &RegistryEnv,
) -> (Command, Vec<String>) {
    let specs: Vec<String> = packages.iter().map(|p| format_spec(pm, p)).collect();
    let (program, args): (&str, Vec<String>) = match pm {
        PackageManager::Npm | PackageManager::Pnpm | PackageManager::Yarn => {
            let program = pm.as_str();
            let mut args = vec!["install".to_string()];
            if !specs.is_empty() {
                args.extend(specs.iter().cloned());
                if dev {
                    // npm/pnpm: `-D`; yarn classic: `--dev`. `-D` is accepted
                    // by all three for `add`, but for `install <pkg>` npm uses
                    // `--save-dev`. Use the long form for portability.
                    args.push("--save-dev".to_string());
                }
            }
            (program, args)
        }
        PackageManager::Cargo => {
            // `cargo add <spec>` for explicit packages; `cargo update` for a
            // lockfile-only refresh.
            if specs.is_empty() {
                ("cargo", vec!["update".to_string()])
            } else {
                let mut args = vec!["add".to_string()];
                args.extend(specs.iter().cloned());
                if dev {
                    args.push("--dev".to_string());
                }
                ("cargo", args)
            }
        }
        PackageManager::Pip => {
            let mut args = vec!["install".to_string()];
            args.extend(specs.iter().cloned());
            ("pip", args)
        }
        PackageManager::Poetry => {
            // `poetry add <spec>`; bare `poetry install` for a refresh.
            if specs.is_empty() {
                ("poetry", vec!["install".to_string()])
            } else {
                let mut args = vec!["add".to_string()];
                args.extend(specs.iter().cloned());
                if dev {
                    args.push("--group".to_string());
                    args.push("dev".to_string());
                }
                ("poetry", args)
            }
        }
    };

    let mut cmd = pm_command(program);
    cmd.current_dir(repo_path);
    cmd.args(&args);
    // Inject private-registry auth (Phase 4). Empty env ⇒ no-op.
    env.apply(&mut cmd);

    let mut argv = vec![program.to_string()];
    argv.extend(args);
    (cmd, argv)
}

/// The dry-run probe shape for an ecosystem: the subprocess command plus which
/// captured stream the parser reads. npm/pip emit their machine-readable plan
/// on **stdout**; `cargo add --dry-run` writes its plan to **stderr**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRunStream {
    Stdout,
    Stderr,
    /// pnpm has no single-shot machine-readable dry-run; the producer captures
    /// the lockfile before & after a `--lockfile-only` mutation and diffs them.
    /// No single stream is parsed.
    LockfileDiff,
}

/// Build the **dry-run** subprocess command for `pm` in `repo_path` for the
/// requested `packages`, plus the stream the parser should read. Returns `None`
/// for managers with no first-class dry-run probe we parse (yarn / poetry ⇒
/// honest absent dry-run; coord treats the coverage gap as `Unknown`).
///
/// Probes (all read-only / lockfile-only — they never touch the real install):
/// - npm:   `npm install <specs> --dry-run --json`            (stdout JSON)
/// - pnpm:  `pnpm add <specs> --lockfile-only`                (lockfile diff)
/// - cargo: `cargo add <specs> --dry-run` (or `--dry-run` update on refresh)   (stderr text)
/// - pip:   `pip install <specs> --dry-run --report -`        (stdout JSON)
///
/// An empty `packages` set is a lockfile-only refresh; npm/pnpm still probe
/// (`npm install --dry-run`), cargo/pip with no specs have no useful add probe
/// and return `None` (the refresh effect is observed post-install in Phase 3).
pub fn build_dry_run_command(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[PackageSpecInput],
    dev: bool,
    env: &RegistryEnv,
) -> Option<(Command, Vec<String>, DryRunStream)> {
    let specs: Vec<String> = packages.iter().map(|p| format_spec(pm, p)).collect();
    let (program, args, stream): (&str, Vec<String>, DryRunStream) = match pm {
        PackageManager::Npm => {
            let mut args = vec!["install".to_string()];
            args.extend(specs.iter().cloned());
            if dev && !specs.is_empty() {
                args.push("--save-dev".to_string());
            }
            args.push("--dry-run".to_string());
            args.push("--json".to_string());
            ("npm", args, DryRunStream::Stdout)
        }
        PackageManager::Pnpm => {
            // `pnpm add … --lockfile-only` mutates only pnpm-lock.yaml; we diff
            // it before/after. With no specs it's a lockfile refresh probe.
            let mut args = if specs.is_empty() {
                vec!["install".to_string(), "--lockfile-only".to_string()]
            } else {
                let mut a = vec!["add".to_string()];
                a.extend(specs.iter().cloned());
                a.push("--lockfile-only".to_string());
                a
            };
            if dev && !specs.is_empty() {
                args.push("--save-dev".to_string());
            }
            ("pnpm", args, DryRunStream::LockfileDiff)
        }
        PackageManager::Cargo => {
            if specs.is_empty() {
                // `cargo update --dry-run` refresh — its plan is verify-side.
                return None;
            }
            let mut args = vec!["add".to_string()];
            args.extend(specs.iter().cloned());
            if dev {
                args.push("--dev".to_string());
            }
            args.push("--dry-run".to_string());
            ("cargo", args, DryRunStream::Stderr)
        }
        PackageManager::Pip => {
            if specs.is_empty() {
                return None;
            }
            let mut args = vec!["install".to_string()];
            args.extend(specs.iter().cloned());
            args.push("--dry-run".to_string());
            args.push("--report".to_string());
            args.push("-".to_string());
            ("pip", args, DryRunStream::Stdout)
        }
        // yarn / poetry: no parsed dry-run probe (honest absent).
        PackageManager::Yarn | PackageManager::Poetry => return None,
    };

    let mut cmd = pm_command(program);
    cmd.current_dir(repo_path);
    cmd.args(&args);
    env.apply(&mut cmd);
    let mut argv = vec![program.to_string()];
    argv.extend(args);
    Some((cmd, argv, stream))
}

/// Build the **native-audit** subprocess command for `pm` in `repo_path`, plus
/// the stream the parser reads (all emit JSON on stdout). Returns `None` for
/// managers with no native auditor we parse (yarn / poetry ⇒ `security_audited`
/// stays false). The audit tool may be absent on the host — that is the
/// caller's concern (a spawn failure ⇒ `security_audited:false`), not this
/// builder's.
///
/// - npm:   `npm audit --json`
/// - pnpm:  `pnpm audit --json`   (npm-compatible output)
/// - cargo: `cargo audit --json`  (requires `cargo-audit` installed)
/// - pip:   `pip-audit -f json`   (requires `pip-audit` installed)
pub fn build_audit_command(
    pm: PackageManager,
    repo_path: &Path,
    env: &RegistryEnv,
) -> Option<(Command, Vec<String>)> {
    let (program, args): (&str, Vec<String>) = match pm {
        PackageManager::Npm => ("npm", vec!["audit".into(), "--json".into()]),
        PackageManager::Pnpm => ("pnpm", vec!["audit".into(), "--json".into()]),
        PackageManager::Cargo => ("cargo", vec!["audit".into(), "--json".into()]),
        PackageManager::Pip => ("pip-audit", vec!["-f".into(), "json".into()]),
        PackageManager::Yarn | PackageManager::Poetry => return None,
    };
    let mut cmd = pm_command(program);
    cmd.current_dir(repo_path);
    cmd.args(&args);
    env.apply(&mut cmd);
    let mut argv = vec![program.to_string()];
    argv.extend(args);
    Some((cmd, argv))
}

/// Render one `{name, version_req?}` spec into the ecosystem's CLI argument
/// form. The separator is NOT universal:
///
/// - npm / yarn / pnpm / cargo / poetry use `name@version_req`
///   (`left-pad@^1.3.0`, `cargo add serde@1`, `poetry add requests@^2.0`).
/// - pip has no `@`-version syntax — it wants a PEP-508 requirement
///   (`requests==2.0`, `requests>=2.0`). When `version_req` already opens with
///   a comparison operator (`==`, `>=`, `<=`, `~=`, `!=`, `>`, `<`) we splice
///   it verbatim (`requests` + `>=2.0`); a bare version (`2.0`) gets the
///   exact-pin `==` so `2.0` ⇒ `requests==2.0`.
///
/// An empty/absent `version_req` ⇒ the bare name in every ecosystem.
fn format_spec(pm: PackageManager, p: &PackageSpecInput) -> String {
    let v = match &p.version_req {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return p.name.clone(),
    };
    match pm {
        PackageManager::Pip => {
            // Already a PEP-508 specifier (operator-prefixed) ⇒ splice verbatim.
            const OPS: [&str; 7] = ["==", ">=", "<=", "~=", "!=", ">", "<"];
            if OPS.iter().any(|op| v.starts_with(op)) {
                format!("{}{}", p.name, v)
            } else {
                // Bare version ⇒ exact pin.
                format!("{}=={}", p.name, v)
            }
        }
        // npm / yarn / pnpm / cargo / poetry: `name@version_req`.
        _ => format!("{}@{}", p.name, v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Empty registry env for the builder tests (no private-registry creds).
    fn env() -> RegistryEnv {
        RegistryEnv::empty()
    }

    #[test]
    fn parse_token_accepts_all_coord_variants() {
        for (tok, pm) in [
            ("npm", PackageManager::Npm),
            ("YARN", PackageManager::Yarn),
            ("pnpm", PackageManager::Pnpm),
            ("Cargo", PackageManager::Cargo),
            ("pip", PackageManager::Pip),
            ("poetry", PackageManager::Poetry),
        ] {
            assert_eq!(parse_token(tok).unwrap(), pm);
        }
    }

    #[test]
    fn parse_token_rejects_pipenv_explicitly() {
        let err = parse_token("pipenv").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("pipenv"), "msg: {msg}");
        assert!(msg.contains("NOT a coord variant"), "msg: {msg}");
    }

    #[test]
    fn parse_token_rejects_unknown() {
        assert!(parse_token("conda").is_err());
    }

    #[test]
    fn autodetect_node_lockfiles() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("pnpm-lock.yaml"), "lockfileVersion: 6").unwrap();
        assert_eq!(autodetect(d.path()).unwrap(), PackageManager::Pnpm);

        let d2 = tempdir().unwrap();
        std::fs::write(d2.path().join("yarn.lock"), "").unwrap();
        assert_eq!(autodetect(d2.path()).unwrap(), PackageManager::Yarn);

        let d3 = tempdir().unwrap();
        std::fs::write(d3.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(autodetect(d3.path()).unwrap(), PackageManager::Npm);

        let d4 = tempdir().unwrap();
        std::fs::write(d4.path().join("package.json"), "{}").unwrap();
        assert_eq!(autodetect(d4.path()).unwrap(), PackageManager::Npm);
    }

    #[test]
    fn autodetect_cargo() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(autodetect(d.path()).unwrap(), PackageManager::Cargo);
    }

    #[test]
    fn autodetect_python_poetry_vs_pip() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"x\"",
        )
        .unwrap();
        assert_eq!(autodetect(d.path()).unwrap(), PackageManager::Poetry);

        let d2 = tempdir().unwrap();
        std::fs::write(d2.path().join("requirements.txt"), "requests==2.0").unwrap();
        assert_eq!(autodetect(d2.path()).unwrap(), PackageManager::Pip);

        // PEP-621 pyproject without [tool.poetry] ⇒ pip.
        let d3 = tempdir().unwrap();
        std::fs::write(d3.path().join("pyproject.toml"), "[project]\nname = \"x\"").unwrap();
        assert_eq!(autodetect(d3.path()).unwrap(), PackageManager::Pip);
    }

    #[test]
    fn autodetect_undetectable_errors() {
        let d = tempdir().unwrap();
        assert!(autodetect(d.path()).is_err());
    }

    #[test]
    fn build_install_command_lockfile_refresh_npm() {
        let d = tempdir().unwrap();
        let (_cmd, argv) = build_install_command(PackageManager::Npm, d.path(), &[], false, &env());
        assert_eq!(argv, vec!["npm", "install"]);
    }

    #[test]
    fn build_install_command_cargo_add_and_update() {
        let d = tempdir().unwrap();
        let pkgs = vec![PackageSpecInput {
            name: "serde".to_string(),
            version_req: Some("1".to_string()),
        }];
        let (_c, argv) =
            build_install_command(PackageManager::Cargo, d.path(), &pkgs, true, &env());
        assert_eq!(argv, vec!["cargo", "add", "serde@1", "--dev"]);

        let (_c2, argv2) =
            build_install_command(PackageManager::Cargo, d.path(), &[], false, &env());
        assert_eq!(argv2, vec!["cargo", "update"]);
    }

    #[test]
    fn build_install_command_npm_dev_flag() {
        let d = tempdir().unwrap();
        let pkgs = vec![PackageSpecInput {
            name: "left-pad".to_string(),
            version_req: None,
        }];
        let (_c, argv) = build_install_command(PackageManager::Npm, d.path(), &pkgs, true, &env());
        assert_eq!(argv, vec!["npm", "install", "left-pad", "--save-dev"]);
    }

    #[test]
    fn pip_spec_uses_pep508_separators_not_at() {
        let d = tempdir().unwrap();
        // Bare version ⇒ exact pin with `==` (NOT `requests@2.0`).
        let bare = vec![PackageSpecInput {
            name: "requests".to_string(),
            version_req: Some("2.0".to_string()),
        }];
        let (_c, argv) = build_install_command(PackageManager::Pip, d.path(), &bare, false, &env());
        assert_eq!(argv, vec!["pip", "install", "requests==2.0"]);

        // Operator-prefixed version ⇒ spliced verbatim.
        let ranged = vec![PackageSpecInput {
            name: "flask".to_string(),
            version_req: Some(">=2.0".to_string()),
        }];
        let (_c2, argv2) =
            build_install_command(PackageManager::Pip, d.path(), &ranged, false, &env());
        assert_eq!(argv2, vec!["pip", "install", "flask>=2.0"]);

        // No version ⇒ bare name.
        let nov = vec![PackageSpecInput {
            name: "rich".to_string(),
            version_req: None,
        }];
        let (_c3, argv3) =
            build_install_command(PackageManager::Pip, d.path(), &nov, false, &env());
        assert_eq!(argv3, vec!["pip", "install", "rich"]);
    }

    #[test]
    fn cargo_and_node_still_use_at_separator() {
        let d = tempdir().unwrap();
        let pkgs = vec![PackageSpecInput {
            name: "serde".to_string(),
            version_req: Some("1".to_string()),
        }];
        let (_c, argv) =
            build_install_command(PackageManager::Cargo, d.path(), &pkgs, false, &env());
        assert_eq!(argv, vec!["cargo", "add", "serde@1"]);
    }
}
