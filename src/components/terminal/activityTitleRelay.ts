import { invoke } from "@tauri-apps/api/core";

/**
 * The xterm OSC 0/2 title relay (`ZoneGrid.onTitleChange`), as a pure
 * function over its two effects (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4 / D3).
 *
 * The OSC title is the pane's current ACTIVITY. It goes to
 * `tab.activityTitle` and to the backend's `terminal_set_title`, which stores
 * it as `TerminalSession::activity_title` only. It never renames the tab: a
 * rename overwrote `tab.title`, and with it the conversation's identity.
 *
 * Fire-and-forget backend write: a failure is non-fatal. At worst
 * `GET /terminals` lacks the activity, and the local tab has it already.
 */
export function relayActivityTitle(
  updateTab: (id: string, updates: { activityTitle: string }) => void,
  tabId: string,
  title: string,
  invokeFn: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = invoke,
): void {
  const trimmed = title.trim();
  if (!trimmed) return;
  updateTab(tabId, { activityTitle: trimmed });
  invokeFn("terminal_set_title", { terminalId: tabId, title: trimmed }).catch((e) => {
    console.warn(`[ZoneGrid] terminal_set_title failed for ${tabId}:`, e);
  });
}
