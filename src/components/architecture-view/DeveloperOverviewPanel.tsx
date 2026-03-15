import { useMemo } from "react";
import {
  Boxes,
  Layers,
  Cpu,
  GitBranch,
  CircleDot,
  ShieldAlert,
  FolderTree,
  FileText,
} from "lucide-react";
import type { SdkArchitectureGraph, SdkArchitectureConstraint } from "@/types/architecture";

interface Props {
  project: SdkArchitectureGraph;
}

export function DeveloperOverviewPanel({ project }: Props) {
  const features = useMemo(
    () => project.nodes.filter((n) => n.type === "feature"),
    [project.nodes],
  );

  const patterns = useMemo(
    () => project.nodes.filter((n) => n.type === "pattern"),
    [project.nodes],
  );

  const orphanCount = useMemo(() => {
    const connected = new Set<string>();
    for (const edge of project.edges) {
      connected.add(edge.source);
      connected.add(edge.target);
    }
    return features.filter((f) => !connected.has(f.id)).length;
  }, [features, project.edges]);

  const avgDeps = useMemo(() => {
    if (features.length === 0) return 0;
    return project.edges.length / features.length;
  }, [features.length, project.edges.length]);

  const constraintsByCategory = useMemo(() => {
    const groups = new Map<string, SdkArchitectureConstraint[]>();
    for (const c of project.constraints) {
      const cat = c.category || "general";
      const list = groups.get(cat) ?? [];
      list.push(c);
      groups.set(cat, list);
    }
    return groups;
  }, [project.constraints]);

  const sortedDirectories = useMemo(
    () => [...project.directories].sort((a, b) => a.path.localeCompare(b.path)),
    [project.directories],
  );

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[3fr_2fr] gap-4">
      {/* Left column */}
      <div className="space-y-4">
        {/* Stats grid */}
        <div className="grid grid-cols-3 gap-2">
          <StatCard
            icon={<Boxes className="w-4 h-4 text-blue-400" />}
            label="Total Features"
            value={features.length}
          />
          <StatCard
            icon={<Layers className="w-4 h-4 text-purple-400" />}
            label="Total Patterns"
            value={patterns.length}
          />
          <StatCard
            icon={<Cpu className="w-4 h-4 text-cyan-400" />}
            label="Tech Stack Size"
            value={project.techStack.length}
          />
          <StatCard
            icon={<GitBranch className="w-4 h-4 text-green-400" />}
            label="Avg Deps/Feature"
            value={avgDeps.toFixed(1)}
          />
          <StatCard
            icon={<CircleDot className="w-4 h-4 text-orange-400" />}
            label="Orphan Features"
            value={orphanCount}
          />
          <StatCard
            icon={<ShieldAlert className="w-4 h-4 text-amber-400" />}
            label="Constraints"
            value={project.constraints.length}
          />
        </div>

        {/* Constraint Awareness */}
        <div className="border border-amber-500/30 bg-amber-500/5 rounded-lg p-4">
          <div className="flex items-center gap-2 mb-3">
            <ShieldAlert className="w-4.5 h-4.5 text-amber-400" />
            <h3 className="text-sm font-semibold text-amber-300">Architectural Constraints</h3>
          </div>
          {project.constraints.length === 0 ? (
            <p className="text-xs text-muted-foreground">No constraints defined</p>
          ) : (
            <div className="space-y-3">
              {Array.from(constraintsByCategory.entries()).map(([category, constraints]) => (
                <div key={category}>
                  <span className="text-[10px] uppercase tracking-wider text-amber-400/60 font-medium">
                    {category}
                  </span>
                  <div className="mt-1 space-y-1.5">
                    {constraints.map((c) => (
                      <div key={c.id} className="flex gap-2 text-xs">
                        <code className="text-amber-400 font-mono text-[11px] shrink-0">
                          {c.id}
                        </code>
                        <span className="text-muted-foreground">{c.description}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Pattern Usage Map */}
        {patterns.length > 0 && (
          <div className="border border-border/50 bg-card rounded-lg p-4">
            <div className="flex items-center gap-2 mb-3">
              <Layers className="w-4 h-4 text-purple-400" />
              <h3 className="text-sm font-semibold">Pattern Usage Map</h3>
            </div>
            <div className="space-y-2">
              {patterns.map((pattern) => (
                <div
                  key={pattern.id}
                  className="px-3 py-2 bg-muted/40 rounded-md"
                  title={pattern.description ?? undefined}
                >
                  <div className="flex items-center gap-2 mb-1">
                    <span className="text-xs font-medium">{pattern.label}</span>
                    {pattern.category && (
                      <span className="text-[10px] text-muted-foreground/60">
                        {pattern.category}
                      </span>
                    )}
                  </div>
                  {pattern.usedBy && pattern.usedBy.length > 0 ? (
                    <div className="flex flex-wrap gap-1">
                      {pattern.usedBy.map((featureId) => (
                        <span
                          key={featureId}
                          className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/10 text-purple-400"
                        >
                          {featureId.replace(/^feature:/, "")}
                        </span>
                      ))}
                    </div>
                  ) : (
                    <span className="text-[10px] text-muted-foreground/40">
                      No features using this pattern
                    </span>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Right column */}
      <div className="space-y-4">
        {/* Project description */}
        {project.description && (
          <div className="border border-border/50 bg-card rounded-lg p-4">
            <div className="flex items-center gap-2 mb-2">
              <FileText className="w-4 h-4 text-muted-foreground" />
              <h3 className="text-sm font-semibold">Project Description</h3>
            </div>
            <p className="text-xs text-muted-foreground leading-relaxed">{project.description}</p>
          </div>
        )}

        {/* Directory Map */}
        <div className="border border-border/50 bg-card rounded-lg p-4">
          <div className="flex items-center gap-2 mb-3">
            <FolderTree className="w-4 h-4 text-blue-400" />
            <h3 className="text-sm font-semibold">Directory Map</h3>
          </div>
          {sortedDirectories.length === 0 ? (
            <p className="text-xs text-muted-foreground/60">No directories defined</p>
          ) : (
            <div className="space-y-1.5">
              {sortedDirectories.map((dir, i) => (
                <div key={i} className="flex items-start gap-2 py-1">
                  <code className="text-xs font-mono text-blue-400 shrink-0">{dir.path}</code>
                  {dir.required && (
                    <span className="text-[9px] px-1 py-0.5 rounded bg-green-500/15 text-green-400 font-medium shrink-0">
                      required
                    </span>
                  )}
                  <span className="text-xs text-muted-foreground">{dir.purpose}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function StatCard({
  icon,
  label,
  value,
}: {
  icon: React.ReactNode;
  label: string;
  value: string | number;
}) {
  return (
    <div className="flex items-center gap-2.5 px-3 py-2.5 bg-card border border-border/50 rounded-lg">
      {icon}
      <div>
        <div className="text-[10px] text-muted-foreground">{label}</div>
        <div className="text-sm font-semibold">{value}</div>
      </div>
    </div>
  );
}
