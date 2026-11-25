//! The listener fds a self-managed upgrade hands to its successor through the
//! `LISTEN_FDS` / `LISTEN_PID` / `LISTEN_FDNAMES` protocol (sd_listen_fds(3);
//! plan D1, "Self-managed").
//!
//! The runner binds its one fixed listener (`127.0.0.1:9876`) in
//! `mcp_api/serve.rs`; plan Phase 2's `listeners::adopt_or_bind` owns that
//! site. Whatever binds or adopts a listener registers its raw fd here under
//! the `LISTEN_FDNAMES` name it must reach the successor by (`api`). The
//! executor reads [`registered`]; an empty registry means a self-managed
//! handoff is NOT available on this process, and the preflight says so.
//!
//! The registry holds raw fd NUMBERS, not ownership: the listener stays owned
//! by the server that serves it until the process exits.

use std::sync::Mutex;

static FDS: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());

/// Record that listener `name` is open on `fd` in this process. A later
/// registration under the same name replaces the earlier one.
pub fn register(name: &str, fd: i32) {
    let mut fds = FDS.lock().unwrap_or_else(|p| p.into_inner());
    fds.retain(|(n, _)| n != name);
    fds.push((name.to_string(), fd));
}

/// Every registered listener, in registration order.
pub fn registered() -> Vec<(String, i32)> {
    FDS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The `LISTEN_FDS` / `LISTEN_FDNAMES` values for `fds` (the successor's fds
/// start at 3, in this order). `LISTEN_PID` is the successor's own pid, set by
/// the spawner at exec time.
pub fn listen_env(fds: &[(String, i32)]) -> Vec<(String, String)> {
    vec![
        ("LISTEN_FDS".to_string(), fds.len().to_string()),
        (
            "LISTEN_FDNAMES".to_string(),
            fds.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(":"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_replaces_by_name_and_env_names_fds_in_order() {
        register("upgrade-test-a", 7);
        register("upgrade-test-b", 9);
        register("upgrade-test-a", 8);
        let mine: Vec<(String, i32)> = registered()
            .into_iter()
            .filter(|(n, _)| n.starts_with("upgrade-test-"))
            .collect();
        assert_eq!(
            mine,
            vec![
                ("upgrade-test-b".to_string(), 9),
                ("upgrade-test-a".to_string(), 8)
            ]
        );
        assert_eq!(
            listen_env(&mine),
            vec![
                ("LISTEN_FDS".to_string(), "2".to_string()),
                (
                    "LISTEN_FDNAMES".to_string(),
                    "upgrade-test-b:upgrade-test-a".to_string()
                ),
            ]
        );
    }
}
