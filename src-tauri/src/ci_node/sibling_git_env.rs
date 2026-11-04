//! The isolated git environment every CI-sibling git call runs under.
//!
//! Temporary copy while qontinui-runner#1995 is open; #1995 deletes this file
//! and qontinui-schemas ci-exec sibling_git.rs carries the fix forward.
//!
//! # Why (finding `e56006ed-cd70-49ca-8e1f-1e1b8daaafd0`)
//!
//! A sibling is fetched over public HTTPS ([`super::sibling`] `clone_url`),
//! but the runner's git children still read the node owner's system and global
//! git config, and inherit whatever `GIT_*` the runner was started with. So a
//! configured `credential.helper` (`gh auth git-credential`, Git Credential
//! Manager) answered a PRIVATE sibling fetch with the owner's token, and a
//! global `url.<ssh>.insteadOf` routed it over the owner's SSH key: a private
//! sibling "worked" with no grant, no audit and no trust check. Measured on
//! git 2.47.3: an ambient private `ls-remote` succeeds through a URL-scoped
//! global helper plus the agent posture's `GIT_CONFIG_COUNT` overlay, and fails
//! fast (128) under this module's environment while public `ls-remote` /
//! `fetch` still succeed.
//!
//! # What the environment is
//!
//! Plan `2026-10-13-local-lane-ci-reads-a-private-sibling-through-a-dispatch-scoped-door`
//! D5, with no door (this copy only narrows):
//!
//! - **Every inherited `GIT_*` variable is removed except an allowlist** —
//!   the prompt-proof keys of [`git_posture::prompt_proof_git_env`] (plus
//!   `GIT_EXEC_PATH` on Windows). An allowlist rather than a denylist, so a git
//!   variable added later cannot reopen the hole. It removes, among others,
//!   the command-scope config of [`git_posture::COMMAND_SCOPE_GIT_CONFIG_ENV`]
//!   (`GIT_CONFIG_PARAMETERS` survives `NOSYSTEM`, an empty global and a
//!   `KEY_0` reset — measured) with the numbered `GIT_CONFIG_KEY_n` /
//!   `VALUE_n` pairs, every [`git_posture::REPO_LOCAL_GIT_ENV`] name,
//!   `GIT_SSH*`, `GIT_PROXY_COMMAND`, and `GIT_TRACE*` / `GIT_CURL_VERBOSE`
//!   (which would print request headers).
//! - `GIT_CONFIG_NOSYSTEM=1` (not on Windows — see below), and
//!   `GIT_CONFIG_GLOBAL` pointing at a per-provision file under the dispatch
//!   root (which cleanup removes) holding only `[credential]` / `helper =`.
//!   The helper reset lives in that FILE, not in an env overlay, because an
//!   empty `GIT_CONFIG_VALUE_n` may reach a mingw `git.exe` as unset and make
//!   git die `missing config value` (see
//!   [`git_posture::ASKPASS_DISABLED_SENTINEL`]). No env value set here is
//!   ever empty.
//! - `GIT_CEILING_DIRECTORIES` = the PARENT of the dispatch root. Sibling git
//!   runs in the dispatch root (`ls-remote`, `init`) and in each sibling's own
//!   fresh repository below it. Without a ceiling, a call in the dispatch root
//!   discovers an ENCLOSING repository upward — the runner's CI root is the
//!   workspace root, itself a git repository — and applies that repo's
//!   `.git/config` (measured: its `credential.helper=store` and
//!   `url.git@github.com:.insteadOf` applied under the otherwise-isolated
//!   environment). Git never treats a ceiling directory itself as a candidate
//!   but does check the cwd, so the ceiling must be the parent: a ceiling equal
//!   to the cwd does NOT stop discovery (measured).
//! - `HOME` (and `USERPROFILE` on Windows) = an empty per-provision directory
//!   under the dispatch root, because git's libcurl reads `~/.netrc`
//!   (`_netrc` on Windows) for HTTP credentials independently of any git
//!   config (measured: an owner `~/.netrc` answered a sibling fetch).
//! - The prompt-proof layer is re-applied after the scrub.
//! - git below 2.32 silently ignores `GIT_CONFIG_GLOBAL` and would read the
//!   owner's config again — the isolation would fail OPEN — so it is refused.
//!
//! # Windows (D5's named fallback)
//!
//! The Windows reading of Phase 0 was not measured, so on Windows the
//! environment takes D5's fallback: `GIT_CONFIG_NOSYSTEM` is NOT set (Git for
//! Windows keeps settings its own install depends on — `http.sslBackend`,
//! `core.autocrlf` and the like — in the system file). The global file's empty
//! `helper =` is read AFTER the system file, so it still clears every system
//! credential helper, URL-scoped ones included. **The recorded residual:** a
//! system-level `url.<x>.insteadOf` on a Windows node still applies.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use qontinui_runner_lib::git_posture;

/// The per-provision global config file, written into the dispatch root.
pub(super) const GLOBAL_CONFIG_FILE: &str = ".sibling-git-global.config";
/// The per-provision empty `HOME`, created in the dispatch root.
pub(super) const HOME_DIR: &str = ".sibling-git-home";
/// Its whole body: an empty `helper =` clears every helper read before it
/// (system config, and any earlier global entry, URL-scoped ones included).
const GLOBAL_CONFIG_BODY: &str = "[credential]\n\thelper =\n";
/// `GIT_CONFIG_GLOBAL` needs git 2.32 (`GIT_CONFIG_COUNT` 2.31).
const MIN_GIT: (u32, u32) = (2, 32);

/// The isolated environment for one provision's sibling git calls.
#[derive(Debug, Clone)]
pub(super) struct SiblingGitEnv {
    /// Inherited `GIT_*` names to remove from every child.
    removals: Vec<OsString>,
    /// What every child gets set, after the removals. Never an empty value.
    sets: Vec<(OsString, OsString)>,
}

/// `GIT_*` names allowed through the scrub: the prompt-proof posture, and
/// `GIT_EXEC_PATH` on Windows (Git for Windows' own wrapper sets it).
fn allowlisted(name: &str) -> bool {
    (cfg!(windows) && name == "GIT_EXEC_PATH")
        || git_posture::prompt_proof_git_env()
            .iter()
            .any(|(k, _)| k == name)
}

/// A name the scrub removes: any `GIT_*` (case-insensitively, as Windows
/// environment names are) that is not allowlisted.
fn scrubbed(name: &OsStr) -> bool {
    let upper = name.to_string_lossy().to_ascii_uppercase();
    upper.starts_with("GIT_") && !allowlisted(&upper)
}

/// The per-provision paths the environment points git at.
#[derive(Debug, Clone)]
struct IsolationPaths {
    /// The already-written global config file.
    global: PathBuf,
    /// The already-created empty home directory.
    home: PathBuf,
    /// The discovery ceiling: the parent of the directory sibling git runs in.
    ceiling: PathBuf,
}

impl SiblingGitEnv {
    /// Write the per-provision global config and empty home under
    /// `dispatch_root` and build the environment against THIS process's
    /// inherited variables.
    pub(super) fn isolated(dispatch_root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dispatch_root)
            .map_err(|e| format!("create {}: {e}", dispatch_root.display()))?;
        let global = dispatch_root.join(GLOBAL_CONFIG_FILE);
        std::fs::write(&global, GLOBAL_CONFIG_BODY)
            .map_err(|e| format!("write {}: {e}", global.display()))?;
        let home = dispatch_root.join(HOME_DIR);
        std::fs::create_dir_all(&home).map_err(|e| format!("create {}: {e}", home.display()))?;
        // Sibling git runs IN the dispatch root and below it; the ceiling must
        // be its parent, because git checks the cwd itself before consulting
        // the ceiling list.
        let ceiling = dispatch_root
            .parent()
            .unwrap_or(dispatch_root)
            .to_path_buf();
        Ok(Self::isolation_for(
            std::env::vars_os(),
            &IsolationPaths {
                global,
                home,
                ceiling,
            },
        ))
    }

    /// The pure core: `inherited` is the parent environment.
    fn isolation_for(
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
        paths: &IsolationPaths,
    ) -> Self {
        let removals = inherited
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| scrubbed(k))
            .collect();
        let mut sets: Vec<(OsString, OsString)> = git_posture::prompt_proof_git_env()
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        // D5's Windows fallback: keep the system file (see the module doc).
        if !cfg!(windows) {
            sets.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
        }
        sets.push((
            "GIT_CONFIG_GLOBAL".into(),
            paths.global.as_os_str().to_owned(),
        ));
        sets.push((
            "GIT_CEILING_DIRECTORIES".into(),
            paths.ceiling.as_os_str().to_owned(),
        ));
        sets.push(("HOME".into(), paths.home.as_os_str().to_owned()));
        if cfg!(windows) {
            sets.push(("USERPROFILE".into(), paths.home.as_os_str().to_owned()));
        }
        debug_assert!(sets.iter().all(|(_, v)| !v.is_empty()));
        Self { removals, sets }
    }

    /// Apply to a command after the host posture: removes every inherited
    /// non-allowlisted `GIT_*`, and any such variable already set on `cmd`,
    /// then sets the isolation.
    pub(super) fn isolate_git_command(&self, cmd: &mut tokio::process::Command) {
        self.isolate_std_git_command(cmd.as_std_mut());
    }

    fn isolate_std_git_command(&self, cmd: &mut std::process::Command) {
        let set_earlier: Vec<OsString> = cmd
            .get_envs()
            .filter(|(k, v)| v.is_some() && scrubbed(k))
            .map(|(k, _)| k.to_owned())
            .collect();
        for k in self.removals.iter().chain(set_earlier.iter()) {
            cmd.env_remove(k);
        }
        for (k, v) in &self.sets {
            cmd.env(k, v);
        }
    }
}

/// Refuse a git too old to honour `GIT_CONFIG_GLOBAL`. `version_output` is
/// `git --version`'s stdout (`git version 2.47.3`, `… 2.45.1.windows.1`).
/// Unreadable output is refused too: the isolation must never fail open.
pub(super) fn check_git_version(version_output: &str) -> Result<(), String> {
    let ver = version_output
        .trim()
        .strip_prefix("git version ")
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    let mut parts = ver.split('.').map(|p| p.parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => {
            if (major, minor) < MIN_GIT {
                Err(format!(
                    "git too old for sibling isolation ({ver} < {}.{})",
                    MIN_GIT.0, MIN_GIT.1
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(format!(
            "cannot read the git version from {:?}; refusing sibling isolation",
            version_output.trim()
        )),
    }
}

/// Run one sibling git call under `env`. Every sibling git invocation goes
/// through here.
pub(super) async fn run_sibling_git(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
    env: &SiblingGitEnv,
) -> Result<String, String> {
    super::checkout::run_git_with(cwd, args, timeout, |cmd| env.isolate_git_command(cmd)).await
}

/// Run `git --version` under `env` in `cwd` and refuse a git below the floor.
/// `before` adjusts the command ahead of the isolation (the test seam a stub
/// `git` on `PATH` enters through; production passes a no-op).
async fn require_git_floor(
    cwd: &Path,
    env: &SiblingGitEnv,
    before: impl FnOnce(&mut tokio::process::Command),
) -> Result<(), String> {
    let out = super::checkout::run_git_with(cwd, &["--version"], Duration::from_secs(60), |cmd| {
        before(cmd);
        env.isolate_git_command(cmd);
    })
    .await?;
    check_git_version(&out)
}

/// Build the isolated environment for one provision and check the git version
/// under it, once.
pub(super) async fn prepare_isolated_sibling_git(
    dispatch_root: &Path,
) -> Result<SiblingGitEnv, String> {
    let env = SiblingGitEnv::isolated(dispatch_root)?;
    require_git_floor(dispatch_root, &env, |_| {}).await?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    /// The hostile parent: an agent-posture overlay with a credential helper,
    /// command-scope config with an SSH rewrite, a foreign repository, traces.
    const HOSTILE: &[(&str, &str)] = &[
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "credential.https://github.com.helper"),
        ("GIT_CONFIG_VALUE_0", "!gh auth git-credential"),
        (
            "GIT_CONFIG_PARAMETERS",
            "'credential.helper'='store' 'url.git@github.com:.insteadof'='https://github.com/'",
        ),
        ("GIT_DIR", "/somewhere/else/.git"),
        ("GIT_WORK_TREE", "/somewhere/else"),
        ("GIT_SSH_COMMAND", "ssh -i /owner/key"),
        ("GIT_TRACE", "1"),
        ("GIT_TRACE_CURL", "1"),
        ("GIT_TRACE_PACKET", "1"),
        ("GIT_CURL_VERBOSE", "1"),
        ("GIT_TERMINAL_PROMPT", "1"),
        ("GIT_CEILING_DIRECTORIES", "/owner/elsewhere"),
        ("GIT_CONFIG_NOSYSTEM", "0"),
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/owner"),
        ("USERPROFILE", "C:\\Users\\owner"),
    ];

    /// The value `env` sets for `name`, if any.
    fn set_value(env: &SiblingGitEnv, name: &str) -> Option<PathBuf> {
        env.sets
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| PathBuf::from(v))
    }

    fn fake_paths() -> IsolationPaths {
        IsolationPaths {
            global: PathBuf::from("/dispatch/.g.config"),
            home: PathBuf::from("/dispatch/.home"),
            ceiling: PathBuf::from("/ci-worktrees"),
        }
    }

    /// The environment a child of `cmd` sees when its parent's is `inherited`:
    /// std's semantics, `env_remove` drops, `env` sets.
    fn child_env(
        inherited: &[(OsString, OsString)],
        cmd: &std::process::Command,
    ) -> BTreeMap<String, String> {
        let mut m: BTreeMap<String, String> = inherited
            .iter()
            .map(|(k, v)| (k.to_string_lossy().into(), v.to_string_lossy().into()))
            .collect();
        for (k, v) in cmd.get_envs() {
            let k = k.to_string_lossy().into_owned();
            match v {
                Some(v) => {
                    m.insert(k, v.to_string_lossy().into_owned());
                }
                None => {
                    m.remove(&k);
                }
            }
        }
        m
    }

    /// The hostile parent, with the same variables ALSO set on the command
    /// earlier (as a host posture would), isolated.
    fn isolated_child() -> (BTreeMap<String, String>, SiblingGitEnv) {
        let inherited = os(HOSTILE);
        let env = SiblingGitEnv::isolation_for(inherited.clone(), &fake_paths());
        let mut cmd = std::process::Command::new("git");
        cmd.env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_1", "credential.helper")
            .env("GIT_CONFIG_VALUE_1", "store");
        env.isolate_std_git_command(&mut cmd);
        (child_env(&inherited, &cmd), env)
    }

    #[test]
    fn an_inherited_config_overlay_is_gone_from_the_child() {
        let (child, _) = isolated_child();
        for k in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_KEY_1",
            "GIT_CONFIG_VALUE_1",
        ] {
            assert!(!child.contains_key(k), "{k} survived: {child:?}");
        }
    }

    #[test]
    fn parameters_and_git_dir_are_gone_and_only_allowlisted_git_vars_survive() {
        let (child, _) = isolated_child();
        assert!(!child.contains_key("GIT_CONFIG_PARAMETERS"), "{child:?}");
        assert!(!child.contains_key("GIT_DIR"), "{child:?}");
        assert!(!child.contains_key("GIT_SSH_COMMAND"), "{child:?}");
        let ours = [
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CEILING_DIRECTORIES",
        ];
        for k in child.keys().filter(|k| k.starts_with("GIT_")) {
            assert!(
                allowlisted(k) || ours.contains(&k.as_str()),
                "{k} is not allowlisted but survived: {child:?}"
            );
        }
        // The prompt-proof layer wins over an inherited value.
        assert_eq!(
            child.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        assert!(child.contains_key("GIT_ASKPASS"));
        // Non-git, non-home variables are untouched.
        assert_eq!(child.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
    }

    /// `~/.netrc` is read by libcurl, not by git config: the owner's home must
    /// not be the child's.
    #[test]
    fn home_is_the_per_provision_dir_not_the_owners() {
        let (child, _) = isolated_child();
        assert_eq!(
            child.get("HOME").map(String::as_str),
            Some("/dispatch/.home")
        );
        if cfg!(windows) {
            assert_eq!(
                child.get("USERPROFILE").map(String::as_str),
                Some("/dispatch/.home")
            );
        } else {
            // Off Windows nothing reads it; it is left as inherited.
            assert_eq!(
                child.get("USERPROFILE").map(String::as_str),
                Some("C:\\Users\\owner")
            );
        }
    }

    #[test]
    fn nosystem_global_and_ceiling_are_set() {
        let (child, _) = isolated_child();
        if cfg!(windows) {
            // D5's fallback: the system file stays; the inherited value is
            // still scrubbed, so nothing sets it.
            assert!(!child.contains_key("GIT_CONFIG_NOSYSTEM"), "{child:?}");
        } else {
            assert_eq!(
                child.get("GIT_CONFIG_NOSYSTEM").map(String::as_str),
                Some("1")
            );
        }
        assert_eq!(
            child.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some("/dispatch/.g.config")
        );
        assert_eq!(
            child.get("GIT_CEILING_DIRECTORIES").map(String::as_str),
            Some("/ci-worktrees")
        );
    }

    #[test]
    fn no_git_trace_survives() {
        let (child, _) = isolated_child();
        for k in child.keys() {
            assert!(
                !k.starts_with("GIT_TRACE") && k != "GIT_CURL_VERBOSE",
                "{k} survived: {child:?}"
            );
        }
    }

    #[test]
    fn the_global_file_resets_the_helper_and_no_env_value_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("d-1");
        let env = SiblingGitEnv::isolated(&root).unwrap();
        let global = set_value(&env, "GIT_CONFIG_GLOBAL").unwrap();
        assert!(global.starts_with(&root), "{}", global.display());
        let body = std::fs::read_to_string(&global).unwrap();
        assert!(body.contains("[credential]"), "{body:?}");
        assert!(
            body.lines().any(|l| l.trim() == "helper ="),
            "the global file must carry an empty `helper =`: {body:?}"
        );
        // The home is an existing, EMPTY directory inside the dispatch root.
        let home = set_value(&env, "HOME").unwrap();
        assert!(home.starts_with(&root), "{}", home.display());
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
        // The ceiling is the dispatch root's parent.
        assert_eq!(
            set_value(&env, "GIT_CEILING_DIRECTORIES").as_deref(),
            Some(dir.path())
        );
        let (child, built) = isolated_child();
        assert!(!built.sets.is_empty());
        for (k, v) in &built.sets {
            assert!(!v.is_empty(), "{k:?} is set empty");
        }
        for (k, v) in child.iter().filter(|(k, _)| k.starts_with("GIT_")) {
            assert!(!v.is_empty(), "{k} is empty in the child");
        }
    }

    #[test]
    fn git_below_2_32_is_refused() {
        let err = check_git_version("git version 2.31.8\n").unwrap_err();
        assert_eq!(err, "git too old for sibling isolation (2.31.8 < 2.32)");
        assert!(check_git_version("git version 1.9.5").is_err());
        assert!(check_git_version("not git at all").is_err());
        assert!(check_git_version("").is_err());
        check_git_version("git version 2.32.0").unwrap();
        check_git_version("git version 2.47.3\n").unwrap();
        check_git_version("git version 2.45.1.windows.1").unwrap();
        check_git_version("git version 3.0.0").unwrap();
    }

    fn git_installed() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// A stub `git` reporting 2.25.1 is refused through the same floor check
    /// the provision runs; the installed git passes it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stub_git_below_the_floor_is_refused_and_real_git_passes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("git");
        std::fs::write(&stub, "#!/bin/sh\necho 'git version 2.25.1'\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = dir.path().join("d-1");
        let env = SiblingGitEnv::isolated(&root).unwrap();
        let err = require_git_floor(&root, &env, |cmd| {
            cmd.env("PATH", &bin);
        })
        .await
        .unwrap_err();
        assert!(
            err.contains("git too old for sibling isolation (2.25.1 < 2.32)"),
            "{err}"
        );
        if git_installed() {
            require_git_floor(&root, &env, |_| {}).await.unwrap();
        }
    }

    /// A sibling git call whose command carries the hostile parent's `GIT_*`
    /// (set by an earlier layer, as a host posture would) before isolation.
    async fn hostile_git(cwd: &Path, env: &SiblingGitEnv, args: &[&str]) -> Result<String, String> {
        super::super::checkout::run_git_with(cwd, args, Duration::from_secs(60), |cmd| {
            for (k, v) in HOSTILE.iter().filter(|(k, _)| k.starts_with("GIT_")) {
                cmd.env(k, v);
            }
            env.isolate_git_command(cmd);
        })
        .await
    }

    /// End to end against the installed git: a hostile parent's helper and
    /// `insteadOf` rewrite do not reach a sibling's git. Skipped only when no
    /// `git` can be spawned.
    #[tokio::test]
    async fn an_owner_helper_and_rewrite_do_not_reach_sibling_git() {
        if !git_installed() {
            eprintln!("git is not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let env = prepare_isolated_sibling_git(dir.path()).await.unwrap();
        let helpers = hostile_git(
            dir.path(),
            &env,
            &["config", "--get-all", "credential.helper"],
        )
        .await
        .unwrap();
        assert_eq!(helpers, "", "only the empty reset may remain: {helpers:?}");
        let rewrites = hostile_git(dir.path(), &env, &["config", "--get-regexp", "^url\\."]).await;
        assert!(
            rewrites.is_err(),
            "an insteadOf rewrite survived: {rewrites:?}"
        );
    }

    fn plain_git(cwd: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// The runner's CI root is itself a git repository. Its `.git/config` —
    /// a credential helper and an `insteadOf` — must not apply to sibling git
    /// run in the dispatch root below it.
    #[tokio::test]
    async fn an_enclosing_repos_config_does_not_reach_sibling_git() {
        if !git_installed() {
            eprintln!("git is not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let enclosing = dir.path();
        plain_git(enclosing, &["init", "-q"]);
        plain_git(enclosing, &["config", "credential.helper", "store"]);
        plain_git(
            enclosing,
            &[
                "config",
                "url.git@github.com:.insteadOf",
                "https://github.com/",
            ],
        );
        let root = enclosing.join(".ci-worktrees").join("d-1");
        let env = prepare_isolated_sibling_git(&root).await.unwrap();
        let helpers = hostile_git(&root, &env, &["config", "--get-all", "credential.helper"])
            .await
            .unwrap();
        assert_eq!(helpers, "", "only the empty reset may remain: {helpers:?}");
        let url = hostile_git(
            &root,
            &env,
            &["ls-remote", "--get-url", "https://github.com/o/n.git"],
        )
        .await
        .unwrap();
        assert_eq!(url, "https://github.com/o/n.git", "rewritten: {url:?}");
    }
}
