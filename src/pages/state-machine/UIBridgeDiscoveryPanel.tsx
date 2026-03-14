/**
 * UIBridgeDiscoveryPanel — Runner-specific discovery panel.
 *
 * Phase 1 — Connect: Scan for SDK apps, connect to one
 * Phase 2 — Collect: Explore (automated) or Record (manual capture session)
 * Phase 3 — Discover: Run fingerprint-based state discovery
 * Phase 4 — Save: Save discovered states to config
 */

import { useState, useCallback, useEffect } from "react";
import {
  Compass,
  Camera,
  Loader2,
  CheckCircle2,
  Save,
  RotateCcw,
  Sparkles,
  Layers,
  Hash,
  Scan,
  Plug,
  Circle,
  Square,
} from "lucide-react";
import { useAppDiscovery, type DiscoveredApp } from "@/hooks/useAppDiscovery";
import { useSdkUIBridge } from "@/hooks/useSdkUIBridge";
import type { useUIBridgeDiscovery } from "@/hooks/useUIBridgeDiscovery";

interface UIBridgeDiscoveryPanelProps {
  discovery: ReturnType<typeof useUIBridgeDiscovery>;
  onConfigCreated: (configId: string) => void;
}

export function UIBridgeDiscoveryPanel({
  discovery,
  onConfigCreated,
}: UIBridgeDiscoveryPanelProps) {
  const [collectTab, setCollectTab] = useState<"explore" | "record">("explore");

  // App discovery and SDK connection
  const appDiscovery = useAppDiscovery();
  const sdk = useSdkUIBridge();

  // All discovered apps (web + desktop)
  const allApps: DiscoveredApp[] = [...appDiscovery.webApps, ...appDiscovery.desktopApps];

  const isConnected = sdk.connectionStatus === "connected";

  // Auto-scan on mount
  useEffect(() => {
    if (allApps.length === 0 && !appDiscovery.isScanning) {
      appDiscovery.scanAll();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Connect to a discovered app
  const handleConnect = useCallback(
    async (app: DiscoveredApp) => {
      await sdk.connect(app.url, {
        appId: app.appId,
        appName: app.appName,
        appType: app.appType,
        framework: app.framework,
        version: app.version,
        capabilities: app.capabilities,
        port: app.port,
      });
    },
    [sdk],
  );

  // Exploration handler — exploreStates returns CooccurrenceExport
  const handleStartExploration = useCallback(async () => {
    const result = await sdk.exploreStates();
    if (result) {
      discovery.setCooccurrenceData(result, "explore");
    }
  }, [sdk, discovery]);

  // Recording handlers — capture session produces CooccurrenceExport
  const handleStartRecording = useCallback(() => {
    sdk.startCaptureSession();
  }, [sdk]);

  const handleStopRecording = useCallback(async () => {
    sdk.stopCaptureSession();
    const result = await sdk.generateCooccurrenceExport();
    if (result) {
      discovery.setCooccurrenceData(result, "record");
    }
  }, [sdk, discovery]);

  // Save handler
  const handleSave = useCallback(async () => {
    const configId = await discovery.saveConfig();
    if (configId) {
      onConfigCreated(configId);
    }
  }, [discovery, onConfigCreated]);

  // State checks
  const hasData = discovery.cooccurrenceData !== null;
  const hasDiscoveryResult = discovery.discoveryResult !== null;
  const captureCount = hasData ? discovery.cooccurrenceData!.presenceMatrix.length : 0;

  return (
    <div className="p-4">
      <div className="max-w-4xl mx-auto space-y-4">
        {/* Phase 1: Connect + Collect — hidden when data collected */}
        <div className={hasData ? "hidden" : "space-y-4"}>
          {/* Connection Card */}
          <div className="rounded-lg border border-border-primary bg-surface-secondary p-4 space-y-3">
            <div className="flex items-center gap-2">
              <Plug className="size-4 text-text-muted" />
              <span className="text-sm font-medium">Connect to SDK App</span>
            </div>

            {/* Scan button + status */}
            <div className="flex items-center gap-3">
              <button
                onClick={() => appDiscovery.scanAll()}
                disabled={appDiscovery.isScanning}
                className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium border border-border-primary text-text-primary hover:bg-surface-primary disabled:opacity-50"
              >
                {appDiscovery.isScanning ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <Scan className="size-3.5" />
                )}
                Scan for Apps
              </button>
              {appDiscovery.lastScanAt && (
                <span className="text-xs text-text-muted">
                  {allApps.length} app{allApps.length !== 1 ? "s" : ""} found
                </span>
              )}
            </div>

            {/* Connected banner */}
            <div
              className={
                isConnected && sdk.connectedApp
                  ? "flex items-center gap-2 p-2 bg-green-500/10 border border-green-500/20 rounded-md"
                  : "hidden"
              }
            >
              <CheckCircle2 className="size-4 text-green-500 shrink-0" />
              <span className="text-sm text-text-primary truncate">
                Connected: <strong>{sdk.connectedApp?.appName}</strong>
              </span>
            </div>

            {/* Discovered apps list — hidden when connected */}
            <div className={!isConnected && allApps.length > 0 ? "space-y-2" : "hidden"}>
              {allApps.map((app) => (
                <div
                  key={app.appId}
                  className="flex items-center justify-between p-2 border border-border-primary rounded-md"
                >
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-text-primary truncate">
                      {app.appName}
                    </div>
                    <div className="text-xs text-text-muted">
                      {app.url} {app.framework && `(${app.framework})`}
                    </div>
                  </div>
                  <button
                    onClick={() => handleConnect(app)}
                    className="shrink-0 inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90"
                  >
                    Connect
                  </button>
                </div>
              ))}
            </div>

            <p
              className={
                !isConnected && allApps.length === 0 && !appDiscovery.isScanning
                  ? "text-xs text-text-muted"
                  : "hidden"
              }
            >
              Click Scan to find apps with the UI Bridge SDK installed.
            </p>
          </div>

          {/* Collection tabs — only when connected */}
          <div className={isConnected ? "" : "hidden"}>
            <div className="inline-flex h-9 items-center justify-center rounded-lg bg-muted p-1 text-muted-foreground">
              <button
                onClick={() => setCollectTab("explore")}
                className={`inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-1 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] ${
                  collectTab === "explore"
                    ? "bg-background shadow-sm text-foreground"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Compass className="size-3.5" />
                Explore
              </button>
              <button
                onClick={() => setCollectTab("record")}
                className={`inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-1 text-sm font-medium whitespace-nowrap transition-[color,box-shadow] ${
                  collectTab === "record"
                    ? "bg-background shadow-sm text-foreground"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Camera className="size-3.5" />
                Record
              </button>
            </div>

            {/* Explore tab */}
            <div className={`mt-3 ${collectTab !== "explore" ? "hidden" : ""}`}>
              <div className="rounded-lg border border-border-primary bg-surface-secondary p-4 space-y-3">
                <p className="text-sm text-text-muted">
                  Automatically explore the connected app by interacting with UI elements and
                  capturing state snapshots.
                </p>

                <div className="flex items-center gap-3">
                  <button
                    onClick={handleStartExploration}
                    disabled={sdk.isExploring}
                    className="inline-flex items-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90 disabled:opacity-50"
                  >
                    {sdk.isExploring ? (
                      <Loader2 className="size-3.5 animate-spin" />
                    ) : (
                      <Compass className="size-3.5" />
                    )}
                    {sdk.isExploring ? "Exploring..." : "Start Exploration"}
                  </button>

                  <button
                    onClick={() => sdk.cancelExploration()}
                    className={
                      sdk.isExploring
                        ? "inline-flex items-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium border border-border-primary text-text-primary hover:bg-surface-primary"
                        : "hidden"
                    }
                  >
                    Cancel
                  </button>
                </div>

                {/* Exploration progress */}
                <div
                  className={
                    sdk.explorationProgress ? "text-xs text-text-muted space-y-1" : "hidden"
                  }
                >
                  <div>
                    Progress: {sdk.explorationProgress?.current ?? 0} /{" "}
                    {sdk.explorationProgress?.total ?? 0} interactions
                  </div>
                  {sdk.explorationProgress?.currentElement && (
                    <div className="truncate">
                      Current: {sdk.explorationProgress.currentElement}
                    </div>
                  )}
                </div>
              </div>
            </div>

            {/* Record tab */}
            <div className={`mt-3 ${collectTab !== "record" ? "hidden" : ""}`}>
              <div className="rounded-lg border border-border-primary bg-surface-secondary p-4 space-y-3">
                <p className="text-sm text-text-muted">
                  Start a capture session to record UI state changes. Navigate the app manually
                  while recording to capture different UI states.
                </p>

                <div className="flex items-center gap-3">
                  <button
                    onClick={handleStartRecording}
                    className={
                      sdk.captureSession.active
                        ? "hidden"
                        : "inline-flex items-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90"
                    }
                  >
                    <Circle className="size-3.5 text-red-400" />
                    Start Recording
                  </button>
                  <button
                    onClick={handleStopRecording}
                    className={
                      sdk.captureSession.active
                        ? "inline-flex items-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium bg-red-600 text-white hover:bg-red-700"
                        : "hidden"
                    }
                  >
                    <Square className="size-3.5" />
                    Stop Recording
                  </button>
                </div>

                {/* Recording status */}
                <div
                  className={
                    sdk.captureSession.active || (sdk.captureSession.captureCount ?? 0) > 0
                      ? "flex items-center gap-4"
                      : "hidden"
                  }
                >
                  <span
                    className={`inline-flex items-center gap-1 text-xs rounded-full px-2 py-0.5 ${
                      sdk.captureSession.active
                        ? "bg-red-500/10 text-red-500"
                        : "bg-surface-primary text-text-muted"
                    }`}
                  >
                    {sdk.captureSession.active && (
                      <span className="size-2 rounded-full bg-red-400 animate-pulse" />
                    )}
                    {sdk.captureSession.captureCount ?? 0} captures
                  </span>
                  <span className="text-xs text-text-muted">
                    {sdk.captureSession.uniqueFingerprints ?? 0} unique fingerprints
                  </span>
                </div>
              </div>
            </div>
          </div>
        </div>

        {/* Phase 2+3: Discovery — shown when co-occurrence data collected */}
        <div className={hasData ? "space-y-4" : "hidden"}>
          {/* Data summary */}
          <div className="rounded-lg border border-border-primary bg-surface-secondary p-4">
            <div className="flex items-center justify-between mb-3">
              <div className="flex items-center gap-2 text-base font-semibold text-text-primary">
                <Layers className="size-4" />
                Co-occurrence Data Collected
              </div>
              <button
                onClick={discovery.reset}
                className="inline-flex items-center gap-1.5 text-sm text-text-muted hover:text-text-primary"
              >
                <RotateCcw className="size-3.5" />
                Start Over
              </button>
            </div>
            <div className="flex items-center gap-4">
              <span className="inline-flex items-center gap-1 text-sm bg-surface-primary rounded-full px-2 py-0.5">
                <Hash className="size-3" />
                {captureCount} captures
              </span>
              <span className="inline-flex items-center text-sm bg-surface-primary rounded-full px-2 py-0.5">
                {discovery.cooccurrenceData?.allFingerprints.length ?? 0} fingerprints
              </span>
              <span className="inline-flex items-center text-sm border border-border-primary rounded-full px-2 py-0.5 capitalize">
                via {discovery.dataSource}
              </span>
            </div>
          </div>

          {/* Discover States button — hidden when result exists */}
          <div
            className={
              hasDiscoveryResult
                ? "hidden"
                : "rounded-lg border border-border-primary bg-surface-secondary p-6 flex flex-col items-center gap-4"
            }
          >
            <p className="text-sm text-text-secondary">
              Run fingerprint-based state discovery to identify UI states from the collected data.
            </p>
            <button
              onClick={discovery.runDiscovery}
              disabled={discovery.isDiscovering}
              className="inline-flex items-center gap-2 rounded-md px-4 py-2.5 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90 disabled:opacity-50"
            >
              {discovery.isDiscovering ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Sparkles className="size-4" />
              )}
              Discover States
            </button>
          </div>

          {/* Discovery Results + Save — hidden when no result */}
          <div
            className={
              hasDiscoveryResult
                ? "rounded-lg border border-border-primary bg-surface-secondary p-4 space-y-4"
                : "hidden"
            }
          >
            <div className="flex items-center gap-2 text-base font-semibold text-text-primary">
              <CheckCircle2 className="size-4 text-green-500" />
              Discovery Complete
            </div>

            <div className="flex items-center gap-4">
              <span className="inline-flex items-center text-sm bg-surface-primary rounded-full px-2 py-0.5">
                {discovery.discoveryResult?.states.length ?? 0} states
              </span>
              <span className="inline-flex items-center text-sm bg-surface-primary rounded-full px-2 py-0.5">
                {discovery.discoveryResult?.statistics.uniqueFingerprints ?? 0} fingerprints
              </span>
              <span className="inline-flex items-center text-sm border border-border-primary rounded-full px-2 py-0.5">
                {discovery.discoveryResult?.statistics.totalCaptures ?? 0} captures
              </span>
            </div>

            {/* State preview */}
            <div className="border border-border-primary rounded-md p-3 space-y-1.5 max-h-40 overflow-y-auto">
              {(discovery.discoveryResult?.states ?? []).map((state) => (
                <div key={state.stateId} className="flex items-center justify-between text-sm">
                  <span className="text-text-primary">{state.name}</span>
                  <span className="text-text-muted">
                    {(state.confidence * 100).toFixed(0)}% confidence
                  </span>
                </div>
              ))}
            </div>

            {/* Save form */}
            <div className="border-t border-border-primary pt-4 space-y-3">
              <div className="space-y-1.5">
                <label htmlFor="config-name" className="text-sm font-medium text-text-primary">
                  Configuration Name
                </label>
                <input
                  id="config-name"
                  type="text"
                  placeholder="e.g., My App States"
                  value={discovery.configName}
                  onChange={(e) => discovery.setConfigName(e.target.value)}
                  className="w-full rounded-md border border-border-primary bg-surface-primary px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:outline-none focus:ring-2 focus:ring-brand-primary"
                />
              </div>
              <button
                onClick={handleSave}
                disabled={discovery.isSaving || !discovery.configName.trim()}
                className="w-full inline-flex items-center justify-center gap-2 rounded-md px-3 py-2 text-sm font-medium bg-brand-primary text-white hover:bg-brand-primary/90 disabled:opacity-50"
              >
                {discovery.isSaving ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Save className="size-4" />
                )}
                Save Configuration
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
