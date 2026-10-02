//! The binding-store doctor check — the split-brain verdict over the
//! `paired_user.json` copies this process would itself compute, serialized
//! into `/coord-mcp/doctor`.
//!
//! Split out of `pair.rs` along an existing responsibility boundary (served
//! policy `modular-code-and-file-size`): convergence — the only writer — stays
//! in `pair.rs`; this module is read-only.

use super::{binding_store_candidate_paths, read_paired_user_file_at};
use serde::Serialize;
use std::path::PathBuf;

/// One computed copy, described. Serialized into `/coord-mcp/doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStoreCopyView {
    pub path: String,
    /// Is this the file [`paired_user_path`](super::paired_user_path) resolves to?
    pub canonical: bool,
    /// `present` | `absent` | `unreadable`.
    pub read: &'static str,
    /// The migrated binding set, sorted. `None` is UNKNOWN (unreadable) —
    /// never an empty list, which would read as "bound to nothing".
    pub tenants: Option<Vec<String>>,
    pub default_tenant_id: Option<String>,
    /// `Some(true)` when this copy is in the pre-v2 single-tenant shape.
    pub legacy_shape: Option<bool>,
    /// `tenant_id -> paired_at`, for the REPORT-only difference arm.
    pub paired_at: Option<std::collections::BTreeMap<String, String>>,
}

/// The split-brain verdict over the computed copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingStoreCheck {
    /// `ok` | `report` | `fail` | `unknown`.
    ///
    /// - `fail` — two live copies disagree on the BINDING SET or on
    ///   `default_tenant_id`. That is a real split brain: which file a
    ///   process reads decides which tenants it believes exist.
    /// - `report` — they agree on both, and differ only on `paired_at`.
    ///   That is cosmetic, and it is the ONLY way the copies differ on the
    ///   operator box today, so failing on it would fail a healthy machine
    ///   from day one and get the check disabled. Deciding priority:
    ///   robustness.
    /// - `unknown` — a copy exists and could not be read. UNKNOWN is never
    ///   "no disagreement" (same discipline as `tenant_slots_unknown` and
    ///   `auth::BindingTenantRead::Unknown`).
    /// - `ok` — fewer than two live copies, or they agree outright.
    pub verdict: &'static str,
    pub detail: String,
    pub copies: Vec<BindingStoreCopyView>,
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
pub fn binding_store_check() -> BindingStoreCheck {
    inspect_binding_store_paths(&binding_store_candidate_paths())
}

/// Path-parameterized core of [`binding_store_check`]. `paths[0]` is the
/// canonical one.
pub(crate) fn inspect_binding_store_paths(paths: &[PathBuf]) -> BindingStoreCheck {
    let copies: Vec<BindingStoreCopyView> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| describe_binding_store_copy(p, i == 0))
        .collect();

    let live: Vec<&BindingStoreCopyView> = copies.iter().filter(|c| c.read != "absent").collect();
    let unreadable = live.iter().filter(|c| c.read == "unreadable").count();
    let readable: Vec<&&BindingStoreCopyView> =
        live.iter().filter(|c| c.read == "present").collect();

    // Order matters: a disagreement we CAN see is a fail even when another
    // copy is unreadable, but an unreadable copy must never let "the rest
    // agree" stand in for "no disagreement".
    let sets_differ = readable
        .windows(2)
        .any(|w| w[0].tenants != w[1].tenants || w[0].default_tenant_id != w[1].default_tenant_id);
    let paired_at_differs = readable
        .windows(2)
        .any(|w| w[0].paired_at != w[1].paired_at);

    let (verdict, detail) = if sets_differ {
        (
            "fail",
            format!(
                "{} live paired_user.json copies disagree on the binding set or on \
                 default_tenant_id — which file a process reads decides which tenants it \
                 believes exist. Run the runner once to converge them (the non-canonical \
                 copy is left as .superseded-<date>), or reconcile by hand.",
                readable.len()
            ),
        )
    } else if unreadable > 0 {
        (
            "unknown",
            format!(
                "{unreadable} of {} live paired_user.json copies could not be read — \
                 agreement is UNKNOWN, not established. An unreadable copy is never \
                 evidence of no disagreement.",
                live.len()
            ),
        )
    } else if paired_at_differs {
        (
            "report",
            format!(
                "{} live paired_user.json copies agree on the binding set and on \
                 default_tenant_id, and differ only on paired_at — cosmetic, reported \
                 rather than failed.",
                readable.len()
            ),
        )
    } else if readable.len() < 2 {
        (
            "ok",
            format!(
                "{} live paired_user.json copy/copies under a path this process computes — \
                 nothing to disagree with.",
                readable.len()
            ),
        )
    } else {
        (
            "ok",
            format!("{} live paired_user.json copies agree.", readable.len()),
        )
    };

    BindingStoreCheck {
        verdict,
        detail,
        copies,
    }
}

fn describe_binding_store_copy(path: &std::path::Path, canonical: bool) -> BindingStoreCopyView {
    let mut view = BindingStoreCopyView {
        path: path.display().to_string(),
        canonical,
        read: "absent",
        tenants: None,
        default_tenant_id: None,
        legacy_shape: None,
        paired_at: None,
    };
    if !path.exists() {
        return view;
    }
    let Some(pf) = read_paired_user_file_at(path) else {
        // Present and unreadable. `tenants: None` stays UNKNOWN.
        view.read = "unreadable";
        return view;
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
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pair::one_binding_store_tests::{
        legacy, v2, write, DEFAULT_TENANT, SECOND_TENANT, USER,
    };

    /// FAILS on a binding-set disagreement, and again on a
    /// `default_tenant_id` disagreement — the two ways a split brain changes
    /// which tenants a process believes exist.
    #[test]
    fn doctor_check_fails_on_a_binding_set_or_default_tenant_disagreement() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/paired_user.json");
        let b = tmp.path().join("b/paired_user.json");

        // Binding-set disagreement: the legacy copy cannot see the second
        // binding at all. This is the operator box's %APPDATA% copy.
        write(&a, &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"));
        write(&b, &legacy());
        let check = inspect_binding_store_paths(&[a.clone(), b.clone()]);
        assert_eq!(check.verdict, "fail", "{}", check.detail);
        assert!(check.failed());
        assert_eq!(
            check.copies[1].legacy_shape,
            Some(true),
            "the legacy shape must be visible in the report"
        );

        // default_tenant_id disagreement, same binding set.
        write(
            &b,
            &format!(
                r#"{{"user_id":"{USER}","tenant_id":"{SECOND_TENANT}",
  "bindings":[
    {{"tenant_id":"{DEFAULT_TENANT}","user_id":"{USER}","paired_at":"2026-09-17T15:42:00Z"}},
    {{"tenant_id":"{SECOND_TENANT}","user_id":"{USER}","paired_at":"2026-08-02T10:00:00Z"}}],
  "default_tenant_id":"{SECOND_TENANT}"}}"#
            ),
        );
        let check = inspect_binding_store_paths(&[a, b]);
        assert_eq!(
            check.verdict, "fail",
            "same tenants, different default — still a split brain: {}",
            check.detail
        );
    }

    /// Only REPORTS a `paired_at`-only difference. This is the ONLY way the
    /// copies differ on the operator box today, so a strict check would fail
    /// a healthy machine from day one and get disabled.
    #[test]
    fn doctor_check_only_reports_a_paired_at_only_difference() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/paired_user.json");
        let b = tmp.path().join("b/paired_user.json");
        write(&a, &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"));
        write(&b, &v2("2026-07-21T09:00:00Z", "2026-07-21T09:00:00Z"));

        let check = inspect_binding_store_paths(&[a.clone(), b.clone()]);
        assert_eq!(
            check.verdict, "report",
            "a paired_at-only difference is cosmetic: {}",
            check.detail
        );
        assert!(!check.failed(), "and it must NOT fail the doctor");
        assert!(check.detail.contains("paired_at"));

        // Identical copies: plain ok.
        write(&b, &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"));
        assert_eq!(inspect_binding_store_paths(&[a, b]).verdict, "ok");
    }

    /// One copy (the shape of a box with no `$QONTINUI_SECURE_STORAGE_DIR`)
    /// is `ok`, not a fail.
    #[test]
    fn doctor_check_is_ok_with_a_single_live_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/paired_user.json");
        let missing = tmp.path().join("b/paired_user.json");
        write(&a, &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"));

        let check = inspect_binding_store_paths(&[a, missing]);
        assert_eq!(check.verdict, "ok", "{}", check.detail);
        assert_eq!(check.copies[1].read, "absent");
        assert_eq!(
            check.copies[1].tenants, None,
            "an ABSENT copy carries no binding list"
        );
    }

    /// UNKNOWN discipline: an unreadable copy must never read as "no
    /// disagreement", and a disagreement we CAN see still fails.
    #[test]
    fn doctor_check_reports_unknown_not_agreement_for_an_unreadable_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/paired_user.json");
        let b = tmp.path().join("b/paired_user.json");
        write(&a, &v2("2026-09-17T15:42:00Z", "2026-08-02T10:00:00Z"));
        write(&b, "\u{0}\u{1}not-json-at-all");

        let check = inspect_binding_store_paths(&[a.clone(), b.clone()]);
        assert_eq!(
            check.verdict, "unknown",
            "an undecryptable/corrupt copy is UNKNOWN, never 'the rest agree': {}",
            check.detail
        );
        assert!(check.is_unknown());
        assert!(!check.failed(), "unknown is not a fail either");
        assert_eq!(check.copies[1].read, "unreadable");
        assert_eq!(
            check.copies[1].tenants, None,
            "an unreadable copy must NOT report an empty binding list — absence is not zero"
        );

        // A visible disagreement beats the unknown: still a fail.
        let c = tmp.path().join("c/paired_user.json");
        write(&c, &legacy());
        assert_eq!(
            inspect_binding_store_paths(&[a, c, b]).verdict,
            "fail",
            "a disagreement we can see is not masked by a copy we cannot read"
        );
    }
}
