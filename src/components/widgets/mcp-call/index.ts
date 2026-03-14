/**
 * MCP Call Widget
 *
 * Dashboard widget for displaying MCP (Model Context Protocol) call execution.
 * Shows tool calls, arguments, results, and server information.
 */

export { McpCallWidget } from "./McpCallWidget";
export { McpCallSummary } from "./McpCallSummary";
export { useMcpCallData } from "./useMcpCallData";
export type {
  McpCallData,
  McpCallWidgetProps,
  McpCallSummaryProps,
  McpCallExecution,
} from "./types";
