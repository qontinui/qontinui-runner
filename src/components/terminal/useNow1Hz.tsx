/**
 * Singleton 1Hz re-render primitive.
 *
 * The runner has at least three components that today each manage a
 * private `setInterval(() => setNowMs(Date.now()), 1000)` so their
 * "Ns since" labels keep ticking — {@link HoldingLockBanner},
 * {@link WaitingLockBanner}, and `FileActivityPanel`. Independent
 * intervals on the same cadence are wasteful (each schedules its own
 * `MessageChannel`/`Timer` fire) and drift apart visually as React
 * batches the resulting state updates differently per component.
 *
 * Phase 2 of the stuck-session heartbeat plan collapses those three
 * tickers into one shared interval via React context:
 *
 *   - {@link Now1HzProvider} runs ONE 1000ms interval and broadcasts
 *     `Date.now()` to every {@link useNow1Hz} consumer in its subtree.
 *   - When no provider is mounted (test harnesses that render a banner
 *     in isolation, headless preview, etc.), the hook falls back to a
 *     private per-component interval so consumers don't need to mock
 *     the provider. Behaviour is identical — just slightly less
 *     efficient.
 *
 * Mounted once at the runner-app root in `App.tsx` so all three
 * existing consumers (plus `TerminalTabBar`'s tooltip-tick subscriber)
 * share the same fire.
 *
 * Per `feedback_planning_priorities.md` (clean / scalable / robust /
 * powerful — programming effort isn't a trade-off): the fallback
 * branch costs a few extra lines but removes a mandatory wiring
 * dependency from every test that wants to render a consumer in
 * isolation.
 */

import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

const Now1HzContext = createContext<number | null>(null);

/**
 * Provider that runs a single 1000ms interval and broadcasts
 * `Date.now()` to every consumer of {@link useNow1Hz}. Mount once at
 * the runner-app root — `<Now1HzProvider>{children}</Now1HzProvider>`.
 *
 * Falls back to a per-component interval when no provider is mounted
 * (e.g. test harnesses that render a banner in isolation), so consumers
 * don't need to mock the provider.
 */
export function Now1HzProvider({ children }: { children: ReactNode }) {
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  return (
    <Now1HzContext.Provider value={nowMs}>{children}</Now1HzContext.Provider>
  );
}

/**
 * Returns `Date.now()` re-broadcast every ~1s. When a
 * {@link Now1HzProvider} is in the tree, all consumers share its
 * single interval. When no provider is mounted (test isolation,
 * headless preview), the hook falls back to a private per-component
 * interval — same observable behaviour, just slightly less efficient.
 */
export function useNow1Hz(): number {
  const ctx = useContext(Now1HzContext);
  const [fallback, setFallback] = useState(() => Date.now());
  useEffect(() => {
    if (ctx !== null) return; // provider in tree → no fallback needed
    const id = setInterval(() => setFallback(Date.now()), 1000);
    return () => clearInterval(id);
  }, [ctx]);
  return ctx ?? fallback;
}
