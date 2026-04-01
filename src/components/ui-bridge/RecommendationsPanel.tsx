/**
 * RecommendationsPanel.tsx
 *
 * Prioritized list of actionable improvement recommendations
 * generated from automation analytics data.
 */

import { Lightbulb } from "lucide-react";
import { useQuery } from "@tanstack/react-query";

interface Recommendation {
  priority: number;
  category: string;
  message: string;
  impact: string;
}

interface RecommendationsPanelProps {
  days?: number;
}

const CATEGORY_COLORS: Record<string, string> = {
  selectors: "bg-blue-500/10 text-blue-500",
  performance: "bg-orange-500/10 text-orange-500",
  annotations: "bg-purple-500/10 text-purple-500",
  reliability: "bg-red-500/10 text-red-500",
};

export function RecommendationsPanel({ days = 7 }: RecommendationsPanelProps) {
  const { data = [], isLoading } = useQuery<Recommendation[]>({
    queryKey: ["graph-analytics", "recommendations", days],
    queryFn: async () => {
      const res = await fetch(
        `http://localhost:9876/ui-bridge/analytics/recommendations?days=${days}`,
      );
      if (!res.ok) return [];
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
  });

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Lightbulb className="w-4 h-4" />
        Improvement Recommendations
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Analyzing...</div>}

      {data.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">
          No recommendations — automation is running well
        </div>
      )}

      {data.length > 0 && (
        <div className="space-y-3">
          {data.map((rec, i) => (
            <div key={i} className="border border-border rounded-md p-3">
              <div className="flex items-start gap-2">
                <span className="shrink-0 w-6 h-6 rounded-full bg-primary/10 text-primary flex items-center justify-center text-xs font-bold">
                  {rec.priority}
                </span>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className={`text-xs px-2 py-0.5 rounded-full font-medium ${CATEGORY_COLORS[rec.category] ?? "bg-muted text-muted-foreground"}`}
                    >
                      {rec.category}
                    </span>
                  </div>
                  <p className="text-sm">{rec.message}</p>
                  <p className="text-xs text-muted-foreground mt-1">{rec.impact}</p>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
