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

// Script
export { ScriptWidget, ScriptSummary, useScriptData, type ScriptData } from "./script";

// Workflow Ref
export {
  WorkflowRefWidget,
  WorkflowRefSummary,
  useWorkflowRefData,
  type WorkflowRefData,
} from "./workflow-ref";

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
