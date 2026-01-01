/**
 * DiscoverySyncIndicator.tsx
 *
 * Compact indicator showing discovery sync status.
 * Can be used in status bars or headers.
 */

import { Cloud, CloudOff, AlertCircle } from "lucide-react";
import { useSyncStatus } from "../../hooks/useDiscoveries";

interface DiscoverySyncIndicatorProps {
  onClick?: () => void;
  showLabel?: boolean;
}

export function DiscoverySyncIndicator({ onClick, showLabel = true }: DiscoverySyncIndicatorProps) {
  const { data: status, isLoading } = useSyncStatus();

  if (isLoading || !status) {
    return null;
  }

  // Don't show anything if no pending discoveries and authenticated
  if (status.pending_count === 0 && status.authenticated) {
    return null;
  }

  const hasIssue = !status.authenticated || status.pending_count > 0;

  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${
        hasIssue ? "text-yellow-500 hover:bg-yellow-500/10" : "text-muted-foreground hover:bg-muted"
      }`}
      title={
        !status.authenticated
          ? "Not connected - discoveries won't sync"
          : `${status.pending_count} discoveries pending sync`
      }
    >
      {!status.authenticated ? (
        <CloudOff className="w-3.5 h-3.5" />
      ) : status.pending_count > 0 ? (
        <AlertCircle className="w-3.5 h-3.5" />
      ) : (
        <Cloud className="w-3.5 h-3.5" />
      )}
      {showLabel && status.pending_count > 0 && <span>{status.pending_count}</span>}
    </button>
  );
}
