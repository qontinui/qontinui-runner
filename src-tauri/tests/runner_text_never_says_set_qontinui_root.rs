//! Regression guard: runner operator-facing text never tells the operator to
//! "Set $QONTINUI_ROOT".
//!
//! ## Why this test exists
//!
//! An installed runner resolves its workspace root from the "Workspace root"
//! setting through the shared `qontinui_types::paths` rungs. An operator of an
//! installed runner never set an environment variable, so a refusal or log line
//! that names `$QONTINUI_ROOT` points at a knob they do not have. qontinui-schemas
//! #269 added one shared sentence for that miss (`WorkspaceRootUnresolved`,
//! rendered by `workspace_paths::require_workspace_root()`), and
//! qontinui-runner #2748 and #2776 moved every runner site onto it or onto
//! `qontinui_types::paths::WORKSPACE_ROOT_SETTING`. This test keeps a NEW site
//! from spelling the variable again.
//!
//! ## What it flags
//!
//! The word `set` / `setting` (any case), then any run of whitespace, quotes,
//! backticks, backslashes or `{`, an optional `the`, an optional `$`, then the
//! name `QONTINUI_ROOT` — or `QONTINUI_ROOT_ENV`, the `qontinui_types::paths`
//! constant a `format!("set ${QONTINUI_ROOT_ENV}")` would interpolate. Before
//! matching, Rust `\`-newline string continuations are joined and every run of
//! whitespace collapses to one space, so a sentence wrapped across source lines
//! (a split Rust literal, Prettier-wrapped JSX text) is still one sentence.
//!
//! Scanned: `src-tauri/src/**` (`.rs`, plus the text assets `.md` `.sh` `.txt`
//! `.py` `.ps1` the runner embeds with `include_str!`) and `src/**` (the
//! frontend: `.ts` `.tsx` `.js` `.jsx` `.json`).
//!
//! Not scanned: comment lines (trimmed start `//`, `/*` or `*` in code files),
//! and in a Rust file everything from a `#[cfg(test)]` that opens a `mod` to the
//! end of the file — test code is where an assertion that the sentence is ABSENT
//! naturally spells it.
//!
//! ## Exemptions
//!
//! - `src-tauri/src/env_agent/apply_repos.rs` `const CLI_UNRESOLVED_ROOT_NOTE`:
//!   a standalone `env apply` exits before the runner publishes its settings door
//!   (`collectors::workspace_root_reads_setting`), so it cannot read the
//!   "Workspace root" setting and `$QONTINUI_ROOT` is the true next action
//!   there. The exemption is checked to still exist and still say it, so a
//!   renamed or reworded constant fails this test instead of silently widening
//!   it.
//! - `src-tauri/src/fleet_commands/`, `src-tauri/src/fleet_skills/`,
//!   `src-tauri/src/fleet_agents/`: the bundled shell skills, commands and
//!   agents, vendored byte-identical from qontinui-claude-config. They are
//!   standalone CLIs that run outside a runner, where the variable is the only
//!   door they have, and they are fixed at their source rather than here.

use std::path::{Path, PathBuf};

/// Relative to `src-tauri/`, so a same-named directory elsewhere is scanned.
const EXEMPT_DIRS: &[&str] = &["src/fleet_commands", "src/fleet_skills", "src/fleet_agents"];
const EXEMPT_FILE: &str = "src-tauri/src/env_agent/apply_repos.rs";
const EXEMPT_CONST: &str = "const CLI_UNRESOLVED_ROOT_NOTE";

const BACKEND_EXTS: &[&str] = &["rs", "md", "sh", "txt", "py", "ps1"];
const FRONTEND_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "json"];
const CODE_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx"];

fn src_tauri() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn walk(dir: &Path, exts: &[&str], skip: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if skip.contains(&path) || entry.file_name() == "node_modules" {
                continue;
            }
            walk(&path, exts, skip, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// Drop comment lines (code files), cut a Rust file's trailing test module, join
/// Rust `\`-newline continuations, and collapse every whitespace run to a space.
fn normalize(text: &str, ext: &str) -> String {
    let code = CODE_EXTS.contains(&ext);
    let mut kept: Vec<&str> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if ext == "rs" && t.starts_with("#[cfg(test)]") {
            let next = lines[i + 1..]
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.is_empty());
            // Only an INLINE module (`mod tests {`): a `#[cfg(test)] mod name;`
            // declaration near the top of a file must not cut the rest of it.
            if next.is_some_and(|l| {
                (l.starts_with("mod ") || l.starts_with("pub mod ")) && l.ends_with('{')
            }) {
                break;
            }
        }
        if code && (t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')) {
            continue;
        }
        kept.push(line);
    }
    let joined = kept.join("\n");
    let mut out = String::with_capacity(joined.len());
    let mut chars = joined.chars().peekable();
    while let Some(c) = chars.next() {
        if ext == "rs" && c == '\\' && chars.peek() == Some(&'\n') {
            chars.next();
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            continue;
        }
        if c.is_whitespace() {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `s[from..]` without the panicking string index (`clippy::string_slice` is
/// denied crate-wide). Every offset passed here sits just past an ASCII match,
/// so it is a char boundary; an out-of-range or mid-char offset yields `""`.
fn tail(s: &str, from: usize) -> &str {
    s.get(from..).unwrap_or("")
}

/// Byte offsets (of the `set`) of every forbidden sentence in normalized text.
fn hits(text: &str) -> Vec<usize> {
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(i) = tail(&lower, from).find("set") {
        let at = from + i;
        from = at + 3;
        if at > 0 && is_ident(b[at - 1]) {
            continue;
        }
        let mut j = at + 3;
        if tail(&lower, j).starts_with("ting") {
            j += 4;
        }
        if j < b.len() && is_ident(b[j]) {
            continue;
        }
        let skip_sep = |mut j: usize| {
            while j < b.len() && matches!(b[j], b' ' | b'"' | b'\'' | b'`' | b'\\' | b'{') {
                j += 1;
            }
            j
        };
        j = skip_sep(j);
        if tail(&lower, j).starts_with("the ") {
            j = skip_sep(j + 4);
        }
        if j < b.len() && b[j] == b'$' {
            j = skip_sep(j + 1);
        }
        for name in ["qontinui_root_env", "qontinui_root"] {
            if tail(&lower, j).starts_with(name) {
                let end = j + name.len();
                if end >= b.len() || !is_ident(b[end]) {
                    found.push(at);
                }
                break;
            }
        }
    }
    found
}

/// The byte range of the exempt constant's declaration, up to its `;`.
fn exempt_range(text: &str) -> Option<std::ops::Range<usize>> {
    let start = text.find(EXEMPT_CONST)?;
    let end = start + tail(text, start).find(';')?;
    Some(start..end)
}

#[test]
fn runner_text_never_says_set_qontinui_root() {
    let tauri = src_tauri();
    let repo = tauri
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf();
    let skip: Vec<PathBuf> = EXEMPT_DIRS.iter().map(|d| tauri.join(d)).collect();
    let mut files = Vec::new();
    walk(&tauri.join("src"), BACKEND_EXTS, &skip, &mut files);
    let backend_count = files.len();
    walk(&repo.join("src"), FRONTEND_EXTS, &[], &mut files);
    assert!(
        backend_count > 100,
        "scanned only {backend_count} backend files — the walk is broken"
    );
    assert!(
        files.len() > backend_count + 100,
        "scanned almost no frontend file — the walk is broken"
    );

    let mut offenders = Vec::new();
    let mut unreadable = Vec::new();
    let mut exemption_used = false;
    for path in &files {
        let rel = path
            .strip_prefix(&repo)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(raw) = std::fs::read_to_string(path) else {
            unreadable.push(rel);
            continue;
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let text = normalize(&raw, ext);
        let exempt = (rel == EXEMPT_FILE).then(|| exempt_range(&text)).flatten();
        for at in hits(&text) {
            if exempt.as_ref().is_some_and(|r| r.contains(&at)) {
                exemption_used = true;
                continue;
            }
            let snippet: String = tail(&text, at).chars().take(100).collect();
            offenders.push(format!("{rel}: {snippet}"));
        }
    }

    assert!(
        unreadable.is_empty(),
        "could not read (non-UTF-8?) so could not check: {unreadable:?}"
    );
    assert!(
        offenders.is_empty(),
        "runner operator text tells the operator to set $QONTINUI_ROOT, a variable an \
         installed runner's operator never set. Render \
         workspace_paths::require_workspace_root()'s shared sentence, or name \
         qontinui_types::paths::WORKSPACE_ROOT_SETTING:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        exemption_used,
        "{EXEMPT_FILE} `{EXEMPT_CONST}` no longer names $QONTINUI_ROOT (or was renamed): \
         drop or update this test's exemption so it cannot silently cover new text"
    );
}

#[test]
fn the_scanner_catches_the_spellings_a_new_site_would_use() {
    let flagged = [
        (
            "let a = \"located — set \\\n     QONTINUI_ROOT or the setting\";",
            "rs",
        ),
        ("format!(\"set ${QONTINUI_ROOT_ENV} first\")", "rs"),
        ("format!(\"set {QONTINUI_ROOT_ENV} first\")", "rs"),
        ("\"Set \\\"$QONTINUI_ROOT\\\" to the folder\"", "rs"),
        ("\"Set `QONTINUI_ROOT` now\"", "rs"),
        ("\"set  the $QONTINUI_ROOT environment variable\"", "rs"),
        ("\"Setting QONTINUI_ROOT helps\"", "rs"),
        (
            "<p>\n          Set\n          $QONTINUI_ROOT to the folder\n        </p>",
            "tsx",
        ),
        ("Set ${QONTINUI_ROOT} now", "md"),
    ];
    for (src, ext) in flagged {
        assert_eq!(
            hits(&normalize(src, ext)).len(),
            1,
            "missed {src:?} ({ext})"
        );
    }
    let clean = [
        ("// set $QONTINUI_ROOT here is a comment", "rs"),
        ("  * set $QONTINUI_ROOT in a JSDoc line", "tsx"),
        ("std::env::set_var(\"QONTINUI_ROOT\", dir);", "rs"),
        ("\"set the \\\"Workspace root\\\" setting\"", "rs"),
        ("\"reset QONTINUI_ROOT\"", "rs"),
        ("\"set QONTINUI_ROOTS\"", "rs"),
        (
            "fn f() {}\n#[cfg(test)]\nmod tests {\n    const X: &str = \"set QONTINUI_ROOT\";\n}",
            "rs",
        ),
    ];
    // A `#[cfg(test)] mod name;` declaration does not hide the code after it.
    let decl = "#[cfg(test)]\nmod tests;\nconst X: &str = \"set QONTINUI_ROOT\";";
    assert_eq!(
        hits(&normalize(decl, "rs")).len(),
        1,
        "a test-module declaration cut the file"
    );
    for (src, ext) in clean {
        assert!(
            hits(&normalize(src, ext)).is_empty(),
            "false positive on {src:?} ({ext})"
        );
    }
}
