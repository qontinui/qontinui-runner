import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { getApiPort } from "@/lib/runner-api";

export interface ConflictHolder {
  task_run_id: string;
  holder_name: string;
}

export interface FileConflict {
  file_path: string;
  other_holders: ConflictHolder[];
}

export interface FileRegistryEntry {
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  registered_at: number;
}

interface ConflictEvent {
  type: string;
  file_path: string;
  task_run_id: string;
  holder_name: string;
  conflicts: FileConflict[];
}

/**
 * Hook to track file conflicts across concurrent sessions.
 *
 * Listens for real-time conflict events from the runner and periodically
 * polls the file registry for the full state.
 */
export function useFileConflicts() {
  const apiPort = getApiPort();
  const [registry, setRegistry] = useState<FileRegistryEntry[]>([]);
  const [recentAlert, setRecentAlert] = useState<ConflictEvent | null>(null);
  const dismissTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Fetch full registry state
  const refreshRegistry = useCallback(async () => {
    try {
      const res = await fetch(`http://127.0.0.1:${apiPort}/file-registry/info`);
      if (res.ok) {
        const data: FileRegistryEntry[] = await res.json();
        setRegistry(data);
      }
    } catch {
      // Silently fail — registry may not be available yet
    }
  }, [apiPort]);

  // Listen for real-time conflict detection events
  useEffect(() => {
    const unlisten = listen<ConflictEvent>("file-conflict-detected", (event) => {
      setRecentAlert(event.payload);

      // Clear previous timer before setting a new one
      if (dismissTimerRef.current) {
        clearTimeout(dismissTimerRef.current);
      }
      dismissTimerRef.current = setTimeout(() => setRecentAlert(null), 10000);

      // Refresh the full registry
      refreshRegistry();
    });

    return () => {
      unlisten.then((fn) => fn());
      if (dismissTimerRef.current) {
        clearTimeout(dismissTimerRef.current);
      }
    };
  }, [refreshRegistry]);

  // Poll registry every 30 seconds
  useEffect(() => {
    refreshRegistry();
    const interval = setInterval(refreshRegistry, 30000);
    return () => clearInterval(interval);
  }, [refreshRegistry]);

  // Derive conflicts: files held by multiple sessions (excluding self from each session's view)
  const conflicts = useMemo(() => {
    const fileMap = new Map<string, FileRegistryEntry[]>();
    for (const entry of registry) {
      if (!fileMap.has(entry.file_path)) {
        fileMap.set(entry.file_path, []);
      }
      fileMap.get(entry.file_path)!.push(entry);
    }

    const result: FileConflict[] = [];
    for (const [filePath, entries] of fileMap) {
      // Only a conflict if 2+ distinct sessions hold the file
      const uniqueHolders = new Set(entries.map((e) => e.holder_task_run_id));
      if (uniqueHolders.size > 1) {
        // Deduplicate holders by task_run_id
        const seen = new Set<string>();
        const holders: ConflictHolder[] = [];
        for (const entry of entries) {
          if (!seen.has(entry.holder_task_run_id)) {
            seen.add(entry.holder_task_run_id);
            holders.push({
              task_run_id: entry.holder_task_run_id,
              holder_name: entry.holder_name,
            });
          }
        }
        result.push({ file_path: filePath, other_holders: holders });
      }
    }
    return result;
  }, [registry]);

  // Per-session conflict count: how many conflicting files does each session have?
  const sessionConflictCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const conflict of conflicts) {
      for (const holder of conflict.other_holders) {
        counts.set(holder.task_run_id, (counts.get(holder.task_run_id) ?? 0) + 1);
      }
    }
    return counts;
  }, [conflicts]);

  const dismissAlert = useCallback(() => {
    setRecentAlert(null);
    if (dismissTimerRef.current) {
      clearTimeout(dismissTimerRef.current);
      dismissTimerRef.current = null;
    }
  }, []);

  return {
    /** Files currently held by multiple sessions */
    conflicts,
    /** Full registry snapshot */
    registry,
    /** Map of session task_run_id → number of conflicting files */
    sessionConflictCounts,
    /** Most recent conflict alert (auto-dismisses after 10s) */
    recentAlert,
    /** Dismiss the current alert */
    dismissAlert,
    /** Force refresh the registry */
    refreshRegistry,
  };
}
