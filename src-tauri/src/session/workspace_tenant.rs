//! The workspace's own statement about which tenant a session acts for.
//!
//! Plan `2026-09-20-per-tenant-coord-credentials-and-a-workspace-tenant-pin`
//! D1, Phase 1. The machine's tenant pin (`~/.qontinui/machine.json`
//! `active_tenant_id`, [`crate::session::tenant_pin`]) is MACHINE-wide, so on a
//! box that works in two tenants' repos it is wrong for every workspace but
//! one. The workaround in use has been repointing the whole machine, which
//! silently makes every OTHER workspace act as the wrong tenant.
//!
//! This module reads three declarations, in order, and stops at the first one
//! that says anything:
//!
//! | Tier | Signal | Scope |
//! |---|---|---|
//! | 1 | `$QONTINUI_TENANT_ID` | the process |
//! | 2 | `tenant:` in `<workspace>/.qontinui/config.yml` | the repo |
//! | 3 | longest-matching path prefix in `~/.qontinui/tenant-map.json` | the machine |
//!
//! **A declaration only SELECTS; it never grants.** Nothing here decides
//! anything on its own — [`crate::coord_mcp::decide_session_tenant`] hands
//! every [`WorkspaceDeclaration::Declared`] tenant to the admission rule that
//! already ships for spawns (`coord_mcp::spawn_tenant_admission`, whose
//! admitted set is "slots held ∪ the default binding") and refuses a tenant
//! outside it. A repo file can therefore pick among bindings a human already
//! paired, and can do nothing else.
//!
//! ## The absence/malformed asymmetry, and where it does NOT apply
//!
//! [`crate::session::tenant_pin`] already encodes it: a *missing* field is
//! fine, a *malformed value* for that field refuses — a machine that tried to
//! state its tenant and produced garbage is not one that never tried. Every
//! tier here follows it: an absent env var / absent file / absent `tenant:`
//! key / no matching prefix is [`WorkspaceDeclaration::Absent`] and
//! contributes nothing, while a present-but-unparseable tenant is
//! [`WorkspaceDeclaration::Unusable`] and refuses.
//!
//! The ONE deliberate exception is tier 2's YAML parse. `.qontinui/config.yml`
//! is **qontinui-web's PR-merge-policy file** (its migration is
//! `backend/alembic/versions/cfgyaml01_config_yaml_overrides.py`); the runner
//! has no other reader for it, and this one reads the `tenant:` key and
//! nothing else. A `config.yml` that does not parse as YAML is a broken
//! merge-policy file, and a broken merge-policy file must not take a session's
//! credential down — so it is `Absent`, not `Unusable`. A `tenant:` key that
//! IS present and is not a uuid is a stated tenant we cannot honor, and that
//! does refuse. That keeps the fail-closed rule on the *tenant statement*
//! without coupling credential selection to a foreign schema's health.
//!
//! ## Purity
//!
//! [`read_workspace_declaration`] is the only function here that touches the
//! environment or the filesystem. Every classification rule is a pure function
//! over already-read bytes, so the authority order and every parse rule are
//! unit-testable without `$HOME`.

use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The env var tier 1 reads.
///
/// **This spelling is not a choice.** The fleet already resolves "the tenant
/// this session acts for" from `QONTINUI_TENANT_ID`
/// (`fleet_skills/coord-revive/coord-revive.sh:3531-3536`, and Step 0 of
/// `/vet-plan`, `/implement-plan`, `/implement-phase`). A second spelling for
/// the same concept would put two env vars on the box that can disagree about
/// which tenant a session is.
pub(crate) const SESSION_TENANT_ENV: &str = "QONTINUI_TENANT_ID";

/// `<qontinui_dir>/tenant-map.json` — tier 3's file name.
pub(crate) const TENANT_MAP_FILENAME: &str = "tenant-map.json";

/// Which tier declared, and what it declared on — carried into the decision so
/// a refusal can name the exact door the operator has to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantDeclarationSource {
    /// Tier 1: the `QONTINUI_TENANT_ID` environment variable.
    Env,
    /// Tier 2: the `tenant:` key of this workspace's `.qontinui/config.yml`.
    ConfigYml { path: PathBuf },
    /// Tier 3: `~/.qontinui/tenant-map.json`. `prefix` is the path prefix that
    /// matched, or `None` when the FILE itself is what could not be read.
    TenantMap {
        path: PathBuf,
        prefix: Option<String>,
    },
}

impl fmt::Display for TenantDeclarationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TenantDeclarationSource::Env => write!(f, "${SESSION_TENANT_ENV}"),
            TenantDeclarationSource::ConfigYml { path } => {
                write!(f, "the `tenant:` key in {}", path.display())
            }
            TenantDeclarationSource::TenantMap {
                path,
                prefix: Some(prefix),
            } => write!(
                f,
                "the longest-matching path prefix `{prefix}` in {}",
                path.display()
            ),
            TenantDeclarationSource::TenantMap { path, prefix: None } => {
                write!(f, "{}", path.display())
            }
        }
    }
}

/// What the workspace tiers say, as one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceDeclaration {
    /// No tier stated anything. **Absence is not a fault** — it contributes
    /// nothing and the authority order falls through to the machine pin
    /// exactly as it did before this module existed.
    Absent,
    /// A tier named a tenant. Still subject to admission: naming a tenant is
    /// not holding a credential for it.
    Declared {
        tenant: Uuid,
        source: TenantDeclarationSource,
    },
    /// A tier stated a tenant that cannot be read as one (a non-uuid env
    /// value / `tenant:` key / map entry, or a `tenant-map.json` that is
    /// present but unparseable). Refuses — silently ignoring a stated tenant
    /// would act as the device's DEFAULT tenant, which is the cross-tenant
    /// write this whole design exists to prevent.
    Unusable {
        source: TenantDeclarationSource,
        detail: String,
    },
}

/// Read the three tiers, in authority order, stopping at the first that says
/// anything. The one impure function in this module.
///
/// `workdir` is the workspace the session was provisioned into
/// (`coord_mcp::workdir_for_nonce`). `None` — a binding registered with no
/// workdir — is "no workspace to ask", which is `Absent`, not a fault; tier 1
/// is process-scoped and still applies.
pub(crate) fn read_workspace_declaration(workdir: Option<&str>) -> WorkspaceDeclaration {
    // Tier 1 — the process. Needs no workspace.
    let env = std::env::var(SESSION_TENANT_ENV).ok();
    match classify_env(env.as_deref()) {
        WorkspaceDeclaration::Absent => {}
        declared => return declared,
    }

    let Some(workdir) = workdir else {
        return WorkspaceDeclaration::Absent;
    };

    // Tier 2 — the repo.
    let config_path = Path::new(workdir).join(".qontinui").join("config.yml");
    let config_bytes = std::fs::read(&config_path).ok();
    match classify_config_yml(&config_path, config_bytes.as_deref()) {
        WorkspaceDeclaration::Absent => {}
        declared => return declared,
    }

    // Tier 3 — the machine's path map. `ambient::qontinui_dir()` is the ONE seam
    // allowed to spell `.qontinui` next to a home directory, and the one a
    // test fixture can steer; a local `dirs::home_dir()` here would be an
    // unguarded ambient read.
    let Some(map_path) =
        qontinui_runner_lib::ambient::qontinui_dir().map(|d| d.join(TENANT_MAP_FILENAME))
    else {
        return WorkspaceDeclaration::Absent;
    };
    let map_bytes = std::fs::read(&map_path).ok();
    classify_tenant_map(&map_path, workdir, map_bytes.as_deref())
}

/// Tier 1's rule, pure. A blank value is not an override — it is the shape a
/// `systemd` unit's `Environment=FOO=` produces, and the same non-override
/// `crate::ambient::qontinui_dir_from` already treats it as.
pub(crate) fn classify_env(raw: Option<&str>) -> WorkspaceDeclaration {
    let Some(raw) = raw else {
        return WorkspaceDeclaration::Absent;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return WorkspaceDeclaration::Absent;
    }
    match Uuid::parse_str(trimmed) {
        Ok(tenant) => WorkspaceDeclaration::Declared {
            tenant,
            source: TenantDeclarationSource::Env,
        },
        Err(e) => WorkspaceDeclaration::Unusable {
            source: TenantDeclarationSource::Env,
            detail: format!("`{trimmed}` is not a tenant uuid ({e})"),
        },
    }
}

/// Only the `tenant:` key of a `config.yml`, and nothing else in it.
///
/// Deliberately deserialized into a one-field struct: serde ignores every
/// other and every unknown key, so a future qontinui-web merge-policy
/// migration cannot become a compatibility surface for credential selection.
#[derive(serde::Deserialize)]
struct ConfigYmlTenantOnly {
    #[serde(default)]
    tenant: Option<serde_yaml::Value>,
}

/// Tier 2's rule, pure over already-read bytes.
///
/// `bytes` is `None` when the file is absent or unreadable — both `Absent`.
/// A YAML parse failure is ALSO `Absent`: see the module docs.
pub(crate) fn classify_config_yml(path: &Path, bytes: Option<&[u8]>) -> WorkspaceDeclaration {
    let Some(bytes) = bytes else {
        return WorkspaceDeclaration::Absent;
    };
    let Ok(parsed) = serde_yaml::from_slice::<ConfigYmlTenantOnly>(bytes) else {
        // A broken merge-policy file must not take a session's credential
        // down. This is the ONE place the absence/malformed asymmetry is
        // deliberately relaxed, and it is relaxed on the FILE, never on a
        // tenant statement.
        return WorkspaceDeclaration::Absent;
    };
    let source = TenantDeclarationSource::ConfigYml {
        path: path.to_path_buf(),
    };
    classify_declared_value(
        parsed.tenant.as_ref().and_then(|v| match v {
            // Key present but explicitly null: the operator never stated one.
            serde_yaml::Value::Null => None,
            other => Some(other.as_str().map(str::to_string).ok_or_else(|| {
                format!("`tenant:` is {}, not a tenant uuid string", kind_of(other))
            })),
        }),
        source,
    )
}

/// The YAML kind of a `tenant:` value we refused, for the refusal text.
fn kind_of(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a boolean",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a list",
        serde_yaml::Value::Mapping(_) => "a mapping",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    }
}

/// Shared tail of tiers 2 and 3: `None` = nothing stated, `Some(Err)` = stated
/// but not even a string, `Some(Ok(s))` = stated as `s`.
fn classify_declared_value(
    stated: Option<Result<String, String>>,
    source: TenantDeclarationSource,
) -> WorkspaceDeclaration {
    match stated {
        None => WorkspaceDeclaration::Absent,
        Some(Err(detail)) => WorkspaceDeclaration::Unusable { source, detail },
        Some(Ok(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                // An empty string is a stated tenant with no tenant in it.
                return WorkspaceDeclaration::Unusable {
                    source,
                    detail: "the declared tenant is empty".to_string(),
                };
            }
            match Uuid::parse_str(trimmed) {
                Ok(tenant) => WorkspaceDeclaration::Declared { tenant, source },
                Err(e) => WorkspaceDeclaration::Unusable {
                    source,
                    detail: format!("`{trimmed}` is not a tenant uuid ({e})"),
                },
            }
        }
    }
}

/// `~/.qontinui/tenant-map.json`: path prefixes this machine maps to tenants.
///
/// ```json
/// { "version": 1, "entries": [ { "path": "D:/portofino-pizzeria", "tenant": "<uuid>" } ] }
/// ```
///
/// For paths outside any repo, and for repos whose owner does not want tenancy
/// in a committed file. Unknown keys (`version`, anything later) are ignored;
/// a missing `entries` is an empty map, not an error.
#[derive(serde::Deserialize)]
struct TenantMapFile {
    #[serde(default)]
    entries: Vec<TenantMapEntry>,
}

#[derive(serde::Deserialize)]
struct TenantMapEntry {
    path: String,
    tenant: serde_json::Value,
}

/// Tier 3's rule, pure over already-read bytes.
///
/// An absent file is `Absent`. A file that is PRESENT and does not parse is
/// `Unusable` — unlike tier 2, this file exists for exactly one purpose, so a
/// parse failure is a broken tenant statement, not collateral from a foreign
/// schema.
pub(crate) fn classify_tenant_map(
    path: &Path,
    workdir: &str,
    bytes: Option<&[u8]>,
) -> WorkspaceDeclaration {
    let Some(bytes) = bytes else {
        return WorkspaceDeclaration::Absent;
    };
    let parsed: TenantMapFile = match serde_json::from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            return WorkspaceDeclaration::Unusable {
                source: TenantDeclarationSource::TenantMap {
                    path: path.to_path_buf(),
                    prefix: None,
                },
                detail: format!("the tenant map does not parse ({e})"),
            }
        }
    };
    let target = normalize_path(workdir);
    // Longest matching prefix wins: a map may name both a root and a repo
    // inside it, and the inner statement is the more specific one. Two
    // entries with the SAME normalized prefix are a config error; the last
    // one in the file wins, deterministically (`max_by_key` keeps the last
    // maximum).
    let Some(entry) = parsed
        .entries
        .iter()
        .filter_map(|e| {
            let prefix = normalize_path(&e.path);
            prefix_matches(&prefix, &target).then_some((prefix, e))
        })
        .max_by_key(|(prefix, _)| prefix.len())
    else {
        return WorkspaceDeclaration::Absent;
    };
    let (prefix, entry) = entry;
    let source = TenantDeclarationSource::TenantMap {
        path: path.to_path_buf(),
        prefix: Some(prefix),
    };
    classify_declared_value(
        Some(
            entry
                .tenant
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "the mapped `tenant` is not a uuid string".to_string()),
        ),
        source,
    )
}

/// Separators normalized and trailing slashes dropped, so `D:\repo\` and
/// `D:/repo` are the same prefix.
fn normalize_path(p: &str) -> String {
    let swapped = p.trim().replace('\\', "/");
    let trimmed = swapped.trim_end_matches('/');
    // A path that was ONLY slashes normalizes to empty, which `prefix_matches`
    // refuses — an empty prefix would match every workspace on the box.
    trimmed.to_string()
}

/// Case-sensitivity follows the platform's own path semantics: Windows paths
/// are case-insensitive, so a map entry written `D:/Repo` must still match a
/// workdir the runner recorded as `D:/repo`.
#[cfg(windows)]
fn path_segment_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(not(windows))]
fn path_segment_eq(a: &str, b: &str) -> bool {
    a == b
}

/// `prefix` covers `target` only on a whole path SEGMENT boundary: `D:/repo`
/// covers `D:/repo` and `D:/repo/sub`, and never `D:/repo-other`.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn prefix_matches(prefix: &str, target: &str) -> bool {
    if prefix.is_empty() || target.is_empty() {
        return false;
    }
    if path_segment_eq(prefix, target) {
        return true;
    }
    target.len() > prefix.len()
        && target.is_char_boundary(prefix.len())
        && target.as_bytes()[prefix.len()] == b'/'
        && path_segment_eq(&target[..prefix.len()], prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn cfg_path() -> PathBuf {
        PathBuf::from("D:/repo/.qontinui/config.yml")
    }

    fn map_path() -> PathBuf {
        PathBuf::from("C:/home/.qontinui/tenant-map.json")
    }

    // ---- tier 1 -----------------------------------------------------------

    #[test]
    fn env_uuid_declares() {
        let t = tenant(0xA1);
        assert_eq!(
            classify_env(Some(&t.to_string())),
            WorkspaceDeclaration::Declared {
                tenant: t,
                source: TenantDeclarationSource::Env
            }
        );
    }

    #[test]
    fn env_absent_or_blank_declares_nothing() {
        assert_eq!(classify_env(None), WorkspaceDeclaration::Absent);
        assert_eq!(classify_env(Some("   ")), WorkspaceDeclaration::Absent);
    }

    #[test]
    fn env_non_uuid_is_unusable_not_silently_dropped() {
        match classify_env(Some("portofino")) {
            WorkspaceDeclaration::Unusable { source, detail } => {
                assert_eq!(source, TenantDeclarationSource::Env);
                assert!(detail.contains("portofino"), "{detail}");
            }
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    // ---- tier 2 -----------------------------------------------------------

    #[test]
    fn config_yml_tenant_key_declares_and_other_keys_are_ignored() {
        let t = tenant(0xB2);
        let yml =
            format!("version: 2\nframework: qontinui\nmerge:\n  policy: squash\ntenant: \"{t}\"\n");
        assert_eq!(
            classify_config_yml(&cfg_path(), Some(yml.as_bytes())),
            WorkspaceDeclaration::Declared {
                tenant: t,
                source: TenantDeclarationSource::ConfigYml { path: cfg_path() }
            }
        );
    }

    #[test]
    fn config_yml_without_a_tenant_key_declares_nothing() {
        let yml = b"version: 2\nmerge:\n  policy: squash\n";
        assert_eq!(
            classify_config_yml(&cfg_path(), Some(yml.as_slice())),
            WorkspaceDeclaration::Absent
        );
        assert_eq!(
            classify_config_yml(&cfg_path(), Some(b"tenant: null\n")),
            WorkspaceDeclaration::Absent
        );
        assert_eq!(
            classify_config_yml(&cfg_path(), None),
            WorkspaceDeclaration::Absent
        );
    }

    /// THE robustness rule for tier 2: a broken merge-policy file must not
    /// take a session's credential down.
    #[test]
    fn config_yml_that_is_not_yaml_is_absent_not_a_refusal() {
        assert_eq!(
            // An unclosed flow sequence hitting EOF: unambiguously a YAML
            // parse error, and one that names `tenant:` — so a pass here
            // cannot be the weaker "it parsed, there was just no tenant key".
            classify_config_yml(&cfg_path(), Some(b"tenant: [unclosed-flow-sequence\n")),
            WorkspaceDeclaration::Absent
        );
    }

    #[test]
    fn config_yml_tenant_key_that_is_not_a_uuid_is_unusable() {
        for bad in [
            "tenant: not-a-uuid\n".to_string(),
            "tenant: 42\n".to_string(),
            "tenant: \"\"\n".to_string(),
        ] {
            match classify_config_yml(&cfg_path(), Some(bad.as_bytes())) {
                WorkspaceDeclaration::Unusable { source, .. } => {
                    assert_eq!(
                        source,
                        TenantDeclarationSource::ConfigYml { path: cfg_path() }
                    )
                }
                other => panic!("expected Unusable for {bad:?}, got {other:?}"),
            }
        }
    }

    // ---- tier 3 -----------------------------------------------------------

    fn map_json(entries: &str) -> String {
        format!("{{\"version\":1,\"entries\":[{entries}]}}")
    }

    #[test]
    fn tenant_map_resolves_by_longest_prefix() {
        let outer = tenant(0xC3);
        let inner = tenant(0xD4);
        let json = map_json(&format!(
            "{{\"path\":\"D:/work\",\"tenant\":\"{outer}\"}},\
             {{\"path\":\"D:/work/pizzeria\",\"tenant\":\"{inner}\"}}"
        ));
        assert_eq!(
            classify_tenant_map(
                &map_path(),
                "D:/work/pizzeria/mobile",
                Some(json.as_bytes())
            ),
            WorkspaceDeclaration::Declared {
                tenant: inner,
                source: TenantDeclarationSource::TenantMap {
                    path: map_path(),
                    prefix: Some("D:/work/pizzeria".to_string())
                }
            },
            "the more specific prefix must win over the enclosing one"
        );
        assert_eq!(
            classify_tenant_map(&map_path(), "D:/work/other", Some(json.as_bytes())),
            WorkspaceDeclaration::Declared {
                tenant: outer,
                source: TenantDeclarationSource::TenantMap {
                    path: map_path(),
                    prefix: Some("D:/work".to_string())
                }
            }
        );
    }

    #[test]
    fn tenant_map_matches_only_on_a_segment_boundary() {
        let t = tenant(0xE5);
        let json = map_json(&format!("{{\"path\":\"D:/work\",\"tenant\":\"{t}\"}}"));
        assert_eq!(
            classify_tenant_map(&map_path(), "D:/work-other", Some(json.as_bytes())),
            WorkspaceDeclaration::Absent,
            "`D:/work` must not cover the sibling directory `D:/work-other`"
        );
    }

    #[test]
    fn tenant_map_normalizes_separators_and_trailing_slashes() {
        let t = tenant(0xF6);
        let json = map_json(&format!(
            "{{\"path\":\"D:\\\\work\\\\\",\"tenant\":\"{t}\"}}"
        ));
        match classify_tenant_map(&map_path(), "D:\\work\\repo", Some(json.as_bytes())) {
            WorkspaceDeclaration::Declared { tenant: got, .. } => assert_eq!(got, t),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn an_absent_tenant_map_declares_nothing_and_an_unparseable_one_refuses() {
        assert_eq!(
            classify_tenant_map(&map_path(), "D:/work", None),
            WorkspaceDeclaration::Absent
        );
        match classify_tenant_map(&map_path(), "D:/work", Some(b"{not json")) {
            WorkspaceDeclaration::Unusable { source, .. } => assert_eq!(
                source,
                TenantDeclarationSource::TenantMap {
                    path: map_path(),
                    prefix: None
                },
                "a present-but-unparseable map names the FILE, not an entry"
            ),
            other => panic!("a present-but-unparseable tenant map must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_map_entry_whose_tenant_is_not_a_uuid_is_unusable() {
        let json = map_json("{\"path\":\"D:/work\",\"tenant\":\"pizzeria\"}");
        match classify_tenant_map(&map_path(), "D:/work", Some(json.as_bytes())) {
            WorkspaceDeclaration::Unusable { source, .. } => assert_eq!(
                source,
                TenantDeclarationSource::TenantMap {
                    path: map_path(),
                    prefix: Some("D:/work".to_string())
                }
            ),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn a_map_with_no_matching_prefix_declares_nothing() {
        let t = tenant(0x17);
        let json = map_json(&format!("{{\"path\":\"D:/elsewhere\",\"tenant\":\"{t}\"}}"));
        assert_eq!(
            classify_tenant_map(&map_path(), "D:/work", Some(json.as_bytes())),
            WorkspaceDeclaration::Absent
        );
    }

    #[test]
    fn an_empty_prefix_never_matches_every_workspace_on_the_box() {
        assert!(!prefix_matches("", "D:/work"));
        assert!(!prefix_matches(&normalize_path("/"), "D:/work"));
    }
}
