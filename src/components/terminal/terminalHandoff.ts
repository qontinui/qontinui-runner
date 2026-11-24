/**
 * Terminal HANDOFF — the frontend half of a runner restart that keeps
 * holder-backed panes alive (plan
 * `2026-10-21-graceful-runner-restart-detaches-holder-panes-instead-of-killing-them`,
 * D2 step 7 and Phase 0 probe 6).
 *
 * On a handoff restart the runner detaches each holder-backed pane from its PTY
 * holder instead of killing it, and emits `terminal-handed-off` in place of
 * `terminal-exit`. The pane's process is STILL RUNNING; the next runner's boot
 * sweep adopts it under the same terminal id and the same lifecycle record.
 *
 * So a handed-off pane must never be treated as exited here:
 *  - no `terminal_session_record_close` — that would close the lifecycle
 *    record the adoption needs, queue a coord `Closed`, and (on the workdir's
 *    last record) revoke the coord-mcp nonce the live `claude` still uses;
 *  - no `isAlive: false` / exit-code update on the tab.
 *
 * The backend refuses such a close too (`CloseOutcome::TerminalHandedOff`);
 * this is the half that keeps the webview from asking.
 */

/** The Tauri event the runner emits for a handed-off pane. */
export const TERMINAL_HANDED_OFF_EVENT = "terminal-handed-off";

/** `terminal-handed-off` payload (`ExitNoticeSink::emit_handed_off`). */
export interface TerminalHandedOffEvent {
  terminalId: string;
  holderPid: number;
}

/** Terminal ids handed off in this webview's lifetime. */
const handedOff = new Set<string>();

/** Record a `terminal-handed-off` notice. Ignores a payload with no id. */
export function noteHandedOff(payload: Partial<TerminalHandedOffEvent> | null | undefined): void {
  const id = payload?.terminalId;
  if (typeof id === "string" && id.length > 0) handedOff.add(id);
}

/** Whether `terminalId` was handed off (its process lives on in a holder). */
export function isHandedOff(terminalId: string): boolean {
  return handedOff.has(terminalId);
}

/** Test seam: forget every recorded handoff. */
export function resetHandedOff(): void {
  handedOff.clear();
}

/**
 * Subscribe to `terminal-handed-off` through `listenFn` (Tauri's `listen`,
 * injected so a test can drive it). Resolves to the unlisten function.
 */
export function listenForHandoffs(
  listenFn: (
    event: string,
    handler: (event: { payload: TerminalHandedOffEvent }) => void,
  ) => Promise<() => void>,
): Promise<() => void> {
  return listenFn(TERMINAL_HANDED_OFF_EVENT, (event) => noteHandedOff(event.payload));
}

/** What a pane-exit callback should do for `terminalId`. */
export type PaneExitDisposition = "handed-off" | "exited";

/**
 * The decision `TerminalPage`'s `handleExit` makes before touching anything:
 * a handed-off pane is NOT an exit — no record-close, no tab update.
 */
export function paneExitDisposition(terminalId: string): PaneExitDisposition {
  return isHandedOff(terminalId) ? "handed-off" : "exited";
}
