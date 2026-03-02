import { useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface TerminalTab {
  id: string;
  title: string;
  pid: number | null;
  isAlive: boolean;
  exitCode: number | null;
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
    createTerminal,
    closeTerminal,
    renameTab,
    updateTab,
  };
}
