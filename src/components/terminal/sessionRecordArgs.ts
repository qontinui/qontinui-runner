/**
 * Pure builders for the durable terminal-session registry `invoke` payloads.
 *
 * Kept free of React / Tauri imports so the resolution logic (which zone does a
 * tab belong to, which working dir / title to record) can be unit-tested in the
 * runner's `node` vitest environment without booting the component tree.
 *
 * The frontend records OPEN/CLOSE through three Tauri commands:
 *   - `terminal_session_record_open`  (args = {@link SessionOpenArgs})
 *   - `terminal_session_record_close` (args = { claudeSessionId, reason })
 *   - `terminal_session_list_open`    (restore — see useTerminalInitialization)
 */

/** A tab shape the open-record builder needs (subset of `TerminalTab`). */
export interface OpenRecordTab {
  id: string;
  title?: string;
  workingDir?: string;
}

/** Args for the `terminal_session_record_open` Tauri command. */
export interface SessionOpenArgs {
  claudeSessionId: string;
  configDir?: string;
  workingDir?: string;
  pageId: string;
  zoneIndex: number;
  title?: string;
  terminalId: string;
}

/**
 * Resolve the zone a tab is assigned to by reverse-lookup over the live
 * `zoneIndex → tabId` assignments. Returns -1 (unassigned/hidden) when the tab
 * is in no zone — the same sentinel the backend record uses.
 */
export function resolveZoneIndex(
  assignments: Record<number, string>,
  tabId: string,
): number {
  return Number(Object.entries(assignments).find(([, id]) => id === tabId)?.[0] ?? -1);
}

/**
 * Build the `terminal_session_record_open` payload for a tab that just bound a
 * `claudeSessionId`. Resolves the tab's current zone and pulls workingDir/title
 * from the live tab list.
 */
export function buildSessionOpenArgs(params: {
  assignments: Record<number, string>;
  tabs: OpenRecordTab[];
  tabId: string;
  claudeSessionId: string;
  configDir: string | undefined;
  pageId: string;
}): SessionOpenArgs {
  const { assignments, tabs, tabId, claudeSessionId, configDir, pageId } = params;
  const tab = tabs.find((t) => t.id === tabId);
  return {
    claudeSessionId,
    configDir,
    workingDir: tab?.workingDir,
    pageId,
    zoneIndex: resolveZoneIndex(assignments, tabId),
    title: tab?.title,
    terminalId: tabId,
  };
}
