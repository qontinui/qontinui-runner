/**
 * Mid-session recovery for a webview-emission gap (flow-control drop), and
 * the catch-up path when a hidden pane is revealed. Extracted from
 * `TerminalInstance` as a leaf module (no xterm / Tauri imports) so its
 * retry, fallback and give-up paths can be driven by tests.
 *
 * Each pass fetches the scrollback ring, which also resets the runner's
 * flow-control counters and so resumes emission. It writes the slice the pane
 * missed, then drains held chunks only up to the first REMAINING hole
 * (`drainHeldChunks`). Post-hole chunks stay held so the next pass's
 * (advanced) ring window can cover the hole first: writing them early would
 * splice the hole out permanently. Retries are bounded. On exhaustion or fetch
 * failure the leftovers are written spliced rather than stranded (full-frame
 * TUI redraws self-heal, and the next remount's ring replay reconciles
 * scrollback).
 *
 * A request that arrives while a pass is in flight is QUEUED rather than
 * dropped: the running pass works from a ring snapshot taken before the
 * newly-missed bytes existed, so silently declining would leave a hole that no
 * later gap detection can see (`nextExpectedOffset` keeps advancing across
 * drops, so the stream looks contiguous afterwards).
 */

import type { ScrollbackRingWindow } from "./backends/types";
import type { PaneOutputStream } from "./paneOutputStream";
import {
  RESYNC_INCOMPLETE_MARKER,
  formatLostOutputMarker,
  lostWindowBytes,
  resyncSliceStart,
} from "./scrollbackReplay";

/**
 * Ring-refetch attempts one pass makes before giving up and marking the pane
 * spliced. Each attempt costs one ring read; the loop only re-runs when the
 * previous window fell short (a hole the fetch did not cover, or bytes dropped
 * while it was in flight), so the common case is exactly one.
 */
export const RESYNC_MAX_ATTEMPTS = 5;

/** The pane's backend, as far as a resync needs it. */
export interface ResyncBackend {
  /** Read the ring. Resets the runner's flow-control counters. */
  readScrollbackRing(): Promise<ScrollbackRingWindow | null>;
  write(data: Uint8Array | string): void;
}

/** What a resync needs from its pane, injected so tests can drive it. */
export interface RingResyncHost {
  /** The pane was torn down. */
  isDisposed(): boolean;
  /** The backend, or `null` while there is none. */
  backend(): ResyncBackend | null;
  /** Transport stats: a ring of `ringBytes` was read and `sliceBytes` replayed. */
  noteRingReplay(ringBytes: number, sliceBytes: number): void;
  /** `PaneVisibilityService.noteResynced`: whether this replay caught the pane up. */
  noteResynced(replayedThrough: number): boolean;
  /** `PaneVisibilityService.noteResyncFailed`. */
  noteResyncFailed(): void;
  /** Flush the acked write queue now. */
  flushStaged(): void;
  /** `terminal_flow_reset`, logged on failure. */
  flowReset(why: string): void;
  warn(message: string, error?: unknown): void;
}

export class RingResync {
  /** A resync was requested while one was already running. */
  private againRequested = false;

  constructor(
    private readonly stream: PaneOutputStream,
    private readonly host: RingResyncHost,
    private readonly maxAttempts: number = RESYNC_MAX_ATTEMPTS,
  ) {}

  /** Run one pass, or queue one more if a pass is already running. */
  async run(): Promise<void> {
    const { stream, host } = this;
    if (stream.resyncInFlight) {
      this.againRequested = true;
      return;
    }
    stream.resyncInFlight = true;
    // Set only on the one exit that PROVES the pane is caught up: a replay
    // that covered every dropped byte AND left no held chunk behind a hole.
    // Every other exit (a rejected fetch, an empty ring, a replay that stopped
    // short of bytes dropped while it was in flight, the retry budget running
    // out) falls through to the `finally`, which re-arms the pane and tells
    // the operator the pane is spliced.
    let caughtUp = false;
    // Whether any ring read came back. Each one reset the runner's counters;
    // if none did, the gate may still be paused, so the `finally` falls back
    // to a bare reset.
    let ringRead = false;
    try {
      for (let attempt = 0; attempt < this.maxAttempts; attempt++) {
        const source = host.backend();
        if (host.isDisposed() || !source) return;
        let ring: ScrollbackRingWindow | null;
        try {
          ring = await source.readScrollbackRing();
        } catch (e) {
          host.warn(`emission-gap resync fetch failed (attempt ${attempt + 1})`, e);
          continue;
        }
        const b = host.backend();
        if (host.isDisposed() || !b) return;
        if (!ring) {
          host.warn(`scrollback ring unavailable for resync (attempt ${attempt + 1})`);
          continue;
        }
        ringRead = true;
        const ringWindow = { startOffset: ring.startOffset, endOffset: ring.endOffset };
        // Ring overrun: the hole starts before the oldest byte the backend
        // still holds, so those bytes are gone from every source and no retry
        // can produce them. `resyncSliceStart` replays the whole ring, which
        // would join the pane's last written byte straight to a later one,
        // indistinguishable from continuous output. Say so in-band instead.
        const lost = lostWindowBytes(ringWindow, stream.writtenThrough);
        if (lost > 0) {
          host.warn(
            `scrollback ring overran the missed window; ${lost} bytes of output are unrecoverable`,
          );
          b.write(formatLostOutputMarker(lost));
        }
        const from = resyncSliceStart(ringWindow, stream.writtenThrough);
        const slice = ring.bytes.subarray(from);
        host.noteRingReplay(ring.bytes.length, slice.length);
        if (slice.length > 0) {
          // Direct write (not the coalesce queue): any pre-gap staged chunks
          // flushed in an earlier microtask, and the held post-gap chunks drain
          // AFTER this, so stream order holds.
          b.write(slice);
        }
        stream.applyResyncRing(ringWindow.endOffset);
        // Clears the pane's "behind" flag only if this ring window reached
        // every byte that was dropped. A chunk dropped while the fetch was in
        // flight sits beyond this (older) snapshot; the next attempt covers it.
        const covered = host.noteResynced(stream.replayedThrough);
        // Stages held chunks up to the first remaining hole, acked in full.
        const noneHeld = stream.drainHeld();
        if (covered && noneHeld) {
          caughtUp = true;
          return;
        }
        host.warn(`emission gap persisted across resync (attempt ${attempt + 1}); retrying`);
      }
    } catch (e) {
      host.warn("emission-gap resync failed", e);
    } finally {
      // Never strand held chunks: on success the loop drained them all; on
      // retry exhaustion or fetch failure, write what remains even though a
      // hole may precede it (spliced beats frozen).
      stream.releaseHeld();
      // A request that arrived mid-pass ran against a stale ring snapshot;
      // run exactly one more pass to cover what it could not see. That pass
      // is the retry, so don't declare the gap unrecovered yet.
      const disposed = host.isDisposed();
      const retryQueued = this.againRequested && !disposed;
      if (!caughtUp && !disposed && !retryQueued) {
        // Stay armed so the next reveal tries again, and surface it, because
        // what the operator is looking at right now has a hole in it.
        host.noteResyncFailed();
        host.warn("emission-gap resync did not complete; output is spliced");
        // Flush the leftovers staged above FIRST, so the marker lands after
        // the spliced output it describes, not in front of it.
        host.flushStaged();
        host.backend()?.write(RESYNC_INCOMPLETE_MARKER);
      }
      if (!ringRead && !disposed && !retryQueued) {
        // No ring read reset the counters. If the gate is paused, no chunk
        // will come to ask again, so reopen it without the replay.
        host.flowReset("resync fallback");
      }
      stream.resyncInFlight = false;
      if (retryQueued) {
        this.againRequested = false;
        void this.run();
      }
    }
  }
}
