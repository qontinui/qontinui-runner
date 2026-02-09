/**
 * Connection Panel
 *
 * Two-section layout for discovering and connecting to UI Bridge targets:
 * 1. Desktop Apps - SDK-embedded apps discovered via port scanning (primary)
 * 2. Mobile Devices - discovered via ADB
 */

import { useState } from "react";
import { Card, CardContent } from "../ui/Card";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  RefreshCw,
  AlertCircle,
  Check,
  ChevronDown,
  ChevronRight,
  Monitor,
  Smartphone,
  Scan,
  Wifi,
  WifiOff,
} from "lucide-react";
import type { ConnectionStatus } from "../../hooks/useExternalUIBridge";
import type { DiscoveredApp, MobileDevice } from "../../hooks/useAppDiscovery";

// ============================================================================
// Types
// ============================================================================

export type ActiveSource = "desktop" | "mobile" | null;

interface ConnectionPanelProps {
  // Discovery props
  webApps: DiscoveredApp[];
  desktopApps: DiscoveredApp[];
  mobileDevices: MobileDevice[];
  isScanningDesktop: boolean;
  isScanningMobile: boolean;
  onScanDesktop: () => Promise<void>;
  onScanMobile: () => Promise<void>;

  // Active source tracking
  activeSource: ActiveSource;
  activeApp: DiscoveredApp | null;
  activeMobileDevice: MobileDevice | null;
  onConnectToApp: (app: DiscoveredApp) => void;
  onConnectToDevice: (device: MobileDevice) => void;
  onDisconnectSource: () => void;

  // SDK connection status (for desktop/mobile apps)
  sdkConnectionStatus?: ConnectionStatus;

  // Error display
  error: string | null;
}

// ============================================================================
// Component
// ============================================================================

export function ConnectionPanel({
  desktopApps,
  mobileDevices,
  isScanningDesktop,
  isScanningMobile,
  onScanDesktop,
  onScanMobile,
  activeSource,
  activeApp,
  activeMobileDevice,
  onConnectToApp,
  onConnectToDevice,
  onDisconnectSource,
  sdkConnectionStatus,
  error,
}: ConnectionPanelProps) {
  const [expandedSections, setExpandedSections] = useState<Record<string, boolean>>({
    desktop: true,
    mobile: true,
  });

  const toggleSection = (section: string) => {
    setExpandedSections((prev) => ({ ...prev, [section]: !prev[section] }));
  };

  return (
    <Card variant="borderless" className="mb-3">
      <CardContent className="pt-2 pb-2 space-y-0">
        {/* Error message */}
        {error && (
          <div className="p-2 mb-2 bg-destructive/10 border border-destructive/30 rounded-md text-sm text-destructive flex items-start gap-2">
            <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        {/* ================================================================ */}
        {/* Section 1: Desktop Apps (SDK - Primary) */}
        {/* ================================================================ */}
        <SectionHeader
          icon={<Monitor className="w-4 h-4" />}
          title="Desktop Apps"
          expanded={expandedSections.desktop}
          onToggle={() => toggleSection("desktop")}
          badge={
            activeSource === "desktop" && activeApp ? (
              sdkConnectionStatus === "connecting" ? (
                <Badge
                  variant="outline"
                  className="gap-1 text-[10px] text-yellow-500 border-yellow-500/30"
                >
                  <RefreshCw className="w-2.5 h-2.5 animate-spin" />
                  Connecting...
                </Badge>
              ) : sdkConnectionStatus === "connected" ? (
                <Badge variant="success" className="gap-1 text-[10px]">
                  <Check className="w-2.5 h-2.5" />
                  {activeApp.appName}
                </Badge>
              ) : sdkConnectionStatus === "error" ? (
                <Badge variant="danger" className="gap-1 text-[10px]">
                  <AlertCircle className="w-2.5 h-2.5" />
                  Error
                </Badge>
              ) : (
                <Badge variant="muted" className="gap-1 text-[10px]">
                  {activeApp.appName}
                </Badge>
              )
            ) : desktopApps.length > 0 ? (
              <Badge variant="muted" className="text-[10px]">
                {desktopApps.length} found
              </Badge>
            ) : null
          }
          action={
            <Button
              variant="ghost"
              size="sm"
              className="h-6 w-6 p-0"
              onClick={(e) => {
                e.stopPropagation();
                onScanDesktop();
              }}
              disabled={isScanningDesktop}
            >
              <Scan className={`w-3.5 h-3.5 ${isScanningDesktop ? "animate-spin" : ""}`} />
            </Button>
          }
        />
        {expandedSections.desktop && (
          <div className="pl-6 pb-2">
            {activeSource === "desktop" && activeApp && (
              <div
                className={`p-2 rounded-md mb-2 ${
                  sdkConnectionStatus === "connected"
                    ? "bg-accent/10 border border-accent/20"
                    : sdkConnectionStatus === "connecting"
                      ? "bg-yellow-500/10 border border-yellow-500/20"
                      : sdkConnectionStatus === "error"
                        ? "bg-destructive/10 border border-destructive/20"
                        : "bg-muted/30 border border-border/50"
                }`}
              >
                <div className="flex items-center gap-2">
                  {sdkConnectionStatus === "connecting" ? (
                    <RefreshCw className="w-4 h-4 text-yellow-500 shrink-0 animate-spin" />
                  ) : (
                    <Monitor className="w-4 h-4 text-accent shrink-0" />
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-sm">{activeApp.appName}</div>
                    <div className="text-xs text-muted-foreground">
                      :{activeApp.port}
                      {activeApp.framework && ` \u00B7 ${activeApp.framework}`}
                      {activeApp.elementCount != null &&
                        ` \u00B7 ${activeApp.elementCount} elements`}
                    </div>
                  </div>
                  <Button variant="outline" size="sm" onClick={onDisconnectSource}>
                    Disconnect
                  </Button>
                </div>
              </div>
            )}

            {(activeSource !== "desktop" || !activeApp) && (
              <>
                {desktopApps.length > 0 ? (
                  <div className="flex flex-wrap gap-2">
                    {desktopApps.map((app) => (
                      <AppCard
                        key={`${app.appId}-${app.port}`}
                        app={app}
                        isActive={activeApp?.appId === app.appId && activeSource === "desktop"}
                        onClick={() => onConnectToApp(app)}
                      />
                    ))}
                  </div>
                ) : (
                  <div className="text-xs text-muted-foreground">
                    {isScanningDesktop
                      ? "Scanning..."
                      : "No SDK-embedded apps found. Click scan to discover."}
                  </div>
                )}
              </>
            )}
          </div>
        )}

        <div className="border-t border-border/50" />

        {/* ================================================================ */}
        {/* Section 2: Mobile Devices */}
        {/* ================================================================ */}
        <SectionHeader
          icon={<Smartphone className="w-4 h-4" />}
          title="Mobile Devices"
          expanded={expandedSections.mobile}
          onToggle={() => toggleSection("mobile")}
          badge={
            activeSource === "mobile" && activeMobileDevice ? (
              <Badge variant="success" className="gap-1 text-[10px]">
                <Check className="w-2.5 h-2.5" />
                {activeMobileDevice.model || activeMobileDevice.deviceId}
              </Badge>
            ) : mobileDevices.length > 0 ? (
              <Badge variant="muted" className="text-[10px]">
                {mobileDevices.length} device{mobileDevices.length !== 1 ? "s" : ""}
              </Badge>
            ) : null
          }
          action={
            <Button
              variant="ghost"
              size="sm"
              className="h-6 w-6 p-0"
              onClick={(e) => {
                e.stopPropagation();
                onScanMobile();
              }}
              disabled={isScanningMobile}
            >
              <Scan className={`w-3.5 h-3.5 ${isScanningMobile ? "animate-spin" : ""}`} />
            </Button>
          }
        />
        {expandedSections.mobile && (
          <div className="pl-6 pb-2">
            {activeSource === "mobile" && activeMobileDevice && (
              <div className="p-2 bg-accent/10 border border-accent/20 rounded-md mb-2">
                <div className="flex items-center gap-2">
                  <Smartphone className="w-4 h-4 text-accent shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-sm">
                      {activeMobileDevice.model || activeMobileDevice.deviceId}
                    </div>
                    <div className="text-xs text-muted-foreground">
                      {activeMobileDevice.deviceType === "emulator"
                        ? "Emulator"
                        : "Physical Device"}
                      {activeMobileDevice.uiBridge && ` \u00B7 UI Bridge available`}
                    </div>
                  </div>
                  <Button variant="outline" size="sm" onClick={onDisconnectSource}>
                    Disconnect
                  </Button>
                </div>
              </div>
            )}

            {(activeSource !== "mobile" || !activeMobileDevice) && (
              <>
                {mobileDevices.length > 0 ? (
                  <div className="flex flex-wrap gap-2">
                    {mobileDevices.map((device) => (
                      <DeviceCard
                        key={device.deviceId}
                        device={device}
                        isActive={
                          activeMobileDevice?.deviceId === device.deviceId &&
                          activeSource === "mobile"
                        }
                        onClick={() => onConnectToDevice(device)}
                      />
                    ))}
                  </div>
                ) : (
                  <div className="text-xs text-muted-foreground">
                    {isScanningMobile
                      ? "Scanning..."
                      : "No mobile devices found. Connect a device via ADB and click scan."}
                  </div>
                )}
              </>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ============================================================================
// Sub-Components
// ============================================================================

function SectionHeader({
  icon,
  title,
  subtitle,
  expanded,
  onToggle,
  badge,
  action,
}: {
  icon: React.ReactNode;
  title: string;
  subtitle?: string;
  expanded: boolean;
  onToggle: () => void;
  badge?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <button
      className="w-full flex items-center gap-2 py-2 text-left hover:bg-muted/30 rounded-sm transition-colors"
      onClick={onToggle}
    >
      {expanded ? (
        <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
      ) : (
        <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
      )}
      <span className="text-muted-foreground">{icon}</span>
      <span className="text-sm font-medium flex-1">
        {title}
        {subtitle && (
          <span className="text-[10px] font-normal text-muted-foreground ml-1.5">{subtitle}</span>
        )}
      </span>
      {badge}
      {action}
    </button>
  );
}

function AppCard({
  app,
  isActive,
  onClick,
}: {
  app: DiscoveredApp;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`p-2 text-left rounded-md border transition-colors min-w-[140px] ${
        isActive
          ? "border-primary bg-primary/10 ring-1 ring-primary/30"
          : "border-border/50 bg-muted/20 hover:bg-muted/40"
      }`}
      onClick={onClick}
    >
      <div className="flex items-center gap-1.5 mb-1">
        <Monitor className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
        <span className="text-sm font-medium truncate">{app.appName}</span>
      </div>
      <div className="text-xs text-muted-foreground space-y-0.5">
        <div>
          :{app.port}
          {app.framework ? ` \u00B7 ${app.framework}` : ""}
        </div>
        {app.elementCount != null && <div>{app.elementCount} elements</div>}
        {app.capabilities.length > 0 && (
          <div className="flex flex-wrap gap-1 mt-1">
            {app.capabilities.slice(0, 3).map((cap) => (
              <Badge key={cap} variant="muted" className="text-[9px] px-1 py-0">
                {cap}
              </Badge>
            ))}
          </div>
        )}
      </div>
    </button>
  );
}

function DeviceCard({
  device,
  isActive,
  onClick,
}: {
  device: MobileDevice;
  isActive: boolean;
  onClick: () => void;
}) {
  const isOnline = device.status === "online";
  const hasUiBridge = !!device.uiBridge;

  return (
    <button
      className={`p-2 text-left rounded-md border transition-colors min-w-[140px] ${
        isActive
          ? "border-primary bg-primary/10 ring-1 ring-primary/30"
          : isOnline
            ? "border-border/50 bg-muted/20 hover:bg-muted/40"
            : "border-border/30 bg-muted/10 opacity-60"
      }`}
      onClick={onClick}
      disabled={!isOnline}
    >
      <div className="flex items-center gap-1.5 mb-1">
        <Smartphone className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
        <span className="text-sm font-medium truncate">{device.model || device.deviceId}</span>
      </div>
      <div className="text-xs text-muted-foreground space-y-0.5">
        <div className="flex items-center gap-1">
          {device.deviceType === "emulator" ? "Emulator" : "Device"}
          {isOnline ? (
            <Wifi className="w-3 h-3 text-green-500" />
          ) : (
            <WifiOff className="w-3 h-3 text-muted-foreground" />
          )}
        </div>
        {hasUiBridge ? (
          <div className="flex items-center gap-1 text-green-500">
            <Check className="w-3 h-3" />
            UI Bridge
          </div>
        ) : isOnline ? (
          <div className="text-muted-foreground">No UI Bridge</div>
        ) : (
          <div className="text-muted-foreground">{device.status}</div>
        )}
      </div>
    </button>
  );
}

export default ConnectionPanel;
