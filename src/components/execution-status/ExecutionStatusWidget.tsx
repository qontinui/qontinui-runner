/**
 * ExecutionStatusWidget Component
 *
 * A compact widget version of the execution status panel that can be
 * embedded in other views. Uses the useExecutionStatus hook internally.
 */

import { useExecutionStatus } from "../../hooks/useExecutionStatus";
import { ExecutionStatusPanel } from "./ExecutionStatusPanel";

interface ExecutionStatusWidgetProps {
  /** Optional className for custom styling */
  className?: string;
}

export function ExecutionStatusWidget({ className = "" }: ExecutionStatusWidgetProps) {
  const { status, clearStatus, isConnected } = useExecutionStatus();

  return (
    <ExecutionStatusPanel
      status={status}
      isConnected={isConnected}
      onClear={clearStatus}
      className={className}
    />
  );
}
