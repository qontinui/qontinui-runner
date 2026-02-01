/**
 * useRenderLogManager Hook
 *
 * React hook that creates and manages a ui-bridge RenderLogManager
 * with Tauri storage integration.
 */

import { useEffect, useRef, useCallback, useMemo } from "react";
import { RenderLogManager, type RenderLogEntry } from "ui-bridge";
import { TauriRenderLogStorage } from "./TauriRenderLogStorage";

export interface UseRenderLogManagerOptions {
  /** Enable the render log manager */
  enabled?: boolean;
  /** Current active tab/route (for metadata) */
  activeTab?: string;
  /** Task run ID (for backend association) */
  taskRunId?: string | number;
  /** Capture on navigation changes */
  captureOnNavigation?: boolean;
  /** Capture DOM changes */
  captureChanges?: boolean;
  /** Debounce time for change captures (ms) */
  changeDebounceMs?: number;
  /** Capture only interactive elements */
  interactiveOnly?: boolean;
  /** Include hidden elements */
  includeHidden?: boolean;
  /** Maximum entries to keep */
  maxEntries?: number;
  /** Callback when entry is added */
  onEntry?: (entry: RenderLogEntry) => void;
}

export interface RenderLogManagerHandle {
  /** Capture a full DOM snapshot */
  captureSnapshot: (trigger?: string) => Promise<void>;
  /** Clear the render log */
  clear: () => Promise<void>;
  /** Get recent entries */
  getEntries: (limit?: number) => Promise<RenderLogEntry[]>;
  /** Get entry count */
  getCount: () => Promise<number>;
  /** Whether the manager is running */
  isRunning: boolean;
}

/**
 * Hook that manages a ui-bridge RenderLogManager with Tauri integration
 */
export function useRenderLogManager(
  options: UseRenderLogManagerOptions = {}
): RenderLogManagerHandle {
  const {
    enabled = true,
    activeTab,
    taskRunId,
    captureOnNavigation = true,
    captureChanges = true,
    interactiveOnly = false,
    includeHidden = false,
    maxEntries = 1000,
    onEntry,
  } = options;

  const isDev = import.meta.env.DEV;
  const isEnabled = enabled && isDev;

  // Create storage and manager once
  const storageRef = useRef<TauriRenderLogStorage | null>(null);
  const managerRef = useRef<RenderLogManager | null>(null);
  const isRunningRef = useRef(false);
  const lastTabRef = useRef<string | undefined>(undefined);

  // Initialize storage and manager
  useEffect(() => {
    if (!isEnabled) return;

    // Create storage with Tauri integration
    storageRef.current = new TauriRenderLogStorage({ maxEntries, taskRunId });

    // Create render log manager
    managerRef.current = new RenderLogManager({
      storage: storageRef.current,
      captureOnNavigation,
      captureChanges,
      captureOptions: {
        interactiveOnly,
        includeHidden,
        maxTextLength: 200,
        maxElements: 5000,
      },
      maxEntries,
      onEntry,
    });

    // Start the manager
    managerRef.current.start();
    isRunningRef.current = true;

    console.debug("[useRenderLogManager] Started render log manager");

    return () => {
      managerRef.current?.stop();
      managerRef.current = null;
      storageRef.current = null;
      isRunningRef.current = false;
      console.debug("[useRenderLogManager] Stopped render log manager");
    };
  }, [
    isEnabled,
    captureOnNavigation,
    captureChanges,
    interactiveOnly,
    includeHidden,
    maxEntries,
    onEntry,
  ]);

  // Update task run ID when it changes
  useEffect(() => {
    if (storageRef.current && taskRunId !== undefined) {
      storageRef.current.setTaskRunId(taskRunId);
    }
  }, [taskRunId]);

  // Capture snapshot on tab change
  useEffect(() => {
    if (!isEnabled || !managerRef.current) return;

    // Skip if tab hasn't changed
    if (lastTabRef.current === activeTab) return;
    const previousTab = lastTabRef.current;
    lastTabRef.current = activeTab;

    // Skip initial mount (handled by manager.start())
    if (previousTab === undefined) return;

    // Delay slightly to let new content render
    const timeoutId = setTimeout(() => {
      managerRef.current?.captureSnapshot({
        trigger: "tab_change",
        previousTab,
        newTab: activeTab,
      });
    }, 100);

    return () => clearTimeout(timeoutId);
  }, [isEnabled, activeTab]);

  // Memoized handler functions
  const captureSnapshot = useCallback(
    async (trigger = "manual") => {
      if (!managerRef.current) return;
      await managerRef.current.captureSnapshot({
        trigger,
        activeTab,
        taskRunId,
      });
    },
    [activeTab, taskRunId]
  );

  const clear = useCallback(async () => {
    if (!managerRef.current) return;
    await managerRef.current.clear();
  }, []);

  const getEntries = useCallback(async (limit?: number) => {
    if (!managerRef.current) return [];
    return managerRef.current.getEntries({ limit });
  }, []);

  const getCount = useCallback(async () => {
    if (!managerRef.current) return 0;
    return managerRef.current.count();
  }, []);

  return useMemo(
    () => ({
      captureSnapshot,
      clear,
      getEntries,
      getCount,
      isRunning: isRunningRef.current,
    }),
    [captureSnapshot, clear, getEntries, getCount]
  );
}
