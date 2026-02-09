/**
 * IdleState Component
 *
 * Displayed when no workflow is actively running.
 * Shows a prompt to navigate to the execute page or view the last run.
 */

import { Workflow, History, Play, Loader2 } from "lucide-react";
import { Button } from "../ui";
import type { IdleStateProps } from "./types";

export function IdleState({
  onGoToRecap,
  onRunLastWorkflow,
  isRunningLastWorkflow,
  lastRunWorkflowName,
  lastRunWorkflowId,
}: IdleStateProps) {
  return (
    <div className="flex flex-1 items-center justify-center pt-16">
      <div className="flex flex-col items-center gap-6 text-center">
        <div className="rounded-full bg-muted p-3">
          <Workflow className="h-8 w-8 text-muted-foreground" />
        </div>

        <div>
          <h2 className="text-2xl font-semibold text-foreground">No Active Workflow</h2>
          <p className="mt-2 text-muted-foreground">
            {lastRunWorkflowId
              ? "Run a workflow to get started"
              : "Create a workflow in the Workflow Builder, then run it here"}
          </p>
        </div>

        <div className="flex flex-col gap-3">
          {/* Run Last Workflow button - shown when there's a last workflow */}
          {lastRunWorkflowId && onRunLastWorkflow && (
            <Button
              size="lg"
              onClick={onRunLastWorkflow}
              disabled={isRunningLastWorkflow}
              className="bg-green-600 hover:bg-green-700 text-white"
              data-ui-id="button-run-last-workflow"
            >
              {isRunningLastWorkflow ? (
                <Loader2 className="mr-2 h-5 w-5 animate-spin" />
              ) : (
                <Play className="mr-2 h-5 w-5" />
              )}
              {isRunningLastWorkflow ? "Starting..." : "Run Last Workflow"}
              {lastRunWorkflowName && (
                <span className="ml-2 text-sm opacity-90">({lastRunWorkflowName})</span>
              )}
            </Button>
          )}

          {onGoToRecap && (
            <Button
              size="lg"
              variant="outline"
              onClick={onGoToRecap}
              className="text-muted-foreground hover:text-foreground"
            >
              <History className="mr-2 h-5 w-5" />
              View Last Run
              {lastRunWorkflowName && (
                <span className="ml-2 text-xs opacity-70">({lastRunWorkflowName})</span>
              )}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
