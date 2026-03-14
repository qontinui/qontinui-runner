import { RefreshCw, Wifi, WifiOff, FileCode, Search } from "lucide-react";
import { useState, useCallback } from "react";
import type { DiscoveredApp, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";

interface DiscoveryPanelProps {
  onIntegrate?: (projectPath: string) => void;
}

export function DiscoveryPanel({ onIntegrate }: DiscoveryPanelProps) {
  const [apps, setApps] = useState<DiscoveredApp[]>([]);
  const [scanning, setScanning] = useState(false);
  const [lastScan, setLastScan] = useState<number | null>(null);

  const scan = useCallback(async () => {
    setScanning(true);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/apps/scan`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      const data: ApiResponse<{ apps: DiscoveredApp[] }> = await resp.json();
      if (data.success && data.data) {
        setApps(data.data.apps || []);
      }
      setLastScan(Date.now());
    } catch (err) {
      console.error("Scan failed:", err);
    } finally {
      setScanning(false);
    }
  }, []);

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium">App Discovery</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Scan for running web applications and check their UI Bridge status
          </p>
        </div>
        <button
          onClick={scan}
          disabled={scanning}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                     bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                     hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
        >
          {scanning ? (
            <RefreshCw className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <Search className="w-3.5 h-3.5" />
          )}
          {scanning ? "Scanning..." : "Scan"}
        </button>
      </div>

      {/* Results */}
      {apps.length === 0 && !scanning && (
        <div className="text-center py-12 text-muted-foreground text-sm">
          {lastScan
            ? "No apps found. Make sure your web applications are running."
            : 'Click "Scan" to discover running web applications.'}
        </div>
      )}

      {apps.length > 0 && (
        <div className="grid gap-3">
          {apps.map((app) => (
            <AppCard key={app.app_id} app={app} onIntegrate={onIntegrate} />
          ))}
        </div>
      )}

      {lastScan && apps.length > 0 && (
        <p className="text-[10px] text-muted-foreground text-right">
          Last scan: {new Date(lastScan).toLocaleTimeString()}
          {" · "}
          {apps.length} app{apps.length !== 1 ? "s" : ""} found
        </p>
      )}
    </div>
  );
}

function AppCard({ app, onIntegrate }: { app: DiscoveredApp; onIntegrate?: (projectPath: string) => void }) {
  const hasUiBridge = app.capabilities.length > 0;

  return (
    <div className="flex items-center gap-3 p-3 rounded-lg border border-border bg-card/50">
      {/* Status indicator */}
      <div
        className={`w-2 h-2 rounded-full shrink-0 ${
          hasUiBridge ? "bg-green-500" : "bg-yellow-500"
        }`}
      />

      {/* App info */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{app.app_name}</span>
          {app.framework && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-400 font-medium">
              {app.framework}
            </span>
          )}
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
            :{app.port}
          </span>
        </div>
        <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
          <span className="truncate">{app.url}</span>
          {app.element_count != null && <span>{app.element_count} elements</span>}
          {app.version && <span>v{app.version}</span>}
        </div>
      </div>

      {/* Status badge */}
      <div className="flex items-center gap-1.5 shrink-0">
        {hasUiBridge ? (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-green-500/10 text-green-400 font-medium">
            <Wifi className="w-3 h-3" />
            Integrated
          </span>
        ) : (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-yellow-500/10 text-yellow-400 font-medium">
            <WifiOff className="w-3 h-3" />
            Not Integrated
          </span>
        )}
      </div>

      {/* Quick actions */}
      {!hasUiBridge && onIntegrate && (
        <button
          onClick={() => onIntegrate(app.base_path)}
          className="flex items-center gap-1 px-2 py-1 text-[10px] font-medium rounded bg-purple-500/10 text-purple-400 border border-purple-500/20 hover:bg-purple-500/20 transition-colors shrink-0"
        >
          <FileCode className="w-3 h-3" />
          Integrate
        </button>
      )}
    </div>
  );
}
