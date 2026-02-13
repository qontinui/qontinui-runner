/**
 * AppSelector.tsx
 *
 * Two-slot picker for assigning discovered apps as Reference and Target.
 * Groups apps by type (Web / Desktop) and shows them as AppCards.
 */

import { Globe, Monitor, Loader2, RefreshCw, AlertCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import type { DiscoveredApp } from "@/hooks/useAppDiscovery";
import { AppCard } from "./AppCard";

interface AppSelectorProps {
  webApps: DiscoveredApp[];
  desktopApps: DiscoveredApp[];
  referenceApp: DiscoveredApp | null;
  targetApp: DiscoveredApp | null;
  onAssignReference: (app: DiscoveredApp) => void;
  onAssignTarget: (app: DiscoveredApp) => void;
  isScanning: boolean;
  error: string | null;
  onScan: () => void;
  /** Called to connect to an app via UI Bridge SDK. Returns true on success. */
  onConnect?: (app: DiscoveredApp) => Promise<boolean>;
  /** Set of app URLs that are already connected */
  connectedUrls?: Set<string>;
}

function getAppRole(
  app: DiscoveredApp,
  referenceApp: DiscoveredApp | null,
  targetApp: DiscoveredApp | null,
): "reference" | "target" | null {
  if (referenceApp && app.appId === referenceApp.appId) return "reference";
  if (targetApp && app.appId === targetApp.appId) return "target";
  return null;
}

export function AppSelector({
  webApps,
  desktopApps,
  referenceApp,
  targetApp,
  onAssignReference,
  onAssignTarget,
  isScanning,
  error,
  onScan,
  onConnect,
  connectedUrls,
}: AppSelectorProps) {
  const allApps = [...webApps, ...desktopApps];

  const handleAssign = (app: DiscoveredApp, role: "reference" | "target") => {
    if (role === "reference") onAssignReference(app);
    else onAssignTarget(app);
  };

  return (
    <div className="space-y-4">
      {/* Scan button + status */}
      <div className="flex items-center gap-3">
        <button
          onClick={onScan}
          disabled={isScanning}
          className="flex items-center gap-2 rounded-lg bg-brand-primary px-4 py-2.5 text-sm font-medium text-white hover:bg-brand-primary/90 disabled:opacity-50 transition-colors"
        >
          {isScanning ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <RefreshCw className="h-4 w-4" />
          )}
          {isScanning ? "Scanning..." : "Scan for Apps"}
        </button>
        {allApps.length > 0 && (
          <span className="text-xs text-text-muted">
            Found {allApps.length} app{allApps.length !== 1 ? "s" : ""}
          </span>
        )}
      </div>

      {/* Error */}
      {error && (
        <div className="flex items-center gap-2 rounded-lg bg-error/10 border border-error/20 px-3 py-2">
          <AlertCircle className="h-4 w-4 text-error shrink-0" />
          <span className="text-xs text-error">{error}</span>
        </div>
      )}

      {/* Selection summary */}
      {(referenceApp || targetApp) && (
        <div className="flex gap-3">
          <div
            className={cn(
              "flex-1 rounded-lg border p-2 text-xs",
              referenceApp
                ? "border-blue-500/50 bg-blue-500/5"
                : "border-dashed border-border bg-surface-canvas",
            )}
          >
            <span className="text-[10px] font-semibold uppercase tracking-wider text-blue-400">
              Reference
            </span>
            {referenceApp ? (
              <p className="mt-0.5 text-text-primary truncate">{referenceApp.appName}</p>
            ) : (
              <p className="mt-0.5 text-text-muted">Select an app</p>
            )}
          </div>
          <div
            className={cn(
              "flex-1 rounded-lg border p-2 text-xs",
              targetApp
                ? "border-amber-500/50 bg-amber-500/5"
                : "border-dashed border-border bg-surface-canvas",
            )}
          >
            <span className="text-[10px] font-semibold uppercase tracking-wider text-amber-400">
              Target
            </span>
            {targetApp ? (
              <p className="mt-0.5 text-text-primary truncate">{targetApp.appName}</p>
            ) : (
              <p className="mt-0.5 text-text-muted">Select an app</p>
            )}
          </div>
        </div>
      )}

      {/* Web Apps group */}
      {webApps.length > 0 && (
        <div>
          <div className="mb-2 flex items-center gap-2">
            <Globe className="h-3.5 w-3.5 text-brand-primary" />
            <span className="text-xs font-medium uppercase tracking-wider text-text-muted">
              Web Apps
            </span>
            <span className="text-[10px] text-text-muted">({webApps.length})</span>
          </div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {webApps.map((app) => (
              <AppCard
                key={app.appId}
                app={app}
                role={getAppRole(app, referenceApp, targetApp)}
                onAssign={(role) => handleAssign(app, role)}
                onConnect={onConnect}
                isConnected={connectedUrls?.has(app.url.replace(/\/+$/, ""))}
              />
            ))}
          </div>
        </div>
      )}

      {/* Desktop Apps group */}
      {desktopApps.length > 0 && (
        <div>
          <div className="mb-2 flex items-center gap-2">
            <Monitor className="h-3.5 w-3.5 text-brand-secondary" />
            <span className="text-xs font-medium uppercase tracking-wider text-text-muted">
              Desktop Apps
            </span>
            <span className="text-[10px] text-text-muted">({desktopApps.length})</span>
          </div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {desktopApps.map((app) => (
              <AppCard
                key={app.appId}
                app={app}
                role={getAppRole(app, referenceApp, targetApp)}
                onAssign={(role) => handleAssign(app, role)}
                onConnect={onConnect}
                isConnected={connectedUrls?.has(app.url.replace(/\/+$/, ""))}
              />
            ))}
          </div>
        </div>
      )}

      {/* Empty state */}
      {!isScanning && allApps.length === 0 && !error && (
        <div className="rounded-lg border border-dashed border-border p-8 text-center">
          <RefreshCw className="mx-auto h-8 w-8 text-text-muted/30 mb-3" />
          <p className="text-sm text-text-muted">No apps discovered yet</p>
          <p className="text-xs text-text-muted/70 mt-1">
            Click "Scan for Apps" to find UI Bridge-enabled applications
          </p>
        </div>
      )}
    </div>
  );
}
