//! Helper functions for spawning child processes on Windows without console windows.
//!
//! On Windows, spawning any console program (cmd, powershell, python, node, npx,
//! git, cargo, etc.) from a GUI app creates a visible console window. These helpers
//! set `CREATE_NO_WINDOW` to suppress that. On non-Windows platforms they are
//! transparent passthroughs.

/// Create a `std::process::Command` with `CREATE_NO_WINDOW` on Windows.
pub fn no_window<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Create a `tokio::process::Command` with `CREATE_NO_WINDOW` on Windows.
pub fn tokio_no_window<S: AsRef<std::ffi::OsStr>>(program: S) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Convenience alias: `no_window("cmd.exe")`.
pub fn cmd_no_window() -> std::process::Command {
    no_window("cmd.exe")
}

/// Convenience alias: `tokio_no_window("cmd.exe")`.
pub fn tokio_cmd_no_window() -> tokio::process::Command {
    tokio_no_window("cmd.exe")
}
