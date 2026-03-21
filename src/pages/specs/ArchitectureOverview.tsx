import { CheckCircle2, Server, Folder, Layers, Lock, ArrowRight, Link2 } from "lucide-react";
import type {
  ArchitectureConfig,
  TechStackEntry,
  ArchitectureConstraint,
  FeatureSpec,
} from "./types";
import { SeverityBadge, CategoryBadge, StatCard, SPEC_LOAD_SOURCE_TOOLTIPS } from "./spec-badges";

function TechStackRow({ entry }: { entry: TechStackEntry }) {
  const categoryColors: Record<string, string> = {
    language: "text-blue-400",
    framework: "text-purple-400",
    library: "text-cyan-400",
    database: "text-green-400",
    service: "text-orange-400",
    tool: "text-amber-400",
    other: "text-muted-foreground",
  };

  return (
    <div className="flex items-center gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
      <Layers
        className={`w-3.5 h-3.5 shrink-0 ${categoryColors[entry.category] || "text-muted-foreground"}`}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-foreground">{entry.name}</span>
          {entry.version && (
            <span className="text-[10px] font-mono text-muted-foreground bg-white/5 px-1.5 py-0.5 rounded">
              {entry.version}
            </span>
          )}
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground/70 border border-white/10">
            {entry.category}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground mt-0.5">{entry.purpose}</p>
      </div>
    </div>
  );
}

function ConstraintRow({ constraint }: { constraint: ArchitectureConstraint }) {
  return (
    <div className="flex items-start gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02]">
      <Lock className="w-3.5 h-3.5 shrink-0 mt-0.5 text-amber-400/60" />
      <div className="flex-1 min-w-0">
        <p className="text-xs text-foreground leading-relaxed">{constraint.description}</p>
        <div className="flex items-center gap-2 mt-1.5">
          <SeverityBadge severity={constraint.severity} />
          <CategoryBadge category={constraint.category} />
          {constraint.verificationHint && (
            <span className="text-[10px] text-muted-foreground/60 italic truncate max-w-[300px]">
              verify: {constraint.verificationHint}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function FeatureRow({ feature }: { feature: FeatureSpec }) {
  const statusColors: Record<string, string> = {
    planned: "bg-blue-500/15 text-blue-400 border-blue-500/30",
    "in-progress": "bg-amber-500/15 text-amber-400 border-amber-500/30",
    completed: "bg-green-500/15 text-green-400 border-green-500/30",
    blocked: "bg-red-500/15 text-red-400 border-red-500/30",
  };
  const priorityColors: Record<string, string> = {
    critical: "text-red-400",
    high: "text-orange-400",
    medium: "text-amber-400",
    low: "text-muted-foreground",
  };

  return (
    <div className="flex items-start gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
      <CheckCircle2
        className={`w-3.5 h-3.5 shrink-0 mt-0.5 ${priorityColors[feature.priority] || "text-muted-foreground"}`}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-foreground">{feature.name}</span>
          <span
            className={`text-[10px] px-1.5 py-0.5 rounded border font-medium ${statusColors[feature.status] || statusColors.planned}`}
          >
            {feature.status}
          </span>
          <span className={`text-[10px] ${priorityColors[feature.priority]}`}>
            {feature.priority}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground mt-0.5 leading-relaxed">
          {feature.description}
        </p>
        {feature.pageSpecId && (
          <div className="flex items-center gap-1 mt-1 text-[10px] text-muted-foreground/50">
            <Link2 className="w-2.5 h-2.5" />
            spec: {feature.pageSpecId}
          </div>
        )}
      </div>
    </div>
  );
}

export function ArchitectureOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ArchitectureConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Server className="w-4 h-4 text-indigo-400" />
          <h2 className="text-sm font-semibold text-foreground">{config.projectName}</h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-indigo-500/10 text-indigo-400 border border-indigo-500/20 font-medium">
            architecture
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
        {config.description && (
          <p className="text-xs text-muted-foreground mt-1.5 leading-relaxed">
            {config.description}
          </p>
        )}
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Tech Stack" value={config.techStack.length} color="text-purple-400" />
        <StatCard label="Patterns" value={config.patterns.length} color="text-cyan-400" />
        <StatCard label="Constraints" value={config.constraints.length} color="text-amber-400" />
        <StatCard label="Features" value={config.features.length} color="text-green-400" />
      </div>

      {config.techStack.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Tech Stack ({config.techStack.length})
          </h3>
          <div className="space-y-1">
            {config.techStack.map((entry) => (
              <TechStackRow key={entry.name} entry={entry} />
            ))}
          </div>
        </div>
      )}

      {config.directories.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Directory Structure ({config.directories.length})
          </h3>
          <div className="space-y-1">
            {config.directories.map((dir) => (
              <div
                key={dir.path}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <Folder className="w-3 h-3 shrink-0 text-amber-400/60" />
                <span className="text-xs font-mono text-foreground">{dir.path}</span>
                {dir.required && (
                  <span className="text-[10px] px-1 py-0.5 rounded bg-red-500/10 text-red-400 border border-red-500/20">
                    required
                  </span>
                )}
                <span className="text-[10px] text-muted-foreground flex-1 truncate text-right">
                  {dir.purpose}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {config.patterns.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Architectural Patterns ({config.patterns.length})
          </h3>
          <div className="space-y-1">
            {config.patterns.map((pattern) => (
              <div
                key={pattern.id}
                className="px-3 py-2 rounded bg-white/[0.02] border border-white/5"
              >
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-foreground">{pattern.name}</span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20">
                    {pattern.scope}
                  </span>
                </div>
                <p className="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                  {pattern.description}
                </p>
              </div>
            ))}
          </div>
        </div>
      )}

      {config.constraints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Constraints ({config.constraints.length})
          </h3>
          <div className="space-y-1">
            {config.constraints.map((constraint) => (
              <ConstraintRow key={constraint.id} constraint={constraint} />
            ))}
          </div>
        </div>
      )}

      {config.features.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Features ({config.features.length})
          </h3>
          <div className="space-y-1">
            {config.features.map((feature) => (
              <FeatureRow key={feature.id} feature={feature} />
            ))}
          </div>
        </div>
      )}

      {config.dependencies.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Feature Dependencies ({config.dependencies.length})
          </h3>
          <div className="space-y-1">
            {config.dependencies.map((dep) => (
              <div
                key={`${dep.featureId}-${dep.dependsOn}`}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <span className="text-xs font-mono text-foreground">{dep.featureId}</span>
                <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
                  {dep.type}
                </span>
                <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
                <span className="text-xs font-mono text-foreground">{dep.dependsOn}</span>
                {dep.description && (
                  <span className="text-[10px] text-muted-foreground/50 truncate flex-1 text-right">
                    {dep.description}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
