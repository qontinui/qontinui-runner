/**
 * terminalScrollback — reading a terminal backend's buffer back as plain text.
 *
 * ## Why this is its own module
 *
 * `TerminalInstance` has TWO callers for this: the imperative
 * `TerminalInstanceHandle.getScrollback` and the `getScrollback` UI Bridge
 * custom action, 1200 lines apart. They were byte-identical copies — the same
 * duplication that let `sendKeys` be hardened on the mounted pane path and left
 * raw on the proxy path.
 *
 * Collapsing them to one function inside `TerminalInstance.tsx` removed the
 * duplication but left the result UNTESTABLE: that file transitively pulls
 * `@xterm/addon-canvas`, which touches `self` at module init and crashes under
 * the runner's `environment: "node"` vitest config, so nothing exported from it
 * can be imported by a test. The custom-action caller was then verified
 * on-page and the imperative one was not, and the argument offered for it was
 * "they share a function, so the tests carry it".
 *
 * That is an argument from SHAPE, and it is the identical reasoning PR #1301's
 * docstring used about the three siblings it had not actually checked — which
 * is the defect this whole change exists to close. So the function lives here
 * instead: a leaf module (zero imports, same reason `terminalWriteResult.ts`
 * and `terminalKeySequence.ts` are leaves) that a test can import directly.
 * Both callers are now covered by measurement rather than by inheritance.
 */

/**
 * The slice of a terminal backend this reader needs.
 *
 * Structural rather than the real `TerminalBackend`, so a test can supply a
 * buffer without constructing an xterm instance — which is precisely what made
 * the previous arrangement unverifiable.
 */
export interface ScrollbackSource {
  getBufferLength(): number;
  getBufferLine(line: number): string | null;
}

/** The window size used when a caller names none. */
export const DEFAULT_SCROLLBACK_LINES = 500;

/**
 * Read at most `maxLines` lines back off a terminal backend's buffer.
 *
 * ## `maxLines` windows over BUFFER ROWS, not over output lines
 *
 * The window is the last `maxLines` ROWS of the buffer, and rows that are
 * empty (`""` / `null`) are dropped rather than emitted. A live pane's current
 * row is normally empty, so `maxLines: 12` over a buffer of 40 written lines
 * returns ELEVEN lines of text, not twelve. Measured on the page, not inferred.
 *
 * That is the shipped contract and it is deliberate — an empty trailing row is
 * not content — but it is easy to misread as an off-by-one, so it is stated
 * here and pinned by a test. A caller that wants "the last N non-empty lines"
 * wants a different function; this one does not pretend to be it.
 */
export function readBufferScrollback(
  backend: ScrollbackSource | null | undefined,
  maxLines: number = DEFAULT_SCROLLBACK_LINES,
): string {
  if (!backend) return "";
  const totalLines = backend.getBufferLength();
  const startLine = Math.max(0, totalLines - maxLines);
  const lines: string[] = [];
  for (let i = startLine; i < totalLines; i++) {
    const line = backend.getBufferLine(i);
    if (line) lines.push(line);
  }
  return lines.join("\n");
}
