/**
 * CircuitBreakerStatus.tsx
 *
 * Card showing circuit breaker status: green "Closed" or red "Tripped".
 */

import { Shield, ShieldAlert } from "lucide-react";
import { Badge } from "../ui/Badge";
import type { ActiveBudgetStatus } from "./types";

interface CircuitBreakerStatusProps {
  budget: ActiveBudgetStatus | null;
}

export function CircuitBreakerStatus({ budget }: CircuitBreakerStatusProps) {
  const tripped = budget?.circuit_breaker_tripped ?? false;

  return (
    <div className="bg-card rounded-lg border border-border/50 p-4">
      <div className="flex items-center gap-2 mb-3">
        {tripped ? (
          <ShieldAlert className="w-4 h-4 text-red-500" />
        ) : (
          <Shield className="w-4 h-4 text-green-500" />
        )}
        <h3 className="text-sm font-semibold">Circuit Breaker</h3>
      </div>

      {!budget ? (
        <div className="text-sm text-muted-foreground">
          <p>No circuit breakers active.</p>
          <p className="text-xs mt-1">
            Breakers arm automatically when a run with a budget starts.
          </p>
        </div>
      ) : (
        <>
          <div className="flex items-center gap-3">
            <Badge variant={tripped ? "danger" : "success"}>{tripped ? "Tripped" : "Closed"}</Badge>
            {tripped && (
              <span className="text-xs text-red-400">Budget exceeded or anomaly detected</span>
            )}
          </div>

          <div className="mt-3 text-xs text-muted-foreground space-y-1">
            <p>Anomaly samples: {budget.anomaly_detector_sample_count}</p>
            {budget.anomaly_detector_mean > 0 && (
              <p>Mean cost: ${budget.anomaly_detector_mean.toFixed(4)}</p>
            )}
          </div>
        </>
      )}
    </div>
  );
}
