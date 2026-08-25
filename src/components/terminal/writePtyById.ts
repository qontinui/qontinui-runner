import { invoke } from "@tauri-apps/api/core";
import { buildWriteFailure, type TerminalWriteResult } from "./terminalWriteResult";

const encoder = new TextEncoder();

/** Encode bytes to base64 without blowing the stack on a large buffer. */
export function uint8ToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, Math.min(i + CHUNK, bytes.length)));
  }
  return btoa(binary);
}

/**
 * Write to a PTY by id, with the SAME honest envelope `TerminalInstance`'s
 * `writePty` returns — for callers that have no mounted pane.
 *
 * This is the write half of the mount-independent terminal surface
 * (manual-test-loop iter 18, item 1): `TerminalBridgeProxies` serves
 * `writeToTerminal` / `sendKeys` / `pasteText` for a pane with no
 * `TerminalInstance` — a flow-grid `assigned-virtual` zone, or any pane during
 * the restore window — and those are automation actions, so they must report
 * the same three outcomes the mounted path does.
 *
 * A write that reached no process must never answer `success: true`. That was
 * iteration 6's defect and it would come straight back if this path used the
 * historical `.catch(() => {})` (which `writeToTerminalById` still does, because
 * it is a fire-and-forget UI affordance rather than an automation result).
 * There is no xterm here to paint the inline restart affordance into, so the
 * envelope is the whole report.
 *
 * @param exit  `null` when the pane is live; `{ exitCode }` when the tab is
 *              known dead. Passing the tab's own liveness rather than probing
 *              keeps this pure enough to test without a PTY.
 * @param invoker  Injected for tests (the same pattern as `registerWhenReady`'s
 *                 `timers` and `resumeVerification`'s `readTail`). Defaults to
 *                 the Tauri `invoke`.
 */
export async function writePtyById(
  terminalId: string,
  text: string,
  exit: { exitCode: number | null } | null,
  invoker: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): Promise<TerminalWriteResult> {
  // Refuse BEFORE the IPC: a write to an exited pane is not an IPC failure and
  // must not be reported as one — the two carry different recoveries.
  if (exit) return buildWriteFailure(terminalId, exit, null);
  const bytes = encoder.encode(text);
  try {
    await invoker("terminal_write", { terminalId, data: uint8ToBase64(bytes) });
    return { success: true, bytes: bytes.length };
  } catch (err) {
    return buildWriteFailure(terminalId, exit, err);
  }
}
