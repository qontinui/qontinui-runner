//! Step output support modules.
//!
//! Houses the Rust-side helpers that back the TypeScript
//! `ScriptedOutputHandler` pipeline (Phase 2 of the orchestration-upgrades
//! plan and Phase A of the script-emitter-wiring plan).
//!
//! The only current submodule — [`script_emitter`] — is the "think-in-code"
//! extraction-expression emitter. TS calls into it via the
//! `emit_extraction_script` Tauri command (see `commands::script_emitter`).

pub mod script_emitter;
