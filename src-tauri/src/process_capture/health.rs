//! Port-based health checking for managed processes.

use std::net::SocketAddr;
use std::time::Duration;

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
/// Returns true if a process was found and kill was attempted.
#[cfg(windows)]
pub async fn kill_port_process(port: u16) -> bool {
    use std::process::Command;

    // Find PID using netstat
    let output = match Command::new("cmd")
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
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(pid_str) = parts.last() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid > 0 {
                    let _ = Command::new("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .output();
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(not(windows))]
pub async fn kill_port_process(port: u16) -> bool {
    use std::process::Command;

    let output = match Command::new("lsof")
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
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
                return true;
            }
        }
    }

    false
}
