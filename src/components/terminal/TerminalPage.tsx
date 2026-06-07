import { useEffect, useCallback, useMemo, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent } from "@qontinui/ui-bridge";
import { createLogger } from "@/lib/logger";
import { TerminalNotification } from "./TerminalNotification";
import { FileConflictBanner } from "./FileConflictBanner";
import { SessionManagerPanel } from "./SessionManagerPanel";
import { ZoneGrid } from "./ZoneGrid";
import { ZoneLayoutPicker } from "./ZoneLayoutPicker";
import { DocFinderModal } from "./DocFinderModal";
import { BatchOperationsBar } from "./BatchOperationsBar";
import { ZoneMinimap } from "./ZoneMinimap";
import { ZoneProfilePicker } from "./ZoneProfilePicker";
import {
  useTerminalSession,
  ZoneMetadataProvider,
  useZoneMetadata,
  TransitionEffectsProvider,
  useTransitionEffects,
  UIStateProvider,
  useUIStateCx,
} from "./contexts";
import { ZoneTimeline } from "./ZoneTimeline";
import { ZoneControlPanel } from "./ZoneControlPanel";
import { OutputSearchBar } from "./OutputSearchBar";
import { TerminalRightPanel } from "./TerminalRightPanel";
import { TerminalOverlays } from "./TerminalOverlays";
import { CommandBar } from "./CommandBar";
import { SuggestionsProvider } from "./suggestions";
import { StatusStrip } from "./StatusStrip";
import { UnzonedChip } from "./UnzonedChip";
import { pickLayout } from "./useZoneLayout";
import { ResultCardProvider, ResultCardMount, useResultCard } from "./result-card";

import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { callRegistry, useTerminalCommands } from "./commands";
import { useTerminalInitialization } from "./useTerminalInitialization";
import { useZoneActions } from "./useZoneActions";
import { writeWhenReady as writeWhenReadyHelper } from "./writeWhenReady";
import { setTerminalSessions, type TerminalSessionEntry } from "@/lib/terminal-sessions-registry";
import { UIBridgeComponentScope } from "@qontinui/ui-bridge";
import { useCommitState } from "./useCommitState";
import { useTabSessionIdCapture } from "./useTabSessionIdCapture";
import { buildSessionOpenArgs } from "./sessionRecordArgs";
import { useRegistryAwareness } from "./useRegistryAwareness";
import { useMidSessionProbe, useMidSessionProbeEnabled } from "./useMidSessionProbe";
import { MidSessionToast } from "./MidSessionToast";
import { HoldingLockBanner, shouldShowHoldingBanner } from "./HoldingLockBanner";
import { WaitingLockBanner } from "./WaitingLockBanner";
import { DeconflictAdvisoryBanner } from "./DeconflictAdvisoryBanner";
import { CoordWarningBanner } from "./CoordWarningBanner";

const logger = createLogger("TerminalPage");

interface TerminalPageProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  onSessionCountChange?: (count: number) => void;
}

export function TerminalPage(props: TerminalPageProps) {
  return (
    // Phase 3 (mount-hydration lift): WindowAssignmentsProvider +
    // TerminalSessionProvider were lifted to App.tsx so terminal session state
    // survives page switches and the `terminal-created` listener stays live for
    // every page. The remaining orthogonal UI providers stay here, scoped to
    // the (single, active-page) page tree and reading the active page's slice
    // off the lifted `useTerminalSession()`.
    <ZoneMetadataProvider>
      <TransitionEffectsProvider>
        <UIStateProvider>
          <SuggestionsProvider>
            <ResultCardProvider>
              <TerminalPageInner {...props} />
            </ResultCardProvider>
          </SuggestionsProvider>
        </UIStateProvider>
      </TransitionEffectsProvider>
    </ZoneMetadataProvider>
  );
}

function TerminalPageInner({
  onNavigateToBuilder: _onNavigateToBuilder,
  onNavigateToActive: _onNavigateToActive,
  onSessionCountChange,
}: TerminalPageProps) {
  // Phase 4 — single context replaces the prior 4-way split. Field set
  // is identical (the new context spreads the same returns).
  const session = useTerminalSession();
  const {
    tabs,
    activeId,
    setActiveId,
    initialized,
    setInitialized,
    createTerminal,
    closeTerminal,
    updateTab,
    reconnectToExistingSessions,
    createPlanTab,
    pageId,
    zoneLayout,
    terminalRefs,
    pendingProfileSessionsRef,
  } = session;

  // Result-card surface — the ResultCardProvider wraps TerminalPageInner
  // (see line ~71). `/metrics` + `/history` slash actions pop a card here.
  const { showCard } = useResultCard();

  // Register page-level UI Bridge actions so AI agents can discover
  // and invoke terminal operations without knowing element IDs.
  useUIComponent({
    id: "terminal-page",
    name: "Terminal Page",
    description:
      "Multi-terminal workspace with zone-based layout. Agents can create terminals via HTTP POST /terminals with initialCommand.",
    actions: [
      {
        id: "create-terminal",
        label: "Create Terminal",
        description: "Spawn a new PTY-backed terminal tab and assign it to the next free zone.",
        handler: async () => {
          await createTerminal();
        },
      },
      {
        id: "list-terminals",
        label: "List Terminals",
        description: "Return [{id, title, isAlive}] for every currently mounted terminal tab.",
        // Coerce isAlive to a strict boolean so the field is always present in
        // the response payload — `undefined` values would otherwise be dropped
        // by JSON.stringify on the IPC boundary, leaving callers without the
        // PTY-liveness signal the cheatsheet promises.
        handler: () =>
          tabs.map((t) => ({
            id: t.id,
            title: t.title,
            isAlive: Boolean(t.isAlive),
          })),
      },
      // Phase 1 (pop-out terminal windows) — open/close pop-out OS windows and
      // move terminal tabs between them. Reachable by agents and the operator.
      {
        id: "open-terminal-window",
        label: "Open Terminal Window",
        description:
          "Open a new pop-out OS window (same process) that hosts its own terminal tabs. Returns the new window record { label, kind, ... }.",
        handler: async () => invoke("open_terminal_window", { placement: null }),
      },
      {
        id: "pop-out-active-terminal",
        label: "Pop Out Active Terminal",
        description:
          "Open a new pop-out window and move the active terminal tab into it. Returns { window, terminalId }.",
        handler: async () => {
          if (!activeId) throw new Error("no active terminal to pop out");
          const rec = await invoke<{ label: string }>("open_terminal_window", {
            placement: null,
          });
          await invoke("assign_session_to_window", {
            sessionId: activeId,
            windowLabel: rec.label,
          });
          return { window: rec.label, terminalId: activeId };
        },
      },
      {
        id: "move-terminal-to-window",
        label: "Move Terminal To Window",
        description:
          "Move a terminal tab to a window. Params: { terminalId: string, windowLabel: string } where windowLabel is 'main' or 'term-N'.",
        paramSchema: { terminalId: "string", windowLabel: "string ('main' | 'term-N')" },
        handler: async (params?: unknown) => {
          const { terminalId, windowLabel: target } = (params ?? {}) as {
            terminalId?: string;
            windowLabel?: string;
          };
          if (!terminalId || !target) {
            throw new Error("move-terminal-to-window requires { terminalId, windowLabel }");
          }
          await invoke("assign_session_to_window", {
            sessionId: terminalId,
            windowLabel: target,
          });
          return { ok: true };
        },
      },
      {
        id: "list-runner-windows",
        label: "List Runner Windows",
        description:
          "Return this runner process's own windows [{ label, kind, title }] (main + pop-outs).",
        handler: async () => invoke("list_runner_windows"),
      },
      // P2 (orphan sweep) — operator-initiated cleanup of empty pop-out windows
      // (no live tab). Reuses the same backend sweep the boot-restore path uses.
      // Returns the labels swept.
      {
        id: "close-empty-terminal-windows",
        label: "Close Empty Terminal Windows",
        description:
          "Close every pop-out OS window that has no live terminal tab and prune its record. Returns the labels closed.",
        handler: async () => invoke("close_empty_terminal_windows"),
      },
    ],
  });

  // Phase 9a — hoist the `terminal-launch-menu` UI Bridge registration
  // out of `TerminalTabBar` so external agents keep getting `create-plain`
  // / `create-ai-session` / `create-best-account` / `create-with-command`
  // when the tab bar is eventually removed. Handlers close over the
  // later-declared `handleQuickLaunch` / `handleLaunchAiSession` — fine
  // because they only execute at action-invocation time (after the
  // function body has run through the const declarations).
  //
  // The `open` / `close` actions that used to live here are dropped — they
  // were UI-popover toggles for the LaunchMenu component, not wire
  // contract. External automation calls the spawn actions directly without
  // needing the popover open. If a caller surfaces dependence on them,
  // restore by lifting `showQuickLaunch` state to `TerminalPage` and
  // threading it down to `TerminalTabBar` as props.
  useUIComponent({
    id: "terminal-launch-menu",
    name: "Terminal Launch Menu",
    description:
      "Creates plain terminals and AI sessions. Use create-plain, create-ai-session, " +
      "create-best-account, or create-with-command to launch terminals programmatically.",
    actions: [
      {
        id: "create-plain",
        label: "Create Plain Terminal",
        description: "Spawn N blank terminals using the user's default shell.",
        paramSchema: { count: "number (>= 1, defaults to 1)" },
        handler: async (params?: unknown) => {
          const { count = 1 } = (params ?? {}) as { count?: number };
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-plain requires { count: number } where count >= 1");
          }
          const tabIds = await callRegistry<string[]>("terminal.spawn", { count });
          return {
            success: true,
            tab_ids: tabIds,
            task_run_ids: [] as Array<string | null>,
          };
        },
      },
      {
        id: "create-ai-session",
        label: "Create AI Session",
        description:
          "Spawn N terminals pre-configured to launch `claude` under the given CLAUDE_CONFIG_DIR, optionally pre-typing a context prompt.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          configDir: "string (absolute path to a Claude Code config dir, required)",
          context: "string (optional initial prompt auto-typed after claude starts)",
        },
        handler: async (params?: unknown) => {
          const {
            count = 1,
            configDir,
            context,
          } = (params ?? {}) as {
            count?: number;
            configDir?: string;
            context?: string;
          };
          if (!configDir)
            throw new Error(
              "create-ai-session requires { count?: number, configDir: string, context?: string }",
            );
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-ai-session: count must be a positive number");
          }
          // configDir + the operator's `account` label are different
          // abstractions; the UI Bridge contract takes raw configDir for
          // historical reasons. Call the local closure directly rather
          // than the registry's account-shaped `terminal.spawn-ai`.
          const tabIds = ((await handleLaunchAiSession(count, configDir, context)) ??
            []) as string[];
          return {
            success: true,
            tab_ids: tabIds,
            task_run_ids: tabIds.map(() => null) as Array<string | null>,
          };
        },
      },
      {
        id: "create-best-account",
        label: "Create AI Session with Best Account",
        description:
          "Like create-ai-session, but picks the AI account with the lowest current utilization. Fails if no accounts are configured.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          context: "string (optional initial prompt auto-typed after claude starts)",
        },
        handler: async (params?: unknown) => {
          const { count = 1, context } = (params ?? {}) as {
            count?: number;
            context?: string;
          };
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-best-account: count must be a positive number");
          }
          // Delegate to registry `terminal.spawn-ai` with the literal
          // `account: "best"`. The registry handler does the lowest-
          // utilization lookup; we rethrow `no-account` as the original
          // "No AI accounts available" wording so existing automation
          // regexes keep matching.
          let tabIds: string[];
          try {
            tabIds = await callRegistry<string[]>("terminal.spawn-ai", {
              count,
              account: "best",
              context,
            });
          } catch (err) {
            const msg = err instanceof Error ? err.message : String(err);
            if (msg.includes("no-account") || msg.toLowerCase().includes("no matching")) {
              throw new Error("No AI accounts available", { cause: err });
            }
            throw err;
          }
          return {
            success: true,
            tab_ids: tabIds,
            task_run_ids: tabIds.map(() => null) as Array<string | null>,
          };
        },
      },
      {
        id: "create-with-command",
        label: "Create Terminal with Command",
        description:
          "Spawn N terminals and auto-type the given shell command into each after the prompt renders.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          command: "string (the shell command to type + Enter, required)",
        },
        handler: async (params?: unknown) => {
          const { count = 1, command } = (params ?? {}) as {
            count?: number;
            command?: string;
          };
          if (!command)
            throw new Error("create-with-command requires { count?: number, command: string }");
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-with-command: count must be a positive number");
          }
          const tabIds = await callRegistry<string[]>("terminal.spawn-with", {
            count,
            command,
          });
          return {
            success: true,
            tab_ids: tabIds,
            task_run_ids: [] as Array<string | null>,
          };
        },
      },
    ],
  });

  useEffect(() => {
    onSessionCountChange?.(tabs.length);
  }, [tabs.length, onSessionCountChange]);

  const {
    fileConflicts,
    fileLockStates,
    pendingYieldRequests,
    pendingLongWaitSignals,
    sessionPersistence,
  } = session;

  // Re-key per-tab fileLockStates (keyed by tab.id) onto session ids so
  // SessionCard can render a "blocked on …" subtitle. session.sessionId
  // equals tab.claudeSessionId for live AI tabs; tabs without a Claude
  // session id are skipped (their lock state still surfaces in the
  // tab-bar via the original tab.id keying).
  const sessionLockStates = useMemo(() => {
    const map = new Map<string, import("./useFileLockTracking").LockState>();
    for (const tab of tabs) {
      if (!tab.claudeSessionId) continue;
      const state = fileLockStates?.[tab.id];
      if (state) map.set(tab.claudeSessionId, state);
    }
    return map;
  }, [tabs, fileLockStates]);

  // ── Hold-side yield banner (Lock-Yield Protocol Phase 2) ─────────────
  //
  // Local-only dismissal set keyed by `${tab.id}:${filePath}`. When the
  // user clicks "Hold" we add the key so the banner stops rendering for
  // that pair. The banner re-appears when:
  //   - the file path changes (different lock contested), OR
  //   - waiterCount drops to 0 then back to ≥1 (a new wave of waiters).
  //
  // The clearing-on-zero-waiters effect handles the second case: when
  // `useFileLockTracking`'s event listeners refresh waiterCount and it
  // hits 0 for a previously-dismissed (tab, file_path), we drop that
  // entry from the set so the next waiter wakes the banner up again.
  //
  // The banner is rendered alongside MidSessionToast (overlay above the
  // active terminal pane). Wiring it into `TerminalTabBar.tsx` instead
  // would force it into the tab-bar's tight horizontal layout — the
  // plan flagged this ambiguity; the cleaner placement is here next to
  // the contention toast, matching its visual language.
  const [dismissedBanners, setDismissedBanners] = useState<Set<string>>(new Set());
  const [showDocFinder, setShowDocFinder] = useState(false);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional sync: drop stale dismissals whenever fileLockStates changes so the banner re-shows on a fresh waiter wave (count 0 → 1 after a "Hold" click). The same React-rules exception pattern is used at lib/runner-api.ts:61 / :79 for the port-listener sync.
    setDismissedBanners((prev) => {
      if (prev.size === 0) return prev;
      let dirty = false;
      const next = new Set(prev);
      for (const key of prev) {
        const sep = key.indexOf(":");
        if (sep < 0) continue;
        const tabId = key.slice(0, sep);
        const filePath = key.slice(sep + 1);
        const state = fileLockStates?.[tabId];
        // Drop the dismissal when the state no longer matches the
        // (tab, file) pair OR when waiterCount has gone to 0.
        if (
          !state ||
          state.kind !== "holding" ||
          state.filePath !== filePath ||
          (state.waiterCount ?? 0) === 0
        ) {
          next.delete(key);
          dirty = true;
        }
      }
      return dirty ? next : prev;
    });
  }, [fileLockStates]);

  const activeTab = useMemo(() => tabs.find((t) => t.id === activeId), [tabs, activeId]);
  const activeLockState = activeId ? fileLockStates?.[activeId] : undefined;
  // Phase 3 — pull per-tab incoming yield requests so the holding
  // banner can shift into request-mode and we can surface the request
  // count to the gating predicate.
  const activeIncomingRequests = activeId ? pendingYieldRequests?.[activeId] : undefined;
  const showHoldingBanner =
    !!activeTab &&
    !!activeId &&
    shouldShowHoldingBanner({
      lockState: activeLockState,
      dismissed: dismissedBanners,
      tabId: activeId,
      incomingRequests: activeIncomingRequests,
    });

  // Phase 3 — resolve the blocker's task_run_id from
  // `counterpartyName` (the blocker's tab title) by walking the local
  // tabs list. Per the plan we use option (b) — pass-from-parent —
  // rather than exporting `findTabByHolderName` from the hook so the
  // hook's surface area stays minimal.
  const showWaitingBanner =
    !!activeTab &&
    !!activeId &&
    activeLockState?.kind === "waiting" &&
    !!activeLockState.filePath &&
    activeLockState.sinceMs !== undefined &&
    !!activeTab.claudeSessionId;
  const blockerTaskRunId = useMemo(() => {
    if (!showWaitingBanner) return undefined;
    const blockerName = activeLockState?.counterpartyName;
    if (!blockerName) return undefined;
    const holderTab = tabs.find((t) => t.title === blockerName);
    return holderTab?.claudeSessionId;
  }, [showWaitingBanner, activeLockState?.counterpartyName, tabs]);

  // Per-tab commit-readiness state (Plan §3) — drives the
  // CommitTrafficLight badge that previously rendered inside the
  // TerminalTabBar tab strip. Phase 9c removed that consumer; the hook
  // still runs because its file-system polling side effects (PG
  // recent-editors snapshot) are load-bearing for other consumers.
  useCommitState(tabs);
  // Per-tab registry-awareness counts (pty-launched-ai-tabs-warning-plan
  // Phase 2). Same situation as commitStates above — the hook's side
  // effect (HTTP polling of /file-registry/probe-conflicts) feeds the
  // global registry; the per-tab badge consumer in TerminalTabBar is
  // gone, but other surfaces (HoldingLockBanner / SuggestionChip) read
  // the live registry instead of needing this return value.
  useRegistryAwareness(tabs);
  // Mid-session probe (Phase 3 of pty-launched-ai-tabs-warning-plan).
  // Fires `/file-registry/probe-conflicts` 500ms after the user types a
  // new turn into an active Claude CLI tab and surfaces predicted
  // collisions via {@link MidSessionToast}. Gated behind the
  // `enable_mid_session_path_prediction` localStorage flag.
  const midSessionProbeEnabled = useMidSessionProbeEnabled();
  const midSessionProbe = useMidSessionProbe(tabs, { enabled: midSessionProbeEnabled });
  // Latest-value refs so the durable-registry record callback below stays
  // identity-stable while still reading the freshest assignments/tabs. The
  // capture hook fires the callback from a long-lived polling closure, so a
  // stale closure would resolve the wrong zone / working dir.
  const assignmentsRef = useRef(zoneLayout.assignments);
  useEffect(() => {
    assignmentsRef.current = zoneLayout.assignments;
  }, [zoneLayout.assignments]);
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  /**
   * Durable session-registry OPEN recorder. Fired the instant a tab binds a
   * `claudeSessionId` (via the capture hook). Resolves the tab's current zone
   * by reverse-lookup over the live assignments, then records the OPEN
   * synchronously through the backend command (NOT the debounced localStorage
   * snapshot). Fire-and-forget — a failed record only loses the durable
   * restore hint, the live session is unaffected.
   */
  const recordSessionOpen = useCallback(
    (tabId: string, claudeSessionId: string, configDir?: string) => {
      const args = buildSessionOpenArgs({
        assignments: assignmentsRef.current,
        tabs: tabsRef.current,
        tabId,
        claudeSessionId,
        configDir,
        pageId,
      });
      invoke("terminal_session_record_open", { ...args }).catch((err) => {
        logger.warn(`terminal_session_record_open failed for ${claudeSessionId}: ${err}`);
      });
    },
    [pageId],
  );

  // Post-spawn polling hook that captures Claude CLI's session id from
  // the on-disk transcript and updates `tab.claudeSessionId` so the
  // commit traffic light has something to key on. Plan §1.5
  // "Frontend session-id capture". Also records the durable session OPEN
  // the instant the id is bound (zone resolved in `recordSessionOpen`).
  const { startCapture: startSessionIdCapture } = useTabSessionIdCapture({
    updateTab,
    tabs,
    onSessionIdBound: recordSessionOpen,
  });

  // Zone-move backstop: when a Claude session is dragged between zones the
  // recorded `zoneIndex` must follow. Watch `zoneLayout.assignments` and, after
  // a short debounce, re-emit `terminal_session_record_open` for any tab whose
  // resolved zone changed since the last emit. We track the last-emitted zone
  // per claudeSessionId in a ref so a re-render that doesn't move anything emits
  // nothing. The initial bind already records via `recordSessionOpen`, so we
  // seed the ref on first observation to avoid a redundant double-record.
  const lastEmittedZoneRef = useRef<Map<string, number>>(new Map());
  useEffect(() => {
    const timer = setTimeout(() => {
      const assignments = zoneLayout.assignments;
      const seen = new Set<string>();
      for (const tab of tabs) {
        if (!tab.claudeSessionId) continue;
        seen.add(tab.claudeSessionId);
        const zoneIndex = Number(
          Object.entries(assignments).find(([, id]) => id === tab.id)?.[0] ?? -1,
        );
        const prevZone = lastEmittedZoneRef.current.get(tab.claudeSessionId);
        if (prevZone === undefined) {
          // First observation — seed without emitting; the id-bind path
          // already recorded the OPEN with the correct zone.
          lastEmittedZoneRef.current.set(tab.claudeSessionId, zoneIndex);
          continue;
        }
        if (prevZone === zoneIndex) continue;
        lastEmittedZoneRef.current.set(tab.claudeSessionId, zoneIndex);
        invoke("terminal_session_record_open", {
          ...buildSessionOpenArgs({
            assignments,
            tabs,
            tabId: tab.id,
            claudeSessionId: tab.claudeSessionId,
            configDir: tab.claudeConfigDir,
            pageId,
          }),
        }).catch((err) => {
          logger.warn(
            `terminal_session_record_open (zone-move) failed for ${tab.claudeSessionId}: ${err}`,
          );
        });
      }
      // Drop tracking for sessions that no longer exist so a reused
      // claudeSessionId re-seeds cleanly.
      for (const sid of [...lastEmittedZoneRef.current.keys()]) {
        if (!seen.has(sid)) lastEmittedZoneRef.current.delete(sid);
      }
    }, 300);
    return () => clearTimeout(timer);
  }, [zoneLayout.assignments, tabs, pageId]);
  const {
    labelsAndTags,
    eventHistory,
    addHistoryEvent,
    metrics: _metrics,
    incrementMetric,
    focusHistory,
  } = useZoneMetadata();

  // The collapsed context spreads the session-state fields, so the
  // entire context value doubles as the stateTracking reference for
  // downstream call shapes that take it whole (useZoneActions,
  // useKeyboardShortcuts).
  const stateTracking = session;

  // Publish a snapshot of the current tabs + per-tab session state to the
  // module-level `terminal-sessions-registry`. The UI Bridge IPC handlers
  // for `GET /control/terminal-sessions[/{id}]` read from there since the
  // dispatcher (`useUIBridgeEventHandler`) sits above `TerminalCoreProvider`
  // and can't reach into this context directly. On unmount we clear the
  // snapshot so a stale view doesn't survive a route switch.
  useEffect(() => {
    const entries: TerminalSessionEntry[] = tabs.map((t) => ({
      id: t.id,
      title: t.title,
      taskRunId: t.claudeSessionId ?? null,
      claudeSessionId: t.claudeSessionId ?? null,
      workingDir: t.workingDir ?? "",
      // `sessionStates` keys are tab.id; tabs without a tracked state
      // (plain pwsh, freshly-created AI tabs before the state machine
      // observes them) fall through to "idle" so the field is always
      // a valid `TerminalSessionState`.
      state: stateTracking.sessionStates[t.id] ?? "idle",
      isAlive: Boolean(t.isAlive),
      exitCode: t.exitCode,
      type: t.type ?? "terminal",
      createdAt: t.createdAt ?? null,
    }));
    setTerminalSessions(entries);
    return () => {
      // Best-effort clear when TerminalPage unmounts so stale entries
      // don't survive tab switches away from `/terminals`.
      setTerminalSessions([]);
    };
  }, [tabs, stateTracking.sessionStates]);

  // ── Terminal-session roster (UI Bridge ground truth) ────────────────
  //
  // A single layout-independent DOM marker carrying the full set of
  // terminal sessions the page is tracking, keyed for verification by
  // stable `claudeSessionId`. Sourced from `tabs` (which holds EVERY
  // session regardless of which zone is currently mounted — unlike the
  // `terminal-input-*` interactive elements, which only mount for visible
  // zones) cross-referenced against the live `zoneLayout.assignments` for
  // each session's zone index.
  //
  // Reading this via `POST /ui-bridge/control/page/read-value
  // {"selector":"[data-page-element=terminal-session-roster]"}` returns the
  // roster JSON directly — no snapshot scraping of element ids and no
  // dependence on the active layout preset (Issues 2 + 3 of the
  // poll-dead-race remediation plan). Plain (non-Claude) shells have no
  // `claudeSessionId`; they are still listed (with `claudeSessionId: null`)
  // so the roster is a faithful census of all tracked terminals.
  const terminalSessionRoster = useMemo(() => {
    const assignments = zoneLayout.assignments;
    const roster = tabs.map((t) => {
      const zoneIndex = Number(
        Object.entries(assignments).find(([, id]) => id === t.id)?.[0] ?? -1,
      );
      return {
        claudeSessionId: t.claudeSessionId ?? null,
        terminalId: t.id,
        zoneIndex,
        title: t.title,
        isReconnecting: Boolean(t.isReconnecting),
      };
    });
    return JSON.stringify(roster);
  }, [tabs, zoneLayout.assignments]);

  const transitionEffects = useTransitionEffects();
  const { handleRestartInZone } = transitionEffects;

  const { workflowGen, sessionManager } = session;

  const {
    state: uiState,
    dispatch,
    toggleFocusMode: _toggleFocusMode,
    // toggleAutoLayout + cycleViewMode were prop-passed into TerminalTabBar's
    // layoutPicker slot; after Phase 9c demolition they're unused. Keyboard
    // shortcuts (Ctrl+Shift+M for view-mode cycle) still drive these via
    // useKeyboardShortcuts.
  } = useUIStateCx();

  useTerminalInitialization({
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
    layoutState: {
      layoutId: zoneLayout.layoutId,
      zoneLabels: labelsAndTags.zoneLabels,
      zoneNotes: labelsAndTags.zoneNotes,
      pinnedZones: labelsAndTags.pinnedZones,
      focusedZone: zoneLayout.focusedZone,
    },
  });

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      // Record the durable session CLOSE when a pty backing a Claude session
      // exits. Read the latest tab from the ref so this stays identity-stable.
      const claudeSessionId = tabsRef.current.find((t) => t.id === terminalId)?.claudeSessionId;
      if (claudeSessionId) {
        invoke("terminal_session_record_close", {
          claudeSessionId,
          reason: "pty-exit",
        }).catch((err) => {
          logger.warn(
            `terminal_session_record_close (pty-exit) failed for ${claudeSessionId}: ${err}`,
          );
        });
      }
      updateTab(terminalId, { isAlive: false, exitCode });
      stateTracking.handleExit(terminalId, exitCode);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [updateTab, stateTracking.handleExit],
  );

  const {
    handleZoneClick,
    handleZoneDoubleClick,
    handleOpenDocFile,
    createAndAssignTerminal,
    handleSortZones,
    handleExportOutput,
    handleExportZone,
  } = useZoneActions({
    tabs,
    dispatch,
    zoneLayout,
    stateTracking,
    labelsAndTags,
    transitionEffects,
    createTerminal,
    createPlanTab,
    incrementMetric,
    setNotification: workflowGen.setNotification,
  });

  useKeyboardShortcuts({
    activeId,
    tabs,
    dispatch,
    swapSource: uiState.swapSource,
    selectedZones: uiState.selectedZones,
    createAndAssignTerminal,
    closeTerminal,
    setActiveId,
    zoneLayout,
    sessionStates: stateTracking.sessionStates,
    handleRestartInZone,
    labelsAndTags,
    focusHistory,
    transitionEffects,
    incrementMetric,
    addHistoryEvent,
    terminalRefs: terminalRefs.current,
    workflowGen,
    sessionManager: {
      frozenCount: sessionManager.frozenCount,
      needsInputCount: sessionManager.needsInputCount,
      sessions: sessionManager.sessions,
      resumeSession: sessionManager.resumeSession,
    },
  });

  const writeWhenReady = (tabId: string, text: string, maxWaitMs = 5000) =>
    writeWhenReadyHelper(terminalRefs.current, tabId, text, {
      maxWaitMs,
      onTimeout: (id) => console.warn(`[LaunchAI] terminal ref for ${id} never became ready`),
    });

  // Hoisted out of the JSX so the command registry (`useTerminalCommands`
  // below) can share the exact same spawn closures with `TerminalTabBar`'s
  // `onQuickLaunch` / `onLaunchAiSession` props. Behavior identical to the
  // prior inline definitions; no useCallback wrap since the originals
  // weren't memoized either and downstream consumers don't depend on
  // stable identity.
  const handleQuickLaunch = async (count: number, autoCommand?: string): Promise<string[]> => {
    const totalTabs = tabs.length + count;
    const layoutId = pickLayout(totalTabs);
    zoneLayout.setLayoutId(layoutId);
    const title = autoCommand ? autoCommand.slice(0, 20) : undefined;
    const createdTabIds: string[] = [];
    for (let i = 0; i < count; i++) {
      const tabId = await createAndAssignTerminal(title);
      if (tabId) createdTabIds.push(tabId);
    }
    if (autoCommand && createdTabIds.length > 0) {
      for (const tabId of createdTabIds) {
        writeWhenReady(tabId, `${autoCommand}\r`);
      }
    }
    return createdTabIds;
  };

  const handleLaunchAiSession = async (
    count: number,
    configDir: string,
    context?: string,
  ): Promise<string[]> => {
    const totalTabs = tabs.length + count;
    const layoutId = pickLayout(totalTabs);
    zoneLayout.setLayoutId(layoutId);

    const isWindows = navigator.platform.startsWith("Win");
    const customCmd = sessionManager.launchCommands?.[configDir];
    // Default to autonomous mode (`--permission-mode bypassPermissions`),
    // matching the operator's `clg`/`clh`/`clp` wrappers — runner-spawned
    // AI sessions are for autonomous development and must not stall on a
    // permission prompt. An explicit per-account launch command (if
    // configured in settings) still overrides.
    const cmd = customCmd
      ? customCmd
      : isWindows
        ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; claude --permission-mode bypassPermissions`
        : `CLAUDE_CONFIG_DIR="${configDir}" claude --permission-mode bypassPermissions`;
    const dirName = configDir.replace(/\\/g, "/").replace(/\/$/, "").split("/").pop() ?? "";
    const label = customCmd ?? dirName.match(/^\.claude-(.+)$/)?.[1] ?? "claude";
    const createdTabIds: string[] = [];
    for (let i = 0; i < count; i++) {
      const tabId = await createAndAssignTerminal(label);
      if (tabId) createdTabIds.push(tabId);
    }
    if (createdTabIds.length > 0) {
      const spawnAt = Date.now();
      for (const tabId of createdTabIds) {
        writeWhenReady(tabId, `${cmd}\r`);
        const tab = tabs.find((t) => t.id === tabId);
        startSessionIdCapture(tabId, tab?.workingDir ?? "", spawnAt, configDir);
      }
      if (context) {
        const safeContext = context.replace(/\n/g, " ");
        setTimeout(() => {
          for (const tabId of createdTabIds) {
            writeWhenReady(tabId, `${safeContext}\r`);
          }
        }, 8000);
      }
    }
    return createdTabIds;
  };

  // Phase 1b — register the Terminal-page command set. Sources the spawn
  // closures above; reads everything else (tabs, zoneLayout, sessionStates,
  // closeTerminal, transitionEffects.handleRestartInZone, terminalRefs)
  // directly from contexts. No visible UI change today — the registry is
  // populated for future CommandBar / palette / AI-tier consumers.
  // New AI sessions must only target operator-CONFIGURED accounts. The
  // `accountUsage` list also probes transcript-derived dirs (the default
  // `~/.claude` from prior sessions), which previously let `/spawn-ai best`
  // resolve to an unauthenticated default account. Restrict spawn
  // candidates to `claude_config_dirs`; fall back to the full list only
  // when nothing is configured yet.
  const spawnAccounts =
    sessionManager.savedConfigDirs.length > 0
      ? sessionManager.accountUsage.filter((a) =>
          sessionManager.savedConfigDirs.includes(a.config_dir),
        )
      : sessionManager.accountUsage;

  useTerminalCommands({
    spawnPlain: handleQuickLaunch,
    spawnAi: handleLaunchAiSession,
    accounts: spawnAccounts,
    sortZones: handleSortZones,
    exportAll: handleExportOutput,
    openDocFinder: () => setShowDocFinder(true),
    showCard,
  });

  if (!initialized) {
    return (
      <div className="h-full flex items-center justify-center bg-[#1a1b26]">
        <div className="flex flex-col items-center gap-3">
          <div className="w-8 h-8 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
          <span className="text-[12px] text-[#565f89]">Loading terminals...</span>
        </div>
      </div>
    );
  }

  return (
    <UIBridgeComponentScope componentId="terminal-page">
      <div className="h-full flex flex-col bg-[#1a1b26]">
        {/* Phase 6 — top status strip. Renders nothing when no signal
            requires attention, keeping the top of viewport clean by
            default. Phase 9c demolition removed `TerminalTabBar` (was
            mounted directly below this); the registry + CommandBar +
            ZoneHoverActions + StatusStrip now cover its surface. */}
        <StatusStrip />
        {/* Layout-independent terminal-session roster marker (UI Bridge
            ground truth). Its textContent is the JSON census of every
            tracked session — read via `POST
            /ui-bridge/control/page/read-value
            {"selector":"[data-page-element=terminal-session-roster]"}`.
            Visually hidden; not interactive. Lists ALL sessions regardless
            of which zones are currently mounted, so verification never
            depends on the active layout preset. */}
        <span
          data-page-element="terminal-session-roster"
          style={{ display: "none" }}
          aria-hidden
        >
          {terminalSessionRoster}
        </span>
        {/* Phase 2 — honesty backstop for sessions past the zone ceiling.
            Renders nothing when every tab is visible; otherwise a "N more"
            chip that opens the ZoneControlPanel's Unassigned (N) list. */}
        <UnzonedChip
          unassignedCount={zoneLayout.unassignedTabIds.length}
          onOpen={() => dispatch({ type: "SET_SHOW_CONTROL_PANEL", payload: true })}
        />

        {showDocFinder && (
          <DocFinderModal
            onSelect={(filePath) => {
              handleOpenDocFile(filePath);
              setShowDocFinder(false);
            }}
            onClose={() => setShowDocFinder(false)}
          />
        )}
        <TerminalNotification
          message={workflowGen.notification?.message ?? null}
          type={workflowGen.notification?.type ?? "success"}
          onDismiss={() => workflowGen.setNotification(null)}
        />
        <FileConflictBanner
          conflicts={fileConflicts.conflicts}
          recentAlert={fileConflicts.recentAlert}
          onDismissAlert={fileConflicts.dismissAlert}
        />

        {uiState.showTimeline && zoneLayout.isMultiZone && (
          <ZoneTimeline
            tabs={tabs}
            assignments={zoneLayout.assignments}
            sessionStates={stateTracking.sessionStates}
            eventHistory={eventHistory}
            onClose={() => dispatch({ type: "SET_SHOW_TIMELINE", payload: false })}
          />
        )}

        {uiState.showOutputSearch && (
          <OutputSearchBar
            outputSearch={uiState.outputSearch}
            onSearchChange={(v) => dispatch({ type: "SET_OUTPUT_SEARCH", payload: v })}
            onClose={() => dispatch({ type: "SET_SHOW_OUTPUT_SEARCH", payload: false })}
            lastOutputLines={stateTracking.lastOutputLines}
          />
        )}

        <div className="flex-1 flex flex-row overflow-hidden">
          {workflowGen.showSidebar && (
            <SessionManagerPanel
              manager={sessionManager}
              selectedSessionId={workflowGen.selectedTranscriptSessionId}
              sessionConflictCounts={fileConflicts.sessionConflictCounts}
              sessionLockStates={sessionLockStates}
            />
          )}

          <div className="flex-1 relative overflow-hidden">
            {tabs.length > 0 ? (
              <ZoneGrid
                onZoneClick={handleZoneClick}
                onZoneDoubleClick={handleZoneDoubleClick}
                onExit={handleExit}
                onExportZone={handleExportZone}
                onUserInputLine={(tabId, input) => midSessionProbe.feed(tabId, input)}
              />
            ) : (
              <div className="h-full flex flex-col items-center justify-center text-[#565f89] gap-2">
                <span className="text-sm">
                  No terminals open. Press{" "}
                  <kbd className="px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#a9b1d6] text-xs font-mono">
                    Ctrl+Shift+T
                  </kbd>{" "}
                  or click + to create one.
                </span>
              </div>
            )}

            {zoneLayout.isMultiZone && <ZoneMinimap />}

            {zoneLayout.isMultiZone && !transitionEffects.batchBarDismissed && (
              <BatchOperationsBar />
            )}

            {/* Mid-session predicted-collision toast (Phase 3). Overlays
                the active terminal — positioned top-right of the
                flex-1 container so it sits over the zone grid, not
                the chrome. Jump-to-holder: focus the tab whose title
                matches the holder name, mirroring LaunchMenu's
                `onJumpToHolder` semantics. */}
            {activeId && midSessionProbe.states[activeId] && (
              <MidSessionToast
                state={midSessionProbe.states[activeId]}
                onDismiss={() => midSessionProbe.dismiss(activeId)}
                onJumpToHolder={(name) => {
                  const target = tabs.find((t) => t.title === name);
                  if (target) setActiveId(target.id);
                }}
              />
            )}

            {/* Lock-Yield Protocol Phase 2 — hold-side yield banner.
                Renders ONLY when the active tab is holding a file lock
                AND at least one OTHER session is waiting on it. Same
                overlay placement as MidSessionToast above. Uses
                `tab.claudeSessionId` as the task_run_id for the yield
                POST (canonical mapping for live AI tabs — same join
                that `useFileLockTracking.findTabByTaskRunId` uses); if
                a tab has no `claudeSessionId` yet (pre-spawn), the
                banner stays hidden because the POST has nowhere
                meaningful to land. */}
            {showHoldingBanner &&
              activeTab &&
              activeId &&
              activeLockState?.kind === "holding" &&
              activeLockState.filePath &&
              activeLockState.sinceMs !== undefined &&
              activeTab.claudeSessionId && (
                <HoldingLockBanner
                  taskRunId={activeTab.claudeSessionId}
                  taskRunName={activeTab.title}
                  filePath={activeLockState.filePath}
                  waiterName={activeLockState.counterpartyName}
                  sinceMs={activeLockState.sinceMs}
                  waiterCount={activeLockState.waiterCount ?? 0}
                  incomingRequests={activeIncomingRequests}
                  onDismissLocal={() => {
                    setDismissedBanners((prev) => {
                      const next = new Set(prev);
                      next.add(`${activeId}:${activeLockState.filePath}`);
                      return next;
                    });
                  }}
                />
              )}

            {/* Lock-Yield Protocol Phase 3 — wait-side yield-request
                banner. Mutually exclusive with the holding banner above
                (a tab is either holding OR waiting on a given path,
                never both for the same file). Renders ONLY when the
                active tab is waiting and we can identify both sides
                — the blocker's task_run_id is resolved locally from
                `tab.title === counterpartyName` (option (b) per the
                plan; avoids exporting `findTabByHolderName` from the
                hook). */}
            {showWaitingBanner &&
              activeTab &&
              activeLockState?.kind === "waiting" &&
              activeLockState.filePath &&
              activeLockState.sinceMs !== undefined &&
              activeTab.claudeSessionId && (
                <WaitingLockBanner
                  taskRunId={activeTab.claudeSessionId}
                  taskRunName={activeTab.title}
                  filePath={activeLockState.filePath}
                  blockerName={activeLockState.counterpartyName}
                  blockerTaskRunId={blockerTaskRunId}
                  sinceMs={activeLockState.sinceMs}
                  // Phase 2 (stuck-session heartbeat) — joined from
                  // `/sessions/idle-status` by useFileLockTracking's
                  // 10s poll; threaded through as a prop so the banner
                  // surfaces "(holder idle Xm)" inline on the headline.
                  // Undefined on holding/idle branches by design (the
                  // hook only attaches it to the waiting branch).
                  holderIdleMs={activeLockState.holderIdleMs}
                  longWaitSignal={(() => {
                    // Phase 3 (stuck-session heartbeat): surface the
                    // most-recent long-wait signal targeting this
                    // waiter for the contested file. The hook dedups
                    // by `(holderTaskRunId, filePath)` so the list is
                    // already keyed cleanly; picking [0] yields the
                    // first-arrived entry, but for the banner copy any
                    // matching entry is acceptable since a new signal
                    // for the same pair replaces in place.
                    const signals = (activeId && pendingLongWaitSignals?.[activeId]) || [];
                    const match = signals.find((s) => s.filePath === activeLockState.filePath);
                    if (!match) return undefined;
                    return {
                      holderName: match.holderName,
                      estimatedRemainingMs: match.estimatedRemainingMs,
                      signaledAtMs: match.signaledAtMs,
                    };
                  })()}
                />
              )}

            {/* Coord-as-Deconflicter Phase 1 (§4.4) — in-session
                advisory banner that surfaces `coord.coordinator_decisions`
                rows fired by the Rust deconflicter loop. Gated on
                `claudeSessionId` per `proj_holding_banner_pty_gate` — the
                emergent-task wiring (commands/productivity.rs:1082 +
                ai_session register sites) ensures every AI tab has one.
                Lives below the lock-yield banners by document order so
                they stack instead of overlapping. */}
            {activeTab?.claudeSessionId && (
              <DeconflictAdvisoryBanner taskRunId={activeTab.claudeSessionId} />
            )}

            {/* L3 (shared-checkout coordination gap fix) — soft warning
                when branch-mutating git is typed into THIS terminal's PTY
                while a coord peer holds the worktree claim on the repo.
                NOT gated on `claudeSessionId` (it applies to any terminal,
                including plain shells where an operator types `git
                switch` by hand). `activeId` is the active tab id, which
                equals the backend terminal id (ZoneGrid passes
                `terminalId={zoneTab.id}`). Soft + dismissible; never
                blocks input. */}
            <CoordWarningBanner activeTerminalId={activeId ?? undefined} />
          </div>

          {/* Phase 2 — `isMultiZone` gate dropped so the Unassigned (N) list
              shows in `single` layout too. Past the 9-zone `full-grid` ceiling
              the only honest surface for hidden sessions is this panel's
              Unassigned list; the UnzonedChip near the StatusStrip opens it. */}
          {uiState.showControlPanel && (
            <ZoneControlPanel onCreateTerminal={() => createAndAssignTerminal()} />
          )}

          <TerminalRightPanel />
        </div>

        <TerminalOverlays onSortZones={handleSortZones} onExport={handleExportOutput} />

        {/* Phase 2 — slash-command bar pinned to the bottom of the
            page chrome. Reads from the command registry populated by
            `useTerminalCommands` above; Ctrl+/ focuses from anywhere. */}
        <CommandBar />
        <ResultCardMount />

        {/* Phase 9c — TerminalTabBar demolished. ZoneLayoutPicker and
            ZoneProfilePicker carry their own `useUIComponent`
            registrations (`zone-layout-picker` and `zone-profile-picker`),
            so external agents discover layout / profile actions through
            those component ids. To preserve those wire contracts WITHOUT
            re-introducing the visual chrome they used to provide, the
            two pickers mount here inside an `aria-hidden` container with
            `display: none` — they self-register on mount, no UI surface.
            Operators reach the same surfaces via slash commands
            (`/layout <preset>`, `/profile-load <name>`, etc.) or via
            external automation calling the UI Bridge action endpoints
            directly. */}
        <div style={{ display: "none" }} aria-hidden>
          <ZoneLayoutPicker
            currentLayoutId={zoneLayout.layoutId}
            // Picking a layout from the picker UI is an explicit operator
            // choice — pin it so auto-grow stops overriding it.
            onSelectLayout={(id) => zoneLayout.setLayoutId(id, { pinned: true })}
            tabCount={tabs.length}
          />
          <ZoneProfilePicker
            currentLayoutId={zoneLayout.layoutId}
            zoneLabels={labelsAndTags.zoneLabels}
            zoneNotes={labelsAndTags.zoneNotes}
            pinnedZones={labelsAndTags.pinnedZones}
            autoApprovePatterns={transitionEffects.autoApprovePatterns}
            pageId={pageId}
            zoneAssignments={zoneLayout.assignments}
            tabs={tabs}
            initialized={initialized}
            onLoadProfile={async (profile) => {
              // Loading a saved profile is a deliberate operator layout choice —
              // pin it so auto-grow doesn't override the restored arrangement.
              zoneLayout.setLayoutId(profile.layoutId, { pinned: true });
              labelsAndTags.setZoneLabels(profile.labels);
              labelsAndTags.setZoneNotes(profile.notes);
              labelsAndTags.setPinnedZones(new Set(profile.pins));
              transitionEffects.setAutoApprovePatterns(profile.autoApprovePatterns);
              if (profile.sessions && profile.sessions.length > 0) {
                pendingProfileSessionsRef.current = profile.sessions;
                const existingTabCount = tabs.length;
                const neededCount = profile.sessions.length;
                const toCreate = Math.max(0, neededCount - existingTabCount);
                for (let i = 0; i < toCreate; i++) {
                  await createAndAssignTerminal();
                }
                if (toCreate === 0) {
                  const assignedTabs = new Set<string>();
                  for (const s of profile.sessions) {
                    const candidate = tabs.find((t) => !assignedTabs.has(t.id));
                    if (!candidate) break;
                    assignedTabs.add(candidate.id);
                    zoneLayout.assignTabToZone(s.zoneIndex, candidate.id);
                  }
                }
              }
            }}
          />
        </div>
      </div>
    </UIBridgeComponentScope>
  );
}
