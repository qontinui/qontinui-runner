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
