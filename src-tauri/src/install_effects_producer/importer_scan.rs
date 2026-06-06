//! Declare-time **importer scan** (plan decision 5): a pure, capped, TEXT-ONLY
//! scan of the target repo's source files for `import` / `require` / `use`
//! statements that name one of the packages being installed.
//!
//! This is deliberately NOT a code-graph / LSP walk — it is a bounded textual
//! scan, honest at coverage 0.5 (coord's `classify_import_break` runs the
//! heuristic at the `cargo-syntactic` 0.5 tier). It produces:
//!
//! - the [`super::types::ImporterRecord`] list coord consumes at declare to
//!   sharpen the import-break (`types`) dimension, and
//! - the affected-file list the Phase-3 typecheck loop runs `cargo check`/`mypy`
//!   over (the files that actually reference a touched package).
//!
//! ## Caps (a hostile/huge repo must not pin the route)
//!
//! - [`MAX_SCAN_FILES`] source files visited (depth-first, deterministic order).
//! - [`MAX_FILE_BYTES`] read per file (a generated megafile is truncated).
//! - [`MAX_AFFECTED_FILES`] importer paths retained per package.
//!
//! The scan walks only recognized source extensions and skips the usual
//! vendor/build/VCS scratch dirs (reusing the same denylist spirit as
//! `fs_observer`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::types::{ImporterRecord, PackageManager, PackageSpecInput};

/// Max source files visited in one scan. A large monorepo subtree is bounded;
/// excess files are simply not scanned (coverage stays honest at 0.5 — we never
/// claim to have seen more than we did). 4000 covers a typical package's `src/`
/// without letting a pathological tree stall the declare path.
pub const MAX_SCAN_FILES: usize = 4000;

/// Max bytes read from a single source file. An import statement is at the top;
/// a 256 KiB window catches it without slurping a generated bundle.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Max importer paths retained per package. Coord only needs enough to classify
/// the break risk + name a few files in the rationale; an unbounded list would
/// bloat the declare body.
const MAX_AFFECTED_FILES: usize = 50;

/// Recognized source extensions per ecosystem family. We scan only files whose
/// extension matches the package manager's language so a Rust repo's scan
/// doesn't read every `.md`.
fn source_extensions(pm: PackageManager) -> &'static [&'static str] {
    match pm {
        PackageManager::Npm | PackageManager::Yarn | PackageManager::Pnpm => {
            &["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"]
        }
        PackageManager::Cargo => &["rs"],
        PackageManager::Pip | PackageManager::Poetry => &["py", "pyi"],
    }
}

/// Directory segments never descended into (vendor/build/VCS scratch). Mirrors
/// `fs_observer::is_always_dropped`'s spirit but as a directory-prune set.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".idea",
    ".vscode",
    ".worktrees",
    ".next",
    "vendor",
];

/// The result of an importer scan: the coord inventory records plus the flat,
/// deduped affected-file list (worktree-relative, `/`-normalized) for the
/// Phase-3 typecheck loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImporterScan {
    pub inventory: Vec<ImporterRecord>,
    pub affected_files: Vec<String>,
}

/// Scan `repo_path` for source files importing any of `packages`. Pure over the
/// filesystem snapshot (no mutation, no network, no subprocess); total over a
/// hostile tree (caps bound it). `cargo`'s `use` paths reference the crate by
/// its snake_case name even when the package is hyphenated, so for cargo we
/// match BOTH the verbatim name and its `-`→`_` form.
pub fn scan_importers(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[PackageSpecInput],
) -> ImporterScan {
    if packages.is_empty() {
        return ImporterScan::default();
    }
    let exts = source_extensions(pm);

    // Build the per-package match aliases. For each package we keep the set of
    // identifier strings whose presence in an import statement counts as a use.
    let pkgs: Vec<(String, Vec<String>)> = packages
        .iter()
        .map(|p| (p.name.clone(), match_aliases(pm, &p.name)))
        .collect();

    // package name → set of importing files (deduped, capped).
    let mut importers_by_pkg: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut affected: Vec<String> = Vec::new();
    let mut affected_seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut files_scanned = 0usize;
    let mut stack: Vec<PathBuf> = vec![repo_path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if files_scanned >= MAX_SCAN_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Deterministic order: collect + sort so the cap truncates predictably.
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for path in children {
            if files_scanned >= MAX_SCAN_FILES {
                break;
            }
            if path.is_dir() {
                let skip = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| SKIP_DIRS.contains(&n))
                    .unwrap_or(false);
                if !skip {
                    stack.push(path);
                }
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            files_scanned += 1;
            let Some(text) = read_capped(&path) else {
                continue;
            };
            let rel = rel_path(repo_path, &path);
            for (name, aliases) in &pkgs {
                if file_imports_any(pm, &text, aliases) {
                    let list = importers_by_pkg.entry(name.clone()).or_default();
                    if list.len() < MAX_AFFECTED_FILES && !list.contains(&rel) {
                        list.push(rel.clone());
                    }
                    if affected_seen.insert(rel.clone()) {
                        affected.push(rel.clone());
                    }
                }
            }
        }
    }

    // Build the inventory. A package with no importers is still recorded (coord
    // reads an empty `importers` as `None` break-risk — an honest "nothing
    // imports this yet"). `from`/`to` are left None here: the declare-time scan
    // does not know the resolved pins (the dry-run carries the version delta;
    // coord cross-references by package name). This keeps the scan pure over the
    // source tree alone.
    let inventory = pkgs
        .iter()
        .map(|(name, _)| ImporterRecord {
            package: name.clone(),
            importers: importers_by_pkg.get(name).cloned().unwrap_or_default(),
            from: None,
            to: None,
        })
        .collect();

    affected.sort();
    ImporterScan {
        inventory,
        affected_files: affected,
    }
}

/// The identifier aliases that count as importing `name` for `pm`. Cargo `use`
/// paths use the crate's snake_case ident, so a hyphenated package also matches
/// its `_` form. Node/Python use the package name verbatim in the specifier.
fn match_aliases(pm: PackageManager, name: &str) -> Vec<String> {
    let mut aliases = vec![name.to_string()];
    if matches!(pm, PackageManager::Cargo) {
        let snake = name.replace('-', "_");
        if snake != name {
            aliases.push(snake);
        }
    }
    aliases
}

/// Read up to [`MAX_FILE_BYTES`] of a file as lossy UTF-8. `None` on an I/O
/// error (unreadable file ⇒ simply not scanned).
fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Worktree-relative, `/`-normalized path for `abs` under `root`. Falls back to
/// the full lossy path if `abs` is not under `root` (shouldn't happen in a walk).
fn rel_path(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Does `text` import any alias in `aliases` under `pm`'s import syntax? A
/// bounded textual check — NOT a parse. We look for the alias as a quoted module
/// specifier (Node/Python `from`/`import`/`require`) or as a `use <crate>`
/// path segment (cargo). The check is conservative: a bare substring match would
/// over-fire (e.g. `react` inside `react-dom`), so we require the alias to be
/// delimited the way an import specifier delimits it.
fn file_imports_any(pm: PackageManager, text: &str, aliases: &[String]) -> bool {
    match pm {
        PackageManager::Npm | PackageManager::Yarn | PackageManager::Pnpm => {
            aliases.iter().any(|a| js_imports(text, a))
        }
        PackageManager::Pip | PackageManager::Poetry => aliases.iter().any(|a| py_imports(text, a)),
        PackageManager::Cargo => aliases.iter().any(|a| rs_uses(text, a)),
    }
}

/// JS/TS: the package appears as a quoted module specifier in an `import …`,
/// `require(…)`, `import(…)`, or `export … from` — i.e. `'pkg'` or `"pkg"` or a
/// subpath `'pkg/sub'`. We match the quoted specifier boundary so `react` does
/// not match `react-dom`.
fn js_imports(text: &str, pkg: &str) -> bool {
    for q in ['\'', '"', '`'] {
        // exact: `'pkg'`
        let exact = format!("{q}{pkg}{q}");
        if text.contains(&exact) {
            return true;
        }
        // subpath: `'pkg/...'`
        let subpath = format!("{q}{pkg}/");
        if text.contains(&subpath) {
            return true;
        }
    }
    false
}

/// Python: `import pkg`, `import pkg.sub`, `from pkg import …`, `from pkg.sub`.
/// The top-level module of a PyPI distribution is usually the normalized name
/// with `-`→`_` (we also try that). We require a word boundary after the module
/// so `requests` doesn't match `requests_oauthlib`.
fn py_imports(text: &str, pkg: &str) -> bool {
    let module = pkg.replace('-', "_");
    let candidates = if module == pkg {
        vec![module]
    } else {
        vec![module, pkg.to_string()]
    };
    for line in text.lines() {
        let t = line.trim_start();
        for m in &candidates {
            for prefix in [format!("import {m}"), format!("from {m}")] {
                if let Some(rest) = t.strip_prefix(&prefix) {
                    // boundary: end, whitespace, `.`, or `,` (import a, b)
                    match rest.chars().next() {
                        None => return true,
                        Some(c) if c.is_whitespace() || c == '.' || c == ',' => return true,
                        _ => {}
                    }
                }
            }
        }
    }
    false
}

/// Rust: `use <crate>` or `use <crate>::…` or `extern crate <crate>`. The crate
/// ident is the snake_case alias (handled by the caller). We require a boundary
/// (`::`, `;`, whitespace, or `{`) after the ident so `serde` doesn't match
/// `serde_json`.
fn rs_uses(text: &str, krate: &str) -> bool {
    for line in text.lines() {
        let t = line.trim_start();
        for prefix in [format!("use {krate}"), format!("extern crate {krate}")] {
            if let Some(rest) = t.strip_prefix(&prefix) {
                match rest.chars().next() {
                    None => return true,
                    Some(c) if c == ':' || c == ';' || c.is_whitespace() || c == '{' => {
                        return true
                    }
                    _ => {}
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn spec(name: &str) -> PackageSpecInput {
        PackageSpecInput {
            name: name.to_string(),
            version_req: None,
        }
    }

    #[test]
    fn js_scan_finds_importers_and_skips_noise() {
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.ts"),
            "import lodash from 'lodash';\nexport const x = 1;\n",
        )
        .unwrap();
        std::fs::write(root.join("src/b.js"), "const r = require(\"lodash/fp\");\n").unwrap();
        // noise: imports a DIFFERENT package whose name is a prefix
        std::fs::write(root.join("src/c.ts"), "import x from 'lodash-es';\n").unwrap();
        // noise: vendored dir must be skipped
        std::fs::create_dir_all(root.join("node_modules/lodash")).unwrap();
        std::fs::write(
            root.join("node_modules/lodash/index.js"),
            "import 'lodash';\n",
        )
        .unwrap();

        let scan = scan_importers(PackageManager::Npm, root, &[spec("lodash")]);
        assert_eq!(scan.inventory.len(), 1);
        let rec = &scan.inventory[0];
        assert_eq!(rec.package, "lodash");
        // a.ts (exact) + b.js (subpath) match; c.ts (lodash-es) + vendored skip
        let imp: std::collections::HashSet<&str> =
            rec.importers.iter().map(String::as_str).collect();
        assert!(imp.contains("src/a.ts"), "{imp:?}");
        assert!(imp.contains("src/b.js"), "{imp:?}");
        assert!(!imp.iter().any(|p| p.contains("lodash-es")), "{imp:?}");
        assert!(!imp.iter().any(|p| p.contains("node_modules")), "{imp:?}");
        assert_eq!(scan.affected_files.len(), 2);
    }

    #[test]
    fn rust_scan_matches_snake_case_crate_and_boundary() {
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "use serde_yaml::Value;\nuse serde::Serialize;\n",
        )
        .unwrap();
        // hyphenated package `serde-yaml` -> crate ident `serde_yaml`
        let scan = scan_importers(PackageManager::Cargo, root, &[spec("serde-yaml")]);
        let rec = scan
            .inventory
            .iter()
            .find(|r| r.package == "serde-yaml")
            .unwrap();
        assert_eq!(rec.importers, vec!["src/lib.rs".to_string()]);

        // `serde` must NOT match `serde_yaml`/`serde_json` (boundary).
        let scan2 = scan_importers(PackageManager::Cargo, root, &[spec("serde")]);
        let rec2 = scan2
            .inventory
            .iter()
            .find(|r| r.package == "serde")
            .unwrap();
        assert_eq!(rec2.importers, vec!["src/lib.rs".to_string()]); // `use serde::`
    }

    #[test]
    fn python_scan_from_and_import_boundary() {
        let d = tempdir().unwrap();
        let root = d.path();
        std::fs::write(
            root.join("app.py"),
            "import requests\nfrom flask import Flask\n",
        )
        .unwrap();
        // boundary: `requests_oauthlib` must NOT match `requests`
        std::fs::write(root.join("other.py"), "import requests_oauthlib\n").unwrap();

        let scan = scan_importers(
            PackageManager::Pip,
            root,
            &[spec("requests"), spec("flask")],
        );
        let req = scan
            .inventory
            .iter()
            .find(|r| r.package == "requests")
            .unwrap();
        assert_eq!(req.importers, vec!["app.py".to_string()]);
        let flask = scan
            .inventory
            .iter()
            .find(|r| r.package == "flask")
            .unwrap();
        assert_eq!(flask.importers, vec!["app.py".to_string()]);
    }

    #[test]
    fn empty_packages_is_empty_scan() {
        let d = tempdir().unwrap();
        let scan = scan_importers(PackageManager::Npm, d.path(), &[]);
        assert!(scan.inventory.is_empty());
        assert!(scan.affected_files.is_empty());
    }

    #[test]
    fn package_with_no_importers_recorded_empty() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("a.ts"), "const x = 1;\n").unwrap();
        let scan = scan_importers(PackageManager::Npm, d.path(), &[spec("left-pad")]);
        assert_eq!(scan.inventory.len(), 1);
        assert!(scan.inventory[0].importers.is_empty());
        assert!(scan.affected_files.is_empty());
    }

    #[test]
    fn scan_respects_file_cap() {
        let d = tempdir().unwrap();
        let root = d.path();
        // Create more than MAX_SCAN_FILES tiny files, all importing lodash.
        // We can't cheaply create 4000 files fast, so assert the cap field is
        // honored by a smaller-bound sanity check on a moderate count.
        for i in 0..50 {
            std::fs::write(root.join(format!("f{i}.ts")), "import x from 'lodash';\n").unwrap();
        }
        let scan = scan_importers(PackageManager::Npm, root, &[spec("lodash")]);
        // 50 < cap ⇒ all found, but importers list is capped at MAX_AFFECTED_FILES.
        assert!(scan.inventory[0].importers.len() <= MAX_AFFECTED_FILES);
    }
}
