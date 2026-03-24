/**
 * DashboardLayout Component
 *
 * Manages the layout of active and summary widgets.
 * Active widget takes 65%, summaries stack on the right (35%).
 */

import { useCallback, useMemo, memo, useState, useRef, useEffect } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { cn } from "../../lib/utils";
import { Pin, EyeOff } from "lucide-react";
import type { ActivityType, ActivityStatus } from "../../types/dashboard/activity-types";
import type { DashboardLayoutState } from "../../hooks/dashboard/useDashboardLayout";
import type { UseWidgetPreferencesResult } from "../../hooks/dashboard/useWidgetPreferences";
import { widgetRegistry, SUMMARY_WIDTH_MIN } from "../../types/dashboard/widget-registry";
import { WidgetHeader } from "./WidgetHeader";
import { IdleState } from "./IdleState";

import { useSessionState } from "../../hooks/useSessionState";

// Import hooks directly to call them statically (avoids dynamic hook issues)
import { useGuiAutomationData } from "@/components/widgets/gui-automation";
import { usePlaywrightData } from "@/components/widgets/playwright";
import { useAiConversationData } from "@/components/widgets/ai-conversation";
import { useVerificationData } from "@/components/widgets/verification";
import { useFindingsData } from "@/components/widgets/findings";
import { useExecutionStatusWidgetData } from "@/components/widgets/execution-status";
import { useShellCommandData } from "@/components/widgets/shell-command";
import { useApiRequestData } from "@/components/widgets/api-request";
import { usePlaywrightTestData } from "@/components/widgets/playwright-test";
import { useWorkflowRefData } from "@/components/widgets/workflow-ref";
import { useMcpCallData } from "@/components/widgets/mcp-call";
import { useExecutionTimelineData } from "@/components/widgets/execution-timeline";
import { useFlowExecutionData } from "@/components/widgets/flow-execution";
import { useCanvasData } from "@/components/widgets/canvas";

// Standalone widgets (not registry-based — they auto-show when events arrive)
import { ConvergenceWidget } from "@/components/widgets/convergence";
import { ConstraintResultsWidget } from "@/components/widgets/constraints";
import { AiStreamWidget } from "@/components/widgets/ai-stream";

/**
 * Props for DashboardLayout.
 */
interface DashboardLayoutProps {
  /** Current layout state */
  layout: DashboardLayoutState;
  /** Callback when a summary widget is clicked */
  onWidgetClick: (type: ActivityType) => void;
  /** Callback to navigate to detail page */
  onNavigateToDetail: (type: ActivityType) => void;
  /** Callback to go to recap page */
  onGoToRecap?: () => void;
  /** Callback to run the last workflow again */
  onRunLastWorkflow?: () => void;
  /** Whether the last workflow is currently being started */
  isRunningLastWorkflow?: boolean;
  /** Name of the last run workflow */
  lastRunWorkflowName?: string | null;
  /** ID of the last run workflow */
  lastRunWorkflowId?: string | null;
  /** Widget pin/hide preferences */
  widgetPreferences?: UseWidgetPreferencesResult;
}

/**
 * Get accent color classes for widget borders.
 */
function getWidgetBorderClasses(
  accentColor: string,
  status: ActivityStatus,
  isActive: boolean,
): string {
  const colorMap: Record<string, { running: string; idle: string }> = {
    blue: {
      running: "border-blue-500 ring-2 ring-blue-500/20",
      idle: "border-blue-500/30",
    },
    purple: {
      running: "border-purple-500 ring-2 ring-purple-500/20",
      idle: "border-purple-500/30",
    },
    green: {
      running: "border-green-500 ring-2 ring-green-500/20",
      idle: "border-green-500/30",
    },
    teal: {
      running: "border-teal-500 ring-2 ring-teal-500/20",
      idle: "border-teal-500/30",
    },
    amber: {
      running: "border-amber-500 ring-2 ring-amber-500/20",
      idle: "border-amber-500/30",
    },
    cyan: {
      running: "border-cyan-500 ring-2 ring-cyan-500/20",
      idle: "border-cyan-500/30",
    },
    slate: {
      running: "border-slate-500 ring-2 ring-slate-500/20",
      idle: "border-slate-500/30",
    },
    orange: {
      running: "border-orange-500 ring-2 ring-orange-500/20",
      idle: "border-orange-500/30",
    },
    indigo: {
      running: "border-indigo-500 ring-2 ring-indigo-500/20",
      idle: "border-indigo-500/30",
    },
    pink: {
      running: "border-pink-500 ring-2 ring-pink-500/20",
      idle: "border-pink-500/30",
    },
    emerald: {
      running: "border-emerald-500 ring-2 ring-emerald-500/20",
      idle: "border-emerald-500/30",
    },
    violet: {
      running: "border-violet-500 ring-2 ring-violet-500/20",
      idle: "border-violet-500/30",
    },
  };

  const colors = colorMap[accentColor] ?? colorMap.blue;

  if (status === "running" && isActive) {
    return colors.running;
  }

  return isActive ? colors.idle : "border-border";
}

/**
 * Props for type-specific widget renderers.
 */
interface WidgetRendererProps {
  status: ActivityStatus;
  onNavigateToDetail: () => void;
}

/**
 * GUI Automation widget renderer - calls useGuiAutomationData statically.
 */
const GuiAutomationRenderer = memo(function GuiAutomationRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("gui_automation")!;
  const data = useGuiAutomationData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Playwright widget renderer - calls usePlaywrightData statically.
 */
const PlaywrightRenderer = memo(function PlaywrightRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("playwright")!;
  const data = usePlaywrightData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Copy button for AI Conversation widget header.
 */
function CopyConversationButton({ entries }: { entries: { line: string }[] }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    if (entries.length === 0) return;
    const text = entries.map((e) => e.line).join("\n");
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [entries]);

  if (entries.length === 0) return null;

  return (
    <button
      onClick={handleCopy}
      className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors rounded focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1"
      aria-label="Copy conversation to clipboard"
      title="Copy conversation"
    >
      {copied ? (
        <>
          <svg
            className="w-3.5 h-3.5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <polyline points="20 6 9 17 4 12" />
          </svg>
          <span>Copied</span>
        </>
      ) : (
        <>
          <svg
            className="w-3.5 h-3.5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
          </svg>
          <span>Copy</span>
        </>
      )}
    </button>
  );
}

/**
 * Session state indicator for the AI Conversation widget header.
 * Shows a colored dot with label reflecting the current Claude session state.
 * Uses unfiltered session state since the Rust executor's task_run_id may
 * differ from the dashboard's task run ID.
 */
function SessionStateIndicator() {
  const { state, isActive } = useSessionState();

  if (!isActive || !state) return null;

  const config: Record<string, { color: string; label: string }> = {
    created: { color: "bg-zinc-400", label: "Starting" },
    initializing: { color: "bg-amber-400 animate-pulse", label: "Initializing" },
    ready: { color: "bg-emerald-500", label: "Ready" },
    processing: { color: "bg-blue-500 animate-pulse", label: "Processing" },
    interrupting: { color: "bg-amber-500 animate-pulse", label: "Interrupting" },
    closing: { color: "bg-zinc-400", label: "Closing" },
  };

  const { color, label } = config[state] ?? { color: "bg-zinc-400", label: state };

  return (
    <div className="flex items-center gap-1.5" title={`Session: ${label}`}>
      <div className={cn("w-2 h-2 rounded-full", color)} />
      <span className="text-[10px] text-muted-foreground font-medium">{label}</span>
    </div>
  );
}

/**
 * AI Conversation widget renderer - calls useAiConversationData statically.
 */
const AiConversationRenderer = memo(function AiConversationRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("ai_conversation")!;
  const data = useAiConversationData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
        actions={
          <div className="flex items-center gap-3">
            <SessionStateIndicator />
            <CopyConversationButton entries={data.entries} />
          </div>
        }
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Verification widget renderer - calls useVerificationData statically.
 */
const VerificationRenderer = memo(function VerificationRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("verification")!;
  const data = useVerificationData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Findings widget renderer - calls useFindingsData statically.
 */
const FindingsRenderer = memo(function FindingsRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("findings")!;
  const data = useFindingsData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Execution Status widget renderer - calls useExecutionStatusWidgetData statically.
 */
const ExecutionStatusRenderer = memo(function ExecutionStatusRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("execution_status")!;
  const data = useExecutionStatusWidgetData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Shell Command widget renderer - calls useShellCommandData statically.
 */
const ShellCommandRenderer = memo(function ShellCommandRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("shell_command")!;
  const data = useShellCommandData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * API Request widget renderer - calls useApiRequestData statically.
 */
const ApiRequestRenderer = memo(function ApiRequestRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("api_request")!;
  const data = useApiRequestData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Playwright Test widget renderer - calls usePlaywrightTestData statically.
 */
const ScriptRenderer = memo(function ScriptRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("script")!;
  const data = usePlaywrightTestData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Workflow Ref widget renderer - calls useWorkflowRefData statically.
 */
const WorkflowRefRenderer = memo(function WorkflowRefRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("workflow_ref")!;
  const data = useWorkflowRefData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * MCP Call widget renderer - calls useMcpCallData statically.
 */
const McpCallRenderer = memo(function McpCallRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("mcp_call")!;
  const data = useMcpCallData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Execution Timeline widget renderer - calls useExecutionTimelineData statically.
 */
const ExecutionTimelineRenderer = memo(function ExecutionTimelineRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("execution_timeline")!;
  const data = useExecutionTimelineData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Flow Execution widget renderer - calls useFlowExecutionData statically.
 */
const FlowExecutionRenderer = memo(function FlowExecutionRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("flow_execution")!;
  const data = useFlowExecutionData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Canvas widget renderer - calls useCanvasData statically.
 */
const CanvasRenderer = memo(function CanvasRenderer({
  status,
  onNavigateToDetail,
}: WidgetRendererProps) {
  const config = widgetRegistry.get("canvas")!;
  const data = useCanvasData();
  const FullComponent = config.FullComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, true);

  return (
    <div
      className={cn(
        "h-full rounded-xl border-2 overflow-hidden bg-background",
        "transition-colors duration-200",
        borderClasses,
      )}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={true}
        detailRoute={config.detailRoute}
        onViewAll={onNavigateToDetail}
      />
      <div className="h-[calc(100%-48px)] overflow-hidden">
        <FullComponent
          isActive={true}
          isSummary={false}
          status={status}
          data={data}
          onNavigateToDetail={onNavigateToDetail}
        />
      </div>
    </div>
  );
});

/**
 * Active widget dispatcher - renders the correct type-specific component.
 * Each type has its own component with static hook calls.
 * Memoized to prevent unnecessary re-renders when parent state changes.
 */
const ActiveWidget = memo(function ActiveWidget({
  type,
  status,
  onNavigateToDetail,
}: {
  type: ActivityType;
  status: ActivityStatus;
  onNavigateToDetail: () => void;
}) {
  // Dispatch to type-specific renderer - each has its own static hooks
  switch (type) {
    case "gui_automation":
      return <GuiAutomationRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "playwright":
      return <PlaywrightRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "ai_conversation":
      return <AiConversationRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "verification":
      return <VerificationRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "findings":
      return <FindingsRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "execution_status":
      return <ExecutionStatusRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "shell_command":
      return <ShellCommandRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "api_request":
      return <ApiRequestRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "script":
      return <ScriptRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "workflow_ref":
      return <WorkflowRefRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "mcp_call":
      return <McpCallRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "execution_timeline":
      return <ExecutionTimelineRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "flow_execution":
      return <FlowExecutionRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    case "canvas":
      return <CanvasRenderer status={status} onNavigateToDetail={onNavigateToDetail} />;
    default:
      return (
        <div className="flex items-center justify-center h-full text-muted-foreground">
          Widget not found: {type}
        </div>
      );
  }
});

/**
 * Props for type-specific summary widget renderers.
 */
interface SummaryRendererProps {
  status: ActivityStatus;
  onClick: () => void;
  onNavigateToDetail: () => void;
  onPin?: () => void;
  onHide?: () => void;
  isPinned?: boolean;
}

/**
 * Helper to render summary widget container.
 */
function SummaryContainer({
  config,
  status,
  onClick,
  onNavigateToDetail: _onNavigateToDetail,
  data,
  onPin,
  onHide,
  isPinned,
}: SummaryRendererProps & {
  config: NonNullable<ReturnType<typeof widgetRegistry.get>>;
  data: unknown;
}) {
  const SummaryComponent = config.SummaryComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, false);

  // Compute width tier from live data for grid layout
  const tier = config.getSummaryWidthTier
    ? config.getSummaryWidthTier(data)
    : "medium";
  const isFull = tier === "full";

  return (
    <div
      style={{
        gridColumn: isFull ? "1 / -1" : undefined,
        minWidth: isFull ? undefined : SUMMARY_WIDTH_MIN[tier],
      }}
      className="transition-all duration-300 ease-in-out"
    >
      <div
        className={cn(
          "relative group rounded-lg border overflow-hidden bg-background cursor-pointer",
          "hover:border-foreground/30 transition-colors duration-200",
          "focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
          borderClasses,
        )}
        onClick={onClick}
        role="button"
        tabIndex={0}
        aria-label={`Switch to ${config.displayName} widget`}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onClick();
          }
        }}
      >
        {/* Pin/Hide controls - appear on hover */}
        {(onPin || onHide) && (
          <div className="absolute top-1 right-1 hidden group-hover:flex gap-0.5 z-10">
            {onPin && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onPin();
                }}
                className={cn(
                  "p-0.5 rounded hover:bg-muted focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50",
                  isPinned && "text-cyan-500",
                )}
                aria-label={isPinned ? "Unpin widget" : "Pin widget"}
                title={isPinned ? "Unpin widget" : "Pin widget"}
              >
                <Pin
                  className={cn("h-3 w-3", isPinned ? "text-cyan-500" : "text-muted-foreground")}
                />
              </button>
            )}
            {onHide && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onHide();
                }}
                className="p-0.5 rounded hover:bg-muted focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50"
                aria-label="Hide widget"
                title="Hide widget"
              >
                <EyeOff className="h-3 w-3 text-muted-foreground" />
              </button>
            )}
          </div>
        )}
        <WidgetHeader
          title={config.displayName}
          icon={config.icon}
          accentColor={config.accentColor}
          status={status}
          isActive={false}
          compact={true}
          onViewAll={onClick}
        />
        <div className="p-3">
          <SummaryComponent isActive={false} isSummary={true} status={status} data={data} />
        </div>
      </div>
    </div>
  );
}

/**
 * GUI Automation summary renderer.
 */
const GuiAutomationSummaryRenderer = memo(function GuiAutomationSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("gui_automation")!;
  const data = useGuiAutomationData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Playwright summary renderer.
 */
const PlaywrightSummaryRenderer = memo(function PlaywrightSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("playwright")!;
  const data = usePlaywrightData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * AI Conversation summary renderer.
 */
const AiConversationSummaryRenderer = memo(function AiConversationSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("ai_conversation")!;
  const data = useAiConversationData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Verification summary renderer.
 */
const VerificationSummaryRenderer = memo(function VerificationSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("verification")!;
  const data = useVerificationData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Findings summary renderer.
 */
const FindingsSummaryRenderer = memo(function FindingsSummaryRenderer(props: SummaryRendererProps) {
  const config = widgetRegistry.get("findings")!;
  const data = useFindingsData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Execution Status summary renderer.
 */
const ExecutionStatusSummaryRenderer = memo(function ExecutionStatusSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("execution_status")!;
  const data = useExecutionStatusWidgetData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Shell Command summary renderer.
 */
const ShellCommandSummaryRenderer = memo(function ShellCommandSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("shell_command")!;
  const data = useShellCommandData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * API Request summary renderer.
 */
const ApiRequestSummaryRenderer = memo(function ApiRequestSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("api_request")!;
  const data = useApiRequestData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Playwright Test summary renderer.
 */
const PlaywrightTestSummaryRenderer = memo(function PlaywrightTestSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("script")!;
  const data = usePlaywrightTestData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Workflow Ref summary renderer.
 */
const WorkflowRefSummaryRenderer = memo(function WorkflowRefSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("workflow_ref")!;
  const data = useWorkflowRefData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * MCP Call summary renderer.
 */
const McpCallSummaryRenderer = memo(function McpCallSummaryRenderer(props: SummaryRendererProps) {
  const config = widgetRegistry.get("mcp_call")!;
  const data = useMcpCallData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Execution Timeline summary renderer.
 */
const ExecutionTimelineSummaryRenderer = memo(function ExecutionTimelineSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("execution_timeline")!;
  const data = useExecutionTimelineData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Flow Execution summary renderer.
 */
const FlowExecutionSummaryRenderer = memo(function FlowExecutionSummaryRenderer(
  props: SummaryRendererProps,
) {
  const config = widgetRegistry.get("flow_execution")!;
  const data = useFlowExecutionData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Canvas summary renderer.
 */
const CanvasSummaryRenderer = memo(function CanvasSummaryRenderer(props: SummaryRendererProps) {
  const config = widgetRegistry.get("canvas")!;
  const data = useCanvasData();
  return <SummaryContainer {...props} config={config} data={data} />;
});

/**
 * Summary widget dispatcher - renders the correct type-specific component.
 * Memoized to prevent unnecessary re-renders when parent state changes.
 */
const SummaryWidget = memo(function SummaryWidget({
  type,
  status,
  onClick,
  onNavigateToDetail,
  onPin,
  onHide,
  isPinned,
}: {
  type: ActivityType;
  status: ActivityStatus;
  onClick: () => void;
  onNavigateToDetail: () => void;
  onPin?: () => void;
  onHide?: () => void;
  isPinned?: boolean;
}) {
  const sharedProps: SummaryRendererProps = {
    status,
    onClick,
    onNavigateToDetail,
    onPin,
    onHide,
    isPinned,
  };

  switch (type) {
    case "gui_automation":
      return <GuiAutomationSummaryRenderer {...sharedProps} />;
    case "playwright":
      return <PlaywrightSummaryRenderer {...sharedProps} />;
    case "ai_conversation":
      return <AiConversationSummaryRenderer {...sharedProps} />;
    case "verification":
      return <VerificationSummaryRenderer {...sharedProps} />;
    case "findings":
      return <FindingsSummaryRenderer {...sharedProps} />;
    case "execution_status":
      return <ExecutionStatusSummaryRenderer {...sharedProps} />;
    case "shell_command":
      return <ShellCommandSummaryRenderer {...sharedProps} />;
    case "api_request":
      return <ApiRequestSummaryRenderer {...sharedProps} />;
    case "script":
      return <PlaywrightTestSummaryRenderer {...sharedProps} />;
    case "workflow_ref":
      return <WorkflowRefSummaryRenderer {...sharedProps} />;
    case "mcp_call":
      return <McpCallSummaryRenderer {...sharedProps} />;
    case "execution_timeline":
      return <ExecutionTimelineSummaryRenderer {...sharedProps} />;
    case "flow_execution":
      return <FlowExecutionSummaryRenderer {...sharedProps} />;
    case "canvas":
      return <CanvasSummaryRenderer {...sharedProps} />;
    default:
      return null;
  }
});

/**
 * DashboardLayout component.
 */
export function DashboardLayout({
  layout,
  onWidgetClick,
  onNavigateToDetail,
  onGoToRecap,
  onRunLastWorkflow,
  isRunningLastWorkflow,
  lastRunWorkflowName,
  lastRunWorkflowId,
  widgetPreferences,
}: DashboardLayoutProps) {
  const {
    activeWidget: rawActiveWidget,
    summaryWidgets: _rawSummaryWidgets,
    activities,
    isIdle,
    detectedWidgets: rawDetectedWidgets,
  } = layout;

  // Apply widget preferences (hide/pin filtering)
  const detectedWidgets = useMemo(() => {
    if (!widgetPreferences) return rawDetectedWidgets;
    return widgetPreferences.applyPreferences(rawDetectedWidgets);
  }, [rawDetectedWidgets, widgetPreferences]);

  // Ensure active widget isn't hidden; fall back to first visible widget
  const activeWidget = useMemo(() => {
    if (rawActiveWidget && detectedWidgets.includes(rawActiveWidget)) return rawActiveWidget;
    return detectedWidgets.length > 0 ? detectedWidgets[0] : null;
  }, [rawActiveWidget, detectedWidgets]);

  // Summary widgets: all visible widgets except the active one
  const summaryWidgets = useMemo(() => {
    return detectedWidgets.filter((type) => type !== activeWidget);
  }, [detectedWidgets, activeWidget]);

  // Get status for a widget
  const getStatus = useCallback(
    (type: ActivityType): ActivityStatus => {
      return activities.get(type)?.status ?? "idle";
    },
    [activities],
  );

  // Find the currently running activity (if any)
  const runningWidget = useMemo(() => {
    for (const [type, state] of activities) {
      if (state.status === "running") {
        return type;
      }
    }
    return null;
  }, [activities]);

  // Check if user has switched away from the running widget
  const showRestoreButton = runningWidget && activeWidget !== runningWidget;

  // Handle navigation to detail
  const handleNavigateToDetail = useCallback(
    (type: ActivityType) => {
      onNavigateToDetail(type);
    },
    [onNavigateToDetail],
  );

  // Handle restore to running widget
  const handleRestoreToRunning = useCallback(() => {
    if (runningWidget) {
      onWidgetClick(runningWidget);
    }
  }, [runningWidget, onWidgetClick]);

  // Tabbed overview for Timeline + Flow (#13)
  const showTimelineFlowTabs =
    (activeWidget === "execution_timeline" || activeWidget === "flow_execution") &&
    detectedWidgets.includes("execution_timeline") &&
    detectedWidgets.includes("flow_execution");

  // Resizable panel state
  const STORAGE_KEY = "qontinui-dashboard-panel-width";
  const MIN_WIDTH = 30;
  const MAX_WIDTH = 80;
  const HANDLE_WIDTH = 8;

  const [leftWidthPercent, setLeftWidthPercent] = useState<number>(() => {
    const saved = instanceStorage.getItem(STORAGE_KEY);
    if (saved) {
      const parsed = parseFloat(saved);
      if (!isNaN(parsed) && parsed >= MIN_WIDTH && parsed <= MAX_WIDTH) return parsed;
    }
    return 65;
  });

  const containerRef = useRef<HTMLDivElement>(null);
  const isDragging = useRef(false);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isDragging.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDragging.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const pct = (x / rect.width) * 100;
      const clamped = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, pct));
      setLeftWidthPercent(clamped);
    };

    const handleMouseUp = () => {
      if (!isDragging.current) return;
      isDragging.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, []);

  // Persist width to instanceStorage
  useEffect(() => {
    instanceStorage.setItem(STORAGE_KEY, String(leftWidthPercent));
  }, [leftWidthPercent]);

  // Show idle state if no widgets detected
  if (detectedWidgets.length === 0 || isIdle) {
    return (
      <IdleState
        onGoToRecap={onGoToRecap}
        onRunLastWorkflow={onRunLastWorkflow}
        isRunningLastWorkflow={isRunningLastWorkflow}
        lastRunWorkflowName={lastRunWorkflowName}
        lastRunWorkflowId={lastRunWorkflowId}
      />
    );
  }

  // Single widget case - show full width
  if (detectedWidgets.length === 1 && activeWidget) {
    return (
      <div className="h-full p-4">
        <div key={activeWidget} className="h-full animate-in fade-in duration-200">
          <ActiveWidget
            type={activeWidget}
            status={getStatus(activeWidget)}
            onNavigateToDetail={() => handleNavigateToDetail(activeWidget)}
          />
        </div>
      </div>
    );
  }

  // Multiple widgets - resizable active + summaries
  return (
    <div ref={containerRef} className="flex h-full p-4" style={{ gap: 0 }}>
      {/* Active Widget - resizable left panel */}
      {activeWidget && (
        <div style={{ width: `${leftWidthPercent}%`, flexShrink: 0 }} className="pr-1">
          <div className="h-full flex flex-col">
            {/* Tabbed header for Timeline + Flow (#13) */}
            {showTimelineFlowTabs && (
              <div
                className="flex border-b border-border bg-muted/10 px-2 rounded-t-xl"
                role="tablist"
              >
                <button
                  className={cn(
                    "px-3 py-1.5 text-xs font-medium border-b-2 transition-colors focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50",
                    activeWidget === "execution_timeline"
                      ? "border-cyan-500 text-foreground"
                      : "border-transparent text-muted-foreground hover:text-foreground",
                  )}
                  onClick={() => onWidgetClick("execution_timeline")}
                  role="tab"
                  aria-selected={activeWidget === "execution_timeline"}
                >
                  Timeline
                </button>
                <button
                  className={cn(
                    "px-3 py-1.5 text-xs font-medium border-b-2 transition-colors focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50",
                    activeWidget === "flow_execution"
                      ? "border-rose-500 text-foreground"
                      : "border-transparent text-muted-foreground hover:text-foreground",
                  )}
                  onClick={() => onWidgetClick("flow_execution")}
                  role="tab"
                  aria-selected={activeWidget === "flow_execution"}
                >
                  Flow
                </button>
              </div>
            )}
            <div
              key={activeWidget}
              className={cn(
                "flex-1 min-h-0 animate-in fade-in duration-200",
                showTimelineFlowTabs && "rounded-t-none",
              )}
            >
              <ActiveWidget
                type={activeWidget}
                status={getStatus(activeWidget)}
                onNavigateToDetail={() => handleNavigateToDetail(activeWidget)}
              />
            </div>
          </div>
        </div>
      )}

      {/* Drag Handle (#15) - improved discoverability */}
      <div
        onMouseDown={handleMouseDown}
        onDoubleClick={() => setLeftWidthPercent(65)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") setLeftWidthPercent(65);
        }}
        role="separator"
        aria-label="Resize panel. Double-click or press Enter to reset."
        tabIndex={0}
        className="shrink-0 flex items-center justify-center cursor-col-resize group hover:bg-muted/50 transition-colors rounded"
        style={{ width: HANDLE_WIDTH }}
        title="Drag to resize. Double-click to reset."
      >
        <div className="flex flex-col items-center justify-center gap-0.5">
          <div className="w-4 h-px bg-muted-foreground/40 group-hover:bg-muted-foreground/70 transition-colors" />
          <div className="w-4 h-px bg-muted-foreground/40 group-hover:bg-muted-foreground/70 transition-colors" />
          <div className="w-4 h-px bg-muted-foreground/40 group-hover:bg-muted-foreground/70 transition-colors" />
        </div>
      </div>

      {/* Summary Widgets - fills remaining space */}
      <div
        className="flex-1 min-w-0 grid gap-3 overflow-y-auto pl-1 auto-rows-min content-start"
        style={{
          containerType: "inline-size",
          gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
        }}
      >
        {/* Restore to Active Process button */}
        {showRestoreButton && (
          <button
            onClick={handleRestoreToRunning}
            className="flex items-center justify-center gap-2 px-3 py-2 text-xs font-medium text-primary bg-primary/10 hover:bg-primary/20 border border-primary/30 rounded-lg transition-colors focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background"
            style={{ gridColumn: "1 / -1" }}
          >
            <span className="w-2 h-2 bg-green-500 rounded-full animate-pulse" />
            Restore to Active Process
          </button>
        )}

        {/* Standalone widgets: convergence tracking, constraint results, and AI output stream */}
        <div style={{ gridColumn: "1 / -1" }}><ConvergenceWidget /></div>
        <div style={{ gridColumn: "1 / -1" }}><ConstraintResultsWidget /></div>
        <div style={{ gridColumn: "1 / -1" }}><AiStreamWidget /></div>

        {summaryWidgets.map((type) => (
          <SummaryWidget
            key={type}
            type={type}
            status={getStatus(type)}
            onClick={() => onWidgetClick(type)}
            onNavigateToDetail={() => handleNavigateToDetail(type)}
            onPin={widgetPreferences ? () => widgetPreferences.togglePin(type) : undefined}
            onHide={widgetPreferences ? () => widgetPreferences.hideWidget(type) : undefined}
            isPinned={widgetPreferences?.isPinned(type)}
          />
        ))}
      </div>
    </div>
  );
}

export default DashboardLayout;
