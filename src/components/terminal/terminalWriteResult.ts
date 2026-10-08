/**
 * PTY-write result envelope — the one shape every write path in
 * `TerminalInstance` returns.
 *
 * A leaf module (its only import is the DOM-free `@/lib/utils`) for the same
 * reason `consumeInputChunk.ts` is one: `TerminalInstance` transitively pulls
 * `@xterm/addon-canvas`, which touches `self` at module init and crashes under
 * the runner's `environment: "node"` vitest config, so nothing exported from
 * that file is testable.
 */

import { describeThrown } from "@/lib/utils";

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
 * Pull the exit code out of a backend `TERMINAL_EXITED: ...` refusal.
 *
 * The Rust envelope reads `... its process exited with code <n>.`, or
 * `code unknown.` when the waiter thread never captured one. Returns `null`
 * for the unknown form, which is the same value the frontend records when a
 * `terminal-exit` event carries no code -- callers already render it as
 * "unknown".
 */
function parseBackendExitCode(detail: string): number | null {
  const match = /exited with code (-?\d+)/.exec(detail);
  return match ? Number(match[1]) : null;
}

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
  const detail = describeThrown(cause, "no detail");
  // The Rust write funnel (`TerminalSession::write`) now refuses a write to an
  // exited PTY with its OWN `TERMINAL_EXITED: ...` envelope. Recognise it, so a
  // pane whose `terminal-exit` event has not yet landed in this component
  // (`exit === null`) still classifies the refusal as TERMINAL_EXITED rather
  // than downgrading the backend's typed diagnosis to TERMINAL_WRITE_FAILED.
  // That downgrade is not cosmetic: `resumeVerification` treats
  // TERMINAL_WRITE_FAILED as retryable and burns the full 31s ladder against a
  // pty that is already gone.
  // `startsWith`, not `includes`: the Rust envelope always PREFIXES the code
  // (`mcp/terminals.rs` classifies the same refusal with `e.starts_with(...)`),
  // so this both matches the guarantee and refuses to promote an unrelated
  // message that merely mentions the token.
  const refusedByBackend = !exit && detail.startsWith(TERMINAL_EXITED);
  if (exit || refusedByBackend) {
    exit = exit ?? { exitCode: parseBackendExitCode(detail) };
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

/**
 * Machine-readable failure code: the clipboard read behind a `paste` action was
 * REJECTED (permission denied, document not focused, no clipboard API).
 */
export const CLIPBOARD_READ_FAILED = "CLIPBOARD_READ_FAILED";

/**
 * Build the typed error a `paste` handler throws when the clipboard read is
 * rejected. Pure, so the envelope is unit-testable without a clipboard.
 */
export function buildClipboardReadFailure(cause: unknown): Error & {
  code: typeof CLIPBOARD_READ_FAILED;
  hint: string;
} {
  const detail = describeThrown(cause, "");
  const hint =
    "The page may lack focus or clipboard permission. Focus the runner window " +
    "and retry, or send the text directly with `pasteText`.";
  const err = new Error(
    `${CLIPBOARD_READ_FAILED}: navigator.clipboard.readText() was rejected${
      detail ? `: ${detail}` : ""
    }. ${hint}`,
  );
  return Object.assign(err, { code: CLIPBOARD_READ_FAILED, hint } as const);
}

/**
 * Read the clipboard for a `paste` custom action — the ONE reader both paste
 * paths (`TerminalInstance` mounted, `TerminalBridgeProxies` by id) share.
 *
 * THE DEFECT this replaces: both paths did
 * `navigator.clipboard.readText().catch(() => "")` followed by
 * `if (!text) return { success: true, bytes: 0 }`, so a REJECTED read was
 * indistinguishable from an empty clipboard and reported green. A rejection
 * now throws a {@link CLIPBOARD_READ_FAILED} error — throwing is the only
 * signal that reaches the SDK executor's `success` (see
 * {@link throwIfWriteFailed}). A genuinely empty clipboard still resolves `""`,
 * which the callers keep answering as `{ success: true, bytes: 0 }`.
 *
 * `read` is injectable for tests; production callers pass nothing.
 */
export async function readClipboardForPaste(
  read: () => Promise<string> = () => navigator.clipboard.readText(),
): Promise<string> {
  try {
    return await read();
  } catch (cause) {
    throw buildClipboardReadFailure(cause);
  }
}
