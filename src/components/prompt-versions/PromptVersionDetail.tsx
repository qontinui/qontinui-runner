/**
 * PromptVersionDetail — Full content and metrics for a single prompt version.
 * Includes a "Restore this version" button that creates a new version with the old content.
 */

import { useState } from "react";
import { ArrowLeft, RotateCcw, Loader2, BarChart3 } from "lucide-react";
import { usePromptVersion, useCreatePromptVersion } from "@/hooks/usePromptVersions";

interface PromptVersionDetailProps {
  templateId: string;
  versionNumber: number;
  onBack: () => void;
  onRestored: () => void;
}

export function PromptVersionDetail({
  templateId,
  versionNumber,
  onBack,
  onRestored,
}: PromptVersionDetailProps) {
  const { data: version, isLoading, error } = usePromptVersion(templateId, versionNumber);
  const createVersion = useCreatePromptVersion(templateId);
  const [restoreConfirm, setRestoreConfirm] = useState(false);

  const handleRestore = async () => {
    if (!version) return;
    try {
      await createVersion.mutateAsync({
        content: version.content,
        change_description: `Restored from version ${versionNumber}`,
      });
      onRestored();
    } catch (e) {
      console.error("Failed to restore version:", e);
    }
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-40">
        <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !version) {
    return (
      <div className="p-4 space-y-4">
        <button
          onClick={onBack}
          className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="w-4 h-4" />
          Back
        </button>
        <div className="text-red-400 text-sm">Failed to load version details.</div>
      </div>
    );
  }

  const metrics = version.performance_metrics;

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <button
          onClick={onBack}
          className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="w-4 h-4" />
          Back to history
        </button>

        <div className="flex items-center gap-2">
          {restoreConfirm ? (
            <>
              <span className="text-xs text-amber-400">Create new version with this content?</span>
              <button
                onClick={handleRestore}
                disabled={createVersion.isPending}
                className="px-3 py-1 text-xs bg-amber-600 text-white rounded hover:bg-amber-500 disabled:opacity-50"
              >
                {createVersion.isPending ? "Restoring..." : "Confirm"}
              </button>
              <button
                onClick={() => setRestoreConfirm(false)}
                className="px-3 py-1 text-xs bg-muted/60 text-muted-foreground rounded hover:bg-muted"
              >
                Cancel
              </button>
            </>
          ) : (
            <button
              onClick={() => setRestoreConfirm(true)}
              className="flex items-center gap-1 px-3 py-1 text-xs bg-muted/60 text-muted-foreground rounded hover:bg-muted"
            >
              <RotateCcw className="w-3.5 h-3.5" />
              Restore this version
            </button>
          )}
        </div>
      </div>

      {/* Meta info */}
      <div className="flex items-center gap-4 text-xs text-muted-foreground">
        <span className="font-mono px-2 py-0.5 rounded bg-muted text-muted-foreground">
          v{version.version_number}
        </span>
        <span>{new Date(version.created_at).toLocaleString()}</span>
        <span className="font-mono" title={version.content_hash}>
          hash: {version.content_hash?.substring(0, 12)}
        </span>
      </div>

      {version.change_description && (
        <div className="text-sm text-foreground bg-muted/50 rounded-md p-3">
          {version.change_description}
        </div>
      )}

      {/* Performance metrics */}
      {metrics &&
        (metrics.success_rate != null ||
          metrics.avg_cost != null ||
          metrics.avg_latency != null) && (
          <div className="space-y-2">
            <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <BarChart3 className="w-3.5 h-3.5" />
              Performance Metrics
            </div>
            <div className="grid grid-cols-3 gap-3">
              {metrics.success_rate != null && (
                <div className="bg-muted/50 rounded-md p-3 text-center">
                  <div className="text-lg font-semibold text-green-400">
                    {(metrics.success_rate * 100).toFixed(1)}%
                  </div>
                  <div className="text-xs text-muted-foreground">Success Rate</div>
                </div>
              )}
              {metrics.avg_cost != null && (
                <div className="bg-muted/50 rounded-md p-3 text-center">
                  <div className="text-lg font-semibold text-primary">
                    ${metrics.avg_cost.toFixed(4)}
                  </div>
                  <div className="text-xs text-muted-foreground">Avg Cost</div>
                </div>
              )}
              {metrics.avg_latency != null && (
                <div className="bg-muted/50 rounded-md p-3 text-center">
                  <div className="text-lg font-semibold text-amber-400">
                    {metrics.avg_latency.toFixed(0)}ms
                  </div>
                  <div className="text-xs text-muted-foreground">Avg Latency</div>
                </div>
              )}
            </div>
          </div>
        )}

      {/* Prompt content */}
      <div>
        <div className="text-xs text-muted-foreground mb-1">Prompt Content</div>
        <pre className="text-xs text-foreground bg-card border border-border p-4 rounded-lg overflow-auto max-h-96 whitespace-pre-wrap font-mono">
          {version.content}
        </pre>
      </div>
    </div>
  );
}

export default PromptVersionDetail;
