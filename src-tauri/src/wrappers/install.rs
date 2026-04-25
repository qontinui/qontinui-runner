//! Wrapper installation pipeline (`pnpm` / `npm` install + uninstall).
//!
//! Implements Phase 1.6 of the wrapper-runner integration plan:
//! - `POST /wrappers/install` — `pnpm install <package>@<version>`
//! - `POST /wrappers/:id/update` — re-run install
//! - `DELETE /wrappers/:id` — stop, remove dir, clear creds, drop cache row
//!
//! Each wrapper installs into its own `<APP_DATA>/qontinui-runner/wrappers/<id>/`
//! directory. The installer creates a stub `package.json` there so `pnpm
//! install` runs in isolation per wrapper (no shared lockfile, see plan
//! "Open questions §1"). After install completes we reads the freshly-
//! resolved `qontinui.wrapper.id` from the package and rename the dir to
//! the canonical id slot — that way the on-disk layout keys directly off
//! the manifest id, not the npm package name.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::credentials::CredentialStore;
use super::manager::WrapperManager;
use super::registry::{scan_one, WrapperRegistry};

/// Body for `POST /wrappers/install`.
#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Successful install response.
#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub id: String,
    pub package: String,
    pub version: String,
    pub install_path: String,
}

/// Errors raised by the installer.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no package manager available — install pnpm or npm and retry")]
    NoPackageManager,
    #[error("install failed: {0}")]
    InstallFailed(String),
    #[error("post-install scan failed: {0}")]
    Scan(String),
    #[error("invalid package spec '{0}'")]
    InvalidSpec(String),
}

/// Pick the preferred Node package manager. Order: `pnpm`, then `npm`.
/// Resolved at startup and at each install (cheap — `--version` is fast).
pub fn detect_package_manager() -> Option<&'static str> {
    for pm in ["pnpm", "npm"] {
        let ok = Command::new(pm)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(pm);
        }
    }
    None
}

/// Install (or update) a wrapper. The flow:
///
/// 1. Pick a temp directory under the wrappers root.
/// 2. Write a stub `package.json` into it so `pnpm install` is happy.
/// 3. Run `pnpm install <package>@<version>` with `cwd = temp dir`.
/// 4. Find the installed package's directory inside `node_modules/`.
/// 5. Read its `qontinui.wrapper.id` to find the canonical wrapper id.
/// 6. Move the temp dir to `<root>/<id>/`, replacing any prior install.
/// 7. Trigger a registry rescan so the new wrapper appears immediately.
pub async fn install(
    registry: &Arc<WrapperRegistry>,
    package: &str,
    version: Option<&str>,
) -> Result<InstallResponse, InstallError> {
    validate_package_spec(package)?;
    let pm = detect_package_manager().ok_or(InstallError::NoPackageManager)?;
    let root = registry.root().to_path_buf();
    std::fs::create_dir_all(&root)?;

    // Stage in a temp slot so a failed install doesn't leave a half-good
    // canonical directory behind.
    let stage = root.join(format!(".install-{}", short_random()));
    if stage.exists() {
        // Highly unlikely (random suffix) but tidy up if it does.
        let _ = std::fs::remove_dir_all(&stage);
    }
    std::fs::create_dir_all(&stage)?;

    // pnpm refuses to install into an empty directory unless either
    // `package.json` exists or `init` is run first; npm is fine either
    // way. Write a minimal manifest unconditionally — keeps the
    // install command identical for both managers.
    let stub = serde_json::json!({
        "name": "qontinui-wrapper-install-stage",
        "private": true,
        "version": "0.0.0"
    });
    std::fs::write(stage.join("package.json"), stub.to_string())?;

    let spec = match version {
        Some(v) if !v.is_empty() => format!("{}@{}", package, v),
        _ => package.to_string(),
    };

    info!(
        "wrappers.install: running '{} install {}' in {}",
        pm,
        spec,
        stage.display()
    );

    let stage_clone = stage.clone();
    let pm_owned = pm.to_string();
    let spec_owned = spec.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut cmd = Command::new(&pm_owned);
        cmd.current_dir(&stage_clone)
            .arg("install")
            .arg(&spec_owned)
            .arg("--ignore-scripts") // No postinstall — see plan §5.1 PR template.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd.output().map_err(|e| format!("spawn {}: {}", pm_owned, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(format!(
                "{} install exited with {}: {}",
                pm_owned, output.status, stderr
            ));
        }
        Ok(())
    })
    .await;

    if let Err(join_err) = &result {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(InstallError::InstallFailed(format!(
            "join error: {}",
            join_err
        )));
    }
    if let Ok(Err(install_err)) = result {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(InstallError::InstallFailed(install_err));
    }

    // Locate the installed package in node_modules. Scoped names map to
    // `node_modules/@scope/name/`.
    let pkg_dir = stage.join("node_modules").join(package);
    if !pkg_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(InstallError::InstallFailed(format!(
            "expected package directory '{}' was not created by {}",
            pkg_dir.display(),
            pm
        )));
    }

    // Read the installed manifest to find its wrapper id.
    let wrapper = scan_one(&pkg_dir)
        .await
        .map_err(|e| InstallError::Scan(e.to_string()))?;
    let id = wrapper.manifest.id.clone();
    let canonical = root.join(&id);

    // Move pkg_dir → canonical, replacing any prior install of the
    // same id. This is an atomic-on-same-volume rename when possible.
    if canonical.exists() {
        // Remove the previous install fully — wrapper bookkeeping owns
        // the directory contents.
        std::fs::remove_dir_all(&canonical)?;
    }
    if let Err(e) = std::fs::rename(&pkg_dir, &canonical) {
        // Cross-device moves fall back to a recursive copy.
        warn!(
            "wrappers.install: rename '{}' → '{}' failed ({}), copying",
            pkg_dir.display(),
            canonical.display(),
            e
        );
        copy_dir_recursive(&pkg_dir, &canonical)?;
    }

    // Tear down the staging dir.
    let _ = std::fs::remove_dir_all(&stage);

    // Refresh the cache so subsequent `GET /wrappers` calls see it.
    registry
        .rescan(&id)
        .await
        .map_err(|e| InstallError::Scan(e.to_string()))?;

    info!(
        "wrappers.install: '{}' (package '{}', version '{}') installed at '{}'",
        id,
        wrapper.package_name,
        wrapper.version,
        canonical.display()
    );

    Ok(InstallResponse {
        id,
        package: wrapper.package_name,
        version: wrapper.version,
        install_path: canonical.to_string_lossy().to_string(),
    })
}

/// Update a wrapper. Reuses [`install`], optionally pinning a new version.
/// The wrapper's existing credentials are preserved (keyring entries are
/// keyed by id, not by version).
pub async fn update(
    registry: &Arc<WrapperRegistry>,
    manager: &Arc<WrapperManager>,
    wrapper_id: &str,
    version: Option<&str>,
) -> Result<InstallResponse, InstallError> {
    let existing = registry
        .get(wrapper_id)
        .await
        .ok_or_else(|| InstallError::InstallFailed(format!("wrapper '{}' is not installed", wrapper_id)))?;
    // Stop first so we don't try to overwrite a held-open file.
    if let Err(e) = manager.stop(wrapper_id).await {
        warn!(
            "wrappers.install: stop('{}') before update failed: {}",
            wrapper_id, e
        );
    }
    install(registry, &existing.package_name, version).await
}

/// Uninstall a wrapper. Stops the subprocess, removes the install
/// directory, clears credentials, and drops the cache entry.
pub async fn uninstall(
    registry: &Arc<WrapperRegistry>,
    manager: &Arc<WrapperManager>,
    wrapper_id: &str,
) -> Result<(), InstallError> {
    let existing = registry.get(wrapper_id).await;
    if let Err(e) = manager.stop(wrapper_id).await {
        warn!(
            "wrappers.install: stop('{}') during uninstall failed: {}",
            wrapper_id, e
        );
    }

    if let Some(w) = &existing {
        let env_names: Vec<String> = w.manifest.env_vars.iter().map(|e| e.name.clone()).collect();
        CredentialStore::clear_for_wrapper(wrapper_id, &env_names);
    }

    let dir = registry.root().join(wrapper_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    registry.remove(wrapper_id).await;
    info!("wrappers.install: '{}' uninstalled", wrapper_id);
    Ok(())
}

/// Lightweight package-spec sanity check — guards against shell-meta
/// characters or empty strings making it into the `pnpm install` argv.
fn validate_package_spec(spec: &str) -> Result<(), InstallError> {
    if spec.is_empty() {
        return Err(InstallError::InvalidSpec(spec.to_string()));
    }
    let bad_chars = [' ', '\t', '\n', '\r', ';', '&', '|', '`', '$', '<', '>'];
    if spec.chars().any(|c| bad_chars.contains(&c)) {
        return Err(InstallError::InvalidSpec(spec.to_string()));
    }
    Ok(())
}

/// 8-char random suffix for staging directory names.
fn short_random() -> String {
    use rand::Rng;
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    let mut rng = rand::rng();
    (0..8)
        .map(|_| chars[rng.random_range(0..chars.len())])
        .collect()
}

/// Recursive directory copy fallback for cross-device renames.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_metacharacters() {
        assert!(validate_package_spec("@scope/name; rm -rf /").is_err());
        assert!(validate_package_spec("name`whoami`").is_err());
        assert!(validate_package_spec("@qontinui/wrapper-v0").is_ok());
        assert!(validate_package_spec("@qontinui/wrapper-v0@0.1.0").is_ok());
    }

    #[test]
    fn rejects_empty_spec() {
        assert!(validate_package_spec("").is_err());
    }

    #[test]
    fn random_suffix_is_8_chars() {
        let s = short_random();
        assert_eq!(s.chars().count(), 8);
    }

    #[allow(dead_code)]
    fn _detect_runs(_root: &Path) {
        // We don't actually invoke `pnpm` in unit tests because not
        // every CI box has it; this stub just exercises the import
        // surface so refactors can't silently drop it.
        let _ = detect_package_manager();
    }
}
