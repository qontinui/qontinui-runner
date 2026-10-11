//! termhost-probe — see `termhost-probe --help` and the crate docs
//! (`src/lib.rs`).

use std::time::Duration;

use clap::Parser;

use termhost_probe::cli::{Cli, Cmd};
use termhost_probe::platform::{native, Host};
use termhost_probe::records::{to_line, Record, Sink};
use termhost_probe::{doctor, inject, selftest, watch};

fn main() {
    let cli = Cli::parse();
    native::init_process();
    let args: Vec<String> = std::env::args().collect();
    let mut host = native::RealHost::new();
    let code = match cli.cmd {
        Cmd::Doctor(a) => {
            let facts = host.doctor_facts(&a.node);
            let report = doctor::report(&facts);
            if a.json {
                println!("{}", to_line(&Record::Doctor(report)));
            } else {
                print!("{}", doctor::render_human(&report));
            }
            0
        }
        Cmd::Watch(a) => {
            let cfg = watch::WatchConfig {
                run_dir: a.run_dir.clone(),
                target: a.target.spec(),
                sources: a.sources.iter().map(|&s| s.into()).collect(),
                host_pids: a.host_pid.clone(),
                run_id: a.run_id.clone(),
                duration: a.duration_ms.map(Duration::from_millis),
                min_valid_intervals: a.min_valid_intervals,
                frames: a.frames.into(),
                expect_visible: a.expect_visible.clone(),
                tag: a.tag,
                echo_stdout: a.stdout,
                stdin_stop: a.stdin_stop,
                compositor: !a.no_compositor,
                args,
            };
            run_or_report("watch", watch::run(&mut host, &cfg))
        }
        Cmd::Inject(a) => match a.key_list() {
            Err(why) => {
                eprintln!("termhost-probe inject: {why}");
                2
            }
            Ok(keys) => {
                let cmd = inject::InjectCmd {
                    run_dir: a.run_dir.clone(),
                    target: a.target.spec(),
                    keys,
                    interval: a.interval(),
                    start_delay: Duration::from_millis(a.start_delay_ms),
                    settle: Duration::from_millis(a.settle_ms),
                    tag: a.tag,
                    activate: a.activate,
                    echo_stdout: a.stdout,
                    args,
                };
                run_or_report("inject", inject::run_cmd(&mut host, &cmd))
            }
        },
        Cmd::Selftest(a) => match a.validate() {
            Err(why) => {
                eprintln!("termhost-probe selftest: {why}");
                2
            }
            Ok(()) => {
                let mut cfg = selftest::SelftestConfig {
                    delays_ms: a.delays_ms.clone(),
                    markers_per_delay: a.markers_per_delay,
                    keys: a.keys,
                    require: a.require.clone(),
                    node: a.node.clone(),
                    run_dir: a.run_dir.clone(),
                    ..selftest::SelftestConfig::default()
                };
                if let Some(dir) = &cfg.run_dir {
                    if let Err(e) = std::fs::create_dir_all(dir) {
                        eprintln!(
                            "termhost-probe selftest: cannot create {}: {e}",
                            dir.display()
                        );
                        cfg.run_dir = None;
                    }
                }
                let mut out = if a.json {
                    Sink::stdout_only()
                } else {
                    Sink::discard()
                };
                let outcome = selftest::run(&mut host, &cfg, &mut out);
                if !a.json {
                    print!("{}", selftest::render_human(&outcome));
                }
                outcome.exit_code
            }
        },
    };
    std::process::exit(code);
}

fn run_or_report(what: &str, r: anyhow::Result<i32>) -> i32 {
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("termhost-probe {what}: {e:#}");
            1
        }
    }
}
