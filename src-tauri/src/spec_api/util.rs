//! Spec API shared utilities.

/// Compile-time no-op marker for call sites that pass a hardcoded
/// `app_id` literal to `resolve_specs_root` (or a callee that should take
/// `app_id`). Stream B introduced the multi-tenant storage signature, Stream C
/// threaded `app_id` through the in-process callers, and Stream D still
/// owns two `spec_api/spec_check.rs` sites that will gain `app_id` when the
/// `POST /spec-check` + `/spec-check/batch` request bodies extend to carry it.
///
/// Inventory: `grep -rn 'unimplemented_app_scoping' src-tauri/src/`
/// returns every remaining site (Stream D will drive this to zero).
///
/// The macro expands to nothing — pure marker. It exists so the grep query
/// above stays cheap and unambiguous (no false-positives from a magic string
/// literal in unrelated code).
#[macro_export]
macro_rules! unimplemented_app_scoping {
    () => {};
}
