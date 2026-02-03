/**
 * Performance Overlay Component
 *
 * Floating overlay showing real-time performance metrics.
 * Toggle with Ctrl+Shift+P keyboard shortcut.
 *
 * Only renders in development mode.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import {
  getPerformanceMonitor,
  type AggregatedMetrics,
  type RenderMetrics,
} from "../../lib/performance";
import { getNetworkMonitor, type NetworkMonitorStats } from "../../lib/network-monitor";
import {
  logPerformanceSummary,
  downloadReport,
} from "../../lib/performance-report";
import { cn } from "../../lib/utils";

// ============================================================================
// Types
// ============================================================================

type TabId = "overview" | "network" | "renders" | "websocket" | "scroll";

interface MetricCardProps {
  label: string;
  value: string | number;
  unit?: string;
  color?: "green" | "yellow" | "red" | "blue" | "gray";
}

// ============================================================================
// Sub-components
// ============================================================================

function MetricCard({ label, value, unit, color = "gray" }: MetricCardProps) {
  const colorClasses = {
    green: "text-green-400",
    yellow: "text-yellow-400",
    red: "text-red-400",
    blue: "text-blue-400",
    gray: "text-gray-300",
  };

  return (
    <div className="flex flex-col">
      <span className="text-[10px] text-gray-500 uppercase">{label}</span>
      <span className={cn("text-sm font-mono", colorClasses[color])}>
        {value}
        {unit && <span className="text-gray-500 text-[10px] ml-0.5">{unit}</span>}
      </span>
    </div>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "px-2 py-1 text-[10px] rounded transition-colors",
        active ? "bg-gray-600 text-white" : "text-gray-400 hover:text-gray-200 hover:bg-gray-700"
      )}
    >
      {children}
    </button>
  );
}

// ============================================================================
// Tab Content Components
// ============================================================================

function OverviewTab({
  metrics,
  networkStats,
}: {
  metrics: AggregatedMetrics | null;
  networkStats: NetworkMonitorStats | null;
}) {
  if (!metrics || !networkStats) {
    return <div className="text-gray-500 text-xs">Collecting metrics...</div>;
  }

  const formatUptime = (ms: number) => {
    const seconds = Math.floor(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    if (hours > 0) return `${hours}h ${minutes % 60}m`;
    if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
    return `${seconds}s`;
  };

  return (
    <div className="grid grid-cols-3 gap-3">
      <MetricCard
        label="Uptime"
        value={formatUptime(metrics.uptime)}
        color="blue"
      />
      <MetricCard
        label="Req/min"
        value={networkStats.requestsPerMinute}
        color={networkStats.requestsPerMinute > 60 ? "red" : networkStats.requestsPerMinute > 30 ? "yellow" : "green"}
      />
      <MetricCard
        label="Avg Latency"
        value={Math.round(networkStats.averageLatency)}
        unit="ms"
        color={networkStats.averageLatency > 500 ? "red" : networkStats.averageLatency > 200 ? "yellow" : "green"}
      />
      <MetricCard
        label="Components"
        value={metrics.renders.size}
        color="blue"
      />
      <MetricCard
        label="WS Messages"
        value={metrics.webSocket.messagesReceived}
        color="blue"
      />
      <MetricCard
        label="Memory"
        value={metrics.memory ? Math.round(metrics.memory.usedJSHeapSize / 1024 / 1024) : "N/A"}
        unit={metrics.memory ? "MB" : ""}
        color={metrics.memory && metrics.memory.heapUtilization > 0.8 ? "red" : "green"}
      />
    </div>
  );
}

function NetworkTab({ networkStats }: { networkStats: NetworkMonitorStats | null }) {
  if (!networkStats) {
    return <div className="text-gray-500 text-xs">No network data</div>;
  }

  const topEndpoints = Array.from(networkStats.endpointStats.entries())
    .sort((a, b) => b[1].requestCount - a[1].requestCount)
    .slice(0, 5);

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-3 gap-3">
        <MetricCard
          label="Total Requests"
          value={networkStats.totalRequests}
          color="blue"
        />
        <MetricCard
          label="Active"
          value={networkStats.activeRequests}
          color={networkStats.activeRequests > 5 ? "yellow" : "green"}
        />
        <MetricCard
          label="Error Rate"
          value={`${Math.round(networkStats.errorRate * 100)}%`}
          color={networkStats.errorRate > 0.05 ? "red" : "green"}
        />
      </div>

      {topEndpoints.length > 0 && (
        <div className="mt-2">
          <div className="text-[10px] text-gray-500 uppercase mb-1">Top Endpoints</div>
          <div className="space-y-1">
            {topEndpoints.map(([endpoint, stats]) => (
              <div key={endpoint} className="flex justify-between text-[10px] font-mono">
                <span className="text-gray-400 truncate max-w-[120px]" title={endpoint}>
                  {endpoint}
                </span>
                <span className="text-gray-300">
                  {stats.requestCount}x / {Math.round(stats.averageLatency)}ms
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function RendersTab({ renders }: { renders: Map<string, RenderMetrics> }) {
  if (renders.size === 0) {
    return <div className="text-gray-500 text-xs">No render data</div>;
  }

  const renderList = Array.from(renders.values());
  const totalRenders = renderList.reduce((sum, r) => sum + r.renderCount, 0);
  const avgTime =
    renderList.reduce((sum, r) => sum + r.totalRenderTime, 0) / Math.max(totalRenders, 1);

  const topRenderers = [...renderList]
    .sort((a, b) => b.renderCount - a.renderCount)
    .slice(0, 5);

  const slowest = [...renderList]
    .sort((a, b) => b.averageRenderTime - a.averageRenderTime)
    .slice(0, 3);

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-3 gap-3">
        <MetricCard label="Total Renders" value={totalRenders} color="blue" />
        <MetricCard
          label="Avg Time"
          value={avgTime.toFixed(1)}
          unit="ms"
          color={avgTime > 16 ? "red" : avgTime > 8 ? "yellow" : "green"}
        />
        <MetricCard label="Components" value={renders.size} color="gray" />
      </div>

      <div>
        <div className="text-[10px] text-gray-500 uppercase mb-1">Most Renders</div>
        <div className="space-y-1">
          {topRenderers.map((r) => (
            <div key={r.componentName} className="flex justify-between text-[10px] font-mono">
              <span className="text-gray-400 truncate max-w-[120px]">{r.componentName}</span>
              <span className="text-gray-300">{r.renderCount}x</span>
            </div>
          ))}
        </div>
      </div>

      <div>
        <div className="text-[10px] text-gray-500 uppercase mb-1">Slowest</div>
        <div className="space-y-1">
          {slowest.map((r) => (
            <div key={r.componentName} className="flex justify-between text-[10px] font-mono">
              <span className="text-gray-400 truncate max-w-[120px]">{r.componentName}</span>
              <span className={r.averageRenderTime > 16 ? "text-red-400" : "text-gray-300"}>
                {r.averageRenderTime.toFixed(1)}ms
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function WebSocketTab({ metrics }: { metrics: AggregatedMetrics | null }) {
  if (!metrics) {
    return <div className="text-gray-500 text-xs">No WebSocket data</div>;
  }

  const ws = metrics.webSocket;
  const messageRate =
    metrics.uptime > 0 ? (ws.messagesReceived / (metrics.uptime / 60000)).toFixed(1) : "0";

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-3">
        <MetricCard
          label="Status"
          value={ws.isConnected ? "Connected" : "Disconnected"}
          color={ws.isConnected ? "green" : "red"}
        />
        <MetricCard label="Reconnects" value={ws.reconnectCount} color={ws.reconnectCount > 5 ? "red" : "green"} />
        <MetricCard label="Messages" value={ws.messagesReceived} color="blue" />
        <MetricCard label="Msg/min" value={messageRate} color="blue" />
        <MetricCard
          label="Avg Latency"
          value={ws.averageLatency.toFixed(1)}
          unit="ms"
          color={ws.averageLatency > 100 ? "yellow" : "green"}
        />
        <MetricCard label="Last Latency" value={ws.lastLatency.toFixed(1)} unit="ms" color="gray" />
      </div>
    </div>
  );
}

function VirtualScrollTab({ metrics }: { metrics: AggregatedMetrics | null }) {
  if (!metrics) {
    return <div className="text-gray-500 text-xs">No scroll data</div>;
  }

  const scroll = metrics.virtualScroll;
  const efficiency =
    scroll.totalItems > 0
      ? Math.round(((scroll.totalItems - scroll.visibleItems) / scroll.totalItems) * 100)
      : 0;

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-3">
        <MetricCard label="Visible Items" value={scroll.visibleItems} color="blue" />
        <MetricCard label="Total Items" value={scroll.totalItems} color="gray" />
        <MetricCard
          label="Visible %"
          value={`${(scroll.visibleRatio * 100).toFixed(1)}%`}
          color={scroll.visibleRatio > 0.5 ? "yellow" : "green"}
        />
        <MetricCard
          label="Memory Saved"
          value={`${efficiency}%`}
          color={efficiency > 80 ? "green" : efficiency > 50 ? "yellow" : "red"}
        />
        <MetricCard
          label="Scroll FPS"
          value={scroll.scrollFps}
          color={scroll.scrollFps < 30 && scroll.scrollFps > 0 ? "red" : "green"}
        />
      </div>
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export interface PerformanceOverlayProps {
  /** Initial visibility state */
  initialVisible?: boolean;
  /** Position on screen */
  position?: "top-left" | "top-right" | "bottom-left" | "bottom-right";
}

/**
 * Performance monitoring overlay.
 * Toggle visibility with Ctrl+Shift+P.
 */
export function PerformanceOverlay({
  initialVisible = false,
  position = "bottom-right",
}: PerformanceOverlayProps) {
  const [isVisible, setIsVisible] = useState(initialVisible);
  const [isMinimized, setIsMinimized] = useState(false);
  const [activeTab, setActiveTab] = useState<TabId>("overview");
  const [metrics, setMetrics] = useState<AggregatedMetrics | null>(null);
  const [networkStats, setNetworkStats] = useState<NetworkMonitorStats | null>(null);

  const updateIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Keyboard shortcut handler
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ctrl+Shift+P to toggle
      if (e.ctrlKey && e.shiftKey && e.key === "P") {
        e.preventDefault();
        setIsVisible((v) => !v);
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Subscribe to metrics updates
  useEffect(() => {
    if (!isVisible) return;

    const monitor = getPerformanceMonitor();
    const networkMonitor = getNetworkMonitor();

    // Initial fetch
    setMetrics(monitor.getMetrics());
    setNetworkStats(networkMonitor.getStats());

    // Subscribe to updates
    const unsubPerf = monitor.subscribe(setMetrics);
    const unsubNetwork = networkMonitor.subscribe(setNetworkStats);

    // Also poll periodically for metrics that don't trigger updates
    updateIntervalRef.current = setInterval(() => {
      setMetrics(monitor.getMetrics());
      setNetworkStats(networkMonitor.getStats());
    }, 1000);

    return () => {
      unsubPerf();
      unsubNetwork();
      if (updateIntervalRef.current) {
        clearInterval(updateIntervalRef.current);
      }
    };
  }, [isVisible]);

  const handleReset = useCallback(() => {
    getPerformanceMonitor().reset();
    getNetworkMonitor().reset();
  }, []);

  const handleLogSummary = useCallback(() => {
    logPerformanceSummary();
  }, []);

  const handleDownload = useCallback(() => {
    downloadReport();
  }, []);

  // Only render in development mode
  if (!import.meta.env.DEV) {
    return null;
  }

  if (!isVisible) {
    return null;
  }

  const positionClasses = {
    "top-left": "top-2 left-2",
    "top-right": "top-2 right-2",
    "bottom-left": "bottom-2 left-2",
    "bottom-right": "bottom-2 right-2",
  };

  return (
    <div
      className={cn(
        "fixed z-[9999] bg-gray-900/95 backdrop-blur-sm rounded-lg shadow-2xl border border-gray-700",
        "text-white font-sans",
        positionClasses[position],
        isMinimized ? "w-auto" : "w-72"
      )}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-gray-700">
        <div className="flex items-center gap-2">
          <div className="w-2 h-2 rounded-full bg-green-400 animate-pulse" />
          <span className="text-xs font-semibold">Performance</span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setIsMinimized((m) => !m)}
            className="p-1 text-gray-400 hover:text-white transition-colors"
            title={isMinimized ? "Expand" : "Minimize"}
          >
            {isMinimized ? "+" : "-"}
          </button>
          <button
            onClick={() => setIsVisible(false)}
            className="p-1 text-gray-400 hover:text-white transition-colors"
            title="Close (Ctrl+Shift+P)"
          >
            x
          </button>
        </div>
      </div>

      {!isMinimized && (
        <>
          {/* Tabs */}
          <div className="flex gap-1 px-2 py-1 border-b border-gray-700 overflow-x-auto">
            <TabButton active={activeTab === "overview"} onClick={() => setActiveTab("overview")}>
              Overview
            </TabButton>
            <TabButton active={activeTab === "network"} onClick={() => setActiveTab("network")}>
              Network
            </TabButton>
            <TabButton active={activeTab === "renders"} onClick={() => setActiveTab("renders")}>
              Renders
            </TabButton>
            <TabButton active={activeTab === "websocket"} onClick={() => setActiveTab("websocket")}>
              WS
            </TabButton>
            <TabButton active={activeTab === "scroll"} onClick={() => setActiveTab("scroll")}>
              Scroll
            </TabButton>
          </div>

          {/* Content */}
          <div className="p-3 min-h-[120px]">
            {activeTab === "overview" && (
              <OverviewTab metrics={metrics} networkStats={networkStats} />
            )}
            {activeTab === "network" && <NetworkTab networkStats={networkStats} />}
            {activeTab === "renders" && <RendersTab renders={metrics?.renders ?? new Map()} />}
            {activeTab === "websocket" && <WebSocketTab metrics={metrics} />}
            {activeTab === "scroll" && <VirtualScrollTab metrics={metrics} />}
          </div>

          {/* Actions */}
          <div className="flex gap-1 px-2 py-2 border-t border-gray-700">
            <button
              onClick={handleReset}
              className="flex-1 px-2 py-1 text-[10px] bg-gray-700 hover:bg-gray-600 rounded transition-colors"
            >
              Reset
            </button>
            <button
              onClick={handleLogSummary}
              className="flex-1 px-2 py-1 text-[10px] bg-gray-700 hover:bg-gray-600 rounded transition-colors"
            >
              Log
            </button>
            <button
              onClick={handleDownload}
              className="flex-1 px-2 py-1 text-[10px] bg-blue-600 hover:bg-blue-500 rounded transition-colors"
            >
              Export
            </button>
          </div>
        </>
      )}
    </div>
  );
}

export default PerformanceOverlay;
