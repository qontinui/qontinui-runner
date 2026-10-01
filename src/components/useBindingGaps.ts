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
  CANCEL_PAIR_ALL_TENANTS_CMD,
  normalizePairAllProgress,
  PAIR_ALL_PROGRESS_EVENT,
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
 *
 * The flow is process-wide (the runner allows one at a time), so its state is
 * taken from {@link PAIR_ALL_PROGRESS_EVENT}, which every mounted hook hears:
 * the banner and the Settings card show the same "Waiting for browser…", the
 * same connect link when the browser could not be launched, and the same
 * results, whichever of them started it.
 */
export function usePairAllTenants(refresh: () => Promise<unknown>): {
  phase: PairAllPhase;
  results: TenantPairResult[] | null;
  error: string | null;
  /** Set when the runner reported the URL to open; `launched` false → show it. */
  connectLink: { url: string; launched: boolean } | null;
  connect: (tenantIds: string[]) => Promise<void>;
  cancel: () => Promise<void>;
  reset: () => void;
} {
  const [phase, setPhase] = useState<PairAllPhase>("idle");
  const [results, setResults] = useState<TenantPairResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connectLink, setConnectLink] = useState<{ url: string; launched: boolean } | null>(null);

  useEffect(() => {
    const unlisten = listen<unknown>(PAIR_ALL_PROGRESS_EVENT, (ev) => {
      const p = normalizePairAllProgress(ev.payload);
      if (p === null) return;
      switch (p.phase) {
        case "waiting":
          setPhase("waiting");
          setResults(null);
          setError(null);
          setConnectLink(null);
          break;
        case "browser":
          setConnectLink({ url: p.connectUrl, launched: p.launched });
          break;
        case "done":
          setResults(p.results);
          setPhase("done");
          setConnectLink(null);
          void refresh();
          break;
        case "error":
          setError(p.error);
          setPhase("error");
          setConnectLink(null);
          break;
        case "cancelled":
          setPhase("idle");
          setConnectLink(null);
          break;
      }
    });
    return () => {
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, [refresh]);

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
        const msg = String(e);
        if (msg === "cancelled") {
          setPhase("idle");
        } else {
          setError(msg);
          setPhase("error");
        }
      } finally {
        setConnectLink(null);
        await refresh();
      }
    },
    [refresh],
  );

  const cancel = useCallback(async () => {
    try {
      await invoke<boolean>(CANCEL_PAIR_ALL_TENANTS_CMD);
    } catch {
      /* an older runner without the command: the flow times out on its own */
    }
  }, []);

  const reset = useCallback(() => {
    setPhase("idle");
    setResults(null);
    setError(null);
    setConnectLink(null);
  }, []);

  return { phase, results, error, connectLink, connect, cancel, reset };
}
