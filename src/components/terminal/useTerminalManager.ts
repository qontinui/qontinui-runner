import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CommandResponse } from "./types";
import { createLogger } from "@/lib/logger";

const logger = createLogger("TerminalManager");

export interface TerminalTab {
  id: string;
  title: string;
  pid: number | null;
  isAlive: boolean;
  exitCode: number | null;
  workingDir?: string;
  createdAt?: number;
  /** Tab type: "terminal" (default) or "plan" (markdown viewer) */
  type?: "terminal" | "plan";
  /** Absolute path to the markdown file (only for plan tabs) */
  planFilePath?: string;
  /** True while the frontend is replaying the scrollback buffer from Rust. */
  isReconnecting?: boolean;
  /** Claude Code session ID running in this tab (set on resume). */
  claudeSessionId?: string;
  /** Claude config dir for the session (set on resume). */
  claudeConfigDir?: string;
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
  page_id?: string;
}

export function useTerminalManager(pageId: string = "default") {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const nextTitleNum = useRef(1);
  const [initialized, setInitialized] = useState(false);

  // Listen for terminals created externally (e.g. via HTTP API) and add them as tabs.
  // Deduplicates by checking if the terminal ID already exists in state.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<TerminalInfo>("terminal-created", (event) => {
      const info = event.payload;
      setTabs((prev) => {
        if (prev.some((t) => t.id === info.id)) return prev;
        logger.info(`External terminal created: ${info.id} (${info.title})`);
        return [
          ...prev,
          {
            id: info.id,
            title: info.title,
            pid: info.pid ?? null,
            isAlive: info.is_alive,
            exitCode: info.exit_code ?? null,
            workingDir: info.working_dir || undefined,
            createdAt: info.created_at,
          },
        ];
      });
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  /**
   * Reconnect to existing Rust PTY sessions that survived a React remount.
   * Returns the ordered list of reconnected tab IDs, or null if no sessions found.
   */
  const reconnectToExistingSessions = useCallback(async (): Promise<string[] | null> => {
    try {
      const result = await invoke<CommandResponse>("terminal_list");
      if (!result.success || !result.data) return null;

      const terminals = (result.data as { terminals: TerminalInfo[] }).terminals;
      if (!terminals || terminals.length === 0) return null;

      // Filter to terminals belonging to this page
      const pageTerminals = terminals.filter((t) => (t.page_id || "default") === pageId);

      // Only reconnect to alive sessions; silently close dead ones
      const dead = pageTerminals.filter((t) => !t.is_alive);
      const alive = pageTerminals.filter((t) => t.is_alive);

      for (const t of dead) {
        invoke("terminal_close", { terminalId: t.id }).catch(() => {});
      }

      if (alive.length === 0) return null;

      logger.info(`Reconnecting to ${alive.length} existing PTY session(s)`);

      // Rebuild tabs from Rust session data (already sorted by created_at)
      const reconnectedTabs: TerminalTab[] = alive.map((info) => ({
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
      return reconnectedTabs.map((t) => t.id);
    } catch (err) {
      console.error("[TerminalManager] Failed to reconnect:", err);
      return null;
    }
  }, [pageId]);

  /** Mark a tab as having completed reconnection (buffer replayed). */
  const markReconnected = useCallback((id: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, isReconnecting: false } : t)));
  }, []);

  const createTerminal = useCallback(
    async (title?: string, workingDir?: string): Promise<string | null> => {
      try {
        const displayTitle = title ?? `Terminal ${nextTitleNum.current++}`;
        const result = await invoke<CommandResponse>("terminal_create", {
          title: displayTitle,
          workingDir: workingDir ?? null,
          pageId: pageId !== "default" ? pageId : null,
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
    },
    [pageId],
  );

  const createPlanTab = useCallback((filePath: string): string => {
    const fileName = filePath.replace(/\\/g, "/").split("/").pop() || "Plan";
    const id = `plan-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const tab: TerminalTab = {
      id,
      title: fileName,
      pid: null,
      isAlive: true,
      exitCode: null,
      type: "plan",
      planFilePath: filePath,
      createdAt: Date.now(),
    };
    setTabs((prev) => [...prev, tab]);
    setActiveId(id);
    return id;
  }, []);

  const closeTerminal = useCallback((id: string) => {
    // Update React state immediately so the UI is responsive.
    // The Rust-side close (process kill + thread join) runs in the background.
    setTabs((prev) => {
      const next = prev.filter((t) => t.id !== id);
      setActiveId((currentActive) => {
        if (currentActive !== id) return currentActive;
        const closedIndex = prev.findIndex((t) => t.id === id);
        return next[Math.min(closedIndex, next.length - 1)]?.id ?? null;
      });
      return next;
    });

    // Only invoke Rust close for terminal tabs (plan tabs have no PTY)
    if (!id.startsWith("plan-")) {
      invoke<CommandResponse>("terminal_close", { terminalId: id }).catch(() => {
        // Terminal may already be gone
      });
    }
  }, []);

  const renameTab = useCallback((id: string, title: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, title } : t)));
  }, []);

  const updateTab = useCallback(
    (
      id: string,
      updates: Partial<
        Pick<
          TerminalTab,
          "isAlive" | "exitCode" | "workingDir" | "claudeSessionId" | "claudeConfigDir"
        >
      >,
    ) => {
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
    createPlanTab,
    closeTerminal,
    renameTab,
    updateTab,
    reconnectToExistingSessions,
    markReconnected,
  };
}
