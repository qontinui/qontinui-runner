//! The platform seam. Everything that touches a display, an input queue or
//! the OS process table goes through these traits, so the engine, the inject
//! arm and the selftest are exercised without a desktop (tests/common's fake
//! desktop implements them in memory).
//!
//! Implementations: [`windows`] (WGC, DXGI duplication, Raw Input, SendInput,
//! PDH, GDI) and [`linux`] (X11 damage + shm, XInput2 raw events, XTEST,
//! /proc). Other platforms get explicit "unsupported" answers, which the
//! doctor reports as INOPERATIVE-ON-THIS-MACHINE.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use crate::codec::Marker;
use crate::model::{InputObs, WindowSample};
use crate::raster::FrameView;
use crate::records::SourceId;

pub mod common;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(not(any(windows, target_os = "linux")))]
pub mod unsupported;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "linux")]
pub use linux as native;
#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported as native;
#[cfg(windows)]
pub use windows as native;

/// A top-level window: an HWND on Windows, an X window id on X11.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub u64);

impl std::fmt::Display for WindowHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

/// How the user named the window to measure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetSpec {
    Handle(u64),
    /// The largest visible top-level window owned by this process.
    Pid(u32),
    /// The largest visible top-level window whose title contains `text`;
    /// with `unique`, exactly one window may match (`inject`: a title is not
    /// enough to be sure keys go where they should).
    Title {
        text: String,
        unique: bool,
    },
}

/// One captured frame, borrowed for the duration of the callback.
pub struct RawFrame<'a> {
    /// Frame time on the probe clock.
    pub t_ns: u64,
    /// When the OLDEST content this frame shows was presented, on the probe
    /// clock, known independently of when the probe got to it: `t_ns` itself
    /// for WGC and Desktop Duplication (their frame time IS the presentation
    /// time); for X11 the oldest DAMAGE event's server timestamp, mapped onto
    /// the probe clock. `None` when it is not known (X11's first frame, or a
    /// server clock not yet established): the frame's lag is then unknown,
    /// never 0.
    pub presented_ns: Option<u64>,
    pub view: FrameView<'a>,
    /// Where `view`'s (0, 0) sits in window coordinates, so marker rects are
    /// reported window-relative for every source.
    pub origin: (i32, i32),
    /// Frames the source coalesced into this one; `None` for a source that
    /// cannot detect skipped frames (WGC, X11).
    pub missed: Option<u32>,
    /// The full window size this frame belongs to.
    pub window_size: (u32, u32),
}

/// What one `next_frame` call produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureStatus {
    Frame,
    Timeout,
    /// Lost but recoverable (a desktop switch, a resolution change); the source
    /// re-establishes itself on the next call.
    Lost(String),
    /// Permanently over (the window closed).
    Ended(String),
}

/// A capture source over one window.
pub trait CaptureSource: Send {
    fn id(&self) -> SourceId;
    fn describe(&self) -> String;
    /// Wait up to `timeout` for the next frame; call `f` with it when one
    /// arrives.
    fn next_frame(&mut self, timeout: Duration, f: &mut dyn FnMut(RawFrame<'_>)) -> CaptureStatus;
}

/// Samples the measured window's state.
pub trait WindowInspector: Send {
    fn sample(&mut self, t_ns: u64) -> WindowSample;
    fn describe(&self) -> String;
}

/// This run's injection tag: what makes the probe's own input
/// distinguishable from an operator's.
///
/// - Windows: `tag` rides in every `SendInput` event's `dwExtraInfo` and comes
///   back in Raw Input's `ExtraInformation`; injected input with any other
///   value is someone else's.
/// - X11: XTEST events carry no payload, so the injector records each key it
///   is about to send in `ledger` and the listener matches XTEST-sourced raw
///   events against it; an unmatched event — a physical device, or another
///   XTEST client — is not ours. A listener in ANOTHER process (`watch`)
///   reads the keys `inject` announces in its `inject.jsonl` before sending
///   them ([`Tagging::ledger_file`]).
#[derive(Clone, Debug)]
pub struct Tagging {
    pub tag: u32,
    pub ledger: Arc<Mutex<VecDeque<u32>>>,
    /// Tagged events the injector has sent.
    pub sent: Arc<AtomicU64>,
    /// Tagged events the listener has recognised.
    pub recognised: Arc<AtomicU64>,
    /// Another process's announced keys.
    ledger_log: Option<Arc<Mutex<LedgerLog>>>,
}

/// Tails an `inject.jsonl` for `inject_intent` records.
#[derive(Debug)]
struct LedgerLog {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl LedgerLog {
    /// Append the ledger ids of every intent written since the last read.
    fn refresh(&mut self, ledger: &mut VecDeque<u32>) {
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        let Ok(n) = f.read_to_end(&mut buf) else {
            return;
        };
        self.offset += n as u64;
        self.partial.extend_from_slice(&buf);
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("inject_intent") {
                continue;
            }
            if let Some(ids) = v.get("ledger").and_then(|l| l.as_array()) {
                ledger.extend(
                    ids.iter()
                        .filter_map(|i| i.as_u64())
                        .filter_map(|i| u32::try_from(i).ok()),
                );
            }
        }
    }
}

/// Remove `id` from the ledger if present (in order, tolerating reordering).
fn take_entry(ledger: &mut VecDeque<u32>, id: u32) -> bool {
    match ledger.iter().position(|&e| e == id) {
        Some(i) => {
            ledger.remove(i);
            true
        }
        None => false,
    }
}

impl Tagging {
    /// A fresh tag for this run: never 0 (what an untagged injection carries)
    /// and never in the 0xFF515700 family Windows uses to mark pen and touch
    /// input.
    pub fn new() -> Tagging {
        let seed = crate::clock::now_ns() ^ (u64::from(std::process::id()) << 32);
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let mut tag = (z ^ (z >> 31)) as u32;
        if tag == 0 || (tag & 0xFFFF_FF00) == 0xFF51_5700 {
            tag = 0x7B4E_0001;
        }
        Tagging {
            tag,
            ledger: Arc::new(Mutex::new(VecDeque::new())),
            sent: Arc::new(AtomicU64::new(0)),
            recognised: Arc::new(AtomicU64::new(0)),
            ledger_log: None,
        }
    }

    /// Also recognise the keys announced in `path` (another process's
    /// `inject.jsonl`; it need not exist yet).
    pub fn ledger_file(&mut self, path: &Path) {
        self.ledger_log = Some(Arc::new(Mutex::new(LedgerLog {
            path: path.to_path_buf(),
            offset: 0,
            partial: Vec::new(),
        })));
    }

    /// Is `id` one of this run's announced events? Consumes the matching
    /// entry. When the in-memory ledger has none, newly announced keys are
    /// read from the ledger file first: `inject` writes and flushes each key's
    /// intent BEFORE sending it, so by the time its event arrives the intent
    /// is on disk.
    pub fn match_ledger(&self, id: u32) -> bool {
        let Ok(mut ledger) = self.ledger.lock() else {
            return false;
        };
        if take_entry(&mut ledger, id) {
            return true;
        }
        match &self.ledger_log {
            Some(log) => {
                if let Ok(mut log) = log.lock() {
                    log.refresh(&mut ledger);
                }
                take_entry(&mut ledger, id)
            }
            None => false,
        }
    }
}

impl Default for Tagging {
    fn default() -> Self {
        Tagging::new()
    }
}

type Queue = Arc<Mutex<VecDeque<InputObs>>>;

/// One real-input listener fanned out to several consumers (the engine's
/// interval accounting AND the inject arm's abort check): Raw Input allows one
/// registration per process, so there is one listener and many subscriptions.
#[derive(Debug, Default)]
pub struct InputBus {
    subscribers: Mutex<Vec<Weak<Mutex<VecDeque<InputObs>>>>>,
    health: Mutex<Option<String>>,
    published: AtomicU64,
}

impl InputBus {
    pub fn new() -> Arc<InputBus> {
        Arc::new(InputBus::default())
    }

    pub fn publish(&self, e: InputObs) {
        self.published.fetch_add(1, Ordering::Relaxed);
        let mut subs = self.subscribers.lock().expect("input bus");
        subs.retain(|w| match w.upgrade() {
            Some(q) => {
                q.lock().expect("input queue").push_back(e);
                true
            }
            None => false,
        });
    }

    /// Record that the listener failed (`Some(why)`) or recovered (`None`).
    pub fn set_health(&self, problem: Option<String>) {
        *self.health.lock().expect("input bus health") = problem;
    }

    pub fn health(&self) -> Result<(), String> {
        match &*self.health.lock().expect("input bus health") {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }

    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        let q: Queue = Arc::new(Mutex::new(VecDeque::new()));
        self.subscribers
            .lock()
            .expect("input bus")
            .push(Arc::downgrade(&q));
        Subscription {
            bus: self.clone(),
            queue: q,
        }
    }

    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }
}

/// One consumer's view of the input bus.
pub struct Subscription {
    bus: Arc<InputBus>,
    queue: Queue,
}

impl Subscription {
    pub fn drain(&mut self) -> Vec<InputObs> {
        self.queue.lock().expect("input queue").drain(..).collect()
    }

    pub fn health(&self) -> Result<(), String> {
        self.bus.health()
    }
}

/// A running real-input listener.
pub struct InputMonitor {
    pub bus: Arc<InputBus>,
    pub describe: String,
    /// Keeps the listener alive; dropping it stops the listener.
    pub guard: Box<dyn Send>,
}

/// Why a key may not be sent right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InjectBlock {
    /// Another window is foreground.
    NotForeground(String),
    /// The input desktop is locked, a secure desktop, or not interactive.
    DesktopUnavailable(String),
    /// The check itself failed.
    Unknown(String),
}

impl std::fmt::Display for InjectBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InjectBlock::NotForeground(d) => write!(f, "target not foreground: {d}"),
            InjectBlock::DesktopUnavailable(d) => write!(f, "input desktop unavailable: {d}"),
            InjectBlock::Unknown(d) => write!(f, "foreground check failed: {d}"),
        }
    }
}

/// Sends keys to one target window.
pub trait KeyInjector: Send {
    /// Is it safe to send a key to the target right now? (Foreground, and an
    /// interactive, unlocked input desktop.)
    fn check_target(&mut self) -> Result<(), InjectBlock>;
    /// Whether `ch` can be typed at all — checked for EVERY key of an arm
    /// before the first is sent, so an arm never stops half-typed.
    fn can_send(&self, ch: char) -> bool;
    /// The ids the listener will match `ch`'s press and release against (X11
    /// ledger ids); empty where each event carries the run's tag itself.
    fn ledger_ids(&self, ch: char) -> Vec<u32>;
    /// Send one character as a key press and release. `tagged` marks it as
    /// this run's; untagged exists for the selftest's must-abort proof.
    fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String>;
    /// Tagged events this run's listener has recognised so far: a listener
    /// that stops hearing the probe's own keys would not hear an operator's.
    fn heard(&self) -> u64;
    /// Try once to make the target foreground (`--activate`).
    fn activate(&mut self) -> Result<(), String>;
    fn describe(&self) -> String;
}

/// Where the selftest window paints a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaintSlot {
    /// Row 0: output and calibration markers.
    Output,
    /// Row 1: echo markers.
    Echo,
}

/// A key the selftest window received and echoed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoPainted {
    pub seq: u32,
    pub ch: char,
    pub t_painted_ns: u64,
}

/// The selftest window's layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TestGeometry {
    pub cell_w: u32,
    pub cell_h: u32,
    /// Client-area origins of the output and echo marker rows.
    pub output_at: (u32, u32),
    pub echo_at: (u32, u32),
}

/// Options for a selftest or doctor probe window.
#[derive(Clone, Debug, PartialEq)]
pub struct TestWindowOptions {
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub client_w: u32,
    pub client_h: u32,
    /// Become foreground (the selftest); the doctor's probe window must not.
    pub activate: bool,
    /// Keep above other windows (the doctor's probe, so DDA sees it).
    pub topmost: bool,
    pub cell_w: u32,
    pub cell_h: u32,
    /// The session id echo markers carry.
    pub echo_session: u16,
}

/// A window the probe owns and paints markers into.
pub trait TestWindow: Send + Sync {
    fn handle(&self) -> WindowHandle;
    fn geometry(&self) -> TestGeometry;
    /// Paint `marker` (or clear the slot) and return the probe-clock time just
    /// after the drawing was flushed.
    fn paint(&self, slot: PaintSlot, marker: Option<Marker>) -> Result<u64, String>;
    /// Cover (or uncover) this window with a second, opaque window of ours.
    fn set_occluded(&self, on: bool) -> Result<(), String>;
    /// Make this window foreground; Ok(true) when it is.
    fn activate(&self) -> Result<bool, String>;
    /// Stop servicing this window's messages for `d` (the OS "not responding"
    /// proof). Returns once the freeze has begun.
    fn freeze(&self, d: Duration) -> Result<(), String>;
    fn echoes(&self) -> Vec<EchoPainted>;
    fn close(&self);
}

/// Per-process memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcMem {
    /// The catalog's number: private bytes (Windows) / PSS (Linux).
    pub catalog_bytes: u64,
    pub rss_bytes: u64,
}

/// GPU engine utilisation by PID and engine type.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuReading {
    Unsupported(String),
    Failed(String),
    Measured(HashMap<u32, BTreeMap<String, f64>>),
}

/// Reads per-process accounting.
pub trait ProcessReader: Send {
    /// Cumulative user + kernel CPU time, ns.
    fn cpu_ns(&mut self, pid: u32) -> Option<u64>;
    fn memory(&mut self, pid: u32) -> Option<ProcMem>;
    fn gpu(&mut self) -> GpuReading;
}

/// The display's refresh interval and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Refresh {
    pub hz: f64,
    pub ns: u64,
    pub source: String,
    /// Nothing reported a rate; `hz` is an assumed 60. KPIs counted in refresh
    /// intervals are then unknown(refresh_rate_unknown).
    pub assumed: bool,
}

impl Refresh {
    pub fn from_hz(hz: f64, source: impl Into<String>) -> Refresh {
        Refresh {
            hz,
            ns: (1e9 / hz).round() as u64,
            source: source.into(),
            assumed: false,
        }
    }

    /// 60 Hz, assumed because no rate could be read (`why`).
    pub fn assumed_60(why: impl Into<String>) -> Refresh {
        Refresh {
            assumed: true,
            ..Refresh::from_hz(60.0, format!("assumed: {}", why.into()))
        }
    }
}

/// Everything the platform offers, behind one seam the selftest drives.
pub trait Host {
    fn doctor_facts(&mut self, node: &str) -> crate::doctor::Facts;
    /// The capture sources this platform has, primary first.
    fn source_kinds(&self) -> Vec<SourceId>;
    fn open_source(
        &mut self,
        kind: SourceId,
        target: WindowHandle,
    ) -> Result<Box<dyn CaptureSource>, String>;
    fn inspector(&mut self, target: WindowHandle) -> Result<Box<dyn WindowInspector>, String>;
    fn input_monitor(&mut self, tagging: &Tagging) -> Result<InputMonitor, String>;
    fn injector(
        &mut self,
        target: WindowHandle,
        tagging: &Tagging,
    ) -> Result<Box<dyn KeyInjector>, String>;
    fn create_test_window(
        &mut self,
        opts: &TestWindowOptions,
    ) -> Result<Arc<dyn TestWindow>, String>;
    fn refresh(&mut self, target: WindowHandle) -> Refresh;
    fn process_reader(&mut self) -> Box<dyn ProcessReader>;
    /// Whether the OS reports hung windows (IsHungAppWindow).
    fn reports_hung_windows(&self) -> bool;
    fn resolve_target(&mut self, spec: &TargetSpec) -> Result<WindowHandle, String>;
}

/// Set by Ctrl+C / SIGINT / SIGTERM; `watch` finishes its summary and exits.
pub static STOP: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_announced_by_another_process_are_recognised_once() {
        let dir = std::env::temp_dir().join(format!(
            "termhost-probe-ledger-{}-{}",
            std::process::id(),
            crate::clock::now_ns()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let log = dir.join("inject.jsonl");
        let mut t = Tagging::new();
        t.ledger_file(&log);
        // Nothing announced yet (the file does not even exist).
        assert!(!t.match_ledger(38));
        std::fs::write(
            &log,
            concat!(
                r#"{"schema":"termhost-probe/v1","type":"start","tool":"termhost-probe"}"#,
                "\n",
                r#"{"schema":"termhost-probe/v1","type":"inject_intent","index":0,"key":"a","ledger":[38,294],"t_ns":"5"}"#,
                "\n"
            ),
        )
        .expect("write");
        assert!(t.match_ledger(38), "the announced press");
        assert!(t.match_ledger(294), "the announced release");
        assert!(!t.match_ledger(38), "each announced event is matched once");
        // Without a ledger file only the in-process ledger counts.
        let plain = Tagging::new();
        assert!(!plain.match_ledger(38));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
