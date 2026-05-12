/**
 * Mid-session probe hook (Phase 3 of
 * `plans/pty-launched-ai-tabs-warning-plan.md`).
 *
 * Watches per-tab newline-terminated input from `TerminalInstance` and,
 * for active AI sessions (`tab.claudeSessionId` set), pings the runner's
 * `/file-registry/probe-conflicts` endpoint after the user pauses
 * typing. When the probe returns predicted collisions, the result is
 * stashed in a per-tab state map for {@link MidSessionToast} to render
 * an in-tab dismissible warning.
 *
 * Gating is intentionally conservative — see {@link shouldFireProbe} —
 * because the user explicitly types a fresh turn into Claude CLI and
 * we want to avoid both (a) firing on bare commands like `ls` and
 * (b) hammering the registry once a session is hot.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ConflictReport, PredictedCollision } from "./useSessionManager";
import { resolvePort } from "@/lib/runner-api";
import { buildProbeUrl } from "./LaunchMenu";

// ── Constants ───────────────────────────────────────────────────────────────

/** Debounce window before a fed line fires the probe (ms). */
export const MID_SESSION_DEBOUNCE_MS = 500;
/** Minimum gap between two probes for the same tab (ms). */
export const MID_SESSION_RATE_LIMIT_MS = 2000;
/** Minimum prompt length to fire a probe (chars). */
export const MID_SESSION_MIN_LINE_LEN = 10;
/** How long a toast stays before auto-dismissal (ms). */
export const MID_SESSION_TOAST_TTL_MS = 8000;
/** Interval at which the auto-prune sweep runs (ms). */
const PRUNE_INTERVAL_MS = 1000;

// ── Types ───────────────────────────────────────────────────────────────────

export interface MidSessionProbeState {
  /** Predicted-collision entries from the most recent probe response. */
  predicted_collisions: PredictedCollision[];
  /** Snapshot moment for auto-dismiss timing. */
  received_at: number;
}

export interface MidSessionProbeApi {
  feed(tabId: string, line: string): void;
  dismiss(tabId: string): void;
  states: Record<string, MidSessionProbeState>;
}

/**
 * Minimal tab shape consumed by the hook. Real callers pass
 * `TerminalTab` from `useTerminalManager`; tests pass plain literals.
 */
interface ProbeTab {
  id: string;
  claudeSessionId?: string;
  workingDir?: string;
}

// ── Pure helpers (exported for tests) ───────────────────────────────────────

/**
 * Gating decision for a candidate probe. Pure so the matrix of failure
 * modes (disabled, too-short, no-session, rate-limited) can be tested
 * without a React tree or fetch shim.
 */
export function shouldFireProbe(
  line: string,
  tab: { claudeSessionId?: string; lastProbeAt?: number },
  now: number,
  opts: { enabled: boolean },
): boolean {
  if (!opts.enabled) return false;
  if (line.length < MID_SESSION_MIN_LINE_LEN) return false;
  if (!tab.claudeSessionId) return false;
  if (tab.lastProbeAt !== undefined && now - tab.lastProbeAt < MID_SESSION_RATE_LIMIT_MS) {
    return false;
  }
  return true;
}

// ── Hook ────────────────────────────────────────────────────────────────────

export function useMidSessionProbe(
  tabs: ProbeTab[],
  opts: { enabled: boolean },
): MidSessionProbeApi {
  const [states, setStates] = useState<Record<string, MidSessionProbeState>>({});

  // Tabs change every render (new array identity). Mirror into a ref so the
  // stable `feed` callback can look up the latest tab metadata without
  // re-binding. Updated via `useEffect` to satisfy the
  // `react-hooks/refs` rule — see `useRegistryAwareness.ts` for the
  // same pattern.
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  // Debounce + rate-limit refs. These are deliberately mutable across
  // renders — the hook itself owns the per-tab clocks.
  const debounceTimersRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});
  const lastProbeAtRef = useRef<Record<string, number>>({});
  const optsRef = useRef(opts);
  useEffect(() => {
    optsRef.current = opts;
  }, [opts]);

  const dismiss = useCallback((tabId: string) => {
    setStates((prev) => {
      if (!(tabId in prev)) return prev;
      const next = { ...prev };
      delete next[tabId];
      return next;
    });
  }, []);

  const fireProbe = useCallback(async (tabId: string, line: string) => {
    const tab = tabsRef.current.find((t) => t.id === tabId);
    if (!tab) return;
    try {
      const port = resolvePort();
      const resp = await fetch(buildProbeUrl(port), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt: line, cwd: tab.workingDir ?? "" }),
      });
      if (!resp.ok) return;
      const json = (await resp.json()) as ConflictReport;
      // Empty predicted_collisions → leave existing state untouched.
      // A noisy keystroke should not erase a prior warning the user
      // hasn't acknowledged yet.
      if (!json.predicted_collisions || json.predicted_collisions.length === 0) return;
      setStates((prev) => ({
        ...prev,
        [tabId]: {
          predicted_collisions: json.predicted_collisions,
          received_at: Date.now(),
        },
      }));
    } catch {
      // Network / abort — silently ignore. The probe is best-effort.
    }
  }, []);

  const feed = useCallback(
    (tabId: string, line: string) => {
      // Reset any pending debounce for this tab — every fresh keystroke
      // restarts the clock.
      const existing = debounceTimersRef.current[tabId];
      if (existing) clearTimeout(existing);

      debounceTimersRef.current[tabId] = setTimeout(() => {
        delete debounceTimersRef.current[tabId];
        const tab = tabsRef.current.find((t) => t.id === tabId);
        if (!tab) return;
        const now = Date.now();
        const decision = shouldFireProbe(
          line,
          { claudeSessionId: tab.claudeSessionId, lastProbeAt: lastProbeAtRef.current[tabId] },
          now,
          optsRef.current,
        );
        if (!decision) return;
        lastProbeAtRef.current[tabId] = now;
        void fireProbe(tabId, line);
      }, MID_SESSION_DEBOUNCE_MS);
    },
    [fireProbe],
  );

  // Auto-prune sweep — one interval for the whole hook, not per-entry
  // timers. Removes entries whose `received_at + TTL` has passed.
  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      setStates((prev) => {
        let changed = false;
        const next: Record<string, MidSessionProbeState> = {};
        for (const [tabId, s] of Object.entries(prev)) {
          if (s.received_at + MID_SESSION_TOAST_TTL_MS < now) {
            changed = true;
            continue;
          }
          next[tabId] = s;
        }
        return changed ? next : prev;
      });
    }, PRUNE_INTERVAL_MS);
    return () => clearInterval(interval);
  }, []);

  // Cleanup pending debounce timers on unmount.
  useEffect(() => {
    const timers = debounceTimersRef.current;
    return () => {
      for (const t of Object.values(timers)) clearTimeout(t);
    };
  }, []);

  return useMemo<MidSessionProbeApi>(
    () => ({ feed, dismiss, states }),
    [feed, dismiss, states],
  );
}

// ── Setting gate ────────────────────────────────────────────────────────────

/**
 * Reactive read of the `enable_mid_session_path_prediction` localStorage
 * flag. Defaults to `false`. Listens to `storage` events so manual edits
 * (or a future settings UI — TODO: phase-3 follow-up) propagate without
 * reload.
 *
 * Lever: `localStorage.setItem("enable_mid_session_path_prediction", "true")`
 * — flip to enable mid-session probing for the current install.
 */
export function useMidSessionProbeEnabled(): boolean {
  const [enabled, setEnabled] = useState<boolean>(() => {
    if (typeof window === "undefined") return false;
    return window.localStorage.getItem("enable_mid_session_path_prediction") === "true";
  });

  useEffect(() => {
    if (typeof window === "undefined") return;
    const onStorage = (e: StorageEvent) => {
      if (e.key !== "enable_mid_session_path_prediction") return;
      setEnabled(e.newValue === "true");
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  return enabled;
}
