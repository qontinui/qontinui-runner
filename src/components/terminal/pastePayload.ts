/**
 * Pure helper for building the byte payload of an interactive clipboard
 * paste into a terminal PTY.
 *
 * Kept as a leaf module (no xterm.js / DOM imports) so vitest can exercise
 * it without pulling in the canvas addon — same rationale as
 * `consumeInputChunk.ts`.
 */

/**
 * Build the text payload for a clipboard paste, mirroring xterm.js's
 * `Clipboard.ts`:
 *   1. Normalize line endings to bare CR (`\r?\n` → `\r`).
 *   2. When the running TUI has enabled DEC private mode 2004, wrap the
 *      whole block in bracketed-paste markers (`\x1b[200~` … `\x1b[201~`).
 *
 * Step 2 is what makes large multi-line pastes work. The runner's Ctrl+V
 * handler writes the clipboard straight to the PTY (to avoid the WebView2
 * double-paste), bypassing xterm's own paste path. Without the markers, a
 * paste that spans more than one PTY read has the newlines in its early
 * chunks interpreted as submit keystrokes by Claude Code's burst-paste
 * heuristic — so the leading lines get sent and the beginning of the paste
 * is lost (the "more than ~7-8 lines gets truncated" symptom). Wrapping in
 * bracketed-paste markers delivers the content atomically regardless of how
 * the bytes are chunked across reads.
 *
 * No trailing `\r` is appended: interactive paste must NOT auto-submit
 * (auto-submit is the job of the separate Rust `submit_prompt` path).
 */
export function buildPastePayload(text: string, bracketedPaste: boolean): string {
  const normalized = text.replace(/\r?\n/g, "\r");
  return bracketedPaste ? `\x1b[200~${normalized}\x1b[201~` : normalized;
}
