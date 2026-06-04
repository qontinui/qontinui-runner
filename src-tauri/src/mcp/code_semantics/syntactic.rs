//! Shared syntactic helpers for the overlay-predict path of the Rust and Python
//! checkers (Phase A).
//!
//! Neither `cargo check` (no in-memory overlay) nor the post-write mypy path
//! produces a `changed_signatures` / `removed_symbols` diff for the *predict*
//! (overlay) case directly, so both checkers fall back to a small, dependency-light
//! syntactic differ: extract top-level declarations from the on-disk text and the
//! overlay text, diff them by name, and — for removed declarations — search sibling
//! source files for word-boundary references. This module owns the language-neutral
//! pieces (hashing + bounded reference search); each checker supplies its own
//! declaration extractor.

use std::path::Path;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// A top-level (or class-level) declaration extracted syntactically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// The bare symbol name (e.g. `foo`, `MyStruct`).
    pub name: String,
    /// A coarse kind label for the §A `removed_symbols.kind` field
    /// (e.g. `fn`, `struct`, `def`, `class`).
    pub kind: String,
    /// The normalized declaration text used for signature-change detection.
    pub normalized: String,
}

/// Stable opaque hex hash of a signature string (the §A `before_hash`/`after_hash`
/// values are opaque, so any stable hash is acceptable). Truncated to 16 hex
/// chars to keep the wire payload small.
pub fn sig_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A `changed_signatures` entry: a declaration present in both versions whose
/// normalized text changed.
pub fn changed_signatures(on_disk: &[Decl], overlay: &[Decl]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for d in on_disk {
        if let Some(o) = overlay
            .iter()
            .find(|o| o.name == d.name && o.kind == d.kind)
        {
            if o.normalized != d.normalized {
                out.push(serde_json::json!({
                    "name": d.name,
                    "before_hash": sig_hash(&d.normalized),
                    "after_hash": sig_hash(&o.normalized),
                }));
            }
        }
    }
    out
}

/// Names of declarations present on disk but absent in the overlay (candidates
/// for `removed_symbols`).
pub fn removed_decls(on_disk: &[Decl], overlay: &[Decl]) -> Vec<Decl> {
    on_disk
        .iter()
        .filter(|d| !overlay.iter().any(|o| o.name == d.name))
        .cloned()
        .collect()
}

/// Directory names never descended into during the reference search.
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".venv"
            | "venv"
            | "__pycache__"
            | "node_modules"
            | "target"
            | ".mypy_cache"
            | ".pytest_cache"
    ) || name.starts_with("target")
}

/// Search sibling source files under `root` (bounded) for word-boundary
/// references to `name`. Returns up to `max_refs` `{file,line}` entries.
///
/// `exclude` is the path of the edited file itself (its own references don't
/// count as "referenced elsewhere"). `ext` is the source extension to scan
/// (e.g. `"py"` or `"rs"`). At most `file_cap` files are inspected.
pub fn references_in_siblings(
    root: &Path,
    exclude: &Path,
    name: &str,
    ext: &str,
    file_cap: usize,
    max_refs: usize,
) -> Vec<serde_json::Value> {
    let exclude_canon = exclude.canonicalize().ok();
    let mut refs: Vec<serde_json::Value> = Vec::new();
    let mut files_seen = 0usize;

    let walker = WalkDir::new(root).into_iter().filter_entry(|e| {
        if e.file_type().is_dir() {
            // Always descend the root itself.
            if e.depth() == 0 {
                return true;
            }
            return !e.file_name().to_str().map(is_skipped_dir).unwrap_or(false);
        }
        true
    });

    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) != Some(ext) {
            continue;
        }
        if files_seen >= file_cap {
            break;
        }
        files_seen += 1;

        // Skip the edited file itself.
        if let Some(ex) = &exclude_canon {
            if entry.path().canonicalize().ok().as_ref() == Some(ex) {
                continue;
            }
        }

        let text = match std::fs::read_to_string(entry.path()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for (i, line) in text.lines().enumerate() {
            if line_has_word(line, name) {
                refs.push(serde_json::json!({
                    "file": path_str(entry.path()),
                    "line": i + 1,
                }));
                if refs.len() >= max_refs {
                    return refs;
                }
                break; // one ref per file is enough to prove "referenced".
            }
        }
    }
    refs
}

/// Forward-slash-normalized path string for the wire.
fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// True if `line` contains `name` as a whole identifier (ASCII word boundary on
/// both sides). Avoids matching substrings of longer identifiers.
pub fn line_has_word(line: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let bytes = line.as_bytes();
    let nb = name.as_bytes();
    let mut start = 0;
    while let Some(pos) = find_from(bytes, nb, start) {
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after_idx = pos + nb.len();
        let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Build a `removed_symbols` array from removed declarations that are still
/// referenced in sibling files of `root` (the Contradiction / active-negation
/// signal). Declarations with no surviving references are dropped.
pub fn removed_symbols(
    removed: &[Decl],
    root: &Path,
    edited_file: &Path,
    ext: &str,
    file_cap: usize,
    max_refs: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for d in removed {
        let referenced_by =
            references_in_siblings(root, edited_file, &d.name, ext, file_cap, max_refs);
        if !referenced_by.is_empty() {
            out.push(serde_json::json!({
                "name": d.name,
                "kind": d.kind,
                "referenced_by": referenced_by,
            }));
        }
    }
    out
}

/// Read a file to a string, returning empty on error (an unreadable on-disk file
/// just yields no decls — honest, never a panic).
pub fn read_or_empty(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundary_match_is_exact() {
        assert!(line_has_word("    foo()", "foo"));
        assert!(line_has_word("x = foo", "foo"));
        assert!(line_has_word("foo", "foo"));
        // substring of a longer identifier must NOT match
        assert!(!line_has_word("foobar()", "foo"));
        assert!(!line_has_word("my_foo()", "foo"));
        assert!(!line_has_word("foo_bar", "foo"));
    }

    #[test]
    fn sig_hash_is_stable_and_distinct() {
        assert_eq!(sig_hash("a"), sig_hash("a"));
        assert_ne!(sig_hash("a"), sig_hash("b"));
        assert_eq!(sig_hash("a").len(), 16);
    }

    #[test]
    fn changed_signatures_detects_normalized_change() {
        let on_disk = vec![Decl {
            name: "f".into(),
            kind: "fn".into(),
            normalized: "fn f(x: i32)".into(),
        }];
        let overlay = vec![Decl {
            name: "f".into(),
            kind: "fn".into(),
            normalized: "fn f(x: i64)".into(),
        }];
        let changed = changed_signatures(&on_disk, &overlay);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "f");
        assert_ne!(changed[0]["before_hash"], changed[0]["after_hash"]);
    }

    #[test]
    fn changed_signatures_ignores_unchanged() {
        let d = vec![Decl {
            name: "f".into(),
            kind: "fn".into(),
            normalized: "fn f()".into(),
        }];
        assert!(changed_signatures(&d, &d).is_empty());
    }

    #[test]
    fn removed_decls_lists_only_deletions() {
        let on_disk = vec![
            Decl {
                name: "a".into(),
                kind: "fn".into(),
                normalized: "fn a()".into(),
            },
            Decl {
                name: "b".into(),
                kind: "fn".into(),
                normalized: "fn b()".into(),
            },
        ];
        let overlay = vec![Decl {
            name: "a".into(),
            kind: "fn".into(),
            normalized: "fn a()".into(),
        }];
        let removed = removed_decls(&on_disk, &overlay);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].name, "b");
    }

    #[test]
    fn removed_symbols_only_when_referenced_in_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let edited = tmp.path().join("mod.rs");
        std::fs::write(&edited, "// edited\n").unwrap();
        // sibling references `gone` and `kept_private`.
        std::fs::write(tmp.path().join("other.rs"), "fn caller() { gone(); }\n").unwrap();
        let removed = vec![
            Decl {
                name: "gone".into(),
                kind: "fn".into(),
                normalized: "pub fn gone()".into(),
            },
            Decl {
                name: "unused".into(),
                kind: "fn".into(),
                normalized: "pub fn unused()".into(),
            },
        ];
        let syms = removed_symbols(&removed, tmp.path(), &edited, "rs", 2000, 20);
        // only `gone` is referenced elsewhere.
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0]["name"], "gone");
        let refs = syms[0]["referenced_by"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["line"], 1);
    }

    #[test]
    fn reference_search_skips_target_and_self() {
        let tmp = tempfile::tempdir().unwrap();
        let edited = tmp.path().join("mod.rs");
        // self references the name — must NOT count.
        std::fs::write(&edited, "fn gone() {}\n").unwrap();
        // a build-artifact dir that must be skipped.
        let target = tmp.path().join("target").join("debug");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("art.rs"), "gone();\n").unwrap();
        let refs = references_in_siblings(tmp.path(), &edited, "gone", "rs", 2000, 20);
        assert!(refs.is_empty(), "self + target/ refs must be excluded");
    }
}
