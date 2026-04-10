/**
 * TerminalCoreContext
 *
 * Foundation context that owns terminal management, zone layout, and terminal refs.
 * All other terminal contexts and components depend on this one.
 */

import {
  createContext,
  useMemo,
  useEffect,
  useRef,
  createRef,
  type ReactNode,
  type RefObject,
} from "react";
import { useTerminalManager } from "../useTerminalManager";
import { useZoneLayout } from "../useZoneLayout";
import { type TerminalInstanceHandle } from "../TerminalInstance";
import { type ZoneSessionInfo } from "../ZoneProfilePicker";

type TerminalManagerReturn = ReturnType<typeof useTerminalManager>;
type ZoneLayoutReturn = ReturnType<typeof useZoneLayout>;

export interface TerminalCoreContextValue extends TerminalManagerReturn {
  pageId: string;
  zoneLayout: ZoneLayoutReturn;
  terminalRefs: React.MutableRefObject<Map<string, RefObject<TerminalInstanceHandle | null>>>;
  pendingProfileSessionsRef: React.MutableRefObject<ZoneSessionInfo[] | null>;
}

export const TerminalCoreContext = createContext<TerminalCoreContextValue | null>(null);

interface TerminalCoreProviderProps {
  pageId: string;
  children: ReactNode;
}

export function TerminalCoreProvider({ pageId, children }: TerminalCoreProviderProps) {
  const terminalManager = useTerminalManager(pageId);
  const { tabs, activeId, setActiveId, updateTab } = terminalManager;

  const tabIds = useMemo(() => tabs.map((t) => t.id), [tabs]);
  const zoneLayout = useZoneLayout(tabIds, pageId);

  // Sync focused zone → active tab
  useEffect(() => {
    if (
      zoneLayout.focusedTabId &&
      zoneLayout.focusedTabId !== activeId &&
      tabs.some((t) => t.id === zoneLayout.focusedTabId)
    ) {
      setActiveId(zoneLayout.focusedTabId);
    }
  }, [zoneLayout.focusedTabId, activeId, setActiveId, tabs]);

  // Terminal refs map — create refs for new tabs, clean up stale ones
  const terminalRefs = useRef<Map<string, RefObject<TerminalInstanceHandle | null>>>(new Map());

  for (const tab of tabs) {
    if (!terminalRefs.current.has(tab.id)) {
      terminalRefs.current.set(tab.id, createRef<TerminalInstanceHandle>());
    }
  }
  for (const key of terminalRefs.current.keys()) {
    if (!tabs.some((t) => t.id === key)) {
      terminalRefs.current.delete(key);
    }
  }

  // Pending Claude sessions to resume after a zone profile load settles
  const pendingProfileSessionsRef = useRef<ZoneSessionInfo[] | null>(null);

  useEffect(() => {
    const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
    const sessions = pendingProfileSessionsRef.current;
    if (!sessions) return;
    pendingProfileSessionsRef.current = null;

    for (const s of sessions) {
      const tabId = zoneLayout.assignments[s.zoneIndex];
      if (tabId && SESSION_ID_RE.test(s.claudeSessionId)) {
        updateTab(tabId, {
          claudeSessionId: s.claudeSessionId,
          claudeConfigDir: s.claudeConfigDir,
        });
        const ref = terminalRefs.current.get(tabId);
        const handle = ref?.current;
        if (handle) {
          handle.writeToTerminal(`claude --resume ${s.claudeSessionId}\r`);
        }
      }
    }
  }, [zoneLayout.assignments, updateTab]);

  const value = useMemo<TerminalCoreContextValue>(
    () => ({
      ...terminalManager,
      pageId,
      zoneLayout,
      terminalRefs,
      pendingProfileSessionsRef,
    }),
    [terminalManager, pageId, zoneLayout],
  );

  return <TerminalCoreContext.Provider value={value}>{children}</TerminalCoreContext.Provider>;
}
