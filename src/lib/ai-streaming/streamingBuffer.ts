/**
 * `StreamingTextBuffer` — the accumulator behind a live AI response.
 *
 * Two problems this solves, both from plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5:
 *
 *  1. **Unbounded growth.** The old `streamingBufferRef` was only ever reset to
 *     empty, so one long turn grew the string without limit. This buffer keeps
 *     a hard retention ceiling and, on overflow, drops from the OLDEST end and
 *     records how much it dropped, so the loss is reported rather than silent.
 *  2. **Quadratic re-render.** Rendering the whole accumulated buffer through a
 *     markdown parser on every line is O(n²). {@link computeStreamWindow}
 *     produces a bounded TAIL of the buffer for display; the full retained text
 *     stays available for callers that need it (message completion, spec
 *     extraction) and for "show earlier".
 *
 * Appends are staged in an array and joined lazily so N appends cost O(total)
 * rather than O(total²).
 *
 * Leaf module: no React / DOM imports, unit-testable under `environment: "node"`.
 */

/**
 * Hard retention ceiling for a single streamed response. ~2M chars ≈ 4 MB of
 * UTF-16. Beyond this the oldest text is dropped (and counted) so a runaway
 * turn cannot exhaust the renderer.
 */
export const STREAM_RETAIN_MAX_CHARS = 2_000_000;

/** After an overflow, retain this much of the tail (hysteresis: trim in slabs). */
export const STREAM_RETAIN_TRIM_TO_CHARS = 1_500_000;

/** Default visible tail: how much of a live response is rendered by default. */
export const STREAM_TAIL_CHARS = 4096;

/** How much more becomes visible per "show earlier" click. */
export const STREAM_EARLIER_STEP_CHARS = 65_536;

/** Result of narrowing a buffer to a display window. */
export interface StreamWindow {
  /** The tail of the retained text that should be rendered. */
  visible: string;
  /** Retained characters that exist but are outside the window. */
  hiddenChars: number;
  /** Characters permanently dropped by the retention cap. */
  droppedChars: number;
  /** True when the whole retained buffer is visible. */
  atStart: boolean;
}

/**
 * Narrow `text` to its last `windowChars` characters.
 *
 * `windowChars <= 0` or `>= text.length` yields the whole string. `droppedChars`
 * is passed through so a caller can distinguish "earlier text you can still
 * reveal" (`hiddenChars`) from "earlier text that no longer exists"
 * (`droppedChars`).
 */
export function computeStreamWindow(
  text: string,
  windowChars: number,
  droppedChars = 0,
): StreamWindow {
  if (windowChars <= 0 || windowChars >= text.length) {
    return { visible: text, hiddenChars: 0, droppedChars, atStart: droppedChars === 0 };
  }
  const start = text.length - windowChars;
  return {
    visible: text.slice(start),
    hiddenChars: start,
    droppedChars,
    atStart: false,
  };
}

/**
 * Render a retained response for the transcript, prefixing the retention-cap
 * notice when the head was trimmed.
 *
 * Every path that moves text out of the streaming buffer and into the message
 * list goes through this — the normal ready/closed completion AND the abandon
 * paths (a new turn starting over an in-flight one). Otherwise the abandon
 * paths reset the buffer and take the trim record with them, so a response the
 * operator can see is incomplete carries nothing saying why.
 */
export function formatRetainedResponse(text: string, droppedChars: number): string {
  if (droppedChars <= 0) return text;
  return (
    `_[${droppedChars} earlier characters of this response were trimmed by ` +
    `the streaming retention cap]_\n\n${text}`
  );
}

/** A streaming buffer's observable size, as a view sees it between renders. */
export interface StreamProgress {
  /** Retained characters currently in the buffer. */
  length: number;
  /** Characters permanently dropped by the retention cap so far. */
  droppedChars: number;
}

/**
 * Should a "show earlier / show all" window collapse back to the default tail?
 *
 * A view can only tell turns apart by watching the buffer shrink — but there
 * are TWO ways it shrinks, and only one of them is a new turn:
 *
 *  - **New turn.** The hook resets the buffer, so both the length and the
 *    dropped count go to zero. Collapse: the widened window belonged to the
 *    previous response.
 *  - **Retention-cap trim.** The cap fires at 2,000,001 chars and trims back to
 *    1,500,000 — a 500k SHRINK mid-stream, with the dropped count rising by
 *    exactly that much. Collapsing here yanks a reader who clicked "Show all"
 *    back to the 4096-char tail, in the middle of the response they asked to
 *    see, for a reason that has nothing to do with them.
 *
 * The rising dropped count is what separates the two.
 */
export function shouldResetStreamWindow(previous: StreamProgress, next: StreamProgress): boolean {
  if (next.droppedChars > previous.droppedChars) return false;
  return next.length < previous.length;
}

export interface StreamingTextBufferOptions {
  /** Retention ceiling. Defaults to {@link STREAM_RETAIN_MAX_CHARS}. */
  maxChars?: number;
  /** Tail size kept after an overflow. Defaults to {@link STREAM_RETAIN_TRIM_TO_CHARS}. */
  trimToChars?: number;
}

export class StreamingTextBuffer {
  private readonly maxChars: number;
  private readonly trimToChars: number;

  /** Materialised text. Always the authoritative retained content after `text`. */
  private materialised = "";
  /** Appends not yet folded into `materialised`. */
  private pending: string[] = [];
  private pendingLength = 0;
  private dropped = 0;

  constructor(options: StreamingTextBufferOptions = {}) {
    this.maxChars = options.maxChars ?? STREAM_RETAIN_MAX_CHARS;
    const requestedTrim = options.trimToChars ?? STREAM_RETAIN_TRIM_TO_CHARS;
    // Trim target must be strictly below the ceiling or every append re-trims.
    this.trimToChars = Math.min(requestedTrim, Math.max(0, this.maxChars - 1));
  }

  /** Append one chunk. O(1); the join is deferred to the next read. */
  append(chunk: string): void {
    if (!chunk) return;
    this.pending.push(chunk);
    this.pendingLength += chunk.length;
  }

  /** Total retained characters (excludes anything dropped by the cap). */
  get length(): number {
    this.materialise();
    return this.materialised.length;
  }

  /** True when nothing has been appended since the last {@link reset}. */
  get isEmpty(): boolean {
    return this.materialised.length === 0 && this.pendingLength === 0;
  }

  /** Characters permanently dropped by the retention cap since the last reset. */
  get droppedChars(): number {
    this.materialise();
    return this.dropped;
  }

  /** The full retained text, oldest-first. Folds pending appends and applies the cap. */
  get text(): string {
    this.materialise();
    return this.materialised;
  }

  /** Narrow the retained text to a display window. */
  window(windowChars: number): StreamWindow {
    return computeStreamWindow(this.text, windowChars, this.dropped);
  }

  /** Forget everything, including the dropped-character count. */
  reset(): void {
    this.materialised = "";
    this.pending = [];
    this.pendingLength = 0;
    this.dropped = 0;
  }

  /** Fold staged appends into the materialised text and apply the cap. */
  private materialise(): void {
    if (this.pendingLength === 0) return;
    this.materialised =
      this.materialised === "" && this.pending.length === 1
        ? this.pending[0]
        : this.materialised + this.pending.join("");
    this.pending = [];
    this.pendingLength = 0;
    this.enforceCap();
  }

  private enforceCap(): void {
    if (this.materialised.length <= this.maxChars) return;
    const drop = this.materialised.length - this.trimToChars;
    this.materialised = this.materialised.slice(drop);
    this.dropped += drop;
  }
}
