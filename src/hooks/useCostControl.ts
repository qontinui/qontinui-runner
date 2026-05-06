/**
 * useCostControl.ts
 *
 * Data hook for the Cost Control Panel.
 * Listens to Tauri events for real-time cost data, fetches historical
 * dashboard data and active budget status via TanStack Query.
 */

import { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { AppEvent } from "@qontinui/shared-types/tauri-events";
import type {
  CostUpdateEvent,
  BudgetWarningEvent,
  CostAnomalyEvent,
  CostDashboard,
  ActiveBudgetStatus,
} from "../components/cost-control/types";

/**
 * Narrow `AppEvent` to a single variant by `event_type` discriminator.
 * See `useTriggers.ts` for the full rationale.
 */
type AppEventOf<T extends AppEvent["event_type"]> = Extract<
  AppEvent,
  { event_type: T }
>;

const MAX_EVENTS = 200;

/** A single fetch error surfaced to the UI. */
export interface CostControlSourceError {
  /** Stable identifier of the failing data source. */
  source: "cost-dashboard" | "budget";
  /** Human-readable message; empty queries swallow their errors and never appear here. */
  message: string;
}

/** Aggregated query status for the Cost Control panel. */
export interface CostControlStatus {
  isLoading: boolean;
  isError: boolean;
  errors: CostControlSourceError[];
}

function errorMessage(err: unknown): string {
  if (err == null) return "Unknown error";
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

export function useCostControl() {
  const queryClient = useQueryClient();
  const [costEvents, setCostEvents] = useState<CostUpdateEvent[]>([]);
  const [budgetWarnings, setBudgetWarnings] = useState<BudgetWarningEvent[]>([]);
  const [anomalies, setAnomalies] = useState<CostAnomalyEvent[]>([]);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Historical dashboard data
  const {
    data: dashboard,
    isLoading: dashboardLoading,
    isError: dashboardIsError,
    error: dashboardError,
    refetch: refetchDashboard,
  } = useQuery<CostDashboard>({
    queryKey: ["cost-dashboard"],
    queryFn: () => invoke<CostDashboard>("get_cost_dashboard", { days: 30 }),
    staleTime: 30_000,
  });

  // Active budget status (may not exist yet - degrade gracefully).
  // The queryFn swallows backend errors and returns null so the panel can
  // render a "no active budget" empty state. isError is therefore reserved
  // for unexpected query-layer failures (e.g. invalidation/serialization).
  const {
    data: activeBudget,
    isLoading: budgetLoading,
    isError: budgetIsError,
    error: budgetError,
    refetch: refetchBudget,
  } = useQuery<ActiveBudgetStatus | null>({
    queryKey: ["active-budget-status"],
    queryFn: async () => {
      try {
        return await invoke<ActiveBudgetStatus>("get_active_budget_status");
      } catch {
        return null;
      }
    },
    refetchInterval: 5_000,
  });

  // Debounced query invalidation on cost-update events
  const scheduleInvalidation = useCallback(() => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => {
      queryClient.invalidateQueries({ queryKey: ["cost-dashboard"] });
      queryClient.invalidateQueries({ queryKey: ["active-budget-status"] });
    }, 300);
  }, [queryClient]);

  // Listen for Tauri events.
  //
  // Wire shape: cost-update / budget-warning / cost-anomaly are emitted via
  // `EventBroadcaster::cost_update()` etc., which serialize an `AppEvent`
  // tagged with `#[serde(tag = "event_type", content = "data")]`. The wire
  // payload therefore looks like `{ event_type: "CostUpdate", data: {…} }`.
  // The downstream `CostUpdateEvent` / `BudgetWarningEvent` / `CostAnomalyEvent`
  // types are flat snake_case (matching the inner `data` object), so we unwrap
  // the envelope here before storing.
  useEffect(() => {
    const unlisteners: (() => void)[] = [];

    // The generated AppEvent envelope (from
    // `qontinui-runner/src-tauri/src/event_system/types.rs`) is the source of
    // truth for the wire shape. Narrowing by `event_type` keeps
    // `event.payload.data.<field>` type-safe and breaks the build at this
    // call site if the Rust struct drifts. The local `CostUpdateEvent` /
    // `BudgetWarningEvent` / `CostAnomalyEvent` shapes still drive React
    // state — they're a structural subset of the generated `data` shape.
    listen<AppEventOf<"CostUpdate">>("cost-update", (event) => {
      const inner = event.payload?.data;
      if (!inner) return;
      setCostEvents((prev) => {
        const next = [inner as unknown as CostUpdateEvent, ...prev];
        return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
      });
      scheduleInvalidation();
    }).then((unlisten) => unlisteners.push(unlisten));

    listen<AppEventOf<"BudgetWarning">>("budget-warning", (event) => {
      const inner = event.payload?.data;
      if (!inner) return;
      setBudgetWarnings((prev) => [inner as unknown as BudgetWarningEvent, ...prev]);
    }).then((unlisten) => unlisteners.push(unlisten));

    listen<AppEventOf<"CostAnomaly">>("cost-anomaly", (event) => {
      const inner = event.payload?.data;
      if (!inner) return;
      setAnomalies((prev) => [inner as unknown as CostAnomalyEvent, ...prev]);
    }).then((unlisten) => unlisteners.push(unlisten));

    return () => {
      unlisteners.forEach((unlisten) => unlisten());
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [scheduleInvalidation]);

  const refresh = useCallback(() => {
    refetchDashboard();
    refetchBudget();
  }, [refetchDashboard, refetchBudget]);

  const status: CostControlStatus = useMemo(() => {
    const errors: CostControlSourceError[] = [];
    if (dashboardIsError) {
      errors.push({ source: "cost-dashboard", message: errorMessage(dashboardError) });
    }
    if (budgetIsError) {
      errors.push({ source: "budget", message: errorMessage(budgetError) });
    }
    return {
      isLoading: dashboardLoading || budgetLoading,
      isError: errors.length > 0,
      errors,
    };
  }, [
    dashboardIsError,
    dashboardError,
    budgetIsError,
    budgetError,
    dashboardLoading,
    budgetLoading,
  ]);

  return {
    costEvents,
    budgetWarnings,
    anomalies,
    dashboard: dashboard ?? null,
    activeBudget: activeBudget ?? null,
    isLoading: status.isLoading,
    status,
    refresh,
  };
}
