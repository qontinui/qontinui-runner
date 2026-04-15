import type { RefObject } from "react";
import type { TerminalInstanceHandle } from "./TerminalInstance";

export type TerminalRefsMap = Map<string, RefObject<TerminalInstanceHandle | null>>;

/**
 * Wait until a terminal ref is ready, then write text to it.
 *
 * Polls every 200ms for up to `maxWaitMs` — bridges the gap between tab
 * creation and TerminalInstance mount / xterm.js init. Calls `onTimeout`
 * if the ref never becomes ready (useful for per-call-site logging).
 */
export function writeWhenReady(
  terminalRefs: TerminalRefsMap,
  tabId: string,
  text: string,
  options: { maxWaitMs?: number; onTimeout?: (tabId: string) => void } = {},
): void {
  const { maxWaitMs = 5000, onTimeout } = options;
  const start = Date.now();
  const poll = () => {
    const handle = terminalRefs.get(tabId)?.current;
    if (handle) {
      handle.writeToTerminal(text);
      return;
    }
    if (Date.now() - start < maxWaitMs) {
      setTimeout(poll, 200);
    } else if (onTimeout) {
      onTimeout(tabId);
    } else {
      console.warn(`[writeWhenReady] terminal ref for ${tabId} never became ready`);
    }
  };
  poll();
}
