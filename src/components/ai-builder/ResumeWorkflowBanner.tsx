/**
 * ResumeWorkflowBanner.tsx
 *
 * Banner displayed when there's a resumable workflow after runner restart.
 */

import { Loader2, Play, RotateCcw } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getAccentColors } from "@/design-system";

export function ResumeWorkflowBanner() {
  const { resumableWorkflow, isRunning, isResuming, handleResumeWorkflow } = useAiBuilder();

  if (!resumableWorkflow || isRunning) {
    return null;
  }

  return (
    <div
      className={`card p-4 border-2 ${getAccentColors("orange").border} ${getAccentColors("orange").bg}`}
    >
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className={`p-2 ${getAccentColors("orange").bg} rounded-lg`}>
            <RotateCcw className={`w-5 h-5 ${getAccentColors("orange").text}`} />
          </div>
          <div>
            <h4 className="font-medium text-foreground">Continue Previous Workflow</h4>
            <p className="text-sm text-muted-foreground">
              <span className="font-medium">{resumableWorkflow.name}</span>
              {resumableWorkflow.totalPhases > 0 && (
                <span className="ml-2">
                  - Step {resumableWorkflow.currentPhase} of {resumableWorkflow.totalPhases}
                </span>
              )}
              <span className="ml-2 capitalize">- {resumableWorkflow.status}</span>
            </p>
          </div>
        </div>
        <button
          onClick={handleResumeWorkflow}
          disabled={isResuming}
          className={`flex items-center gap-2 px-4 py-2 ${getAccentColors("orange").bgSolid} text-white rounded-md font-medium hover:bg-orange-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
        >
          {isResuming ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              Resuming...
            </>
          ) : (
            <>
              <Play className="w-4 h-4" />
              Continue
            </>
          )}
        </button>
      </div>
    </div>
  );
}
