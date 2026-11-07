/**
 * Per-(page, window) persistence for the Session Manager sidebar's open state
 * (`workflowGen.showSidebar`).
 *
 * In tmux work mode the sidebar is the ONLY navigator: every session but the
 * one on screen is reachable from its list and nowhere else (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4). The mode itself is
 * persisted beside the zone layout, so a sidebar that reloaded closed would
 * bring the operator back to one pane with no visible way to the others. The
 * flag is stored under the same per-(page, window) key shape as the layout
 * ({@link pageWindowStorageKey}) so a pop-out window keeps its own.
 */

import { useEffect, useState } from "react";

import { instanceStorage } from "@/lib/instance-storage";

import { pageWindowStorageKey } from "./pageWindowStorageKey";

const BASE_STORAGE_KEY = "qontinui-session-sidebar";

/** The sidebar's state on a page/window with nothing stored — today's default. */
export const DEFAULT_SHOW_SIDEBAR = false;

/** The `instanceStorage` key holding one page/window's sidebar state. */
export function sessionSidebarStorageKey(pageId: string, windowLabel: string = "main"): string {
  return pageWindowStorageKey(BASE_STORAGE_KEY, pageId, windowLabel);
}

/**
 * Read a stored sidebar state. Absent is the default (the page predates the
 * key). A value that is present but not a boolean is also the default, and is
 * logged with the value, so a corrupted preference is visible rather than
 * silently read as either state.
 */
export function parseShowSidebar(raw: unknown): boolean {
  if (raw === null || raw === undefined) return DEFAULT_SHOW_SIDEBAR;
  if (typeof raw === "boolean") return raw;
  console.warn(
    `[sessionSidebar] persisted showSidebar ${JSON.stringify(raw)} is not a boolean; using ${DEFAULT_SHOW_SIDEBAR}`,
  );
  return DEFAULT_SHOW_SIDEBAR;
}

export function loadShowSidebar(pageId: string, windowLabel?: string): boolean {
  return parseShowSidebar(
    instanceStorage.getJSON<unknown>(sessionSidebarStorageKey(pageId, windowLabel), null),
  );
}

/**
 * `useState` for the sidebar flag, loaded from and written back to the
 * page/window's key. Every change is written, including the default, so the
 * stored value always names what the operator last saw.
 */
export function usePersistedShowSidebar(
  pageId: string,
  windowLabel?: string,
): [boolean, React.Dispatch<React.SetStateAction<boolean>>] {
  const [showSidebar, setShowSidebar] = useState<boolean>(() =>
    loadShowSidebar(pageId, windowLabel),
  );
  useEffect(() => {
    instanceStorage.setJSON(sessionSidebarStorageKey(pageId, windowLabel), showSidebar);
  }, [pageId, windowLabel, showSidebar]);
  return [showSidebar, setShowSidebar];
}
