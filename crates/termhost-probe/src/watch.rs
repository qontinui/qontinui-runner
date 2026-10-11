//! `watch`: the outside-in capture loop over one measured window and its
//! host's process tree, writing `<run-dir>/probe.jsonl`.
//!
//! Per frame (WGC primary, DXGI duplication as the cross-check on Windows;
//! X11 damage + shm on Linux) it records the frame time and the decoded
//! markers; every 100 ms it samples the window's state; every 500 ms the
//! process groups; for the whole run the real-input listener feeds interval
//! validity. It ends on `--duration-ms`, on the run's stop file
//! (`<run-dir>/stop`, the same file that stops the generators), on Ctrl+C, on
//! stdin EOF (`--stdin-stop`), or when the window closes — and always writes
//! its summary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::clock::{now_ns, wall_rfc3339};
use crate::engine::{Engine, EngineSetup};
use crate::feed::GenFeed;
use crate::model::{FramesMode, ModelConfig, MonitorPresence};
use crate::platform::{stop_requested, Host, Tagging, TargetSpec};
use crate::records::{
    CaptureRecord, EndRecord, IncidentRecord, Ns, Record, Sink, SourceId, StartRecord,
};
use crate::sampler::{Sampler, SamplerConfig};

/// The stop file the generators also watch.
pub const STOP_FILE: &str = "stop";

#[derive(Clone, Debug)]
pub struct WatchConfig {
    pub run_dir: PathBuf,
    pub target: TargetSpec,
    /// Empty = the platform's sources (WGC + DDA, or X11).
    pub sources: Vec<SourceId>,
    pub host_pids: Vec<u32>,
    pub run_id: Option<String>,
    pub duration: Option<Duration>,
    pub min_valid_intervals: u64,
    pub frames: FramesMode,
    pub expect_visible: Vec<u16>,
    /// A co-running `inject`'s tag, so its keys count as the probe's.
    pub tag: Option<u32>,
    pub echo_stdout: bool,
    pub stdin_stop: bool,
    pub compositor: bool,
    pub args: Vec<String>,
}

pub fn start_record(subcommand: &'static str, args: &[String], tag: Option<u32>) -> Record {
    Record::Start(StartRecord {
        tool: "termhost-probe",
        version: env!("CARGO_PKG_VERSION"),
        subcommand,
        pid: std::process::id(),
        platform: std::env::consts::OS,
        clock: crate::clock::clock_name(),
        t0_ns: Ns(now_ns()),
        at: wall_rfc3339(),
        codec: crate::spec::spec().clone(),
        args: args.to_vec(),
        tag: tag.map(|t| format!("0x{t:08x}")),
    })
}

fn incident(
    kind: &'static str,
    detail: String,
    stops_run: bool,
    source: Option<SourceId>,
) -> Record {
    Record::Incident(IncidentRecord {
        kind,
        at: wall_rfc3339(),
        detail,
        stops_run,
        source,
    })
}

fn stdin_eof_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    std::thread::spawn(move || {
        let mut sink = std::io::sink();
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut sink);
        f.store(true, Ordering::Relaxed);
    });
    flag
}

/// Run `watch`. Returns the process exit code.
pub fn run(host: &mut dyn Host, cfg: &WatchConfig) -> Result<i32> {
    std::fs::create_dir_all(&cfg.run_dir)
        .with_context(|| format!("creating {}", cfg.run_dir.display()))?;
    let path = cfg.run_dir.join("probe.jsonl");
    let mut sink = Sink::to_file(&path, cfg.echo_stdout)
        .with_context(|| format!("opening {}", path.display()))?;
    sink.emit(&start_record("watch", &cfg.args, cfg.tag));

    let target = match host.resolve_target(&cfg.target) {
        Ok(t) => t,
        Err(why) => {
            sink.emit(&incident(
                "harness_error",
                format!("target window not found: {why}"),
                true,
                None,
            ));
            sink.emit(&Record::End(EndRecord {
                t_ns: Ns(now_ns()),
                exit_code: 1,
                reason: format!("target window not found: {why}"),
            }));
            eprintln!("termhost-probe watch: target window not found: {why}");
            return Ok(1);
        }
    };
    let refresh = host.refresh(target);
    let kinds = if cfg.sources.is_empty() {
        host.source_kinds()
    } else {
        cfg.sources.clone()
    };
    let mut sources = Vec::new();
    for &kind in &kinds {
        match host.open_source(kind, target) {
            Ok(s) => {
                sink.emit(&Record::Capture(CaptureRecord {
                    source: kind,
                    state: "started",
                    target: target.to_string(),
                    detail: s.describe(),
                    refresh_hz: refresh.hz,
                    refresh_source: refresh.source.clone(),
                }));
                sources.push(s);
            }
            Err(why) => sink.emit(&Record::Capture(CaptureRecord {
                source: kind,
                state: "failed",
                target: target.to_string(),
                detail: why,
                refresh_hz: refresh.hz,
                refresh_source: refresh.source.clone(),
            })),
        }
    }
    let opened: Vec<SourceId> = sources.iter().map(|s| s.id()).collect();
    let primary = opened
        .first()
        .copied()
        .or(kinds.first().copied())
        .unwrap_or(SourceId::Synthetic);
    let secondary =
        (primary == SourceId::Wgc && opened.contains(&SourceId::Dda)).then_some(SourceId::Dda);
    if opened.is_empty() {
        sink.emit(&incident(
            "capture_lost",
            "no capture source could be opened; capture KPIs will be unknown".into(),
            false,
            None,
        ));
    }

    let mut tagging = Tagging::new();
    if let Some(t) = cfg.tag {
        tagging.tag = t;
    }
    let monitor = host.input_monitor(&tagging);
    if let Err(why) = &monitor {
        sink.emit(&incident(
            "harness_error",
            format!("the real-input listener could not start, so no interval can be shown free of operator input: {why}"),
            false,
            None,
        ));
    }
    let inspector = match host.inspector(target) {
        Ok(i) => Some(i),
        Err(why) => {
            sink.emit(&incident(
                "harness_error",
                format!("no window inspector: {why}"),
                false,
                None,
            ));
            None
        }
    };

    let mut mc = ModelConfig::new(primary, secondary, refresh.ns);
    mc.min_valid_intervals = cfg.min_valid_intervals;
    mc.frames = cfg.frames;
    mc.monitor = if monitor.is_ok() {
        MonitorPresence::Configured
    } else {
        MonitorPresence::Absent
    };
    if !cfg.expect_visible.is_empty() {
        mc.expect_visible = Some(cfg.expect_visible.iter().copied().collect());
    }
    let mut setup = EngineSetup::new(mc);
    setup.sources = sources;
    setup.inspector = inspector;
    setup.input = monitor.as_ref().ok().map(|m| m.bus.subscribe());
    setup.feeds = vec![Box::new(GenFeed::new(&cfg.run_dir))];
    if !cfg.host_pids.is_empty() || cfg.compositor || cfg.run_id.is_some() {
        setup.sampler = Some(Sampler::new(
            SamplerConfig {
                host_pids: cfg.host_pids.clone(),
                run_id: cfg.run_id.clone(),
                compositor: cfg.compositor,
                display: std::env::var("DISPLAY").ok(),
            },
            host.process_reader(),
        ));
    }

    let t_start = now_ns();
    let deadline = cfg.duration.map(|d| t_start + d.as_nanos() as u64);
    let stdin_eof = cfg.stdin_stop.then(stdin_eof_flag);
    let stop_file = cfg.run_dir.join(STOP_FILE);
    let mut engine = Engine::start(setup, sink, t_start);
    let reason = loop {
        engine.pump_until(now_ns() + 100_000_000);
        if deadline.is_some_and(|d| now_ns() >= d) {
            break "duration elapsed".to_string();
        }
        if stop_file_present(&stop_file) {
            break format!("stop file {}", stop_file.display());
        }
        if stop_requested() {
            break "interrupted".to_string();
        }
        if stdin_eof
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            break "stdin closed".to_string();
        }
        if let Some(why) = engine.ended_sources().get(&primary) {
            break format!("the primary capture source ended: {why}");
        }
    };
    let (summary, _model, mut sink) = engine.finish(now_ns());
    sink.emit(&Record::End(EndRecord {
        t_ns: Ns(now_ns()),
        exit_code: 0,
        reason,
    }));
    sink.flush();
    if !cfg.echo_stdout {
        println!(
            "{}",
            crate::records::to_line(&Record::Summary(Box::new(summary)))
        );
    }
    drop(monitor);
    Ok(0)
}

fn stop_file_present(p: &Path) -> bool {
    p.exists()
}
