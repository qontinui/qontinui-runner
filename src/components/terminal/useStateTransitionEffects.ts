import { useState, useEffect, useCallback, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import type { SessionState } from "./useZoneLayout";
import { playNeedsInputChime, playCompletionChime, playErrorAlert } from "./notificationSound";

export interface UseStateTransitionEffectsParams {
  sessionStates: Record<string, SessionState>;
  prevSessionStatesRef: React.MutableRefObject<Record<string, SessionState>>;
  tabs: Array<{ id: string; title: string; exitCode?: number | null }>;
  assignments: Record<number, string>;
  lastOutputLines: Record<string, string[]>;
  terminalRefs: Map<string, React.RefObject<{ writeToTerminal: (data: string) => void } | null>>;
  stateEntryTimeRef: React.MutableRefObject<Record<string, number>>;
  stateTimeAccumRef: React.MutableRefObject<Record<SessionState, number>>;
  setFocusedZone: (z: number) => void;
  handleRestartInZone: (zoneIdx: number) => void;
  addHistoryEvent: (type: string, session: string, zone?: number, color?: string) => void;
}

export interface UseStateTransitionEffectsReturn {
  flashingTabs: Set<string>;
  unseenNeedsInput: Set<string>;
  setUnseenNeedsInput: React.Dispatch<React.SetStateAction<Set<string>>>;
  autoApprovePatterns: string[];
  setAutoApprovePatterns: (patterns: string[]) => void;
  autoApproveCount: number;
  autoFocusNeedsInput: boolean;
  toggleAutoFocus: () => void;
  soundEnabled: boolean;
  toggleSound: () => void;
  desktopNotify: boolean;
  setDesktopNotify: React.Dispatch<React.SetStateAction<boolean>>;
  autoRestart: boolean;
  setAutoRestart: React.Dispatch<React.SetStateAction<boolean>>;
  autoRestartCount: number;
  pendingRestarts: Record<number, number>;
  cancelPendingRestart: (zoneIndex: number) => void;
  batchBarDismissed: boolean;
  setBatchBarDismissed: React.Dispatch<React.SetStateAction<boolean>>;
}

export function useStateTransitionEffects(
  params: UseStateTransitionEffectsParams,
): UseStateTransitionEffectsReturn {
  const {
    sessionStates,
    prevSessionStatesRef,
    tabs,
    assignments,
    lastOutputLines,
    terminalRefs,
    stateEntryTimeRef,
    stateTimeAccumRef,
    setFocusedZone,
    handleRestartInZone,
    addHistoryEvent,
  } = params;

  // ── State ─────────────────────────────────────────────────────────────────

  const [flashingTabs, setFlashingTabs] = useState<Set<string>>(new Set());
  const [unseenNeedsInput, setUnseenNeedsInput] = useState<Set<string>>(new Set());
  const [batchBarDismissed, setBatchBarDismissed] = useState(false);

  const [autoApprovePatterns, setAutoApprovePatternsState] = useState<string[]>(() =>
    instanceStorage.getJSON<string[]>("zone-auto-approve-patterns", []),
  );
  const [autoApproveCount, setAutoApproveCount] = useState(0);

  const [autoRestart, setAutoRestart] = useState(
    () => instanceStorage.getItem("zone-auto-restart") === "true",
  );
  const [autoRestartCount, setAutoRestartCount] = useState(0);
  const handleRestartInZoneRef = useRef<(zoneIdx: number) => void>(() => {});
  useEffect(() => {
    handleRestartInZoneRef.current = handleRestartInZone;
  });

  const [pendingRestarts, setPendingRestarts] = useState<Record<number, number>>({});
  const pendingRestartTimersRef = useRef<Record<number, ReturnType<typeof setTimeout>>>({});

  const [autoFocusNeedsInput, setAutoFocusNeedsInput] = useState(
    () => instanceStorage.getItem("zone-auto-focus") === "true",
  );

  const [soundEnabled, setSoundEnabled] = useState(
    () => instanceStorage.getItem("zone-sound-notify") === "true",
  );

  const [desktopNotify, setDesktopNotify] = useState(
    () => instanceStorage.getItem("zone-desktop-notify") === "true",
  );

  const prevNeedsInputCountRef = useRef(0);

  // ── Callbacks ─────────────────────────────────────────────────────────────

  const setAutoApprovePatterns = useCallback((patterns: string[]) => {
    setAutoApprovePatternsState(patterns);
  }, []);

  const cancelPendingRestart = useCallback((zoneIndex: number) => {
    const timer = pendingRestartTimersRef.current[zoneIndex];
    if (timer) {
      clearTimeout(timer);
      delete pendingRestartTimersRef.current[zoneIndex];
    }
    setPendingRestarts((prev) => {
      const next = { ...prev };
      delete next[zoneIndex];
      return next;
    });
  }, []);

  const toggleAutoFocus = useCallback(() => {
    setAutoFocusNeedsInput((prev) => {
      const next = !prev;
      instanceStorage.setItem("zone-auto-focus", String(next));
      return next;
    });
  }, []);

  const toggleSound = useCallback(() => {
    setSoundEnabled((prev) => {
      const next = !prev;
      instanceStorage.setItem("zone-sound-notify", String(next));
      // Play a test chime when enabling
      if (next) playNeedsInputChime();
      return next;
    });
  }, []);

  // ── Effects ───────────────────────────────────────────────────────────────

  // Request notification permission when desktop notifications are enabled
  useEffect(() => {
    if (desktopNotify && "Notification" in window && Notification.permission === "default") {
      Notification.requestPermission();
    }
  }, [desktopNotify]);

  // Persist autoApprovePatterns
  useEffect(() => {
    instanceStorage.setJSON("zone-auto-approve-patterns", autoApprovePatterns);
  }, [autoApprovePatterns]);

  // Un-dismiss batch bar when needs-input count increases
  const needsInputCount = Object.values(sessionStates).filter((s) => s === "needs-input").length;

  useEffect(() => {
    if (needsInputCount > prevNeedsInputCountRef.current) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- transition detection (count increased)
      setBatchBarDismissed(false);
    }
    prevNeedsInputCountRef.current = needsInputCount;
  }, [needsInputCount]);

  // ── Main state transition effect ──────────────────────────────────────────

  useEffect(() => {
    const prev = prevSessionStatesRef.current;
    const now = Date.now();
    const newFlashing: string[] = [];
    const newErrors: string[] = [];
    const newCompleted: string[] = [];

    for (const [tabId, state] of Object.entries(sessionStates)) {
      // Track entry time for any state change
      if (prev[tabId] !== state) {
        // Accumulate time in previous state
        const prevState = prev[tabId];
        if (prevState && stateEntryTimeRef.current[tabId]) {
          const elapsed = now - stateEntryTimeRef.current[tabId];
          stateTimeAccumRef.current[prevState] += elapsed;
        }
        stateEntryTimeRef.current[tabId] = now;

        // Log state transitions to history
        const tab = tabs.find((t) => t.id === tabId);
        const zone = Object.entries(assignments).find(([, id]) => id === tabId);
        const zoneNum = zone ? Number(zone[0]) : undefined;

        if (state === "needs-input") {
          addHistoryEvent("Needs input", tab?.title ?? tabId, zoneNum, "#e0af68");
        } else if (state === "error" && prev[tabId] !== "error") {
          addHistoryEvent("Error", tab?.title ?? tabId, zoneNum, "#f7768e");
        } else if (state === "completed" && prev[tabId] !== "completed") {
          addHistoryEvent("Completed", tab?.title ?? tabId, zoneNum, "#9ece6a");

          // Auto-restart: schedule restart for completed sessions (exit 0) after 2s
          if (autoRestart && zoneNum !== undefined) {
            const completedTab = tabs.find((t) => t.id === tabId);
            if (
              completedTab &&
              (completedTab.exitCode === 0 ||
                completedTab.exitCode === null ||
                completedTab.exitCode === undefined)
            ) {
              const capturedZoneNum = zoneNum;
              const capturedTitle = completedTab.title ?? tabId;
              const restartAt = Date.now() + 2000;
              // eslint-disable-next-line react-hooks/set-state-in-effect -- schedule pending restart countdown
              setPendingRestarts((prev) => ({ ...prev, [capturedZoneNum]: restartAt }));

              const timer = setTimeout(() => {
                handleRestartInZoneRef.current(capturedZoneNum);
                setAutoRestartCount((c) => c + 1);
                addHistoryEvent("Auto-restarted", capturedTitle, capturedZoneNum, "#7dcfff");
                setPendingRestarts((prev) => {
                  const next = { ...prev };
                  delete next[capturedZoneNum];
                  return next;
                });
                delete pendingRestartTimersRef.current[capturedZoneNum];
              }, 2000);

              pendingRestartTimersRef.current[capturedZoneNum] = timer;
            }
          }
        }
      }
      if (state === "needs-input" && prev[tabId] !== "needs-input") {
        newFlashing.push(tabId);
      }
      if (state === "error" && prev[tabId] !== "error") {
        newErrors.push(tabId);
      }
      if (state === "completed" && prev[tabId] !== "completed") {
        newCompleted.push(tabId);
      }
    }

    prevSessionStatesRef.current = sessionStates;

    // Track unseen needs-input
    if (newFlashing.length > 0) {
      setUnseenNeedsInput((old) => {
        const next = new Set(old);
        for (const id of newFlashing) next.add(id);
        return next;
      });
    }

    // Auto-approve: check last output lines against patterns
    if (newFlashing.length > 0 && autoApprovePatterns.length > 0) {
      for (const tabId of newFlashing) {
        const lines = lastOutputLines[tabId] ?? [];
        const lastFew = lines.slice(-5).join("\n");
        const matched = autoApprovePatterns.some((pattern) => {
          try {
            return new RegExp(pattern, "i").test(lastFew);
          } catch {
            return false;
          }
        });
        if (matched) {
          const ref = terminalRefs.get(tabId);
          ref?.current?.writeToTerminal("y\r");
          setAutoApproveCount((c) => c + 1);
          const tab = tabs.find((t) => t.id === tabId);
          addHistoryEvent("Auto-approved", tab?.title ?? tabId, undefined, "#9ece6a");
        }
      }
    }

    if (newFlashing.length > 0) {
      setFlashingTabs((old) => {
        const next = new Set(old);
        for (const id of newFlashing) next.add(id);
        return next;
      });

      // Auto-focus: jump to the first newly-needs-input zone
      if (autoFocusNeedsInput) {
        const firstFlashing = newFlashing[0];
        const zoneIdx = Object.entries(assignments).find(([, tabId]) => tabId === firstFlashing);
        if (zoneIdx) {
          setFocusedZone(Number(zoneIdx[0]));
        }
      }

      // Play notification sound
      if (soundEnabled) {
        playNeedsInputChime();
      }

      // Desktop notifications for needs-input transitions
      if (
        desktopNotify &&
        document.hidden &&
        "Notification" in window &&
        Notification.permission === "granted"
      ) {
        for (const tabId of newFlashing) {
          const tab = tabs.find((t) => t.id === tabId);
          const zoneNum = Object.entries(assignments).find(([, tid]) => tid === tabId)?.[0];
          new Notification("Session needs input", {
            body: `Zone ${zoneNum ? Number(zoneNum) + 1 : "?"}: ${tab?.title ?? tabId}`,
            tag: `zone-input-${tabId}`,
          });
        }
      }

      // Clear flash after animation duration (1s)
      const timer = setTimeout(() => {
        setFlashingTabs((old) => {
          const next = new Set(old);
          for (const id of newFlashing) next.delete(id);
          return next;
        });
      }, 1000);
      return () => clearTimeout(timer);
    }

    // Desktop notifications for error transitions
    if (
      newErrors.length > 0 &&
      desktopNotify &&
      document.hidden &&
      "Notification" in window &&
      Notification.permission === "granted"
    ) {
      for (const tabId of newErrors) {
        const tab = tabs.find((t) => t.id === tabId);
        const zoneNum = Object.entries(assignments).find(([, tid]) => tid === tabId)?.[0];
        new Notification("Session error", {
          body: `Zone ${zoneNum ? Number(zoneNum) + 1 : "?"}: ${tab?.title ?? tabId}`,
          tag: `zone-error-${tabId}`,
        });
      }
    }

    // Play completion chime for completed transitions
    if (soundEnabled && newCompleted.length > 0) {
      playCompletionChime();
    }

    // Play error alert for error transitions
    if (soundEnabled && newErrors.length > 0) {
      playErrorAlert();
    }
  }, [
    sessionStates,
    autoFocusNeedsInput,
    soundEnabled,
    desktopNotify,
    assignments,
    autoApprovePatterns,
    lastOutputLines,
    tabs,
    addHistoryEvent,
    autoRestart,
    terminalRefs,
    setFocusedZone,
    stateEntryTimeRef,
    stateTimeAccumRef,
    prevSessionStatesRef,
  ]);

  return {
    flashingTabs,
    unseenNeedsInput,
    setUnseenNeedsInput,
    autoApprovePatterns,
    setAutoApprovePatterns,
    autoApproveCount,
    autoFocusNeedsInput,
    toggleAutoFocus,
    soundEnabled,
    toggleSound,
    desktopNotify,
    setDesktopNotify,
    autoRestart,
    setAutoRestart,
    autoRestartCount,
    pendingRestarts,
    cancelPendingRestart,
    batchBarDismissed,
    setBatchBarDismissed,
  };
}
