//! The binding-store doctor check — the verdict over the `paired_user.json`
//! copies this process would itself compute, serialized into
//! `/coord-mcp/doctor`.
//!
//! Split out of `pair.rs` along an existing responsibility boundary (served
//! policy `modular-code-and-file-size`): convergence — the only writer — stays
//! in `pair.rs`; this module is read-only.
//!
//! ## The property, and why this is not a pairwise comparison
//!
//! The property this check names is *"which file a process reads decides which
//! tenants it believes exist"* — so THIS process's binding store must hold
//! every binding it should. It used to be tested by asking whether every
//! computed copy AGREED with every other, which was a valid proxy only while a
//! non-canonical copy was a stray that converge would retire.
//!
//! Under a `$QONTINUI_SECURE_STORAGE_DIR` override the one other computed copy
//! is the bare `data_local_dir()` default, and since qontinui-runner#1756
//! converge deliberately RETAINS it (`SupersedeVerdict::RetainBareDefault`): it
//! is another installation's canonical store, not a stray copy of ours. The
//! merge from it is one-way (bare default → canonical) and deliberately
//! incomplete — a tenant this process holds no credential for is withheld, a
//! tenant paired here never flows back, and `default_tenant_id` is the
//! canonical's own choice. So two copies differing is the permanent, intended
//! state there, and a pairwise check failed every correctly configured
//! instance runner forever while telling the operator to do something that
//! could not clear it. Plan
//! `2026-10-02-binding-store-doctor-fails-forever-on-the-store-an-override-runner-must-retain`.
//!
//! The check now fails only on a genuine inconsistency of THIS process's store
//! — a tenant converge WOULD have merged (this process holds its credential)
//! that the canonical lacks — and reports the expected one-way-merge residue
//! by name.

use super::{
    binding_store_candidate_paths, holds_credential_predicate, paired_user_path_with,
    read_paired_user_file_at, PairedBinding,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One computed copy, described. Serialized into `/coord-mcp/doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStoreCopyView {
    pub path: String,
    /// Is this the file [`paired_user_path`](super::paired_user_path) resolves
    /// to? Equivalent to `role == "canonical"`; kept because it was already
    /// serialized.
    pub canonical: bool,
    /// `canonical` | `foreign_canonical` | `stray`.
    ///
    /// - `canonical` — `paths[0]`, this process's own store.
    /// - `foreign_canonical` — the bare `data_local_dir()` default while this
    ///   process runs under an override: another installation's live store,
    ///   which converge absorbs from one-way and never renames.
    /// - `stray` — any other non-canonical path. Unreachable in production
    ///   (the candidate set is at most canonical + bare default); evaluated by
    ///   the same rules as `foreign_canonical` — converge absorbs it the same
    ///   one-way — and labelled apart only so the report never calls a
    ///   non-default path another installation's store.
    pub role: &'static str,
    /// `present` | `absent` | `unreadable`.
    pub read: &'static str,
    /// The migrated binding set, sorted. `None` is UNKNOWN (unreadable) —
    /// never an empty list, which would read as "bound to nothing".
    pub tenants: Option<Vec<String>>,
    pub default_tenant_id: Option<String>,
    /// `Some(true)` when this copy is in the pre-v2 single-tenant shape.
    pub legacy_shape: Option<bool>,
    /// `tenant_id -> paired_at`, for the REPORT-only difference arm.
    pub paired_at: Option<BTreeMap<String, String>>,
}

/// The binding-store verdict over the computed copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStoreCheck {
    /// `ok` | `report` | `fail` | `unknown`. Evaluated in this order, first
    /// match wins:
    ///
    /// 1. `unknown` — the canonical copy exists and could not be read.
    /// 2. `unknown` — a non-canonical copy exists and could not be read: merge
    ///    COMPLETENESS is unknown. An unreadable input must neither fire the
    ///    detector nor read as agreement (same discipline as
    ///    `tenant_slots_unknown` and `auth::BindingTenantRead::Unknown`).
    /// 3. `fail` — the canonical is ABSENT while a non-canonical copy carries a
    ///    tenant this process holds a credential for: a credential with no
    ///    binding store. Converge never synthesizes a canonical, so the remedy
    ///    is the heal / a re-pair, not a restart.
    /// 4. `report` — the canonical is absent otherwise: no binding store, this
    ///    process is unpaired. Honest about an absent store; not a fault.
    /// 5. `ok` — no non-canonical copy is live (no override, or the bare
    ///    default is absent): nothing to compare.
    /// 6. `fail` — a MERGE GAP: a tenant in a non-canonical copy, absent from
    ///    the canonical, for which this process's credential predicate answers
    ///    `Some(true)`. Converge would have merged it; its absence is a real
    ///    inconsistency of THIS store. The next start of this runner converges
    ///    it.
    /// 7. `report` — the expected one-way-merge residue and the cosmetic
    ///    differences: tenants withheld for lack of a credential
    ///    ([`Self::withheld_no_credential`]), tenants whose credential read is
    ///    UNKNOWN ([`Self::credential_unknown`] — named, never a fail), tenants
    ///    the canonical carries that the other copy lacks, a differing
    ///    `default_tenant_id`, a shared tenant differing in `paired_at` or
    ///    `user_id`, or a canonical still in the legacy shape.
    /// 8. `ok` — otherwise.
    ///
    /// A merge-gap fail and report-arm residue can co-exist; the verdict is
    /// the strongest, and `detail` names every arm that applied.
    pub verdict: &'static str,
    pub detail: String,
    pub copies: Vec<BindingStoreCopyView>,
    /// Tenants carried by a non-canonical copy, absent from the canonical,
    /// for which this process holds NO credential (or whose id is malformed —
    /// converge withholds those too). Expected one-way-merge residue. Sorted.
    pub withheld_no_credential: Vec<String>,
    /// Tenants carried by a non-canonical copy, absent from the canonical,
    /// whose credential read is UNKNOWN (`None`). Converge withholds them
    /// fail-closed; the doctor names them rather than failing or dropping
    /// them. Sorted.
    pub credential_unknown: Vec<String>,
}

impl BindingStoreCheck {
    /// Does this verdict FAIL the doctor?
    pub fn failed(&self) -> bool {
        self.verdict == "fail"
    }
    /// Is the comparison UNKNOWN? A caller must not read this as agreement.
    pub fn is_unknown(&self) -> bool {
        self.verdict == "unknown"
    }
}

/// Inspect the `paired_user.json` copies this process would itself compute.
/// Read-only — the doctor never writes.
///
/// The credential predicate is [`holds_credential_predicate`] — the SAME one
/// `converge_binding_store()` merges under — so the doctor and the merge
/// cannot disagree about "credentialed". See that fn for what "credentialed"
/// does and does not establish (the legacy slot's keychain fallback is shared
/// across instances).
pub fn binding_store_check() -> BindingStoreCheck {
    let paths = binding_store_candidate_paths();
    // Passed explicitly for the same reason `converge_binding_store_with`
    // takes it: resolved ambiently inside the core it would point at the
    // developer's real home in every temp-home test.
    let bare_default = paired_user_path_with(None);
    let mgr = crate::auth::AuthManager::new();
    let holds_credential = holds_credential_predicate(&mgr);
    inspect_binding_store_paths(&paths, bare_default.as_deref(), &holds_credential)
}

/// Path-parameterized core of [`binding_store_check`]. `paths[0]` is the
/// canonical one; `bare_default` is the `data_local_dir()` path (`None` in
/// hermetic tests), used only to LABEL a non-canonical copy
/// `foreign_canonical` rather than `stray` — both are judged by one rule set.
///
/// `holds_credential(tenant, is_default)` answers exactly as it does for
/// `converge_binding_store_with`, with `is_default` computed the same way: the
/// canonical's effective default tenant. With the canonical ABSENT there is no
/// default (converge never reaches its predicate then), so `is_default` is
/// `false` for every tenant — deliberately: the legacy slot can be answered by
/// another instance's keychain token, and letting it vouch for an absent
/// store's default would fail every unpaired instance runner on a box whose
/// primary is paired.
pub(crate) fn inspect_binding_store_paths(
    paths: &[PathBuf],
    bare_default: Option<&Path>,
    holds_credential: &dyn Fn(&uuid::Uuid, bool) -> Option<bool>,
) -> BindingStoreCheck {
    let described: Vec<(BindingStoreCopyView, Option<Vec<PairedBinding>>)> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| describe_binding_store_copy(p, copy_role(i, p, bare_default)))
        .collect();
    let copies: Vec<BindingStoreCopyView> = described.iter().map(|(v, _)| v.clone()).collect();
    let check = |verdict: &'static str, detail: String, sets: TenantSets| BindingStoreCheck {
        verdict,
        detail,
        copies: copies.clone(),
        withheld_no_credential: sets.withheld.into_iter().collect(),
        credential_unknown: sets.unknown.into_iter().collect(),
    };

    let Some(((canonical, canonical_bindings), others)) = described.split_first() else {
        return check(
            "unknown",
            "no paired_user.json path could be resolved for this process — the binding \
             store's state is UNKNOWN, not established."
                .to_string(),
            TenantSets::default(),
        );
    };

    // 1. Canonical unreadable.
    if canonical.read == "unreadable" {
        return check(
            "unknown",
            format!(
                "the canonical binding store {} exists and could not be read — which tenants \
                 this process believes exist is UNKNOWN, not established.",
                canonical.path
            ),
            TenantSets::default(),
        );
    }

    // 2. A non-canonical copy unreadable.
    let unreadable: Vec<&str> = others
        .iter()
        .filter(|(c, _)| c.read == "unreadable")
        .map(|(c, _)| c.path.as_str())
        .collect();
    if !unreadable.is_empty() {
        return check(
            "unknown",
            format!(
                "non-canonical paired_user.json cop(ies) {unreadable:?} exist and could not be \
                 read — merge COMPLETENESS is UNKNOWN, not established. An unreadable copy is \
                 never evidence that nothing is missing from the canonical."
            ),
            TenantSets::default(),
        );
    }

    let live: Vec<&(BindingStoreCopyView, Option<Vec<PairedBinding>>)> =
        others.iter().filter(|(c, _)| c.read == "present").collect();
    let canonical_tenants: BTreeSet<String> = canonical.tenants.iter().flatten().cloned().collect();
    let default_tenant = canonical.default_tenant_id.as_deref();
    let sets =
        classify_missing_tenants(&live, &canonical_tenants, default_tenant, holds_credential);

    // 3 + 4. Canonical absent.
    if canonical.read == "absent" {
        if !sets.gap.is_empty() {
            let detail = format!(
                "the canonical binding store {} is ABSENT, yet this process holds a credential \
                 for tenant(s) {:?} carried by another copy — a credential with no binding \
                 store, so this process reports itself unpaired while it is not. Converge \
                 never synthesizes a canonical, so a restart will not clear this: run the \
                 binding-store heal, or re-pair this runner.{}",
                canonical.path,
                sets.gap,
                residue_suffix(&sets)
            );
            return check("fail", detail, sets);
        }
        let detail = format!(
            "no binding store: the canonical {} is absent, so this process is unpaired \
             (reported, not failed — pairing creates this file).{}",
            canonical.path,
            residue_suffix(&sets)
        );
        return check("report", detail, sets);
    }

    // 5. Nothing to compare.
    if live.is_empty() {
        return check(
            "ok",
            "one live paired_user.json under a path this process computes — nothing to \
             compare."
                .to_string(),
            sets,
        );
    }

    // 6 + 7. Every arm that applies is named; the verdict is the strongest.
    let mut arms: Vec<String> = Vec::new();
    if !sets.gap.is_empty() {
        arms.push(format!(
            "MERGE GAP: tenant(s) {:?} are carried by another copy and this process holds a \
             credential for them, but the canonical {} lacks them — converge would have merged \
             them. The next start of this runner converges it; do not restart a live runner to \
             force it.",
            sets.gap, canonical.path
        ));
    }
    if !sets.withheld.is_empty() {
        arms.push(format!(
            "withheld_no_credential: tenant(s) {:?} are carried by another copy and this \
             process holds no credential for them — expected one-way-merge residue, never \
             merged.",
            sets.withheld
        ));
    }
    if !sets.unknown.is_empty() {
        arms.push(format!(
            "credential_unknown: tenant(s) {:?} are carried by another copy and this process's \
             credential read for them is UNKNOWN — converge withholds them fail-closed; named, \
             not failed.",
            sets.unknown
        ));
    }
    let mut canonical_only: BTreeSet<String> = BTreeSet::new();
    let mut default_differs: Vec<&str> = Vec::new();
    let mut shared_differs: BTreeSet<String> = BTreeSet::new();
    for (other, other_bindings) in &live {
        let other_tenants: BTreeSet<String> = other.tenants.iter().flatten().cloned().collect();
        canonical_only.extend(canonical_tenants.difference(&other_tenants).cloned());
        if other.default_tenant_id != canonical.default_tenant_id {
            default_differs.push(other.path.as_str());
        }
        for b in other_bindings.iter().flatten() {
            let key = b.tenant_id.trim();
            if let Some(c) = canonical_bindings
                .iter()
                .flatten()
                .find(|c| c.tenant_id.trim() == key)
            {
                if c.paired_at != b.paired_at || c.user_id != b.user_id {
                    shared_differs.insert(key.to_string());
                }
            }
        }
    }
    if !canonical_only.is_empty() {
        arms.push(format!(
            "tenant(s) {canonical_only:?} are bound in the canonical but not in the other \
             copy — paired here; the merge is one-way, so this is expected."
        ));
    }
    if !default_differs.is_empty() {
        arms.push(format!(
            "default_tenant_id differs from {default_differs:?} — the canonical's default is \
             its own choice and is never taken from another copy."
        ));
    }
    if !shared_differs.is_empty() {
        arms.push(format!(
            "shared tenant(s) {shared_differs:?} differ in paired_at or user_id — cosmetic; the \
             merge gap is by tenant id only, and converge adopts the other copy's record on \
             the next start only where it is newer."
        ));
    }
    if canonical.legacy_shape == Some(true) {
        arms.push(
            "the canonical is still in the legacy single-tenant shape — converge migrates it \
             to v2 on the next start."
                .to_string(),
        );
    }

    let verdict = if !sets.gap.is_empty() {
        "fail"
    } else if arms.is_empty() {
        "ok"
    } else {
        "report"
    };
    let detail = if arms.is_empty() {
        format!(
            "{} live non-canonical paired_user.json cop(ies) — the canonical {} holds every \
             binding this process should, and nothing differs.",
            live.len(),
            canonical.path
        )
    } else {
        arms.join(" ")
    };
    check(verdict, detail, sets)
}

/// The tenants carried by a non-canonical copy and absent from the canonical,
/// split by what this process's credential predicate answers for them —
/// exactly the three outcomes converge's fold gives the same tenant.
#[derive(Debug, Default)]
struct TenantSets {
    /// `Some(true)` — converge would merge it.
    gap: BTreeSet<String>,
    /// `Some(false)`, or a malformed id.
    withheld: BTreeSet<String>,
    /// `None`.
    unknown: BTreeSet<String>,
}

fn classify_missing_tenants(
    live: &[&(BindingStoreCopyView, Option<Vec<PairedBinding>>)],
    canonical_tenants: &BTreeSet<String>,
    default_tenant: Option<&str>,
    holds_credential: &dyn Fn(&uuid::Uuid, bool) -> Option<bool>,
) -> TenantSets {
    let mut sets = TenantSets::default();
    let missing: BTreeSet<String> = live
        .iter()
        .flat_map(|(c, _)| c.tenants.iter().flatten().cloned())
        .filter(|t| !canonical_tenants.contains(t))
        .collect();
    for key in missing {
        // Same fail-closed reading as converge: a malformed id is withheld.
        let Ok(t) = uuid::Uuid::parse_str(&key) else {
            sets.withheld.insert(key);
            continue;
        };
        let is_default = default_tenant.map(str::trim) == Some(key.as_str());
        match holds_credential(&t, is_default) {
            Some(true) => sets.gap.insert(key),
            Some(false) => sets.withheld.insert(key),
            None => sets.unknown.insert(key),
        };
    }
    sets
}

/// The withheld / unknown residue, appended to an absent-canonical detail so
/// every arm that applied is named.
fn residue_suffix(sets: &TenantSets) -> String {
    let mut s = String::new();
    if !sets.withheld.is_empty() {
        s.push_str(&format!(
            " withheld_no_credential (another copy carries them; no credential here): {:?}.",
            sets.withheld
        ));
    }
    if !sets.unknown.is_empty() {
        s.push_str(&format!(
            " credential_unknown (credential read UNKNOWN): {:?}.",
            sets.unknown
        ));
    }
    s
}

/// `paths[0]` is `canonical`; a non-canonical path equal to `bare_default` is
/// `foreign_canonical`; anything else is `stray`.
fn copy_role(index: usize, path: &Path, bare_default: Option<&Path>) -> &'static str {
    if index == 0 {
        "canonical"
    } else if bare_default == Some(path) {
        "foreign_canonical"
    } else {
        "stray"
    }
}

/// Describe one copy. The raw bindings ride beside the view (not in it) so
/// the shared-tenant `user_id` comparison needs no second read and adds no
/// field to the serialized report.
fn describe_binding_store_copy(
    path: &Path,
    role: &'static str,
) -> (BindingStoreCopyView, Option<Vec<PairedBinding>>) {
    let mut view = BindingStoreCopyView {
        path: path.display().to_string(),
        canonical: role == "canonical",
        role,
        read: "absent",
        tenants: None,
        default_tenant_id: None,
        legacy_shape: None,
        paired_at: None,
    };
    if !path.exists() {
        return (view, None);
    }
    let Some(pf) = read_paired_user_file_at(path) else {
        // Present and unreadable. `tenants: None` stays UNKNOWN.
        view.read = "unreadable";
        return (view, None);
    };
    view.read = "present";
    let bindings = pf.effective_bindings();
    let mut tenants: Vec<String> = bindings
        .iter()
        .map(|b| b.tenant_id.trim().to_string())
        .collect();
    tenants.sort();
    tenants.dedup();
    view.tenants = Some(tenants);
    view.default_tenant_id = pf
        .effective_default_tenant_id()
        .map(|d| d.trim().to_string());
    view.legacy_shape = Some(!pf.is_v2());
    view.paired_at = Some(
        bindings
            .iter()
            .filter_map(|b| {
                b.paired_at
                    .as_ref()
                    .map(|p| (b.tenant_id.trim().to_string(), p.clone()))
            })
            .collect(),
    );
    (view, Some(bindings))
}

// ============================================================================
// Tests — hermetic: temp dirs, an injected credential predicate, and an
// explicit `bare_default` (never the developer's real home).
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pair::converge_binding_store_with;
    use crate::pair::one_binding_store_tests::{
        credentialed, legacy, v2, write, DEFAULT_TENANT, SECOND_TENANT, UNCREDENTIALED_TENANT, USER,
    };

    const PAIRED_AT: &str = "2026-09-17T15:42:00Z";

    /// A v2 store holding `tenants` (all paired by [`USER`] at
    /// [`PAIRED_AT`]), with `default` as its `default_tenant_id`.
    fn store(default: &str, tenants: &[&str]) -> String {
        let bindings: Vec<String> = tenants
            .iter()
            .map(|t| {
                format!(
                    r#"{{"tenant_id": "{t}", "user_id": "{USER}", "paired_at": "{PAIRED_AT}"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"user_id": "{USER}", "tenant_id": "{default}", "bindings": [{}], "default_tenant_id": "{default}"}}"#,
            bindings.join(", ")
        )
    }

    /// The instance-runner layout: `canonical` is the override store,
    /// `foreign` is the bare default (another installation's live store).
    struct TwoCopies {
        _tmp: tempfile::TempDir,
        canonical: PathBuf,
        foreign: PathBuf,
    }

    fn two_copies() -> TwoCopies {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp
            .path()
            .join("instances/test-1a0ce93cab6-2/paired_user.json");
        let foreign = tmp.path().join("primary/paired_user.json");
        TwoCopies {
            _tmp: tmp,
            canonical,
            foreign,
        }
    }

    impl TwoCopies {
        fn inspect(
            &self,
            holds_credential: &dyn Fn(&uuid::Uuid, bool) -> Option<bool>,
        ) -> BindingStoreCheck {
            inspect_binding_store_paths(
                &[self.canonical.clone(), self.foreign.clone()],
                Some(self.foreign.as_path()),
                holds_credential,
            )
        }
    }

    /// Answers `answer` for [`SECOND_TENANT`] and `Some(true)` for every other
    /// tenant.
    fn second_answers(answer: Option<bool>) -> impl Fn(&uuid::Uuid, bool) -> Option<bool> {
        move |t: &uuid::Uuid, _is_default: bool| {
            if t.to_string() == SECOND_TENANT {
                answer
            } else {
                Some(true)
            }
        }
    }

    fn never_called(_t: &uuid::Uuid, _d: bool) -> Option<bool> {
        panic!("the credential predicate must not be consulted here")
    }

    // ------------------------------------------------------------------
    // The one-way merge residue: report, never fail
    // ------------------------------------------------------------------

    /// THE CASE THIS MODULE WAS REWRITTEN FOR. An instance runner whose bare
    /// default carries a tenant it holds no credential for: converge withholds
    /// that tenant by design, so the canonical correctly lacks it. That is
    /// expected residue, not a split brain.
    #[test]
    fn two_copies_with_a_withheld_tenant_report_and_never_fail() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&second_answers(Some(false)));
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(!check.failed(), "withheld residue must not fail the doctor");
        assert_eq!(
            check.withheld_no_credential,
            vec![SECOND_TENANT.to_string()]
        );
        assert!(check.credential_unknown.is_empty());
        assert!(check.detail.contains("withheld_no_credential"));
        assert_eq!(check.copies[1].role, "foreign_canonical");
    }

    /// A tenant converge WOULD have merged (this process holds its
    /// credential) that the canonical lacks is a real inconsistency of THIS
    /// store — and the remedy named is the true one.
    #[test]
    fn a_credentialed_tenant_missing_from_the_canonical_fails() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&second_answers(Some(true)));
        assert_eq!(check.verdict, "fail", "{}", check.detail);
        assert!(check.failed());
        assert!(
            check.detail.contains(SECOND_TENANT),
            "the detail must name the gap: {}",
            check.detail
        );
        assert!(
            check
                .detail
                .contains("The next start of this runner converges it"),
            "the remedy must be the true one: {}",
            check.detail
        );
        assert!(
            check.detail.contains("do not restart a live runner"),
            "{}",
            check.detail
        );
        assert!(
            !check.detail.contains(".superseded-"),
            "the retired remedy promised a rename converge never performs here: {}",
            check.detail
        );
        assert!(check.withheld_no_credential.is_empty());
    }

    /// An UNKNOWN credential read must not fire the detector, and must not be
    /// dropped either: it is named.
    #[test]
    fn an_unknown_credential_read_is_named_and_never_fails() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&second_answers(None));
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(!check.failed());
        assert_eq!(check.credential_unknown, vec![SECOND_TENANT.to_string()]);
        assert!(check.withheld_no_credential.is_empty());
        assert!(check.detail.contains("credential_unknown"));
    }

    /// A tenant this process paired on its own never flows back to the bare
    /// default. Expected; reported.
    #[test]
    fn a_tenant_only_the_canonical_carries_is_reported() {
        let b = two_copies();
        write(
            &b.canonical,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );
        write(&b.foreign, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(check.detail.contains(SECOND_TENANT), "{}", check.detail);
        assert!(check.withheld_no_credential.is_empty());
        assert!(check.credential_unknown.is_empty());
    }

    /// `default_tenant_id` is the canonical's own choice and is never taken
    /// from the other copy.
    #[test]
    fn a_differing_default_tenant_over_the_same_set_is_reported() {
        let b = two_copies();
        write(
            &b.canonical,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );
        write(
            &b.foreign,
            &store(SECOND_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(
            check.detail.contains("default_tenant_id"),
            "{}",
            check.detail
        );
    }

    /// Only REPORTS a `paired_at`-only difference — cosmetic. Identical
    /// copies are plain `ok`.
    #[test]
    fn a_paired_at_only_difference_is_reported_and_identical_copies_are_ok() {
        let b = two_copies();
        write(
            &b.canonical,
            &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"),
        );
        write(
            &b.foreign,
            &v2("2026-07-21T09:00:00Z", "2026-07-21T09:00:00Z"),
        );

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(!check.failed());
        assert!(check.detail.contains("paired_at"));

        write(
            &b.foreign,
            &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"),
        );
        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "ok", "{}", check.detail);
    }

    /// A shared tenant recorded under a different `user_id` is reported, not
    /// failed: the merge gap is by tenant id only.
    #[test]
    fn a_shared_tenant_differing_in_user_id_is_reported() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT])
                .replace(USER, "22222222-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        );

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(check.detail.contains("user_id"), "{}", check.detail);
    }

    /// A legacy-shaped foreign copy cannot see the second binding at all. The
    /// old pairwise check FAILED on this; it is a copy that knows less than
    /// the canonical, which is not an inconsistency of this store.
    #[test]
    fn a_legacy_shaped_foreign_copy_is_reported_not_failed() {
        let b = two_copies();
        write(
            &b.canonical,
            &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"),
        );
        write(&b.foreign, &legacy());

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(!check.failed());
        assert_eq!(
            check.copies[1].legacy_shape,
            Some(true),
            "the legacy shape must be visible in the report"
        );
    }

    /// A canonical still in the legacy shape is reported: converge migrates it.
    #[test]
    fn a_legacy_shaped_canonical_is_reported() {
        let b = two_copies();
        write(&b.canonical, &legacy());
        write(&b.foreign, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(check.detail.contains("legacy"), "{}", check.detail);
    }

    /// One live copy (the shape of a box with no override, or an override box
    /// whose bare default is absent) is `ok`.
    #[test]
    fn a_single_live_copy_is_ok() {
        let b = two_copies();
        write(
            &b.canonical,
            &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"),
        );

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "ok", "{}", check.detail);
        assert_eq!(check.copies[1].read, "absent");
        assert_eq!(
            check.copies[1].tenants, None,
            "an ABSENT copy carries no binding list"
        );

        let only =
            inspect_binding_store_paths(std::slice::from_ref(&b.canonical), None, &never_called);
        assert_eq!(only.verdict, "ok", "{}", only.detail);
    }

    // ------------------------------------------------------------------
    // An absent canonical
    // ------------------------------------------------------------------

    /// A credential with no binding store is the one absent-canonical case
    /// that is a real fault. Converge never synthesizes a canonical, so the
    /// remedy is the heal / a re-pair — not a restart. With nothing
    /// credentialed it is only an unpaired process: reported.
    #[test]
    fn an_absent_canonical_fails_only_when_a_foreign_tenant_is_credentialed() {
        let b = two_copies();
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&second_answers(Some(true)));
        assert_eq!(check.verdict, "fail", "{}", check.detail);
        assert!(check.detail.contains("re-pair"), "{}", check.detail);
        assert!(check.detail.contains("heal"), "{}", check.detail);
        assert!(
            !check.detail.contains("next start"),
            "converge cannot clear this, so it must not be named: {}",
            check.detail
        );
        assert_eq!(check.copies[0].read, "absent");

        let check = b.inspect(&|_t: &uuid::Uuid, _d: bool| Some(false));
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(check.detail.contains("unpaired"), "{}", check.detail);
        assert_eq!(
            check.withheld_no_credential,
            vec![SECOND_TENANT.to_string(), DEFAULT_TENANT.to_string()],
            "sorted (`7ac1…` before `c231…`), and named even here"
        );
    }

    /// With the canonical absent there is no default tenant, so the predicate
    /// is never asked with `is_default = true` — the legacy slot (whose
    /// keychain fallback is shared across instances) must not vouch for an
    /// absent store.
    #[test]
    fn an_absent_canonical_never_asks_the_predicate_as_the_default() {
        let b = two_copies();
        write(&b.foreign, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));

        let check = b.inspect(&|_t: &uuid::Uuid, is_default: bool| Some(is_default));
        assert_eq!(check.verdict, "report", "{}", check.detail);
    }

    /// No store anywhere: reported honestly as unpaired, not `ok`.
    #[test]
    fn an_absent_canonical_with_no_foreign_copy_is_reported() {
        let b = two_copies();
        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert!(
            check.detail.contains("no binding store"),
            "{}",
            check.detail
        );
        assert!(!check.failed());
    }

    // ------------------------------------------------------------------
    // Roles
    // ------------------------------------------------------------------

    /// A non-canonical path that is not the bare default is a `stray`, never
    /// another installation's store.
    #[test]
    fn a_path_that_is_not_the_bare_default_is_labelled_stray() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(&b.foreign, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));

        let check = inspect_binding_store_paths(
            &[b.canonical.clone(), b.foreign.clone()],
            None,
            &never_called,
        );
        assert_eq!(check.copies[0].role, "canonical");
        assert!(check.copies[0].canonical);
        assert_eq!(check.copies[1].role, "stray");
        assert!(!check.copies[1].canonical);
        assert_eq!(check.verdict, "ok", "{}", check.detail);
    }

    /// `paths[1]` is `foreign_canonical` exactly when it equals `bare_default`.
    #[test]
    fn the_second_path_is_foreign_canonical_only_when_it_is_the_bare_default() {
        let b = two_copies();
        let paths = [b.canonical.clone(), b.foreign.clone()];
        let elsewhere = b.foreign.with_file_name("elsewhere.json");

        let role_with = |bare: Option<&Path>| {
            inspect_binding_store_paths(&paths, bare, &never_called).copies[1].role
        };
        assert_eq!(role_with(Some(b.foreign.as_path())), "foreign_canonical");
        assert_eq!(role_with(Some(elsewhere.as_path())), "stray");
        assert_eq!(role_with(None), "stray");
        // The canonical keeps its role even if it is also the bare default.
        let check = inspect_binding_store_paths(&paths, Some(b.canonical.as_path()), &never_called);
        assert_eq!(check.copies[0].role, "canonical");
        assert_eq!(check.copies[1].role, "stray");
    }

    // ------------------------------------------------------------------
    // UNKNOWN discipline
    // ------------------------------------------------------------------

    /// An unreadable foreign copy makes merge completeness UNKNOWN: neither a
    /// fail nor agreement, and never an empty binding list.
    #[test]
    fn an_unreadable_foreign_copy_is_unknown() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(&b.foreign, "\u{0}\u{1}not-json-at-all");

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "unknown", "{}", check.detail);
        assert!(check.is_unknown());
        assert!(!check.failed(), "unknown is not a fail either");
        assert!(check.detail.contains("COMPLETENESS"), "{}", check.detail);
        assert_eq!(check.copies[1].read, "unreadable");
        assert_eq!(
            check.copies[1].tenants, None,
            "an unreadable copy must NOT report an empty binding list — absence is not zero"
        );
    }

    /// An unreadable canonical is UNKNOWN — it wins over everything below it.
    #[test]
    fn an_unreadable_canonical_is_unknown() {
        let b = two_copies();
        write(&b.canonical, "\u{0}\u{1}not-json-at-all");
        write(
            &b.foreign,
            &store(DEFAULT_TENANT, &[DEFAULT_TENANT, SECOND_TENANT]),
        );

        let check = b.inspect(&never_called);
        assert_eq!(check.verdict, "unknown", "{}", check.detail);
        assert_eq!(check.copies[0].read, "unreadable");
        assert_eq!(check.copies[0].tenants, None);
    }

    /// No resolvable path at all is UNKNOWN, not `ok`.
    #[test]
    fn no_resolvable_path_is_unknown() {
        let check = inspect_binding_store_paths(&[], None, &never_called);
        assert_eq!(check.verdict, "unknown", "{}", check.detail);
        assert!(check.copies.is_empty());
    }

    // ------------------------------------------------------------------
    // End to end — the regression #1756 introduced
    // ------------------------------------------------------------------

    /// Converge an instance runner's two-copy home with the PRODUCTION
    /// predicate shape (`credentialed`), then inspect the result with the
    /// same predicate. Converge merges the credentialed tenant, withholds the
    /// uncredentialed one and retains the bare default — and the doctor must
    /// then read that steady state as `report`, not `fail`. Under the
    /// pairwise check this failed forever; it is the test that would have
    /// caught the regression.
    #[test]
    fn converge_then_inspect_an_override_home_with_a_withheld_tenant_is_not_a_fail() {
        let b = two_copies();
        write(&b.canonical, &store(DEFAULT_TENANT, &[DEFAULT_TENANT]));
        write(
            &b.foreign,
            &store(
                DEFAULT_TENANT,
                &[DEFAULT_TENANT, SECOND_TENANT, UNCREDENTIALED_TENANT],
            ),
        );

        let report = converge_binding_store_with(
            &b.canonical,
            std::slice::from_ref(&b.foreign),
            Some(b.foreign.as_path()),
            &credentialed,
            "2026-10-02",
        );
        assert_eq!(report.merged, vec![SECOND_TENANT.to_string()]);
        assert_eq!(
            report.withheld_no_credential,
            vec![UNCREDENTIALED_TENANT.to_string()]
        );
        assert_eq!(report.retained, vec![b.foreign.clone()]);
        assert!(b.foreign.exists(), "the bare default is never renamed");

        let check = b.inspect(&credentialed);
        assert_ne!(check.verdict, "fail", "{}", check.detail);
        assert_eq!(check.verdict, "report", "{}", check.detail);
        assert_eq!(
            check.withheld_no_credential,
            vec![UNCREDENTIALED_TENANT.to_string()],
            "the doctor and the merge must agree on what was withheld"
        );
        assert!(check.credential_unknown.is_empty());
    }
}
