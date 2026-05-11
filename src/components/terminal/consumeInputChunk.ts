/**
 * Pure helper extracted from {@link TerminalInstance}'s xterm.js `onData`
 * newline loop so the input-accumulator semantics can be unit-tested
 * without standing up xterm.js or the WebGL canvas addon.
 *
 * Lives in its own module (rather than next to `TerminalInstance`) so
 * the test file doesn't pull `@xterm/addon-canvas` through — that addon
 * touches `self` at module init, which blows up under vitest's
 * `environment: "node"`.
 *
 * Behavior mirrors the historical loop in `TerminalInstance.tsx`:
 *   - Iterates `chunk` char-by-char.
 *   - On `\r` or `\n`, trims the current accumulator and — if non-empty
 *     — emits it as a line. Empty lines (bare newlines) are dropped.
 *   - Only chars with code >= 32 accumulate; ESC, BEL, and other
 *     control bytes are filtered out so they don't pollute the
 *     auto-name or the mid-session probe payload.
 *   - The first newline-terminated non-empty line in the chunk (if
 *     any) is also surfaced as `firstInputLineIfAny` UNLESS
 *     `firstInputAlreadyReported` is true — gating the caller's
 *     `onFirstInput` to "exactly once per session" without duplicating
 *     the gate inside this helper.
 *
 * The returned `accum` is the leftover (post-last-newline) partial
 * line that the caller threads back in on the next chunk.
 */
export function consumeInputChunk(
  chunk: string,
  accum: string,
  firstInputAlreadyReported: boolean,
): { accum: string; lines: string[]; firstInputLineIfAny: string | undefined } {
  const lines: string[] = [];
  let firstInputLineIfAny: string | undefined;
  let current = accum;
  for (const ch of chunk) {
    if (ch === "\r" || ch === "\n") {
      const line = current.trim();
      if (line.length > 0) {
        lines.push(line);
        if (!firstInputAlreadyReported && firstInputLineIfAny === undefined) {
          firstInputLineIfAny = line;
        }
      }
      current = "";
    } else if (ch.charCodeAt(0) >= 32) {
      current += ch;
    }
  }
  return { accum: current, lines, firstInputLineIfAny };
}
