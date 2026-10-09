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
const GROUPS: &[GroupEntry] = &[crate::commands::meta_optimizer::IPC_GROUP];

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
