//! Replay of real Claude Code pane output through the runner's OWN grid and
//! screen predicates (plan
//! `2026-11-08-finished-sessions-held-open-by-idle-chrome-and-hedge-words`,
//! Phase 1).
//!
//! Each fixture under `fixtures/idle-chrome/` is the raw PTY byte stream of a
//! FINISHED pane, trimmed to the shortest tail (starting at a frame's
//! cursor-hide, `ESC[?25l`) whose replay renders exactly the screen the full
//! stream renders — text, cursor and the input row's DIM attributes — and
//! gzipped. The name is `<class>-<pane>-<cols>x<rows>.vt.gz`. The classes:
//!
//! - `auto-update` — the updater's notification under the prompt
//!   (`✔ Update installed · Restart to apply`, `✘ Auto-update failed · Try
//!   claude doctor`).
//! - `compaction` — idle compaction (`● Compacted while idle, before the prompt
//!   cache expired …`), with the prompt showing DIM ghost text
//!   (`_(No suggestion.)_`).
//! - `finished-turn` — a plain finished turn whose `✻ <Verb>ed for … · done …`
//!   line the grid used to mangle (`·dne 9:09 AM`) because it ignored CHA.
//!
//! [`replay_corpus_from_dir`] replays a whole directory of captured
//! `/terminals/{id}/buffer` JSON documents the same way and prints a per-class
//! table; it is `#[ignore]`d and reads `QONTINUI_IDLE_REPLAY_DIR`.

use std::io::Read;

use qontinui_runner_lib::looping_agent::idle::{idle_veto, snapshot_shows_dialog};

use crate::terminal::graceful_exit::{exit_prompt_ready, input_prompt_ready, ScreenText};
use crate::terminal::grid::Grid;

/// Feed `bytes` into a fresh `cols`×`rows` grid and read it back the way the
/// session does (`TerminalSession::grid_screen`).
pub(crate) fn replay(bytes: &[u8], cols: u16, rows: u16) -> ScreenText {
    let mut grid = Grid::new(cols, rows);
    let mut parser = vte::Parser::new();
    grid.feed(&mut parser, bytes);
    let snap = grid.text_snapshot();
    ScreenText {
        cursor_row_dim: Some(grid.row_dim_mask(snap.cursor_row)),
        lines: snap.lines,
        cursor_row: snap.cursor_row,
        cursor_col: snap.cursor_col,
    }
}

/// Every check the wind-down closer makes on a pane's screen, as one verdict:
/// the idle predicate (what wind-down's `grid_busy` reads), the dialog guard,
/// and the `/exit` input-box check (cursor at the input start, nothing typed;
/// a DIM prompt suggestion counts as nothing typed).
fn verdict(screen: &ScreenText) -> Result<(), String> {
    if let Some(veto) = idle_veto(&screen.lines, screen.cursor_row) {
        return Err(format!("not idle: {veto}"));
    }
    if let Some(marker) = snapshot_shows_dialog(&screen.lines) {
        return Err(format!("dialog: {marker}"));
    }
    exit_prompt_ready(screen).map_err(|e| format!("input box: {e}"))
}

fn gunzip(gz: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut out)
        .expect("fixture is valid gzip");
    out
}

/// `(name, cols, rows, gzipped bytes)`.
const FIXTURES: &[(&str, u16, u16, &[u8])] = &[
    (
        "auto-update-ec38aad6",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/auto-update-ec38aad6-74x13.vt.gz"),
    ),
    (
        "auto-update-d54cf93c",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/auto-update-d54cf93c-74x13.vt.gz"),
    ),
    (
        "auto-update-915350bb",
        74,
        14,
        include_bytes!("fixtures/idle-chrome/auto-update-915350bb-74x14.vt.gz"),
    ),
    (
        "auto-update-8b6db331",
        120,
        30,
        include_bytes!("fixtures/idle-chrome/auto-update-8b6db331-120x30.vt.gz"),
    ),
    (
        "compaction-a6882515",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/compaction-a6882515-74x13.vt.gz"),
    ),
    (
        "compaction-7d87c782",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/compaction-7d87c782-74x13.vt.gz"),
    ),
    (
        "compaction-9fa86f2d",
        120,
        30,
        include_bytes!("fixtures/idle-chrome/compaction-9fa86f2d-120x30.vt.gz"),
    ),
    (
        "finished-turn-8ace97cd",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/finished-turn-8ace97cd-74x13.vt.gz"),
    ),
    (
        "finished-turn-89efc321",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/finished-turn-89efc321-74x13.vt.gz"),
    ),
    (
        "finished-turn-951454f0",
        74,
        13,
        include_bytes!("fixtures/idle-chrome/finished-turn-951454f0-74x13.vt.gz"),
    ),
];

fn fixture(name: &str) -> (Vec<u8>, u16, u16) {
    let (_, cols, rows, gz) = FIXTURES
        .iter()
        .find(|(n, ..)| *n == name)
        .unwrap_or_else(|| panic!("no fixture {name}"));
    (gunzip(gz), *cols, *rows)
}

fn screen_of(name: &str) -> ScreenText {
    let (bytes, cols, rows) = fixture(name);
    replay(&bytes, cols, rows)
}

fn text(screen: &ScreenText) -> String {
    screen.lines.join("\n")
}

#[test]
fn every_finished_pane_fixture_reads_idle_with_an_empty_input_box() {
    for (name, ..) in FIXTURES {
        let screen = screen_of(name);
        assert_eq!(
            verdict(&screen),
            Ok(()),
            "{name} must read idle:\n{}",
            text(&screen)
        );
    }
}

#[test]
fn the_fixtures_show_the_chrome_their_class_names() {
    let has = |name: &str, needle: &str| {
        let t = text(&screen_of(name));
        assert!(t.contains(needle), "{name} lacks {needle:?}:\n{t}");
    };
    has(
        "auto-update-ec38aad6",
        "✔ Update installed · Restart to apply",
    );
    has(
        "auto-update-915350bb",
        "✘ Auto-update failed · Try claude doct",
    );
    has(
        "compaction-a6882515",
        "● Compacted while idle, before the prompt cache expired",
    );
    // `Conversation compacted (ctrl+o for history)` is painted between the
    // notice and the prompt, then overwritten: no captured final screen keeps it.
    has("compaction-9fa86f2d", "● Compacted while idle");
    has("compaction-9fa86f2d", "❯\u{a0}_(No suggestion.)_");
    has(
        "auto-update-8b6db331",
        "_(No suggestion — waiting on the background build.)_",
    );
    // CHA: the finished-turn line renders whole, so the matcher reads it.
    has(
        "finished-turn-8ace97cd",
        "✻ Brewed for 1m 16s · done 9:09 AM",
    );
    has("finished-turn-8ace97cd", "SAFE TO CLOSE — 1 trees checked");
}

#[test]
fn ghost_text_is_dim_and_a_typed_draft_over_it_is_not_empty() {
    // The prompt suggestion is ghost text: painted DIM, so it is not a draft.
    let screen = screen_of("compaction-9fa86f2d");
    assert_eq!(verdict(&screen), Ok(()));
    // The paste arm never takes a suggestion as empty (its CR is unchecked).
    assert!(input_prompt_ready(&screen).is_err());
    // The same bytes with the operator's typed draft painted over the
    // suggestion (normal pen, cursor after the draft): not empty.
    let (mut bytes, cols, rows) = fixture("compaction-9fa86f2d");
    let row = screen.cursor_row + 1;
    bytes.extend_from_slice(format!("\x1b[{row};3H\x1b[22mrun it\x1b[K").as_bytes());
    let typed = replay(&bytes, cols, rows);
    let why = verdict(&typed).unwrap_err();
    assert!(why.starts_with("input box:"), "{why}");
    // Typed text with the cursor moved back to the input start: still a
    // draft, because it is not dim.
    bytes.extend_from_slice(format!("\x1b[{row};3H").as_bytes());
    let homed = replay(&bytes, cols, rows);
    assert_eq!(homed.cursor_col, 2);
    let why = verdict(&homed).unwrap_err();
    assert!(why.contains("not the empty-prompt placeholder"), "{why}");
}

#[test]
fn live_work_painted_over_a_finished_pane_still_reads_busy() {
    for name in [
        "auto-update-ec38aad6",
        "compaction-a6882515",
        "finished-turn-89efc321",
    ] {
        let (bytes, cols, rows) = fixture(name);
        let screen = replay(&bytes, cols, rows);
        let prompt = screen.cursor_row + 1;
        for live in [
            // A live spinner on the row above the input box's top edge.
            "✻ Cogitating… (12s · ↑ 1.2k tokens)".to_string(),
            "✶ Compacting conversation…".to_string(),
            // A finished-turn line whose turn left a shell running.
            "✻ Worked for 1m 2s · done 9:09 AM · 1 shell still running".to_string(),
        ] {
            let mut b = bytes.clone();
            let above_edge = prompt.saturating_sub(2).max(1);
            b.extend_from_slice(
                format!("\x1b[{above_edge};1H{live}\x1b[K\x1b[{prompt};3H").as_bytes(),
            );
            let s = replay(&b, cols, rows);
            assert!(
                idle_veto(&s.lines, s.cursor_row).is_some(),
                "{name} with {live:?} must read busy:\n{}",
                text(&s)
            );
        }
    }
}

/// Replay every `*.json` pane buffer in `QONTINUI_IDLE_REPLAY_DIR` (the shape
/// `GET /terminals/{id}/buffer` returns: `terminal.{cols,rows}` and
/// base64 `buffer.data`) and print one row per pane plus a per-class table.
/// Run with `--ignored --nocapture`.
#[test]
#[ignore = "reads a captured corpus from QONTINUI_IDLE_REPLAY_DIR"]
fn replay_corpus_from_dir() {
    use base64::Engine;
    let Ok(dir) = std::env::var("QONTINUI_IDLE_REPLAY_DIR") else {
        eprintln!("QONTINUI_IDLE_REPLAY_DIR is unset; nothing replayed");
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus dir is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut table: std::collections::BTreeMap<(&str, bool), usize> = Default::default();
    for path in &paths {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("readable"))
                .expect("buffer JSON");
        let dim = |k: &str| doc["terminal"][k].as_u64().unwrap_or(0) as u16;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(doc["buffer"]["data"].as_str().unwrap_or_default())
            .expect("base64 buffer.data");
        let screen = replay(&bytes, dim("cols"), dim("rows"));
        let t = text(&screen);
        let class = if ["Auto-updat", "Update installed", "Auto-update failed"]
            .iter()
            .any(|m| t.contains(m))
        {
            "auto-update"
        } else if t.contains("Compacted while idle") || t.contains("Conversation compacted") {
            "compaction"
        } else {
            "other"
        };
        let v = verdict(&screen);
        let idle = idle_veto(&screen.lines, screen.cursor_row).is_none();
        *table.entry((class, idle)).or_default() += 1;
        println!(
            "{}\t{class}\t{}\t{}",
            path.file_stem().unwrap_or_default().to_string_lossy(),
            if idle { "IDLE" } else { "BUSY" },
            v.err().unwrap_or_default()
        );
    }
    for ((class, idle), n) in &table {
        println!("{class:12} {:4} {n}", if *idle { "idle" } else { "busy" });
    }
}
