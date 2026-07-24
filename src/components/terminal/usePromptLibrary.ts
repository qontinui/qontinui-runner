/**
 * Prompt-library fetch/cache/refresh hook.
 *
 * Fetches once on mount (the Rust side already holds a 45s-TTL + ETag
 * cache, so re-mounts are cheap) and exposes `refresh` for the modal's
 * manual reload. Errors from the `invoke` itself (runner-side setup
 * failures) surface as a degraded state with a reason — the same
 * "never a silent empty list" posture the Rust command keeps for
 * auth/network failures.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import type { PromptLibraryAuth, PromptTemplate } from "./promptLibraryApi";
import { listPromptTemplates } from "./promptLibraryApi";

export interface PromptLibraryState {
  prompts: PromptTemplate[];
  auth: PromptLibraryAuth;
  reason?: string;
  loading: boolean;
  /** Re-fetch (Rust serves its cache within the TTL; a 304 is cheap). */
  refresh: () => void;
}

export function usePromptLibrary(): PromptLibraryState {
  const [prompts, setPrompts] = useState<PromptTemplate[]>([]);
  const [auth, setAuth] = useState<PromptLibraryAuth>({ state: "ok" });
  const [reason, setReason] = useState<string | undefined>(undefined);
  const [loading, setLoading] = useState(true);
  // Monotonic token so a stale in-flight fetch can never clobber a newer one.
  const fetchSeq = useRef(0);

  const refresh = useCallback(() => {
    const seq = ++fetchSeq.current;
    setLoading(true);
    void (async () => {
      try {
        const res = await listPromptTemplates();
        if (seq !== fetchSeq.current) return;
        setPrompts(res.prompts ?? []);
        setAuth(res.auth ?? { state: "ok" });
        setReason(res.reason);
      } catch (err) {
        if (seq !== fetchSeq.current) return;
        setPrompts([]);
        setAuth({ state: "ok" });
        setReason(
          `prompt library unavailable: ${err instanceof Error ? err.message : String(err)}`,
        );
      } finally {
        if (seq === fetchSeq.current) setLoading(false);
      }
    })();
  }, []);

  // Fetch on mount. Async IIFE so the setState-bearing callback is not
  // invoked synchronously in the effect body (set-state-in-effect) — the
  // DocFinderModal scan-on-mount precedent.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      refresh();
    })();
    return () => {
      cancelled = true;
      // Invalidate any in-flight fetch on unmount.
      fetchSeq.current += 1;
    };
  }, [refresh]);

  return { prompts, auth, reason, loading, refresh };
}
