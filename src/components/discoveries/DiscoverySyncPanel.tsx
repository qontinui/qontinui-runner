/**
 * DiscoverySyncPanel.tsx
 *
 * Panel showing discovery sync status and pending discoveries.
 * Allows manual sync triggering and clearing of discoveries.
 */

import { useState } from "react";
import {
  Cloud,
  CloudOff,
  RefreshCw,
  Trash2,
  AlertCircle,
  CheckCircle2,
  Clock,
  ChevronRight,
  X,
} from "lucide-react";
import {
  useDiscoverySummary,
  useSyncDiscoveries,
  useClearDiscovery,
  useClearFailedDiscoveries,
} from "../../hooks/useDiscoveries";
import {
  getDiscoveryTypeLabel,
  getDiscoveryTypeColor,
  formatConfidence,
  type DiscoveryPreview,
  type DiscoveryType,
} from "../../types/discoveries";

export function DiscoverySyncPanel() {
  const { data: summary, isLoading, refetch } = useDiscoverySummary();
  const syncMutation = useSyncDiscoveries();
  const clearMutation = useClearDiscovery();
  const clearFailedMutation = useClearFailedDiscoveries();
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <div className="flex items-center gap-2 text-muted-foreground">
          <RefreshCw className="w-4 h-4 animate-spin" />
          <span>Loading discovery status...</span>
        </div>
      </div>
    );
  }

  if (!summary) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <div className="flex items-center gap-2 text-muted-foreground">
          <CloudOff className="w-4 h-4" />
          <span>No discovery data available</span>
        </div>
      </div>
    );
  }

  const hasFailedDiscoveries = summary.recent.some((d) => d.attempt_count > 0 && d.last_error);

  return (
    <div className="rounded-lg border border-border bg-card">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <div className="flex items-center gap-3">
          {summary.can_sync ? (
            <Cloud className="w-5 h-5 text-green-500" />
          ) : (
            <CloudOff className="w-5 h-5 text-muted-foreground" />
          )}
          <div>
            <h3 className="font-medium">Discovery Sync</h3>
            <p className="text-sm text-muted-foreground">
              {summary.pending_count === 0
                ? "No pending discoveries"
                : `${summary.pending_count} pending, ${summary.ready_for_sync} ready`}
            </p>
          </div>
        </div>

        <div className="flex items-center gap-2">
          {hasFailedDiscoveries && (
            <button
              onClick={() => clearFailedMutation.mutate()}
              disabled={clearFailedMutation.isPending}
              className="p-2 rounded-md hover:bg-muted transition-colors text-muted-foreground hover:text-destructive"
              title="Clear failed discoveries"
            >
              <Trash2 className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={() => refetch()}
            className="p-2 rounded-md hover:bg-muted transition-colors"
            title="Refresh status"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
          <button
            onClick={() => syncMutation.mutate()}
            disabled={!summary.can_sync || summary.ready_for_sync === 0 || syncMutation.isPending}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {syncMutation.isPending ? (
              <>
                <RefreshCw className="w-4 h-4 animate-spin" />
                <span>Syncing...</span>
              </>
            ) : (
              <>
                <Cloud className="w-4 h-4" />
                <span>Sync Now</span>
              </>
            )}
          </button>
        </div>
      </div>

      {/* Sync result message */}
      {syncMutation.isSuccess && (
        <div className="px-4 py-2 bg-green-500/10 border-b border-green-500/20 flex items-center gap-2 text-sm text-green-500">
          <CheckCircle2 className="w-4 h-4" />
          <span>
            Synced {syncMutation.data.sent} discoveries
            {syncMutation.data.failed > 0 && `, ${syncMutation.data.failed} failed`}
          </span>
        </div>
      )}

      {syncMutation.isError && (
        <div className="px-4 py-2 bg-destructive/10 border-b border-destructive/20 flex items-center gap-2 text-sm text-destructive">
          <AlertCircle className="w-4 h-4" />
          <span>
            {syncMutation.error instanceof Error ? syncMutation.error.message : "Sync failed"}
          </span>
        </div>
      )}

      {/* Not authenticated warning */}
      {!summary.can_sync && summary.pending_count > 0 && (
        <div className="px-4 py-2 bg-yellow-500/10 border-b border-yellow-500/20 flex items-center gap-2 text-sm text-yellow-600">
          <AlertCircle className="w-4 h-4" />
          <span>Not authenticated. Connect to qontinui-web to sync discoveries.</span>
        </div>
      )}

      {/* Recent discoveries list */}
      {summary.recent.length > 0 && (
        <div className="divide-y divide-border">
          {summary.recent.map((discovery) => (
            <DiscoveryItem
              key={discovery.id}
              discovery={discovery}
              expanded={expandedId === discovery.id}
              onToggle={() => setExpandedId(expandedId === discovery.id ? null : discovery.id)}
              onClear={() => clearMutation.mutate(discovery.id)}
              isClearing={clearMutation.isPending}
            />
          ))}
        </div>
      )}

      {/* Empty state */}
      {summary.pending_count === 0 && (
        <div className="p-8 text-center text-muted-foreground">
          <CheckCircle2 className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>All discoveries synced</p>
        </div>
      )}
    </div>
  );
}

interface DiscoveryItemProps {
  discovery: DiscoveryPreview;
  expanded: boolean;
  onToggle: () => void;
  onClear: () => void;
  isClearing: boolean;
}

function DiscoveryItem({ discovery, expanded, onToggle, onClear, isClearing }: DiscoveryItemProps) {
  const hasError = discovery.attempt_count > 0 && discovery.last_error;

  return (
    <div className="p-3">
      <div className="flex items-center gap-3 cursor-pointer" onClick={onToggle}>
        <ChevronRight className={`w-4 h-4 transition-transform ${expanded ? "rotate-90" : ""}`} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span
              className={`text-xs font-medium px-2 py-0.5 rounded ${getDiscoveryTypeColor(
                discovery.discovery_type as DiscoveryType,
              )} bg-current/10`}
            >
              {getDiscoveryTypeLabel(discovery.discovery_type as DiscoveryType)}
            </span>
            <span className="font-medium truncate">{discovery.title}</span>
          </div>
          <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
            <span>Confidence: {formatConfidence(discovery.confidence)}</span>
            <span>{discovery.runs_observed} runs</span>
            {discovery.attempt_count > 0 && (
              <span className="flex items-center gap-1">
                <Clock className="w-3 h-3" />
                {discovery.attempt_count} attempts
              </span>
            )}
          </div>
        </div>

        {hasError && <AlertCircle className="w-4 h-4 text-destructive flex-shrink-0" />}

        <button
          onClick={(e) => {
            e.stopPropagation();
            onClear();
          }}
          disabled={isClearing}
          className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-destructive transition-colors"
          title="Remove from queue"
        >
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Expanded details */}
      {expanded && (
        <div className="mt-3 ml-7 p-3 rounded bg-muted/50 text-sm">
          <dl className="grid grid-cols-2 gap-2">
            <dt className="text-muted-foreground">ID:</dt>
            <dd className="font-mono text-xs">{discovery.id}</dd>
            <dt className="text-muted-foreground">Created:</dt>
            <dd>{new Date(discovery.created_at).toLocaleString()}</dd>
            {hasError && (
              <>
                <dt className="text-muted-foreground">Last Error:</dt>
                <dd className="text-destructive col-span-2 break-all">{discovery.last_error}</dd>
              </>
            )}
          </dl>
        </div>
      )}
    </div>
  );
}
