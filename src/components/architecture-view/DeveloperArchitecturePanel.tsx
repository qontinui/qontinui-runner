import { useState, useMemo, useEffect } from "react";
import { LayoutDashboard, GitBranch, Cpu, Search, Bot, Loader2, Boxes } from "lucide-react";
import type { SdkArchitectureGraph } from "@/types/architecture";
import { DeveloperOverviewPanel } from "./DeveloperOverviewPanel";
import { DeveloperDependencyGraph } from "./DeveloperDependencyGraph";
import { DeveloperTechMap } from "./DeveloperTechMap";
import { DeveloperExplorerPanel } from "./DeveloperExplorerPanel";
import { DeveloperAgenticPanel } from "./DeveloperAgenticPanel";

type ViewId = "overview" | "dependencies" | "tech-map" | "explorer" | "agentic";

const views: Array<{ id: ViewId; label: string; icon: typeof LayoutDashboard }> = [
  { id: "overview", label: "Overview", icon: LayoutDashboard },
  { id: "dependencies", label: "Dependencies", icon: GitBranch },
  { id: "tech-map", label: "Tech Map", icon: Cpu },
  { id: "explorer", label: "Explorer", icon: Search },
  { id: "agentic", label: "Agentic", icon: Bot },
];

interface Props {
  specs: SdkArchitectureGraph[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
}

export function DeveloperArchitecturePanel({
  specs,
  loading,
  error,
  onRefresh: _onRefresh,
}: Props) {
  const [activeView, setActiveView] = useState<ViewId>("overview");
  const [selectedProjectIndex, setSelectedProjectIndex] = useState(0);

  // Load spec page IDs for coverage overlay badges
  const [specPageIds, setSpecPageIds] = useState<Set<string>>(new Set());
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const resp = await fetch("http://localhost:9876/specs");
        const json = await resp.json();
        if (!cancelled && json.data) {
          const ids = new Set<string>();
          for (const spec of json.data) {
            if (spec.metadata?.pageId) ids.add(spec.metadata.pageId);
          }
          setSpecPageIds(ids);
        }
      } catch {
        // Specs endpoint may not be available
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const clampedIndex = useMemo(
    () => Math.min(selectedProjectIndex, Math.max(0, specs.length - 1)),
    [selectedProjectIndex, specs.length],
  );

  if (loading && specs.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (specs.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-center space-y-3">
        <Boxes className="w-12 h-12 text-muted-foreground/30" />
        <div>
          <h3 className="text-sm font-medium text-muted-foreground">No Architecture Specs</h3>
          <p className="text-xs text-muted-foreground/60 mt-1 max-w-sm">
            No architecture specs found. Generate one from the UI Bridge integration page.
          </p>
        </div>
      </div>
    );
  }

  const project = specs[clampedIndex];

  return (
    <div className="flex-1 min-h-0 flex flex-col gap-3">
      {/* Project selector (only when multiple projects) */}
      {specs.length > 1 && (
        <div className="shrink-0">
          <select
            value={clampedIndex}
            onChange={(e) => setSelectedProjectIndex(Number(e.target.value))}
            className="px-3 py-1.5 text-sm bg-muted border border-border rounded-md"
          >
            {specs.map((s, i) => (
              <option key={`${s.projectName}-${i}`} value={i}>
                {s.projectName} ({s.appUrl})
              </option>
            ))}
          </select>
        </div>
      )}

      {/* Error banner */}
      {error && (
        <div className="px-3 py-2 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-400 shrink-0">
          {error}
        </div>
      )}

      {/* View navigation pills */}
      <div className="flex items-center gap-1 bg-muted/50 rounded-lg p-1 shrink-0 self-start">
        {views.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => setActiveView(id)}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-md transition-colors ${
              activeView === id
                ? "bg-background text-foreground shadow-xs"
                : "text-muted-foreground hover:text-foreground hover:bg-muted"
            }`}
          >
            <Icon className="w-3.5 h-3.5" />
            {label}
          </button>
        ))}
      </div>

      {/* Active view */}
      <div className="flex-1 min-h-0 overflow-auto">
        {activeView === "overview" && <DeveloperOverviewPanel project={project} />}
        {activeView === "dependencies" && <DeveloperDependencyGraph project={project} specPageIds={specPageIds} />}
        {activeView === "tech-map" && <DeveloperTechMap project={project} />}
        {activeView === "explorer" && <DeveloperExplorerPanel project={project} />}
        {activeView === "agentic" && <DeveloperAgenticPanel project={project} />}
      </div>
    </div>
  );
}
