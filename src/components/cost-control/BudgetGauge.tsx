/**
 * BudgetGauge.tsx
 *
 * Circular gauge showing budget utilization percentage with color coding.
 * Green (<60%), yellow (60-80%), red (>80%).
 */

import type { ActiveBudgetStatus } from "./types";

interface BudgetGaugeProps {
  budget: ActiveBudgetStatus;
}

function getGaugeColor(fraction: number): string {
  const used = 1 - fraction;
  if (used >= 0.8) return "text-red-500";
  if (used >= 0.6) return "text-yellow-500";
  return "text-green-500";
}

function getStrokeColor(fraction: number): string {
  const used = 1 - fraction;
  if (used >= 0.8) return "#ef4444";
  if (used >= 0.6) return "#eab308";
  return "#22c55e";
}

function formatCost(usd: number): string {
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}

export function BudgetGauge({ budget }: BudgetGaugeProps) {
  const usedFraction = 1 - budget.remaining_fraction;
  const usedPercent = Math.min(usedFraction * 100, 100);
  const radius = 52;
  const circumference = 2 * Math.PI * radius;
  const strokeDashoffset = circumference * (1 - usedFraction);
  const color = getStrokeColor(budget.remaining_fraction);
  const textColor = getGaugeColor(budget.remaining_fraction);

  return (
    <div className="flex flex-col items-center gap-2">
      <div className="relative w-36 h-36">
        <svg viewBox="0 0 120 120" className="w-full h-full -rotate-90">
          {/* Background circle */}
          <circle
            cx="60"
            cy="60"
            r={radius}
            fill="none"
            stroke="hsl(var(--muted))"
            strokeWidth="8"
          />
          {/* Progress arc */}
          <circle
            cx="60"
            cy="60"
            r={radius}
            fill="none"
            stroke={color}
            strokeWidth="8"
            strokeDasharray={circumference}
            strokeDashoffset={strokeDashoffset}
            strokeLinecap="round"
            className="transition-all duration-500"
          />
        </svg>
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className={`text-xl font-bold ${textColor}`}>
            {usedPercent.toFixed(0)}%
          </span>
          <span className="text-[10px] text-muted-foreground">used</span>
        </div>
      </div>
      <div className="text-center">
        <p className="text-sm font-medium">
          {formatCost(budget.total_cost_usd)} / {formatCost(budget.max_cost_usd)}
        </p>
        <p className="text-xs text-muted-foreground">
          {(budget.remaining_fraction * 100).toFixed(0)}% remaining
        </p>
      </div>
    </div>
  );
}
