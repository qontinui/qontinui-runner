/**
 * ImageDetailModal Component
 *
 * A comprehensive modal for displaying detailed image recognition information.
 * Shows screenshot, template, match results, confidence scores, and top match candidates.
 */

import React, { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { X, Maximize2, CheckCircle2, XCircle, Target, Monitor } from "lucide-react";
import { ImageRecognitionEntry } from "../managers/LogManager";
import { cn } from "../lib/utils";

interface ImageDetailModalProps {
  entry: ImageRecognitionEntry | null;
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Section component for consistent styling
 */
interface SectionProps {
  title: string;
  children: React.ReactNode;
  className?: string;
}

function Section({ title, children, className }: SectionProps) {
  return (
    <div className={cn("space-y-2", className)}>
      <h3 className="text-sm font-semibold text-foreground border-b border-border pb-1">{title}</h3>
      <div className="text-sm">{children}</div>
    </div>
  );
}

/**
 * Image viewer modal for full-size screenshots
 */
interface ImageViewerProps {
  imagePath?: string;
  imageData?: string; // Base64 encoded image
  title: string;
  isOpen: boolean;
  onClose: () => void;
}

function ImageViewer({ imagePath, imageData, title, isOpen, onClose }: ImageViewerProps) {
  const imageSrc = imageData
    ? `data:image/png;base64,${imageData}`
    : imagePath
      ? `file://${imagePath}`
      : "";

  if (!imageSrc) return null;

  return (
    <Dialog.Root open={isOpen} onOpenChange={onClose}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/80 z-50" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 z-50 max-h-[90vh] max-w-[90vw]"
          aria-describedby="image-description"
        >
          <Dialog.Title className="sr-only">{title}</Dialog.Title>
          <p id="image-description" className="sr-only">
            Full resolution view of {title}
          </p>
          <div className="relative">
            <img
              src={imageSrc}
              alt={title}
              className="max-h-[90vh] max-w-[90vw] object-contain rounded-lg"
            />
            <Dialog.Close asChild>
              <button
                className="absolute top-4 right-4 p-2 bg-black/50 hover:bg-black/70 text-white rounded-full transition-colors"
                aria-label="Close"
              >
                <X className="w-6 h-6" />
              </button>
            </Dialog.Close>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * Main modal component
 */
export default function ImageDetailModal({ entry, isOpen, onClose }: ImageDetailModalProps) {
  const [screenshotViewerOpen, setScreenshotViewerOpen] = useState(false);
  const [templateViewerOpen, setTemplateViewerOpen] = useState(false);
  const [matchedRegionViewerOpen, setMatchedRegionViewerOpen] = useState(false);

  if (!entry) return null;

  return (
    <>
      <Dialog.Root open={isOpen} onOpenChange={onClose}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 bg-black/50 backdrop-blur-sm z-40" />
          <Dialog.Content
            className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 bg-card border border-border rounded-xl shadow-2xl z-40 max-h-[90vh] w-[90vw] max-w-4xl overflow-hidden flex flex-col"
            aria-describedby="image-detail-description"
          >
            {/* Header */}
            <div className="flex items-center justify-between p-6 border-b border-border">
              <div className="flex items-center gap-3">
                <div
                  className={cn(
                    "p-2 rounded-lg",
                    entry.found
                      ? "bg-green-100 dark:bg-green-900/30"
                      : "bg-red-100 dark:bg-red-900/30",
                  )}
                >
                  {entry.found ? (
                    <CheckCircle2 className="w-5 h-5 text-green-600 dark:text-green-400" />
                  ) : (
                    <XCircle className="w-5 h-5 text-red-600 dark:text-red-400" />
                  )}
                </div>
                <div>
                  <Dialog.Title className="text-lg font-semibold">
                    Image Recognition Details
                  </Dialog.Title>
                  <p id="image-detail-description" className="text-sm text-muted-foreground">
                    {entry.timestamp}
                  </p>
                </div>
              </div>
              <Dialog.Close asChild>
                <button
                  className="p-2 hover:bg-accent rounded-lg transition-colors"
                  aria-label="Close"
                >
                  <X className="w-5 h-5" />
                </button>
              </Dialog.Close>
            </div>

            {/* Content */}
            <div className="flex-1 overflow-y-auto p-6 space-y-6">
              {/* Match Result Section */}
              <Section title="Match Result">
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Template</div>
                    <div className="font-medium">{entry.template}</div>
                  </div>
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Result</div>
                    <div>
                      <span
                        className={cn(
                          "inline-flex items-center px-3 py-1 rounded-full text-sm font-medium",
                          entry.found
                            ? "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-400"
                            : "bg-red-100 text-red-800 dark:bg-red-900/30 dark:text-red-400",
                        )}
                      >
                        {entry.found ? "FOUND" : "NOT FOUND"}
                      </span>
                    </div>
                  </div>
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Confidence</div>
                    <div className="font-mono">
                      <span
                        className={cn(
                          "font-semibold",
                          entry.confidence >= entry.threshold
                            ? "text-green-600 dark:text-green-400"
                            : "text-red-600 dark:text-red-400",
                        )}
                      >
                        {(entry.confidence * 100).toFixed(2)}%
                      </span>
                      <span className="text-muted-foreground ml-2">
                        (threshold: {(entry.threshold * 100).toFixed(0)}%)
                      </span>
                    </div>
                  </div>
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Location</div>
                    <div className="font-mono text-sm">
                      {entry.location ? (
                        <div className="flex items-center gap-2">
                          <Target className="w-4 h-4 text-muted-foreground" />({entry.location.x},{" "}
                          {entry.location.y}) - {entry.location.width}x{entry.location.height}
                        </div>
                      ) : (
                        <span className="text-muted-foreground">No location</span>
                      )}
                    </div>
                  </div>
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Captured Screen</div>
                    <div className="font-mono text-sm">
                      <div className="flex items-center gap-2">
                        <Monitor className="w-4 h-4 text-muted-foreground" />
                        {entry.monitorIndex !== undefined && entry.monitorIndex !== null
                          ? `Monitor ${entry.monitorIndex + 1}`
                          : "All monitors"}
                      </div>
                    </div>
                  </div>
                </div>

                {!entry.found && entry.gap !== undefined && (
                  <div className="mt-4 p-3 bg-orange-100 dark:bg-orange-900/20 rounded-lg border border-orange-200 dark:border-orange-800">
                    <div className="text-sm font-medium text-orange-900 dark:text-orange-300">
                      Match Quality
                    </div>
                    <div className="text-sm text-orange-800 dark:text-orange-400 mt-1">
                      Gap: {(entry.gap * 100).toFixed(2)}% ({entry.percentOff?.toFixed(2)}% below
                      threshold)
                    </div>
                  </div>
                )}
              </Section>

              {/* Best Match Location Section */}
              {entry.bestMatchLocation && (
                <Section title="Best Match Location">
                  <div className="font-mono text-sm bg-muted/30 rounded-md p-3 border border-border">
                    {entry.bestMatchLocation}
                  </div>
                </Section>
              )}

              {/* Visual Data Section */}
              {(entry.screenshotPath ||
                entry.screenshotData ||
                entry.visualDebugImage ||
                entry.templatePath ||
                entry.imageData ||
                entry.matchedRegionImage) && (
                <Section title="Visual Data">
                  <div className="space-y-4">
                    {/* Template and Matched Region side-by-side comparison */}
                    {(entry.templatePath || entry.imageData || entry.matchedRegionImage) && (
                      <div className="flex gap-6 flex-wrap">
                        {/* Template Image */}
                        {(entry.templatePath || entry.imageData) && (
                          <div className="space-y-2">
                            <div className="text-xs text-muted-foreground font-medium">
                              Template Image
                            </div>
                            <div className="relative group inline-block">
                              <img
                                src={
                                  entry.imageData
                                    ? `data:image/png;base64,${entry.imageData}`
                                    : `file://${entry.templatePath}`
                                }
                                alt="Template"
                                className="max-w-[200px] max-h-[200px] object-contain rounded-lg border border-border cursor-pointer hover:border-primary transition-colors"
                                onClick={() => setTemplateViewerOpen(true)}
                              />
                              <button
                                onClick={() => setTemplateViewerOpen(true)}
                                className="absolute top-2 right-2 p-2 bg-black/50 hover:bg-black/70 text-white rounded-lg opacity-0 group-hover:opacity-100 transition-opacity"
                                aria-label="View full size template"
                              >
                                <Maximize2 className="w-4 h-4" />
                              </button>
                            </div>
                          </div>
                        )}

                        {/* Matched Region Image (cropped from screenshot at match location) */}
                        {entry.matchedRegionImage && (
                          <div className="space-y-2">
                            <div className="text-xs text-muted-foreground font-medium">
                              Matched Region
                              <span className="ml-1 text-xs opacity-60">(actual pixels)</span>
                            </div>
                            <div className="relative group inline-block">
                              <img
                                src={`data:image/png;base64,${entry.matchedRegionImage}`}
                                alt="Matched region from screenshot"
                                className={cn(
                                  "max-w-[200px] max-h-[200px] object-contain rounded-lg border-2 cursor-pointer transition-colors",
                                  entry.found
                                    ? "border-green-500 hover:border-green-400"
                                    : "border-red-500 hover:border-red-400",
                                )}
                                onClick={() => setMatchedRegionViewerOpen(true)}
                              />
                              <button
                                onClick={() => setMatchedRegionViewerOpen(true)}
                                className="absolute top-2 right-2 p-2 bg-black/50 hover:bg-black/70 text-white rounded-lg opacity-0 group-hover:opacity-100 transition-opacity"
                                aria-label="View full size matched region"
                              >
                                <Maximize2 className="w-4 h-4" />
                              </button>
                            </div>
                          </div>
                        )}
                      </div>
                    )}

                    {/* Screenshot with annotations (full width) */}
                    {entry.visualDebugImage || entry.screenshotPath || entry.screenshotData ? (
                      <div className="space-y-2">
                        <div className="text-xs text-muted-foreground font-medium">
                          Screenshot
                          {entry.screenshotTimestamp && (
                            <span className="ml-2 font-normal">({entry.screenshotTimestamp})</span>
                          )}
                        </div>
                        <div className="relative group">
                          <img
                            src={
                              entry.visualDebugImage
                                ? `data:image/png;base64,${entry.visualDebugImage}`
                                : entry.screenshotData
                                  ? `data:image/jpeg;base64,${entry.screenshotData}`
                                  : `file://${entry.screenshotPath}`
                            }
                            alt="Search screenshot"
                            className="w-full rounded-lg border border-border cursor-pointer hover:border-primary transition-colors"
                            onClick={() => setScreenshotViewerOpen(true)}
                          />
                          <button
                            onClick={() => setScreenshotViewerOpen(true)}
                            className="absolute top-2 right-2 p-2 bg-black/50 hover:bg-black/70 text-white rounded-lg opacity-0 group-hover:opacity-100 transition-opacity"
                            aria-label="View full size screenshot"
                          >
                            <Maximize2 className="w-4 h-4" />
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="space-y-2">
                        <div className="text-xs text-muted-foreground font-medium">Screenshot</div>
                        <div className="w-full h-32 rounded-lg border border-border bg-muted/20 flex items-center justify-center">
                          <span className="text-xs text-muted-foreground">
                            Screenshot not available
                          </span>
                        </div>
                      </div>
                    )}

                    {/* Match Details */}
                    {entry.debug?.top_matches && entry.debug.top_matches.length > 0 && (
                      <div className="space-y-2">
                        <div className="text-xs text-muted-foreground font-medium">
                          Match Details (showing {entry.debug.top_matches.length} candidates)
                        </div>
                        <div className="space-y-1 text-xs">
                          {entry.debug.top_matches.map((match: unknown, idx: number) => {
                            if (typeof match !== "object" || match === null) {
                              return null;
                            }
                            const typedMatch = match as {
                              confidence?: number;
                              location?: { x?: number; y?: number };
                            };
                            const confidence = typedMatch.confidence || 0;
                            const meetsThreshold = confidence >= entry.threshold;
                            const location = typedMatch.location || {};

                            return (
                              <div
                                key={idx}
                                className={cn(
                                  "p-2 rounded border font-mono",
                                  meetsThreshold
                                    ? "bg-green-50 dark:bg-green-900/20 border-green-200 dark:border-green-800"
                                    : "bg-red-50 dark:bg-red-900/20 border-red-200 dark:border-red-800",
                                )}
                              >
                                <div className="flex items-center justify-between">
                                  <span
                                    className={cn(
                                      "font-semibold",
                                      meetsThreshold
                                        ? "text-green-700 dark:text-green-400"
                                        : "text-red-700 dark:text-red-400",
                                    )}
                                  >
                                    #{idx + 1}: {(confidence * 100).toFixed(2)}%
                                  </span>
                                  <span className="text-muted-foreground">
                                    ({location.x || 0}, {location.y || 0})
                                  </span>
                                </div>
                                {!meetsThreshold && (
                                  <div className="text-xs text-muted-foreground mt-1">
                                    Below threshold by{" "}
                                    {((entry.threshold - confidence) * 100).toFixed(2)}%
                                  </div>
                                )}
                              </div>
                            );
                          })}
                        </div>
                      </div>
                    )}
                  </div>
                </Section>
              )}

              {/* Node Information */}
              {entry.node && (
                <Section title="Node Information">
                  <div className="text-sm">
                    <div className="text-xs text-muted-foreground mb-1">Node ID</div>
                    <div className="font-mono text-xs bg-muted/30 rounded-md p-2 border border-border break-all">
                      {entry.node}
                    </div>
                  </div>
                </Section>
              )}
            </div>

            {/* Footer */}
            <div className="border-t border-border p-4 flex justify-end">
              <Dialog.Close asChild>
                <button className="btn-secondary">Close</button>
              </Dialog.Close>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>

      {/* Full-size image viewers */}
      {(entry.visualDebugImage || entry.screenshotPath || entry.screenshotData) && (
        <ImageViewer
          imagePath={entry.screenshotPath}
          imageData={entry.visualDebugImage || entry.screenshotData}
          title="Full Size Screenshot"
          isOpen={screenshotViewerOpen}
          onClose={() => setScreenshotViewerOpen(false)}
        />
      )}
      {(entry.templatePath || entry.imageData) && (
        <ImageViewer
          imagePath={entry.templatePath}
          imageData={entry.imageData}
          title="Full Size Template"
          isOpen={templateViewerOpen}
          onClose={() => setTemplateViewerOpen(false)}
        />
      )}
      {entry.matchedRegionImage && (
        <ImageViewer
          imageData={entry.matchedRegionImage}
          title="Matched Region"
          isOpen={matchedRegionViewerOpen}
          onClose={() => setMatchedRegionViewerOpen(false)}
        />
      )}
    </>
  );
}
