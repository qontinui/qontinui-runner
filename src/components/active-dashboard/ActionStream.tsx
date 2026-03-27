/**
 * ActionStream Component
 *
 * Displays a live stream of actions being executed.
 * Shows action type, status, timing, and hierarchical structure.
 * Uses virtual scrolling for large action lists (100+ items) to maintain performance.
 */

import { useCallback, useEffect, useState, type CSSProperties } from "react";
import { CheckCircle2, XCircle, Loader2, ChevronRight } from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge, ScrollArea, FixedVirtualList } from "../ui";
import type { ActionStreamProps, ActionItem, ActionType, ActionStatus } from "./types";
import { getStatusColors, getAccentColors } from "@/design-system";
import { useVirtualScrollMetrics } from "../../hooks";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 100;
/** Height of each action row in pixels */
const ACTION_ROW_HEIGHT = 64;

/**
 * Get action type color class using design system.
 */
function getActionTypeColorClass(actionType: ActionType): string {
  const blueColors = getAccentColors("blue");
  const greenColors = getAccentColors("green");
  const purpleColors = getAccentColors("purple");
  const slateColors = getAccentColors("slate");
  const cyanColors = getAccentColors("cyan");

  const colorMap: Record<ActionType, string> = {
    FIND: cn(blueColors.bg, blueColors.text, "border", blueColors.border),
    CLICK: cn(greenColors.bg, greenColors.text, "border", greenColors.border),
    TYPE: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
    WAIT: cn(slateColors.bg, slateColors.text, "border", slateColors.border),
    GO_TO_STATE: cn(cyanColors.bg, cyanColors.text, "border", cyanColors.border),
    // indigo not in design system, using purple as closest
    RUN_WORKFLOW: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
  };

  return colorMap[actionType] || cn(slateColors.bg, slateColors.text, "border", slateColors.border);
}

function StatusIcon({ status }: { status: ActionStatus }) {
  const pendingColors = getStatusColors("pending");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  switch (status) {
    case "running":
      return <Loader2 className={cn("h-4 w-4 animate-spin", pendingColors.text)} />;
    case "success":
      return <CheckCircle2 className={cn("h-4 w-4", successColors.text)} />;
    case "failed":
      return <XCircle className={cn("h-4 w-4", errorColors.text)} />;
    default:
      return <div className="h-4 w-4 rounded-full border-2 border-muted-foreground/50" />;
  }
}

function ActionRow({ action, isActive }: { action: ActionItem; isActive: boolean }) {
  const [now] = useState(() => Date.now());
  const relativeTime = action.timestamp
    ? `+${((now - action.timestamp) / 1000).toFixed(1)}s`
    : "0.0s";

  const pendingColors = getStatusColors("pending");
  const errorColors = getStatusColors("error");
  const colorClass = getActionTypeColorClass(action.action_type);

  return (
    <div
      className={cn(
        "flex items-start gap-3 border-l-2 px-4 py-3 transition-colors hover:bg-muted/30",
        isActive ? cn(pendingColors.border, pendingColors.bg) : "border-transparent",
      )}
      style={{ paddingLeft: `${action.level * 24 + 16}px` }}
    >
      <StatusIcon status={action.status} />

      <span className="w-16 font-mono text-xs text-muted-foreground">{relativeTime}</span>

      <Badge className={cn(colorClass, "text-xs")}>{action.action_type}</Badge>

      <div className="flex-1">
        <div className="flex items-center gap-2">
          <span className="font-mono text-sm text-foreground">{action.target}</span>
          {action.children && action.children.length > 0 && (
            <ChevronRight className="h-3 w-3 text-muted-foreground" />
          )}
        </div>
        {action.result && <p className="mt-1 text-xs text-muted-foreground">{action.result}</p>}
        {action.error && <p className={cn("mt-1 text-xs", errorColors.text)}>{action.error}</p>}
      </div>

      {action.duration && (
        <span className="font-mono text-xs text-muted-foreground">{action.duration}ms</span>
      )}
    </div>
  );
}

export function ActionStream({ actions, currentAction: _currentAction }: ActionStreamProps) {
  // Virtual scroll metrics tracking (dev mode only)
  const { updateItemCounts, recordScrollFrame } = useVirtualScrollMetrics();

  // Use virtual scrolling for large lists
  const useVirtualScroll = actions.length > VIRTUAL_SCROLL_THRESHOLD;

  // Report item counts when action list changes
  useEffect(() => {
    const visibleItems = useVirtualScroll
      ? Math.min(actions.length, Math.ceil(400 / ACTION_ROW_HEIGHT))
      : actions.length;
    updateItemCounts(visibleItems, actions.length);
  }, [actions.length, useVirtualScroll, updateItemCounts]);

  // Render function for virtualized items
  const renderVirtualItem = useCallback(
    (action: ActionItem, _index: number, style: CSSProperties) => (
      <div style={style}>
        <ActionRow action={action} isActive={action.status === "running"} />
      </div>
    ),
    [],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((action: ActionItem) => action.id, []);

  // Scroll handler to record scroll frames for virtual scroll metrics
  const handleScroll = useCallback(() => {
    recordScrollFrame();
  }, [recordScrollFrame]);

  return (
    <div className="flex h-[40%] flex-col bg-card">
      <div className="flex items-center justify-between border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-foreground">Action Stream</h3>
        <div className="flex gap-2">
          <Badge variant="muted" className="text-xs">
            {actions.length} actions
            {useVirtualScroll && " (virtualized)"}
          </Badge>
        </div>
      </div>

      {useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <FixedVirtualList
          items={actions}
          itemHeight={ACTION_ROW_HEIGHT}
          renderItem={renderVirtualItem}
          getItemKey={getItemKey}
          reverseOrder={true}
          overscanCount={10}
          onScroll={handleScroll}
        />
      ) : (
        /* Regular scrolling for small lists */
        <ScrollArea className="flex-1">
          <div className="flex flex-col-reverse">
            {actions.map((action) => (
              <ActionRow key={action.id} action={action} isActive={action.status === "running"} />
            ))}
          </div>
        </ScrollArea>
      )}
    </div>
  );
}
