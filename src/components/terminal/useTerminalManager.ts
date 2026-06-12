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
  /**
   * True when a boot-restore typed `claude --resume` into this tab but the
   * Claude UI handshake never appeared (after one retry) — the pane is most
   * likely still a bare shell. Surfaced as an explicit operator-clickable
   * "resume failed — retry" affordance (`ResumeFailedBanner`); cleared when a
   * retry verifies. While set, the durable record keeps its backend
   * restore-pending marker so the liveness poll can't flip it `poll-dead`.
   */
  resumeFailed?: boolean;
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
  /**
   * True when the PTY child runs Claude with tool permissions bypassed
   * (`--dangerously-skip-permissions` or `--permission-mode bypassPermissions`).
   * Set from the Rust `terminal-bypass-permissions` event, which the runner
   * emits (after `terminal-created`) when it command-sniffs a bypass flag on
   * the spawn command. Threaded into `useSessionStateTracking` →
   * `detectSessionState` so approval-shaped TTY patterns are suppressed for
   * these sessions — a bypass session can never await tool approval, so any
   * approval-shaped match is a phantom (the 12-min `rm -rf` misread,
   * 2026-06-07; see `sessionStateDetector.ts`). Absent/false on every
   * non-bypass tab.
   */
  bypassPermissions?: boolean;
  /**
   * Marks a tab synthesized from a debug-gated test-fixtures `injected_tab`
   * spec (`syntheticTabs.ts`). Synthetic tabs are fed ONLY to
   * `useSessionStateTracking` + `useSessionManager` for StatusStrip
   * bucketing — they never back a real PTY and must never render a terminal
   * pane. Phase 3 uses this flag for inertness guards. Absent/false on every
   * real tab.
   */
  __synthetic?: boolean;
}

/** Tauri event payload from `commands::productivity::spawn_worker_session`. */
interface WorkerRegisteredPayload {
  terminalId: string;
  taskRunId: string;
}

/**
 * Pure helper: resolve the durable session-CLOSE record args for a tab that is
 * being EXPLICITLY closed by the user. Returns `null` when the tab has no
 * `claudeSessionId` (a plain shell — nothing to record). Exported so the close-
 * recording contract can be unit-tested without booting React (vitest runs in
 * a `node` environment with no React Testing Library — see existing
 * `useTerminalManager.test.ts`).
 */
export function buildSessionCloseRecord(
  tabs: TerminalTab[],
  id: string,
): { claudeSessionId: string; reason: "explicit" } | null {
  const closing = tabs.find((t) => t.id === id);
  if (!closing?.claudeSessionId) return null;
  return { claudeSessionId: closing.claudeSessionId, reason: "explicit" };
}

/**
 * Pure helper: decide whether a `terminal-created` event belongs to the page
 * this manager instance is scoped to. With the session provider lifted above
 * the page (so every page's manager is mounted simultaneously), each page's
 * `terminal-created` listener must claim ONLY the terminals created for its own
 * page — otherwise a terminal created for page B would be ingested into every
 * page's tab slice.
 *
 * Older wire forms that omit `pageId` hydrate to `"default"` on the Rust side
 * (`TerminalInfo`), so an undefined/empty value here is treated as "default".
 *
 * Exported so `useTerminalManager.test.ts` can drive the page-routing contract
 * without booting React.
 */
export function shouldIngestCreatedTerminal(
  eventPageId: string | undefined | null,
  pageId: string,
): boolean {
  return (eventPageId || "default") === pageId;
}

/**
 * Pure helper: fold a `terminal-created` payload into the existing tab list.
 * Returns the next tabs array — the SAME identity when the terminal already
 * exists (dedup), otherwise a new array with the tab appended. `pendingTaskRunId`
 * is the worker mark drained from the race buffer by the caller (or `undefined`);
 * `pendingBypass` is the bypass-permissions mark drained the same way (a
 * `terminal-bypass-permissions` event that arrived before this `terminal-created`).
 *
 * Exported so `useTerminalManager.test.ts` can drive the ingest + dedup contract
 * without booting React.
 */
export function reduceCreatedTerminal(
  tabs: TerminalTab[],
  info: TerminalInfo,
  pendingTaskRunId: string | undefined,
  pendingBypass = false,
): TerminalTab[] {
  if (tabs.some((t) => t.id === info.id)) return tabs;
  return [
    ...tabs,
    {
      id: info.id,
      title: info.title,
      pid: info.pid ?? null,
      isAlive: info.isAlive,
      exitCode: info.exitCode ?? null,
      workingDir: info.workingDir || undefined,
      createdAt: info.createdAt,
      taskRunId: pendingTaskRunId,
      bypassPermissions: pendingBypass || undefined,
    },
  ];
}

/**
 * Pure helper: resolve the `activeId` to set after a `terminal-created` ingest.
 *
 * Returns `info.id` when this is a genuinely-NEW tab (`isNewTab === true`) so a
 * freshly-docked gate continuation is auto-selected the moment it lands —
 * surfacing it the same way the frontend-initiated `createTerminal` path does
 * (`setActiveId(info.id)`). Returns `null` (caller keeps the current selection)
 * when the ingest is a dedup'd re-delivery so a re-delivered event never steals
 * focus.
 *
 * Exported so `useTerminalManager.test.ts` can drive the auto-select contract
 * without booting React (the listener is a thin `if (id) setActiveId(id)`).
 */
export function nextActiveIdAfterIngest(info: TerminalInfo, isNewTab: boolean): string | null {
  return isNewTab ? info.id : null;
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

/**
 * Pure helper: decide whether `tabs` should be replaced when applying a
 * bypass-permissions mark for `terminalId`. Returns the next tabs array (same
 * identity if no change), and a `buffered` flag the caller uses to record the
 * mark in `pendingBypassMarks` when the tab record hasn't arrived yet.
 *
 * Mirrors `applyWorkerMark` — the `terminal-bypass-permissions` event can
 * arrive before OR after `terminal-created` lands the tab in React state.
 * Exported so `useTerminalManager.test.ts` can drive the race-safety +
 * idempotency contract without booting React.
 */
export function applyBypassMark(
  tabs: TerminalTab[],
  terminalId: string,
): { tabs: TerminalTab[]; buffered: boolean } {
  const idx = tabs.findIndex((t) => t.id === terminalId);
  if (idx < 0) return { tabs, buffered: true };
  if (tabs[idx].bypassPermissions === true) return { tabs, buffered: false };
  const next = tabs.slice();
  next[idx] = { ...tabs[idx], bypassPermissions: true };
  return { tabs: next, buffered: false };
}

// `TerminalInfo` is imported from `@qontinui/shared-types/tauri-events` —
// generated from the canonical Rust struct in
// `qontinui-schemas/rust/src/terminal.rs`. Field names are camelCase via
// `#[serde(rename_all = "camelCase")]`. Future serde renames break this
// file at compile time instead of silently dropping events.

export function useTerminalManager(pageId: string = "default", windowLabel: string = "main") {
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
  /**
   * Bypass-permissions marks (`terminalId`) received from the Rust
   * `terminal-bypass-permissions` event before their tab record exists in
   * React state. Same race shape as `pendingWorkerMarks`: the event is emitted
   * right after `terminal-created`, but arrival order at the webview is not
   * strictly guaranteed (and reconnect rebuilds tabs without re-firing it).
   */
  const pendingBypassMarks = useRef<Set<string>>(new Set());
  /**
   * Ids this manager has already ingested via `terminal-created`. Drives the
   * auto-select decision (`nextActiveIdAfterIngest`) OUTSIDE the `setTabs`
   * updater so the updater stays pure under StrictMode double-invoke — a
   * re-delivered `terminal-created` is a no-op here (already present) and never
   * re-steals focus, mirroring `reduceCreatedTerminal`'s id-dedup.
   */
  const ingestedIds = useRef<Set<string>>(new Set());

  const markAsWorker = useCallback((terminalId: string, taskRunId: string) => {
    setTabs((prev) => {
      const result = applyWorkerMark(prev, terminalId, taskRunId);
      if (result.buffered) {
        pendingWorkerMarks.current.set(terminalId, taskRunId);
      }
      return result.tabs;
    });
  }, []);

  const markAsBypass = useCallback((terminalId: string) => {
    setTabs((prev) => {
      const result = applyBypassMark(prev, terminalId);
      if (result.buffered) {
        pendingBypassMarks.current.add(terminalId);
      }
      return result.tabs;
    });
  }, []);

  // Listen for terminals created externally (e.g. via HTTP API) and add them as
  // tabs. This is the ONLY live-ingest path for externally-created terminals
  // (e.g. a docked gate continuation). With the session provider lifted above
  // TerminalPage (so every page's manager is mounted at once), the listener
  // ALWAYS runs regardless of which page the operator is viewing — a
  // `terminal-created` event is therefore never dropped by a page switch. Each
  // page's listener claims ONLY the terminals tagged with its own `pageId`
  // (`shouldIngestCreatedTerminal`), and dedups by id (`reduceCreatedTerminal`).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<TerminalInfo>("terminal-created", (event) => {
      const info = event.payload;
      // Route the event to the page it belongs to. A terminal created for
      // another page is ignored here — its own page's manager will claim it.
      if (!shouldIngestCreatedTerminal(info.pageId, pageId)) return;
      // Drain the race buffer OUTSIDE the reducer so the `setTabs` updater
      // stays pure (React StrictMode double-invokes updaters in dev; a
      // delete-inside-reducer would miss on the second pass).
      const pendingTaskRunId = pendingWorkerMarks.current.get(info.id);
      if (pendingTaskRunId !== undefined) {
        pendingWorkerMarks.current.delete(info.id);
      }
      const pendingBypass = pendingBypassMarks.current.has(info.id);
      if (pendingBypass) {
        pendingBypassMarks.current.delete(info.id);
      }
      // Decide auto-select OUTSIDE the `setTabs` updater so the updater stays
      // pure (StrictMode double-invokes updaters in dev). `ingestedIds` is a
      // ref-backed dedup set so a re-delivered `terminal-created` is a no-op
      // here just as `reduceCreatedTerminal` dedups it in the updater — only a
      // genuinely-new tab triggers selection, so a re-delivery never steals
      // focus. The pure decision lives in `nextActiveIdAfterIngest`, fed a
      // synthetic prev/next pair reflecting whether this id is new.
      const wasNew = !ingestedIds.current.has(info.id);
      const selectId = nextActiveIdAfterIngest(info, wasNew);
      setTabs((prev) => reduceCreatedTerminal(prev, info, pendingTaskRunId, pendingBypass));
      if (selectId !== null) {
        ingestedIds.current.add(info.id);
        logger.info(`External terminal created: ${info.id} (${info.title}) [page ${pageId}]`);
        // Auto-select the just-appended externally-created tab so a docked gate
        // continuation is surfaced (selected) the moment it lands — matching
        // `createTerminal`'s `setActiveId(info.id)` (the frontend-initiated
        // path). Without this the continuation tab is appended un-selected and
        // the operator sees nothing (the surfacing bug). The App-level
        // `terminal-focus-request` listener handles the complementary main-view
        // switch (`setActiveTab("terminal")`).
        setActiveId(selectId);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [pageId]);

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

  // Bypass-aware needs-input detection (plan
  // `2026-06-07-runner-continuation-defer-and-phantom-needs-input.md`).
  // The Rust `TerminalManager::create` emits `terminal-bypass-permissions`
  // (after `terminal-created`) when the spawn command implies bypassed tool
  // permissions; marking the tab here lets `useSessionStateTracking` skip
  // approval-shaped TTY patterns for it (those can only ever be phantoms on a
  // bypass session).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string }>("terminal-bypass-permissions", (event) => {
      const { id } = event.payload;
      if (!id) return;
      markAsBypass(id);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [markAsBypass]);

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
          // Phase 2 (pop-out windows): tag the pane with its owning window so
          // its coord-session identity doesn't collide with a same-(title,cwd)
          // pane in another window. "main" → omitted (legacy/back-compat key).
          windowLabel: windowLabel !== "main" ? windowLabel : null,
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
    [pageId, windowLabel],
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
    // Capture the closing tab's Claude session id (read-only) so we can record
    // an EXPLICIT durable close after the state update. We read it out of the
    // updater's `prev` without mutating inside the updater (StrictMode double-
    // invokes updaters in dev).
    let closeRecord: ReturnType<typeof buildSessionCloseRecord> = null;
    // Update React state immediately so the UI is responsive.
    // The Rust-side close (process kill + thread join) runs in the background.
    setTabs((prev) => {
      closeRecord = buildSessionCloseRecord(prev, id);
      const next = prev.filter((t) => t.id !== id);
      setActiveId((currentActive) => {
        if (currentActive !== id) return currentActive;
        const closedIndex = prev.findIndex((t) => t.id === id);
        return next[Math.min(closedIndex, next.length - 1)]?.id ?? null;
      });
      return next;
    });

    // Record the durable session CLOSE for an explicit user close (only when
    // the tab was running a Claude session). Fire-and-forget.
    if (closeRecord) {
      invoke<CommandResponse>("terminal_session_record_close", closeRecord).catch(() => {
        // Best-effort — the live close still proceeds below.
      });
    }

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
          | "isAlive"
          | "exitCode"
          | "workingDir"
          | "claudeSessionId"
          | "claudeConfigDir"
          | "isReconnecting"
          | "resumeFailed"
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
    markAsBypass,
  };
}
