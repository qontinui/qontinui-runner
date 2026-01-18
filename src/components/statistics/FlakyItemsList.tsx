/**
 * FlakyItemsList.tsx
 *
 * Shows list of flaky transitions and templates with:
 * - Success rate
 * - Total attempts
 * - Severity indicator
 */

import { AlertTriangle, ArrowRightLeft, Image } from "lucide-react";
import { getAccentColors, getSeverityColors } from "@/design-system";
import type { FlakyItem } from "../../types/statistics";

interface FlakyItemsListProps {
  items: FlakyItem[];
}

export function FlakyItemsList({ items }: FlakyItemsListProps) {
  const getSeverityColor = (rate: number) => {
    if (rate < 0.5) {
      const colors = getSeverityColors("high");
      return `${colors.text} ${colors.bg}`;
    }
    if (rate < 0.7) {
      const colors = getSeverityColors("medium");
      return `${colors.text} ${colors.bg}`;
    }
    const colors = getSeverityColors("low");
    return `${colors.text} ${colors.bg}`;
  };

  const getSeverityLabel = (rate: number) => {
    if (rate < 0.5) return "High";
    if (rate < 0.7) return "Medium";
    return "Low";
  };

  const getSeverityTextColor = (rate: number) => {
    if (rate < 0.5) return getSeverityColors("high").text;
    if (rate < 0.7) return getSeverityColors("medium").text;
    return getSeverityColors("low").text;
  };

  if (items.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <AlertTriangle className="w-4 h-4" />
          Flaky Items
        </h3>
        <div className="text-center text-muted-foreground py-4">
          <p>No flaky items detected</p>
          <p className="text-xs mt-1">Items with 20-80% success rate appear here</p>
        </div>
      </div>
    );
  }

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <AlertTriangle className={`w-4 h-4 ${getAccentColors("orange").text}`} />
        Flaky Items
        <span
          className={`text-xs ${getAccentColors("orange").bg} ${getAccentColors("orange").text} px-2 py-0.5 rounded-full`}
        >
          {items.length}
        </span>
      </h3>

      <div className="space-y-2 max-h-64 overflow-auto">
        {items.map((item, idx) => (
          <div
            key={`${item.name}-${idx}`}
            className="p-3 rounded-md bg-muted/30 hover:bg-muted/50 transition-colors"
          >
            <div className="flex items-start justify-between gap-2">
              <div className="flex items-center gap-2 min-w-0">
                {item.item_type === "transition" ? (
                  <ArrowRightLeft className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
                ) : (
                  <Image className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
                )}
                <div className="min-w-0">
                  <p className="text-sm font-medium truncate">{item.name}</p>
                  <p className="text-xs text-muted-foreground">{item.total_attempts} attempts</p>
                </div>
              </div>
              <div className="text-right flex-shrink-0">
                <div className={`text-sm font-medium ${getSeverityTextColor(item.success_rate)}`}>
                  {(item.success_rate * 100).toFixed(0)}%
                </div>
                <span
                  className={`text-xs px-1.5 py-0.5 rounded ${getSeverityColor(item.success_rate)}`}
                >
                  {getSeverityLabel(item.success_rate)}
                </span>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
