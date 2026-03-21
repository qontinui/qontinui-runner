/**
 * GuiAutomationWidget Component
 *
 * Full widget view for GUI automation activity.
 * Displays stats bar, screenshot view, action stream, and image recognition results.
 */

import { useState, useEffect } from "react";
import {
  Clock,
  CheckCircle2,
  XCircle,
  Loader2,
  Activity,
  Target,
  Image as ImageIcon,
  AlertCircle,
  ChevronLeft,
  ChevronRight,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea, Progress, Button } from "@/components/ui";
import type { GuiAutomationWidgetProps, GuiAutomationData } from "./types";
import type {
  ActionItem,
  ActionStatus,
  ImageRecognitionResult,
  ScreenshotInfo,
} from "@/components/active-dashboard/types";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Format elapsed time as "Xm Ys" string.
 */
function formatTime(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m ${secs}s`;
}

/**
 * Status icon for action items.
 */
function StatusIcon({ status }: { status: ActionStatus }) {
  const pendingColors = getStatusColors("pending");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  switch (status) {
    case "running":
      return <Loader2 className={cn("h-4 w-4 animate-spin", pendingColors.text)} />;
    case "success":
      return <CheckCircle2 className={cn("h-4 w-4", successColors.text)} />;
    case "failed":
    case "error":
      return <XCircle className={cn("h-4 w-4", errorColors.text)} />;
    case "timeout":
      return <AlertCircle className={cn("h-4 w-4", pausedColors.text)} />;
    default:
      return <div className="h-4 w-4 rounded-full border-2 border-muted-foreground/50" />;
  }
}

/**
 * Stats bar showing key metrics.
 */
function StatsBar({ stats }: { stats: GuiAutomationData["stats"] }) {
  const successColors = getStatusColors("success");
  const blueColors = getAccentColors("blue");

  return (
    <div className="flex items-center gap-6 border-b border-border px-4 py-3 bg-muted/20">
      {/* Elapsed Time */}
      <div className="flex items-center gap-2">
        <Clock className="h-4 w-4 text-muted-foreground" />
        <div>
          <p className="text-xs text-muted-foreground">Elapsed</p>
          <p className="font-mono text-sm text-foreground">{formatTime(stats.elapsedTime)}</p>
        </div>
      </div>

      {/* Actions Completed */}
      <div className="flex items-center gap-2">
        <Activity className="h-4 w-4 text-muted-foreground" />
        <div>
          <p className="text-xs text-muted-foreground">Actions</p>
          <p className="font-mono text-sm text-foreground">{stats.actionsCompleted}</p>
        </div>
      </div>

      {/* Success Rate */}
      <div className="flex items-center gap-2">
        <CheckCircle2 className={cn("h-4 w-4", successColors.text)} />
        <div>
          <p className="text-xs text-muted-foreground">Success Rate</p>
          <p className={cn("font-mono text-sm", successColors.text)}>
            {stats.successRate.toFixed(1)}%
          </p>
        </div>
      </div>

      {/* Average Confidence */}
      <div className="flex items-center gap-2">
        <Target className={cn("h-4 w-4", blueColors.text)} />
        <div>
          <p className="text-xs text-muted-foreground">Avg Confidence</p>
          <p className={cn("font-mono text-sm", blueColors.text)}>
            {stats.averageConfidence.toFixed(1)}%
          </p>
        </div>
      </div>
    </div>
  );
}

/**
 * Screenshot view - shows the latest screenshot with navigation.
 */
function ScreenshotView({ screenshots }: { screenshots?: ScreenshotInfo[] }) {
  const [currentIndex, setCurrentIndex] = useState(0);
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [imageError, setImageError] = useState(false);

  // Ensure screenshots is always an array - memoize to avoid dependency issues
  const screenshotCount = screenshots?.length ?? 0;

  // Reset to latest screenshot when new ones arrive
  useEffect(() => {
    if (screenshotCount > 0) {
      setCurrentIndex(0); // Most recent is first (sorted by modified desc)
    }
  }, [screenshotCount]);

  // Convert file path to displayable URL
  useEffect(() => {
    const currentScreenshot = screenshots?.[currentIndex];
    if (currentScreenshot?.path) {
      try {
        const src = convertFileSrc(currentScreenshot.path);
        setImageSrc(src);
        setImageError(false);
      } catch {
        setImageError(true);
      }
    } else {
      setImageSrc(null);
    }
  }, [screenshots, currentIndex]);

  const goNext = () => {
    setCurrentIndex((prev) => Math.min(prev + 1, screenshotCount - 1));
  };

  const goPrev = () => {
    setCurrentIndex((prev) => Math.max(prev - 1, 0));
  };

  const currentScreenshot = screenshots?.[currentIndex];

  if (screenshotCount === 0) {
    return (
      <div className="flex-1 border-b border-border bg-muted/10 flex items-center justify-center min-h-[200px]">
        <div className="text-center">
          <ImageIcon className="h-12 w-12 text-muted-foreground/50 mx-auto mb-2" />
          <p className="text-sm text-muted-foreground">No screenshots captured yet</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 border-b border-border bg-black/90 flex flex-col min-h-[200px] relative">
      {/* Screenshot display */}
      <div className="flex-1 flex items-center justify-center overflow-hidden p-2">
        {imageSrc && !imageError ? (
          <img
            src={imageSrc}
            alt={currentScreenshot?.filename || "Screenshot"}
            className="max-h-full max-w-full object-contain"
            onError={() => setImageError(true)}
          />
        ) : (
          <div className="text-center">
            <ImageIcon className="h-12 w-12 text-muted-foreground/50 mx-auto mb-2" />
            <p className="text-sm text-muted-foreground">
              {imageError ? "Failed to load screenshot" : "Loading..."}
            </p>
          </div>
        )}
      </div>

      {/* Navigation controls */}
      {screenshotCount > 1 && (
        <div className="absolute bottom-2 left-1/2 -translate-x-1/2 flex items-center gap-2 bg-black/70 rounded-full px-3 py-1">
          <Button
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 text-white/70 hover:text-white hover:bg-white/10"
            onClick={goPrev}
            disabled={currentIndex === 0}
          >
            <ChevronLeft className="h-4 w-4" />
          </Button>
          <span className="text-xs text-white/70 font-mono">
            {currentIndex + 1} / {screenshotCount}
          </span>
          <Button
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0 text-white/70 hover:text-white hover:bg-white/10"
            onClick={goNext}
            disabled={currentIndex === screenshotCount - 1}
          >
            <ChevronRight className="h-4 w-4" />
          </Button>
        </div>
      )}

      {/* Filename badge */}
      {currentScreenshot && (
        <div className="absolute top-2 left-2">
          <Badge className="bg-black/70 text-white/90 text-xs border-0">
            {currentScreenshot.filename}
          </Badge>
        </div>
      )}
    </div>
  );
}

/**
 * Get action type color mapping using design system.
 */
function getActionTypeColorClass(actionType: string): string {
  const blueColors = getAccentColors("blue");
  const greenColors = getAccentColors("green");
  const purpleColors = getAccentColors("purple");
  const slateColors = getAccentColors("slate");
  const cyanColors = getAccentColors("cyan");

  const colorMap: Record<string, string> = {
    find: cn(blueColors.bg, blueColors.text, "border", blueColors.border),
    FIND: cn(blueColors.bg, blueColors.text, "border", blueColors.border),
    find_all: cn(blueColors.bg, blueColors.text, "border", blueColors.border),
    FIND_ALL: cn(blueColors.bg, blueColors.text, "border", blueColors.border),
    click: cn(greenColors.bg, greenColors.text, "border", greenColors.border),
    CLICK: cn(greenColors.bg, greenColors.text, "border", greenColors.border),
    type: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
    TYPE: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
    wait: cn(slateColors.bg, slateColors.text, "border", slateColors.border),
    WAIT: cn(slateColors.bg, slateColors.text, "border", slateColors.border),
    go_to_state: cn(cyanColors.bg, cyanColors.text, "border", cyanColors.border),
    GO_TO_STATE: cn(cyanColors.bg, cyanColors.text, "border", cyanColors.border),
    // indigo not in design system, using purple as closest
    run_workflow: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
    RUN_WORKFLOW: cn(purpleColors.bg, purpleColors.text, "border", purpleColors.border),
  };

  return colorMap[actionType] || cn(slateColors.bg, slateColors.text, "border", slateColors.border);
}

/**
 * Single action row in the stream.
 */
function ActionRow({ action, isActive }: { action: ActionItem; isActive: boolean }) {
  const relativeTime = action.timestamp
    ? `+${((Date.now() - action.timestamp) / 1000).toFixed(1)}s`
    : "0.0s";

  const colorClass = getActionTypeColorClass(action.action_type);
  const pendingColors = getStatusColors("pending");
  const errorColors = getStatusColors("error");

  return (
    <div
      className={cn(
        "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors hover:bg-muted/30",
        isActive ? cn(pendingColors.border, pendingColors.bg) : "border-transparent",
      )}
      style={{ paddingLeft: `${action.level * 20 + 16}px` }}
    >
      <StatusIcon status={action.status} />

      <span className="w-14 font-mono text-xs text-muted-foreground shrink-0">{relativeTime}</span>

      <Badge className={cn(colorClass, "text-xs shrink-0")}>{action.action_type}</Badge>

      <div className="flex-1 min-w-0">
        <span className="font-mono text-sm text-foreground truncate block">{action.target}</span>
        {action.result && (
          <p className="mt-0.5 text-xs text-muted-foreground truncate">{action.result}</p>
        )}
        {action.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{action.error}</p>
        )}
      </div>

      {action.duration && (
        <span className="font-mono text-xs text-muted-foreground shrink-0">
          {action.duration}ms
        </span>
      )}
    </div>
  );
}

/**
 * Action stream panel.
 */
function ActionStream({
  actions,
  currentAction: _currentAction,
}: {
  actions: ActionItem[];
  currentAction: string | null;
}) {
  return (
    <div className="flex flex-col h-[45%] min-h-[180px]">
      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10">
        <h3 className="text-sm font-semibold text-foreground">Action Stream</h3>
        <Badge variant="muted" className="text-xs">
          {actions.length} action{actions.length !== 1 ? "s" : ""}
        </Badge>
      </div>

      <ScrollArea className="flex-1">
        <div className="flex flex-col-reverse">
          {actions.slice(0, 50).map((action) => (
            <ActionRow key={action.id} action={action} isActive={action.status === "running"} />
          ))}
        </div>
        {actions.length === 0 && (
          <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
            No actions executed yet
          </div>
        )}
      </ScrollArea>
    </div>
  );
}

/**
 * Image recognition result row.
 */
function RecognitionRow({ result }: { result: ImageRecognitionResult }) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  return (
    <div className="flex items-center gap-3 rounded-lg bg-muted/30 p-2.5">
      <div className="h-8 w-8 rounded bg-muted/50 flex items-center justify-center shrink-0">
        <ImageIcon className="h-4 w-4 text-muted-foreground" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <p className="font-mono text-xs text-foreground truncate">{result.template}</p>
          <Badge
            className={cn(
              "text-[10px] shrink-0 border",
              result.found
                ? cn(successColors.bg, successColors.text, successColors.border)
                : cn(errorColors.bg, errorColors.text, errorColors.border),
            )}
          >
            {result.found ? "FOUND" : "NOT FOUND"}
          </Badge>
        </div>
        <div className="mt-1 flex items-center gap-2">
          <Progress value={result.confidence} className="h-1 flex-1" />
          <span className="font-mono text-[10px] text-muted-foreground shrink-0">
            {result.confidence.toFixed(1)}%
          </span>
        </div>
      </div>
    </div>
  );
}

/**
 * Image recognition results panel.
 */
function ImageRecognitionPanel({ results }: { results: ImageRecognitionResult[] }) {
  if (results.length === 0) {
    return null;
  }

  return (
    <div className="border-t border-border">
      <div className="flex items-center justify-between px-4 py-2 bg-muted/10">
        <h3 className="text-sm font-semibold text-foreground">Image Recognition</h3>
        <Badge variant="muted" className="text-xs">
          {results.length} result{results.length !== 1 ? "s" : ""}
        </Badge>
      </div>
      <div className="p-3 space-y-2 max-h-[150px] overflow-y-auto">
        {results.slice(0, 5).map((result) => (
          <RecognitionRow key={result.id || result.template} result={result} />
        ))}
      </div>
    </div>
  );
}

/**
 * Full GUI Automation widget component.
 */
export function GuiAutomationWidget({
  isActive: _isActive,
  isSummary,
  status: _status,
  data,
  onNavigateToDetail: _onNavigateToDetail,
  className,
}: GuiAutomationWidgetProps) {
  // If summary mode, don't render (use GuiAutomationSummary instead)
  if (isSummary) {
    return null;
  }

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StatsBar stats={data.stats} />

      {/* Screenshot View Placeholder */}
      <ScreenshotView screenshots={data.screenshots} />

      {/* Action Stream */}
      <ActionStream actions={data.actions} currentAction={data.currentAction} />

      {/* Image Recognition Results */}
      <ImageRecognitionPanel results={data.imageRecognitionResults} />
    </div>
  );
}

export default GuiAutomationWidget;
