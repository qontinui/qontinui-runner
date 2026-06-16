/**
 * Prepare clipboard text for delivery to a PTY, mirroring what xterm.js does
 * inside `Terminal.paste()` (see xterm's `browser/Clipboard.ts`).
 *
 * The runner intercepts Ctrl+V itself and writes the result straight to the
 * Rust PTY via `terminal_write` (Tauri's webview doesn't fire the browser
 * clipboard events xterm relies on, and a native paste would double-write).
 * That direct path previously sent the RAW clipboard text, which broke pasting
 * into TUIs that enable bracketed paste mode (Claude Code, vim, fzf, …):
 *
 *   - Without the `ESC[200~ … ESC[201~` envelope, the app sees the paste as a
 *     stream of keystrokes. Every embedded newline reads as Enter, so a
 *     multi-line paste is submitted line-by-line and only the trailing
 *     fragment survives in the input ("only the last few lines paste").
 *   - The newline burst also forces the TUI to re-render mid-paste, colliding
 *     with terminal line-wrapping ("misplaced spaces and characters").
 *
 * This helper is the pure core so it can be unit-tested without standing up
 * xterm.js or a real PTY (same rationale as {@link consumeInputChunk}).
 */

/** Bracketed paste start marker (DEC mode 2004). */
const BRACKET_START = "\x1b[200~";
/** Bracketed paste end marker. */
const BRACKET_END = "\x1b[201~";

/**
 * @param text                Raw clipboard text.
 * @param bracketedPasteMode  Whether the foreground app enabled DEC mode 2004.
 * @returns The byte string to write to the PTY.
 */
export function preparePasteData(text: string, bracketedPasteMode: boolean): string {
  // Normalize line endings to CR. A real terminal sends \r on Enter, a shell
  // treats \r as submit, and a TUI reading bracketed paste expects \r between
  // lines. Matches xterm's `prepareTextForTerminal`: `\r\n` and lone `\n` → `\r`.
  let normalized = text.replace(/\r?\n/g, "\r");

  if (!bracketedPasteMode) {
    // No envelope to protect — deliver as-is (the app sees raw keystrokes,
    // exactly as if typed).
    return normalized;
  }

  // Strip any embedded paste markers before wrapping. Copying output from
  // another terminal session can carry a literal `ESC[201~` in the clipboard;
  // left in place it would close the envelope early and let the remainder be
  // interpreted as commands. xterm doesn't guard this, but the runner's whole
  // purpose is shuttling terminal text between panes, so the risk is real here.
  normalized = normalized.split(BRACKET_START).join("").split(BRACKET_END).join("");

  return BRACKET_START + normalized + BRACKET_END;
}
