/**
 * useCanaryAlerts Hook
 *
 * Listens for `canary-alert` Tauri events emitted when a canary rollout
 * detects a significant regression (rollback verdict). Shows a toast-style
 * notification so the user is aware without needing to check the dashboard.
 */

import { useEffect, useState, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface CanaryAlert {
  id: string;
  canary_id: string;
  recommendation_id?: string;
  verdict: string;
  message: string;
  p_value?: number | null;
  delta?: number;
  baseline_success_rate?: number;
  canary_success_rate?: number;
  timestamp: number;
}

interface CanaryAlertPayload {
  canary_id: string;
  recommendation_id?: string;
  verdict: string;
  message: string;
  p_value?: number | null;
  delta?: number;
  baseline_success_rate?: number;
  canary_success_rate?: number;
}

let alertCounter = 0;

export function useCanaryAlerts() {
  const [alerts, setAlerts] = useState<CanaryAlert[]>([]);

  useEffect(() => {
    let unlistenFn: UnlistenFn | null = null;

    const setup = async () => {
      unlistenFn = await listen<CanaryAlertPayload>("canary-alert", (event) => {
        const payload = event.payload;
        const alert: CanaryAlert = {
          id: `canary-alert-${++alertCounter}`,
          canary_id: payload.canary_id,
          recommendation_id: payload.recommendation_id,
          verdict: payload.verdict,
          message: payload.message,
          p_value: payload.p_value,
          delta: payload.delta,
          baseline_success_rate: payload.baseline_success_rate,
          canary_success_rate: payload.canary_success_rate,
          timestamp: Date.now(),
        };
        setAlerts((prev) => [...prev, alert]);

        // Auto-dismiss after 8 seconds (longer than normal toasts for important alerts)
        setTimeout(() => {
          setAlerts((prev) => prev.filter((a) => a.id !== alert.id));
        }, 8000);
      });
    };

    setup();

    return () => {
      if (unlistenFn) unlistenFn();
    };
  }, []);

  const dismissAlert = useCallback((id: string) => {
    setAlerts((prev) => prev.filter((a) => a.id !== id));
  }, []);

  return { alerts, dismissAlert };
}
