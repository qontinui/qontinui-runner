//! Long-lived Claude OAuth tokens (`claude setup-token`) held per account.
//!
//! A Claude account normally authenticates through `<config dir>/.credentials.json`:
//! an ~8 h access token plus a refresh token that is REPLACED on every renewal
//! (measured 2026-11-03 on merytshost: two accounts renewed and both refresh
//! tokens rotated). One login therefore cannot be copied between machines —
//! whichever copy renews first invalidates the other — and every machine needs
//! its own interactive `/login` per account, repeated whenever the refresh
//! token's own expiry (days to weeks) passes.
//!
//! `claude setup-token` issues a long-lived OAuth token instead, used through
//! the `CLAUDE_CODE_OAUTH_TOKEN` environment variable. Claude Code also reads
//! that variable from the `env` block of the account's own `settings.json`
//! (measured with Claude Code 2.1.318: an empty config dir whose `settings.json`
//! carried `env.CLAUDE_CODE_OAUTH_TOKEN` answered "401 OAuth access token is
//! invalid" for a bogus token, not "Not logged in"). Every launch path the
//! runner and the fleet's shell launchers have already pins `CLAUDE_CONFIG_DIR`
//! to the account dir, so a token stored there reaches all of them with no
//! spawn-side change. The token is installed there by
//! `qontinui-claude-config/scripts/claude-token.sh`.
//!
//! What the runner needs is to RECOGNISE such an account: account selection,
//! the usage probe, the warm path and the doctor all used to read only
//! `.credentials.json`, so an account holding only a long-lived token looked
//! logged out and was never selected. This module is the one reader they share.
//!
//! The value is never logged. Only a value shaped like a Claude OAuth token
//! (`sk-ant-oat…`) counts, so a placeholder or an emptied entry can never pass
//! for a login.

use std::path::{Path, PathBuf};

/// The variable Claude Code reads a long-lived OAuth token from.
pub const ENV_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Prefix every Claude OAuth token carries (`sk-ant-oat01-…`).
const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

/// The value as a usable token, or `None` for anything that is not one.
fn usable(value: &str) -> Option<String> {
    let v = value.trim();
    (v.starts_with(OAUTH_TOKEN_PREFIX) && v.len() > OAUTH_TOKEN_PREFIX.len()).then(|| v.to_string())
}

/// Path of the settings file a long-lived token for `config_dir` lives in.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// The long-lived token stored in `<config_dir>/settings.json` at
/// `env.CLAUDE_CODE_OAUTH_TOKEN`, if there is a usable one.
///
/// Checks `config_dir` itself only — never a fallback location — so a
/// per-account selection filter cannot collapse several accounts onto one
/// shared token (the same reason `has_valid_credentials` reads its own dir).
pub fn from_config_dir(config_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(settings_path(config_dir)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    usable(json.get("env")?.get(ENV_VAR)?.as_str()?)
}

/// The long-lived token a `claude` child with NO `CLAUDE_CONFIG_DIR` pin would
/// authenticate with: the process environment, then `$CLAUDE_CONFIG_DIR`'s
/// settings, then the default `~/.claude` settings.
pub fn ambient() -> Option<String> {
    ambient_with(
        std::env::var(ENV_VAR).ok(),
        std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
        dirs::home_dir(),
    )
}

/// [`ambient`] with its three inputs injected, so it is testable without
/// touching the process environment.
pub fn ambient_with(
    env_token: Option<String>,
    claude_config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<String> {
    if let Some(t) = env_token.as_deref().and_then(usable) {
        return Some(t);
    }
    if let Some(t) = claude_config_dir.as_deref().and_then(from_config_dir) {
        return Some(t);
    }
    home.map(|h| h.join(".claude"))
        .as_deref()
        .and_then(from_config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "sk-ant-oat01-abcdefghijklmnop";

    fn dir_with_settings(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("settings.json"), body).expect("write settings");
        dir
    }

    #[test]
    fn reads_the_token_from_the_settings_env_block() {
        let dir = dir_with_settings(&format!(r#"{{"env":{{"{ENV_VAR}":"{TOKEN}"}}}}"#));
        assert_eq!(from_config_dir(dir.path()).as_deref(), Some(TOKEN));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let dir = dir_with_settings(&format!(r#"{{"env":{{"{ENV_VAR}":"  {TOKEN}\n"}}}}"#));
        assert_eq!(from_config_dir(dir.path()).as_deref(), Some(TOKEN));
    }

    #[test]
    fn a_value_that_is_not_an_oauth_token_does_not_count() {
        for bogus in ["", "   ", "sk-ant-oat", "sk-ant-api03-xyz", "REPLACE_ME"] {
            let dir = dir_with_settings(&format!(r#"{{"env":{{"{ENV_VAR}":"{bogus}"}}}}"#));
            assert_eq!(
                from_config_dir(dir.path()),
                None,
                "{bogus:?} must not count"
            );
        }
    }

    #[test]
    fn absent_settings_or_entry_or_bad_json_is_none() {
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(from_config_dir(empty.path()), None);
        assert_eq!(from_config_dir(dir_with_settings("{}").path()), None);
        assert_eq!(
            from_config_dir(dir_with_settings(r#"{"env":{"OTHER":"x"}}"#).path()),
            None
        );
        assert_eq!(from_config_dir(dir_with_settings("{not json").path()), None);
        assert_eq!(
            from_config_dir(dir_with_settings(&format!(r#"{{"env":{{"{ENV_VAR}":42}}}}"#)).path()),
            None
        );
    }

    #[test]
    fn ambient_prefers_the_process_env_then_the_pinned_dir_then_home() {
        let pinned = dir_with_settings(&format!(
            r#"{{"env":{{"{ENV_VAR}":"sk-ant-oat01-pinned"}}}}"#
        ));
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(home.path().join(".claude")).expect("mkdir");
        std::fs::write(
            home.path().join(".claude").join("settings.json"),
            format!(r#"{{"env":{{"{ENV_VAR}":"sk-ant-oat01-home"}}}}"#),
        )
        .expect("write");

        let h = || Some(home.path().to_path_buf());
        let p = || Some(pinned.path().to_path_buf());
        assert_eq!(
            ambient_with(Some(TOKEN.into()), p(), h()).as_deref(),
            Some(TOKEN)
        );
        assert_eq!(
            ambient_with(None, p(), h()).as_deref(),
            Some("sk-ant-oat01-pinned")
        );
        assert_eq!(
            ambient_with(Some("not-a-token".into()), None, h()).as_deref(),
            Some("sk-ant-oat01-home")
        );
        assert_eq!(ambient_with(None, None, None), None);
    }
}
