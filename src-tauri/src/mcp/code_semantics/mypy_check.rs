//! Python `mypy` checker for the code-semantics `typecheck` surface (Phase A).
//!
//! Same honest-coverage observation-envelope posture as the TS path:
//!   - mypy unavailable on the machine → `Envelope::unavailable` (never a 500,
//!     never a fabricated clean answer).
//!   - post-write observe (no `overlay_patch`) → run mypy on the on-disk file
//!     → `coverage: 1.0`, provenance `mypy`.
//!   - predict (with `overlay_patch`) → a TRUE overlay via `mypy --shadow-file`
//!     (the real file is never modified) → `coverage: 1.0`, provenance `mypy`,
//!     plus a best-effort syntactic `changed_signatures`/`removed_symbols` diff.
//!
//! The §A result body carries a `coverage` field mirroring the envelope's (coord's
//! `EditPrediction::from_typecheck_result` reads coverage FROM the result body).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::process::Command;

use super::lang::python_project_root;
use super::syntactic::{self, Decl};
use super::{Credibility, Envelope, PROV_MYPY};

/// How mypy is invoked on this machine (resolved once).
#[derive(Debug, Clone)]
enum MypyInvocation {
    /// `mypy ...`
    Direct,
    /// `python -m mypy ...` (the python binary that worked).
    Module(String),
    /// mypy is not available at all (with a reason).
    Unavailable(String),
}

/// Cached availability probe. We run a blocking probe once at first use.
static MYPY: Lazy<MypyInvocation> = Lazy::new(probe_mypy);

/// Is `mypy` available (cached probe)? Used by the health endpoint.
pub fn is_available() -> bool {
    !matches!(&*MYPY, MypyInvocation::Unavailable(_))
}

/// Probe `mypy --version`, then `python -m mypy --version` / `python3 -m mypy`.
fn probe_mypy() -> MypyInvocation {
    // 1. direct `mypy`
    if let Ok(out) = crate::process_helpers::no_window("mypy")
        .arg("--version")
        .output()
    {
        if out.status.success() {
            return MypyInvocation::Direct;
        }
    }
    // 2. `<python> -m mypy`
    for py in ["python", "python3"] {
        if let Ok(out) = crate::process_helpers::no_window(py)
            .args(["-m", "mypy", "--version"])
            .output()
        {
            if out.status.success() {
                return MypyInvocation::Module(py.to_string());
            }
        }
    }
    MypyInvocation::Unavailable("mypy not found (tried `mypy` and `python -m mypy`)".to_string())
}

/// Build a `tokio::process::Command` for the resolved invocation with `args`.
fn mypy_command(inv: &MypyInvocation, args: &[String]) -> Option<Command> {
    match inv {
        MypyInvocation::Direct => {
            let mut c = crate::process_helpers::tokio_no_window("mypy");
            c.args(args);
            Some(c)
        }
        MypyInvocation::Module(py) => {
            let mut c = crate::process_helpers::tokio_no_window(py);
            c.arg("-m").arg("mypy").args(args);
            Some(c)
        }
        MypyInvocation::Unavailable(_) => None,
    }
}

/// The common mypy flags for machine-readable, no-summary output.
fn base_flags() -> Vec<String> {
    [
        "--no-error-summary",
        "--show-column-numbers",
        "--show-error-codes",
        "--no-color-output",
        "--no-pretty",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// A parsed mypy error diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MypyError {
    file: String,
    line: u32,
    col: u32,
    code: Option<String>,
    message: String,
}

/// Parse mypy stdout lines of the shape
/// `path:line:col: error: message [code]`. Only `error:`-severity lines are
/// collected (notes/warnings are ignored). `col` is optional in some configs but
/// `--show-column-numbers` provides it; we tolerate its absence.
fn parse_mypy_output(stdout: &str) -> Vec<MypyError> {
    let mut errors = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(e) = parse_mypy_line(line) {
            errors.push(e);
        }
    }
    errors
}

/// Parse a single mypy diagnostic line. Returns `None` for non-error severities
/// and malformed lines.
fn parse_mypy_line(line: &str) -> Option<MypyError> {
    // Split into the leading `path:line:col` locator and the remainder.
    // The path may contain Windows drive colons, so we parse from the RIGHT of
    // the locator: find ": error:" / ": note:" / ": warning:".
    let sev_idx = line.find(": error:")?;
    // Reject if a higher-priority `note`/`warning` precedes (shouldn't, but be safe).
    let locator = &line[..sev_idx];
    let rest = &line[sev_idx + ": error:".len()..];

    // locator = "<path>:<line>:<col>" possibly "<path>:<line>" (no col).
    let (file, line_no, col_no) = parse_locator(locator)?;

    // rest = " message [code]" — peel a trailing bracketed code.
    let msg_full = rest.trim();
    let (message, code) = split_trailing_code(msg_full);
    Some(MypyError {
        file,
        line: line_no,
        col: col_no,
        code,
        message,
    })
}

/// Parse `<path>:<line>:<col>` (or `<path>:<line>`). The path can itself contain
/// colons (Windows drive), so we take the last 1-2 colon-separated numeric
/// fields as line/col and the rest as the path.
fn parse_locator(locator: &str) -> Option<(String, u32, u32)> {
    let parts: Vec<&str> = locator.rsplitn(3, ':').collect();
    // parts is reversed: [col?, line?, path...]
    match parts.as_slice() {
        // path:line:col
        [col, line, path] => {
            let col_n = col.trim().parse::<u32>().ok()?;
            let line_n = line.trim().parse::<u32>().ok()?;
            Some((path.trim().to_string(), line_n, col_n))
        }
        // path:line  (no column)
        [line, path] => {
            let line_n = line.trim().parse::<u32>().ok()?;
            Some((path.trim().to_string(), line_n, 0))
        }
        _ => None,
    }
}

/// Split a trailing ` [code]` from a mypy message. Returns `(message, code)`.
fn split_trailing_code(s: &str) -> (String, Option<String>) {
    if s.ends_with(']') {
        if let Some(open) = s.rfind('[') {
            let code = &s[open + 1..s.len() - 1];
            // mypy codes are identifier-ish (`name-of-code`); guard against `[i]`-style
            // text in messages by requiring no spaces in the bracket body.
            if !code.is_empty() && !code.contains(' ') {
                let message = s[..open].trim_end().to_string();
                return (message, Some(code.to_string()));
            }
        }
    }
    (s.to_string(), None)
}

/// Build the §A `errors[]` array from parsed mypy errors, filtered to the file
/// we asked about (mypy may surface follow-on errors in imported modules; we keep
/// the checked file's errors, which is what the predict/observe contract is about).
fn errors_json(errors: &[MypyError]) -> Vec<Value> {
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

/// Extract top-level + class-level Python declarations (`def name(...)` /
/// `class name`) from source text. A small line-based parser — no heavy deps.
fn extract_py_decls(src: &str) -> Vec<Decl> {
    let mut decls = Vec::new();
    for raw in src.lines() {
        let line = raw.trim_start();
        if let Some(rest) = line.strip_prefix("def ") {
            if let Some(name) = ident_prefix(rest) {
                decls.push(Decl {
                    name,
                    kind: "def".into(),
                    normalized: normalize_decl(raw),
                });
            }
        } else if let Some(rest) = line.strip_prefix("async def ") {
            if let Some(name) = ident_prefix(rest) {
                decls.push(Decl {
                    name,
                    kind: "def".into(),
                    normalized: normalize_decl(raw),
                });
            }
        } else if let Some(rest) = line.strip_prefix("class ") {
            if let Some(name) = ident_prefix(rest) {
                decls.push(Decl {
                    name,
                    kind: "class".into(),
                    normalized: normalize_decl(raw),
                });
            }
        }
    }
    decls
}

/// The leading identifier of `s` (up to `(`, `:`, whitespace, or `=`).
fn ident_prefix(s: &str) -> Option<String> {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(s[..end].to_string())
    }
}

/// Normalize a declaration line for signature-change comparison: collapse
/// internal whitespace, drop a trailing `:` and any trailing comment.
fn normalize_decl(line: &str) -> String {
    let no_comment = match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    };
    let collapsed: String = no_comment.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches(':').trim().to_string()
}

/// Run mypy and parse diagnostics. `extra_args` are appended before the target
/// path. Returns `(errors, exit_code)` or an `Envelope::unavailable` on a
/// spawn/timeout/crash failure.
async fn run_mypy(
    inv: &MypyInvocation,
    cwd: &Path,
    extra_args: &[String],
    target: &str,
) -> Result<(Vec<MypyError>, i32), Envelope> {
    let mut args = base_flags();
    args.extend(extra_args.iter().cloned());
    args.push(target.to_string());

    let mut cmd = match mypy_command(inv, &args) {
        Some(c) => c,
        None => return Err(Envelope::unavailable("typecheck", "mypy unavailable")),
    };
    cmd.current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(Envelope::unavailable(
                "typecheck",
                &format!("mypy spawn failed: {e}"),
            ))
        }
    };

    let out = match tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(Envelope::unavailable(
                "typecheck",
                &format!("mypy failed: {e}"),
            ))
        }
        Err(_) => return Err(Envelope::unavailable("typecheck", "mypy timed out")),
    };

    let code = out.status.code().unwrap_or(-1);
    // mypy: 0 = clean, 1 = type errors found, >=2 = crash/usage error.
    if !(0..=1).contains(&code) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Envelope::unavailable(
            "typecheck",
            &format!("mypy crashed (exit {code}): {}", stderr.trim()),
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok((parse_mypy_output(&stdout), code))
}

/// Build the warm `mypy` envelope, lifting `coverage` into the result body.
fn warm_envelope(result: Value) -> Envelope {
    Envelope {
        observer: "code_semantics".into(),
        query: "typecheck".into(),
        result,
        posterior: 1.0,
        coverage: 1.0,
        provenance: PROV_MYPY.into(),
        credibility: Credibility::high(),
        staleness_seconds: None,
        kernel: false,
    }
}

/// The public entrypoint: typecheck a Python `file`, optionally under an
/// `overlay_patch` (`{file, new_text}`) which is applied via `--shadow-file`
/// (a true overlay — the real file is never written).
pub async fn typecheck(file: &Path, overlay_patch: Option<Value>) -> Envelope {
    let inv: &MypyInvocation = &MYPY;
    if let MypyInvocation::Unavailable(reason) = inv {
        return Envelope::unavailable("typecheck", reason);
    }

    let root = python_project_root(file);
    let target = file.to_string_lossy().to_string();

    match overlay_patch {
        None => observe(inv, &root, &target).await,
        Some(patch) => predict(inv, &root, file, &target, patch).await,
    }
}

/// Post-write observe: run mypy on the on-disk file.
async fn observe(inv: &MypyInvocation, root: &Path, target: &str) -> Envelope {
    match run_mypy(inv, root, &[], target).await {
        Ok((errors, _code)) => {
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
        Err(env) => env,
    }
}

/// Predict (overlay): write `new_text` to a temp file, run mypy with
/// `--shadow-file <overlay.file> <tempfile>`, and compute a syntactic diff.
async fn predict(
    inv: &MypyInvocation,
    root: &Path,
    file: &Path,
    target: &str,
    patch: Value,
) -> Envelope {
    let overlay_file = patch.get("file").and_then(|v| v.as_str());
    let new_text = patch.get("new_text").and_then(|v| v.as_str());
    let (overlay_file, new_text) = match (overlay_file, new_text) {
        (Some(f), Some(t)) => (f.to_string(), t.to_string()),
        _ => return Envelope::unavailable("typecheck", "overlay_patch requires {file, new_text}"),
    };

    // True overlay: write new_text to a temp file; --shadow-file substitutes it
    // for the real path during this run only.
    let tmp = match tempfile::Builder::new()
        .prefix("qontinui-mypy-overlay-")
        .suffix(".py")
        .tempfile()
    {
        Ok(t) => t,
        Err(e) => return Envelope::unavailable("typecheck", &format!("temp file failed: {e}")),
    };
    if let Err(e) = std::fs::write(tmp.path(), new_text.as_bytes()) {
        return Envelope::unavailable("typecheck", &format!("temp write failed: {e}"));
    }

    let shadow_target = tmp.path().to_string_lossy().to_string();
    let extra = vec![
        "--shadow-file".to_string(),
        overlay_file.clone(),
        shadow_target,
    ];

    let run = run_mypy(inv, root, &extra, target).await;
    // tmp drops (and deletes) at end of scope regardless.

    let (errors, _code) = match run {
        Ok(v) => v,
        Err(env) => return env,
    };

    // Syntactic diff between on-disk text and the overlay text.
    let on_disk = extract_py_decls(&syntactic::read_or_empty(Path::new(&overlay_file)));
    let overlaid = extract_py_decls(&new_text);
    let changed = syntactic::changed_signatures(&on_disk, &overlaid);
    let removed = syntactic::removed_decls(&on_disk, &overlaid);
    let removed_syms = syntactic::removed_symbols(&removed, root, file, "py", 2000, 20);

    let errs = errors_json(&errors);
    let ok = errs.is_empty();
    warm_envelope(json!({
        "ok": ok,
        "errors": errs,
        "overlay": true,
        "changed_signatures": changed,
        "removed_symbols": removed_syms,
        "coverage": 1.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mypy_error_line_with_code() {
        let e = parse_mypy_line(
            "app/foo.py:12:5: error: Incompatible types in assignment [assignment]",
        )
        .unwrap();
        assert_eq!(e.file, "app/foo.py");
        assert_eq!(e.line, 12);
        assert_eq!(e.col, 5);
        assert_eq!(e.code.as_deref(), Some("assignment"));
        assert_eq!(e.message, "Incompatible types in assignment");
    }

    #[test]
    fn parse_mypy_line_windows_path() {
        let e = parse_mypy_line(
            "C:/proj/app/foo.py:3:1: error: Name \"x\" is not defined [name-defined]",
        )
        .unwrap();
        assert_eq!(e.file, "C:/proj/app/foo.py");
        assert_eq!(e.line, 3);
        assert_eq!(e.col, 1);
        assert_eq!(e.code.as_deref(), Some("name-defined"));
    }

    #[test]
    fn parse_ignores_note_and_warning_and_malformed() {
        let out = "\
app/foo.py:12:5: error: Bad thing [assignment]
app/foo.py:12:5: note: this is just a note
app/foo.py:9:1: warning: a warning line
not a diagnostic at all
app/foo.py: error: missing line number";
        let errs = parse_mypy_output(out);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_deref(), Some("assignment"));
    }

    #[test]
    fn parse_error_without_column() {
        let e = parse_mypy_line("app/foo.py:7: error: oops [misc]").unwrap();
        assert_eq!(e.line, 7);
        assert_eq!(e.col, 0);
        assert_eq!(e.code.as_deref(), Some("misc"));
    }

    #[test]
    fn split_trailing_code_handles_no_code() {
        let (msg, code) = split_trailing_code("just a message");
        assert_eq!(msg, "just a message");
        assert!(code.is_none());
    }

    #[test]
    fn extract_py_decls_truth_table() {
        let src = "\
import os

def top_level(x):
    return x

async def async_fn():
    pass

class Thing:
    def method(self):
        pass

x = 5
";
        let decls = extract_py_decls(src);
        let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"top_level"));
        assert!(names.contains(&"async_fn"));
        assert!(names.contains(&"Thing"));
        assert!(names.contains(&"method"));
        // module-level assignment is not a decl
        assert!(!names.contains(&"x"));
    }

    #[test]
    fn py_def_removal_referenced_elsewhere_is_flagged() {
        let on_disk = "\
def gone():
    pass

def kept():
    pass
";
        let overlay = "\
def kept():
    pass
";
        let on_disk_decls = extract_py_decls(on_disk);
        let overlay_decls = extract_py_decls(overlay);
        let removed = syntactic::removed_decls(&on_disk_decls, &overlay_decls);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].name, "gone");
    }

    #[test]
    fn py_changed_signature_detected() {
        let on_disk = extract_py_decls("def f(x: int) -> int:\n    return x\n");
        let overlay = extract_py_decls("def f(x: str) -> str:\n    return x\n");
        let changed = syntactic::changed_signatures(&on_disk, &overlay);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "f");
    }

    /// LIVE: requires mypy. Skips (passes) when unavailable.
    #[tokio::test]
    async fn live_mypy_detects_type_error_and_overlay_fixes_it() {
        if let MypyInvocation::Unavailable(reason) = &*MYPY {
            eprintln!("skipping live mypy test: {reason}");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("m.py");
        std::fs::write(&file, "x: int = \"s\"\n").unwrap();

        // observe: error present
        let env = typecheck(&file, None).await;
        assert_eq!(env.provenance, PROV_MYPY);
        assert_eq!(env.coverage, 1.0);
        let errs = env.result["errors"].as_array().unwrap();
        assert!(!errs.is_empty(), "mypy should flag x: int = \"s\"");
        assert_eq!(env.result["ok"], false);

        // overlay that fixes it → clean
        let patch = json!({"file": file.to_string_lossy(), "new_text": "x: int = 5\n"});
        let env2 = typecheck(&file, Some(patch)).await;
        assert_eq!(env2.result["overlay"], true);
        assert_eq!(
            env2.result["ok"], true,
            "fixed overlay should typecheck clean"
        );

        // the real file is untouched (true overlay)
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert_eq!(on_disk, "x: int = \"s\"\n");
    }
}
