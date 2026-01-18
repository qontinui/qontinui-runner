/**
 * PatternList.tsx
 *
 * Displays identified patterns with confidence scores and recommendations.
 */

import { useState } from "react";
import {
  TrendingUp,
  TrendingDown,
  Wrench,
  Users,
  RotateCcw,
  Clock,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type { Pattern, PatternType } from "../../types/learning";

interface PatternListProps {
  patterns: Pattern[];
}

function PatternTypeIcon({ type }: { type: PatternType }) {
  switch (type) {
    case "SuccessStrategy":
      return <TrendingUp className="w-4 h-4 text-green-500" />;
    case "FailureMode":
      return <TrendingDown className="w-4 h-4 text-red-500" />;
    case "ToolUsage":
      return <Wrench className="w-4 h-4 text-blue-500" />;
    case "Collaboration":
      return <Users className="w-4 h-4 text-purple-500" />;
    case "IterationBehavior":
      return <Clock className="w-4 h-4 text-cyan-500" />;
    case "ErrorRecovery":
      return <AlertTriangle className="w-4 h-4 text-orange-500" />;
    case "DecisionMaking":
      return <RotateCcw className="w-4 h-4 text-yellow-500" />;
    default:
      return <TrendingUp className="w-4 h-4 text-gray-500" />;
  }
}

function PatternTypeLabel({ type }: { type: PatternType }) {
  const labels: Record<PatternType, string> = {
    SuccessStrategy: "Success Strategy",
    FailureMode: "Failure Mode",
    ToolUsage: "Tool Usage",
    Collaboration: "Collaboration",
    IterationBehavior: "Iteration",
    ErrorRecovery: "Error Recovery",
    DecisionMaking: "Decision Making",
  };
  return <span className="text-xs text-gray-400">{labels[type]}</span>;
}

function ConfidenceBar({ confidence }: { confidence: number }) {
  const percentage = Math.round(confidence * 100);
  const getColor = () => {
    if (percentage >= 80) return "bg-green-500";
    if (percentage >= 60) return "bg-blue-500";
    if (percentage >= 40) return "bg-yellow-500";
    return "bg-red-500";
  };

  return (
    <div className="flex items-center gap-2">
      <div className="w-full h-2 bg-gray-700 rounded-full overflow-hidden">
        <div
          className={`h-full ${getColor()} transition-all`}
          style={{ width: `${percentage}%` }}
        />
      </div>
      <span className="text-xs text-gray-400 w-12">{percentage}%</span>
    </div>
  );
}

function PatternCard({ pattern }: { pattern: Pattern }) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="bg-gray-800 rounded-lg overflow-hidden">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full p-3 flex items-start gap-3 hover:bg-gray-750 transition-colors text-left"
      >
        <PatternTypeIcon type={pattern.pattern_type} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h4 className="font-medium text-white truncate">{pattern.name}</h4>
            <PatternTypeLabel type={pattern.pattern_type} />
          </div>
          <p className="text-sm text-gray-400 line-clamp-1">{pattern.description}</p>
          <div className="mt-2">
            <ConfidenceBar confidence={pattern.confidence} />
          </div>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-gray-400">{pattern.occurrences} occurrences</span>
          {expanded ? (
            <ChevronDown className="w-4 h-4 text-gray-400" />
          ) : (
            <ChevronRight className="w-4 h-4 text-gray-400" />
          )}
        </div>
      </button>

      {expanded && (
        <div className="px-3 pb-3 space-y-3 border-t border-gray-700">
          {/* Success Rate */}
          <div className="flex items-center justify-between pt-3">
            <span className="text-sm text-gray-400">Success Rate</span>
            <span
              className={`text-sm font-medium ${pattern.success_rate >= 0.7 ? "text-green-400" : pattern.success_rate >= 0.5 ? "text-yellow-400" : "text-red-400"}`}
            >
              {Math.round(pattern.success_rate * 100)}%
            </span>
          </div>

          {/* Triggers */}
          {pattern.triggers.length > 0 && (
            <div>
              <span className="text-xs text-gray-500 uppercase">Triggers</span>
              <div className="flex flex-wrap gap-1 mt-1">
                {pattern.triggers.map((trigger, idx) => (
                  <span key={idx} className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                    {trigger}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* Recommendations */}
          {pattern.recommendations.length > 0 && (
            <div>
              <span className="text-xs text-gray-500 uppercase">Recommendations</span>
              <ul className="mt-1 space-y-1">
                {pattern.recommendations.map((rec, idx) => (
                  <li key={idx} className="text-sm text-gray-300 flex items-start gap-2">
                    <span className="text-green-500">•</span>
                    {rec}
                  </li>
                ))}
              </ul>
            </div>
          )}

          {/* Observed In */}
          {pattern.observed_in.length > 0 && (
            <div>
              <span className="text-xs text-gray-500 uppercase">
                Observed in {pattern.observed_in.length} tasks
              </span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function PatternList({ patterns }: PatternListProps) {
  if (patterns.length === 0) {
    return (
      <div className="bg-gray-800 rounded-lg p-6 text-center">
        <TrendingUp className="w-8 h-8 text-gray-600 mx-auto mb-2" />
        <p className="text-gray-400">No patterns identified yet</p>
        <p className="text-sm text-gray-500 mt-1">Run more tasks to start identifying patterns</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {patterns.map((pattern) => (
        <PatternCard key={pattern.id} pattern={pattern} />
      ))}
    </div>
  );
}
