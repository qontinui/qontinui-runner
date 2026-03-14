/**
 * UI Bridge Widget Types
 */

import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { StepStats } from "../shared/types";

/**
 * UI Bridge action types.
 */
export type UiBridgeActionType = "navigate" | "execute" | "assert" | "snapshot" | "unknown";

/**
 * UI Bridge execution data.
 */
export interface UiBridgeExecution {
  id: string;
  name: string;
  status: "pending" | "running" | "success" | "failed" | "skipped" | "unknown";
  startTime?: number;
  endTime?: number;
  durationMs?: number;
  error?: string;
  output?: string;
  /** Action type */
  actionType: UiBridgeActionType;
  /** Target URL or element */
  target?: string;
  /** Command executed */
  command?: string;
}

/**
 * Data provided by the useUiBridgeData hook.
 */
export interface UiBridgeData {
  /** All UI Bridge executions */
  executions: UiBridgeExecution[];
  /** Currently running execution (if any) */
  currentExecution: UiBridgeExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Assertion pass/fail counts */
  assertionStats: { passed: number; failed: number; total: number };
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full UiBridgeWidget component.
 */
export interface UiBridgeWidgetProps extends BaseWidgetProps {
  data: UiBridgeData;
}

/**
 * Props for the UiBridgeSummary component.
 */
export interface UiBridgeSummaryProps extends BaseWidgetProps {
  data: UiBridgeData;
}
