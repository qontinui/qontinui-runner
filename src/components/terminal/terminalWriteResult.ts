/**
 * PTY-write result envelope — the one shape every write path in
 * `TerminalInstance` returns.
 *
 * A leaf module (zero imports) for the same reason `consumeInputChunk.ts` is
 * one: `TerminalInstance` transitively pulls `@xterm/addon-canvas`, which
 * touches `self` at module init and crashes under the runner's `environment:
 * "node"` vitest config, so nothing exported from that file is testable.
 */

/** Machine-readable failure code: the PTY behind this pane is gone. */
export const TERMINAL_EXITED = "TERMINAL_EXITED";
/** Machine-readable failure code: the write was attempted and the IPC failed. */
export const TERMINAL_WRITE_FAILED = "TERMINAL_WRITE_FAILED";

/** Result envelope for a PTY write. Every write path in `TerminalInstance` returns it. */
export type TerminalWriteResult =
  | { success: true; bytes: number }
  | {
      success: false;
      code: typeof TERMINAL_EXITED | typeof TERMINAL_WRITE_FAILED;
      error: string;
      hint: string;
      terminalId: string;
      exitCode?: number | null;
    };


/**
 * Build the failure envelope for a refused or failed PTY write.
 *
 * THE DEFECT this replaces: every `invoke("terminal_write", …)` in this file
 * ended in `.catch(() => {})`. A write to a terminal whose process had exited
 * therefore reported nothing at all — the imperative handle returned `void`,
 * and the UI Bridge `writeToTerminal` / `sendKeys` custom actions resolved,
 * which the SDK executor reports as `success: true`. An automation client got
 * a green result for input that reached no process, and an operator clicking
 * Approve-all on a dead pane saw no reason why nothing happened.
 *
 * Two distinguishable failures, because the recovery differs: `TERMINAL_EXITED`
 * (the process is gone — restart it) versus `TERMINAL_WRITE_FAILED` (the IPC
 * itself failed while the pane is still live — retry / read the runner log).
 *
 * Pure, so the envelope is unit-testable without a PTY.
 */
export function buildWriteFailure(
  terminalId: string,
  exit: { exitCode: number | null } | null,
  cause: unknown,
): Extract<TerminalWriteResult, { success: false }> {
  if (exit) {
    return {
      success: false,
      code: TERMINAL_EXITED,
      terminalId,
      exitCode: exit.exitCode,
      error: `${TERMINAL_EXITED}: terminal ${terminalId} is not writable — its process exited with code ${
        exit.exitCode ?? "unknown"
      }.`,
      hint:
        "Restart the session before writing to it: the Restart button in the zone's " +
        "hover actions, or the `/restart [<zone>]` terminal command " +
        "(`terminal.restart`). Reading scrollback still works on an exited pane.",
    };
  }
  const detail = cause instanceof Error ? cause.message : String(cause);
  return {
    success: false,
    code: TERMINAL_WRITE_FAILED,
    terminalId,
    error: `${TERMINAL_WRITE_FAILED}: terminal_write failed for ${terminalId}: ${detail}`,
    hint:
      "The pane is not marked exited, so this is an IPC/backend failure rather " +
      "than a dead process. Check the runner log for the terminal_write command.",
  };
}


/**
 * Turn a failed {@link TerminalWriteResult} into a throw, so the UI Bridge
 * executor reports `success: false` with the envelope's message.
 *
 * The SDK's `executeAction` treats a resolved custom-action handler as a
 * success no matter what it resolved WITH — so returning the failure envelope
 * would still surface as `success: true` with the failure buried in `result`.
 * Throwing is the only signal that reaches the caller's `success` field.
 */
export function throwIfWriteFailed(result: TerminalWriteResult): TerminalWriteResult {
  if (result.success) return result;
  const err = new Error(`${result.error} ${result.hint}`);
  Object.assign(err, {
    code: result.code,
    terminalId: result.terminalId,
    exitCode: result.exitCode ?? null,
  });
  throw err;
}
