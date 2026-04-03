/**
 * useCostControl.ts
 *
 * Data hook for the Cost Control Panel.
 * Listens to Tauri events for real-time cost data, fetches historical
 * dashboard data and active budget status via TanStack Query.
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  CostUpdateEvent,
  BudgetWarningEvent,
  CostAnomalyEvent,
  CostDashboard,
  ActiveBudgetStatus,
} from "../components/cost-control/types";

const MAX_EVENTS = 200;

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
    refetch: refetchDashboard,
  } = useQuery<CostDashboard>({
    queryKey: ["cost-dashboard"],
    queryFn: () => invoke<CostDashboard>("get_cost_dashboard", { days: 30 }),
    staleTime: 30_000,
  });

  // Active budget status (may not exist yet - degrade gracefully)
  const {
    data: activeBudget,
    isLoading: budgetLoading,
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

  // Listen for Tauri events
  useEffect(() => {
    const unlisteners: (() => void)[] = [];

    listen<CostUpdateEvent>("cost-update", (event) => {
      setCostEvents((prev) => {
        const next = [event.payload, ...prev];
        return next.length > MAX_EVENTS ? next.slice(0, MAX_EVENTS) : next;
      });
      scheduleInvalidation();
    }).then((unlisten) => unlisteners.push(unlisten));

    listen<BudgetWarningEvent>("budget-warning", (event) => {
      setBudgetWarnings((prev) => [event.payload, ...prev]);
    }).then((unlisten) => unlisteners.push(unlisten));

    listen<CostAnomalyEvent>("cost-anomaly", (event) => {
      setAnomalies((prev) => [event.payload, ...prev]);
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

  return {
    costEvents,
    budgetWarnings,
    anomalies,
    dashboard: dashboard ?? null,
    activeBudget: activeBudget ?? null,
    isLoading: dashboardLoading || budgetLoading,
    refresh,
  };
}
