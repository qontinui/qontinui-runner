/**
 * PlaywrightWidget Component
 *
 * Full widget for displaying Playwright test execution status.
 * Shows progress bar, test results list, console output, and screenshots.
 *
 * Note: The widget header is rendered by DashboardLayout, not by this component.
 */

import { useState } from "react";
import {
  CheckCircle,
  XCircle,
  Clock,
  Terminal,
  ChevronDown,
  ChevronRight,
  Loader2,
  SkipForward,
  Image,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea } from "@/components/ui";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { PlaywrightData, TestSpec, TestStatus } from "./types";
import { usePlaywrightDataWithStatus } from "./usePlaywrightData";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Get icon for test status.
 */
function StatusIcon({ status, className }: { status: TestStatus; className?: string }) {
  const iconClass = cn("h-4 w-4", className);
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");
  const pendingColors = getStatusColors("pending");

  switch (status) {
    case "passed":
      return <CheckCircle className={cn(iconClass, successColors.text)} />;
    case "failed":
      return <XCircle className={cn(iconClass, errorColors.text)} />;
    case "skipped":
      return <SkipForward className={cn(iconClass, pausedColors.text)} />;
    case "running":
      return <Loader2 className={cn(iconClass, pendingColors.text, "animate-spin")} />;
    case "pending":
    default:
      return <Clock className={cn(iconClass, "text-muted-foreground")} />;
  }
}

/**
 * Badge variant for test status.
 */
function getStatusBadgeVariant(
  status: TestStatus,
): "success" | "danger" | "warning" | "info" | "muted" {
  switch (status) {
    case "passed":
      return "success";
    case "failed":
      return "danger";
    case "skipped":
      return "warning";
    case "running":
      return "info";
    default:
      return "muted";
  }
}

/**
 * Format duration for display.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * Test result row component.
 */
function TestRow({ test }: { test: TestSpec }) {
  const pendingColors = getStatusColors("pending");
  const errorColors = getStatusColors("error");

  return (
    <div
      className={cn(
        "flex items-center gap-3 px-4 py-2 border-b border-border/50 hover:bg-muted/30 transition-colors",
        test.status === "running" && pendingColors.bg,
        test.status === "failed" && errorColors.bg,
      )}
    >
      <StatusIcon status={test.status} />

      <div className="flex-1 min-w-0">
        <span className="text-sm truncate block">{test.title}</span>
        {test.error && (
          <p className={cn("text-xs truncate mt-0.5", errorColors.text)}>{test.error}</p>
        )}
      </div>

      <div className="flex items-center gap-2 shrink-0">
        <Badge variant={getStatusBadgeVariant(test.status)} size="sm">
          {test.status}
        </Badge>
        {test.durationMs > 0 && (
          <span className="text-xs text-muted-foreground font-mono">
            {formatDuration(test.durationMs)}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * Progress summary bar showing pass/fail/skip breakdown.
 */
function ProgressSummary({ data }: { data: PlaywrightData }) {
  const { totalTests, passedTests, failedTests, skippedTests, isRunning } = data;
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");
  const greenColors = getAccentColors("green");
  const redColors = getAccentColors("red");
  const amberColors = getAccentColors("amber");

  if (totalTests === 0) return null;

  const passedPercent = (passedTests / totalTests) * 100;
  const failedPercent = (failedTests / totalTests) * 100;
  const skippedPercent = (skippedTests / totalTests) * 100;

  return (
    <div className="px-4 py-3 border-b border-border">
      {/* Progress bar with segments */}
      <div className="relative h-2 w-full overflow-hidden rounded-full bg-muted mb-2">
        <div
          className={cn(
            "absolute left-0 top-0 h-full transition-all duration-300",
            greenColors.bgSolid,
          )}
          style={{ width: `${passedPercent}%` }}
        />
        <div
          className={cn("absolute top-0 h-full transition-all duration-300", redColors.bgSolid)}
          style={{
            left: `${passedPercent}%`,
            width: `${failedPercent}%`,
          }}
        />
        <div
          className={cn("absolute top-0 h-full transition-all duration-300", amberColors.bgSolid)}
          style={{
            left: `${passedPercent + failedPercent}%`,
            width: `${skippedPercent}%`,
          }}
        />
      </div>

      {/* Summary counts */}
      <div className="flex items-center justify-between text-xs">
        <div className="flex items-center gap-4">
          <span className="flex items-center gap-1.5">
            <CheckCircle className={cn("h-3 w-3", successColors.text)} />
            <span className={successColors.text}>{passedTests} passed</span>
          </span>
          <span className="flex items-center gap-1.5">
            <XCircle className={cn("h-3 w-3", errorColors.text)} />
            <span className={errorColors.text}>{failedTests} failed</span>
          </span>
          <span className="flex items-center gap-1.5">
            <SkipForward className={cn("h-3 w-3", pausedColors.text)} />
            <span className={pausedColors.text}>{skippedTests} skipped</span>
          </span>
        </div>
        <div className="flex items-center gap-2">
          {isRunning && (
            <Badge variant="info" size="sm" className="animate-pulse">
              Running
            </Badge>
          )}
          <span className="text-muted-foreground font-mono">{formatDuration(data.durationMs)}</span>
        </div>
      </div>
    </div>
  );
}

/**
 * Collapsible console output section.
 */
function ConsoleOutput({
  lines,
  isExpanded,
  onToggle,
}: {
  lines: string[];
  isExpanded: boolean;
  onToggle: () => void;
}) {
  if (lines.length === 0) return null;

  return (
    <div className="border-t border-border">
      <button
        onClick={onToggle}
        className="w-full flex items-center gap-2 px-4 py-2 hover:bg-muted/30 transition-colors"
      >
        {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        <Terminal className="h-4 w-4 text-muted-foreground" />
        <span className="text-sm font-medium">Console Output</span>
        <Badge variant="muted" size="sm">
          {lines.length}
        </Badge>
      </button>

      {isExpanded && (
        <ScrollArea className="max-h-32 bg-muted/20">
          <pre className="px-4 py-2 text-xs font-mono text-muted-foreground whitespace-pre-wrap">
            {lines.join("\n")}
          </pre>
        </ScrollArea>
      )}
    </div>
  );
}

/**
 * Screenshots grid.
 */
function ScreenshotsGrid({ screenshots }: { screenshots: string[] }) {
  if (screenshots.length === 0) return null;

  return (
    <div className="border-t border-border p-4">
      <div className="flex items-center gap-2 mb-2">
        <Image className="h-4 w-4 text-muted-foreground" />
        <span className="text-sm font-medium">Screenshots</span>
        <Badge variant="muted" size="sm">
          {screenshots.length}
        </Badge>
      </div>
      <div className="grid grid-cols-4 gap-2">
        {screenshots.slice(0, 8).map((path, idx) => {
          // Convert file path to Tauri asset URL
          const imageSrc = convertFileSrc(path);
          return (
            <div
              key={path}
              className="aspect-video bg-muted rounded border border-border overflow-hidden"
            >
              <img
                src={imageSrc}
                alt={`Screenshot ${idx + 1}`}
                className="w-full h-full object-cover"
                onError={(e) => {
                  // Hide broken images
                  (e.target as HTMLImageElement).style.display = "none";
                }}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}

/**
 * PlaywrightWidget - Full widget component for active dashboard.
 */
export function PlaywrightWidget({ className }: BaseWidgetProps) {
  const { data, isLoading } = usePlaywrightDataWithStatus();
  const [consoleExpanded, setConsoleExpanded] = useState(false);

  const purpleColors = getAccentColors("purple");

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {isLoading ? (
        <div className="flex items-center justify-center p-8">
          <Loader2 className={cn("h-6 w-6 animate-spin", purpleColors.text)} />
        </div>
      ) : data.tests.length === 0 && !data.isRunning ? (
        <div className="flex flex-col items-center justify-center p-8 text-muted-foreground">
          <Terminal className="h-8 w-8 mb-2" />
          <p className="text-sm">No Playwright tests executed</p>
        </div>
      ) : (
        <>
          <ProgressSummary data={data} />

          <ScrollArea className="flex-1 max-h-64">
            <div className="flex flex-col">
              {data.tests.map((test, idx) => (
                <TestRow key={`${test.title}-${idx}`} test={test} />
              ))}
            </div>
          </ScrollArea>

          <ConsoleOutput
            lines={data.consoleOutput}
            isExpanded={consoleExpanded}
            onToggle={() => setConsoleExpanded(!consoleExpanded)}
          />

          <ScreenshotsGrid screenshots={data.screenshots} />
        </>
      )}
    </div>
  );
}

export default PlaywrightWidget;
