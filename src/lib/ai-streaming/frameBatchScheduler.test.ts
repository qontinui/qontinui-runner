/**
 * Tests for `FrameBatchScheduler` — the rAF + `setTimeout`-floor batcher behind
 * `useAiSession`'s streaming appends (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5a).
 *
 * The headline property under test is "N streamed lines produce ONE render
 * pass": the hook calls `schedule()` per line, and exactly one flush callback
 * must run per batch window.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { DEFAULT_BATCH_FLOOR_MS, FrameBatchScheduler } from "./frameBatchScheduler";
import { StreamingTextBuffer } from "./streamingBuffer";

/** Controllable stand-in for `requestAnimationFrame`. */
function makeFrameSource() {
  const pending = new Map<number, () => void>();
  let nextHandle = 1;
  return {
    request: (cb: () => void) => {
      const handle = nextHandle++;
      pending.set(handle, cb);
      return handle;
    },
    cancel: (handle: number) => {
      pending.delete(handle);
    },
    /** Fire every queued frame callback, as the browser would. */
    tick: () => {
      const callbacks = [...pending.values()];
      pending.clear();
      for (const cb of callbacks) cb();
    },
    get pendingCount() {
      return pending.size;
    },
  };
}

describe("FrameBatchScheduler", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("collapses N schedule() calls into ONE flush", () => {
    const frames = makeFrameSource();
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    for (let i = 0; i < 500; i++) {
      scheduler.schedule();
    }
    expect(flush).not.toHaveBeenCalled();

    frames.tick();
    expect(flush).toHaveBeenCalledTimes(1);

    // The floor timer must have been cancelled by the frame flush.
    vi.advanceTimersByTime(DEFAULT_BATCH_FLOOR_MS * 5);
    expect(flush).toHaveBeenCalledTimes(1);
  });

  it("still flushes on the wall-clock floor when frames are suspended", () => {
    // Occluded / minimised WebView2: rAF never fires. An rAF-only batcher would
    // stall streaming here — this is why the floor timer exists.
    const frames = makeFrameSource();
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      floorMs: 100,
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.schedule();
    scheduler.schedule();

    vi.advanceTimersByTime(99);
    expect(flush).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(flush).toHaveBeenCalledTimes(1);
    // The frame request must have been cancelled by the timer flush.
    expect(frames.pendingCount).toBe(0);
  });

  it("works with no frame source at all (timer-only degradation)", () => {
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      floorMs: 100,
      requestFrame: () => null,
    });

    scheduler.schedule();
    vi.advanceTimersByTime(100);
    expect(flush).toHaveBeenCalledTimes(1);
  });

  it("re-arms for the next batch after a flush", () => {
    const frames = makeFrameSource();
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    scheduler.schedule();
    frames.tick();
    scheduler.schedule();
    frames.tick();

    expect(flush).toHaveBeenCalledTimes(2);
  });

  it("cancel() drops a pending flush; flushNow() runs it immediately", () => {
    const frames = makeFrameSource();
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    scheduler.schedule();
    expect(scheduler.isScheduled).toBe(true);
    scheduler.cancel();
    expect(scheduler.isScheduled).toBe(false);
    frames.tick();
    vi.advanceTimersByTime(1000);
    expect(flush).not.toHaveBeenCalled();

    scheduler.schedule();
    scheduler.flushNow();
    expect(flush).toHaveBeenCalledTimes(1);
    frames.tick();
    vi.advanceTimersByTime(1000);
    expect(flush).toHaveBeenCalledTimes(1);
  });

  it("dispose() stops all further work", () => {
    const frames = makeFrameSource();
    const flush = vi.fn();
    const scheduler = new FrameBatchScheduler(flush, {
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    scheduler.schedule();
    scheduler.dispose();
    frames.tick();
    vi.advanceTimersByTime(1000);
    scheduler.schedule();
    frames.tick();
    vi.advanceTimersByTime(1000);

    expect(flush).not.toHaveBeenCalled();
  });

  it("batched appends render the complete, correctly ordered buffer once", () => {
    // End-to-end shape of the hook: append synchronously, schedule per line,
    // one flush per frame reads the whole buffer.
    const frames = makeFrameSource();
    const buffer = new StreamingTextBuffer();
    const rendered: string[] = [];
    const scheduler = new FrameBatchScheduler(() => rendered.push(buffer.text), {
      requestFrame: frames.request,
      cancelFrame: frames.cancel,
    });

    const lines = Array.from({ length: 200 }, (_, i) => `line ${i}`);
    for (const line of lines) {
      buffer.append(line + "\n");
      scheduler.schedule();
    }
    frames.tick();

    expect(rendered).toHaveLength(1);
    expect(rendered[0]).toBe(lines.map((l) => l + "\n").join(""));
  });
});
