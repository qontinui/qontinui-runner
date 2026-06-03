/**
 * wheelScroll — pure helpers for the Shift+wheel "force local scrollback"
 * gesture.
 *
 * Why this exists: xterm.js disables its own viewport wheel-scrolling whenever
 * the foreground program enables a mouse-tracking protocol that reports wheel
 * events (DECSET 1000/1002/1003 → the VT200/DRAG/ANY protocols, all of which
 * include `CoreMouseEventType.WHEEL`). See `@xterm/xterm` `Viewport.ts`:
 *
 *   coreMouseService.onProtocolChange(type =>
 *     scrollableElement.updateOptions({ handleMouseWheel: !(type & WHEEL) }))
 *
 * In that state a plain wheel event is forwarded to the program instead of
 * scrolling the local scrollback — so terminals running a mouse-mode TUI
 * (an AI session, lazygit, htop, `less --mouse`, vim with `set mouse=a`, …)
 * appear "stuck" while plain-shell terminals scroll fine. xterm's
 * `attachCustomWheelEventHandler` can't help here because the wheel listener
 * `return`s early (`if (requestedEvents.wheel) return;`) before the custom
 * handler runs once mouse mode is active.
 *
 * The fix (wired in `TerminalInstance`) is a capture-phase `wheel` listener on
 * the terminal container that, when Shift is held, takes the event over and
 * scrolls the xterm scrollback directly via `backend.scrollLines()`. This
 * matches the long-standing "modifier bypasses application mouse reporting"
 * convention in Konsole / GNOME Terminal / Windows Terminal, giving the
 * operator a reliable "always scroll the terminal" gesture regardless of what
 * the inner program does with the mouse.
 *
 * This module is a leaf (no xterm / DOM imports beyond the `WheelEvent` shape)
 * so vitest can exercise the delta math without loading the WASM backend —
 * same rationale as `consumeInputChunk.ts`.
 */

/**
 * Approximate cell height in CSS px, used to convert pixel-mode wheel deltas
 * (the common case in WebView2 / Chromium) into a line count. Derived from the
 * shared TERMINAL_OPTIONS (`fontSize: 14` × `lineHeight: 1.2` ≈ 16.8). A small
 * error here only affects scroll speed, never correctness — the fractional
 * accumulator in the caller absorbs the remainder.
 */
export const DEFAULT_CELL_HEIGHT_PX = 17;

/** The subset of `WheelEvent` the delta math depends on. */
export interface WheelLike {
  deltaY: number;
  /** Horizontal delta — carries the value when Shift+wheel is remapped to
   * horizontal scroll (Windows/Chromium). Used only when `deltaY` is 0. */
  deltaX: number;
  /** 0 = pixels (DOM_DELTA_PIXEL), 1 = lines, 2 = pages. */
  deltaMode: number;
}

/**
 * Convert a wheel event into a signed scrollback line delta.
 *
 * Sign convention matches `Terminal.scrollLines(amount)`: positive scrolls
 * DOWN (toward newest output), negative scrolls UP (into history) — i.e. the
 * result carries the same sign as `deltaY`.
 *
 * Returns a FRACTIONAL value on purpose: pixel-mode deltas from a trackpad are
 * often well under one line, and the caller accumulates the remainder across
 * events so slow scrolling still advances smoothly instead of snapping a whole
 * line per tick (or, with naive rounding, never moving at all).
 *
 * Critically for this codebase's runtime (Tauri WebView2 / Chromium on
 * Windows): holding Shift while turning the wheel is remapped by the browser
 * to HORIZONTAL scroll, so the event arrives with `deltaY === 0` and the
 * magnitude in `deltaX`. Since the override gesture is *specifically*
 * Shift+wheel, we fold `deltaX` in (preferring `deltaY` when present, e.g. on
 * macOS / trackpads) so it works on every platform.
 *
 * @param e            wheel event (`deltaY`/`deltaX`/`deltaMode` are read)
 * @param rows         current terminal row count (for page-mode deltas)
 * @param cellHeightPx px height of one row, for pixel-mode deltas
 */
export function wheelToLineDelta(
  e: WheelLike,
  rows: number,
  cellHeightPx: number = DEFAULT_CELL_HEIGHT_PX,
): number {
  // Prefer the vertical delta; fall back to the horizontal one for the
  // Windows/Chromium Shift+wheel→horizontal-scroll remapping (see above).
  const raw = e.deltaY !== 0 ? e.deltaY : e.deltaX;
  if (!raw || !Number.isFinite(raw)) return 0;
  const px = cellHeightPx > 0 ? cellHeightPx : DEFAULT_CELL_HEIGHT_PX;
  switch (e.deltaMode) {
    case 1: // DOM_DELTA_LINE — delta already counts lines.
      return raw;
    case 2: // DOM_DELTA_PAGE — scroll (nearly) a full screen, keeping 1 row of overlap.
      return raw * Math.max(1, rows - 1);
    default: // DOM_DELTA_PIXEL (0) and anything unknown — convert px → lines.
      return raw / px;
  }
}
