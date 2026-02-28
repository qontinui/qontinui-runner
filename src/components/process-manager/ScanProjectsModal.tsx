import { useState, useCallback } from "react";
import { cn } from "../../lib/utils";
import {
  X,
  Loader2,
  CheckSquare,
  Square,
  FolderSearch,
  Terminal,
  AlertTriangle,
} from "lucide-react";

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  health_port: number | null;
  parser: string;
  auto_start: boolean;
  category: string;
  buffer_size: number;
  enabled: boolean;
}

const CATEGORY_COLORS: Record<string, string> = {
  frontend: "text-blue-400 bg-blue-500/10 border-blue-500/20",
  backend: "text-green-400 bg-green-500/10 border-green-500/20",
  database: "text-amber-400 bg-amber-500/10 border-amber-500/20",
  build: "text-orange-400 bg-orange-500/10 border-orange-500/20",
  testing: "text-purple-400 bg-purple-500/10 border-purple-500/20",
  general: "text-zinc-400 bg-zinc-500/10 border-zinc-500/20",
};

interface ScanProjectsModalProps {
  configs: ProcessConfig[];
  scanning: boolean;
  error: string | null;
  onAdd: (selected: ProcessConfig[]) => void;
  onClose: () => void;
}

export function ScanProjectsModal({
  configs,
  scanning,
  error,
  onAdd,
  onClose,
}: ScanProjectsModalProps) {
  const [selected, setSelected] = useState<Set<string>>(() => new Set(configs.map((c) => c.id)));

  const toggleConfig = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const selectAll = useCallback(() => {
    setSelected(new Set(configs.map((c) => c.id)));
  }, [configs]);

  const deselectAll = useCallback(() => {
    setSelected(new Set());
  }, []);

  const handleAdd = useCallback(() => {
    const selectedConfigs = configs.filter((c) => selected.has(c.id));
    onAdd(selectedConfigs);
  }, [configs, selected, onAdd]);

  const selectedCount = selected.size;

  const truncatePath = (p: string) => {
    if (p.length <= 50) return p;
    return "..." + p.slice(-47);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-zinc-900 border border-white/10 rounded-lg shadow-2xl w-[560px] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-white/10">
          <div className="flex items-center gap-2">
            <FolderSearch className="w-4 h-4 text-cyan-400" />
            <h3 className="text-sm font-medium text-zinc-200">Discovered Projects</h3>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-zinc-500 hover:text-zinc-300 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto px-5 py-4">
          {scanning ? (
            <div className="flex flex-col items-center justify-center py-12 gap-3">
              <Loader2 className="w-8 h-8 text-cyan-400 animate-spin" />
              <p className="text-sm text-zinc-400">Scanning workspace...</p>
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <AlertTriangle className="w-8 h-8 text-red-400" />
              <p className="text-sm text-red-300">{error}</p>
            </div>
          ) : configs.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 gap-3">
              <Terminal className="w-8 h-8 text-zinc-600" />
              <p className="text-sm text-zinc-400">No new projects found.</p>
              <p className="text-xs text-zinc-500">
                All discovered projects are already configured.
              </p>
            </div>
          ) : (
            <>
              <p className="text-xs text-zinc-400 mb-3">
                Found {configs.length} new process{configs.length !== 1 ? "es" : ""}:
              </p>
              <div className="flex flex-col gap-1.5">
                {configs.map((config) => {
                  const isSelected = selected.has(config.id);
                  return (
                    <button
                      key={config.id}
                      onClick={() => toggleConfig(config.id)}
                      className={cn(
                        "flex items-center gap-3 px-3 py-2.5 rounded-lg border text-left transition-colors",
                        isSelected
                          ? "bg-cyan-900/15 border-cyan-700/40 hover:bg-cyan-900/25"
                          : "bg-zinc-800/50 border-white/5 hover:bg-zinc-800",
                      )}
                    >
                      {isSelected ? (
                        <CheckSquare className="w-4 h-4 text-cyan-400 shrink-0" />
                      ) : (
                        <Square className="w-4 h-4 text-zinc-600 shrink-0" />
                      )}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-sm font-medium text-zinc-200">{config.name}</span>
                          <span
                            className={cn(
                              "text-[10px] px-1.5 py-0.5 rounded border",
                              CATEGORY_COLORS[config.category] || CATEGORY_COLORS.general,
                            )}
                          >
                            {config.category}
                          </span>
                        </div>
                        <div className="text-xs text-zinc-500 font-mono mt-0.5 truncate">
                          {config.command} {config.args.join(" ")}
                        </div>
                        <div className="text-xs text-zinc-600 mt-0.5 truncate">
                          {truncatePath(config.cwd)}
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-5 py-3 border-t border-white/10">
          <div className="flex gap-3">
            {configs.length > 0 && !scanning && (
              <>
                <button
                  onClick={selectAll}
                  className="text-xs text-cyan-400 hover:text-cyan-300 transition-colors"
                >
                  Select All
                </button>
                <button
                  onClick={deselectAll}
                  className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
                >
                  Deselect All
                </button>
              </>
            )}
          </div>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="px-3 py-1.5 text-xs bg-zinc-800 border border-white/10 rounded text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              {configs.length === 0 || scanning ? "Close" : "Cancel"}
            </button>
            {configs.length > 0 && !scanning && (
              <button
                onClick={handleAdd}
                disabled={selectedCount === 0}
                className="px-3 py-1.5 text-xs bg-cyan-900/50 border border-cyan-700 rounded text-cyan-300 hover:bg-cyan-800/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Add Selected ({selectedCount})
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
