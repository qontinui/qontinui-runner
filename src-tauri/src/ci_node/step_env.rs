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
//! login. So the executor then SETS it, for every step, to an empty
//! per-dispatch directory ([`fresh_empty_config_dir`]), and a `claude` in a
//! step starts with no login. Removals are applied before any variable is set,
//! so that executor-owned value is set rather than removed.
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

/// The name of the per-dispatch empty config directory, under the dispatch
/// root (which cleanup removes whole).
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

/// (Re)create the empty `CLAUDE_CONFIG_DIR` a step gets, under the dispatch
/// root. Called before EVERY step, so a step that wrote into it cannot hand its
/// files to the next one. `Err` means no empty directory could be guaranteed:
/// the caller fails the step rather than run it with the variable pointing
/// somewhere that is not empty.
pub(crate) fn fresh_empty_config_dir(dispatch_root: &Path) -> Result<PathBuf, String> {
    let dir = dispatch_root.join(EMPTY_CONFIG_DIR_NAME);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("could not empty {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
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
    fn the_config_dir_is_recreated_empty_and_a_file_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fresh_empty_config_dir(tmp.path()).unwrap();
        assert_eq!(dir, tmp.path().join(".ci-claude-config"));
        assert!(dir.is_dir(), "the config dir was not created");

        std::fs::write(dir.join(".credentials.json"), "x").unwrap();
        let again = fresh_empty_config_dir(tmp.path()).unwrap();
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_dir(&again).unwrap().count(),
            0,
            "a file planted by one step survived into the next step's config dir"
        );

        let file_root = tmp.path().join("not-a-directory");
        std::fs::write(&file_root, "x").unwrap();
        let err = fresh_empty_config_dir(&file_root).unwrap_err();
        assert!(err.contains("could not create"), "got: {err}");
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
