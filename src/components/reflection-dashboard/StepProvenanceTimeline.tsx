/**
 * StepProvenanceTimeline.tsx
 *
 * Timeline visualization of step provenance data, showing which agent
 * generated which step and overlaying UI Bridge element interactions.
 */

import { GitBranch } from "lucide-react";
import { useStepProvenance, type StepProvenance } from "../../hooks/useGraphAnalytics";

interface StepProvenanceTimelineProps {
  workflowId: string;
}

const AGENT_COLORS: Record<string, string> = {
  discovery: "bg-blue-500",
  builder: "bg-green-500",
  fixer: "bg-orange-500",
  hardener: "bg-purple-500",
  verifier: "bg-cyan-500",
};

function getAgentColor(agent: string): string {
  for (const [key, color] of Object.entries(AGENT_COLORS)) {
    if (agent.toLowerCase().includes(key)) return color;
  }
  return "bg-muted-foreground";
}

function ProvenanceStep({ step }: { step: StepProvenance }) {
  let eventCount = 0;
  if (step.ui_bridge_event_ids != null) {
    try {
      eventCount = JSON.parse(step.ui_bridge_event_ids).length;
    } catch {
      eventCount = 0;
    }
  }
  const hasUiBridgeEvents = eventCount > 0;

  return (
    <div className="flex items-start gap-3 py-2">
      {/* Timeline dot */}
      <div className="flex flex-col items-center mt-1">
        <div className={`w-3 h-3 rounded-full ${getAgentColor(step.generating_agent)}`} />
        <div className="w-px h-full bg-border" />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 pb-3">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{step.step_name}</span>
          <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
            {step.phase}
          </span>
          {step.step_index >= 0 && (
            <span className="text-xs text-muted-foreground">#{step.step_index}</span>
          )}
        </div>
        <div className="flex items-center gap-2 mt-0.5">
          <span className="text-xs text-muted-foreground">
            by <span className="font-mono">{step.generating_agent}</span>
          </span>
          {step.generation_iteration != null && (
            <span className="text-xs text-muted-foreground">iter {step.generation_iteration}</span>
          )}
          {hasUiBridgeEvents && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-primary/10 text-primary">
              {eventCount} UI interaction{eventCount !== 1 ? "s" : ""}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

export function StepProvenanceTimeline({ workflowId }: StepProvenanceTimelineProps) {
  const { data: steps, isLoading, error } = useStepProvenance(workflowId);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <GitBranch className="w-4 h-4" />
        Step Provenance Timeline
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">Loading provenance...</div>
      )}

      {error && (
        <div className="text-center text-destructive py-8">
          Failed to load provenance: {String(error)}
        </div>
      )}

      {steps && steps.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          No step provenance data for this workflow
        </div>
      )}

      {steps && steps.length > 0 && (
        <div className="max-h-96 overflow-y-auto">
          {steps.map((step) => (
            <ProvenanceStep key={step.id} step={step} />
          ))}
        </div>
      )}

      {/* Legend */}
      {steps && steps.length > 0 && (
        <div className="flex flex-wrap gap-3 mt-3 pt-3 border-t border-border">
          {Object.entries(AGENT_COLORS).map(([agent, color]) => (
            <div key={agent} className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <div className={`w-2 h-2 rounded-full ${color}`} />
              {agent}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
