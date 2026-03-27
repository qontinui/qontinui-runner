import { useMemo } from "react";
import { DecisionCard } from "./DecisionCard";
import type { DecisionPreview } from "@/types/decision-trail";

interface TimelineViewProps {
  decisions: DecisionPreview[];
  onSelectDecision: (id: string) => void;
}

interface DayGroup {
  label: string;
  decisions: DecisionPreview[];
}

function groupByDay(decisions: DecisionPreview[]): DayGroup[] {
  const groups = new Map<string, DecisionPreview[]>();

  for (const d of decisions) {
    const date = new Date(d.timestamp);
    const key = date.toLocaleDateString("en-US", {
      weekday: "long",
      month: "long",
      day: "numeric",
      year: "numeric",
    });
    const existing = groups.get(key) || [];
    existing.push(d);
    groups.set(key, existing);
  }

  return Array.from(groups.entries()).map(([label, decisions]) => ({ label, decisions }));
}

export function TimelineView({ decisions, onSelectDecision }: TimelineViewProps) {
  const groups = useMemo(() => groupByDay(decisions), [decisions]);

  if (decisions.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        No decisions yet. Decisions are captured automatically during development.
      </div>
    );
  }

  return (
    <div className="relative pl-6 space-y-6">
      {/* Vertical timeline line */}
      <div className="absolute left-2.5 top-0 bottom-0 w-px bg-border" />

      {groups.map((group) => (
        <div key={group.label}>
          {/* Day marker */}
          <div className="flex items-center gap-3 mb-3 -ml-6">
            <div className="w-5 h-5 rounded-full bg-primary/20 border-2 border-primary flex items-center justify-center z-10">
              <div className="w-1.5 h-1.5 rounded-full bg-primary" />
            </div>
            <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
              {group.label}
            </h3>
          </div>

          {/* Decision cards for this day */}
          <div className="space-y-2 ml-2">
            {group.decisions.map((d) => (
              <div key={d.id} className="relative">
                {/* Connector dot */}
                <div className="absolute -left-[22px] top-4 w-2 h-2 rounded-full bg-border" />
                <DecisionCard decision={d} onClick={onSelectDecision} />
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
