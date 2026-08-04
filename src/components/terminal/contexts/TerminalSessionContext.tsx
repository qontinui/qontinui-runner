/**
 * TerminalSessionContext — plan 2026-05-22-coord-native-session-coordination
 * §Phase 4.
 *
 * Collapses the previous 4-way split into one provider with one
 * `useTerminalSession()` hook. The 4 collapsed contexts:
 *
 *   - TerminalCoreContext   (PTY tabs, zone layout, terminal refs)
 *   - SessionStateContext   (per-tab session-state machine, snapshots,
 *                            findings, unread tracking)
 *   - ShellInfraContext     (file conflicts, file lock tracking, session
 *                            persistence)
 *   - AiFeaturesContext     (workflow gen, analysis, transcripts,
 *                            session manager, shell integration, session
 *                            summary)
 *
 * Kept SEPARATE per the plan (orthogonal UI concerns, not session
 * state):
 *
 *   - TransitionEffectsContext
 *   - UIStateContext
 *   - ZoneMetadataContext
 *
 * The new src/contexts/SessionContext.tsx is a different concept — it
 * wraps the COORD-native session_* Tauri surface. This file is the
 * runner's LOCAL terminal-page state collapse. The two are intentional
 * separate scopes per plan §Phase 4 / §D6 (typed intent → coord) and
 * the runner's existing terminal-page wiring (which is heavy with
 * coord-unrelated UI state like zone layout, AI workflow generation,
 * findings, transcripts).
 *
 * Provider order (Phase 3 mount-hydration lift):
 *
 *   App.tsx:
 *     <WindowAssignmentsProvider>
 *       <TerminalSessionProvider pages={…} activePageId={…}>  // always mounted
 *         <TerminalPage>  // single instance, reads the active page's slice
 *           <ZoneMetadataProvider>
 *             <TransitionEffectsProvider>
 *               <UIStateProvider>{children}</UIStateProvider>
 *             …
 *
 * `TerminalSessionProvider` renders one always-mounted `PageSessionScope` per
 * terminal page (each owning its page's tab + zone + AI state and its own live
 * `terminal-created` listener) and surfaces the ACTIVE page's value through
 * `TerminalSessionContext`. Switching terminal pages no longer destroys any
 * page's state, and a `terminal-created` event is never dropped by a page
 * switch.
 *
 * The previously-circular dependencies (TransitionEffects + ZoneMetadata
 * read from TerminalCore + SessionState) collapse to a single
 * upward read from `useTerminalSession()` — no cycle.
 */

import {
  createContext,
  useState,
  useCallback,
  useEffect,
  useRef,
  useMemo,
  createRef,
  useContext,
  memo,
  type ReactNode,
  type RefObject,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useWindowAssignments, MAIN_WINDOW_LABEL } from "./WindowAssignmentsContext";
import { subscribeTerminalOutputStream } from "../terminalEventDemux";
import { hasRenderConsumer } from "../terminalRenderConsumers";

import { useTerminalManager } from "../useTerminalManager";
import { useZoneLayout } from "../useZoneLayout";
import { type TerminalInstanceHandle } from "../TerminalInstance";
import { type ZoneSessionInfo } from "../zoneProfileStorage";
import { writeWhenReady } from "../writeWhenReady";
import { fetchLiveClaudeSessionIds } from "../liveClaudeSessions";
import { decideColdResume } from "../useTerminalInitialization";

import { useSessionStateTracking } from "../useSessionStateTracking";
import { useOutputSnapshots } from "../useOutputSnapshots";
import { useTerminalFindings } from "../useTerminalFindings";

import { useFileConflicts } from "../useFileConflicts";
import { useFileLockTracking } from "../useFileLockTracking";
import { useSessionPersistence } from "../useSessionPersistence";

import { useShellIntegration } from "../useShellIntegration";
import { useWorkflowGeneration } from "../useWorkflowGeneration";
import { useAnalysis } from "../useAnalysis";
import { useFindingsActions } from "../useFindingsActions";
import { useTranscriptSessions } from "../useTranscriptSessions";
import { useSessionManager } from "../useSessionManager";
import { deriveSyntheticTabs } from "../syntheticTabs";
import { useZoneLabelsAndTags } from "../useZoneLabelsAndTags";
import { TerminalOutputCoalescer, base64ToBytes } from "../terminalOutputTap";

import { instanceStorage } from "@/lib/instance-storage";

// ---------------------------------------------------------------------------
// Return-type aliases — preserve the prior shapes one-for-one so
// downstream consumers don't see a change in the value-object signature.
// ---------------------------------------------------------------------------

type TerminalManagerReturn = ReturnType<typeof useTerminalManager>;
type ZoneLayoutReturn = ReturnType<typeof useZoneLayout>;
type StateTrackingReturn = ReturnType<typeof useSessionStateTracking>;
type SnapshotsReturn = ReturnType<typeof useOutputSnapshots>;
type FindingsReturn = ReturnType<typeof useTerminalFindings>;
type ShellIntegrationReturn = ReturnType<typeof useShellIntegration>;
type WorkflowGenReturn = ReturnType<typeof useWorkflowGeneration>;
type AnalysisReturn = ReturnType<typeof useAnalysis>;
type FindingsActionsReturn = ReturnType<typeof useFindingsActions>;
type SessionManagerReturn = ReturnType<typeof useSessionManager>;
type FileConflictsReturn = ReturnType<typeof useFileConflicts>;
type SessionPersistenceReturn = ReturnType<typeof useSessionPersistence>;
type LabelsAndTagsReturn = ReturnType<typeof useZoneLabelsAndTags>;

/**
 * Flat value-object exposed by `useTerminalSession()`. The shape is the
 * union of the 4 prior context values; field names are PRESERVED to
 * keep downstream callsites mechanical to migrate.
 *
 * The two large `extends` clauses (`TerminalManagerReturn`,
 * `StateTrackingReturn`) flatten the prior TerminalCore + SessionState
 * surfaces so every field shows up directly on the value-object instead
 * of nested under `terminalManager.*` / `stateTracking.*`. Behavior
 * matches the prior consumers, which previously destructured these
 * fields off `useTerminalCore()` / `useSessionState()` directly.
 */
export interface TerminalSessionContextValue extends TerminalManagerReturn, StateTrackingReturn {
  // — TerminalCore extras —
  pageId: string;
  zoneLayout: ZoneLayoutReturn;
  terminalRefs: React.MutableRefObject<Map<string, RefObject<TerminalInstanceHandle | null>>>;
  pendingProfileSessionsRef: React.MutableRefObject<ZoneSessionInfo[] | null>;
  /**
   * Per-page zone labels / notes / pins / tags. Owned by the per-page
   * `PageSessionScope` (namespaced by `pageId` in `instanceStorage`) so the
   * cold-start init backstop can run inside the scope; `ZoneMetadataProvider`
   * re-exposes this exact reference to the wider page tree.
   */
  labelsAndTags: LabelsAndTagsReturn;

  // — SessionState extras —
  snapshots: SnapshotsReturn;
  processOutputRef: React.MutableRefObject<((tabId: string, text: string) => void) | undefined>;
  activeFindings: FindingsReturn["activeFindings"];
  allFindings: FindingsReturn["allFindings"];

  // — ShellInfra —
  fileConflicts: FileConflictsReturn;
  /**
   * File-lock maps (`lockStates`, `pendingYieldRequests`,
   * `pendingLongWaitSignals`) are NOT on this value — they live in the page's
   * terminal hot store and are read with `useHotField(pageId, …)` /
   * `useTabHotSlice(pageId, tabId)`. Same for `lastOutputLines`,
   * `activityData` and `stateDurations`.
   */
  sessionPersistence: SessionPersistenceReturn;

  // — AiFeatures —
  shellIntegration: ShellIntegrationReturn;
  workflowGen: WorkflowGenReturn;
  analysis: AnalysisReturn;
  findingsActions: FindingsActionsReturn;
  sessionManager: SessionManagerReturn;
  transcriptSessions: ReturnType<typeof useTranscriptSessions>["sessions"];
  sessionsLoading: boolean;
  refreshSessions: () => void;
  loadMessages: ReturnType<typeof useTranscriptSessions>["loadMessages"];
  sessionSummary: string | null;
  sessionSummaryLoading: boolean;
  handleSummarizeSession: (messages: Array<{ msg_type: string; text: string }>) => Promise<void>;
  getScrollback: (tabId: string, maxLines?: number) => string;
  getActiveSelection: () => string;
}

export const TerminalSessionContext = createContext<TerminalSessionContextValue | null>(null);

interface PageSessionScopeProps {
  pageId: string;
  /**
   * This page's `TerminalPageConfig.defaultWorkingDir` — the cwd terminals
   * spawned on this page default to when the caller passes none (set when a
   * project is activated). Threaded into `useTerminalManager` so the fallback
   * is applied before the Rust `terminal_create` command derives `intent_repo`
   * from `working_dir` (projects-dashboard plan §7.2 step 2).
   */
  defaultWorkingDir?: string;
  /**
   * Publish (or, with `null`, retract) this page's computed session value to
   * the lifted `TerminalSessionProvider`. The provider keeps a Map keyed by
   * pageId and surfaces the ACTIVE page's value through context.
   */
  register: (pageId: string, value: TerminalSessionContextValue | null) => void;
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
}

/**
 * Per-page state scope. Owns every piece of terminal-page state previously
 * split across TerminalCore + SessionState + ShellInfra + AiFeatures (plus the
 * page-namespaced `labelsAndTags`). Wire order matches the prior provider
 * nesting precisely so the underlying hooks see the same input timing.
 *
 * Phase 3 (mount-hydration lift): one of these is mounted PER terminal page,
 * all simultaneously, regardless of which page the operator is viewing. That
 * keeps every page's `terminal-created` listener live (no dropped events on a
 * page switch) and preserves every page's tab state across switches. The
 * component renders nothing — it only computes its page's value and publishes
 * it via `register`; the lifted provider routes the active page's value into
 * the `TerminalSessionContext` the page tree consumes.
 */
// React.memo so a re-render of the lifted provider (driven by its own
// `setValues` when ANY page publishes a new value) does NOT cascade back into
// every scope. Without this, each scope re-renders on every provider render,
// recomputes its (ref-unstable) `value`, re-`register`s it → `setValues` →
// provider re-renders → … a self-sustaining infinite render loop (introduced
// when #476 lifted the provider + added the register feedback path). Props are
// stable (pageId, the useCallback `register`, and App's memoized nav handlers),
// so memo short-circuits the cascade; the scope still re-renders normally when
// its OWN session hooks change.
const PageSessionScope = memo(function PageSessionScope({
  pageId,
  defaultWorkingDir,
  register,
  onNavigateToBuilder,
  onNavigateToActive,
}: PageSessionScopeProps) {
  // Phase 1 (pop-out windows): render only the tabs THIS window owns. With a
  // single ("main") window and no assignments every tab is owned by "main", so
  // `tabs === allTabs` and every downstream consumer behaves byte-identically.
  // Read first so the window label flows into the terminal manager below
  // (Phase 2: the create-time pane key is tagged with the owning window).
  const { windowLabel: myWindowLabel, isOwned, windowForPage } = useWindowAssignments();

  // ---- TerminalCore ----
  const terminalManager = useTerminalManager(pageId, myWindowLabel, defaultWorkingDir);
  const {
    tabs: allTabs,
    activeId,
    setActiveId,
    updateTab,
    renameTab,
    createTerminal,
  } = terminalManager;
  // Pop-out-page binding takes precedence over the per-terminal owner map: when
  // this whole page is detached into a window, that window renders ALL of the
  // page's tabs and every other window renders NONE of them. Otherwise fall
  // back to the per-terminal `isOwned` filter (single-terminal "send to
  // window"; everything stays on "main" in the common single-window case).
  const boundWindow = windowForPage(pageId);
  const tabs = useMemo(() => {
    if (boundWindow) {
      return boundWindow === myWindowLabel ? allTabs : [];
    }
    return allTabs.filter((t) => isOwned(t.id));
  }, [allTabs, isOwned, boundWindow, myWindowLabel]);

  // A terminal created in a pop-out window must belong to THAT window, not the
  // default "main". Assign it immediately after creation (the broadcast event
  // then drops it from main and claims it here). No-op in the main window.
  const createTerminalForWindow = useCallback<typeof createTerminal>(
    async (title?: string, workingDir?: string, tenantId?: string) => {
      const id = await createTerminal(title, workingDir, tenantId);
      if (id && myWindowLabel !== MAIN_WINDOW_LABEL) {
        try {
          await invoke("assign_session_to_window", {
            sessionId: id,
            windowLabel: myWindowLabel,
          });
        } catch {
          /* best-effort: tab still works, just stays in main */
        }
      }
      return id;
    },
    [createTerminal, myWindowLabel],
  );

  const tabIds = useMemo(() => tabs.map((t) => t.id), [tabs]);
  // Live-tab set so the zone layout can exclude exited tombstones from the
  // hidden-but-LIVE "N more" count (and surface them separately).
  const liveTabIds = useMemo(() => new Set(tabs.filter((t) => t.isAlive).map((t) => t.id)), [tabs]);
  const zoneLayout = useZoneLayout(tabIds, pageId, myWindowLabel, liveTabIds);

  // Page-namespaced zone labels / notes / pins / tags. Moved into the scope
  // (from ZoneMetadataProvider) so each page owns its own labels regardless of
  // which page is active, and the cold-start restore path (init backstop) can
  // apply saved cosmetics to the right page. `ZoneMetadataProvider` re-exposes
  // this exact reference to the page tree, so downstream consumers are
  // unchanged.
  const labelsAndTags = useZoneLabelsAndTags(zoneLayout.layoutId, zoneLayout.assignments, pageId);

  // Sync focused zone → active tab.
  useEffect(() => {
    if (
      zoneLayout.focusedTabId &&
      zoneLayout.focusedTabId !== activeId &&
      tabs.some((t) => t.id === zoneLayout.focusedTabId)
    ) {
      setActiveId(zoneLayout.focusedTabId);
    }
  }, [zoneLayout.focusedTabId, activeId, setActiveId, tabs]);

  // Terminal refs map — create refs for new tabs, clean up stale ones.
  const terminalRefs = useRef<Map<string, RefObject<TerminalInstanceHandle | null>>>(new Map());

  useEffect(() => {
    const map = terminalRefs.current;
    for (const tab of tabs) {
      if (!map.has(tab.id)) {
        map.set(tab.id, createRef<TerminalInstanceHandle>());
      }
    }
    for (const key of map.keys()) {
      if (!tabs.some((t) => t.id === key)) {
        map.delete(key);
      }
    }
  }, [tabs]);

  // Pending Claude sessions to resume after a zone profile load settles.
  const pendingProfileSessionsRef = useRef<ZoneSessionInfo[] | null>(null);

  useEffect(() => {
    const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
    const sessions = pendingProfileSessionsRef.current;
    if (!sessions || sessions.length === 0) return;

    const isWindows = navigator.platform.startsWith("Win");
    const buildResumeCmd = (sessionId: string, configDir: string | undefined) => {
      // Autonomous resume (matches clg/clh/clp) so a re-attached session
      // doesn't stall on a permission prompt.
      const base = `claude --permission-mode bypassPermissions --resume ${sessionId}`;
      if (!configDir) return `${base}\r`;
      return isWindows
        ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}\r`
        : `CLAUDE_CONFIG_DIR="${configDir}" ${base}\r`;
    };

    // Process only sessions whose zone now has an assignment; leave the
    // rest in the ref for the next assignments tick. The partition (and the
    // ref update) stays SYNCHRONOUS so a re-fire of this effect while the
    // async liveness gate below is in flight can never double-type a resume.
    const remaining: ZoneSessionInfo[] = [];
    const toResume: Array<{ s: ZoneSessionInfo; tabId: string }> = [];
    for (const s of sessions) {
      const tabId = zoneLayout.assignments[s.zoneIndex];
      if (tabId && SESSION_ID_RE.test(s.claudeSessionId)) {
        toResume.push({ s, tabId });
      } else {
        remaining.push(s);
      }
    }
    pendingProfileSessionsRef.current = remaining.length > 0 ? remaining : null;
    if (toResume.length === 0) return;

    void (async () => {
      // P1 restore idempotence: a zone profile can name sessions that are
      // ALREADY alive elsewhere (another page, another window, a process
      // outside the runner) — typing `--resume <id>` for one forks it: two
      // live processes on one transcript. Gate every typed resume on the same
      // liveness oracle the cold-restore drain uses (fetchLiveClaudeSessionIds
      // + decideColdResume). One fetch covers the batch — unlike the drain's
      // ~500 ms-spaced typing, this loop types back-to-back within
      // milliseconds, so the read IS "immediately before typing" for all of
      // them. `null` (registry unreadable) fails CLOSED: skip the whole
      // batch. Skipped sessions are dropped, not re-parked — the profile's
      // layout/tabs are untouched, and the operator can still resume any of
      // them by hand (single-id operator-clicked resume paths stay ungated
      // by design: the click is the intent).
      const liveIds = await fetchLiveClaudeSessionIds();
      for (const { s, tabId } of toResume) {
        const decision = decideColdResume(liveIds, s.claudeSessionId);
        if (decision !== "respawn") {
          console.warn(
            `[TerminalSession] profile resume SKIPPED for session ${s.claudeSessionId}: ` +
              (decision === "skip-alive"
                ? "a live Claude process already hosts this id (typing --resume would fork it)"
                : "live-session registry unreadable — failing closed toward not-respawning"),
          );
          continue;
        }
        updateTab(tabId, {
          claudeSessionId: s.claudeSessionId,
          claudeConfigDir: s.claudeConfigDir,
        });
        // Durable-registry OPEN at type time (#548 Phase 1): `--resume` names
        // the exact id in the typed command — no transcript guess.
        const resumedTab = tabs.find((t) => t.id === tabId);
        invoke("terminal_session_record_open", {
          claudeSessionId: s.claudeSessionId,
          configDir: s.claudeConfigDir,
          workingDir: resumedTab?.workingDir,
          pageId,
          zoneIndex: s.zoneIndex,
          title: resumedTab?.title,
          terminalId: tabId,
          bindOrigin: "pinned",
        }).catch((err) => console.warn(`[TerminalSession] profile resume record failed:`, err));
        writeWhenReady(
          terminalRefs.current,
          tabId,
          buildResumeCmd(s.claudeSessionId, s.claudeConfigDir),
          {
            onTimeout: (id) =>
              console.warn(
                `[TerminalSession] profile resume: terminal ref for ${id} never became ready`,
              ),
          },
        );
      }
    })();
  }, [zoneLayout.assignments, updateTab, tabs, pageId]);

  // Transcripts are read up here (rather than in the AiFeatures block below)
  // because the debug-gated synthetic-tab seam derives synthetic tabs from
  // them and must feed those tabs into `useSessionStateTracking` /
  // `useSessionManager`. Pure hook reordering — no behavioral change for the
  // real path (transcripts with no `injected_tab` produce zero synthetic tabs).
  const {
    sessions: transcriptSessions,
    loading: sessionsLoading,
    refresh: refreshSessions,
    loadMessages,
  } = useTranscriptSessions();

  // Synthetic tabs from tab-backed injected fakes (debug-gated test-fixtures
  // seam). Only the MAIN window derives them — pop-out windows own a disjoint
  // tab subset and the fakes correlate to the main window's session list.
  // Real transcripts never carry `injected_tab`, so this is empty in
  // production (and dead code in a release build without `test-fixtures`).
  const { tabs: syntheticTabs, seedLastOutput: syntheticSeedLastOutput } = useMemo(
    () =>
      myWindowLabel === MAIN_WINDOW_LABEL
        ? deriveSyntheticTabs(transcriptSessions)
        : { tabs: [], seedLastOutput: {} },
    [myWindowLabel, transcriptSessions],
  );

  // Real tabs PLUS synthetic tabs — fed ONLY to the two consumers that drive
  // StatusStrip bucketing. Every other consumer keeps the real `tabs` so a
  // synthetic tab never renders a terminal pane.
  const tabsWithSynthetic = useMemo(
    () => (syntheticTabs.length > 0 ? [...tabs, ...syntheticTabs] : tabs),
    [tabs, syntheticTabs],
  );

  // ---- SessionState ----
  const processOutputRef = useRef<((tabId: string, text: string) => void) | undefined>(undefined);

  const stateTracking = useSessionStateTracking({
    pageId,
    tabs: tabsWithSynthetic,
    seedLastOutput: syntheticSeedLastOutput,
    processOutput: (tabId, text) => processOutputRef.current?.(tabId, text),
    getBufferLines: (tabId, maxLines) => {
      const ref = terminalRefs.current.get(tabId);
      const scrollback = ref?.current?.getScrollback?.(maxLines) ?? "";
      if (!scrollback) return [];
      return scrollback
        .split("\n")
        .filter((l) => l.trim().length > 0)
        .slice(-maxLines);
    },
  });

  const snapshots = useOutputSnapshots(pageId);

  const { processOutput, activeFindings, allFindings } = useTerminalFindings(activeId ?? null);

  // Wire findings processOutput into stateTracking's processOutputRef.
  useEffect(() => {
    processOutputRef.current = processOutput;
  }, [processOutput]);

  // ---- Global terminal-output tap (Phase 2 — flow-grid virtualization) ----
  //
  // Session-state tracking (`sessionStates`, `lastOutputLines`, `staleTabs`,
  // `activityData`, …) used to be fed ONLY by `onOutput` callbacks fired from
  // MOUNTED `TerminalInstance` components. Phase 3 unmounts offscreen instances
  // to virtualize large grids — which would make those sessions state-blind.
  // This single always-mounted subscriber decouples tracking from instance mount
  // state: it taps `terminal-output` once per page-scope and feeds
  // `stateTracking.handleOutput` for every one of THIS page's tabs regardless of
  // which instances are currently rendered — and, since Phase 2 of the
  // many-sessions plan, regardless of whether a mounted pane is visible. Only
  // the *tracking* feed lives here (the `onOutput`→`handleOutput` wiring in
  // `ZoneGrid` is gone); `TerminalInstance` owns the xterm write path via a
  // per-terminal handler on the same demuxed listener (`terminalEventDemux`),
  // NOT a second `listen()` of its own.
  //
  // Placed at the per-`PageSessionScope` level (not once at the provider root):
  // each scope already owns exactly one page's tab roster AND that page's
  // `useSessionStateTracking` instance, and every `terminal-output` event's
  // `terminalId` belongs to exactly one page. So the scope both (a) knows which
  // tabs are "its own" and (b) holds the right `handleOutput` — a provider-root
  // tap would have to re-derive the owning page per event and route across
  // per-page trackers. Scopes are all always-mounted (see the class comment), so
  // one listener per scope stays alive across layout/zone/page switches.
  //
  // Decode + throttle live in the pure `terminalOutputTap` leaf (OOM lesson
  // from incident #532): one streaming `TextDecoder` per terminalId (byte-for-
  // byte identical to each instance's `outputDecoderRef`), and per-tab text
  // coalesced across a `requestAnimationFrame` so `handleOutput` is called once
  // per tab per frame, not once per raw event. Ordering is preserved per tab.
  const coalescerRef = useRef(new TerminalOutputCoalescer());
  const handleOutputRef = useRef(stateTracking.handleOutput);
  useEffect(() => {
    handleOutputRef.current = stateTracking.handleOutput;
  }, [stateTracking.handleOutput]);

  // Current owned-tab roster, read by the (stable) listener via a ref so a tab
  // open/close never re-subscribes the listener. Only real tabs have PTYs that
  // emit `terminal-output`; synthetic (test-fixture) tabs never do, so the real
  // `tabs` set is the correct ownership filter (matches what the instances that
  // previously fed tracking covered).
  const tabIdSetRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    const ids = new Set(tabs.map((t) => t.id));
    tabIdSetRef.current = ids;
    // Drop decoders/buffers for tabs that closed so they don't accumulate.
    coalescerRef.current.retain(ids);
  }, [tabs]);

  useEffect(() => {
    const coalescer = coalescerRef.current;
    let rafHandle: ReturnType<typeof requestAnimationFrame> | null = null;
    // Proxy-ack accumulator for tabs NOTHING is rendering — no mounted
    // TerminalInstance (virtualized-offscreen or non-active page) or a
    // mounted-but-HIDDEN one, which stops writing to xterm and stops its ack
    // timer under the Phase-2 visibility tier. The backend gates webview
    // emission on acked bytes (see session.rs EmissionGate); with nobody
    // render-acking, emission for the tab would stop at the high watermark,
    // and THIS tap — the consumer that keeps session-state tracking
    // mount-independent — would go blind. The tap is a real consumer, so it
    // acks the bytes it consumed, but ONLY while no pane is rendering the tab
    // (a rendering pane's render-based acks must not be double-counted); see
    // `terminalRenderConsumers.ts`. Acks batch per rAF flush so the invoke
    // rate is one call per tab per frame at most.
    const pendingAcks = new Map<string, number>();

    // rAF suspends in occluded/minimized windows (WebView2), which would
    // starve the proxy-acks below — the gap would cross the high watermark
    // and emission (and with it this tap's state-tracking feed) would pause
    // until window restore. Pair every rAF schedule with a setTimeout floor
    // so the flush fires even without paint ticks; whichever fires first
    // cancels the other.
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    const ACK_FLUSH_FLOOR_MS = 200;

    const flush = () => {
      if (rafHandle !== null) {
        cancelAnimationFrame(rafHandle);
        rafHandle = null;
      }
      if (timeoutHandle !== null) {
        clearTimeout(timeoutHandle);
        timeoutHandle = null;
      }
      const fn = handleOutputRef.current;
      for (const [tabId, text] of coalescer.drain()) {
        fn(tabId, text);
      }
      for (const [tabId, bytesAcked] of pendingAcks) {
        invoke("terminal_ack", { terminalId: tabId, bytesAcked }).catch(() => {});
      }
      pendingAcks.clear();
    };
    const scheduleFlush = () => {
      if (rafHandle !== null || timeoutHandle !== null) return;
      rafHandle = requestAnimationFrame(flush);
      timeoutHandle = setTimeout(flush, ACK_FLUSH_FLOOR_MS);
    };

    // Shares the window's ONE `terminal-output` listener with the per-terminal
    // demux (Phase 2 / A3) instead of installing a second global tap.
    const unlisten = subscribeTerminalOutputStream((payload) => {
      const tid = payload.terminalId;
      // Drop events for tabs this scope doesn't own (another page/window) or
      // doesn't know about — exactly one scope owns any given terminalId.
      if (!tabIdSetRef.current.has(tid)) return;
      const bytes = base64ToBytes(payload.data);
      coalescer.push(tid, bytes);
      if (bytes.length > 0 && !hasRenderConsumer(tid)) {
        pendingAcks.set(tid, (pendingAcks.get(tid) ?? 0) + bytes.length);
      }
      scheduleFlush();
    });

    return () => {
      if (rafHandle !== null) cancelAnimationFrame(rafHandle);
      if (timeoutHandle !== null) clearTimeout(timeoutHandle);
      // Do NOT flush on teardown: the scope only unmounts when its whole page is
      // removed, so its tracker + tabs are going away too — a trailing sub-frame
      // update would target unmounted state. Just release the listener.
      unlisten();
    };
  }, []);

  // ---- ShellInfra ----
  const fileConflicts = useFileConflicts();
  // Publishes into the page hot store; holds no reactive state of its own, so
  // a lock poll no longer re-renders this scope (and with it the whole page).
  useFileLockTracking(pageId, tabs);
  const sessionPersistence = useSessionPersistence(pageId);

  // ---- AiFeatures ----
  // Cross-hook ref wiring — shellIntegration needs setRightPanelMode
  // from workflowGen, but workflowGen is defined after shellIntegration.
  // Refs break the cycle, same pattern as the prior AiFeaturesProvider.
  const rightPanelModeSetterRef = useRef<
    React.Dispatch<
      React.SetStateAction<
        "transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null
      >
    >
  >(() => {});
  const selectedSessionSetterRef = useRef<React.Dispatch<React.SetStateAction<string | null>>>(
    () => {},
  );

  const shellIntegration = useShellIntegration({
    tabs,
    updateTab,
    renameTab,
    createTerminal,
    setSessionStates: stateTracking.setSessionStates,
    terminalRefs,
    setRightPanelMode: (v) => rightPanelModeSetterRef.current(v as never),
    setSelectedTranscriptSessionId: (v) => selectedSessionSetterRef.current(v as never),
    pageId,
  });

  const getScrollback = useCallback(
    (tabId: string, maxLines = 500): string => {
      const ref = terminalRefs.current.get(tabId);
      return ref?.current?.getScrollback?.(maxLines) ?? "";
    },
    [terminalRefs],
  );

  const getActiveSelection = useCallback((): string => {
    if (!activeId) return "";
    const ref = terminalRefs.current.get(activeId);
    return ref?.current?.getSelection?.() ?? "";
  }, [activeId, terminalRefs]);

  // `desktopNotify` was previously read off TransitionEffectsContext
  // (which lived ABOVE AiFeaturesContext in the prior provider tree).
  // Post-collapse, TerminalSession is the OUTERMOST provider and
  // TransitionEffects depends on it — so we can't reach in. The flag
  // is persisted in `instanceStorage` ("zone-desktop-notify") by the
  // same hook that owns it (`useStateTransitionEffects`); reading
  // directly here keeps the wire-up identical. Reactive to changes
  // through a `storage`-style poll would be over-engineering — the
  // operator toggling the setting can wait for the next session-
  // manager re-render (which fires on every session-state delta
  // anyway via the upstream sessionStates dep).
  const desktopNotifyFlag = instanceStorage.getItem("zone-desktop-notify") === "true";

  const sessionManager = useSessionManager({
    // Synthetic tabs included so `tabSessionMap` correlation places tab-backed
    // injected fakes (idle / error / completed) in the right StatusStrip
    // bucket. Empty in production (real transcripts carry no `injected_tab`).
    tabs: tabsWithSynthetic,
    sessionStates: stateTracking.sessionStates,
    staleTabs: stateTracking.staleTabs,
    transcriptSessions,
    sessionsLoading,
    desktopNotify: desktopNotifyFlag,
    onRefreshSessions: refreshSessions,
    onResumeSession: shellIntegration.handleResumeSession,
    onSelectSession: (sessionId: string) => {
      selectedSessionSetterRef.current(sessionId);
      rightPanelModeSetterRef.current("transcript" as never);
    },
  });

  const workflowGen = useWorkflowGeneration({
    activeId,
    tabs,
    loadMessages,
    onNavigateToBuilder,
    onNavigateToActive,
  });

  const analysis = useAnalysis({
    activeId,
    tabs,
    commandHistories: shellIntegration.commandHistories,
    getScrollback,
    getActiveSelection,
    latestPlanContent: workflowGen.latestPlanContent,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  const findingsActions = useFindingsActions({
    activeId,
    tabs,
    terminalRefs,
    createTerminal,
    pendingResumeRef: shellIntegration.pendingResumeRef,
    runGeneration: workflowGen.runGeneration,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  // Session summary (AI Tier 2).
  const [sessionSummary, setSessionSummary] = useState<string | null>(null);
  const [sessionSummaryLoading, setSessionSummaryLoading] = useState(false);

  const handleSummarizeSession = useCallback(
    async (messages: Array<{ msg_type: string; text: string }>) => {
      setSessionSummaryLoading(true);
      setSessionSummary(null);
      try {
        const text = messages
          .map((m) => `${m.msg_type === "user" ? "User" : "Assistant"}: ${m.text}`)
          .join("\n\n");
        const result = await invoke<{
          success: boolean;
          data?: unknown;
          message?: string;
        }>("analyze_session_summary", { input: text });
        if (result.success && result.data) {
          const data = result.data as
            | Array<{ type?: string; content?: string }>
            | { panels?: Array<{ content?: string }> };
          let summaryContent: string | null = null;

          if (Array.isArray(data)) {
            const markdownPanel = data.find((p) => p.type === "markdown" || p.content);
            summaryContent = markdownPanel?.content ?? null;
          } else if (data && "panels" in data && Array.isArray(data.panels)) {
            summaryContent = data.panels[0]?.content ?? null;
          }

          setSessionSummary(summaryContent || "Summary generated but no content available.");
        } else {
          setSessionSummary(result.message || "Unable to generate summary.");
        }
      } catch {
        setSessionSummary("Failed to generate summary.");
      } finally {
        setSessionSummaryLoading(false);
      }
    },
    [],
  );

  // Wire the cross-hook refs now that workflowGen is defined.
  useEffect(() => {
    rightPanelModeSetterRef.current = workflowGen.setRightPanelMode;
    selectedSessionSetterRef.current = workflowGen.setSelectedTranscriptSessionId;
  });

  // Memoize the value object. Keys in deps list mirror the prior
  // 4-provider memo signals so re-renders are stable.
  const value = useMemo<TerminalSessionContextValue>(
    () => ({
      // TerminalCore
      ...terminalManager,
      // Phase 1: override the manager's full tab set + creator with the
      // window-scoped versions so this window renders/creates only its tabs.
      tabs,
      createTerminal: createTerminalForWindow,
      pageId,
      zoneLayout,
      terminalRefs,
      pendingProfileSessionsRef,
      labelsAndTags,
      // SessionState (spread)
      ...stateTracking,
      snapshots,
      processOutputRef,
      activeFindings,
      allFindings,
      // ShellInfra
      fileConflicts,
      sessionPersistence,
      // AiFeatures
      shellIntegration,
      workflowGen,
      analysis,
      findingsActions,
      sessionManager,
      transcriptSessions,
      sessionsLoading,
      refreshSessions,
      loadMessages,
      sessionSummary,
      sessionSummaryLoading,
      handleSummarizeSession,
      getScrollback,
      getActiveSelection,
    }),
    [
      terminalManager,
      tabs,
      createTerminalForWindow,
      pageId,
      zoneLayout,
      labelsAndTags,
      stateTracking,
      snapshots,
      activeFindings,
      allFindings,
      fileConflicts,
      sessionPersistence,
      shellIntegration,
      workflowGen,
      analysis,
      findingsActions,
      sessionManager,
      transcriptSessions,
      sessionsLoading,
      refreshSessions,
      loadMessages,
      sessionSummary,
      sessionSummaryLoading,
      handleSummarizeSession,
      getScrollback,
      getActiveSelection,
    ],
  );

  // Publish this page's value to the lifted provider whenever it changes, and
  // retract it on unmount (page removed). Done in an effect — never during
  // render — so a child never sets parent state mid-render.
  useEffect(() => {
    register(pageId, value);
    return () => register(pageId, null);
  }, [register, pageId, value]);

  // Renders nothing: the lifted provider routes the active page's value into
  // context and renders the (single) page tree.
  return null;
});

interface TerminalSessionProviderProps {
  /**
   * Every terminal page that should keep live state (all stay mounted).
   * `defaultWorkingDir` (from `TerminalPageConfig`) is carried through so each
   * page's `createTerminal` can fall back to it — see `PageSessionScopeProps`.
   */
  pages: Array<{ id: string; defaultWorkingDir?: string }>;
  /** The page the operator is currently viewing — its value is surfaced. */
  activePageId: string;
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  children: ReactNode;
}

/**
 * Lifted, always-mounted terminal session provider (Phase 3 mount-hydration).
 *
 * Mounts ABOVE `TerminalPage` (in `App.tsx`) so it survives both app-tab
 * switches and terminal-PAGE switches. It renders one always-mounted
 * `PageSessionScope` per terminal page; each scope keeps its page's tab state
 * and runs its own live `terminal-created` listener. The provider surfaces the
 * ACTIVE page's value through `TerminalSessionContext`, so the single
 * `TerminalPage` consuming `useTerminalSession()` sees only its page's slice —
 * exactly as before — while page switches no longer destroy any page's state
 * and a `terminal-created` event is never dropped.
 *
 * Invariant: alive PTY ⇒ tab survives navigation; `terminal-created` never
 * dropped.
 */
export function TerminalSessionProvider({
  pages,
  activePageId,
  onNavigateToBuilder,
  onNavigateToActive,
  children,
}: TerminalSessionProviderProps) {
  const [values, setValues] = useState<Record<string, TerminalSessionContextValue | null>>({});

  const register = useCallback((pageId: string, value: TerminalSessionContextValue | null) => {
    setValues((prev) => {
      if (value === null) {
        if (!(pageId in prev)) return prev;
        const next = { ...prev };
        delete next[pageId];
        return next;
      }
      if (prev[pageId] === value) return prev;
      return { ...prev, [pageId]: value };
    });
  }, []);

  // Always mount the active page even if it isn't yet in `pages` (e.g. a page
  // removed from the list while still selected, mid-transition) so the page
  // tree always has a value to read once its scope publishes.
  const scopedPages = useMemo(() => {
    const scoped = pages.map((p) => ({ id: p.id, defaultWorkingDir: p.defaultWorkingDir }));
    if (!scoped.some((p) => p.id === activePageId)) {
      scoped.push({ id: activePageId, defaultWorkingDir: undefined });
    }
    return scoped;
  }, [pages, activePageId]);

  const activeValue = values[activePageId] ?? null;

  return (
    <>
      {scopedPages.map((p) => (
        <PageSessionScope
          key={p.id}
          pageId={p.id}
          defaultWorkingDir={p.defaultWorkingDir}
          register={register}
          onNavigateToBuilder={onNavigateToBuilder}
          onNavigateToActive={onNavigateToActive}
        />
      ))}
      {/* Gate the page tree on the active page's value. A freshly-mounted
          scope publishes its value in an effect (post-paint), so for the very
          first frame after an app boot or a switch to a brand-new page,
          `activeValue` is momentarily null. Rendering `children` then would
          throw in `useTerminalSession()`. The window is sub-frame; show the
          same spinner TerminalPage uses while not `initialized`. */}
      {activeValue ? (
        <TerminalSessionContext.Provider value={activeValue}>
          {children}
        </TerminalSessionContext.Provider>
      ) : (
        <div className="h-full flex items-center justify-center bg-[#1a1b26]">
          <div className="flex flex-col items-center gap-3">
            <div className="w-8 h-8 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
            <span className="text-[12px] text-[#565f89]">Loading terminals...</span>
          </div>
        </div>
      )}
    </>
  );
}

/**
 * The new single hook. Replaces the previous four:
 *
 *   - useTerminalCore()
 *   - useSessionState()  (terminal-page variant — NOT the hooks/ one)
 *   - useShellInfra()
 *   - useAiFeatures()
 *
 * Throws when called outside a `TerminalSessionProvider`.
 */
export function useTerminalSession(): TerminalSessionContextValue {
  const ctx = useContext(TerminalSessionContext);
  if (!ctx) {
    throw new Error("useTerminalSession must be used within a TerminalSessionProvider");
  }
  return ctx;
}
