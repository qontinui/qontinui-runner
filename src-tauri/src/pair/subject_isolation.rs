//! The binding store of a SUBJECT runner — one launched under an instance root
//! (`QONTINUI_INSTANCE_ROOT`, plan
//! `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`
//! D2).
//!
//! A runner carrying a `QONTINUI_SECURE_STORAGE_DIR` override ordinarily lists
//! the bare `data_local_dir()` default as a second binding-store candidate, so
//! [`super::converge_binding_store`] can absorb bindings an earlier,
//! override-less run left there. For a subject that second candidate IS the
//! harness runner's `paired_user.json`: absorbing it would let the subject act
//! as the harness's device, and the doctor would judge it against the
//! harness's store. Under an instance root the candidate set is therefore the
//! canonical path ALONE — the subject pairs (or runs unpaired) as its own
//! identity. Explicit provisioning INTO the root (a launcher copying a pairing
//! snapshot into `<root>/secure`) is unaffected; only the implicit read outside
//! it is gone.

use std::path::{Path, PathBuf};

use crate::instance_env::INSTANCE_ROOT_SECURE_SUBDIR;

/// The `paired_user.json` paths a process would itself compute, given its raw
/// `QONTINUI_SECURE_STORAGE_DIR` and its instance root.
///
/// - No instance root: exactly
///   [`super::binding_store_candidate_paths_with`] — canonical first, then the
///   bare default when an override is set. Unchanged behaviour.
/// - Under an instance root: ONE path. The override when it is non-blank (the
///   runner's startup has already refused one outside the root, and supplies
///   `<root>/secure` when the launcher set none); otherwise `<root>/secure`
///   directly, so a process that never ran that startup — a lib bin launched
///   with only a root — still cannot fall back to the machine-global default.
pub(crate) fn binding_store_candidate_paths_for(
    override_dir: Option<String>,
    instance_root: Option<&Path>,
) -> Vec<PathBuf> {
    let Some(root) = instance_root else {
        return super::binding_store_candidate_paths_with(override_dir);
    };
    let canonical = override_dir
        .filter(|s| !s.trim().is_empty())
        .and_then(|dir| super::paired_user_path_with(Some(dir)))
        .unwrap_or_else(|| {
            root.join(INSTANCE_ROOT_SECURE_SUBDIR)
                .join("paired_user.json")
        });
    vec![canonical]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness's bare `data_local_dir()` default — what an override-carrying
    /// PRIMARY-side process still lists as its second candidate.
    fn bare_default() -> Option<PathBuf> {
        super::super::paired_user_path_with(None)
    }

    #[test]
    fn without_a_root_the_candidates_are_unchanged() {
        let dir = std::env::temp_dir().join("subject-isolation-override");
        let over = Some(dir.to_string_lossy().into_owned());
        assert_eq!(
            binding_store_candidate_paths_for(over.clone(), None),
            super::super::binding_store_candidate_paths_with(over),
        );
        assert_eq!(
            binding_store_candidate_paths_for(None, None),
            super::super::binding_store_candidate_paths_with(None),
        );
        // And the two-path shape is still there when an override is set (on
        // any box that resolves a data-local dir at all).
        if let Some(bare) = bare_default() {
            let got = binding_store_candidate_paths_for(Some(dir.to_string_lossy().into()), None);
            assert_eq!(got, vec![dir.join("paired_user.json"), bare]);
        }
    }

    #[test]
    fn under_a_root_the_override_is_the_only_candidate() {
        let root = std::env::temp_dir().join("subject-root");
        let secure = root.join("secure");
        let got = binding_store_candidate_paths_for(
            Some(secure.to_string_lossy().into_owned()),
            Some(&root),
        );
        assert_eq!(got, vec![secure.join("paired_user.json")]);
        if let Some(bare) = bare_default() {
            assert!(
                !got.contains(&bare),
                "a subject must never list the harness's bare-default binding store"
            );
        }
    }

    #[test]
    fn under_a_root_with_no_override_the_root_store_is_the_only_candidate() {
        let root = std::env::temp_dir().join("subject-root-no-override");
        let expected = vec![root.join("secure").join("paired_user.json")];
        assert_eq!(
            binding_store_candidate_paths_for(None, Some(&root)),
            expected
        );
        // A blank override is no override — never the machine-global default.
        assert_eq!(
            binding_store_candidate_paths_for(Some(String::new()), Some(&root)),
            expected
        );
        assert_eq!(
            binding_store_candidate_paths_for(Some("  ".into()), Some(&root)),
            expected
        );
    }
}
