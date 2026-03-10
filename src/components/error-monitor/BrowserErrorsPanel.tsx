/**
 * BrowserErrorsPanel.tsx
 *
 * Collapsible panel that displays browser-side error data from the UI Bridge
 * SDK. Shows health status (healthy/degraded/broken), recent browser errors
 * with severity and source location, and error snapshots.
 *
 * Fetches from the runner proxy endpoints:
 *   GET /ui-bridge/sdk/console/health
 *   GET /ui-bridge/sdk/console/browser-events
 */

import { useState } from "react";
import {
  AlertCircle,
  AlertTriangle,
  Bug,
  CheckCircle,
  ChevronDown,
  Clock,
  Globe,
  RefreshCw,
  Unplug,
  Zap,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { useBrowserErrors } from "../../hooks/useBrowserErrors";
import type {
  BrowserHealthStatus,
  BrowserCapturedEvent,
  FingerprintedEvent,
} from "../../types/browserErrors";

// =============================================================================
// Sub-components
// =============================================================================

function HealthBadge({ status, score }: { status: BrowserHealthStatus; score: number }) {
  const config: Record<BrowserHealthStatus, { label: string; className: string }> = {
    healthy: {
      label: "Healthy",
      className: "bg-green-500/20 text-green-500 border-green-500/30",
    },
    degraded: {
      label: "Degraded",
      className: "bg-yellow-500/20 text-yellow-500 border-yellow-500/30",
    },
    broken: {
      label: "Broken",
      className: "bg-red-500/20 text-red-500 border-red-500/30",
    },
  };

  const c = config[status] || config.healthy;

  return (
    <span
      data-content-role="badge"
      data-content-label="browser health status"
      className={cn("px-2 py-0.5 rounded-md text-xs font-medium border", c.className)}
    >
      {c.label} ({score}/100)
    </span>
  );
}

function BrowserSeverityIcon({ type, level }: { type: string; level?: string }) {
  const className = "w-4 h-4";

  // Determine severity from event type or console level
  if (type === "react-error") {
    return <AlertCircle className={cn(className, "text-red-600")} />;
  }
  if (type === "resource-error") {
    return <AlertTriangle className={cn(className, "text-red-500")} />;
  }
  if (type === "network" || type === "ws-disconnection") {
    return <Zap className={cn(className, "text-orange-500")} />;
  }
  if (level === "error") {
    return <Bug className={cn(className, "text-red-500")} />;
  }
  if (level === "warn") {
    return <AlertTriangle className={cn(className, "text-yellow-500")} />;
  }
  return <Bug className={cn(className, "text-red-500")} />;
}

/**
 * Extract a human-readable message from a browser event.
 */
function getEventMessage(event: BrowserCapturedEvent): string {
  if (event.message) return event.message;
  if (event.errorMessage) return event.errorMessage;
  if (event.type === "network") {
    return `${event.method ?? "GET"} ${event.requestUrl ?? "unknown"} -> ${event.status ?? "no response"}`;
  }
  if (event.type === "resource-error") {
    return `Failed to load ${event.tagName ?? "resource"}: ${event.resourceUrl ?? "unknown"}`;
  }
  if (event.type === "react-error") {
    return event.errorMessage ?? "React error boundary triggered";
  }
  return `${event.type} event`;
}

/**
 * Extract a source location string from the event.
 */
function getEventSource(event: BrowserCapturedEvent): string | null {
  // For console events, the stack trace often contains the source location
  if (event.stack) {
    // Extract first meaningful line from stack
    const lines = event.stack.split("\n").filter((l) => l.trim().startsWith("at "));
    if (lines.length > 0) {
      const match = lines[0].match(/\((.+)\)/);
      if (match) return match[1];
      return lines[0].trim().replace(/^at\s+/, "");
    }
  }
  if (event.requestUrl) return event.requestUrl;
  if (event.resourceUrl) return event.resourceUrl;
  return null;
}

/**
 * Format a unix timestamp (ms) to a relative or short time string.
 */
function formatBrowserTime(timestampMs: number): string {
  const now = Date.now();
  const diffMs = now - timestampMs;
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);

  if (diffMins < 1) return "just now";
  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;

  return new Date(timestampMs).toLocaleTimeString();
}

/**
 * Get a human-readable label for the event type.
 */
function getEventTypeLabel(type: string): string {
  switch (type) {
    case "console":
      return "Console";
    case "network":
      return "Network";
    case "react-error":
      return "React Error";
    case "resource-error":
      return "Resource";
    case "ws-disconnection":
      return "WebSocket";
    case "long-task":
      return "Long Task";
    case "long-animation-frame":
      return "Long Frame";
    case "hmr":
      return "HMR";
    default:
      return type;
  }
}

function BrowserErrorItem({
  group,
  isExpanded,
  onToggleExpand,
}: {
  group: FingerprintedEvent;
  isExpanded: boolean;
  onToggleExpand: () => void;
}) {
  const event = group.event;
  const message = getEventMessage(event);
  const source = getEventSource(event);
  const typeLabel = getEventTypeLabel(event.type);

  return (
    <div
      className={cn(
        "border-b border-border/50 last:border-b-0",
        "hover:bg-muted/30 transition-colors",
      )}
    >
      {/* Header row */}
      <div
        role="button"
        tabIndex={0}
        className="flex items-start gap-3 px-4 py-2.5 cursor-pointer"
        onClick={onToggleExpand}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggleExpand();
          }
        }}
      >
        {/* Severity icon */}
        <div className="flex-shrink-0 mt-0.5">
          <BrowserSeverityIcon type={event.type} level={event.level} />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-0.5">
            <span
              data-content-role="badge"
              data-content-label="browser event type"
              className="px-1.5 py-0.5 rounded text-xs font-medium bg-muted text-muted-foreground"
            >
              {typeLabel}
            </span>
            {group.count > 1 && (
              <span className="text-xs text-muted-foreground">x{group.count}</span>
            )}
          </div>
          <p className="text-sm text-foreground line-clamp-2">{message}</p>
          <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
            {source && (
              <span className="truncate max-w-[300px]" title={source}>
                {source}
              </span>
            )}
            <span className="flex items-center gap-1 flex-shrink-0">
              <Clock className="w-3 h-3" />
              {formatBrowserTime(group.lastSeen)}
            </span>
          </div>
        </div>

        {/* Expand indicator */}
        <ChevronDown
          className={cn(
            "w-4 h-4 text-muted-foreground transition-transform flex-shrink-0",
            isExpanded && "rotate-180",
          )}
        />
      </div>

      {/* Expanded details */}
      {isExpanded && (
        <div className="px-4 pb-3 pl-11 space-y-2">
          {/* Full message */}
          <div>
            <span className="text-xs font-medium text-muted-foreground">Message</span>
            <p className="text-sm mt-1 whitespace-pre-wrap bg-muted/50 p-2 rounded">{message}</p>
          </div>

          {/* Stack trace */}
          {event.stack && (
            <div>
              <span className="text-xs font-medium text-muted-foreground">Stack Trace</span>
              <pre className="text-xs mt-1 bg-muted/50 p-2 rounded overflow-x-auto max-h-48">
                {event.stack}
              </pre>
            </div>
          )}

          {/* Component stack (React errors) */}
          {event.componentStack && (
            <div>
              <span className="text-xs font-medium text-muted-foreground">Component Stack</span>
              <pre className="text-xs mt-1 bg-muted/50 p-2 rounded overflow-x-auto max-h-32">
                {event.componentStack}
              </pre>
            </div>
          )}

          {/* URL */}
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Globe className="w-3 h-3" />
            <span>{event.url}</span>
          </div>

          {/* Occurrence info */}
          {group.count > 1 && (
            <div className="text-xs text-muted-foreground">
              Seen {group.count} times between {formatBrowserTime(group.firstSeen)} and{" "}
              {formatBrowserTime(group.lastSeen)}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function NoConnectionState() {
  return (
    <div className="flex flex-col items-center justify-center text-center py-6 px-4">
      <div className="w-12 h-12 rounded-full bg-muted flex items-center justify-center mb-3">
        <Unplug className="w-6 h-6 text-muted-foreground" />
      </div>
      <h4 className="text-sm font-medium mb-1">No UI Bridge Connection</h4>
      <p className="text-xs text-muted-foreground max-w-xs">
        Connect to an app with the UI Bridge SDK to see browser-side errors. Use the UI Bridge tab
        to establish a connection.
      </p>
    </div>
  );
}

function HealthyEmptyState() {
  return (
    <div className="flex items-center gap-3 px-4 py-4">
      <div className="w-8 h-8 rounded-full bg-green-500/20 flex items-center justify-center flex-shrink-0">
        <CheckCircle className="w-4 h-4 text-green-500" />
      </div>
      <div>
        <p className="text-sm font-medium">No Browser Errors</p>
        <p className="text-xs text-muted-foreground">
          The connected app is healthy with no recent browser errors.
        </p>
      </div>
    </div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

interface BrowserErrorsPanelProps {
  /** Whether the panel starts collapsed. Default: false */
  defaultCollapsed?: boolean;
}

export function BrowserErrorsPanel({ defaultCollapsed = false }: BrowserErrorsPanelProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const { health, deduplicatedErrors, loading, error, noConnection, refresh } = useBrowserErrors({
    refreshInterval: 30000,
  });

  const errorCount = deduplicatedErrors.length;
  const hasErrors = errorCount > 0;

  return (
    <div className="border-b border-border">
      {/* Collapsible header */}
      <div
        role="button"
        tabIndex={0}
        className="flex items-center justify-between px-4 py-2.5 bg-card hover:bg-muted/30 cursor-pointer transition-colors"
        onClick={() => setCollapsed(!collapsed)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setCollapsed(!collapsed);
          }
        }}
      >
        <div className="flex items-center gap-2">
          <Globe className="w-4 h-4 text-primary" />
          <span className="text-sm font-medium">Browser Errors</span>
          {health && <HealthBadge status={health.status} score={health.score} />}
          {hasErrors && (
            <span
              data-content-role="badge"
              data-content-label="browser error count"
              className="px-2 py-0.5 text-xs bg-red-500/20 text-red-500 rounded-full"
            >
              {errorCount}
            </span>
          )}
          {noConnection && <span className="text-xs text-muted-foreground">(not connected)</span>}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={(e) => {
              e.stopPropagation();
              refresh();
            }}
            disabled={loading}
            className="p-1 hover:bg-muted rounded transition-colors"
            title="Refresh browser errors"
          >
            <RefreshCw className={cn("w-3.5 h-3.5", loading && "animate-spin")} />
          </button>
          <ChevronDown
            className={cn(
              "w-4 h-4 text-muted-foreground transition-transform",
              collapsed && "-rotate-90",
            )}
          />
        </div>
      </div>

      {/* Collapsible body */}
      {!collapsed && (
        <div className="border-t border-border/50">
          {/* Error state */}
          {error && (
            <div className="px-4 py-3 text-sm text-red-500 flex items-center gap-2">
              <AlertCircle className="w-4 h-4 flex-shrink-0" />
              <span>{error}</span>
            </div>
          )}

          {/* Loading state (initial only) */}
          {loading && !health && !noConnection && !error && (
            <div className="px-4 py-4 text-center text-muted-foreground">
              <RefreshCw className="w-5 h-5 mx-auto mb-1 animate-spin" />
              <p className="text-xs">Loading browser errors...</p>
            </div>
          )}

          {/* No connection */}
          {noConnection && <NoConnectionState />}

          {/* Connected: show content */}
          {health && !noConnection && (
            <>
              {/* Health summary bar */}
              <div className="px-4 py-2 bg-muted/20 flex items-center gap-4 text-xs text-muted-foreground">
                <span>
                  Score: <span className="font-medium text-foreground">{health.score}/100</span>
                </span>
                {health.breakdown && (
                  <>
                    {health.breakdown.crashes > 0 && (
                      <span className="text-red-600">
                        {health.breakdown.crashes} crash
                        {health.breakdown.crashes !== 1 ? "es" : ""}
                      </span>
                    )}
                    {health.breakdown.errors > 0 && (
                      <span className="text-red-500">
                        {health.breakdown.errors} error
                        {health.breakdown.errors !== 1 ? "s" : ""}
                      </span>
                    )}
                    {health.breakdown.warnings > 0 && (
                      <span className="text-yellow-500">
                        {health.breakdown.warnings} warning
                        {health.breakdown.warnings !== 1 ? "s" : ""}
                      </span>
                    )}
                  </>
                )}
                {health.errorRate > 0 && <span>{health.errorRate}/min</span>}
                <span className="ml-auto">{health.summary}</span>
              </div>

              {/* Top issue highlight */}
              {health.topIssue && (
                <div className="px-4 py-2 bg-red-500/5 border-b border-border/50 flex items-start gap-2">
                  <AlertCircle className="w-4 h-4 text-red-500 flex-shrink-0 mt-0.5" />
                  <div className="min-w-0">
                    <span className="text-xs font-medium text-red-500">Top Issue</span>
                    <p className="text-sm text-foreground line-clamp-2">
                      {health.topIssue.message}
                    </p>
                    <span className="text-xs text-muted-foreground">
                      {formatBrowserTime(health.topIssue.timestamp)}
                    </span>
                  </div>
                </div>
              )}

              {/* Error list */}
              {hasErrors ? (
                <div className="flex flex-col max-h-[400px] overflow-y-auto">
                  {deduplicatedErrors.map((group) => (
                    <BrowserErrorItem
                      key={group.fingerprint}
                      group={group}
                      isExpanded={expandedId === group.fingerprint}
                      onToggleExpand={() =>
                        setExpandedId(expandedId === group.fingerprint ? null : group.fingerprint)
                      }
                    />
                  ))}
                </div>
              ) : (
                <HealthyEmptyState />
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default BrowserErrorsPanel;
