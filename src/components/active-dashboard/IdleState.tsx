/**
 * IdleState Component
 *
 * Displayed when no workflow is actively running.
 * Shows a prompt to navigate to the execute page or view the last run.
 */

import { ArrowRight, Workflow, History } from "lucide-react";
import { Button } from "../ui";
import type { IdleStateProps } from "./types";

export function IdleState({ onGoToExecute, onGoToRecap, lastRunWorkflowName }: IdleStateProps) {
  return (
    <div className="flex flex-1 items-center justify-center pt-16">
      <div className="flex flex-col items-center gap-6 text-center">
        <div className="rounded-full bg-muted p-3">
          <Workflow className="h-8 w-8 text-muted-foreground" />
        </div>

        <div>
          <h2 className="text-2xl font-semibold text-foreground">No Active Workflow</h2>
          <p className="mt-2 text-muted-foreground">Start a workflow from the Execute page</p>
        </div>

        <div className="flex flex-col gap-3">
          <Button size="lg" onClick={onGoToExecute} className="bg-primary hover:bg-primary/90">
            Go to Execute
            <ArrowRight className="ml-2 h-5 w-5" />
          </Button>

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
