import { useState, useEffect, useCallback, useRef } from "react";
import type { SessionState } from "./useZoneLayout";
import { detectSessionState } from "./sessionStateDetector";

export interface UseSessionStateTrackingParams {
  tabs: Array<{ id: string; isAlive: boolean; exitCode: number | null }>;
  processOutput?: (tabId: string, text: string) => void;
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
  const { tabs, processOutput } = params;

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

  // ── Idle / stale detection interval (2s) ──────────────────────────────────

  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      setSessionStates((prev) => {
        const next = { ...prev };
        let changed = false;
        for (const tab of tabs) {
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
      const newStale = new Set<string>();
      for (const tab of tabs) {
        const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
        const state = sessionStates[tab.id] ?? "idle";
        if (state === "working" && lastOutput > 0 && now - lastOutput > 60000) {
          newStale.add(tab.id);
        }
      }
      setStaleTabs((prev) => {
        if (prev.size !== newStale.size || [...newStale].some((id) => !prev.has(id))) {
          return newStale;
        }
        return prev;
      });
    }, 2000);
    return () => clearInterval(interval);
  }, [tabs, sessionStates]);

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
  }, [sessionStates]); // Re-start interval when states change for immediate update

  // ── Activity sparkline tick interval (2s) ─────────────────────────────────

  useEffect(() => {
    const interval = setInterval(() => {
      const buffers = activityBuffersRef.current;
      for (const tabId of Object.keys(buffers)) {
        // Push current accumulator and reset; keep last 30 points
        buffers[tabId] = [...(buffers[tabId] ?? []), 0].slice(-30);
      }
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

      // Track last output lines for compact view (keep last 20 non-empty lines)
      // Normal display shows 6; hover preview shows up to 20
      // Strip ANSI escape sequences for cleaner display
      // eslint-disable-next-line no-control-regex
      const stripped = text.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "").replace(/\r/g, "");
      const newLines = stripped.split("\n").filter((l) => l.trim().length > 0);
      if (newLines.length > 0) {
        setLastOutputLines((prev) => {
          const existing = prev[tabId] ?? [];
          const combined = [...existing, ...newLines].slice(-20);
          return { ...prev, [tabId]: combined };
        });
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
    activityData: activityBuffersRef.current,
    prevSessionStatesRef,
    handleExit,
    handleOutput,
  };
}
