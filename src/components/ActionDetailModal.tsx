/**
 * ActionDetailModal Component
 *
 * A comprehensive modal for displaying detailed action information.
 * Shows configuration, runtime results, state context, timing, and screenshots.
 */

import React, { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { X, Clock, CheckCircle2, XCircle, Maximize2 } from "lucide-react";
import { DisplayNode } from "../types/treeEvents";
import { ActionLogEntry } from "../types/displayProfile";
import { cn } from "../lib/utils";

// Type definitions for metadata structure
interface ActionMetadata {
  config?: Record<string, unknown>;
  runtime?: Record<string, unknown>;
  state_context?: StateContext;
  timing?: Timing;
  outcome?: Outcome;
  screenshot_reference?: string;
  visual_debug_reference?: string;
  [key: string]: unknown;
}

interface StateContext {
  active_before: string[];
  active_after: string[];
  changed: boolean;
  activated: string[];
  deactivated: string[];
}

interface Timing {
  end_time?: number;
  duration_ms: number;
}

interface Outcome {
  error?: string;
  retry_count?: number;
}

interface ActionDetailModalProps {
  action: DisplayNode | ActionLogEntry | null;
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Format a timestamp to a readable string
 */
function formatDateTime(timestamp: number): string {
  return new Date(timestamp * 1000).toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

/**
 * Format duration in milliseconds
 */
function formatDuration(durationSeconds: number): string {
  const ms = durationSeconds * 1000;

  if (ms < 1000) {
    return `${Math.round(ms)}ms`;
  } else if (ms < 60000) {
    return `${(ms / 1000).toFixed(2)}s`;
  } else {
    const minutes = Math.floor(ms / 60000);
    const seconds = ((ms % 60000) / 1000).toFixed(0);
    return `${minutes}m ${seconds}s`;
  }
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
 * Code block component for JSON display
 */
interface CodeBlockProps {
  data: any;
  title?: string;
}

function CodeBlock({ data, title }: CodeBlockProps) {
  return (
    <div className="space-y-1">
      {title && <div className="text-xs text-muted-foreground">{title}</div>}
      <pre className="bg-muted/30 rounded-md p-3 overflow-x-auto text-xs font-mono border border-border">
        {JSON.stringify(data, null, 2)}
      </pre>
    </div>
  );
}

/**
 * Screenshot viewer modal
 */
interface ScreenshotViewerProps {
  screenshotPath: string;
  isOpen: boolean;
  onClose: () => void;
}

function ScreenshotViewer({ screenshotPath, isOpen, onClose }: ScreenshotViewerProps) {
  return (
    <Dialog.Root open={isOpen} onOpenChange={onClose}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/80 z-50" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 z-50 max-h-[90vh] max-w-[90vw]"
          aria-describedby="screenshot-description"
        >
          <Dialog.Title className="sr-only">Full Size Screenshot</Dialog.Title>
          <Dialog.Description id="screenshot-description" className="sr-only">
            Full resolution view of the action screenshot
          </Dialog.Description>
          <div className="relative">
            <img
              src={`file://${screenshotPath}`}
              alt="Action screenshot"
              className="max-h-[90vh] max-w-[90vw] object-contain rounded-lg"
            />
            <Dialog.Close asChild>
              <button
                className="absolute top-4 right-4 p-2 bg-black/50 hover:bg-black/70 rounded-full text-white transition-colors"
                aria-label="Close screenshot viewer"
              >
                <X className="w-5 h-5" />
              </button>
            </Dialog.Close>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * Type guard to check if metadata has expected structure
 */
function isActionMetadata(
  metadata: Record<string, unknown> | undefined,
): metadata is ActionMetadata {
  return metadata !== undefined;
}

/**
 * Helper to safely get a string value from runtime
 */
function getRuntimeString(runtime: Record<string, unknown>, key: string): string | undefined {
  const value = runtime[key];
  return typeof value === "string" ? value : undefined;
}

/**
 * Helper to safely get a number value from runtime
 */
function getRuntimeNumber(runtime: Record<string, unknown>, key: string): number | undefined {
  const value = runtime[key];
  return typeof value === "number" ? value : undefined;
}

/**
 * Helper to safely get a boolean value from runtime
 */
function getRuntimeBoolean(runtime: Record<string, unknown>, key: string): boolean | undefined {
  const value = runtime[key];
  return typeof value === "boolean" ? value : undefined;
}

/**
 * Helper to safely get an array value from runtime
 */
function getRuntimeArray(runtime: Record<string, unknown>, key: string): unknown[] | undefined {
  const value = runtime[key];
  return Array.isArray(value) ? value : undefined;
}

/**
 * Main ActionDetailModal component
 */
export default function ActionDetailModal({ action, isOpen, onClose }: ActionDetailModalProps) {
  const [screenshotViewerOpen, setScreenshotViewerOpen] = useState(false);

  if (!action) return null;

  // Get action name/type - compatible with both DisplayNode and ActionLogEntry
  const actionName = "name" in action ? action.name : action.action_type;

  // Type guard the metadata
  const metadata = isActionMetadata(action.metadata) ? action.metadata : undefined;

  const config = (metadata?.config || {}) as Record<string, unknown>;
  const runtime = (metadata?.runtime || {}) as Record<string, unknown>;
  const stateContext = metadata?.state_context;
  const timing = metadata?.timing;
  const outcome = metadata?.outcome;
  const screenshotRef = metadata?.screenshot_reference;
  const visualDebugRef = metadata?.visual_debug_reference;

  const hasMultipleMatches = Boolean(
    runtime.top_matches && Array.isArray(runtime.top_matches) && runtime.top_matches.length > 0,
  );
  const hasError = action.status === "failed" || outcome?.error !== undefined;

  return (
    <>
      <Dialog.Root open={isOpen} onOpenChange={onClose}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 bg-black/50 z-40" />
          <Dialog.Content
            className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 bg-background border border-border rounded-lg shadow-lg w-full max-w-4xl max-h-[90vh] overflow-hidden z-50 flex flex-col"
            aria-describedby="action-detail-description"
          >
            {/* Header */}
            <div className="flex items-center justify-between p-4 border-b border-border">
              <Dialog.Title className="text-lg font-semibold text-foreground flex items-center gap-2">
                {action.status === "success" && <CheckCircle2 className="w-5 h-5 text-green-500" />}
                {action.status === "failed" && <XCircle className="w-5 h-5 text-red-500" />}
                {actionName}
              </Dialog.Title>
              <Dialog.Close asChild>
                <button
                  className="p-1 hover:bg-accent rounded-md transition-colors"
                  aria-label="Close modal"
                >
                  <X className="w-5 h-5" />
                </button>
              </Dialog.Close>
            </div>

            <Dialog.Description id="action-detail-description" className="sr-only">
              Detailed information about the {actionName} action including configuration, runtime
              results, and timing
            </Dialog.Description>

            {/* Scrollable Content */}
            <div className="flex-1 overflow-y-auto p-4 space-y-4">
              {/* Action Overview */}
              <Section title="Action Overview">
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <span className="text-muted-foreground">Type:</span>{" "}
                    <span className="font-mono">{actionName}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">Status:</span>{" "}
                    <span
                      className={cn(
                        "font-semibold",
                        action.status === "success" && "text-green-500",
                        action.status === "failed" && "text-red-500",
                        action.status === "running" && "text-blue-500",
                        action.status === "pending" && "text-yellow-500",
                      )}
                    >
                      {action.status === "success" && "✓ Success"}
                      {action.status === "failed" && "✗ Failed"}
                      {action.status === "running" && "Running..."}
                      {action.status === "pending" && "Pending"}
                    </span>
                  </div>
                  {action.duration !== undefined && action.duration !== null && (
                    <div>
                      <span className="text-muted-foreground">Duration:</span>{" "}
                      <span className="font-mono">{formatDuration(action.duration)}</span>
                    </div>
                  )}
                  {timing && timing.duration_ms !== undefined && (
                    <div>
                      <span className="text-muted-foreground">Precise Duration:</span>{" "}
                      <span className="font-mono">{timing.duration_ms}ms</span>
                    </div>
                  )}
                </div>
              </Section>

              {/* Error Display */}
              {hasError && (
                <Section
                  title="Error Details"
                  className="bg-red-500/10 -m-4 p-4 border-l-4 border-red-500"
                >
                  <div className="space-y-2">
                    <div className="flex items-start gap-2">
                      <XCircle className="w-5 h-5 text-red-500 mt-0.5 flex-shrink-0" />
                      <div className="flex-1">
                        <div className="font-semibold text-red-400">
                          {outcome?.error ? "Error" : "Action Failed"}
                        </div>
                        <div className="text-red-300 mt-1 font-mono text-xs whitespace-pre-wrap">
                          {outcome?.error || action.error || "Unknown error occurred"}
                        </div>
                      </div>
                    </div>
                    {outcome?.retry_count !== undefined && outcome.retry_count > 0 && (
                      <div className="text-xs text-muted-foreground">
                        Retries attempted: {outcome.retry_count}
                      </div>
                    )}
                  </div>
                </Section>
              )}

              {/* Configuration */}
              <Section title="Configuration">
                {Object.keys(config).length > 0 ? (
                  <CodeBlock data={config} />
                ) : (
                  <div className="text-muted-foreground italic">No configuration available</div>
                )}
              </Section>

              {/* Runtime Results */}
              {Object.keys(runtime).length > 0 && (
                <Section title="Runtime Results">
                  <div className="space-y-3">
                    {/* FIND action with multiple matches */}
                    {hasMultipleMatches &&
                      (() => {
                        const topMatches = getRuntimeArray(runtime, "top_matches");
                        if (!topMatches) return null;

                        return (
                          <div>
                            <div className="font-medium mb-2">
                              Match Results ({topMatches.length} found):
                            </div>
                            <div className="space-y-1 pl-4">
                              {topMatches.map((match: any, idx: number) => (
                                <div
                                  key={idx}
                                  className="text-xs font-mono bg-muted/20 p-2 rounded border border-border"
                                >
                                  <span className="text-muted-foreground">{idx + 1}.</span> [
                                  {match.location.x}, {match.location.y}, {match.dimensions.w},{" "}
                                  {match.dimensions.h}]{" "}
                                  <span className="text-green-400">
                                    confidence: {(match.confidence * 100).toFixed(1)}%
                                  </span>
                                </div>
                              ))}
                            </div>
                          </div>
                        ) as React.ReactNode;
                      })()}

                    {/* Single FIND result */}
                    {(() => {
                      const found = getRuntimeBoolean(runtime, "found");
                      const confidence = getRuntimeNumber(runtime, "confidence");
                      const location = runtime.location;
                      const dimensions = runtime.dimensions;

                      if (found === undefined || hasMultipleMatches) return null;

                      return (
                        <div>
                          <div className="font-medium mb-1">Find Result:</div>
                          <div className="pl-4 space-y-1 text-xs">
                            <div>
                              Found:{" "}
                              <span className={found ? "text-green-400" : "text-red-400"}>
                                {found ? "Yes" : "No"}
                              </span>
                            </div>
                            {confidence !== undefined && (
                              <div>
                                Confidence:{" "}
                                <span className="text-green-400">
                                  {(confidence * 100).toFixed(1)}%
                                </span>
                              </div>
                            )}
                            {(() => {
                              if (!location || typeof location !== "object" || location === null)
                                return null;
                              if (!("x" in location) || !("y" in location)) return null;
                              return (
                                <div>
                                  Location: ({(location as any).x}, {(location as any).y})
                                </div>
                              ) as React.ReactNode;
                            })()}
                            {(() => {
                              if (
                                !dimensions ||
                                typeof dimensions !== "object" ||
                                dimensions === null
                              )
                                return null;
                              if (!("w" in dimensions) || !("h" in dimensions)) return null;
                              return (
                                <div>
                                  Dimensions: {(dimensions as any).w} x {(dimensions as any).h}
                                </div>
                              ) as React.ReactNode;
                            })()}
                          </div>
                        </div>
                      );
                    })()}

                    {/* TYPE action result */}
                    {(() => {
                      const typedText = getRuntimeString(runtime, "typed_text");
                      const charCount = getRuntimeNumber(runtime, "character_count");

                      if (!typedText) return null;

                      return (
                        <div>
                          <div className="font-medium mb-1">Typed Text:</div>
                          <div className="pl-4 font-mono text-xs bg-muted/20 p-2 rounded border border-border">
                            "{typedText}"
                            {charCount !== undefined && (
                              <span className="text-muted-foreground ml-2">
                                ({charCount} chars)
                              </span>
                            )}
                          </div>
                        </div>
                      );
                    })()}

                    {/* CLICK action result */}
                    {(() => {
                      const clickedAt = runtime.clicked_at;
                      const button = getRuntimeString(runtime, "button");
                      const targetType = getRuntimeString(runtime, "target_type");

                      if (
                        !clickedAt ||
                        typeof clickedAt !== "object" ||
                        clickedAt === null ||
                        !("x" in clickedAt) ||
                        !("y" in clickedAt)
                      ) {
                        return null;
                      }

                      return (
                        <div>
                          <div className="font-medium mb-1">Click Details:</div>
                          <div className="pl-4 space-y-1 text-xs">
                            <div>
                              Position: ({(clickedAt as any).x}, {(clickedAt as any).y})
                            </div>
                            {button && <div>Button: {button}</div>}
                            {targetType && <div>Target Type: {targetType}</div>}
                          </div>
                        </div>
                      );
                    })()}

                    {/* GO_TO_STATE action result */}
                    {(() => {
                      const targetStates = getRuntimeArray(runtime, "target_states");
                      const sourceStates = getRuntimeArray(runtime, "source_states");
                      const targetsReached = getRuntimeArray(runtime, "targets_reached");
                      const transitionsExecuted = getRuntimeArray(runtime, "transitions_executed");
                      const alreadyAtTarget = getRuntimeBoolean(runtime, "already_at_target");

                      if (!targetStates) return null;

                      return (
                        <div>
                          <div className="font-medium mb-1">State Transition:</div>
                          <div className="pl-4 space-y-1 text-xs">
                            {sourceStates && (
                              <div>
                                From: <span className="font-mono">{sourceStates.join(", ")}</span>
                              </div>
                            )}
                            <div>
                              To: <span className="font-mono">{targetStates.join(", ")}</span>
                            </div>
                            {targetsReached && (
                              <div>
                                Reached:{" "}
                                <span className="font-mono text-green-400">
                                  {targetsReached.join(", ")}
                                </span>
                              </div>
                            )}
                            {transitionsExecuted && (
                              <div>
                                Transitions:{" "}
                                <span className="font-mono">{transitionsExecuted.join(", ")}</span>
                              </div>
                            )}
                            {alreadyAtTarget && (
                              <div className="text-blue-400">Already at target state</div>
                            )}
                          </div>
                        </div>
                      );
                    })()}

                    {/* RUN_WORKFLOW action result */}
                    {(() => {
                      const workflowName = getRuntimeString(runtime, "workflow_name");
                      const workflowStatus = getRuntimeString(runtime, "workflow_status");
                      const configWorkflowName =
                        typeof config.workflowName === "string" ? config.workflowName : undefined;
                      const configWorkflowId =
                        typeof config.workflowId === "string" ? config.workflowId : undefined;

                      if (
                        actionName !== "RUN_WORKFLOW" ||
                        !(workflowName || configWorkflowName || configWorkflowId)
                      ) {
                        return null;
                      }

                      return (
                        <div>
                          <div className="font-medium mb-1">Workflow Details:</div>
                          <div className="pl-4 space-y-1 text-xs">
                            <div>
                              Workflow:{" "}
                              <span className="font-mono">
                                {workflowName || configWorkflowName || configWorkflowId}
                              </span>
                            </div>
                            {workflowStatus && (
                              <div>
                                Status:{" "}
                                <span
                                  className={
                                    workflowStatus === "success" ? "text-green-400" : "text-red-400"
                                  }
                                >
                                  {workflowStatus}
                                </span>
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    })()}

                    {/* IF action result */}
                    {(() => {
                      const conditionPassed = getRuntimeBoolean(runtime, "condition_passed");
                      const branchTaken = getRuntimeString(runtime, "branch_taken");

                      if (conditionPassed === undefined) return null;

                      return (
                        <div>
                          <div className="font-medium mb-1">Condition Result:</div>
                          <div className="pl-4 space-y-1 text-xs">
                            <div>
                              Passed:{" "}
                              <span className={conditionPassed ? "text-green-400" : "text-red-400"}>
                                {conditionPassed ? "Yes" : "No"}
                              </span>
                            </div>
                            {branchTaken && (
                              <div>
                                Branch Taken: <span className="font-mono">{branchTaken}</span>
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    })()}

                    {/* Full runtime data */}
                    <details className="mt-3">
                      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
                        View all runtime data
                      </summary>
                      <div className="mt-2">
                        <CodeBlock data={runtime} />
                      </div>
                    </details>
                  </div>
                </Section>
              )}

              {/* State Context */}
              {stateContext && (
                <Section title="State Context">
                  <div className="space-y-2 text-xs">
                    <div className="grid grid-cols-2 gap-3">
                      <div>
                        <div className="font-medium mb-1">States Before:</div>
                        <div className="pl-2">
                          {stateContext.active_before.length > 0 ? (
                            stateContext.active_before.map((state) => (
                              <div key={state} className="font-mono text-muted-foreground">
                                {state}
                              </div>
                            ))
                          ) : (
                            <div className="text-muted-foreground italic">None</div>
                          )}
                        </div>
                      </div>
                      <div>
                        <div className="font-medium mb-1">States After:</div>
                        <div className="pl-2">
                          {stateContext.active_after.length > 0 ? (
                            stateContext.active_after.map((state) => (
                              <div key={state} className="font-mono">
                                {state}
                              </div>
                            ))
                          ) : (
                            <div className="text-muted-foreground italic">None</div>
                          )}
                        </div>
                      </div>
                    </div>

                    {stateContext.changed && (
                      <div className="pt-2 border-t border-border">
                        {stateContext.activated.length > 0 && (
                          <div className="mb-2">
                            <div className="font-medium text-green-400">Activated:</div>
                            <div className="pl-2">
                              {stateContext.activated.map((state) => (
                                <div key={state} className="font-mono text-green-400">
                                  + {state}
                                </div>
                              ))}
                            </div>
                          </div>
                        )}
                        {stateContext.deactivated.length > 0 && (
                          <div>
                            <div className="font-medium text-red-400">Deactivated:</div>
                            <div className="pl-2">
                              {stateContext.deactivated.map((state) => (
                                <div key={state} className="font-mono text-red-400">
                                  - {state}
                                </div>
                              ))}
                            </div>
                          </div>
                        )}
                      </div>
                    )}

                    {!stateContext.changed && (
                      <div className="text-muted-foreground italic">No state changes</div>
                    )}
                  </div>
                </Section>
              )}

              {/* Timing */}
              {timing && (
                <Section title="Timing">
                  <div className="space-y-1 text-xs">
                    <div className="flex items-center gap-2">
                      <Clock className="w-4 h-4 text-muted-foreground" />
                      <span className="text-muted-foreground">Start:</span>
                      <span className="font-mono">{formatDateTime(action.timestamp)}</span>
                    </div>
                    {timing.end_time !== undefined && action.end_timestamp && (
                      <div className="flex items-center gap-2">
                        <Clock className="w-4 h-4 text-muted-foreground" />
                        <span className="text-muted-foreground">End:</span>
                        <span className="font-mono">{formatDateTime(action.end_timestamp)}</span>
                      </div>
                    )}
                    <div className="flex items-center gap-2">
                      <Clock className="w-4 h-4 text-muted-foreground" />
                      <span className="text-muted-foreground">Duration:</span>
                      <span className="font-mono">{timing.duration_ms}ms</span>
                    </div>
                  </div>
                </Section>
              )}

              {/* Screenshots */}
              {(screenshotRef || visualDebugRef) && (
                <Section title="Screenshot">
                  <div className="space-y-3">
                    {screenshotRef && typeof screenshotRef === "string" && (
                      <div>
                        <div className="text-xs text-muted-foreground mb-2">Action Screenshot:</div>
                        <div className="relative group">
                          <img
                            src={`file://${screenshotRef}`}
                            alt="Action screenshot"
                            className="w-full max-w-md rounded-lg border border-border cursor-pointer hover:opacity-90 transition-opacity"
                            onClick={() => setScreenshotViewerOpen(true)}
                          />
                          <button
                            onClick={() => setScreenshotViewerOpen(true)}
                            className="absolute top-2 right-2 p-2 bg-black/50 hover:bg-black/70 rounded-md opacity-0 group-hover:opacity-100 transition-opacity"
                            aria-label="View full size"
                          >
                            <Maximize2 className="w-4 h-4 text-white" />
                          </button>
                        </div>
                        <div className="text-xs text-muted-foreground mt-2 font-mono break-all">
                          {screenshotRef}
                        </div>
                      </div>
                    )}

                    {visualDebugRef && typeof visualDebugRef === "string" && (
                      <div>
                        <div className="text-xs text-muted-foreground mb-2">Visual Debug:</div>
                        <div className="relative group">
                          <img
                            src={`file://${visualDebugRef}`}
                            alt="Visual debug"
                            className="w-full max-w-md rounded-lg border border-border"
                          />
                        </div>
                        <div className="text-xs text-muted-foreground mt-2 font-mono break-all">
                          {visualDebugRef}
                        </div>
                      </div>
                    )}
                  </div>
                </Section>
              )}
            </div>

            {/* Footer */}
            <div className="border-t border-border p-4 flex justify-end gap-2">
              <button
                onClick={onClose}
                className="px-4 py-2 rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors text-sm font-medium"
              >
                Close
              </button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>

      {/* Screenshot Viewer */}
      {screenshotRef && typeof screenshotRef === "string" && (
        <ScreenshotViewer
          screenshotPath={screenshotRef}
          isOpen={screenshotViewerOpen}
          onClose={() => setScreenshotViewerOpen(false)}
        />
      )}
    </>
  );
}
