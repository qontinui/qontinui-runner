/**
 * SdkAppConnector — Reusable component for scanning, discovering, and
 * connecting to UI Bridge SDK-enabled applications.
 *
 * Used by: Architecture page (SDK Projects), UI Bridge States page, etc.
 *
 * Renders a compact card with:
 *  - Scan button to find SDK apps
 *  - List of discovered apps with Connect buttons
 *  - Connected app banner with Disconnect option
 */

import { useState, useCallback, useEffect } from "react";
import { Scan, Loader2, CheckCircle2, Plug, Unplug } from "lucide-react";
import { useAppDiscovery, type DiscoveredApp } from "@/hooks/useAppDiscovery";
import { useSdkUIBridge } from "@/hooks/useSdkUIBridge";

// ============================================================================
// Types
// ============================================================================

export interface SdkAppConnectorProps {
  /** Called when an app is successfully connected */
  onConnected?: (app: DiscoveredApp) => void;
  /** Called when disconnected */
  onDisconnected?: () => void;
  /** Auto-scan for apps on mount (default: true) */
  autoScan?: boolean;
  /** Compact mode — less vertical padding (default: false) */
  compact?: boolean;
}

export interface SdkAppConnection {
  /** The connected app, or null */
  connectedApp: DiscoveredApp | null;
  /** Whether an app is currently connected */
  isConnected: boolean;
  /** All discovered apps */
  allApps: DiscoveredApp[];
}

// ============================================================================
// Component
// ============================================================================

export function SdkAppConnector({
  onConnected,
  onDisconnected,
  autoScan = true,
  compact = false,
}: SdkAppConnectorProps) {
  const appDiscovery = useAppDiscovery();
  const sdk = useSdkUIBridge();
  const [connectingAppId, setConnectingAppId] = useState<string | null>(null);
  const [connectedUrl, setConnectedUrl] = useState<string | null>(null);

  const allApps: DiscoveredApp[] = [...appDiscovery.webApps, ...appDiscovery.desktopApps];

  const isConnected = sdk.connectionStatus === "connected";

  // Auto-scan on mount
  useEffect(() => {
    if (autoScan && allApps.length === 0 && !appDiscovery.isScanning) {
      appDiscovery.scanAll();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleConnect = useCallback(
    async (app: DiscoveredApp) => {
      setConnectingAppId(app.appId);
      try {
        const success = await sdk.connect(app.url, {
          appId: app.appId,
          appName: app.appName,
          appType: app.appType,
          framework: app.framework,
          version: app.version,
          capabilities: app.capabilities,
          port: app.port,
        });
        if (success) {
          setConnectedUrl(app.url);
          onConnected?.(app);
        }
      } finally {
        setConnectingAppId(null);
      }
    },
    [sdk, onConnected],
  );

  const handleDisconnect = useCallback(() => {
    if (connectedUrl) {
      sdk.disconnect(connectedUrl);
      setConnectedUrl(null);
      onDisconnected?.();
    }
  }, [sdk, connectedUrl, onDisconnected]);

  const padding = compact ? "p-3" : "p-4";

  return (
    <div className={`rounded-lg border border-border/50 bg-card ${padding} space-y-2.5`}>
      <div className="flex items-center gap-2">
        <Plug className="w-4 h-4 text-muted-foreground" />
        <span className="text-sm font-medium">Connect to SDK App</span>
      </div>

      {/* Scan button + status */}
      <div className="flex items-center gap-3">
        <button
          onClick={() => appDiscovery.scanAll()}
          disabled={appDiscovery.isScanning}
          className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium border border-border text-foreground hover:bg-muted disabled:opacity-50 transition-colors"
        >
          {appDiscovery.isScanning ? (
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <Scan className="w-3.5 h-3.5" />
          )}
          Scan for Apps
        </button>
        {appDiscovery.lastScanAt && !appDiscovery.isScanning && (
          <span className="text-xs text-muted-foreground">
            {allApps.length} app{allApps.length !== 1 ? "s" : ""} found
          </span>
        )}
      </div>

      {/* Connected banner */}
      {isConnected && sdk.connectedApp && (
        <div className="flex items-center justify-between gap-2 p-2 bg-green-500/10 border border-green-500/20 rounded-md">
          <div className="flex items-center gap-2 min-w-0">
            <CheckCircle2 className="w-4 h-4 text-green-500 shrink-0" />
            <span className="text-sm truncate">
              Connected: <strong>{sdk.connectedApp.appName}</strong>
            </span>
          </div>
          <button
            onClick={handleDisconnect}
            className="shrink-0 inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
          >
            <Unplug className="w-3 h-3" />
            Disconnect
          </button>
        </div>
      )}

      {/* Discovered apps list — hidden when connected */}
      {!isConnected && allApps.length > 0 && (
        <div className="space-y-1.5">
          {allApps.map((app) => (
            <div
              key={app.appId}
              className="flex items-center justify-between gap-2 p-2 border border-border/50 rounded-md"
            >
              <div className="min-w-0">
                <div className="text-sm font-medium truncate">{app.appName}</div>
                <div className="text-xs text-muted-foreground truncate">
                  :{app.port} {app.framework && `· ${app.framework}`}
                  {app.elementCount != null && ` · ${app.elementCount} elements`}
                </div>
              </div>
              <button
                onClick={() => handleConnect(app)}
                disabled={connectingAppId === app.appId}
                className="shrink-0 inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium bg-blue-600/20 text-blue-400 hover:bg-blue-600/30 border border-blue-600/30 transition-colors disabled:opacity-50"
              >
                {connectingAppId === app.appId ? (
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                ) : null}
                Connect
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Empty state */}
      {!isConnected && allApps.length === 0 && !appDiscovery.isScanning && (
        <p className="text-xs text-muted-foreground">
          Click Scan to find apps with the UI Bridge SDK installed.
        </p>
      )}

      {/* Error */}
      {appDiscovery.error && <p className="text-xs text-red-400">{appDiscovery.error}</p>}
    </div>
  );
}
