/**
 * Hooks shared by {@link BindingGapAskBanner} and the Settings card's
 * "Workspaces" section — plan
 * `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`,
 * D2/D3. Both surfaces read ONE source, `get_binding_gaps`, so they cannot
 * disagree about which workspaces lack a credential.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  BINDING_GAP_NUDGE_EVENT,
  GET_BINDING_GAPS_CMD,
  normalizeBindingGapView,
  normalizePairAllResults,
  PAIR_ALL_TENANTS_CMD,
  type BindingGapView,
  type TenantPairResult,
} from "./binding-gap-ask-logic";

/**
 * The per-tenant view, read on mount and on every binding-gap nudge. `null`
 * is UNKNOWN (an older runner without the command, or a failed read).
 */
export function useBindingGapView(): {
  view: BindingGapView | null;
  refresh: () => Promise<BindingGapView | null>;
} {
  const [view, setView] = useState<BindingGapView | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = normalizeBindingGapView(await invoke<unknown>(GET_BINDING_GAPS_CMD));
      setView(next);
      return next;
    } catch {
      setView(null);
      return null;
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const read = () => {
      invoke<unknown>(GET_BINDING_GAPS_CMD)
        .then((raw) => {
          if (!cancelled) setView(normalizeBindingGapView(raw));
        })
        .catch(() => {
          if (!cancelled) setView(null);
        });
    };
    read();
    const unlisten = listen(BINDING_GAP_NUDGE_EVENT, read);
    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  return { view, refresh };
}

export type PairAllPhase = "idle" | "waiting" | "done" | "error";

/**
 * Runs `pair_all_tenants` — ONE browser sign-in for the given tenants — and
 * re-reads the view afterwards (the pull path; the nudge event is only a
 * hint).
 */
export function usePairAllTenants(refresh: () => Promise<unknown>): {
  phase: PairAllPhase;
  results: TenantPairResult[] | null;
  error: string | null;
  connect: (tenantIds: string[]) => Promise<void>;
  reset: () => void;
} {
  const [phase, setPhase] = useState<PairAllPhase>("idle");
  const [results, setResults] = useState<TenantPairResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const connect = useCallback(
    async (tenantIds: string[]) => {
      setPhase("waiting");
      setResults(null);
      setError(null);
      try {
        const raw = await invoke<unknown>(PAIR_ALL_TENANTS_CMD, {
          tenantIds: tenantIds.length > 0 ? tenantIds : null,
        });
        setResults(normalizePairAllResults(raw));
        setPhase("done");
      } catch (e) {
        setError(String(e));
        setPhase("error");
      } finally {
        await refresh();
      }
    },
    [refresh],
  );

  const reset = useCallback(() => {
    setPhase("idle");
    setResults(null);
    setError(null);
  }, []);

  return { phase, results, error, connect, reset };
}
