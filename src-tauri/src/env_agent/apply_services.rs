//! The `services` half of the local apply (plan
//! `2026-07-02-devenv-copy-canonical-config-phase2-agent-apply`, P2b slice 2).
//!
//! Rewrites **this box's own** `~/.qontinui/profiles.json` toward the canonical
//! machine's service topology. No new authority is involved: `collect_services`
//! reads `crate::profiles::load()`, so the apply writes back into a file the
//! runner already owns and already writes ([`crate::profiles::ensure_coord_url`]).
//!
//! ## The load-bearing constraint
//!
//! [`crate::env_agent::collectors::sanitize_url`] emits `scheme://host[:port]`
//! and nothing else — it drops **userinfo AND path**. So canonical's
//! `database_url` carries neither the DB password nor the database name, and
//! `profiles.json` holds the only copy of both. A naive "write canonical's value
//! into the profile" would therefore blank the password and re-point the box at
//! the wrong database, and would blank `blob.access_key` / `blob.secret_key` /
//! `auth.token` — real credentials the captured section never carries.
//!
//! The merge is consequently **strictly additive over a fixed key set**
//! ([`FIELDS`]):
//!
//! - `database_url` / `redis_url` / `coord_url` / `blob.endpoint` — take scheme,
//!   host and port from canonical; **preserve local userinfo, path, query and
//!   fragment** ([`repoint_url`]).
//! - `blob.kind` / `blob.bucket` — plain strings, replaced wholesale.
//! - `blob.region` / `blob.access_key` / `blob.secret_key` — never written, so
//!   they survive by construction.
//! - `auth` — never touched at all.
//! - `port_*` — liveness OBSERVATIONS, not settings. They drift constantly (a
//!   box with the backend down differs from canonical every capture) and are
//!   therefore the most likely thing to be mistaken for an action. Not in
//!   [`FIELDS`] ⇒ never applied.
//!
//! The file is edited as a `serde_json::Value` tree — read → mutate only the
//! named keys → write — never through a typed `ProfilesFile` round-trip, which
//! would silently DROP any unknown keys the user's file carries. Same idiom, and
//! same reason, as [`crate::profiles::ensure_coord_url`].

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::apply::{AppliedChange, SectionApply, SectionStatus, SkipRecord};
use super::collectors::sanitize_url;
use super::pull::{Change, SectionPlan};

/// The envelope section this module applies.
pub const SERVICES_SECTION: &str = "services";

/// Where one `services` key lands inside a profile, and how its value is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// A top-level profile key holding a URL. Canonical supplies scheme/host/
    /// port; everything else is preserved from the local value.
    Url(&'static str),
    /// A `blob` sub-field holding a URL. Merged like [`Field::Url`].
    BlobUrl(&'static str),
    /// A `blob` sub-field holding a plain, non-secret string. Replaced wholesale.
    BlobPlain(&'static str),
}

impl Field {
    /// The JSON path of this field inside a profile object.
    fn json_path(self) -> Vec<String> {
        match self {
            Field::Url(k) => vec![k.to_string()],
            Field::BlobUrl(k) | Field::BlobPlain(k) => vec!["blob".to_string(), k.to_string()],
        }
    }

    /// True when the merge must preserve the local value's userinfo and path.
    fn is_url(self) -> bool {
        matches!(self, Field::Url(_) | Field::BlobUrl(_))
    }
}

/// The FIXED set of `services` keys an apply may write, and where each lands.
///
/// Exhaustive on purpose: a key absent from this table is never applied, so a
/// backend that grows the `services` section can never widen what this runner
/// writes. That is the same "unrecognized input falls to the inert branch" idiom
/// as `SectionPolicy::from_wire`'s unknown → `ReportOnly`.
pub const FIELDS: &[(&str, Field)] = &[
    ("database_url", Field::Url("database_url")),
    ("redis_url", Field::Url("redis_url")),
    ("coord_url", Field::Url("coord_url")),
    ("blob_kind", Field::BlobPlain("kind")),
    ("blob_endpoint", Field::BlobUrl("endpoint")),
    ("blob_bucket", Field::BlobPlain("bucket")),
];

/// Look up a `services` key in [`FIELDS`].
fn field_for(key: &str) -> Option<Field> {
    FIELDS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, field)| *field)
}

/// One write this apply would make to the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEdit {
    /// The `services` section key that produced this edit.
    pub key: String,
    /// JSON path inside the profile object (`["blob","endpoint"]`, …).
    pub path: Vec<String>,
    /// The profile's current value, when it had one.
    pub current: Option<String>,
    /// The value that would be written.
    pub proposed: String,
    /// True when the merge preserved local userinfo/path from `current`.
    pub preserved_local_parts: bool,
}

impl ServiceEdit {
    /// `current`, safe to print or log. A URL field is reduced to
    /// `scheme://host[:port]` so a DSN password can never reach a terminal or
    /// the audit log; a non-URL local value is elided entirely rather than
    /// guessed at.
    pub fn redacted_current(&self) -> Option<String> {
        self.current.as_deref().map(|v| redact(v, self.is_url()))
    }

    /// `proposed`, safe to print or log — see [`Self::redacted_current`].
    pub fn redacted_proposed(&self) -> String {
        redact(&self.proposed, self.is_url())
    }

    fn is_url(&self) -> bool {
        field_for(&self.key).map(Field::is_url).unwrap_or(true)
    }
}

/// Reduce a value to something safe to print. URL-valued fields go through the
/// same choke point the capture uses; anything that fails to parse is elided
/// (never echoed) because an unparseable DSN is exactly where a raw password
/// would sit.
fn redact(value: &str, is_url: bool) -> String {
    if !is_url {
        return value.to_string();
    }
    sanitize_url(value).unwrap_or_else(|| "<non-URL value elided>".to_string())
}

/// A `services` change that will NOT be applied, and why. Surfaced rather than
/// dropped: a silently-ignored change reads as "nothing to do".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSkip {
    pub key: String,
    pub reason: String,
}

/// The result of planning the `services` apply against a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServicePlan {
    pub edits: Vec<ServiceEdit>,
    pub skipped: Vec<ServiceSkip>,
    /// Advisory lines for the operator (e.g. blob credentials this apply cannot
    /// supply). Never a failure.
    pub notes: Vec<String>,
}

impl ServicePlan {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

/// Re-point `local` at `canonical`'s scheme, host and port while preserving
/// **everything else `local` carries** — userinfo, path, query and fragment.
///
/// This is the function the whole slice turns on. `postgres://user:pw@old:5432/mydb`
/// merged with canonical `postgres://new:5433` must yield
/// `postgres://user:pw@new:5433/mydb`: move the topology, keep the credentials
/// and the database name.
///
/// Errors (rather than guessing) when either side is not a parseable URL with a
/// host, or when the scheme change is not representable — the caller turns that
/// into a reported skip, never a write.
pub fn repoint_url(local: &str, canonical: &str) -> Result<String, String> {
    let target =
        url::Url::parse(canonical).map_err(|e| format!("canonical value is not a URL ({e})"))?;
    if target.host_str().is_none() {
        return Err("canonical value has no host".to_string());
    }
    let mut merged = url::Url::parse(local)
        .map_err(|e| format!("the local value is not a URL ({e}) — refusing to overwrite it"))?;
    if merged.host_str().is_none() {
        return Err("the local value has no host — refusing to overwrite it".to_string());
    }

    if merged.scheme() != target.scheme() {
        merged.set_scheme(target.scheme()).map_err(|_| {
            format!(
                "cannot change scheme {} -> {} without rewriting the whole URL",
                merged.scheme(),
                target.scheme()
            )
        })?;
    }
    merged
        .set_host(target.host_str())
        .map_err(|e| format!("cannot set host: {e}"))?;
    merged
        .set_port(target.port())
        .map_err(|_| "cannot set port on this URL".to_string())?;

    Ok(merged.to_string())
}

/// Plan the `services` apply: which profile fields would change, which reported
/// changes are not appliable, and why. **Pure — unit-tested.** No I/O.
///
/// `profile_obj` is the target profile as it exists in `profiles.json` (the raw
/// JSON object, so unknown keys are visible and preserved). `section` is the
/// pull's diff for `services`; only [`SectionPlan::actionable`] changes are
/// considered, so a non-applyable policy, an `Extra` local key and a repo-derived
/// key are all already excluded before this function sees them.
pub fn plan_services(section: &SectionPlan, profile_obj: &Map<String, Value>) -> ServicePlan {
    let mut plan = ServicePlan::default();
    let mut wants_blob_credentials = false;

    for change in section.actionable() {
        let (key, canonical) = match change {
            Change::Missing { key, canonical } => (key.as_str(), canonical.as_str()),
            Change::Differs { key, canonical, .. } => (key.as_str(), canonical.as_str()),
            // `actionable()` already filters these out; keep the arm total.
            Change::Extra { .. } => continue,
        };

        let Some(field) = field_for(key) else {
            plan.skipped.push(ServiceSkip {
                key: key.to_string(),
                reason: if key.starts_with("port_") {
                    "a liveness observation, not a setting — nothing to write".to_string()
                } else {
                    "not a profile setting this runner knows how to write".to_string()
                },
            });
            continue;
        };

        let path = field.json_path();
        let current = read_path(profile_obj, &path);

        let (proposed, preserved) = if field.is_url() {
            match current.as_deref() {
                // A local value exists: move ONLY scheme/host/port onto it.
                Some(local) => match repoint_url(local, canonical) {
                    Ok(merged) => (merged, true),
                    Err(reason) => {
                        plan.skipped.push(ServiceSkip {
                            key: key.to_string(),
                            reason,
                        });
                        continue;
                    }
                },
                // Nothing local to preserve — canonical's topology-only value is
                // the whole truth we have. Purely additive.
                None => (canonical.to_string(), false),
            }
        } else {
            (canonical.to_string(), false)
        };

        if already_matches(field, current.as_deref(), &proposed) {
            // The captured section is stale (or the two sides only render
            // differently): the stored value ALREADY carries canonical's
            // topology, so there is nothing to write. Comparing normalized forms
            // matters here — `http://h` and `http://h/` are the same URL, and a
            // byte comparison would emit a cosmetic-only rewrite of the file.
            continue;
        }

        if matches!(field, Field::BlobUrl(_) | Field::BlobPlain(_))
            && profile_obj.get("blob").and_then(Value::as_object).is_none()
        {
            wants_blob_credentials = true;
        }

        plan.edits.push(ServiceEdit {
            key: key.to_string(),
            path,
            current,
            proposed,
            preserved_local_parts: preserved,
        });
    }

    if wants_blob_credentials {
        plan.notes.push(
            "this profile has no `blob` block — the topology keys (kind/endpoint/bucket) will be \
             created, but `access_key`/`secret_key` are credentials canonical never carries and \
             must be supplied locally"
                .to_string(),
        );
    }

    plan.edits.sort_by(|a, b| a.key.cmp(&b.key));
    plan.skipped.sort_by(|a, b| a.key.cmp(&b.key));
    plan
}

/// True when the profile's stored value already carries the proposed topology.
///
/// URL fields compare **normalized** (`Url::parse(..).to_string()`) rather than
/// byte-for-byte: `http://h` and `http://h/` are the same URL, and a byte
/// comparison would rewrite the file for nothing. A value that does not parse
/// never counts as matching — the caller has already refused to overwrite it.
fn already_matches(field: Field, current: Option<&str>, proposed: &str) -> bool {
    let Some(current) = current else {
        return false;
    };
    if !field.is_url() {
        return current == proposed;
    }
    match url::Url::parse(current) {
        Ok(parsed) => parsed.to_string() == proposed,
        Err(_) => false,
    }
}

/// Read a nested string value out of a profile object.
fn read_path(profile_obj: &Map<String, Value>, path: &[String]) -> Option<String> {
    let mut cur = Value::Object(profile_obj.clone());
    for segment in path {
        cur = cur.get(segment)?.clone();
    }
    cur.as_str().map(str::to_string)
}

// ============================================================================
// Target selection
// ============================================================================

/// Why the `services` apply cannot run on this box. Every variant is a **report
/// and change nothing** outcome, never a partial write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// No home directory, so no `~/.qontinui/profiles.json` path exists.
    NoHomeDir,
    /// The runner is on the `legacy-env` fallback: there is no `profiles.json`
    /// to merge into. Synthesising one would be **inventing** configuration
    /// rather than reconciling it — same fail-safe shape as slice 3's
    /// no-version-manager case.
    NoProfilesFile(PathBuf),
    /// The file exists but could not be read or parsed. Refuse rather than
    /// clobber (mirrors `ensure_coord_url_at`).
    Unreadable(String),
    /// The selected profile is not present in the file. Creating it would again
    /// be inventing configuration — the capture this diff came from read the
    /// ACTIVE profile, so a missing one means the box is not in the state the
    /// diff describes.
    ProfileAbsent { name: String },
}

impl TargetError {
    pub fn message(&self) -> String {
        match self {
            Self::NoHomeDir => "could not resolve the home directory".to_string(),
            Self::NoProfilesFile(p) => format!(
                "no {} — this box resolves its services from legacy env vars, so there is no \
                 profile to reconcile. Writing one would invent configuration rather than \
                 reconcile it; reporting only.",
                p.display()
            ),
            Self::Unreadable(e) => format!("{e} — refusing to overwrite it"),
            Self::ProfileAbsent { name } => {
                format!("profile '{name}' is not present in profiles.json — refusing to create it")
            }
        }
    }
}

/// The profile this apply targets, plus the parsed file it lives in.
#[derive(Debug, Clone)]
pub struct ProfileTarget {
    pub path: PathBuf,
    /// Which named profile is being written.
    pub name: String,
    /// The whole parsed file, so the writer preserves every unknown key.
    pub root: Value,
}

impl ProfileTarget {
    /// The target profile's own JSON object.
    pub fn profile_obj(&self) -> Map<String, Value> {
        self.root
            .get("profiles")
            .and_then(|p| p.get(&self.name))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }
}

/// Which named profile an apply targets, resolved **explicitly** rather than by
/// implicitly writing whatever `profiles::load()` happened to resolve.
///
/// Order: an explicit `--profile` → `QONTINUI_ENV` → the file's `active` →
/// `"dev"`. That is exactly `profiles::load_inner`'s chain, which is what makes
/// the apply converge: the capture the diff came from read the ACTIVE profile,
/// so writing any other one would leave drift permanently unresolved.
pub fn select_profile_name(
    root: &Value,
    env_active: Option<&str>,
    explicit: Option<&str>,
) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| env_active.map(str::to_string))
        .or_else(|| {
            root.get("active")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "dev".to_string())
}

/// Resolve the apply target from a concrete path. Path-parameterized so tests
/// are hermetic (mirrors `profiles::ensure_coord_url_at`).
pub fn resolve_target_at(
    path: &Path,
    env_active: Option<&str>,
    explicit: Option<&str>,
) -> Result<ProfileTarget, TargetError> {
    if !path.exists() {
        return Err(TargetError::NoProfilesFile(path.to_path_buf()));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| TargetError::Unreadable(format!("reading {}: {e}", path.display())))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| TargetError::Unreadable(format!("parsing {}: {e}", path.display())))?;
    if !root.is_object() {
        return Err(TargetError::Unreadable(format!(
            "{}: root is not a JSON object",
            path.display()
        )));
    }

    let name = select_profile_name(&root, env_active, explicit);
    let present = root
        .get("profiles")
        .and_then(|p| p.get(&name))
        .map(Value::is_object)
        .unwrap_or(false);
    if !present {
        return Err(TargetError::ProfileAbsent { name });
    }

    Ok(ProfileTarget {
        path: path.to_path_buf(),
        name,
        root,
    })
}

/// Resolve the apply target for this machine (`~/.qontinui/profiles.json`).
pub fn resolve_target(explicit: Option<&str>) -> Result<ProfileTarget, TargetError> {
    let path = crate::profiles::profiles_path().ok_or(TargetError::NoHomeDir)?;
    let env_active = std::env::var("QONTINUI_ENV").ok();
    resolve_target_at(&path, env_active.as_deref(), explicit)
}

// ============================================================================
// The write
// ============================================================================

/// Apply `edits` to a parsed profiles file **in memory**. Only the named keys
/// are touched; every sibling — known or unknown, at any depth — rides along
/// untouched. **Pure — unit-tested.**
pub fn edit_root(
    root: &mut Value,
    profile_name: &str,
    edits: &[ServiceEdit],
) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let profiles = root
        .get_mut("profiles")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "\"profiles\" is not a JSON object".to_string())?;
    let profile = profiles
        .get_mut(profile_name)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("profile '{profile_name}' is not a JSON object"))?;

    for edit in edits {
        let (last, parents) = edit
            .path
            .split_last()
            .ok_or_else(|| format!("edit for '{}' has an empty path", edit.key))?;
        let mut cursor = &mut *profile;
        for segment in parents {
            let entry = cursor
                .entry(segment.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            cursor = entry
                .as_object_mut()
                .ok_or_else(|| format!("'{segment}' is not a JSON object"))?;
        }
        cursor.insert(last.clone(), Value::String(edit.proposed.clone()));
    }
    Ok(())
}

/// Write the edited file back atomically (tmp + rename), pretty-printed with a
/// trailing newline — the same shape `ensure_coord_url_at` leaves behind.
pub fn write_target(target: &ProfileTarget, root: &Value) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec_pretty(root).map_err(|e| format!("serialize profiles.json: {e}"))?;
    bytes.push(b'\n');
    let tmp = target.path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target.path)
        .map_err(|e| format!("rename {}: {e}", target.path.display()))?;
    Ok(())
}

// ============================================================================
// Driver entry point
// ============================================================================

/// Plan — and, with `confirm`, perform — the `services` apply for one section.
///
/// This module owns the conversion from its raw [`ServiceEdit`]s (which carry
/// real DSNs, because the write needs them) to the driver's redacted
/// [`AppliedChange`]s. Doing it here is what makes secret-safety structural: the
/// report type the renderers and `--json` see has never held a credential.
pub fn apply_section(
    section: &SectionPlan,
    explicit_profile: Option<&str>,
    confirm: bool,
) -> SectionApply {
    let name = section.section.as_str();

    let target = match resolve_target(explicit_profile) {
        Ok(t) => t,
        Err(e) => {
            let mut out = SectionApply::inert(name, SectionStatus::Blocked(e.message()));
            // A `legacy-env` box is a legitimate configuration, not a failure —
            // say what would have to change for an apply to become possible.
            if matches!(e, TargetError::NoProfilesFile(_)) {
                out.notes.push(
                    "create ~/.qontinui/profiles.json (signing in writes one) to make this \
                     section appliable"
                        .to_string(),
                );
            }
            return out;
        }
    };

    let plan = plan_services(section, &target.profile_obj());
    let mut out = SectionApply {
        section: name.to_string(),
        status: SectionStatus::NothingToDo,
        target: Some(format!("profile '{}'", target.name)),
        changes: to_changes(&plan.edits),
        skipped: plan
            .skipped
            .iter()
            .map(|s| SkipRecord {
                key: s.key.clone(),
                reason: s.reason.clone(),
            })
            .collect(),
        notes: plan.notes.clone(),
    };

    if plan.edits.is_empty() {
        return out;
    }
    if !confirm {
        out.status = SectionStatus::Planned;
        return out;
    }

    let mut root = target.root.clone();
    if let Err(e) = edit_root(&mut root, &target.name, &plan.edits) {
        out.status = SectionStatus::Blocked(e);
        out.changes.clear();
        return out;
    }
    match write_target(&target, &root) {
        Ok(()) => out.status = SectionStatus::Applied,
        Err(e) => {
            out.status = SectionStatus::Blocked(e);
            out.changes.clear();
        }
    }
    out
}

/// Redact raw edits into the driver's report vocabulary.
fn to_changes(edits: &[ServiceEdit]) -> Vec<AppliedChange> {
    edits
        .iter()
        .map(|e| AppliedChange {
            key: e.key.clone(),
            from: e.redacted_current(),
            to: e.redacted_proposed(),
            detail: e.preserved_local_parts.then(|| {
                "local credentials and path preserved — canonical carries topology only".to_string()
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_agent::pull::{compute_plan, CanonicalConfig};
    use serde_json::json;

    /// Build a `services` SectionPlan from canonical/local section literals via
    /// the real `compute_plan`, so these tests exercise the same policy and
    /// derived-key filtering the CLI does.
    fn services_section(canonical: Value, local: Value) -> SectionPlan {
        let canonical_cfg = CanonicalConfig {
            canonical_machine_id: Some("canon".to_string()),
            canonical_machine_name: Some("spaceship".to_string()),
            schema_version: None,
            captured_at: None,
            sections: json!({ "services": canonical })
                .as_object()
                .unwrap()
                .clone(),
            section_policy: json!({ "services": "applyable" })
                .as_object()
                .unwrap()
                .clone(),
            derived_keys: Map::new(),
        };
        let local_sections = json!({ "services": local });
        let plan = compute_plan(
            &canonical_cfg,
            local_sections.as_object().unwrap(),
            "env-1",
            "this-box",
        );
        plan.sections
            .into_iter()
            .find(|s| s.section == SERVICES_SECTION)
            .expect("services section planned")
    }

    fn profile(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    // ------------------------------------------------------------------
    // repoint_url — the load-bearing merge
    // ------------------------------------------------------------------

    /// The canonical case from the plan: move host and port, keep the password
    /// AND the database name. Getting this wrong destroys local credentials and
    /// points the box at the wrong database.
    #[test]
    fn repoint_preserves_userinfo_and_path() {
        assert_eq!(
            repoint_url("postgres://user:pw@old:5432/mydb", "postgres://new:5433").unwrap(),
            "postgres://user:pw@new:5433/mydb"
        );
    }

    #[test]
    fn repoint_preserves_query_and_fragment() {
        assert_eq!(
            repoint_url("redis://h:6379/0?ssl=true", "redis://newh:6380").unwrap(),
            "redis://newh:6380/0?ssl=true"
        );
    }

    /// A coord URL's `/ws` path is what makes it dialable — moving the host must
    /// not drop it.
    #[test]
    fn repoint_preserves_ws_path() {
        assert_eq!(
            repoint_url("ws://old:9870/ws", "ws://coord.example:9871").unwrap(),
            "ws://coord.example:9871/ws"
        );
    }

    #[test]
    fn repoint_moves_scheme_when_it_differs() {
        assert_eq!(
            repoint_url("http://old:9000/x", "https://new").unwrap(),
            "https://new/x"
        );
    }

    /// The legacy libpq keyword DSN (`host=… password=…`) is not a URL. It must
    /// be REFUSED, not overwritten — overwriting would blank the password.
    #[test]
    fn repoint_refuses_a_non_url_local_value() {
        let err = repoint_url(
            "host=localhost port=5432 user=q password=secret dbname=qontinui_db",
            "postgres://new:5433",
        )
        .unwrap_err();
        assert!(err.contains("refusing to overwrite"), "got {err}");
    }

    #[test]
    fn repoint_refuses_a_non_url_canonical_value() {
        assert!(repoint_url("postgres://u:p@h:5432/db", "not a url").is_err());
    }

    // ------------------------------------------------------------------
    // plan_services
    // ------------------------------------------------------------------

    /// End-to-end for the constraint: canonical carries topology only, and the
    /// planned write still has the local password and database name.
    #[test]
    fn plan_repoints_dsn_and_keeps_credentials() {
        let section = services_section(
            json!({"database_url": "postgres://new:5433"}),
            json!({"database_url": "postgres://old:5432"}),
        );
        let plan = plan_services(
            &section,
            &profile(json!({"database_url": "postgres://user:pw@old:5432/mydb"})),
        );
        assert_eq!(plan.edits.len(), 1);
        let edit = &plan.edits[0];
        assert_eq!(edit.proposed, "postgres://user:pw@new:5433/mydb");
        assert!(edit.preserved_local_parts);
        assert_eq!(edit.path, vec!["database_url".to_string()]);
    }

    /// `port_*` keys drift on every capture (a box with the backend down differs
    /// from canonical) and are the single most likely thing to be mistaken for an
    /// action. They must produce NO edit — only a reported skip.
    #[test]
    fn port_keys_produce_no_apply_action() {
        let section = services_section(
            json!({"port_backend_8000": "listening", "port_redis_6379": "listening"}),
            json!({"port_backend_8000": "closed", "port_redis_6379": "closed"}),
        );
        let plan = plan_services(&section, &profile(json!({})));
        assert!(plan.edits.is_empty(), "got {:?}", plan.edits);
        assert_eq!(plan.skipped.len(), 2);
        assert!(plan.skipped[0].reason.contains("liveness observation"));
    }

    /// A `services` key outside the fixed set is never written, so a backend
    /// growing the section cannot widen what this runner mutates.
    #[test]
    fn unknown_services_key_is_skipped_not_written() {
        let section = services_section(
            json!({"future_service_url": "https://new"}),
            json!({"future_service_url": "https://old"}),
        );
        let plan = plan_services(&section, &profile(json!({})));
        assert!(plan.edits.is_empty());
        assert_eq!(plan.skipped[0].key, "future_service_url");
    }

    /// A non-applyable policy yields nothing, even for keys in the fixed set —
    /// the policy gate lives in `SectionPlan::actionable`, and this pins that the
    /// apply honours it rather than re-deriving permission.
    #[test]
    fn non_applyable_policy_yields_no_edits() {
        let canonical_cfg = CanonicalConfig {
            canonical_machine_id: Some("canon".to_string()),
            canonical_machine_name: None,
            schema_version: None,
            captured_at: None,
            sections: json!({"services": {"database_url": "postgres://new:5433"}})
                .as_object()
                .unwrap()
                .clone(),
            // The server did not mark it applyable.
            section_policy: json!({"services": "report_only"})
                .as_object()
                .unwrap()
                .clone(),
            derived_keys: Map::new(),
        };
        let local = json!({"services": {"database_url": "postgres://old:5432"}});
        let full = compute_plan(&canonical_cfg, local.as_object().unwrap(), "e", "box");
        let section = full.sections.into_iter().next().unwrap();
        let plan = plan_services(
            &section,
            &profile(json!({"database_url": "postgres://u:p@old:5432/db"})),
        );
        assert!(plan.edits.is_empty());
    }

    /// A local value that is not a URL is REPORTED, never overwritten.
    #[test]
    fn unparseable_local_value_is_skipped_with_a_reason() {
        let section = services_section(
            json!({"database_url": "postgres://new:5433"}),
            json!({"database_url": "postgres://old:5432"}),
        );
        let plan = plan_services(
            &section,
            &profile(json!({"database_url": "host=localhost password=secret dbname=q"})),
        );
        assert!(plan.edits.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("refusing to overwrite"));
    }

    /// Nothing local to preserve ⇒ canonical's topology-only value is written
    /// as-is. Purely additive.
    #[test]
    fn missing_local_key_is_added_from_canonical() {
        let section = services_section(json!({"redis_url": "redis://new:6380"}), json!({}));
        let plan = plan_services(&section, &profile(json!({})));
        assert_eq!(plan.edits.len(), 1);
        assert_eq!(plan.edits[0].proposed, "redis://new:6380");
        assert!(!plan.edits[0].preserved_local_parts);
    }

    /// The blob sub-fields land under `blob`, and a profile with no blob block
    /// gets a note that credentials cannot come from canonical.
    #[test]
    fn blob_fields_target_the_blob_object_and_note_missing_credentials() {
        let section = services_section(
            json!({"blob_kind": "minio", "blob_bucket": "qontinui-dev",
                   "blob_endpoint": "http://new:9000"}),
            json!({}),
        );
        let plan = plan_services(&section, &profile(json!({})));
        let paths: Vec<Vec<String>> = plan.edits.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&vec!["blob".to_string(), "kind".to_string()]));
        assert!(paths.contains(&vec!["blob".to_string(), "endpoint".to_string()]));
        assert!(paths.contains(&vec!["blob".to_string(), "bucket".to_string()]));
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.notes[0].contains("access_key"));
    }

    /// A STALE capture (the file was already updated out of band) must produce
    /// no edit: the stored value already carries canonical's topology. The
    /// comparison is normalized, so `http://new:9000` vs `http://new:9000/` is
    /// not a cosmetic-only rewrite of the operator's file.
    #[test]
    fn already_matching_stored_value_produces_no_edit() {
        let section = services_section(
            json!({"blob_endpoint": "http://new:9000"}),
            json!({"blob_endpoint": "http://old:9000"}),
        );
        let plan = plan_services(
            &section,
            &profile(json!({"blob": {"endpoint": "http://new:9000"}})),
        );
        assert!(plan.edits.is_empty(), "got {:?}", plan.edits);
    }

    // ------------------------------------------------------------------
    // Secret safety — the apply-side mirror of `collectors::secret_safety_*`
    // ------------------------------------------------------------------

    /// A profile carrying real credentials must survive an apply with all of
    /// them intact, and `auth` must be byte-identical.
    #[test]
    fn secret_safety_credentials_survive_an_apply() {
        let original = json!({
            "active": "dev",
            "profiles": {
                "dev": {
                    "database_url": "postgres://user:pw@old:5432/mydb",
                    "redis_url": "redis://old:6379/0",
                    "coord_url": "ws://old:9870/ws",
                    "blob": {
                        "kind": "minio",
                        "endpoint": "http://old:9000",
                        "region": "us-east-1",
                        "access_key": "AKIAEXAMPLE",
                        "secret_key": "s3cr3t-key",
                        "bucket": "old-bucket"
                    },
                    "auth": {"kind": "static-dev-token", "token": "dev-token-123"}
                }
            }
        });
        let section = services_section(
            json!({
                "database_url": "postgres://new:5433",
                "redis_url": "redis://new:6380",
                "coord_url": "ws://new:9871",
                "blob_kind": "s3",
                "blob_endpoint": "https://s3.example",
                "blob_bucket": "new-bucket",
                "port_backend_8000": "listening",
            }),
            json!({
                "database_url": "postgres://old:5432",
                "redis_url": "redis://old:6379",
                "coord_url": "ws://old:9870",
                "blob_kind": "minio",
                "blob_endpoint": "http://old:9000",
                "blob_bucket": "old-bucket",
                "port_backend_8000": "closed",
            }),
        );
        let profile_obj = profile(original["profiles"]["dev"].clone());
        let plan = plan_services(&section, &profile_obj);

        let mut root = original.clone();
        edit_root(&mut root, "dev", &plan.edits).unwrap();
        let dev = &root["profiles"]["dev"];

        // Topology moved…
        assert_eq!(dev["database_url"], "postgres://user:pw@new:5433/mydb");
        assert_eq!(dev["redis_url"], "redis://new:6380/0");
        assert_eq!(dev["coord_url"], "ws://new:9871/ws");
        assert_eq!(dev["blob"]["kind"], "s3");
        assert_eq!(dev["blob"]["endpoint"], "https://s3.example/");
        assert_eq!(dev["blob"]["bucket"], "new-bucket");

        // …and every credential survived.
        assert_eq!(dev["blob"]["access_key"], "AKIAEXAMPLE");
        assert_eq!(dev["blob"]["secret_key"], "s3cr3t-key");
        assert_eq!(dev["blob"]["region"], "us-east-1");
        assert_eq!(
            dev["auth"], original["profiles"]["dev"]["auth"],
            "the auth block must never be touched"
        );
    }

    /// Neither the rendered nor the logged form of an edit may carry a DSN
    /// password — both go through the capture's own sanitizer.
    #[test]
    fn secret_safety_redaction_drops_the_password() {
        let edit = ServiceEdit {
            key: "database_url".to_string(),
            path: vec!["database_url".to_string()],
            current: Some("postgres://user:hunter2@old:5432/mydb".to_string()),
            proposed: "postgres://user:hunter2@new:5433/mydb".to_string(),
            preserved_local_parts: true,
        };
        let current = edit.redacted_current().unwrap();
        let proposed = edit.redacted_proposed();
        assert_eq!(current, "postgres://old:5432");
        assert_eq!(proposed, "postgres://new:5433");
        assert!(!current.contains("hunter2"));
        assert!(!proposed.contains("hunter2"));
    }

    /// An unparseable value is ELIDED rather than echoed — an unparseable DSN is
    /// exactly where a raw password sits.
    #[test]
    fn secret_safety_unparseable_value_is_elided_not_echoed() {
        let edit = ServiceEdit {
            key: "database_url".to_string(),
            path: vec!["database_url".to_string()],
            current: Some("host=localhost password=hunter2 dbname=q".to_string()),
            proposed: "postgres://new:5433".to_string(),
            preserved_local_parts: false,
        };
        let current = edit.redacted_current().unwrap();
        assert!(!current.contains("hunter2"), "got {current}");
        assert!(current.contains("elided"));
    }

    /// Plain, non-secret blob fields are shown verbatim — redaction must not
    /// turn a bucket name into `<non-URL value elided>`.
    #[test]
    fn plain_blob_fields_are_not_redacted() {
        let edit = ServiceEdit {
            key: "blob_bucket".to_string(),
            path: vec!["blob".to_string(), "bucket".to_string()],
            current: Some("old-bucket".to_string()),
            proposed: "new-bucket".to_string(),
            preserved_local_parts: false,
        };
        assert_eq!(edit.redacted_current().as_deref(), Some("old-bucket"));
        assert_eq!(edit.redacted_proposed(), "new-bucket");
    }

    /// The boundary that makes secret-safety structural: raw edits carry real
    /// DSNs (the write needs them), and everything crossing into the driver's
    /// report has been through the sanitizer.
    #[test]
    fn secret_safety_report_conversion_redacts_every_change() {
        let raw = vec![
            ServiceEdit {
                key: "database_url".to_string(),
                path: vec!["database_url".to_string()],
                current: Some("postgres://u:hunter2@old:5432/mydb".to_string()),
                proposed: "postgres://u:hunter2@new:5433/mydb".to_string(),
                preserved_local_parts: true,
            },
            ServiceEdit {
                key: "blob_bucket".to_string(),
                path: vec!["blob".to_string(), "bucket".to_string()],
                current: None,
                proposed: "new-bucket".to_string(),
                preserved_local_parts: false,
            },
        ];
        let changes = to_changes(&raw);
        let rendered = format!("{changes:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert_eq!(changes[0].from.as_deref(), Some("postgres://old:5432"));
        assert_eq!(changes[0].to, "postgres://new:5433");
        assert!(changes[0].detail.is_some());
        // A plain, non-secret field is passed through verbatim and carries no
        // "credentials preserved" claim.
        assert_eq!(changes[1].to, "new-bucket");
        assert!(changes[1].detail.is_none());
    }

    // ------------------------------------------------------------------
    // edit_root — unknown keys survive
    // ------------------------------------------------------------------

    #[test]
    fn edit_root_preserves_unknown_keys_everywhere() {
        let mut root = json!({
            "active": "dev",
            "unknown_top_level": {"keep": "me"},
            "profiles": {
                "dev": {"database_url": "postgres://u:p@old:5432/db", "unknown_key": 42},
                "staging": {"database_url": "postgres://s:s@stage:5432/db"}
            }
        });
        let edits = vec![ServiceEdit {
            key: "database_url".to_string(),
            path: vec!["database_url".to_string()],
            current: Some("postgres://u:p@old:5432/db".to_string()),
            proposed: "postgres://u:p@new:5433/db".to_string(),
            preserved_local_parts: true,
        }];
        edit_root(&mut root, "dev", &edits).unwrap();
        assert_eq!(
            root["profiles"]["dev"]["database_url"],
            "postgres://u:p@new:5433/db"
        );
        assert_eq!(root["profiles"]["dev"]["unknown_key"], 42);
        assert_eq!(root["unknown_top_level"]["keep"], "me");
        assert_eq!(
            root["profiles"]["staging"]["database_url"], "postgres://s:s@stage:5432/db",
            "a non-target profile must never be touched"
        );
    }

    #[test]
    fn edit_root_with_no_edits_is_a_no_op() {
        let original = json!({"profiles": {"dev": {}}});
        let mut root = original.clone();
        edit_root(&mut root, "dev", &[]).unwrap();
        assert_eq!(root, original);
    }

    // ------------------------------------------------------------------
    // Target selection
    // ------------------------------------------------------------------

    /// The `legacy-env` box: no profiles.json ⇒ report and change nothing.
    /// Synthesising a file would be inventing configuration.
    #[test]
    fn legacy_env_source_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let err = resolve_target_at(&path, None, None).unwrap_err();
        assert!(matches!(err, TargetError::NoProfilesFile(_)));
        assert!(err.message().contains("legacy env vars"));
        assert!(!path.exists(), "no file may be created");
    }

    #[test]
    fn target_selection_follows_the_profiles_resolution_chain() {
        let root = json!({"active": "staging", "profiles": {}});
        assert_eq!(select_profile_name(&root, None, None), "staging");
        assert_eq!(select_profile_name(&root, Some("cloud"), None), "cloud");
        assert_eq!(
            select_profile_name(&root, Some("cloud"), Some("prod")),
            "prod",
            "an explicit --profile wins over QONTINUI_ENV"
        );
        assert_eq!(
            select_profile_name(&json!({"profiles": {}}), None, None),
            "dev",
            "no active and no override falls back to dev"
        );
    }

    #[test]
    fn absent_profile_is_refused_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, br#"{"active":"dev","profiles":{"dev":{}}}"#).unwrap();
        let err = resolve_target_at(&path, None, Some("nope")).unwrap_err();
        assert_eq!(
            err,
            TargetError::ProfileAbsent {
                name: "nope".to_string()
            }
        );
    }

    #[test]
    fn unparseable_profiles_file_is_refused_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            resolve_target_at(&path, None, None),
            Err(TargetError::Unreadable(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    #[test]
    fn write_target_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            br#"{"active":"dev","profiles":{"dev":{"database_url":"postgres://u:p@old:5432/db"}}}"#,
        )
        .unwrap();
        let target = resolve_target_at(&path, None, None).unwrap();
        assert_eq!(target.name, "dev");
        let mut root = target.root.clone();
        edit_root(
            &mut root,
            "dev",
            &[ServiceEdit {
                key: "database_url".to_string(),
                path: vec!["database_url".to_string()],
                current: Some("postgres://u:p@old:5432/db".to_string()),
                proposed: "postgres://u:p@new:5433/db".to_string(),
                preserved_local_parts: true,
            }],
        )
        .unwrap();
        write_target(&target, &root).unwrap();

        let reread: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            reread["profiles"]["dev"]["database_url"],
            "postgres://u:p@new:5433/db"
        );
        // The atomic-write temp file must not be left behind.
        assert!(!path.with_extension("json.tmp").exists());
    }
}
