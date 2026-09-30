import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import type { SessionState } from "./useZoneLayout";
import { detectSessionState, needsInputShape } from "./sessionStateDetector";
import {
  TERMINAL_AGENT_STATE_EVENT,
  fetchTerminalAgentStates,
  isEventSourced,
  isNeedsInputState,
  isTerminalAgentStateEvent,
  offerAgentObservation,
  sessionStateToObservation,
  verdictToSessionState,
  type AgentTruthEntry,
  type TerminalAgentStateEvent,
} from "./agentTruth";
import { applyActivityDigest } from "./activityDigestTracking";
import { nextOutputLines, OUTPUT_LINES_WINDOW } from "./outputLineTracking";
import { getTerminalHotStore } from "./terminalHotStore";

/** Element-wise equality for the sparkline ring buffers. */
function sameNumbers(a: number[] | undefined, b: number[]): boolean {
  if (!a || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export interface UseSessionStateTrackingParams {
  /**
   * Terminal page that owns these tabs. Hot per-tab data (`lastOutputLines`,
   * `activityData`, `stateDurations`) is published into that page's
   * {@link getTerminalHotStore} instead of React state, so one output frame
   * no longer re-renders every `useTerminalSession()` consumer.
   */
  pageId: string;
  tabs: Array<{
    id: string;
    isAlive: boolean;
    exitCode: number | null;
    /**
     * True when the tab's PTY child runs Claude with tool permissions bypassed
     * (`--dangerously-skip-permissions` / `--permission-mode bypassPermissions`).
     * Forwarded to `detectSessionState` so approval-shaped TTY patterns are
     * suppressed for it — a bypass session can never await tool approval, so
     * any such match is a phantom (plan
     * `2026-06-07-runner-continuation-defer-and-phantom-needs-input.md`).
     */
    bypassPermissions?: boolean;
  }>;
  processOutput?: (tabId: string, text: string) => void;
  /** Read rendered lines from the xterm buffer (handles cursor movements correctly). */
  getBufferLines?: (tabId: string, maxLines: number) => string[];
  /**
   * Pre-age seeds for `lastOutputTimeRef`, keyed by tab id (used by the
   * debug-gated synthetic-tab seam, `syntheticTabs.ts`). Each entry is applied
   * ONLY if the tab has no recorded output yet, so a real recording is never
   * clobbered. Seeding a value in the past lets the existing 60s staleness
   * sweep classify a synthetic "idle" tab stale on its next tick.
   */
  seedLastOutput?: Record<string, number>;
}

/**
 * Cold (non-per-frame) tracking surface. The hot maps — `lastOutputLines`,
 * `activityData`, `stateDurations` — deliberately do NOT appear here: they
 * live in the page's {@link getTerminalHotStore} and are read through
 * `useHotField` / `useTabHotSlice`.
 */
export interface UseSessionStateTrackingReturn {
  /**
   * What each tab's chip renders. For a tab whose runner verdict comes from an
   * event channel (hooks / OSC 9999) this is the verdict's projection; for any
   * other tab it is the webview's own fallback inference, which is also
   * offered to the runner as a `regex` observation. `unknown` means nothing
   * has been observed — never read it as `idle`.
   */
  sessionStates: Record<string, SessionState>;
  setSessionStates: React.Dispatch<React.SetStateAction<Record<string, SessionState>>>;
  /**
   * The runner's merged verdict + hook delivery per terminal id, from the
   * `terminal-agent-state` event (and `get_terminal_agent_states` at mount).
   * Empty on a runner build that predates the reducer. Keystroke writers read
   * it ONLY through `isAuthoritativePermissionAsk`.
   */
  agentVerdicts: Record<string, AgentTruthEntry>;
  /**
   * The one door for any NON-event inference about a tab's state (the screen
   * detector, shell integration). Ignored for a tab whose verdict is
   * event-sourced; otherwise applied locally and offered to the runner's
   * reducer. `compute` receives the current state and returns the new one, or
   * `null` for no change.
   */
  offerInferredState: (
    tabId: string,
    compute: (current: SessionState) => SessionState | null,
  ) => void;
  staleTabs: Set<string>;
  stateTimeAccum: React.MutableRefObject<Record<SessionState, number>>;
  stateEntryTimeRef: React.MutableRefObject<Record<string, number>>;
  prevSessionStatesRef: React.MutableRefObject<Record<string, SessionState>>;
  handleExit: (terminalId: string, exitCode: number | null) => void;
  handleOutput: (tabId: string, text: string) => void;
  /**
   * Feed tracking from the runner's `terminal-activity` digest instead of from
   * the output stream — the path for tabs at visibility tier `unwatched` for
   * which the runner emits no `terminal-output` at all (plan Phase 5 / A4).
   * A session configured with `unwatched_flush_interval_ms` is fed through
   * {@link UseSessionStateTrackingReturn.handleOutput} instead and never
   * reaches this one.
   */
  handleActivityDigest: (tabId: string, bytesDelta: number, lines: string[]) => void;
}

export function useSessionStateTracking(
  params: UseSessionStateTrackingParams,
): UseSessionStateTrackingReturn {
  const { pageId, tabs, processOutput, getBufferLines, seedLastOutput } = params;
  const getBufferLinesRef = useRef(getBufferLines);
  useEffect(() => {
    getBufferLinesRef.current = getBufferLines;
  }, [getBufferLines]);

  const hotStore = getTerminalHotStore(pageId);

  // ── State ─────────────────────────────────────────────────────────────────

  const [sessionStates, setSessionStatesRaw] = useState<Record<string, SessionState>>({});
  const [staleTabs, setStaleTabs] = useState<Set<string>>(new Set());
  const [agentVerdicts, setAgentVerdicts] = useState<Record<string, AgentTruthEntry>>({});

  /**
   * Synchronous mirror of `sessionStates`. Every write goes through
   * {@link setSessionStates} below, which computes the next map from this ref
   * and publishes it, so the detector always sees the state it last produced
   * (and the offer to the runner is made outside any React updater).
   */
  const sessionStatesRef = useRef<Record<string, SessionState>>({});
  const agentVerdictsRef = useRef<Record<string, AgentTruthEntry>>({});

  const setSessionStates = useCallback<
    React.Dispatch<React.SetStateAction<Record<string, SessionState>>>
  >((action) => {
    const prev = sessionStatesRef.current;
    const next = typeof action === "function" ? action(prev) : action;
    if (next === prev) return;
    sessionStatesRef.current = next;
    setSessionStatesRaw(next);
  }, []);

  // ── Refs ───────────────────────────────────────────────────────────────────

  const lastOutputTimeRef = useRef<Record<string, number>>({});
  const stateEntryTimeRef = useRef<Record<string, number>>({});
  const prevSessionStatesRef = useRef<Record<string, SessionState>>({});

  const stateTimeAccum = useRef<Record<SessionState, number>>({
    unknown: 0,
    idle: 0,
    working: 0,
    "needs-input": 0,
    completed: 0,
    error: 0,
  });

  // Activity sparkline: ring buffer of output byte counts per 2s interval per tab
  const activityBuffersRef = useRef<Record<string, number[]>>({});

  // ── Idle / stale detection interval (2s) ──────────────────────────────────

  // Use a ref for tabs so the interval doesn't need to be recreated on every
  // tab change. The interval reads the latest value via the ref.
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      const currentTabs = tabsRef.current;

      const quietTabs: string[] = [];
      setSessionStates((prev) => {
        const next = { ...prev };
        let changed = false;
        for (const tab of currentTabs) {
          const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
          const current = next[tab.id] ?? "unknown";
          if (!tab.isAlive && current !== "completed" && current !== "error") {
            next[tab.id] = tab.exitCode === 0 || tab.exitCode === null ? "completed" : "error";
            changed = true;
          } else if (
            current === "working" &&
            now - lastOutput > 10000 &&
            // A quiet screen is not evidence against an event-sourced verdict
            // (a 12-minute `rm -rf` is legitimately quiet).
            !isEventSourced(agentVerdictsRef.current[tab.id]?.verdict)
          ) {
            next[tab.id] = "idle";
            quietTabs.push(tab.id);
            changed = true;
          }
        }
        return changed ? next : prev;
      });
      for (const tabId of quietTabs) {
        offerAgentObservation({ terminalId: tabId, source: "screen_stability", busy: false });
      }

      // Detect stale "working" sessions (no output for 60s)
      // Read sessionStates via the updater to get the latest value
      setStaleTabs((prevStale) => {
        const newStale = new Set<string>();
        // Use a sync read of sessionStates via a separate ref
        // We only need the "working" check, so read from setSessionStates updater isn't needed
        for (const tab of currentTabs) {
          const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
          if (lastOutput > 0 && now - lastOutput > 60000) {
            newStale.add(tab.id);
          }
        }
        if (prevStale.size !== newStale.size || [...newStale].some((id) => !prevStale.has(id))) {
          return newStale;
        }
        return prevStale;
      });
    }, 2000);
    return () => clearInterval(interval);
  }, [setSessionStates]); // Stable interval — reads tabs via ref

  // ── Seed pre-aged lastOutput for synthetic (test-fixture) tabs ────────────
  //
  // Apply each seed ONLY when the tab has no recorded output yet so a real
  // recording is never clobbered. The seeded (past) timestamp lets the 60s
  // staleness sweep above mark a synthetic "idle" tab stale on its next tick.
  // The dead-tab cleanup effect below removes vanished tabs' ref entries, so
  // no extra cleanup is needed here.
  useEffect(() => {
    if (!seedLastOutput) return;
    for (const [tabId, ts] of Object.entries(seedLastOutput)) {
      if (lastOutputTimeRef.current[tabId] === undefined) {
        lastOutputTimeRef.current[tabId] = ts;
      }
    }
  }, [seedLastOutput]);

  // ── Clean up state for removed tabs ──────────────────────────────────────

  useEffect(() => {
    const tabIdSet = new Set(tabs.map((t) => t.id));

    setSessionStates((prev) => {
      const deadKeys = Object.keys(prev).filter((id) => !tabIdSet.has(id));
      // A verdict can arrive before its tab exists on this page (initial
      // load races tab hydration); render it once the tab appears.
      const seeds = tabs.filter((t) => {
        const entry = agentVerdictsRef.current[t.id];
        return prev[t.id] === undefined && !!entry && entry.verdict.state.name !== "unknown";
      });
      if (deadKeys.length === 0 && seeds.length === 0) return prev;
      const next = { ...prev };
      for (const key of deadKeys) delete next[key];
      for (const t of seeds) next[t.id] = verdictToSessionState(agentVerdictsRef.current[t.id].verdict);
      return next;
    });

    // Drop every hot-store entry (output lines, sparkline, duration, lock
    // state) belonging to a tab that no longer exists.
    hotStore.retainTabs(tabIdSet);

    // Clean up refs too
    for (const id of Object.keys(lastOutputTimeRef.current)) {
      if (!tabIdSet.has(id)) delete lastOutputTimeRef.current[id];
    }
    for (const id of Object.keys(stateEntryTimeRef.current)) {
      if (!tabIdSet.has(id)) delete stateEntryTimeRef.current[id];
    }
    for (const id of Object.keys(prevSessionStatesRef.current)) {
      if (!tabIdSet.has(id)) delete prevSessionStatesRef.current[id];
    }
    for (const id of Object.keys(activityBuffersRef.current)) {
      if (!tabIdSet.has(id)) delete activityBuffersRef.current[id];
    }
  }, [tabs, hotStore, setSessionStates]);

  // ── Runner verdicts (`terminal-agent-state`) ──────────────────────────────
  //
  // The merge lives in the runner's `agent_truth` reducer; this only mirrors
  // it. A runner build without the event simply never calls back, leaving the
  // fallback inference below in charge — the defensive default.

  const applyAgentState = useCallback(
    (payload: TerminalAgentStateEvent) => {
      if (!isTerminalAgentStateEvent(payload)) return;
      const entry: AgentTruthEntry = {
        verdict: payload.verdict,
        hookDelivery: payload.hookDelivery ?? { status: "unknown" },
      };
      agentVerdictsRef.current = { ...agentVerdictsRef.current, [payload.terminalId]: entry };
      setAgentVerdicts(agentVerdictsRef.current);
      // A verdict that has observed nothing must not erase what the fallback
      // has already inferred; every other verdict is what the chip renders.
      if (entry.verdict.state.name === "unknown") return;
      if (!tabsRef.current.some((t) => t.id === payload.terminalId)) return;
      const mapped = verdictToSessionState(entry.verdict);
      setSessionStates((prev) =>
        prev[payload.terminalId] === mapped ? prev : { ...prev, [payload.terminalId]: mapped },
      );
    },
    [setSessionStates],
  );

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    try {
      listen<TerminalAgentStateEvent>(TERMINAL_AGENT_STATE_EVENT, (event) => {
        if (!cancelled) applyAgentState(event.payload);
      })
        .then((fn) => {
          if (cancelled) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // No Tauri event bridge (tests, plain browser): fallback inference only.
    }
    void fetchTerminalAgentStates().then((rows) => {
      if (cancelled) return;
      for (const row of rows) applyAgentState(row);
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [applyAgentState]);

  /**
   * Apply a non-event inference for `tabId` and offer it to the runner, unless
   * an event channel already owns this tab's state.
   */
  const applyInference = useCallback(
    (
      tabId: string,
      compute: (current: SessionState) => SessionState | null,
      /** Lazily classifies a needs-input match — only called when one is offered. */
      shapeOf?: () => "approval_shaped" | "question_shaped" | null,
    ): void => {
      if (isEventSourced(agentVerdictsRef.current[tabId]?.verdict)) return;
      const current = sessionStatesRef.current[tabId] ?? "unknown";
      const next = compute(current);
      if (!next || next === current) return;
      setSessionStates((prev) => ({ ...prev, [tabId]: next }));
      const shape = isNeedsInputState(next) ? (shapeOf?.() ?? undefined) : undefined;
      const state = sessionStateToObservation(next, shape);
      if (state) offerAgentObservation({ terminalId: tabId, source: "regex", state });
    },
    [setSessionStates],
  );

  const offerInferredState = useCallback(
    (tabId: string, compute: (current: SessionState) => SessionState | null) =>
      applyInference(tabId, compute),
    [applyInference],
  );

  // ── Duration formatting interval (10s) ────────────────────────────────────

  useEffect(() => {
    const formatDuration = (ms: number): string => {
      const seconds = Math.floor(ms / 1000);
      if (seconds < 60) return `${seconds}s`;
      const minutes = Math.floor(seconds / 60);
      if (minutes < 60) return `${minutes}m`;
      const hours = Math.floor(minutes / 60);
      const remainMin = minutes % 60;
      return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
    };

    const update = () => {
      const now = Date.now();
      // Reuse the previously-published string for every tab whose formatted
      // duration is unchanged, so the store's identity check bails out and no
      // consumer re-renders. Without this the sweep published a brand-new
      // object every 10 s whether or not any label had actually ticked over.
      const published = hotStore.getField("stateDurations");
      const durations: Record<string, string> = {};
      for (const [tabId, entryTime] of Object.entries(stateEntryTimeRef.current)) {
        const formatted = formatDuration(now - entryTime);
        const prev = published[tabId];
        durations[tabId] = prev === formatted ? prev : formatted;
      }
      hotStore.setField("stateDurations", durations);
    };

    update();
    const interval = setInterval(update, 10000);
    return () => clearInterval(interval);
  }, [hotStore]); // Stable interval — duration updates independently of state changes

  // ── Activity sparkline tick interval (2s) ─────────────────────────────────

  useEffect(() => {
    const interval = setInterval(() => {
      const buffers = activityBuffersRef.current;
      const published = hotStore.getField("activityData");
      const next: Record<string, number[]> = {};
      for (const tabId of Object.keys(buffers)) {
        // Push current accumulator and reset; keep last 30 points. The ref
        // buffer always rotates (otherwise later output would land in an
        // already-elapsed 2 s bucket) — only PUBLISHING is conditional.
        const rotated = [...(buffers[tabId] ?? []), 0].slice(-30);
        buffers[tabId] = rotated;
        const prev = published[tabId];
        // Reuse the published array when the values are identical (an idle
        // tab whose ring is already full of zeros), so the store's identity
        // check bails and no sparkline re-renders. Publish a COPY otherwise:
        // `handleOutput` mutates the ref buffer's last slot in place, and a
        // published snapshot must not change under its consumers.
        next[tabId] = prev && sameNumbers(prev, rotated) ? prev : [...rotated];
      }
      hotStore.setField("activityData", next);
    }, 2000);
    return () => clearInterval(interval);
  }, [hotStore]);

  // ── Callbacks ─────────────────────────────────────────────────────────────

  const handleExit = useCallback((terminalId: string, exitCode: number | null) => {
    // Note: the caller (TerminalPage) should also call updateTab to set isAlive/exitCode
    setSessionStates((prev) => ({
      ...prev,
      [terminalId]: exitCode === 0 || exitCode === null ? "completed" : "error",
    }));
  }, [setSessionStates]);

  const handleOutput = useCallback(
    (tabId: string, text: string) => {
      lastOutputTimeRef.current[tabId] = Date.now();

      // Accumulate bytes for sparkline
      if (!activityBuffersRef.current[tabId]) {
        activityBuffersRef.current[tabId] = [];
      }
      const buf = activityBuffersRef.current[tabId];
      if (buf.length === 0) buf.push(0);
      buf[buf.length - 1] += text.length;

      // Use the session state detector for pattern matching. Look up the
      // tab's bypass flag (via the stable `tabsRef`, so this callback need not
      // re-create when tabs change) so the detector can suppress approval-
      // shaped patterns for bypass sessions.
      //
      // For a tab whose verdict is event-sourced (hooks / OSC 9999) the
      // detector is NOT run at all — `applyInference` returns before
      // `compute` is called.
      const bypassPermissions =
        tabsRef.current.find((t) => t.id === tabId)?.bypassPermissions === true;
      applyInference(
        tabId,
        (current) => detectSessionState(text, current, { bypassPermissions }),
        () => needsInputShape(text, { bypassPermissions }),
      );

      // Track last output lines for compact view. `nextOutputLines` owns the
      // choice between the tab's xterm buffer (authoritative) and the ANSI-strip
      // fallback (for a tab with no buffer to read), and returns `null` when
      // this chunk changes nothing. See that module for why the fallback keys on
      // an EMPTY read rather than on a missing reader, and why that stopped
      // being harmless once `unwatched_flush_interval_ms` was wired through.
      const bufferReader = getBufferLinesRef.current;
      const bufferLines = bufferReader ? bufferReader(tabId, OUTPUT_LINES_WINDOW) : [];
      const nextLines = nextOutputLines(bufferLines, text, () =>
        hotStore.getLastOutputLines(tabId),
      );
      if (nextLines) {
        hotStore.setTabOutputLines(tabId, nextLines);
      }

      processOutput?.(tabId, text);
    },
    [processOutput, hotStore, applyInference],
  );

  /**
   * Feed state tracking from the runner's `terminal-activity` digest — the
   * replacement feed for tabs the runner has stopped emitting output for
   * (visibility tier `unwatched`; plan Phase 5 / A4).
   *
   * It carries what the tap used to derive from raw bytes, but better:
   *  - `bytesDelta` is what the PTY actually produced, so an unwatched tab's
   *    sparkline stays comparable with a focused tab's (the digest's own text
   *    length would not be);
   *  - `lines` is the RENDERED server-side VT grid, which resolves cursor
   *    motion, line rewrites and full-frame TUI redraws — strictly more
   *    faithful than the regex ANSI-strip fallback an unmounted tab used to
   *    get, since no xterm buffer exists to read for these tabs.
   *
   * Deliberately NOT routed through `handleOutput`: that would double-count
   * `text.length` into the sparkline and try to read a buffer that isn't there.
   */
  const handleActivityDigest = useCallback(
    (tabId: string, bytesDelta: number, lines: string[]) => {
      const { outputLines, detectorText } = applyActivityDigest(
        {
          lastOutputTime: lastOutputTimeRef.current,
          activityBuffers: activityBuffersRef.current,
        },
        tabId,
        bytesDelta,
        lines,
        Date.now(),
      );

      if (outputLines) {
        hotStore.setTabOutputLines(tabId, outputLines);
      }
      if (detectorText.length === 0) return;

      // The rendered screen tail is what a state chip keys off, so run the
      // same detector the tap runs — on the digest instead of on raw bytes.
      const bypassPermissions =
        tabsRef.current.find((t) => t.id === tabId)?.bypassPermissions === true;
      applyInference(
        tabId,
        (current) => detectSessionState(detectorText, current, { bypassPermissions }),
        () => needsInputShape(detectorText, { bypassPermissions }),
      );
      processOutput?.(tabId, detectorText);
    },
    [processOutput, hotStore, applyInference],
  );

  // Memoize the return so the value object's identity only changes when
  // something in it actually changed. Previously this was a bare object
  // literal, which made the whole `TerminalSessionContext` value churn on
  // every render of the page scope (plan §0 A1).
  return useMemo(
    () => ({
      sessionStates,
      setSessionStates,
      agentVerdicts,
      offerInferredState,
      staleTabs,
      stateTimeAccum,
      stateEntryTimeRef,
      prevSessionStatesRef,
      handleExit,
      handleOutput,
      handleActivityDigest,
    }),
    [
      sessionStates,
      setSessionStates,
      agentVerdicts,
      offerInferredState,
      staleTabs,
      handleExit,
      handleOutput,
      handleActivityDigest,
    ],
  );
}
