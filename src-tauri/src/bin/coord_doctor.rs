//! `coord_doctor` — headless runner self-check for coord access + gate
//! registration (plan 2026-06-13 Phase 4).
//!
//! Runs the SAME nine ordered checks as the in-app `coord_doctor` Tauri command
//! (both delegate to `qontinui_runner_lib::coord_doctor`) and prints the link +
//! its fix. Eight are BLOCKING — they stop at the first red; the ninth is
//! ADVISORY and always runs, even after a blocking red, rendering `WARN`
//! without changing the verdict. Green on all of them ⇒ "this runner can set
//! gates."
//!
//! The exit code follows `overall_ok`, so an advisory red alone still exits 0 —
//! a warning is not a gate failure, and CI callers must not treat it as one.
//!
//! ## Bound-port caveat
//!
//! This headless bin has no running Tauri runtime, so it cannot observe the
//! runner's ACTUALLY-BOUND loopback API port (check 6 needs it to compare
//! against the `.mcp.json` URL). It passes `bound_api_port: None`, and check 6
//! reports the port as unverifiable — run the in-app command (Settings → coord
//! doctor) for the authoritative `.mcp.json` port verdict. Checks 1-5 and 7 are
//! fully authoritative headless.
//!
//! ## Usage
//!
//! ```text
//! coord_doctor                   # human-readable report; exit 0 = can set gates (advisory
//!                                # warnings included), 1 = blocked
//! coord_doctor --json            # machine-readable DoctorReport JSON
//! coord_doctor --onboarding-doc  # emit the generated onboarding checklist (markdown), exit 0
//! ```

use std::process::ExitCode;

use qontinui_runner_lib::coord_doctor::{diagnose, render_onboarding_doc, DoctorInputs};

fn main() -> ExitCode {
    // Doc-emit mode: print the onboarding checklist generated from CHECK_SPECS
    // and exit 0 WITHOUT running any live check. The CI freshness gate uses
    // this to regenerate `docs/runner-onboarding.md` and diff it.
    if std::env::args().any(|a| a == "--onboarding-doc") {
        print!("{}", render_onboarding_doc());
        return ExitCode::SUCCESS;
    }

    let json = std::env::args().any(|a| a == "--json");

    let inputs = DoctorInputs {
        // Headless: no live runtime → bound port unknown (see module docs).
        bound_api_port: None,
        // Default Claude config location (the spawn-inherited ambient default).
        claude_config_dir: None,
    };
    let report = diagnose(&inputs);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("coord_doctor: failed to serialize report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", report.render());
    }

    if report.overall_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
