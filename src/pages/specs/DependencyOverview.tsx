import { GitBranch, ArrowRight } from "lucide-react";
import type { DependencyConfig, DependencyLink, ModuleRef } from "./types";
import { StatCard, SPEC_LOAD_SOURCE_TOOLTIPS } from "./spec-badges";

const ARTIFACT_COLORS: Record<string, string> = {
  page: "text-purple-400",
  "api-endpoint": "text-cyan-400",
  "data-entity": "text-green-400",
  module: "text-blue-400",
  service: "text-orange-400",
  component: "text-pink-400",
};

const LINK_TYPE_COLORS: Record<string, string> = {
  calls: "bg-cyan-500/10 text-cyan-400 border-cyan-500/20",
  reads: "bg-green-500/10 text-green-400 border-green-500/20",
  writes: "bg-amber-500/10 text-amber-400 border-amber-500/20",
  renders: "bg-purple-500/10 text-purple-400 border-purple-500/20",
  imports: "bg-blue-500/10 text-blue-400 border-blue-500/20",
  extends: "bg-pink-500/10 text-pink-400 border-pink-500/20",
  configures: "bg-orange-500/10 text-orange-400 border-orange-500/20",
};

function LinkRow({ link }: { link: DependencyLink }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className={`text-[10px] ${ARTIFACT_COLORS[link.from.kind] || "text-muted-foreground"}`}>
        {link.from.kind}
      </span>
      <span className="text-xs font-mono text-foreground">
        {link.from.label || link.from.artifactId}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span
        className={`text-[10px] px-1.5 py-0.5 rounded border ${LINK_TYPE_COLORS[link.type] || "bg-white/5 text-muted-foreground border-white/10"}`}
      >
        {link.type}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className={`text-[10px] ${ARTIFACT_COLORS[link.to.kind] || "text-muted-foreground"}`}>
        {link.to.kind}
      </span>
      <span className="text-xs font-mono text-foreground">
        {link.to.label || link.to.artifactId}
      </span>
      {!link.required && (
        <span className="text-[10px] text-muted-foreground/40 italic ml-auto">optional</span>
      )}
    </div>
  );
}

function ModuleCard({ module }: { module: ModuleRef }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
        {module.moduleType}
      </span>
      <span className="text-xs font-medium text-foreground">{module.name}</span>
      <span className="text-[10px] font-mono text-muted-foreground/50">{module.path}</span>
      {module.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {module.description}
        </span>
      )}
    </div>
  );
}

export function DependencyOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: DependencyConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  const byType = new Map<string, number>();
  for (const link of config.links) {
    byType.set(link.type, (byType.get(link.type) || 0) + 1);
  }

  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <GitBranch className="w-4 h-4 text-blue-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Dependencies"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/10 text-blue-400 border border-blue-500/20 font-medium">
            dependencies
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Modules" value={config.modules.length} color="text-blue-400" />
        <StatCard label="Links" value={config.links.length} color="text-cyan-400" />
        <StatCard
          label="Required"
          value={config.links.filter((l) => l.required).length}
          color="text-red-400"
        />
        <StatCard label="Clusters" value={config.clusters?.length || 0} color="text-purple-400" />
      </div>

      <div className="flex flex-wrap gap-1.5">
        {Array.from(byType.entries())
          .sort((a, b) => b[1] - a[1])
          .map(([type, count]) => (
            <span
              key={type}
              className={`text-[10px] px-2 py-0.5 rounded border ${LINK_TYPE_COLORS[type] || "bg-white/5 text-muted-foreground border-white/10"}`}
            >
              {type}: {count}
            </span>
          ))}
      </div>

      {config.modules.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Modules ({config.modules.length})
          </h3>
          <div className="space-y-1">
            {config.modules.map((mod) => (
              <ModuleCard key={mod.id} module={mod} />
            ))}
          </div>
        </div>
      )}

      {config.links.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Dependency Links ({config.links.length})
          </h3>
          <div className="space-y-1">
            {config.links.map((link) => (
              <LinkRow key={link.id} link={link} />
            ))}
          </div>
        </div>
      )}

      {config.clusters && config.clusters.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Build Clusters ({config.clusters.length})
          </h3>
          <div className="space-y-2">
            {config.clusters
              .sort((a, b) => a.buildOrder - b.buildOrder)
              .map((cluster) => (
                <div
                  key={cluster.id}
                  className="px-3 py-2 rounded border border-white/5 bg-white/[0.02]"
                >
                  <div className="flex items-center gap-2">
                    <span className="text-[10px] font-mono text-muted-foreground/50 tabular-nums w-6">
                      #{cluster.buildOrder}
                    </span>
                    <span className="text-xs font-medium text-foreground">{cluster.name}</span>
                    <span className="text-[10px] text-muted-foreground">
                      {cluster.artifacts.length} artifacts
                    </span>
                  </div>
                  <p className="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                    {cluster.description}
                  </p>
                  <div className="flex flex-wrap gap-1 mt-1.5">
                    {cluster.artifacts.map((ref) => (
                      <span
                        key={`${ref.artifactId}-${ref.kind}`}
                        className={`text-[10px] px-1.5 py-0.5 rounded bg-white/5 border border-white/10 ${ARTIFACT_COLORS[ref.kind] || "text-muted-foreground"}`}
                      >
                        {ref.label || ref.artifactId}
                      </span>
                    ))}
                  </div>
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}
