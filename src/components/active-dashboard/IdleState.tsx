/**
 * IdleState Component
 *
 * Displayed when no workflow is actively running.
 * Shows a prompt to navigate to the execute page.
 */

import { ArrowRight, Workflow } from "lucide-react";
import { Button } from "../ui";
import type { IdleStateProps } from "./types";

export function IdleState({ onGoToExecute }: IdleStateProps) {
  return (
    <div className="flex flex-1 items-center justify-center">
      <div className="flex flex-col items-center gap-6 text-center">
        <div className="rounded-full bg-muted p-6">
          <Workflow className="h-16 w-16 text-muted-foreground" />
        </div>

        <div>
          <h2 className="text-2xl font-semibold text-foreground">No Active Workflow</h2>
          <p className="mt-2 text-muted-foreground">Start a workflow from the Execute page</p>
        </div>

        <Button size="lg" onClick={onGoToExecute} className="bg-primary hover:bg-primary/90">
          Go to Execute
          <ArrowRight className="ml-2 h-5 w-5" />
        </Button>
      </div>
    </div>
  );
}
