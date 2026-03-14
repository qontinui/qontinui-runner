/**
 * Shell Command Widget Types
 */

import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { ShellCommandExecution, StepStats } from "../shared/types";

/**
 * Data provided by the useShellCommandData hook.
 */
export interface ShellCommandData {
  /** List of shell command executions */
  commands: ShellCommandExecution[];
  /** Currently running command (if any) */
  currentCommand: ShellCommandExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full ShellCommandWidget component.
 */
export interface ShellCommandWidgetProps extends BaseWidgetProps {
  data: ShellCommandData;
}

/**
 * Props for the ShellCommandSummary component.
 */
export interface ShellCommandSummaryProps extends BaseWidgetProps {
  data: ShellCommandData;
}
