import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import type { CommandResponse } from "./types";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface SavedSessionConfig {
  /** Original tab title */
  title: string;
  /** Working directory the terminal was opened in */
  workingDir?: string;
  /** Which zone this tab was assigned to (-1 for unassigned) */
  zoneIndex: number;
  /** User-assigned label/tag for this zone */
  label?: string;
  /** User-assigned notes for this zone */
  notes?: string;
  /** Whether the zone was pinned */
  pinned?: boolean;
  /** Timestamp of when this was saved */
  savedAt: number;
  /** Path to saved scrollback file (if any) */
  scrollbackPath?: string;
  /** Whether this was a Claude Code session */
  isClaudeSession?: boolean;
  /** Claude session ID for resume */
  claudeSessionId?: string;
  /** Claude config dir */
  claudeConfigDir?: string;
  /** Tab type: "terminal" (default) or "plan" */
  type?: "terminal" | "plan";
  /** File path for plan tabs */
  planFilePath?: string;
}

export interface SavedSessionLayout {
  /** Layout preset ID (e.g., "quad", "six", "nine") */
  layoutId: string;
  /** Session configs, one per zone */
  sessions: SavedSessionConfig[];
  /** When the layout was saved */
  savedAt: number;
  /** Focused zone index */
  focusedZone: number;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STORAGE_KEY = "qontinui-terminal-session-layout";
const DEBOUNCE_MS = 500;
const STALE_THRESHOLD_MS = 24 * 60 * 60 * 1000; // 24 hours

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export interface SaveSessionLayoutParams {
  layoutId: string;
  tabs: Array<{
    id: string;
    title: string;
    workingDir?: string;
    claudeSessionId?: string;
    claudeConfigDir?: string;
    type?: "terminal" | "plan";
    planFilePath?: string;
  }>;
  assignments: Record<number, string>; // zoneIndex -> tabId
  zoneLabels: Record<number, string>;
  zoneNotes: Record<number, string>;
  pinnedZones: Set<number>;
  focusedZone: number;
}

export function useSessionPersistence() {
  const debounceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clean up debounce timer on unmount
  useEffect(() => {
    return () => {
      if (debounceTimer.current !== null) {
        clearTimeout(debounceTimer.current);
      }
    };
  }, []);

  const saveSessionLayout = useCallback((params: SaveSessionLayoutParams) => {
    if (debounceTimer.current !== null) {
      clearTimeout(debounceTimer.current);
    }

    debounceTimer.current = setTimeout(() => {
      const { layoutId, tabs, assignments, zoneLabels, zoneNotes, pinnedZones, focusedZone } =
        params;

      const now = Date.now();
      const tabsById = new Map(tabs.map((t) => [t.id, t]));
      const assignedTabIds = new Set<string>();
      const sessions: SavedSessionConfig[] = [];

      // Build configs for assigned zones
      for (const [zoneIndexStr, tabId] of Object.entries(assignments)) {
        const zoneIndex = Number(zoneIndexStr);
        const tab = tabsById.get(tabId);
        if (!tab) continue;

        assignedTabIds.add(tabId);
        sessions.push({
          title: tab.title,
          workingDir: tab.workingDir,
          zoneIndex,
          label: zoneLabels[zoneIndex] || undefined,
          notes: zoneNotes[zoneIndex] || undefined,
          pinned: pinnedZones.has(zoneIndex) || undefined,
          savedAt: now,
          isClaudeSession: !!tab.claudeSessionId,
          claudeSessionId: tab.claudeSessionId,
          claudeConfigDir: tab.claudeConfigDir,
          type: tab.type,
          planFilePath: tab.planFilePath,
        });
      }

      // Include unassigned tabs at the end with zoneIndex: -1
      for (const tab of tabs) {
        if (assignedTabIds.has(tab.id)) continue;
        sessions.push({
          title: tab.title,
          workingDir: tab.workingDir,
          zoneIndex: -1,
          savedAt: now,
          isClaudeSession: !!tab.claudeSessionId,
          claudeSessionId: tab.claudeSessionId,
          claudeConfigDir: tab.claudeConfigDir,
          type: tab.type,
          planFilePath: tab.planFilePath,
        });
      }

      const layout: SavedSessionLayout = {
        layoutId,
        sessions,
        savedAt: now,
        focusedZone,
      };

      try {
        instanceStorage.setJSON(STORAGE_KEY, layout);
      } catch {
        // storage may be full or unavailable — silently ignore
      }
    }, DEBOUNCE_MS);
  }, []);

  /**
   * Save scrollback buffers for all active terminals to disk.
   * Returns a map of tabId -> file path for each successfully saved buffer.
   * Best-effort: failures are logged but do not throw.
   */
  const saveScrollbackBuffers = useCallback(
    async (tabs: Array<{ id: string }>): Promise<Record<string, string>> => {
      const pathMap: Record<string, string> = {};

      const results = await Promise.allSettled(
        tabs.map(async (tab) => {
          try {
            const result = await invoke<CommandResponse>("terminal_save_scrollback", {
              terminalId: tab.id,
            });
            if (result.success && result.data) {
              const path = (result.data as { path: string | null }).path;
              if (path) {
                pathMap[tab.id] = path;
              }
            }
          } catch (err) {
            console.warn(`[SessionPersistence] Failed to save scrollback for ${tab.id}:`, err);
          }
        }),
      );

      // Log any unexpected rejections
      for (const r of results) {
        if (r.status === "rejected") {
          console.warn("[SessionPersistence] Scrollback save rejected:", r.reason);
        }
      }

      return pathMap;
    },
    [],
  );

  /**
   * Update the persisted layout with scrollback paths.
   * Called after saveScrollbackBuffers to add file paths to the saved config.
   */
  const updateScrollbackPaths = useCallback(
    (pathMap: Record<string, string>, tabIdToSessionIndex: Record<string, number>) => {
      try {
        const layout = instanceStorage.getJSON<SavedSessionLayout | null>(STORAGE_KEY, null);
        if (!layout) return;

        for (const [tabId, path] of Object.entries(pathMap)) {
          const sessionIndex = tabIdToSessionIndex[tabId];
          if (sessionIndex !== undefined && layout.sessions[sessionIndex]) {
            layout.sessions[sessionIndex].scrollbackPath = path;
          }
        }

        instanceStorage.setJSON(STORAGE_KEY, layout);
      } catch {
        // Best-effort
      }
    },
    [],
  );

  const getSavedLayout = useCallback((): SavedSessionLayout | null => {
    return instanceStorage.getJSON<SavedSessionLayout | null>(STORAGE_KEY, null);
  }, []);

  const clearSavedLayout = useCallback(() => {
    instanceStorage.removeItem(STORAGE_KEY);
  }, []);

  const hasSavedLayout = useCallback((): boolean => {
    const layout = getSavedLayout();
    if (!layout) return false;
    return Date.now() - layout.savedAt < STALE_THRESHOLD_MS;
  }, [getSavedLayout]);

  return {
    saveSessionLayout,
    saveScrollbackBuffers,
    updateScrollbackPaths,
    getSavedLayout,
    clearSavedLayout,
    hasSavedLayout,
  };
}
