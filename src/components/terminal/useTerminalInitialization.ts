import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LAYOUT_PRESETS } from "./useZoneLayout";
import type { TerminalTab } from "./useTerminalManager";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { CommandResponse } from "./types";
import type { SaveSessionLayoutParams } from "./useSessionPersistence";

/** Validate session IDs before interpolating into shell commands. */
const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
function isValidSessionId(id: string): boolean {
  return SESSION_ID_RE.test(id);
}

/** Validate config dir paths — reject shell metacharacters. */
const SAFE_PATH_RE = /^[a-zA-Z0-9_\-./\\: ]+$/;
function sanitizeConfigDir(dir: string | undefined): string | undefined {
  if (!dir) return undefined;
  return SAFE_PATH_RE.test(dir) ? dir : undefined;
}

interface UseTerminalInitializationParams {
  tabs: TerminalTab[];
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  reconnectToExistingSessions: () => Promise<string[] | null>;
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  createPlanTab: (filePath: string) => string | null;
  setInitialized: (v: boolean) => void;
  updateTab: (
    id: string,
    updates: Partial<{ claudeSessionId?: string; claudeConfigDir?: string }>,
  ) => void;
  zoneLayout: {
    layoutId: string;
    setLayoutId: (id: string) => void;
    assignTabToZone: (zoneIdx: number, tabId: string) => void;
    setFocusedZone: (zoneIdx: number) => void;
    assignments: Record<number, string>;
  };
  labelsAndTags: {
    setZoneLabel: (zoneIdx: number, label: string) => void;
    setZoneNote: (zoneIdx: number, note: string) => void;
    setPinnedZones: React.Dispatch<React.SetStateAction<Set<number>>>;
  };
  sessionPersistence: {
    saveSessionLayout: (params: SaveSessionLayoutParams) => void;
    saveScrollbackBuffers: (tabs: Array<{ id: string }>) => Promise<Record<string, string>>;
    updateScrollbackPaths: (
      pathMap: Record<string, string>,
      tabIdToSessionIndex: Record<string, number>,
    ) => void;
    getSavedLayout: () => {
      layoutId: string;
      focusedZone: number;
      sessions: Array<{
        zoneIndex: number;
        title: string;
        workingDir?: string;
        type?: "terminal" | "plan";
        planFilePath?: string;
        scrollbackPath?: string;
        isClaudeSession?: boolean;
        claudeSessionId?: string;
        claudeConfigDir?: string;
        label?: string;
        notes?: string;
        pinned?: boolean;
      }>;
    } | null;
    clearSavedLayout: () => void;
    hasSavedLayout: () => boolean;
  };
  layoutState: {
    layoutId: string;
    zoneLabels: Record<number, string>;
    zoneNotes: Record<number, string>;
    pinnedZones: Set<number>;
    focusedZone: number;
  };
}

export function useTerminalInitialization({
  tabs,
  terminalRefs,
  reconnectToExistingSessions,
  createTerminal,
  createPlanTab,
  setInitialized,
  updateTab,
  zoneLayout,
  labelsAndTags,
  sessionPersistence,
  layoutState,
}: UseTerminalInitializationParams) {
  const mountCountRef = useRef(0);
  useEffect(() => {
    mountCountRef.current += 1;
    const mountNum = mountCountRef.current;
    if (mountNum > 1) {
      console.warn(
        `[TerminalPage] REMOUNTED (mount #${mountNum}) — all terminal tabs were lost. ` +
          `This usually means the parent component tree unmounted (e.g., auth state change).`,
      );
    }
    return () => {
      console.warn(
        `[TerminalPage] UNMOUNTED (was mount #${mountNum}), ${tabs.length} tab(s) will be destroyed`,
      );
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const didInit = useRef(false);
  const pendingRestoresRef = useRef<
    Array<{
      tabId: string;
      scrollbackPath?: string;
      isClaudeSession?: boolean;
      claudeSessionId?: string;
      claudeConfigDir?: string;
    }>
  >([]);

  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;

    (async () => {
      const reconnectedTabIds = await reconnectToExistingSessions();

      // Even when reconnection succeeds, merge saved session metadata
      // (claudeSessionId, labels, etc.) that the PTY data doesn't carry.
      // We use the returned tab IDs directly (ordered by creation time,
      // matching the sequential zone auto-assignment) to avoid reading
      // stale zoneLayout.assignments from this closure.
      if (reconnectedTabIds) {
        const saved = sessionPersistence.hasSavedLayout()
          ? sessionPersistence.getSavedLayout()
          : null;
        if (saved && saved.sessions.length > 0) {
          // Restore layout preset if it differs
          if (saved.layoutId !== zoneLayout.layoutId) {
            const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
            if (preset) zoneLayout.setLayoutId(preset.id);
          }

          for (const session of saved.sessions) {
            // Match by zone index: tabs are auto-assigned to zones sequentially
            const tabId =
              session.zoneIndex >= 0 && session.zoneIndex < reconnectedTabIds.length
                ? reconnectedTabIds[session.zoneIndex]
                : undefined;

            // Restore zone labels, notes, pins
            if (session.zoneIndex >= 0) {
              if (session.label) {
                labelsAndTags.setZoneLabel(session.zoneIndex, session.label);
              }
              if (session.notes) {
                labelsAndTags.setZoneNote(session.zoneIndex, session.notes);
              }
              if (session.pinned) {
                labelsAndTags.setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
              }
            }

            // Merge Claude session ID into the reconnected tab and queue resume
            if (tabId && session.claudeSessionId && isValidSessionId(session.claudeSessionId)) {
              const safeConfigDir = sanitizeConfigDir(session.claudeConfigDir);
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: safeConfigDir,
              });
              pendingRestoresRef.current.push({
                tabId,
                isClaudeSession: true,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: safeConfigDir,
              });
            }
          }

          if (saved.focusedZone >= 0) {
            zoneLayout.setFocusedZone(saved.focusedZone);
          }

          // Auto-resume Claude sessions after terminals are ready
          if (pendingRestoresRef.current.length > 0) {
            setTimeout(async () => {
              for (const restore of pendingRestoresRef.current) {
                if (restore.isClaudeSession && restore.claudeSessionId) {
                  const ref = terminalRefs.current.get(restore.tabId);
                  const handle = ref?.current;
                  if (handle) {
                    try {
                      const resumeCmd = `claude --resume ${restore.claudeSessionId}\r`;
                      await new Promise((r) => setTimeout(r, 500));
                      handle.writeToTerminal(resumeCmd);
                    } catch (err) {
                      console.warn(
                        `[TerminalPage] Failed to resume Claude session for ${restore.tabId}:`,
                        err,
                      );
                    }
                  }
                }
              }
              pendingRestoresRef.current = [];
            }, 1500);
          }

          sessionPersistence.clearSavedLayout();
        }
      }

      if (!reconnectedTabIds) {
        const saved = sessionPersistence.hasSavedLayout()
          ? sessionPersistence.getSavedLayout()
          : null;
        if (saved && saved.sessions.length > 0) {
          if (saved.layoutId !== zoneLayout.layoutId) {
            const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
            if (preset) zoneLayout.setLayoutId(preset.id);
          }

          const assignedSessions = saved.sessions.filter((s) => s.zoneIndex >= 0);
          const unassignedSessions = saved.sessions.filter((s) => s.zoneIndex < 0);

          for (const session of assignedSessions) {
            if (session.type === "plan" && session.planFilePath) {
              const tabId = createPlanTab(session.planFilePath);
              if (tabId && session.zoneIndex >= 0) {
                zoneLayout.assignTabToZone(session.zoneIndex, tabId);
              }
              if (session.label) {
                labelsAndTags.setZoneLabel(session.zoneIndex, session.label);
              }
              if (session.notes) {
                labelsAndTags.setZoneNote(session.zoneIndex, session.notes);
              }
              if (session.pinned) {
                labelsAndTags.setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
              }
              continue;
            }

            const tabId = await createTerminal(session.title, session.workingDir);
            if (tabId && session.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(session.zoneIndex, tabId);
            }
            if (session.label) {
              labelsAndTags.setZoneLabel(session.zoneIndex, session.label);
            }
            if (session.notes) {
              labelsAndTags.setZoneNote(session.zoneIndex, session.notes);
            }
            if (session.pinned) {
              labelsAndTags.setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
            }
            if (tabId && (session.scrollbackPath || session.isClaudeSession)) {
              pendingRestoresRef.current.push({
                tabId,
                scrollbackPath: session.scrollbackPath,
                isClaudeSession: session.isClaudeSession,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: sanitizeConfigDir(session.claudeConfigDir),
              });
            }
            if (tabId && session.claudeSessionId) {
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: sanitizeConfigDir(session.claudeConfigDir),
              });
            }
          }

          for (const session of unassignedSessions) {
            if (session.type === "plan" && session.planFilePath) {
              createPlanTab(session.planFilePath);
              continue;
            }
            const tabId = await createTerminal(session.title, session.workingDir);
            if (tabId && (session.scrollbackPath || session.isClaudeSession)) {
              pendingRestoresRef.current.push({
                tabId,
                scrollbackPath: session.scrollbackPath,
                isClaudeSession: session.isClaudeSession,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: sanitizeConfigDir(session.claudeConfigDir),
              });
            }
            if (tabId && session.claudeSessionId) {
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: sanitizeConfigDir(session.claudeConfigDir),
              });
            }
          }

          if (saved.focusedZone >= 0) {
            zoneLayout.setFocusedZone(saved.focusedZone);
          }

          if (pendingRestoresRef.current.length > 0) {
            setTimeout(async () => {
              for (const restore of pendingRestoresRef.current) {
                const ref = terminalRefs.current.get(restore.tabId);
                const handle = ref?.current;

                if (restore.scrollbackPath && handle) {
                  try {
                    const result = await invoke<CommandResponse>("terminal_get_saved_scrollback", {
                      filePath: restore.scrollbackPath,
                    });
                    if (result.success && result.data) {
                      const encoded = (result.data as { data: string }).data;
                      if (encoded) {
                        const raw = atob(encoded);
                        const decoded = new TextDecoder().decode(
                          Uint8Array.from(raw, (c) => c.charCodeAt(0)),
                        );
                        handle.writeToDisplay(decoded);
                      }
                    }
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to restore scrollback for ${restore.tabId}:`,
                      err,
                    );
                  }
                }

                if (
                  restore.isClaudeSession &&
                  restore.claudeSessionId &&
                  isValidSessionId(restore.claudeSessionId) &&
                  handle
                ) {
                  try {
                    const resumeCmd = `claude --resume ${restore.claudeSessionId}\r`;
                    await new Promise((r) => setTimeout(r, 500));
                    handle.writeToTerminal(resumeCmd);
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to resume Claude session for ${restore.tabId}:`,
                      err,
                    );
                  }
                }
              }

              try {
                await invoke("terminal_cleanup_scrollback");
              } catch (err) {
                console.warn("[TerminalPage] Failed to cleanup scrollback files:", err);
              }

              pendingRestoresRef.current = [];
            }, 1500);
          }

          sessionPersistence.clearSavedLayout();
        } else {
          await createTerminal();
        }
      }
      setInitialized(true);
    })();
  }, [
    reconnectToExistingSessions,
    createTerminal,
    createPlanTab,
    setInitialized,
    sessionPersistence,
    zoneLayout,
    labelsAndTags,
    updateTab,
    terminalRefs,
  ]);

  // Auto-save session layout for persistence across app restarts
  useEffect(() => {
    if (tabs.length === 0) return;
    sessionPersistence.saveSessionLayout({
      layoutId: layoutState.layoutId,
      tabs,
      assignments: zoneLayout.assignments,
      zoneLabels: layoutState.zoneLabels,
      zoneNotes: layoutState.zoneNotes,
      pinnedZones: layoutState.pinnedZones,
      focusedZone: layoutState.focusedZone,
    });
  }, [
    tabs,
    zoneLayout.assignments,
    layoutState.layoutId,
    layoutState.focusedZone,
    layoutState.zoneLabels,
    layoutState.zoneNotes,
    layoutState.pinnedZones,
    sessionPersistence,
  ]);

  // Refs for unmount/close handlers that need latest values
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const zoneLayoutRef = useRef(zoneLayout);
  useEffect(() => {
    zoneLayoutRef.current = zoneLayout;
  }, [zoneLayout]);
  const layoutStateRef = useRef(layoutState);
  useEffect(() => {
    layoutStateRef.current = layoutState;
  }, [layoutState]);

  // Immediate save on unmount (page switch) — the debounced auto-save may not
  // have flushed, so we save synchronously to avoid losing state.
  useEffect(() => {
    return () => {
      const ls = layoutStateRef.current;
      const currentTabs = tabsRef.current;
      if (currentTabs.length > 0) {
        sessionPersistence.saveSessionLayout({
          layoutId: ls.layoutId,
          tabs: currentTabs,
          assignments: zoneLayoutRef.current.assignments,
          zoneLabels: ls.zoneLabels,
          zoneNotes: ls.zoneNotes,
          pinnedZones: ls.pinnedZones,
          focusedZone: ls.focusedZone,
        });
      }
    };
  }, [sessionPersistence]);

  // Save scrollback buffers to disk when the window is about to close

  const handleWindowClose = useCallback(async () => {
    const currentTabs = tabsRef.current;
    if (currentTabs.length === 0) return;

    try {
      const terminalTabs = currentTabs.filter((t) => t.type !== "plan");
      const pathMap = await sessionPersistence.saveScrollbackBuffers(terminalTabs);
      const tabIdToSessionIndex: Record<string, number> = {};
      const currentAssignments = zoneLayoutRef.current.assignments;
      const assignedTabIds = new Set(Object.values(currentAssignments));
      let idx = 0;
      for (const [, tabId] of Object.entries(currentAssignments)) {
        if (currentTabs.some((t) => t.id === tabId)) {
          tabIdToSessionIndex[tabId] = idx++;
        }
      }
      for (const tab of currentTabs) {
        if (!assignedTabIds.has(tab.id)) {
          tabIdToSessionIndex[tab.id] = idx++;
        }
      }
      sessionPersistence.updateScrollbackPaths(pathMap, tabIdToSessionIndex);
    } catch (err) {
      console.warn("[TerminalPage] Failed to save scrollback on close:", err);
    }
  }, [sessionPersistence]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    getCurrentWindow()
      .onCloseRequested(async () => {
        await handleWindowClose();
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [handleWindowClose]);
}
