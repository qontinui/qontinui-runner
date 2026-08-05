/**
 * scrollbackReplay — pure helper for deduping live `terminal-output` chunks
 * against a scrollback-ring replay.
 *
 * Why this exists: `TerminalInstance` remounts whenever the zone layout
 * reshapes (maximize / single-view toggle, layout switch, hidden↔assigned
 * reclassification) and on page reload. Each remount discards the xterm
 * buffer, and the grid bootstrap (`terminal_get_grid` + `paintGrid`) restores
 * only the visible rows×cols screen — so a remounted pane had an EMPTY
 * scrollback until new output accumulated. The fix replays the Rust-side
 * 1 MB scrollback ring (`terminal_get_scrollback`) into the fresh xterm
 * before any live bytes land.
 *
 * The ring snapshot covers absolute stream offsets `[startOffset, endOffset)`.
 * Live `terminal-output` events are stamped with their chunk's absolute start
 * `offset` (see the reader thread in `src-tauri/src/terminal/session.rs`), so
 * any byte below `endOffset` is already in the buffer via the replay. This
 * helper trims a chunk to its not-yet-replayed suffix — exact, no heuristics.
 *
 * Leaf module (no xterm / Tauri imports) so vitest can exercise the boundary
 * math without loading the backends — same rationale as `wheelScroll.ts`.
 */

/** A live or buffered output chunk plus its absolute stream offset. */
export interface OffsetChunk {
  bytes: Uint8Array;
  /**
   * Absolute byte offset of `bytes[0]` in the session's output stream.
   * `undefined` for events from a runner build that predates offset
   * stamping — those are written unconditionally (pre-fix behavior).
   */
  offset?: number;
}

/**
 * Return the portion of `chunk` NOT yet covered by a ring replay that ends at
 * `replayedThrough`, or `null` if the chunk is entirely covered.
 *
 * - No `offset` on the chunk → no provenance → return it whole.
 * - Entirely below the boundary → `null` (already written via the ring).
 * - Straddling the boundary → the unreplayed suffix.
 */
export function trimReplayedChunk(chunk: OffsetChunk, replayedThrough: number): Uint8Array | null {
  const { bytes, offset } = chunk;
  if (offset === undefined || replayedThrough <= 0) return bytes;
  const end = offset + bytes.length;
  if (end <= replayedThrough) return null;
  if (offset >= replayedThrough) return bytes;
  return bytes.subarray(replayedThrough - offset);
}

/** The absolute stream window `[startOffset, endOffset)` a ring snapshot covers. */
export interface RingWindow {
  startOffset: number;
  endOffset: number;
}

/**
 * Detect a webview-emission gap in the live `terminal-output` stream.
 *
 * The Rust reader gates webview emission (NOT the PTY read) on flow-control
 * backpressure: when the unacked gap exceeds the high watermark, chunks are
 * dropped from the event stream and the next delivered chunk starts beyond
 * the end of the previous one. `expectedNextOffset` is the exclusive end of
 * the last chunk this instance observed; `null` means nothing observed yet
 * (no baseline → no gap), and an unstamped chunk (`undefined` offset, from a
 * pre-offset-stamping runner build) can never signal a gap.
 */
export function isEmissionGap(
  expectedNextOffset: number | null,
  chunkOffset: number | undefined,
): boolean {
  if (expectedNextOffset === null || chunkOffset === undefined) return false;
  return chunkOffset > expectedNextOffset;
}

/**
 * Byte index into a ring snapshot's data at which a mid-session resync write
 * should start, given that the xterm buffer already holds content through
 * absolute offset `writtenThrough`.
 *
 * - Hole predates the ring (`writtenThrough` below the ring start): the
 *   missed bytes are gone (ring capacity exceeded) — start at 0 and replay
 *   the whole ring; full-frame TUI redraws self-heal the visual state.
 * - Ring holds nothing new: returns the ring length (empty slice).
 */
export function resyncSliceStart(ring: RingWindow, writtenThrough: number): number {
  if (writtenThrough <= ring.startOffset) return 0;
  if (writtenThrough >= ring.endOffset) return ring.endOffset - ring.startOffset;
  return writtenThrough - ring.startOffset;
}

/**
 * How many bytes a resync can NEVER recover: the span between what this pane
 * has written and the oldest byte the backend's ring still holds.
 *
 * When a pane stays hidden (or a flow-control drop persists) longer than the
 * ring's capacity, the head of the missed window has already rolled out of the
 * ring. `resyncSliceStart` then replays the whole ring — correct, but it joins
 * the pane's last written byte to a LATER one with nothing marking the seam.
 * The caller uses this count to write an explicit marker instead, because a
 * silent join is indistinguishable from continuous output.
 *
 * `writtenThrough <= 0` means this instance has never written a stamped byte
 * (cold mount): the ring starting above 0 is just session history, not loss.
 */
export function lostWindowBytes(ring: RingWindow, writtenThrough: number): number {
  if (writtenThrough <= 0) return 0;
  if (writtenThrough >= ring.startOffset) return 0;
  return ring.startOffset - writtenThrough;
}

/**
 * The in-band notice written into the pane when {@link lostWindowBytes} is
 * non-zero. Plain ASCII on purpose — an emoji or box-drawing glyph renders at
 * an ambiguous cell width and can shear the line on a narrow pane.
 */
export function formatLostOutputMarker(lostBytes: number): string {
  return (
    `\r\n\x1b[1;33m[qontinui] ${lostBytes} bytes of output were lost here — ` +
    `the scrollback ring rolled past them while this pane was hidden\x1b[0m\r\n`
  );
}

/**
 * The in-band notice written into the pane when a resync gives up without
 * having covered the whole missed window (retries exhausted, or the ring
 * fetch kept failing). The pane is left ARMED, so a later reveal retries — but
 * what is on screen right now is spliced and must say so.
 */
export const RESYNC_INCOMPLETE_MARKER =
  "\r\n\x1b[1;33m[qontinui] output resync did not complete — some output may be " +
  "missing above\x1b[0m\r\n";

/** Result of draining held post-gap chunks after a ring resync write. */
export interface HeldDrainResult {
  /** Trimmed, non-empty chunks safe to write now, in stream order. */
  writable: Uint8Array[];
  /** `writtenThrough` advanced past the writable chunks. */
  writtenThrough: number;
  /**
   * Chunks that must stay held: the first of them starts beyond
   * `writtenThrough`, i.e. a hole still precedes it — writing it (or
   * anything after it) would splice the hole out permanently. The caller
   * refetches the ring to cover the hole and drains again.
   */
  reheld: OffsetChunk[];
}

/**
 * Drain chunks held during an emission-gap resync, WITHOUT crossing a hole.
 *
 * Chunks are consumed in order. Each contiguous chunk (starting at or below
 * `writtenThrough`) is trimmed against `replayedThrough` and queued for
 * write; the first chunk that starts beyond `writtenThrough` — a hole the
 * ring fetch did not cover (e.g. a second gap opened during the fetch) —
 * stops the drain, and it plus everything after it is returned as `reheld`
 * so a retry can cover the hole first. Unstamped chunks (no offset) can
 * never witness a hole and are written as-is.
 */
export function drainHeldChunks(
  held: OffsetChunk[],
  writtenThrough: number,
  replayedThrough: number,
): HeldDrainResult {
  const writable: Uint8Array[] = [];
  let through = writtenThrough;
  for (let i = 0; i < held.length; i++) {
    const c = held[i];
    if (c.offset !== undefined && c.offset > through) {
      return { writable, writtenThrough: through, reheld: held.slice(i) };
    }
    const slice = trimReplayedChunk(c, replayedThrough);
    if (slice && slice.length > 0) writable.push(slice);
    if (c.offset !== undefined) through = Math.max(through, c.offset + c.bytes.length);
  }
  return { writable, writtenThrough: through, reheld: [] };
}
