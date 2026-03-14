/**
 * MCP Call Widget Types
 */

import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { StepExecution, StepStats } from "../shared/types";

/**
 * MCP call specific execution data.
 */
export interface McpCallExecution extends StepExecution {
  /** MCP server ID */
  serverId: string;
  /** MCP server name */
  serverName?: string;
  /** Tool name that was called */
  toolName: string;
  /** Arguments passed to the tool */
  arguments?: Record<string, unknown>;
  /** Result returned by the tool */
  result?: unknown;
  /** Whether the tool returned an error */
  isError?: boolean;
}

/**
 * Data provided by the useMcpCallData hook.
 */
export interface McpCallData {
  /** List of MCP call executions */
  calls: McpCallExecution[];
  /** Currently running call (if any) */
  currentCall: McpCallExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full McpCallWidget component.
 */
export interface McpCallWidgetProps extends BaseWidgetProps {
  data: McpCallData;
}

/**
 * Props for the McpCallSummary component.
 */
export interface McpCallSummaryProps extends BaseWidgetProps {
  data: McpCallData;
}
