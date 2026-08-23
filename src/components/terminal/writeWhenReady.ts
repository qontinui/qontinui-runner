import type { RefObject } from "react";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import { buildWriteFailure, type TerminalWriteResult } from "./terminalWriteResult";

export type TerminalRefsMap = Map<string, RefObject<TerminalInstanceHandle | null>>;

/**
 * Wait until a terminal ref is ready, then write text to it.
 *
 * Polls every 200ms for up to `maxWaitMs` — bridges the gap between tab
 * creation and TerminalInstance mount / xterm.js init. Calls `onTimeout`
 * if the ref never becomes ready (useful for per-call-site logging).
 *
 * ## Why this resolves a {@link TerminalWriteResult}
 *
 * THE DEFECT this closes: this helper used to return `void` and drop the
 * `Promise<TerminalWriteResult>` that `writeToTerminal` resolves. Every caller
 * — including the boot-restore resume path — therefore could not tell a write
 * that reached the PTY from one refused with `TERMINAL_EXITED` /
 * `TERMINAL_WRITE_FAILED`. `typeResumeAndVerify` then spent the full
 * ~31 s handshake budget (2 attempts × 15 s poll) polling the scrollback of a
 * pane whose process was already gone, and reported a generic "handshake not
 * observed" instead of the typed reason it already had in hand.
 *
 * The ref never becoming ready is reported in the SAME envelope
 * (`TERMINAL_WRITE_FAILED`, built by {@link buildWriteFailure} with no exit
 * record — the pane is not known-exited, the write simply never got a handle),
 * so a caller has exactly one shape to branch on. `onTimeout` still fires for
 * call-site logging; it is now additive, not the only signal.
 */
export function writeWhenReady(
  terminalRefs: TerminalRefsMap,
  tabId: string,
  text: string,
  options: { maxWaitMs?: number; onTimeout?: (tabId: string) => void } = {},
): Promise<TerminalWriteResult> {
  const { maxWaitMs = 5000, onTimeout } = options;
  const start = Date.now();
  return new Promise<TerminalWriteResult>((resolve) => {
    const poll = () => {
      const handle = terminalRefs.get(tabId)?.current;
      if (handle) {
        // `writeToTerminal` never rejects (it resolves the failure envelope),
        // but a test double or a future handle might — fold a throw into the
        // same typed envelope rather than leaking an unhandled rejection.
        Promise.resolve(handle.writeToTerminal(text)).then(resolve, (err: unknown) =>
          resolve(buildWriteFailure(tabId, null, err)),
        );
        return;
      }
      if (Date.now() - start < maxWaitMs) {
        setTimeout(poll, 200);
        return;
      }
      if (onTimeout) {
        onTimeout(tabId);
      } else {
        console.warn(`[writeWhenReady] terminal ref for ${tabId} never became ready`);
      }
      resolve(
        buildWriteFailure(
          tabId,
          null,
          new Error(`terminal ref for ${tabId} never became ready within ${maxWaitMs}ms`),
        ),
      );
    };
    poll();
  });
}
