/**
 * AutomationTab Component
 *
 * Displays automation metrics for a task run:
 * - Actions summary with visual bar chart
 * - States visited
 * - Transitions diagram
 * - Template matching results
 * - Screenshots gallery
 */

import { useState, useEffect } from "react";
import {
  Activity,
  CheckCircle2,
  XCircle,
  SkipForward,
  Layers,
  ArrowRight,
  Image,
  Eye,
} from "lucide-react";
import type { RecapStats } from "@/types/recap";

interface AutomationTabProps {
  taskRunId: string;
  stats?: RecapStats;
}

interface ScreenshotInfo {
  path: string;
  timestamp: number;
  description?: string;
}

export function AutomationTab({ taskRunId, stats }: AutomationTabProps) {
  const [screenshots, setScreenshots] = useState<ScreenshotInfo[]>([]);
  const [selectedScreenshot, setSelectedScreenshot] = useState<string | null>(null);

  // Calculate percentages for the bar chart
  const total = stats?.total_actions || 0;
  const successful = stats?.successful_actions || 0;
  const failed = stats?.failed_actions || 0;
  const skipped = stats?.skipped_actions || 0;

  const successPercent = total > 0 ? (successful / total) * 100 : 0;
  const failedPercent = total > 0 ? (failed / total) * 100 : 0;
  const skippedPercent = total > 0 ? (skipped / total) * 100 : 0;

  return (
    <div className="space-y-4">
      {/* Actions Summary */}
      <div data-ui-id="recap-actions-summary" className="card p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-medium flex items-center gap-2">
            <Activity className="w-5 h-5 text-primary" />
            Actions Summary
          </h3>
          <span data-ui-id="recap-actions-total" className="text-sm text-muted-foreground">
            {total} total actions
          </span>
        </div>

        {/* Stats row */}
        <div className="flex items-center gap-4 mb-4">
          <div data-ui-id="recap-actions-success" className="flex items-center gap-2">
            <CheckCircle2 className="w-4 h-4 text-green-500" />
            <span className="text-sm">{successful} success</span>
          </div>
          <div data-ui-id="recap-actions-failed" className="flex items-center gap-2">
            <XCircle className="w-4 h-4 text-red-500" />
            <span className="text-sm">{failed} failed</span>
          </div>
          <div data-ui-id="recap-actions-skipped" className="flex items-center gap-2">
            <SkipForward className="w-4 h-4 text-muted-foreground" />
            <span className="text-sm">{skipped} skipped</span>
          </div>
        </div>

        {/* Visual Bar Chart */}
        <div data-ui-id="recap-actions-chart" className="h-4 bg-muted rounded-full overflow-hidden flex">
          {total > 0 ? (
            <>
              <div
                className="bg-green-500 h-full transition-all"
                style={{ width: `${successPercent}%` }}
              />
              <div
                className="bg-red-500 h-full transition-all"
                style={{ width: `${failedPercent}%` }}
              />
              <div
                className="bg-gray-500 h-full transition-all"
                style={{ width: `${skippedPercent}%` }}
              />
            </>
          ) : (
            <div className="w-full h-full flex items-center justify-center text-xs text-muted-foreground">
              No actions recorded
            </div>
          )}
        </div>
      </div>

      {/* States & Transitions Row */}
      <div className="grid grid-cols-2 gap-4">
        {/* States Visited */}
        <div data-ui-id="recap-states-visited" className="card p-4">
          <h3 className="font-medium flex items-center gap-2 mb-3">
            <Layers className="w-5 h-5 text-blue-500" />
            States Visited
          </h3>
          <p className="text-sm text-muted-foreground">
            No state tracking data available for this run.
          </p>
        </div>

        {/* Transitions */}
        <div data-ui-id="recap-transitions" className="card p-4">
          <h3 className="font-medium flex items-center gap-2 mb-3">
            <ArrowRight className="w-5 h-5 text-purple-500" />
            Transitions
          </h3>
          <p className="text-sm text-muted-foreground">
            No transition data available for this run.
          </p>
        </div>
      </div>

      {/* Template Matching */}
      <div data-ui-id="recap-template-matches" className="card p-4">
        <h3 className="font-medium flex items-center gap-2 mb-3">
          <Eye className="w-5 h-5 text-amber-500" />
          Template Matching
        </h3>
        <p className="text-sm text-muted-foreground">
          No template matching results available for this run.
        </p>
      </div>

      {/* Screenshots Gallery */}
      <div data-ui-id="recap-screenshots-gallery" className="card p-4">
        <h3 className="font-medium flex items-center gap-2 mb-3">
          <Image className="w-5 h-5 text-teal-500" />
          Annotated Screenshots
        </h3>
        {screenshots.length > 0 ? (
          <div className="grid grid-cols-4 gap-2">
            {screenshots.map((screenshot, i) => (
              <button
                key={i}
                onClick={() => setSelectedScreenshot(screenshot.path)}
                className="aspect-video bg-muted rounded overflow-hidden hover:ring-2 ring-primary transition-all"
              >
                <img
                  src={screenshot.path}
                  alt={screenshot.description || `Screenshot ${i + 1}`}
                  className="w-full h-full object-cover"
                />
              </button>
            ))}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No screenshots captured for this run.
          </p>
        )}
      </div>

      {/* Screenshot Modal */}
      {selectedScreenshot && (
        <div
          className="fixed inset-0 z-50 bg-black/80 flex items-center justify-center p-8"
          onClick={() => setSelectedScreenshot(null)}
        >
          <img
            src={selectedScreenshot}
            alt="Screenshot"
            className="max-w-full max-h-full object-contain"
          />
        </div>
      )}
    </div>
  );
}

export default AutomationTab;
