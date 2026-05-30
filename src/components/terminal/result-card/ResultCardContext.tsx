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

export interface ResultCardSpec {
  title: string;
  subtitle?: string;
  sections?: ResultCardSection[];
  body?: React.ReactNode;
  footer?: { label: string; onClick: () => void | Promise<void> };
}

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
