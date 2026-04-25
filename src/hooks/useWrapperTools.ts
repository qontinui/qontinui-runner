/**
 * useWrapperTools
 *
 * Phase 4 (wrapper-runner integration): exposes installed wrapper actions as
 * agent tools, alongside an HTTP-backed dispatch helper. The hook is the
 * single integration point for any in-app TS agent loop and for the
 * Terminal session's "N wrapper tools available" header badge.
 *
 * Responsibilities:
 *  - Fetch `/wrappers` once on mount (and on demand via `refresh`).
 *  - Filter to wrappers eligible for tool injection (running / idle / unknown).
 *  - Build the tool list + reverse-map from `tool name -> (wrapperId, actionId)`
 *    via `buildWrapperTools`.
 *  - Expose `dispatch(toolName, params)` that resolves the route table and
 *    POSTs to `/wrappers/:id/dispatch`.
 *
 * Design notes:
 *  - The route table is the source of truth for tool->action resolution. We
 *    never parse the tool name back into an action id at dispatch time
 *    because the dash->underscore rewrite isn't unambiguously reversible
 *    (see `tools.ts`).
 *  - The hook is tolerant of backend errors: if `/wrappers` is unreachable
 *    we surface an error and an empty tool list instead of throwing.
 *  - Snapshots are stable per session: we don't auto-refresh on install
 *    events. The caller can wire a refresh button (the badge popover does).
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { dispatchAction, listWrappers } from "@/lib/wrappers/api";
import {
  buildWrapperTools,
  isWrapperEligibleForTools,
  type WrapperToolDefinition,
  type WrapperToolRouteTable,
} from "@/lib/wrappers/tools";
import type { DispatchResult, InstalledWrapper } from "@/lib/wrappers/types";

export interface UseWrapperToolsResult {
  /** Tool definitions ready to merge into an agent's tool list. */
  tools: WrapperToolDefinition[];
  /** Reverse-mapping from tool name to wrapper+action descriptor. */
  routes: WrapperToolRouteTable;
  /** Wrappers seen by the most recent fetch. */
  wrappers: InstalledWrapper[];
  /** True while a fetch is in flight. */
  loading: boolean;
  /** Last error, if any. Cleared on the next successful fetch. */
  error: string | null;
  /** Re-fetch from `/wrappers`; useful after installing a new wrapper. */
  refresh: () => Promise<void>;
  /**
   * Dispatch a wrapper-tool invocation by tool name. Returns whatever the
   * runner returns (`{ result }` or `{ error }`). Throws only if the tool
   * name is unknown — callers should treat this as a programming error
   * (the agent shouldn't be able to invoke a tool we didn't expose).
   */
  dispatch: <T = unknown>(
    toolName: string,
    params: Record<string, unknown>,
  ) => Promise<DispatchResult<T>>;
}

/**
 * Hook that returns the wrapper-action tool list + dispatcher.
 *
 * @param enabled gate the fetch; defaults to true. Pass `false` to defer
 *   loading until a parent component is ready (e.g., wait for auth).
 */
export function useWrapperTools(enabled: boolean = true): UseWrapperToolsResult {
  const [wrappers, setWrappers] = useState<InstalledWrapper[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await listWrappers();
      const eligible = list.filter(isWrapperEligibleForTools);
      setWrappers(eligible);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setWrappers([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    void refresh();
  }, [enabled, refresh]);

  const { tools, routes } = useMemo(() => buildWrapperTools(wrappers), [wrappers]);

  const dispatch = useCallback(
    async <T = unknown>(
      toolName: string,
      params: Record<string, unknown>,
    ): Promise<DispatchResult<T>> => {
      const route = routes.get(toolName);
      if (!route) {
        // Unknown tool — return as an error envelope. We don't throw because
        // this often runs inside an agent loop where exceptions become
        // ungraceful failure modes.
        return {
          error: `Unknown wrapper tool: ${toolName}. Did the wrapper get uninstalled mid-session?`,
        };
      }
      return dispatchAction<T>(route.wrapperId, route.actionId, params);
    },
    [routes],
  );

  return { tools, routes, wrappers, loading, error, refresh, dispatch };
}
