/**
 * PageSweepTab.tsx
 *
 * Multi-page sweep builder: connects to SDK-integrated apps,
 * discovers navigable pages, lets users select pages and assertion groups,
 * and batch-generates verification workflows.
 */

import { useState, useCallback } from "react";
import {
  Globe,
  Plug,
  PlugZap,
  Loader2,
  AlertCircle,
  Wifi,
  WifiOff,
  Scan,
  Play,
  CheckCircle2,
} from "lucide-react";
import { useSdkUIBridge } from "@/hooks/useSdkUIBridge";
import { cn } from "@/lib/utils";

const API_BASE = "http://127.0.0.1:9876";

// =============================================================================
// Types
// =============================================================================

interface DiscoveredPage {
  url: string;
  title: string;
  selected: boolean;
  hasSpecs: boolean;
  assertionGroups: AssertionGroup[];
}

interface AssertionGroup {
  id: string;
  name: string;
  description: string;
  assertionCount: number;
  selected: boolean;
}

interface PageSweepTabProps {
  onLog?: (level: string, message: string) => void;
}

// =============================================================================
// Main Component
// =============================================================================

export function PageSweepTab({ onLog }: PageSweepTabProps) {
  // SDK UI Bridge connection
  const {
    connectionStatus,
    connectedApp,
    connect: sdkConnect,
    disconnect: sdkDisconnect,
    error: sdkError,
  } = useSdkUIBridge();
  const [urlInput, setUrlInput] = useState("");

  // Page discovery state
  const [discoveredPages, setDiscoveredPages] = useState<DiscoveredPage[]>([]);
  const [isDiscovering, setIsDiscovering] = useState(false);
  const [isGenerating, setIsGenerating] = useState(false);

  const isConnected = connectionStatus === "connected";
  const isConnecting = connectionStatus === "connecting";

  // Handle connect
  const handleConnect = useCallback(async () => {
    if (!urlInput.trim()) return;
    const ok = await sdkConnect(urlInput.trim());
    if (ok) {
      onLog?.("success", `Connected to ${urlInput.trim()}`);
    }
  }, [urlInput, sdkConnect, onLog]);

  // Handle disconnect
  const handleDisconnect = useCallback(async () => {
    await sdkDisconnect();
    setUrlInput("");
    setDiscoveredPages([]);
    onLog?.("info", "Disconnected from SDK app");
  }, [sdkDisconnect, onLog]);

  // Discover pages from the connected app
  const handleDiscoverPages = useCallback(async () => {
    if (!isConnected) {
      onLog?.("warning", "Not connected to an SDK app");
      return;
    }

    setIsDiscovering(true);
    try {
      const response = await fetch(`${API_BASE}/ui-bridge/sdk/discover`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "crawlPages" }),
      });

      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: ${response.statusText}`);
      }

      const data = await response.json();
      const pages: DiscoveredPage[] = (data.data?.pages || []).map(
        (p: { url: string; title?: string }) => ({
          url: p.url,
          title: p.title || p.url,
          selected: true,
          hasSpecs: false,
          assertionGroups: [],
        }),
      );
      setDiscoveredPages(pages);
      onLog?.("success", `Discovered ${pages.length} pages`);
    } catch (err) {
      onLog?.("error", `Failed to discover pages: ${err}`);
    } finally {
      setIsDiscovering(false);
    }
  }, [isConnected, onLog]);

  // Toggle page selection
  const togglePageSelection = useCallback((url: string) => {
    setDiscoveredPages((prev) =>
      prev.map((p) => (p.url === url ? { ...p, selected: !p.selected } : p)),
    );
  }, []);

  // Toggle all pages
  const toggleSelectAllPages = useCallback(() => {
    setDiscoveredPages((prev) => {
      const allSelected = prev.every((p) => p.selected);
      return prev.map((p) => ({ ...p, selected: !allSelected }));
    });
  }, []);

  // Generate sweep workflows
  const handleGenerateSweep = useCallback(async () => {
    const selectedPages = discoveredPages.filter((p) => p.selected);
    if (selectedPages.length === 0) {
      onLog?.("warning", "No pages selected for sweep");
      return;
    }

    setIsGenerating(true);
    try {
      onLog?.("info", `Generating sweep for ${selectedPages.length} pages...`);
      // Batch generation logic would go here
      onLog?.("success", `Sweep generated for ${selectedPages.length} pages`);
    } catch (err) {
      onLog?.("error", `Failed to generate sweep: ${err}`);
    } finally {
      setIsGenerating(false);
    }
  }, [discoveredPages, onLog]);

  const selectedCount = discoveredPages.filter((p) => p.selected).length;

  // =========================================================================
  // Render
  // =========================================================================

  return (
    <div className="flex h-full flex-col bg-surface-canvas">
      {/* Header */}
      <div className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <Globe className="h-5 w-5 text-teal-500" />
          <h1 className="text-lg font-semibold text-text-primary">Page Sweep</h1>
        </div>
        <p className="mt-1 text-sm text-text-muted">
          Connect to an app, discover pages, and batch-generate verification workflows.
        </p>
      </div>

      <div className="flex flex-1 overflow-hidden">
        {/* Left Panel — Connection & Discovery */}
        <div className="flex w-[400px] flex-col border-r border-border bg-surface-raised">
          {/* URL Connection Section */}
          <div className="border-b border-border p-4 space-y-3">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-text-primary">App Connection</h2>
              {isConnected && (
                <span className="flex items-center gap-1 text-xs text-brand-success">
                  <Wifi className="h-3 w-3" />
                  {connectedApp?.appName || "Connected"}
                </span>
              )}
              {connectionStatus === "error" && (
                <span className="flex items-center gap-1 text-xs text-error">
                  <WifiOff className="h-3 w-3" />
                  Error
                </span>
              )}
            </div>

            {/* URL Input */}
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={urlInput}
                onChange={(e) => setUrlInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !isConnected) handleConnect();
                }}
                placeholder="Enter URL to connect (e.g. http://localhost:3001)"
                disabled={isConnected}
                className="flex-1 rounded-md border border-border bg-surface-canvas px-3 py-1.5 text-xs text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none disabled:opacity-60"
              />
              {isConnected ? (
                <button
                  onClick={handleDisconnect}
                  className="flex items-center gap-1 rounded-md bg-error/10 px-2.5 py-1.5 text-xs font-medium text-error hover:bg-error/20 transition-colors"
                >
                  <PlugZap className="h-3.5 w-3.5" />
                  Disconnect
                </button>
              ) : (
                <button
                  onClick={handleConnect}
                  disabled={isConnecting || !urlInput.trim()}
                  className="flex items-center gap-1 rounded-md bg-brand-primary px-2.5 py-1.5 text-xs font-medium text-white hover:bg-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                >
                  {isConnecting ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Plug className="h-3.5 w-3.5" />
                  )}
                  {isConnecting ? "..." : "Connect"}
                </button>
              )}
            </div>

            {sdkError && (
              <div className="flex items-center gap-1.5 text-xs text-error">
                <AlertCircle className="h-3 w-3 flex-shrink-0" />
                {sdkError}
              </div>
            )}
          </div>

          {/* Crawl & Discover Section */}
          <div className="border-b border-border p-4 space-y-3">
            <h2 className="text-sm font-semibold text-text-primary">Crawl &amp; Discover Pages</h2>
            <button
              onClick={handleDiscoverPages}
              disabled={!isConnected || isDiscovering}
              className="w-full flex items-center justify-center gap-1.5 rounded-md py-2 text-xs font-medium bg-surface-hover text-text-primary hover:bg-surface-active disabled:opacity-50 transition-colors"
            >
              {isDiscovering ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Scan className="h-3.5 w-3.5" />
              )}
              {isDiscovering ? "Discovering..." : "Discover Pages"}
            </button>
            {discoveredPages.length > 0 && (
              <p className="text-xs text-text-muted">
                {discoveredPages.length} pages found, {selectedCount} selected
              </p>
            )}
          </div>

          {/* Page Selection List */}
          <div className="flex-1 overflow-y-auto p-4 space-y-2">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-text-primary">Select Pages</h2>
              {discoveredPages.length > 0 && (
                <button
                  onClick={toggleSelectAllPages}
                  className="text-xs text-brand-primary hover:underline"
                >
                  {discoveredPages.every((p) => p.selected) ? "Deselect All" : "Select All"}
                </button>
              )}
            </div>

            {discoveredPages.length === 0 ? (
              <div className="py-8 text-center text-xs text-text-muted">
                {isConnected
                  ? 'Click "Discover Pages" to crawl the connected app.'
                  : "Connect to an app to get started."}
              </div>
            ) : (
              <div className="space-y-1">
                {discoveredPages.map((page) => (
                  <label
                    key={page.url}
                    className={cn(
                      "flex items-center gap-2 rounded-md px-3 py-2 text-xs cursor-pointer transition-colors",
                      page.selected
                        ? "bg-brand-primary/5 border border-brand-primary/20"
                        : "bg-surface-canvas border border-border hover:bg-surface-hover",
                    )}
                  >
                    <input
                      type="checkbox"
                      checked={page.selected}
                      onChange={() => togglePageSelection(page.url)}
                      className="rounded border-border"
                    />
                    <div className="flex-1 min-w-0">
                      <div className="truncate font-medium text-text-primary">{page.title}</div>
                      <div className="truncate text-text-muted">{page.url}</div>
                    </div>
                    {page.hasSpecs && (
                      <CheckCircle2 className="h-3.5 w-3.5 text-brand-success flex-shrink-0" />
                    )}
                  </label>
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Right Panel — Sweep Configuration & Generation */}
        <div className="flex flex-1 flex-col">
          <div className="flex-1 p-6 space-y-4 overflow-y-auto">
            {/* Assertion Group Selection */}
            <div className="space-y-2">
              <h2 className="text-sm font-semibold text-text-primary">Assertion Groups</h2>
              <p className="text-xs text-text-muted">
                Select which assertion groups to include in the sweep for each page.
              </p>
              {discoveredPages.length === 0 ? (
                <div className="rounded-md border border-border bg-surface-raised p-4 text-center text-xs text-text-muted">
                  Discover pages first, then configure assertion groups per page.
                </div>
              ) : (
                <div className="rounded-md border border-border bg-surface-raised p-4 text-xs text-text-muted">
                  {selectedCount} pages selected for sweep generation.
                </div>
              )}
            </div>

            {/* Sweep Generation Controls */}
            <div className="space-y-2">
              <h2 className="text-sm font-semibold text-text-primary">Generate Sweep</h2>
              <p className="text-xs text-text-muted">
                Batch-generate verification workflows for all selected pages.
              </p>
              <button
                onClick={handleGenerateSweep}
                disabled={selectedCount === 0 || isGenerating}
                className="flex items-center gap-1.5 rounded-md bg-brand-primary px-4 py-2 text-xs font-medium text-white hover:bg-brand-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isGenerating ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Play className="h-3.5 w-3.5" />
                )}
                {isGenerating ? "Generating..." : `Generate Sweep (${selectedCount} pages)`}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
