/**
 * How a runner `terminal-activity` digest updates session-state tracking
 * (plan `2026-07-28-runner-many-sessions-performance` Phase 5, root cause A4).
 *
 * ## Why this exists at all
 *
 * Before Phase 5, tracking for a tab with no mounted pane was kept alive by a
 * **proxy-ack**: the page-level output tap acked bytes on the tab's behalf, so
 * the runner's flow-control gate would not wedge at its high watermark and stop
 * emitting to a tab that nothing was render-acking. That was a deliberate
 * mechanism, not an accident — without it the tap went blind, and the tap is
 * what makes tracking mount-independent.
 *
 * Phase 5 removes the need for it by making the runner tier-aware: a tab no
 * pane declares is `unwatched` and, by default, gets no `terminal-output` at
 * all, so there is nothing left to ack. (A configured
 * `unwatched_flush_interval_ms` changes what such a tab RECEIVES — see the
 * note below — but not the ack story: a held tier is never ack-gated
 * either.)
 *
 * This module is the replacement feed. It must cover everything the tap's
 * byte stream used to supply for such a tab:
 *
 *  | tracking surface        | old (tap bytes)                | new (digest)                |
 *  |-------------------------|--------------------------------|-----------------------------|
 *  | staleness / last output | arrival time of a chunk        | arrival time of a digest    |
 *  | activity sparkline      | `text.length` of decoded bytes | `bytesDelta` from the PTY   |
 *  | `lastOutputLines`       | regex ANSI-strip of raw bytes  | the runner's RENDERED grid  |
 *  | state chip              | `detectSessionState(text)`     | same, on the rendered tail  |
 *
 * Two of those are strict upgrades. `bytesDelta` is what the PTY actually
 * produced, so an unwatched tab's sparkline stays comparable with a focused
 * tab's — the digest's own text length bears no relation to throughput. And the
 * rendered grid resolves cursor motion, line rewrites and full-frame TUI
 * redraws exactly the way xterm does, whereas the ANSI-strip fallback (which is
 * what an unmounted tab actually got, since there is no xterm buffer to read)
 * accumulated overdrawn frames as if they were new lines.
 *
 * One case produces no digests at all: an operator who sets the
 * `unwatched_flush_interval_ms` performance cap puts `unwatched` sessions back
 * on `terminal-output` at that cadence, and the runner then stands the digest
 * down for them (`TerminalSession::emit_activity_digest_if_due`) so the page
 * tap is not double-counted. Nothing here needs to change for it — this module
 * simply never runs — but "unwatched implies digest" is no longer the whole
 * story.
 *
 * And the tap is NOT an equal substitute in that configuration, which is worth
 * saying because #1084's own reasoning assumed it was: such a tab still has no
 * mounted xterm, so its output lines come from `outputLineTracking`'s
 * ANSI-strip fallback, i.e. the very path the table above calls a downgrade.
 * Everything else (sparkline, state chip, last-output time) the tap does serve
 * as well as it serves `background`.
 *
 * Pure leaf (no React, no Tauri) so the replacement can be tested against the
 * behavior it replaces — same rationale as `flowControl.ts`,
 * `scrollbackReplay.ts` and `paneVisibilityService.ts`.
 */

/** The tracking state a digest mutates, held as refs by the owning hook. */
export interface DigestTrackingState {
  /** tabId → epoch ms of the last observed output. Drives the staleness sweep. */
  lastOutputTime: Record<string, number>;
  /** tabId → sparkline ring; the LAST slot is the open 2 s bucket. */
  activityBuffers: Record<string, number[]>;
}

/** What the caller must publish after applying a digest. */
export interface DigestEffects {
  /**
   * Lines to publish as the tab's `lastOutputLines`, or `null` when the digest
   * carried none (a burst of pure control bytes) — in which case the previously
   * published lines must be left alone rather than blanked.
   */
  outputLines: string[] | null;
  /**
   * Text to run the session-state detector (and `processOutput`) over. Empty
   * when the digest carried no lines: there is nothing to match, and running
   * the detector on `""` would only risk a spurious transition.
   */
  detectorText: string;
}

/** How many trailing lines a tab's compact view shows. Mirrors the runner's
 *  `ACTIVITY_DIGEST_LINES`, so a well-behaved digest is never truncated here. */
export const DIGEST_OUTPUT_LINES = 20;

/**
 * Apply one digest to `state` (mutating it in place, as the hook's refs are)
 * and return what the caller must publish.
 *
 * `bytesDelta` is added to the OPEN sparkline bucket, exactly as the tap adds
 * `text.length` — the 2 s rotation that closes buckets is the caller's, so a
 * digest and a live chunk land in the same bucket for the same wall-clock
 * window regardless of which fed the tab.
 */
export function applyActivityDigest(
  state: DigestTrackingState,
  tabId: string,
  bytesDelta: number,
  lines: readonly string[],
  now: number,
): DigestEffects {
  state.lastOutputTime[tabId] = now;

  const buffers = state.activityBuffers;
  const buf = (buffers[tabId] ??= []);
  if (buf.length === 0) buf.push(0);
  buf[buf.length - 1] += bytesDelta;

  if (lines.length === 0) {
    return { outputLines: null, detectorText: "" };
  }
  const outputLines = lines.slice(-DIGEST_OUTPUT_LINES);
  return { outputLines, detectorText: outputLines.join("\n") };
}
