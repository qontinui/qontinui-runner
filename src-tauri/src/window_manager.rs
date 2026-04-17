//! OS-level window enumeration and activation.
//!
//! Provides cross-platform support for listing visible top-level windows
//! and bringing them to the foreground — used by the UI Bridge to activate
//! browser windows when `window.focus()` is blocked by the browser.
//!
//! Platform implementations:
//! - **Windows**: Win32 `EnumWindows` / `SetForegroundWindow` via the `windows` crate
//! - **Linux**: Subprocess calls to `wmctrl` or `xdotool`
//! - **macOS**: Subprocess calls to `osascript` (AppleScript)

use serde::Serialize;

/// Information about a single top-level OS window.
#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub pid: u32,
    pub title: String,
    pub class_name: String,
    pub is_visible: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// List visible top-level windows, optionally filtered by title substring.
pub fn list_windows(filter: Option<&str>) -> Vec<WindowInfo> {
    #[cfg(target_os = "windows")]
    {
        list_windows_windows(filter)
    }
    #[cfg(target_os = "linux")]
    {
        list_windows_linux(filter)
    }
    #[cfg(target_os = "macos")]
    {
        list_windows_macos(filter)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = filter;
        Vec::new()
    }
}

/// Activate (bring to foreground) a window by PID.
pub fn activate_window_by_pid(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        activate_window_windows_by_pid(pid)
    }
    #[cfg(target_os = "linux")]
    {
        activate_window_linux_by_pid(pid)
    }
    #[cfg(target_os = "macos")]
    {
        activate_window_macos_by_pid(pid)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        false
    }
}

/// Activate a window whose title contains the given substring.
pub fn activate_window_by_title(title_contains: &str) -> bool {
    let windows = list_windows(Some(title_contains));
    if let Some(w) = windows.first() {
        activate_window_by_pid(w.pid)
    } else {
        false
    }
}

// ─── Windows implementation ───────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn list_windows_windows(filter: Option<&str>) -> Vec<WindowInfo> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::sync::Mutex;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible,
    };

    // Collect windows in a thread-safe vec via the EnumWindows callback.
    let results: Mutex<Vec<WindowInfo>> = Mutex::new(Vec::new());
    let filter_lower = filter.map(|f| f.to_lowercase());

    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // Safety: lparam carries a pointer to our (Mutex<Vec<WindowInfo>>, Option<String>) tuple.
        let context =
            &*(lparam.0 as *const (Mutex<Vec<WindowInfo>>, Option<String>));

        // Skip invisible windows.
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        // Get title.
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len == 0 {
            return TRUE; // skip untitled windows
        }
        let mut title_buf = vec![0u16; (title_len + 1) as usize];
        GetWindowTextW(hwnd, &mut title_buf);
        let title = OsString::from_wide(&title_buf[..title_len as usize])
            .to_string_lossy()
            .into_owned();

        // Apply filter.
        if let Some(ref f) = context.1 {
            if !title.to_lowercase().contains(f) {
                return TRUE;
            }
        }

        // Get class name.
        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf);
        let class_name = OsString::from_wide(&class_buf[..class_len as usize])
            .to_string_lossy()
            .into_owned();

        // Get PID.
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        // Get window rect.
        let mut rect = windows::Win32::Foundation::RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);

        let info = WindowInfo {
            pid,
            title,
            class_name,
            is_visible: true,
            x: rect.left,
            y: rect.top,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        };

        if let Ok(mut vec) = context.0.lock() {
            vec.push(info);
        }
        TRUE
    }

    let context = (results, filter_lower);
    unsafe {
        let _ = EnumWindows(
            Some(enum_callback),
            LPARAM(&context as *const _ as isize),
        );
    }
    context.0.into_inner().unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn activate_window_windows_by_pid(pid: u32) -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::Input::KeyboardAndMouse::keybd_event;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SW_RESTORE, GetWindowTextLengthW,
    };

    // First find an appropriate HWND for the given PID.
    use std::sync::Mutex;
    let found: Mutex<Option<HWND>> = Mutex::new(None);
    let target_pid = pid;

    unsafe extern "system" fn find_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &*(lparam.0 as *const (Mutex<Option<HWND>>, u32));

        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        // Skip untitled windows
        if GetWindowTextLengthW(hwnd) == 0 {
            return TRUE;
        }

        let mut wnd_pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut wnd_pid));

        if wnd_pid == context.1 {
            if let Ok(mut slot) = context.0.lock() {
                *slot = Some(hwnd);
            }
            return BOOL(0); // stop enumeration
        }
        TRUE
    }

    let context = (found, target_pid);
    unsafe {
        let _ = EnumWindows(
            Some(find_callback),
            LPARAM(&context as *const _ as isize),
        );
    }

    let hwnd = match context.0.into_inner().unwrap_or(None) {
        Some(h) => h,
        None => return false,
    };

    unsafe {
        // Restore if minimized.
        let _ = ShowWindow(hwnd, SW_RESTORE);

        // The keybd_event trick: sending a no-op keystroke allows
        // SetForegroundWindow to succeed even if we're not the foreground thread.
        keybd_event(0, 0, Default::default(), 0);
        SetForegroundWindow(hwnd).as_bool()
    }
}

// ─── Linux implementation ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn list_windows_linux(filter: Option<&str>) -> Vec<WindowInfo> {
    // Try wmctrl first, fall back to xdotool.
    if let Ok(output) = std::process::Command::new("wmctrl")
        .args(["-l", "-p"])
        .output()
    {
        if output.status.success() {
            return parse_wmctrl_output(&String::from_utf8_lossy(&output.stdout), filter);
        }
    }

    // Fallback: xdotool
    if let Ok(output) = std::process::Command::new("xdotool")
        .args(["search", "--name", ""])
        .output()
    {
        if output.status.success() {
            return parse_xdotool_output(&String::from_utf8_lossy(&output.stdout), filter);
        }
    }

    Vec::new()
}

#[cfg(target_os = "linux")]
fn parse_wmctrl_output(stdout: &str, filter: Option<&str>) -> Vec<WindowInfo> {
    let filter_lower = filter.map(|f| f.to_lowercase());
    let mut results = Vec::new();

    for line in stdout.lines() {
        // wmctrl -l -p format: <wid> <desktop> <pid> <host> <title...>
        let parts: Vec<&str> = line.splitn(5, char::is_whitespace).collect();
        if parts.len() < 5 {
            continue;
        }
        // Filter out whitespace-only segments
        let non_empty: Vec<&str> = line.split_whitespace().collect();
        if non_empty.len() < 4 {
            continue;
        }
        let pid = non_empty.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        // Title is everything after the 4th whitespace-separated token
        let title = non_empty[4..].join(" ");

        if let Some(ref f) = filter_lower {
            if !title.to_lowercase().contains(f) {
                continue;
            }
        }

        results.push(WindowInfo {
            pid,
            title,
            class_name: String::new(), // wmctrl doesn't provide class by default
            is_visible: true,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
    }
    results
}

#[cfg(target_os = "linux")]
fn parse_xdotool_output(stdout: &str, filter: Option<&str>) -> Vec<WindowInfo> {
    let filter_lower = filter.map(|f| f.to_lowercase());
    let mut results = Vec::new();

    for line in stdout.lines() {
        let wid = line.trim();
        if wid.is_empty() {
            continue;
        }

        // Get window name via xdotool
        let title = std::process::Command::new("xdotool")
            .args(["getwindowname", wid])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        if let Some(ref f) = filter_lower {
            if !title.to_lowercase().contains(f) {
                continue;
            }
        }

        let pid = std::process::Command::new("xdotool")
            .args(["getwindowpid", wid])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .unwrap_or(0);

        results.push(WindowInfo {
            pid,
            title,
            class_name: String::new(),
            is_visible: true,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
    }
    results
}

#[cfg(target_os = "linux")]
fn activate_window_linux_by_pid(pid: u32) -> bool {
    // Try wmctrl first: find window ID for PID, then activate.
    if let Ok(output) = std::process::Command::new("wmctrl")
        .args(["-l", "-p"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    if let Ok(wnd_pid) = parts[2].parse::<u32>() {
                        if wnd_pid == pid {
                            let wid = parts[0];
                            return std::process::Command::new("wmctrl")
                                .args(["-i", "-a", wid])
                                .status()
                                .map(|s| s.success())
                                .unwrap_or(false);
                        }
                    }
                }
            }
        }
    }

    // Fallback: xdotool search by pid, then activate first match.
    if let Ok(output) = std::process::Command::new("xdotool")
        .args(["search", "--pid", &pid.to_string()])
        .output()
    {
        if output.status.success() {
            if let Some(wid) = String::from_utf8_lossy(&output.stdout).lines().next() {
                return std::process::Command::new("xdotool")
                    .args(["windowactivate", wid.trim()])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            }
        }
    }

    false
}

// ─── macOS implementation ─────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn list_windows_macos(filter: Option<&str>) -> Vec<WindowInfo> {
    // Use osascript to get visible processes with their names and PIDs.
    let script = r#"
        set output to ""
        tell application "System Events"
            set procs to every process whose visible is true
            repeat with p in procs
                set procName to name of p
                set procPid to unix id of p
                set output to output & procPid & "||" & procName & linefeed
            end repeat
        end tell
        return output
    "#;

    let output = match std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };

    let filter_lower = filter.map(|f| f.to_lowercase());
    let mut results = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, "||").collect();
        if parts.len() < 2 {
            continue;
        }
        let pid = parts[0].trim().parse::<u32>().unwrap_or(0);
        let title = parts[1].trim().to_string();

        if let Some(ref f) = filter_lower {
            if !title.to_lowercase().contains(f) {
                continue;
            }
        }

        results.push(WindowInfo {
            pid,
            title,
            class_name: String::new(),
            is_visible: true,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
    }
    results
}

#[cfg(target_os = "macos")]
fn activate_window_macos_by_pid(pid: u32) -> bool {
    let script = format!(
        r#"tell application "System Events" to set frontmost of (first process whose unix id is {}) to true"#,
        pid
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
