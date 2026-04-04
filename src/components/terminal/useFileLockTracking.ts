/**
 * Tracks file lock states per terminal tab.
 *
 * Uses two data sources:
 * 1. Tauri events (`file-lock-waiting`, `file-lock-acquired`) for real-time waiting state
 * 2. Polling `/file-locks/info` for which sessions currently hold locks
 *
 * Events use `holder_name` to match tabs by title (since task_run_id and
 * claudeSessionId are different identifiers). Polling uses holder_name too.
 */

import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { TerminalTab } from "./useTerminalManager";

export type FileLockState = "holding" | "waiting" | null;

interface FileLockEvent {
  type: string;
  file_path: string;
  task_run_id: string;
  holder_name: string;
  blocked_by?: string;
}

interface FileLockInfoEntry {
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  acquired_at: number;
}

export function useFileLockTracking(tabs: TerminalTab[]): Record<string, FileLockState> {
  const [lockStates, setLockStates] = useState<Record<string, FileLockState>>({});
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  // Track which holder_names are in "waiting" state (set by events, cleared by poll)
  const waitingHolders = useRef(new Set<string>());

  // Find tab ID by holder_name (matches tab title)
  const findTabByHolderName = (holderName: string): string | undefined => {
    for (const tab of tabsRef.current) {
      if (tab.title === holderName) return tab.id;
    }
    return undefined;
  };

  // Listen for file-lock events
  useEffect(() => {
    let unlistenWaiting: (() => void) | null = null;
    let unlistenAcquired: (() => void) | null = null;

    listen<FileLockEvent>("file-lock-waiting", (event) => {
      const { holder_name } = event.payload;
      waitingHolders.current.add(holder_name);
      const tabId = findTabByHolderName(holder_name);
      if (tabId) {
        setLockStates((prev) => ({ ...prev, [tabId]: "waiting" }));
      }
    }).then((fn) => {
      unlistenWaiting = fn;
    });

    listen<FileLockEvent>("file-lock-acquired", (event) => {
      const { holder_name } = event.payload;
      waitingHolders.current.delete(holder_name);
      const tabId = findTabByHolderName(holder_name);
      if (tabId) {
        setLockStates((prev) => ({ ...prev, [tabId]: "holding" }));
      }
    }).then((fn) => {
      unlistenAcquired = fn;
    });

    return () => {
      unlistenWaiting?.();
      unlistenAcquired?.();
    };
  }, []);

  // Poll /file-locks/info to detect which tabs hold locks
  useEffect(() => {
    let active = true;

    const poll = async () => {
      try {
        const port =
          typeof window !== "undefined" &&
          (window as unknown as Record<string, unknown>).__QONTINUI_PORT__
            ? Number((window as unknown as Record<string, unknown>).__QONTINUI_PORT__)
            : 9876;
        const resp = await fetch(`http://127.0.0.1:${port}/file-locks/info`);
        if (!resp.ok || !active) return;
        const locks = (await resp.json()) as FileLockInfoEntry[];

        // Build set of holder_names that hold locks
        const holdingNames = new Set(locks.map((l) => l.holder_name));

        setLockStates(() => {
          const next: Record<string, FileLockState> = {};
          for (const tab of tabsRef.current) {
            if (holdingNames.has(tab.title)) {
              // Tab holds locks — if it was waiting, it has now acquired
              waitingHolders.current.delete(tab.title);
              next[tab.id] = "holding";
            } else if (waitingHolders.current.has(tab.title)) {
              next[tab.id] = "waiting";
            } else {
              next[tab.id] = null;
            }
          }
          return next;
        });
      } catch {
        // Silently fail
      }
    };

    poll();
    const interval = setInterval(poll, 10_000);
    return () => {
      active = false;
      clearInterval(interval);
    };
  }, []);

  return lockStates;
}
