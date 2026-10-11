//! X11 basics: the connection, the root window's pixel format, waiting on the
//! socket, window geometry, target lookup, the refresh rate and window state.
//!
//! Every component opens its OWN connection and uses it from one thread, so
//! no event is ever buffered behind another thread's reply (x11rb's
//! event-loop guidance).

use std::os::fd::AsRawFd;
use std::time::Duration;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageOrder, MapState, Window};
use x11rb::rust_connection::RustConnection;

use crate::model::WindowSample;
use crate::platform::common::{occlusion, pick_by_title, x11_focus_verdict};
use crate::platform::{Refresh, TargetSpec, WindowHandle, WindowInspector};
use crate::raster::{PixelFormat, Rect};
use crate::records::OccluderInfo;

/// One connection and the facts about its default screen.
pub struct Display {
    pub conn: RustConnection,
    pub root: Window,
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub visual: u32,
    /// The root's pixel format, when it is one the decoder reads (24/32-bit
    /// TrueColor with 8-bit channels).
    pub format: Option<PixelFormat>,
    pub name: String,
}

impl Display {
    pub fn open() -> Result<Display, String> {
        let name = std::env::var("DISPLAY")
            .map_err(|_| "DISPLAY is not set: no X display to capture".to_string())?;
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| format!("connecting to X display {name}: {e}"))?;
        let setup = conn.setup();
        let s = setup
            .roots
            .get(screen)
            .ok_or("the X display has no default screen")?;
        let format = root_format(setup, s);
        Ok(Display {
            root: s.root,
            width: s.width_in_pixels,
            height: s.height_in_pixels,
            depth: s.root_depth,
            visual: s.root_visual,
            format,
            name,
            conn,
        })
    }

    pub fn describe(&self) -> String {
        let setup = self.conn.setup();
        format!(
            "X display {} ({} release {}), screen {}x{} depth {}",
            self.name,
            String::from_utf8_lossy(&setup.vendor),
            setup.release_number,
            self.width,
            self.height,
            self.depth
        )
    }

    /// Whether the server has an extension.
    pub fn has_extension(&self, name: &'static str) -> bool {
        self.conn
            .extension_information(name)
            .ok()
            .flatten()
            .is_some()
    }

    /// A round trip: every earlier request has been processed when it returns.
    pub fn sync(&self) -> Result<(), String> {
        self.conn
            .get_input_focus()
            .map_err(|e| e.to_string())?
            .reply()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The pixel value of an RGB colour in the root visual.
    pub fn pixel(&self, rgb: [u8; 3]) -> u32 {
        (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
    }

    /// The window's rectangle in root coordinates.
    pub fn rect(&self, w: Window) -> Option<Rect> {
        let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
        let t = self
            .conn
            .translate_coordinates(w, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some(Rect {
            x: i32::from(t.dst_x),
            y: i32::from(t.dst_y),
            w: u32::from(g.width),
            h: u32::from(g.height),
        })
    }

    /// The screen rectangle.
    pub fn screen(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: u32::from(self.width),
            h: u32::from(self.height),
        }
    }

    pub fn exists(&self, w: Window) -> bool {
        self.conn
            .get_window_attributes(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some()
    }

    /// `w`'s ancestor that is a direct child of the root (its WM frame, or
    /// itself on a bare server).
    pub fn top_level(&self, w: Window) -> Window {
        let mut cur = w;
        for _ in 0..64 {
            let Some(tree) = self.conn.query_tree(cur).ok().and_then(|c| c.reply().ok()) else {
                return cur;
            };
            if tree.parent == self.root || tree.parent == x11rb::NONE {
                return cur;
            }
            cur = tree.parent;
        }
        cur
    }

    fn atom(&self, name: &str) -> Option<u32> {
        self.conn
            .intern_atom(true, name.as_bytes())
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|&a| a != x11rb::NONE)
    }

    fn property32(&self, w: Window, atom: u32) -> Option<Vec<u32>> {
        let r = self
            .conn
            .get_property(false, w, atom, AtomEnum::ANY, 0, 64)
            .ok()?
            .reply()
            .ok()?;
        r.value32().map(|v| v.collect())
    }

    fn text_property(&self, w: Window, atom: u32) -> Option<String> {
        let r = self
            .conn
            .get_property(false, w, atom, AtomEnum::ANY, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        (!r.value.is_empty()).then(|| String::from_utf8_lossy(&r.value).into_owned())
    }

    fn class_name(&self, w: Window) -> String {
        self.text_property(w, AtomEnum::WM_CLASS.into())
            .map(|c| {
                c.split('\0')
                    .rfind(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string()
            })
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| format!("window 0x{w:x}"))
    }

    /// `w` and its ancestors below the root.
    fn ancestry(&self, w: Window) -> Vec<Window> {
        let mut chain = vec![w];
        let mut cur = w;
        for _ in 0..64 {
            let Some(tree) = self.conn.query_tree(cur).ok().and_then(|c| c.reply().ok()) else {
                break;
            };
            if tree.parent == self.root || tree.parent == x11rb::NONE {
                break;
            }
            chain.push(tree.parent);
            cur = tree.parent;
        }
        chain
    }

    /// Whether keys sent now would reach `target`: the X keyboard focus is
    /// the target or inside it (XTEST delivers to the focus), and a window
    /// manager's `_NET_ACTIVE_WINDOW`, when set, agrees — the hint alone goes
    /// stale when focus moves to a popup or a client sets it itself.
    pub fn is_foreground(&self, target: Window) -> bool {
        let Some(focus) = self
            .conn
            .get_input_focus()
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.focus)
        else {
            return false;
        };
        let chain = if focus > 1 {
            self.ancestry(focus)
        } else {
            vec![focus]
        };
        let active_top = self
            .atom("_NET_ACTIVE_WINDOW")
            .and_then(|a| self.property32(self.root, a))
            .and_then(|v| v.first().copied())
            .filter(|&a| a != x11rb::NONE)
            .map(|a| self.top_level(a));
        x11_focus_verdict(&chain, target, active_top, self.top_level(target))
    }

    /// Visible mapped windows above `target`'s top level, with their rects.
    fn windows_above(&self, target: Window) -> Option<Vec<(Window, Rect)>> {
        let top = self.top_level(target);
        let tree = self.conn.query_tree(self.root).ok()?.reply().ok()?;
        let at = tree.children.iter().position(|&c| c == top)?;
        let mut out = Vec::new();
        for &w in &tree.children[at + 1..] {
            let Some(attrs) = self
                .conn
                .get_window_attributes(w)
                .ok()
                .and_then(|c| c.reply().ok())
            else {
                continue;
            };
            if attrs.map_state != MapState::VIEWABLE
                || attrs.class == x11rb::protocol::xproto::WindowClass::INPUT_ONLY
            {
                continue;
            }
            if let Some(r) = self.rect(w) {
                out.push((w, r));
            }
        }
        Some(out)
    }
}

fn root_format(
    setup: &x11rb::protocol::xproto::Setup,
    s: &x11rb::protocol::xproto::Screen,
) -> Option<PixelFormat> {
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == s.root_depth)?
        .bits_per_pixel;
    if bpp != 32 {
        return None;
    }
    let visual = s
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == s.root_visual)?;
    if (visual.red_mask, visual.green_mask, visual.blue_mask)
        != (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
    {
        return None;
    }
    Some(if setup.image_byte_order == ImageOrder::LSB_FIRST {
        PixelFormat::Bgra8
    } else {
        PixelFormat::Argb8
    })
}

/// Wait until the connection's socket is readable (or `timeout`).
pub fn wait_readable(conn: &RustConnection, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: conn.stream().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: one pollfd on a socket we hold open.
    unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
}

/// The refresh rate RandR reports, else an assumed 60 Hz, marked as assumed
/// (Xvfb has no vsync at all; KPIs counted in refresh intervals are then
/// unknown).
pub fn refresh() -> Refresh {
    if let Ok(d) = Display::open() {
        if d.has_extension("RANDR") {
            if let Some(r) = d
                .conn
                .randr_get_screen_info(d.root)
                .ok()
                .and_then(|c| c.reply().ok())
            {
                if (1..=1000).contains(&r.rate) {
                    return Refresh::from_hz(f64::from(r.rate), "RandR screen info");
                }
            }
        }
    }
    Refresh::assumed_60("RandR reports no refresh rate")
}

/// Every top-level client window (direct children of the root, and their
/// children one level down — a window manager's frames hold the clients).
fn clients(d: &Display) -> Vec<Window> {
    let mut out = Vec::new();
    if let Some(tree) = d.conn.query_tree(d.root).ok().and_then(|c| c.reply().ok()) {
        for &w in &tree.children {
            out.push(w);
            if let Some(sub) = d.conn.query_tree(w).ok().and_then(|c| c.reply().ok()) {
                out.extend(sub.children);
            }
        }
    }
    out
}

fn viewable_area(d: &Display, w: Window) -> Option<u64> {
    let attrs = d.conn.get_window_attributes(w).ok()?.reply().ok()?;
    (attrs.map_state == MapState::VIEWABLE).then(|| d.rect(w).map_or(0, |r| r.area()))
}

pub fn resolve(spec: &TargetSpec) -> Result<WindowHandle, String> {
    let d = Display::open()?;
    match spec {
        TargetSpec::Handle(v) => {
            let w = u32::try_from(*v).map_err(|_| format!("0x{v:x} is not an X window id"))?;
            if d.exists(w) {
                Ok(WindowHandle(u64::from(w)))
            } else {
                Err(format!("0x{w:x} is not a window"))
            }
        }
        TargetSpec::Pid(pid) => {
            let atom = d
                .atom("_NET_WM_PID")
                .ok_or("no window carries _NET_WM_PID on this display")?;
            clients(&d)
                .into_iter()
                .filter(|&w| d.property32(w, atom).and_then(|v| v.first().copied()) == Some(*pid))
                .filter_map(|w| viewable_area(&d, w).map(|a| (w, a)))
                .max_by_key(|&(_, a)| a)
                .map(|(w, _)| WindowHandle(u64::from(w)))
                .ok_or_else(|| format!("process {pid} has no viewable window with _NET_WM_PID"))
        }
        TargetSpec::Title { text, unique } => {
            if text.trim().is_empty() {
                return Err("an empty title would match every window".into());
            }
            let net_name = d.atom("_NET_WM_NAME");
            let matches: Vec<(Window, u64)> = clients(&d)
                .into_iter()
                .filter(|&w| {
                    let titles = [
                        d.text_property(w, AtomEnum::WM_NAME.into()),
                        net_name.and_then(|a| d.text_property(w, a)),
                    ];
                    titles.iter().flatten().any(|s| s.contains(text.as_str()))
                })
                .filter_map(|w| viewable_area(&d, w).map(|a| (w, a)))
                .collect();
            pick_by_title(&matches, text, *unique).map(|w| WindowHandle(u64::from(w)))
        }
    }
}

/// Samples one X window.
pub struct Inspector {
    d: Display,
    target: Window,
    wm_state: Option<u32>,
}

impl Inspector {
    pub fn new(target: WindowHandle) -> Result<Inspector, String> {
        let d = Display::open()?;
        let w = u32::try_from(target.0).map_err(|_| format!("{target} is not an X window id"))?;
        if !d.exists(w) {
            return Err(format!("{target} is not a window"));
        }
        let wm_state = d.atom("WM_STATE");
        Ok(Inspector {
            d,
            target: w,
            wm_state,
        })
    }
}

impl WindowInspector for Inspector {
    fn sample(&mut self, t_ns: u64) -> WindowSample {
        let Some(attrs) = self
            .d
            .conn
            .get_window_attributes(self.target)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return WindowSample {
                t_ns,
                foreground: Some(false),
                visible: Some(false),
                ..WindowSample::default()
            };
        };
        let viewable = attrs.map_state == MapState::VIEWABLE;
        // ICCCM WM_STATE 3 = Iconic; no property (a bare server) = not iconic.
        let minimized = self
            .wm_state
            .and_then(|a| self.d.property32(self.target, a))
            .and_then(|v| v.first().copied())
            .map_or(Some(false), |s| Some(s == 3));
        let (occluded_fraction, occluders) =
            match (self.d.rect(self.target), self.d.windows_above(self.target)) {
                (Some(target), Some(above)) => {
                    // Only the part of the window on screen can be seen at all.
                    let target = target.intersect(&self.d.screen()).unwrap_or(target);
                    let rects: Vec<Rect> = above.iter().map(|(_, r)| *r).collect();
                    let (fraction, who) = occlusion(&target, &rects);
                    let info = who
                        .into_iter()
                        .map(|i| OccluderInfo {
                            class: self.d.class_name(above[i].0),
                            pid: 0,
                        })
                        .collect();
                    (Some(fraction), info)
                }
                _ => (None, Vec::new()),
            };
        WindowSample {
            t_ns,
            foreground: Some(self.d.is_foreground(self.target)),
            visible: Some(viewable),
            // X11 has no cloaking: positively not cloaked (an unmapped window
            // is `visible: false`).
            cloaked: Some(false),
            minimized,
            occluded_fraction,
            occluders,
            hung: None,
        }
    }

    fn describe(&self) -> String {
        format!(
            "X window 0x{:x} ({}) on {}",
            self.target,
            self.d.class_name(self.target),
            self.d.name
        )
    }
}
