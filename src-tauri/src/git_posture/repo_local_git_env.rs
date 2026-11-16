// The ONE copy of the repository-local git env list, read by three readers:
// `git_posture.rs` and `build.rs` pull it in with `include!` (a build script is
// its own crate and cannot `use` the crate's code), and
// `scripts/lib/repo-local-git-env.mjs` reads it as TEXT, taking every
// double-quoted GIT_ literal and refusing any count but 13. So a name appears
// in double quotes only inside the list below, and nothing above the list
// quotes one. No inner doc here: this file is spliced into two crates.

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
/// Lives in `git_posture`, beside the credential posture, because it has
/// consumers on both sides of the lib/bin split and outside the crate:
/// [`crate::process_helpers::no_window`] /
/// [`crate::process_helpers::tokio_no_window`] (owned by the lib and
/// re-exported to the bin) apply it to every git subprocess the runner starts;
/// the bin-only `git_trunk` resolver applies it to the commands its seam tests
/// hand it; `build.rs`'s `build_script_git` applies it to the build script's
/// git (the embedded sha and the provenance stamps); and `vite.config.ts`
/// `computeBuildId` and `scripts/frontend-provenance.mjs` `GIT_ENV` read it
/// through `scripts/lib/repo-local-git-env.mjs`. It sits in its own file so
/// that every one of them reads this list rather than a copy of it.
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
