//! What a CI-node child process inherits from the runner.
//!
//! Temporary copy while qontinui-runner#1995 is open: #1995 deletes this file
//! together with `tools.rs`, and qontinui-schemas ci-exec `child_env.rs`
//! carries the same allowlist forward (plan
//! `2026-11-06-ci-node-provisioning-children-inherit-only-an-allowlisted-environment`,
//! D6). The two lists are kept identical.
//!
//! # Why an allowlist
//!
//! Every provisioning child — `find_python`'s probe, `python -m venv`,
//! `pip install`, and the `verify` and companion runs that EXECUTE the tool
//! just installed — used to start from the runner's whole environment minus
//! five prefixes (`PIP_*`, `PYTHON*`, `POETRY_*`, `NPM_CONFIG_*`,
//! `VIRTUAL_ENV`). That environment holds plaintext credentials (the runner
//! names three in `crate::terminal::CREDENTIAL_VALUE_ENV_VARS`), and what a
//! verification executes is third-party code: cargo-nextest carries no digest,
//! and the Python entries resolve their transitive dependencies unhashed. A
//! denylist protects only the names someone enumerated; an allowlist protects
//! by construction (coord memory `6bfac12d-5b6f-49d3-8fdf-85c3412fc333`).
//!
//! # The three policies (D1, D2)
//!
//! * [`ChildEnv::Provisioning`] — every provisioning child but the one that
//!   fetches. The environment is CLEARED and only [`PROVISIONING_BASE_ENV`]
//!   comes back, plus [`PIP_CONFIG_FILE`] at its explicit value.
//! * [`ChildEnv::ProvisioningNetwork`] — `pip install` alone: the base set
//!   plus [`PROVISIONING_NETWORK_ENV`], so a proxied box keeps provisioning.
//!   Only that child, because `--only-binary=:all:` runs no package code during
//!   the install while `verify` EXECUTES the tool, and a proxy URL can carry a
//!   credential. `download()` is in-process reqwest and spawns no child.
//! * [`ChildEnv::ContainerRuntime`] — docker / podman (`services.rs`). That is
//!   host software resolved from the runner's PATH, and its per-host needs
//!   (`DOCKER_HOST`, `DOCKER_CONTEXT`, `XDG_RUNTIME_DIR` for a rootless socket,
//!   `HOME`) are unmeasured, so it keeps the runner's environment; a container
//!   receives only the names its `-e NAME` flags list. The five-prefix removal
//!   and `PIP_CONFIG_FILE` no longer apply to it: they target ecosystems docker
//!   and podman never read.
//!
//! The set is CLOSED (D4). There is no operator-configured passthrough,
//! because a configured name would re-open the exposure this closes. A name is
//! added the way a registry entry is: a reviewed change naming the consumer
//! that breaks without it, never a wildcard.
//!
//! # The one exception: a caller's explicit `envs`
//!
//! `tools::run_capture_env` takes an `envs` list from its caller and applies
//! it AFTER the allowlist, without passing it through [`admits`]. That is
//! deliberate and caller-owned: it exists so `services` can hand a container
//! runtime a secret without putting it on an argv. No production provisioning
//! call passes any `envs` — they all go through `tools::run_capture`, which
//! passes `&[]` — and `child_env::tests` pins that every production caller
//! with a non-empty `envs` runs under [`ChildEnv::ContainerRuntime`]. What
//! the spawn HELPER sets is different: that is re-applied only through
//! [`admits`].
//!
//! # What this does not close
//!
//! Only the ENVIRONMENT channel. A child still runs as the runner's OS user,
//! so it can read that user's files (device-JWT files, `.mcp.json` nonces,
//! cloud credential files) and, on Windows, its registry — the same-OS-user
//! residual of plan `2026-10-13-ci-node-runs-model-dependent-eval-steps`.

use std::ffi::{OsStr, OsString};

/// What a child started by `tools::run_capture_env` inherits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildEnv {
    /// A provisioning child that needs no network: `find_python`,
    /// `python -m venv`, `verify` and the companion version runs. Inherits
    /// [`PROVISIONING_BASE_ENV`] only.
    Provisioning,
    /// The one provisioning child that fetches, `pip install`. Inherits
    /// [`PROVISIONING_BASE_ENV`] and [`PROVISIONING_NETWORK_ENV`].
    ProvisioningNetwork,
    /// A container-runtime CLI (`services.rs`). Inherits the runner's whole
    /// environment, unchanged (see the module docs for why).
    ContainerRuntime,
}

/// Inherited by every provisioning child. Each entry names the consumer that
/// breaks without it (D3).
///
/// Left out on purpose: `NODE_OPTIONS` (a `--require` hook straight into npm's
/// verification), `XDG_RUNTIME_DIR`, and the manifest's step-only
/// `ENV_ALLOWLIST` (`manifest.rs`) — `provision()` takes no step environment.
/// `windir`, `SystemDrive`, `USER` and `LOGNAME` enter only on a recorded
/// failure.
pub(crate) const PROVISIONING_BASE_ENV: &[&str] = &[
    // `find_python` runs bare `python3` / `python` / `py`, and node's Unix
    // `bin/npm` starts `#!/usr/bin/env node`.
    "PATH",
    // Python's `os.path.expanduser` and node's `os.homedir()` (Unix).
    "HOME",
    // The same two on Windows.
    "USERPROFILE",
    // `tempfile` in ensurepip and pip (Unix).
    "TMPDIR",
    // `tempfile` on Windows, which otherwise falls back to the CWD.
    "TEMP",
    "TMP",
    // The Windows loader and CPython's start-up; without it `python -m venv`
    // fails (the owner plan's venv failure).
    "SystemRoot",
    // Per-user config and data directories that pip and npm resolve on
    // Windows (the same venv failure).
    "APPDATA",
    "LOCALAPPDATA",
    // npm.cmd's `FOR /F … IN ('CALL …')` runs `%ComSpec%`.
    "ComSpec",
    // cmd.exe's resolution of an extensionless command name.
    "PATHEXT",
    // The output encoding of the children whose `--version` text is parsed.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    // Linux only, and a loader path rather than a credential. The CPython
    // `actions/setup-python` installs is built `--enable-shared` with an rpath
    // naming its build-time tool cache, and setup-python exports
    // `LD_LIBRARY_PATH=<installDir>/lib` on Linux so that interpreter loads
    // from a tool cache anywhere else, as on a self-hosted pool
    // (`actions/python-versions` `builders/ubuntu-python-builder.psm1`,
    // `actions/setup-python` `src/find-python.ts`, read 2026-11-06).
    #[cfg(target_os = "linux")]
    "LD_LIBRARY_PATH",
];

/// Inherited by [`ChildEnv::ProvisioningNetwork`] — `pip install` — on top of
/// [`PROVISIONING_BASE_ENV`], so a box behind a proxy keeps provisioning (D2).
/// `PIP_*` stays out: the index is the registry entry's, never the host's.
pub(crate) const PROVISIONING_NETWORK_ENV: &[&str] = &[
    // Proxy routing, in both spellings: pip reads them through the standard
    // library's `getproxies()`, which accepts either.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    // The trust roots a TLS-intercepting proxy needs.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

/// Set on every provisioning child at an explicit value, and never inherited:
/// a host's own `PIP_CONFIG_FILE` is not on either list, so it cannot reach a
/// child.
///
/// Leaving the variable out does NOT disable pip's config — pip then falls back
/// to its defaults (`/etc/pip.conf`, `~/.pip/pip.conf`,
/// `~/.config/pip/pip.conf`, `%APPDATA%\pip\pip.ini`). `--index-url` overrides
/// a config `index-url`, but `extra-index-url`, `find-links`, `trusted-host`
/// and `pre` are ADDITIVE and survive, so a box with an internal mirror
/// configured still had a dependency-confusion surface. Setting the variable to
/// the null device is the documented way to make pip load no config file at
/// all.
///
/// THE SPELLING IS LOAD-BEARING AND MUST BE LOWERCASE. pip's skip is an exact
/// string comparison, not a path resolution
/// (`_internal/configuration.py::_load_config_files`):
///
/// ```text
/// if config_files[kinds.ENV][0:1] == [os.devnull]:
///     return   # this return is what skips GLOBAL and SITE too
/// ```
///
/// and `os.devnull` on Windows is `'nul'`, lowercase (`ntpath.py`).
/// `"NUL" != "nul"`, so an uppercase value never fires that return: measured
/// with this repo's own Python, `PIP_CONFIG_FILE=NUL` gives
/// `devnull match: False` and still loads the GLOBAL and SITE configs — i.e.
/// exactly the additive `extra-index-url`/`find-links`/`trusted-host` this is
/// here to neutralise. Only the USER config gets suppressed, incidentally,
/// because `iter_config_files` drops USER whenever the variable is set at all.
/// With `nul` the match is True and nothing is loaded. POSIX `/dev/null`
/// matches exactly and is fine.
pub(crate) const PIP_CONFIG_FILE: &str = "PIP_CONFIG_FILE";

/// [`PIP_CONFIG_FILE`]'s value: the null device, in the spelling pip compares.
const PIP_CONFIG_FILE_VALUE: &str = if cfg!(target_os = "windows") {
    "nul"
} else {
    "/dev/null"
};

/// The allowlists a policy draws from, or `None` when it inherits everything.
fn allowlists(policy: ChildEnv) -> Option<&'static [&'static [&'static str]]> {
    match policy {
        ChildEnv::Provisioning => Some(&[PROVISIONING_BASE_ENV]),
        ChildEnv::ProvisioningNetwork => Some(&[PROVISIONING_BASE_ENV, PROVISIONING_NETWORK_ENV]),
        ChildEnv::ContainerRuntime => None,
    }
}

/// Environment names compare case-insensitively on Windows, where the OS
/// does (`Path` is `PATH`), and exactly everywhere else — the rule
/// `crates/pty-holder/src/spawn.rs` `holder_env` applies.
fn same_name(name: &OsStr, listed: &str) -> bool {
    if cfg!(target_os = "windows") {
        name.eq_ignore_ascii_case(listed)
    } else {
        name == listed
    }
}

/// Does `policy` let a variable named `name` through to the child?
///
/// One rule for the inherited environment AND for any variable the spawn
/// helper set explicitly, so what a helper chooses to set can never widen the
/// set. Every name passes under a policy that inherits.
pub(crate) fn admits(policy: ChildEnv, name: &OsStr) -> bool {
    match allowlists(policy) {
        None => true,
        Some(lists) => lists
            .iter()
            .flat_map(|list| list.iter())
            .any(|listed| same_name(name, listed)),
    }
}

/// The environment a child under `policy` starts from, built from `inherited`
/// (the runner's own environment, in production).
///
/// `None` means INHERIT: the caller leaves the command's environment alone.
/// `Some(env)` is the WHOLE environment: the caller clears the command's and
/// applies exactly this — every inherited variable [`admits`] lets through,
/// then [`PIP_CONFIG_FILE`] at its explicit value. Pure, so the allowlist is
/// tested against a synthetic environment rather than this machine's.
pub(crate) fn provisioning_env(
    policy: ChildEnv,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> Option<Vec<(OsString, OsString)>> {
    allowlists(policy)?;
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter(|(name, _)| admits(policy, name))
        .collect();
    env.push((PIP_CONFIG_FILE.into(), PIP_CONFIG_FILE_VALUE.into()));
    Some(env)
}

#[cfg(test)]
mod tests;
