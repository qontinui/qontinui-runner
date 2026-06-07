import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LAYOUT_PRESETS } from "./useZoneLayout";
import { writeWhenReady } from "./writeWhenReady";
import type { TerminalTab } from "./useTerminalManager";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { CommandResponse, TerminalSessionRecord } from "./types";
import type { SaveSessionLayoutParams } from "./useSessionPersistence";
import { rememberSessionId } from "./lastKnownSessionIds";

/**
 * Fetch the durable RESTORABLE session records for `pageId` from the backend
 * registry, filtered to this page and deduped by `claudeSessionId` (defensive
 * — the registry should already enforce one row per id, but a duplicate would
 * otherwise spawn two tabs for one session). This is the SOURCE OF TRUTH for
 * the zone↔session binding on restore, replacing the ephemeral-tabId
 * creation-order mapping the localStorage snapshot used to drive.
 *
 * `terminal_session_list_open` returns the restorable superset: `open` records
 * (hard-crash case) PLUS in-grace `closed`/`pty-exit` records (graceful-restart
 * case, where `handleExit` flipped every live PTY to `closed`). The backend
 * owns the state/reason/grace gating, so we deliberately do NOT re-filter on
 * `state === "open"` here — that would drop the pty-exit records the backend
 * just decided are restorable.
 *
 * Exported for unit testing the restore-binding logic without booting React.
 */
export async function fetchOpenRecords(pageId: string): Promise<TerminalSessionRecord[]> {
  let resp: CommandResponse | null;
  try {
    resp = await invoke<CommandResponse>("terminal_session_list_open");
  } catch (err) {
    console.warn("[TerminalPage] terminal_session_list_open failed:", err);
    return [];
  }
  const sessions = (resp?.data as { sessions?: TerminalSessionRecord[] } | undefined)?.sessions;
  if (!Array.isArray(sessions)) return [];
  const byId = new Map<string, TerminalSessionRecord>();
  for (const rec of sessions) {
    if (!rec || typeof rec.claudeSessionId !== "string") continue;
    if ((rec.pageId ?? "default") !== pageId) continue;
    if (!byId.has(rec.claudeSessionId)) byId.set(rec.claudeSessionId, rec);
  }
  return [...byId.values()];
}

/** Build a `claude --resume <id>` command, optionally prefixed with CLAUDE_CONFIG_DIR. */
function buildResumeCmd(sessionId: string, configDir: string | undefined): string {
  const base = `claude --resume ${sessionId}`;
  if (!configDir) return `${base}\r`;
  const isWindows = navigator.platform.startsWith("Win");
  return isWindows
    ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}\r`
    : `CLAUDE_CONFIG_DIR="${configDir}" ${base}\r`;
}

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
  /** Which terminal page this restore runs for ("default" when unset). */
  pageId: string;
  tabs: TerminalTab[];
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  reconnectToExistingSessions: () => Promise<string[] | null>;
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  createPlanTab: (filePath: string) => string | null;
  setInitialized: (v: boolean) => void;
  updateTab: (
    id: string,
    updates: Partial<{
      claudeSessionId?: string;
      claudeConfigDir?: string;
      isReconnecting?: boolean;
    }>,
  ) => void;
  zoneLayout: {
    layoutId: string;
    setLayoutId: (id: string, opts?: { pinned?: boolean }) => void;
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
  pageId,
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

  // Gate for the debounced auto-save effect. The restore path recreates
  // plain shells and only *asynchronously* types `claude --resume <id>` /
  // re-attaches `claudeSessionId`. If the debounced auto-save fires while
  // those tabs are still plain (no claudeSessionId yet), it persists the
  // degraded layout and clobbers the good saved Claude layout — leaving
  // nothing to resume on the next reopen. We keep auto-save suppressed
  // until restore has fully drained (resume commands issued / ids merged),
  // then open the gate exactly once. The flag is opened in a `finally` so
  // it ALWAYS flips — even when there were no saved sessions or restore
  // threw — so brand-new sessions still persist normally.
  const restoreCompleteRef = useRef(false);

  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;

    (async () => {
      // True once a deferred resume/scrollback drain timer is scheduled. When
      // set, that timer owns flipping `restoreCompleteRef` (after it issues the
      // resume commands); the `finally` below only opens the gate when NO drain
      // was scheduled, so the flag always flips exactly once.
      let drainScheduled = false;
      try {
        // 1) Reconnect to live PTYs that survived a React remount. These tabs
        //    are plain (no claudeSessionId on the wire) but their ids are the
        //    SAME stable terminal ids the registry recorded under `terminalId`.
        const reconnectedTabIds = await reconnectToExistingSessions();
        const reconnectedSet = new Set(reconnectedTabIds ?? []);

        // 2) The durable backend session registry is the SOURCE OF TRUTH for
        //    which Claude sessions exist and their zones. The localStorage
        //    snapshot is demoted to cosmetics only (layout / labels / notes /
        //    pins / focusedZone / scrollback) — matched by zoneIndex.
        const openRecords = await fetchOpenRecords(pageId);

        // Cosmetic snapshot — never the resumable Claude set / zone binding.
        const saved = sessionPersistence.hasSavedLayout()
          ? sessionPersistence.getSavedLayout()
          : null;

        // Per-zone cosmetics lookups from the snapshot (matched by zoneIndex).
        const cosmeticsByZone = new Map<
          number,
          { label?: string; notes?: string; pinned?: boolean; scrollbackPath?: string }
        >();
        if (saved) {
          for (const s of saved.sessions) {
            if (s.zoneIndex < 0) continue;
            cosmeticsByZone.set(s.zoneIndex, {
              label: s.label,
              notes: s.notes,
              pinned: s.pinned,
              scrollbackPath: s.scrollbackPath,
            });
          }
        }

        const applyZoneCosmetics = (zoneIndex: number) => {
          if (zoneIndex < 0) return;
          const c = cosmeticsByZone.get(zoneIndex);
          if (!c) return;
          if (c.label) labelsAndTags.setZoneLabel(zoneIndex, c.label);
          if (c.notes) labelsAndTags.setZoneNote(zoneIndex, c.notes);
          if (c.pinned) labelsAndTags.setPinnedZones((prev) => new Set([...prev, zoneIndex]));
        };

        // Restore the layout preset from the cosmetic snapshot if it differs.
        // A saved snapshot is the operator's deliberate arrangement, so pin it
        // — auto-grow must not override a restored layout.
        if (saved && saved.layoutId !== zoneLayout.layoutId) {
          const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
          if (preset) zoneLayout.setLayoutId(preset.id, { pinned: true });
        }

        // 3) Bind every open Claude record to its RECORDED zone — Claude zones
        //    are claimed from records FIRST so the creation-order auto-fill in
        //    `useZoneLayout` can never steal a zone a record owns (it only fills
        //    zones still empty after this loop runs).
        for (const rec of openRecords) {
          const safeConfigDir = sanitizeConfigDir(rec.configDir);
          const validSessionId = isValidSessionId(rec.claudeSessionId);

          // a) A live reconnected PTY is already running this session (React
          //    remount): match by the record's stable terminalId. Just rebind
          //    the zone + re-attach the claudeSessionId; no resume needed.
          if (reconnectedSet.has(rec.terminalId)) {
            const tabId = rec.terminalId;
            if (rec.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(rec.zoneIndex, tabId);
              applyZoneCosmetics(rec.zoneIndex);
            }
            updateTab(tabId, {
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: safeConfigDir,
            });
            rememberSessionId(tabId, rec.claudeSessionId, safeConfigDir);
            continue;
          }

          // b) Cold restart (no live pty): recreate the tab, bind its recorded
          //    zone, attach the session id, and queue a `claude --resume` via
          //    the existing drain loop. Re-assert the OPEN record under the new
          //    ephemeral terminal id so the registry tracks the live tab.
          const tabId = await createTerminal(rec.title, rec.workingDir);
          if (!tabId) continue;
          if (rec.zoneIndex >= 0) {
            zoneLayout.assignTabToZone(rec.zoneIndex, tabId);
            applyZoneCosmetics(rec.zoneIndex);
          }
          updateTab(tabId, {
            claudeSessionId: rec.claudeSessionId,
            claudeConfigDir: safeConfigDir,
            // Show a "resuming" affordance until `claude --resume` lands;
            // cleared in the drain loop after the resume command is written.
            isReconnecting: true,
          });
          rememberSessionId(tabId, rec.claudeSessionId, safeConfigDir);

          if (validSessionId) {
            pendingRestoresRef.current.push({
              tabId,
              scrollbackPath:
                rec.zoneIndex >= 0 ? cosmeticsByZone.get(rec.zoneIndex)?.scrollbackPath : undefined,
              isClaudeSession: true,
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: safeConfigDir,
            });
          }

          // Re-assert the OPEN record under the freshly created terminal id so
          // the registry's `terminalId` tracks the live tab (the next restart
          // reconnect-matches on this id).
          invoke("terminal_session_record_open", {
            claudeSessionId: rec.claudeSessionId,
            configDir: rec.configDir,
            workingDir: rec.workingDir,
            pageId,
            zoneIndex: rec.zoneIndex,
            title: rec.title,
            terminalId: tabId,
          }).catch((err) => {
            console.warn(`[TerminalPage] re-record open failed for ${rec.claudeSessionId}:`, err);
          });
        }

        // 4) Plan tabs are cosmetic-only state held in the snapshot (no PTY, no
        //    registry record) — recreate them so the markdown viewers survive a
        //    cold restart. Skip on a React remount: live plan tabs are gone with
        //    the unmounted tree but their snapshot entry still re-creates them
        //    only when we cold-started (no reconnected pty tabs).
        if (saved && !reconnectedTabIds) {
          for (const session of saved.sessions) {
            if (session.type !== "plan" || !session.planFilePath) continue;
            const tabId = createPlanTab(session.planFilePath);
            if (tabId && session.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(session.zoneIndex, tabId);
              applyZoneCosmetics(session.zoneIndex);
            }
          }
        }

        // 5) Focused zone from the cosmetic snapshot.
        if (saved && saved.focusedZone >= 0) {
          zoneLayout.setFocusedZone(saved.focusedZone);
        }

        // 6) Drain: restore scrollback then issue `claude --resume` for every
        //    cold-created Claude tab. Identical mechanism to the prior restore;
        //    the gate (`restoreCompleteRef`) flips in the drain's finally.
        if (pendingRestoresRef.current.length > 0) {
          drainScheduled = true;
          setTimeout(async () => {
            try {
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
                  isValidSessionId(restore.claudeSessionId)
                ) {
                  try {
                    const resumeCmd = buildResumeCmd(
                      restore.claudeSessionId,
                      restore.claudeConfigDir,
                    );
                    await new Promise((r) => setTimeout(r, 500));
                    writeWhenReady(terminalRefs.current, restore.tabId, resumeCmd, {
                      onTimeout: (id) =>
                        console.warn(
                          `[TerminalPage] resume: terminal ref for ${id} never became ready`,
                        ),
                    });
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to resume Claude session for ${restore.tabId}:`,
                      err,
                    );
                  } finally {
                    // Resume command issued (or best-effort failed) — drop the
                    // "resuming" affordance so the live `claude` UI shows.
                    updateTab(restore.tabId, { isReconnecting: false });
                  }
                }
              }

              try {
                await invoke("terminal_cleanup_scrollback");
              } catch (err) {
                console.warn("[TerminalPage] Failed to cleanup scrollback files:", err);
              }

              pendingRestoresRef.current = [];
            } finally {
              // Restored tabs now carry their claudeSessionId / scrollback —
              // safe to let the debounced auto-save persist the layout.
              restoreCompleteRef.current = true;
            }
          }, 1500);
        }

        // NOTE: deliberately do NOT clearSavedLayout(). The cosmetic snapshot
        // must survive until the debounced auto-save overwrites it (after the
        // drain has run and tabs hold their ids). The resumable Claude set now
        // comes from the registry, so a failed/never-firing resume no longer
        // risks losing sessions — the registry record persists regardless.
        // No default terminal — start empty so users can launch AI sessions via the Launch Menu.
        setInitialized(true);
      } finally {
        // Always open the auto-save gate. If a drain timer was scheduled it
        // owns the flip (after resume commands are issued); otherwise — no
        // saved sessions, nothing to restore, or restore threw — open it now
        // so brand-new sessions still persist. Never leave it permanently
        // closed (that would silently disable persistence).
        if (!drainScheduled) {
          restoreCompleteRef.current = true;
        }
      }
    })();
  }, [
    pageId,
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
    // Suppress auto-save until restore has fully completed. Otherwise the
    // debounced save can fire while the restore path still holds plain shells
    // (no claudeSessionId yet) and clobber the good saved Claude layout. Once
    // the gate opens, the `updateTab` calls that attach claudeSessionId mutate
    // `tabs`, re-running this effect — so no save is permanently lost.
    if (!restoreCompleteRef.current) return;
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

  // Save scrollback buffers to disk when the window is about to close.
  //
  // NOTE: we deliberately do NOT record session CLOSEs to the durable registry
  // here. Window teardown is ambiguous with a supervisor restart — the same
  // `onCloseRequested` fires whether the user is quitting for good or the
  // supervisor is bouncing the app. Recording closes on window teardown would
  // mark still-live Claude sessions as closed and drop them from the next
  // restore. Closes are recorded only on EXPLICIT tab close (useTerminalManager
  // `closeTerminal`) and on pty-exit (TerminalPage `handleExit`).
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
