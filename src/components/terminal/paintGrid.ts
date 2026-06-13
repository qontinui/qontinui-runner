/**
 * paintGrid — render a Rust-side `GridSnapshot` into an `ITerminalBackend`.
 *
 * Used by the live-first bootstrap as a "catch-up" mechanism, NOT as the
 * source of truth. The live `terminal-output` listener is authoritative for
 * forward motion; paintGrid fills the gap between mount and listener attach
 * (those bytes are dropped by Tauri but live in the Rust grid). The periodic
 * idle re-sync was retired in Phase 2 — see the `paintGrid` doc below.
 *
 * Design constraints:
 *   - DECAWM-off (`\x1b[?7l` / `\x1b[?7h`) wraps the whole paint to prevent
 *     trailing-column auto-wrap when backend.cols < snapshot.cols.
 *   - Absolute CUP per row (`\x1b[<r+1>;1H`) is idempotent — safe to write
 *     at any time without corrupting the live cursor's prior state.
 *   - Wide-2 cells: emit the LEFT half (which carries the actual char)
 *     and SKIP the RIGHT-spacer cell (xterm advances the cursor for us).
 *   - SGR delta tracking minimises escape-sequence noise — we only emit
 *     fg/bg/attrs that differ from the previously-painted cell.
 *   - Single `backend.write()` at the end — assembled into one string.
 *
 * See `plans/terminal-grid-bootstrap-redesign.md` Layers 2 + 3.
 */

import type { ITerminalBackend } from "./backends";

// ---- Wire format (must match `qontinui-runner/src-tauri/src/terminal/grid.rs`)

/**
 * High byte 0x01 marks the "default" color sentinel (matches Rust
 * `COLOR_DEFAULT = 0x0100_0000`). Low 24 bits hold packed RGB (0xRRGGBB).
 */
export const COLOR_DEFAULT = 0x0100_0000;

/** Attribute bitfield — keep in sync with `grid.rs`. */
export const ATTR_BOLD = 1 << 0;
export const ATTR_ITALIC = 1 << 1;
export const ATTR_UNDERLINE = 1 << 2;
export const ATTR_INVERSE = 1 << 3;
export const ATTR_DIM = 1 << 4;
export const ATTR_STRIKE = 1 << 5;
export const ATTR_WIDE_LEFT = 1 << 6;
export const ATTR_WIDE_RIGHT = 1 << 7;

/** Cell as serialised by the Rust `Cell` struct (note serde renames). */
export interface GridCell {
  /** Rust `ch` → JSON `c`. */
  c: string;
  /** Foreground color: COLOR_DEFAULT or 0x00RRGGBB. */
  fg: number;
  /** Background color: COLOR_DEFAULT or 0x00RRGGBB. */
  bg: number;
  /** Rust `attrs` → JSON `a` (bitfield of ATTR_*). */
  a: number;
}

export interface GridCursor {
  row: number;
  col: number;
  visible: boolean;
}

export interface GridSnapshot {
  cols: number;
  rows: number;
  cursor: GridCursor;
  /** Optional OSC 0/2 title. */
  title?: string;
  /**
   * True when the Rust grid is on the alternate screen (DEC `?1049h`). The
   * grid is single-buffer, so a snapshot taken in this state holds alt-screen
   * content; the bootstrap paint hard-skips when this is true rather than
   * overlay it onto a freshly-mounted xterm. Always present from the current
   * runner build; optional so an older build (no field) degrades to `false`.
   */
  alt_screen?: boolean;
  /** Row-major: cells[row * cols + col]. Length === rows * cols. */
  cells: GridCell[];
}

// ---- Internals -----------------------------------------------------------

/** Tracks the SGR pen state we last emitted, so we can diff against the next cell. */
interface PenState {
  fg: number;
  bg: number;
  attrs: number;
}

/** Sentinel "uninitialised" pen — forces an initial SGR emission. */
const PEN_UNSET: PenState = { fg: -1, bg: -1, attrs: -1 };

function appendSgrColor(out: string[], code: 38 | 48, color: number): void {
  if (color === COLOR_DEFAULT) {
    // 39 = default fg, 49 = default bg.
    out.push(`\x1b[${code === 38 ? 39 : 49}m`);
    return;
  }
  const r = (color >> 16) & 0xff;
  const g = (color >> 8) & 0xff;
  const b = color & 0xff;
  out.push(`\x1b[${code};2;${r};${g};${b}m`);
}

/** Emit minimal SGR escape(s) to transition `prev` → `next`. */
function appendSgrDelta(out: string[], prev: PenState, next: PenState): void {
  const fgChanged = prev.fg !== next.fg;
  const bgChanged = prev.bg !== next.bg;
  const attrsChanged = prev.attrs !== next.attrs;

  if (!fgChanged && !bgChanged && !attrsChanged) {
    return;
  }

  // If any attr bit was CLEARED (prev had it, next doesn't), the safest
  // thing to do is reset the pen and re-emit everything fresh — there's no
  // single CSI to clear an arbitrary subset, and Layer 3 prioritises
  // correctness over byte-count.
  const cleared = prev.attrs & ~next.attrs;
  if (cleared !== 0 || prev === PEN_UNSET) {
    out.push("\x1b[0m");
    // Reset baseline forces re-emission of fg/bg/attrs below.
    prev = { fg: COLOR_DEFAULT, bg: COLOR_DEFAULT, attrs: 0 };
  }

  // Emit set-only attrs (next has bits prev didn't).
  const added = next.attrs & ~prev.attrs;
  if (added & ATTR_BOLD) out.push("\x1b[1m");
  if (added & ATTR_DIM) out.push("\x1b[2m");
  if (added & ATTR_ITALIC) out.push("\x1b[3m");
  if (added & ATTR_UNDERLINE) out.push("\x1b[4m");
  if (added & ATTR_INVERSE) out.push("\x1b[7m");
  if (added & ATTR_STRIKE) out.push("\x1b[9m");

  if (next.fg !== prev.fg) appendSgrColor(out, 38, next.fg);
  if (next.bg !== prev.bg) appendSgrColor(out, 48, next.bg);
}

/** Replace control characters that would corrupt the terminal stream. */
function safeChar(ch: string): string {
  if (ch.length === 0) return " ";
  const code = ch.codePointAt(0) ?? 0x20;
  // Allow normal printable codepoints (>= 0x20) and selected whitespace.
  // Tab (0x09) is intentionally dropped — paintGrid uses absolute CUP, so a
  // raw tab inside a cell would mis-advance the cursor.
  if (code < 0x20 || code === 0x7f) {
    return " ";
  }
  return ch;
}

// ---- Public API ----------------------------------------------------------

/**
 * Paint a `GridSnapshot` into `backend`. Issues a single `backend.write()`.
 *
 * DECAWM-off + absolute CUP per row makes this an idempotent overlay. As of
 * Phase 2 (`plans/2026-06-13-terminal-rendering-robustness.md`) this is used
 * ONLY for the two one-shot bootstraps (mount-gap + reconnect) in
 * `TerminalInstance`, and the caller hard-skips it when the snapshot reports
 * the alt screen is active. The periodic idle reconciliation that re-painted
 * concurrently with the live stream was retired — the single-buffer grid
 * cannot model the alt screen, so a concurrent full-screen overlay could not
 * be made safe for a TUI like Claude.
 */
export function paintGrid(backend: ITerminalBackend, snapshot: GridSnapshot): void {
  const { cols, rows, cells, cursor } = snapshot;
  if (cols <= 0 || rows <= 0 || cells.length < cols * rows) {
    return;
  }

  const out: string[] = [];

  // 1. Disable autowrap for the duration of the paint. Re-enabled in step 4.
  out.push("\x1b[?7l");

  // 2. Per-row paint with SGR delta tracking. Reset pen at the start of
  //    every row so the row is self-contained — a partial paint that gets
  //    overwritten by a live byte stream still leaves the pen in a known
  //    state at row boundaries.
  for (let r = 0; r < rows; r++) {
    // Absolute CUP — CSI is 1-based.
    out.push(`\x1b[${r + 1};1H`);
    let pen: PenState = PEN_UNSET;

    for (let c = 0; c < cols; c++) {
      const cell = cells[r * cols + c];
      if (!cell) continue;

      // Skip the RIGHT-spacer of a wide-2 cell — its left half already
      // wrote the glyph and the parser advanced the cursor by 2.
      if ((cell.a & ATTR_WIDE_RIGHT) !== 0) {
        continue;
      }

      const next: PenState = { fg: cell.fg, bg: cell.bg, attrs: cell.a };
      appendSgrDelta(out, pen, next);
      pen = next;

      out.push(safeChar(cell.c));
    }

    // End-of-row pen reset so any trailing garbage doesn't bleed into the
    // next row's CUP+content emission below.
    out.push("\x1b[0m");
  }

  // 3. Restore cursor to the snapshot's position + visibility.
  const cr = Math.min(Math.max(cursor.row, 0), rows - 1) + 1;
  const cc = Math.min(Math.max(cursor.col, 0), cols - 1) + 1;
  out.push(`\x1b[${cr};${cc}H`);
  out.push(cursor.visible ? "\x1b[?25h" : "\x1b[?25l");

  // 4. Re-enable autowrap.
  out.push("\x1b[?7h");

  backend.write(out.join(""));
}
