//! The probe's own X window: the selftest's measured window and the doctor's
//! capture probe.
//!
//! Two connections, each used by one thread: the window thread owns the
//! window, repaints it on Expose and answers every KeyPress with an echo
//! marker; the painter connection paints scheduled markers from the caller's
//! thread, covers the window with an override-redirect occluder, and moves the
//! input focus. A paint returns only after a round trip, so its time is when
//! the X server had drawn it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeGCAux, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux,
    EventMask, InputFocus, PropMode, Rectangle, StackMode, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

use super::x11::{wait_readable, Display};
use crate::clock::now_ns;
use crate::codec::{encode_values, marker_seq, Marker, MarkerKind, CELLS, PALETTE};
use crate::platform::{
    EchoPainted, PaintSlot, TestGeometry, TestWindow, TestWindowOptions, WindowHandle,
};

const PAD: u32 = 8;
/// A mid-grey: impure under every threshold, so the occluder can never read
/// as a marker.
const OCCLUDER_RGB: [u8; 3] = [120, 120, 128];

struct Shared {
    geometry: TestGeometry,
    echo_session: u16,
    state: Mutex<[Option<Marker>; 2]>,
    echoes: Mutex<Vec<EchoPainted>>,
}

/// Paint one slot with `d`'s connection and `gc`.
fn paint_slot(
    d: &Display,
    win: Window,
    gc: u32,
    geometry: &TestGeometry,
    slot: PaintSlot,
    marker: Option<Marker>,
) -> Result<(), String> {
    let (x, y) = match slot {
        PaintSlot::Output => geometry.output_at,
        PaintSlot::Echo => geometry.echo_at,
    };
    let values = marker.map(encode_values).unwrap_or([0; CELLS]);
    for (value, colour) in PALETTE.iter().enumerate() {
        let rects: Vec<Rectangle> = values
            .iter()
            .enumerate()
            .filter(|(_, &v)| usize::from(v) == value)
            .map(|(i, _)| Rectangle {
                x: (x + i as u32 * geometry.cell_w) as i16,
                y: y as i16,
                width: geometry.cell_w as u16,
                height: geometry.cell_h as u16,
            })
            .collect();
        if rects.is_empty() {
            continue;
        }
        d.conn
            .change_gc(gc, &ChangeGCAux::new().foreground(d.pixel(colour.rgb)))
            .map_err(|e| e.to_string())?;
        d.conn
            .poly_fill_rectangle(win, gc, &rects)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn window_thread(d: Display, win: Window, gc: u32, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        loop {
            let ev = match d.conn.poll_for_event() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(_) => return,
            };
            match ev {
                Event::Expose(e) if e.window == win && e.count == 0 => {
                    let st = shared.state.lock().map(|g| *g).unwrap_or([None, None]);
                    let _ = paint_slot(&d, win, gc, &shared.geometry, PaintSlot::Output, st[0]);
                    let _ = paint_slot(&d, win, gc, &shared.geometry, PaintSlot::Echo, st[1]);
                    let _ = d.conn.flush();
                }
                Event::KeyPress(e) if e.event == win => {
                    let seq = shared.echoes.lock().map(|v| v.len()).unwrap_or(0) as u32;
                    let m = Marker::new(
                        MarkerKind::Echo,
                        shared.echo_session,
                        marker_seq(u64::from(seq)),
                    );
                    if let Ok(mut st) = shared.state.lock() {
                        st[1] = Some(m);
                    }
                    if paint_slot(&d, win, gc, &shared.geometry, PaintSlot::Echo, Some(m)).is_ok()
                        && d.sync().is_ok()
                    {
                        let t = now_ns();
                        if let Ok(mut v) = shared.echoes.lock() {
                            v.push(EchoPainted {
                                seq,
                                // Which key does not matter for the round trip.
                                ch: '?',
                                t_painted_ns: t,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        wait_readable(&d.conn, Duration::from_millis(50));
    }
    let _ = d.conn.destroy_window(win);
    let _ = d.conn.flush();
}

pub struct X11TestWindow {
    shared: Arc<Shared>,
    win: Window,
    occluder: Window,
    painter: Mutex<(Display, u32)>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl X11TestWindow {
    pub fn create(opts: &TestWindowOptions) -> Result<X11TestWindow, String> {
        let d = Display::open()?;
        if d.format.is_none() {
            return Err(format!(
                "the root visual (depth {}) is not 24/32-bit TrueColor",
                d.depth
            ));
        }
        let geometry = TestGeometry {
            cell_w: opts.cell_w,
            cell_h: opts.cell_h,
            output_at: (PAD, PAD),
            echo_at: (PAD, PAD * 2 + opts.cell_h),
        };
        let w = opts.client_w.max(PAD * 2 + opts.cell_w * CELLS as u32);
        let h = opts.client_h.max(PAD * 3 + opts.cell_h * 2);
        let win = d.conn.generate_id().map_err(|e| e.to_string())?;
        let black = d.pixel([0, 0, 0]);
        d.conn
            .create_window(
                d.depth,
                win,
                d.root,
                opts.x as i16,
                opts.y as i16,
                w as u16,
                h as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                d.visual,
                &CreateWindowAux::new()
                    .background_pixel(black)
                    // The doctor's probe bypasses any window manager and never
                    // takes focus.
                    .override_redirect(u32::from(!opts.activate))
                    .event_mask(
                        EventMask::EXPOSURE
                            | EventMask::KEY_PRESS
                            | EventMask::STRUCTURE_NOTIFY
                            | EventMask::FOCUS_CHANGE,
                    ),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| format!("CreateWindow: {e}"))?;
        let _ = d.conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            opts.title.as_bytes(),
        );
        let gc = d.conn.generate_id().map_err(|e| e.to_string())?;
        d.conn
            .create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))
            .map_err(|e| e.to_string())?;
        d.conn.map_window(win).map_err(|e| e.to_string())?;
        if opts.topmost {
            let _ = d
                .conn
                .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        d.sync()?;

        // The painter's own connection and GC on the same window, plus the
        // occluder it maps and unmaps.
        let p = Display::open()?;
        let pgc = p.conn.generate_id().map_err(|e| e.to_string())?;
        p.conn
            .create_gc(pgc, win, &CreateGCAux::new().graphics_exposures(0))
            .map_err(|e| e.to_string())?;
        let occluder = p.conn.generate_id().map_err(|e| e.to_string())?;
        p.conn
            .create_window(
                p.depth,
                occluder,
                p.root,
                opts.x as i16,
                opts.y as i16,
                w as u16 + 20,
                h as u16 + 20,
                0,
                WindowClass::INPUT_OUTPUT,
                p.visual,
                &CreateWindowAux::new()
                    .background_pixel(p.pixel(OCCLUDER_RGB))
                    .override_redirect(1),
            )
            .map_err(|e| e.to_string())?;
        let _ = p.conn.change_property8(
            PropMode::REPLACE,
            occluder,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"termhost-probe occluder",
        );
        p.sync()?;

        let shared = Arc::new(Shared {
            geometry,
            echo_session: opts.echo_session,
            state: Mutex::new([None, None]),
            echoes: Mutex::new(Vec::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2) = (shared.clone(), stop.clone());
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("x11-test-window".into())
            .spawn(move || {
                let _ = tx.send(());
                window_thread(d, win, gc, s2, st2)
            })
            .map_err(|e| format!("spawning the window thread: {e}"))?;
        let _ = rx.recv_timeout(Duration::from_secs(2));
        // Give the server a moment to map and expose the window.
        std::thread::sleep(Duration::from_millis(100));
        Ok(X11TestWindow {
            shared,
            win,
            occluder,
            painter: Mutex::new((p, pgc)),
            stop,
            thread: Mutex::new(Some(thread)),
        })
    }
}

impl TestWindow for X11TestWindow {
    fn handle(&self) -> WindowHandle {
        WindowHandle(u64::from(self.win))
    }

    fn geometry(&self) -> TestGeometry {
        self.shared.geometry
    }

    fn paint(&self, slot: PaintSlot, marker: Option<Marker>) -> Result<u64, String> {
        if let Ok(mut st) = self.shared.state.lock() {
            st[match slot {
                PaintSlot::Output => 0,
                PaintSlot::Echo => 1,
            }] = marker;
        }
        let guard = self.painter.lock().map_err(|_| "painter poisoned")?;
        let (p, gc) = (&guard.0, guard.1);
        paint_slot(p, self.win, gc, &self.shared.geometry, slot, marker)?;
        p.sync()?;
        Ok(now_ns())
    }

    fn set_occluded(&self, on: bool) -> Result<(), String> {
        let guard = self.painter.lock().map_err(|_| "painter poisoned")?;
        let p = &guard.0;
        if on {
            let r = p
                .rect(self.win)
                .ok_or("the selftest window has no geometry")?;
            p.conn
                .configure_window(
                    self.occluder,
                    &ConfigureWindowAux::new()
                        .x(r.x - 10)
                        .y(r.y - 10)
                        .width(r.w + 20)
                        .height(r.h + 20)
                        .stack_mode(StackMode::ABOVE),
                )
                .map_err(|e| e.to_string())?;
            p.conn
                .map_window(self.occluder)
                .map_err(|e| e.to_string())?;
            let _ = p.conn.configure_window(
                self.occluder,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
        } else {
            p.conn
                .unmap_window(self.occluder)
                .map_err(|e| e.to_string())?;
        }
        p.sync()?;
        std::thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    fn activate(&self) -> Result<bool, String> {
        let guard = self.painter.lock().map_err(|_| "painter poisoned")?;
        let p = &guard.0;
        let _ = p.conn.configure_window(
            self.win,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );
        p.conn
            .set_input_focus(InputFocus::PARENT, self.win, x11rb::CURRENT_TIME)
            .map_err(|e| e.to_string())?;
        p.sync()?;
        Ok(p.is_foreground(self.win))
    }

    fn freeze(&self, _d: Duration) -> Result<(), String> {
        Err("X11 has no OS 'not responding' state to provoke".into())
    }

    fn echoes(&self) -> Vec<EchoPainted> {
        self.shared
            .echoes
            .lock()
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut t) = self.thread.lock() {
            if let Some(h) = t.take() {
                let _ = h.join();
            }
        }
        if let Ok(guard) = self.painter.lock() {
            let _ = guard.0.conn.destroy_window(self.occluder);
            let _ = guard.0.conn.flush();
        }
    }
}

impl Drop for X11TestWindow {
    fn drop(&mut self) {
        self.close();
    }
}
