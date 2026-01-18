/**
 * FlowStepNode.tsx
 *
 * Custom node component for React Flow representing a flow step.
 */

import { memo } from "react";
import { Handle, Position } from "@xyflow/react";
import {
  Bot,
  Wrench,
  GitBranch,
  GitMerge,
  User,
  Shuffle,
  Clock,
  Repeat,
  Square,
  XSquare,
  Play,
} from "lucide-react";
import type { FlowStep } from "../../types/flow";
import { STEP_TYPE_COLORS, STEP_TYPE_LABELS, getStepTypeName } from "../../types/flow";

export interface FlowStepNodeData {
  step: FlowStep;
  isStart: boolean;
  isSelected: boolean;
  isRunning?: boolean;
  isComplete?: boolean;
  hasError?: boolean;
}

interface FlowStepNodeProps {
  data: FlowStepNodeData;
  selected?: boolean;
}

function getStepIcon(type: string) {
  switch (type) {
    case "agent":
      return Bot;
    case "tool":
      return Wrench;
    case "conditional":
      return GitBranch;
    case "parallel":
      return GitMerge;
    case "human_input":
      return User;
    case "transform":
      return Shuffle;
    case "wait":
      return Clock;
    case "loop":
      return Repeat;
    case "end":
      return Square;
    case "fail":
      return XSquare;
    default:
      return Play;
  }
}

function FlowStepNodeComponent({ data, selected }: FlowStepNodeProps) {
  const { step, isStart, isRunning, isComplete, hasError } = data;
  const stepType = getStepTypeName(step.step_type);
  const color = STEP_TYPE_COLORS[stepType] || "#6b7280";
  const label = STEP_TYPE_LABELS[stepType] || stepType;
  const Icon = getStepIcon(stepType);

  // Determine status style
  let statusRing = "";
  if (isRunning) statusRing = "ring-2 ring-yellow-500 animate-pulse";
  else if (hasError) statusRing = "ring-2 ring-red-500";
  else if (isComplete) statusRing = "ring-2 ring-green-500";
  else if (selected) statusRing = "ring-2 ring-blue-500";

  // Determine border style for start node
  const startBorder = isStart ? "border-2 border-green-500" : "";

  return (
    <>
      {/* Input Handle - not shown for start step types */}
      {stepType !== "end" && stepType !== "fail" && (
        <Handle type="target" position={Position.Top} className="w-3 h-3 !bg-gray-400" />
      )}

      <div
        className={`
          bg-gray-800 rounded-lg shadow-lg min-w-[180px] max-w-[250px]
          ${statusRing} ${startBorder}
        `}
        style={{
          borderColor: isStart ? undefined : color,
          borderWidth: isStart ? undefined : "2px",
        }}
      >
        {/* Header */}
        <div
          className="flex items-center gap-2 px-3 py-2 rounded-t-lg"
          style={{ backgroundColor: `${color}20` }}
        >
          <Icon className="w-4 h-4" style={{ color }} />
          <span className="text-xs font-medium text-gray-300">{label}</span>
          {isStart && (
            <span className="text-xs bg-green-500/20 text-green-400 px-1.5 py-0.5 rounded ml-auto">
              Start
            </span>
          )}
        </div>

        {/* Content */}
        <div className="px-3 py-2 space-y-1">
          <h4 className="font-medium text-white text-sm truncate">{step.name}</h4>
          {step.description && (
            <p className="text-xs text-gray-400 line-clamp-2">{step.description}</p>
          )}

          {/* Step-specific info */}
          {stepType === "agent" && "role" in step.step_type && (
            <div className="text-xs text-gray-500">
              Role: <span className="text-gray-400">{step.step_type.role}</span>
            </div>
          )}
          {stepType === "tool" && "tool_id" in step.step_type && (
            <div className="text-xs text-gray-500">
              Tool: <span className="text-gray-400">{step.step_type.tool_id}</span>
            </div>
          )}
          {stepType === "wait" && "seconds" in step.step_type && (
            <div className="text-xs text-gray-500">
              Duration: <span className="text-gray-400">{step.step_type.seconds}s</span>
            </div>
          )}

          {/* Retry badge */}
          {step.retry_count > 0 && (
            <span className="text-xs bg-gray-700 text-gray-400 px-1.5 py-0.5 rounded">
              {step.retry_count} retries
            </span>
          )}

          {/* Continue on error badge */}
          {step.continue_on_error && (
            <span className="text-xs bg-yellow-500/20 text-yellow-400 px-1.5 py-0.5 rounded">
              Continue on error
            </span>
          )}
        </div>

        {/* Status indicator */}
        {(isRunning || isComplete || hasError) && (
          <div className="px-3 pb-2">
            {isRunning && (
              <span className="text-xs bg-yellow-500/20 text-yellow-400 px-2 py-0.5 rounded">
                Running...
              </span>
            )}
            {isComplete && !hasError && (
              <span className="text-xs bg-green-500/20 text-green-400 px-2 py-0.5 rounded">
                Complete
              </span>
            )}
            {hasError && (
              <span className="text-xs bg-red-500/20 text-red-400 px-2 py-0.5 rounded">Failed</span>
            )}
          </div>
        )}
      </div>

      {/* Output Handles */}
      {stepType === "conditional" ? (
        <>
          <Handle
            type="source"
            position={Position.Bottom}
            id="then"
            className="w-3 h-3 !bg-green-500"
            style={{ left: "30%" }}
          />
          <Handle
            type="source"
            position={Position.Bottom}
            id="else"
            className="w-3 h-3 !bg-red-500"
            style={{ left: "70%" }}
          />
        </>
      ) : stepType !== "end" && stepType !== "fail" ? (
        <Handle type="source" position={Position.Bottom} className="w-3 h-3 !bg-gray-400" />
      ) : null}
    </>
  );
}

export const FlowStepNode = memo(FlowStepNodeComponent);
