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
//! (`util/mod.rs`). A third walks every source file under `src/` and fails
//! when a `#[path = "…"]` outside the lib resolves to a file the lib owns —
//! the same second copy under a different module name, which a check on
//! names cannot see.
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
const ALLOWED_IN_BOTH_ROOTS: &[&str] = &["auth", "test_env", "util"];

/// Children of `util` allowed in both the lib's inline `pub mod util { … }`
/// and the bin's `util/mod.rs`. Empty: the bin re-exports the lib's
/// `error_chain` instead of declaring it.
const ALLOWED_IN_BOTH_UTIL_TREES: &[&str] = &[];

const LIB_RS: &str = include_str!("lib.rs");
const MAIN_RS: &str = include_str!("main.rs");
const UTIL_MOD_RS: &str = include_str!("util/mod.rs");

/// Splits leading outer attributes (`#[...]`, possibly several, possibly with
/// nested brackets, parens or string literals) off `line`. Returns the
/// attribute texts and the remainder, or `None` when an attribute does not
/// close on this line.
fn split_leading_attrs(line: &str) -> Option<(Vec<&str>, &str)> {
    let mut attrs = Vec::new();
    let mut rest = line.trim_start();
    while rest.starts_with("#[") {
        let mut depth = 0usize;
        let mut in_str = false;
        let mut escaped = false;
        let mut end = None;
        for (i, c) in rest.char_indices() {
            if in_str {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '[' | '(' => depth += 1,
                ']' | ')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        // `end` follows an ASCII `]`, so it is a char boundary.
        let (attr, tail) = rest.split_at(end?);
        attrs.push(attr);
        rest = tail.trim_start();
    }
    Some((attrs, rest))
}

/// The module name a line declares, if it is a `mod` declaration.
///
/// Accepts leading attributes on the same line (`#[cfg(test)] mod x;`), an
/// optional visibility (`pub`, `pub(crate)`, `pub(super)`, …) and either form
/// (`mod x;` or inline `mod x {`). Callers decide indentation; comment lines
/// never match because they do not start with `#[`/`mod`/`pub`.
fn declared_mod_name(line: &str) -> Option<&str> {
    let (_, mut rest) = split_leading_attrs(line)?;
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

/// The value of a `path = "…"` key inside one attribute's text — the plain
/// `#[path = "…"]` and the `#[cfg_attr(…, path = "…")]` form alike.
fn path_attr_value(attr: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(found) = attr.get(from..)?.find("path") {
        let at = from + found;
        from = at + "path".len();
        let preceded_by_ident = attr
            .get(..at)?
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if preceded_by_ident {
            continue;
        }
        let Some(value) = attr
            .get(from..)?
            .trim_start()
            .strip_prefix('=')
            .map(str::trim_start)
            .and_then(|v| v.strip_prefix('"'))
        else {
            continue;
        };
        return value.split_once('"').map(|(v, _)| v);
    }
    None
}

/// Lexically normalises a `/`-separated path relative to `src/` (`.` dropped,
/// `..` folded; a `..` above `src/` is kept as a leading `../`).
fn normalize_rel(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Net `{`/`}` count of one line of code, skipping `//` comments, string
/// literals and `'{'`-style char literals. Approximate on purpose: it only
/// feeds the inline-module stack, whose resolution is added to — never
/// substituted for — the file-relative one.
fn net_braces(code: &str) -> i64 {
    let chars: Vec<char> = code.chars().collect();
    let (mut net, mut i, mut in_str) = (0i64, 0usize, false);
    while let Some(&c) = chars.get(i) {
        if in_str {
            match c {
                '\\' => i += 1,
                '"' => in_str = false,
                _ => {}
            }
        } else {
            match c {
                '/' if chars.get(i + 1) == Some(&'/') => break,
                '"' => in_str = true,
                '\'' if chars.get(i + 2) == Some(&'\'') => i += 2,
                '{' => net += 1,
                '}' => net -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    net
}

/// One non-inline `mod x;` declaration carrying a `#[path = "…"]`.
#[derive(Debug)]
struct PathSite {
    /// Declaring file, relative to `src/`.
    file: String,
    /// 1-based line of the `mod` declaration.
    line: usize,
    /// Where the attribute can point, relative to `src/` and normalised: the
    /// file-directory resolution, plus the inline-module one when the
    /// declaration sits inside `mod … { }` blocks.
    targets: Vec<String>,
}

/// Every `#[path]`-carrying `mod x;` in one file — attribute on the same line
/// or on preceding lines (doc comments and blank lines in between allowed,
/// multi-line attributes joined).
///
/// Resolution follows the reference: outside inline modules a `#[path]` is
/// relative to the declaring file's directory; inside `mod a { mod b { … } }`
/// it is relative to `<dir>/a/b/` for a mod-rs file (`mod.rs`, a crate root)
/// and `<dir>/<stem>/a/b/` otherwise.
fn path_sites_in(rel: &str, src: &str) -> Vec<PathSite> {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let mod_rs = matches!(name, "mod.rs" | "lib.rs" | "main.rs") || dir == "bin";
    let inline_base = if mod_rs {
        dir.to_owned()
    } else {
        format!("{dir}/{}", name.trim_end_matches(".rs"))
    };
    let mut sites = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    // A multi-line attribute being joined, and how many lines it has taken. An
    // attribute that has not closed within `MAX_ATTR_LINES` is dropped, so a
    // stray `#[` (say, inside a raw string) cannot swallow the rest of a file.
    const MAX_ATTR_LINES: usize = 16;
    let mut open_attr: Option<(String, usize)> = None;
    let mut inline_stack: Vec<(String, i64)> = Vec::new();
    let mut depth = 0i64;
    for (idx, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        let joined;
        let mut attr_lines = 0;
        let item = if let Some((mut acc, taken)) = open_attr.take() {
            acc.push(' ');
            acc.push_str(trimmed);
            attr_lines = taken;
            joined = acc;
            joined.as_str()
        } else if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        } else {
            trimmed
        };
        let rest = if item.starts_with("#[") {
            let Some((attrs, rest)) = split_leading_attrs(item) else {
                if attr_lines < MAX_ATTR_LINES {
                    open_attr = Some((item.to_owned(), attr_lines + 1));
                } else {
                    pending.clear();
                }
                continue;
            };
            pending.extend(attrs.into_iter().map(str::to_owned));
            if rest.is_empty() {
                continue;
            }
            rest
        } else {
            item
        };
        let decl = declared_mod_name(rest);
        if decl.is_some() && !rest.contains('{') {
            if let Some(target) = pending.iter().find_map(|a| path_attr_value(a)) {
                let mut targets = vec![normalize_rel(&format!("{dir}/{target}"))];
                if !inline_stack.is_empty() {
                    let names: Vec<&str> = inline_stack.iter().map(|(n, _)| n.as_str()).collect();
                    targets.push(normalize_rel(&format!(
                        "{inline_base}/{}/{target}",
                        names.join("/")
                    )));
                }
                sites.push(PathSite {
                    file: rel.to_owned(),
                    line: idx + 1,
                    targets,
                });
            }
        }
        pending.clear();
        let before = depth;
        depth += net_braces(rest);
        if let Some(name) = decl.filter(|_| depth > before) {
            inline_stack.push((name.to_owned(), before));
        }
        inline_stack.retain(|(_, opened_at)| depth > *opened_at);
    }
    sites
}

/// Every `.rs` file under `dir`, as `(path relative to root, contents)`.
fn rust_files(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(root, &path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            out.push((rel, text));
        }
    }
}

/// Module paths (relative to `src/`, no extension) whose files the LIB
/// declares: its column-0 non-inline `mod x;` items plus the children of its
/// inline `pub mod util { … }`. A file is lib-owned when it is `lib.rs`,
/// `<m>.rs`, or anything under `<m>/`.
fn lib_module_paths() -> Vec<String> {
    let mut paths: Vec<String> = LIB_RS
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace) && !line.contains('{'))
        .filter_map(declared_mod_name)
        .map(str::to_owned)
        .collect();
    paths.extend(
        inline_child_mods(LIB_RS, "util")
            .into_iter()
            .map(|c| format!("util/{c}")),
    );
    paths
}

fn lib_owns(
    module_paths: &[String],
    extra: &std::collections::BTreeSet<String>,
    file: &str,
) -> bool {
    file == "lib.rs"
        || extra.contains(file)
        || module_paths.iter().any(|m| {
            file.strip_prefix(m.as_str())
                .is_some_and(|tail| tail == ".rs" || tail.starts_with('/'))
        })
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
    assert_eq!(declared_mod_name("#[cfg(test)] mod x;"), Some("x"));
    assert_eq!(
        declared_mod_name("#[cfg(test)] pub(crate) mod x;"),
        Some("x")
    );
    assert_eq!(
        declared_mod_name("#[allow(dead_code)] #[cfg(any())] mod x {"),
        Some("x")
    );
    assert_eq!(
        declared_mod_name("#[cfg_attr(unix, path = \"a]b.rs\")] mod x;"),
        Some("x")
    );
    assert_eq!(declared_mod_name("#[cfg(any("), None);
    assert_eq!(declared_mod_name("#[derive(Debug)] struct J;"), None);
}

/// The name ratchet above reads `mod` NAMES. A `#[path = "auth.rs"] mod
/// auth_v2;` in a bin file compiles the lib's `auth.rs` a second time under
/// another name and slips past it, so this walks every source file and fails
/// when a `#[path]` from outside the lib resolves to a file the lib owns.
#[test]
fn no_path_attribute_outside_the_lib_points_at_a_lib_file() {
    let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src_root, &src_root, &mut files);
    let sites: Vec<PathSite> = files
        .iter()
        .flat_map(|(rel, text)| path_sites_in(rel, text))
        .collect();

    // Guard against a vacuous pass: the walk must see the tree and find the
    // `#[path]` sites known to exist, resolved where rustc resolves them.
    assert!(files.len() > 500, "walk saw only {} .rs files", files.len());
    for (file, target) in [
        ("bin/scan_spec_distinctness.rs", "spec_api/types.rs"),
        ("bin/scan_spec_distinctness.rs", "spec_api/distinctness.rs"),
        (
            "step_executor/executor.rs",
            "step_executor/typed_dispatch_corpus_tests.rs",
        ),
        ("wedge_diagnostics.rs", "../build.rs"),
    ] {
        assert!(
            sites
                .iter()
                .any(|s| s.file == file && s.targets.iter().any(|t| t == target)),
            "known `#[path]` site {file} -> {target} not found; sites: {sites:#?}"
        );
    }

    let module_paths = lib_module_paths();
    // Files the lib reaches through its own `#[path]` attributes are lib-owned
    // too; iterate because such a file can carry one in turn.
    let mut extra = std::collections::BTreeSet::new();
    loop {
        let before = extra.len();
        for site in &sites {
            if lib_owns(&module_paths, &extra, &site.file) {
                extra.extend(site.targets.iter().cloned());
            }
        }
        if extra.len() == before {
            break;
        }
    }
    for owned in [
        "auth.rs",
        "secure_storage.rs",
        "process_helpers.rs",
        "coord_mcp_config.rs",
        "fs_atomic.rs",
        "fs_perms.rs",
        "machine_identity.rs",
        "util/error_chain.rs",
    ] {
        assert!(
            lib_owns(&module_paths, &extra, owned),
            "{owned} not recognised as lib-owned (module paths: {module_paths:?})"
        );
    }
    assert!(!lib_owns(&module_paths, &extra, "main.rs"));
    assert!(!lib_owns(&module_paths, &extra, "util/path_extraction.rs"));

    let offenders: Vec<String> = sites
        .iter()
        .filter(|s| !lib_owns(&module_paths, &extra, &s.file))
        .flat_map(|s| {
            s.targets
                .iter()
                .filter(|t| lib_owns(&module_paths, &extra, t))
                .map(move |t| format!("{}:{} -> {t}", s.file, s.line))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "`#[path]` declaration(s) outside the lib resolve to a lib-owned file: {offenders:?}. \
         That file then compiles twice, duplicating its statics and types. Import the lib's \
         module (`pub(crate) use qontinui_runner_lib::<name>;`) instead."
    );
}

#[test]
fn path_sites_resolve_like_rustc() {
    // Built from quoted lines so this file's own walk never reads them as
    // attributes.
    let src = [
        "#[cfg(test)]",
        "#[path = \"a.rs\"]",
        "mod x;",
        "#[cfg_attr(unix, path = \"../b.rs\")] pub(crate) mod y;",
        "mod outer {",
        "    #[path = \"c.rs\"]",
        "    mod z;",
        "}",
        "#[cfg(any(",
        "    test,",
        "))]",
        "#[path = \"d.rs\"]",
        "mod w;",
        "#[path = \"e.rs\"]",
        "mod inline { }",
    ]
    .join("\n");
    let sites = path_sites_in("dir/file.rs", &src);
    let got: Vec<(usize, Vec<String>)> = sites.into_iter().map(|s| (s.line, s.targets)).collect();
    assert_eq!(
        got,
        vec![
            (3, vec!["dir/a.rs".to_owned()]),
            (4, vec!["b.rs".to_owned()]),
            (
                7,
                vec!["dir/c.rs".to_owned(), "dir/file/outer/c.rs".to_owned()]
            ),
            (13, vec!["dir/d.rs".to_owned()]),
        ]
    );
    let in_mod_rs = path_sites_in(
        "dir/mod.rs",
        &["mod outer {", "    #[path = \"c.rs\"]", "    mod z;", "}"].join("\n"),
    );
    assert_eq!(in_mod_rs[0].targets, vec!["dir/c.rs", "dir/outer/c.rs"]);
    assert_eq!(
        path_attr_value("#[cfg_attr(x, path = \"p.rs\")]"),
        Some("p.rs")
    );
    assert_eq!(path_attr_value("#[doc = \"my_path = \\\"no\\\"\"]"), None);
}
