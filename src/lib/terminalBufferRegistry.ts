/**
 * terminalBufferRegistry
 *
 * Cross-page module-level registry mapping a terminal session ID to a
 * function that serializes the current xterm.js buffer text. Powers the
 * `GET /ui-bridge/sdk/terminal/sessions/:session_id/buffer` HTTP endpoint
 * via the `useTerminalBufferIpc` Tauri-event listener.
 *
 * Why module-level (not React context)?
 *
 * `TerminalCoreProvider` is per-page (keyed by `pageId`), so `terminalRefs`
 * inside it only knows about tabs mounted on its own page. The HTTP endpoint
 * is global — callers identify a session by ID alone, with no page context.
 * A single module-level Map gives every TerminalInstance one place to
 * register itself and one place for the IPC listener to look it up,
 * regardless of which TerminalPage tree it lives in.
 *
 * Lifecycle: TerminalInstance.useEffect registers on mount, unregisters on
 * unmount. Stale entries are impossible because the registration is keyed
 * by the same `terminalId` that React uses for element identity.
 */

/**
 * Function that returns a snapshot of the active buffer text for a session.
 *
 * Implementation contract:
 * - `maxLines` defaults to all available lines.
 * - Returned `lines` are oldest-first within the requested window.
 * - `totalLines` reflects the full buffer length even if the returned
 *   `lines` slice is truncated to `maxLines`.
 */
export type TerminalBufferReader = (maxLines?: number) => {
  lines: string[];
  totalLines: number;
};

const readers = new Map<string, TerminalBufferReader>();

export function registerTerminalBufferReader(
  sessionId: string,
  reader: TerminalBufferReader,
): void {
  readers.set(sessionId, reader);
}

export function unregisterTerminalBufferReader(sessionId: string): void {
  readers.delete(sessionId);
}

export function lookupTerminalBufferReader(
  sessionId: string,
): TerminalBufferReader | null {
  return readers.get(sessionId) ?? null;
}

/**
 * Test/diagnostic helper — returns the registered session IDs. Not used in
 * production code paths.
 */
export function listRegisteredSessionIds(): string[] {
  return Array.from(readers.keys());
}
