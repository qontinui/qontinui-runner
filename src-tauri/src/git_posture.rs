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
