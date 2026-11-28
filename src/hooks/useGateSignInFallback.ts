/**
 * The first-run ("gate") sign-in screens' answer to an UNBOUNDED sign-in
 * decision (`@/lib/savedBackendUrl`): hold the pending sign-in instead of
 * starting it, then run it only on the operator's explicit choice. Shared by
 * LoginScreen and the setup wizard's GitHub step so the rule has one copy;
 * `SignInFallbackNotice` renders it.
 *
 *   - `withhold(decision, action)` — record the unbounded decision and the
 *     sign-in it held back. The caller starts no sign-in.
 *   - `signInToProduction()` — run the held action against
 *     `PRODUCTION_BACKEND_URL` itself (not a re-run of the resolver), so the
 *     click does exactly what its label says.
 *   - `retry()` — ONE status read (`readSignInDecision`). Bounded: run the held
 *     action against that target. Still unbounded: keep the notice, with the
 *     new decision's reason.
 */

import { useCallback, useRef, useState } from "react";

import {
  PRODUCTION_BACKEND_URL,
  readSignInDecision,
  type SignInDecision,
} from "@/lib/savedBackendUrl";

export interface GateSignInFallback<A> {
  decision: SignInDecision;
  action: A;
}

export interface GateSignInFallbackState<A> {
  /** The withheld sign-in, or `null` when nothing is held. */
  fallback: GateSignInFallback<A> | null;
  /** True while `retry()`'s status read is in flight. */
  reading: boolean;
  withhold: (decision: SignInDecision, action: A) => void;
  clear: () => void;
  signInToProduction: () => Promise<void>;
  retry: () => Promise<void>;
}

export function useGateSignInFallback<A>(
  run: (action: A, backendUrl: string) => Promise<void>,
): GateSignInFallbackState<A> {
  const [fallback, setFallback] = useState<GateSignInFallback<A> | null>(null);
  const [reading, setReading] = useState(false);
  // Bumped by every withhold/clear. A retry whose read straddles a bump (the
  // operator edited the held credentials, or a new sign-in replaced the held
  // one) drops its result instead of running a stale action.
  const generation = useRef(0);

  const withhold = useCallback((decision: SignInDecision, action: A) => {
    generation.current += 1;
    setFallback({ decision, action });
  }, []);

  const clear = useCallback(() => {
    generation.current += 1;
    setFallback(null);
  }, []);

  const signInToProduction = useCallback(async () => {
    if (fallback === null || reading) return;
    setFallback(null);
    await run(fallback.action, PRODUCTION_BACKEND_URL);
  }, [fallback, reading, run]);

  const retry = useCallback(async () => {
    if (fallback === null || reading) return;
    const { action } = fallback;
    const issuedAt = generation.current;
    setReading(true);
    let decision: SignInDecision;
    try {
      decision = await readSignInDecision("gate");
    } finally {
      setReading(false);
    }
    if (generation.current !== issuedAt) return;
    if (!decision.bounded) {
      setFallback({ decision, action });
      return;
    }
    setFallback(null);
    await run(action, decision.target);
  }, [fallback, reading, run]);

  return { fallback, reading, withhold, clear, signInToProduction, retry };
}
