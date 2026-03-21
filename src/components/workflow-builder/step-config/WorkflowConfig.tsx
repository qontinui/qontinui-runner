import { Workflow } from "lucide-react";
import type { UnifiedStep, WorkflowStep } from "../../../types/unified-workflow";

export function WorkflowConfig({
  step,
  onChangeWorkflow,
}: {
  step: UnifiedStep & { type: "workflow" };
  onChangeWorkflow?: () => void;
}) {
  const wfStep = step as WorkflowStep;
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3 p-3 bg-zinc-800 border border-zinc-700 rounded-lg">
        <div className="p-2 rounded-md bg-blue-500/10 text-blue-400">
          <Workflow className="w-5 h-5" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="font-medium text-zinc-200">
            {wfStep.workflow_name || "No workflow selected"}
          </div>
          {wfStep.workflow_id && (
            <p className="text-xs text-zinc-500 mt-0.5 font-mono truncate">{wfStep.workflow_id}</p>
          )}
        </div>
      </div>

      {onChangeWorkflow && (
        <button
          onClick={onChangeWorkflow}
          className="w-full px-3 py-2 text-sm bg-zinc-700 hover:bg-zinc-600 rounded-md text-zinc-200 transition-colors"
        >
          Change Workflow
        </button>
      )}

      {!wfStep.workflow_id && (
        <p className="text-sm text-amber-400/80">
          No workflow selected. Click &quot;Change Workflow&quot; to pick one.
        </p>
      )}
    </div>
  );
}
