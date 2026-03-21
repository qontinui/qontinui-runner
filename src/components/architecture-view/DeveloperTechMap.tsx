import { useState, useMemo } from "react";
import { Cpu, Layers } from "lucide-react";
import type { SdkArchitectureGraph, SdkArchitectureNode } from "@/types/architecture";

interface Props {
  project: SdkArchitectureGraph;
}

export function DeveloperTechMap({ project }: Props) {
  const { nodes, techStack } = project;
  const [hoveredTech, setHoveredTech] = useState<string | null>(null);
  const [hoveredFeature, setHoveredFeature] = useState<string | null>(null);

  const features = useMemo(
    () => nodes.filter((n): n is SdkArchitectureNode & { type: "feature" } => n.type === "feature"),
    [nodes],
  );

  const techToFeatures = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const feature of features) {
      for (const techName of feature.techUsed ?? []) {
        const existing = map.get(techName);
        if (existing) {
          existing.push(feature.label);
        } else {
          map.set(techName, [feature.label]);
        }
      }
    }
    return map;
  }, [features]);

  const featureToTech = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const feature of features) {
      if (feature.techUsed && feature.techUsed.length > 0) {
        map.set(feature.label, [...feature.techUsed]);
      }
    }
    return map;
  }, [features]);

  const techByCategory = useMemo(() => {
    const groups = new Map<string, typeof techStack>();
    for (const tech of techStack) {
      const category = tech.category || "Other";
      const existing = groups.get(category);
      if (existing) {
        existing.push(tech);
      } else {
        groups.set(category, [tech]);
      }
    }
    return groups;
  }, [techStack]);

  const isHighlightActive = hoveredTech !== null || hoveredFeature !== null;

  function isTechHighlighted(techName: string): boolean {
    if (!isHighlightActive) return false;
    if (hoveredTech === techName) return true;
    if (hoveredFeature) {
      const techList = featureToTech.get(hoveredFeature);
      return techList ? techList.includes(techName) : false;
    }
    return false;
  }

  function isFeatureHighlighted(featureLabel: string): boolean {
    if (!isHighlightActive) return false;
    if (hoveredFeature === featureLabel) return true;
    if (hoveredTech) {
      const featureList = techToFeatures.get(hoveredTech);
      return featureList ? featureList.includes(featureLabel) : false;
    }
    return false;
  }

  return (
    <div className="flex gap-3 h-full">
      {/* Left column: Tech by Category */}
      <div className="flex-1 overflow-auto space-y-3 pr-1">
        {Array.from(techByCategory.entries()).map(([category, techs]) => (
          <div
            key={category}
            className="border border-border/50 rounded-lg bg-card overflow-hidden"
          >
            <div className="px-3 py-2 border-b border-border/50 bg-muted/30">
              <div className="flex items-center gap-1.5">
                <Cpu className="w-3.5 h-3.5 text-muted-foreground" />
                <h3 className="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">
                  {category}
                </h3>
              </div>
            </div>
            <div className="p-2 space-y-1">
              {techs.map((tech) => {
                const featureCount = techToFeatures.get(tech.name)?.length ?? 0;
                const highlighted = isTechHighlighted(tech.name);
                const dimmed = isHighlightActive && !highlighted;

                return (
                  <div
                    key={tech.name}
                    onMouseEnter={() => setHoveredTech(tech.name)}
                    onMouseLeave={() => setHoveredTech(null)}
                    className={`px-2.5 py-2 rounded cursor-default transition-all ${
                      highlighted
                        ? "ring-2 ring-blue-500/50 bg-blue-500/10"
                        : dimmed
                          ? "opacity-40"
                          : "bg-muted/30 hover:bg-muted/60"
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <div className="flex items-baseline gap-1.5 min-w-0">
                        <span className="text-sm font-medium truncate">{tech.name}</span>
                        {tech.version && (
                          <span className="text-[10px] text-muted-foreground shrink-0">
                            v{tech.version}
                          </span>
                        )}
                      </div>
                      {featureCount > 0 && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/15 text-blue-400 shrink-0">
                          {featureCount} {featureCount === 1 ? "feature" : "features"}
                        </span>
                      )}
                    </div>
                    <div className="text-[10px] text-muted-foreground mt-0.5 truncate">
                      {tech.purpose}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        ))}

        {techByCategory.size === 0 && (
          <div className="flex items-center justify-center h-32 text-sm text-muted-foreground/60">
            No tech stack defined
          </div>
        )}
      </div>

      {/* Right column: Feature sidebar */}
      <div className="w-72 shrink-0 border border-border/50 rounded-lg bg-card overflow-hidden flex flex-col">
        <div className="px-3 py-2 border-b border-border/50 bg-muted/30">
          <div className="flex items-center gap-1.5">
            <Layers className="w-3.5 h-3.5 text-muted-foreground" />
            <h3 className="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">
              Features
            </h3>
            <span className="text-[10px] text-muted-foreground/60 ml-auto">{features.length}</span>
          </div>
        </div>
        <div className="flex-1 overflow-auto p-2 space-y-1">
          {features.map((feature) => {
            const highlighted = isFeatureHighlighted(feature.label);
            const dimmed = isHighlightActive && !highlighted;
            const techList = featureToTech.get(feature.label) ?? [];

            return (
              <div
                key={feature.id}
                onMouseEnter={() => setHoveredFeature(feature.label)}
                onMouseLeave={() => setHoveredFeature(null)}
                className={`px-2.5 py-2 rounded cursor-default transition-all ${
                  highlighted
                    ? "ring-2 ring-blue-500/50 bg-blue-500/10"
                    : dimmed
                      ? "opacity-40"
                      : "bg-muted/30 hover:bg-muted/60"
                }`}
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="text-sm font-medium truncate">{feature.label}</span>
                  {feature.priority && (
                    <span
                      className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${
                        feature.priority === "critical"
                          ? "bg-red-500/20 text-red-400"
                          : feature.priority === "high"
                            ? "bg-orange-500/20 text-orange-400"
                            : feature.priority === "medium"
                              ? "bg-yellow-500/20 text-yellow-400"
                              : "bg-gray-500/20 text-gray-400"
                      }`}
                    >
                      {feature.priority}
                    </span>
                  )}
                </div>
                {techList.length > 0 && (
                  <div className="flex flex-wrap gap-1 mt-1.5">
                    {techList.map((t) => (
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
          })}

          {features.length === 0 && (
            <div className="flex items-center justify-center h-24 text-xs text-muted-foreground/60">
              No features defined
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
