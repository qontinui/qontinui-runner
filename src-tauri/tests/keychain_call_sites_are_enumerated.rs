//! Ratchet: **every OS-keychain call site in the runner is named here, and no
//! test reaches the keychain by flipping `QONTINUI_DISABLE_KEYCHAIN`.**
//!
//! ## Why this test exists
//!
//! Runner unit tests used to write the developer's real credential store. The
//! test-only `AuthManager` constructors gave every instance a fresh
//! `com.qontinui.runner.test.<uuid>` service name, and `store_tokens` wrote a
//! keychain backup under it whenever `QONTINUI_DISABLE_KEYCHAIN` was unset —
//! which is every local `cargo test`. Measured 2026-10-05: 745 such entries in
//! one machine's Windows Credential Manager. Tests built with
//! `AuthManager::new()` also read, and could overwrite, the operator's live
//! `com.qontinui.runner` token. CI never saw it because CI sets the variable.
//!
//! The fix is one structural guard: a test binary marks itself at load time
//! (`auth::deny_os_keychain_for_this_test_process`, called from a
//! `#[cfg(test)]` ctor in `lib.rs` and `main.rs`) and `AuthManager` refuses the
//! keychain while the mark is set. It replaced a dozen per-module
//! `set_var("QONTINUI_DISABLE_KEYCHAIN", "1")` workarounds.
//!
//! The guard covers `AuthManager` only. This ratchet keeps the rest honest:
//!
//! - A new `Entry::new(` call (the `keyring` crate's only door) outside the
//!   allowlist fails here, so whoever adds one must decide whether tests can
//!   reach it — and give it a test seam (as `registry_creds` does with a fake
//!   store) or route it through `AuthManager`'s guard.
//! - A new `set_var("QONTINUI_DISABLE_KEYCHAIN"` fails here, because it means
//!   someone is re-growing the per-module workaround instead of trusting the
//!   guard.
//!
//! A source scan, not a proof: it cannot see a keychain call spelled through a
//! re-export or a macro. It makes the ordinary way of adding one impossible to
//! merge unnoticed. Plan `2026-10-05-runner-unit-tests-write-to-the-real-os-keychain`.

use std::path::{Path, PathBuf};

/// Files allowed to construct a `keyring::Entry`, relative to `src/`, with why
/// tests cannot reach the real keychain through each.
const KEYRING_CALL_SITES: &[(&str, &str)] = &[
    (
        "auth.rs",
        "AuthManager — refused in every test binary by the load-time marker",
    ),
    (
        "config_facade.rs",
        "no test calls its keychain getters or setters",
    ),
    (
        "install_effects_producer/registry_creds.rs",
        "tests use the FakeStore behind RegistryTokenLookup",
    ),
    (
        "security/credential_proxy.rs",
        "tests cover placeholder generation only",
    ),
    (
        "wrappers/credentials.rs",
        "tests cover name validation only",
    ),
];

/// Files still allowed to `set_var("QONTINUI_DISABLE_KEYCHAIN", ...)`.
/// `commands/auth.rs` `hermetic_auth_manager` is owned by open PR
/// qontinui-runner#2001; Phase 6 of the plan removes it after that lands.
const DISABLE_KEYCHAIN_SETTERS: &[&str] = &["commands/auth.rs"];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `src/`-relative path with forward slashes, so the allowlists read the same
/// on every OS.
fn rel(path: &Path) -> String {
    path.strip_prefix(src_root())
        .expect("under src/")
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every `src/` file whose non-comment lines contain `needle`.
fn files_containing(needle: &str) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let mut hits: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            text.lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(needle))
        })
        .map(|path| rel(path))
        .collect();
    hits.sort();
    hits
}

#[test]
fn every_keyring_entry_construction_is_on_the_allowlist() {
    let allowed: Vec<&str> = KEYRING_CALL_SITES.iter().map(|(path, _)| *path).collect();
    let found = files_containing("Entry::new(");
    let unlisted: Vec<&String> = found
        .iter()
        .filter(|path| !allowed.contains(&path.as_str()))
        .collect();
    assert!(
        unlisted.is_empty(),
        "new OS-keychain call site(s) {unlisted:?}: add each to KEYRING_CALL_SITES with the \
         reason no test can reach the real keychain through it (a fake store, or AuthManager's \
         test-process guard). Found: {found:?}"
    );
    // A listed file that no longer constructs an Entry is a stale row; drop it
    // so the list stays the true inventory.
    let stale: Vec<&&str> = allowed
        .iter()
        .filter(|path| !found.iter().any(|f| f == *path))
        .collect();
    assert!(
        stale.is_empty(),
        "KEYRING_CALL_SITES lists file(s) with no Entry::new( call: {stale:?}"
    );
}

#[test]
fn no_test_reenables_the_per_module_keychain_workaround() {
    let found = files_containing("set_var(\"QONTINUI_DISABLE_KEYCHAIN\"");
    let unlisted: Vec<&String> = found
        .iter()
        .filter(|path| !DISABLE_KEYCHAIN_SETTERS.contains(&path.as_str()))
        .collect();
    assert!(
        unlisted.is_empty(),
        "{unlisted:?} set QONTINUI_DISABLE_KEYCHAIN. A test binary already never reaches the OS \
         keychain (auth::deny_os_keychain_for_this_test_process), so the per-module workaround \
         is redundant and races sibling tests on process env — remove it."
    );
}
