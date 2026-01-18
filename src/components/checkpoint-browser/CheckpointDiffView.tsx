/**
 * CheckpointDiffView.tsx
 *
 * Displays the diff between two checkpoints.
 */

import { ArrowRight, RefreshCw, Database, Plus, Check, AlertTriangle } from "lucide-react";
import type { CheckpointDiff } from "../../types/checkpoint";

interface CheckpointDiffViewProps {
  diff: CheckpointDiff;
}

export function CheckpointDiffView({ diff }: CheckpointDiffViewProps) {
  const hasStateChange = diff.state_change !== null;
  const hasChannelChanges = diff.channel_changes.length > 0;
  const hasAddedFindings = diff.added_findings.length > 0;
  const hasResolvedFindings = diff.resolved_findings.length > 0;
  const noChanges =
    !hasStateChange && !hasChannelChanges && !hasAddedFindings && !hasResolvedFindings;

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="bg-gray-800 rounded-lg p-4">
        <h3 className="text-lg font-semibold text-white mb-4">Checkpoint Comparison</h3>

        {/* Summary Stats */}
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div className="bg-gray-900 rounded-lg p-3 text-center">
            <div className="text-2xl font-bold text-white">
              {diff.iteration_diff > 0 ? "+" : ""}
              {diff.iteration_diff}
            </div>
            <div className="text-xs text-gray-400">Iterations</div>
          </div>
          <div className="bg-gray-900 rounded-lg p-3 text-center">
            <div className="text-2xl font-bold text-blue-400">{diff.channel_changes.length}</div>
            <div className="text-xs text-gray-400">Channel Changes</div>
          </div>
          <div className="bg-gray-900 rounded-lg p-3 text-center">
            <div className="text-2xl font-bold text-red-400">{diff.added_findings.length}</div>
            <div className="text-xs text-gray-400">New Findings</div>
          </div>
          <div className="bg-gray-900 rounded-lg p-3 text-center">
            <div className="text-2xl font-bold text-green-400">{diff.resolved_findings.length}</div>
            <div className="text-xs text-gray-400">Resolved</div>
          </div>
        </div>
      </div>

      {noChanges && (
        <div className="bg-gray-800 rounded-lg p-6 text-center">
          <RefreshCw className="w-8 h-8 text-gray-600 mx-auto mb-2" />
          <p className="text-gray-400">No significant changes between checkpoints</p>
        </div>
      )}

      {/* State Change */}
      {hasStateChange && diff.state_change && (
        <div className="bg-gray-800 rounded-lg p-4">
          <h4 className="text-sm font-medium text-gray-400 mb-3">State Change</h4>
          <div className="flex items-center gap-3 text-lg">
            <span className="text-orange-400 font-mono">{diff.state_change[0]}</span>
            <ArrowRight className="w-5 h-5 text-gray-500" />
            <span className="text-green-400 font-mono">{diff.state_change[1]}</span>
          </div>
        </div>
      )}

      {/* Channel Changes */}
      {hasChannelChanges && (
        <div className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center gap-2 mb-3">
            <Database className="w-4 h-4 text-blue-400" />
            <h4 className="text-sm font-medium text-gray-400">Channel Changes</h4>
          </div>
          <div className="space-y-2">
            {diff.channel_changes.map((change, idx) => (
              <div key={idx} className="bg-gray-900 rounded p-3 flex items-center justify-between">
                <span className="font-mono text-sm text-white">{change.channel}</span>
                <div className="flex items-center gap-2 text-sm">
                  <span className="text-gray-400">v{change.from_version}</span>
                  <ArrowRight className="w-4 h-4 text-gray-500" />
                  <span className="text-blue-400">v{change.to_version}</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Added Findings */}
      {hasAddedFindings && (
        <div className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center gap-2 mb-3">
            <Plus className="w-4 h-4 text-red-400" />
            <h4 className="text-sm font-medium text-gray-400">New Findings</h4>
          </div>
          <div className="space-y-2">
            {diff.added_findings.map((findingId, idx) => (
              <div key={idx} className="bg-gray-900 rounded p-3 flex items-center gap-3">
                <AlertTriangle className="w-4 h-4 text-red-400 flex-shrink-0" />
                <span className="text-sm text-gray-300">{findingId}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Resolved Findings */}
      {hasResolvedFindings && (
        <div className="bg-gray-800 rounded-lg p-4">
          <div className="flex items-center gap-2 mb-3">
            <Check className="w-4 h-4 text-green-400" />
            <h4 className="text-sm font-medium text-gray-400">Resolved Findings</h4>
          </div>
          <div className="space-y-2">
            {diff.resolved_findings.map((findingId, idx) => (
              <div key={idx} className="bg-gray-900 rounded p-3 flex items-center gap-3">
                <Check className="w-4 h-4 text-green-400 flex-shrink-0" />
                <span className="text-sm text-gray-300 line-through">{findingId}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
