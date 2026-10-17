//! X11 capture (D2): DAMAGE tells the probe the moment anything is drawn —
//! event-driven, no polling — and a MIT-SHM `GetImage` (or plain `GetImage`
//! when MIT-SHM is unavailable) reads the damaged rows of the window from the
//! root window's framebuffer. The frame time is `CLOCK_MONOTONIC` when the
//! damage batch was received.
//!
//! The probe's own lag is measured against when the damage HAPPENED, not when
//! it was drained. Every DamageNotify carries the server's millisecond
//! timestamp of the drawing (the server updates its current time before
//! stamping the event), and round trips on a private, never-mapped window map
//! that clock onto the probe's ([`ServerClock`]). A frame's presentation time
//! is the oldest damage it shows, so damage that queued in the socket while
//! the probe was busy scanning shows up as lag — and trips the `behind` guard
//! — instead of vanishing into a frame time taken when the queue was finally
//! drained.
//!
//! The read covers the window's full width over the damaged rows plus a
//! margin: a host that repaints only the changed cells of a marker (the start
//! sentinel is the same every time) still has its whole marker re-read.
//! Pixels are read into a shared-memory segment, scanned in place, and never
//! written anywhere.

use std::os::fd::OwnedFd;
use std::time::Duration;

use memmap2::MmapMut;
use x11rb::connection::Connection;
use x11rb::protocol::damage::{ConnectionExt as _, ReportLevel};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, ImageFormat, PropMode, Window,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

use super::x11::{wait_readable, Display};
use crate::clock::now_ns;
use crate::platform::common::{oldest_damage_ns, ServerClock};
use crate::platform::{CaptureSource, CaptureStatus, RawFrame, WindowHandle};
use crate::raster::{FrameView, PixelFormat, Rect};
use crate::records::SourceId;

/// Rows re-read above and below a damaged rectangle.
const MARGIN_ROWS: i32 = 64;
/// How often the server clock mapping is renewed while capturing.
const CLOCK_SYNC_EVERY_NS: u64 = 2_000_000_000;
/// Round trips tried when the source opens.
const CLOCK_SYNC_TRIES: u32 = 5;

struct Shm {
    seg: u32,
    map: MmapMut,
}

/// A round trip whose PropertyNotify has not been read yet.
#[derive(Clone, Copy, Debug)]
struct SyncBracket {
    /// The low 16 bits of the ChangeProperty's sequence number, which the
    /// PropertyNotify it caused carries.
    sequence: u16,
    sent_ns: u64,
    answered_ns: u64,
}

pub struct X11Source {
    d: Display,
    target: Window,
    damage: u32,
    format: PixelFormat,
    shm: Option<Shm>,
    shm_note: Option<String>,
    /// Damaged rectangles not yet read, with each event's server timestamp.
    pending: Vec<(Rect, u32)>,
    initial: bool,
    /// DAMAGE timestamps on the probe clock.
    clock: ServerClock,
    /// An unmapped input-only window of ours and the property whose
    /// zero-length appends make the server stamp a PropertyNotify; `None` if
    /// it could not be created (every frame's lag is then unknown).
    clock_window: Option<(Window, u32)>,
    sync: Option<SyncBracket>,
    next_sync_ns: u64,
}

/// An unmapped input-only window (never shown, never focused) whose property
/// changes the server stamps with its current time.
fn clock_window(d: &Display) -> Result<(Window, u32), String> {
    let atom = d
        .conn
        .intern_atom(false, b"_TERMHOST_PROBE_CLOCK")
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("InternAtom: {e}"))?
        .atom;
    let win = d.conn.generate_id().map_err(|e| e.to_string())?;
    d.conn
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            d.root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("CreateWindow: {e}"))?;
    Ok((win, atom))
}

/// A shared-memory segment of `size` bytes the X server can write into
/// (MIT-SHM 1.2 file-descriptor passing).
fn attach_shm(d: &Display, size: usize) -> Result<Shm, String> {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        std::path::PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let path = dir.join(format!(
        "termhost-probe-{}-{}",
        std::process::id(),
        now_ns()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("creating {}: {e}", path.display()))?;
    // Unlinked at once: the segment lives exactly as long as the descriptors.
    let _ = std::fs::remove_file(&path);
    file.set_len(size as u64)
        .map_err(|e| format!("sizing the segment: {e}"))?;
    // SAFETY: a private, freshly created file mapped shared with the X server,
    // which writes into it only inside our GetImage requests.
    let map = unsafe { MmapMut::map_mut(&file) }.map_err(|e| format!("mmap: {e}"))?;
    let fd = OwnedFd::from(file.try_clone().map_err(|e| format!("dup: {e}"))?);
    let seg = d.conn.generate_id().map_err(|e| e.to_string())?;
    d.conn
        .shm_attach_fd(seg, fd, false)
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| format!("ShmAttachFd: {e}"))?;
    Ok(Shm { seg, map })
}

impl X11Source {
    pub fn open(target: WindowHandle) -> Result<X11Source, String> {
        let d = Display::open()?;
        let w = u32::try_from(target.0).map_err(|_| format!("{target} is not an X window id"))?;
        if !d.exists(w) {
            return Err(format!("{target} is not a window"));
        }
        let format = d.format.ok_or_else(|| {
            format!(
                "the root visual is not 24/32-bit TrueColor with 8-bit channels (depth {}); markers cannot be read",
                d.depth
            )
        })?;
        if !d.has_extension("DAMAGE") {
            return Err("the X server has no DAMAGE extension".into());
        }
        if d.has_extension("XFIXES") {
            let _ = d
                .conn
                .xfixes_query_version(5, 0)
                .map_err(|e| e.to_string())?
                .reply();
        }
        d.conn
            .damage_query_version(1, 1)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("DamageQueryVersion: {e}"))?;
        let damage = d.conn.generate_id().map_err(|e| e.to_string())?;
        // On the ROOT: damage there includes drawing into every window, and
        // the root is what the read comes from.
        d.conn
            .damage_create(damage, d.root, ReportLevel::RAW_RECTANGLES)
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("DamageCreate: {e}"))?;
        let size = usize::from(d.width) * usize::from(d.height) * 4;
        let (shm, shm_note) = if !d.has_extension("MIT-SHM") {
            (None, Some("the X server has no MIT-SHM".to_string()))
        } else {
            match d.conn.shm_query_version().ok().and_then(|c| c.reply().ok()) {
                Some(v) if (v.major_version, v.minor_version) >= (1, 2) => {
                    match attach_shm(&d, size) {
                        Ok(s) => (Some(s), None),
                        Err(e) => (None, Some(format!("MIT-SHM attach failed: {e}"))),
                    }
                }
                Some(v) => (
                    None,
                    Some(format!(
                        "MIT-SHM {}.{} lacks fd passing (1.2)",
                        v.major_version, v.minor_version
                    )),
                ),
                None => (None, Some("MIT-SHM did not answer".into())),
            }
        };
        let clock_window = clock_window(&d).ok();
        let mut src = X11Source {
            d,
            target: w,
            damage,
            format,
            shm,
            shm_note,
            pending: Vec::new(),
            initial: true,
            clock: ServerClock::default(),
            clock_window,
            sync: None,
            next_sync_ns: 0,
        };
        for _ in 0..CLOCK_SYNC_TRIES {
            src.sync_clock();
            src.drain()?;
            if src.clock.is_established() {
                break;
            }
        }
        // Damage from before the first frame is moot: that frame reads the
        // whole window, and its queueing time is not the probe's lag.
        src.pending.clear();
        src.next_sync_ns = now_ns() + CLOCK_SYNC_EVERY_NS;
        Ok(src)
    }

    /// Why reads go through plain GetImage, if they do.
    pub fn shm_note(&self) -> Option<&str> {
        self.shm_note.as_deref()
    }

    /// Whether DAMAGE timestamps can be placed on the probe clock (frames
    /// carry a presentation time, so the probe's lag is measured).
    pub fn clock_established(&self) -> bool {
        self.clock.is_established()
    }

    /// Start one round trip that brackets a server timestamp: a zero-length
    /// append to the clock window's property, checked — so when this returns
    /// the server has processed it and the PropertyNotify it caused is queued
    /// on the connection. [`Self::drain`] reads that event and feeds the
    /// bracket to the clock.
    fn sync_clock(&mut self) {
        let Some((win, atom)) = self.clock_window else {
            return;
        };
        let sent_ns = now_ns();
        let Ok(cookie) =
            self.d
                .conn
                .change_property8(PropMode::APPEND, win, atom, AtomEnum::STRING, &[])
        else {
            return;
        };
        let sequence = cookie.sequence_number() as u16;
        if cookie.check().is_ok() {
            self.sync = Some(SyncBracket {
                sequence,
                sent_ns,
                answered_ns: now_ns(),
            });
        }
    }

    fn drain(&mut self) -> Result<(), String> {
        loop {
            match self.d.conn.poll_for_event() {
                Ok(Some(Event::DamageNotify(ev))) if ev.damage == self.damage => {
                    self.pending.push((
                        Rect {
                            x: i32::from(ev.area.x),
                            y: i32::from(ev.area.y),
                            w: u32::from(ev.area.width),
                            h: u32::from(ev.area.height),
                        },
                        ev.timestamp,
                    ));
                }
                Ok(Some(Event::PropertyNotify(ev)))
                    if self.clock_window == Some((ev.window, ev.atom)) =>
                {
                    // Only the event of the round trip in flight: its
                    // bracket is the one that contains the server's read.
                    if let Some(b) = self.sync.filter(|b| b.sequence == ev.sequence) {
                        self.sync = None;
                        self.clock.observe(b.sent_ns, b.answered_ns, ev.time);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(e) => return Err(format!("the X connection failed: {e}")),
            }
        }
    }

    /// Read `region` (root coordinates, on screen) and hand it to `f`.
    fn read(
        &mut self,
        region: Rect,
        win: Rect,
        t_ns: u64,
        presented_ns: Option<u64>,
        f: &mut dyn FnMut(RawFrame<'_>),
    ) -> Result<(), String> {
        let (w, h) = (region.w as usize, region.h as usize);
        let origin = (region.x - win.x, region.y - win.y);
        let window_size = (win.w, win.h);
        match &self.shm {
            Some(shm) => {
                self.d
                    .conn
                    .shm_get_image(
                        self.d.root,
                        region.x as i16,
                        region.y as i16,
                        region.w as u16,
                        region.h as u16,
                        !0,
                        ImageFormat::Z_PIXMAP.into(),
                        shm.seg,
                        0,
                    )
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| format!("ShmGetImage: {e}"))?;
                let bytes = &shm.map[..w * h * 4];
                let view =
                    FrameView::new(bytes, w, h, w * 4, self.format).map_err(|e| e.to_string())?;
                f(RawFrame {
                    t_ns,
                    presented_ns,
                    view,
                    origin,
                    // X11 damage cannot say how many frames were skipped.
                    missed: None,
                    window_size,
                });
            }
            None => {
                let reply = self
                    .d
                    .conn
                    .get_image(
                        ImageFormat::Z_PIXMAP,
                        self.d.root,
                        region.x as i16,
                        region.y as i16,
                        region.w as u16,
                        region.h as u16,
                        !0,
                    )
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| format!("GetImage: {e}"))?;
                let view = FrameView::new(&reply.data, w, h, w * 4, self.format)
                    .map_err(|e| e.to_string())?;
                f(RawFrame {
                    t_ns,
                    presented_ns,
                    view,
                    origin,
                    // X11 damage cannot say how many frames were skipped.
                    missed: None,
                    window_size,
                });
            }
        }
        Ok(())
    }
}

impl CaptureSource for X11Source {
    fn id(&self) -> SourceId {
        SourceId::X11
    }

    fn describe(&self) -> String {
        format!(
            "X11 DAMAGE + {} of window 0x{:x} on {}; {}",
            if self.shm.is_some() {
                "MIT-SHM GetImage"
            } else {
                "GetImage (no shared memory)"
            },
            self.target,
            self.d.name,
            if self.clock.is_established() {
                "damage timestamps on the probe clock (frame lag measured)"
            } else {
                "the server clock could not be established (frame lag unknown)"
            }
        )
    }

    fn next_frame(&mut self, timeout: Duration, f: &mut dyn FnMut(RawFrame<'_>)) -> CaptureStatus {
        let Some(win) = self.d.rect(self.target) else {
            return CaptureStatus::Ended("the window is gone".into());
        };
        let Some(visible) = win.intersect(&self.d.screen()) else {
            std::thread::sleep(timeout);
            return CaptureStatus::Timeout;
        };
        let t_ns;
        let presented_ns;
        let region = if self.initial {
            // The window as it is now: the first frame. When its content was
            // drawn is not known, so neither is its lag.
            self.initial = false;
            t_ns = now_ns();
            presented_ns = None;
            Some(visible)
        } else {
            if now_ns() >= self.next_sync_ns {
                self.sync_clock();
                self.next_sync_ns = now_ns() + CLOCK_SYNC_EVERY_NS;
            }
            if let Err(e) = self.drain() {
                return CaptureStatus::Ended(e);
            }
            if self.pending.is_empty() {
                wait_readable(&self.d.conn, timeout);
                if let Err(e) = self.drain() {
                    return CaptureStatus::Ended(e);
                }
            }
            t_ns = now_ns();
            let mut stamps = Vec::new();
            let touched: Vec<Rect> = self
                .pending
                .drain(..)
                .filter_map(|(r, ms)| {
                    let on_screen = r.intersect(&visible)?;
                    stamps.push(ms);
                    Some(on_screen)
                })
                .collect();
            presented_ns = oldest_damage_ns(&self.clock, &stamps);
            touched
                .iter()
                .map(|r| (r.y, r.bottom()))
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
                .and_then(|(top, bottom)| {
                    let band = Rect {
                        x: visible.x,
                        y: top - MARGIN_ROWS,
                        w: visible.w,
                        h: (bottom - top + 2 * MARGIN_ROWS) as u32,
                    };
                    band.intersect(&visible)
                })
        };
        let Some(region) = region else {
            return CaptureStatus::Timeout;
        };
        match self.read(region, win, t_ns, presented_ns, f) {
            Ok(()) => CaptureStatus::Frame,
            Err(e) => CaptureStatus::Lost(e),
        }
    }
}

impl Drop for X11Source {
    fn drop(&mut self) {
        let _ = self.d.conn.damage_destroy(self.damage);
        if let Some(shm) = &self.shm {
            let _ = self.d.conn.shm_detach(shm.seg);
        }
        if let Some((win, _)) = self.clock_window {
            let _ = self.d.conn.destroy_window(win);
        }
        let _ = self.d.conn.flush();
    }
}
