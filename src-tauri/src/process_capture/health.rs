//! Port-based health checking for managed processes.

use std::net::SocketAddr;
use std::time::Duration;

/// Cheaply check whether a PID is still alive. Used by the port_health_loop
/// to detect inner service-worker death while the outer `cmd.exe` shim is
/// still running. Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
/// + `GetExitCodeProcess`; Unix: `kill(pid, 0)`.
///
/// Returns `false` for PID 0, for permission-denied, and for any PID that
/// has exited or never existed.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code as *mut u32);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // `kill(pid, 0)` returns 0 if the process exists and we have permission
    // to signal it; -1 + ESRCH if it does not exist.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Force-kill an entire descendant subtree rooted at `root_pid`, including
/// the root. Uses the same BFS shape as `kill_port_process` but takes a PID
/// instead of a port — needed by Phase 5 restart-time reaping when the
/// tracked descendant list is known directly.
///
/// Returns the number of `Stop-Process` / `kill -9` calls attempted.
#[cfg(windows)]
pub async fn kill_descendant_tree(root_pid: u32) -> usize {
    let script = format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$rootPid = {root_pid}
if ($rootPid -le 0) {{ exit 1 }}

$childrenByParent = @{{}}
foreach ($p in (Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)) {{
    $ppid = [int]$p.ParentProcessId
    if (-not $childrenByParent.ContainsKey($ppid)) {{ $childrenByParent[$ppid] = New-Object System.Collections.Generic.List[int] }}
    [void]$childrenByParent[$ppid].Add([int]$p.ProcessId)
}}

$toKill = New-Object System.Collections.Generic.HashSet[int]
$queue = New-Object System.Collections.Generic.Queue[int]
[void]$queue.Enqueue([int]$rootPid)
while ($queue.Count -gt 0) {{
    $current = $queue.Dequeue()
    if (-not $toKill.Add($current)) {{ continue }}
    if ($childrenByParent.ContainsKey($current)) {{
        foreach ($c in $childrenByParent[$current]) {{ [void]$queue.Enqueue($c) }}
    }}
}}

$count = $toKill.Count
foreach ($k in $toKill) {{ Stop-Process -Id $k -Force }}
Write-Output $count
exit 0
"#
    );

    let output = match crate::process_helpers::no_window("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return 0,
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .last()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(not(windows))]
pub async fn kill_descendant_tree(root_pid: u32) -> usize {
    if root_pid == 0 {
        return 0;
    }
    // Best-effort: SIGKILL the root and any process whose ppid chain leads
    // back to it. /proc walk mirrors process_tree::snapshot_process_table_sync.
    let mut to_kill = std::collections::HashSet::new();
    to_kill.insert(root_pid);

    if let Ok(entries) = std::fs::read_dir("/proc") {
        // Build child->parent index first.
        let mut parent_of: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for entry in entries.flatten() {
            let s = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let pid: u32 = match s.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let close = match stat.rfind(')') {
                Some(i) => i,
                None => continue,
            };
            let tail = &stat[close + 1..];
            let fields: Vec<&str> = tail.split_whitespace().collect();
            let ppid: u32 = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            parent_of.insert(pid, ppid);
        }
        // Any PID whose ancestry chain reaches root_pid is a descendant.
        for (&pid, _) in parent_of.iter() {
            let mut cur = pid;
            for _ in 0..64 {
                match parent_of.get(&cur).copied() {
                    Some(p) if p == root_pid => {
                        to_kill.insert(pid);
                        break;
                    }
                    Some(0) | None => break,
                    Some(p) => cur = p,
                }
            }
        }
    }

    let mut killed = 0usize;
    for pid in to_kill {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        killed += 1;
    }
    killed
}

/// Check if a port is currently in use (accepting connections).
pub fn is_port_in_use(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let socket = match socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };

    socket
        .connect_timeout(&addr.into(), Duration::from_millis(500))
        .is_ok()
}

/// Wait for a port to become free (not accepting connections).
/// Returns true if the port is free, false if timeout elapsed.
pub async fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = Duration::from_millis(200);

    loop {
        if !is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(check_interval).await;
    }
}

/// Wait for a port to start accepting connections.
/// Returns true if the port is up, false if timeout elapsed.
pub async fn wait_for_port_ready(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = Duration::from_millis(500);

    loop {
        if is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(check_interval).await;
    }
}

/// Kill the process using a specific port (Windows-specific).
/// Returns true if at least one kill was attempted.
///
/// Walks the descendant tree of every PID that owns a listening socket on
/// the port and force-kills each. Walking via `Win32_Process.ParentProcessId`
/// is essential because uvicorn / multiprocessing workers often outlive the
/// original `OwningProcess` reported by `Get-NetTCPConnection`: the parent
/// exits after handing the listener fd to a child, leaving netstat pointing
/// at a dead PID while the actual port holder is an orphan child. A plain
/// `taskkill /F /PID <dead>` (or even `/T`) does nothing in that case.
#[cfg(windows)]
pub async fn kill_port_process(port: u16) -> bool {
    let script = format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$port = {port}
$rootPids = @(Get-NetTCPConnection -LocalPort $port -State Listen | Select-Object -ExpandProperty OwningProcess -Unique | Where-Object {{ $_ -gt 0 }})
if ($rootPids.Count -eq 0) {{
    $netstat = & netstat -ano
    foreach ($line in $netstat) {{
        if ($line -match 'LISTENING' -and $line -match ":$port\b") {{
            $parts = $line.Trim() -split '\s+'
            $candidate = [int]$parts[-1]
            if ($candidate -gt 0 -and $rootPids -notcontains $candidate) {{ $rootPids += $candidate }}
        }}
    }}
}}
if ($rootPids.Count -eq 0) {{ exit 1 }}

$childrenByParent = @{{}}
foreach ($p in (Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)) {{
    $ppid = [int]$p.ParentProcessId
    if (-not $childrenByParent.ContainsKey($ppid)) {{ $childrenByParent[$ppid] = New-Object System.Collections.Generic.List[int] }}
    [void]$childrenByParent[$ppid].Add([int]$p.ProcessId)
}}

$toKill = New-Object System.Collections.Generic.HashSet[int]
$queue = New-Object System.Collections.Generic.Queue[int]
foreach ($r in $rootPids) {{ [void]$queue.Enqueue([int]$r) }}
while ($queue.Count -gt 0) {{
    $current = $queue.Dequeue()
    if (-not $toKill.Add($current)) {{ continue }}
    if ($childrenByParent.ContainsKey($current)) {{
        foreach ($c in $childrenByParent[$current]) {{ [void]$queue.Enqueue($c) }}
    }}
}}

foreach ($k in $toKill) {{ Stop-Process -Id $k -Force }}
exit 0
"#
    );

    let attempted = matches!(
        crate::process_helpers::no_window("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output(),
        Ok(o) if o.status.success()
    );

    if attempted {
        return true;
    }

    // Fallback: original netstat + taskkill path. Keeps us functional if
    // PowerShell isn't on PATH for some reason. Adds /T so direct children
    // die with the parent.
    let output = match crate::process_helpers::cmd_no_window()
        .args([
            "/C",
            &format!("netstat -ano | findstr LISTENING | findstr :{}", port),
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut killed_any = false;
    let mut seen = std::collections::HashSet::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(pid_str) = parts.last() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid > 0 && seen.insert(pid) {
                    let _ = crate::process_helpers::no_window("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .output();
                    killed_any = true;
                }
            }
        }
    }

    killed_any
}

#[cfg(not(windows))]
pub async fn kill_port_process(port: u16) -> bool {
    let output = match crate::process_helpers::no_window("lsof")
        .args(["-ti", &format!(":{}", port)])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            if pid > 0 {
                let _ = crate::process_helpers::no_window("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
                return true;
            }
        }
    }

    false
}
