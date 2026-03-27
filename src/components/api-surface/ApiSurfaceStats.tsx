import type { ScanSummary, ApiSurfaceDiff } from "./types";

interface Props {
  summary: ScanSummary | null;
  diff: ApiSurfaceDiff | null;
  scanning: boolean;
  onScan: () => void;
  onSaveSnapshot: () => void;
}

export function ApiSurfaceStats({ summary, diff, scanning, onScan, onSaveSnapshot }: Props) {
  return (
    <div className="flex flex-col gap-3 p-4 border-b border-zinc-700/50">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-zinc-200">API Surface Map</h2>
        <div className="flex gap-2">
          <button
            onClick={onScan}
            disabled={scanning}
            className="px-3 py-1 text-xs font-medium rounded bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white"
          >
            {scanning ? "Scanning..." : "Scan"}
          </button>
          {summary && (
            <button
              onClick={onSaveSnapshot}
              className="px-3 py-1 text-xs font-medium rounded bg-zinc-700 hover:bg-zinc-600 text-zinc-300"
            >
              Save Snapshot
            </button>
          )}
        </div>
      </div>

      {summary && (
        <div className="grid grid-cols-4 gap-2">
          <StatCard label="Tauri Commands" value={summary.totalTauriCommands} color="#f97316" />
          <StatCard label="MCP Routes" value={summary.totalMcpRoutes} color="#22c55e" />
          <StatCard label="PgDb Methods" value={summary.totalPgMethods} color="#a855f7" />
          <StatCard label="Clorinde Queries" value={summary.totalClorindeQueries} color="#06b6d4" />
          <StatCard label="DB Tables" value={summary.totalDbTables} color="#71717a" />
          <StatCard label="Python Events" value={summary.totalPythonEvents} color="#eab308" />
          <StatCard label="Connections" value={summary.totalConnections} color="#3b82f6" />
          <StatCard label="Orphans" value={summary.totalOrphans} color="#ef4444" />
        </div>
      )}

      {summary && (
        <div className="text-[10px] text-zinc-500">
          Scan completed in {summary.scanDurationMs}ms
        </div>
      )}

      {diff && (
        <div className="text-xs text-zinc-400 bg-zinc-800/50 rounded px-3 py-2">
          {diff.summary}
        </div>
      )}
    </div>
  );
}

function StatCard({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="bg-zinc-800/60 rounded px-3 py-2 flex flex-col">
      <span className="text-[10px] text-zinc-500">{label}</span>
      <span className="text-lg font-bold" style={{ color }}>
        {value}
      </span>
    </div>
  );
}
