//! Ratchet: no module may be declared in BOTH crate roots.
//!
//! A module declared by `lib.rs` AND by `main.rs` compiles twice — once into
//! `qontinui_runner_lib`, once into the `qontinui-runner` bin — so every
//! static in it exists twice in one process and every type in it exists as
//! two nominally distinct types. The rule is one owner: the lib declares the
//! module and the bin imports it with `pub(crate) use qontinui_runner_lib::X;`.
//!
//! This test reads both crate roots as text and fails, naming the module, when
//! a top-level `mod` name appears in both outside [`ALLOWED_IN_BOTH_ROOTS`].
//! A second assertion does the same for the children of `util`, which the lib
//! declares inline (`pub mod util { … }`) and the bin declares as a file tree
//! (`util/mod.rs`).
//!
//! The allowlists only shrink. Plan
//! `2026-10-04-runner-seven-modules-compile-into-both-crates-and-split-their-process-state`.

/// Top-level module names allowed in both crate roots.
///
/// - `auth` — owed by Phase 2 of the plan above; remove it when the bin
///   imports the lib's copy.
/// - `test_env` — per-crate BY DESIGN: both copies only re-export the one
///   `ambient::test_support`, so no state is duplicated.
/// - `util` — the NAME is shared, the contents are not: the lib's inline
///   `util` holds lib-only children and the bin's `util/` tree holds bin-only
///   ones. Duplicated children are policed by
///   [`ALLOWED_IN_BOTH_UTIL_TREES`].
const ALLOWED_IN_BOTH_ROOTS: &[&str] = &[
    "auth",
    "coord_mcp_config",
    "fs_atomic",
    "fs_perms",
    "machine_identity",
    "process_helpers",
    "secure_storage",
    "test_env",
    "util",
];

/// Children of `util` allowed in both the lib's inline `pub mod util { … }`
/// and the bin's `util/mod.rs`.
const ALLOWED_IN_BOTH_UTIL_TREES: &[&str] = &["error_chain"];

const LIB_RS: &str = include_str!("lib.rs");
const MAIN_RS: &str = include_str!("main.rs");
const UTIL_MOD_RS: &str = include_str!("util/mod.rs");

/// The module name a line declares, if it is a `mod` declaration.
///
/// Accepts an optional visibility (`pub`, `pub(crate)`, `pub(super)`, …) and
/// either form (`mod x;` or inline `mod x {`). Callers decide indentation;
/// comment lines never match because they do not start with `mod`/`pub`.
fn declared_mod_name(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    if let Some(after_pub) = rest.strip_prefix("pub") {
        rest = if let Some(paren) = after_pub.strip_prefix('(') {
            paren.split_once(')')?.1
        } else if after_pub.starts_with(char::is_whitespace) {
            after_pub
        } else {
            return None;
        }
        .trim_start();
    }
    let name_and_tail = rest.strip_prefix("mod ")?.trim_start();
    let end = name_and_tail
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(name_and_tail.len());
    // `end` sits on an ASCII boundary (or the end), so `split_at` cannot panic.
    let (name, tail) = name_and_tail.split_at(end);
    let tail = tail.trim_start();
    (!name.is_empty() && (tail.starts_with(';') || tail.starts_with('{'))).then_some(name)
}

/// `mod` names declared at column 0 of a source file — the crate root's (or a
/// `mod.rs`'s) own children. Indented declarations belong to inline modules
/// and are not this file's top level.
fn top_level_mods(src: &str) -> std::collections::BTreeSet<String> {
    src.lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(declared_mod_name)
        .map(str::to_owned)
        .collect()
}

/// `mod` names declared directly inside the column-0 inline module `name`
/// (`… mod <name> {` up to its column-0 closing `}`). Panics when the inline
/// block is absent, so a refactor that turns it into a file cannot make the
/// nested assertion pass vacuously.
fn inline_child_mods(src: &str, name: &str) -> std::collections::BTreeSet<String> {
    let mut lines = src.lines();
    lines
        .by_ref()
        .find(|line| {
            !line.starts_with(char::is_whitespace)
                && declared_mod_name(line) == Some(name)
                && line.trim_end().ends_with('{')
        })
        .unwrap_or_else(|| panic!("no inline `mod {name} {{` at column 0"));
    lines
        .take_while(|line| !line.starts_with('}'))
        .filter_map(declared_mod_name)
        .map(str::to_owned)
        .collect()
}

/// Names in both sets that the allowlist does not cover, sorted.
fn unallowed_overlap(
    a: &std::collections::BTreeSet<String>,
    b: &std::collections::BTreeSet<String>,
    allowed: &[&str],
) -> Vec<String> {
    a.intersection(b)
        .filter(|name| !allowed.contains(&name.as_str()))
        .cloned()
        .collect()
}

#[test]
fn no_module_is_declared_in_both_crate_roots() {
    let lib = top_level_mods(LIB_RS);
    let bin = top_level_mods(MAIN_RS);
    // Guard against a parser regression making the test vacuous.
    assert!(
        lib.contains("ambient") && bin.contains("coord_http"),
        "mod extraction found nothing recognisable (lib: {lib:?}, bin: {bin:?})"
    );
    let offenders = unallowed_overlap(&lib, &bin, ALLOWED_IN_BOTH_ROOTS);
    assert!(
        offenders.is_empty(),
        "module(s) declared in BOTH lib.rs and main.rs: {offenders:?}. Each compiles twice, \
         duplicating its statics and types. Declare it in lib.rs only and import it in \
         main.rs with `pub(crate) use qontinui_runner_lib::<name>;` (the allowlist only shrinks)."
    );
}

#[test]
fn no_util_child_is_declared_in_both_util_trees() {
    let lib = inline_child_mods(LIB_RS, "util");
    let bin = top_level_mods(UTIL_MOD_RS);
    assert!(
        !lib.is_empty() && bin.contains("path_extraction"),
        "util child extraction found nothing recognisable (lib: {lib:?}, bin: {bin:?})"
    );
    let offenders = unallowed_overlap(&lib, &bin, ALLOWED_IN_BOTH_UTIL_TREES);
    assert!(
        offenders.is_empty(),
        "util child module(s) declared in BOTH lib.rs's inline `pub mod util` and \
         util/mod.rs: {offenders:?}. Each compiles twice. Keep the lib's declaration and \
         re-export it from util/mod.rs with `pub use qontinui_runner_lib::util::<name>;`."
    );
}

#[test]
fn declared_mod_name_reads_every_declaration_form() {
    assert_eq!(declared_mod_name("mod a;"), Some("a"));
    assert_eq!(declared_mod_name("pub mod b_2;"), Some("b_2"));
    assert_eq!(declared_mod_name("pub(crate) mod c {"), Some("c"));
    assert_eq!(declared_mod_name("    pub mod d;"), Some("d"));
    assert_eq!(declared_mod_name("// mod e;"), None);
    assert_eq!(declared_mod_name("/// mod f;"), None);
    assert_eq!(
        declared_mod_name("pub(crate) use qontinui_runner_lib::g;"),
        None
    );
    assert_eq!(declared_mod_name("public mod h;"), None);
    assert_eq!(declared_mod_name("mod i"), None);
}
