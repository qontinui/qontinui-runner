import { useState, useEffect, useCallback, useRef } from "react";
import type { SessionState } from "./useZoneLayout";
import { detectSessionState } from "./sessionStateDetector";

export interface UseSessionStateTrackingParams {
  tabs: Array<{ id: string; isAlive: boolean; exitCode: number | null }>;
  processOutput?: (tabId: string, text: string) => void;
  /** Read rendered lines from the xterm buffer (handles cursor movements correctly). */
  getBufferLines?: (tabId: string, maxLines: number) => string[];
}

export interface UseSessionStateTrackingReturn {
  sessionStates: Record<string, SessionState>;
  setSessionStates: React.Dispatch<React.SetStateAction<Record<string, SessionState>>>;
  lastOutputLines: Record<string, string[]>;
  staleTabs: Set<string>;
  stateDurations: Record<string, string>;
  stateTimeAccum: React.MutableRefObject<Record<SessionState, number>>;
  stateEntryTimeRef: React.MutableRefObject<Record<string, number>>;
  activityData: Record<string, number[]>;
  prevSessionStatesRef: React.MutableRefObject<Record<string, SessionState>>;
  handleExit: (terminalId: string, exitCode: number | null) => void;
  handleOutput: (tabId: string, text: string) => void;
}

export function useSessionStateTracking(
  params: UseSessionStateTrackingParams,
): UseSessionStateTrackingReturn {
  const { tabs, processOutput, getBufferLines } = params;
  const getBufferLinesRef = useRef(getBufferLines);
  useEffect(() => {
    getBufferLinesRef.current = getBufferLines;
  }, [getBufferLines]);

  // ── State ─────────────────────────────────────────────────────────────────

  const [sessionStates, setSessionStates] = useState<Record<string, SessionState>>({});
  const [lastOutputLines, setLastOutputLines] = useState<Record<string, string[]>>({});
  const [staleTabs, setStaleTabs] = useState<Set<string>>(new Set());
  const [stateDurations, setStateDurations] = useState<Record<string, string>>({});

  // ── Refs ───────────────────────────────────────────────────────────────────

  const lastOutputTimeRef = useRef<Record<string, number>>({});
  const stateEntryTimeRef = useRef<Record<string, number>>({});
  const prevSessionStatesRef = useRef<Record<string, SessionState>>({});

  const stateTimeAccum = useRef<Record<SessionState, number>>({
    idle: 0,
    working: 0,
    "needs-input": 0,
    completed: 0,
    error: 0,
  });

  // Activity sparkline: ring buffer of output byte counts per 2s interval per tab
  const activityBuffersRef = useRef<Record<string, number[]>>({});
  // State snapshot of activity buffers, updated on the 2s tick interval
  const [activityDataSnapshot, setActivityDataSnapshot] = useState<Record<string, number[]>>({});

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

      setSessionStates((prev) => {
        const next = { ...prev };
        let changed = false;
        for (const tab of currentTabs) {
          const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
          const current = next[tab.id] ?? "idle";
          if (!tab.isAlive && current !== "completed" && current !== "error") {
            next[tab.id] = tab.exitCode === 0 || tab.exitCode === null ? "completed" : "error";
            changed = true;
          } else if (current === "working" && now - lastOutput > 10000) {
            next[tab.id] = "idle";
            changed = true;
          }
        }
        return changed ? next : prev;
      });

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
  }, []); // Stable interval — reads tabs via ref

  // ── Clean up state for removed tabs ──────────────────────────────────────

  useEffect(() => {
    const tabIdSet = new Set(tabs.map((t) => t.id));

    // eslint-disable-next-line react-hooks/set-state-in-effect -- cleanup state for removed tabs
    setSessionStates((prev) => {
      const deadKeys = Object.keys(prev).filter((id) => !tabIdSet.has(id));
      if (deadKeys.length === 0) return prev;
      const next = { ...prev };
      for (const key of deadKeys) delete next[key];
      return next;
    });

    setLastOutputLines((prev) => {
      const deadKeys = Object.keys(prev).filter((id) => !tabIdSet.has(id));
      if (deadKeys.length === 0) return prev;
      const next = { ...prev };
      for (const key of deadKeys) delete next[key];
      return next;
    });

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
  }, [tabs]);

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
      const durations: Record<string, string> = {};
      for (const [tabId, entryTime] of Object.entries(stateEntryTimeRef.current)) {
        durations[tabId] = formatDuration(now - entryTime);
      }
      setStateDurations(durations);
    };

    update();
    const interval = setInterval(update, 10000);
    return () => clearInterval(interval);
  }, []); // Stable interval — duration updates independently of state changes

  // ── Activity sparkline tick interval (2s) ─────────────────────────────────

  useEffect(() => {
    const interval = setInterval(() => {
      const buffers = activityBuffersRef.current;
      for (const tabId of Object.keys(buffers)) {
        // Push current accumulator and reset; keep last 30 points
        buffers[tabId] = [...(buffers[tabId] ?? []), 0].slice(-30);
      }
      // Update the state snapshot so consumers re-render with latest data
      setActivityDataSnapshot({ ...buffers });
    }, 2000);
    return () => clearInterval(interval);
  }, []);

  // ── Callbacks ─────────────────────────────────────────────────────────────

  const handleExit = useCallback((terminalId: string, exitCode: number | null) => {
    // Note: the caller (TerminalPage) should also call updateTab to set isAlive/exitCode
    setSessionStates((prev) => ({
      ...prev,
      [terminalId]: exitCode === 0 || exitCode === null ? "completed" : "error",
    }));
  }, []);

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

      // Use the session state detector for pattern matching
      setSessionStates((prev) => {
        const current = prev[tabId] ?? "idle";
        const detected = detectSessionState(text, current);
        if (detected && detected !== current) {
          return { ...prev, [tabId]: detected };
        }
        return prev;
      });

      // Track last output lines for compact view (keep last 20 non-empty lines).
      // Read from the xterm buffer which correctly handles cursor movements,
      // line rewrites, and all escape sequences. Falls back to ANSI-stripping
      // the raw output if the buffer isn't available yet.
      const bufferReader = getBufferLinesRef.current;
      if (bufferReader) {
        const lines = bufferReader(tabId, 20);
        if (lines.length > 0) {
          setLastOutputLines((prev) => ({ ...prev, [tabId]: lines }));
        }
      } else {
        // Fallback: strip ANSI and accumulate from raw output
        const stripped = text
          // eslint-disable-next-line no-control-regex
          .replace(/\x1b\[[0-9;:?>=! ]*[a-zA-Z@`]/g, "") // CSI sequences
          // eslint-disable-next-line no-control-regex
          .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "") // OSC sequences
          // eslint-disable-next-line no-control-regex
          .replace(/\x1b[()#%*+\-./][0-9A-Za-z]/g, "") // Character set designations
          // eslint-disable-next-line no-control-regex
          .replace(/\x1b[NODMEHc789>=<]/g, "") // Simple escape sequences
          // eslint-disable-next-line no-control-regex
          .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "") // Stray control chars
          .replace(/\r/g, "");
        const newLines = stripped.split("\n").filter((l) => l.trim().length > 0);
        if (newLines.length > 0) {
          setLastOutputLines((prev) => {
            const existing = prev[tabId] ?? [];
            const combined = [...existing, ...newLines];
            const deduped: string[] = [];
            for (const line of combined) {
              if (deduped.length === 0 || line !== deduped[deduped.length - 1]) {
                deduped.push(line);
              }
            }
            return { ...prev, [tabId]: deduped.slice(-20) };
          });
        }
      }

      processOutput?.(tabId, text);
    },
    [processOutput],
  );

  return {
    sessionStates,
    setSessionStates,
    lastOutputLines,
    staleTabs,
    stateDurations,
    stateTimeAccum,
    stateEntryTimeRef,
    activityData: activityDataSnapshot,
    prevSessionStatesRef,
    handleExit,
    handleOutput,
  };
}
