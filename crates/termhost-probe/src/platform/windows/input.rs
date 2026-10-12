//! Input on Windows.
//!
//! The listener is Raw Input (`RIDEV_INPUTSINK` on a hidden window of a
//! dedicated thread): non-blocking, so it adds no latency to anyone's
//! keystrokes — unlike a low-level keyboard hook, which sits in every key's
//! path. Each event is classified by `RAWINPUTHEADER.hDevice` (NULL = injected)
//! and the extra-information field (`RAWKEYBOARD.ExtraInformation` /
//! `RAWMOUSE.ulExtraInformation`), which carries SendInput's `dwExtraInfo`:
//! injected with this run's tag = the probe; injected otherwise = someone
//! else's injection; from a device = real input. Only the device kind and the
//! origin are kept — never which key or where the pointer went.
//!
//! The injector sends VIRTUAL-KEY presses (`VK_A`..`VK_Z`, `VK_0`..`VK_9`)
//! with the tag, after checking that the input desktop is the interactive
//! one and that the target is foreground. Not KEYEVENTF_UNICODE (a VK_PACKET
//! delivered to the foreground thread): on the first hosted run (PR #2177,
//! run 38315983341, windows-latest) the Raw Input listener heard NONE of
//! such a key's events — it had recognised 0 of 2 about a second after the
//! key was sent, and the next arm's subscription had received no event of
//! it either. That is deafness, not latency. A virtual key goes through the
//! system's keyboard input path, which Raw Input reports for injected input
//! (`hDevice` NULL, the tag in `ExtraInformation`). The keys are lowercase letters and digits only (as
//! `--keys` allows), typed unshifted.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
    DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, HDESK, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    MAPVK_VK_TO_VSC, VIRTUAL_KEY,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassExW, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_QUIT,
    WNDCLASSEXW,
};

use super::{hwnd, window};
use crate::clock::now_ns;
use crate::model::{InputDevice, InputObs, InputOrigin};
use crate::platform::{InjectBlock, InputBus, InputMonitor, KeyInjector, Tagging, WindowHandle};

struct ListenerState {
    bus: Arc<InputBus>,
    tagging: Tagging,
}

thread_local! {
    static LISTENER: RefCell<Option<ListenerState>> = const { RefCell::new(None) };
}

fn classify(raw: &RAWINPUT, tag: u32) -> Option<(InputDevice, InputOrigin)> {
    let injected = raw.header.hDevice.is_invalid();
    let (device, extra) = if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
        // SAFETY: dwType says the union holds a keyboard record.
        (InputDevice::Keyboard, unsafe {
            raw.data.keyboard.ExtraInformation
        })
    } else if raw.header.dwType == RIM_TYPEMOUSE.0 {
        // SAFETY: dwType says the union holds a mouse record.
        (InputDevice::Pointer, unsafe {
            raw.data.mouse.ulExtraInformation
        })
    } else {
        (InputDevice::Other, 0)
    };
    let origin = match (injected, extra == tag) {
        (true, true) => InputOrigin::Probe,
        (true, false) => InputOrigin::ForeignInjected,
        (false, _) => InputOrigin::Real,
    };
    Some((device, origin))
}

unsafe extern "system" fn listener_proc(
    h: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        let t = now_ns();
        let mut size = 0u32;
        let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        // SAFETY: lparam is the HRAWINPUT of this WM_INPUT; the first call
        // only reports the size, the second fills an 8-byte-aligned buffer of
        // at least that size.
        unsafe {
            let hraw = HRAWINPUT(lparam.0 as *mut core::ffi::c_void);
            GetRawInputData(hraw, RID_INPUT, None, &mut size, header);
            if size > 0 && size as usize <= 4096 {
                let mut buf = vec![0u64; raw_input_words(size)];
                let got = GetRawInputData(
                    hraw,
                    RID_INPUT,
                    Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                    &mut size,
                    header,
                );
                if got != u32::MAX && got as usize >= std::mem::size_of::<RAWINPUTHEADER>() {
                    let raw = &*(buf.as_ptr() as *const RAWINPUT);
                    LISTENER.with(|s| {
                        if let Some(state) = s.borrow().as_ref() {
                            if let Some((device, origin)) = classify(raw, state.tagging.tag) {
                                if origin == InputOrigin::Probe {
                                    state.tagging.recognised.fetch_add(1, Ordering::Relaxed);
                                }
                                state.bus.publish(InputObs {
                                    t_ns: t,
                                    device,
                                    origin,
                                });
                            }
                        }
                    });
                }
            }
        }
    }
    // SAFETY: default processing (required for WM_INPUT cleanup).
    unsafe { DefWindowProcW(h, msg, wparam, lparam) }
}

/// The buffer (in 8-byte words, so it is aligned) for a raw input record of
/// `size` bytes. A keyboard record is SHORTER than `RAWINPUT` (whose union is
/// sized by `RAWMOUSE`), and a `&RAWINPUT` must never cover unallocated bytes,
/// so the buffer is at least `size_of::<RAWINPUT>()`.
fn raw_input_words(size: u32) -> usize {
    (size as usize)
        .max(std::mem::size_of::<RAWINPUT>())
        .div_ceil(8)
}

static CLASS_SEQ: AtomicU32 = AtomicU32::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn devices(
    target: HWND,
    flags: windows::Win32::UI::Input::RAWINPUTDEVICE_FLAGS,
) -> [RAWINPUTDEVICE; 2] {
    [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06, // keyboard
            dwFlags: flags,
            hwndTarget: target,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02, // mouse
            dwFlags: flags,
            hwndTarget: target,
        },
    ]
}

fn listener_thread(bus: Arc<InputBus>, tagging: Tagging, ready: mpsc::Sender<Result<u32, String>>) {
    let class = wide(&format!(
        "termhost-probe-rawinput-{}-{}",
        std::process::id(),
        CLASS_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // SAFETY: a window class and a hidden window owned by this thread; raw
    // input is registered for it and removed before the thread ends.
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            let _ = ready.send(Err("GetModuleHandleW failed".into()));
            return;
        };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(listener_proc),
            hInstance: module.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            let _ = ready.send(Err(format!("RegisterClassExW: {:?}", GetLastError())));
            return;
        }
        let h = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            w!("termhost-probe raw input"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("CreateWindowExW (listener): {e}")));
                return;
            }
        };
        LISTENER.with(|s| {
            *s.borrow_mut() = Some(ListenerState {
                bus: bus.clone(),
                tagging,
            })
        });
        if let Err(e) = RegisterRawInputDevices(
            &devices(h, RIDEV_INPUTSINK),
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        ) {
            let _ = DestroyWindow(h);
            let _ = ready.send(Err(format!("RegisterRawInputDevices: {e}")));
            return;
        }
        let _ = ready.send(Ok(GetCurrentThreadId()));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
        let _ = RegisterRawInputDevices(
            &devices(HWND::default(), RIDEV_REMOVE),
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        );
        let _ = DestroyWindow(h);
    }
    bus.set_health(Some("the raw input listener stopped".into()));
}

/// Stops the listener thread when dropped.
struct ListenerGuard {
    thread_id: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to the listener's own message queue.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start the Raw Input listener.
pub fn start_monitor(tagging: &Tagging) -> Result<InputMonitor, String> {
    let bus = InputBus::new();
    let (tx, rx) = mpsc::channel();
    let (b, t) = (bus.clone(), tagging.clone());
    let handle = std::thread::Builder::new()
        .name("raw-input".into())
        .spawn(move || listener_thread(b, t, tx))
        .map_err(|e| format!("spawning the raw input thread: {e}"))?;
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(thread_id)) => Ok(InputMonitor {
            bus,
            describe: format!(
                "Raw Input listener (RIDEV_INPUTSINK, keyboard + mouse), tag 0x{:08x}",
                tagging.tag
            ),
            guard: Box::new(ListenerGuard {
                thread_id,
                handle: Some(handle),
            }),
        }),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => Err("the raw input listener did not start within 5 s".into()),
    }
}

fn desktop_name(desk: HDESK) -> Result<String, String> {
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    // SAFETY: names a desktop handle the caller holds.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
    }
    .map_err(|e| format!("naming a desktop: {e}"))?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

/// The name of the current input desktop (`Default` when interactive and
/// unlocked), or why it cannot be opened (a secure desktop denies access).
pub fn input_desktop() -> Result<String, String> {
    // SAFETY: opens, names and closes a desktop handle.
    unsafe {
        let desk = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
            .map_err(|e| format!("OpenInputDesktop: {e} (a secure desktop — lock screen, UAC or Ctrl+Alt+Del — denies it)"))?;
        let named = desktop_name(desk);
        let _ = CloseDesktop(desk);
        named
    }
}

/// The desktop the calling thread runs on. Windows it creates live there and
/// its injected input targets it, so it must be the input desktop.
pub fn thread_desktop() -> Result<String, String> {
    // SAFETY: GetThreadDesktop returns a handle that is not ours to close.
    let desk = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
        .map_err(|e| format!("GetThreadDesktop: {e}"))?;
    desktop_name(desk)
}

/// Sends tagged keys to one window.
pub struct Injector {
    target: u64,
    tagging: Tagging,
}

impl Injector {
    pub fn new(target: WindowHandle, tagging: Tagging) -> Result<Injector, String> {
        // SAFETY: IsWindow accepts any value.
        if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd(target))) }
            .as_bool()
        {
            return Err(format!("{target} is not a window"));
        }
        Ok(Injector {
            target: target.0,
            tagging,
        })
    }
}

/// The virtual key that types `ch` unshifted: `VK_A`..`VK_Z` are the
/// uppercase ASCII codes, `VK_0`..`VK_9` the digits'.
fn virtual_key(ch: char) -> Option<u16> {
    match ch {
        'a'..='z' => Some(ch.to_ascii_uppercase() as u16),
        '0'..='9' => Some(ch as u16),
        _ => None,
    }
}

/// The key-down and key-up events that type `ch`, tagged with `extra`.
fn key_events(ch: char, extra: usize) -> Option<[INPUT; 2]> {
    let vk = virtual_key(ch)?;
    // SAFETY: a pure table lookup in the active keyboard layout.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    let event = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: extra,
            },
        },
    };
    Some([event(false), event(true)])
}

impl KeyInjector for Injector {
    fn check_target(&mut self) -> Result<(), InjectBlock> {
        match input_desktop() {
            Ok(name) if name.eq_ignore_ascii_case("Default") => {}
            Ok(name) => {
                return Err(InjectBlock::DesktopUnavailable(format!(
                    "the input desktop is {name:?}, not Default"
                )))
            }
            Err(why) => return Err(InjectBlock::DesktopUnavailable(why)),
        }
        if window::is_foreground(hwnd(WindowHandle(self.target))) {
            Ok(())
        } else {
            Err(InjectBlock::NotForeground(format!(
                "the foreground window is {}",
                window::foreground_description()
            )))
        }
    }

    fn can_send(&self, ch: char) -> bool {
        // A virtual key Raw Input reports: lowercase letters and digits.
        virtual_key(ch).is_some()
    }

    fn ledger_ids(&self, _ch: char) -> Vec<u32> {
        // Each event carries the run's tag in dwExtraInfo: no ledger needed.
        Vec::new()
    }

    fn heard(&self) -> u64 {
        self.tagging.recognised.load(Ordering::Relaxed)
    }

    fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String> {
        let extra = if tagged { self.tagging.tag as usize } else { 0 };
        let Some(inputs) = key_events(ch, extra) else {
            return Err(format!(
                "{ch:?} has no virtual key here (lowercase letters and digits only)"
            ));
        };
        // SAFETY: two well-formed INPUT records; SendInput reads them.
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != 2 {
            // SAFETY: reads the thread's last error.
            return Err(format!("SendInput sent {sent} of 2 events ({:?}; UIPI blocks injection into a more privileged window)", unsafe {
                GetLastError()
            }));
        }
        if tagged {
            self.tagging.sent.fetch_add(2, Ordering::Relaxed);
        }
        Ok(())
    }

    fn activate(&mut self) -> Result<(), String> {
        if window::activate(hwnd(WindowHandle(self.target))) {
            Ok(())
        } else {
            Err(format!(
                "the target did not become foreground (the foreground is {})",
                window::foreground_description()
            ))
        }
    }

    fn describe(&self) -> String {
        format!(
            "SendInput (virtual keys) into {} with tag 0x{:08x}",
            WindowHandle(self.target),
            self.tagging.tag
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_virtual_keys_raw_input_reports_not_unicode_packets() {
        let tag = 0x3e2a_5220usize;
        let [down, up] = key_events('a', tag).expect("a letter");
        // SAFETY: reading the keyboard variant just built.
        let (d, u) = unsafe { (down.Anonymous.ki, up.Anonymous.ki) };
        assert_eq!(d.wVk, VIRTUAL_KEY(0x41), "VK_A, not a VK_PACKET carrier");
        assert_eq!(d.dwFlags, Default::default(), "no KEYEVENTF_UNICODE");
        assert_eq!(u.dwFlags, KEYEVENTF_KEYUP);
        assert_eq!((d.dwExtraInfo, u.dwExtraInfo), (tag, tag));
        let seven = key_events('7', 0).expect("a digit");
        // SAFETY: as above.
        assert_eq!(unsafe { seven[0].Anonymous.ki.wVk }, VIRTUAL_KEY(0x37));
        for ch in ['A', 'é', ' ', '~'] {
            assert!(key_events(ch, tag).is_none(), "{ch:?}");
        }
    }

    #[test]
    fn a_raw_input_buffer_always_covers_a_whole_rawinput() {
        let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        // A keyboard record: the header plus RAWKEYBOARD, shorter than RAWINPUT.
        let keyboard = header + 16;
        assert!(keyboard < std::mem::size_of::<RAWINPUT>() as u32);
        for size in [
            keyboard,
            header,
            1,
            std::mem::size_of::<RAWINPUT>() as u32,
            4096,
        ] {
            assert!(
                raw_input_words(size) * 8 >= std::mem::size_of::<RAWINPUT>(),
                "size {size}"
            );
            assert!(raw_input_words(size) * 8 >= size as usize);
        }
    }
}
