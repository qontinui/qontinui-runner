//! Command-line interface. Every subcommand writes JSON lines
//! (`termhost-probe/v1`, see [`crate::records`]).
//!
//! Exit codes: 0 success; 1 failure (a gated selftest check failed, or a setup
//! error); 2 usage; 3 the input arm was refused or aborted for safety.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::model::FramesMode;
use crate::platform::TargetSpec;
use crate::records::SourceId;

#[derive(Debug, Parser)]
#[command(
    name = "termhost-probe",
    version,
    about = "termhost-bench's outside-in instrument: screen-capture marker decoding, window/process sampling, D9-safe key injection",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Which measurement mechanisms work on this machine (OPERATIVE /
    /// DEGRADED / INOPERATIVE-ON-THIS-MACHINE / UNKNOWN). Read-only: it shows a
    /// small non-activating probe window for about a second and spawns node.
    Doctor(DoctorArgs),
    /// Capture one window: frames + decoded markers, window state, real-input
    /// exclusion, process samples -> <run-dir>/probe.jsonl.
    Watch(WatchArgs),
    /// The D9 input arm: tagged keys into the target, foreground checked before
    /// every key, abort on any real input -> <run-dir>/inject.jsonl.
    Inject(InjectArgs),
    /// Prove the instrument on its own window (steals focus and injects keys:
    /// run it on CI, a bench machine or a private display, not a desktop in use).
    Selftest(SelftestArgs),
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// One JSON line instead of the table.
    #[arg(long)]
    pub json: bool,
    /// The node to compare clocks with.
    #[arg(long, default_value = "node")]
    pub node: String,
}

/// Exactly one way of naming the measured window.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct TargetArgs {
    /// Window handle (HWND / X window id), decimal or 0x-hex.
    #[arg(long, value_parser = parse_u64)]
    pub hwnd: Option<u64>,
    /// The largest visible top-level window of this process.
    #[arg(long)]
    pub pid: Option<u32>,
    /// The largest visible top-level window whose title contains this text
    /// (never empty). `inject` refuses a title that more than one window
    /// matches.
    #[arg(long, value_parser = parse_title)]
    pub title: Option<String>,
}

impl TargetArgs {
    /// The target; `unique` makes a title match exactly one window.
    pub fn spec(&self, unique: bool) -> TargetSpec {
        match (self.hwnd, self.pid, &self.title) {
            (Some(h), _, _) => TargetSpec::Handle(h),
            (_, Some(p), _) => TargetSpec::Pid(p),
            (_, _, Some(t)) => TargetSpec::Title {
                text: t.clone(),
                unique,
            },
            // clap's group guarantees one is set; an empty title matches
            // nothing (resolution refuses it).
            _ => TargetSpec::Title {
                text: String::new(),
                unique: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FramesArg {
    All,
    Changes,
    None,
}

impl From<FramesArg> for FramesMode {
    fn from(f: FramesArg) -> Self {
        match f {
            FramesArg::All => FramesMode::All,
            FramesArg::Changes => FramesMode::Changes,
            FramesArg::None => FramesMode::None,
        }
    }
}

/// The capture cross-check's profile (`model::analysis::CrosscheckProfile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CrosscheckProfileArg {
    Strict,
    SoftwareCompositor,
}

impl From<CrosscheckProfileArg> for crate::model::analysis::CrosscheckProfile {
    fn from(p: CrosscheckProfileArg) -> Self {
        match p {
            CrosscheckProfileArg::Strict => Self::Strict,
            CrosscheckProfileArg::SoftwareCompositor => Self::SoftwareCompositor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceArg {
    Wgc,
    Dda,
    X11,
}

impl From<SourceArg> for SourceId {
    fn from(s: SourceArg) -> Self {
        match s {
            SourceArg::Wgc => SourceId::Wgc,
            SourceArg::Dda => SourceId::Dda,
            SourceArg::X11 => SourceId::X11,
        }
    }
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// The run directory: probe.jsonl is written here, gen-*.jsonl are read
    /// from here, and the stop file here ends the run.
    #[arg(long)]
    pub run_dir: PathBuf,
    #[command(flatten)]
    pub target: TargetArgs,
    /// Capture sources, primary first (default: wgc,dda on Windows, x11 on Linux).
    #[arg(long, value_enum, value_delimiter = ',')]
    pub sources: Vec<SourceArg>,
    /// A root of the host's process tree (repeatable).
    #[arg(long)]
    pub host_pid: Vec<u32>,
    /// The run id generators carry (`--termhost-bench-generator <id>`).
    #[arg(long)]
    pub run_id: Option<String>,
    #[arg(long)]
    pub duration_ms: Option<u64>,
    /// Valid 500 ms intervals needed before `hangs` is a count (the catalog's
    /// min_n).
    #[arg(long, default_value_t = crate::stats::HANGS.min_n)]
    pub min_valid_intervals: u64,
    #[arg(long, value_enum, default_value = "all")]
    pub frames: FramesArg,
    /// Sessions that should be on screen (default: every session seen).
    #[arg(long, value_delimiter = ',')]
    pub expect_visible: Vec<u16>,
    /// A co-running `inject`'s tag (0x-hex), so its keys are the probe's.
    #[arg(long, value_parser = parse_tag)]
    pub tag: Option<u32>,
    /// Echo every record to stdout (default: only the summary).
    #[arg(long)]
    pub stdout: bool,
    /// End the run when stdin closes.
    #[arg(long)]
    pub stdin_stop: bool,
    /// Do not sample the compositor (dwm.exe / the X server).
    #[arg(long)]
    pub no_compositor: bool,
    /// The capture cross-check's profile: `strict` (default; every real-GPU
    /// host) or `software-compositor` (the hosted VM, plan D6). The harness
    /// passes the one its environment declares.
    #[arg(long, value_enum, default_value = "strict")]
    pub crosscheck_profile: CrosscheckProfileArg,
}

#[derive(Debug, Args)]
pub struct InjectArgs {
    #[arg(long)]
    pub run_dir: PathBuf,
    #[command(flatten)]
    pub target: TargetArgs,
    /// How many keys (letters a-z, repeating).
    #[arg(long, default_value_t = 100, conflicts_with = "keys")]
    pub count: usize,
    /// Exactly these characters: lowercase ASCII letters and digits, the set
    /// every platform's injector can type with one key.
    #[arg(long)]
    pub keys: Option<String>,
    #[arg(long, default_value_t = 200)]
    pub interval_ms: u64,
    #[arg(long, default_value_t = 0)]
    pub start_delay_ms: u64,
    #[arg(long, default_value_t = 500)]
    pub settle_ms: u64,
    /// Use this tag (0x-hex) instead of a fresh one (pass the same to `watch --tag`).
    #[arg(long, value_parser = parse_tag)]
    pub tag: Option<u32>,
    /// Try once to make the target foreground before the first key (only
    /// after the quiet period below).
    #[arg(long)]
    pub activate: bool,
    /// Nobody may have used the desktop for this long before `inject` changes
    /// focus or types anything; any real or foreign input refuses the arm.
    #[arg(long, default_value_t = 2000)]
    pub quiet_ms: u64,
    #[arg(long)]
    pub stdout: bool,
}

impl InjectArgs {
    pub fn key_list(&self) -> Result<Vec<char>, String> {
        match &self.keys {
            None => Ok(crate::inject::letters(self.count)),
            Some(k) => {
                if k.is_empty() {
                    return Err("--keys is empty".into());
                }
                if let Some(bad) = k
                    .chars()
                    .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
                {
                    return Err(format!(
                        "--keys may hold only lowercase ASCII letters and digits (got {bad:?})"
                    ));
                }
                Ok(k.chars().collect())
            }
        }
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

#[derive(Debug, Args)]
pub struct SelftestArgs {
    /// JSON lines instead of the table.
    #[arg(long)]
    pub json: bool,
    /// A mechanism that must be OPERATIVE (repeatable), e.g. capture.x11.
    #[arg(long)]
    pub require: Vec<String>,
    #[arg(long, value_delimiter = ',', default_values_t = [0u64, 50, 200])]
    pub delays_ms: Vec<u64>,
    #[arg(long, default_value_t = 20)]
    pub markers_per_delay: usize,
    #[arg(long, default_value_t = 20)]
    pub keys: usize,
    #[arg(long, default_value = "node")]
    pub node: String,
    /// Keep the internal engines' records here.
    #[arg(long)]
    pub run_dir: Option<PathBuf>,
    /// Also append a Markdown summary (every check, gated or not, and the
    /// mechanisms) to this file — e.g. `$GITHUB_STEP_SUMMARY`.
    #[arg(long)]
    pub summary_md: Option<PathBuf>,
}

impl SelftestArgs {
    /// `--require` names must be mechanisms the doctor reports.
    pub fn validate(&self) -> Result<(), String> {
        for r in &self.require {
            if !crate::doctor::MECHANISMS.iter().any(|(id, _)| id == r) {
                let known: Vec<&str> = crate::doctor::MECHANISMS
                    .iter()
                    .map(|(id, _)| *id)
                    .collect();
                return Err(format!(
                    "--require {r}: not a mechanism (one of {})",
                    known.join(", ")
                ));
            }
        }
        if self.delays_ms.is_empty() || self.markers_per_delay == 0 || self.keys == 0 {
            return Err(
                "--delays-ms, --markers-per-delay and --keys must be non-empty / positive".into(),
            );
        }
        Ok(())
    }
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let t = s.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).map_err(|e| e.to_string()),
        None => t
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string()),
    }
}

fn parse_title(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        return Err("an empty title would match every window".into());
    }
    Ok(s.to_string())
}

fn parse_tag(s: &str) -> Result<u32, String> {
    let v = parse_u64(s)?;
    let t = u32::try_from(v).map_err(|_| format!("tag {s} does not fit in 32 bits"))?;
    if t == 0 {
        return Err("tag 0 is what an untagged injection carries".into());
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn watch_parses_targets_sources_and_pids() {
        let cli = Cli::try_parse_from([
            "termhost-probe",
            "watch",
            "--run-dir",
            "r",
            "--hwnd",
            "0x1a2b",
            "--sources",
            "wgc,dda",
            "--host-pid",
            "4",
            "--host-pid",
            "9",
            "--run-id",
            "run-1",
        ])
        .expect("parses");
        let Cmd::Watch(w) = cli.cmd else {
            panic!("watch")
        };
        assert_eq!(w.target.spec(false), TargetSpec::Handle(0x1a2b));
        assert_eq!(w.sources, vec![SourceArg::Wgc, SourceArg::Dda]);
        assert_eq!(w.host_pid, vec![4, 9]);
        assert_eq!(w.min_valid_intervals, 60);
    }

    #[test]
    fn a_target_is_required_and_exclusive() {
        assert!(Cli::try_parse_from(["termhost-probe", "watch", "--run-dir", "r"]).is_err());
        assert!(Cli::try_parse_from([
            "termhost-probe",
            "watch",
            "--run-dir",
            "r",
            "--pid",
            "1",
            "--title",
            "x"
        ])
        .is_err());
    }

    #[test]
    fn inject_keys_are_validated() {
        let cli = Cli::try_parse_from([
            "termhost-probe",
            "inject",
            "--run-dir",
            "r",
            "--pid",
            "1",
            "--keys",
            "ab\u{3}",
        ])
        .expect("parses");
        let Cmd::Inject(i) = cli.cmd else {
            panic!("inject")
        };
        assert!(i.key_list().is_err());
        assert!(parse_tag("0").is_err());
        // Uppercase would need a modifier the injectors do not send.
        let cli = Cli::try_parse_from([
            "termhost-probe",
            "inject",
            "--run-dir",
            "r",
            "--pid",
            "1",
            "--keys",
            "aB1",
        ])
        .expect("parses");
        let Cmd::Inject(i) = cli.cmd else {
            panic!("inject")
        };
        assert!(i.key_list().is_err());
        assert_eq!(parse_tag("0x7b4e0001"), Ok(0x7b4e_0001));
    }

    #[test]
    fn an_empty_title_is_refused_and_inject_titles_must_be_unique() {
        for title in ["", "   "] {
            assert!(
                Cli::try_parse_from([
                    "termhost-probe",
                    "inject",
                    "--run-dir",
                    "r",
                    "--title",
                    title
                ])
                .is_err(),
                "{title:?}"
            );
        }
        let cli = Cli::try_parse_from([
            "termhost-probe",
            "inject",
            "--run-dir",
            "r",
            "--title",
            "Terminal",
        ])
        .expect("parses");
        let Cmd::Inject(i) = cli.cmd else {
            panic!("inject")
        };
        assert_eq!(
            i.target.spec(true),
            TargetSpec::Title {
                text: "Terminal".into(),
                unique: true
            }
        );
        assert_eq!(i.quiet_ms, 2000);
    }

    #[test]
    fn selftest_require_names_are_checked() {
        let cli = Cli::try_parse_from([
            "termhost-probe",
            "selftest",
            "--json",
            "--require",
            "capture.x11",
        ])
        .expect("parses");
        let Cmd::Selftest(s) = cli.cmd else {
            panic!("selftest")
        };
        assert!(s.validate().is_ok());
        assert_eq!(s.delays_ms, vec![0, 50, 200]);
        let cli = Cli::try_parse_from(["termhost-probe", "selftest", "--require", "capture.vnc"])
            .expect("parses");
        let Cmd::Selftest(s) = cli.cmd else {
            panic!("selftest")
        };
        assert!(s.validate().is_err());
    }
}
