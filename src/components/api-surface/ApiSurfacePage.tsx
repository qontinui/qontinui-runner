/**
 * ApiSurfacePage — main page combining the graph, orphan panel, connection
 * chain viewer, and stats dashboard for the API surface map.
 */

import { useState, useCallback } from "react";
import { Search, Eye, EyeOff, Layers } from "lucide-react";
import { getApiBase } from "@/lib/runner-api";
import { ApiSurfaceGraph } from "./ApiSurfaceGraph";
import { ApiSurfaceStats } from "./ApiSurfaceStats";
import { OrphanPanel } from "./OrphanPanel";
import { ConnectionChain } from "./ConnectionChain";
import type { ApiSurface, ApiSurfaceDiff, OrphanedEndpoint, NodeCategory } from "./types";

const ALL_LAYERS: NodeCategory[] = [
  "frontend",
  "tauri_command",
  "mcp_route",
  "ui_bridge_route",
  "pg_method",
  "python_event",
  "clorinde_query",
  "db_table",
];

export function ApiSurfacePage() {
  const [surface, setSurface] = useState<ApiSurface | null>(null);
  const [diff, setDiff] = useState<ApiSurfaceDiff | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedNode, setSelectedNode] = useState<{ type: string; name: string } | null>(null);
  const [showOrphans, setShowOrphans] = useState(true);
  const [showOrphanPanel, setShowOrphanPanel] = useState(true);
  const [layerFilter, setLayerFilter] = useState<Set<NodeCategory>>(new Set(ALL_LAYERS));
  const [searchQuery, setSearchQuery] = useState("");
  const [showLayerMenu, setShowLayerMenu] = useState(false);

  const handleScan = useCallback(async () => {
    setScanning(true);
    setError(null);
    try {
      const res = await fetch(`${getApiBase()}/api-surface/scan`, { method: "POST" });
      const json = await res.json();
      if (json.success && json.data) {
        setSurface(json.data);
        // Try to compute diff against last snapshot
        try {
          const diffRes = await fetch(`${getApiBase()}/api-surface/diff`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ current: json.data }),
          });
          const diffJson = await diffRes.json();
          if (diffJson.success && diffJson.data) {
            setDiff(diffJson.data);
          }
        } catch {
          // Diff is optional
        }
      } else {
        setError(json.error ?? "Scan failed");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setScanning(false);
    }
  }, []);

  const handleSaveSnapshot = useCallback(async () => {
    if (!surface) return;
    try {
      await fetch(`${getApiBase()}/api-surface/snapshot`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(surface),
      });
    } catch {
      // Best effort
    }
  }, [surface]);

  const handleOrphanSelect = useCallback((orphan: OrphanedEndpoint) => {
    setSelectedNode({ type: orphan.endpointType, name: orphan.name });
  }, []);

  const toggleLayer = (layer: NodeCategory) => {
    setLayerFilter((prev) => {
      const next = new Set(prev);
      if (next.has(layer)) next.delete(layer);
      else next.add(layer);
      return next;
    });
  };

  return (
    <div className="flex flex-col h-full bg-zinc-900">
      {/* Stats bar */}
      <ApiSurfaceStats
        summary={surface?.summary ?? null}
        diff={diff}
        scanning={scanning}
        onScan={handleScan}
        onSaveSnapshot={handleSaveSnapshot}
      />

      {error && (
        <div className="px-4 py-2 text-xs text-red-400 bg-red-900/20 border-b border-red-800/50">
          {error}
        </div>
      )}

      {/* Toolbar */}
      {surface && (
        <div className="flex items-center gap-2 px-4 py-2 border-b border-zinc-700/50">
          <div className="relative flex-1 max-w-xs">
            <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-zinc-500" />
            <input
              type="text"
              placeholder="Search endpoints..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-8 pr-3 py-1.5 text-xs bg-zinc-800 border border-zinc-700 rounded text-zinc-300 placeholder:text-zinc-600"
            />
          </div>

          <button
            onClick={() => setShowOrphans(!showOrphans)}
            className={`flex items-center gap-1.5 px-2.5 py-1.5 text-xs rounded border ${
              showOrphans
                ? "border-red-600 text-red-400 bg-red-900/20"
                : "border-zinc-700 text-zinc-400 bg-zinc-800"
            }`}
          >
            {showOrphans ? <Eye className="w-3 h-3" /> : <EyeOff className="w-3 h-3" />}
            Orphans
          </button>

          <div className="relative">
            <button
              onClick={() => setShowLayerMenu(!showLayerMenu)}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs rounded border border-zinc-700 text-zinc-400 bg-zinc-800"
            >
              <Layers className="w-3 h-3" />
              Layers
            </button>
            {showLayerMenu && (
              <div className="absolute top-full left-0 mt-1 z-50 bg-zinc-800 border border-zinc-700 rounded shadow-xl p-2 min-w-[180px]">
                {ALL_LAYERS.map((layer) => (
                  <label
                    key={layer}
                    className="flex items-center gap-2 px-2 py-1 text-xs text-zinc-300 hover:bg-zinc-700/50 rounded cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={layerFilter.has(layer)}
                      onChange={() => toggleLayer(layer)}
                      className="rounded border-zinc-600"
                    />
                    {layer.replace(/_/g, " ")}
                  </label>
                ))}
              </div>
            )}
          </div>

          <button
            onClick={() => setShowOrphanPanel(!showOrphanPanel)}
            className={`px-2.5 py-1.5 text-xs rounded border ${
              showOrphanPanel
                ? "border-zinc-600 text-zinc-300 bg-zinc-700"
                : "border-zinc-700 text-zinc-400 bg-zinc-800"
            }`}
          >
            Sidebar
          </button>
        </div>
      )}

      {/* Main content */}
      {surface ? (
        <div className="flex flex-1 min-h-0">
          {/* Graph */}
          <div className="flex-1 min-w-0">
            <ApiSurfaceGraph
              surface={surface}
              showOrphans={showOrphans}
              layerFilter={layerFilter}
              searchQuery={searchQuery}
              onNodeSelect={setSelectedNode}
            />
          </div>

          {/* Right sidebar */}
          {showOrphanPanel && (
            <div className="w-80 border-l border-zinc-700/50 flex flex-col overflow-hidden">
              {/* Connection chain (top half) */}
              <div className="flex-1 overflow-y-auto border-b border-zinc-700/50">
                <ConnectionChain surface={surface} selectedNode={selectedNode} />
              </div>
              {/* Orphan panel (bottom half) */}
              <div className="flex-1 overflow-hidden">
                <OrphanPanel orphans={surface.orphans} onSelect={handleOrphanSelect} />
              </div>
            </div>
          )}
        </div>
      ) : (
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center">
            <p className="text-zinc-400 text-sm mb-3">
              Scan the codebase to map all API endpoints and their connections
            </p>
            <button
              onClick={handleScan}
              disabled={scanning}
              className="px-4 py-2 text-sm font-medium rounded bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white"
            >
              {scanning ? "Scanning..." : "Start Scan"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
