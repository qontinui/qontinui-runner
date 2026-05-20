/**
 * useRunnerTier
 *
 * Reads the current runner tier from the Rust backend (the `get_runner_tier`
 * Tauri command). Defaults to "local" while loading so the UI never blocks
 * on this read.
 *
 * Tier values:
 *   - "local"            — fully offline; no provider, no qontinui account.
 *   - "local_provider"   — local-only operation pointing at a self-hosted
 *                          AI provider (e.g. Ollama).
 *   - "qontinui_account" — full qontinui-web integration; this is the only
 *                          tier that requires a qontinui-account JWT.
 *
 * Re-fires when `setRunnerTier` is invoked elsewhere in the app via the
 * `runner-tier-changed` window event — SetupWizard / AccountSettings can
 * dispatch `new CustomEvent("runner-tier-changed")` after updating the
 * tier to make every consumer of this hook re-read.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type RunnerTier = "local" | "local_provider" | "qontinui_account";

export function useRunnerTier(): {
  tier: RunnerTier;
  loading: boolean;
  refresh: () => Promise<void>;
} {
  const [tier, setTier] = useState<RunnerTier>("local");
  const [loading, setLoading] = useState(true);

  const refresh = async () => {
    try {
      const t = await invoke<RunnerTier>("get_runner_tier");
      setTier(t);
    } catch (err) {
      // Failing to read the tier leaves us at the default ("local"), which
      // is the safe choice — never block UI boot on this command.
      console.error("[useRunnerTier] get_runner_tier failed:", err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    // Defer the initial fetch off the effect body so the setState inside
    // `refresh()` doesn't fire synchronously during the same commit
    // (react-hooks/set-state-in-effect). The event-driven re-fetches
    // already run outside the effect body via the listener callback.
    queueMicrotask(() => void refresh());
    const onChange = () => void refresh();
    window.addEventListener("runner-tier-changed", onChange);
    return () => window.removeEventListener("runner-tier-changed", onChange);
  }, []);

  return { tier, loading, refresh };
}
