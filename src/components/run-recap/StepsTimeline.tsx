/**
 * StepsTimeline Component
 *
 * Displays a flat list of steps in a timeline format.
 * Used when stages are not available.
 */

import { Activity } from "lucide-react";
import type { RecapStep } from "@/types/recap";
import { StepItem } from "./StepItem";

interface StepsTimelineProps {
  steps: RecapStep[];
}

export function StepsTimeline({ steps }: StepsTimelineProps) {
  if (steps.length === 0) {
    return (
      <div className="card p-6 text-center text-muted-foreground">
        <Activity className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No steps recorded</p>
      </div>
    );
  }

  return (
    <div data-ui-id="recap-steps-timeline" className="card">
      <div className="px-4 py-3 border-b border-border">
        <h3 className="text-sm font-medium">Steps ({steps.length})</h3>
      </div>
      <div data-ui-id="recap-steps-list" className="p-2 space-y-1 max-h-[400px] overflow-y-auto">
        {steps.map((step, index) => (
          <StepItem key={`${step.name}-${index}`} step={step} />
        ))}
      </div>
    </div>
  );
}

export default StepsTimeline;
