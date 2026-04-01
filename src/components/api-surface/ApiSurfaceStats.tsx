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
        <div className="text-xs bg-zinc-800/50 rounded px-3 py-2 space-y-1">
          <div className="text-zinc-400">{diff.summary}</div>
          {diff.addedCommands.length > 0 && (
            <DiffLine
              color="#22c55e"
              label={`+${diff.addedCommands.length} commands`}
              items={diff.addedCommands.map((c) => c.name)}
            />
          )}
          {diff.removedCommands.length > 0 && (
            <DiffLine
              color="#ef4444"
              label={`-${diff.removedCommands.length} commands`}
              items={diff.removedCommands}
            />
          )}
          {diff.addedRoutes.length > 0 && (
            <DiffLine
              color="#22c55e"
              label={`+${diff.addedRoutes.length} routes`}
              items={diff.addedRoutes.map((r) => `${r.method} ${r.path}`)}
            />
          )}
          {diff.removedRoutes.length > 0 && (
            <DiffLine
              color="#ef4444"
              label={`-${diff.removedRoutes.length} routes`}
              items={diff.removedRoutes}
            />
          )}
          {diff.addedPgMethods.length > 0 && (
            <DiffLine
              color="#22c55e"
              label={`+${diff.addedPgMethods.length} PgDb methods`}
              items={diff.addedPgMethods.map((m) => m.name)}
            />
          )}
          {diff.removedPgMethods.length > 0 && (
            <DiffLine
              color="#ef4444"
              label={`-${diff.removedPgMethods.length} PgDb methods`}
              items={diff.removedPgMethods}
            />
          )}
          {diff.newOrphans.length > 0 && (
            <DiffLine
              color="#f97316"
              label={`+${diff.newOrphans.length} orphans`}
              items={diff.newOrphans.map((o) => `${o.endpointType}: ${o.name}`)}
            />
          )}
          {diff.resolvedOrphans.length > 0 && (
            <DiffLine
              color="#22c55e"
              label={`-${diff.resolvedOrphans.length} orphans resolved`}
              items={diff.resolvedOrphans}
            />
          )}
          {diff.newConnections.length > 0 && (
            <DiffLine
              color="#3b82f6"
              label={`+${diff.newConnections.length} connections`}
              items={diff.newConnections.map((c) => `${c.fromName} → ${c.toName}`)}
            />
          )}
          {diff.brokenConnections.length > 0 && (
            <DiffLine
              color="#ef4444"
              label={`-${diff.brokenConnections.length} connections broken`}
              items={diff.brokenConnections.map((c) => `${c.fromName} → ${c.toName}`)}
            />
          )}
        </div>
      )}
    </div>
  );
}

function DiffLine({ color, label, items }: { color: string; label: string; items: string[] }) {
  return (
    <div className="flex items-start gap-2">
      <span className="font-medium whitespace-nowrap" style={{ color }}>
        {label}
      </span>
      <span className="text-zinc-500 truncate">
        {items.slice(0, 5).join(", ")}
        {items.length > 5 ? ` (+${items.length - 5} more)` : ""}
      </span>
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
