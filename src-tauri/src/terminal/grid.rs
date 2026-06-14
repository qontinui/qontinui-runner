//! Server-side VT parser — per-session cell grid state.
//!
//! `Grid` consumes raw PTY output via `vte::Parser` and maintains a
//! coherent cell-grid snapshot of the terminal viewport. The frontend
//! fetches `GridSnapshot` once at mount and paints the final state in
//! a single write — no byte-stream replay, no scroll-off ambiguity.
//! See `plans/terminal-grid-snapshot.md` for the full design.

use serde::Serialize;
use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

// ---- Color encoding ------------------------------------------------------

/// High byte 0x01 marks the "default" color sentinel. Low 24 bits hold
/// packed RGB (0xRRGGBB) for explicit colors. We pick 0x01 (not 0x00)
/// for the default tag so that an all-zero `u32` cell still renders as
/// black-on-black explicit color rather than "default" — keeps Default
/// derives unambiguous when callers want truly explicit black.
pub const COLOR_DEFAULT: u32 = 0x0100_0000;
const COLOR_RGB_MASK: u32 = 0x00FF_FFFF;

#[inline]
fn pack_rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

// ---- Attribute bitfield --------------------------------------------------

pub const ATTR_BOLD: u16 = 1 << 0;
pub const ATTR_ITALIC: u16 = 1 << 1;
pub const ATTR_UNDERLINE: u16 = 1 << 2;
pub const ATTR_INVERSE: u16 = 1 << 3;
pub const ATTR_DIM: u16 = 1 << 4;
pub const ATTR_STRIKE: u16 = 1 << 5;
/// Left half of a width-2 glyph; carries the actual char.
pub const ATTR_WIDE_LEFT: u16 = 1 << 6;
/// Right half of a width-2 glyph; sentinel cell, ch is ' '.
pub const ATTR_WIDE_RIGHT: u16 = 1 << 7;

// ---- Cell ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cell {
    #[serde(rename = "c")]
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    #[serde(rename = "a")]
    pub attrs: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
            attrs: 0,
        }
    }
}

// ---- Cursor --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

// ---- Grid ----------------------------------------------------------------

pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    cursor: Cursor,
    saved_cursor: Option<Cursor>,
    scroll_top: u16,
    scroll_bottom: u16,
    title: Option<String>,
    /// Pen state — applied to printed cells.
    cur_fg: u32,
    cur_bg: u32,
    cur_attrs: u16,
    /// DEC private mode flags we observe but the grid itself does not gate
    /// cell mutation on. `sync_output` (DEC 2026) is read by the session
    /// reader thread via [`Grid::sync_output`] to coalesce a multi-read frame
    /// into one emit; `alt_screen` (DEC 1049) is surfaced in the snapshot for
    /// the bootstrap-paint guard.
    sync_output: bool,
    alt_screen: bool,
    dirty: bool,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let cells = vec![Cell::default(); cols as usize * rows as usize];
        Self {
            cols,
            rows,
            cells,
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: true,
            },
            saved_cursor: None,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            title: None,
            cur_fg: COLOR_DEFAULT,
            cur_bg: COLOR_DEFAULT,
            cur_attrs: 0,
            sync_output: false,
            alt_screen: false,
            dirty: false,
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }
    pub fn rows(&self) -> u16 {
        self.rows
    }
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Whether the VT stream is currently inside a DEC mode 2026
    /// (synchronized output) block — i.e. a `?2026h` was seen with no
    /// matching `?2026l` yet. Full-screen TUIs (Claude Code) bracket a
    /// whole-frame redraw in `?2026h … ?2026l` so the terminal can swap the
    /// frame atomically. The reader thread reads this at the emit boundary to
    /// coalesce a multi-read frame into a single `terminal-output` event,
    /// killing mid-frame overdraw. Observe-only in the grid itself.
    pub fn sync_output(&self) -> bool {
        self.sync_output
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut new_cells = vec![Cell::default(); cols as usize * rows as usize];
        let copy_rows = self.rows.min(rows);
        let copy_cols = self.cols.min(cols);
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                let src = (r as usize) * (self.cols as usize) + c as usize;
                let dst = (r as usize) * (cols as usize) + c as usize;
                new_cells[dst] = self.cells[src];
            }
        }
        self.cells = new_cells;
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        if self.cursor.row >= rows {
            self.cursor.row = rows - 1;
        }
        if self.cursor.col >= cols {
            self.cursor.col = cols - 1;
        }
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = Cell::default();
        }
        self.cursor = Cursor {
            row: 0,
            col: 0,
            visible: self.cursor.visible,
        };
        self.dirty = true;
    }

    pub fn row_text(&self, row: u16) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = (row as usize) * (self.cols as usize);
        let end = start + self.cols as usize;
        let mut s = String::with_capacity(self.cols as usize);
        for cell in &self.cells[start..end] {
            if cell.attrs & ATTR_WIDE_RIGHT != 0 {
                continue;
            }
            s.push(cell.ch);
        }
        // Trim trailing whitespace per row for the buffer endpoint contract.
        let trimmed = s.trim_end().to_string();
        trimmed
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.rows).map(|r| self.row_text(r)).collect()
    }

    pub fn snapshot(&self) -> GridSnapshot {
        GridSnapshot {
            cols: self.cols,
            rows: self.rows,
            cursor: self.cursor,
            title: self.title.clone(),
            alt_screen: self.alt_screen,
            cells: self.cells.clone(),
        }
    }

    /// Convenience: feed bytes through a parser into this grid.
    pub fn feed(&mut self, parser: &mut vte::Parser, bytes: &[u8]) {
        let mut perf = GridPerformer::new(self);
        parser.advance(&mut perf, bytes);
    }

    // ---- internal helpers ------------------------------------------------

    fn idx(&self, row: u16, col: u16) -> usize {
        (row as usize) * (self.cols as usize) + col as usize
    }

    fn put_cell(&mut self, row: u16, col: u16, cell: Cell) {
        let i = self.idx(row, col);
        if let Some(slot) = self.cells.get_mut(i) {
            *slot = cell;
            self.dirty = true;
        }
    }

    fn clear_row_range(&mut self, row: u16, start_col: u16, end_col_exclusive: u16) {
        if row >= self.rows {
            return;
        }
        let start = self.idx(row, start_col.min(self.cols));
        let end = self.idx(row, end_col_exclusive.min(self.cols));
        for cell in &mut self.cells[start..end] {
            *cell = Cell::default();
        }
        self.dirty = true;
    }

    fn scroll_up(&mut self, n: u16) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        if top > bottom {
            return;
        }
        let region_rows = bottom - top + 1;
        let n = (n as usize).min(region_rows);
        let cols = self.cols as usize;
        for r in top..=bottom {
            let src = r + n;
            if src <= bottom {
                let src_start = src * cols;
                let dst_start = r * cols;
                self.cells
                    .copy_within(src_start..src_start + cols, dst_start);
            } else {
                let dst_start = r * cols;
                for cell in &mut self.cells[dst_start..dst_start + cols] {
                    *cell = Cell::default();
                }
            }
        }
        self.dirty = true;
    }

    fn newline(&mut self) {
        if self.cursor.row >= self.scroll_bottom {
            self.scroll_up(1);
            self.cursor.row = self.scroll_bottom;
        } else {
            self.cursor.row += 1;
        }
    }

    fn print_char(&mut self, ch: char) {
        let width = ch.width().unwrap_or(1) as u16;
        if width == 0 {
            return;
        }
        if self.cursor.col + width > self.cols {
            // Wrap.
            self.cursor.col = 0;
            self.newline();
        }
        let row = self.cursor.row.min(self.rows.saturating_sub(1));
        let col = self.cursor.col;
        if width == 2 {
            let left = Cell {
                ch,
                fg: self.cur_fg,
                bg: self.cur_bg,
                attrs: self.cur_attrs | ATTR_WIDE_LEFT,
            };
            let right = Cell {
                ch: ' ',
                fg: self.cur_fg,
                bg: self.cur_bg,
                attrs: self.cur_attrs | ATTR_WIDE_RIGHT,
            };
            self.put_cell(row, col, left);
            if col + 1 < self.cols {
                self.put_cell(row, col + 1, right);
            }
            self.cursor.col += 2;
        } else {
            let cell = Cell {
                ch,
                fg: self.cur_fg,
                bg: self.cur_bg,
                attrs: self.cur_attrs,
            };
            self.put_cell(row, col, cell);
            self.cursor.col += 1;
        }
    }

    fn handle_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.reset_pen();
            return;
        }
        let flat: Vec<u16> = params.iter().flatten().copied().collect();
        let mut i = 0;
        while i < flat.len() {
            let p = flat[i];
            match p {
                0 => self.reset_pen(),
                1 => self.cur_attrs |= ATTR_BOLD,
                2 => self.cur_attrs |= ATTR_DIM,
                3 => self.cur_attrs |= ATTR_ITALIC,
                4 => self.cur_attrs |= ATTR_UNDERLINE,
                7 => self.cur_attrs |= ATTR_INVERSE,
                9 => self.cur_attrs |= ATTR_STRIKE,
                22 => self.cur_attrs &= !(ATTR_BOLD | ATTR_DIM),
                23 => self.cur_attrs &= !ATTR_ITALIC,
                24 => self.cur_attrs &= !ATTR_UNDERLINE,
                27 => self.cur_attrs &= !ATTR_INVERSE,
                29 => self.cur_attrs &= !ATTR_STRIKE,
                30..=37 => self.cur_fg = ansi_color_16(p as u8 - 30, false),
                90..=97 => self.cur_fg = ansi_color_16(p as u8 - 90, true),
                39 => self.cur_fg = COLOR_DEFAULT,
                40..=47 => self.cur_bg = ansi_color_16(p as u8 - 40, false),
                100..=107 => self.cur_bg = ansi_color_16(p as u8 - 100, true),
                49 => self.cur_bg = COLOR_DEFAULT,
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 1 < flat.len() {
                        match flat[i + 1] {
                            2 => {
                                if i + 4 < flat.len() {
                                    let r = flat[i + 2] as u8;
                                    let g = flat[i + 3] as u8;
                                    let b = flat[i + 4] as u8;
                                    let c = pack_rgb(r, g, b);
                                    if is_fg {
                                        self.cur_fg = c;
                                    } else {
                                        self.cur_bg = c;
                                    }
                                    i += 4;
                                }
                            }
                            5 => {
                                if i + 2 < flat.len() {
                                    let n = flat[i + 2] as u8;
                                    let c = palette_256(n);
                                    if is_fg {
                                        self.cur_fg = c;
                                    } else {
                                        self.cur_bg = c;
                                    }
                                    i += 2;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn reset_pen(&mut self) {
        self.cur_fg = COLOR_DEFAULT;
        self.cur_bg = COLOR_DEFAULT;
        self.cur_attrs = 0;
    }
}

// ---- Wire format ---------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// True when the grid is currently on the alternate screen (DEC `?1049h`).
    /// The grid is single-buffer (no save/swap), so a snapshot taken while
    /// this is true holds alt-screen content; the frontend bootstrap paint
    /// hard-skips in that case rather than overlay alt content onto a fresh
    /// xterm. See `paintGrid.ts` / `TerminalInstance.tsx`.
    pub alt_screen: bool,
    pub cells: Vec<Cell>,
}

// ---- Text snapshot, search, diff ----------------------------------------
//
// Higher-level helpers that operate on the rendered grid as text. These
// are what consumers (the agentic verifier, automated tests, the UI Bridge
// cell-content test pattern) should reach for instead of poking at cells
// or replaying byte streams.

/// Compact text-only view of a grid. Stable wire shape — safe to consume
/// from the verification module, mobile bridge, automated tests.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// Each row's rendered text, trimmed-right.
    pub lines: Vec<String>,
    /// Lines joined with `\n`, suitable for pasting into a verifier prompt.
    pub text: String,
    pub cursor_row: u16,
    pub cursor_col: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One match from `Grid::search`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
    /// The full row text (trimmed-right) where the match was found.
    pub line: String,
}

/// Errors `Grid::search` can return when the caller passes a regex.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
}

/// One row-level change between two grids.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LineChange {
    /// Row exists in `b` but not `a` (rows added on the larger grid).
    Added { row: u16, after: String },
    /// Row exists in `a` but not `b` (rows removed on the smaller grid).
    Removed { row: u16, before: String },
    /// Row index exists in both but the rendered text differs.
    Modified {
        row: u16,
        before: String,
        after: String,
    },
}

/// Result of `Grid::diff_lines`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridDiff {
    pub a_rows: u16,
    pub b_rows: u16,
    pub changes: Vec<LineChange>,
}

impl Grid {
    /// Cheap text-only view for verifiers / external tools. The
    /// `lines` and `text` fields contain the same data in two shapes:
    /// `lines` for row-level access, `text` for prompt insertion.
    pub fn text_snapshot(&self) -> TextSnapshot {
        let lines = self.lines();
        let text = lines.join("\n");
        TextSnapshot {
            cols: self.cols,
            rows: self.rows,
            lines,
            text,
            cursor_row: self.cursor.row,
            cursor_col: self.cursor.col,
            title: self.title.clone(),
        }
    }

    /// Search the rendered grid for `needle`. When `regex` is true, the
    /// needle is compiled as a regex; otherwise it's a case-sensitive
    /// substring match. Returns at most one hit per row (the leftmost),
    /// matching how a user would scan the screen.
    pub fn search(&self, needle: &str, regex: bool) -> Result<Vec<SearchHit>, SearchError> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        if regex {
            let re =
                regex::Regex::new(needle).map_err(|e| SearchError::InvalidRegex(e.to_string()))?;
            for row in 0..self.rows {
                let line = self.row_text(row);
                if let Some(m) = re.find(&line) {
                    // Byte offsets → char-column offsets. Row text is
                    // typically ASCII for terminal output, but be safe.
                    let start_col = line[..m.start()].chars().count() as u16;
                    let end_col = line[..m.end()].chars().count() as u16;
                    hits.push(SearchHit {
                        row,
                        start_col,
                        end_col,
                        line,
                    });
                }
            }
        } else {
            for row in 0..self.rows {
                let line = self.row_text(row);
                if let Some(byte_idx) = line.find(needle) {
                    let start_col = line[..byte_idx].chars().count() as u16;
                    let end_col = start_col + needle.chars().count() as u16;
                    hits.push(SearchHit {
                        row,
                        start_col,
                        end_col,
                        line,
                    });
                }
            }
        }
        Ok(hits)
    }

    /// Row-by-row diff of two grids. Identical rows are omitted; only
    /// rows that differ — added, removed, or modified — show up in the
    /// `changes` list.
    pub fn diff_lines(&self, other: &Grid) -> GridDiff {
        let a_lines = self.lines();
        let b_lines = other.lines();
        let common = a_lines.len().min(b_lines.len());
        let mut changes = Vec::new();

        for row in 0..common {
            if a_lines[row] != b_lines[row] {
                changes.push(LineChange::Modified {
                    row: row as u16,
                    before: a_lines[row].clone(),
                    after: b_lines[row].clone(),
                });
            }
        }
        if a_lines.len() > common {
            for row in common..a_lines.len() {
                changes.push(LineChange::Removed {
                    row: row as u16,
                    before: a_lines[row].clone(),
                });
            }
        } else if b_lines.len() > common {
            for row in common..b_lines.len() {
                changes.push(LineChange::Added {
                    row: row as u16,
                    after: b_lines[row].clone(),
                });
            }
        }

        GridDiff {
            a_rows: self.rows,
            b_rows: other.rows,
            changes,
        }
    }
}

// ---- 256-color palette ---------------------------------------------------

fn ansi_color_16(idx: u8, bright: bool) -> u32 {
    // Standard xterm 16-color palette.
    static BASE: [(u8, u8, u8); 8] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
    ];
    static BRIGHT: [(u8, u8, u8); 8] = [
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let (r, g, b) = if bright {
        BRIGHT[idx as usize & 7]
    } else {
        BASE[idx as usize & 7]
    };
    pack_rgb(r, g, b)
}

fn palette_256(n: u8) -> u32 {
    if n < 16 {
        ansi_color_16(n & 7, n >= 8)
    } else if n < 232 {
        // 6x6x6 cube
        let v = n - 16;
        let r_idx = v / 36;
        let g_idx = (v % 36) / 6;
        let b_idx = v % 6;
        let map = |i: u8| -> u8 {
            if i == 0 {
                0
            } else {
                55 + i * 40
            }
        };
        pack_rgb(map(r_idx), map(g_idx), map(b_idx))
    } else {
        // grayscale 24-step
        let v = 8 + (n - 232) * 10;
        pack_rgb(v, v, v)
    }
}

// ---- Performer -----------------------------------------------------------

pub struct GridPerformer<'a> {
    grid: &'a mut Grid,
}

impl<'a> GridPerformer<'a> {
    pub fn new(grid: &'a mut Grid) -> Self {
        Self { grid }
    }
}

#[inline]
fn first_param(params: &Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .filter(|v| *v != 0)
        .unwrap_or(default)
}

#[inline]
fn first_param_default_zero(params: &Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .unwrap_or(default)
}

impl<'a> Perform for GridPerformer<'a> {
    fn print(&mut self, c: char) {
        self.grid.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.grid.newline(),
            b'\r' => self.grid.cursor.col = 0,
            b'\x08' => {
                if self.grid.cursor.col > 0 {
                    self.grid.cursor.col -= 1;
                }
            }
            b'\t' => {
                let cols = self.grid.cols;
                let next = ((self.grid.cursor.col / 8) + 1) * 8;
                self.grid.cursor.col = next.min(cols.saturating_sub(1));
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.first() == Some(&b'?');
        match action {
            'H' | 'f' => {
                let row = first_param(params, 1).saturating_sub(1);
                let col = params
                    .iter()
                    .nth(1)
                    .and_then(|p| p.first().copied())
                    .filter(|v| *v != 0)
                    .unwrap_or(1)
                    .saturating_sub(1);
                self.grid.cursor.row = row.min(self.grid.rows.saturating_sub(1));
                self.grid.cursor.col = col.min(self.grid.cols.saturating_sub(1));
            }
            'A' => {
                let n = first_param(params, 1);
                self.grid.cursor.row = self.grid.cursor.row.saturating_sub(n);
            }
            'B' => {
                let n = first_param(params, 1);
                self.grid.cursor.row =
                    (self.grid.cursor.row + n).min(self.grid.rows.saturating_sub(1));
            }
            'C' => {
                let n = first_param(params, 1);
                self.grid.cursor.col =
                    (self.grid.cursor.col + n).min(self.grid.cols.saturating_sub(1));
            }
            'D' => {
                let n = first_param(params, 1);
                self.grid.cursor.col = self.grid.cursor.col.saturating_sub(n);
            }
            'J' => {
                let mode = first_param_default_zero(params, 0);
                let cols = self.grid.cols;
                let rows = self.grid.rows;
                match mode {
                    0 => {
                        // From cursor to end of screen.
                        let row = self.grid.cursor.row;
                        let col = self.grid.cursor.col;
                        self.grid.clear_row_range(row, col, cols);
                        for r in (row + 1)..rows {
                            self.grid.clear_row_range(r, 0, cols);
                        }
                    }
                    1 => {
                        let row = self.grid.cursor.row;
                        let col = self.grid.cursor.col;
                        for r in 0..row {
                            self.grid.clear_row_range(r, 0, cols);
                        }
                        self.grid.clear_row_range(row, 0, col + 1);
                    }
                    2 | 3 => {
                        for r in 0..rows {
                            self.grid.clear_row_range(r, 0, cols);
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = first_param_default_zero(params, 0);
                let cols = self.grid.cols;
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col;
                match mode {
                    0 => self.grid.clear_row_range(row, col, cols),
                    1 => self.grid.clear_row_range(row, 0, col + 1),
                    2 => self.grid.clear_row_range(row, 0, cols),
                    _ => {}
                }
            }
            'm' => self.grid.handle_sgr(params),
            'r' => {
                let top = first_param(params, 1).saturating_sub(1);
                let bottom = params
                    .iter()
                    .nth(1)
                    .and_then(|p| p.first().copied())
                    .filter(|v| *v != 0)
                    .unwrap_or(self.grid.rows)
                    .saturating_sub(1);
                let bottom = bottom.min(self.grid.rows.saturating_sub(1));
                if top < bottom {
                    self.grid.scroll_top = top;
                    self.grid.scroll_bottom = bottom;
                    self.grid.cursor.row = 0;
                    self.grid.cursor.col = 0;
                }
            }
            'h' if private => {
                for p in params.iter().flatten() {
                    match *p {
                        25 => self.grid.cursor.visible = true,
                        1049 => self.grid.alt_screen = true,
                        2026 => self.grid.sync_output = true,
                        _ => {}
                    }
                }
            }
            'l' if private => {
                for p in params.iter().flatten() {
                    match *p {
                        25 => self.grid.cursor.visible = false,
                        1049 => self.grid.alt_screen = false,
                        2026 => self.grid.sync_output = false,
                        _ => {}
                    }
                }
            }
            's' => self.grid.saved_cursor = Some(self.grid.cursor),
            'u' => {
                if let Some(c) = self.grid.saved_cursor {
                    self.grid.cursor = c;
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.len() < 2 {
            return;
        }
        let kind = params[0];
        if kind == b"0" || kind == b"2" {
            if let Ok(s) = std::str::from_utf8(params[1]) {
                self.grid.title = Some(s.to_string());
            }
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        // ESC 7 / ESC 8 — DECSC / DECRC save/restore cursor.
        match byte {
            b'7' => self.grid.saved_cursor = Some(self.grid.cursor),
            b'8' => {
                if let Some(c) = self.grid.saved_cursor {
                    self.grid.cursor = c;
                }
            }
            _ => {}
        }
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(grid: &mut Grid, bytes: &[u8]) {
        let mut parser = vte::Parser::new();
        grid.feed(&mut parser, bytes);
    }

    #[test]
    fn empty_snapshot_has_correct_dims_and_default_cells() {
        let grid = Grid::new(80, 24);
        let snap = grid.snapshot();
        assert_eq!(snap.cols, 80);
        assert_eq!(snap.rows, 24);
        assert_eq!(snap.cells.len(), 80 * 24);
        assert!(snap.cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(snap.cursor.row, 0);
        assert_eq!(snap.cursor.col, 0);
        assert!(snap.cursor.visible);
    }

    #[test]
    fn print_hello_advances_cursor() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"hello");
        let cells = &grid.snapshot().cells;
        assert_eq!(cells[0].ch, 'h');
        assert_eq!(cells[1].ch, 'e');
        assert_eq!(cells[2].ch, 'l');
        assert_eq!(cells[3].ch, 'l');
        assert_eq!(cells[4].ch, 'o');
        assert_eq!(grid.cursor().col, 5);
        assert_eq!(grid.cursor().row, 0);
    }

    #[test]
    fn sgr_truecolor_then_reset() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"\x1b[38;2;255;0;0mABC\x1b[0mD");
        let cells = &grid.snapshot().cells;
        assert_eq!(cells[0].ch, 'A');
        assert_eq!(cells[0].fg, 0x00FF_0000);
        assert_eq!(cells[1].fg, 0x00FF_0000);
        assert_eq!(cells[2].fg, 0x00FF_0000);
        assert_eq!(cells[3].ch, 'D');
        assert_eq!(cells[3].fg, COLOR_DEFAULT);
        assert_eq!(cells[3].attrs, 0);
    }

    #[test]
    fn cursor_position_is_one_indexed() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"\x1b[10;20H");
        assert_eq!(grid.cursor().row, 9);
        assert_eq!(grid.cursor().col, 19);
    }

    #[test]
    fn erase_display_2j_then_home() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"hello world");
        feed(&mut grid, b"\x1b[2J\x1b[H");
        assert!(grid.snapshot().cells.iter().all(|c| *c == Cell::default()));
        assert_eq!(grid.cursor().row, 0);
        assert_eq!(grid.cursor().col, 0);
    }

    #[test]
    fn wide_char_emits_sentinel_pair() {
        let mut grid = Grid::new(80, 24);
        // U+4F60 你 has east-asian width 2.
        feed(&mut grid, "你".as_bytes());
        let cells = &grid.snapshot().cells;
        assert_eq!(cells[0].ch, '你');
        assert_ne!(cells[0].attrs & ATTR_WIDE_LEFT, 0);
        assert_ne!(cells[1].attrs & ATTR_WIDE_RIGHT, 0);
        assert_eq!(grid.cursor().col, 2);
    }

    #[test]
    fn resize_preserves_content_and_fills_default() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"hello");
        grid.resize(100, 30);
        let snap = grid.snapshot();
        assert_eq!(snap.cols, 100);
        assert_eq!(snap.rows, 30);
        assert_eq!(snap.cells.len(), 100 * 30);
        // Original content preserved.
        assert_eq!(snap.cells[0].ch, 'h');
        assert_eq!(snap.cells[4].ch, 'o');
        // Cells beyond original 80x24 region are default.
        let last = snap.cells.len() - 1;
        assert_eq!(snap.cells[last], Cell::default());
        // Cell at (row=0, col=80) is in the new region — default.
        assert_eq!(snap.cells[80], Cell::default());
    }

    #[test]
    fn dec_2026_sync_output_does_not_corrupt_cells() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"\x1b[?2026hABC\x1b[?2026lDEF");
        let cells = &grid.snapshot().cells;
        assert_eq!(cells[0].ch, 'A');
        assert_eq!(cells[1].ch, 'B');
        assert_eq!(cells[2].ch, 'C');
        assert_eq!(cells[3].ch, 'D');
        assert_eq!(cells[4].ch, 'E');
        assert_eq!(cells[5].ch, 'F');
    }

    #[test]
    fn sync_output_getter_tracks_dec_2026_open_and_close() {
        let mut grid = Grid::new(80, 24);
        assert!(!grid.sync_output(), "starts closed");
        // Open the synchronized-output block.
        feed(&mut grid, b"\x1b[?2026h");
        assert!(grid.sync_output(), "open after ?2026h");
        // Bytes inside the block keep it open (mid-frame).
        feed(&mut grid, b"PARTIAL");
        assert!(grid.sync_output(), "still open mid-frame");
        // Close it.
        feed(&mut grid, b"\x1b[?2026l");
        assert!(!grid.sync_output(), "closed after ?2026l");
    }

    #[test]
    fn osc_set_title() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"\x1b]0;Hello Title\x07");
        assert_eq!(grid.title(), Some("Hello Title"));
    }

    #[test]
    fn newline_and_carriage_return() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"abc\r\ndef");
        assert_eq!(grid.snapshot().cells[0].ch, 'a');
        assert_eq!(grid.snapshot().cells[80].ch, 'd');
        assert_eq!(grid.cursor().row, 1);
        assert_eq!(grid.cursor().col, 3);
    }

    #[test]
    fn lines_strips_trailing_whitespace() {
        let mut grid = Grid::new(80, 24);
        feed(&mut grid, b"hi");
        let lines = grid.lines();
        assert_eq!(lines[0], "hi");
        assert_eq!(lines[1], "");
    }

    #[test]
    fn scroll_when_cursor_passes_bottom() {
        let mut grid = Grid::new(80, 3);
        feed(&mut grid, b"a\r\nb\r\nc\r\nd");
        // After scrolling, row 0 should be 'b', row 1 'c', row 2 'd'.
        let snap = grid.snapshot();
        assert_eq!(snap.cells[0].ch, 'b');
        assert_eq!(snap.cells[80].ch, 'c');
        assert_eq!(snap.cells[160].ch, 'd');
    }

    // ---- text_snapshot / search / diff -----------------------------------

    #[test]
    fn text_snapshot_joins_lines_with_newline() {
        let mut grid = Grid::new(20, 3);
        feed(&mut grid, b"hello\r\nworld");
        let snap = grid.text_snapshot();
        assert_eq!(snap.lines.len(), 3);
        assert_eq!(snap.lines[0], "hello");
        assert_eq!(snap.lines[1], "world");
        assert_eq!(snap.lines[2], "");
        assert_eq!(snap.text, "hello\nworld\n");
        assert_eq!(snap.cursor_row, 1);
        assert_eq!(snap.cursor_col, 5);
    }

    #[test]
    fn search_substring_returns_one_hit_per_row() {
        let mut grid = Grid::new(20, 4);
        feed(&mut grid, b"foo bar foo\r\nclaude\r\nfoo");
        let hits = grid.search("foo", false).unwrap();
        assert_eq!(hits.len(), 2, "two rows contain 'foo'");
        assert_eq!(hits[0].row, 0);
        assert_eq!(hits[0].start_col, 0);
        assert_eq!(hits[0].end_col, 3);
        assert_eq!(hits[0].line, "foo bar foo");
        assert_eq!(hits[1].row, 2);
    }

    #[test]
    fn search_regex_compiles_and_matches() {
        let mut grid = Grid::new(40, 3);
        feed(&mut grid, b"build #42 ok\r\nbuild #007 fail");
        let hits = grid.search(r"#(\d+)", true).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].row, 0);
        assert_eq!(hits[1].row, 1);
    }

    #[test]
    fn search_invalid_regex_returns_error() {
        let grid = Grid::new(20, 2);
        let err = grid.search("(unclosed", true);
        assert!(matches!(err, Err(SearchError::InvalidRegex(_))));
    }

    #[test]
    fn search_empty_needle_returns_empty() {
        let mut grid = Grid::new(20, 2);
        feed(&mut grid, b"anything");
        assert!(grid.search("", false).unwrap().is_empty());
    }

    #[test]
    fn diff_lines_modified_only() {
        let mut a = Grid::new(20, 3);
        let mut b = Grid::new(20, 3);
        feed(&mut a, b"hello\r\nworld\r\nbye");
        feed(&mut b, b"hello\r\nWORLD\r\nbye");
        let d = a.diff_lines(&b);
        assert_eq!(d.changes.len(), 1);
        match &d.changes[0] {
            LineChange::Modified { row, before, after } => {
                assert_eq!(*row, 1);
                assert_eq!(before, "world");
                assert_eq!(after, "WORLD");
            }
            other => panic!("expected Modified, got {:?}", other),
        }
    }

    #[test]
    fn diff_lines_added_and_removed() {
        let mut a = Grid::new(20, 2);
        let mut b = Grid::new(20, 4);
        feed(&mut a, b"hi");
        feed(&mut b, b"hi\r\n\r\nadded1\r\nadded2");
        let d = a.diff_lines(&b);
        // a has 2 rows of which row 1 is empty in both -> only Added entries.
        let added: Vec<_> = d
            .changes
            .iter()
            .filter(|c| matches!(c, LineChange::Added { .. }))
            .collect();
        assert_eq!(added.len(), 2);

        let mut c = Grid::new(20, 4);
        feed(&mut c, b"hi\r\n\r\nlost1\r\nlost2");
        let mut shorter = Grid::new(20, 2);
        feed(&mut shorter, b"hi");
        let d2 = c.diff_lines(&shorter);
        let removed: Vec<_> = d2
            .changes
            .iter()
            .filter(|x| matches!(x, LineChange::Removed { .. }))
            .collect();
        assert_eq!(removed.len(), 2);
    }
}
