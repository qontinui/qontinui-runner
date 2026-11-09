/**
 * The terminal's Ctrl+V / Cmd+V paste chord, and the one thing its handler
 * must do besides reading the clipboard: CANCEL the keystroke's default action.
 *
 * Why this exists. `TerminalInstance` pastes manually on Ctrl+V (clipboard read
 * → `preparePasteData` → `terminal_write`) and returns `false` from xterm's
 * custom key handler. But xterm does NOT `preventDefault` a keystroke the
 * custom handler rejects, so the webview still runs its default Ctrl+V
 * action, which fires a native `paste` event on xterm's textarea. xterm 6
 * listens for `paste` on that textarea itself (`CoreBrowserTerminal`), and
 * its handler writes the text to the PTY via `onData` — a SECOND copy of
 * everything the manual path already wrote.
 *
 * The capture-phase `paste` listener `TerminalInstance` used to install on
 * the same textarea could not stop that: `stopPropagation` does not reach
 * other listeners registered on the SAME node, and xterm's handler never reads
 * `defaultPrevented`. Cancelling the keydown stops the native paste from ever
 * firing, which refuses the duplicate at the earliest point.
 *
 * The cost: the native paste was also an accidental FALLBACK. When
 * the clipboard read (`readClipboard`: native, then `navigator.clipboard`) fails, this keystroke now pastes nothing,
 * so the handler logs that rejection rather than swallowing it.
 *
 * Other native paste routes (Ctrl+Shift+V, Shift+Insert, the Edit menu) are
 * untouched: the manual path does not handle them, so xterm's own paste is
 * their single writer.
 */
export function isPasteChord(event: KeyboardEvent): boolean {
  return (
    event.type === "keydown" &&
    event.key.toLowerCase() === "v" &&
    (event.ctrlKey || event.metaKey) &&
    !event.shiftKey &&
    !event.altKey
  );
}

/**
 * When `event` is the paste chord, cancel its default action (so no native
 * `paste` event follows) and answer `true`; otherwise leave it alone and
 * answer `false`.
 */
export function claimPasteChord(event: KeyboardEvent): boolean {
  if (!isPasteChord(event)) return false;
  event.preventDefault();
  return true;
}
