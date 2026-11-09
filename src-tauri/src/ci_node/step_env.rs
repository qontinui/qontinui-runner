//! The environment every CI step loses, and the empty `CLAUDE_CONFIG_DIR` it
//! gets instead (plan `2026-11-07-runner-ci-node-step-env-scrub`).
//!
//! A step used to inherit the runner's whole process environment, so the
//! password variables the runner strips from every session child, any model
//! credential, and a `CLAUDE_CONFIG_DIR` pointing at a logged-in account all
//! reached repo-authored CI code. The executor ([`super::executor`]) now
//! removes, from EVERY step:
//!
//! - the model-credential class [`MODEL_CREDENTIAL_ENV`];
//! - every inherited key ending in [`MODEL_COMMAND_SUFFIX`], so a model command
//!   the host holds never reaches a step;
//! - the runner's own credential values,
//!   [`crate::terminal::CREDENTIAL_VALUE_ENV_VARS`]. That is the list
//!   runner#1995's `RunnerProcess::scrubbed_env` hands ci-exec, so this half
//!   does not change when the runner moves onto ci-exec.
//!
//! Removing `CLAUDE_CONFIG_DIR` alone would let a `claude` run inside a step
//! fall back to the default config dir under the inherited `HOME`: the owner's
//! login. So the executor then SETS it, for every step, to a directory of that
//! step's own, `<dispatch root>/.ci-claude-config/<step index>`, made just
//! before the step runs and real and empty at the moment it is made
//! ([`fresh_empty_config_dir`]). A `claude` in a step therefore starts with no
//! login, and a process an earlier step left running, writing where ITS
//! `CLAUDE_CONFIG_DIR` points, writes into that earlier step's directory,
//! which no later step is pointed at. The parent sits inside the dispatch root,
//! which the dispatch's cleanup removes whole. Removals are applied before any
//! variable is set, so that executor-owned value is set rather than removed.
//!
//! ## What this does not do
//!
//! `HOME` is still inherited. The scrub stops a `claude` in a step from picking
//! up the owner's login BY DEFAULT; it does not stop repo code from finding
//! `~/.claude*`, or any other file the node owner can read, by path. Nor does
//! it stop a process an earlier step left running from replacing a later
//! step's directory with a link after that directory is made (see
//! [`fresh_empty_config_dir`]). A step
//! runs as the node owner's OS user, and this module is not a sandbox. Its
//! scope is parity with ci-exec's step scrub (plan
//! `2026-10-13-ci-node-runs-model-dependent-eval-steps` D11), plus the
//! per-step directory above, which is stricter than ci-exec's one directory
//! per dispatch.
//!
//! ## A port, with a known end
//!
//! This is a port of qontinui-schemas `ci-exec/src/model/scrub.rs` (blob
//! `9224131db`) and of ci-exec `manifest.rs`'s `_MODEL_CMD` suffix test, for the
//! runner's own copy of the CI executor. It is deleted together with
//! `executor.rs` when the runner moves onto ci-exec (runner#1995 or a
//! successor), whose `run_step` applies the same scrub. If plan
//! `2026-10-13-ci-node-runs-model-dependent-eval-steps`'s D2 fallback fires
//! instead, this module IS the runner's permanent step scrub, and that fallback
//! extends it rather than adding a second copy.
//!
//! One deliberate departure from a literal port: an inherited key is removed
//! under its own `OsString`, never re-encoded. ci-exec's `step_env_removals`
//! at that blob converts it lossily, so a key that is not valid Unicode and
//! ends in the suffix is "removed" there under a spelling that names a
//! different variable.
//!
//! Provisioning children (tool installs, sibling git, service containers) do
//! not pass through this module. Plan
//! `2026-11-06-ci-node-provisioning-children-inherit-only-an-allowlisted-environment`
//! owns their environment, which it builds from an allowlist (`child_env`)
//! rather than from this scrub.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The model-credential class, removed from every CI step's environment.
///
/// Spelled as ci-exec's `MODEL_CREDENTIAL_ENV` spells it, in its order and with
/// its grouping. `tests::the_credential_class_is_exactly_the_ported_list` pins
/// every name as a literal, and
/// `tests::every_gateway_credential_name_is_in_the_class` pins the runner's own
/// gateway lists (`crate::model_gateway`) inside it.
pub(crate) const MODEL_CREDENTIAL_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CONFIG_DIR",
    // The provider-switch set.
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    // Credential-bearing names read off the installed CLI (`claude` 2.1.296,
    // `strings` of the binary, 2026-10-13): the Bedrock bearer token, the
    // Foundry key/token, the first-party AWS key, the refresh and session
    // tokens, the gateway token and the identity token.
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "ANTHROPIC_AWS_API_KEY",
    "ANTHROPIC_IDENTITY_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
    "CLAUDE_CODE_GATEWAY_TOKEN",
    // Provider switches the same binary reads beside the ones above: each
    // re-routes a `claude` in a step away from the (absent) login.
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_SKIP_MANTLE_AUTH",
    "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
    // Other providers' keys.
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// The suffix of an inherited key that names a model command. Every inherited
/// key ending in it is removed from a step, so a model command the host holds
/// never chooses what a step runs as its model. ci-exec `manifest.rs`'s
/// `MODEL_COMMAND_SUFFIX`.
const MODEL_COMMAND_SUFFIX: &str = "_MODEL_CMD";

/// The variable the executor pins, for every step, to an empty directory.
pub(crate) const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// The per-dispatch parent of every step's empty config directory, under the
/// dispatch root (which cleanup removes whole).
const EMPTY_CONFIG_DIR_NAME: &str = ".ci-claude-config";

/// Whether `key` names a model command. On Windows, where environment names
/// are case-insensitive, the suffix is too: a `docs_eval_model_cmd` there IS
/// `DOCS_EVAL_MODEL_CMD`.
fn is_model_command_key(key: &OsStr) -> bool {
    model_command_suffix_matches(key, cfg!(windows))
}

/// The suffix test, with case-insensitivity as a parameter so both arms are
/// testable on any OS.
///
/// It reads the lossy view of the key. The suffix is ASCII and lossy decoding
/// replaces only invalid sequences, never an ASCII byte, so that view ends in
/// the suffix exactly when the key does. Bytes are compared rather than a
/// `str` sliced, which could panic off a char boundary.
fn model_command_suffix_matches(key: &OsStr, case_insensitive: bool) -> bool {
    let key = key.to_string_lossy();
    let key = key.as_bytes();
    let suffix = MODEL_COMMAND_SUFFIX.as_bytes();
    let Some(tail) = key
        .len()
        .checked_sub(suffix.len())
        .and_then(|start| key.get(start..))
    else {
        return false;
    };
    if case_insensitive {
        tail.eq_ignore_ascii_case(suffix)
    } else {
        tail == suffix
    }
}

/// Every variable to remove from a step's environment, given the keys the
/// runner's own process holds and the host's credential list: the class, the
/// host list, and every inherited key that names a model command.
///
/// A matched inherited key is returned AS INHERITED, never re-encoded, so a key
/// that is not valid Unicode is removed under its own spelling. Sorted and
/// de-duplicated.
pub(crate) fn step_env_removals(
    inherited_keys: impl IntoIterator<Item = OsString>,
    host_scrubbed: &[&str],
) -> Vec<OsString> {
    let mut out: Vec<OsString> = MODEL_CREDENTIAL_ENV
        .iter()
        .chain(host_scrubbed)
        .map(|k| OsString::from(*k))
        .collect();
    out.extend(
        inherited_keys
            .into_iter()
            .filter(|k| is_model_command_key(k)),
    );
    out.sort();
    out.dedup();
    out
}

/// The removals for a step run from this process: its own environment's keys,
/// with the runner's credential values as the host list. Computed per step,
/// as ci-exec does, so a variable the runner gained mid-dispatch is covered.
pub(crate) fn current_step_env_removals() -> Vec<OsString> {
    step_env_removals(
        std::env::vars_os().map(|(k, _)| k),
        crate::terminal::CREDENTIAL_VALUE_ENV_VARS,
    )
}

/// (Re)create the empty `CLAUDE_CONFIG_DIR` of the step at `step_index`:
/// `<dispatch_root>/.ci-claude-config/<step_index>`, a directory of that step's
/// own, so a process an earlier step left running, writing where ITS
/// `CLAUDE_CONFIG_DIR` points, writes into the earlier step's directory and
/// not into this one.
///
/// Nothing found at either path is followed. The parent `.ci-claude-config` is
/// kept if it is a real directory (it holds the earlier steps' directories);
/// anything else there (a regular file, or a symlink, broken or not) is
/// unlinked and the parent made anew. At the step's own path, a real directory
/// is removed whole and anything else is unlinked. Both are then made with
/// `create_dir`, never `create_dir_all`: if anything appears at the path in
/// between, a link planted after the removal included, the creation fails, and
/// with it the step, rather than the link being followed. What is guaranteed is
/// that the step's directory is real and empty at the moment it is made.
///
/// Not closed here (each would need directory-fd-relative calls, or a directory
/// the node owner cannot write):
/// - the parent swapped for a link after its check and before the step's
///   directory is made, which then makes that directory behind the link;
/// - the parent or the step's directory removed and replaced by a link AFTER
///   the step's directory is made and before the step runs, by a process an
///   earlier step left running: the step is then pointed through that link.
///
/// Both need a process running as the node owner, which can read the owner's
/// login directly anyway (see the module docs).
///
/// `Err` means no empty directory could be guaranteed: the caller fails the
/// step rather than run it with the variable pointing somewhere that is not
/// empty. The dispatch root itself must already exist; it holds the worktree.
pub(crate) fn fresh_empty_config_dir(
    dispatch_root: &Path,
    step_index: usize,
) -> Result<PathBuf, String> {
    fresh_empty_config_dir_with(dispatch_root, step_index, |_| {})
}

/// [`fresh_empty_config_dir`], with a hook run in the window between the step
/// directory's removal and its creation: the window a planted link would use.
/// Production passes a no-op; a test plants a link there.
fn fresh_empty_config_dir_with(
    dispatch_root: &Path,
    step_index: usize,
    between_remove_and_create: impl FnOnce(&Path),
) -> Result<PathBuf, String> {
    let parent = dispatch_root.join(EMPTY_CONFIG_DIR_NAME);
    match inspect(&parent)? {
        Some(meta) if meta.is_dir() => {}
        Some(meta) => {
            remove_non_directory(&parent, &meta)
                .map_err(|e| format!("could not remove {}: {e}", parent.display()))?;
            create_new_dir(&parent)?;
        }
        None => create_new_dir(&parent)?,
    }

    let dir = parent.join(step_index.to_string());
    match inspect(&dir)? {
        Some(meta) if meta.is_dir() => std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("could not empty {}: {e}", dir.display()))?,
        Some(meta) => remove_non_directory(&dir, &meta)
            .map_err(|e| format!("could not remove {}: {e}", dir.display()))?,
        None => {}
    }
    between_remove_and_create(&dir);
    create_new_dir(&dir)?;
    Ok(dir)
}

/// What is at `path`, read with `symlink_metadata` (a link is reported as a
/// link, never followed). `None` when nothing is there; `Err` when it cannot be
/// read, which includes a path whose parent is not a directory.
fn inspect(path: &Path) -> Result<Option<std::fs::Metadata>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not inspect {}: {e}", path.display())),
    }
}

/// Make `path` as a NEW directory with `create_dir`, which fails when anything
/// already exists there (a link, broken or not, included) instead of reusing
/// or following it, as `create_dir_all` would.
fn create_new_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir(path).map_err(|e| format!("could not create {}: {e}", path.display()))
}

/// Remove an entry `symlink_metadata` says is not a real directory: a regular
/// file or a symlink, unlinked itself and never followed. Windows refuses
/// `remove_file` on a directory symlink or junction, which `remove_dir` removes
/// (again without following it).
#[cfg_attr(not(windows), allow(unused_variables))]
fn remove_non_directory(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if meta.file_type().is_symlink_dir() {
            return std::fs::remove_dir(path);
        }
    }
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The class, spelled out as literals so an edit to [`MODEL_CREDENTIAL_ENV`]
    /// has to edit this too. Never derived from the constant: a test written
    /// against its own constant pins nothing.
    const PORTED_CLASS: [&str; 31] = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
        "ANTHROPIC_BEDROCK_BASE_URL",
        "ANTHROPIC_VERTEX_BASE_URL",
        "ANTHROPIC_FOUNDRY_BASE_URL",
        "ANTHROPIC_FOUNDRY_RESOURCE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "ANTHROPIC_FOUNDRY_API_KEY",
        "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
        "ANTHROPIC_AWS_API_KEY",
        "ANTHROPIC_IDENTITY_TOKEN",
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
        "CLAUDE_CODE_GATEWAY_TOKEN",
        "CLAUDE_CODE_USE_MANTLE",
        "CLAUDE_CODE_USE_GATEWAY",
        "CLAUDE_CODE_SKIP_MANTLE_AUTH",
        "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    ];

    /// The runner's three plaintext password variables, as literals.
    const PASSWORDS: [&str; 3] = [
        "QONTINUI_OPERATOR2_PASSWORD",
        "QONTINUI_TEST_LOGIN_PASSWORD",
        "QONTINUI_TEST_AUTO_LOGIN_PASSWORD",
    ];

    fn has(removals: &[OsString], name: &str) -> bool {
        removals.iter().any(|k| k.as_os_str() == OsStr::new(name))
    }

    /// Equal to the ported list in BOTH directions, 31 names, none twice. A
    /// one-direction check (every literal is present) would pass with a name
    /// added, and a missing literal in the test would pass with one removed.
    #[test]
    fn the_credential_class_is_exactly_the_ported_list() {
        let expected: BTreeSet<&str> = PORTED_CLASS.into_iter().collect();
        assert_eq!(
            expected.len(),
            31,
            "the literal list names a variable twice"
        );
        assert_eq!(
            MODEL_CREDENTIAL_ENV.len(),
            31,
            "the class must hold exactly the 31 ported names"
        );
        let actual: BTreeSet<&str> = MODEL_CREDENTIAL_ENV.iter().copied().collect();
        assert_eq!(
            actual.len(),
            MODEL_CREDENTIAL_ENV.len(),
            "MODEL_CREDENTIAL_ENV names a variable twice"
        );
        let missing: Vec<&&str> = expected.difference(&actual).collect();
        let unexpected: Vec<&&str> = actual.difference(&expected).collect();
        assert!(
            missing.is_empty() && unexpected.is_empty(),
            "the class drifted from the port: missing {missing:?}, unexpected {unexpected:?}"
        );
    }

    #[test]
    fn removals_cover_the_class_the_host_list_and_inherited_model_commands() {
        let inherited = [
            "PATH",
            "HOME",
            "DOCS_EVAL_MODEL_CMD",
            "REFERENCE_EVAL_MODEL_CMD",
        ]
        .map(OsString::from);
        let r = step_env_removals(inherited, &PASSWORDS);
        for name in PORTED_CLASS {
            assert!(has(&r, name), "the class name {name} is not removed");
        }
        for name in ["DOCS_EVAL_MODEL_CMD", "REFERENCE_EVAL_MODEL_CMD"] {
            assert!(
                has(&r, name),
                "the inherited model command {name} is not removed"
            );
        }
        for name in PASSWORDS {
            assert!(has(&r, name), "the host credential {name} is not removed");
        }
        for name in ["PATH", "HOME"] {
            assert!(
                !has(&r, name),
                "{name} is outside every class and must stay"
            );
        }
    }

    /// Exactly the class when nothing else is given; a duplicated inherited key
    /// and a host name already in the class each appear once; the output is
    /// sorted.
    #[test]
    fn removals_are_the_bare_class_sorted_and_deduplicated() {
        let bare = step_env_removals(Vec::<OsString>::new(), &[]);
        assert_eq!(
            bare.len(),
            31,
            "nothing inherited, no host list: just the class"
        );
        for name in PORTED_CLASS {
            assert!(has(&bare, name), "{name}");
        }

        let r = step_env_removals(
            ["X_MODEL_CMD", "X_MODEL_CMD"].map(OsString::from),
            &["ANTHROPIC_API_KEY"],
        );
        let count = |name: &str| {
            r.iter()
                .filter(|k| k.as_os_str() == OsStr::new(name))
                .count()
        };
        assert_eq!(count("X_MODEL_CMD"), 1, "a duplicated inherited key");
        assert_eq!(
            count("ANTHROPIC_API_KEY"),
            1,
            "a host name already in the class"
        );
        assert_eq!(r.len(), 32);
        assert!(
            r.windows(2).all(|w| w[0] < w[1]),
            "the removals are not sorted and de-duplicated"
        );
    }

    /// Both arms on every OS, through the parameterised matcher. `_MODEL_CMD`
    /// alone matches (ci-exec's boundary); the suffix must END the key.
    #[test]
    fn the_model_command_suffix_matches_on_both_arms() {
        let m = |key: &str, ci: bool| model_command_suffix_matches(OsStr::new(key), ci);
        for ci in [false, true] {
            assert!(m("X_MODEL_CMD", ci), "X_MODEL_CMD (case_insensitive={ci})");
            assert!(m("_MODEL_CMD", ci), "_MODEL_CMD (case_insensitive={ci})");
            assert!(!m("MODEL_CMD", ci), "MODEL_CMD (case_insensitive={ci})");
            assert!(
                !m("X_MODEL_CMDS", ci),
                "X_MODEL_CMDS (case_insensitive={ci})"
            );
            assert!(!m("", ci), "the empty key (case_insensitive={ci})");
        }
        assert!(
            !m("x_model_cmd", false),
            "lower case must not match case-sensitively"
        );
        assert!(
            m("x_model_cmd", true),
            "lower case must match case-insensitively"
        );
    }

    /// A key that is not valid Unicode is removed under its OWN bytes. A lossy
    /// round trip would hand `env_remove` a different variable's name.
    #[cfg(unix)]
    #[test]
    fn a_non_unicode_inherited_model_command_is_removed_under_its_own_spelling() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let key = OsString::from_vec(b"P\xff_MODEL_CMD".to_vec());
        let r = step_env_removals([key], &[]);
        assert!(
            r.iter().any(|k| k.as_bytes() == b"P\xff_MODEL_CMD"),
            "the inherited spelling is not in the removals"
        );
        assert!(
            !has(&r, "P\u{fffd}_MODEL_CMD"),
            "the removals carry the lossy spelling, which names another variable"
        );
    }

    #[test]
    fn each_step_gets_its_own_config_dir_recreated_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let first = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(first, tmp.path().join(".ci-claude-config").join("0"));
        assert!(first.is_dir(), "the config dir was not created");

        std::fs::write(first.join(".credentials.json"), "x").unwrap();
        let again = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(again, first);
        assert_eq!(
            std::fs::read_dir(&again).unwrap().count(),
            0,
            "a file planted in a step's config dir survived its recreation"
        );

        let second = fresh_empty_config_dir(tmp.path(), 1).unwrap();
        assert_eq!(second, tmp.path().join(".ci-claude-config").join("1"));
        assert_ne!(second, first, "two steps must never share a config dir");
    }

    #[test]
    fn a_dispatch_root_that_is_a_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file_root = tmp.path().join("not-a-directory");
        std::fs::write(&file_root, "x").unwrap();
        let err = fresh_empty_config_dir(&file_root, 0).unwrap_err();
        assert!(err.contains(".ci-claude-config"), "got: {err}");
    }

    /// A regular file where the step's config dir goes is replaced by an empty
    /// directory, rather than failing the step.
    #[test]
    fn a_regular_file_at_the_config_dir_path_is_replaced_by_an_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".ci-claude-config").join("0");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "planted").unwrap();

        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(dir, path);
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "the planted file was not replaced by a directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    /// A symlink where the step's config dir goes is UNLINKED and replaced by a
    /// real empty directory, whether it is broken or points at a directory: the
    /// directory behind the link (an owner's `~/.claude`, say) is never
    /// emptied through it, and the step is never pointed into it.
    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_config_dir_path_is_unlinked_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        std::fs::create_dir_all(&parent).unwrap();

        std::os::unix::fs::symlink(tmp.path().join("nowhere"), parent.join("0")).unwrap();
        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "the broken link was not replaced by a real directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        let login = tmp.path().join("owner-claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join(".credentials.json"), "x").unwrap();
        std::os::unix::fs::symlink(&login, parent.join("1")).unwrap();
        let dir = fresh_empty_config_dir(tmp.path(), 1).unwrap();
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "the link to a directory was not replaced by a real directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(
            login.join(".credentials.json").exists(),
            "the directory behind the link was emptied through it"
        );
    }

    /// The names a directory holds, sorted: what "untouched" is checked against.
    fn entries(dir: &Path) -> Vec<OsString> {
        let mut names: Vec<OsString> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// THE RACE: a link planted at the step's path AFTER its removal and before
    /// its creation (the seam runs in exactly that window). `create_dir` finds
    /// the path taken and fails, so the step fails instead of being pointed
    /// through the link, and the link is left as planted: refused, not removed
    /// or replaced. This is the test that catches a `create_dir_all` there,
    /// which returns Ok through the link and turns it red.
    #[cfg(unix)]
    #[test]
    fn a_link_planted_after_the_removal_fails_the_call_and_is_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let login = tmp.path().join("owner-claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join(".credentials.json"), "x").unwrap();

        let err = fresh_empty_config_dir_with(tmp.path(), 0, |leaf| {
            std::os::unix::fs::symlink(&login, leaf).unwrap();
        })
        .expect_err("a link planted after the removal was followed");
        assert!(
            err.contains("could not create") && err.contains(".ci-claude-config"),
            "got: {err}"
        );
        let leaf = tmp.path().join(".ci-claude-config").join("0");
        assert!(
            std::fs::symlink_metadata(&leaf)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the planted link is no longer a link: it was removed or replaced, not refused"
        );
        assert_eq!(
            entries(&login),
            [OsString::from(".credentials.json")],
            "the link target was touched"
        );
    }

    /// A parent `.ci-claude-config` that is a symlink, to a directory holding a
    /// login or to nothing, is unlinked and made anew as a real directory: the
    /// step's directory lands inside the dispatch root, never behind the link,
    /// and the link's target is left exactly as it was.
    #[cfg(unix)]
    #[test]
    fn a_parent_that_is_a_symlink_is_unlinked_and_made_anew_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        let login = tmp.path().join("owner-claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join(".credentials.json"), "x").unwrap();
        std::os::unix::fs::symlink(&login, &parent).unwrap();

        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(dir, parent.join("0"));
        assert!(
            std::fs::symlink_metadata(&parent).unwrap().is_dir(),
            "the parent link was not replaced by a real directory"
        );
        assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(
            entries(&login),
            [OsString::from(".credentials.json")],
            "the directory behind the parent link was touched"
        );

        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &parent).unwrap();
        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert!(
            std::fs::symlink_metadata(&parent).unwrap().is_dir(),
            "a broken parent link was not replaced by a real directory"
        );
        assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
        assert!(
            !tmp.path().join("nowhere").exists(),
            "the broken link's target was created through it"
        );
    }

    /// A parent `.ci-claude-config` that is a regular file is replaced by a real
    /// directory rather than failing the step.
    #[test]
    fn a_parent_that_is_a_regular_file_is_replaced_by_a_real_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        std::fs::write(&parent, "planted").unwrap();

        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(dir, parent.join("0"));
        assert!(
            std::fs::symlink_metadata(&parent).unwrap().is_dir(),
            "the planted file was not replaced by a directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    /// Make `link` a junction to `target` with `cmd /C mklink /J`, and check
    /// that Rust sees it as a link: a fixture that made something else would
    /// let the tests below pass without exercising the link arm at all.
    #[cfg(windows)]
    fn mklink_junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .status()
            .expect("cmd runs");
        assert!(status.success(), "mklink /J failed: {status}");
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "mklink /J made something Rust does not see as a link"
        );
    }

    /// Windows: a JUNCTION at the step's path (which `remove_file` refuses) is
    /// removed as a link and replaced by a real empty directory; the junction's
    /// target keeps its file.
    #[cfg(windows)]
    #[test]
    fn a_junction_at_the_config_dir_path_is_unlinked_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        std::fs::create_dir_all(&parent).unwrap();
        let login = tmp.path().join("owner-claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join(".credentials.json"), "x").unwrap();
        let leaf = parent.join("0");
        mklink_junction(&leaf, &login);

        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(dir, leaf);
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "the junction was not replaced by a real directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(
            entries(&login),
            [OsString::from(".credentials.json")],
            "the junction's target was touched"
        );
    }

    /// Windows: a parent `.ci-claude-config` that is a JUNCTION to a directory
    /// holding a login is removed as a link and made anew as a real directory.
    /// The step's directory is made inside that real parent, never behind the
    /// junction, and the login directory keeps exactly its file.
    #[cfg(windows)]
    #[test]
    fn a_parent_that_is_a_junction_is_unlinked_and_made_anew_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let login = tmp.path().join("owner-claude");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join(".credentials.json"), "x").unwrap();
        let parent = tmp.path().join(".ci-claude-config");
        mklink_junction(&parent, &login);

        let dir = fresh_empty_config_dir(tmp.path(), 0).unwrap();
        assert_eq!(dir, parent.join("0"));
        let meta = std::fs::symlink_metadata(&parent).unwrap();
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "the parent junction was not replaced by a real directory"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(
            entries(&login),
            [OsString::from(".credentials.json")],
            "the directory behind the parent junction was touched"
        );
    }

    /// Every credential or route the runner's model gateway knows about is in
    /// the class, so a name the runner learns there cannot outrun the step
    /// scrub. 15 names at authoring; the floor keeps the check from passing
    /// over emptied lists.
    #[test]
    fn every_gateway_credential_name_is_in_the_class() {
        use crate::model_gateway as gateway;
        let names: Vec<&str> = gateway::SHADOWING_CREDENTIAL_ENV
            .iter()
            .chain(gateway::PROVIDER_SWITCH_ENV)
            .copied()
            .chain([gateway::CUSTOM_HEADERS_ENV, gateway::BASE_URL_ENV])
            .collect();
        assert!(names.len() >= 15, "the gateway lists shrank: {names:?}");
        let missing: Vec<&str> = names
            .into_iter()
            .filter(|n| !MODEL_CREDENTIAL_ENV.contains(n))
            .collect();
        assert!(
            missing.is_empty(),
            "a gateway credential is not in the step scrub's class: {missing:?}"
        );
    }
}
