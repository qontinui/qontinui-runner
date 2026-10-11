//! Input on X11.
//!
//! The listener selects XInput2 RAW events on the root window (XI 2.2, so
//! they arrive regardless of grabs): every key, button and motion from every
//! device, without sitting in anyone's input path. Each raw event names its
//! source device. A physical device is real input. XTEST injections all come
//! from the server's XTEST devices and carry no payload, so the probe's own are
//! told apart by the run's LEDGER: the injector records each key event it is
//! about to send, and an XTEST event that matches no ledger entry is someone
//! else's injection. Only device kind and origin are kept — never which key.
//!
//! The injector sends keys with XTEST to the focused window, checking before
//! each that the target holds the focus. On a private Xvfb display nothing
//! else can type, which is why injection there is always safe (D9).

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, ConnectionExt as _, EventMask as XiEventMask, XIEventMask};
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, InputFocus, StackMode, Window, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;

use super::x11::{wait_readable, Display};
use crate::clock::now_ns;
use crate::model::{InputDevice, InputObs, InputOrigin};
use crate::platform::{InjectBlock, InputBus, InputMonitor, KeyInjector, Tagging, WindowHandle};

/// A ledger entry: the keycode, plus 0x100 for a release.
fn ledger_id(keycode: u32, release: bool) -> u32 {
    keycode | if release { 0x100 } else { 0 }
}

/// The XTEST slave devices' ids.
fn xtest_devices(d: &Display) -> Result<BTreeSet<u16>, String> {
    let reply = d
        .conn
        .xinput_xi_query_device(xinput::Device::ALL)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XIQueryDevice: {e}"))?;
    Ok(reply
        .infos
        .iter()
        .filter(|i| String::from_utf8_lossy(&i.name).contains("XTEST"))
        .map(|i| i.deviceid)
        .collect())
}

fn classify(
    tagging: &Tagging,
    xtest: &BTreeSet<u16>,
    source: u16,
    ledger: Option<u32>,
    device: InputDevice,
) -> InputObs {
    let origin = if xtest.contains(&source) {
        match ledger {
            Some(id) if tagging.match_ledger(id) => InputOrigin::Probe,
            _ => InputOrigin::ForeignInjected,
        }
    } else {
        InputOrigin::Real
    };
    if origin == InputOrigin::Probe {
        tagging.recognised.fetch_add(1, Ordering::Relaxed);
    }
    InputObs {
        t_ns: now_ns(),
        device,
        origin,
    }
}

fn listener(
    d: Display,
    bus: Arc<InputBus>,
    tagging: Tagging,
    xtest: BTreeSet<u16>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        loop {
            let ev = match d.conn.poll_for_event() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(e) => {
                    bus.set_health(Some(format!(
                        "the XInput2 listener's X connection failed: {e}"
                    )));
                    return;
                }
            };
            let obs = match ev {
                Event::XinputRawKeyPress(e) => Some(classify(
                    &tagging,
                    &xtest,
                    e.sourceid,
                    Some(ledger_id(e.detail, false)),
                    InputDevice::Keyboard,
                )),
                Event::XinputRawKeyRelease(e) => Some(classify(
                    &tagging,
                    &xtest,
                    e.sourceid,
                    Some(ledger_id(e.detail, true)),
                    InputDevice::Keyboard,
                )),
                Event::XinputRawButtonPress(e)
                | Event::XinputRawButtonRelease(e)
                | Event::XinputRawMotion(e) => Some(classify(
                    &tagging,
                    &xtest,
                    e.sourceid,
                    None,
                    InputDevice::Pointer,
                )),
                _ => None,
            };
            if let Some(o) = obs {
                bus.publish(o);
            }
        }
        wait_readable(&d.conn, Duration::from_millis(100));
    }
}

struct ListenerGuard {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start the XInput2 raw-event listener.
pub fn start_monitor(tagging: &Tagging) -> Result<InputMonitor, String> {
    let d = Display::open()?;
    if !d.has_extension("XInputExtension") {
        return Err("the X server has no XInput extension".into());
    }
    let v = d
        .conn
        .xinput_xi_query_version(2, 2)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XIQueryVersion: {e}"))?;
    if v.major_version < 2 {
        return Err(format!(
            "XInput {}.{} has no raw events (need 2.x)",
            v.major_version, v.minor_version
        ));
    }
    let xtest = xtest_devices(&d)?;
    let mask = XIEventMask::RAW_KEY_PRESS
        | XIEventMask::RAW_KEY_RELEASE
        | XIEventMask::RAW_BUTTON_PRESS
        | XIEventMask::RAW_BUTTON_RELEASE
        | XIEventMask::RAW_MOTION;
    d.conn
        .xinput_xi_select_events(
            d.root,
            &[XiEventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![mask],
            }],
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("XISelectEvents (raw events on the root): {e}"))?;
    let describe = format!(
        "XInput {}.{} raw-event listener on {} (XTEST devices {:?} matched against the run ledger)",
        v.major_version, v.minor_version, d.name, xtest
    );
    let bus = InputBus::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (b, t, s) = (bus.clone(), tagging.clone(), stop.clone());
    let handle = std::thread::Builder::new()
        .name("xi2-raw".into())
        .spawn(move || listener(d, b, t, xtest, s))
        .map_err(|e| format!("spawning the listener: {e}"))?;
    Ok(InputMonitor {
        bus,
        describe,
        guard: Box::new(ListenerGuard {
            stop,
            handle: Some(handle),
        }),
    })
}

/// Whether the X server offers XTEST.
pub fn xtest_available(d: &Display) -> Result<String, String> {
    if !d.has_extension("XTEST") {
        return Err("the X server has no XTEST extension".into());
    }
    let v = d
        .conn
        .xtest_get_version(2, 2)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XTestGetVersion: {e}"))?;
    Ok(format!(
        "XTEST {}.{} with a per-run ledger",
        v.major_version, v.minor_version
    ))
}

/// Keycodes for the printable ASCII letters and digits (column 0 keysyms equal
/// the ASCII code for these).
fn keymap(d: &Display) -> Result<HashMap<char, u8>, String> {
    let setup = d.conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let count = max - min + 1;
    let reply = d
        .conn
        .get_keyboard_mapping(min, count)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("GetKeyboardMapping: {e}"))?;
    let per = usize::from(reply.keysyms_per_keycode.max(1));
    let mut out = HashMap::new();
    for (i, chunk) in reply.keysyms.chunks(per).enumerate() {
        let Some(&sym) = chunk.first() else { continue };
        if let Some(ch) =
            char::from_u32(sym).filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            out.entry(ch).or_insert(min + i as u8);
        }
    }
    if out.is_empty() {
        return Err("the keyboard mapping has no keycode for any ASCII letter".into());
    }
    Ok(out)
}

/// Sends tagged keys to one X window with XTEST.
pub struct Injector {
    d: Display,
    target: Window,
    tagging: Tagging,
    keys: HashMap<char, u8>,
}

impl Injector {
    pub fn new(target: WindowHandle, tagging: Tagging) -> Result<Injector, String> {
        let d = Display::open()?;
        let w = u32::try_from(target.0).map_err(|_| format!("{target} is not an X window id"))?;
        if !d.exists(w) {
            return Err(format!("{target} is not a window"));
        }
        xtest_available(&d)?;
        let keys = keymap(&d)?;
        Ok(Injector {
            d,
            target: w,
            tagging,
            keys,
        })
    }
}

impl KeyInjector for Injector {
    fn check_target(&mut self) -> Result<(), InjectBlock> {
        if !self.d.exists(self.target) {
            return Err(InjectBlock::Unknown("the target window is gone".into()));
        }
        if self.d.is_foreground(self.target) {
            Ok(())
        } else {
            Err(InjectBlock::NotForeground(
                "the X keyboard focus is not inside the target (or the window manager says another window is active)".into(),
            ))
        }
    }

    fn can_send(&self, ch: char) -> bool {
        self.keys.contains_key(&ch)
    }

    fn ledger_ids(&self, ch: char) -> Vec<u32> {
        self.keys.get(&ch).map_or_else(Vec::new, |&code| {
            vec![
                ledger_id(u32::from(code), false),
                ledger_id(u32::from(code), true),
            ]
        })
    }

    fn heard(&self) -> u64 {
        self.tagging.recognised.load(Ordering::Relaxed)
    }

    fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String> {
        let code = *self
            .keys
            .get(&ch)
            .ok_or_else(|| format!("no keycode for {ch:?}"))?;
        if tagged {
            if let Ok(mut l) = self.tagging.ledger.lock() {
                l.push_back(ledger_id(u32::from(code), false));
                l.push_back(ledger_id(u32::from(code), true));
            }
        }
        let root = self.d.root;
        for ty in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
            self.d
                .conn
                .xtest_fake_input(ty, code, x11rb::CURRENT_TIME, root, 0, 0, 0)
                .map_err(|e| format!("XTestFakeInput: {e}"))?;
        }
        self.d.sync()?;
        if tagged {
            self.tagging.sent.fetch_add(2, Ordering::Relaxed);
        }
        Ok(())
    }

    fn activate(&mut self) -> Result<(), String> {
        let _ = self.d.conn.configure_window(
            self.target,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        self.d
            .conn
            .set_input_focus(InputFocus::PARENT, self.target, x11rb::CURRENT_TIME)
            .map_err(|e| e.to_string())?;
        self.d.sync()?;
        if self.d.is_foreground(self.target) {
            Ok(())
        } else {
            Err("the target did not take the input focus".into())
        }
    }

    fn describe(&self) -> String {
        format!("XTEST into X window 0x{:x} on {}", self.target, self.d.name)
    }
}
