/**
 * React hook bridging the live page contexts to the pure
 * {@link reconcileChips} engine.
 *
 * Mounts at `TerminalPageInner` once; downstream consumers (the
 * per-zone `<SuggestionChip>` mount in `ZoneGrid`'s `ZoneCell`) read
 * from a tiny context the hook publishes. Centralising the engine
 * here means:
 *
 *  - One re-evaluation per tick / state change, not one per zone.
 *  - Suppression state lives in one place (`Set<string>` keyed
 *    `${zoneIdx}:${ruleId}`).
 *  - The 5s heartbeat that drives time-gated rules
 *    (`stuck-needs-input`) is shared, not per-rule.
 *
 * Suppression is in-memory only — a 5min auto-expiry is enforced via
 * `Map<key, expiryMs>`. Persisting across page-reload was deemed
 * unnecessary for v1 (the chip-storm scenarios a refresh might suppress
 * aren't long-lived) and would couple the engine to `instanceStorage`.
 */

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import { useTerminalSession } from "../contexts";
import { reconcileChips, type ChipCandidate } from "./rules";

const SUPPRESS_DURATION_MS = 5 * 60_000;
/** Re-evaluation cadence for time-gated rules. 5s means the
 *  `stuck-needs-input` rule fires within at most 5s of the 30s mark. */
const TICK_MS = 5_000;
/** Hard cap on simultaneous chips across the whole page. Matches
 *  plan §5(6) — max 3 on page, max 1 per zone (the latter enforced
 *  by the reconciler's per-zone dedupe). */
const MAX_ON_PAGE = 3;

interface SuggestionsContextValue {
  /** Per-zone chip — at most one entry per zoneIdx. */
  chipsByZone: ReadonlyMap<number, ChipCandidate>;
  /** Dismiss a chip; suppresses the `(zoneIdx, ruleId)` pair for 5min. */
  dismiss: (zoneIdx: number, ruleId: string) => void;
}

const SuggestionsContext = createContext<SuggestionsContextValue | null>(null);

/**
 * Mount once at TerminalPageInner; subscribes to session contexts,
 * evaluates rules on every state change + every 5s, and exposes the
 * resulting chip map via `useSuggestionForZone`.
 */
export function SuggestionsProvider({ children }: { children: React.ReactNode }) {
  const session = useTerminalSession();
  const { tabs, sessionStates, zoneLayout, stateEntryTimeRef } = session;
  const { layout, layoutId, focusedZone, assignments } = zoneLayout;

  // 5s heartbeat — bumps `tick` so the engine re-evaluates time-gated
  // rules even when no state has changed.
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), TICK_MS);
    return () => clearInterval(id);
  }, []);

  // Suppression state: Map<"zoneIdx:ruleId", expiryEpochMs>.
  const [suppression, setSuppression] = useState<Map<string, number>>(
    () => new Map(),
  );

  // Compute the active suppression key set, cheaply re-deriving when
  // expirations move out of date.
  const suppressed = useMemo(() => {
    const now = Date.now();
    const live = new Set<string>();
    for (const [key, expiresAt] of suppression) {
      if (expiresAt > now) live.add(key);
    }
    return live;
    // tick included so an expired suppression is dropped from the
    // active set within one heartbeat, even when no other deps change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [suppression, tick]);

  // The chip map. Depends on session state, the tick (time), and
  // suppression.
  const chipsByZone = useMemo(() => {
    return reconcileChips(
      {
        nowMs: Date.now(),
        tabsCount: tabs.length,
        zoneCount: layout.zones.length,
        currentLayoutId: layoutId,
        assignments,
        sessionStates,
        stateEntryMs: stateEntryTimeRef.current,
        focusedZone,
      },
      suppressed,
      MAX_ON_PAGE,
    );
    // stateEntryTimeRef is a mutable ref so its identity is stable;
    // the tick dep covers cadence-driven re-eval.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    tabs.length,
    sessionStates,
    layout.zones.length,
    layoutId,
    assignments,
    focusedZone,
    suppressed,
    tick,
  ]);

  const dismiss = useCallback((zoneIdx: number, ruleId: string) => {
    const key = `${zoneIdx}:${ruleId}`;
    const expiresAt = Date.now() + SUPPRESS_DURATION_MS;
    setSuppression((prev) => {
      const next = new Map(prev);
      next.set(key, expiresAt);
      return next;
    });
  }, []);

  const value = useMemo<SuggestionsContextValue>(
    () => ({ chipsByZone, dismiss }),
    [chipsByZone, dismiss],
  );

  return (
    <SuggestionsContext.Provider value={value}>
      {children}
    </SuggestionsContext.Provider>
  );
}

/**
 * Read the chip (if any) for a single zone. Returns `null` when nothing
 * is active for that zone — the chip component renders nothing in that
 * case, keeping zone-cell chrome zero when the rules are quiet.
 */
export function useSuggestionForZone(zoneIdx: number): {
  chip: ChipCandidate | null;
  dismiss: () => void;
} {
  const ctx = useContext(SuggestionsContext);
  // Outside a provider: render no chips, dismiss is a no-op. This
  // matches the React "context fallback" convention so the chip
  // component is safe to mount in zones that aren't wrapped (e.g.
  // a future per-zone profile preview).
  const chip = ctx?.chipsByZone.get(zoneIdx) ?? null;
  const dismiss = useCallback(() => {
    if (chip) ctx?.dismiss(zoneIdx, chip.ruleId);
  }, [ctx, zoneIdx, chip]);
  return { chip, dismiss };
}
