/**
 * DomSnapshotsPanel.tsx
 *
 * Panel showing captured DOM snapshots for AI debugging.
 * Displays list of captures with URL, timestamp, size, and source.
 */

import { useState } from "react";
import {
  Code,
  Globe,
  Clock,
  FileCode,
  ExternalLink,
  ChevronRight,
  RefreshCw,
  Puzzle,
  Play,
  AlertCircle,
  Zap,
  GitCompare,
  X,
} from "lucide-react";
import { useDomCaptures, useDomCaptureHtml } from "../../hooks/useDomCaptures";
import { HtmlViewerModal } from "./HtmlViewerModal";
import { DomDiffModal } from "./DomDiffModal";
import type { DomCapture, DomCaptureSource, DomCaptureTrigger } from "../../types/domCapture";

/** Format bytes to human readable size */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Format timestamp to relative time */
function formatTime(timestamp: string): string {
  const date = new Date(timestamp);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSec = Math.floor(diffMs / 1000);
  const diffMin = Math.floor(diffSec / 60);
  const diffHour = Math.floor(diffMin / 60);

  if (diffSec < 60) return "just now";
  if (diffMin < 60) return `${diffMin}m ago`;
  if (diffHour < 24) return `${diffHour}h ago`;
  return date.toLocaleDateString();
}

/** Get icon for capture source */
function SourceIcon({ source }: { source: DomCaptureSource }) {
  if (source === "extension") {
    return (
      <span title="Browser Extension">
        <Puzzle className="w-4 h-4 text-blue-500" />
      </span>
    );
  }
  return (
    <span title="Playwright">
      <Play className="w-4 h-4 text-green-500" />
    </span>
  );
}

/** Get badge for trigger type */
function TriggerBadge({ trigger }: { trigger: DomCaptureTrigger }) {
  const styles = {
    on_demand: "bg-blue-500/10 text-blue-500 border-blue-500/20",
    auto: "bg-green-500/10 text-green-500 border-green-500/20",
    error: "bg-red-500/10 text-red-500 border-red-500/20",
  };

  const icons = {
    on_demand: <Zap className="w-3 h-3" />,
    auto: <RefreshCw className="w-3 h-3" />,
    error: <AlertCircle className="w-3 h-3" />,
  };

  const labels = {
    on_demand: "On Demand",
    auto: "Auto",
    error: "Error",
  };

  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-full border ${styles[trigger]}`}
    >
      {icons[trigger]}
      {labels[trigger]}
    </span>
  );
}

interface Props {
  /** Optional task run ID to filter captures */
  taskRunId?: string;
}

export function DomSnapshotsPanel({ taskRunId }: Props) {
  const { data: captures, isLoading, refetch } = useDomCaptures(taskRunId);
  const [selectedCapture, setSelectedCapture] = useState<DomCapture | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // Compare mode state
  const [compareMode, setCompareMode] = useState(false);
  const [compareSelection, setCompareSelection] = useState<[DomCapture | null, DomCapture | null]>([
    null,
    null,
  ]);
  const [showDiffModal, setShowDiffModal] = useState(false);

  // Fetch HTML for comparison (only when both are selected)
  const { data: leftHtml } = useDomCaptureHtml(compareSelection[0]?.id ?? null);
  const { data: rightHtml } = useDomCaptureHtml(compareSelection[1]?.id ?? null);

  const handleCompareSelect = (capture: DomCapture) => {
    if (!compareMode) return;

    // If already selected, deselect
    if (compareSelection[0]?.id === capture.id) {
      setCompareSelection([null, compareSelection[1]]);
      return;
    }
    if (compareSelection[1]?.id === capture.id) {
      setCompareSelection([compareSelection[0], null]);
      return;
    }

    // Add to selection
    if (!compareSelection[0]) {
      setCompareSelection([capture, compareSelection[1]]);
    } else if (!compareSelection[1]) {
      setCompareSelection([compareSelection[0], capture]);
    } else {
      // Replace the first one
      setCompareSelection([capture, compareSelection[1]]);
    }
  };

  const getSelectionIndex = (captureId: string): number | null => {
    if (compareSelection[0]?.id === captureId) return 1;
    if (compareSelection[1]?.id === captureId) return 2;
    return null;
  };

  const exitCompareMode = () => {
    setCompareMode(false);
    setCompareSelection([null, null]);
  };

  const canCompare = compareSelection[0] && compareSelection[1] && leftHtml && rightHtml;

  if (isLoading) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="flex items-center gap-2 text-muted-foreground">
          <RefreshCw className="w-4 h-4 animate-spin" />
          <span>Loading DOM captures...</span>
        </div>
      </div>
    );
  }

  if (!captures || captures.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center p-8 text-center">
        <Code className="w-12 h-12 text-muted-foreground/30 mb-4" />
        <h3 className="text-lg font-medium mb-2">No DOM Snapshots</h3>
        <p className="text-sm text-muted-foreground max-w-md">
          DOM snapshots will appear here when captured via the browser extension or Playwright. Use
          the Qontinui DOM Capture extension to capture page HTML for debugging.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border flex-shrink-0">
        <div className="flex items-center gap-3">
          <Code className="w-5 h-5 text-primary" />
          <div>
            <h3 className="font-medium">DOM Snapshots</h3>
            <p className="text-sm text-muted-foreground">
              {captures.length} capture{captures.length !== 1 ? "s" : ""}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {compareMode ? (
            <>
              <span className="text-sm text-muted-foreground">Select 2 captures to compare</span>
              <button
                onClick={() => setShowDiffModal(true)}
                disabled={!canCompare}
                className={`px-3 py-1.5 text-sm rounded-md flex items-center gap-1.5 ${
                  canCompare
                    ? "bg-primary text-primary-foreground hover:bg-primary/90"
                    : "bg-muted text-muted-foreground cursor-not-allowed"
                }`}
              >
                <GitCompare className="w-4 h-4" />
                Compare
              </button>
              <button
                onClick={exitCompareMode}
                className="p-2 rounded-md hover:bg-muted transition-colors"
                title="Exit compare mode"
              >
                <X className="w-4 h-4" />
              </button>
            </>
          ) : (
            <>
              {captures.length >= 2 && (
                <button
                  onClick={() => setCompareMode(true)}
                  className="px-3 py-1.5 text-sm border border-border rounded-md hover:bg-muted transition-colors flex items-center gap-1.5"
                  title="Compare two captures"
                >
                  <GitCompare className="w-4 h-4" />
                  Compare
                </button>
              )}
              <button
                onClick={() => refetch()}
                className="p-2 rounded-md hover:bg-muted transition-colors"
                title="Refresh"
              >
                <RefreshCw className="w-4 h-4" />
              </button>
            </>
          )}
        </div>
      </div>

      {/* Capture List */}
      <div className="flex-1 overflow-y-auto">
        <div className="divide-y divide-border">
          {captures.map((capture) => {
            const selectionIndex = getSelectionIndex(capture.id);
            const isSelected = selectionIndex !== null;

            return (
              <div
                key={capture.id}
                className={`hover:bg-muted/30 transition-colors ${
                  isSelected ? "bg-primary/10 ring-1 ring-primary/30" : ""
                }`}
              >
                {/* Main row */}
                <div
                  className="flex items-center gap-3 p-3 cursor-pointer"
                  onClick={() => {
                    if (compareMode) {
                      handleCompareSelect(capture);
                    } else {
                      setExpandedId(expandedId === capture.id ? null : capture.id);
                    }
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      if (compareMode) {
                        handleCompareSelect(capture);
                      } else {
                        setExpandedId(expandedId === capture.id ? null : capture.id);
                      }
                    }
                  }}
                  role="button"
                  tabIndex={0}
                >
                  {compareMode ? (
                    <div
                      className={`w-6 h-6 rounded-full border-2 flex items-center justify-center text-xs font-bold ${
                        isSelected
                          ? "bg-primary border-primary text-primary-foreground"
                          : "border-muted-foreground"
                      }`}
                    >
                      {isSelected ? selectionIndex : ""}
                    </div>
                  ) : (
                    <ChevronRight
                      className={`w-4 h-4 text-muted-foreground transition-transform ${
                        expandedId === capture.id ? "rotate-90" : ""
                      }`}
                    />
                  )}
                  <SourceIcon source={capture.source} />
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <Globe className="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
                      <span className="text-sm font-medium truncate" title={capture.url}>
                        {new URL(capture.url).hostname}
                        {new URL(capture.url).pathname}
                      </span>
                    </div>
                    <div className="flex items-center gap-2 mt-0.5 text-xs text-muted-foreground">
                      <Clock className="w-3 h-3" />
                      <span>{formatTime(capture.timestamp)}</span>
                      <span className="text-muted-foreground/50">|</span>
                      <FileCode className="w-3 h-3" />
                      <span>{formatSize(capture.htmlSizeBytes)}</span>
                    </div>
                  </div>
                  <TriggerBadge trigger={capture.trigger} />
                </div>

                {/* Expanded details */}
                {expandedId === capture.id && (
                  <div className="px-10 pb-3 space-y-2">
                    <div className="text-sm">
                      <span className="text-muted-foreground">Title:</span>{" "}
                      <span className="text-foreground">{capture.pageTitle}</span>
                    </div>
                    {capture.selector && (
                      <div className="text-sm">
                        <span className="text-muted-foreground">Selector:</span>{" "}
                        <code className="px-1.5 py-0.5 bg-muted rounded text-xs">
                          {capture.selector}
                        </code>
                      </div>
                    )}
                    <div className="text-sm">
                      <span className="text-muted-foreground">Full URL:</span>{" "}
                      <a
                        href={capture.url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-primary hover:underline inline-flex items-center gap-1"
                      >
                        {capture.url}
                        <ExternalLink className="w-3 h-3" />
                      </a>
                    </div>
                    <div className="flex gap-2 mt-3">
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          setSelectedCapture(capture);
                        }}
                        className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
                      >
                        View HTML
                      </button>
                      {capture.linkedScreenshot && (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            // Open screenshot in new tab
                            window.open(capture.linkedScreenshot, "_blank");
                          }}
                          className="px-3 py-1.5 text-sm border border-border rounded-md hover:bg-muted transition-colors"
                        >
                          View Screenshot
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* HTML Viewer Modal */}
      {selectedCapture && (
        <HtmlViewerModal capture={selectedCapture} onClose={() => setSelectedCapture(null)} />
      )}

      {/* DOM Diff Modal */}
      {showDiffModal && compareSelection[0] && compareSelection[1] && leftHtml && rightHtml && (
        <DomDiffModal
          isOpen={showDiffModal}
          onClose={() => setShowDiffModal(false)}
          leftHtml={leftHtml}
          rightHtml={rightHtml}
          leftTitle={compareSelection[0].pageTitle || new URL(compareSelection[0].url).hostname}
          rightTitle={compareSelection[1].pageTitle || new URL(compareSelection[1].url).hostname}
          leftTimestamp={compareSelection[0].timestamp}
          rightTimestamp={compareSelection[1].timestamp}
        />
      )}
    </div>
  );
}
