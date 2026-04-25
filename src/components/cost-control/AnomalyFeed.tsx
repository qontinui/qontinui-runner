/**
 * AnomalyFeed.tsx
 *
 * Scrollable list of detected cost anomalies.
 */

import { AlertTriangle } from "lucide-react";
import type { CostAnomalyEvent } from "./types";

interface AnomalyFeedProps {
  anomalies: CostAnomalyEvent[];
}

function formatTime(timestamp: number): string {
  try {
    return new Date(timestamp).toLocaleTimeString();
  } catch {
    return String(timestamp);
  }
}

export function AnomalyFeed({ anomalies }: AnomalyFeedProps) {
  if (anomalies.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border/50 p-4">
        <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
          <AlertTriangle className="w-4 h-4" />
          Anomaly Detection
        </h3>
        <div className="text-center text-muted-foreground py-6">
          <p className="text-sm">No anomalies detected.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="bg-card rounded-lg border border-border/50 p-4">
      <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
        <AlertTriangle className="w-4 h-4 text-yellow-500" />
        Anomaly Detection
        <span className="text-xs text-muted-foreground font-normal">({anomalies.length})</span>
      </h3>
      <div className="max-h-48 overflow-y-auto space-y-2">
        {anomalies.map((anomaly, i) => (
          <div
            key={`${anomaly.timestamp}-${i}`}
            className="rounded-md bg-yellow-500/5 border border-yellow-500/20 p-2.5 text-xs"
          >
            <div className="flex items-center justify-between mb-1">
              <span className="text-yellow-500 font-medium">
                z-score: {anomaly.z_score.toFixed(2)}
              </span>
              <span className="text-muted-foreground">{formatTime(anomaly.timestamp)}</span>
            </div>
            <p className="text-muted-foreground">{anomaly.message}</p>
            <div className="flex gap-3 mt-1 text-muted-foreground">
              <span>Cost: ${anomaly.cost_usd.toFixed(4)}</span>
              <span>Mean: ${anomaly.mean_cost_usd.toFixed(4)}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
