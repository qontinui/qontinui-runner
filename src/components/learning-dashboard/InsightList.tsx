/**
 * InsightList.tsx
 *
 * Displays generated insights with categories and recommendations.
 */

import { useState } from "react";
import {
  Lightbulb,
  Zap,
  Target,
  Shield,
  TrendingUp,
  Wrench,
  Users,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type { Insight, InsightCategory } from "../../types/learning";

interface InsightListProps {
  insights: Insight[];
}

function CategoryIcon({ category }: { category: InsightCategory }) {
  switch (category) {
    case "Performance":
      return <Zap className="w-4 h-4 text-yellow-500" />;
    case "Quality":
      return <Target className="w-4 h-4 text-blue-500" />;
    case "Efficiency":
      return <TrendingUp className="w-4 h-4 text-green-500" />;
    case "Reliability":
      return <Shield className="w-4 h-4 text-purple-500" />;
    case "Strategy":
      return <Lightbulb className="w-4 h-4 text-orange-500" />;
    case "ToolUsage":
      return <Wrench className="w-4 h-4 text-cyan-500" />;
    case "Collaboration":
      return <Users className="w-4 h-4 text-pink-500" />;
    default:
      return <Lightbulb className="w-4 h-4 text-gray-500" />;
  }
}

function getCategoryLabel(category: InsightCategory): string {
  const labels: Record<InsightCategory, string> = {
    Performance: "Performance",
    Quality: "Quality",
    Efficiency: "Efficiency",
    Reliability: "Reliability",
    Strategy: "Strategy",
    ToolUsage: "Tool Usage",
    Collaboration: "Collaboration",
  };
  return labels[category];
}

function getCategoryColor(category: InsightCategory): string {
  const colors: Record<InsightCategory, string> = {
    Performance: "bg-yellow-500/10 text-yellow-400 border-yellow-500/20",
    Quality: "bg-blue-500/10 text-blue-400 border-blue-500/20",
    Efficiency: "bg-green-500/10 text-green-400 border-green-500/20",
    Reliability: "bg-purple-500/10 text-purple-400 border-purple-500/20",
    Strategy: "bg-orange-500/10 text-orange-400 border-orange-500/20",
    ToolUsage: "bg-cyan-500/10 text-cyan-400 border-cyan-500/20",
    Collaboration: "bg-pink-500/10 text-pink-400 border-pink-500/20",
  };
  return colors[category];
}

function InsightCard({ insight }: { insight: Insight }) {
  const [expanded, setExpanded] = useState(false);
  const confidencePercent = Math.round(insight.confidence * 100);

  return (
    <div className="bg-gray-800 rounded-lg overflow-hidden">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full p-3 flex items-start gap-3 hover:bg-gray-750 transition-colors text-left"
      >
        <CategoryIcon category={insight.category} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span
              className={`text-xs px-2 py-0.5 rounded-full border ${getCategoryColor(insight.category)}`}
            >
              {getCategoryLabel(insight.category)}
            </span>
            <span className="text-xs text-gray-400">{confidencePercent}% confidence</span>
          </div>
          <p className="text-sm text-gray-200 mt-1">{insight.content}</p>
        </div>
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-gray-400 flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-gray-400 flex-shrink-0" />
        )}
      </button>

      {expanded && (
        <div className="px-3 pb-3 space-y-3 border-t border-gray-700">
          {/* Evidence */}
          {insight.evidence.length > 0 && (
            <div className="pt-3">
              <span className="text-xs text-gray-500 uppercase">Evidence</span>
              <ul className="mt-1 space-y-1">
                {insight.evidence.map((ev, idx) => (
                  <li key={idx} className="text-sm text-gray-400 flex items-start gap-2">
                    <span className="text-gray-600">•</span>
                    {ev}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {/* Recommendations */}
          {insight.recommendations.length > 0 && (
            <div>
              <span className="text-xs text-gray-500 uppercase">Recommendations</span>
              <ul className="mt-1 space-y-1">
                {insight.recommendations.map((rec, idx) => (
                  <li key={idx} className="text-sm text-gray-300 flex items-start gap-2">
                    <span className="text-green-500">→</span>
                    {rec}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {/* Related Patterns */}
          {insight.related_patterns.length > 0 && (
            <div>
              <span className="text-xs text-gray-500 uppercase">Related Patterns</span>
              <div className="flex flex-wrap gap-1 mt-1">
                {insight.related_patterns.map((pat, idx) => (
                  <span key={idx} className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                    {pat}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* Generated At */}
          <div className="text-xs text-gray-500">
            Generated: {new Date(insight.generated_at).toLocaleString()}
          </div>
        </div>
      )}
    </div>
  );
}

export function InsightList({ insights }: InsightListProps) {
  if (insights.length === 0) {
    return (
      <div className="bg-gray-800 rounded-lg p-6 text-center">
        <Lightbulb className="w-8 h-8 text-gray-600 mx-auto mb-2" />
        <p className="text-gray-400">No insights generated yet</p>
        <p className="text-sm text-gray-500 mt-1">
          Insights will appear as patterns are identified
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {insights.map((insight) => (
        <InsightCard key={insight.id} insight={insight} />
      ))}
    </div>
  );
}
