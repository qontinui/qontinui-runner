//! The fleet's ONE non-interactive git credential posture.
//!
//! Lives in the LIB crate because it has two consumers on opposite sides of the
//! lib/bin split: `credential_helper` (bin) applies it to the eight seams that
//! spawn a `claude`, and `process_helpers` (compiled into BOTH crates) applies
//! its prompt-closing subset to every git subprocess the runner starts. A
//! second copy is exactly the accretion the dossier
//! `git-push-hang-credential-helper` exists to stop, so there is one list and
//! everything else derives from it.

/// A `GIT_ASKPASS` value that closes git's askpass layer without ever emitting
/// an EMPTY environment value.
///
/// git resolves an askpass program as `GIT_ASKPASS` env -> `core.askPass`
/// config -> `SSH_ASKPASS` env, stopping at the first NON-NULL value; only when
/// that yields nothing does it consult `GIT_TERMINAL_PROMPT`. Pointing
/// `GIT_ASKPASS` at a path that exists on no fleet box therefore shadows both
/// `core.askPass` and an inherited `SSH_ASKPASS`, cannot exec, and hands the
/// decision straight to `GIT_TERMINAL_PROMPT=0` -- a fast, readable
/// `terminal prompts disabled` instead of a silent hang.
///
/// It is deliberately NOT the empty string. An empty-valued env var would
/// depend on `Command::env(k, "")` reaching a mingw `git.exe` as set-but-empty
/// rather than as unset, which is unverifiable from Linux and whose failure
/// mode (a silently dropped `GIT_ASKPASS`, or an empty `GIT_CONFIG_VALUE_n`
/// that makes git `die("missing config value")` on every invocation) is worse
/// than the hang it replaces. See
/// `knowledge-base/qontinui-specific/git-push-non-interactive.md`.
pub const ASKPASS_DISABLED_SENTINEL: &str = "/qontinui-runner/askpass-disabled";

/// Env that gives a runner-spawned `git` a fully non-interactive credential
/// posture, so no process the runner starts can ever block on a credential UI.
/// A push that cannot authenticate FAILS FAST with a readable auth error
/// instead of hanging with no output.
///
/// THREE PROMPT LAYERS, one switch each -- a credential prompt is reachable
/// through any of them, so closing two still leaves the hang:
///
/// 1. Git Credential Manager's own UI (account chooser, login dialog) --
///    closed by `GCM_INTERACTIVE=never`.
/// 2. git's askpass fallback (`GIT_ASKPASS` / `core.askPass` / `SSH_ASKPASS`,
///    i.e. VS Code's askpass script or Git for Windows' `git-askpass.exe`) --
///    closed by [`ASKPASS_DISABLED_SENTINEL`].
/// 3. git's own terminal prompt (a `/dev/tty` read) -- closed by
///    `GIT_TERMINAL_PROMPT=0`.
///
/// Measured 2026-09-03 (git 2.47.3): `GIT_TERMINAL_PROMPT=0` ALONE still hangs
/// indefinitely behind a blocking askpass program, which is why layer 2 is not
/// optional. No value emitted here is ever empty (see
/// [`ASKPASS_DISABLED_SENTINEL`]).
///
/// Also emitted: a github.com-scoped `credential.helper` of
/// `!gh auth git-credential`, layered via git's `GIT_CONFIG_COUNT` /
/// `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` env mechanism (NO file writes;
/// applies to ALL cwds) -- routing unregistered / umbrella-root GitHub access
/// through the user's own `gh` auth, which is what turns "fails fast" into
/// "usually just works".
///
/// TRADE-OFF, stated rather than hidden: a human wanting FIRST-TIME interactive
/// auth to a non-GitHub host (gitlab/azure/bitbucket) from a runner terminal
/// now gets `terminal prompts disabled` instead of a dialog. That is deliberate
/// -- the measured population of these PTYs is autonomous Claude Code sessions,
/// where the dialog is an infinite hang. The recovery is either authenticating
/// once outside the runner, or a caller-supplied `extra_env` override: every
/// seam applies this posture BEFORE `extra_env`.
///
/// PRECEDENCE -- why this does NOT clobber the per-session coord helper for
/// REGISTERED repos: `credential.helper` is MULTI-VALUED. Git accumulates every
/// configured helper into an ordered list and queries them in config-read order
/// (system -> global -> local -> worktree -> `GIT_CONFIG_*` env) until one returns a
/// username+password. A repo's `--local` coord helper is therefore read — and
/// tried — BEFORE this env-injected github.com helper: for a coord-registered
/// repo the coord helper emits the push token and git never reaches the `gh`
/// fallback. We deliberately do NOT reset the helper list (no empty-string
/// entry): a github.com-scoped reset injected via env is read AFTER — and would
/// thus wipe — the local coord helper, breaking registered-repo pushes. (A
/// HAND-RUN push outside a runner is the opposite case and DOES want
/// `-c credential.helper=` first, because git APPENDS helpers rather than
/// replacing them and there is no coord helper to protect; that recipe lives in
/// the knowledge-base note, not here.)
///
/// The github.com config pair(s) are APPENDED after any `GIT_CONFIG_*` already
/// present in the child's inherited environment (we read `GIT_CONFIG_COUNT` and
/// index from there), so a caller/parent that already injected git config keeps
/// it rather than having `KEY_0`/`VALUE_0`/`COUNT` silently overwritten.
///
/// Set on the child env BEFORE any caller-supplied `extra_env` so a caller can
/// still intentionally override. Not platform-gated: GCM is Windows-centric but
/// every var here is harmless (and correct) cross-platform.
pub fn non_interactive_git_env() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![
        // Layer 1 — GCM's own UI.
        ("GCM_INTERACTIVE".to_string(), "never".to_string()),
        // Layer 3 — git's terminal prompt.
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        // Layer 2 — git's askpass chain. Shadows core.askPass and SSH_ASKPASS.
        (
            "GIT_ASKPASS".to_string(),
            ASKPASS_DISABLED_SENTINEL.to_string(),
        ),
    ];

    // The env-layered git config entries.
    let cfg: Vec<(&str, &str)> = vec![(
        "credential.https://github.com.helper",
        "!gh auth git-credential",
    )];

    // Append to any inherited GIT_CONFIG_* rather than overwriting index 0.
    let base: usize = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    for (i, (k, v)) in cfg.iter().enumerate() {
        let idx = base + i;
        out.push((format!("GIT_CONFIG_KEY_{idx}"), (*k).to_string()));
        out.push((format!("GIT_CONFIG_VALUE_{idx}"), (*v).to_string()));
    }
    out.push((
        "GIT_CONFIG_COUNT".to_string(),
        (base + cfg.len()).to_string(),
    ));
    out
}

/// The PROMPT-CLOSING SUBSET of [`non_interactive_git_env`], for git processes
/// the runner runs ITSELF (as opposed to the agent processes it spawns).
///
/// DERIVED BY FILTER from the one posture, never restated, so the two cannot
/// drift: it is exactly the entries that close a prompt layer, with the
/// `GIT_CONFIG_*` credential-helper FALLBACK dropped.
///
/// Why the split is one rule and not accretion: a spawned agent needs a way to
/// SUCCEED at an unregistered GitHub push, which is what the `gh` helper
/// fallback provides. The runner's own git subprocesses never need a credential
/// fallback — they are local operations, or they carry their own credential in
/// the URL (`commands/new_project.rs`) or an `http.extraHeader`
/// (`agent_pusher`). What they DO need is the guarantee they can never block,
/// and injecting read-time `GIT_CONFIG_*` overlay into the runner's own
/// `git config` reads would change what those reads see.
///
/// Applied at the single chokepoint every runner git invocation already passes
/// through — [`crate::process_helpers::no_window`] and
/// [`crate::process_helpers::tokio_no_window`] — because the alternative,
/// remembering it at ~100 call sites, is what left `commands/new_project.rs`'s
/// real `git push -u origin main` able to hang on a credential prompt while its
/// own timeout message conceded "git may be waiting on credentials".
pub fn prompt_proof_git_env() -> Vec<(String, String)> {
    non_interactive_git_env()
        .into_iter()
        .filter(|(k, _)| !k.starts_with("GIT_CONFIG_"))
        .collect()
}

/// The REPOSITORY-LOCAL git environment variables the runner scrubs from every
/// git child it starts: what `git rev-parse --local-env-vars` prints (git
/// 2.47.3) MINUS [`COMMAND_SCOPE_GIT_CONFIG_ENV`] — exactly the set git itself
/// clears when it crosses into another repository (a submodule). Any of them
/// inherited by a child makes git read a repository, index, object store or
/// config file OTHER than the one `-C` / `current_dir` names: `-C` does not
/// override an inherited `GIT_DIR`, so a runner started from a git hook (which
/// exports `GIT_DIR`) or any shell that exported these would answer — and
/// write — about the CALLER's repo. `GIT_CONFIG` (the legacy whole-config-FILE
/// override) stays on the list: it names a file, which is repo-locating.
/// `repo_local_git_env_covers_gits_own_list` pins this against the installed
/// git, so a git that grows the list fails a test rather than silently
/// reopening the hole.
///
/// Lives here, beside the credential posture, because it has consumers on both
/// sides of the lib/bin split: [`crate::process_helpers::no_window`] /
/// [`crate::process_helpers::tokio_no_window`] (compiled into BOTH crates)
/// apply it to every git subprocess the runner starts, and the bin-only
/// `git_trunk` resolver applies it to the commands its seam tests hand it.
pub const REPO_LOCAL_GIT_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// The names on git's `--local-env-vars` list that are NOT scrubbed, because
/// they carry COMMAND-SCOPE config (`git -c k=v`, and the `GIT_CONFIG_COUNT`
/// overlay with its numbered `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`
/// pairs, which are not on git's list at all) rather than a repository
/// location. git's own submodule code (`prepare_submodule_repo_env`) keeps
/// exactly these two when it clears the rest — measured: `GIT_CONFIG_COUNT=1
/// GIT_CONFIG_KEY_0=foo.bar … git -c baz.q=p submodule foreach` still sees
/// `COUNT=1` and `'baz.q'='p'` inside the submodule. Scrubbing them would drop
/// the operator's env-injected `safe.directory` (so `rev-parse` fails with
/// "dubious ownership" on a box that needs it) and the agent session's
/// env-injected credential helper / proxy / CA settings
/// ([`non_interactive_git_env`]), which a runner started from such a session
/// inherits and the drift probe's fetch / `ls-remote` need.
pub const COMMAND_SCOPE_GIT_CONFIG_ENV: &[&str] = &["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT"];

/// Remove every [`REPO_LOCAL_GIT_ENV`] variable from `cmd`'s child
/// environment, whether inherited from this process or set on `cmd` earlier.
/// Command-scope config ([`COMMAND_SCOPE_GIT_CONFIG_ENV`] and the numbered
/// `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs) passes through untouched,
/// as it does across git's own submodule boundary.
///
/// [`crate::process_helpers::no_window`] already applies this to every git it
/// builds; call it directly only on a command built some other way. A caller
/// that deliberately points git elsewhere sets the variable AFTER construction
/// (a later `.env` wins over this removal).
pub fn scrub_repo_local_git_env(cmd: &mut std::process::Command) {
    for var in REPO_LOCAL_GIT_ENV {
        cmd.env_remove(var);
    }
}

#[cfg(test)]
mod repo_local_env_tests {
    use super::*;

    #[test]
    fn the_scrub_removes_the_list_and_keeps_command_scope_config() {
        let mut cmd = std::process::Command::new("git");
        let kept = [
            ("GIT_CONFIG_PARAMETERS", "'baz.q'='p'"),
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "safe.directory"),
            ("GIT_CONFIG_VALUE_0", "*"),
            ("GIT_CONFIG_KEY_1", "credential.helper"),
            ("GIT_CONFIG_VALUE_1", "!gh auth git-credential"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ];
        for (k, v) in kept {
            cmd.env(k, v);
        }
        scrub_repo_local_git_env(&mut cmd);
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for var in REPO_LOCAL_GIT_ENV {
            assert!(
                envs.iter().any(|(k, v)| k == var && v.is_none()),
                "{var} must be removed; envs: {envs:?}"
            );
        }
        for (k, v) in kept {
            assert!(
                envs.iter()
                    .any(|(name, val)| name == k && val.as_deref() == Some(v)),
                "{k}={v} is command-scope, not repo-local, and must survive: {envs:?}"
            );
        }
    }

    /// The list is git's, not a hand-picked subset: every name the installed
    /// git reports as repository-local is on it, except the named
    /// command-scope exemption ([`COMMAND_SCOPE_GIT_CONFIG_ENV`] — config, not
    /// a repository location, and kept by git's own submodule boundary), which
    /// must itself be on git's list so it cannot exempt a name git never
    /// reported. Skipped only when no `git` can be spawned at all.
    #[test]
    fn repo_local_git_env_covers_gits_own_list() {
        let Ok(out) = std::process::Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
        else {
            eprintln!("git is not installed; skipping");
            return;
        };
        assert!(out.status.success(), "{out:?}");
        let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        assert!(
            names.iter().any(|n| n == "GIT_DIR"),
            "git's list must at least name GIT_DIR, or this test proves nothing: {names:?}"
        );
        for exempt in COMMAND_SCOPE_GIT_CONFIG_ENV {
            assert!(
                names.iter().any(|n| n == exempt),
                "exemption {exempt} is not on git's list: {names:?}"
            );
            assert!(
                !REPO_LOCAL_GIT_ENV.contains(exempt),
                "{exempt} is both exempt and scrubbed"
            );
        }
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| {
                !REPO_LOCAL_GIT_ENV.contains(&n.as_str())
                    && !COMMAND_SCOPE_GIT_CONFIG_ENV.contains(&n.as_str())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "REPO_LOCAL_GIT_ENV is missing git's {missing:?}"
        );
    }
}
