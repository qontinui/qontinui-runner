/**
 * useRunnerTier
 *
 * Reads the current runner tier from the Rust backend (the `get_runner_tier`
 * Tauri command).
 *
 * Tier values:
 *   - "local"            — fully offline; no provider, no qontinui account.
 *   - "local_provider"   — local-only operation pointing at a self-hosted
 *                          AI provider (e.g. Ollama).
 *   - "qontinui_account" — full qontinui-web integration; this is the only
 *                          tier that requires a qontinui-account JWT.
 *
 * NO-DOWNGRADE (see the no-silent-downgrade audit): a failed read used to be
 * swallowed with `console.error`, leaving `tier = "local"` — the initial
 * placeholder — so a transient backend hiccup silently made every cloud
 * feature vanish for a Tier 2 user with no error anywhere. "We could not read
 * the tier" is UNKNOWN, not "local".
 *
 * Three changes encode that:
 *   1. `get_runner_tier` now returns `Err` (rather than the string "local")
 *      when settings.json is unreadable, so a failure is actually reported.
 *   2. A failed read is RETRIED with backoff while `loading` stays true, so
 *      consumers that gate on `loading` keep showing their loading state
 *      instead of falling through to the local-guest shell.
 *   3. After the retries are exhausted, `tierKnown` is false and `error`
 *      carries the reason. `tier` remains a PLACEHOLDER in that state — never
 *      treat it as the user's real tier without checking `tierKnown`.
 *
 * Re-fires when `setRunnerTier` is invoked elsewhere in the app via the
 * `runner-tier-changed` window event — SetupWizard / AccountSettings can
 * dispatch `new CustomEvent("runner-tier-changed")` after updating the
 * tier to make every consumer of this hook re-read.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type RunnerTier = "local" | "local_provider" | "qontinui_account";

/** Attempts (including the first) before we settle on "tier unknown". */
const MAX_ATTEMPTS = 5;
/** Backoff between attempts, ms. Total ≈ 7.5s before settling. */
const RETRY_DELAYS_MS = [250, 500, 1500, 3000, 3000];

export interface UseRunnerTierResult {
  /**
   * The runner tier. **Only meaningful when `tierKnown` is true** — otherwise
   * it is the "local" placeholder, not a verdict about the user.
   */
  tier: RunnerTier;
  /** True once a real tier has been read from the backend at least once. */
  tierKnown: boolean;
  /** True while the first read (including retries) is still in flight. */
  loading: boolean;
  /** Why the tier could not be read, after retries were exhausted. */
  error: string | null;
  refresh: () => Promise<void>;
}

export function useRunnerTier(): UseRunnerTierResult {
  const [tier, setTier] = useState<RunnerTier>("local");
  const [tierKnown, setTierKnown] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const cancelled = useRef(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    let lastErr: unknown = null;

    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
      if (cancelled.current) return;
      try {
        const t = await invoke<RunnerTier>("get_runner_tier");
        if (cancelled.current) return;
        setTier(t);
        setTierKnown(true);
        setError(null);
        setLoading(false);
        return;
      } catch (err) {
        lastErr = err;
        console.error(
          `[useRunnerTier] get_runner_tier failed (attempt ${attempt + 1}/${MAX_ATTEMPTS}):`,
          err,
        );
        if (attempt < MAX_ATTEMPTS - 1) {
          await new Promise((r) => setTimeout(r, RETRY_DELAYS_MS[attempt]));
        }
      }
    }

    if (cancelled.current) return;
    // Retries exhausted. Do NOT pretend the user is Tier 0 — report UNKNOWN
    // and let the caller decide how loudly to say so. Any previously-read
    // tier is deliberately left in place (a later failure must not demote a
    // tier we already established).
    setError(lastErr instanceof Error ? lastErr.message : String(lastErr ?? "unknown error"));
    setLoading(false);
  }, []);

  useEffect(() => {
    cancelled.current = false;
    // Defer the initial fetch off the effect body so the setState inside
    // `refresh()` doesn't fire synchronously during the same commit
    // (react-hooks/set-state-in-effect). The event-driven re-fetches
    // already run outside the effect body via the listener callback.
    queueMicrotask(() => void refresh());
    const onChange = () => void refresh();
    window.addEventListener("runner-tier-changed", onChange);
    return () => {
      cancelled.current = true;
      window.removeEventListener("runner-tier-changed", onChange);
    };
  }, [refresh]);

  return { tier, tierKnown, loading, error, refresh };
}
