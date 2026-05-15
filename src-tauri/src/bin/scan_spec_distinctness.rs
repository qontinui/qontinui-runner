//! Plan 04 Step 8 — diagnostic scanner for the on-disk spec corpus.
//!
//! Walks `<root>/pages/<id>/state-machine.derived.json` for every page id,
//! runs the distinctness validator (`spec_api::distinctness::report`) on each,
//! and emits the results as JSON (default) or CSV. Used by the cleanup
//! workflow (Plan 04 Path A) to seed its work-list, and as a periodic-audit
//! smoke test once Plan 05 lands.
//!
//! ## Usage
//!
//! ```text
//! scan_spec_distinctness                                  # JSON, default root
//! scan_spec_distinctness --csv                            # CSV
//! scan_spec_distinctness --root <path> --json             # alternate root
//! scan_spec_distinctness --filter emptyCriteria           # show only specs with empty criteria
//! scan_spec_distinctness --filter identicalStates
//! scan_spec_distinctness --filter subsetDomination
//! ```
//!
//! ## Module visibility approach (Plan 04 Step 8 implementer notes)
//!
//! Per Plan 04 Step 8's "module visibility" sidebar: `spec_api/distinctness.rs`
//! is only reachable through `crate::spec_api::*` in `main.rs`. Bin targets
//! (`src/bin/*`) live in a separate crate that can only see the library
//! crate's public surface (`lib.rs`). Adding `pub mod spec_api;` to `lib.rs`
//! (approach (a)) cascades — `spec_api/mod.rs` and `handlers.rs` import
//! `crate::mcp::types::ApiState`, which transitively requires exposing the
//! entire runner internals (action_service, commands, config_storage,
//! doctor, reflection, …).
//!
//! Chosen approach: **(c) — inline via `#[path]`**. The bin file declares
//! `types` + `distinctness` as bin-local modules pointing at the source files
//! in `src/spec_api/`. No `lib.rs` change required. `cfg(test)` blocks inside
//! `distinctness.rs` don't compile under `cargo build --bin …` and are out of
//! scope for this binary; they continue to compile under `cargo test --bin
//! qontinui-runner` (their original home in `main.rs`'s `crate::spec_api`
//! tree).

use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// ---------------------------------------------------------------------------
// Module visibility — see header doc-comment.
// ---------------------------------------------------------------------------

#[path = "../spec_api/types.rs"]
mod types;

#[path = "../spec_api/distinctness.rs"]
mod distinctness;

use distinctness::DistinctnessViolation;
use types::IrPageSpec;

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Json,
    Csv,
}

#[derive(Debug)]
struct Args {
    mode: OutputMode,
    root: Option<PathBuf>,
    filter: Option<FilterReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterReason {
    EmptyCriteria,
    IdenticalStates,
    SubsetDomination,
}

impl FilterReason {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "emptyCriteria" => Some(Self::EmptyCriteria),
            "identicalStates" => Some(Self::IdenticalStates),
            "subsetDomination" => Some(Self::SubsetDomination),
            _ => None,
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = OutputMode::Json;
    let mut root: Option<PathBuf> = None;
    let mut filter: Option<FilterReason> = None;
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        match a.as_str() {
            "--json" => {
                mode = OutputMode::Json;
                i += 1;
            }
            "--csv" => {
                mode = OutputMode::Csv;
                i += 1;
            }
            "--root" => {
                let val = raw
                    .get(i + 1)
                    .ok_or_else(|| "--root requires a path argument".to_string())?;
                root = Some(PathBuf::from(val));
                i += 2;
            }
            "--filter" => {
                let val = raw
                    .get(i + 1)
                    .ok_or_else(|| "--filter requires a reason argument".to_string())?;
                filter = Some(FilterReason::from_str(val).ok_or_else(|| {
                    format!(
                        "unknown --filter value '{}': expected one of \
                         emptyCriteria | identicalStates | subsetDomination",
                        val
                    )
                })?);
                i += 2;
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    Ok(Args { mode, root, filter })
}

fn print_help() {
    println!(
        "scan_spec_distinctness — Plan 04 Step 8\n\n\
         Usage:\n\
         \x20 scan_spec_distinctness [--json | --csv] [--root <path>] [--filter <reason>]\n\n\
         Options:\n\
         \x20 --json                  Emit JSON (default).\n\
         \x20 --csv                   Emit CSV.\n\
         \x20 --root <path>           Specs root (default: <CARGO_MANIFEST_DIR>/../specs).\n\
         \x20 --filter <reason>       Show only specs with at least one violation of <reason>.\n\
         \x20                         <reason>: emptyCriteria | identicalStates | subsetDomination\n\
         \x20 -h, --help              Print this message.\n"
    );
}

// ---------------------------------------------------------------------------
// Storage primitives (inline — see module visibility note above)
// ---------------------------------------------------------------------------

/// Resolution mirrors `spec_api::storage::resolve_specs_root`, minus the
/// embedded-pages fallback (the scanner is an on-disk diagnostic tool).
fn resolve_specs_root() -> PathBuf {
    if let Ok(override_path) = std::env::var("QONTINUI_SPECS_ROOT") {
        return PathBuf::from(override_path);
    }
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest_dir).join("..").join("specs");
        if let Ok(canon) = p.canonicalize() {
            return canon;
        }
        return p;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("specs")
}

/// List the page ids that exist under `<root>/pages/`. Returns an empty vec
/// if the directory does not exist.
fn list_pages(root: &Path) -> io::Result<Vec<String>> {
    let pages_dir = root.join("pages");
    let mut ids: Vec<String> = Vec::new();
    if !pages_dir.exists() {
        return Ok(ids);
    }
    for entry in fs::read_dir(&pages_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Read an IR document. Returns `Ok(None)` if `state-machine.derived.json`
/// is missing.
fn read_ir(root: &Path, page_id: &str) -> Result<Option<IrPageSpec>, String> {
    let path = root
        .join("pages")
        .join(page_id)
        .join("state-machine.derived.json");
    if !path.exists() {
        return Ok(None);
    }
    let data =
        fs::read_to_string(&path).map_err(|e| format!("read {} failed: {}", path.display(), e))?;
    let doc: IrPageSpec = serde_json::from_str(&data)
        .map_err(|e| format!("parse {} failed: {}", path.display(), e))?;
    Ok(Some(doc))
}

// ---------------------------------------------------------------------------
// Scan report
// ---------------------------------------------------------------------------

#[derive(Default, Serialize, Debug)]
struct ScanSummary {
    total_specs: usize,
    clean: usize,
    with_empty_criteria: usize,
    with_identical_states: usize,
    with_subset_domination: usize,
    any_violation: usize,
}

#[derive(Serialize, Debug)]
struct SpecScanEntry {
    spec_id: String,
    state_count: usize,
    ok: bool,
    violations: Vec<DistinctnessViolation>,
}

#[derive(Serialize, Debug)]
struct ScanReport {
    summary: ScanSummary,
    specs: Vec<SpecScanEntry>,
}

fn update_summary(s: &mut ScanSummary, entry: &SpecScanEntry) {
    s.total_specs += 1;
    if entry.ok {
        s.clean += 1;
        return;
    }
    s.any_violation += 1;
    let mut has_empty = false;
    let mut has_identical = false;
    let mut has_subset = false;
    for v in &entry.violations {
        match v {
            DistinctnessViolation::EmptyCriteria { .. } => has_empty = true,
            DistinctnessViolation::IdenticalStates { .. } => has_identical = true,
            DistinctnessViolation::SubsetDomination { .. } => has_subset = true,
        }
    }
    if has_empty {
        s.with_empty_criteria += 1;
    }
    if has_identical {
        s.with_identical_states += 1;
    }
    if has_subset {
        s.with_subset_domination += 1;
    }
}

fn matches_filter(entry: &SpecScanEntry, filter: Option<FilterReason>) -> bool {
    let Some(f) = filter else {
        return true; // no filter — keep everything
    };
    entry.violations.iter().any(|v| {
        matches!(
            (f, v),
            (
                FilterReason::EmptyCriteria,
                DistinctnessViolation::EmptyCriteria { .. }
            ) | (
                FilterReason::IdenticalStates,
                DistinctnessViolation::IdenticalStates { .. }
            ) | (
                FilterReason::SubsetDomination,
                DistinctnessViolation::SubsetDomination { .. }
            )
        )
    })
}

// ---------------------------------------------------------------------------
// CSV emitter (hand-written — no `csv` crate dep)
// ---------------------------------------------------------------------------

/// CSV header row matches the spec in Plan 04 Step 8:
///   spec_id,state_count,has_empty_criteria,identical_state_count,
///   subset_domination_count,ok
///
/// `identical_state_count` and `subset_domination_count` are the per-spec
/// violation counts (each `IdenticalStates` violation can list multiple state
/// ids — we count *violations*, not states). `has_empty_criteria` is a 0/1
/// flag because the per-criterion count is rarely actionable.
fn write_csv<W: Write>(mut w: W, report: &ScanReport) -> io::Result<()> {
    writeln!(
        w,
        "spec_id,state_count,has_empty_criteria,identical_state_count,subset_domination_count,ok"
    )?;
    for e in &report.specs {
        let mut has_empty = 0u32;
        let mut identical_count = 0u32;
        let mut subset_count = 0u32;
        for v in &e.violations {
            match v {
                DistinctnessViolation::EmptyCriteria { .. } => has_empty = 1,
                DistinctnessViolation::IdenticalStates { .. } => identical_count += 1,
                DistinctnessViolation::SubsetDomination { .. } => subset_count += 1,
            }
        }
        writeln!(
            w,
            "{},{},{},{},{},{}",
            csv_escape(&e.spec_id),
            e.state_count,
            has_empty,
            identical_count,
            subset_count,
            if e.ok { "true" } else { "false" }
        )?;
    }
    Ok(())
}

/// Spec ids are kebab-case in practice (`action-plan`, `library`, …) so
/// quoting is unlikely to fire — but be defensive: if the id contains a
/// comma, double-quote, or newline, wrap in double quotes and escape any
/// embedded quotes per RFC 4180.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!();
            print_help();
            return ExitCode::from(2);
        }
    };

    let root = args.root.unwrap_or_else(resolve_specs_root);

    let ids = match list_pages(&root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: list_pages({}) failed: {}", root.display(), e);
            return ExitCode::from(2);
        }
    };

    let mut report = ScanReport {
        summary: ScanSummary::default(),
        specs: Vec::new(),
    };

    for id in &ids {
        let doc = match read_ir(&root, id) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("warning: read_ir({}) failed: {}", id, e);
                continue;
            }
        };
        let r = distinctness::report(&doc);
        let entry = SpecScanEntry {
            spec_id: id.clone(),
            state_count: doc.states.len(),
            ok: r.ok,
            violations: r.violations,
        };
        update_summary(&mut report.summary, &entry);
        if matches_filter(&entry, args.filter) {
            report.specs.push(entry);
        }
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    match args.mode {
        OutputMode::Json => {
            if let Err(e) = serde_json::to_writer_pretty(&mut out, &report) {
                eprintln!("error: writing JSON failed: {}", e);
                return ExitCode::from(2);
            }
            let _ = writeln!(out);
        }
        OutputMode::Csv => {
            if let Err(e) = write_csv(&mut out, &report) {
                eprintln!("error: writing CSV failed: {}", e);
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Tests — intentionally omitted from this bin file because including
// `distinctness.rs` via `#[path]` also pulls in its `#[cfg(test)]` block,
// which references `crate::spec_api::types::*` (a path only valid under
// `main.rs`'s module tree, not under this bin crate). `cargo build --bin …`
// is unaffected (cfg(test) is off); `cargo test --bin scan_spec_distinctness`
// would currently fail to compile that pre-existing test block. Unit-test
// coverage for the validator already lives in `spec_api/distinctness.rs`'s
// own test module (compiled under `cargo test --bin qontinui-runner`).
// ---------------------------------------------------------------------------
