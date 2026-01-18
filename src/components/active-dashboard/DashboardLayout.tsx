/**
 * DashboardLayout Component
 *
 * Manages the layout of active and summary widgets.
 * Active widget takes 65%, summaries stack on the right (35%).
 */

import { useCallback, memo } from "react";
import { cn } from "../../lib/utils";
import type { ActivityType, ActivityStatus } from "../../types/dashboard/activity-types";
import type { DashboardLayoutState } from "../../hooks/dashboard/useDashboardLayout";
import { widgetRegistry } from "../../types/dashboard/widget-registry";
import { WidgetHeader } from "./WidgetHeader";
import { IdleState } from "./IdleState";

// Import hooks directly to call them statically (avoids dynamic hook issues)
import { useGuiAutomationData } from "./widgets/gui-automation";
import { usePlaywrightData } from "./widgets/playwright";
import { useAiConversationData } from "./widgets/ai-conversation";
import { useVerificationData } from "./widgets/verification";
import { useFindingsData } from "./widgets/findings";
import { useExecutionStatusWidgetData } from "./widgets/execution-status";

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
  /** Callback to go to execute page */
  onGoToExecute: () => void;
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
}

/**
 * Helper to render summary widget container.
 */
function SummaryContainer({
  config,
  status,
  onClick,
  onNavigateToDetail,
  data,
}: SummaryRendererProps & {
  config: NonNullable<ReturnType<typeof widgetRegistry.get>>;
  data: unknown;
}) {
  const SummaryComponent = config.SummaryComponent;
  const borderClasses = getWidgetBorderClasses(config.accentColor, status, false);

  return (
    <div
      className={cn(
        "rounded-lg border overflow-hidden bg-background cursor-pointer",
        "hover:border-foreground/30 transition-colors duration-200",
        borderClasses,
      )}
      onClick={onClick}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") onClick();
      }}
    >
      <WidgetHeader
        title={config.displayName}
        icon={config.icon}
        accentColor={config.accentColor}
        status={status}
        isActive={false}
        compact={true}
        onViewAll={onNavigateToDetail}
      />
      <div className="p-3">
        <SummaryComponent isActive={false} isSummary={true} status={status} data={data} />
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
 * Summary widget dispatcher - renders the correct type-specific component.
 * Memoized to prevent unnecessary re-renders when parent state changes.
 */
const SummaryWidget = memo(function SummaryWidget({
  type,
  status,
  onClick,
  onNavigateToDetail,
}: {
  type: ActivityType;
  status: ActivityStatus;
  onClick: () => void;
  onNavigateToDetail: () => void;
}) {
  switch (type) {
    case "gui_automation":
      return (
        <GuiAutomationSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
    case "playwright":
      return (
        <PlaywrightSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
    case "ai_conversation":
      return (
        <AiConversationSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
    case "verification":
      return (
        <VerificationSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
    case "findings":
      return (
        <FindingsSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
    case "execution_status":
      return (
        <ExecutionStatusSummaryRenderer
          status={status}
          onClick={onClick}
          onNavigateToDetail={onNavigateToDetail}
        />
      );
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
  onGoToExecute,
}: DashboardLayoutProps) {
  const { activeWidget, summaryWidgets, activities, isIdle, detectedWidgets } = layout;

  // Get status for a widget
  const getStatus = useCallback(
    (type: ActivityType): ActivityStatus => {
      return activities.get(type)?.status ?? "idle";
    },
    [activities],
  );

  // Handle navigation to detail
  const handleNavigateToDetail = useCallback(
    (type: ActivityType) => {
      onNavigateToDetail(type);
    },
    [onNavigateToDetail],
  );

  // Show idle state if no widgets detected
  if (detectedWidgets.length === 0 || isIdle) {
    return <IdleState onGoToExecute={onGoToExecute} />;
  }

  // Single widget case - show full width
  if (detectedWidgets.length === 1 && activeWidget) {
    return (
      <div className="h-full p-4">
        <ActiveWidget
          type={activeWidget}
          status={getStatus(activeWidget)}
          onNavigateToDetail={() => handleNavigateToDetail(activeWidget)}
        />
      </div>
    );
  }

  // Multiple widgets - 65% active, 35% summaries
  return (
    <div className="flex h-full gap-4 p-4">
      {/* Active Widget - 65% */}
      {activeWidget && (
        <div className="w-[65%]">
          <ActiveWidget
            type={activeWidget}
            status={getStatus(activeWidget)}
            onNavigateToDetail={() => handleNavigateToDetail(activeWidget)}
          />
        </div>
      )}

      {/* Summary Widgets - 35% */}
      <div className="w-[35%] flex flex-col gap-3 overflow-y-auto">
        {summaryWidgets.map((type) => (
          <SummaryWidget
            key={type}
            type={type}
            status={getStatus(type)}
            onClick={() => onWidgetClick(type)}
            onNavigateToDetail={() => handleNavigateToDetail(type)}
          />
        ))}
      </div>
    </div>
  );
}

export default DashboardLayout;
