/**
 * StatusPanel Component
 *
 * Right sidebar panel showing execution summary, screenshots,
 * image recognition results, and warnings/anomalies.
 */

import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { AlertTriangle, ImageIcon, X } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle, Badge, ScrollArea, Progress } from "../ui";
import type { StatusPanelProps } from "./types";
import { getAccentColors, getStatusColors } from "@/design-system";

export function StatusPanel({ executionState }: StatusPanelProps) {
  const [selectedScreenshot, setSelectedScreenshot] = useState<string | null>(null);

  const formatTime = (seconds: number) => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m ${secs}s`;
  };

  const allScreenshots = [
    ...(executionState.screenshots?.annotated.map((s) => ({ ...s, type: "annotated" as const })) ||
      []),
    ...(executionState.screenshots?.playwright.map((s) => ({
      ...s,
      type: "playwright" as const,
    })) || []),
  ].sort((a, b) => {
    const aTime = a.modified ? new Date(a.modified).getTime() : 0;
    const bTime = b.modified ? new Date(b.modified).getTime() : 0;
    return bTime - aTime;
  });

  const displayedScreenshots = allScreenshots.slice(0, 6);
  const totalScreenshots = allScreenshots.length;

  // Demo image recognition data
  const imageRecognitionResults = [
    { template: "save-button.png", found: true, confidence: 92.3, location: "(245, 320)" },
    { template: "cancel-btn.png", found: true, confidence: 88.1, location: "(340, 320)" },
    { template: "error-icon.png", found: false, confidence: 45.2, location: null },
  ];

  // Demo warnings data
  const warnings = [
    { severity: "amber", message: "Slow transition: 3.2s (expected <1s)" },
    { severity: "amber", message: "Low confidence: 67% on 'submit-btn'" },
  ];

  return (
    <div className="w-[30%] bg-card">
      <ScrollArea className="h-full">
        <div className="flex flex-col gap-4 p-4">
          {/* Execution Summary Card */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-semibold">Execution Summary</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-2 gap-4 text-sm">
                <div>
                  <p className="text-muted-foreground">Elapsed Time</p>
                  <p className="font-mono text-lg text-foreground">
                    {formatTime(executionState.elapsedTime)}
                  </p>
                </div>
                <div>
                  <p className="text-muted-foreground">Actions</p>
                  <p className="font-mono text-lg text-foreground">
                    {executionState.actionsCompleted} completed
                  </p>
                </div>
                <div>
                  <p className="text-muted-foreground">Success Rate</p>
                  <p className={`font-mono text-lg ${getAccentColors("green").text}`}>
                    {executionState.successRate.toFixed(1)}%
                  </p>
                </div>
                <div>
                  <p className="text-muted-foreground">Avg Confidence</p>
                  <p className={`font-mono text-lg ${getAccentColors("blue").text}`}>
                    {executionState.averageConfidence.toFixed(1)}%
                  </p>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Captured Screenshots Card */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between">
                <CardTitle className="text-sm font-semibold">Captured Screenshots</CardTitle>
                <Badge variant="muted">{totalScreenshots}</Badge>
              </div>
            </CardHeader>
            <CardContent>
              {displayedScreenshots.length > 0 ? (
                <>
                  <div className="grid grid-cols-3 gap-2">
                    {displayedScreenshots.map((screenshot, i) => (
                      <button
                        key={`${screenshot.path}-${i}`}
                        onClick={() => setSelectedScreenshot(screenshot.path)}
                        className="group relative aspect-[4/3] overflow-hidden rounded-lg bg-muted/50 hover:bg-muted/70 transition-colors"
                      >
                        <div className="absolute inset-0 flex items-center justify-center">
                          <ImageIcon className="h-6 w-6 text-muted-foreground" />
                        </div>
                        <Badge
                          className={`absolute top-1 right-1 text-[10px] px-1.5 py-0 ${
                            screenshot.type === "annotated"
                              ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} ${getAccentColors("blue").border}`
                              : `${getAccentColors("purple").bg} ${getAccentColors("purple").text} ${getAccentColors("purple").border}`
                          }`}
                        >
                          {screenshot.type === "annotated" ? "A" : "P"}
                        </Badge>
                        <div className="absolute bottom-0 left-0 right-0 bg-gradient-to-t from-black/60 to-transparent p-1.5 opacity-0 group-hover:opacity-100 transition-opacity">
                          <p className="text-[10px] text-zinc-300 truncate">
                            {screenshot.filename}
                          </p>
                        </div>
                      </button>
                    ))}
                  </div>
                  {totalScreenshots > 6 && (
                    <button
                      className={`mt-3 text-sm ${getAccentColors("blue").text} hover:opacity-80 transition-colors`}
                    >
                      View All ({totalScreenshots})
                    </button>
                  )}
                </>
              ) : (
                <p className="text-sm text-muted-foreground text-center py-4">
                  No screenshots captured
                </p>
              )}
            </CardContent>
          </Card>

          {/* Recent Image Recognition Card */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-semibold">Recent Image Recognition</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              {imageRecognitionResults.map((match, i) => (
                <div
                  key={`${match.template}-${i}`}
                  className="flex items-center gap-3 rounded-lg bg-muted/30 p-3"
                >
                  <div className="h-10 w-10 rounded bg-muted/50 flex items-center justify-center">
                    <ImageIcon className="h-5 w-5 text-muted-foreground" />
                  </div>
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <p className="font-mono text-xs text-foreground">{match.template}</p>
                      <Badge
                        className={`text-xs ${
                          match.found
                            ? `${getStatusColors("success").bg} ${getStatusColors("success").text} ${getStatusColors("success").border}`
                            : `${getStatusColors("error").bg} ${getStatusColors("error").text} ${getStatusColors("error").border}`
                        }`}
                      >
                        {match.found ? "FOUND" : "NOT FOUND"}
                      </Badge>
                    </div>
                    <div className="mt-1.5 flex items-center gap-2">
                      <Progress value={match.confidence} className="h-1" />
                      <span className="font-mono text-xs text-muted-foreground">
                        {match.confidence}%
                      </span>
                    </div>
                    {match.location && (
                      <p className="mt-1 font-mono text-xs text-muted-foreground">
                        {match.location}
                      </p>
                    )}
                  </div>
                </div>
              ))}
            </CardContent>
          </Card>

          {/* Warnings & Anomalies Card */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between">
                <CardTitle className="text-sm font-semibold">Warnings & Anomalies</CardTitle>
                <Badge
                  className={`${getAccentColors("amber").bg} ${getAccentColors("amber").text} ${getAccentColors("amber").border}`}
                >
                  <AlertTriangle className="mr-1 h-3 w-3" />
                  {warnings.length}
                </Badge>
              </div>
            </CardHeader>
            <CardContent className="space-y-2">
              {warnings.map((warning, i) => (
                <div
                  key={`warning-${i}-${warning.message.slice(0, 20)}`}
                  className="flex items-start gap-2 rounded-lg bg-muted/30 p-3"
                >
                  <AlertTriangle
                    className={`h-4 w-4 shrink-0 ${
                      warning.severity === "amber"
                        ? getAccentColors("amber").text
                        : getAccentColors("red").text
                    }`}
                  />
                  <p className="flex-1 text-sm text-foreground">{warning.message}</p>
                </div>
              ))}
            </CardContent>
          </Card>
        </div>
      </ScrollArea>

      {/* Screenshot Preview Dialog */}
      <Dialog.Root open={!!selectedScreenshot} onOpenChange={() => setSelectedScreenshot(null)}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 bg-black/80 z-50" />
          <Dialog.Content
            className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 z-50 max-w-4xl w-full"
            aria-describedby="screenshot-preview-description"
          >
            <Dialog.Title className="sr-only">Screenshot Preview</Dialog.Title>
            <p id="screenshot-preview-description" className="sr-only">
              Full size preview of the selected screenshot
            </p>
            <div className="relative bg-card border border-border rounded-lg">
              <div className="aspect-video bg-muted rounded-lg flex items-center justify-center">
                <ImageIcon className="h-16 w-16 text-muted-foreground" />
              </div>
              <Dialog.Close asChild>
                <button
                  className="absolute top-4 right-4 p-2 bg-black/50 hover:bg-black/70 text-white rounded-full transition-colors"
                  aria-label="Close"
                >
                  <X className="w-5 h-5" />
                </button>
              </Dialog.Close>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}
