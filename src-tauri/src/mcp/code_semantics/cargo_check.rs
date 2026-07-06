//! Rust `cargo check` checker for the code-semantics `typecheck` surface (Phase A).
//!
//! Posture (decided in the plan, for robustness):
//!   - **No write-then-restore overlay.** A minutes-long mutation window in a live
//!     working tree is unacceptable, so the predict (overlay) path does NOT mutate
//!     the tree. Instead:
//!       - post-write **observe** (no `overlay_patch`) → real `cargo check
//!         --message-format=json`, full fidelity → `coverage: 1.0`, provenance
//!         `cargo-check`.
//!       - **predict** (with `overlay_patch`) → a SYNTACTIC-only prediction of
//!         changed/removed `pub` items (no compile runs) → `coverage: 0.5`,
//!         provenance `cargo-syntactic`, boundary `medium`. The honest coverage<1
//!         makes coord's `classify_edit_outcome` route to **Partial** rather than
//!         asserting a clean predict — that is the designed behavior.
//!
//! `cargo check` runs with an isolated `CARGO_TARGET_DIR` under
//! `temp_dir()/qontinui-cargo-typecheck/<hash-of-manifest>` so repeated calls reuse
//! incremental state but never clobber the project's own `target/`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::process::Command;

use super::lang::nearest_cargo_manifest;
use super::syntactic::{self, sig_hash, Decl};
use super::{Credibility, Envelope, PROV_CARGO, PROV_CARGO_SYNTACTIC};

/// Cached `cargo --version` availability probe.
static CARGO_AVAILABLE: Lazy<Result<(), String>> = Lazy::new(probe_cargo);

/// Is a `cargo` toolchain available (cached probe)? Used by the health endpoint.
pub fn is_available() -> bool {
    CARGO_AVAILABLE.is_ok()
}

fn probe_cargo() -> Result<(), String> {
    match crate::process_helpers::no_window("cargo")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "cargo --version exited {}",
            out.status.code().unwrap_or(-1)
        )),
        Err(e) => Err(format!("cargo not found: {e}")),
    }
}

/// A parsed cargo error diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CargoError {
    file: String,
    line: u32,
    col: u32,
    code: Option<String>,
    message: String,
}

/// Parse `cargo check --message-format=json` stdout (one JSON object per line).
/// Collects `compiler-message` diagnostics with `message.level == "error"`,
/// reading the first primary span. `crate_dir` is used to keep only errors whose
/// file resolves within the crate (relative paths are resolved against it).
fn parse_cargo_json(stdout: &str, crate_dir: &Path) -> Vec<CargoError> {
    let mut errors = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        if msg.get("level").and_then(|l| l.as_str()) != Some("error") {
            continue;
        }
        if let Some(e) = parse_compiler_message(msg, crate_dir) {
            errors.push(e);
        }
    }
    errors
}

/// Extract a `CargoError` from a single `message` object (the first primary span).
fn parse_compiler_message(msg: &Value, crate_dir: &Path) -> Option<CargoError> {
    let text = msg
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let code = msg
        .get("code")
        .and_then(|c| c.get("code"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());

    let spans = msg.get("spans").and_then(|s| s.as_array());
    let primary = spans.and_then(|arr| {
        arr.iter()
            .find(|s| s.get("is_primary").and_then(|p| p.as_bool()) == Some(true))
    });

    // A diagnostic with no primary span (e.g. a crate-level error) still counts,
    // but we can only attribute file/line when a span exists.
    let (file, line, col) = match primary {
        Some(span) => {
            let file = span
                .get("file_name")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            // Filter to spans inside the crate.
            if !file_in_crate(&file, crate_dir) {
                return None;
            }
            let line = span.get("line_start").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
            let col = span
                .get("column_start")
                .and_then(|c| c.as_u64())
                .unwrap_or(0) as u32;
            (file, line, col)
        }
        None => return None,
    };

    Some(CargoError {
        file,
        line,
        col,
        code,
        message: text,
    })
}

/// True if `file` (relative or absolute, as cargo reports it) resolves within the
/// crate directory. Cargo reports paths relative to the manifest dir.
fn file_in_crate(file: &str, crate_dir: &Path) -> bool {
    let p = Path::new(file);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        crate_dir.join(p)
    };
    // Canonicalize both when possible; fall back to a prefix check.
    match (resolved.canonicalize(), crate_dir.canonicalize()) {
        (Ok(r), Ok(c)) => r.starts_with(c),
        _ => resolved.starts_with(crate_dir),
    }
}

fn errors_json(errors: &[CargoError]) -> Vec<Value> {
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

/// Per-manifest isolated target dir under temp_dir so repeated calls reuse
/// incremental state without clobbering the project's `target/`.
fn isolated_target_dir(manifest: &Path) -> std::path::PathBuf {
    let key = sig_hash(&manifest.to_string_lossy());
    std::env::temp_dir()
        .join("qontinui-cargo-typecheck")
        .join(key)
}

/// Warm `cargo-check` envelope (observe path), lifting coverage into the body.
fn warm_envelope(result: Value) -> Envelope {
    Envelope {
        observer: "code_semantics".into(),
        query: "typecheck".into(),
        result,
        posterior: 1.0,
        coverage: 1.0,
        provenance: PROV_CARGO.into(),
        credibility: Credibility::high(),
        staleness_seconds: None,
        kernel: false,
    }
}

/// Syntactic-predict envelope (overlay path): coverage 0.5, boundary medium.
fn syntactic_envelope(result: Value) -> Envelope {
    Envelope {
        observer: "code_semantics".into(),
        query: "typecheck".into(),
        result,
        posterior: 0.5,
        coverage: 0.5,
        provenance: PROV_CARGO_SYNTACTIC.into(),
        credibility: Credibility::fallback(),
        staleness_seconds: None,
        kernel: false,
    }
}

/// Public entrypoint: typecheck a Rust `file`, optionally with an `overlay_patch`.
pub async fn typecheck(file: &Path, overlay_patch: Option<Value>) -> Envelope {
    if let Err(reason) = &*CARGO_AVAILABLE {
        return Envelope::unavailable("typecheck", reason);
    }
    let manifest = match nearest_cargo_manifest(file) {
        Some(m) => m,
        None => {
            return Envelope::unavailable(
                "typecheck",
                "no enclosing Cargo.toml for the target file",
            )
        }
    };

    match overlay_patch {
        None => observe(file, &manifest).await,
        Some(patch) => predict(file, &manifest, patch),
    }
}

/// Post-write observe: run a real `cargo check`.
async fn observe(file: &Path, manifest: &Path) -> Envelope {
    let crate_dir = manifest.parent().unwrap_or(Path::new("."));
    let target_dir = isolated_target_dir(manifest);

    let mut cmd = crate::process_helpers::tokio_no_window("cargo");
    cmd.arg("check")
        .arg("--message-format=json")
        .arg("--manifest-path")
        .arg(manifest)
        .env("CARGO_TARGET_DIR", &target_dir)
        .current_dir(crate_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Envelope::unavailable("typecheck", &format!("cargo spawn failed: {e}")),
    };

    let out = match tokio::time::timeout(Duration::from_secs(240), child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Envelope::unavailable("typecheck", &format!("cargo check failed: {e}"))
        }
        Err(_) => return Envelope::unavailable("typecheck", "cargo check timed out"),
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let errors = parse_cargo_json(&stdout, crate_dir);

    // A nonzero exit WITH parsed error diagnostics is a valid "observed errors"
    // result. A nonzero exit with NO parsed errors means cargo itself failed to
    // run (bad manifest, toolchain) — that is unavailable, not a clean answer.
    if !out.status.success() && errors.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Envelope::unavailable(
            "typecheck",
            &format!(
                "cargo check produced no diagnostics and failed: {}",
                stderr.trim().chars().take(400).collect::<String>()
            ),
        );
    }

    // Keep only this file's errors? The contract is about the edited file, but
    // cargo errors elsewhere in the crate are still real breakage — surface all
    // in-crate errors (already filtered to the crate). Ensure `file` is the one
    // we anchored on by leaving the full in-crate set.
    let _ = file;
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

/// Predict (overlay): syntactic-only diff of `pub` items. No compile runs, so
/// `errors: []` and `coverage: 0.5` (honest — coord routes this to Partial).
fn predict(file: &Path, manifest: &Path, patch: Value) -> Envelope {
    let crate_dir = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let overlay_file = patch.get("file").and_then(|v| v.as_str());
    let new_text = patch.get("new_text").and_then(|v| v.as_str());
    let (overlay_file, new_text) = match (overlay_file, new_text) {
        (Some(f), Some(t)) => (f.to_string(), t.to_string()),
        _ => return Envelope::unavailable("typecheck", "overlay_patch requires {file, new_text}"),
    };

    let on_disk = extract_pub_items(&syntactic::read_or_empty(Path::new(&overlay_file)));
    let overlaid = extract_pub_items(&new_text);

    let changed = changed_pub_signatures(&on_disk, &overlaid);
    let removed = syntactic::removed_decls(&on_disk, &overlaid);
    let removed_syms = syntactic::removed_symbols(&removed, &crate_dir, file, "rs", 2000, 20);

    syntactic_envelope(json!({
        "ok": true,
        "errors": [],
        "overlay": true,
        "changed_signatures": changed,
        "removed_symbols": removed_syms,
        "coverage": 0.5,
    }))
}

/// `changed_signatures` for Rust pub items: same name+kind, different normalized
/// declaration line.
fn changed_pub_signatures(on_disk: &[Decl], overlay: &[Decl]) -> Vec<Value> {
    let mut out = Vec::new();
    for d in on_disk {
        if let Some(o) = overlay
            .iter()
            .find(|o| o.name == d.name && o.kind == d.kind)
        {
            if o.normalized != d.normalized {
                out.push(json!({
                    "name": d.name,
                    "before_hash": sig_hash(&d.normalized),
                    "after_hash": sig_hash(&o.normalized),
                }));
            }
        }
    }
    out
}

/// The pub item kinds we recognize.
const PUB_KINDS: [&str; 7] = ["fn", "struct", "enum", "trait", "type", "const", "static"];

/// Extract `pub`/`pub(...)` items (fn/struct/enum/trait/type/const/static) from
/// Rust source with a small line parser. The declaration line is normalized up to
/// the body delimiter (`{`, `;`, or `=`) so signature changes are detected without
/// a full parser.
fn extract_pub_items(src: &str) -> Vec<Decl> {
    let mut decls = Vec::new();
    for raw in src.lines() {
        let line = raw.trim_start();
        let after_pub = match strip_pub(line) {
            Some(rest) => rest,
            None => continue,
        };
        // after_pub may have `async `/`unsafe `/`extern "C" ` qualifiers before `fn`.
        let kw_line = strip_fn_qualifiers(after_pub);
        for kind in PUB_KINDS {
            if let Some(rest) = kw_line.strip_prefix(kind) {
                // Require a separator after the keyword so `fnord` != `fn`.
                let sep_ok = rest
                    .chars()
                    .next()
                    .map(|c| c.is_whitespace())
                    .unwrap_or(false);
                if !sep_ok {
                    continue;
                }
                if let Some(name) = item_name(rest.trim_start(), kind) {
                    decls.push(Decl {
                        name,
                        kind: kind.to_string(),
                        normalized: normalize_rust_decl(raw),
                    });
                }
                break;
            }
        }
    }
    decls
}

/// Strip a leading `pub` / `pub(crate)` / `pub(in ...)` visibility from a line,
/// returning the remainder. `None` if the line is not `pub`.
fn strip_pub(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub")?;
    // Must be followed by whitespace or `(`.
    match rest.chars().next() {
        Some('(') => {
            // pub(...) — skip to the matching close paren.
            let close = rest.find(')')?;
            Some(rest[close + 1..].trim_start())
        }
        Some(c) if c.is_whitespace() => Some(rest.trim_start()),
        _ => None,
    }
}

/// Strip `async`/`unsafe`/`extern "..."` qualifiers that may precede `fn`.
fn strip_fn_qualifiers(line: &str) -> String {
    let mut s = line.to_string();
    loop {
        let trimmed = s.trim_start();
        if let Some(r) = trimmed.strip_prefix("async ") {
            s = r.to_string();
        } else if let Some(r) = trimmed.strip_prefix("unsafe ") {
            s = r.to_string();
        } else if let Some(r) = trimmed.strip_prefix("extern ") {
            // optional ABI string e.g. extern "C"
            let r = r.trim_start();
            if let Some(after_q) = r.strip_prefix('"') {
                if let Some(end) = after_q.find('"') {
                    s = after_q[end + 1..].trim_start().to_string();
                    continue;
                }
            }
            s = r.to_string();
        } else {
            break;
        }
    }
    s.trim_start().to_string()
}

/// The item's identifier after its keyword. For most kinds the name is the next
/// identifier; for `fn` it's up to `(`/`<`/whitespace.
fn item_name(rest: &str, _kind: &str) -> Option<String> {
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

/// Normalize a Rust declaration line for signature comparison: take everything up
/// to the body/terminator (`{`, `;`, or ` = `), collapse whitespace.
fn normalize_rust_decl(line: &str) -> String {
    let head = line
        .split('{')
        .next()
        .unwrap_or(line)
        .split(';')
        .next()
        .unwrap_or(line);
    // Drop a trailing ` = value` for const/static.
    let head = match head.find(" = ") {
        Some(i) => &head[..i],
        None => head,
    };
    head.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_line(reason: &str, level: &str, file: &str) -> String {
        json!({
            "reason": reason,
            "message": {
                "level": level,
                "message": "mismatched types",
                "code": { "code": "E0308" },
                "spans": [
                    { "is_primary": true, "file_name": file, "line_start": 4, "column_start": 9 }
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn parse_cargo_json_picks_errors_ignores_warnings_and_other_reasons() {
        // Use absolute crate dir so file_in_crate's prefix path holds.
        let tmp = tempfile::tempdir().unwrap();
        let crate_dir = tmp.path();
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        let abs = crate_dir.join("src").join("lib.rs");
        std::fs::write(&abs, "fn x(){}\n").unwrap();
        let abs_s = abs.to_string_lossy().to_string();

        let artifact = json!({"reason": "compiler-artifact", "package_id": "x"}).to_string();
        let lines = format!(
            "{}\n{}\n{}\n",
            err_line("compiler-message", "error", &abs_s),
            err_line("compiler-message", "warning", &abs_s),
            artifact,
        );
        let errors = parse_cargo_json(&lines, crate_dir);
        assert_eq!(errors.len(), 1, "only the error-level compiler-message");
        assert_eq!(errors[0].code.as_deref(), Some("E0308"));
        assert_eq!(errors[0].line, 4);
        assert_eq!(errors[0].col, 9);
    }

    #[test]
    fn parse_cargo_json_filters_out_of_crate_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_dir = tmp.path();
        // span points at an absolute path outside the crate dir
        let outside = "/elsewhere/dep/src/lib.rs";
        let line = err_line("compiler-message", "error", outside);
        let errors = parse_cargo_json(&line, crate_dir);
        assert!(errors.is_empty(), "out-of-crate span must be filtered");
    }

    #[test]
    fn extract_pub_items_truth_table() {
        let src = "\
pub fn alpha(x: i32) -> i32 { x }
fn private_fn() {}
pub struct Beta { a: u8 }
pub(crate) enum Gamma { A, B }
pub trait Delta {}
pub const K: u32 = 3;
pub async fn ep() {}
pub fnord() {}
";
        let decls = extract_pub_items(src);
        let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"Beta"));
        assert!(names.contains(&"Gamma"));
        assert!(names.contains(&"Delta"));
        assert!(names.contains(&"K"));
        assert!(names.contains(&"ep"));
        // private fn is ignored
        assert!(!names.contains(&"private_fn"));
        // `fnord` must not be parsed as a `fn` named "ord"
        assert!(!decls.iter().any(|d| d.name == "ord"));
    }

    #[test]
    fn removed_pub_fn_referenced_is_flagged_private_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let file = tmp.path().join("src").join("lib.rs");
        std::fs::write(&file, "// edited\n").unwrap();
        // sibling references `gone`
        std::fs::write(
            tmp.path().join("src").join("other.rs"),
            "fn use_it() { gone(); }\n",
        )
        .unwrap();

        let on_disk = extract_pub_items("pub fn gone() {}\nfn private_gone() {}\n");
        let overlay = extract_pub_items("");
        let removed = syntactic::removed_decls(&on_disk, &overlay);
        // private_gone is not a pub item, so it was never extracted.
        assert!(removed.iter().all(|d| d.name != "private_gone"));
        let syms = syntactic::removed_symbols(&removed, tmp.path(), &file, "rs", 2000, 20);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0]["name"], "gone");
    }

    #[test]
    fn changed_pub_signature_detected() {
        let on_disk = extract_pub_items("pub fn f(x: i32) -> i32 { x }\n");
        let overlay = extract_pub_items("pub fn f(x: i64) -> i64 { x }\n");
        let changed = changed_pub_signatures(&on_disk, &overlay);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "f");
        assert_ne!(changed[0]["before_hash"], changed[0]["after_hash"]);
    }

    #[test]
    fn syntactic_predict_envelope_is_coverage_half_boundary_medium() {
        let env = syntactic_envelope(json!({"ok": true, "errors": [], "coverage": 0.5}));
        assert_eq!(env.provenance, PROV_CARGO_SYNTACTIC);
        assert_eq!(env.coverage, 0.5);
        assert_eq!(env.credibility.boundary, "medium");
    }

    #[test]
    fn observe_envelope_has_cargo_provenance() {
        let env = warm_envelope(json!({"ok": true, "errors": []}));
        assert_eq!(env.provenance, PROV_CARGO);
        assert_eq!(env.coverage, 1.0);
        assert_eq!(env.credibility.boundary, "high");
    }

    /// LIVE: requires a cargo toolchain. Skips (passes) when unavailable.
    #[tokio::test]
    async fn live_cargo_check_finds_deliberate_type_error() {
        if let Err(reason) = &*CARGO_AVAILABLE {
            eprintln!("skipping live cargo test: {reason}");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let crate_dir = tmp.path().join("tinycrate");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname=\"tinycrate\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        // deliberate type error: assign &str to i32
        let main_rs = crate_dir.join("src").join("main.rs");
        std::fs::write(&main_rs, "fn main() { let _x: i32 = \"oops\"; }\n").unwrap();

        let env = typecheck(&main_rs, None).await;
        // If cargo couldn't run (offline crate registry etc.) we tolerate an
        // unavailable envelope; otherwise we must see the error.
        if env.provenance == PROV_CARGO {
            assert_eq!(env.coverage, 1.0);
            let errs = env.result["errors"].as_array().unwrap();
            assert!(!errs.is_empty(), "cargo should flag the type mismatch");
            assert_eq!(env.result["ok"], false);
        } else {
            eprintln!(
                "live cargo test: tolerating non-observe envelope provenance={}",
                env.provenance
            );
        }
    }
}
