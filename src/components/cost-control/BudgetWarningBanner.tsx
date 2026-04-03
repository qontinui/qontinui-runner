/**
 * BudgetWarningBanner.tsx
 *
 * Dismissible alert banner that appears when budget warnings arrive.
 */

import { useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import type { BudgetWarningEvent } from "./types";

interface BudgetWarningBannerProps {
  warnings: BudgetWarningEvent[];
}

export function BudgetWarningBanner({ warnings }: BudgetWarningBannerProps) {
  const [dismissed, setDismissed] = useState<Set<string>>(new Set());

  const visible = warnings.filter((w) => !dismissed.has(String(w.timestamp)));
  if (visible.length === 0) return null;

  const latest = visible[0];
  const remainingPct = (latest.remaining_fraction * 100).toFixed(0);

  return (
    <div className="rounded-lg border border-yellow-500/30 bg-yellow-500/10 p-3 flex items-start gap-3">
      <AlertTriangle className="w-5 h-5 text-yellow-500 shrink-0 mt-0.5" />
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium text-yellow-500">
          Budget Warning - {remainingPct}% remaining
        </p>
        <p className="text-xs text-muted-foreground mt-1">
          {latest.message}
        </p>
        <p className="text-xs text-muted-foreground mt-0.5">
          ${latest.total_cost_usd.toFixed(2)} / ${latest.budget_limit_usd.toFixed(2)}
        </p>
        {visible.length > 1 && (
          <p className="text-xs text-yellow-500/70 mt-1">
            +{visible.length - 1} more warning{visible.length > 2 ? "s" : ""}
          </p>
        )}
      </div>
      <button
        onClick={() => {
          setDismissed((prev) => {
            const next = new Set(prev);
            visible.forEach((w) => next.add(String(w.timestamp)));
            return next;
          });
        }}
        className="p-1 rounded hover:bg-muted transition-colors shrink-0"
        aria-label="Dismiss warning"
      >
        <X className="w-4 h-4 text-muted-foreground" />
      </button>
    </div>
  );
}
