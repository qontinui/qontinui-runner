//! The engine: one thread per capture source (each scans its frames for
//! markers, so WGC and DDA decode in parallel), and a pump that feeds frames,
//! window-state samples, real-input events, emitted markers and process samples
//! into the [`Model`] and writes the records it returns. `watch` runs it until
//! told to stop; the selftest pumps it phase by phase.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::clock::now_ns;
use crate::feed::{EmitFeed, FeedEvent};
use crate::model::{FrameObs, Model, ModelConfig};
use crate::platform::{CaptureSource, CaptureStatus, RawFrame, Subscription, WindowInspector};
use crate::raster::{self, Rect, ScanOptions};
use crate::records::{IncidentRecord, MarkerObs, Record, Sink, SourceId, WatchSummary};
use crate::sampler::Sampler;

/// What an engine runs on.
pub struct EngineSetup {
    pub model: ModelConfig,
    pub scan: ScanOptions,
    pub sources: Vec<Box<dyn CaptureSource>>,
    pub inspector: Option<Box<dyn WindowInspector>>,
    pub input: Option<Subscription>,
    pub sampler: Option<Sampler>,
    pub feeds: Vec<Box<dyn EmitFeed>>,
    pub window_every_ns: u64,
    pub process_every_ns: u64,
}

impl EngineSetup {
    pub fn new(model: ModelConfig) -> Self {
        EngineSetup {
            model,
            scan: ScanOptions::default(),
            sources: Vec::new(),
            inspector: None,
            input: None,
            sampler: None,
            feeds: Vec::new(),
            window_every_ns: 100_000_000,
            process_every_ns: 500_000_000,
        }
    }
}

enum CaptureMsg {
    Frame(FrameObs),
    Status(SourceId, CaptureStatus),
}

fn capture_loop(
    mut src: Box<dyn CaptureSource>,
    tx: Sender<CaptureMsg>,
    stop: Arc<AtomicBool>,
    scan: ScanOptions,
) {
    let id = src.id();
    let mut index = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let status = src.next_frame(Duration::from_millis(50), &mut |raw: RawFrame<'_>| {
            index += 1;
            // The probe's own health: how late the frame reached it after its
            // oldest content was presented — unknown, never 0, when the source
            // cannot say when that was — and how long scanning took (a probe
            // that falls behind inflates latency).
            let received = now_ns();
            let result = raster::scan(&raw.view, &scan);
            let scan_ns = now_ns().saturating_sub(received);
            let markers = result
                .markers
                .iter()
                .map(|f| MarkerObs {
                    kind: f.marker.kind,
                    session: f.marker.session,
                    seq: f.marker.seq,
                    rect: f.rect.offset(raw.origin.0, raw.origin.1),
                })
                .collect();
            let _ = tx.send(CaptureMsg::Frame(FrameObs {
                source: id,
                index,
                t_ns: raw.t_ns,
                width: raw.window_size.0,
                height: raw.window_size.1,
                region: Rect {
                    x: raw.origin.0,
                    y: raw.origin.1,
                    w: raw.view.width() as u32,
                    h: raw.view.height() as u32,
                },
                markers,
                rejected: result.rejected.len() as u32,
                missed: raw.missed,
                scan_ns,
                lag_ns: raw.presented_ns.map(|p| received.saturating_sub(p)),
            }));
        });
        match status {
            CaptureStatus::Frame | CaptureStatus::Timeout => {}
            CaptureStatus::Lost(_) => {
                let _ = tx.send(CaptureMsg::Status(id, status));
            }
            CaptureStatus::Ended(_) => {
                let _ = tx.send(CaptureMsg::Status(id, status));
                return;
            }
        }
    }
}

/// A running engine.
pub struct Engine {
    model: Model,
    rx: Receiver<CaptureMsg>,
    // Held so the channel never disconnects, even with no capture sources.
    _tx: Sender<CaptureMsg>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    inspector: Option<Box<dyn WindowInspector>>,
    input: Option<Subscription>,
    sampler: Option<Sampler>,
    feeds: Vec<Box<dyn EmitFeed>>,
    sink: Sink,
    window_every: u64,
    process_every: u64,
    next_window: u64,
    next_process: u64,
    next_feed: u64,
    monitor_healthy: Option<bool>,
    ended: BTreeMap<SourceId, String>,
}

impl Engine {
    pub fn start(setup: EngineSetup, sink: Sink, t_start: u64) -> Engine {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let threads = setup
            .sources
            .into_iter()
            .map(|src| {
                let (tx, stop, scan) = (tx.clone(), stop.clone(), setup.scan);
                std::thread::Builder::new()
                    .name(format!("capture-{}", src.id().as_str()))
                    .spawn(move || capture_loop(src, tx, stop, scan))
                    .expect("spawn a capture thread")
            })
            .collect();
        Engine {
            model: Model::new(setup.model, t_start),
            rx,
            _tx: tx,
            stop,
            threads,
            inspector: setup.inspector,
            input: setup.input,
            sampler: setup.sampler,
            feeds: setup.feeds,
            sink,
            window_every: setup.window_every_ns.max(1),
            process_every: setup.process_every_ns.max(1),
            next_window: t_start,
            next_process: t_start,
            next_feed: t_start,
            monitor_healthy: None,
            ended: BTreeMap::new(),
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn emit(&mut self, r: Record) {
        self.sink.emit(&r);
    }

    /// Sources that ended for good, with why.
    pub fn ended_sources(&self) -> &BTreeMap<SourceId, String> {
        &self.ended
    }

    fn handle(&mut self, msg: CaptureMsg) {
        match msg {
            CaptureMsg::Frame(obs) => {
                for r in self.model.on_frame(obs) {
                    self.sink.emit(&r);
                }
            }
            CaptureMsg::Status(source, CaptureStatus::Lost(detail)) => {
                self.sink.emit(&Record::Incident(
                    IncidentRecord::new(
                        "capture_lost",
                        format!(
                            "{} capture lost (re-establishing): {detail}",
                            source.as_str()
                        ),
                        false,
                    )
                    .about_source(source),
                ))
            }
            CaptureMsg::Status(source, CaptureStatus::Ended(detail)) => {
                self.sink.emit(&Record::Incident(
                    IncidentRecord::new(
                        "capture_lost",
                        format!("{} capture ended: {detail}", source.as_str()),
                        source == self.model.config().primary,
                    )
                    .about_source(source),
                ));
                self.ended.insert(source, detail);
            }
            CaptureMsg::Status(_, _) => {}
        }
    }

    fn periodic(&mut self, now: u64) {
        if now >= self.next_window {
            if let Some(insp) = self.inspector.as_mut() {
                let s = insp.sample(now);
                self.model.on_window_sample(&s);
            }
            self.next_window =
                (self.next_window + self.window_every).max(now + self.window_every / 2);
        }
        if let Some(sub) = self.input.as_mut() {
            let events = sub.drain();
            let healthy = sub.health();
            for e in events {
                for r in self.model.on_input(&e) {
                    self.sink.emit(&r);
                }
            }
            let ok = healthy.is_ok();
            if self.monitor_healthy != Some(ok) {
                self.model.on_monitor_health(now, ok);
                if let Err(why) = healthy {
                    self.sink.emit(&Record::Incident(IncidentRecord::new(
                        "harness_error",
                        format!("the real-input listener failed; intervals cannot be shown free of operator input: {why}"),
                        false,
                    )));
                }
                self.monitor_healthy = Some(ok);
            }
        }
        if now >= self.next_feed {
            for f in self.feeds.iter_mut() {
                for e in f.poll() {
                    match e {
                        FeedEvent::Emitted(e) => self.model.on_emitted(e),
                        FeedEvent::Started(s) => self.model.on_gen_start(s),
                        FeedEvent::Exited { session, t_ns } => {
                            self.model.on_gen_exit(session, t_ns)
                        }
                    }
                }
            }
            self.next_feed = now + 50_000_000;
        }
        if now >= self.next_process {
            if let Some(s) = self.sampler.as_mut() {
                let rec = s.sample(now);
                self.model.on_process_sample(&rec);
                for e in rec
                    .exited
                    .iter()
                    .filter(|e| e.group == crate::sampler::GROUP_HOST)
                {
                    self.sink.emit(&Record::Incident(IncidentRecord::new(
                        "host_crashed",
                        format!("host process {} ({}) exited during the run", e.pid, e.name),
                        false,
                    )));
                }
                self.sink.emit(&Record::Sample(rec));
            }
            self.next_process =
                (self.next_process + self.process_every).max(now + self.process_every / 2);
        }
        for r in self.model.tick(now) {
            self.sink.emit(&r);
        }
    }

    /// Pump until `deadline_ns`.
    pub fn pump_until(&mut self, deadline_ns: u64) {
        self.pump_while(deadline_ns, |_| true);
    }

    /// Pump until `deadline_ns` or until `keep` turns false; returns true when
    /// `keep` ended it.
    pub fn pump_while(&mut self, deadline_ns: u64, mut keep: impl FnMut(&Model) -> bool) -> bool {
        loop {
            let now = now_ns();
            if now >= deadline_ns {
                return false;
            }
            if !keep(&self.model) {
                return true;
            }
            let wait = (deadline_ns - now).clamp(1_000_000, 5_000_000);
            match self.rx.recv_timeout(Duration::from_nanos(wait)) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
            }
            while let Ok(msg) = self.rx.try_recv() {
                self.handle(msg);
            }
            self.periodic(now_ns());
        }
    }

    /// Stop the capture threads, take the last frames, and build the summary
    /// (written to the sink). Returns the summary, the final model and the
    /// sink (for the caller's closing record).
    pub fn finish(mut self, t_end: u64) -> (WatchSummary, Model, Sink) {
        self.stop.store(true, Ordering::Relaxed);
        for t in std::mem::take(&mut self.threads) {
            let _ = t.join();
        }
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.periodic(t_end);
        let (records, summary) = self.model.finish(t_end);
        for r in records {
            self.sink.emit(&r);
        }
        self.sink.emit(&Record::Summary(Box::new(summary.clone())));
        self.sink.flush();
        let model = std::mem::replace(
            &mut self.model,
            Model::new(ModelConfig::new(SourceId::Synthetic, None, 1), 0),
        );
        let sink = std::mem::replace(&mut self.sink, Sink::discard());
        (summary, model, sink)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in std::mem::take(&mut self.threads) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::synth::Canvas;

    const MS: u64 = 1_000_000;

    /// Frames stamped when the probe drained them — like X11's — whose
    /// content was presented `presented_behind_ms` earlier, or at a time the
    /// source cannot tell.
    struct Backlogged {
        presented_behind_ms: Option<u64>,
        canvas: Canvas,
    }

    impl CaptureSource for Backlogged {
        fn id(&self) -> SourceId {
            SourceId::Synthetic
        }
        fn describe(&self) -> String {
            "backlogged".into()
        }
        fn next_frame(
            &mut self,
            _timeout: Duration,
            f: &mut dyn FnMut(RawFrame<'_>),
        ) -> CaptureStatus {
            std::thread::sleep(Duration::from_millis(4));
            let t_ns = now_ns();
            f(RawFrame {
                t_ns,
                presented_ns: self
                    .presented_behind_ms
                    .map(|b| t_ns.saturating_sub(b * MS)),
                view: self.canvas.view(),
                origin: (0, 0),
                missed: None,
                window_size: (8, 8),
            });
            CaptureStatus::Frame
        }
    }

    fn run(presented_behind_ms: Option<u64>) -> (WatchSummary, Vec<String>) {
        let mut setup = EngineSetup::new(ModelConfig::new(SourceId::Synthetic, None, 16_666_667));
        setup.sources = vec![Box::new(Backlogged {
            presented_behind_ms,
            canvas: Canvas::new(8, 8, [0, 0, 0]),
        })];
        let (sink, lines) = Sink::memory();
        let t0 = now_ns();
        let mut engine = Engine::start(setup, sink, t0);
        engine.pump_until(t0 + 300 * MS);
        let (summary, _, _) = engine.finish(now_ns());
        let lines = lines.lock().expect("lines").clone();
        (summary, lines)
    }

    #[test]
    fn a_frames_lag_runs_from_its_presentation_not_from_when_it_was_drained() {
        // Damage that waited 120 ms in the socket: the frame is stamped when
        // drained, but the probe was 120 ms behind the screen.
        let (s, _) = run(Some(120));
        assert_eq!(s.probe.behind, Some(true), "{:?}", s.probe);
        assert!(
            s.probe.lag_ms_max.is_some_and(|l| l >= 120.0),
            "{:?}",
            s.probe
        );
        assert_eq!(s.output_latency_ms.reason, Some("harness_error"));
        // A source that cannot say when its frames were presented: lag
        // unknown, never 0.
        let (s, lines) = run(None);
        assert_eq!(s.probe.behind, None, "{:?}", s.probe);
        assert!(s.probe.frames > 0 && s.probe.frames_lag_unknown == s.probe.frames);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("\"type\":\"frame\"") && l.contains("\"lag_ms\":null")),
            "a frame record must say its lag is unknown"
        );
    }
}
