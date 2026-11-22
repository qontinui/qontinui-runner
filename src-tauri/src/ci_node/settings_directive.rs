//! Inbound `events.ci.settings_requested.<device_id>` — the owner reconfigures
//! this device's CI-node lane from qontinui-web (plan
//! `2026-08-07-runner-local-ci-parity-and-web-configuration`, Phase 4).
//!
//! The write path is: web `PUT /devenv/machines/{id}/ci-node` saves the desired
//! config on the machine row → coord `POST /devenv/ci-node-dispatch` (or its
//! agent twin) proves the device belongs to the caller's tenant, STORES the
//! directive as this device's latest, and publishes it → THIS module validates
//! it again and persists it into `settings.json` through
//! [`crate::settings::update_ci_node_settings`], the one `ci_node` writer
//! (atomic write + parse-cache invalidation, under
//! `CI_NODE_SETTINGS_WRITE_LOCK`).
//!
//! # Two arrivals, one gate: live and fetched
//!
//! Plan `2026-11-22-ci-node-directive-replays-on-connect`. A directive reaches
//! this module two ways. [`apply_live`] takes the frame on the `device_ci`
//! socket. [`apply_fetched`] takes the copy [`super::directive_fetch`] reads
//! from coord's store on every connect, which is how a directive sent while
//! this device was OFFLINE (coord's publish had no subscriber) still applies.
//! Both go through [`validate`], the single gate, and both do ONE
//! compare-and-write inside the settings writer's closure:
//!
//! 1. A directive whose coord-minted `directive_id` equals the last applied one
//!    is a no-op replay — checked FIRST, and nothing is written.
//! 2. Otherwise it applies only when its `requested_at` (coord's clock) is
//!    STRICTLY newer than the last applied one's, so the live and fetched
//!    copies of different directives can arrive in either order and the newest
//!    still wins. Coord clock against coord clock: box skew never enters.
//! 3. A FETCHED directive older than the owner's last local panel save
//!    (`local_edited_at`, this box's clock) is not applied — see [`decide`]
//!    for the skew this one cross-clock comparison carries. A LIVE frame
//!    always counts as newer than a local edit: the owner just acted.
//! 4. A directive with no `directive_id` comes from a coord that predates the
//!    store. Live, it applies unconditionally (the pre-store behaviour);
//!    fetched, it cannot be ordered and is skipped (such a coord has no store
//!    to fetch from anyway).
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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use tracing::{info, warn};
use uuid::Uuid;

use crate::settings::{CiNodeSettings, SettingsWrite};

/// Upper bound on `repo_allowlist` length (mirrors the web schema).
const MAX_ALLOWLIST_ENTRIES: usize = 100;
/// Longest single allowlist entry (mirrors the web schema).
const MAX_ALLOWLIST_ENTRY_LEN: usize = 200;
/// Concurrency bounds. A directive asking for 0 is a mistake, not a request to
/// idle — `admission` clamps 0 up to 1 anyway, so accepting it would persist a
/// number that lies about what the device does.
pub(crate) const MAX_CONCURRENT_BUILDS_MIN: u32 = 1;
pub(crate) const MAX_CONCURRENT_BUILDS_MAX: u32 = 64;
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
    /// Absent or `null` = `None` = "use the host suggestion"
    /// (`ci_node::host_sizing::suggested_concurrent_builds`); `Some(n)` is an
    /// explicit override, validated to `1..=64`. Carried through to
    /// `CiNodeSettings` verbatim, so a directive can express either.
    #[serde(default)]
    pub max_concurrent_builds: Option<u32>,
    #[serde(default)]
    pub repo_allowlist: Vec<String>,
    #[serde(default = "default_min_free_disk_gb")]
    pub min_free_disk_gb: u64,
    /// May a dispatch converge this box's global toolchains toward canonical?
    ///
    /// `#[serde(default)]` = FALSE, and that default is the contract with a
    /// web build that predates this field: a directive that does not mention
    /// it turns convergence OFF rather than leaving whatever was there. That
    /// is the same direction every other field here runs — a directive states
    /// the whole desired config, so an omitted grant is an absent grant, never
    /// an inherited one.
    #[serde(default)]
    pub canonical_converge: bool,
    /// The web `devenv_machines.id` this config was saved against. Logged only
    /// — it is a correlation aid for the owner reading runner logs next to the
    /// dashboard, and grants nothing.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Coord's id for this directive, minted once per accepted directive and
    /// identical on the live frame and the stored (fetched) copy. Absent on a
    /// coord that predates the directive store. Parsed leniently: a malformed
    /// value reads as absent rather than refusing an otherwise-valid frame.
    #[serde(default, deserialize_with = "lenient_uuid")]
    pub directive_id: Option<Uuid>,
    /// Coord's clock at accept time (truncated to microseconds, so the live
    /// copy and the `timestamptz`-round-tripped fetched copy compare equal).
    /// Older coords send it too, uncompared. Parsed leniently, like
    /// `directive_id`.
    #[serde(default, deserialize_with = "lenient_rfc3339")]
    pub requested_at: Option<DateTime<Utc>>,
}

fn default_min_free_disk_gb() -> u64 {
    20
}

/// `directive_id`: a string that parses as a UUID, else `None`.
fn lenient_uuid<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .as_ref()
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok()))
}

/// `requested_at`: an RFC 3339 string, else `None`.
fn lenient_rfc3339<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
    let raw = Option::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .as_ref()
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc)))
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
    if let Some(n) = directive.max_concurrent_builds {
        if !(MAX_CONCURRENT_BUILDS_MIN..=MAX_CONCURRENT_BUILDS_MAX).contains(&n) {
            return Err(DirectiveRejection::ConcurrencyOutOfRange(n));
        }
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
        // Verbatim: an explicit value is an operator override, and an absent
        // one hands the choice to the host suggestion.
        max_concurrent_builds: directive.max_concurrent_builds,
        repo_allowlist: allowlist,
        min_free_disk_gb: directive.min_free_disk_gb,
        // No range to check — it is a boolean grant. What it needs instead is
        // for the ABSENT case to land on false, which the serde default above
        // guarantees, and for it to be un-settable from a repo, which it is:
        // this struct is only ever built from an owner-authored directive.
        canonical_converge: directive.canonical_converge,
        // Bookkeeping is the writer's, not the gate's: `validate` decides only
        // the settings fields. `apply` fills these from the directive and the
        // on-disk document inside the compare-and-write.
        last_directive_id: None,
        last_directive_requested_at: None,
        local_edited_at: None,
    })
}

/// Which door a directive arrived through. Logged as `source=live|fetched`,
/// and it decides the local-edit branch of [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectiveSource {
    /// The `settings_requested` frame on the `device_ci` socket.
    Live,
    /// The copy read from coord's store on connect ([`super::directive_fetch`]).
    Fetched,
}

impl DirectiveSource {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Fetched => "fetched",
        }
    }

    /// The parenthetical in the applied line — `(fetched on connect)` is the
    /// string the plan's arming probe reads in `runner-tauri.log`.
    fn arrival(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Fetched => "fetched on connect",
        }
    }
}

/// What one `apply_*` call did. Typed so the tests assert the DECISION rather
/// than a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    /// Validated and persisted.
    Applied,
    /// [`validate`] refused it; nothing was written.
    Refused(DirectiveRejection),
    /// Its `directive_id` is the last one applied — a replay; nothing written.
    AlreadyApplied,
    /// Its `requested_at` is not strictly newer than the last applied
    /// directive's — a newer one already won; nothing written.
    Superseded,
    /// FETCHED, and older than the owner's last local panel save; nothing
    /// written.
    SupersededByLocal,
    /// FETCHED with no `directive_id` / `requested_at`, so it cannot be
    /// ordered against what is on disk; nothing written.
    Unordered,
    /// Validated and chosen, but `settings.json` could not be written (or its
    /// base could not be read); the previous settings stand.
    PersistFailed(String),
}

/// PURE: should `directive` (already validated) replace what `current` holds?
/// `None` = apply; `Some(outcome)` = skip with that reason.
///
/// Order matters and is pinned by tests: id equality FIRST (so a replay is a
/// no-op even when its timestamp would also lose), then newest-wins on coord's
/// clock, then — fetched only — the local-edit branch.
///
/// **Clock skew in the local-edit branch.** `local_edited_at` is this box's
/// clock and `requested_at` is coord's, and no tolerance is added: the
/// comparison is direct, so the window in which it can misorder equals this
/// box's clock error against coord. A box running N seconds AHEAD treats a
/// fetched directive sent up to N s after a local edit as older (skipped until
/// a newer one arrives); a box running N s BEHIND lets a fetched directive sent
/// up to N s before a local edit through. Both other comparisons are
/// coord-vs-coord.
pub(crate) fn decide(
    current: &CiNodeSettings,
    directive: &CiSettingsDirective,
    source: DirectiveSource,
) -> Option<ApplyOutcome> {
    // Without identity (a coord that predates the store) only the live frame
    // applies, unconditionally; a fetched copy cannot be ordered.
    let unidentified = match source {
        DirectiveSource::Live => None,
        DirectiveSource::Fetched => Some(ApplyOutcome::Unordered),
    };
    let Some(id) = directive.directive_id else {
        return unidentified;
    };
    if current.last_directive_id == Some(id) {
        return Some(ApplyOutcome::AlreadyApplied);
    }
    let Some(at) = directive.requested_at else {
        return unidentified;
    };
    if current
        .last_directive_requested_at
        .is_some_and(|last| at <= last)
    {
        return Some(ApplyOutcome::Superseded);
    }
    if source == DirectiveSource::Fetched
        && current.local_edited_at.is_some_and(|local| at < local)
    {
        return Some(ApplyOutcome::SupersededByLocal);
    }
    None
}

/// A live `settings_requested` frame. See [`apply`].
pub(crate) fn apply_live(directive: CiSettingsDirective) -> ApplyOutcome {
    apply(directive, DirectiveSource::Live)
}

/// The copy fetched from coord's store on connect. See [`apply`].
pub(crate) fn apply_fetched(directive: CiSettingsDirective) -> ApplyOutcome {
    apply(directive, DirectiveSource::Fetched)
}

/// Validate, then ONE compare-and-write under `CI_NODE_SETTINGS_WRITE_LOCK`
/// ([`crate::settings::update_ci_node_settings`]): read the on-disk
/// bookkeeping, [`decide`], and either write the validated fields plus the
/// directive's id and `requested_at` — carrying `local_edited_at` through — or
/// write nothing at all.
///
/// Never panics and never propagates: a refused, skipped or unpersistable
/// directive leaves the existing settings exactly as they were and logs why.
/// There is no ack channel back to coord — the owner's evidence is this log
/// line plus the device's subsequent behaviour, which the web surface is
/// careful never to present as a confirmed apply.
fn apply(directive: CiSettingsDirective, source: DirectiveSource) -> ApplyOutcome {
    let machine = directive.machine_id.clone();
    let id = directive
        .directive_id
        .map_or_else(|| "none".to_string(), |u| u.to_string());
    let at = directive
        .requested_at
        .map_or_else(|| "none".to_string(), |t| t.to_rfc3339());
    let src = source.label();
    let validated = match validate(&directive) {
        Ok(s) => s,
        Err(rejection) => {
            warn!(
                "ci_node: REFUSED settings directive (source={src}, directive_id={id}, \
                 machine_id={machine:?}): {}",
                rejection.reason()
            );
            return ApplyOutcome::Refused(rejection);
        }
    };

    let summary = format!(
        "enabled={} repos={} concurrency={} min_free_disk_gb={}",
        validated.enabled,
        validated.repo_allowlist.len(),
        validated
            .max_concurrent_builds
            .map_or_else(|| "suggested".to_string(), |n| n.to_string()),
        validated.min_free_disk_gb,
    );
    let outcome = crate::settings::update_ci_node_settings(|ci| {
        if let Some(skip) = decide(ci, &directive, source) {
            return SettingsWrite::Skip(skip);
        }
        // The high-water mark only moves forward. A live frame from a coord
        // that predates the store (no id) — or one whose `requested_at` is
        // absent — still applies (`decide`), but must not erase or rewind the
        // recorded mark, or a later replay of an older stored directive would
        // win newest-wins against nothing.
        *ci = CiNodeSettings {
            last_directive_id: directive.directive_id.or(ci.last_directive_id),
            last_directive_requested_at: directive
                .requested_at
                .max(ci.last_directive_requested_at),
            local_edited_at: ci.local_edited_at,
            ..validated
        };
        SettingsWrite::Persist(ApplyOutcome::Applied)
    })
    .unwrap_or_else(ApplyOutcome::PersistFailed);

    match &outcome {
        ApplyOutcome::Applied => info!(
            "ci_node: applied settings directive ({}) source={src} directive_id={id} \
             requested_at={at} machine_id={machine:?}: {summary} \
             — persisted to settings.json; governs the next dispatch",
            source.arrival()
        ),
        ApplyOutcome::AlreadyApplied => info!(
            "ci_node: settings directive already applied, no-op replay \
             (source={src}, directive_id={id})"
        ),
        ApplyOutcome::Superseded => info!(
            "ci_node: settings directive superseded by a newer applied directive, not applied \
             (source={src}, directive_id={id}, requested_at={at})"
        ),
        ApplyOutcome::SupersededByLocal => info!(
            "ci_node: settings directive superseded-by-local: older than the owner's last \
             local edit, not applied (source={src}, directive_id={id}, requested_at={at})"
        ),
        ApplyOutcome::Unordered => warn!(
            "ci_node: settings directive SKIPPED: it carries no directive_id/requested_at, so \
             it cannot be ordered against the applied one (source={src}, machine_id={machine:?})"
        ),
        ApplyOutcome::PersistFailed(e) => warn!(
            "ci_node: settings directive validated but could NOT be persisted \
             (source={src}, directive_id={id}, machine_id={machine:?}): {e} \
             — previous settings still in force"
        ),
        ApplyOutcome::Refused(_) => {}
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive() -> CiSettingsDirective {
        CiSettingsDirective {
            enabled: true,
            max_concurrent_builds: Some(2),
            repo_allowlist: vec!["qontinui/qontinui-runner".to_string()],
            min_free_disk_gb: 20,
            canonical_converge: false,
            machine_id: None,
            directive_id: None,
            requested_at: None,
        }
    }

    /// A directive from a web build that predates the field turns convergence
    /// OFF, and that is the contract rather than an accident: the grant lets a
    /// repo's manifest drive rustup/volta/pyenv on the owner's machine, so it
    /// has to be stated to exist. Deserialized from JSON on purpose — the
    /// serde default is the thing under test, not the struct literal.
    #[test]
    fn an_omitted_convergence_grant_is_no_grant() {
        let raw = serde_json::json!({
            "enabled": true,
            "max_concurrent_builds": 1,
            "repo_allowlist": ["qontinui/qontinui-runner"],
            "min_free_disk_gb": 20,
        });
        let d: CiSettingsDirective =
            serde_json::from_value(raw).expect("a directive without the field must still parse");
        assert!(!d.canonical_converge);
        let settings = validate(&d).expect("otherwise valid");
        assert!(
            !settings.canonical_converge,
            "an absent grant must never persist as a granted one"
        );
    }

    /// …and an explicit grant survives validation, or the owner's web toggle
    /// would be a no-op.
    #[test]
    fn an_explicit_convergence_grant_is_carried_through() {
        let d = CiSettingsDirective {
            canonical_converge: true,
            ..directive()
        };
        assert!(validate(&d).expect("otherwise valid").canonical_converge);
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
        assert_eq!(d.max_concurrent_builds, Some(3));
        assert_eq!(validate(&d).unwrap().max_concurrent_builds, Some(3));
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
        assert_eq!(
            d.max_concurrent_builds, None,
            "missing concurrency must mean 'use the host suggestion'"
        );
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
        d.max_concurrent_builds = Some(0);
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::ConcurrencyOutOfRange(0)
        );
        d.max_concurrent_builds = Some(MAX_CONCURRENT_BUILDS_MAX + 1);
        assert_eq!(
            validate(&d).unwrap_err(),
            DirectiveRejection::ConcurrencyOutOfRange(MAX_CONCURRENT_BUILDS_MAX + 1)
        );
        d.max_concurrent_builds = Some(MAX_CONCURRENT_BUILDS_MAX);
        assert!(validate(&d).is_ok());
        d.max_concurrent_builds = None;
        assert_eq!(validate(&d).unwrap().max_concurrent_builds, None);
    }

    /// Plan 2026-09-22-ci-capacity-...: a directive can say "use the host
    /// suggestion" (absent or `null`) as well as an explicit value, and
    /// out-of-range explicit values are still refused.
    #[test]
    fn concurrency_wire_forms() {
        let parse = |v: serde_json::Value| -> CiSettingsDirective {
            serde_json::from_value(v).expect("must parse")
        };
        assert_eq!(parse(serde_json::json!({})).max_concurrent_builds, None);
        let null = parse(serde_json::json!({"max_concurrent_builds": null}));
        assert_eq!(null.max_concurrent_builds, None);
        assert_eq!(validate(&null).unwrap().max_concurrent_builds, None);
        let three = parse(serde_json::json!({"max_concurrent_builds": 3}));
        assert_eq!(validate(&three).unwrap().max_concurrent_builds, Some(3));
        for bad in [0u32, 65] {
            let d = parse(serde_json::json!({"max_concurrent_builds": bad}));
            assert_eq!(
                validate(&d).unwrap_err(),
                DirectiveRejection::ConcurrencyOutOfRange(bad)
            );
        }
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

    // ---- Live vs fetched ordering (plan 2026-11-22-ci-node-directive-replays-on-connect, D2) ----

    /// A settings.json carrying a known, non-default `ci_node` section.
    const SEEDED: &str =
        r#"{"ci_node":{"enabled":false,"repo_allowlist":["seed/repo"],"min_free_disk_gb":33}}"#;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// An identified directive, as coord now mints them.
    fn identified(id: Uuid, requested_at: &str, enabled: bool, repo: &str) -> CiSettingsDirective {
        CiSettingsDirective {
            enabled,
            repo_allowlist: vec![repo.to_string()],
            directive_id: Some(id),
            requested_at: Some(at(requested_at)),
            ..directive()
        }
    }

    fn on_disk() -> CiNodeSettings {
        crate::settings::get_ci_node_settings()
    }

    /// (b) The fetched copy of the directive already applied live is a no-op:
    /// NO write at all, so the bytes and the mtime are unchanged.
    #[test]
    fn a_fetched_copy_of_the_applied_directive_writes_nothing() {
        let amb = crate::test_env::isolated_ambient();
        let path = amb.write_settings_json(SEEDED);
        let id = Uuid::now_v7();
        let d = identified(id, "2026-11-22T10:00:00.123456Z", true, "a/one");
        assert_eq!(apply_live(d.clone()), ApplyOutcome::Applied);
        let bytes = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Let a write, if one happened, land on a later mtime tick.
        std::thread::sleep(std::time::Duration::from_millis(30));

        assert_eq!(apply_fetched(d), ApplyOutcome::AlreadyApplied);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }

    /// Id equality runs BEFORE the timestamp compare: a replay whose timestamp
    /// also loses is reported as the replay it is.
    #[test]
    fn id_equality_is_checked_before_newest_wins() {
        let id = Uuid::now_v7();
        let current = CiNodeSettings {
            last_directive_id: Some(id),
            last_directive_requested_at: Some(at("2026-11-22T10:00:00Z")),
            ..CiNodeSettings::default()
        };
        let replay = identified(id, "2026-11-22T10:00:00Z", true, "a/one");
        assert_eq!(
            decide(&current, &replay, DirectiveSource::Fetched),
            Some(ApplyOutcome::AlreadyApplied)
        );
        assert_eq!(
            decide(&current, &replay, DirectiveSource::Live),
            Some(ApplyOutcome::AlreadyApplied)
        );
    }

    /// (c) A fetched OLDER directive arriving after a newer live one is
    /// skipped, and the newer one's settings stand.
    #[test]
    fn a_fetched_older_directive_after_a_newer_live_one_is_skipped() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        let newer = identified(Uuid::now_v7(), "2026-11-22T10:00:05Z", true, "a/newer");
        let older = identified(Uuid::now_v7(), "2026-11-22T10:00:01Z", false, "a/older");
        assert_eq!(apply_live(newer.clone()), ApplyOutcome::Applied);

        assert_eq!(apply_fetched(older), ApplyOutcome::Superseded);
        let ci = on_disk();
        assert!(ci.enabled);
        assert_eq!(ci.repo_allowlist, vec!["a/newer"]);
        assert_eq!(ci.last_directive_id, newer.directive_id);
        assert_eq!(ci.last_directive_requested_at, newer.requested_at);
    }

    /// Strictly newer: an equal `requested_at` under a different id does not
    /// win (the first applied stands).
    #[test]
    fn an_equal_requested_at_is_not_newer() {
        let current = CiNodeSettings {
            last_directive_id: Some(Uuid::now_v7()),
            last_directive_requested_at: Some(at("2026-11-22T10:00:00Z")),
            ..CiNodeSettings::default()
        };
        let tie = identified(Uuid::now_v7(), "2026-11-22T10:00:00Z", true, "a/one");
        assert_eq!(
            decide(&current, &tie, DirectiveSource::Live),
            Some(ApplyOutcome::Superseded)
        );
    }

    /// (d) A live directive after a fetched one applies when newer, and
    /// records its own identity.
    #[test]
    fn a_newer_live_directive_after_a_fetched_one_applies() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        let fetched = identified(Uuid::now_v7(), "2026-11-22T10:00:01Z", true, "a/fetched");
        let live = identified(Uuid::now_v7(), "2026-11-22T10:00:02Z", false, "a/live");
        assert_eq!(apply_fetched(fetched), ApplyOutcome::Applied);

        assert_eq!(apply_live(live.clone()), ApplyOutcome::Applied);
        let ci = on_disk();
        assert!(!ci.enabled);
        assert_eq!(ci.repo_allowlist, vec!["a/live"]);
        assert_eq!(ci.last_directive_id, live.directive_id);
        assert_eq!(ci.last_directive_requested_at, live.requested_at);
    }

    /// (e) A FETCHED directive older than the owner's last local edit is
    /// skipped (and logged as superseded-by-local); the local edit stands. A
    /// LIVE frame of the same age still applies, because the owner just acted.
    #[test]
    fn a_fetched_directive_older_than_a_local_edit_is_skipped() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        crate::settings::update_ci_node_settings(|ci| {
            crate::commands::resource_guard_settings::apply_local_edit(
                ci,
                false,
                None,
                vec!["local/edit".to_string()],
                40,
                at("2026-11-22T10:00:10Z"),
            );
            SettingsWrite::Persist(())
        })
        .unwrap();
        let before = on_disk();
        let stale = identified(Uuid::now_v7(), "2026-11-22T10:00:09Z", true, "a/stale");

        assert_eq!(apply_fetched(stale.clone()), ApplyOutcome::SupersededByLocal);
        assert_eq!(on_disk(), before, "the local edit must stand");

        assert_eq!(apply_live(stale), ApplyOutcome::Applied);
        let ci = on_disk();
        assert!(ci.enabled);
        assert_eq!(
            ci.local_edited_at,
            Some(at("2026-11-22T10:00:10Z")),
            "an applied directive carries local_edited_at through"
        );
    }

    /// (f) A refused (wildcard) fetched directive writes nothing, the same as
    /// live: `validate` is the single gate for both doors.
    #[test]
    fn a_refused_fetched_directive_writes_nothing() {
        let amb = crate::test_env::isolated_ambient();
        let path = amb.write_settings_json(SEEDED);
        let before = std::fs::read(&path).unwrap();
        for source in [DirectiveSource::Fetched, DirectiveSource::Live] {
            let mut d = identified(Uuid::now_v7(), "2026-11-22T10:00:00Z", true, "a/one");
            d.repo_allowlist = vec!["*".to_string()];
            let got = match source {
                DirectiveSource::Fetched => apply_fetched(d),
                DirectiveSource::Live => apply_live(d),
            };
            assert_eq!(
                got,
                ApplyOutcome::Refused(DirectiveRejection::Wildcard("*".to_string())),
                "{source:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{source:?}");
        }
    }

    /// (h) A live frame with no `directive_id` (a coord that predates the
    /// store) still applies, even over a newer recorded directive, which is
    /// the pre-store behaviour; a fetched one without identity cannot be
    /// ordered and is skipped.
    #[test]
    fn a_live_frame_without_directive_id_still_applies() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);
        let recorded = identified(Uuid::now_v7(), "2026-11-22T10:00:05Z", false, "a/recorded");
        assert_eq!(apply_live(recorded.clone()), ApplyOutcome::Applied);

        // The pinned wire shape an older coord sends: requested_at, no id.
        let legacy: CiSettingsDirective = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "repo_allowlist": ["a/legacy"],
            "min_free_disk_gb": 20,
            "machine_id": "m-1",
            "requested_at": "2026-08-07T12:00:00Z",
        }))
        .unwrap();
        assert_eq!(legacy.directive_id, None);
        assert_eq!(apply_fetched(legacy.clone()), ApplyOutcome::Unordered);
        assert_eq!(apply_live(legacy), ApplyOutcome::Applied);
        let ci = on_disk();
        assert!(ci.enabled);
        assert_eq!(ci.repo_allowlist, vec!["a/legacy"]);
        // ...without rewinding the recorded high-water mark: the legacy frame's
        // older `requested_at` and missing id do not replace it, so a replay of
        // an older stored directive still loses newest-wins.
        assert_eq!(ci.last_directive_id, recorded.directive_id);
        assert_eq!(ci.last_directive_requested_at, recorded.requested_at);
        let older = identified(Uuid::now_v7(), "2026-11-22T10:00:03Z", false, "a/older");
        assert_eq!(apply_fetched(older), ApplyOutcome::Superseded);
    }

    /// The new wire keys parse, and a malformed value reads as absent rather
    /// than refusing an otherwise-valid frame.
    #[test]
    fn directive_identity_parses_leniently() {
        let id = Uuid::now_v7();
        let d: CiSettingsDirective = serde_json::from_value(serde_json::json!({
            "directive_id": id.to_string(),
            "requested_at": "2026-11-22T10:00:00.123456Z",
        }))
        .unwrap();
        assert_eq!(d.directive_id, Some(id));
        assert_eq!(d.requested_at, Some(at("2026-11-22T10:00:00.123456Z")));
        let bad: CiSettingsDirective = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "directive_id": "not-a-uuid",
            "requested_at": 12,
        }))
        .expect("a malformed identity must not refuse the frame");
        assert_eq!((bad.directive_id, bad.requested_at), (None, None));
        assert!(bad.enabled);
    }

    /// (j) The local save and a directive apply never lose each other's
    /// writes: both go through the one compare-and-write door, which holds
    /// `CI_NODE_SETTINGS_WRITE_LOCK` for the whole read-decide-write, and each
    /// carries the other's fields through.
    #[test]
    fn a_local_save_and_a_directive_apply_never_lose_each_other() {
        let amb = crate::test_env::isolated_ambient();
        amb.write_settings_json(SEEDED);

        // The door holds the lock while the closure decides: no other writer
        // could land between its read and its write.
        crate::settings::update_ci_node_settings(|_| {
            assert!(
                crate::settings::ci_node_settings_write_lock_is_held(),
                "update_ci_node_settings must hold the write lock across its closure"
            );
            SettingsWrite::Skip(())
        })
        .unwrap();

        let directive = identified(Uuid::now_v7(), "2026-11-22T10:00:05Z", true, "a/remote");
        assert_eq!(apply_live(directive.clone()), ApplyOutcome::Applied);

        // A local save after the directive keeps the directive's bookkeeping.
        crate::commands::resource_guard_settings::save_ci_node_settings_impl(
            false,
            None,
            vec!["local/edit".to_string()],
            20,
        )
        .expect("a valid local save");
        let ci = on_disk();
        assert_eq!(ci.repo_allowlist, vec!["local/edit"]);
        assert_eq!(ci.last_directive_id, directive.directive_id);
        assert_eq!(ci.last_directive_requested_at, directive.requested_at);
        let stamped = ci
            .local_edited_at
            .expect("the local save stamps local_edited_at");

        // A newer directive after the local save keeps the stamp.
        let newer = identified(Uuid::now_v7(), "2999-01-01T00:00:00Z", true, "a/newer");
        assert_eq!(apply_fetched(newer.clone()), ApplyOutcome::Applied);
        let ci = on_disk();
        assert_eq!(ci.local_edited_at, Some(stamped));
        assert_eq!(ci.last_directive_id, newer.directive_id);
    }
}
