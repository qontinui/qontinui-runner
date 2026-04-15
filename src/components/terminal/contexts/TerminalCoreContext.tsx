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
import { writeWhenReady } from "../writeWhenReady";

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

  useEffect(() => {
    const map = terminalRefs.current;
    for (const tab of tabs) {
      if (!map.has(tab.id)) {
        map.set(tab.id, createRef<TerminalInstanceHandle>());
      }
    }
    for (const key of map.keys()) {
      if (!tabs.some((t) => t.id === key)) {
        map.delete(key);
      }
    }
  }, [tabs]);

  // Pending Claude sessions to resume after a zone profile load settles
  const pendingProfileSessionsRef = useRef<ZoneSessionInfo[] | null>(null);

  useEffect(() => {
    const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
    const sessions = pendingProfileSessionsRef.current;
    if (!sessions || sessions.length === 0) return;

    const isWindows = navigator.platform.startsWith("Win");
    const buildResumeCmd = (sessionId: string, configDir: string | undefined) => {
      const base = `claude --resume ${sessionId}`;
      if (!configDir) return `${base}\r`;
      return isWindows
        ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}\r`
        : `CLAUDE_CONFIG_DIR="${configDir}" ${base}\r`;
    };

    // Effect can fire multiple times as assignments settle (one per terminal
    // creation). Process only sessions whose zone now has an assignment, and
    // leave the rest in the ref for the next assignments tick.
    const remaining: ZoneSessionInfo[] = [];
    for (const s of sessions) {
      const tabId = zoneLayout.assignments[s.zoneIndex];
      if (tabId && SESSION_ID_RE.test(s.claudeSessionId)) {
        updateTab(tabId, {
          claudeSessionId: s.claudeSessionId,
          claudeConfigDir: s.claudeConfigDir,
        });
        writeWhenReady(
          terminalRefs.current,
          tabId,
          buildResumeCmd(s.claudeSessionId, s.claudeConfigDir),
          {
            onTimeout: (id) =>
              console.warn(
                `[TerminalCore] profile resume: terminal ref for ${id} never became ready`,
              ),
          },
        );
      } else {
        remaining.push(s);
      }
    }
    pendingProfileSessionsRef.current = remaining.length > 0 ? remaining : null;
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
