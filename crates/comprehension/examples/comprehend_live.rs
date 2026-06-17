//! Run the REAL comprehension LLM leg end-to-end against the live `claude` CLI.
//!
//! This is the runtime-deferred path made executable: it loads the committed
//! connect-runner snapshot + discovery fixtures, builds the prompt context,
//! spawns the **actual** subscription-billed `claude` CLI exactly as
//! [`qontinui_comprehension::llm::comprehension_argv`] specifies (NEVER the
//! metered API — per `feedback_no_anthropic_api`), parses `.structured_output`
//! into a `FunctionalSpec`, runs the deterministic `assemble_spec` (clamp +
//! discovery IR overlay + ledger collation), and writes the comprehended spec to
//! the path given as the first CLI arg.
//!
//! Usage:
//!   cargo run --example comprehend_live -- <out_spec_path> [model]
//!
//! `model` defaults to `sonnet`. The `claude` CLI must be authenticated on the
//! host. Output is NON-DETERMINISTIC (an LLM) — this is an evidence/verification
//! tool, not a golden test (those live in `tests/`).

use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command;

use qontinui_comprehension::clamp::EvidenceClass;
use qontinui_comprehension::input::{DiscoveryResult, Snapshot};
use qontinui_comprehension::llm::{comprehension_argv, parse_envelope};
use qontinui_comprehension::mapping::degraded_evidence_classes;
use qontinui_comprehension::worker::assemble_spec;

const SNAPSHOT: &str = include_str!("../tests/fixtures/comprehension/connect-runner.snapshot.json");
const DISCOVERY: &str =
    include_str!("../tests/fixtures/comprehension/connect-runner.discovery.json");

const SOURCE_URL: &str = "https://app.qontinui.io/connect-runner";

/// The connect-runner degraded-explorer evidence classes — the same map the
/// oracle test builds, so the live spec is clamped against the identical bound.
fn evidence_classes() -> BTreeMap<String, EvidenceClass> {
    degraded_evidence_classes(
        &["deviceName".into(), "deviceId".into()],
        &["Device".into()],
        &["pairConfirm".into()],
        &[
            ("Device".into(), "state".into()),
            ("Device".into(), "callback".into()),
        ],
    )
}

/// Build the prompt context the worker feeds the CLI: the raw observation
/// substrate (snapshot + discovery) plus the comprehension task framing.
fn build_prompt(snapshot: &Snapshot, discovery: &DiscoveryResult) -> String {
    let snap_json = serde_json::to_string_pretty(snapshot).unwrap();
    let disc_json = serde_json::to_string_pretty(discovery).unwrap();
    format!(
        "Comprehend the following ONE web page ({SOURCE_URL}) into a FunctionalSpec.\n\
         The page serves NO AWAS manifest, so this is the DEGRADED explorer path: \
         rendered elements and form fields are Observed; opaque tokens / redirect \
         semantics deduced from them are Inferred; the auth model is Inferred (the \
         page sits behind the authed app shell); every server-side operation effect \
         is Assumed.\n\n\
         Emit entities, operations (with inputs/validation/effect), and the auth \
         model. Do NOT emit uiStates/navigation — those are owned by the discovery \
         substrate and overwritten downstream.\n\n\
         === UI BRIDGE SNAPSHOT ===\n{snap_json}\n\n\
         === DISCOVERY StateDiscoveryResult ===\n{disc_json}\n"
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out_path = args
        .next()
        .expect("usage: comprehend_live <out_spec_path> [model]");
    let model = args.next().unwrap_or_else(|| "sonnet".into());

    let snapshot: Snapshot = serde_json::from_str(SNAPSHOT).expect("snapshot fixture parses");
    let discovery: DiscoveryResult =
        serde_json::from_str(DISCOVERY).expect("discovery fixture parses");

    let prompt = build_prompt(&snapshot, &discovery);
    let argv = comprehension_argv(&prompt, &model);

    eprintln!("=== claude CLI invocation (model={model}) ===");
    eprintln!("claude {}", redact_argv(&argv));
    eprintln!("=== prompt context ({} bytes) ===", prompt.len());

    let output = Command::new("claude")
        .args(&argv)
        .output()
        .expect("spawn claude CLI");

    if !output.status.success() {
        eprintln!(
            "claude exited {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }

    // Emit the raw envelope for the evidence record.
    std::io::stderr()
        .write_all(b"=== raw claude stdout (envelope) ===\n")
        .ok();
    std::io::stderr().write_all(&output.stdout).ok();
    std::io::stderr().write_all(b"\n").ok();

    let inferred = match parse_envelope(&output.stdout) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("parse_envelope failed: {e}");
            std::process::exit(2);
        }
    };

    let classes = evidence_classes();
    let observed_at = "2026-06-14T00:00:00Z";
    let spec = assemble_spec(
        inferred,
        &discovery,
        &classes,
        SOURCE_URL,
        Some(observed_at),
    );

    let spec_json = serde_json::to_string_pretty(&spec).expect("spec serializes");
    std::fs::write(&out_path, &spec_json).expect("write spec");
    eprintln!("=== comprehended FunctionalSpec written to {out_path} ===");
    println!("{spec_json}");
}

/// Redact the long `--json-schema` arg for human-readable logging.
fn redact_argv(argv: &[String]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--json-schema" {
            out.push("--json-schema".to_string());
            out.push("<FunctionalSpec schema JSON>".to_string());
            i += 2;
        } else if argv[i] == "--system-prompt" {
            out.push("--system-prompt".to_string());
            out.push("<honesty-rules system prompt>".to_string());
            i += 2;
        } else if argv[i].len() > 120 {
            out.push("<prompt-context>".to_string());
            i += 1;
        } else {
            out.push(format!("{:?}", argv[i]));
            i += 1;
        }
    }
    out.join(" ")
}
