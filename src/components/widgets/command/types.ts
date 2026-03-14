/**
 * Command Widget Types
 */

import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { StepStats } from "../shared/types";

/**
 * Command mode for the unified command step type.
 */
export type CommandMode = "shell" | "check" | "check_group" | "test";

/**
 * Command execution data.
 */
export interface CommandExecution {
  id: string;
  name: string;
  status: "pending" | "running" | "success" | "failed" | "skipped" | "unknown";
  startTime?: number;
  endTime?: number;
  durationMs?: number;
  error?: string;
  output?: string;
  /** Command mode: shell, check, check_group, test */
  mode?: CommandMode;
  /** The command that was executed */
  command: string;
  /** Working directory */
  workingDirectory?: string;
  /** Exit code */
  exitCode?: number;
  /** Standard output */
  stdout?: string;
  /** Standard error */
  stderr?: string;
  /** Original command template (with {{variable}} placeholders) */
  templateCommand?: string;
  /** Variables resolved during execution */
  resolvedVariables?: Record<string, string>;
}

/**
 * Data provided by the useCommandData hook.
 */
export interface CommandData {
  /** All command executions */
  commands: CommandExecution[];
  /** Currently running command (if any) */
  currentCommand: CommandExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Stats broken down by mode */
  statsByMode: Record<string, { total: number; successful: number; failed: number }>;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full CommandWidget component.
 */
export interface CommandWidgetProps extends BaseWidgetProps {
  data: CommandData;
}

/**
 * Props for the CommandSummary component.
 */
export interface CommandSummaryProps extends BaseWidgetProps {
  data: CommandData;
}
