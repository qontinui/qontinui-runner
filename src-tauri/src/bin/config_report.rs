//! `config_report` — headless dump of this runner's effective configuration,
//! layer by layer, with per-value provenance
//! (plan `2026-08-20-effective-config-provenance-and-env-generation`, Phases 1-5).
//!
//! Runs the SAME fifteen-layer inventory as the in-app `config_report` Tauri
//! command (both delegate to `qontinui_runner_lib::config_report`), in the same
//! order, with the same byte-stable formatting.
//!
//! ## What this bin can and cannot see — and why it says so
//!
//! This bin links only the lib crate. Ten of the fifteen layers live in
//! BIN-only modules of the runner binary (`settings`, `launch_env`,
//! `api_config`, `mcp_api`, `terminal`, `coord_mcp`, `session`, `ai_provider`,
//! `prompt_library`, `mcp::fleet_policy_poller`, `build_drift`,
//! `config_facade`), which are not in this binary at all — so it cannot resolve
//! them, and it does not pretend to. TWO MORE live in a different executable
//! entirely (the dev supervisor's spawn env; the `qontinui-shim` binary's
//! argv), and those get their own, different `UNKNOWN` sentence — "run the
//! in-app config report" is the right remediation for a bin-module layer and
//! the wrong one for a value assembled in another process. Every such layer is
//! printed as a row reading `UNKNOWN — not observable from the headless bin`,
//! naming the symbol that owns it.
//!
//! That is deliberate and is the report's central discipline: a layer that
//! cannot be read is never rendered as the value the code would have fallen
//! back to, and never silently omitted. Run the in-app report (or the Tauri
//! command) for the bin-only layers.
//!
//! ## Usage
//!
//! ```text
//! config_report              # human-readable layered report
//! config_report --json       # machine-readable ConfigReport JSON
//! config_report --layer-doc  # emit the generated layer inventory (markdown)
//! ```
//!
//! `--layer-doc` is what publishes `docs/runner-config-layers.md`; CI holds the
//! checked-in copy byte-exact against a fresh run
//! (`config_report::tests::config_report_checked_in_layer_doc_is_the_generators_output`),
//! so regenerate and commit it after touching `LAYER_SPECS`. **From bash**
//! (Git Bash on Windows) — PowerShell's `>` writes UTF-16LE with a BOM, which
//! `include_str!` cannot read, so the redirect would break the build rather
//! than refresh the doc:
//!
//! ```text
//! cargo run --bin config_report -- --layer-doc > docs/runner-config-layers.md
//! ```
//!
//! Exit code is 0 whenever a report was produced: an `UNKNOWN` layer is a
//! finding, not a failure of the tool, and this command makes no claim about
//! whether the configuration is *correct*.

use std::process::ExitCode;

use qontinui_runner_lib::config_report::{
    build_report, render_layer_doc, ConfigReportInputs, Observer,
};

fn main() -> ExitCode {
    // Doc-emit mode: print the layer inventory generated from LAYER_SPECS and
    // exit WITHOUT resolving anything live.
    if std::env::args().any(|a| a == "--layer-doc") {
        print!("{}", render_layer_doc());
        return ExitCode::SUCCESS;
    }

    let json = std::env::args().any(|a| a == "--json");

    let inputs = ConfigReportInputs {
        observer: Observer::HeadlessBin,
        // Headless: every one of these lives in a runner-binary module
        // (`settings`, `api_config`, `ai_provider`) that is not linked here, so
        // there is nothing honest to inject. The driver turns each `None` into
        // `Unknown { reason: "not observable from the headless bin …" }` naming
        // the symbol that owns it (see the module docs). `ConfigReportInputs`
        // is deliberately not `Default`, so a layer landed by a later phase
        // forces this list to grow rather than silently defaulting to "no".
        // Phase 5 — layer 1. `settings::load_settings_full` is a runner-binary
        // module, so its whole-file provenance is structurally unreadable here.
        // The `None` is doing real work: a bin that shelled out to a settings
        // reader of its own could produce a `loaded` for a DIFFERENT file than
        // the running runner reads (the two config-dir resolvers are separate
        // implementations of one rule, and they HAVE disagreed before — about
        // an exported-but-empty `QONTINUI_CONFIG_DIR`; that is the whole reason
        // layers 2 and 3 stay separate rows), and a wrong `loaded` is the most
        // dangerous reading this layer can emit.
        settings_struct: None,
        config_dir: None,
        api_endpoint_registry: None,
        claude_config_dir: None,
        // Phase 3 — the env generations. `launch_env` and `terminal` are
        // runner-binary modules, so the three env layers are `Unknown` here for
        // the same structural reason as the rest. The env-generation SECTION is
        // `None` for a sharper reason worth stating: this process's own
        // `std::env::vars()` is the env of whatever SHELL invoked it, not the
        // runner's — printing it under a "the runner's environment" heading
        // would be a plausible, wrong answer, which is the one failure mode
        // this whole report exists to prevent. The renderer prints the absence
        // explicitly rather than omitting the section.
        launch_env_snapshot: None,
        adhoc_env_reads: None,
        supervisor_injected_env: None,
        // Phase 4 — the two coord-served TIME-VARYING layers. `prompt_library`
        // and `mcp::fleet_policy_poller` are runner-binary modules AND their
        // state is a process-global cache filled by a background loop that
        // exists only inside a running runner. A second process cannot see
        // that cache at all, so this is structurally UNKNOWN twice over — and
        // an `Unknown` here says "not observable", never "the dial is off",
        // which on a fail-safe dial whose resting value IS `off` would be an
        // unusually convincing wrong answer.
        coord_prompt_documents: None,
        fleet_policy_dial: None,
        // Phase 4 — the carriers. Layer 12 (`session::claude_hook`) and layer
        // 14 (`coord_mcp`) are bin-only modules. Layer 13 is owned by the
        // `qontinui-shim` binary, and the driver gives it the EXTERNAL-BINARY
        // reason rather than the bin-module one: "run the in-app config
        // report" is the right remediation for the first two and the wrong one
        // for a value assembled in another executable's argv.
        claude_settings_carrier: None,
        mcp_config_carrier: None,
        mcp_json: None,
        env_generations: None,
    };
    let report = build_report(&inputs);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("config_report: failed to serialize report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", report.render());
    }

    ExitCode::SUCCESS
}
