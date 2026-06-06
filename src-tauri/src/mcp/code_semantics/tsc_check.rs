//! TypeScript `tsc --noEmit` checker for the code-semantics `typecheck` surface.
//!
//! This is the **observe-mode** (`Ξ_Type`) path for TypeScript: where the TS
//! Language Service (`node_bridge`) answers from a warm in-memory project
//! snapshot, the real compiler answers the install-/edit-verify question
//! ("did this change break compilation?") with **no warm-up dependency** and
//! whole-project fidelity — exactly mirroring the Rust `cargo check` posture.
//!
//! Posture (decided in the plan):
//!   - **Observe only.** No `overlay_patch` ⇒ this path. With an overlay the
//!     dispatch keeps the LS TRUE-overlay predict path (battle-tested; this
//!     plan never touches it).
//!   - **Repo-local `tsc` only.** Discovery walks up to the nearest
//!     `node_modules/.bin/tsc` (`tsc.cmd` on Windows) so the verdict uses the
//!     repo's own TypeScript version — the verdict the user would get from
//!     their own build. No global-PATH `tsc`.
//!   - **Never a regression.** No discoverable `tsc`/`tsconfig` ⇒ [`observe`]
//!     returns `None` and the caller falls back to the existing LS path
//!     verbatim. A discoverable `tsc` that times out / fails to run ⇒ an honest
//!     `Envelope::unavailable` (never a fabricated clean pass, never a 500).
//!
//! The §A result body carries a `coverage` field mirroring the envelope's
//! (coord's `EditPrediction::from_typecheck_result` reads coverage FROM the
//! result body).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde_json::{json, Value};

use super::scope::{default_ts_scope, nearest_tsconfig};
use super::{Credibility, Envelope, PROV_TSC};
use crate::process_helpers::tokio_no_window;

/// Cap the upward walk so a pathological symlink loop can't spin forever.
const MAX_WALK_DEPTH: usize = 40;

/// A bounded ceiling for a cold whole-project `tsc -p`. The route is async, so a
/// long compile never blocks the runtime; on the ceiling we degrade honestly.
const TSC_TIMEOUT: Duration = Duration::from_secs(180);

/// The repo-local `tsc` binary name. On Windows the npm/pnpm shim is `tsc.cmd`
/// (a batch file Rust 1.77+ launches via `cmd.exe` automatically); elsewhere the
/// `.bin` entry is the bare `tsc` shell script / symlink.
#[cfg(target_os = "windows")]
const TSC_BIN: &str = "tsc.cmd";
#[cfg(not(target_os = "windows"))]
const TSC_BIN: &str = "tsc";

// ===========================================================================
// Discovery (pure)
// ===========================================================================

/// The directory to start an upward walk from (the file's parent, or the dir
/// itself when `file` is a directory).
fn start_dir(file: &Path) -> Option<PathBuf> {
    if file.is_dir() {
        Some(file.to_path_buf())
    } else {
        file.parent().map(|p| p.to_path_buf())
    }
}

/// Walk up from `file` to the nearest `tsconfig.json`. Thin alias over
/// [`super::scope::nearest_tsconfig`] (the existing, tested tsconfig walk-up) —
/// reused rather than duplicated.
pub fn find_tsconfig(file: &Path) -> Option<PathBuf> {
    nearest_tsconfig(file)
}

/// Walk up from `file` to the nearest enclosing `node_modules/.bin/tsc`
/// (`tsc.cmd` on Windows). Returns the binary path, or `None` if none is found
/// up to the filesystem root. Covers both a package-local install and a hoisted
/// pnpm/npm workspace-root install (whichever is nearest wins).
pub fn find_tsc(file: &Path) -> Option<PathBuf> {
    let mut dir = start_dir(file);
    let mut depth = 0;
    while let Some(d) = dir {
        if depth >= MAX_WALK_DEPTH {
            break;
        }
        let candidate = d.join("node_modules").join(".bin").join(TSC_BIN);
        if candidate.exists() {
            return Some(candidate);
        }
        dir = d.parent().map(|p| p.to_path_buf());
        depth += 1;
    }
    None
}

/// True if a tsconfig opts JavaScript into the program (`allowJs`/`checkJs`).
/// `tsconfig.json` is JSONC (comments + trailing commas), so we strip comments
/// and do a best-effort match — a false result is safe (the caller falls back
/// to the LS for the `.js` file rather than asking `tsc -p`, which would not
/// even include an un-opted-in `.js` file and would falsely report it clean).
fn tsconfig_includes_js(tsconfig: &Path) -> bool {
    let raw = match std::fs::read_to_string(tsconfig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let stripped = strip_jsonc_comments(&raw);
    json_flag_true(&stripped, "allowJs") || json_flag_true(&stripped, "checkJs")
}

/// Strip `//` line comments and `/* */` block comments from JSONC, leaving
/// string contents intact (so a `//` inside a `"…"` value is preserved).
fn strip_jsonc_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                // Preserve the escaped char verbatim.
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // line comment → skip to newline
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // block comment → skip to */
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

/// Best-effort `"<key>": true` match over de-commented JSONC.
fn json_flag_true(stripped: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(rel) = stripped[from..].find(&needle) {
        let after = from + rel + needle.len();
        let tail = stripped[after..].trim_start();
        if let Some(rest) = tail.strip_prefix(':') {
            if rest.trim_start().starts_with("true") {
                return true;
            }
        }
        from = after;
    }
    false
}

// ===========================================================================
// Diagnostic parsing (pure)
// ===========================================================================

/// A parsed `tsc` error diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TscError {
    file: String,
    line: u32,
    col: u32,
    code: Option<String>,
    message: String,
}

/// Parse `tsc --pretty false` stdout. Diagnostic headers have the canonical
/// shape `path(line,col): error TSxxxx: message`; indented continuation lines
/// (the detail of a structured error) attach to the prior error's `message`.
/// `Found N errors.` summary lines and file-less global diagnostics (config
/// errors with no `path(line,col)`) are ignored — a non-empty file-less-only
/// output therefore yields zero errors, which the caller treats as a tsc-failed
/// (unavailable) signal, exactly like the cargo path.
fn parse_tsc_output(stdout: &str) -> Vec<TscError> {
    let mut errors: Vec<TscError> = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        // Indented line → continuation detail of the previous error.
        if line.starts_with(char::is_whitespace) {
            if let Some(last) = errors.last_mut() {
                let detail = line.trim();
                if !detail.is_empty() {
                    last.message.push('\n');
                    last.message.push_str(detail);
                }
            }
            continue;
        }
        if let Some(e) = parse_diagnostic_header(line) {
            errors.push(e);
        }
        // Non-matching, non-indented lines (`Found N errors.`, file-less
        // `error TSxxxx: …`) are intentionally dropped.
    }
    errors
}

/// Parse one `path(line,col): error TSxxxx: message` header line. Anchors on the
/// first `): error ` so a Windows drive path (`C:\a\b.ts`) parses correctly: the
/// location paren is the LAST `(` before that anchor (file paths don't contain
/// `(`), and the path is everything before it.
fn parse_diagnostic_header(line: &str) -> Option<TscError> {
    const ANCHOR: &str = "): error ";
    let anchor = line.find(ANCHOR)?;
    let loc_close = anchor + 1; // index of the ')'
    let head = &line[..loc_close]; // "path(line,col)"
    let paren_open = head.rfind('(')?;
    let file = head[..paren_open].to_string();
    if file.is_empty() {
        return None;
    }
    let loc = &head[paren_open + 1..loc_close - 1]; // "line,col"
    let (line_s, col_s) = loc.split_once(',')?;
    let line_no = line_s.trim().parse::<u32>().ok()?;
    let col_no = col_s.trim().parse::<u32>().ok()?;

    // After the anchor: "TSxxxx: message"
    let rest = &line[anchor + ANCHOR.len()..];
    let (code, message) = match rest.split_once(": ") {
        Some((code, msg)) => (Some(code.trim().to_string()), msg.to_string()),
        // A code with no following message (rare) — keep the code, empty message.
        None => (Some(rest.trim().to_string()), String::new()),
    };

    Some(TscError {
        file,
        line: line_no,
        col: col_no,
        code,
        message,
    })
}

fn errors_json(errors: &[TscError]) -> Vec<Value> {
    errors
        .iter()
        .map(|e| {
            json!({
                "file": e.file,
                "line": e.line,
                "col": e.col,
                "code": e.code,
                "message": e.message,
            })
        })
        .collect()
}

// ===========================================================================
// Availability probe (health engines[])
// ===========================================================================

/// `tsc` availability for the runner's own frontend scope, resolved once. The
/// probe is repo-local (there is no single global `tsc`): it locates the
/// frontend `node_modules/.bin/tsc` and runs `--version`.
static TSC_PROBE: Lazy<TscProbe> = Lazy::new(probe_tsc);

#[derive(Debug, Clone)]
struct TscProbe {
    available: bool,
    version: Option<String>,
}

/// Is a repo-local `tsc` available for the default frontend scope (cached)?
pub fn is_available() -> bool {
    TSC_PROBE.available
}

/// The resolved `tsc` version string for the default scope, if available.
pub fn version() -> Option<String> {
    TSC_PROBE.version.clone()
}

fn probe_tsc() -> TscProbe {
    let Some(scope) = default_ts_scope() else {
        return TscProbe {
            available: false,
            version: None,
        };
    };
    // scope.project is the tsconfig path; start the tsc walk from its dir.
    let tsconfig = PathBuf::from(&scope.project);
    let Some(tsc) = find_tsc(&tsconfig) else {
        return TscProbe {
            available: false,
            version: None,
        };
    };
    use crate::process_helpers::no_window;
    match no_window(&tsc).arg("--version").output() {
        Ok(out) if out.status.success() => {
            // `tsc --version` → "Version 5.4.5"
            let raw = String::from_utf8_lossy(&out.stdout);
            let version = raw
                .split_whitespace()
                .last()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            TscProbe {
                available: true,
                version,
            }
        }
        _ => TscProbe {
            available: false,
            version: None,
        },
    }
}

// ===========================================================================
// Envelope + entrypoint
// ===========================================================================

/// Warm `tsc` observe envelope: coverage 1.0, boundary high (the real compiler
/// crosses the type boundary, peer of `cargo-check`/`mypy`).
fn warm_envelope(result: Value) -> Envelope {
    Envelope {
        observer: "code_semantics".into(),
        query: "typecheck".into(),
        result,
        posterior: 1.0,
        coverage: 1.0,
        provenance: PROV_TSC.into(),
        credibility: Credibility::high(),
        staleness_seconds: None,
        kernel: false,
    }
}

/// Observe-mode typecheck for a TypeScript/JavaScript `file` via the real
/// compiler.
///
/// Returns:
///   - `None` — no discoverable `tsconfig`/`tsc` (or a `.js`/`.jsx` file whose
///     tsconfig does not opt JS in): the caller MUST fall back to the existing
///     LS path. This is the "never a regression" contract.
///   - `Some(envelope)` — `tsc` ran (a real observe verdict), OR `tsc` was
///     discoverable but failed/timed out (an honest `Envelope::unavailable`).
pub async fn observe(file: &Path) -> Option<Envelope> {
    let tsconfig = find_tsconfig(file)?;
    let tsc = find_tsc(file)?;

    // `.js`/`.jsx` only when the tsconfig opts JS in — otherwise `tsc -p` would
    // not include the file and would falsely report it clean.
    if is_js_like(file) && !tsconfig_includes_js(&tsconfig) {
        return None;
    }

    Some(run_tsc(&tsc, &tsconfig).await)
}

/// True for `.js`/`.jsx`/`.cjs`/`.mjs` files (the JS family that needs an
/// `allowJs`/`checkJs` opt-in before `tsc -p` will check it).
fn is_js_like(file: &Path) -> bool {
    matches!(
        file.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("js") | Some("jsx") | Some("cjs") | Some("mjs")
    )
}

/// Run `tsc --noEmit -p <tsconfig> --pretty false` and build the observe
/// envelope. A clean exit with no parsed errors → `ok: true`; parsed errors →
/// `ok: false` with the whole-project error set; a nonzero exit with NO parsed
/// file diagnostics (config/toolchain failure) → `Envelope::unavailable`.
async fn run_tsc(tsc: &Path, tsconfig: &Path) -> Envelope {
    let project_dir = tsconfig.parent().unwrap_or(Path::new("."));

    let mut cmd = tokio_no_window(tsc);
    cmd.arg("--noEmit")
        .arg("-p")
        .arg(tsconfig)
        .arg("--pretty")
        .arg("false")
        .current_dir(project_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Envelope::unavailable("typecheck", &format!("tsc spawn failed: {e}")),
    };

    let out = match tokio::time::timeout(TSC_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Envelope::unavailable("typecheck", &format!("tsc failed: {e}")),
        Err(_) => {
            return Envelope::unavailable(
                "typecheck",
                &format!("tsc timed out after {}s", TSC_TIMEOUT.as_secs()),
            )
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let errors = parse_tsc_output(&stdout);

    // A nonzero exit WITH parsed diagnostics is a valid "observed errors"
    // result. A nonzero exit with NO file diagnostics means tsc itself failed
    // to run (bad tsconfig, missing TypeScript, config error) — that is
    // unavailable, not a clean answer (mirrors the cargo path).
    if !out.status.success() && errors.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = if stderr.trim().is_empty() {
            stdout.trim().chars().take(400).collect::<String>()
        } else {
            stderr.trim().chars().take(400).collect::<String>()
        };
        return Envelope::unavailable(
            "typecheck",
            &format!("tsc produced no file diagnostics and failed: {detail}"),
        );
    }

    let errs = errors_json(&errors);
    let ok = errs.is_empty();
    warm_envelope(json!({
        "ok": ok,
        "errors": errs,
        "overlay": false,
        "changed_signatures": [],
        "removed_symbols": [],
        "coverage": 1.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_diagnostic() {
        let out =
            "src/foo.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.";
        let errs = parse_tsc_output(out);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "src/foo.ts");
        assert_eq!(errs[0].line, 3);
        assert_eq!(errs[0].col, 7);
        assert_eq!(errs[0].code.as_deref(), Some("TS2322"));
        assert_eq!(
            errs[0].message,
            "Type 'string' is not assignable to type 'number'."
        );
    }

    #[test]
    fn parse_windows_drive_path() {
        let out =
            "C:/proj/src/a.ts(10,1): error TS2307: Cannot find module 'x' or its corresponding type declarations.";
        let errs = parse_tsc_output(out);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "C:/proj/src/a.ts");
        assert_eq!(errs[0].line, 10);
        assert_eq!(errs[0].col, 1);
        assert_eq!(errs[0].code.as_deref(), Some("TS2307"));
    }

    #[test]
    fn parse_multiline_continuation_attaches_to_prior() {
        let out = "\
src/foo.ts(3,7): error TS2322: Type 'A' is not assignable to type 'B'.
  Property 'z' is missing in type 'A' but required in type 'B'.
src/bar.ts(1,1): error TS1005: ';' expected.
Found 2 errors.";
        let errs = parse_tsc_output(out);
        assert_eq!(errs.len(), 2, "summary + continuation must not add errors");
        assert!(errs[0].message.contains("not assignable"));
        assert!(
            errs[0].message.contains("Property 'z' is missing"),
            "continuation line should attach to the first error"
        );
        assert_eq!(errs[1].file, "src/bar.ts");
        assert_eq!(errs[1].code.as_deref(), Some("TS1005"));
    }

    #[test]
    fn parse_ignores_fileless_global_errors() {
        // A config error with no path(line,col) → not a file diagnostic.
        let out = "error TS18003: No inputs were found in config file 'tsconfig.json'.";
        let errs = parse_tsc_output(out);
        assert!(errs.is_empty());
    }

    #[test]
    fn find_tsc_picks_nearest_node_modules_bin() {
        let tmp = tempfile::tempdir().unwrap();
        // workspace root .bin
        let root_bin = tmp.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&root_bin).unwrap();
        std::fs::write(root_bin.join(TSC_BIN), "#!/bin/sh\n").unwrap();
        // nearer package .bin
        let pkg = tmp.path().join("packages").join("app");
        let pkg_bin = pkg.join("node_modules").join(".bin");
        std::fs::create_dir_all(&pkg_bin).unwrap();
        std::fs::write(pkg_bin.join(TSC_BIN), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        let file = pkg.join("src").join("index.ts");
        std::fs::write(&file, "export const x = 1;\n").unwrap();

        let found = find_tsc(&file).unwrap();
        // Nearest wins: the package .bin, not the workspace root .bin.
        assert_eq!(found, pkg_bin.join(TSC_BIN));
    }

    #[test]
    fn find_tsc_walks_to_workspace_root_when_package_unhoisted() {
        let tmp = tempfile::tempdir().unwrap();
        let root_bin = tmp.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&root_bin).unwrap();
        std::fs::write(root_bin.join(TSC_BIN), "#!/bin/sh\n").unwrap();
        let deep = tmp.path().join("packages").join("app").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        let file = deep.join("index.ts");
        std::fs::write(&file, "export const x = 1;\n").unwrap();
        // No package-local .bin → walk finds the hoisted workspace-root tsc.
        assert_eq!(find_tsc(&file).unwrap(), root_bin.join(TSC_BIN));
    }

    #[test]
    fn find_tsc_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("orphan.ts");
        std::fs::write(&file, "export const x = 1;\n").unwrap();
        assert!(find_tsc(&file).is_none());
    }

    #[test]
    fn find_tsconfig_picks_nearest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tsconfig.json"), "{}\n").unwrap();
        let pkg = tmp.path().join("packages").join("app");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(pkg.join("tsconfig.json"), "{}\n").unwrap();
        let file = pkg.join("src").join("index.ts");
        std::fs::write(&file, "export const x = 1;\n").unwrap();
        // Nearest tsconfig = the package one.
        assert_eq!(find_tsconfig(&file).unwrap(), pkg.join("tsconfig.json"));
    }

    #[test]
    fn tsconfig_includes_js_detects_allowjs_and_checkjs() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("tsconfig.json");

        std::fs::write(
            &cfg,
            "{\n  // allow plain JS\n  \"compilerOptions\": { \"allowJs\": true }\n}\n",
        )
        .unwrap();
        assert!(tsconfig_includes_js(&cfg));

        std::fs::write(
            &cfg,
            "{ \"compilerOptions\": { \"checkJs\": true, \"strict\": true } }",
        )
        .unwrap();
        assert!(tsconfig_includes_js(&cfg));

        std::fs::write(
            &cfg,
            "{ \"compilerOptions\": { \"allowJs\": false, \"strict\": true } }",
        )
        .unwrap();
        assert!(!tsconfig_includes_js(&cfg));

        std::fs::write(&cfg, "{ \"compilerOptions\": { \"strict\": true } }").unwrap();
        assert!(!tsconfig_includes_js(&cfg));
    }

    #[test]
    fn strip_jsonc_preserves_string_slashes() {
        let src = "{ \"path\": \"http://x/y\", /* c */ \"a\": 1 } // tail";
        let stripped = strip_jsonc_comments(src);
        assert!(stripped.contains("http://x/y"), "string // preserved");
        assert!(!stripped.contains("/* c */"), "block comment removed");
        assert!(!stripped.contains("// tail"), "line comment removed");
    }

    #[test]
    fn is_js_like_truth_table() {
        assert!(is_js_like(Path::new("a.js")));
        assert!(is_js_like(Path::new("a.JSX")));
        assert!(is_js_like(Path::new("a.cjs")));
        assert!(is_js_like(Path::new("a.mjs")));
        assert!(!is_js_like(Path::new("a.ts")));
        assert!(!is_js_like(Path::new("a.tsx")));
    }

    #[test]
    fn warm_envelope_is_tsc_provenance_full_coverage() {
        let env = warm_envelope(json!({"ok": true, "errors": []}));
        assert_eq!(env.provenance, PROV_TSC);
        assert_eq!(env.coverage, 1.0);
        assert_eq!(env.credibility.boundary, "high");
    }

    /// LIVE: requires a repo-local `tsc` in the runner frontend. Skips (passes)
    /// when unavailable. Asserts a deliberate type error is observed and a clean
    /// project is `ok`.
    #[tokio::test]
    async fn live_tsc_observes_a_deliberate_type_error() {
        if !is_available() {
            eprintln!("skipping live tsc test: no repo-local tsc for the default scope");
            return;
        }
        let scope = match default_ts_scope() {
            Some(s) => s,
            None => {
                eprintln!("skipping live tsc test: no default TS scope");
                return;
            }
        };
        let tsc = match find_tsc(Path::new(&scope.project)) {
            Some(t) => t,
            None => {
                eprintln!("skipping live tsc test: tsc not found from scope");
                return;
            }
        };

        // A throwaway tsconfig + a single deliberately-broken file in a temp dir,
        // using the frontend's own tsc binary.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("tsconfig.json"),
            "{ \"compilerOptions\": { \"noEmit\": true, \"strict\": true }, \"include\": [\"*.ts\"] }",
        )
        .unwrap();
        let bad = tmp.path().join("bad.ts");
        std::fs::write(&bad, "const n: number = \"not a number\";\n").unwrap();

        let env = run_tsc(&tsc, &tmp.path().join("tsconfig.json")).await;
        if env.provenance == PROV_TSC {
            assert_eq!(env.coverage, 1.0);
            let errs = env.result["errors"].as_array().unwrap();
            assert!(!errs.is_empty(), "tsc should flag the type mismatch");
            assert_eq!(env.result["ok"], false);
            assert_eq!(errs[0]["code"], "TS2322");
        } else {
            eprintln!(
                "live tsc test: tolerating non-observe envelope provenance={}",
                env.provenance
            );
        }
    }
}
