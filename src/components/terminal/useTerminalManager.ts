import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TerminalInfo } from "@qontinui/shared-types/tauri-events";
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
  /**
   * Coordinator `task_run_id` for tabs backed by a registered `WorkerSession`.
   * Presence is the worker marker — `ZoneGrid::onTitleChange` skips the local
   * `renameTab` + backend `terminal_set_title` invoke so worker tabs stay
   * pinned at `Worker N` in the tab strip. Mirrors the Phase 1 backend gate
   * (`set_title_unless_worker`) on the frontend side.
   */
  taskRunId?: string;
}

/** Tauri event payload from `commands::productivity::spawn_worker_session`. */
interface WorkerRegisteredPayload {
  terminalId: string;
  taskRunId: string;
}

/**
 * Pure helper: decide whether `tabs` should be replaced when applying a
 * worker mark for `terminalId`. Returns the next tabs array (same identity
 * if no change), and a `buffered` flag the caller uses to record the mark
 * in `pendingWorkerMarks` when the tab record hasn't arrived yet.
 *
 * Exported so `useTerminalManager.test.ts` can drive the race-safety + idempotency
 * contract without booting React.
 */
export function applyWorkerMark(
  tabs: TerminalTab[],
  terminalId: string,
  taskRunId: string,
): { tabs: TerminalTab[]; buffered: boolean } {
  const idx = tabs.findIndex((t) => t.id === terminalId);
  if (idx < 0) return { tabs, buffered: true };
  if (tabs[idx].taskRunId === taskRunId) return { tabs, buffered: false };
  const next = tabs.slice();
  next[idx] = { ...tabs[idx], taskRunId };
  return { tabs: next, buffered: false };
}

// `TerminalInfo` is imported from `@qontinui/shared-types/tauri-events` —
// generated from the canonical Rust struct in
// `qontinui-schemas/rust/src/terminal.rs`. Field names are camelCase via
// `#[serde(rename_all = "camelCase")]`. Future serde renames break this
// file at compile time instead of silently dropping events.

export function useTerminalManager(pageId: string = "default") {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const nextTitleNum = useRef(1);
  const [initialized, setInitialized] = useState(false);
  /**
   * Worker marks (`terminalId → taskRunId`) received from the Rust side
   * before their tab record exists in React state. The Tauri command path
   * emits `terminal-created` then `worker-registered` in order, but their
   * arrival order at the webview is not strictly guaranteed; on reconnect
   * we also call `list_workers` while `terminal_list` is mid-flight, so a
   * buffer is the simplest race-safe shape.
   */
  const pendingWorkerMarks = useRef<Map<string, string>>(new Map());

  const markAsWorker = useCallback((terminalId: string, taskRunId: string) => {
    setTabs((prev) => {
      const result = applyWorkerMark(prev, terminalId, taskRunId);
      if (result.buffered) {
        pendingWorkerMarks.current.set(terminalId, taskRunId);
      }
      return result.tabs;
    });
  }, []);

  // Listen for terminals created externally (e.g. via HTTP API) and add them as tabs.
  // Deduplicates by checking if the terminal ID already exists in state.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<TerminalInfo>("terminal-created", (event) => {
      const info = event.payload;
      // Drain the race buffer OUTSIDE the reducer so the `setTabs` updater
      // stays pure (React StrictMode double-invokes updaters in dev; a
      // delete-inside-reducer would miss on the second pass).
      const pendingTaskRunId = pendingWorkerMarks.current.get(info.id);
      if (pendingTaskRunId !== undefined) {
        pendingWorkerMarks.current.delete(info.id);
      }
      setTabs((prev) => {
        if (prev.some((t) => t.id === info.id)) return prev;
        logger.info(`External terminal created: ${info.id} (${info.title})`);
        return [
          ...prev,
          {
            id: info.id,
            title: info.title,
            pid: info.pid ?? null,
            isAlive: info.isAlive,
            exitCode: info.exitCode ?? null,
            workingDir: info.workingDir || undefined,
            createdAt: info.createdAt,
            taskRunId: pendingTaskRunId,
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

  // Mirror the Phase 1 backend worker gate on the frontend. The Rust side
  // emits `worker-registered` right after `SessionManager::register_worker`
  // succeeds in `commands::productivity::spawn_worker_session`; consuming
  // it here lets `ZoneGrid::onTitleChange` skip OSC 0/2 `renameTab` for
  // worker pty tabs.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<WorkerRegisteredPayload>("worker-registered", (event) => {
      const { terminalId, taskRunId } = event.payload;
      if (!terminalId || !taskRunId) return;
      markAsWorker(terminalId, taskRunId);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [markAsWorker]);

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
      const pageTerminals = terminals.filter((t) => (t.pageId || "default") === pageId);

      // Only reconnect to alive sessions; silently close dead ones
      const dead = pageTerminals.filter((t) => !t.isAlive);
      const alive = pageTerminals.filter((t) => t.isAlive);

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
        isAlive: info.isAlive,
        exitCode: info.exitCode ?? null,
        workingDir: info.workingDir || undefined,
        createdAt: info.createdAt,
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

      // Backfill the Phase 2 worker marker for reconnected tabs. The Rust
      // `SessionManager` keeps `WorkerSession` registrations across reloads
      // of the React tree, so a `worker-registered` event won't re-fire for
      // these tabs — we have to ask. Fire-and-forget; if it fails the
      // worker tabs simply lose the gate on the frontend (backend gate
      // still holds).
      invoke<Array<{ terminal_id: string; task_run_id: string }>>("list_workers")
        .then((workers) => {
          for (const w of workers) {
            if (w.terminal_id && w.task_run_id) {
              markAsWorker(w.terminal_id, w.task_run_id);
            }
          }
        })
        .catch((err) => {
          logger.warn(`list_workers backfill failed: ${err}`);
        });

      return reconnectedTabs.map((t) => t.id);
    } catch (err) {
      console.error("[TerminalManager] Failed to reconnect:", err);
      return null;
    }
  }, [pageId, markAsWorker]);

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
          isAlive: info.isAlive,
          exitCode: info.exitCode ?? null,
          workingDir: info.workingDir || undefined,
          createdAt: info.createdAt,
        };

        setTabs((prev) => {
          // Deduplicate: the terminal-created event listener may have already added this tab
          if (prev.some((t) => t.id === info.id)) return prev;
          return [...prev, tab];
        });
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
    markAsWorker,
  };
}
