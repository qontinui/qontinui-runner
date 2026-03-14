/**
 * Dashboard Widgets
 *
 * Export all widget components and registration function.
 */

// Registration
export { registerAllWidgets, areWidgetsRegistered } from "./register-widgets";

// GUI Automation
export {
  GuiAutomationWidget,
  GuiAutomationSummary,
  useGuiAutomationData,
  type GuiAutomationData,
} from "./gui-automation";

// Playwright
export {
  PlaywrightWidget,
  PlaywrightSummary,
  usePlaywrightData,
  type PlaywrightData,
} from "./playwright";

// AI Conversation
export {
  AiConversationWidget,
  AiConversationSummary,
  useAiConversationData,
  type AiConversationData,
} from "./ai-conversation";

// Verification
export {
  VerificationWidget,
  VerificationSummary,
  useVerificationData,
  type VerificationData,
} from "./verification";

// Findings
export { FindingsWidget, FindingsSummary, useFindingsData, type FindingsData } from "./findings";

// Execution Status
export {
  ExecutionStatusFullWidget,
  ExecutionStatusSummary,
  useExecutionStatusWidgetData,
  type ExecutionStatusWidgetData,
} from "./execution-status";

// Shell Command
export {
  ShellCommandWidget,
  ShellCommandSummary,
  useShellCommandData,
  type ShellCommandData,
} from "./shell-command";

// API Request
export {
  ApiRequestWidget,
  ApiRequestSummary,
  useApiRequestData,
  type ApiRequestData,
} from "./api-request";

// Playwright Test
export {
  PlaywrightTestWidget,
  PlaywrightTestSummary,
  usePlaywrightTestData,
  type PlaywrightTestData,
} from "./playwright-test";

// Workflow Ref
export {
  WorkflowRefWidget,
  WorkflowRefSummary,
  useWorkflowRefData,
  type WorkflowRefData,
} from "./workflow-ref";

// MCP Call
export { McpCallWidget, McpCallSummary, useMcpCallData, type McpCallData } from "./mcp-call";

// Execution Timeline
export {
  ExecutionTimelineWidget,
  ExecutionTimelineSummary,
  useExecutionTimelineData,
  type ExecutionTimelineData,
} from "./execution-timeline";

// Flow Execution
export {
  FlowExecutionWidget,
  FlowExecutionSummary,
  useFlowExecutionData,
  type FlowExecutionData,
} from "./flow-execution";

// Trace Viewer
export {
  TraceViewerWidget,
  TraceViewerSummary,
  TraceViewerDashboardWidget,
  TraceViewerDashboardSummary,
  useTraceViewerData,
  useTraceViewerWidgetData,
  type TraceViewerWidgetData,
} from "./trace-viewer";

// Convergence (standalone widget - not registry-based)
export { ConvergenceWidget } from "./convergence";

// Constraint Results (standalone widget - not registry-based)
export {
  ConstraintResultsWidget,
  useConstraintResults,
  type ConstraintResultsData,
} from "./constraints";

// AI Stream (standalone widget - not registry-based)
export { AiStreamWidget } from "./ai-stream";

// Shared components
export {
  StepStatusBadge,
  StepExecutionList,
  StepOutputPanel,
  StepStatsBar,
  type StepExecution,
  type StepExecutionStatus,
  type StepStats,
} from "./shared";
