//! Automation-stack capability health — the `/health` `automationStack` object
//! (plan `2026-08-29-merytshost-has-no-display-stack-for-ui-automation`,
//! Phase 5).
//!
//! ## Why
//!
//! A headless Linux box runs the runner perfectly happily and then fails EVERY
//! UI action, one action at a time, with a different error each time: `xcap`
//! reports no monitors, `wmctrl`/`xdotool` are missing binaries, the recorder's
//! ffmpeg cannot open the X display, AT-SPI has no bus. Nothing on `/health`
//! said "this box cannot automate", so the diagnosis was re-derived from
//! scratch per failing action. This module answers that question ONCE, on the
//! endpoint every fleet probe already polls.
//!
//! ## Shape: named capabilities with a verdict + a reason
//!
//! Each entry is `{verdict, required, reason, value, checkedAt}` and the
//! capability map ALWAYS carries every key. That shape is deliberate and is
//! meant to be joined, not sat beside: the sibling plan
//! `2026-08-24-headless-box-has-no-working-coord-credential-door` proposes a
//! `credentialDoors` summary on this same payload, and two independently-shaped
//! capability summaries on one `/health` is the divergence we are heading off.
//!
//! ## UNKNOWN is emitted, never defaulted away
//!
//! [`crate::session::tracking_health::health_json`] has the bug this module
//! must not repeat: its pre-first-pass arm OMITS `liveClaudeTotal` /
//! `trackedOpenTotal`, so a consumer cannot tell "no sessions" from "never
//! checked". Here every key is present in every arm, and a probe that could not
//! run reports `"unknown"` with a reason — NEVER `false`. `false` is a measured
//! negative; absence of a measurement is not one.
//!
//! ## Latency
//!
//! `/health` is on a latency budget (sampled up to 10s on a loaded box), so the
//! synchronous probes here are an env read and a handful of `stat`s. The one
//! probe that can block — the AT-SPI D-Bus round trip — is NOT run inline: a
//! detached task ([`run_periodic`]) refreshes it on a slow interval under a
//! hard timeout and publishes the result, exactly the way `tracking_health`
//! publishes its last pass. Before the first pass the entry reads `unknown`
//! with `checkedAt: null`.
//!
//! Wayland is deliberately out of scope (plan decision D2) — the probes here
//! describe the X11 stack only.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tracing::debug;

/// How often the AT-SPI bus probe re-runs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ATSPI_CHECK_INTERVAL: Duration = Duration::from_secs(300);
/// Boot delay before the first AT-SPI probe — the a11y bus is activated on
/// demand, so probing during startup would measure the launch race, not the
/// steady state.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ATSPI_INITIAL_DELAY: Duration = Duration::from_secs(20);
/// Hard ceiling on one AT-SPI probe. A wedged D-Bus daemon must produce an
/// `unknown` verdict, never a hung task.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ATSPI_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Display resolution (shared with the video recorder)
// ---------------------------------------------------------------------------

/// The X11 display the runner will actually bind to, from its inherited
/// environment — `None` when `DISPLAY` is unset or empty.
///
/// This is the SINGLE resolution: `xcap`, `wmctrl`/`xdotool` and AT-SPI all
/// read `$DISPLAY` out of the process environment themselves, and
/// `video_recorder`'s x11grab arm calls this so the recorder stops being the
/// one consumer that ignored it. Do not re-derive it elsewhere — a second
/// answer here is a box that records a different screen than it clicks on.
pub fn resolve_display() -> Option<String> {
    std::env::var("DISPLAY")
        .ok()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Local abstract/unix socket path a `:N` display listens on, when the value
/// has that form. `None` for a networked (`host:0`) or otherwise non-local
/// display, which we do not claim to be able to probe.
fn local_display_socket(display: &str) -> Option<PathBuf> {
    let rest = display.strip_prefix(':')?;
    // `:N` or `:N.S` — the screen suffix does not change the socket.
    let num = rest.split('.').next()?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(PathBuf::from(format!("/tmp/.X11-unix/X{num}")))
}

// ---------------------------------------------------------------------------
// Capability verdicts
// ---------------------------------------------------------------------------

/// A capability's resolution state. `Unknown` is a first-class answer — it is
/// what a probe that could not RUN reports, and it never collapses to
/// `Unresolvable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Resolvable,
    Unresolvable,
    Unknown,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Resolvable => "resolvable",
            Verdict::Unresolvable => "unresolvable",
            Verdict::Unknown => "unknown",
        }
    }
}

/// One named capability: a verdict, whether this platform's automation depends
/// on it, why the verdict is what it is, the concrete resolved value (a display
/// string, an absolute binary path) when there is one, and when it was
/// measured.
#[derive(Debug, Clone)]
pub struct Capability {
    pub verdict: Verdict,
    pub required: bool,
    pub reason: String,
    pub value: Option<String>,
    /// Unix millis of the measurement; `None` only when no measurement has
    /// been taken yet (the AT-SPI entry before its first pass).
    pub checked_at_ms: Option<i64>,
}

impl Capability {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": self.verdict.as_str(),
            "required": self.required,
            "reason": self.reason,
            "value": self.value,
            "checkedAt": self.checked_at_ms,
        })
    }
}

// ---------------------------------------------------------------------------
// PATH resolution
// ---------------------------------------------------------------------------

/// First executable named `program` on the inherited `PATH`.
///
/// The inherited PATH is the point: `window_manager` and the recorder shell out
/// with a bare program name, so what matters is what the RUNNER's environment
/// resolves — not what a login shell would.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat", "com"] {
                let c = dir.join(format!("{program}.{ext}"));
                if is_executable(&c) {
                    return Some(c);
                }
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Capability entry for a PATH-resolved binary.
fn tool_capability(program: &str, required: bool, now_ms: i64) -> Capability {
    match which(program) {
        Some(p) => Capability {
            verdict: Verdict::Resolvable,
            required,
            reason: format!("`{program}` resolves on the runner's inherited PATH"),
            value: Some(p.to_string_lossy().to_string()),
            checked_at_ms: Some(now_ms),
        },
        None => Capability {
            verdict: Verdict::Unresolvable,
            required,
            reason: format!("`{program}` is not on the runner's inherited PATH"),
            value: None,
            checked_at_ms: Some(now_ms),
        },
    }
}

// ---------------------------------------------------------------------------
// Display capability
// ---------------------------------------------------------------------------

/// Capability entry for the X11 display binding: whether one is bound, and
/// WHICH — the same [`resolve_display`] answer the recorder and every
/// `$DISPLAY`-reading subsystem use.
fn display_capability(now_ms: i64) -> Capability {
    if !cfg!(target_os = "linux") {
        return Capability {
            verdict: Verdict::Unknown,
            required: false,
            reason: "not an X11 platform — display binding is not probed here (the X11 stack \
                     probes are Linux-only; Wayland is out of scope by plan decision D2)"
                .to_string(),
            value: None,
            checked_at_ms: Some(now_ms),
        };
    }

    let Some(display) = resolve_display() else {
        return Capability {
            verdict: Verdict::Unresolvable,
            required: true,
            reason: "DISPLAY is unset — screen capture, window control, AT-SPI and the x11grab \
                     recorder have no display to bind"
                .to_string(),
            value: None,
            checked_at_ms: Some(now_ms),
        };
    };

    match local_display_socket(&display) {
        Some(sock) if sock.exists() => Capability {
            verdict: Verdict::Resolvable,
            required: true,
            reason: format!(
                "DISPLAY={display} and its X socket {} exists",
                sock.display()
            ),
            value: Some(display),
            checked_at_ms: Some(now_ms),
        },
        Some(sock) => Capability {
            verdict: Verdict::Unresolvable,
            required: true,
            reason: format!(
                "DISPLAY={display} but no X server is listening — {} does not exist",
                sock.display()
            ),
            value: Some(display),
            checked_at_ms: Some(now_ms),
        },
        // A networked or otherwise non-`:N` display. It may work perfectly; we
        // just have no cheap local probe for it, and an unprobed display is
        // UNKNOWN, never a negative.
        None => Capability {
            verdict: Verdict::Unknown,
            required: true,
            reason: format!(
                "DISPLAY={display} is not a local `:N` display — liveness was not probed"
            ),
            value: Some(display),
            checked_at_ms: Some(now_ms),
        },
    }
}

// ---------------------------------------------------------------------------
// AT-SPI bus probe (out-of-band, cached)
// ---------------------------------------------------------------------------

/// Result of one AT-SPI bus probe.
#[derive(Debug, Clone)]
struct AtspiProbe {
    checked_at_ms: i64,
    verdict: Verdict,
    reason: String,
}

/// Latest completed AT-SPI probe — `/health` reads this, [`run_periodic`] is
/// the sole writer. `None` until the first pass completes.
static LATEST_ATSPI: OnceLock<Mutex<Option<AtspiProbe>>> = OnceLock::new();

fn latest_atspi_cell() -> &'static Mutex<Option<AtspiProbe>> {
    LATEST_ATSPI.get_or_init(|| Mutex::new(None))
}

fn latest_atspi() -> Option<AtspiProbe> {
    latest_atspi_cell().lock().ok().and_then(|g| g.clone())
}

fn store_atspi(probe: AtspiProbe) {
    if let Ok(mut g) = latest_atspi_cell().lock() {
        *g = Some(probe);
    }
}

/// Capability entry for the AT-SPI accessibility bus, read from the cache.
fn atspi_capability(now_ms: i64) -> Capability {
    if !cfg!(target_os = "linux") {
        return Capability {
            verdict: Verdict::Unresolvable,
            required: false,
            reason: "AT-SPI is Linux-only — this platform drives accessibility through UIA/AX \
                     instead, so the AT-SPI bus is not part of its automation stack"
                .to_string(),
            value: None,
            checked_at_ms: Some(now_ms),
        };
    }

    match latest_atspi() {
        Some(p) => Capability {
            verdict: p.verdict,
            required: true,
            reason: p.reason,
            value: None,
            checked_at_ms: Some(p.checked_at_ms),
        },
        // Explicit UNKNOWN with a null timestamp: the probe has not run, which
        // is not the same statement as "the bus does not answer".
        None => Capability {
            verdict: Verdict::Unknown,
            required: true,
            reason: "the AT-SPI bus probe has not completed a pass yet".to_string(),
            value: None,
            checked_at_ms: None,
        },
    }
}

/// One bounded AT-SPI probe: connect to the accessibility bus and read the
/// registry root's children. Connecting alone only proves an address was
/// resolved, so the round trip is what we assert on.
#[cfg(target_os = "linux")]
async fn probe_atspi_once() -> AtspiProbe {
    use atspi::proxy::accessible::AccessibleProxy;
    use atspi::AccessibilityConnection;
    use zbus::proxy::CacheProperties;

    let now_ms = chrono::Utc::now().timestamp_millis();

    let attempt = async {
        let conn = AccessibilityConnection::new()
            .await
            .map_err(|e| format!("could not connect to the AT-SPI bus: {e}"))?;
        let proxy = AccessibleProxy::builder(conn.connection())
            .destination("org.a11y.atspi.Registry")
            .map_err(|e| format!("bad AT-SPI registry destination: {e}"))?
            .path("/org/a11y/atspi/accessible/root")
            .map_err(|e| format!("bad AT-SPI registry path: {e}"))?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(|e| format!("could not build the AT-SPI registry proxy: {e}"))?;
        let children = proxy
            .get_children()
            .await
            .map_err(|e| format!("the AT-SPI registry did not answer: {e}"))?;
        Ok::<usize, String>(children.len())
    };

    match tokio::time::timeout(ATSPI_PROBE_TIMEOUT, attempt).await {
        Ok(Ok(n)) => AtspiProbe {
            checked_at_ms: now_ms,
            verdict: Verdict::Resolvable,
            reason: format!("the AT-SPI registry answered — {n} application(s) on the bus"),
        },
        Ok(Err(e)) => AtspiProbe {
            checked_at_ms: now_ms,
            verdict: Verdict::Unresolvable,
            reason: e,
        },
        // A timeout is not a measured negative: the bus may be alive and slow.
        Err(_) => AtspiProbe {
            checked_at_ms: now_ms,
            verdict: Verdict::Unknown,
            reason: format!(
                "the AT-SPI bus probe exceeded its {}s ceiling — bus state is unknown, not absent",
                ATSPI_PROBE_TIMEOUT.as_secs()
            ),
        },
    }
}

/// Detached periodic AT-SPI probe — spawned once at startup (main.rs setup),
/// alongside the session-tracking and build-drift loops. Every pass is bounded
/// and fail-open; a non-Linux build has nothing to probe and returns
/// immediately.
pub async fn run_periodic() {
    #[cfg(target_os = "linux")]
    {
        tokio::time::sleep(ATSPI_INITIAL_DELAY).await;
        loop {
            let probe = probe_atspi_once().await;
            match probe.verdict {
                Verdict::Resolvable => debug!(reason = %probe.reason, "AT-SPI bus probe: ok"),
                _ => tracing::warn!(
                    verdict = probe.verdict.as_str(),
                    reason = %probe.reason,
                    "AT-SPI bus probe: the accessibility bus is not usable — \
                     accessibility-driven automation will fail on this box"
                ),
            }
            store_atspi(probe);
            tokio::time::sleep(ATSPI_CHECK_INTERVAL).await;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        debug!("AT-SPI bus probe: not a Linux build — nothing to probe");
    }
}

// ---------------------------------------------------------------------------
// /health object
// ---------------------------------------------------------------------------

/// The full capability set, in the order it is reported.
fn capabilities(now_ms: i64) -> Vec<(&'static str, Capability)> {
    // On a non-X11 platform the X11 binaries genuinely do not resolve, and
    // saying so is honest — but that platform's automation does not depend on
    // them, so `required` is false there and they never drag the rollup down.
    let x11_required = cfg!(target_os = "linux");
    vec![
        ("display", display_capability(now_ms)),
        // Window enumeration and activation (`window_manager::list_windows_linux`
        // tries wmctrl first, then xdotool) — but xdotool is also the only
        // synthetic-input path, so neither is a fallback for the other.
        ("wmctrl", tool_capability("wmctrl", x11_required, now_ms)),
        ("xdotool", tool_capability("xdotool", x11_required, now_ms)),
        // Required on every platform: the video recorder shells out to ffmpeg
        // regardless of which grabber the platform arm selects.
        ("ffmpeg", tool_capability("ffmpeg", true, now_ms)),
        // Informational: these PROVISION a display rather than use one. Their
        // absence is only interesting on a box that has no display bound.
        ("xvfb", tool_capability("Xvfb", false, now_ms)),
        ("x11vnc", tool_capability("x11vnc", false, now_ms)),
        ("atspiBus", atspi_capability(now_ms)),
    ]
}

/// The `/health` `automationStack` object: can this box drive a GUI, and if
/// not, which named capability is missing and why.
///
/// Every key is emitted in every arm — see the module docs on why an omitted
/// key is the defect this shape exists to avoid.
pub fn health_json() -> serde_json::Value {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let caps = capabilities(now_ms);

    let named = |v: Verdict| -> Vec<&'static str> {
        caps.iter()
            .filter(|(_, c)| c.required && c.verdict == v)
            .map(|(n, _)| *n)
            .collect()
    };
    let unresolvable = named(Verdict::Unresolvable);
    let unknown = named(Verdict::Unknown);
    let required_total = caps.iter().filter(|(_, c)| c.required).count();

    // Rollup over the REQUIRED capabilities only, and unresolvable outranks
    // unknown: a known-missing dependency is a firmer verdict than an
    // unmeasured one.
    let (verdict, reason) = if !unresolvable.is_empty() {
        (
            Verdict::Unresolvable,
            format!(
                "{} of {required_total} required capabilities unresolvable: {}",
                unresolvable.len(),
                unresolvable.join(", ")
            ),
        )
    } else if !unknown.is_empty() {
        (
            Verdict::Unknown,
            format!(
                "{} of {required_total} required capabilities unknown (not measured): {}",
                unknown.len(),
                unknown.join(", ")
            ),
        )
    } else {
        (
            Verdict::Resolvable,
            format!("all {required_total} required capabilities resolvable"),
        )
    };

    let mut map = serde_json::Map::new();
    for (name, cap) in &caps {
        map.insert((*name).to_string(), cap.json());
    }

    serde_json::json!({
        "platform": std::env::consts::OS,
        "verdict": verdict.as_str(),
        "reason": reason,
        "checkedAt": now_ms,
        "capabilities": serde_json::Value::Object(map),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_display_socket_only_for_bare_numeric_displays() {
        assert_eq!(
            local_display_socket(":99"),
            Some(PathBuf::from("/tmp/.X11-unix/X99"))
        );
        assert_eq!(
            local_display_socket(":0.0"),
            Some(PathBuf::from("/tmp/.X11-unix/X0"))
        );
        // Networked / malformed forms are not probed.
        assert_eq!(local_display_socket("host.example.com:0"), None);
        assert_eq!(local_display_socket(":"), None);
        assert_eq!(local_display_socket(":abc"), None);
    }

    /// A capability entry ALWAYS carries all five keys, including explicit
    /// nulls — the `tracking_health` omission bug must not reappear here.
    #[test]
    fn capability_entry_always_carries_every_key() {
        let cap = Capability {
            verdict: Verdict::Unknown,
            required: true,
            reason: "not measured".to_string(),
            value: None,
            checked_at_ms: None,
        };
        let v = cap.json();
        for key in ["verdict", "required", "reason", "value", "checkedAt"] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
        assert!(v["value"].is_null());
        assert!(v["checkedAt"].is_null());
        assert_eq!(v["verdict"], "unknown");
    }

    /// The whole capability map is emitted whatever the environment looks
    /// like, and every entry has the same key set.
    #[test]
    fn health_json_emits_the_full_capability_map() {
        let v = health_json();
        assert!(v["platform"].is_string());
        assert!(v["verdict"].is_string());
        assert!(v["reason"].is_string());
        assert!(v["checkedAt"].is_i64());
        let caps = v["capabilities"].as_object().expect("capabilities object");
        for name in [
            "display", "wmctrl", "xdotool", "ffmpeg", "xvfb", "x11vnc", "atspiBus",
        ] {
            let entry = caps.get(name).unwrap_or_else(|| panic!("missing {name}"));
            for key in ["verdict", "required", "reason", "value", "checkedAt"] {
                assert!(
                    entry.get(key).is_some(),
                    "capability {name} missing key {key}"
                );
            }
        }
        assert_eq!(caps.len(), 7);
    }

    /// Before any pass, the AT-SPI entry is an explicit `unknown` with a null
    /// timestamp — never `false`, never an omitted key.
    #[cfg(target_os = "linux")]
    #[test]
    fn atspi_never_probed_reports_unknown_not_false() {
        // Only meaningful when no pass has been stored by another test in this
        // binary; assert the never-probed branch directly instead.
        if latest_atspi().is_none() {
            let cap = atspi_capability(42);
            assert_eq!(cap.verdict, Verdict::Unknown);
            assert!(cap.checked_at_ms.is_none());
            assert!(cap.required);
        }
        // And a stored pass flows straight through.
        store_atspi(AtspiProbe {
            checked_at_ms: 7,
            verdict: Verdict::Resolvable,
            reason: "answered".to_string(),
        });
        let cap = atspi_capability(42);
        assert_eq!(cap.verdict, Verdict::Resolvable);
        assert_eq!(cap.checked_at_ms, Some(7));
    }

    /// A binary that certainly is not installed resolves to `unresolvable`
    /// with a reason; one that certainly is (this test binary's own dir is not
    /// on PATH, so use a POSIX staple) resolves to an absolute path.
    #[test]
    fn tool_capability_reports_path_or_reason() {
        let missing = tool_capability("qontinui-definitely-not-a-real-binary", true, 1);
        assert_eq!(missing.verdict, Verdict::Unresolvable);
        assert!(missing.value.is_none());
        assert!(missing
            .reason
            .contains("not on the runner's inherited PATH"));

        #[cfg(unix)]
        {
            let sh = tool_capability("sh", true, 1);
            assert_eq!(sh.verdict, Verdict::Resolvable);
            assert!(sh.value.is_some());
        }
    }
}
