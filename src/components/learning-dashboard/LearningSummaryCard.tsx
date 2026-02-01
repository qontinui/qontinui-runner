/**
 * LearningSummaryCard.tsx
 *
 * Shows overview statistics for the learning system.
 */

import {
  Brain,
  TrendingUp,
  TrendingDown,
  Minus,
  CheckCircle,
  XCircle,
  Activity,
} from "lucide-react";
import type { LearningSummary, Trend } from "../../types/learning";

interface LearningSummaryCardProps {
  summary: LearningSummary;
  trend: Trend;
}

export function LearningSummaryCard({ summary, trend }: LearningSummaryCardProps) {
  const successRate =
    summary.total_tasks > 0
      ? Math.round((summary.successful_tasks / summary.total_tasks) * 100)
      : 0;

  const getTrendIcon = () => {
    switch (trend) {
      case "Improving":
        return <TrendingUp className="w-4 h-4 text-green-500" />;
      case "Declining":
        return <TrendingDown className="w-4 h-4 text-red-500" />;
      case "Stable":
        return <Minus className="w-4 h-4 text-gray-500" />;
      case "Insufficient":
        return <Activity className="w-4 h-4 text-gray-400" />;
    }
  };

  const getTrendLabel = () => {
    switch (trend) {
      case "Improving":
        return "Improving";
      case "Declining":
        return "Declining";
      case "Stable":
        return "Stable";
      case "Insufficient":
        return "Insufficient Data";
    }
  };

  const getTrendColor = () => {
    switch (trend) {
      case "Improving":
        return "text-green-500";
      case "Declining":
        return "text-red-500";
      case "Stable":
        return "text-gray-500";
      case "Insufficient":
        return "text-gray-400";
    }
  };

  return (
    <div className="bg-gray-800 rounded-lg p-4 space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain className="w-5 h-5 text-purple-400" />
          <h3 className="font-semibold text-white">Learning Summary</h3>
        </div>
        <div className={`flex items-center gap-1 text-sm ${getTrendColor()}`}>
          {getTrendIcon()}
          <span>{getTrendLabel()}</span>
        </div>
      </div>

      {/* Main Stats Grid */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        {/* Total Tasks */}
        <div className="bg-gray-900 rounded-lg p-3">
          <div className="text-2xl font-bold text-white">{summary.total_tasks}</div>
          <div className="text-xs text-gray-400">Total Tasks</div>
        </div>

        {/* Success Rate */}
        <div className="bg-gray-900 rounded-lg p-3">
          <div
            className={`text-2xl font-bold ${successRate >= 70 ? "text-green-400" : successRate >= 50 ? "text-yellow-400" : "text-red-400"}`}
          >
            {successRate}%
          </div>
          <div className="text-xs text-gray-400">Success Rate</div>
        </div>

        {/* Patterns */}
        <div className="bg-gray-900 rounded-lg p-3">
          <div className="text-2xl font-bold text-blue-400">{summary.patterns_identified}</div>
          <div className="text-xs text-gray-400">Patterns Found</div>
        </div>

        {/* Insights */}
        <div className="bg-gray-900 rounded-lg p-3">
          <div className="text-2xl font-bold text-purple-400">{summary.insights_generated}</div>
          <div className="text-xs text-gray-400">Insights</div>
        </div>
      </div>

      {/* Success/Failure Breakdown */}
      <div className="flex items-center gap-4">
        <div className="flex items-center gap-2">
          <CheckCircle className="w-4 h-4 text-green-500" />
          <span className="text-sm text-gray-300">{summary.successful_tasks} successful</span>
        </div>
        <div className="flex items-center gap-2">
          <XCircle className="w-4 h-4 text-red-500" />
          <span className="text-sm text-gray-300">{summary.failed_tasks} failed</span>
        </div>
      </div>

      {/* Additional Stats */}
      <div className="flex items-center justify-between text-sm text-gray-400 border-t border-gray-700 pt-3">
        <span>{summary.strategies_tracked} strategies tracked</span>
        <span>{summary.tools_tracked} tools tracked</span>
        <span>{summary.feedback_items} feedback items</span>
      </div>
    </div>
  );
}
