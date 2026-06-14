/**
 * Dev-only terminal instrumentation hook.
 *
 * Exposes `window.__qontinuiTerminal` in the browser/webview console for
 * diagnosing rendering issues (the "text on top of text" / ghosting class).
 * Everything here is OFF by default and has zero cost on the hot path unless
 * `debugOverlap` is explicitly enabled from the console.
 *
 * History: Phase 0 of `plans/2026-06-13-terminal-rendering-robustness.md` also
 * shipped a `setIdleReconcile(false)` kill switch for the PERIODIC idle
 * reconciliation paint. Phase 2 then *retired* that periodic paint entirely
 * (the single-buffer Rust grid can't model the alt screen Claude's TUI lives
 * on, so the full-screen overlay could never be made safe). With the periodic
 * path gone the toggle would gate nothing, so it was removed rather than left
 * wired to dead code. The overlap instrumentation below stays — it now tracks
 * the surviving one-shot bootstrap paints.
 */

/** A single recorded paintGrid event for the console ring. */
export interface PaintGridLogEntry {
  terminalId: string;
  /** Which paint fired it: the mount-gap bootstrap or the reconnect bootstrap. */
  reason: "mount-bootstrap" | "reconnect-bootstrap";
  /** Whether the snapshot reported the alt screen (DEC ?1049h) was active. */
  altScreen: boolean;
  /** Whether the paint was actually applied (false = hard-skipped on alt-screen). */
  applied: boolean;
  /** Total bytes written to this pane via the live stream up to this point. */
  bytesWritten: number;
  ts: number;
}

interface QontinuiTerminalDebug {
  /**
   * When true, paintGrid bootstrap events are pushed to a console ring and
   * logged. Default false. Flip from the console:
   * `window.__qontinuiTerminal.debugOverlap = true`.
   */
  debugOverlap: boolean;
  /** Most recent paint events (newest last), capped at `ringSize`. */
  paintLog: PaintGridLogEntry[];
  /** Max entries retained in `paintLog`. */
  ringSize: number;
  /** Dump the ring to the console. */
  dump(): PaintGridLogEntry[];
}

const RING_SIZE = 200;

declare global {
  interface Window {
    __qontinuiTerminal?: QontinuiTerminalDebug;
  }
}

/** Lazily create + return the singleton debug object (no-op off the browser). */
export function getTerminalDebug(): QontinuiTerminalDebug | null {
  if (typeof window === "undefined") return null;
  if (!window.__qontinuiTerminal) {
    const log: PaintGridLogEntry[] = [];
    window.__qontinuiTerminal = {
      debugOverlap: false,
      paintLog: log,
      ringSize: RING_SIZE,
      dump() {
        // eslint-disable-next-line no-console
        console.table(this.paintLog);
        return this.paintLog;
      },
    };
  }
  return window.__qontinuiTerminal;
}

/**
 * Record a bootstrap paintGrid event. No-ops unless `debugOverlap` is on, so
 * it is safe to call unconditionally on the (rare) bootstrap path.
 */
export function recordPaintGrid(entry: PaintGridLogEntry): void {
  const dbg = getTerminalDebug();
  if (!dbg || !dbg.debugOverlap) return;
  dbg.paintLog.push(entry);
  while (dbg.paintLog.length > dbg.ringSize) dbg.paintLog.shift();
  // eslint-disable-next-line no-console
  console.debug(
    `[TerminalOverlap ${entry.terminalId}] ${entry.reason} altScreen=${entry.altScreen} applied=${entry.applied} bytes=${entry.bytesWritten}`,
  );
}
