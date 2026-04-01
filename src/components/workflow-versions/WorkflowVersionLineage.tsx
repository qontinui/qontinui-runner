/**
 * WorkflowVersionLineage.tsx
 *
 * Displays the version history of a workflow, showing how it has evolved
 * through generation iterations with diff summaries and triggers.
 */

import { GitBranch, GitCommit } from "lucide-react";
import { useWorkflowVersions, type WorkflowVersion } from "../../hooks/useGraphAnalytics";
import { useExecution } from "../../contexts";

const TRIGGER_COLORS: Record<string, string> = {
  generation: "bg-green-500/10 text-green-500",
  reflection: "bg-purple-500/10 text-purple-500",
  manual: "bg-blue-500/10 text-blue-500",
  fix: "bg-orange-500/10 text-orange-500",
};

function getTriggerStyle(trigger: string): string {
  for (const [key, style] of Object.entries(TRIGGER_COLORS)) {
    if (trigger.toLowerCase().includes(key)) return style;
  }
  return "bg-muted text-muted-foreground";
}

function VersionNode({ version }: { version: WorkflowVersion }) {
  const created = new Date(version.created_at).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

  return (
    <div className="flex items-start gap-3 py-2">
      <div className="flex flex-col items-center mt-1">
        <GitCommit className="w-4 h-4 text-muted-foreground" />
        <div className="w-px h-full bg-border" />
      </div>
      <div className="flex-1 min-w-0 pb-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-semibold">v{version.version_number}</span>
          <span className={`text-xs px-1.5 py-0.5 rounded ${getTriggerStyle(version.trigger)}`}>
            {version.trigger}
          </span>
        </div>
        {version.diff_summary && (
          <p className="text-xs text-muted-foreground mt-0.5 line-clamp-2">
            {version.diff_summary}
          </p>
        )}
        <div className="text-xs text-muted-foreground mt-0.5">{created}</div>
      </div>
    </div>
  );
}

export function WorkflowVersionLineage() {
  const { selectedWorkflow } = useExecution();
  const workflowId = selectedWorkflow || "";
  const { data: versions, isLoading, error } = useWorkflowVersions(workflowId);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <GitBranch className="w-4 h-4" />
        Workflow Version Lineage
      </h3>

      {!workflowId && (
        <div className="text-center text-muted-foreground py-8">
          Select a workflow to view version history
        </div>
      )}

      {isLoading && workflowId && (
        <div className="text-center text-muted-foreground py-8">Loading versions...</div>
      )}

      {error && (
        <div className="text-center text-destructive py-8">
          Failed to load versions: {String(error)}
        </div>
      )}

      {versions && versions.length === 0 && (
        <div className="text-center text-muted-foreground py-8">No version history available</div>
      )}

      {versions && versions.length > 0 && (
        <div className="max-h-96 overflow-y-auto">
          {versions.map((version) => (
            <VersionNode key={version.id} version={version} />
          ))}
        </div>
      )}
    </div>
  );
}
