/**
 * FlowStepNode.tsx
 *
 * Custom node component for React Flow representing a flow step.
 * Shows visual execution state indicators during flow execution.
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
  CheckCircle2,
  XCircle,
  Loader2,
} from "lucide-react";
import type { FlowStep } from "../../types/flow";
import { STEP_TYPE_COLORS, STEP_TYPE_LABELS, getStepTypeName } from "../../types/flow";
import { useStepExecutionStatus } from "./FlowExecutionContext";

export interface FlowStepNodeData {
  step: FlowStep;
  isStart: boolean;
  isSelected: boolean;
  /** @deprecated Use FlowExecutionContext instead - kept for backward compatibility */
  isRunning?: boolean;
  /** @deprecated Use FlowExecutionContext instead - kept for backward compatibility */
  isComplete?: boolean;
  /** @deprecated Use FlowExecutionContext instead - kept for backward compatibility */
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
  const { step, isStart } = data;
  const stepType = getStepTypeName(step.step_type);
  const color = STEP_TYPE_COLORS[stepType] || "#6b7280";
  const label = STEP_TYPE_LABELS[stepType] || stepType;
  const Icon = getStepIcon(stepType);

  // Get execution status from context
  const executionStatus = useStepExecutionStatus(step.id);

  // Merge context status with legacy props (context takes precedence)
  const isRunning = executionStatus.isRunning || data.isRunning;
  const isComplete = executionStatus.isComplete || data.isComplete;
  const hasError = executionStatus.hasError || data.hasError;

  // Determine container style based on execution status
  let containerClasses = "bg-gray-800 rounded-lg shadow-lg min-w-[180px] max-w-[250px] transition-all duration-300";
  let ringStyle = "";
  let glowStyle = "";

  if (isRunning) {
    // Currently executing: emerald/green pulsing glow
    ringStyle = "ring-2 ring-emerald-400";
    glowStyle = "shadow-[0_0_20px_rgba(52,211,153,0.5)]";
    containerClasses += " animate-pulse";
  } else if (hasError) {
    // Failed: red border and glow
    ringStyle = "ring-2 ring-red-500";
    glowStyle = "shadow-[0_0_15px_rgba(239,68,68,0.4)]";
  } else if (isComplete) {
    // Completed successfully: green border
    ringStyle = "ring-2 ring-emerald-500";
  } else if (selected) {
    // Selected but not executing
    ringStyle = "ring-2 ring-blue-500";
  }

  // Start node border
  const startBorder = isStart ? "border-2 border-green-500" : "";

  return (
    <>
      {/* Input Handle - not shown for start step types */}
      {stepType !== "end" && stepType !== "fail" && (
        <Handle type="target" position={Position.Top} className="w-3 h-3 !bg-gray-400" />
      )}

      <div
        className={`${containerClasses} ${ringStyle} ${glowStyle} ${startBorder}`}
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
          {/* Step type icon with execution status overlay */}
          <div className="relative">
            <Icon className="w-4 h-4" style={{ color }} />
            {/* Execution status indicator overlay */}
            {isRunning && (
              <div className="absolute -top-1 -right-1">
                <Loader2 className="w-3 h-3 text-emerald-400 animate-spin" />
              </div>
            )}
            {isComplete && !hasError && (
              <div className="absolute -top-1 -right-1">
                <CheckCircle2 className="w-3 h-3 text-emerald-400" />
              </div>
            )}
            {hasError && (
              <div className="absolute -top-1 -right-1">
                <XCircle className="w-3 h-3 text-red-400" />
              </div>
            )}
          </div>
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

        {/* Execution Status Bar */}
        {(isRunning || isComplete || hasError) && (
          <div className="px-3 pb-2">
            {isRunning && (
              <div className="flex items-center gap-1.5">
                <Loader2 className="w-3 h-3 text-emerald-400 animate-spin" />
                <span className="text-xs text-emerald-400 font-medium">Running...</span>
              </div>
            )}
            {isComplete && !hasError && (
              <div className="flex items-center gap-1.5">
                <CheckCircle2 className="w-3 h-3 text-emerald-400" />
                <span className="text-xs text-emerald-400 font-medium">Complete</span>
                {executionStatus.durationMs !== undefined && (
                  <span className="text-xs text-gray-500 ml-auto">
                    {executionStatus.durationMs >= 1000
                      ? `${(executionStatus.durationMs / 1000).toFixed(1)}s`
                      : `${executionStatus.durationMs}ms`}
                  </span>
                )}
              </div>
            )}
            {hasError && (
              <div className="flex items-center gap-1.5">
                <XCircle className="w-3 h-3 text-red-400" />
                <span className="text-xs text-red-400 font-medium">Failed</span>
              </div>
            )}
            {hasError && executionStatus.error && (
              <p className="text-xs text-red-300/80 mt-1 line-clamp-2" title={executionStatus.error}>
                {executionStatus.error}
              </p>
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
