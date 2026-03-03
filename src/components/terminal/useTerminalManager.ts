import { useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface TerminalTab {
  id: string;
  title: string;
  pid: number | null;
  isAlive: boolean;
  exitCode: number | null;
  workingDir?: string;
  createdAt?: number;
  /** True while the frontend is replaying the scrollback buffer from Rust. */
  isReconnecting?: boolean;
}

interface TerminalInfo {
  id: string;
  title: string;
  pid: number | null;
  cols: number;
  rows: number;
  working_dir: string;
  is_alive: boolean;
  exit_code: number | null;
  created_at: number;
  total_bytes_produced: number;
}

interface CommandResponse {
  success: boolean;
  message: string | null;
  data: Record<string, unknown> | null;
}

export function useTerminalManager() {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const nextTitleNum = useRef(1);
  const [initialized, setInitialized] = useState(false);

  /**
   * Reconnect to existing Rust PTY sessions that survived a React remount.
   * Returns true if sessions were found and restored.
   */
  const reconnectToExistingSessions = useCallback(async (): Promise<boolean> => {
    try {
      const result = await invoke<CommandResponse>("terminal_list");
      if (!result.success || !result.data) return false;

      const terminals = (result.data as { terminals: TerminalInfo[] }).terminals;
      if (!terminals || terminals.length === 0) return false;

      console.log(`[TerminalManager] Reconnecting to ${terminals.length} existing PTY session(s)`);

      // Rebuild tabs from Rust session data (already sorted by created_at)
      const reconnectedTabs: TerminalTab[] = terminals.map((info) => ({
        id: info.id,
        title: info.title,
        pid: info.pid ?? null,
        isAlive: info.is_alive,
        exitCode: info.exit_code ?? null,
        workingDir: info.working_dir || undefined,
        createdAt: info.created_at,
        isReconnecting: true,
      }));

      // Update nextTitleNum to avoid collisions
      for (const tab of reconnectedTabs) {
        const match = tab.title.match(/^Terminal (\d+)$/);
        if (match) {
          nextTitleNum.current = Math.max(nextTitleNum.current, parseInt(match[1], 10) + 1);
        }
      }

      setTabs(reconnectedTabs);
      // Select the last tab (most recently created)
      setActiveId(reconnectedTabs[reconnectedTabs.length - 1].id);
      return true;
    } catch (err) {
      console.error("[TerminalManager] Failed to reconnect:", err);
      return false;
    }
  }, []);

  /** Mark a tab as having completed reconnection (buffer replayed). */
  const markReconnected = useCallback((id: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, isReconnecting: false } : t)));
  }, []);

  const createTerminal = useCallback(async (title?: string): Promise<string | null> => {
    try {
      const displayTitle = title ?? `Terminal ${nextTitleNum.current++}`;
      const result = await invoke<CommandResponse>("terminal_create", {
        title: displayTitle,
      });

      if (!result.success || !result.data) return null;

      const info = result.data as unknown as TerminalInfo;
      const tab: TerminalTab = {
        id: info.id,
        title: info.title,
        pid: info.pid ?? null,
        isAlive: info.is_alive,
        exitCode: info.exit_code ?? null,
        workingDir: info.working_dir || undefined,
        createdAt: info.created_at,
      };

      setTabs((prev) => [...prev, tab]);
      setActiveId(info.id);
      return info.id;
    } catch (err) {
      console.error("Failed to create terminal:", err);
      return null;
    }
  }, []);

  const closeTerminal = useCallback(async (id: string) => {
    try {
      await invoke<CommandResponse>("terminal_close", { terminalId: id });
    } catch {
      // Terminal may already be gone
    }

    setTabs((prev) => {
      const next = prev.filter((t) => t.id !== id);
      setActiveId((currentActive) => {
        if (currentActive !== id) return currentActive;
        const closedIndex = prev.findIndex((t) => t.id === id);
        return next[Math.min(closedIndex, next.length - 1)]?.id ?? null;
      });
      return next;
    });
  }, []);

  const renameTab = useCallback((id: string, title: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, title } : t)));
  }, []);

  const updateTab = useCallback(
    (id: string, updates: Partial<Pick<TerminalTab, "isAlive" | "exitCode">>) => {
      setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, ...updates } : t)));
    },
    [],
  );

  return {
    tabs,
    activeId,
    setActiveId,
    initialized,
    setInitialized,
    createTerminal,
    closeTerminal,
    renameTab,
    updateTab,
    reconnectToExistingSessions,
    markReconnected,
  };
}
