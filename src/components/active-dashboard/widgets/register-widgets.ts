/**
 * Widget Registration
 *
 * Registers all dashboard widgets with the widget registry.
 * This must be called before using the dashboard.
 */

import { widgetRegistry, defaultDetectors } from "../../../types/dashboard/widget-registry";

// Import GUI Automation widget
import { GuiAutomationWidget, GuiAutomationSummary, useGuiAutomationData } from "./gui-automation";

// Import Playwright widget
import { PlaywrightWidget, PlaywrightSummary, usePlaywrightData } from "./playwright";

// Import AI Conversation widget
import {
  AiConversationWidget,
  AiConversationSummary,
  useAiConversationData,
} from "./ai-conversation";

// Import Verification widget
import { VerificationWidget, VerificationSummary, useVerificationData } from "./verification";

// Import Findings widget
import { FindingsWidget, FindingsSummary, useFindingsData } from "./findings";

// Import Execution Status widget
import {
  ExecutionStatusFullWidget,
  ExecutionStatusSummary,
  useExecutionStatusWidgetData,
} from "./execution-status";

// Import Shell Command widget
import { ShellCommandWidget, ShellCommandSummary, useShellCommandData } from "./shell-command";

// Import API Request widget
import { ApiRequestWidget, ApiRequestSummary, useApiRequestData } from "./api-request";

// Import Playwright Test widget
import {
  PlaywrightTestWidget,
  PlaywrightTestSummary,
  usePlaywrightTestData,
} from "./playwright-test";

// Import Workflow Ref widget
import { WorkflowRefWidget, WorkflowRefSummary, useWorkflowRefData } from "./workflow-ref";

// Import MCP Call widget
import { McpCallWidget, McpCallSummary, useMcpCallData } from "./mcp-call";

// Import Execution Timeline widget
import {
  ExecutionTimelineWidget,
  ExecutionTimelineSummary,
  useExecutionTimelineData,
} from "./execution-timeline";

// Import Flow Execution widget
import { FlowExecutionWidget, FlowExecutionSummary, useFlowExecutionData } from "./flow-execution";

/**
 * Register all dashboard widgets.
 * Call this once at app initialization.
 */
export function registerAllWidgets(): void {
  // Skip if already registered
  if (widgetRegistry.isInitialized()) {
    return;
  }

  // Execution Timeline - high-level overview of all workflow steps
  widgetRegistry.register({
    id: "execution_timeline",
    displayName: "Execution Timeline",
    icon: "ListOrdered",
    accentColor: "cyan",
    FullComponent: ExecutionTimelineWidget,
    SummaryComponent: ExecutionTimelineSummary,
    useData: useExecutionTimelineData,
    detectActivity: defaultDetectors.execution_timeline,
    defaultPriority: 5, // Highest priority - shows first
    detailRoute: "/logs/timeline",
  });

  // Flow Execution - Flow Designer deterministic workflow execution
  widgetRegistry.register({
    id: "flow_execution",
    displayName: "Flow Execution",
    icon: "GitBranch",
    accentColor: "emerald",
    FullComponent: FlowExecutionWidget,
    SummaryComponent: FlowExecutionSummary,
    useData: useFlowExecutionData,
    detectActivity: defaultDetectors.flow_execution,
    defaultPriority: 8,
    detailRoute: "/flow-designer",
  });

  // GUI Automation - primary widget for GUI automation setup/navigation
  widgetRegistry.register({
    id: "gui_automation",
    displayName: "GUI Automation",
    icon: "Monitor",
    accentColor: "blue",
    FullComponent: GuiAutomationWidget,
    SummaryComponent: GuiAutomationSummary,
    useData: useGuiAutomationData,
    detectActivity: defaultDetectors.gui_automation,
    defaultPriority: 10,
    detailRoute: "/logs/actions",
  });

  // Playwright - for Playwright test execution
  widgetRegistry.register({
    id: "playwright",
    displayName: "Playwright",
    icon: "Globe",
    accentColor: "purple",
    FullComponent: PlaywrightWidget,
    SummaryComponent: PlaywrightSummary,
    useData: usePlaywrightData,
    detectActivity: defaultDetectors.playwright,
    defaultPriority: 15,
    detailRoute: "/logs/playwright",
  });

  // AI Conversation - for AI analysis and chat
  widgetRegistry.register({
    id: "ai_conversation",
    displayName: "AI Conversation",
    icon: "Bot",
    accentColor: "green",
    FullComponent: AiConversationWidget,
    SummaryComponent: AiConversationSummary,
    useData: useAiConversationData,
    detectActivity: defaultDetectors.ai_conversation,
    defaultPriority: 20,
    detailRoute: "/ai-output",
  });

  // Verification - answers "Did the fix work?"
  widgetRegistry.register({
    id: "verification",
    displayName: "Verification",
    icon: "CheckCircle",
    accentColor: "teal",
    FullComponent: VerificationWidget,
    SummaryComponent: VerificationSummary,
    useData: useVerificationData,
    detectActivity: defaultDetectors.verification,
    defaultPriority: 25,
    detailRoute: "/verification",
  });

  // Findings - AI-detected issues
  widgetRegistry.register({
    id: "findings",
    displayName: "Findings",
    icon: "AlertTriangle",
    accentColor: "amber",
    FullComponent: FindingsWidget,
    SummaryComponent: FindingsSummary,
    useData: useFindingsData,
    detectActivity: defaultDetectors.findings,
    defaultPriority: 30,
    detailRoute: "/findings",
  });

  // Execution Status - Real-time agentic feature status
  widgetRegistry.register({
    id: "execution_status",
    displayName: "Execution Status",
    icon: "Activity",
    accentColor: "cyan",
    FullComponent: ExecutionStatusFullWidget,
    SummaryComponent: ExecutionStatusSummary,
    useData: useExecutionStatusWidgetData,
    detectActivity: defaultDetectors.execution_status,
    defaultPriority: 35,
    detailRoute: "/settings/agentic",
  });

  // Shell Command - Shell/terminal command execution
  widgetRegistry.register({
    id: "shell_command",
    displayName: "Shell Command",
    icon: "Terminal",
    accentColor: "slate",
    FullComponent: ShellCommandWidget,
    SummaryComponent: ShellCommandSummary,
    useData: useShellCommandData,
    detectActivity: defaultDetectors.shell_command,
    defaultPriority: 18,
    detailRoute: "/logs/shell",
  });

  // API Request - HTTP API request/response
  widgetRegistry.register({
    id: "api_request",
    displayName: "API Request",
    icon: "Globe2",
    accentColor: "orange",
    FullComponent: ApiRequestWidget,
    SummaryComponent: ApiRequestSummary,
    useData: useApiRequestData,
    detectActivity: defaultDetectors.api_request,
    defaultPriority: 22,
    detailRoute: "/logs/api",
  });

  // Playwright Test - Script execution
  widgetRegistry.register({
    id: "script",
    displayName: "Playwright Test",
    icon: "FileCode",
    accentColor: "indigo",
    FullComponent: PlaywrightTestWidget,
    SummaryComponent: PlaywrightTestSummary,
    useData: usePlaywrightTestData,
    detectActivity: defaultDetectors.script,
    defaultPriority: 19,
    detailRoute: "/logs/scripts",
  });

  // Workflow Ref - Sub-workflow execution
  widgetRegistry.register({
    id: "workflow_ref",
    displayName: "Sub-Workflow",
    icon: "GitBranch",
    accentColor: "pink",
    FullComponent: WorkflowRefWidget,
    SummaryComponent: WorkflowRefSummary,
    useData: useWorkflowRefData,
    detectActivity: defaultDetectors.workflow_ref,
    defaultPriority: 25,
    detailRoute: "/logs/workflows",
  });

  // MCP Call - MCP tool calls to external servers
  widgetRegistry.register({
    id: "mcp_call",
    displayName: "MCP Calls",
    icon: "Plug",
    accentColor: "violet",
    FullComponent: McpCallWidget,
    SummaryComponent: McpCallSummary,
    useData: useMcpCallData,
    detectActivity: defaultDetectors.mcp_call,
    defaultPriority: 24,
    detailRoute: "/logs/mcp",
  });

  // Mark registry as initialized
  widgetRegistry.markInitialized();
}

/**
 * Check if widgets are registered.
 */
export function areWidgetsRegistered(): boolean {
  return widgetRegistry.isInitialized();
}
