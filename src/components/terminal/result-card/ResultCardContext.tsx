/**
 * ResultCardContext
 *
 * A result-card surface independent of the CommandBar: a context that owns a
 * single, optional `ResultCardSpec`. A producer calls `showCard(spec)` to
 * present a card; `dismissCard()` clears it. The card itself is rendered by
 * `ResultCardMount` (a separate component) so producers and the mount can both
 * read the surface.
 *
 * State is a pure, EXPORTED reducer (`resultCardReducer`) consumed by the
 * provider via `useReducer`. The reducer is exported so the runner's
 * node-environment vitest (no jsdom, no @testing-library/react — see
 * vitest.config.ts + HoldingLockBanner.test.tsx) can exercise the state
 * machine without rendering React. Mirrors the provider/createContext +
 * null-context-guard style of `contexts/ZoneMetadataContext.tsx`.
 */

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useReducer,
  type ReactNode,
} from "react";

export interface ResultCardSection {
  heading?: string;
  rows: { label: string; value: string; valueColor?: string; labelColor?: string }[];
}

interface ResultCardBase {
  title: string;
  subtitle?: string;
  footer?: { label: string; onClick: () => void | Promise<void> };
  /**
   * How many things this card SHOWS — the number `/metrics` and `/history`
   * report as their effect.
   *
   * Optional for a `sections` card, where it can be derived by summing the
   * rows; REQUIRED for a `body` card, where it cannot. That asymmetry is the
   * whole point of the union below. `useTerminalCommands.ts::countCardRows`
   * summed `sections` unconditionally, so `/history` — whose spec is
   * `title`/`subtitle`/`body`, the events being a React node — reported zero
   * events over a card headed "EVENT HISTORY (47)", every single time. It was
   * indistinguishable from a genuinely empty history.
   *
   * Making it a required field of the body arm means the next builder that
   * renders its content as a node cannot repeat that: it does not compile
   * until it says how many things it drew.
   */
  itemCount?: number;
}

/**
 * A card carries its content EITHER as structured `sections` or as a React
 * `body` node — and a body-only card must declare {@link
 * ResultCardBase.itemCount}, because nothing else can count a node.
 */
export type ResultCardSpec = ResultCardBase &
  (
    | { sections: ResultCardSection[]; body?: React.ReactNode }
    | { sections?: undefined; body: React.ReactNode; itemCount: number }
  );

export interface ResultCardContextValue {
  showCard: (spec: ResultCardSpec) => void;
  dismissCard: () => void;
  card: ResultCardSpec | null;
}

export type ResultCardAction = { type: "show"; spec: ResultCardSpec } | { type: "dismiss" };

/**
 * Pure reducer for the result-card surface.
 *   - "show"    → action.spec (replaces any current card)
 *   - "dismiss" → null
 */
export function resultCardReducer(
  state: ResultCardSpec | null,
  action: ResultCardAction,
): ResultCardSpec | null {
  switch (action.type) {
    case "show":
      return action.spec;
    case "dismiss":
      return null;
    default:
      return state;
  }
}

export const ResultCardContext = createContext<ResultCardContextValue | null>(null);

interface ResultCardProviderProps {
  children: ReactNode;
}

export function ResultCardProvider({ children }: ResultCardProviderProps) {
  const [card, dispatch] = useReducer(resultCardReducer, null);

  const showCard = useCallback((spec: ResultCardSpec) => {
    dispatch({ type: "show", spec });
  }, []);

  const dismissCard = useCallback(() => {
    dispatch({ type: "dismiss" });
  }, []);

  const value = useMemo<ResultCardContextValue>(
    () => ({ showCard, dismissCard, card }),
    [showCard, dismissCard, card],
  );

  return <ResultCardContext.Provider value={value}>{children}</ResultCardContext.Provider>;
}

/**
 * Read the result-card surface. Throws if used outside `ResultCardProvider`
 * (mirrors the null-context guard used across the terminal contexts).
 */
export function useResultCard(): ResultCardContextValue {
  const ctx = useContext(ResultCardContext);
  if (ctx === null) {
    throw new Error("useResultCard must be used within a ResultCardProvider");
  }
  return ctx;
}
