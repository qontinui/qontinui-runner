//! Replay census for the turn-complete recognizer (plan
//! `2026-10-23-wind-down-idle-check-mistakes-the-completed-turn-line-for-a-spinner`,
//! Phase 0 item 1 and Phase 5's falsifier).
//!
//! Feeds captured raw pane buffers (`GET /terminals/{id}/buffer`, base64-decoded)
//! through the runner's own VT grid model and counts which clause of
//! [`snapshot_idle_verdict`] decides each one. `#[ignore]`d: it reads
//! `$IDLE_CENSUS_DIR`, which holds a `manifest.json` —
//! `[{"file": "...", "status": "...", "cols": N, "rows": N}, ...]` — and the
//! buffer files it names. Those captures are session content: they stay in a
//! local scratch directory and are never committed. The test prints clause
//! COUNTS only, never pane text — for a pane that still reads `glyph:*`, the
//! offending line is reduced to a SHAPE (letter runs → `w`, digit runs → `9`)
//! and only the count per shape is printed, so a wording the completed-turn
//! recognizer does not cover can be named without copying session content.
//!
//! ```text
//! IDLE_CENSUS_DIR=<dir> cargo-guard.sh test -- idle_census --ignored --nocapture
//! ```

use std::collections::BTreeMap;

use qontinui_runner_lib::looping_agent::idle::{
    is_completed_turn_line, snapshot_idle_verdict, IdleVerdict,
};
use qontinui_runner_lib::wind_down::GridBusyEvidence;

use super::grid::Grid;

#[test]
#[ignore = "reads captured pane buffers from $IDLE_CENSUS_DIR"]
fn idle_census_replay() {
    let dir = std::env::var("IDLE_CENSUS_DIR").expect("IDLE_CENSUS_DIR names the capture dir");
    let manifest = std::fs::read_to_string(format!("{dir}/manifest.json")).expect("manifest.json");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&manifest).expect("manifest is a JSON array");
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &entries {
        let field = |k: &str| entry[k].as_u64().and_then(|n| u16::try_from(n).ok());
        let (Some(file), Some(cols), Some(rows)) =
            (entry["file"].as_str(), field("cols"), field("rows"))
        else {
            panic!("manifest entry needs file/cols/rows: {entry}");
        };
        let status = entry["status"].as_str().unwrap_or("unknown");
        let bytes = std::fs::read(format!("{dir}/{file}")).expect("buffer file");
        let mut grid = Grid::new(cols, rows);
        grid.feed(&mut vte::Parser::new(), &bytes);
        let snap = grid.text_snapshot();
        let verdict = snapshot_idle_verdict(&snap.lines, snap.cursor_row);
        let clause = GridBusyEvidence::from_screen(verdict)
            .map_or_else(|| "idle".to_string(), GridBusyEvidence::wire);
        *counts.entry(format!("{status} {clause}")).or_default() += 1;
        if let IdleVerdict::Glyph(glyph) = verdict {
            *shapes.entry(culprit_shape(&snap.lines, glyph)).or_default() += 1;
        }
    }
    println!("idle census over {} panes: {counts:#?}", entries.len());
    println!("shapes of the lines behind glyph:* verdicts: {shapes:#?}");
}

/// The shape of what made a `glyph:*` verdict: the first glyph line that is
/// not a completed-turn line, or — when every glyph line IS one, so the
/// exemption's POSITION rule refused it — the last such line, the rows
/// between it and the next `❯` row (`⏎`-joined), and how many glyph lines
/// the screen carries.
fn culprit_shape(lines: &[String], glyph: char) -> String {
    if let Some(line) = lines
        .iter()
        .find(|l| l.contains(glyph) && !is_completed_turn_line(l))
    {
        return shape_of(line);
    }
    let glyph_rows: Vec<usize> = (0..lines.len())
        .filter(|&r| lines[r].contains(glyph))
        .collect();
    let Some(&last) = glyph_rows.last() else {
        return "<none>".to_string();
    };
    let gap: Vec<String> = lines[last + 1..]
        .iter()
        .take_while(|l| !l.contains('❯'))
        .map(|l| shape_of(l))
        .collect();
    let below = if lines[last + 1..].iter().any(|l| l.contains('❯')) {
        "❯ below"
    } else {
        "no ❯ below"
    };
    format!(
        "{} glyph line(s), all completed turns; gap [{}]; {below}",
        glyph_rows.len(),
        gap.join(" ⏎ ")
    )
}

/// A line with its letter runs replaced by `w` and its digit runs by `9`.
fn shape_of(line: &str) -> String {
    let mut out = String::new();
    let mut last = None;
    for ch in line.trim().chars() {
        let class = if ch.is_alphabetic() {
            'w'
        } else if ch.is_ascii_digit() {
            '9'
        } else {
            ch
        };
        if !(Some(class) == last && (class == 'w' || class == '9')) {
            out.push(class);
        }
        last = Some(class);
    }
    out
}
