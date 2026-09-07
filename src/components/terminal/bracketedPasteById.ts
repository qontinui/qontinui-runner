import { invoke } from "@tauri-apps/api/core";

/** Machine-readable failure code: the PTY's bracketed-paste state is unknown. */
export const BRACKETED_PASTE_UNKNOWN = "BRACKETED_PASTE_UNKNOWN";

/**
 * Read a terminal's CURRENT bracketed-paste (DEC private mode 2004) state by
 * id, with no mounted xterm required.
 *
 * ## Why (manual-test-loop iter 24, item 6)
 *
 * `preparePasteData` needs to know whether the foreground app asked for the
 * `ESC[200~ … ESC[201~` envelope. The mounted path reads it off xterm.js
 * (`backend.bracketedPasteMode`), which is only meaningful while a
 * `TerminalInstance` exists — so `TerminalBridgeProxies`, which serves exactly
 * the panes that mount nothing, passed a hardcoded `false`.
 *
 * The result was a capability that changed with the VIEWPORT: the identical
 * `pasteText` call delivered a properly bracketed paste to a pane scrolled into
 * view, and a bare keystroke stream — every embedded newline read as Enter, so
 * a multi-line paste submitted line-by-line — to the same pane scrolled out of
 * it. Nothing reported the difference; both answered `success: true`.
 *
 * The runner's server-side VT parser sees every output byte of every session,
 * mounted or not, so the Rust side is the one place that can answer for both.
 * See `src-tauri/src/terminal/grid.rs` (`Grid::bracketed_paste`) and
 * `terminal_get_bracketed_paste`.
 *
 * ## Unknown is thrown, never guessed
 *
 * Defaulting to `false` when the probe fails would silently reinstate the exact
 * defect this closes — a paste that reaches the PTY with the wrong framing and
 * is reported green. So an unreadable state is a typed failure. Callers that
 * already know the pane is dead should skip the probe entirely and let the
 * write path answer `TERMINAL_EXITED`, which is the more specific diagnosis.
 *
 * @param invoker Injected for tests; defaults to the Tauri `invoke`.
 */
export async function readBracketedPasteMode(
  terminalId: string,
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): Promise<boolean> {
  let resp: unknown;
  try {
    resp = await invoker("terminal_get_bracketed_paste", { terminalId });
  } catch (err) {
    throw invalid(
      `could not read bracketed-paste state for terminal ${terminalId}: ` +
        `${err instanceof Error ? err.message : String(err)}. Nothing was written.`,
    );
  }
  const value = (resp as { data?: { bracketedPaste?: unknown } } | null)?.data?.bracketedPaste;
  if (typeof value !== "boolean") {
    throw invalid(
      `terminal_get_bracketed_paste returned no boolean 'bracketedPaste' for ` +
        `terminal ${terminalId}. Guessing would send the wrong bytes to a live ` +
        `PTY, so nothing was written.`,
    );
  }
  return value;
}

function invalid(detail: string): Error {
  const err = new Error(`${BRACKETED_PASTE_UNKNOWN}: ${detail}`) as Error & { code?: string };
  err.code = BRACKETED_PASTE_UNKNOWN;
  return err;
}
