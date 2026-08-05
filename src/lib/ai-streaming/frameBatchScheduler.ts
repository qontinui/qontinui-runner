/**
 * `FrameBatchScheduler` — coalesce many rapid "something changed" signals into
 * ONE callback per animation frame, with a wall-clock floor so the callback
 * still runs when `requestAnimationFrame` is suspended.
 *
 * This is the generic extraction of the pairing already used by the terminal
 * output tap (`TerminalOutputCoalescer` + the rAF/`setTimeout` flush loop in
 * `TerminalSessionContext`): rAF alone is NOT sufficient because WebView2
 * suspends animation frames in occluded/minimised windows, which would stall
 * an rAF-only batcher indefinitely. Both timers are armed together and the
 * first one to fire flushes and cancels the other.
 *
 * Leaf module: zero React / Tauri / DOM-type imports, so it unit-tests under
 * the runner's `environment: "node"` vitest config (the frame source degrades
 * to "timer only" when `requestAnimationFrame` is absent).
 */

/** Default wall-clock floor: flush at least this often even with no frames. */
export const DEFAULT_BATCH_FLOOR_MS = 100;

export interface FrameBatchSchedulerOptions {
  /**
   * Maximum delay between `schedule()` and the flush, even when animation
   * frames never fire (occluded window). Defaults to {@link DEFAULT_BATCH_FLOOR_MS}.
   */
  floorMs?: number;
  /**
   * Frame source. Return `null` to signal "no frame source available", in
   * which case the floor timer is the only driver. Defaults to the ambient
   * `requestAnimationFrame` when present.
   */
  requestFrame?: (callback: () => void) => number | null;
  /** Cancels a handle previously returned by `requestFrame`. */
  cancelFrame?: (handle: number) => void;
}

type FrameFns = {
  request: (callback: () => void) => number | null;
  cancel: (handle: number) => void;
};

function defaultFrameFns(): FrameFns {
  const g = globalThis as {
    requestAnimationFrame?: (cb: (t: number) => void) => number;
    cancelAnimationFrame?: (handle: number) => void;
  };
  if (typeof g.requestAnimationFrame === "function") {
    const raf = g.requestAnimationFrame.bind(g);
    const caf = g.cancelAnimationFrame?.bind(g);
    return {
      request: (cb) => raf(() => cb()),
      cancel: (handle) => caf?.(handle),
    };
  }
  return { request: () => null, cancel: () => {} };
}

export class FrameBatchScheduler {
  private readonly onFlush: () => void;
  private readonly floorMs: number;
  private readonly frame: FrameFns;

  private frameHandle: number | null = null;
  private timerHandle: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  constructor(onFlush: () => void, options: FrameBatchSchedulerOptions = {}) {
    this.onFlush = onFlush;
    this.floorMs = options.floorMs ?? DEFAULT_BATCH_FLOOR_MS;
    const fallback = defaultFrameFns();
    this.frame = {
      request: options.requestFrame ?? fallback.request,
      cancel: options.cancelFrame ?? fallback.cancel,
    };
  }

  /** True while a flush is pending. */
  get isScheduled(): boolean {
    return this.frameHandle !== null || this.timerHandle !== null;
  }

  /**
   * Request a flush. Idempotent: calling this N times before the flush fires
   * still produces exactly ONE callback.
   */
  schedule(): void {
    if (this.disposed || this.isScheduled) return;
    this.frameHandle = this.frame.request(() => {
      this.frameHandle = null;
      this.fire();
    });
    this.timerHandle = setTimeout(() => {
      this.timerHandle = null;
      this.fire();
    }, this.floorMs);
  }

  /** Drop any pending flush without running the callback. */
  cancel(): void {
    if (this.frameHandle !== null) {
      this.frame.cancel(this.frameHandle);
      this.frameHandle = null;
    }
    if (this.timerHandle !== null) {
      clearTimeout(this.timerHandle);
      this.timerHandle = null;
    }
  }

  /**
   * Run the callback immediately, cancelling any pending flush first.
   * Runs unconditionally (not only when scheduled) so callers can use it as a
   * "settle now" barrier before reading the batched state.
   */
  flushNow(): void {
    if (this.disposed) return;
    this.cancel();
    this.onFlush();
  }

  /** Permanently stop the scheduler. Further `schedule()` calls are no-ops. */
  dispose(): void {
    this.cancel();
    this.disposed = true;
  }

  private fire(): void {
    if (this.disposed) return;
    this.cancel();
    this.onFlush();
  }
}
