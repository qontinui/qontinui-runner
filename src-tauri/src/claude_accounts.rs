//! Machine-global Claude account roster store (`claude-accounts.json`).
//!
//! The Claude account roster — which Claude config dirs exist on this
//! machine, the account-selection mode, the optional manual `config_dir`
//! pin, and per-account launch commands — describes a MACHINE-GLOBAL fact,
//! not a per-runner-instance one. Storing it only in the per-instance
//! `settings.json` broke supervisor-spawned temp/named runners: they boot
//! with `QONTINUI_CONFIG_DIR=<config>/com.qontinui.runner/instances/<id>`,
//! load a fresh empty settings file, see an empty roster, and the spawned
//! `claude` falls back to the ambient `~/.claude` instead of least-usage
//! rotation across the configured accounts.
//!
//! This module owns the dedicated `claude-accounts.json` file at the
//! UNSCOPED canonical path `dirs::config_dir()/com.qontinui.runner/`,
//! deliberately IGNORING `QONTINUI_CONFIG_DIR` — the same resolution rule
//! as `active_instances.json` in `instance_manager.rs`. `load_settings()`
//! overlays the four roster fields from this file onto the in-memory
//! `Settings` for EVERY instance (primary + temp + named). Precedence is
//! load-bearing: when the file EXISTS, the overlay overwrites all four
//! fields UNCONDITIONALLY (not merge-if-non-empty) — per-instance
//! `settings.json` files keep accumulating stale shadow copies of the
//! roster via whole-`Settings` saves, and those shadows are harmless only
//! because the overlay always wins. When the file is ABSENT, per-instance
//! values load untouched (legacy behavior).
//!
//! Concurrency model: LAST-WRITER-WINS. Every write goes through
//! `crate::fs_atomic::atomic_write` (temp file + rename), so concurrent
//! runner instances can never torn-write or corrupt the file — but a
//! slower writer overwrites a faster one's roster wholesale. Roster edits
//! are rare operator-driven actions, so this is acceptable.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::settings::AccountSelectionMode;

const CLAUDE_ACCOUNTS_FILE: &str = "claude-accounts.json";

/// On-disk shape of `claude-accounts.json`: exactly the four machine-global
/// roster fields, with types matching the corresponding `Settings` fields
/// (`Settings.claude_config_dirs`, `Settings.ai.claude_cli.account_selection_mode`,
/// `Settings.ai.claude_cli.config_dir`, `Settings.claude_account_launch_commands`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeAccountsFile {
    /// Directories containing Claude Code configs (each with a `projects/` subdir).
    #[serde(default)]
    pub claude_config_dirs: Vec<String>,
    /// How to pick the account when multiple config dirs exist (snake_case on disk).
    #[serde(default)]
    pub account_selection_mode: AccountSelectionMode,
    /// Manual `CLAUDE_CONFIG_DIR` pin (only meaningful in `Manual` mode).
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Custom launch command per account config dir (key: config_dir path).
    #[serde(default)]
    pub claude_account_launch_commands: HashMap<String, String>,
}

impl ClaudeAccountsFile {
    /// True when no roster data is present (selection mode alone — a plain
    /// default — does not count as roster data).
    fn is_empty_roster(&self) -> bool {
        self.claude_config_dirs.is_empty()
            && self.config_dir.is_none()
            && self.claude_account_launch_commands.is_empty()
    }
}

/// Pure path builder: `<config_root>/com.qontinui.runner/claude-accounts.json`.
///
/// Split out from [`claude_accounts_file_path`] so the path contract is
/// unit-testable without touching process env or the real config dir.
fn accounts_path_under(config_root: &Path) -> PathBuf {
    config_root
        .join("com.qontinui.runner")
        .join(CLAUDE_ACCOUNTS_FILE)
}

/// Canonical machine-global path to `claude-accounts.json`.
///
/// ALWAYS rooted at `dirs::config_dir()` — `QONTINUI_CONFIG_DIR` is
/// deliberately ignored (unlike `settings::get_settings_path`), so every
/// instance on the machine resolves the SAME file. Mirrors the unscoped
/// `session_file_path` resolver for `active_instances.json`.
pub fn claude_accounts_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| accounts_path_under(&d))
}

/// Unscoped canonical `settings.json` (the PRIMARY runner's file) — the
/// migration source. Must NOT honor `QONTINUI_CONFIG_DIR`: a secondary
/// running the migration has to probe the primary's file, not its own
/// instance-scoped copy.
fn unscoped_settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("com.qontinui.runner").join("settings.json"))
}

/// Load the roster from `path`. Fail-open: missing or corrupt file → `None`
/// (never an error — settings load must always succeed).
fn load_from(path: &Path) -> Option<ClaudeAccountsFile> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!(
                "Failed to read {} ({}); falling back to per-instance roster",
                path.display(),
                e
            );
            return None;
        }
    };
    match serde_json::from_str::<ClaudeAccountsFile>(&contents) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!(
                "Corrupt {} ({}); ignoring it (fail-open) — per-instance roster applies",
                path.display(),
                e
            );
            None
        }
    }
}

/// Load the machine-global roster, or `None` when absent/corrupt/unresolvable.
pub fn load() -> Option<ClaudeAccountsFile> {
    load_from(&claude_accounts_file_path()?)
}

/// Atomically persist the roster to `path` (parent dir created if needed).
fn save_to(path: &Path, file: &ClaudeAccountsFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    let contents = serde_json::to_string_pretty(file)
        .map_err(|e| format!("Failed to serialize claude-accounts: {}", e))?;
    crate::fs_atomic::atomic_write(path, contents.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

/// Atomically persist the roster to the canonical machine-global path.
pub fn save(file: &ClaudeAccountsFile) -> Result<(), String> {
    let path = claude_accounts_file_path()
        .ok_or_else(|| "Failed to resolve config directory for claude-accounts.json".to_string())?;
    save_to(&path, file)
}

/// One-shot best-effort migration: when `claude-accounts.json` is ABSENT but
/// the unscoped canonical `settings.json` holds a NON-EMPTY roster, seed the
/// machine-global file from it once. Returns `true` only when a seed was
/// written. Never errors — migration failure just means legacy behavior.
fn migrate_seed(accounts_path: &Path, unscoped_settings: &Path) -> bool {
    if accounts_path.exists() {
        return false;
    }
    let contents = match std::fs::read_to_string(unscoped_settings) {
        Ok(c) => c,
        Err(_) => return false, // no unscoped settings.json — nothing to migrate
    };

    // Minimal probe of just the roster fields. Parsing the full `Settings`
    // here would couple the migration to every settings field's parseability;
    // the probe stays valid even if unrelated fields are malformed-by-schema.
    #[derive(Deserialize, Default)]
    struct CliProbe {
        #[serde(default)]
        config_dir: Option<String>,
        #[serde(default)]
        account_selection_mode: AccountSelectionMode,
    }
    #[derive(Deserialize, Default)]
    struct AiProbe {
        #[serde(default)]
        claude_cli: CliProbe,
    }
    #[derive(Deserialize)]
    struct SettingsProbe {
        #[serde(default)]
        claude_config_dirs: Vec<String>,
        #[serde(default)]
        claude_account_launch_commands: HashMap<String, String>,
        #[serde(default)]
        ai: AiProbe,
    }

    let probe: SettingsProbe = match serde_json::from_str(&contents) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Skipping claude-accounts migration: cannot parse {} ({})",
                unscoped_settings.display(),
                e
            );
            return false;
        }
    };

    let seed = ClaudeAccountsFile {
        claude_config_dirs: probe.claude_config_dirs,
        account_selection_mode: probe.ai.claude_cli.account_selection_mode,
        config_dir: probe.ai.claude_cli.config_dir,
        claude_account_launch_commands: probe.claude_account_launch_commands,
    };
    if seed.is_empty_roster() {
        return false; // empty roster — leave legacy per-instance behavior intact
    }
    match save_to(accounts_path, &seed) {
        Ok(()) => {
            info!(
                "Seeded machine-global {} from the unscoped settings.json roster \
                 ({} config dir(s))",
                accounts_path.display(),
                seed.claude_config_dirs.len()
            );
            true
        }
        Err(e) => {
            warn!("Failed to seed claude-accounts.json: {}", e);
            false
        }
    }
}

/// Load the machine-global roster, running the one-shot migration first
/// (at most once per process — `load_settings` calls this on every load).
pub fn load_with_migration() -> Option<ClaudeAccountsFile> {
    static MIGRATE_ONCE: std::sync::Once = std::sync::Once::new();
    MIGRATE_ONCE.call_once(|| {
        if let (Some(accounts), Some(settings)) =
            (claude_accounts_file_path(), unscoped_settings_path())
        {
            migrate_seed(&accounts, &settings);
        }
    });
    load()
}

/// Overwrite the four roster fields on `settings` from `roster` —
/// UNCONDITIONALLY (even with empty values): when `claude-accounts.json`
/// exists it is the single source of truth, and the stale shadow copies in
/// per-instance settings.json files must never win. Pure so the precedence
/// contract is unit-testable.
pub fn apply_roster_overlay(settings: &mut crate::settings::Settings, roster: ClaudeAccountsFile) {
    settings.claude_config_dirs = roster.claude_config_dirs;
    settings.claude_account_launch_commands = roster.claude_account_launch_commands;
    settings.ai.claude_cli.config_dir = roster.config_dir;
    settings.ai.claude_cli.account_selection_mode = roster.account_selection_mode;
}

/// Read-modify-write the machine-global roster file (last-writer-wins).
///
/// When the file is absent (and the migration found nothing to seed), the
/// starting point is the CURRENT effective roster from `load_settings()` —
/// not `Default` — so the first single-field write doesn't drop sibling
/// roster fields that so far only lived in a per-instance settings.json.
pub fn update(mutate: impl FnOnce(&mut ClaudeAccountsFile)) -> Result<(), String> {
    let mut file = load_with_migration().unwrap_or_else(roster_from_current_settings);
    mutate(&mut file);
    save(&file)
}

/// Snapshot the roster fields from the current effective settings.
fn roster_from_current_settings() -> ClaudeAccountsFile {
    let s = crate::settings::load_settings();
    ClaudeAccountsFile {
        claude_config_dirs: s.claude_config_dirs,
        account_selection_mode: s.ai.claude_cli.account_selection_mode,
        config_dir: s.ai.claude_cli.config_dir,
        claude_account_launch_commands: s.claude_account_launch_commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_builder_is_unscoped_canonical_shape() {
        let p = accounts_path_under(Path::new("/cfg"));
        assert_eq!(
            p,
            Path::new("/cfg")
                .join("com.qontinui.runner")
                .join("claude-accounts.json")
        );
    }

    /// The canonical path must come straight from `dirs::config_dir()` —
    /// `QONTINUI_CONFIG_DIR` plays no part. The resolver never reads env, so
    /// this asserts equality with the dirs-derived path without mutating
    /// process env (env-mutation would race other tests' `load_settings`).
    #[test]
    fn canonical_path_ignores_qontinui_config_dir() {
        let expected = dirs::config_dir().map(|d| accounts_path_under(&d));
        assert_eq!(claude_accounts_file_path(), expected);
        if let Some(p) = claude_accounts_file_path() {
            assert!(
                !p.to_string_lossy().contains("instances"),
                "claude-accounts.json must never resolve under an instance scope"
            );
        }
    }

    #[test]
    fn roster_round_trips_with_snake_case_mode() {
        let mut cmds = HashMap::new();
        cmds.insert("C:\\Users\\x\\.claude-work".to_string(), "clg".to_string());
        let file = ClaudeAccountsFile {
            claude_config_dirs: vec!["C:\\Users\\x\\.claude-work".to_string()],
            account_selection_mode: AccountSelectionMode::LeastUsage,
            config_dir: Some("C:\\Users\\x\\.claude-work".to_string()),
            claude_account_launch_commands: cmds,
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        assert!(
            json.contains("\"least_usage\""),
            "AccountSelectionMode must serialize snake_case: {json}"
        );
        let parsed: ClaudeAccountsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, file);
    }

    #[test]
    fn load_is_fail_open_on_missing_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-accounts.json");
        // Missing → None.
        assert_eq!(load_from(&path), None);
        // Corrupt → None (fail-open, no panic/error).
        std::fs::write(&path, b"{not json!!").unwrap();
        assert_eq!(load_from(&path), None);
        // Partial JSON (missing fields) → defaults fill in.
        std::fs::write(&path, br#"{"claude_config_dirs":["/a"]}"#).unwrap();
        let parsed = load_from(&path).unwrap();
        assert_eq!(parsed.claude_config_dirs, vec!["/a".to_string()]);
        assert_eq!(
            parsed.account_selection_mode,
            AccountSelectionMode::LeastUsage
        );
        assert_eq!(parsed.config_dir, None);
    }

    #[test]
    fn save_round_trips_through_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("claude-accounts.json");
        let file = ClaudeAccountsFile {
            claude_config_dirs: vec!["/x".into(), "/y".into()],
            ..Default::default()
        };
        save_to(&path, &file).unwrap();
        assert_eq!(load_from(&path), Some(file));
    }

    /// Machine-global roster must win UNCONDITIONALLY over stale per-instance
    /// shadow copies — including when the roster's values are emptier than
    /// the shadow's.
    #[test]
    #[allow(clippy::field_reassign_with_default)] // Settings has ~60 fields; literal construction is impractical
    fn overlay_overwrites_all_four_fields_unconditionally() {
        let mut settings = crate::settings::Settings::default();
        settings.claude_config_dirs = vec!["/stale-shadow".into()];
        settings
            .claude_account_launch_commands
            .insert("/stale-shadow".into(), "stale-cmd".into());
        settings.ai.claude_cli.config_dir = Some("/stale-shadow".into());
        settings.ai.claude_cli.account_selection_mode = AccountSelectionMode::Manual;

        let mut cmds = HashMap::new();
        cmds.insert("/global".to_string(), "clg".to_string());
        let roster = ClaudeAccountsFile {
            claude_config_dirs: vec!["/global".into()],
            account_selection_mode: AccountSelectionMode::LeastUsage,
            config_dir: None, // must overwrite the Some(...) shadow
            claude_account_launch_commands: cmds.clone(),
        };
        apply_roster_overlay(&mut settings, roster);

        assert_eq!(settings.claude_config_dirs, vec!["/global".to_string()]);
        assert_eq!(settings.claude_account_launch_commands, cmds);
        assert_eq!(settings.ai.claude_cli.config_dir, None);
        assert_eq!(
            settings.ai.claude_cli.account_selection_mode,
            AccountSelectionMode::LeastUsage
        );

        // Empty-but-present roster also wins (unconditional, not merge-if-non-empty).
        apply_roster_overlay(&mut settings, ClaudeAccountsFile::default());
        assert!(settings.claude_config_dirs.is_empty());
        assert!(settings.claude_account_launch_commands.is_empty());
    }

    #[test]
    fn migration_seeds_from_nonempty_unscoped_roster() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("claude-accounts.json");
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            br#"{
                "claude_config_dirs": ["/acct-a", "/acct-b"],
                "claude_account_launch_commands": {"/acct-a": "clg"},
                "ai": {"claude_cli": {"config_dir": "/acct-a",
                                       "account_selection_mode": "manual"}}
            }"#,
        )
        .unwrap();

        assert!(migrate_seed(&accounts, &settings));
        let seeded = load_from(&accounts).unwrap();
        assert_eq!(
            seeded.claude_config_dirs,
            vec!["/acct-a".to_string(), "/acct-b".to_string()]
        );
        assert_eq!(seeded.config_dir, Some("/acct-a".to_string()));
        assert_eq!(seeded.account_selection_mode, AccountSelectionMode::Manual);
        assert_eq!(
            seeded.claude_account_launch_commands.get("/acct-a"),
            Some(&"clg".to_string())
        );
    }

    #[test]
    fn migration_skips_empty_roster() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("claude-accounts.json");
        let settings = dir.path().join("settings.json");
        // Unscoped settings exists but holds no roster data.
        std::fs::write(&settings, br#"{"app_mode": "advanced"}"#).unwrap();
        assert!(!migrate_seed(&accounts, &settings));
        assert!(!accounts.exists());
        // No unscoped settings at all → also no seed.
        std::fs::remove_file(&settings).unwrap();
        assert!(!migrate_seed(&accounts, &settings));
        assert!(!accounts.exists());
    }

    #[test]
    fn migration_never_touches_existing_accounts_file() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("claude-accounts.json");
        let settings = dir.path().join("settings.json");
        let existing = ClaudeAccountsFile {
            claude_config_dirs: vec!["/already-here".into()],
            ..Default::default()
        };
        save_to(&accounts, &existing).unwrap();
        std::fs::write(&settings, br#"{"claude_config_dirs": ["/would-clobber"]}"#).unwrap();

        assert!(!migrate_seed(&accounts, &settings));
        assert_eq!(load_from(&accounts), Some(existing));
    }
}
