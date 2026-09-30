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
//! overlays the roster fields from this file onto the in-memory
//! `Settings` for EVERY instance (primary + temp + named). Precedence is
//! load-bearing: when the file EXISTS, the overlay overwrites every roster
//! field UNCONDITIONALLY (not merge-if-non-empty) — per-instance
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

/// On-disk shape of `claude-accounts.json`: exactly the six machine-global
/// roster fields, with types matching the corresponding `Settings` fields
/// (`Settings.claude_config_dirs`, `Settings.ai.claude_cli.account_selection_mode`,
/// `Settings.ai.claude_cli.account_selection_pinned`,
/// `Settings.ai.claude_cli.config_dir`, `Settings.claude_account_launch_commands`,
/// `Settings.claude_default_launch_command`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeAccountsFile {
    /// Directories containing Claude Code configs (each with a `projects/` subdir).
    #[serde(default)]
    pub claude_config_dirs: Vec<String>,
    /// How to pick the account when multiple config dirs exist (snake_case on disk).
    #[serde(default)]
    pub account_selection_mode: AccountSelectionMode,
    /// `true` = this machine's `account_selection_mode` wins over the fleet's
    /// `account_selection_mode` fleet-policy domain (see
    /// [`resolve_selection_mode`]). Machine-global for the same reason the mode
    /// is: a per-instance pin would let a temp runner and the primary on one
    /// box disagree about whether the fleet governs them. Defaults to `false`
    /// on a file written before the field existed.
    #[serde(default)]
    pub account_selection_pinned: bool,
    /// Manual `CLAUDE_CONFIG_DIR` pin (only meaningful in `Manual` mode).
    #[serde(default)]
    pub config_dir: Option<String>,
    /// Custom launch command per account config dir (key: config_dir path).
    #[serde(default)]
    pub claude_account_launch_commands: HashMap<String, String>,
    /// Default AI launch command template for accounts WITHOUT a per-account
    /// override. `None` = the built-in default
    /// (`claude --permission-mode bypassPermissions --session-id <uuid>`).
    /// `{sessionId}` in the template is substituted with the fresh pinned id;
    /// without the placeholder the runner appends `--session-id <uuid>`.
    #[serde(default)]
    pub claude_default_launch_command: Option<String>,
    /// Every key this build does not know, carried through a read-modify-write
    /// verbatim. The file is shared by every runner build on the machine, so
    /// without this an OLDER build's roster write silently drops a field a
    /// newer build added — which is exactly how `account_selection_pinned`
    /// would be lost (unpinning the machine and handing it to the fleet) by a
    /// build that predates it. Builds that predate THIS field still drop
    /// unknown keys; this makes every later field survive them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ClaudeAccountsFile {
    /// True when no roster data is present (selection mode and its pin alone
    /// — plain defaults — do not count as roster data).
    fn is_empty_roster(&self) -> bool {
        self.claude_config_dirs.is_empty()
            && self.config_dir.is_none()
            && self.claude_account_launch_commands.is_empty()
            && self.claude_default_launch_command.is_none()
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
        #[serde(default)]
        account_selection_pinned: bool,
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
        claude_default_launch_command: Option<String>,
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
        account_selection_pinned: probe.ai.claude_cli.account_selection_pinned,
        config_dir: probe.ai.claude_cli.config_dir,
        claude_account_launch_commands: probe.claude_account_launch_commands,
        claude_default_launch_command: probe.claude_default_launch_command,
        extra: serde_json::Map::new(),
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

/// Overwrite the six roster fields on `settings` from `roster` —
/// UNCONDITIONALLY (even with empty values): when `claude-accounts.json`
/// exists it is the single source of truth, and the stale shadow copies in
/// per-instance settings.json files must never win. Pure so the precedence
/// contract is unit-testable.
pub fn apply_roster_overlay(settings: &mut crate::settings::Settings, roster: ClaudeAccountsFile) {
    settings.claude_config_dirs = roster.claude_config_dirs;
    settings.claude_account_launch_commands = roster.claude_account_launch_commands;
    settings.claude_default_launch_command = roster.claude_default_launch_command;
    settings.ai.claude_cli.config_dir = roster.config_dir;
    settings.ai.claude_cli.account_selection_mode = roster.account_selection_mode;
    settings.ai.claude_cli.account_selection_pinned = roster.account_selection_pinned;
}

/// The one rule deciding which account-selection mode governs. PURE.
///
/// A PINNED machine uses its local mode, full stop. An unpinned machine uses
/// the fleet's `account_selection_mode` fleet-policy value when the fleet has
/// an opinion (`Some`), else its local mode. This is FORCE-APPLY, not a
/// default-fill: the local mode is always a concrete value on disk (the field
/// is not an `Option`), so a fleet term that only filled an absent local value
/// would never take effect on any machine that has ever saved settings.
///
/// Every decision site reads the mode through this function — the picker
/// ([`crate::ai_provider::account_usage::pick_best_account`]), the spawn-time
/// config-dir resolution (`ai_provider::config::get_effective_config_dir`) and
/// the per-device account report — so the fleet value cannot half-apply (the
/// picker rotating under the fleet mode while the spawn path honours a local
/// `manual`).
pub fn resolve_selection_mode(
    local: AccountSelectionMode,
    pinned: bool,
    fleet: Option<AccountSelectionMode>,
) -> AccountSelectionMode {
    if pinned {
        local
    } else {
        fleet.unwrap_or(local)
    }
}

/// [`resolve_selection_mode`] applied to one `ClaudeCliSettings` document
/// plus the fleet-policy cache.
///
/// For decision sites that are already handed the (roster-overlaid)
/// `get_ai_settings().claude_cli` — reading the mode and the pin off the SAME
/// document they were given keeps them consistent with it (and keeps their
/// unit tests hermetic: a test's settings value, not the host machine's
/// `claude-accounts.json`, decides the local half).
pub fn resolve_for_cli_settings(cli: &crate::settings::ClaudeCliSettings) -> AccountSelectionMode {
    resolve_selection_mode(
        cli.account_selection_mode,
        cli.account_selection_pinned,
        crate::mcp::fleet_policy_poller::fleet_account_selection_mode(),
    )
}

/// The machine's LOCAL `(mode, pinned)` pair, before any fleet term.
///
/// Same precedence as [`apply_roster_overlay`], expressed once: when
/// `claude-accounts.json` exists it is the single source of truth for the
/// roster fields (including these two), and the per-instance `settings.json`
/// copy is a stale shadow. Only when the file is absent/corrupt does the
/// per-instance value apply. Both halves come from the SAME source so a pin
/// can never be read from one document and the mode it pins from another.
fn local_selection() -> (AccountSelectionMode, bool) {
    match load() {
        Some(roster) => (
            roster.account_selection_mode,
            roster.account_selection_pinned,
        ),
        None => {
            let cli = crate::settings::get_ai_settings().claude_cli;
            (cli.account_selection_mode, cli.account_selection_pinned)
        }
    }
}

/// This machine's LOCAL account-selection mode — what its settings say,
/// WITHOUT the fleet-policy term. For surfaces that report "local vs
/// effective"; a decision site reads [`effective_selection_mode`].
pub fn local_selection_mode() -> AccountSelectionMode {
    local_selection().0
}

/// The account-selection mode that will ACTUALLY be applied on this machine:
/// the local `(mode, pinned)` pair ([`local_selection_mode`]'s source)
/// resolved against the fleet's `account_selection_mode` policy by
/// [`resolve_selection_mode`].
///
/// Read this — never `get_ai_settings().claude_cli.account_selection_mode` —
/// wherever the mode is being REPORTED (e.g. the per-device account feed to
/// coord) or ACTED on (the account picker), because a report is a claim about
/// the machine, not about one instance's settings document, and the picker
/// must rotate under the same mode the report names.
pub fn effective_selection_mode() -> AccountSelectionMode {
    let (local, pinned) = local_selection();
    resolve_selection_mode(
        local,
        pinned,
        crate::mcp::fleet_policy_poller::fleet_account_selection_mode(),
    )
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
        account_selection_pinned: s.ai.claude_cli.account_selection_pinned,
        config_dir: s.ai.claude_cli.config_dir,
        claude_account_launch_commands: s.claude_account_launch_commands,
        claude_default_launch_command: s.claude_default_launch_command,
        extra: serde_json::Map::new(),
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
            account_selection_pinned: true,
            config_dir: Some("C:\\Users\\x\\.claude-work".to_string()),
            claude_account_launch_commands: cmds,
            claude_default_launch_command: None,
            extra: serde_json::Map::new(),
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
            AccountSelectionMode::HighestExpectedUsage
        );
        assert_eq!(parsed.config_dir, None);
        // A roster written before the pin existed decodes UNPINNED.
        assert!(!parsed.account_selection_pinned);
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
    fn overlay_overwrites_all_six_fields_unconditionally() {
        let mut settings = crate::settings::Settings::default();
        settings.claude_config_dirs = vec!["/stale-shadow".into()];
        settings
            .claude_account_launch_commands
            .insert("/stale-shadow".into(), "stale-cmd".into());
        settings.claude_default_launch_command = Some("stale-default".into());
        settings.ai.claude_cli.config_dir = Some("/stale-shadow".into());
        settings.ai.claude_cli.account_selection_mode = AccountSelectionMode::Manual;
        settings.ai.claude_cli.account_selection_pinned = false;

        let mut cmds = HashMap::new();
        cmds.insert("/global".to_string(), "clg".to_string());
        let roster = ClaudeAccountsFile {
            claude_config_dirs: vec!["/global".into()],
            account_selection_mode: AccountSelectionMode::LeastUsage,
            account_selection_pinned: true,
            config_dir: None, // must overwrite the Some(...) shadow
            claude_account_launch_commands: cmds.clone(),
            claude_default_launch_command: Some("claude --model opus".into()),
            extra: serde_json::Map::new(),
        };
        apply_roster_overlay(&mut settings, roster);

        assert_eq!(settings.claude_config_dirs, vec!["/global".to_string()]);
        assert_eq!(settings.claude_account_launch_commands, cmds);
        assert_eq!(
            settings.claude_default_launch_command,
            Some("claude --model opus".to_string())
        );
        assert_eq!(settings.ai.claude_cli.config_dir, None);
        assert_eq!(
            settings.ai.claude_cli.account_selection_mode,
            AccountSelectionMode::LeastUsage
        );
        assert!(settings.ai.claude_cli.account_selection_pinned);

        // Empty-but-present roster also wins (unconditional, not merge-if-non-empty).
        apply_roster_overlay(&mut settings, ClaudeAccountsFile::default());
        assert!(settings.claude_config_dirs.is_empty());
        assert!(!settings.ai.claude_cli.account_selection_pinned);
        assert!(settings.claude_account_launch_commands.is_empty());
        assert_eq!(settings.claude_default_launch_command, None);
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

    /// The migration carries the pin across with the mode: a primary whose
    /// unscoped settings.json says "pinned" must not seed an UNPINNED roster,
    /// which would hand the machine to the fleet the moment the roster exists.
    #[test]
    fn migration_seeds_the_pin_beside_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("claude-accounts.json");
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            br#"{
                "claude_config_dirs": ["/acct-a"],
                "ai": {"claude_cli": {"account_selection_mode": "manual",
                                       "account_selection_pinned": true}}
            }"#,
        )
        .unwrap();

        assert!(migrate_seed(&accounts, &settings));
        let seeded = load_from(&accounts).unwrap();
        assert_eq!(seeded.account_selection_mode, AccountSelectionMode::Manual);
        assert!(seeded.account_selection_pinned);
    }

    /// The resolver over its whole domain: 3 local modes × pinned/unpinned ×
    /// {no fleet opinion, each of the 3 fleet modes}. The expected value is
    /// spelled as the RULE ("pinned ⇒ local; unpinned ⇒ fleet, else local")
    /// per cell rather than as a copy of the function body, and two cells are
    /// additionally pinned to literals so a rule that inverted the pin would
    /// not pass by agreeing with itself.
    #[test]
    fn resolver_grid_pinned_keeps_local_unpinned_takes_the_fleet() {
        let modes = [
            AccountSelectionMode::Manual,
            AccountSelectionMode::LeastUsage,
            AccountSelectionMode::HighestExpectedUsage,
        ];
        let fleets = [
            None,
            Some(AccountSelectionMode::Manual),
            Some(AccountSelectionMode::LeastUsage),
            Some(AccountSelectionMode::HighestExpectedUsage),
        ];
        let mut cells = 0;
        for local in modes {
            for pinned in [true, false] {
                for fleet in fleets {
                    let expected = match (pinned, fleet) {
                        (true, _) => local,
                        (false, Some(f)) => f,
                        (false, None) => local,
                    };
                    assert_eq!(
                        resolve_selection_mode(local, pinned, fleet),
                        expected,
                        "local={local:?} pinned={pinned} fleet={fleet:?}"
                    );
                    cells += 1;
                }
            }
        }
        assert_eq!(cells, 24);

        // Literal anchors for the two cells the design turns on.
        assert_eq!(
            resolve_selection_mode(
                AccountSelectionMode::Manual,
                false,
                Some(AccountSelectionMode::HighestExpectedUsage)
            ),
            AccountSelectionMode::HighestExpectedUsage,
            "an UNPINNED manual machine is force-applied by the fleet"
        );
        assert_eq!(
            resolve_selection_mode(
                AccountSelectionMode::Manual,
                true,
                Some(AccountSelectionMode::HighestExpectedUsage)
            ),
            AccountSelectionMode::Manual,
            "a PINNED machine ignores the fleet"
        );
    }

    /// `resolve_for_cli_settings` reads the mode AND the pin off the document
    /// it is handed, and the fleet term from the poller cache — pinned here to
    /// each state so a concurrently running poller test cannot decide it.
    #[test]
    fn resolve_for_cli_settings_reads_mode_and_pin_off_one_document() {
        let pin = crate::mcp::fleet_policy_poller::pin_account_selection_for_test(None);
        let unpinned_manual = crate::settings::ClaudeCliSettings {
            account_selection_mode: AccountSelectionMode::Manual,
            account_selection_pinned: false,
            ..Default::default()
        };
        let pinned_manual = crate::settings::ClaudeCliSettings {
            account_selection_pinned: true,
            ..unpinned_manual.clone()
        };
        // No fleet opinion ⇒ local either way.
        assert_eq!(
            resolve_for_cli_settings(&unpinned_manual),
            AccountSelectionMode::Manual
        );
        assert_eq!(
            resolve_for_cli_settings(&pinned_manual),
            AccountSelectionMode::Manual
        );

        pin.set(Some(AccountSelectionMode::LeastUsage));
        assert_eq!(
            resolve_for_cli_settings(&unpinned_manual),
            AccountSelectionMode::LeastUsage
        );
        assert_eq!(
            resolve_for_cli_settings(&pinned_manual),
            AccountSelectionMode::Manual
        );
    }

    /// A key this build does not know survives a load → save round trip, so a
    /// field a NEWER build adds (the way `account_selection_pinned` was added)
    /// is not dropped by this build's roster writes.
    #[test]
    fn unknown_roster_keys_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-accounts.json");
        std::fs::write(
            &path,
            br#"{"claude_config_dirs":["/a"],"account_selection_pinned":true,
                 "some_future_field":{"nested":1}}"#,
        )
        .unwrap();
        let mut loaded = load_from(&path).unwrap();
        assert!(loaded.account_selection_pinned);
        loaded.claude_config_dirs.push("/b".into());
        save_to(&path, &loaded).unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["some_future_field"], serde_json::json!({"nested": 1}));
        assert_eq!(raw["account_selection_pinned"], serde_json::json!(true));
        assert_eq!(raw["claude_config_dirs"], serde_json::json!(["/a", "/b"]));
        // A KNOWN key is never duplicated into the catch-all.
        assert!(!loaded.extra.contains_key("account_selection_pinned"));
    }
}
