/**
 * TopPatternsWidget.tsx
 *
 * Compact card for the Active Dashboard showing a summary of
 * active cross-run patterns (recurring findings, fix oscillations).
 */

import { AlertTriangle, TrendingUp } from "lucide-react";
import { useCrossRunPatterns } from "../../hooks/useGraphAnalytics";

export function TopPatternsWidget() {
  const { data: patterns } = useCrossRunPatterns();

  const activePatterns = patterns?.filter(p => p.status === "active") || [];
  if (activePatterns.length === 0) return null;

  const recurring = activePatterns.filter(p => p.pattern_type === "recurring_finding").length;
  const oscillations = activePatterns.filter(p => p.pattern_type === "fix_oscillation").length;

  return (
    <div className="bg-card rounded-lg border border-border p-3">
      <div className="flex items-center gap-2 mb-2">
        <TrendingUp className="w-4 h-4 text-orange-400" />
        <span className="text-sm font-medium">Cross-Run Patterns</span>
      </div>
      <div className="flex gap-4 text-xs">
        {recurring > 0 && (
          <span className="flex items-center gap-1 text-red-400">
            <AlertTriangle className="w-3 h-3" />
            {recurring} recurring finding{recurring !== 1 ? "s" : ""}
          </span>
        )}
        {oscillations > 0 && (
          <span className="flex items-center gap-1 text-yellow-500">
            <AlertTriangle className="w-3 h-3" />
            {oscillations} fix oscillation{oscillations !== 1 ? "s" : ""}
          </span>
        )}
      </div>
    </div>
  );
}
