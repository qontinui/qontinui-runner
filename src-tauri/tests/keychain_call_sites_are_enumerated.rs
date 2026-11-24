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
//! The same mark shuts `instance_env::os_keychain_allowed`, the gate every other
//! keychain store names, so in a unit-test binary no gated store reaches the
//! keychain either. This ratchet keeps the inventory honest:
//!
//! - A new `Entry::new` call (any of the `keyring` crate's constructors —
//!   `new`, `new_with_target`, `new_with_credential`) outside the allowlist
//!   fails here, so whoever adds one must decide whether tests can
//!   reach it — and put it behind `os_keychain_allowed` or route it through
//!   `AuthManager`'s guard.
//! - A literal `set_var("QONTINUI_DISABLE_KEYCHAIN"` anywhere under `src/`
//!   fails here, and so does any mention of the `"QONTINUI_DISABLE_KEYCHAIN"`
//!   name outside the files that own it. Either means someone is re-growing the
//!   per-module workaround instead of trusting the guard. A per-file scan
//!   cannot see a setter wrapped in a helper inside one of those files; review
//!   is the backstop there.
//!
//! A source scan, not a proof: it cannot see a keychain call spelled through an
//! aliased import, a re-export or a macro. The load-time guard itself covers the
//! lib and runner-bin unit-test binaries only; integration tests and the
//! `src/bin/*` test binaries have no hook, which is why new call sites must be
//! named here. It makes the ordinary way of adding one impossible to
//! merge unnoticed. Plan `2026-10-05-runner-unit-tests-write-to-the-real-os-keychain`.

use std::path::{Path, PathBuf};

/// Files allowed to construct a `keyring::Entry`, relative to `src/`, with why
/// tests cannot reach the real keychain through each.
const KEYRING_CALL_SITES: &[(&str, &str)] = &[
    (
        "ambient.rs",
        "no call: the keychain-gate source scan names `Entry::new` in string literals",
    ),
    (
        "auth.rs",
        "AuthManager — refused in every test binary by the load-time marker",
    ),
    (
        "config_facade.rs",
        "behind os_keychain_allowed, which the test-process marker shuts",
    ),
    (
        "install_effects_producer/registry_creds.rs",
        "behind os_keychain_allowed; tests use the FakeStore behind RegistryTokenLookup",
    ),
    (
        "security/credential_proxy.rs",
        "behind os_keychain_allowed; tests cover placeholder generation only",
    ),
    (
        "wrappers/credentials.rs",
        "behind os_keychain_allowed; tests cover name validation only",
    ),
];

/// Files allowed to name `"QONTINUI_DISABLE_KEYCHAIN"` at all, and why.
const DISABLE_KEYCHAIN_NAMERS: &[(&str, &str)] = &[
    ("auth.rs", "the reader, and the guard's own regression test"),
    (
        "ambient.rs",
        "the ambient-key roster IsolatedAmbient captures and restores",
    ),
    ("ci_node/manifest.rs", "the CI env it forwards to CI steps"),
    (
        "instance_env.rs",
        "the startup default a subject runner exports under an instance root",
    ),
];

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
    let found = files_containing("Entry::new");
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
        "KEYRING_CALL_SITES lists file(s) with no Entry::new call: {stale:?}"
    );
}

#[test]
fn no_test_reenables_the_per_module_keychain_workaround() {
    let allowed: Vec<&str> = DISABLE_KEYCHAIN_NAMERS
        .iter()
        .map(|(path, _)| *path)
        .collect();
    let found = files_containing("\"QONTINUI_DISABLE_KEYCHAIN\"");
    let unlisted: Vec<&String> = found
        .iter()
        .filter(|path| !allowed.contains(&path.as_str()))
        .collect();
    assert!(
        unlisted.is_empty(),
        "{unlisted:?} name QONTINUI_DISABLE_KEYCHAIN. A test binary already never reaches the OS \
         keychain (auth::deny_os_keychain_for_this_test_process), so the per-module workaround \
         is redundant and races sibling tests on process env — remove it."
    );
    let stale: Vec<&&str> = allowed
        .iter()
        .filter(|path| !found.iter().any(|f| f == *path))
        .collect();
    assert!(
        stale.is_empty(),
        "DISABLE_KEYCHAIN_NAMERS lists file(s) that no longer name the variable: {stale:?}"
    );

    let setters = files_containing("set_var(\"QONTINUI_DISABLE_KEYCHAIN\"");
    assert!(
        setters.is_empty(),
        "{setters:?} set QONTINUI_DISABLE_KEYCHAIN; the load-time guard makes that \
         redundant — remove it."
    );
}
