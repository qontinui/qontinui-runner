/**
 * StepsTimeline Component
 *
 * Displays a flat list of steps in a timeline format.
 * Used when stages are not available.
 * Uses virtual scrolling for large step lists (50+ items) to maintain performance.
 */

import { useCallback, type CSSProperties } from "react";
import { Activity } from "lucide-react";
import type { RecapStep } from "@/types/recap";
import { StepItem } from "./StepItem";
import { FixedVirtualList } from "../ui";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 50;
/** Height of each step item in pixels (collapsed state) */
const STEP_ITEM_HEIGHT = 56;

interface StepsTimelineProps {
  steps: RecapStep[];
  onAiStepClick?: (phase: string, iteration?: number) => void;
}

export function StepsTimeline({ steps, onAiStepClick }: StepsTimelineProps) {
  // Use virtual scrolling for large lists
  const useVirtualScroll = steps.length > VIRTUAL_SCROLL_THRESHOLD;

  // Render function for virtualized items
  const renderVirtualItem = useCallback(
    (step: RecapStep, index: number, style: CSSProperties) => (
      <div style={style}>
        <StepItem
          key={`${step.name}-${index}`}
          step={step}
          onAiStepClick={
            step.step_type === "ai_session" && onAiStepClick
              ? () => onAiStepClick(step.phase || "agentic")
              : undefined
          }
        />
      </div>
    ),
    [onAiStepClick],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((step: RecapStep, index: number) => `${step.name}-${index}`, []);

  if (steps.length === 0) {
    return (
      <div className="card p-6 text-center text-muted-foreground">
        <Activity className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No steps recorded</p>
      </div>
    );
  }

  return (
    <div className="card">
      <div className="px-4 py-3 border-b border-border">
        <h3 className="text-sm font-medium">
          Steps ({steps.length}){useVirtualScroll && " - virtualized"}
        </h3>
      </div>
      {useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <div className="p-2 h-[400px]">
          <FixedVirtualList
            items={steps}
            itemHeight={STEP_ITEM_HEIGHT}
            renderItem={renderVirtualItem}
            getItemKey={getItemKey}
            overscanCount={10}
          />
        </div>
      ) : (
        /* Regular scrolling for small lists */
        <div className="p-2 space-y-1 max-h-[400px] overflow-y-auto">
          {steps.map((step, index) => (
            <StepItem
              key={`${step.name}-${index}`}
              step={step}
              onAiStepClick={
                step.step_type === "ai_session" && onAiStepClick
                  ? () => onAiStepClick(step.phase || "agentic")
                  : undefined
              }
            />
          ))}
        </div>
      )}
    </div>
  );
}

export default StepsTimeline;
