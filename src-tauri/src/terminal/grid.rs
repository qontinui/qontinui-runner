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
    /// DEC private mode flags we observe but do not gate on.
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
                self.cells.copy_within(src_start..src_start + cols, dst_start);
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
    pub cells: Vec<Cell>,
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

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
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
}
