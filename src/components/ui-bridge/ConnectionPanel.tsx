/**
 * Connection Panel
 *
 * Shows the UI Bridge extension connection status and allows
 * selecting a browser tab to inspect.
 */

import { useState, useCallback } from "react";
import { Card, CardHeader, CardTitle, CardContent } from "../ui/Card";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  Globe,
  RefreshCw,
  Link,
  Unlink,
  Chrome,
  AlertCircle,
  Check,
  ChevronDown,
  ChevronRight,
  ExternalLink,
} from "lucide-react";
import type { BrowserTab, ConnectionStatus } from "../../hooks/useExternalUIBridge";

interface ConnectionPanelProps {
  connectionStatus: ConnectionStatus;
  isExtensionConnected: boolean;
  connectedTabInfo: BrowserTab | null;
  browserTabs: BrowserTab[];
  isLoadingTabs: boolean;
  error: string | null;
  elementCount: number;
  onRefreshTabs: () => Promise<void>;
  onConnectToTab: (tabId: number) => Promise<void>;
  onDisconnect: () => void;
  onCheckExtension: () => Promise<boolean>;
}

export function ConnectionPanel({
  connectionStatus,
  isExtensionConnected,
  connectedTabInfo,
  browserTabs,
  isLoadingTabs,
  error,
  elementCount,
  onRefreshTabs,
  onConnectToTab,
  onDisconnect,
  onCheckExtension,
}: ConnectionPanelProps) {
  const [showTabList, setShowTabList] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);

  const handleRefreshTabs = useCallback(async () => {
    await onCheckExtension();
    await onRefreshTabs();
    setShowTabList(true);
  }, [onCheckExtension, onRefreshTabs]);

  const handleConnectToTab = useCallback(
    async (tabId: number) => {
      setIsConnecting(true);
      try {
        await onConnectToTab(tabId);
        setShowTabList(false);
      } finally {
        setIsConnecting(false);
      }
    },
    [onConnectToTab],
  );

  const getStatusBadge = () => {
    switch (connectionStatus) {
      case "connected":
        return (
          <Badge variant="success" className="gap-1">
            <Check className="w-3 h-3" />
            Connected
          </Badge>
        );
      case "connecting":
        return (
          <Badge variant="warning" className="gap-1">
            <RefreshCw className="w-3 h-3 animate-spin" />
            Connecting
          </Badge>
        );
      case "error":
        return (
          <Badge variant="danger" className="gap-1">
            <AlertCircle className="w-3 h-3" />
            Error
          </Badge>
        );
      default:
        return (
          <Badge variant="muted" className="gap-1">
            <Unlink className="w-3 h-3" />
            Disconnected
          </Badge>
        );
    }
  };

  const getExtensionBadge = () => {
    if (isExtensionConnected) {
      return (
        <Badge variant="outline" className="gap-1 text-green-500 border-green-500/30">
          <Chrome className="w-3 h-3" />
          Extension Ready
        </Badge>
      );
    }
    return (
      <Badge variant="outline" className="gap-1 text-muted-foreground">
        <Chrome className="w-3 h-3" />
        Extension Not Connected
      </Badge>
    );
  };

  return (
    <Card variant="borderless" className="mb-3">
      <CardHeader className="py-2">
        <CardTitle className="flex items-center gap-2 text-base">
          <Globe className="w-4 h-4 text-primary" />
          Browser Connection
        </CardTitle>
        <div className="flex items-center gap-2">
          {getStatusBadge()}
          {getExtensionBadge()}
        </div>
      </CardHeader>

      <CardContent className="pt-0 pb-3 space-y-3">
        {/* Connected tab info */}
        {connectedTabInfo && connectionStatus === "connected" && (
          <div className="p-2 bg-accent/10 border border-accent/20 rounded-md">
            <div className="flex items-start gap-2">
              <Link className="w-4 h-4 text-accent mt-0.5 shrink-0" />
              <div className="flex-1 min-w-0">
                <div className="font-medium text-sm truncate">{connectedTabInfo.title}</div>
                <div className="text-xs text-muted-foreground truncate flex items-center gap-1">
                  <span>{connectedTabInfo.url}</span>
                  <a
                    href={connectedTabInfo.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-primary hover:underline inline-flex"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <ExternalLink className="w-3 h-3" />
                  </a>
                </div>
                <div className="text-xs text-muted-foreground mt-1">
                  {elementCount} elements discovered
                </div>
              </div>
              <Button variant="outline" size="sm" onClick={onDisconnect}>
                Disconnect
              </Button>
            </div>
          </div>
        )}

        {/* Error message */}
        {error && (
          <div className="p-2 bg-destructive/10 border border-destructive/30 rounded-md text-sm text-destructive flex items-start gap-2">
            <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        {/* Tab selection */}
        {connectionStatus !== "connected" && (
          <div>
            <div className="flex items-center gap-2 mb-2">
              <Button
                variant="outline"
                size="sm"
                onClick={handleRefreshTabs}
                disabled={isLoadingTabs}
              >
                <RefreshCw className={`w-3.5 h-3.5 mr-1 ${isLoadingTabs ? "animate-spin" : ""}`} />
                {isLoadingTabs ? "Loading..." : "Refresh Tabs"}
              </Button>
              {browserTabs.length > 0 && (
                <button
                  className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                  onClick={() => setShowTabList(!showTabList)}
                >
                  {showTabList ? (
                    <ChevronDown className="w-3.5 h-3.5" />
                  ) : (
                    <ChevronRight className="w-3.5 h-3.5" />
                  )}
                  {browserTabs.length} tab{browserTabs.length !== 1 ? "s" : ""} available
                </button>
              )}
            </div>

            {/* Tab list */}
            {showTabList && browserTabs.length > 0 && (
              <div className="space-y-1 max-h-48 overflow-y-auto">
                {browserTabs.map((tab) => (
                  <button
                    key={tab.id}
                    className="w-full p-2 text-left bg-muted/30 hover:bg-muted/50 rounded-md transition-colors disabled:opacity-50"
                    onClick={() => handleConnectToTab(tab.id)}
                    disabled={isConnecting || connectionStatus === "connecting"}
                  >
                    <div className="flex items-start gap-2">
                      {tab.favIconUrl ? (
                        <img
                          src={tab.favIconUrl}
                          alt=""
                          className="w-4 h-4 mt-0.5 shrink-0"
                          onError={(e) => {
                            (e.target as HTMLImageElement).style.display = "none";
                          }}
                        />
                      ) : (
                        <Globe className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
                      )}
                      <div className="flex-1 min-w-0">
                        <div className="text-sm font-medium truncate">{tab.title}</div>
                        <div className="text-xs text-muted-foreground truncate">{tab.url}</div>
                      </div>
                      {tab.active && (
                        <Badge variant="muted" className="text-[10px]">
                          Active
                        </Badge>
                      )}
                    </div>
                  </button>
                ))}
              </div>
            )}

            {/* No tabs message */}
            {showTabList && browserTabs.length === 0 && !isLoadingTabs && (
              <div className="text-sm text-muted-foreground text-center py-4">
                {isExtensionConnected
                  ? "No browser tabs found. Open a page in your browser."
                  : "Extension not connected. Make sure the Qontinui DevTools extension is installed and enabled."}
              </div>
            )}

            {/* Help text */}
            {!showTabList && !isExtensionConnected && (
              <div className="text-xs text-muted-foreground">
                <p>To inspect browser elements:</p>
                <ol className="list-decimal list-inside mt-1 space-y-0.5">
                  <li>Install the Qontinui DevTools browser extension</li>
                  <li>Open a page in your browser</li>
                  <li>Click "Refresh Tabs" above to see available pages</li>
                </ol>
              </div>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export default ConnectionPanel;
