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
 *
 * FIRST LOAD vs. RE-READ: `loading` covers only the first read, `refreshing`
 * covers the event-driven re-reads. They were one flag, and that is what made
 * the Setup Wizard's Tier step appear inert: choosing a tier dispatches
 * `runner-tier-changed`, the re-read raised `loading`, `AuthProvider` folds
 * `loading` into `auth.loading`, `resolveAuthGate` returns `"loading"`, and the
 * wizard — an early return at the time — was replaced and remounted at step 0
 * within ~17ms, so the click looked like it did nothing. `refreshing` must
 * never be fed into an auth-gate input.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type RunnerTier = "local" | "local_provider" | "qontinui_account";

/**
 * Wire result of the `set_runner_tier` Tauri command
 * (`src-tauri/src/commands/auth.rs`).
 *
 * The command used to return nothing, so "saved" and "silently discarded" were
 * indistinguishable — on a secondary (supervisor-launched) runner it was a
 * complete no-op that still logged and returned success. `applied` (in effect
 * for this process) and `persisted` (survives a restart) are now reported
 * separately; callers MUST NOT treat `persisted === false` as a plain success.
 */
export interface SetRunnerTierResult {
  /** The tier is in effect for the rest of this runner process's lifetime. */
  applied: boolean;
  /** The tier was written to settings.json and survives a restart. */
  persisted: boolean;
  /** Why `persisted` is false — e.g. `"secondary_runner"`. Absent when true. */
  reason?: string;
}

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
  /**
   * True while the FIRST read (including retries) is still in flight — i.e.
   * while the tier is genuinely not known yet.
   *
   * Deliberately NOT true for a re-read of an already-known tier: this flag is
   * folded into `AuthProvider`'s `loading`, which the App's auth gate turns
   * into the `"loading"` surface. Setting it on every re-read made the
   * `runner-tier-changed` event (dispatched by the wizard's own TierStep right
   * after it writes the tier) flip the gate off `"wizard"` and unmount
   * `<SetupWizard>` mid-flight, destroying `currentStep` — so the wizard
   * silently reset to step 0 on every tier choice. See `refreshing`.
   */
  loading: boolean;
  /**
   * True while a re-read of an ALREADY-KNOWN tier is in flight. Purely
   * informational — it must never be folded into any auth-gate input, or it
   * re-creates the unmount above.
   */
  refreshing: boolean;
  /** Why the tier could not be read, after retries were exhausted. */
  error: string | null;
  refresh: () => Promise<void>;
}

export function useRunnerTier(): UseRunnerTierResult {
  const [tier, setTier] = useState<RunnerTier>("local");
  const [tierKnown, setTierKnown] = useState(false);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cancelled = useRef(false);
  // Mirror of `tierKnown` for `refresh`, which is `useCallback(..., [])` and
  // would otherwise close over the initial `false` forever — every re-read
  // would look like a first load and re-enter the global loading gate.
  const tierKnownRef = useRef(false);

  const refresh = useCallback(async () => {
    // First load vs. re-read. Only the first load may raise `loading`, which
    // AuthProvider folds into `auth.loading` and the App gate turns into the
    // "loading" surface — a re-read that did so unmounted the SetupWizard.
    const firstLoad = !tierKnownRef.current;
    if (firstLoad) setLoading(true);
    else setRefreshing(true);
    setError(null);
    let lastErr: unknown = null;

    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
      if (cancelled.current) return;
      try {
        const t = await invoke<RunnerTier>("get_runner_tier");
        if (cancelled.current) return;
        setTier(t);
        tierKnownRef.current = true;
        setTierKnown(true);
        setError(null);
        setLoading(false);
        setRefreshing(false);
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
    // Clear BOTH flags on every exit path — whichever one this call raised.
    // A failed re-read must not demote an already-known tier (`tierKnownRef`
    // and `tierKnown` are deliberately left alone here).
    setLoading(false);
    setRefreshing(false);
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

  return { tier, tierKnown, loading, refreshing, error, refresh };
}
