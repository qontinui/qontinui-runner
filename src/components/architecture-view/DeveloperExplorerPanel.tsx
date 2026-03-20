import { useState, useEffect, useMemo } from "react";
import { Search } from "lucide-react";
import type {
  SdkArchitectureGraph,
  SdkArchitectureNode,
  SdkArchitectureConstraint,
} from "@/types/architecture";

interface Props {
  project: SdkArchitectureGraph;
}

type FilterType = "features" | "patterns" | "tech" | "directories" | "constraints";

const ALL_FILTERS: FilterType[] = ["features", "patterns", "tech", "directories", "constraints"];

const FILTER_LABELS: Record<FilterType, string> = {
  features: "Features",
  patterns: "Patterns",
  tech: "Tech",
  directories: "Directories",
  constraints: "Constraints",
};

const PRIORITY_ORDER: Record<string, number> = {
  core: 0,
  critical: 1,
  high: 2,
  medium: 3,
  low: 4,
};

interface MatchedFeature {
  kind: "feature";
  node: SdkArchitectureNode;
}

interface MatchedPattern {
  kind: "pattern";
  node: SdkArchitectureNode;
}

interface MatchedTech {
  kind: "tech";
  tech: SdkArchitectureGraph["techStack"][number];
  featureCount: number;
}

interface MatchedDirectory {
  kind: "directory";
  dir: SdkArchitectureGraph["directories"][number];
}

interface MatchedConstraint {
  kind: "constraint";
  constraint: SdkArchitectureConstraint;
}

type MatchedItem =
  | MatchedFeature
  | MatchedPattern
  | MatchedTech
  | MatchedDirectory
  | MatchedConstraint;

export function DeveloperExplorerPanel({ project }: Props) {
  const [inputValue, setInputValue] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [enabledFilters, setEnabledFilters] = useState<Set<FilterType>>(() => new Set(ALL_FILTERS));

  // Debounce search input by 150ms
  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedQuery(inputValue);
    }, 150);
    return () => clearTimeout(timer);
  }, [inputValue]);

  const toggleFilter = (filter: FilterType) => {
    setEnabledFilters((prev) => {
      const next = new Set(prev);
      if (next.has(filter)) {
        next.delete(filter);
      } else {
        next.add(filter);
      }
      return next;
    });
  };

  // Pre-compute tech-to-feature-count map
  const techFeatureCount = useMemo(() => {
    const map = new Map<string, number>();
    const features = project.nodes.filter((n) => n.type === "feature");
    for (const feature of features) {
      for (const techName of feature.techUsed ?? []) {
        map.set(techName, (map.get(techName) ?? 0) + 1);
      }
    }
    return map;
  }, [project.nodes]);

  // Pre-compute pattern usedBy from features
  const patternUsedBy = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const node of project.nodes) {
      if (node.type === "pattern" && node.usedBy && node.usedBy.length > 0) {
        map.set(node.label, [...node.usedBy]);
      }
    }
    return map;
  }, [project.nodes]);

  const results = useMemo(() => {
    const query = debouncedQuery.toLowerCase().trim();
    const items: MatchedItem[] = [];

    function matches(text: string | undefined | null): boolean {
      if (!text) return false;
      return text.toLowerCase().includes(query);
    }

    function matchesAny(...texts: (string | undefined | null)[]): boolean {
      if (!query) return true;
      return texts.some((t) => matches(t));
    }

    // Features
    if (enabledFilters.has("features")) {
      const features = project.nodes
        .filter((n) => n.type === "feature")
        .filter((n) =>
          matchesAny(n.label, n.description, ...(n.entryPoints ?? []), ...(n.techUsed ?? [])),
        );

      // Sort by priority (core first)
      features.sort((a, b) => {
        const pa = PRIORITY_ORDER[a.priority ?? ""] ?? 99;
        const pb = PRIORITY_ORDER[b.priority ?? ""] ?? 99;
        if (pa !== pb) return pa - pb;
        return a.label.localeCompare(b.label);
      });

      for (const node of features) {
        items.push({ kind: "feature", node });
      }
    }

    // Patterns
    if (enabledFilters.has("patterns")) {
      const patterns = project.nodes
        .filter((n) => n.type === "pattern")
        .filter((n) => matchesAny(n.label, n.description, n.category))
        .sort((a, b) => a.label.localeCompare(b.label));

      for (const node of patterns) {
        items.push({ kind: "pattern", node });
      }
    }

    // Tech Stack
    if (enabledFilters.has("tech")) {
      const techs = project.techStack
        .filter((t) => matchesAny(t.name, t.category, t.purpose, t.version))
        .sort((a, b) => a.name.localeCompare(b.name));

      for (const tech of techs) {
        items.push({
          kind: "tech",
          tech,
          featureCount: techFeatureCount.get(tech.name) ?? 0,
        });
      }
    }

    // Directories
    if (enabledFilters.has("directories")) {
      const dirs = project.directories
        .filter((d) => matchesAny(d.path, d.purpose))
        .sort((a, b) => a.path.localeCompare(b.path));

      for (const dir of dirs) {
        items.push({ kind: "directory", dir });
      }
    }

    // Constraints
    if (enabledFilters.has("constraints")) {
      const constraints = project.constraints
        .filter((c) => matchesAny(c.id, c.description, c.category))
        .sort((a, b) => a.id.localeCompare(b.id));

      for (const constraint of constraints) {
        items.push({ kind: "constraint", constraint });
      }
    }

    return items;
  }, [
    debouncedQuery,
    enabledFilters,
    project.nodes,
    project.techStack,
    project.directories,
    project.constraints,
    techFeatureCount,
  ]);

  // Group results by kind for section rendering
  const grouped = useMemo(() => {
    const sections: Array<{ type: FilterType; label: string; items: MatchedItem[] }> = [];

    const featureItems = results.filter((r) => r.kind === "feature");
    const patternItems = results.filter((r) => r.kind === "pattern");
    const techItems = results.filter((r) => r.kind === "tech");
    const dirItems = results.filter((r) => r.kind === "directory");
    const constraintItems = results.filter((r) => r.kind === "constraint");

    if (featureItems.length > 0)
      sections.push({ type: "features", label: "Features", items: featureItems });
    if (patternItems.length > 0)
      sections.push({ type: "patterns", label: "Patterns", items: patternItems });
    if (techItems.length > 0)
      sections.push({ type: "tech", label: "Tech Stack", items: techItems });
    if (dirItems.length > 0)
      sections.push({ type: "directories", label: "Directories", items: dirItems });
    if (constraintItems.length > 0)
      sections.push({ type: "constraints", label: "Constraints", items: constraintItems });

    return sections;
  }, [results]);

  const hasQuery = debouncedQuery.trim().length > 0;
  const hasResults = results.length > 0;

  return (
    <div className="flex flex-col h-full gap-3">
      {/* Search bar */}
      <div className="flex-shrink-0 relative">
        <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
        <input
          type="text"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          placeholder="Search features, patterns, tech, directories, constraints..."
          className="w-full pl-8 pr-3 py-2 text-sm bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-blue-500/50 focus:border-blue-500/50"
        />
      </div>

      {/* Filter pills */}
      <div className="flex-shrink-0 flex flex-wrap gap-1.5">
        {ALL_FILTERS.map((filter) => {
          const active = enabledFilters.has(filter);
          return (
            <button
              key={filter}
              onClick={() => toggleFilter(filter)}
              className={`px-2.5 py-1 text-[11px] rounded-md transition-colors ${
                active
                  ? "bg-blue-500/15 text-blue-400 border border-blue-500/30"
                  : "bg-muted/50 text-muted-foreground border border-transparent hover:bg-muted hover:text-foreground"
              }`}
            >
              {FILTER_LABELS[filter]}
            </button>
          );
        })}
      </div>

      {/* Results */}
      <div className="flex-1 min-h-0 overflow-auto space-y-4">
        {!hasResults && hasQuery && (
          <div className="flex items-center justify-center h-32 text-sm text-muted-foreground/60">
            No results for &apos;{debouncedQuery}&apos;
          </div>
        )}

        {!hasResults && !hasQuery && (
          <div className="flex items-center justify-center h-32 text-sm text-muted-foreground/60">
            No items match the active filters
          </div>
        )}

        {grouped.map((section) => (
          <div key={section.type}>
            <h3 className="text-[10px] font-medium text-muted-foreground uppercase tracking-wider mb-2 sticky top-0 bg-background py-1 z-10">
              {section.label}
              <span className="text-muted-foreground/50 ml-1.5">({section.items.length})</span>
            </h3>
            <div className="space-y-1.5">
              {section.items.map((item) => {
                switch (item.kind) {
                  case "feature":
                    return <FeatureCard key={item.node.id} node={item.node} />;
                  case "pattern":
                    return (
                      <PatternCard
                        key={item.node.id}
                        node={item.node}
                        usedBy={patternUsedBy.get(item.node.label)}
                      />
                    );
                  case "tech":
                    return (
                      <TechCard
                        key={item.tech.name}
                        tech={item.tech}
                        featureCount={item.featureCount}
                      />
                    );
                  case "directory":
                    return <DirectoryCard key={item.dir.path} dir={item.dir} />;
                  case "constraint":
                    return <ConstraintCard key={item.constraint.id} constraint={item.constraint} />;
                }
              })}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function PriorityBadge({ priority }: { priority: string }) {
  const colorClass =
    priority === "core" || priority === "critical"
      ? "bg-red-500/20 text-red-400"
      : priority === "high"
        ? "bg-orange-500/20 text-orange-400"
        : priority === "medium"
          ? "bg-yellow-500/20 text-yellow-400"
          : "bg-gray-500/20 text-gray-400";

  return (
    <span className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${colorClass}`}>{priority}</span>
  );
}

function StatusBadge({ status }: { status: string }) {
  const colorClass =
    status === "completed"
      ? "bg-green-500/15 text-green-400"
      : status === "in-progress"
        ? "bg-blue-500/15 text-blue-400"
        : status === "blocked"
          ? "bg-red-500/15 text-red-400"
          : "bg-gray-500/15 text-gray-400";

  return (
    <span className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${colorClass}`}>{status}</span>
  );
}

function FeatureCard({ node }: { node: SdkArchitectureNode }) {
  return (
    <div className="px-3 py-2.5 bg-muted/30 rounded-lg border border-border/30 hover:bg-muted/50 transition-colors">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium truncate">{node.label}</span>
        <div className="flex items-center gap-1 shrink-0 ml-auto">
          {node.priority && <PriorityBadge priority={node.priority} />}
          {node.status && <StatusBadge status={node.status} />}
        </div>
      </div>

      {node.description && (
        <p className="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed">
          {node.description}
        </p>
      )}

      {node.entryPoints && node.entryPoints.length > 0 && (
        <div className="mt-1.5 space-y-0.5">
          {node.entryPoints.slice(0, 3).map((ep, i) => (
            <div
              key={`${ep}-${i}`}
              className="text-[10px] font-mono text-blue-400 truncate"
              title={ep}
            >
              {ep}
            </div>
          ))}
          {node.entryPoints.length > 3 && (
            <div className="text-[10px] text-muted-foreground/50">
              +{node.entryPoints.length - 3} more
            </div>
          )}
        </div>
      )}

      {node.techUsed && node.techUsed.length > 0 && (
        <div className="flex flex-wrap gap-1 mt-1.5">
          {node.techUsed.map((t) => (
            <span
              key={t}
              className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground"
            >
              {t}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function PatternCard({
  node,
  usedBy,
}: {
  node: SdkArchitectureNode;
  usedBy: string[] | undefined;
}) {
  return (
    <div className="px-3 py-2.5 bg-muted/30 rounded-lg border border-border/30 hover:bg-muted/50 transition-colors">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium truncate">{node.label}</span>
        {node.category && (
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-400 shrink-0 ml-auto">
            {node.category}
          </span>
        )}
      </div>

      {node.description && (
        <p className="text-xs text-muted-foreground mt-1 leading-relaxed">{node.description}</p>
      )}

      {usedBy && usedBy.length > 0 && (
        <div className="mt-1.5">
          <span className="text-[10px] text-muted-foreground/60">Used by: </span>
          <span className="text-[10px] text-foreground">{usedBy.join(", ")}</span>
        </div>
      )}
    </div>
  );
}

function TechCard({
  tech,
  featureCount,
}: {
  tech: SdkArchitectureGraph["techStack"][number];
  featureCount: number;
}) {
  return (
    <div className="px-3 py-2.5 bg-muted/30 rounded-lg border border-border/30 hover:bg-muted/50 transition-colors">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium">{tech.name}</span>
        {tech.version && <span className="text-[10px] text-muted-foreground">v{tech.version}</span>}
        <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground shrink-0 ml-auto">
          {tech.category}
        </span>
      </div>
      <p className="text-xs text-muted-foreground mt-1">{tech.purpose}</p>
      {featureCount > 0 && (
        <div className="text-[10px] text-blue-400 mt-1">
          Used by {featureCount} {featureCount === 1 ? "feature" : "features"}
        </div>
      )}
    </div>
  );
}

function DirectoryCard({ dir }: { dir: SdkArchitectureGraph["directories"][number] }) {
  return (
    <div className="px-3 py-2.5 bg-muted/30 rounded-lg border border-border/30 hover:bg-muted/50 transition-colors">
      <div className="flex items-center gap-2">
        <span className="text-sm font-mono text-blue-400 truncate" title={dir.path}>
          {dir.path}
        </span>
        {dir.required && (
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-red-500/15 text-red-400 shrink-0 ml-auto">
            required
          </span>
        )}
      </div>
      <p className="text-xs text-muted-foreground mt-1">{dir.purpose}</p>
    </div>
  );
}

function ConstraintCard({ constraint }: { constraint: SdkArchitectureConstraint }) {
  return (
    <div className="px-3 py-2.5 bg-amber-500/5 rounded-lg border border-amber-500/20 hover:bg-amber-500/10 transition-colors">
      <div className="flex items-center gap-2">
        <span className="text-sm font-mono text-amber-400 truncate" title={constraint.id}>
          {constraint.id}
        </span>
        <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/15 text-amber-400 shrink-0 ml-auto">
          {constraint.category}
        </span>
      </div>
      <p className="text-xs text-muted-foreground mt-1">{constraint.description}</p>
    </div>
  );
}
