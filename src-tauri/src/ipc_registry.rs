//! Per-module Tauri IPC registration.
//!
//! The runner used to register every Tauri command in ONE
//! `tauri::generate_handler![...]` inside `run_app`. That macro expands to one
//! closure holding one match arm per command, and at debug opt-level 0 every
//! arm keeps its own stack slot for the moved `Invoke<Wry>` — so the closure's
//! frame grew to ~916 KB at ~957 commands, and the first webview IPC call
//! overflowed a 1 MB main-thread stack whenever the `/STACK` link flag went
//! missing (qontinui-runner#1941; plan `2026-10-02-split-run-app-invoke-handler`).
//!
//! Instead, each module that owns commands declares them once with
//! [`ipc_group!`](crate::ipc_group), which emits:
//!
//! - `IPC_NAMES: &[&str]` — the bare command names, and
//! - `ipc_handle(Invoke<Wry>) -> bool` — a `#[inline(never)]` fn whose body is
//!   that module's own `generate_handler!`, so each group gets its own (small)
//!   frame.
//!
//! [`dispatch`] routes an invoke by name to the owning group BEFORE calling it.
//! Routing must happen first: a `generate_handler!` closure consumes the
//! `Invoke` by value even when it returns `false` for an unknown name, so
//! handlers cannot be chained "try A, then B".
//!
//! Every command stays APP-LEVEL and bare-named (`invoke("<cmd>")`). Tauri
//! plugins are deliberately not used — plugin commands are reachable only as
//! `plugin:<name>|<cmd>`, which is what broke runtime IPC and got the previous
//! split reverted at `1f1d807f6`.
//!
//! Adding a command: add its fn name to the `ipc_group!` list in the module
//! that defines it (a module's FIRST command also needs its `GROUPS` entry
//! below). `tests/tauri_commands_are_registered.rs` fails for any
//! `#[tauri::command]` fn that no `ipc_group!` lists, for any module whose
//! `ipc_group!` has no `GROUPS` entry (its commands would compile and then
//! answer "Command not found"), and for any name registered twice.

use std::collections::HashMap;
use std::sync::OnceLock;

use tauri::ipc::Invoke;
use tauri::Wry;

/// A group's dispatch entry point.
pub(crate) type GroupHandler = fn(Invoke<Wry>) -> bool;

/// One `GROUPS` entry: a module's command names and the handler that owns
/// them. `ipc_group!` emits it as the module's `IPC_GROUP`, so names and
/// handler can never be paired across two different modules.
pub(crate) type GroupEntry = (&'static [&'static str], GroupHandler);

/// Declare the Tauri commands a module owns. Invoke it once, in the module
/// that defines the commands, with the bare fn names:
///
/// ```ignore
/// crate::ipc_group!(a11y_capture, a11y_click);
/// ```
///
/// Invoke it at FILE level (not inside an inline `mod {}`): the registration
/// guard maps each `ipc_group!` to its module by file path.
///
/// Command names are `stringify!`d from the fn idents, which is exactly the
/// name `#[tauri::command]` registers unless the attribute carries `rename`.
/// No command uses `rename`, and the registration guard test refuses one.
#[macro_export]
macro_rules! ipc_group {
    ($($cmd:ident),+ $(,)?) => {
        /// The bare Tauri command names this module registers.
        pub(crate) const IPC_NAMES: &[&str] = &[$(stringify!($cmd)),+];

        /// Dispatch one invoke to this module's commands. `#[inline(never)]`
        /// keeps the generated match in its own stack frame.
        #[inline(never)]
        pub(crate) fn ipc_handle(invoke: ::tauri::ipc::Invoke<::tauri::Wry>) -> bool {
            // The macro's closure parameter is untyped; binding it to a fn
            // pointer types it. It captures nothing, so the coercion is free.
            let handler: fn(::tauri::ipc::Invoke<::tauri::Wry>) -> bool =
                ::tauri::generate_handler![$($cmd),+];
            handler(invoke)
        }

        /// This module's `ipc_registry::GROUPS` entry.
        pub(crate) const IPC_GROUP: $crate::ipc_registry::GroupEntry = (IPC_NAMES, ipc_handle);
    };
}

/// Every command group. One `crate::<module>::IPC_GROUP` per module that
/// invokes [`ipc_group!`](crate::ipc_group).
const GROUPS: &[GroupEntry] = &[
    crate::commands::accessibility::IPC_GROUP,
    crate::commands::activity_timeline::IPC_GROUP,
    crate::commands::adaptive_learning::IPC_GROUP,
    crate::commands::agentic_metrics::IPC_GROUP,
    crate::commands::ai_data::IPC_GROUP,
    crate::commands::ai_generation::IPC_GROUP,
    crate::commands::ai_session::IPC_GROUP,
    crate::commands::ai_settings::IPC_GROUP,
    crate::commands::auth::IPC_GROUP,
    crate::commands::autostart::IPC_GROUP,
    crate::commands::backup::IPC_GROUP,
    crate::commands::checkpoint_browser::IPC_GROUP,
    crate::commands::checkpoints::IPC_GROUP,
    crate::commands::checks::IPC_GROUP,
    crate::commands::chunk_labels::IPC_GROUP,
    crate::commands::claims::IPC_GROUP,
    crate::commands::clipboard::IPC_GROUP,
    crate::commands::cloud_sync_settings::IPC_GROUP,
    crate::commands::command_interpreter::IPC_GROUP,
    crate::commands::comparison::IPC_GROUP,
    crate::commands::config::IPC_GROUP,
    crate::commands::container_settings::IPC_GROUP,
    crate::commands::context::IPC_GROUP,
    crate::commands::coord_mode::IPC_GROUP,
    crate::commands::cost_budget_settings::IPC_GROUP,
    crate::commands::cost_dashboard::IPC_GROUP,
    crate::commands::dag_workflows::IPC_GROUP,
    crate::commands::database::IPC_GROUP,
    crate::commands::dataset::IPC_GROUP,
    crate::commands::debug::IPC_GROUP,
    crate::commands::deconflict::IPC_GROUP,
    crate::commands::dev_findings::IPC_GROUP,
    crate::commands::devenv_enroll::IPC_GROUP,
    crate::commands::discoveries::IPC_GROUP,
    crate::commands::doctor::IPC_GROUP,
    crate::commands::durable_execution::IPC_GROUP,
    crate::commands::event_search::IPC_GROUP,
    crate::commands::execution::bridge_execution::IPC_GROUP,
    crate::commands::execution::executor_status::IPC_GROUP,
    crate::commands::execution::python_executor::IPC_GROUP,
    crate::commands::execution::system_ops::IPC_GROUP,
    crate::commands::execution::workflow_execution::IPC_GROUP,
    crate::commands::execution_reporting::IPC_GROUP,
    crate::commands::execution_variables::IPC_GROUP,
    crate::commands::extraction::IPC_GROUP,
    crate::commands::file_browser::IPC_GROUP,
    crate::commands::findings::IPC_GROUP,
    crate::commands::fleet_sessions::IPC_GROUP,
    crate::commands::flow::IPC_GROUP,
    crate::commands::global_log_sources::IPC_GROUP,
    crate::commands::helper_tasks::IPC_GROUP,
    crate::commands::hooks::IPC_GROUP,
    crate::commands::instances::IPC_GROUP,
    crate::commands::interaction::IPC_GROUP,
    crate::commands::issues::IPC_GROUP,
    crate::commands::knowledge::IPC_GROUP,
    crate::commands::known_issues::IPC_GROUP,
    crate::commands::learning::IPC_GROUP,
    crate::commands::library_sync::IPC_GROUP,
    crate::commands::lock_yield_policy_settings::IPC_GROUP,
    crate::commands::log_api::IPC_GROUP,
    crate::commands::logging::IPC_GROUP,
    crate::commands::looping_agents::IPC_GROUP,
    crate::commands::mcp::IPC_GROUP,
    crate::commands::meta_optimizer::IPC_GROUP,
    crate::commands::mobile::IPC_GROUP,
    crate::commands::mobile_settings::IPC_GROUP,
    crate::commands::new_project::IPC_GROUP,
    crate::commands::operator_doors::IPC_GROUP,
    crate::commands::orchestration_loop_configs::IPC_GROUP,
    crate::commands::otel_settings::IPC_GROUP,
    crate::commands::page_spec_store::IPC_GROUP,
    crate::commands::path_settings::IPC_GROUP,
    crate::commands::performance_metrics::IPC_GROUP,
    crate::commands::performance_settings::IPC_GROUP,
    crate::commands::playwright_settings::IPC_GROUP,
    crate::commands::project_logs::IPC_GROUP,
    crate::commands::project_preview::IPC_GROUP,
    crate::commands::rag::IPC_GROUP,
    crate::commands::recap::IPC_GROUP,
    crate::commands::regression::IPC_GROUP,
    crate::commands::remote_attach::IPC_GROUP,
    crate::commands::remote_create::IPC_GROUP,
    crate::commands::resource_guard_settings::IPC_GROUP,
    crate::commands::saved_projects::IPC_GROUP,
    crate::commands::screenshot::IPC_GROUP,
    crate::commands::screenshots::IPC_GROUP,
    crate::commands::script_emitter::IPC_GROUP,
    crate::commands::scripted_output_settings::IPC_GROUP,
    crate::commands::security_settings::IPC_GROUP,
    crate::commands::self_healing_settings::IPC_GROUP,
    crate::commands::session::IPC_GROUP,
    crate::commands::session_identity::IPC_GROUP,
    crate::commands::session_info::IPC_GROUP,
    crate::commands::setup_wizard::IPC_GROUP,
    crate::commands::shell_commands::IPC_GROUP,
    crate::commands::spec_drift::IPC_GROUP,
    crate::commands::spec_sync_state::IPC_GROUP,
    crate::commands::state_explorer::IPC_GROUP,
    crate::commands::state_machine::IPC_GROUP,
    crate::commands::state_machine_configs::IPC_GROUP,
    crate::commands::step_outputs::IPC_GROUP,
    crate::commands::storage::IPC_GROUP,
    crate::commands::subagent::IPC_GROUP,
    crate::commands::task_sync::IPC_GROUP,
    crate::commands::tenant::IPC_GROUP,
    crate::commands::terminal::IPC_GROUP,
    crate::commands::terminal_analysis::IPC_GROUP,
    crate::commands::terminal_windows::IPC_GROUP,
    crate::commands::test_orchestrator::IPC_GROUP,
    crate::commands::testing::IPC_GROUP,
    crate::commands::tiered_info::IPC_GROUP,
    crate::commands::token_analytics::IPC_GROUP,
    crate::commands::transcript::IPC_GROUP,
    crate::commands::ui_bridge::IPC_GROUP,
    crate::commands::ui_bridge_baselines::IPC_GROUP,
    crate::commands::verification::IPC_GROUP,
    crate::commands::video::IPC_GROUP,
    crate::commands::watchers::IPC_GROUP,
    crate::commands::web_integration::IPC_GROUP,
    crate::commands::window_manager::IPC_GROUP,
    crate::commands::workflow_events::IPC_GROUP,
    crate::commands::worktrees::IPC_GROUP,
    crate::config_report_cmd::IPC_GROUP,
    crate::coord_doctor_cmd::IPC_GROUP,
    crate::coord_drain_state::IPC_GROUP,
    crate::crash_dumps::IPC_GROUP,
    crate::doctor::commands::IPC_GROUP,
    crate::error_monitor::commands::IPC_GROUP,
    crate::error_monitor::workflow::IPC_GROUP,
    crate::mcp::steward::IPC_GROUP,
    crate::orchestration_loop::commands::IPC_GROUP,
    crate::process_capture::commands::IPC_GROUP,
    crate::prompt_library::IPC_GROUP,
    crate::repo_detection::IPC_GROUP,
    crate::spec_experimentation::commands::IPC_GROUP,
    crate::ui_error::IPC_GROUP,
];

/// Name -> owning group's handler, built once on first use.
fn routes() -> &'static HashMap<&'static str, GroupHandler> {
    static ROUTES: OnceLock<HashMap<&'static str, GroupHandler>> = OnceLock::new();
    ROUTES.get_or_init(|| {
        let mut map = HashMap::with_capacity(GROUPS.iter().map(|(n, _)| n.len()).sum());
        for (names, handler) in GROUPS {
            for name in *names {
                // A duplicate is a programming error the unit test below
                // rejects; first registration wins, matching a match arm.
                map.entry(*name).or_insert(*handler);
            }
        }
        map
    })
}

/// The group handler that owns `command`, if any.
pub(crate) fn lookup(command: &str) -> Option<GroupHandler> {
    routes().get(command).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_group_is_non_empty() {
        for (i, (names, _)) in GROUPS.iter().enumerate() {
            assert!(!names.is_empty(), "GROUPS[{i}] registers no commands");
        }
    }

    /// A name in two groups would route silently to whichever was inserted
    /// first, leaving the other group's command unreachable.
    #[test]
    fn no_command_name_is_in_two_groups() {
        let mut seen = HashSet::new();
        let dupes: Vec<&str> = GROUPS
            .iter()
            .flat_map(|(names, _)| names.iter().copied())
            .filter(|name| !seen.insert(*name))
            .collect();
        assert!(
            dupes.is_empty(),
            "command names registered by more than one group: {dupes:?}"
        );
    }

    #[test]
    fn every_group_name_routes_to_a_handler() {
        for (names, _) in GROUPS {
            for name in *names {
                assert!(lookup(name).is_some(), "`{name}` does not route");
            }
        }
        assert!(lookup("__no_such_command__").is_none());
    }

    /// A generated handler's frame is roughly (arm count) × this size at debug
    /// opt-level 0 (944 bytes on tauri 2.11.1 — plan
    /// `2026-10-02-split-run-app-invoke-handler` §5). If it grows past 4 KiB,
    /// group sizes need re-checking against the 128 KB frame budget.
    #[test]
    fn invoke_size_stays_within_the_frame_budget_model() {
        let size = std::mem::size_of::<Invoke<Wry>>();
        println!("size_of::<tauri::ipc::Invoke<tauri::Wry>>() = {size} bytes");
        assert!(
            size <= 4096,
            "Invoke<Wry> is {size} bytes; re-check ipc_group! sizes against the 128 KB frame budget"
        );
    }
}
