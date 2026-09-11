import { useState, useEffect, useCallback, useRef } from "react";
import { deliverApprovals } from "./approveAll";
import { evaluateTransitions } from "./transitionAutomation";
import { instanceStorage } from "@/lib/instance-storage";
import type { SessionState } from "./useZoneLayout";
import { playNeedsInputChime, playCompletionChime, playErrorAlert } from "./notificationSound";

export interface UseStateTransitionEffectsParams {
  sessionStates: Record<string, SessionState>;
  prevSessionStatesRef: React.MutableRefObject<Record<string, SessionState>>;
  tabs: Array<{ id: string; title: string; exitCode?: number | null }>;
  assignments: Record<number, string>;
  /**
   * Lazy reader for a tab's last rendered lines. A stable function (backed by
   * the page's terminal hot store) rather than the whole map: the auto-approve
   * branch only reads lines at the instant a tab transitions to
   * `needs-input`, so taking the map as a dependency re-ran this entire effect
   * on every output frame for nothing (plan
   * `2026-07-28-runner-many-sessions-performance` §0 A1).
   */
  getLastOutputLines: (tabId: string) => string[];
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
}

export function useStateTransitionEffects(
  params: UseStateTransitionEffectsParams,
): UseStateTransitionEffectsReturn {
  const {
    sessionStates,
    prevSessionStatesRef,
    tabs,
    assignments,
    getLastOutputLines,
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

  // ── Main state transition effect ──────────────────────────────────────────
  //
  // The DECISION half lives in `transitionAutomation.ts` — a pure, tested
  // module (`evaluateTransitions`). What stays here is the EFFECT half: dwell
  // timing, history logging, the restart countdown, delivery, and the
  // notification surfaces. The split is what makes the decision testable at
  // all, since `vitest.config.ts` is `environment: "node"` and a hook cannot be
  // rendered.
  //
  // `evaluateTransitions` deliberately does NOT advance `prevSessionStatesRef`
  // — this effect does, on the line marked below, and it must remain the only
  // place that does. See the module header for why a second advancer silently
  // kills flashing, auto-focus and the window-title counts.

  useEffect(() => {
    const now = Date.now();
    const outcome = evaluateTransitions({
      prev: prevSessionStatesRef.current,
      next: sessionStates,
      tabs,
      assignments,
      autoApprovePatterns,
      autoRestart,
      getLastOutputLines,
    });

    const {
      newNeedsInput: newFlashing,
      newErrors,
      newCompleted,
      approvals,
      restarts,
      stateChanges,
    } = outcome;

    // Dwell-time accounting + history, in observation order.
    for (const change of stateChanges) {
      if (change.from && stateEntryTimeRef.current[change.tabId]) {
        stateTimeAccumRef.current[change.from] +=
          now - stateEntryTimeRef.current[change.tabId];
      }
      stateEntryTimeRef.current[change.tabId] = now;

      if (change.to === "needs-input") {
        addHistoryEvent("Needs input", change.title, change.zoneIdx, "#e0af68");
      } else if (change.to === "error") {
        addHistoryEvent("Error", change.title, change.zoneIdx, "#f7768e");
      } else if (change.to === "completed") {
        addHistoryEvent("Completed", change.title, change.zoneIdx, "#9ece6a");
      }
    }

    // Auto-restart: schedule the 2s countdown for each eligible completed zone.
    for (const restart of restarts) {
      const { zoneIdx, title } = restart;
      const restartAt = Date.now() + 2000;
      setPendingRestarts((prev) => ({ ...prev, [zoneIdx]: restartAt }));

      const timer = setTimeout(() => {
        handleRestartInZoneRef.current(zoneIdx);
        setAutoRestartCount((c) => c + 1);
        addHistoryEvent("Auto-restarted", title, zoneIdx, "#7dcfff");
        setPendingRestarts((prev) => {
          const next = { ...prev };
          delete next[zoneIdx];
          return next;
        });
        delete pendingRestartTimersRef.current[zoneIdx];
      }, 2000);

      pendingRestartTimersRef.current[zoneIdx] = timer;
    }

    // THE SINGLE ADVANCE. Nothing else may write this ref.
    prevSessionStatesRef.current = sessionStates;

    // Track unseen needs-input
    if (newFlashing.length > 0) {
      setUnseenNeedsInput((old) => {
        const next = new Set(old);
        for (const id of newFlashing) next.add(id);
        return next;
      });
    }

    // Auto-approve delivery. `approvals` is an INTENT; only the envelope that
    // comes back may be counted.
    //
    // The same delivery path `/approve-all`, Ctrl+Shift+Enter and the overlay
    // buttons use, and the reason this one matters most: an auto-approve rule
    // answers `y` on the operator's behalf with no one watching.
    // `ref?.current?.writeToTerminal("y\r")` is a silent no-op for any pane
    // without a mounted `TerminalInstance` — which for a headless polling
    // workflow, the exact case this feature exists for, is the COMMON state —
    // and the counter and history event fired regardless. So the page recorded
    // auto-approvals that reached no process, and `/metrics` renders that
    // counter.
    //
    // Fire-and-forget by construction (this is a state-transition effect, not a
    // command), but the count and the log entry wait on the envelope. A write
    // that did not land leaves no trace, which is the honest record: the prompt
    // is still waiting.
    for (const tabId of approvals) {
      const tab = tabs.find((t) => t.id === tabId);
      void deliverApprovals([tabId], terminalRefs, "y\r").then((report) => {
        if (report.delivered === 0) return;
        setAutoApproveCount((c) => c + report.delivered);
        addHistoryEvent("Auto-approved", tab?.title ?? tabId, undefined, "#9ece6a");
      });
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
    getLastOutputLines,
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
  };
}
