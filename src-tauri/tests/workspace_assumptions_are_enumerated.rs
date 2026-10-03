//! The static workspace-assumption sweep: a checked-in ROSTER with one ratchet.
//!
//! Phase 4 of plan
//! `2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports`.
//!
//! ## Why this test exists
//!
//! `capability_manifest::CAPABILITY_SPECS` reports only what someone thought to
//! list — the parent plan's admitted blind spot. This test is the other half: a
//! text scan of `src-tauri/src` (no binary, no model, no scoring — the shape of
//! `tauri_commands_are_registered.rs`) for a FIXED table of literal patterns that
//! each encode one way the runner can silently assume it is running on a
//! developer's workspace rather than on an operator's published install.
//!
//! ## Where the class table comes from
//!
//! The FLEET-NOUN classes are not defined here. They are read, at test time,
//! from `qontinui-schemas/fleet-nouns.toml` — the ONE vocabulary every
//! fleet-noun lint reads — out of the sibling checkout `src-tauri/Cargo.toml`
//! already path-depends on (plan
//! `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists`,
//! Phase A6). Every `[[class]]` in that file is a class here, under the file's
//! id, matched per the file's contract: one comment-stripped LINE at a time
//! (string literals kept), `exclude` SPAN-scoped (it removes only the matches
//! it overlaps, never the line). Today that is `repo_layout`, `dev_ports`,
//! `supervisor_dependency`, `tenant_literal`, `machine_path`,
//! `fleet_host_name`, `fleet_device_uuid` — read the file, not this list.
//!
//! A missing, unparseable, wrong-version or class-less vocabulary is a RED
//! naming the path, never a skip: an absent vocabulary is UNKNOWN, not "no
//! fleet nouns".
//!
//! Runner-local, because the vocabulary deliberately does not carry them:
//!
//! | class id           | pattern family |
//! |--------------------|----------------|
//! | `plans_dir`        | `.plans_dir` / `QONTINUI_PLANS_DIR` reads (`$QONTINUI_PLANS_DIR` is a product constant there) |
//! | `os_bound_tooling` | `powershell` `pwsh` `cmd.exe` `taskkill` `schtasks` `.ps1` literals outside `cfg(windows)`; and a `cfg(windows)` fn with no non-windows sibling of the same name in its file (a platform class, not a fleet noun) |
//!
//! plus runner-local STRUCTURAL refinements ADDED to vocabulary classes
//! (which the file's header permits; see `RUNNER_REFINEMENTS`). The vocabulary
//! is narrower than this roster was before it, and these restore PART of the
//! Rust-shaped coverage it does not carry — `repo_layout` and
//! `supervisor_dependency` only. `tenant_literal` and `machine_path` are
//! deliberately left at the vocabulary's width (the fleet's own tenant/org
//! literals and fleet drive/home paths), so arbitrary UUID literals and generic
//! install paths such as `C:\Program Files\…` are no longer reported. The
//! refinements: to `repo_layout`, a product sibling repo named
//! BARE as a path component in a literal (`workspace.join("qontinui-web")`,
//! `"multistate"`, `"ui-bridge"`); to `supervisor_dependency`, a call to a
//! snake_case helper named for the supervisor (`check_supervisor_available()`)
//! and a bare `9875` port constant in code (literals and chars blanked).
//!
//! Excluded from the scan: `#[cfg(test)]` items (inline modules, fns, `use`s),
//! whole files declared through `#[cfg(test)] mod x;` or carrying
//! `#![cfg(test)]`, everything outside `src/` (so `tests/` and the planted
//! fixtures), and every comment (line, block and doc).
//!
//! ## What fails, and what does not
//!
//! ROSTER STALENESS — a hit with no row in `docs/workspace-assumptions.json`,
//! or a row with no hit — plus the vacuity guards (the `scanned_files` floor,
//! every class matching its own planted fixture, the vocabulary's own examples
//! and look-alikes through this matcher, an unreadable vocabulary), plus the
//! UNREVIEWED RATCHET: the `unreviewed` row count must equal the
//! `unreviewed_ceiling` checked into the dispositions file. Above it, the test
//! names the new rows (a new assumption is looked at once, by its author, at
//! PR time); below it, the test asks for the ceiling to be lowered, so a
//! triage gain is held. Because a NET count cannot see a swap (one assumption
//! removed, a new one added), regeneration also refuses any unreviewed row
//! under a `(class, file, symbol)` key the checked-in roster does not carry,
//! until the dispositions file names that key (reviewed, or an explicit
//! `disposition = "unreviewed"` acknowledgement). The base for that rule is
//! the roster at `merge-base HEAD origin/main` (then `main`), never the
//! working-tree JSON, and it runs in both modes; when that base cannot be read
//! (no git, a shallow clone) the test prints `SWAP RULE UNKNOWN: <why>` and
//! does not fail. Its granularity is the KEY: a swap WITHIN a key the base
//! already carries (one line of a symbol replaced by another) is caught only
//! if it raises the net count. It does **not** fail on `defect` counts (parent
//! Non-goal 2 stands). It prints
//! `unreviewed=<n> unreviewed_ceiling=<n> defect=<n> scanned_files=<n> skipped_files=<n (reason)>`,
//! so "0 hits" is distinguishable from "scanned nothing".
//!
//! ## Dispositions
//!
//! Every row carries a disposition from `docs/workspace-assumptions.dispositions.toml`,
//! keyed by `(class, file, symbol)` — SYMBOL-keyed, so line drift does not orphan
//! them — one of `unreviewed` (the default for a key with no entry),
//! `fallback_correct`, `dev_only_surface`, or `defect(<plan stem>)` — a defect
//! cites the plan that owns its fix. Each entry lists the `reviewed` excerpts it
//! was judged against; a new excerpt under the same key renders `unreviewed`
//! (`NEW since review`) instead of inheriting the verdict. An entry, or a
//! reviewed excerpt, that no longer matches any hit is itself staleness.
//!
//! `repo_layout` rows whose enclosing symbol is named in a `CAPABILITY_SPECS`
//! `anchor` carry that row's id in `capability`; a `repo_layout` row with no
//! capability is visibly a gap in the manifest's roster.
//!
//! ## Regenerating
//!
//! ```text
//! UPDATE_WORKSPACE_ASSUMPTIONS=1 bash <root>/qontinui-claude-config/scripts/cargo-guard.sh \
//!     test --test workspace_assumptions_are_enumerated
//! ```
//!
//! rewrites `docs/workspace-assumptions.{json,md}` from the scan plus the
//! dispositions file. Review the diff like any other: a new row is a new
//! workspace assumption someone just shipped. Regeneration REFUSES when the
//! unreviewed count would exceed `unreviewed_ceiling` (it names the rows), so
//! the ratchet cannot be laundered through a regenerate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

/// The plan whose Phase 4 this roster is. NOT what a `defect(...)` cites: a
/// defect cites the plan that owns its fix.
const PLAN_STEM: &str =
    "2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports";

/// The plan whose Phase A6 re-pointed the class table at `fleet-nouns.toml`
/// and added the `unreviewed_ceiling` ratchet.
const VOCABULARY_PLAN_STEM: &str =
    "2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists";

/// Vacuity floor on the number of `.rs` files actually scanned.
///
/// Derived at authoring (2026-09-22, `784129948`): `git ls-files 'src-tauri/src/*.rs'`
/// listed 1569 files; the sweep scanned 1553 and skipped 16 (13 declared through
/// `#[cfg(test)] mod x;`, 3 carrying `#![cfg(test)]`). The floor sits well under
/// that so ordinary deletions do not
/// trip it, and far above zero so a broken walker, a moved root or an empty dir
/// cannot pass with "0 hits".
const SCANNED_FILES_FLOOR: usize = 1200;

const ROSTER_JSON: &str = "../docs/workspace-assumptions.json";
const ROSTER_MD: &str = "../docs/workspace-assumptions.md";
const DISPOSITIONS_TOML: &str = "../docs/workspace-assumptions.dispositions.toml";
const FIXTURE_DIR: &str = "tests/fixtures/workspace-assumptions";
const UPDATE_ENV: &str = "UPDATE_WORKSPACE_ASSUMPTIONS";
/// The ONE fleet-noun vocabulary, read from the sibling qontinui-schemas
/// checkout this crate already path-depends on (`../../qontinui-schemas/rust`).
const FLEET_NOUNS_TOML: &str = "../../qontinui-schemas/fleet-nouns.toml";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ===========================================================================
// The class table — the fleet-noun VOCABULARY (qontinui-schemas) plus the two
// runner-local classes. Fixed, literal, no scoring.
// ===========================================================================

/// What a pattern is matched against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// The code text of a line with comments removed (string literals kept).
    Code,
    /// The code text of a line with comments removed AND string-literal
    /// contents blanked — code alone (char literals are blanked too).
    Skel,
    /// The raw source text of each string literal, one literal at a time.
    Literal,
}

struct LocalPattern {
    target: Target,
    pattern: &'static str,
    /// SPAN-scoped, like the vocabulary's: removes only the matches it overlaps.
    exclude: Option<&'static str>,
}

struct LocalClass {
    id: &'static str,
    patterns: &'static [LocalPattern],
}

/// The classes that are NOT fleet nouns and so are NOT in `fleet-nouns.toml`
/// (its header says so): `plans_dir` flags code reading the product's own
/// optional `QONTINUI_PLANS_DIR` setting, and `os_bound_tooling` is a PLATFORM
/// class (`powershell.exe` is correct under `cfg(windows)`). Both stay here.
const RUNNER_LOCAL_CLASSES: &[LocalClass] = &[
    LocalClass {
        id: "plans_dir",
        patterns: &[LocalPattern {
            target: Target::Code,
            pattern: r"\.plans_dir\b|\bQONTINUI_PLANS_DIR\b",
            exclude: None,
        }],
    },
    LocalClass {
        id: OS_BOUND,
        // The second half of this class — a `cfg(windows)` fn with no
        // non-windows sibling — is structural, see `scan_source`.
        patterns: &[LocalPattern {
            target: Target::Literal,
            pattern: r"(?i)\b(?:powershell|pwsh|taskkill|schtasks)\b|\bcmd\.exe\b|\.ps1\b",
            exclude: None,
        }],
    },
];

/// A URL, or a GitHub `qontinui/<repo>` slug at the start of a literal: a
/// REFERENCE to a repo, not a layout. Span-scoped, so it removes only a repo
/// name inside the URL/slug.
const REPO_REFERENCE_EXCLUDE: &str =
    r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s"]*|^qontinui/[A-Za-z0-9_.-]+"#;

/// Runner-local STRUCTURAL refinements added to a vocabulary class — which the
/// vocabulary's header permits ("a consumer may keep such refinements locally
/// but may not redefine a class id"). A refinement ADDS matches to a class the
/// vocabulary owns; naming a class the vocabulary does not carry is a red.
const RUNNER_REFINEMENTS: &[(&str, LocalPattern)] = &[
    // The vocabulary's repo_layout names a sibling only RELATIVELY
    // (`../qontinui-web`) or as a fleet-only repo (`qontinui-dev-notes`); a
    // product repo named BARE as a path component — `workspace.join("qontinui-web")`,
    // `"**/qontinui-web/**"`, `const WEB_REPO_DIR = "qontinui-web"`,
    // `["qontinui", "multistate"]` — is the same workspace-layout assumption in
    // Rust's spelling. Restores the roster's pre-vocabulary repo_layout coverage.
    (
        "repo_layout",
        LocalPattern {
            target: Target::Literal,
            pattern: r"(?:^|[/\\])(?:qontinui-(?:claude-config|dev-notes|schemas|web|coord|supervisor|mcp|inspect|stack|mobile|devtools|root)|multistate)(?:[/\\]|$)",
            exclude: Some(REPO_REFERENCE_EXCLUDE),
        },
    ),
    // The `ui-bridge` sibling repo — as a bare `.join("ui-bridge")` (or any
    // literal that IS the name), a `../ui-bridge` relative path, or its
    // `ui-bridge/packages` tree. NOT every `ui-bridge` path segment: the
    // runner's own `/ui-bridge/...` HTTP routes are not layout.
    (
        "repo_layout",
        LocalPattern {
            target: Target::Literal,
            pattern: r"^ui-bridge$|\.\.[/\\]ui-bridge\b|(?:^|[/\\])ui-bridge[/\\]packages\b",
            exclude: Some(REPO_REFERENCE_EXCLUDE),
        },
    ),
    // A CALL to any snake_case helper named for the supervisor —
    // `check_supervisor_available()`, `supervisor_injected_reading()` — or one
    // passed by path as an argument
    // (`spawn_blocking(auto_continue::check_supervisor_available)`). The
    // in-process `worker_supervisor` / `task_supervisor` MODULES are not the dev
    // supervisor, and a module path is followed by `::`, never by `,` or `)`.
    // The vocabulary's accessor arm names only url/port/host/client-shaped
    // helpers; the supervisor-PRESENCE probe is the call this catches.
    (
        "supervisor_dependency",
        LocalPattern {
            target: Target::Code,
            pattern: r"\b[a-z0-9_]*(?:_supervisor|supervisor_)[a-z0-9_]*\s*\(|::[a-z0-9_]*(?:_supervisor|supervisor_)[a-z0-9_]*\s*[,)]",
            exclude: None,
        },
    ),
    // A bare `9875` port CONSTANT in code (`("supervisor", 9875)`, `9875u16,`).
    // The vocabulary deliberately needs a port context in TEXT (a bare 9875
    // there hits `PR #9875`); in code with literals and chars blanked, a bare
    // integer 9875 is a port — with any integer suffix (`9875u16`, `9875_i32`)
    // and as a range end (`0..9875`, `0..=9875`). `#`, `:`, a lone `.` before it
    // are excluded (`#9875`, `:9875` — already the vocabulary's — `1.9875`), and
    // so is a fractional tail (`9875.0`).
    (
        "supervisor_dependency",
        LocalPattern {
            target: Target::Skel,
            pattern: r"(?:^|[^0-9A-Za-z_#:.]|\.\.=?)9875(?:_?[ui](?:8|16|32|64|128|size))?(?:[^0-9A-Za-z_.]|\.[^0-9]|$)",
            exclude: None,
        },
    ),
];

const OS_BOUND: &str = "os_bound_tooling";

/// `fleet-nouns.toml`, parsed. Only the fields this consumer reads; the file's
/// other fields (`why`, `literals`, `portability_probes`, …) are ignored.
#[derive(Debug, Deserialize)]
struct Vocabulary {
    version: u32,
    #[serde(default)]
    lookalikes: Vec<String>,
    #[serde(default, rename = "class")]
    classes: Vec<VocabClass>,
    #[serde(default, rename = "product_constant")]
    product_constants: Vec<ProductConstant>,
}

#[derive(Debug, Deserialize)]
struct VocabClass {
    id: String,
    pattern: String,
    #[serde(default)]
    exclude: Option<String>,
    #[serde(default)]
    examples: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ProductConstant {
    value: String,
}

/// The one vocabulary version this matcher implements.
const VOCABULARY_VERSION: u32 = 1;

fn vocabulary_path() -> PathBuf {
    crate_root().join(FLEET_NOUNS_TOML)
}

/// Read and parse the vocabulary. EVERY failure is an `Err` naming the path —
/// an absent file is UNKNOWN, never "no fleet nouns", so no caller may turn it
/// into a skip.
fn load_vocabulary(path: &Path) -> Result<Vocabulary, String> {
    let text = fs::read_to_string(path).map_err(|e| {
        format!(
            "FLEET-NOUN VOCABULARY UNREADABLE: {}: {e}. The workspace-assumption classes are \
             read from the sibling qontinui-schemas checkout (src-tauri/Cargo.toml already \
             path-depends on it); check it out beside this repo at or past schemas 02fdbb97. \
             An absent vocabulary is UNKNOWN, never \"no fleet nouns\" — this is not a skip.",
            path.display()
        )
    })?;
    let vocab: Vocabulary = toml::from_str(&text)
        .map_err(|e| format!("FLEET-NOUN VOCABULARY UNPARSEABLE: {}: {e}", path.display()))?;
    if vocab.version != VOCABULARY_VERSION {
        return Err(format!(
            "FLEET-NOUN VOCABULARY VERSION {} at {} — this matcher implements version \
             {VOCABULARY_VERSION}; re-read the file's HOW A CONSUMER MATCHES contract and update \
             the runner",
            vocab.version,
            path.display()
        ));
    }
    if vocab.classes.is_empty() {
        return Err(format!(
            "FLEET-NOUN VOCABULARY EMPTY: {} declares no [[class]] — a vocabulary with no \
             classes would scan nothing and pass",
            path.display()
        ));
    }
    Ok(vocab)
}

struct CompiledPattern {
    target: Target,
    re: Regex,
    /// SPAN-scoped: a match of `re` is discarded when its span overlaps a
    /// match of this in the same text (the vocabulary's contract). It never
    /// hides a separate, non-overlapping match elsewhere on the line.
    exclude: Option<Regex>,
    /// `true` for a runner-local pattern (not from the vocabulary).
    local: bool,
}

struct CompiledClass {
    id: String,
    patterns: Vec<CompiledPattern>,
}

/// The first surviving match of `re` in `text` (its start offset), after the
/// span-scoped `exclude`. Matches are non-overlapping, left to right.
fn first_surviving_match(re: &Regex, exclude: Option<&Regex>, text: &str) -> Option<usize> {
    let excluded: Vec<(usize, usize)> = exclude
        .map(|e| e.find_iter(text).map(|m| (m.start(), m.end())).collect())
        .unwrap_or_default();
    re.find_iter(text)
        .find(|m| !excluded.iter().any(|&(s, e)| m.start() < e && s < m.end()))
        .map(|m| m.start())
}

/// Does `text` (one line, no terminator) hit `class` through its VOCABULARY
/// patterns alone? The contract check in `vocabulary_contract_holds_under_this_matcher`.
fn vocabulary_hits(class: &CompiledClass, text: &str) -> bool {
    class
        .patterns
        .iter()
        .filter(|p| !p.local)
        .any(|p| first_surviving_match(&p.re, p.exclude.as_ref(), text).is_some())
}

fn compile_local(p: &LocalPattern, id: &str) -> Result<CompiledPattern, String> {
    Ok(CompiledPattern {
        target: p.target,
        re: Regex::new(p.pattern).map_err(|e| format!("runner-local `{id}` pattern: {e}"))?,
        exclude: p
            .exclude
            .map(Regex::new)
            .transpose()
            .map_err(|e| format!("runner-local `{id}` exclude: {e}"))?,
        local: true,
    })
}

/// The full class table: every vocabulary class (in file order, matched
/// against each comment-stripped LINE, as the vocabulary's contract says),
/// plus the runner-local refinements and classes.
fn compile_classes_from(path: &Path) -> Result<Vec<CompiledClass>, String> {
    let vocab = load_vocabulary(path)?;
    let mut out: Vec<CompiledClass> = Vec::new();
    for c in &vocab.classes {
        if out.iter().any(|o| o.id == c.id) {
            return Err(format!("{}: duplicate class id `{}`", path.display(), c.id));
        }
        if RUNNER_LOCAL_CLASSES.iter().any(|l| l.id == c.id) {
            return Err(format!(
                "{}: class `{}` is runner-local here and may not also come from the vocabulary",
                path.display(),
                c.id
            ));
        }
        let re = Regex::new(&c.pattern)
            .map_err(|e| format!("{}: class `{}` pattern: {e}", path.display(), c.id))?;
        let exclude = c
            .exclude
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| format!("{}: class `{}` exclude: {e}", path.display(), c.id))?;
        out.push(CompiledClass {
            id: c.id.clone(),
            patterns: vec![CompiledPattern {
                target: Target::Code,
                re,
                exclude,
                local: false,
            }],
        });
    }
    for (id, p) in RUNNER_REFINEMENTS {
        let class = out.iter_mut().find(|c| c.id == *id).ok_or_else(|| {
            format!(
                "runner refinement for `{id}`, but {} carries no such class — a refinement may \
                 only ADD to a vocabulary class",
                path.display()
            )
        })?;
        class.patterns.push(compile_local(p, id)?);
    }
    for l in RUNNER_LOCAL_CLASSES {
        out.push(CompiledClass {
            id: l.id.to_string(),
            patterns: l
                .patterns
                .iter()
                .map(|p| compile_local(p, l.id))
                .collect::<Result<_, _>>()?,
        });
    }
    Ok(out)
}

/// The class table from the sibling vocabulary — PANICS (red, naming the path)
/// when it cannot be read. Never a skip.
fn compile_classes() -> Vec<CompiledClass> {
    compile_classes_from(&vocabulary_path()).unwrap_or_else(|e| panic!("{e}"))
}

// ===========================================================================
// Lexer — comments out, strings kept, literals extracted.
// ===========================================================================

#[derive(Default, Clone)]
struct Line {
    /// Comments removed; string literals kept verbatim.
    code: String,
    /// Comments removed; string-literal CONTENTS blanked. Structure is read here.
    skel: String,
    /// Raw source text of every string literal that STARTS on this line.
    literals: Vec<String>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lex(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().collect();
    let mut lines = vec![Line::default()];
    let n = chars.len();
    let mut i = 0;

    macro_rules! cur {
        () => {
            lines.last_mut().expect("at least one line")
        };
    }

    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if c == '\n' {
            lines.push(Line::default());
            i += 1;
            continue;
        }
        // Line comment (incl. doc comments).
        if c == '/' && next == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (nested).
        if c == '/' && next == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '\n' {
                    lines.push(Line::default());
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 1;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        // Raw string: r"..." / r#"..."# (optionally b-prefixed; the `b` was
        // already emitted as an ordinary char).
        let prev_ident = i > 0
            && is_ident(chars[i - 1])
            && !(chars[i - 1] == 'b' && (i < 2 || !is_ident(chars[i - 2])));
        if c == 'r' && !prev_ident {
            let mut j = i + 1;
            while j < n && chars[j] == '#' {
                j += 1;
            }
            if j < n && chars[j] == '"' {
                let hashes = j - (i + 1);
                let start_line = lines.len() - 1;
                for &ch in &chars[i..=j] {
                    cur!().code.push(ch);
                    cur!().skel.push(ch);
                }
                i = j + 1;
                let mut buf = String::new();
                loop {
                    if i >= n {
                        break;
                    }
                    let ch = chars[i];
                    if ch == '"' && (0..hashes).all(|k| chars.get(i + 1 + k) == Some(&'#')) {
                        for &cc in &chars[i..i + 1 + hashes] {
                            cur!().code.push(cc);
                            cur!().skel.push(cc);
                        }
                        i += 1 + hashes;
                        break;
                    }
                    if ch == '\n' {
                        lines.push(Line::default());
                    } else {
                        cur!().code.push(ch);
                        cur!().skel.push(' ');
                    }
                    buf.push(ch);
                    i += 1;
                }
                lines[start_line].literals.push(buf);
                continue;
            }
        }
        // Ordinary string.
        if c == '"' {
            let start_line = lines.len() - 1;
            cur!().code.push('"');
            cur!().skel.push('"');
            i += 1;
            let mut buf = String::new();
            while i < n {
                let ch = chars[i];
                if ch == '\\' && i + 1 < n {
                    let esc = chars[i + 1];
                    buf.push(ch);
                    buf.push(esc);
                    cur!().code.push(ch);
                    cur!().skel.push(' ');
                    if esc == '\n' {
                        lines.push(Line::default());
                    } else {
                        cur!().code.push(esc);
                        cur!().skel.push(' ');
                    }
                    i += 2;
                    continue;
                }
                if ch == '"' {
                    cur!().code.push('"');
                    cur!().skel.push('"');
                    i += 1;
                    break;
                }
                if ch == '\n' {
                    lines.push(Line::default());
                } else {
                    cur!().code.push(ch);
                    cur!().skel.push(' ');
                }
                buf.push(ch);
                i += 1;
            }
            lines[start_line].literals.push(buf);
            continue;
        }
        // Char literal vs lifetime.
        if c == '\'' {
            let is_char = match next {
                Some('\\') => true,
                Some(_) => chars.get(i + 2) == Some(&'\''),
                None => false,
            };
            if is_char {
                let mut j = i + 1;
                if chars[j] == '\\' {
                    j += 2;
                }
                while j < n && chars[j] != '\'' && chars[j] != '\n' {
                    j += 1;
                }
                // Blank the whole char literal in skel (it may be '{' or '"').
                for &ch in &chars[i..=j.min(n - 1)] {
                    cur!().code.push(ch);
                    cur!().skel.push(if ch == '\'' { '\'' } else { ' ' });
                }
                i = j + 1;
                continue;
            }
        }
        cur!().code.push(c);
        cur!().skel.push(c);
        i += 1;
    }
    join_attribute_lines(lines)
}

/// Fold an attribute spanning several lines (`#[cfg(all(\n test,\n …))]`) into
/// ONE logical line, so everything downstream sees the whole predicate. Line
/// numbers are never reported, so merging lines costs nothing.
fn join_attribute_lines(lines: Vec<Line>) -> Vec<Line> {
    let balance = |s: &str| {
        s.chars().fold(0i32, |b, c| match c {
            '[' => b + 1,
            ']' => b - 1,
            _ => b,
        })
    };
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut iter = lines.into_iter();
    while let Some(mut line) = iter.next() {
        let t = line.skel.trim_start();
        if t.starts_with("#[") || t.starts_with("#![") {
            let mut open = balance(&line.skel);
            let mut joined = 0;
            while open > 0 && joined < 40 {
                let Some(next) = iter.next() else { break };
                open += balance(&next.skel);
                line.code.push(' ');
                line.code.push_str(&next.code);
                line.skel.push(' ');
                line.skel.push_str(&next.skel);
                line.literals.extend(next.literals);
                joined += 1;
            }
        }
        out.push(line);
    }
    out
}

// ===========================================================================
// Structure — enclosing symbol, cfg(test) / cfg(windows) regions.
// ===========================================================================

/// A parsed `cfg(...)` predicate.
#[derive(Debug)]
enum Pred {
    Atom(String, Option<String>),
    All(Vec<Pred>),
    Any(Vec<Pred>),
    Not(Box<Pred>),
}

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    LParen,
    RParen,
    Comma,
    Eq,
}

fn tokenize_cfg(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            ',' => out.push(Tok::Comma),
            '=' => out.push(Tok::Eq),
            '"' => {
                let mut j = i + 1;
                let mut v = String::new();
                while j < chars.len() && chars[j] != '"' {
                    v.push(chars[j]);
                    j += 1;
                }
                out.push(Tok::Str(v));
                i = j;
            }
            c if is_ident(c) => {
                let mut v = String::new();
                while i < chars.len() && is_ident(chars[i]) {
                    v.push(chars[i]);
                    i += 1;
                }
                out.push(Tok::Ident(v));
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn parse_pred(toks: &[Tok], i: &mut usize) -> Option<Pred> {
    let Some(Tok::Ident(name)) = toks.get(*i) else {
        return None;
    };
    *i += 1;
    match toks.get(*i) {
        Some(Tok::LParen) if matches!(name.as_str(), "all" | "any" | "not") => {
            *i += 1;
            let mut items = Vec::new();
            while !matches!(toks.get(*i), Some(Tok::RParen) | None) {
                items.push(parse_pred(toks, i)?);
                if toks.get(*i) == Some(&Tok::Comma) {
                    *i += 1;
                }
            }
            *i += 1; // `)`
            Some(match name.as_str() {
                "all" => Pred::All(items),
                "any" => Pred::Any(items),
                _ => Pred::Not(Box::new(items.into_iter().next()?)),
            })
        }
        Some(Tok::Eq) => {
            *i += 1;
            let Some(Tok::Str(v)) = toks.get(*i) else {
                return None;
            };
            *i += 1;
            Some(Pred::Atom(name.clone(), Some(v.clone())))
        }
        _ => Some(Pred::Atom(name.clone(), None)),
    }
}

/// The predicate of a `#[cfg(...)]` / `#![cfg(...)]` attribute (not `cfg_attr`).
fn cfg_pred(attr: &str) -> Option<Pred> {
    let t = attr.trim();
    let inner = t
        .strip_prefix("#![")
        .or_else(|| t.strip_prefix("#["))?
        .trim_start();
    let rest = inner.strip_prefix("cfg")?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let toks = tokenize_cfg(rest);
    parse_pred(&toks, &mut 0)
}

/// Does every configuration satisfying `p` also satisfy `atom`? `test` under
/// `all(...)` counts, under `not(...)` does not, under `any(...)` only when
/// every branch requires it.
fn requires(p: &Pred, atom: &dyn Fn(&str, Option<&str>) -> bool) -> bool {
    match p {
        Pred::Atom(n, v) => atom(n, v.as_deref()),
        Pred::All(xs) => xs.iter().any(|x| requires(x, atom)),
        Pred::Any(xs) => !xs.is_empty() && xs.iter().all(|x| requires(x, atom)),
        Pred::Not(_) => false,
    }
}

fn is_test_atom(n: &str, v: Option<&str>) -> bool {
    n == "test" && v.is_none()
}

fn is_windows_atom(n: &str, v: Option<&str>) -> bool {
    (n == "windows" && v.is_none())
        || (matches!(n, "target_os" | "target_family") && v == Some("windows"))
}

fn is_unixish_atom(n: &str, v: Option<&str>) -> bool {
    (n == "unix" && v.is_none())
        || (n == "target_family" && v == Some("unix"))
        || (n == "target_os"
            && matches!(
                v,
                Some(
                    "linux"
                        | "macos"
                        | "android"
                        | "ios"
                        | "freebsd"
                        | "openbsd"
                        | "netbsd"
                        | "dragonfly"
                )
            ))
}

/// Does `p` exclude Windows (`not(windows)`, `unix`, `target_os = "linux"`, …)?
fn excludes_windows(p: &Pred) -> bool {
    match p {
        Pred::Atom(n, v) => is_unixish_atom(n, v.as_deref()),
        Pred::Not(inner) => requires(inner, &is_windows_atom),
        Pred::All(xs) => xs.iter().any(excludes_windows),
        Pred::Any(xs) => !xs.is_empty() && xs.iter().all(excludes_windows),
    }
}

/// What a set of attributes on ONE item says about when it compiles.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct CfgFlags {
    test: bool,
    windows: bool,
    not_windows: bool,
}

impl CfgFlags {
    fn of(attrs: &[String]) -> Self {
        let mut f = CfgFlags::default();
        for a in attrs {
            if let Some(p) = cfg_pred(a) {
                f.test |= requires(&p, &is_test_atom);
                f.windows |= requires(&p, &is_windows_atom);
                f.not_windows |= excludes_windows(&p);
            }
        }
        f
    }

    fn any(self) -> bool {
        self.test || self.windows || self.not_windows
    }
}

/// Split leading outer attributes off a trimmed code line. Returns the
/// attributes and the remaining item text.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn split_attrs(line: &str) -> (Vec<String>, String) {
    let mut attrs = Vec::new();
    let mut rest = line.trim();
    while rest.starts_with("#[") {
        let mut depth = 0;
        let mut end = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            attrs.push(rest.to_string());
            return (attrs, String::new());
        };
        attrs.push(rest[..=end].to_string());
        rest = rest[end + 1..].trim_start();
    }
    (attrs, rest.to_string())
}

/// Does the text after the attributes start an ITEM (ends in `;` or a `{}`
/// body) rather than a struct field, enum variant or match arm (ends in `,`
/// or at the enclosing `}`)?
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn starts_item(rest: &str) -> bool {
    let mut r = rest.trim_start();
    if let Some(after) = r.strip_prefix("pub") {
        let after = after.trim_start();
        r = if after.starts_with('(') {
            after
                .find(')')
                .map_or(after, |i| after[i + 1..].trim_start())
        } else {
            after
        };
    }
    let word: String = r
        .chars()
        .take_while(|c| is_ident(*c) || *c == '!')
        .collect();
    matches!(
        word.as_str(),
        "fn" | "impl"
            | "mod"
            | "struct"
            | "enum"
            | "trait"
            | "use"
            | "const"
            | "static"
            | "type"
            | "extern"
            | "unsafe"
            | "async"
            | "union"
            | "macro_rules!"
            | "let"
    )
}

/// One cfg'd item currently open. Regions nest (a `cfg(test)` module inside a
/// `cfg(windows)` one), so they live on a stack.
struct Region {
    flags: CfgFlags,
    start_depth: i32,
    start_bracket: i32,
    entered: bool,
    /// A field / variant / match arm: it ends at the next `,` on its own level.
    comma_closes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    class: String,
    file: String,
    symbol: String,
    excerpt: String,
}

struct Structure {
    fn_re: Regex,
    impl_re: Regex,
    const_re: Regex,
    item_re: Regex,
    extern_block_re: Regex,
    mod_decl_re: Regex,
    path_attr_re: Regex,
}

impl Structure {
    fn new() -> Self {
        Self {
            fn_re: Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            impl_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(impl|trait)\b(.*)$").unwrap(),
            const_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(const|static)\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            item_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(struct|enum|union|type|trait|mod|macro_rules!)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
            extern_block_re: Regex::new(r#"^(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?extern\b[^;]*$"#).unwrap(),
            mod_decl_re: Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap(),
            path_attr_re: Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).unwrap(),
        }
    }

    /// The type name an `impl`/`trait` header names, e.g.
    /// `impl<T> Foo for Bar<T> where …` → `Bar`.
    #[expect(
        clippy::string_slice,
        reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
    )]
    fn impl_name(&self, skel_trimmed: &str) -> Option<String> {
        let caps = self.impl_re.captures(skel_trimmed)?;
        let mut rest = caps.get(2)?.as_str().trim_start().to_string();
        if caps.get(1)?.as_str() == "impl" && rest.starts_with('<') {
            let mut depth = 0;
            let mut cut = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            cut = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = rest[cut..].to_string();
        }
        if let Some(idx) = rest.rfind(" for ") {
            rest = rest[idx + 5..].to_string();
        }
        let head: String = rest
            .trim_start()
            .trim_start_matches('&')
            .trim_start_matches("dyn ")
            .chars()
            .take_while(|c| is_ident(*c) || *c == ':')
            .collect();
        let name = head.rsplit("::").next().unwrap_or("").to_string();
        (!name.is_empty()).then_some(name)
    }

    /// `(mod name, #[path] override)` for every `mod x;` declaration whose cfg
    /// satisfies `want`.
    fn cfg_mod_decls(
        &self,
        lines: &[Line],
        want: fn(CfgFlags) -> bool,
    ) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        for line in lines {
            let (attrs, rest) = if line.skel.trim_start().starts_with("#[") {
                split_attrs(&line.code)
            } else {
                (Vec::new(), line.code.trim().to_string())
            };
            pending.extend(attrs);
            if rest.is_empty() {
                continue;
            }
            if want(CfgFlags::of(&pending)) {
                if let Some(c) = self.mod_decl_re.captures(&rest) {
                    let path = pending
                        .iter()
                        .find_map(|a| self.path_attr_re.captures(a).map(|p| p[1].to_string()));
                    out.push((c[1].to_string(), path));
                }
            }
            pending.clear();
        }
        out
    }

    /// An inner `#![cfg(...)]` on the file itself.
    fn inner_cfg(&self, lines: &[Line], want: fn(CfgFlags) -> bool) -> bool {
        lines.iter().any(|l| {
            // Skeleton decides it IS an attribute; `code` supplies its text.
            l.skel.trim_start().starts_with("#![")
                && want(CfgFlags::of(&[l.code.trim_start().to_string()]))
        })
    }
}

fn excerpt_of(code: &str) -> String {
    let collapsed = code.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 140 {
        let cut: String = collapsed.chars().take(137).collect();
        format!("{cut}...")
    } else {
        collapsed
    }
}

/// Scan one source file's text. `whole_file_windows` suppresses the literal
/// half of `os_bound_tooling` for a file that only compiles on Windows.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn scan_source(
    st: &Structure,
    classes: &[CompiledClass],
    rel: &str,
    src: &str,
    whole_file_windows: bool,
) -> Vec<Hit> {
    let lines = lex(src);
    let mut hits = Vec::new();

    let mut depth: i32 = 0;
    let mut bracket: i32 = 0;
    // (name, depth at which its body opened)
    let mut fn_stack: Vec<(String, i32)> = Vec::new();
    let mut impl_stack: Vec<(String, i32)> = Vec::new();
    // Depth at which each open `extern "…" {` block's body started.
    let mut extern_stack: Vec<i32> = Vec::new();
    let mut pending_extern = false;
    let mut pending_fn: Option<String> = None;
    let mut pending_impl: Option<String> = None;
    let mut const_ctx: Option<(String, i32)> = None;
    let mut pending_attrs: Vec<String> = Vec::new();
    let mut regions: Vec<Region> = Vec::new();
    // cfg(windows) / non-windows fn names, for the sibling rule.
    let mut windows_fns: Vec<(String, String)> = Vec::new(); // (qualified symbol, bare name)
    let mut other_os_fns: BTreeSet<String> = BTreeSet::new();

    for line in &lines {
        let skel_t = line.skel.trim();

        // ---- attributes / item starts --------------------------------------
        // Attributes are read INSIDE open regions too: a `cfg(test)` module
        // inside a `cfg(windows)` one is its own, nested region.
        let mut line_fn_name: Option<String> =
            st.fn_re.captures(&line.skel).map(|c| c[1].to_string());
        {
            // Gate on the SKELETON: a template string whose line begins `#[cfg(test)]`
            // is data, not an attribute, and must not open a region (43 such
            // attribute-shaped lines live inside string literals today). The
            // attribute TEXT still comes from `code`, which keeps the literals a
            // predicate needs (`target_os = "windows"`).
            let is_attr_line = line.skel.trim_start().starts_with("#[");
            let (attrs, rest) = if is_attr_line {
                split_attrs(&line.code)
            } else {
                (Vec::new(), line.code.trim().to_string())
            };
            let had_attrs = !attrs.is_empty();
            pending_attrs.extend(attrs);
            if !rest.is_empty() && !pending_attrs.is_empty() {
                let flags = CfgFlags::of(&pending_attrs);
                if flags.any() {
                    regions.push(Region {
                        flags,
                        start_depth: depth,
                        start_bracket: bracket,
                        entered: false,
                        comma_closes: !starts_item(&rest),
                    });
                }
                pending_attrs.clear();
            } else if !rest.is_empty() || (!had_attrs && !skel_t.is_empty()) {
                pending_attrs.clear();
            }
        }

        let in_test = regions.iter().any(|r| r.flags.test);
        let in_windows = whole_file_windows || regions.iter().any(|r| r.flags.windows);

        // ---- the windows-sibling bookkeeping --------------------------------
        // A fn is platform-bound when an open region says so and the fn sits at
        // item level inside it (not a local fn nested in another fn's body).
        if let (Some(name), false) = (&line_fn_name, in_test) {
            let item_level = |r: &Region| fn_stack.last().is_none_or(|(_, d)| *d <= r.start_depth);
            if !extern_stack.is_empty() {
                // An `extern "…" { fn …; }` DECLARATION. It has no body, so it
                // can never have a non-windows sibling — rostering it would be
                // a row no code change could ever resolve. Tracked as its own
                // scope rather than off the cfg attribute that opened the
                // region: the `extern` block is usually bare INSIDE an already
                // cfg'd module (`wedge_diagnostics::windows_thread_census`).
                //
                // Bound, stated rather than implied: the scope opens at the
                // block's `{`, and a line is scanned before its own braces are
                // counted — so a block whose HEADER LINE carries a declaration,
                // the one-line `extern "system" { fn OneLiner() -> u32; }`
                // above all, would still roster it. What is covered is a block
                // whose header line carries no `fn`, which is all seven extern
                // headers under `src/` today.
            } else if regions.iter().any(|r| r.flags.not_windows && item_level(r)) {
                other_os_fns.insert(name.clone());
            } else if regions
                .iter()
                .any(|r| r.flags.windows && !r.flags.test && item_level(r))
            {
                let q = match impl_stack.last() {
                    Some((t, _)) => format!("{t}::{name}"),
                    None => name.clone(),
                };
                windows_fns.push((q, name.clone()));
            }
        }

        // ---- symbol for hits on this line ----------------------------------
        if fn_stack.is_empty() && const_ctx.is_none() {
            if let Some(c) = st.const_re.captures(skel_t) {
                if !matches!(&c[2], "fn" | "unsafe" | "async" | "extern") {
                    const_ctx = Some((format!("{} {}", &c[1], &c[2]), depth));
                }
            }
        }
        let symbol = {
            let sig_fn = line_fn_name.as_ref().or(pending_fn.as_ref());
            let base = if let Some(name) = sig_fn {
                Some(name.clone())
            } else if let Some((name, _)) = fn_stack.last() {
                Some(name.clone())
            } else {
                const_ctx.as_ref().map(|(n, _)| n.clone())
            };
            // The impl enclosing the fn (not one opened inside it).
            let fn_depth = fn_stack.last().map_or(i32::MAX, |(_, d)| *d);
            let owner = if sig_fn.is_some() {
                impl_stack.last()
            } else {
                impl_stack.iter().rev().find(|(_, d)| *d <= fn_depth)
            };
            match (base, owner) {
                (Some(b), Some((t, _))) => format!("{t}::{b}"),
                (Some(b), None) => b,
                (None, Some((t, _))) => format!("{t}::<impl>"),
                (None, None) => {
                    if let Some(c) = st.item_re.captures(skel_t) {
                        format!("{} {}", &c[1], &c[2])
                    } else if let Some(t) = st.impl_name(skel_t) {
                        format!("{t}::<impl>")
                    } else {
                        "<module>".to_string()
                    }
                }
            }
        };

        // ---- hits ----------------------------------------------------------
        if !in_test {
            for class in classes {
                if class.id == OS_BOUND && in_windows {
                    continue;
                }
                let mut excerpt: Option<String> = None;
                for p in &class.patterns {
                    let (re, exclude) = (&p.re, p.exclude.as_ref());
                    excerpt = match p.target {
                        // One line, terminator stripped (the vocabulary's
                        // contract: a lone `\r` means different things per engine).
                        Target::Code => {
                            first_surviving_match(re, exclude, line.code.trim_end_matches('\r'))
                                .map(|_| excerpt_of(&line.code))
                        }
                        Target::Skel => {
                            first_surviving_match(re, exclude, line.skel.trim_end_matches('\r'))
                                .map(|_| excerpt_of(&line.code))
                        }
                        Target::Literal => line.literals.iter().find_map(|lit| {
                            let m_start = first_surviving_match(re, exclude, lit)?;
                            // A multi-line literal: quote the literal's own line
                            // holding the match, not the line that opens it.
                            Some(if lit.contains('\n') {
                                let start = lit[..m_start].rfind('\n').map_or(0, |i| i + 1);
                                let end =
                                    lit[m_start..].find('\n').map_or(lit.len(), |i| m_start + i);
                                excerpt_of(&lit[start..end])
                            } else {
                                excerpt_of(&line.code)
                            })
                        }),
                    };
                    if excerpt.is_some() {
                        break;
                    }
                }
                if let Some(excerpt) = excerpt {
                    hits.push(Hit {
                        class: class.id.to_string(),
                        file: rel.to_string(),
                        symbol: symbol.clone(),
                        excerpt,
                    });
                }
            }
        }

        // ---- structure -----------------------------------------------------
        // Read BEFORE the `take()` below moves the name out: `line_fn_name` is
        // `None` afterwards whatever the line held, so testing it after the
        // move would make the `extern "C" fn` guard below a no-op.
        let had_fn_on_line = line_fn_name.is_some();
        if let Some(name) = line_fn_name.take() {
            pending_fn = Some(name);
        }
        if let Some(name) = st.impl_name(skel_t) {
            pending_impl = Some(name);
        }
        // `extern "C" fn foo() {}` is a FN, not an FFI block: its `{` belongs to
        // the fn. Letting it arm `pending_extern` would leave that arming alive
        // until some LATER unclaimed `{` — an `impl` or `mod` header — which
        // would then be mistaken for an FFI scope, costing every symbol inside
        // it its `Type::` qualification (and so its disposition key) and
        // exempting its cfg(windows) fns from the sibling check.
        if !had_fn_on_line && st.extern_block_re.is_match(skel_t) {
            pending_extern = true;
        }
        for ch in line.skel.chars() {
            match ch {
                '(' | '[' => bracket += 1,
                ')' | ']' => bracket = (bracket - 1).max(0),
                '{' => {
                    depth += 1;
                    if let Some(name) = pending_fn.take() {
                        fn_stack.push((name, depth));
                    } else if pending_extern {
                        pending_extern = false;
                        extern_stack.push(depth);
                    } else if let Some(name) = pending_impl.take() {
                        impl_stack.push((name, depth));
                    }
                    for r in regions.iter_mut() {
                        if !r.entered && depth == r.start_depth + 1 {
                            r.entered = true;
                        }
                    }
                }
                '}' => {
                    depth -= 1;
                    while fn_stack.last().is_some_and(|(_, d)| *d > depth) {
                        fn_stack.pop();
                    }
                    while impl_stack.last().is_some_and(|(_, d)| *d > depth) {
                        impl_stack.pop();
                    }
                    while extern_stack.last().is_some_and(|d| *d > depth) {
                        extern_stack.pop();
                    }
                    if const_ctx.as_ref().is_some_and(|(_, d)| *d > depth) {
                        const_ctx = None;
                    }
                    // An entered item closes at its own `}`; anything — a field,
                    // variant or arm with no trailing comma — closes when the
                    // enclosing block does.
                    regions.retain(|r| {
                        !((r.entered && depth == r.start_depth) || depth < r.start_depth)
                    });
                }
                ';' => {
                    if bracket == 0 {
                        pending_fn = None;
                        pending_impl = None;
                        pending_extern = false;
                        if const_ctx.as_ref().is_some_and(|(_, d)| *d == depth) {
                            const_ctx = None;
                        }
                    }
                    regions.retain(|r| {
                        !(!r.entered && depth == r.start_depth && bracket == r.start_bracket)
                    });
                }
                ',' => {
                    regions.retain(|r| {
                        !(r.comma_closes
                            && !r.entered
                            && depth == r.start_depth
                            && bracket == r.start_bracket)
                    });
                }
                _ => {}
            }
        }
    }

    // ---- os_bound_tooling, structural half ---------------------------------
    if !whole_file_windows {
        for (q, bare) in windows_fns {
            if !other_os_fns.contains(&bare) {
                hits.push(Hit {
                    class: OS_BOUND.to_string(),
                    file: rel.to_string(),
                    symbol: q,
                    excerpt: format!("cfg(windows)-only fn `{bare}` has no cfg(not(windows)) sibling in this file"),
                });
            }
        }
    }
    hits
}

// ===========================================================================
// The walk.
// ===========================================================================

#[derive(Default)]
struct ScanResult {
    hits: Vec<Hit>,
    scanned_files: usize,
    /// reason -> count
    skipped: BTreeMap<String, usize>,
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Is `file` a crate root (its `mod x;` children live beside it)?
fn is_mod_root(src_root: &Path, file: &Path) -> bool {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "mod.rs" || name == "lib.rs" || name == "main.rs" {
        return true;
    }
    // src/bin/<x>.rs is a crate root too.
    file.parent() == Some(src_root.join("bin").as_path())
}

/// Scan every `.rs` file under `src_root`. `rel_base` is what hit paths are made
/// relative to (the crate root, so paths read `src/...`).
fn scan_tree(src_root: &Path, rel_base: &Path) -> ScanResult {
    let st = Structure::new();
    let classes = compile_classes();
    let mut files = Vec::new();
    walk(src_root, &mut files);
    files.sort();

    // Pass 1: read + lex every file; collect cfg(test)/cfg(windows) mod decls.
    let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut result = ScanResult::default();
    let mut test_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut test_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut windows_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut windows_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut inner_test: BTreeSet<PathBuf> = BTreeSet::new();

    for f in &files {
        let Ok(src) = fs::read_to_string(f) else {
            *result.skipped.entry("unreadable".into()).or_default() += 1;
            continue;
        };
        let lines = lex(&src);
        if st.inner_cfg(&lines, |f| f.test) {
            inner_test.insert(f.clone());
        }
        if st.inner_cfg(&lines, |f| f.windows && !f.test) {
            windows_files.insert(f.clone());
        }
        let parent = f.parent().unwrap_or(src_root).to_path_buf();
        let child_dir = if is_mod_root(src_root, f) {
            parent.clone()
        } else {
            parent.join(f.file_stem().unwrap_or_default())
        };
        for (kind, set_f, set_d) in [
            (
                (|f: CfgFlags| f.test) as fn(CfgFlags) -> bool,
                &mut test_files,
                &mut test_dirs,
            ),
            (
                |f: CfgFlags| f.windows && !f.test,
                &mut windows_files,
                &mut windows_dirs,
            ),
        ] {
            for (name, path) in st.cfg_mod_decls(&lines, kind) {
                if let Some(p) = path {
                    set_f.insert(parent.join(p));
                } else {
                    set_f.insert(child_dir.join(format!("{name}.rs")));
                    set_d.insert(child_dir.join(&name));
                }
            }
        }
        sources.insert(f.clone(), src);
    }

    for (f, src) in &sources {
        let under = |dirs: &BTreeSet<PathBuf>| dirs.iter().any(|d| f.starts_with(d));
        if inner_test.contains(f) {
            *result.skipped.entry("#![cfg(test)]".into()).or_default() += 1;
            continue;
        }
        if test_files.contains(f) || under(&test_dirs) {
            *result
                .skipped
                .entry("cfg(test) mod decl".into())
                .or_default() += 1;
            continue;
        }
        let rel = f
            .strip_prefix(rel_base)
            .unwrap_or(f)
            .display()
            .to_string()
            .replace('\\', "/");
        let win = windows_files.contains(f) || under(&windows_dirs);
        result
            .hits
            .extend(scan_source(&st, &classes, &rel, src, win));
        result.scanned_files += 1;
    }
    result
}

fn check_floor(result: &ScanResult) -> Result<(), String> {
    if result.scanned_files < SCANNED_FILES_FLOOR {
        return Err(format!(
            "VACUOUS SCAN: scanned_files={} is below the floor of {SCANNED_FILES_FLOOR}. \
             The walker, the scan root or the cfg(test) exclusion is broken — this is \
             never a pass with zero hits.",
            result.scanned_files
        ));
    }
    Ok(())
}

// ===========================================================================
// Dispositions + roster.
// ===========================================================================

#[derive(Debug, Deserialize)]
struct DispositionsFile {
    /// The ratchet: the roster's `unreviewed` ROW count must EQUAL this. More
    /// is a new, un-looked-at assumption (red, naming the rows); fewer means
    /// someone triaged one and the ceiling must come down to hold the gain.
    /// Absent is a red, never "no ceiling".
    unreviewed_ceiling: Option<usize>,
    #[serde(default)]
    disposition: Vec<DispositionEntry>,
}

#[derive(Debug, Deserialize)]
struct DispositionEntry {
    class: String,
    file: String,
    symbol: String,
    disposition: String,
    #[serde(default)]
    note: Option<String>,
    /// The exact excerpts this disposition was reviewed against. A hit under
    /// the same key with any OTHER excerpt renders `unreviewed` — a new
    /// assumption never silently inherits an old verdict.
    ///
    /// **The pin's resolution is one excerpt per (physical line, class), and
    /// excerpts are truncated at 137 chars.** Vocabulary classes match each
    /// physical line, so every matching line of a multi-line string literal is
    /// its own row; a `Target::Literal` pattern (the runner-local ones) still
    /// reports a multi-line literal ONCE, at the line holding its first match.
    /// So the pin does NOT notice an edit past the truncation point (count the
    /// truncated ones with `grep -c '\.\.\.",$'` on the dispositions file
    /// rather than trusting a number here), nor, for a Literal-target class, a
    /// second match further down the same literal. Within one symbol those
    /// edits keep the reviewed verdict silently; a re-read of the symbol is
    /// what catches them.
    #[serde(default)]
    reviewed: Vec<String>,
}

struct Disposition {
    disposition: String,
    note: Option<String>,
    reviewed: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    class: String,
    file: String,
    symbol: String,
    excerpt: String,
    count: usize,
    disposition: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Roster {
    schema: u32,
    generator: String,
    plan: String,
    rows: Vec<Row>,
}

fn valid_disposition(d: &str) -> bool {
    if matches!(d, "unreviewed" | "fallback_correct" | "dev_only_surface") {
        return true;
    }
    let re = Regex::new(r"^defect\(\d{4}-\d{2}-\d{2}-[a-z0-9-]+\)$").unwrap();
    re.is_match(d)
}

type Key = (String, String, String);

fn load_dispositions(root: &Path) -> Result<(BTreeMap<Key, Disposition>, usize), String> {
    let path = root.join(DISPOSITIONS_TOML);
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let parsed: DispositionsFile =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let ceiling = parsed.unreviewed_ceiling.ok_or_else(|| {
        format!(
            "{} has no top-level `unreviewed_ceiling = <n>` — the ratchet's checked-in bound \
             is required (an absent ceiling is not \"unbounded\")",
            path.display()
        )
    })?;
    let mut out = BTreeMap::new();
    for e in parsed.disposition {
        if !valid_disposition(&e.disposition) {
            return Err(format!(
                "invalid disposition {:?} for ({}, {}, {}) — must be unreviewed | fallback_correct \
                 | dev_only_surface | defect(<plan stem>)",
                e.disposition, e.class, e.file, e.symbol
            ));
        }
        if e.disposition != "unreviewed" && e.reviewed.is_empty() {
            return Err(format!(
                "disposition {:?} for ({}, {}, {}) lists no `reviewed` excerpts — record the \
                 excerpt(s) it was reviewed against",
                e.disposition, e.class, e.file, e.symbol
            ));
        }
        let key = (e.class.clone(), e.file.clone(), e.symbol.clone());
        let d = Disposition {
            disposition: e.disposition,
            note: e.note,
            reviewed: e.reviewed.into_iter().collect(),
        };
        if out.insert(key, d).is_some() {
            return Err(format!(
                "duplicate disposition key ({}, {}, {})",
                e.class, e.file, e.symbol
            ));
        }
    }
    Ok((out, ceiling))
}

/// The exact ratchet on `unreviewed` rows. `fresh` is this scan's roster,
/// `checked` the checked-in one (used only to NAME what is new).
fn check_ceiling(fresh: &[Row], checked: &[Row], ceiling: usize) -> Result<(), String> {
    let unreviewed: Vec<&Row> = fresh
        .iter()
        .filter(|r| r.disposition == "unreviewed")
        .collect();
    let n = unreviewed.len();
    if n > ceiling {
        let was: BTreeSet<String> = checked
            .iter()
            .filter(|r| r.disposition == "unreviewed")
            .map(row_ident)
            .collect();
        let new_rows: Vec<String> = unreviewed
            .iter()
            .map(|r| row_ident(r))
            .filter(|k| !was.contains(k))
            .collect();
        let named = if new_rows.is_empty() {
            "(none differ from the checked-in roster, so the ceiling itself was lowered below \
             the roster's own unreviewed count — restore it)"
                .to_string()
        } else {
            new_rows.join("\n    ")
        };
        return Err(format!(
            "UNREVIEWED ABOVE CEILING: unreviewed={n} > unreviewed_ceiling={ceiling} in \
             {DISPOSITIONS_TOML}. New unreviewed row(s):\n    {named}\n\
             Review each: give it a disposition (fallback_correct | dev_only_surface | \
             defect(<plan stem>)) with its `reviewed` excerpt, or remove the assumption. \
             Raising the ceiling instead is a visible, reviewable choice — make it only with \
             the reason in the PR."
        ));
    }
    if n < ceiling {
        return Err(format!(
            "UNREVIEWED BELOW CEILING: unreviewed={n} < unreviewed_ceiling={ceiling}. Lower \
             `unreviewed_ceiling` to {n} in {DISPOSITIONS_TOML} so the gain is held (exact \
             ratchet)."
        ));
    }
    Ok(())
}

/// A `git` invocation in `repo` with the repository-selecting variables an
/// enclosing hook or worktree may export removed, so `current_dir` is
/// authoritative (the same hazard `fleet_skills/.../lib/git-scope.sh` names).
fn git_in(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .args(args)
        .output()
        .map_err(|e| format!("`git {}` could not run: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The roster JSON at `merge-base HEAD <main>` (trying `origin/main`, then
/// `main`), with a label naming what was read. `Err` says why it could not be
/// read (no git, no main ref, a shallow clone with no merge base, the file
/// absent there) — the caller reports that as UNKNOWN.
fn merge_base_roster(repo: &Path) -> Result<(String, Vec<Row>), String> {
    let mut why = Vec::new();
    for main in ["origin/main", "main"] {
        let spec = format!("{main}^{{commit}}");
        if let Err(e) = git_in(repo, &["rev-parse", "--verify", "--quiet", &spec]) {
            why.push(format!("{main}: no such ref ({e})"));
            continue;
        }
        let base = match git_in(repo, &["merge-base", "HEAD", main]) {
            Ok(b) => b.trim().to_string(),
            Err(e) => {
                let shallow = git_in(repo, &["rev-parse", "--is-shallow-repository"])
                    .map(|s| s.trim() == "true")
                    .unwrap_or(false);
                why.push(format!(
                    "{main}: no merge base{} ({e})",
                    if shallow {
                        " — the clone is SHALLOW"
                    } else {
                        ""
                    }
                ));
                continue;
            }
        };
        let object = format!("{base}:docs/workspace-assumptions.json");
        let text = match git_in(repo, &["show", &object]) {
            Ok(t) => t,
            Err(e) => {
                why.push(format!(
                    "{main}: roster unreadable at merge base {base} ({e})"
                ));
                continue;
            }
        };
        let roster: Roster = serde_json::from_str(&text)
            .map_err(|e| format!("roster at merge base {base} ({main}) does not parse: {e}"))?;
        return Ok((format!("merge-base({main}) {base}"), roster.rows));
    }
    Err(why.join("; "))
}

/// Unreviewed rows under a `(class, file, symbol)` key the base roster
/// does not carry at all and the dispositions file does not name. The net
/// count ratchet cannot see a SWAP (one assumption removed, a new one added);
/// this can, so regeneration refuses such a row until its author either
/// reviews it or acknowledges it with an explicit `disposition = "unreviewed"`
/// entry — a visible line in the hand-edited file either way.
fn unacknowledged_new_keys(
    fresh: &[Row],
    checked: &[Row],
    explicit: &BTreeSet<Key>,
) -> Vec<String> {
    let known: BTreeSet<Key> = checked
        .iter()
        .map(|r| (r.class.clone(), r.file.clone(), r.symbol.clone()))
        .collect();
    fresh
        .iter()
        .filter(|r| r.disposition == "unreviewed")
        .filter(|r| {
            let k = (r.class.clone(), r.file.clone(), r.symbol.clone());
            !known.contains(&k) && !explicit.contains(&k)
        })
        .map(row_ident)
        .collect()
}

/// `(capability id, identifier tokens of its anchor)` for every CAPABILITY_SPECS
/// row, read as TEXT out of `capability_manifest.rs` (this test builds no binary).
/// Parsed per `CapabilitySpec { … }` block, so a spec missing an `anchor` yields
/// no tokens for ITS id rather than shifting every later row by one.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn capability_anchors(root: &Path) -> Vec<(String, BTreeSet<String>)> {
    let src = fs::read_to_string(root.join("src/capability_manifest.rs")).unwrap_or_default();
    let Some(start) = src.find("pub const CAPABILITY_SPECS") else {
        return Vec::new();
    };
    let body = &src[start..];
    let body = &body[..body.find("\n];").unwrap_or(body.len())];
    let id_re = Regex::new(r#"\bid:\s*"([a-z_]+)""#).unwrap();
    let anchor_re = Regex::new(r#"\banchor:\s*"([^"]*)""#).unwrap();
    body.split("CapabilitySpec {")
        .skip(1)
        .filter_map(|block| {
            let id = id_re.captures(block)?[1].to_string();
            let toks = anchor_re
                .captures(block)
                .map(|c| {
                    c[1].split(|ch: char| !is_ident(ch))
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some((id, toks))
        })
        .collect()
}

/// A `repo_layout` row feeds a capability when its enclosing symbol's final
/// segment AND its module (file stem or a parent dir) are both named in that
/// capability's anchor. Direct naming only — a caller two frames up is not
/// traced, which is the gap the `capability`-less rows make visible.
fn capability_for(hit: &Hit, anchors: &[(String, BTreeSet<String>)]) -> Option<String> {
    if hit.class != "repo_layout" {
        return None;
    }
    let sym = hit
        .symbol
        .rsplit("::")
        .next()
        .unwrap_or("")
        .rsplit(' ')
        .next()
        .unwrap_or("");
    let path = Path::new(&hit.file);
    let mut modules: Vec<String> = path
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if let Some(stem) = path.file_stem() {
        modules.push(stem.to_string_lossy().to_string());
    }
    anchors
        .iter()
        .find(|(_, toks)| toks.contains(sym) && modules.iter().any(|m| toks.contains(m)))
        .map(|(id, _)| id.clone())
}

struct Built {
    roster: Roster,
    unreviewed_ceiling: usize,
    /// Every key with an entry in the dispositions file (any disposition,
    /// including an explicit `unreviewed`).
    explicit_keys: BTreeSet<Key>,
    /// A disposition key, or one of its reviewed excerpts, that matches no hit.
    orphan_dispositions: Vec<String>,
    /// Rows under a dispositioned key whose excerpt was never reviewed.
    new_since_review: usize,
}

fn build_roster(root: &Path, hits: &[Hit]) -> Result<Built, String> {
    let (dispositions, unreviewed_ceiling) = load_dispositions(root)?;
    let explicit_keys: BTreeSet<Key> = dispositions.keys().cloned().collect();
    let anchors = capability_anchors(root);

    let mut counted: BTreeMap<Hit, usize> = BTreeMap::new();
    for h in hits {
        *counted.entry(h.clone()).or_default() += 1;
    }
    let mut used: BTreeSet<(Key, String)> = BTreeSet::new();
    let mut new_since_review = 0;
    let rows: Vec<Row> = counted
        .into_iter()
        .map(|(h, count)| {
            let key = (h.class.clone(), h.file.clone(), h.symbol.clone());
            let (disposition, note) = match dispositions.get(&key) {
                Some(d) if d.reviewed.contains(&h.excerpt) || d.disposition == "unreviewed" => {
                    used.insert((key, h.excerpt.clone()));
                    (d.disposition.clone(), d.note.clone())
                }
                Some(d) => {
                    new_since_review += 1;
                    (
                        "unreviewed".to_string(),
                        Some(format!(
                            "NEW since review — this key is `{}` for {} reviewed excerpt(s), not this one",
                            d.disposition,
                            d.reviewed.len()
                        )),
                    )
                }
                None => ("unreviewed".to_string(), None),
            };
            Row {
                capability: capability_for(&h, &anchors),
                class: h.class,
                file: h.file,
                symbol: h.symbol,
                excerpt: h.excerpt,
                count,
                disposition,
                note,
            }
        })
        .collect();
    let mut orphan_dispositions = Vec::new();
    for (k, d) in &dispositions {
        let (c, f, sym) = k;
        if d.disposition == "unreviewed" {
            if !used.iter().any(|(uk, _)| uk == k) {
                orphan_dispositions.push(format!("({c}, {f}, {sym}) matches no hit"));
            }
            continue;
        }
        for ex in &d.reviewed {
            if !used.contains(&(k.clone(), ex.clone())) {
                orphan_dispositions.push(format!(
                    "({c}, {f}, {sym}) reviewed excerpt {ex:?} matches no hit"
                ));
            }
        }
    }
    Ok(Built {
        roster: Roster {
            schema: 1,
            generator: format!(
                "GENERATED by src-tauri/tests/workspace_assumptions_are_enumerated.rs — \
                 regenerate with {UPDATE_ENV}=1; edit dispositions in \
                 docs/workspace-assumptions.dispositions.toml, never here"
            ),
            plan: PLAN_STEM.to_string(),
            rows,
        },
        unreviewed_ceiling,
        explicit_keys,
        orphan_dispositions,
        new_since_review,
    })
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('`', "'")
}

fn render_md(roster: &Roster, class_ids: &[String]) -> String {
    let mut out = String::new();
    out.push_str("# Workspace assumptions — the static roster\n\n");
    out.push_str(
        "<!-- GENERATED by src-tauri/tests/workspace_assumptions_are_enumerated.rs. \
         Do not edit by hand: set dispositions in \
         docs/workspace-assumptions.dispositions.toml and regenerate with \
         UPDATE_WORKSPACE_ASSUMPTIONS=1. -->\n\n",
    );
    out.push_str(&format!(
        "Every place `src-tauri/src` (outside `cfg(test)` and comments) matches one of the \
         fixed workspace-assumption patterns — Phase 4 of plan `{PLAN_STEM}`. The fleet-noun \
         classes come from `qontinui-schemas/fleet-nouns.toml` (plan `{VOCABULARY_PLAN_STEM}`, \
         Phase A6); `plans_dir` and `os_bound_tooling` are runner-local. The test fails when \
         this file is stale, and when the `unreviewed` row count differs from the \
         `unreviewed_ceiling` checked into the dispositions file (a ratchet: it may only \
         fall, and each fall lowers the ceiling). It never fails on a `defect` count. \
         `docs/workspace-assumptions.json` is the machine-readable twin.\n\n"
    ));
    out.push_str(
        "Dispositions: `unreviewed` (not yet triaged), `fallback_correct` (the \
                  assumption degrades correctly off a workspace), `dev_only_surface` (only a \
                  developer box reaches it), `defect(<plan stem>)` (a user-facing path that \
                  depends on a workspace, cited to the plan that owns the fix). A disposition \
                  covers only the excerpts it was reviewed against; a later excerpt under the \
                  same symbol renders `unreviewed` with a `NEW since review` note.\n\n",
    );

    let mut by_class: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for c in class_ids {
        by_class.insert(c.as_str(), Vec::new());
    }
    for r in &roster.rows {
        by_class.entry(r.class.as_str()).or_default().push(r);
    }

    out.push_str(
        "| class | rows | hits | unreviewed | fallback_correct | dev_only_surface | defect |\n",
    );
    out.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for c in class_ids {
        let rows = &by_class[c.as_str()];
        let n = |p: &dyn Fn(&str) -> bool| rows.iter().filter(|r| p(&r.disposition)).count();
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            c,
            rows.len(),
            rows.iter().map(|r| r.count).sum::<usize>(),
            n(&|d| d == "unreviewed"),
            n(&|d| d == "fallback_correct"),
            n(&|d| d == "dev_only_surface"),
            n(&|d| d.starts_with("defect(")),
        ));
    }
    out.push('\n');

    for c in class_ids {
        let rows = &by_class[c.as_str()];
        out.push_str(&format!("## `{}` ({} rows)\n\n", c, rows.len()));
        if rows.is_empty() {
            out.push_str("_No hits._\n\n");
            continue;
        }
        let cap_col = c == "repo_layout";
        if cap_col {
            out.push_str("| file | symbol | excerpt | n | disposition | capability |\n|---|---|---|---:|---|---|\n");
        } else {
            out.push_str("| file | symbol | excerpt | n | disposition |\n|---|---|---|---:|---|\n");
        }
        for r in rows {
            let disp = match &r.note {
                Some(n) => format!("{} — {}", r.disposition, md_escape(n)),
                None => r.disposition.clone(),
            };
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | {} | {}",
                r.file,
                md_escape(&r.symbol),
                md_escape(&r.excerpt),
                r.count,
                disp
            ));
            if cap_col {
                out.push_str(&format!(
                    " | {} |\n",
                    r.capability
                        .as_deref()
                        .map_or("— (no CAPABILITY_SPECS row)".to_string(), |c| format!(
                            "`{c}`"
                        ))
                ));
            } else {
                out.push_str(" |\n");
            }
        }
        out.push('\n');
    }
    out
}

fn to_json(roster: &Roster) -> String {
    let mut s = serde_json::to_string_pretty(roster).expect("roster serializes");
    s.push('\n');
    s
}

fn row_ident(r: &Row) -> String {
    format!("[{}] {} :: {} :: {}", r.class, r.file, r.symbol, r.excerpt)
}

fn staleness(checked: &str, fresh: &Roster) -> Vec<String> {
    let old: Vec<Row> = serde_json::from_str::<Roster>(checked)
        .map(|r| r.rows)
        .unwrap_or_default();
    let old_map: BTreeMap<String, &Row> = old.iter().map(|r| (row_ident(r), r)).collect();
    let new_map: BTreeMap<String, &Row> = fresh.rows.iter().map(|r| (row_ident(r), r)).collect();
    let mut out = Vec::new();
    for (k, r) in &new_map {
        match old_map.get(k) {
            None => out.push(format!(
                "HIT WITH NO ROW: {k}  (enclosing fn `{}` in {})",
                r.symbol, r.file
            )),
            Some(o) if o != r => out.push(format!(
                "ROW CHANGED: {k}  (count {}→{}, disposition {}→{}, capability {:?}→{:?})",
                o.count, r.count, o.disposition, r.disposition, o.capability, r.capability
            )),
            _ => {}
        }
    }
    for k in old_map.keys() {
        if !new_map.contains_key(k) {
            out.push(format!("ROW WITH NO HIT: {k}"));
        }
    }
    out
}

// ===========================================================================
// Tests.
// ===========================================================================

#[test]
fn workspace_assumption_roster_is_fresh() {
    let root = crate_root();
    let result = scan_tree(&root.join("src"), &root);

    let built = build_roster(&root, &result.hits).unwrap_or_else(|e| panic!("{e}"));
    let rows = &built.roster.rows;
    let unreviewed = rows
        .iter()
        .filter(|r| r.disposition == "unreviewed")
        .count();
    let defect = rows
        .iter()
        .filter(|r| r.disposition.starts_with("defect("))
        .count();
    let skipped_total: usize = result.skipped.values().sum();
    let skipped_detail = result
        .skipped
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let counts = format!(
        "unreviewed={unreviewed} unreviewed_ceiling={} defect={defect} new_since_review={} \
         scanned_files={} skipped_files={skipped_total} ({skipped_detail})",
        built.unreviewed_ceiling, built.new_since_review, result.scanned_files
    );
    println!("{counts}");
    // CI captures test stdout on a green run; the job summary is where a
    // reader actually sees the counts. GitHub Actions ONLY: the runner's own
    // CI-node lane (`.qontinui/ci.toml`) sets no `GITHUB_STEP_SUMMARY` and
    // passes a closed env allowlist, so a green run there still shows no
    // counts — read them from the job log, or run the test locally.
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&summary)
        {
            let _ = writeln!(f, "workspace assumptions: `{counts}`");
        }
    }
    let mut per_class: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in rows {
        let e = per_class.entry(r.class.as_str()).or_default();
        e.0 += 1;
        e.1 += r.count;
    }
    for (c, (n_rows, n_hits)) in &per_class {
        println!("  {c}: rows={n_rows} hits={n_hits}");
    }

    check_floor(&result).unwrap_or_else(|e| panic!("{e}"));

    let class_ids: Vec<String> = compile_classes().into_iter().map(|c| c.id).collect();
    let json = to_json(&built.roster);
    let md = render_md(&built.roster, &class_ids);
    let json_path = root.join(ROSTER_JSON);
    let md_path = root.join(ROSTER_MD);

    let checked_json = fs::read_to_string(&json_path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let checked_rows: Vec<Row> = serde_json::from_str::<Roster>(&checked_json)
        .map(|r| r.rows)
        .unwrap_or_default();
    let ceiling = check_ceiling(rows, &checked_rows, built.unreviewed_ceiling);

    // The swap rule's base is the roster at the MERGE BASE, never the
    // working-tree JSON — which a regenerate (after an acknowledgement that is
    // then deleted) or a hand edit can make carry the new key. Unreadable base
    // is UNKNOWN: said loudly, never a silent pass.
    let repo_root = root.parent().unwrap_or(&root).to_path_buf();
    let unacked: Option<Vec<String>> = match merge_base_roster(&repo_root) {
        Ok((base, base_rows)) => {
            println!("swap rule: known keys from the roster at {base}");
            Some(unacknowledged_new_keys(
                rows,
                &base_rows,
                &built.explicit_keys,
            ))
        }
        Err(why) => {
            let line = format!("SWAP RULE UNKNOWN: {why}");
            println!("{line}");
            eprintln!("{line}");
            if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
                use std::io::Write;
                if let Ok(mut f) = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&summary)
                {
                    let _ = writeln!(f, "workspace assumptions: `{line}`");
                }
            }
            None
        }
    };
    let unacked_message = |u: &[String]| {
        format!(
            "NEW UNREVIEWED ASSUMPTION(S) under a key the merge-base roster does not carry:\n    \
             {}\nReview each in {DISPOSITIONS_TOML} (a disposition with its `reviewed` \
             excerpt), or acknowledge it there with an explicit `disposition = \"unreviewed\"` \
             entry and a note — the net unreviewed count cannot see a swap, so a new key is \
             never admitted silently.",
            u.join("\n    ")
        )
    };

    if std::env::var(UPDATE_ENV).is_ok_and(|v| v == "1") {
        assert!(
            built.orphan_dispositions.is_empty(),
            "dispositions with no matching hit (fix the key or delete the entry): {:#?}",
            built.orphan_dispositions
        );
        // Regeneration may not launder a new unreviewed row into the checked-in
        // roster: above the ceiling it refuses, naming the rows. Below it, the
        // roster is written and the normal run then asks for the ceiling to fall.
        if unreviewed > built.unreviewed_ceiling {
            panic!("{}", ceiling.expect_err("above the ceiling is an error"));
        }
        if let Some(u) = unacked.as_ref().filter(|u| !u.is_empty()) {
            panic!("{}", unacked_message(u));
        }
        if let Err(e) = &ceiling {
            println!("NOTE: {e}");
        }
        fs::write(&json_path, &json).expect("write roster json");
        fs::write(&md_path, &md).expect("write roster md");
        println!(
            "regenerated {} and {}",
            json_path.display(),
            md_path.display()
        );
        return;
    }

    let checked_md = fs::read_to_string(&md_path)
        .unwrap_or_default()
        .replace("\r\n", "\n");

    let mut problems = staleness(&checked_json, &built.roster);
    for o in &built.orphan_dispositions {
        problems.push(format!(
            "DISPOSITION WITH NO HIT: {o} in {DISPOSITIONS_TOML}"
        ));
    }
    if problems.is_empty() && checked_json != json {
        problems.push(
            "docs/workspace-assumptions.json differs from a fresh render (header/order)".into(),
        );
    }
    if checked_md != md {
        problems.push("docs/workspace-assumptions.md differs from a fresh render".into());
    }
    if let Err(e) = ceiling {
        problems.push(e);
    }
    if let Some(u) = unacked.as_ref().filter(|u| !u.is_empty()) {
        problems.push(unacked_message(u));
    }
    assert!(
        problems.is_empty(),
        "\nThe workspace-assumption roster is STALE or off its unreviewed ceiling ({} \
         problem(s)):\n\n  {}\n\n\
         A new row is a new workspace assumption: review it, give it a disposition in \
         docs/workspace-assumptions.dispositions.toml if you can, then regenerate with\n  \
         {UPDATE_ENV}=1 cargo-guard.sh test --test workspace_assumptions_are_enumerated\n",
        problems.len(),
        problems.join("\n  ")
    );
}

/// Every class matches its own planted fixture — so a class whose regex rots to
/// matching nothing cannot hide behind "0 hits". The fixtures live under
/// `tests/`, which the real scan never walks.
#[test]
fn every_class_matches_its_planted_fixture() {
    let root = crate_root();
    let st = Structure::new();
    let classes = compile_classes();
    for class in &classes {
        let path = root.join(FIXTURE_DIR).join(format!("{}.rs.txt", class.id));
        let src = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()));
        let hits = scan_source(&st, &classes, "fixture.rs", &src, false);
        let mine: Vec<&Hit> = hits.iter().filter(|h| h.class == class.id).collect();
        assert!(
            !mine.is_empty(),
            "class `{}` matched nothing in its planted fixture {} — the pattern is broken",
            class.id,
            path.display()
        );
        assert!(
            mine.iter().all(|h| h.symbol.starts_with("planted")),
            "class `{}` fixture hits must be attributed to a `planted*` fn, got {:?}",
            class.id,
            mine
        );
    }
}

/// Everything under `cfg(test)`, `cfg(windows)` (for the literal half of
/// `os_bound_tooling`) and in comments is excluded.
#[test]
fn excluded_regions_yield_no_hits() {
    let root = crate_root();
    let path = root.join(FIXTURE_DIR).join("excluded.rs.txt");
    let src = fs::read_to_string(&path).expect("excluded fixture");
    let hits = scan_source(
        &Structure::new(),
        &compile_classes(),
        "fixture.rs",
        &src,
        false,
    );
    assert!(hits.is_empty(), "excluded regions produced hits: {hits:#?}");
}

/// Constructs that are cfg'd but END early — a struct field, an enum variant,
/// a match arm — and the lexer's hard cases. Every hit after them MUST still
/// be reported: this asserts the exact set, so a region that fails to close
/// (and silently swallows the rest of the file) reds, and so does a lexer that
/// mistakes a comment or a char literal for code.
#[test]
fn constructs_that_end_early_do_not_swallow_later_hits() {
    let root = crate_root();
    let path = root.join(FIXTURE_DIR).join("must_report.rs.txt");
    let src = fs::read_to_string(&path).expect("must_report fixture");
    let hits = scan_source(
        &Structure::new(),
        &compile_classes(),
        "fixture.rs",
        &src,
        false,
    );
    let got: BTreeSet<(String, String)> =
        hits.iter().cloned().map(|h| (h.class, h.symbol)).collect();
    let want: BTreeSet<(String, String)> = [
        ("dev_ports", "after_cfg_field"),
        ("dev_ports", "after_cfg_variant"),
        ("machine_path", "cfg_match_arm"),
        ("machine_path", "after_cfg_match_arm"),
        ("machine_path", "lexer_raw_string"),
        ("machine_path", "lexer_nested_comment"),
        ("dev_ports", "lexer_chars"),
        ("supervisor_dependency", "lexer_chars"),
        ("dev_ports", "lexer_lifetime"),
        (
            "os_bound_tooling",
            "Handles::windows_method_without_sibling",
        ),
        ("supervisor_dependency", "after_nested_regions"),
        ("machine_path", "after_nested_test_mod"),
        ("os_bound_tooling", "after_nested_test_mod"),
        // An `extern "C" fn` must not arm the FFI scope: both rows keep their
        // `Console::` qualification, and `win_only` keeps its structural row.
        ("os_bound_tooling", "Console::launcher"),
        ("os_bound_tooling", "Console::win_only"),
        // These two rows show only that the template does not swallow what
        // FOLLOWS it — they stand with the skeleton gate and without it. The
        // gate itself is pinned by
        // `attribute_shaped_lines_inside_string_literals_are_data`.
        ("dev_ports", "after_template"),
        ("supervisor_dependency", "after_template"),
    ]
    .iter()
    .map(|(c, s)| (c.to_string(), s.to_string()))
    .collect();
    assert_eq!(
        got, want,
        "must_report fixture: hit set differs (left = got, right = want)"
    );

    // Spelled out, because these are what the region STACK and the `extern`-block
    // scope buy — and both sides wear the same (class, symbol) pair, so only the
    // EXCERPT tells them apart. Flatten the stack and `after_nested_test_mod`
    // stops being windows-only (no structural row) and starts reporting its
    // `taskkill` literal instead: the pair survives, the meaning inverts.
    let os_bound_excerpts: Vec<&str> = hits
        .iter()
        .filter(|h| h.class == OS_BOUND && h.symbol == "after_nested_test_mod")
        .map(|h| h.excerpt.as_str())
        .collect();
    assert_eq!(
        os_bound_excerpts.len(),
        1,
        "expected exactly one os_bound_tooling hit on `after_nested_test_mod`, got {os_bound_excerpts:?}"
    );
    assert!(
        os_bound_excerpts[0].starts_with("cfg(windows)-only fn"),
        "the outer cfg(windows) region did not survive the nested cfg(test) module — regions \
         must be a STACK, not one region at a time. Got {:?} instead of the structural row.",
        os_bound_excerpts[0]
    );
    assert!(
        !got.contains(&(OS_BOUND.to_string(), "GetLastErrorShim".to_string())),
        "an `extern \"…\"` FFI declaration was rostered as a windows-only fn with no \
         sibling — a row no code change can ever resolve"
    );
}

/// cfg predicates are parsed, not prefix-matched: `test` anywhere as a positive
/// `all(...)` atom counts, under `not(...)` it does not; same for windows.
#[test]
fn cfg_predicates_are_parsed_not_prefix_matched() {
    let f = |a: &str| CfgFlags::of(&[a.to_string()]);
    assert!(f("#[cfg(test)]").test);
    assert!(f("#[cfg(all(feature = \"x\", test))]").test);
    assert!(f("#[cfg(all( test , feature = \"x\" ))]").test);
    assert!(f("#[cfg(any(test, all(test, unix)))]").test);
    assert!(!f("#[cfg(not(test))]").test);
    assert!(!f("#[cfg(any(test, feature = \"x\"))]").test);
    assert!(!f("#[cfg_attr(test, derive(Debug))]").test);
    assert!(f("#[cfg(all(unix, target_os = \"windows\"))]").windows);
    assert!(f("#[cfg(target_family = \"windows\")]").windows);
    assert!(!f("#[cfg(not(windows))]").windows);
    assert!(f("#[cfg(not(windows))]").not_windows);
    assert!(f("#[cfg(any(target_os = \"linux\", target_os = \"macos\"))]").not_windows);
    assert!(!f("#[cfg(any(unix, windows))]").not_windows);

    // Inner attributes, including a multi-line one (joined by the lexer).
    let st = Structure::new();
    let lines = lex("//! doc\n#![cfg(all(\n    feature = \"z\",\n    test\n))]\nfn f() {}\n");
    assert!(st.inner_cfg(&lines, |f| f.test));
    let lines = lex("#![cfg(not(test))]\nfn f() {}\n");
    assert!(!st.inner_cfg(&lines, |f| f.test));
}

/// The skeleton gate (F5): an attribute-shaped line that is really string DATA
/// must neither open a region nor skip a whole file.
///
/// Pinned HERE rather than in `must_report.rs.txt`, because a template's bogus
/// region is unobservable through the hits AFTER it: never entered (the
/// skeleton blanks the template's braces), it dies at the next `;` or `,` at
/// its own depth, or at the enclosing `}`. Only a hit INSIDE the live region
/// shows it; the line that OPENS it is the smallest such case, and is what
/// this asserts. The third gate site, `cfg_mod_decls`, is NOT pinned here: it
/// needs a `#[cfg(test)]` and a `mod x;` both inside one literal to fake a
/// whole-file skip, and no such shape exists.
#[test]
fn attribute_shaped_lines_inside_string_literals_are_data() {
    let st = Structure::new();
    let classes = compile_classes();

    let src = "fn holds_a_template() -> &'static str {\n    r#\"\n#[cfg(test)] the supervisor answers on :9875\n\"#\n}\n";
    let hits = scan_source(&st, &classes, "fixture.rs", src, false);
    assert!(
        hits.iter()
            .any(|h| h.class == "supervisor_dependency" && h.symbol == "holds_a_template"),
        "an attribute-shaped line inside a string literal opened a cfg(test) region and \
         swallowed the hit on its own line — attribute detection must be gated on the \
         SKELETON, not on `code`. Got {hits:#?}"
    );

    let lines = lex("fn t() -> &'static str {\n    r#\"\n#![cfg(test)]\n\"#\n}\n");
    assert!(
        !st.inner_cfg(&lines, |f| f.test),
        "a `#![cfg(test)]` line inside a string literal skipped the whole file"
    );
}

/// Pointing the scan root at an empty dir fails on the floor rather than passing
/// with zero hits.
#[test]
fn empty_scan_root_fails_on_the_scanned_files_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = scan_tree(dir.path(), dir.path());
    assert_eq!(result.scanned_files, 0);
    assert!(result.hits.is_empty());
    let err = check_floor(&result).expect_err("an empty scan root must fail the floor");
    assert!(err.contains("VACUOUS SCAN"), "{err}");
}

/// The vocabulary's own planted positives and negatives, run through THIS
/// matcher: every `examples` entry hits its class, and no `lookalikes` entry or
/// product constant hits any class. The schemas repo proves the patterns under
/// three engines; this proves the runner implements the file's contract —
/// above all the SPAN-scoped `exclude` (`the spaceship operator <=> ran on
/// spaceship` must still hit; `C:\\Users\\Public\\x and C:\\Users\\josh\\y` too).
#[test]
fn vocabulary_contract_holds_under_this_matcher() {
    let path = vocabulary_path();
    let vocab = load_vocabulary(&path).unwrap_or_else(|e| panic!("{e}"));
    let classes = compile_classes_from(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut problems = Vec::new();
    let mut examples = 0;
    for vc in &vocab.classes {
        let class = classes.iter().find(|c| c.id == vc.id).expect("compiled");
        if vc.examples.is_empty() {
            problems.push(format!("class `{}` carries no examples", vc.id));
        }
        for ex in &vc.examples {
            examples += 1;
            if !vocabulary_hits(class, ex) {
                problems.push(format!("example {ex:?} does not hit its class `{}`", vc.id));
            }
        }
    }
    let negatives: Vec<&str> = vocab
        .lookalikes
        .iter()
        .map(String::as_str)
        .chain(vocab.product_constants.iter().map(|p| p.value.as_str()))
        .collect();
    for text in &negatives {
        for class in &classes {
            if vocabulary_hits(class, text) {
                problems.push(format!("negative {text:?} hits class `{}`", class.id));
            }
        }
    }
    assert!(
        examples > 0 && !negatives.is_empty(),
        "vacuous contract check: examples={examples} negatives={}",
        negatives.len()
    );
    assert!(
        problems.is_empty(),
        "{} disagrees with this matcher:\n  {}",
        path.display(),
        problems.join("\n  ")
    );
}

/// A missing vocabulary is a RED that names the path — never a skip, never
/// "no fleet nouns".
#[test]
fn missing_vocabulary_is_red_naming_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("qontinui-schemas").join("fleet-nouns.toml");
    let err = compile_classes_from(&missing)
        .err()
        .expect("a missing vocabulary must be an error");
    assert!(err.contains("UNREADABLE"), "{err}");
    assert!(err.contains(&missing.display().to_string()), "{err}");
}

/// An unparseable, wrong-version or class-less vocabulary is a red naming the
/// path, too.
#[test]
fn unusable_vocabulary_is_red_naming_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fleet-nouns.toml");
    for (body, want) in [
        ("version = 1\n[[class]\nid = ", "UNPARSEABLE"),
        (
            "version = 2\n[[class]]\nid = \"x\"\npattern = \"x\"\n",
            "VERSION",
        ),
        ("version = 1\n", "EMPTY"),
        (
            "version = 1\n[[class]]\nid = \"plans_dir\"\npattern = \"x\"\n",
            "runner-local",
        ),
        (
            "version = 1\n[[class]]\nid = \"dev_ports\"\npattern = \"x\"\n",
            "may only ADD",
        ),
    ] {
        fs::write(&path, body).expect("write");
        let err = compile_classes_from(&path)
            .err()
            .unwrap_or_else(|| panic!("{body:?} must be an error"));
        assert!(err.contains(want), "{body:?}: {err}");
        assert!(err.contains(&path.display().to_string()), "{body:?}: {err}");
    }
}

fn row(class: &str, symbol: &str, disposition: &str) -> Row {
    Row {
        class: class.to_string(),
        file: "src/x.rs".to_string(),
        symbol: symbol.to_string(),
        excerpt: "e".to_string(),
        count: 1,
        disposition: disposition.to_string(),
        capability: None,
        note: None,
    }
}

/// The exact ratchet: equal passes; one more unreviewed row is a red naming it;
/// one fewer is a red asking for the ceiling to fall.
#[test]
fn unreviewed_ceiling_is_an_exact_ratchet() {
    let checked = vec![
        row("dev_ports", "a", "unreviewed"),
        row("dev_ports", "b", "fallback_correct"),
    ];
    assert!(check_ceiling(&checked, &checked, 1).is_ok());

    let mut grown = checked.clone();
    grown.push(row("machine_path", "brand_new", "unreviewed"));
    let err = check_ceiling(&grown, &checked, 1).expect_err("above the ceiling");
    assert!(err.contains("UNREVIEWED ABOVE CEILING"), "{err}");
    assert!(
        err.contains("[machine_path] src/x.rs :: brand_new"),
        "{err}"
    );
    assert!(
        !err.contains(":: a ::"),
        "an old unreviewed row is not new: {err}"
    );

    let triaged = vec![
        row("dev_ports", "a", "fallback_correct"),
        row("dev_ports", "b", "fallback_correct"),
    ];
    let err = check_ceiling(&triaged, &checked, 1).expect_err("below the ceiling");
    assert!(err.contains("UNREVIEWED BELOW CEILING"), "{err}");
    assert!(err.contains("to 0"), "{err}");
}

/// A swap — one unreviewed assumption removed, a new one added — keeps the net
/// count equal, so the ceiling passes; the new-key check still names the new
/// row until the dispositions file acknowledges it.
#[test]
fn a_swapped_in_unreviewed_key_is_not_admitted_silently() {
    let checked = vec![row("dev_ports", "old", "unreviewed")];
    let fresh = vec![row("dev_ports", "brand_new", "unreviewed")];
    assert!(
        check_ceiling(&fresh, &checked, 1).is_ok(),
        "net count is unchanged"
    );
    let got = unacknowledged_new_keys(&fresh, &checked, &BTreeSet::new());
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains(":: brand_new ::"), "{got:?}");
    let acked: BTreeSet<Key> = [(
        "dev_ports".to_string(),
        "src/x.rs".to_string(),
        "brand_new".to_string(),
    )]
    .into_iter()
    .collect();
    assert!(unacknowledged_new_keys(&fresh, &checked, &acked).is_empty());
    // A key already in the checked-in roster is not new.
    assert!(unacknowledged_new_keys(&checked, &checked, &BTreeSet::new()).is_empty());
}

/// The runner-local refinements restore what the vocabulary narrowed: a bare
/// sibling-repo name used as a path, the `ui-bridge` name, and a bare 9875
/// port constant in code — without hitting a URL, an owner/repo slug, the
/// runner's own `/ui-bridge/` routes, `PR #9875` or a CJK `\u{9875}` escape.
#[test]
fn runner_refinements_restore_bare_sibling_names_and_port_constants() {
    let st = Structure::new();
    let classes = compile_classes();
    let hits = |src: &str| -> BTreeSet<(String, String)> {
        scan_source(&st, &classes, "fixture.rs", src, false)
            .into_iter()
            .map(|h| (h.class, h.symbol))
            .collect()
    };
    let src = "fn a(w: &Path) -> PathBuf { w.join(\"qontinui-web\") }\n\
               fn b() -> &'static str { \"**/qontinui-mobile/**\" }\n\
               fn c() -> [&'static str; 2] { [\"qontinui\", \"multistate\"] }\n\
               fn d() -> &'static str { \"ui-bridge\" }\n\
               fn e() -> (&'static str, u16) { (\"supervisor\", 9875) }\n\
               fn f() -> u16 { 9875u16 }\n";
    let got = hits(src);
    for (class, sym) in [
        ("repo_layout", "a"),
        ("repo_layout", "b"),
        ("repo_layout", "c"),
        ("repo_layout", "d"),
        ("supervisor_dependency", "e"),
        ("supervisor_dependency", "f"),
    ] {
        assert!(
            got.contains(&(class.to_string(), sym.to_string())),
            "expected ({class}, {sym}) in {got:?}"
        );
    }
    let clean = "fn g() -> &'static str { \"https://github.com/qontinui/qontinui-web/pull/1\" }\n\
                 fn h() -> &'static str { \"qontinui/qontinui-web\" }\n\
                 fn i() -> &'static str { \"/ui-bridge/control/page\" }\n\
                 fn j() -> &'static str { \"see PR #9875\" }\n\
                 fn k() -> char { '\\u{9875}' }\n\
                 fn l() -> f64 { 1.9875 }\n\
                 fn m() -> &'static str { \"qontinui-runner\" }\n";
    let got = hits(clean);
    assert!(got.is_empty(), "look-alikes produced hits: {got:?}");
}

fn git_ok(repo: &Path, args: &[&str]) {
    let mut full = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ];
    full.extend_from_slice(args);
    git_in(repo, &full).unwrap_or_else(|e| panic!("{e}"));
}

fn write_roster(repo: &Path, rows: Vec<Row>) {
    let dir = repo.join("docs");
    fs::create_dir_all(&dir).expect("mkdir docs");
    let roster = Roster {
        schema: 1,
        generator: "t".into(),
        plan: "t".into(),
        rows,
    };
    fs::write(dir.join("workspace-assumptions.json"), to_json(&roster)).expect("write roster");
}

/// Both bypasses of a working-tree base: (a) acknowledge → regenerate →
/// delete the acknowledgement, and (b) hand-edit the JSON. Either way the
/// working-tree roster CARRIES the new key; the merge-base roster does not,
/// so the rule still names the row — committed or not.
#[test]
fn swap_rule_base_is_the_merge_base_not_the_working_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git_ok(repo, &["init", "-q", "-b", "main"]);
    write_roster(repo, vec![row("dev_ports", "old", "unreviewed")]);
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
    git_ok(repo, &["checkout", "-q", "-b", "feature"]);

    let fresh = vec![
        row("dev_ports", "old", "unreviewed"),
        row("dev_ports", "brand_new", "unreviewed"),
    ];
    // The bypass state: the working-tree (and even committed) roster already
    // carries `brand_new`, and no acknowledgement names it.
    write_roster(repo, fresh.clone());
    for commit in [false, true] {
        if commit {
            git_ok(repo, &["commit", "-q", "-am", "regenerated / hand-edited"]);
        }
        let working: Roster = serde_json::from_str(
            &fs::read_to_string(repo.join("docs/workspace-assumptions.json")).expect("read"),
        )
        .expect("parse");
        assert!(
            unacknowledged_new_keys(&fresh, &working.rows, &BTreeSet::new()).is_empty(),
            "precondition: a working-tree base would admit the row"
        );
        let (label, base) = merge_base_roster(repo).unwrap_or_else(|e| panic!("{e}"));
        assert!(label.contains("merge-base(main)"), "{label}");
        let got = unacknowledged_new_keys(&fresh, &base, &BTreeSet::new());
        assert_eq!(got.len(), 1, "commit={commit}: {got:?}");
        assert!(got[0].contains(":: brand_new ::"), "{got:?}");
    }
}

/// No main ref to take a merge base against is UNKNOWN with a reason, not an
/// empty base (which would flag everything) and not a pass.
#[test]
fn swap_rule_base_unreadable_is_an_error_with_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    git_ok(repo, &["init", "-q", "-b", "trunk"]);
    write_roster(repo, vec![]);
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-q", "-m", "only"]);
    let err = merge_base_roster(repo).expect_err("no main ref");
    // Both refs must be tried and reported: `origin/main: …` first, then the
    // local-`main` fallback after the `; ` separator (`"origin/main:"` alone
    // already contains `main:`, so that would not prove the fallback ran).
    assert!(err.starts_with("origin/main:"), "{err}");
    assert!(err.contains("; main:"), "{err}");
}

/// The widened bare-port refinement: suffixes and range ends hit; a
/// fractional `9875.0` does not.
#[test]
fn bare_port_refinement_covers_suffixes_and_ranges() {
    let st = Structure::new();
    let classes = compile_classes();
    let sup = |src: &str| {
        scan_source(&st, &classes, "fixture.rs", src, false)
            .iter()
            .any(|h| h.class == "supervisor_dependency")
    };
    for hit in [
        "fn a() -> u64 { 9875u64 }",
        "fn a() -> i32 { 9875_i32 }",
        "fn a() -> u16 { 9875_u16 }",
        "fn a() { for p in 0..9875 {} }",
        "fn a() { for p in 0..=9875 {} }",
        "fn a() -> String { 9875.to_string() }",
    ] {
        assert!(sup(hit), "expected a hit: {hit}");
    }
    for miss in [
        "fn a() -> f64 { 9875.0 }",
        "fn a() -> f64 { 1.9875 }",
        "fn a() -> u32 { 98750 }",
    ] {
        assert!(!sup(miss), "expected no hit: {miss}");
    }
}
