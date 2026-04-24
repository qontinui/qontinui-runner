/**
 * ExternalLogsTab.tsx
 *
 * Tab for viewing external log files from the target project being automated.
 * Shows logs from user-configured log sources (e.g., frontend/backend logs).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  RefreshCw,
  FolderOpen,
  Settings,
  AlertCircle,
  FileText,
  Info,
  Camera,
  Sparkles,
  Copy,
  Check,
} from "lucide-react";
import type { LogSourceContent, ProjectLogConfig } from "../types/projectLogs";
import { useAutoScroll } from "../hooks";
import { getAccentColors, getStatusColors } from "@/design-system";

interface ExternalLogsTabProps {
  /** Project configuration with log sources */
  config: ProjectLogConfig | null;
  /** Content from all enabled log sources */
  sources: LogSourceContent[];
  /** Whether logs are being refreshed */
  loading: boolean;
  /** Error message if refresh failed */
  error?: string;
  /** Last refresh timestamp */
  lastRefresh?: string;
  /** Callback to refresh logs */
  onRefresh: () => void;
  /** Callback to open log source manager */
  onConfigureSources: () => void;
}

export function ExternalLogsTab({
  config,
  sources = [],
  loading,
  error,
  lastRefresh,
  onRefresh,
  onConfigureSources,
}: ExternalLogsTabProps) {
  const [selectedSourceId, setSelectedSourceId] = useState<string | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [autoRefreshInterval, setAutoRefreshInterval] = useState(5000);
  const [showProjectInfo, setShowProjectInfo] = useState(true);
  const [copiedPath, setCopiedPath] = useState<string | null>(null);
  const logViewerRef = useRef<HTMLDivElement>(null);
  const refreshIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Safely access sources array - memoize to prevent dependency changes on every render
  const safeSources = useMemo(() => sources || [], [sources]);

  // Copy path to clipboard
  const handleCopyPath = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      setCopiedPath(path);
      setTimeout(() => setCopiedPath(null), 2000);
    } catch (err) {
      console.error("Failed to copy path:", err);
    }
  };

  // Auto-scroll when new logs come in
  useAutoScroll({
    enabled: true,
    containerRef: logViewerRef,
    dependencies: [safeSources],
  });

  // Auto-refresh logic
  useEffect(() => {
    if (autoRefresh && !loading) {
      refreshIntervalRef.current = setInterval(() => {
        onRefresh();
      }, autoRefreshInterval);
    }

    return () => {
      if (refreshIntervalRef.current) {
        clearInterval(refreshIntervalRef.current);
        refreshIntervalRef.current = null;
      }
    };
  }, [autoRefresh, autoRefreshInterval, loading, onRefresh]);

  // Select first source by default when sources change
  useEffect(() => {
    if (safeSources.length > 0 && !selectedSourceId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- initialize default selection once sources arrive
      setSelectedSourceId(safeSources[0].sourceId);
    }
  }, [safeSources, selectedSourceId]);

  // Get currently selected source content
  const selectedSource = safeSources.find((s) => s.sourceId === selectedSourceId);

  // No config state
  if (!config) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <FolderOpen className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium mb-2">No Project Selected</p>
        <p className="text-sm mb-4">Select a project to view external logs</p>
        {/* Show error if config failed to load */}
        {error && (
          <div className="flex flex-col gap-2 px-3 py-2 bg-destructive/10 text-destructive text-sm rounded-md mt-4 max-w-2xl">
            <div className="flex items-center gap-2">
              <AlertCircle className="w-4 h-4 shrink-0" />
              <span className="font-medium">Error loading project logs</span>
            </div>
            <div className="text-xs text-left ml-6 font-mono bg-destructive/5 p-2 rounded">
              {error}
            </div>
            <div className="text-xs text-left ml-6 text-muted-foreground">
              The runner may need to be rebuilt if Rust commands were recently added.
            </div>
          </div>
        )}
      </div>
    );
  }

  // No sources available (either none configured globally, or none selected for this project)
  if (safeSources.length === 0 && !loading) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <FileText className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium mb-2">No Log Sources Available</p>
        <p className="text-sm mb-4">
          Select log sources for this project or configure them in Settings
        </p>
        <button
          onClick={onConfigureSources}
          className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
        >
          <Settings className="w-4 h-4" />
          Select Log Sources
        </button>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between pb-3 border-b border-border">
        <div className="flex items-center gap-3">
          {/* Source selector tabs */}
          <div className="flex gap-1">
            {safeSources.map((source) => {
              const hasError = !!source.error;
              return (
                <button
                  key={source.sourceId}
                  onClick={() => setSelectedSourceId(source.sourceId)}
                  className={`
                    flex items-center gap-2 px-3 py-1.5 text-sm rounded-md transition-colors
                    ${
                      selectedSourceId === source.sourceId
                        ? "bg-primary text-primary-foreground"
                        : "bg-muted hover:bg-muted/80 text-muted-foreground"
                    }
                    ${hasError ? "opacity-70" : ""}
                  `}
                >
                  {hasError && (
                    <AlertCircle className={`w-3 h-3 ${getStatusColors("warning").icon}`} />
                  )}
                  {source.sourceName}
                  <span className="text-xs opacity-70">({source.totalLines})</span>
                </button>
              );
            })}
          </div>
        </div>

        <div className="flex items-center gap-2">
          {/* Auto-refresh toggle */}
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
              className="rounded border-border"
            />
            Auto-refresh
          </label>

          {autoRefresh && (
            <select
              aria-label="Auto-refresh interval"
              value={autoRefreshInterval}
              onChange={(e) => setAutoRefreshInterval(Number(e.target.value))}
              className="text-xs bg-muted border border-border rounded px-2 py-1"
            >
              <option value={1000}>1s</option>
              <option value={2000}>2s</option>
              <option value={5000}>5s</option>
              <option value={10000}>10s</option>
            </select>
          )}

          {/* Manual refresh button */}
          <button
            onClick={onRefresh}
            disabled={loading}
            className="flex items-center gap-2 px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md transition-colors disabled:opacity-50"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
            Refresh
          </button>

          {/* Configure sources button */}
          <button
            onClick={onConfigureSources}
            className="flex items-center gap-2 px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md transition-colors"
          >
            <Settings className="w-4 h-4" />
            Configure
          </button>
        </div>
      </div>

      {/* Project directories info panel */}
      {config && showProjectInfo && (
        <div className="mt-3 p-3 bg-muted/30 border border-border rounded-lg">
          <div className="flex items-center justify-between mb-2">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Info className="w-4 h-4 text-primary" />
              Project Directories
            </div>
            <button
              onClick={() => setShowProjectInfo(false)}
              className="text-xs text-muted-foreground hover:text-foreground transition-colors"
            >
              Hide
            </button>
          </div>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-2 text-xs">
            {/* Logs directory */}
            <div className="flex items-center gap-2 p-2 bg-background rounded border border-border">
              <FileText className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0`} />
              <div className="flex-1 min-w-0">
                <div className="text-muted-foreground mb-0.5">Runner Logs</div>
                <div className="truncate font-mono" title={config.logDirectory}>
                  {config.logDirectory}
                </div>
              </div>
              <button
                onClick={() => handleCopyPath(config.logDirectory)}
                className="p-1 hover:bg-muted rounded transition-colors"
                title="Copy path"
              >
                {copiedPath === config.logDirectory ? (
                  <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
                ) : (
                  <Copy className="w-3 h-3 text-muted-foreground" />
                )}
              </button>
            </div>

            {/* Screenshots directory */}
            <div className="flex items-center gap-2 p-2 bg-background rounded border border-border">
              <Camera className={`w-4 h-4 ${getAccentColors("purple").text} shrink-0`} />
              <div className="flex-1 min-w-0">
                <div className="text-muted-foreground mb-0.5">Screenshots</div>
                <div className="truncate font-mono" title={config.screenshotDirectory}>
                  {config.screenshotDirectory}
                </div>
              </div>
              <button
                onClick={() => handleCopyPath(config.screenshotDirectory)}
                className="p-1 hover:bg-muted rounded transition-colors"
                title="Copy path"
              >
                {copiedPath === config.screenshotDirectory ? (
                  <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
                ) : (
                  <Copy className="w-3 h-3 text-muted-foreground" />
                )}
              </button>
            </div>

            {/* AI Output directory */}
            <div className="flex items-center gap-2 p-2 bg-background rounded border border-border">
              <Sparkles className={`w-4 h-4 ${getAccentColors("amber").text} shrink-0`} />
              <div className="flex-1 min-w-0">
                <div className="text-muted-foreground mb-0.5">AI Output</div>
                <div className="truncate font-mono" title={config.aiOutputDirectory}>
                  {config.aiOutputDirectory}
                </div>
              </div>
              <button
                onClick={() => handleCopyPath(config.aiOutputDirectory)}
                className="p-1 hover:bg-muted rounded transition-colors"
                title="Copy path"
              >
                {copiedPath === config.aiOutputDirectory ? (
                  <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
                ) : (
                  <Copy className="w-3 h-3 text-muted-foreground" />
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Toggle to show project info if hidden */}
      {config && !showProjectInfo && (
        <button
          onClick={() => setShowProjectInfo(true)}
          className="mt-2 text-xs text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1"
        >
          <Info className="w-3 h-3" />
          Show project directories
        </button>
      )}

      {/* Error banner - show user-friendly messages for known errors */}
      {error && (
        <div className="flex items-center gap-2 px-3 py-2 bg-destructive/10 text-destructive text-sm rounded-md mt-2">
          <AlertCircle className="w-4 h-4" />
          {error.includes("projectId") || error.includes("missing required key")
            ? "Please select a project in the Settings tab to view external logs."
            : error.includes("not found") || error.includes("No such file")
              ? "Log file not found. The file may not exist yet or the path may be incorrect."
              : error.includes("permission") || error.includes("Permission denied")
                ? "Permission denied. Cannot read the log file."
                : error}
        </div>
      )}

      {/* Log content */}
      <div ref={logViewerRef} className="flex-1 mt-3 overflow-auto">
        {selectedSource ? (
          <div className="space-y-1">
            {selectedSource.error ? (
              <div
                className={`flex items-center gap-2 ${getStatusColors("warning").icon} text-sm p-4`}
              >
                <AlertCircle className="w-4 h-4" />
                {selectedSource.error}
              </div>
            ) : selectedSource.lines.length === 0 ? (
              <div className="text-muted-foreground text-sm text-center py-8">
                No log entries found
              </div>
            ) : (
              <pre className="font-mono text-xs leading-relaxed whitespace-pre-wrap">
                {selectedSource.lines.map((line, index) => (
                  <div key={`line-${index}`} className="hover:bg-muted/50 px-2 py-0.5">
                    {line}
                  </div>
                ))}
              </pre>
            )}
          </div>
        ) : (
          <div className="text-muted-foreground text-sm text-center py-8">
            Select a log source to view
          </div>
        )}
      </div>

      {/* Footer with file path and last refresh */}
      <div className="flex items-center justify-between pt-2 border-t border-border text-xs text-muted-foreground">
        <span className="truncate" title={selectedSource?.filePath}>
          {selectedSource?.filePath || "No source selected"}
        </span>
        <span>
          {lastRefresh
            ? `Last refresh: ${new Date(lastRefresh).toLocaleTimeString()}`
            : "Not refreshed"}
        </span>
      </div>
    </div>
  );
}
