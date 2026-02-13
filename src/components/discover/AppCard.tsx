/**
 * AppCard.tsx
 *
 * Displays a discovered UI Bridge-enabled application as a card.
 * Shows app name, type icon, port, framework, element count, and capabilities.
 * Optionally shows a Connect button to establish a UI Bridge SDK connection.
 */

import { useState } from "react";
import { Globe, Monitor, Smartphone, Cpu, Plug, Loader2, Check, AlertCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import type { DiscoveredApp } from "@/hooks/useAppDiscovery";

export type AppRole = "reference" | "target" | null;

interface AppCardProps {
  app: DiscoveredApp;
  role: AppRole;
  onAssign: (role: "reference" | "target") => void;
  disabled?: boolean;
  /** Called to connect to this app via UI Bridge SDK. Returns true on success. */
  onConnect?: (app: DiscoveredApp) => Promise<boolean>;
  /** Whether the SDK is already connected to this app's URL */
  isConnected?: boolean;
}

function getAppIcon(appType: string) {
  switch (appType) {
    case "web":
      return <Globe className="h-5 w-5" />;
    case "desktop":
      return <Monitor className="h-5 w-5" />;
    case "mobile":
      return <Smartphone className="h-5 w-5" />;
    default:
      return <Cpu className="h-5 w-5" />;
  }
}

function getRoleBorder(role: AppRole): string {
  if (role === "reference") return "border-blue-500 bg-blue-500/5";
  if (role === "target") return "border-amber-500 bg-amber-500/5";
  return "border-border";
}

function getRoleBadge(role: AppRole) {
  if (role === "reference") {
    return (
      <span className="rounded-full bg-blue-500/20 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-blue-400">
        Reference
      </span>
    );
  }
  if (role === "target") {
    return (
      <span className="rounded-full bg-amber-500/20 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-amber-400">
        Target
      </span>
    );
  }
  return null;
}

export function AppCard({ app, role, onAssign, disabled, onConnect, isConnected }: AppCardProps) {
  const [isConnecting, setIsConnecting] = useState(false);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [justConnected, setJustConnected] = useState(false);

  const handleConnect = async () => {
    if (!onConnect || isConnected || isConnecting) return;
    setIsConnecting(true);
    setConnectError(null);
    setJustConnected(false);

    try {
      const success = await onConnect(app);
      if (success) {
        setJustConnected(true);
        // Clear the "just connected" indicator after 3 seconds
        setTimeout(() => setJustConnected(false), 3000);
      } else {
        setConnectError("Connection failed");
      }
    } catch (err) {
      setConnectError(err instanceof Error ? err.message : "Connection failed");
    } finally {
      setIsConnecting(false);
    }
  };

  const showConnected = isConnected || justConnected;

  return (
    <div
      className={cn(
        "rounded-lg border-2 p-3 transition-all",
        getRoleBorder(role),
        disabled && "opacity-50 pointer-events-none",
      )}
    >
      {/* Header: icon + name + role badge */}
      <div className="flex items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-surface-hover text-text-muted">
          {getAppIcon(app.appType)}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-text-primary truncate">{app.appName}</span>
            {getRoleBadge(role)}
            {showConnected && (
              <span className="rounded-full bg-green-500/20 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-green-400">
                Connected
              </span>
            )}
          </div>
          <div className="flex items-center gap-2 mt-0.5 text-xs text-text-muted">
            <span>:{app.port}</span>
            {app.framework && (
              <>
                <span className="text-text-muted/50">·</span>
                <span>{app.framework}</span>
              </>
            )}
            {app.elementCount != null && (
              <>
                <span className="text-text-muted/50">·</span>
                <span>{app.elementCount} elements</span>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Capabilities */}
      {app.capabilities.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {app.capabilities.slice(0, 5).map((cap) => (
            <span
              key={cap}
              className="rounded bg-surface-hover px-1.5 py-0.5 text-[10px] text-text-muted"
            >
              {cap}
            </span>
          ))}
          {app.capabilities.length > 5 && (
            <span className="text-[10px] text-text-muted">+{app.capabilities.length - 5}</span>
          )}
        </div>
      )}

      {/* Connect error */}
      {connectError && (
        <div className="mt-2 flex items-center gap-1.5 text-[10px] text-error">
          <AlertCircle className="h-3 w-3 shrink-0" />
          <span className="truncate">{connectError}</span>
        </div>
      )}

      {/* Action buttons */}
      <div className="mt-3 flex gap-2">
        {onConnect && (
          <button
            onClick={handleConnect}
            disabled={showConnected || isConnecting}
            className={cn(
              "flex items-center justify-center gap-1.5 rounded-md py-1.5 px-3 text-xs font-medium transition-colors",
              showConnected
                ? "bg-green-500/20 text-green-400 cursor-default"
                : isConnecting
                  ? "bg-brand-primary/20 text-brand-primary cursor-wait"
                  : "bg-brand-primary/10 text-brand-primary hover:bg-brand-primary/20",
            )}
          >
            {isConnecting ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : showConnected ? (
              <Check className="h-3 w-3" />
            ) : (
              <Plug className="h-3 w-3" />
            )}
            {isConnecting ? "Connecting..." : showConnected ? "Connected" : "Connect"}
          </button>
        )}
        <button
          onClick={() => onAssign("reference")}
          disabled={role === "reference"}
          className={cn(
            "flex-1 rounded-md py-1.5 text-xs font-medium transition-colors",
            role === "reference"
              ? "bg-blue-500/20 text-blue-400 cursor-default"
              : "bg-surface-hover text-text-muted hover:bg-blue-500/10 hover:text-blue-400",
          )}
        >
          {role === "reference" ? "Reference" : "Set Reference"}
        </button>
        <button
          onClick={() => onAssign("target")}
          disabled={role === "target"}
          className={cn(
            "flex-1 rounded-md py-1.5 text-xs font-medium transition-colors",
            role === "target"
              ? "bg-amber-500/20 text-amber-400 cursor-default"
              : "bg-surface-hover text-text-muted hover:bg-amber-500/10 hover:text-amber-400",
          )}
        >
          {role === "target" ? "Target" : "Set Target"}
        </button>
      </div>
    </div>
  );
}
