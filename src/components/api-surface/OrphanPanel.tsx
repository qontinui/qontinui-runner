import { useMemo, useState } from "react";
import { AlertTriangle, Search, ChevronDown, ChevronRight } from "lucide-react";
import type { OrphanedEndpoint } from "./types";
import { NODE_COLORS, type NodeCategory } from "./types";

interface Props {
  orphans: OrphanedEndpoint[];
  onSelect: (orphan: OrphanedEndpoint) => void;
}

const TYPE_LABELS: Record<string, string> = {
  tauri_command: "Tauri Commands",
  mcp_route: "MCP Routes",
  ui_bridge_route: "UI Bridge Routes",
  pg_method: "PgDb Methods",
  clorinde_query: "Clorinde Queries",
  python_event: "Python Events",
};

export function OrphanPanel({ orphans, onSelect }: Props) {
  const [search, setSearch] = useState("");
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const filtered = useMemo(() => {
    if (!search) return orphans;
    const q = search.toLowerCase();
    return orphans.filter(
      (o) => o.name.toLowerCase().includes(q) || o.file.toLowerCase().includes(q),
    );
  }, [orphans, search]);

  const grouped = useMemo(() => {
    const groups: Record<string, OrphanedEndpoint[]> = {};
    for (const o of filtered) {
      (groups[o.endpointType] ??= []).push(o);
    }
    return groups;
  }, [filtered]);

  const toggleGroup = (type: string) => {
    setExpanded((prev) => ({ ...prev, [type]: !prev[type] }));
  };

  if (orphans.length === 0) {
    return (
      <div className="p-4 text-center text-zinc-500 text-xs">No orphaned endpoints detected</div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 border-b border-zinc-700/50">
        <div className="flex items-center gap-2 mb-2">
          <AlertTriangle className="w-4 h-4 text-red-400" />
          <span className="text-sm font-semibold text-zinc-200">Orphans ({filtered.length})</span>
        </div>
        <div className="relative">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3 h-3 text-zinc-500" />
          <input
            type="text"
            aria-label="Filter orphans"
            placeholder="Filter orphans..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-full pl-7 pr-2 py-1.5 text-xs bg-zinc-800 border border-zinc-700 rounded text-zinc-300 placeholder:text-zinc-600"
          />
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {Object.entries(grouped).map(([type, items]) => {
          const isExpanded = expanded[type] !== false; // default open
          const colors = NODE_COLORS[type as NodeCategory];
          return (
            <div key={type}>
              <button
                onClick={() => toggleGroup(type)}
                className="w-full flex items-center gap-2 px-3 py-2 text-xs font-medium hover:bg-zinc-800/50"
                style={{ color: colors?.text ?? "#a1a1aa" }}
              >
                {isExpanded ? (
                  <ChevronDown className="w-3 h-3" />
                ) : (
                  <ChevronRight className="w-3 h-3" />
                )}
                {TYPE_LABELS[type] ?? type} ({items.length})
              </button>
              {isExpanded &&
                items.map((orphan) => (
                  <button
                    key={`${orphan.endpointType}-${orphan.name}`}
                    onClick={() => onSelect(orphan)}
                    className="w-full text-left px-5 py-1.5 text-xs hover:bg-zinc-800/50 border-l-2 border-transparent hover:border-red-500"
                  >
                    <div className="text-zinc-300 font-mono truncate">{orphan.name}</div>
                    <div className="text-[10px] text-zinc-500 truncate">
                      {orphan.file}:{orphan.line} — {orphan.reason}
                    </div>
                  </button>
                ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
