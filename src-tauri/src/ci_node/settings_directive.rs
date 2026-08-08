//! Inbound `events.ci.settings_requested.<device_id>` — the owner reconfigures
//! this device's CI-node lane from qontinui-web (plan
//! `2026-08-07-runner-local-ci-parity-and-web-configuration`, Phase 4).
//!
//! The write path is: web `PUT /devenv/machines/{id}/ci-node` saves the desired
//! config on the machine row → coord `POST /devenv/ci-node-dispatch` proves the
//! device belongs to the caller's tenant and publishes the directive → THIS
//! module validates it again and persists it into `settings.json` through
//! [`crate::settings::save_ci_node_settings`], the one settings writer (atomic
//! write + parse-cache invalidation). There is no second writer and no poll.
//!
//! # Why this module re-validates everything coord already validated
//!
//! Because coord is not the security boundary for THIS machine — the machine is.
//! `repo_allowlist` decides which repositories' own `.qontinui/ci.toml` commands
//! this box will execute; `enabled` decides whether it executes any. A directive
//! is just JSON that arrived on a socket, and the runner has no way to prove
//! which surface authored it. So every rule the web form enforces and coord
//! re-enforces is enforced a THIRD time here, and this is the copy that matters:
//!
//! * **No wildcards, ever.** `*`, `?`, `[`, and the pseudo-slug `all` are
//!   refused outright. `CiNodeSettings::repo_allowlist`'s own contract is that
//!   allowlisting is a deliberate act, one repo at a time; a single accepted
//!   `"*"` would convert this device into a general-purpose execution host for
//!   any repo coord ever dispatches. Rejecting the whole directive (rather than
//!   dropping the offending entry) is deliberate: silently applying the safe
//!   subset of a payload someone tried to widen with is how a machine ends up
//!   in a state nobody chose.
//! * **`min_free_disk_gb ≥ 1`.** Zero DISABLES the disk guard, and this fleet
//!   has hit `os error 112` for real.
//! * **`enabled` is not special-cased.** A directive that fails validation
//!   turns nothing on — the previous settings stand, untouched.
//!
//! # When the change takes effect
//!
//! [`crate::settings::get_ci_node_settings`] re-reads the persisted document on
//! every call (behind an mtime/size-invalidated parse cache), and every consumer
//! calls it at the moment it needs the value. So:
//!
//! * `repo_allowlist`, `max_concurrent_builds`, `min_free_disk_gb` — read inside
//!   `admission::submit` per dispatch, so they govern the **next dispatch**. No
//!   restart, and no effect on a build already running (a build admitted under
//!   the old allowlist runs to completion; that is intended — revoking consent
//!   stops future work, it does not kill work in flight).
//! * `max_concurrent_builds` as ADVERTISED capacity — republished on the next
//!   fleet heartbeat/budget POST, not instantly.
//! * `enabled` — consulted by `admission::submit` per dispatch, so DISABLING is
//!   effective immediately for admission purposes.

use serde::Deserialize;
use tracing::{info, warn};

use crate::settings::CiNodeSettings;

/// Upper bound on `repo_allowlist` length (mirrors the web schema).
const MAX_ALLOWLIST_ENTRIES: usize = 100;
/// Longest single allowlist entry (mirrors the web schema).
const MAX_ALLOWLIST_ENTRY_LEN: usize = 200;
/// Concurrency bounds. A directive asking for 0 is a mistake, not a request to
/// idle — `admission` clamps 0 up to 1 anyway, so accepting it would persist a
/// number that lies about what the device does.
const MAX_CONCURRENT_BUILDS_MIN: u32 = 1;
const MAX_CONCURRENT_BUILDS_MAX: u32 = 64;
/// `min_free_disk_gb` bounds. **0 disables the guard** — see the module doc.
const MIN_FREE_DISK_GB_MIN: u64 = 1;
const MIN_FREE_DISK_GB_MAX: u64 = 100_000;

/// Wire shape of the settings directive. Unknown keys are tolerated (coord also
/// sends `machine_id` / `requested_at`, which are correlation aids, not
/// settings); every settings key defaults to the RUNNER'S OWN default so a lean
/// or truncated payload can only narrow what this device will do.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CiSettingsDirective {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_concurrent_builds")]
    pub max_concurrent_builds: u32,
    #[serde(default)]
    pub repo_allowlist: Vec<String>,
    #[serde(default = "default_min_free_disk_gb")]
    pub min_free_disk_gb: u64,
    /// The web `devenv_machines.id` this config was saved against. Logged only
    /// — it is a correlation aid for the owner reading runner logs next to the
    /// dashboard, and grants nothing.
    #[serde(default)]
    pub machine_id: Option<String>,
}

fn default_max_concurrent_builds() -> u32 {
    1
}

fn default_min_free_disk_gb() -> u64 {
    20
}

/// Why a directive was refused. Typed so the tests assert the DECISION rather
/// than prose, and so the wildcard refusal keeps its own identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectiveRejection {
    /// A glob metacharacter, or the `all` pseudo-slug — "allow everything".
    Wildcard(String),
    EmptyEntry,
    EntryTooLong(usize),
    MalformedEntry(String),
    TooManyEntries(usize),
    ConcurrencyOutOfRange(u32),
    /// `min_free_disk_gb == 0` — turning the disk guard OFF.
    DiskGuardDisabled,
    DiskFloorOutOfRange(u64),
}

impl DirectiveRejection {
    /// One-line log reason. The runner has no reply channel for settings, so
    /// this ends up in `runner-tauri.log` and nowhere else.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Wildcard(entry) => format!(
                "repo_allowlist entry {entry:?} is a wildcard / all-repos value; \
                 the allowlist has no allow-everything form"
            ),
            Self::EmptyEntry => "repo_allowlist contains an empty entry".to_string(),
            Self::EntryTooLong(len) => {
                format!("repo_allowlist entry of length {len} exceeds {MAX_ALLOWLIST_ENTRY_LEN}")
            }
            Self::MalformedEntry(entry) => {
                format!("repo_allowlist entry {entry:?} is not `owner/name` or a bare repo name")
            }
            Self::TooManyEntries(n) => {
                format!("repo_allowlist has {n} entries, above the {MAX_ALLOWLIST_ENTRIES} limit")
            }
            Self::ConcurrencyOutOfRange(v) => format!(
                "max_concurrent_builds {v} is outside \
                 {MAX_CONCURRENT_BUILDS_MIN}..={MAX_CONCURRENT_BUILDS_MAX}"
            ),
            Self::DiskGuardDisabled => format!(
                "min_free_disk_gb 0 would DISABLE the disk guard; the floor is \
                 {MIN_FREE_DISK_GB_MIN} GiB"
            ),
            Self::DiskFloorOutOfRange(v) => format!(
                "min_free_disk_gb {v} is outside {MIN_FREE_DISK_GB_MIN}..={MIN_FREE_DISK_GB_MAX}"
            ),
        }
    }
}

/// Does this entry try to name more than one repo? Glob metacharacters and the
/// `all` pseudo-slug, case-insensitively.
fn is_wildcard_entry(entry: &str) -> bool {
    entry.contains('*')
        || entry.contains('?')
        || entry.contains('[')
        || entry.eq_ignore_ascii_case("all")
}

/// `owner/name` or a bare repo name over `[A-Za-z0-9._-]`. Stricter than
/// [`super::repo_slug_is_safe`] (which permits arbitrary `/` nesting) because
/// an allowlist entry is compared against a coord slug or its basename — a
/// deeper path could never match one, so accepting it would only ever be a
/// silent no-op the owner mistook for a grant.
fn entry_charset_ok(entry: &str) -> bool {
    if entry.contains("..") {
        return false;
    }
    let mut segments = entry.split('/');
    let (Some(first), second, None) = (segments.next(), segments.next(), segments.next()) else {
        return false;
    };
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    segment_ok(first) && second.map(segment_ok).unwrap_or(true)
}

/// PURE validation: directive → the `CiNodeSettings` that would be persisted,
/// or the first rejection. No filesystem, no globals — this is the unit under
/// test, and it is the function that actually protects the machine.
pub(crate) fn validate(
    directive: &CiSettingsDirective,
) -> Result<CiNodeSettings, DirectiveRejection> {
    if !(MAX_CONCURRENT_BUILDS_MIN..=MAX_CONCURRENT_BUILDS_MAX)
        .contains(&directive.max_concurrent_builds)
    {
        return Err(DirectiveRejection::ConcurrencyOutOfRange(
            directive.max_concurrent_builds,
        ));
    }
    if directive.min_free_disk_gb == 0 {
        return Err(DirectiveRejection::DiskGuardDisabled);
    }
    if !(MIN_FREE_DISK_GB_MIN..=MIN_FREE_DISK_GB_MAX).contains(&directive.min_free_disk_gb) {
        return Err(DirectiveRejection::DiskFloorOutOfRange(
            directive.min_free_disk_gb,
        ));
    }
    if directive.repo_allowlist.len() > MAX_ALLOWLIST_ENTRIES {
        return Err(DirectiveRejection::TooManyEntries(
            directive.repo_allowlist.len(),
        ));
    }

    let mut allowlist: Vec<String> = Vec::with_capacity(directive.repo_allowlist.len());
    for raw in &directive.repo_allowlist {
        let entry = raw.trim();
        if entry.is_empty() {
            return Err(DirectiveRejection::EmptyEntry);
        }
        // Wildcard check FIRST, so `"*"` is refused as an attempt to allow
        // everything rather than as a charset typo.
        if is_wildcard_entry(entry) {
            return Err(DirectiveRejection::Wildcard(entry.to_string()));
        }
        if entry.len() > MAX_ALLOWLIST_ENTRY_LEN {
            return Err(DirectiveRejection::EntryTooLong(entry.len()));
        }
        if !entry_charset_ok(entry) {
            return Err(DirectiveRejection::MalformedEntry(entry.to_string()));
        }
        if !allowlist.iter().any(|e| e == entry) {
            allowlist.push(entry.to_string());
        }
    }

    Ok(CiNodeSettings {
        enabled: directive.enabled,
        max_concurrent_builds: directive.max_concurrent_builds,
        repo_allowlist: allowlist,
        min_free_disk_gb: directive.min_free_disk_gb,
    })
}

/// Validate, then persist. Called from the WS frame handler.
///
/// Never panics and never propagates: a refused or unpersistable directive
/// leaves the existing settings exactly as they were and logs why. There is no
/// ack channel back to coord — the owner's evidence is this log line plus the
/// device's subsequent behaviour, which the web surface is careful never to
/// present as a confirmed apply.
pub(crate) fn apply(directive: CiSettingsDirective) {
    let machine = directive.machine_id.clone();
    let settings = match validate(&directive) {
        Ok(s) => s,
        Err(rejection) => {
            warn!(
                "ci_node: REFUSED settings directive (machine_id={machine:?}): {}",
                rejection.reason()
            );
            return;
        }
    };

    let summary = format!(
        "enabled={} repos={} concurrency={} min_free_disk_gb={}",
        settings.enabled,
        settings.repo_allowlist.len(),
        settings.max_concurrent_builds,
        settings.min_free_disk_gb,
    );
    match crate::settings::save_ci_node_settings(settings) {
        Ok(()) => info!(
            "ci_node: applied settings directive (machine_id={machine:?}): {summary} \
             — persisted to settings.json; governs the next dispatch"
        ),
        Err(e) => warn!(
            "ci_node: settings directive validated but could NOT be persisted \
             (machine_id={machine:?}): {e} — previous settings still in force"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive() -> CiSettingsDirective {
        CiSettingsDirective {
            enabled: true,
            max_concurrent_builds: 2,
            repo_allowlist: vec!["qontinui/qontinui-runner".to_string()],
            min_free_disk_gb: 20,
            machine_id: None,
        }
    }

    /// The pinned wire contract coord publishes, including the two correlation
    /// keys that are NOT settings.
    #[test]
    fn directive_parses_coord_wire_contract() {
        let d: CiSettingsDirective = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "max_concurrent_builds": 3,
            "repo_allowlist": ["qontinui/qontinui-runner", "qontinui-coord"],
            "min_free_disk_gb": 25,
            "machine_id": "3f2b1c00-0000-4000-8000-000000000001",
            "requested_at": "2026-08-07T12:00:00Z",
        }))
        .expect("coord's pinned payload must parse");
        assert!(d.enabled);
        assert_eq!(d.max_concurrent_builds, 3);
        assert_eq!(d.repo_allowlist.len(), 2);
        assert_eq!(d.min_free_disk_gb, 25);
        assert_eq!(
            d.machine_id.as_deref(),
            Some("3f2b1c00-0000-4000-8000-000000000001")
        );
    }

    /// Every omitted key falls back to the runner's own default, and every one
    /// of those defaults is the SAFE direction — a truncated or hostile-lean
    /// payload can only ever narrow this device's exposure, never widen it.
    #[test]
    fn omitted_keys_default_fail_closed() {
        let d: CiSettingsDirective =
            serde_json::from_value(serde_json::json!({})).expect("empty object parses");
        assert!(!d.enabled, "missing `enabled` must mean OFF");
        assert!(
            d.repo_allowlist.is_empty(),
            "missing allowlist must allow nothing"
        );
        assert_eq!(d.max_concurrent_builds, 1);
        assert_eq!(d.min_free_disk_gb, 20);

        // And the resulting settings equal the shipped inert default exactly.
        assert_eq!(validate(&d).unwrap(), CiNodeSettings::default());
    }

    /// THE allowlist boundary: no wildcard, in any spelling, from any surface.
    /// The web form refuses these too — this test exists because the runner is
    /// not allowed to rely on that.
    #[test]
    fn wildcard_and_all_repos_entries_are_refused() {
        for bad in [
            "*",
            "qontinui/*",
            "*/runner",
            "**",
            "qontinui-run?er",
            "qontinui/[a-z]*",
            "all",
            "ALL",
            "All",
        ] {
            let mut d = directive();
            d.repo_allowlist = vec![bad.to_string()];
            assert_eq!(
                validate(&d).unwrap_err(),
                DirectiveRejection::Wildcard(bad.to_string()),
                "{bad:?} must be refused as a wildcard"
            );
        }
    }

    /// A wildcard anywhere in the list poisons the WHOLE directive — the safe
    /// subset is NOT silently applied.
    #[test]
    fn a_single_wildcard_rejects_the_entire_directive() {
        let mut d = directive();
        d.repo_allowlist = vec![
            "qontinui/qontinui-runner".to_string(),
            "*".to_string(),
            "qontinui/qontinui-coord".to_string(),
        ];
        assert!(matches!(
            validate(&d).unwrap_err(),
            DirectiveRejection::Wildcard(_)
        ));
    }

    /// 0 disables the disk guard; 1 is the floor and is accepted. The rule is
    /// "≥ 1", not "≥ the 20 GiB default".
    #[test]
    fn min_free_disk_gb_must_stay_at_least_one() {
        let mut d = directive();
        d.min_free_disk_gb = 0;
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::DiskGuardDisabled
        );

        d.min_free_disk_gb = 1;
        assert_eq!(validate(&d).unwrap().min_free_disk_gb, 1);

        d.min_free_disk_gb = MIN_FREE_DISK_GB_MAX + 1;
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::DiskFloorOutOfRange(MIN_FREE_DISK_GB_MAX + 1)
        );
    }

    #[test]
    fn concurrency_bounds_are_enforced() {
        let mut d = directive();
        d.max_concurrent_builds = 0;
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::ConcurrencyOutOfRange(0)
        );
        d.max_concurrent_builds = MAX_CONCURRENT_BUILDS_MAX + 1;
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::ConcurrencyOutOfRange(MAX_CONCURRENT_BUILDS_MAX + 1)
        );
        d.max_concurrent_builds = MAX_CONCURRENT_BUILDS_MAX;
        assert!(validate(&d).is_ok());
    }

    #[test]
    fn malformed_entries_are_refused() {
        for bad in [
            "owner/../secret",
            "owner/name/extra",
            "has space",
            "/leading",
            "trailing/",
            "https://github.com/o/n",
            "owner\\name",
        ] {
            let mut d = directive();
            d.repo_allowlist = vec![bad.to_string()];
            assert!(
                matches!(
                    validate(&d).unwrap_err(),
                    DirectiveRejection::MalformedEntry(_)
                ),
                "{bad:?} must be refused as malformed"
            );
        }

        let mut d = directive();
        d.repo_allowlist = vec!["   ".to_string()];
        assert_eq!(validate(&d).unwrap_err(), DirectiveRejection::EmptyEntry);

        d.repo_allowlist = vec!["x".repeat(MAX_ALLOWLIST_ENTRY_LEN + 1)];
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::EntryTooLong(MAX_ALLOWLIST_ENTRY_LEN + 1)
        );

        d.repo_allowlist = (0..=MAX_ALLOWLIST_ENTRIES)
            .map(|i| format!("r{i}"))
            .collect();
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::TooManyEntries(MAX_ALLOWLIST_ENTRIES + 1)
        );
    }

    /// Accepted entries are trimmed and de-duplicated with order preserved, and
    /// the result is exactly what `admission::repo_allowed` matches against
    /// (full slug OR bare basename).
    #[test]
    fn accepted_entries_are_normalized_and_match_admission() {
        let mut d = directive();
        d.repo_allowlist = vec![
            "  qontinui/qontinui-runner ".to_string(),
            "qontinui-coord".to_string(),
            "qontinui/qontinui-runner".to_string(),
        ];
        let settings = validate(&d).unwrap();
        assert_eq!(
            settings.repo_allowlist,
            vec!["qontinui/qontinui-runner", "qontinui-coord"]
        );
        assert!(super::super::admission::repo_allowed(
            &settings.repo_allowlist,
            "qontinui/qontinui-runner"
        ));
        assert!(super::super::admission::repo_allowed(
            &settings.repo_allowlist,
            "qontinui/qontinui-coord"
        ));
        assert!(!super::super::admission::repo_allowed(
            &settings.repo_allowlist,
            "someone-else/private"
        ));
    }

    /// A refused directive must leave the machine's posture alone: `validate`
    /// returns `Err` and yields NO settings to persist, so `apply` cannot write
    /// a partial or widened document.
    #[test]
    fn a_refused_directive_yields_no_settings() {
        let mut d = directive();
        d.enabled = true;
        d.repo_allowlist = vec!["*".to_string()];
        assert!(
            validate(&d).is_err(),
            "no CiNodeSettings may be produced from a wildcard directive"
        );
    }
}
