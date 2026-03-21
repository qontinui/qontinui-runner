import { Gauge, Zap, Layers, Monitor, Smartphone } from "lucide-react";
import type {
  ConstraintConfig,
  PerformanceBudget,
  BundleBudget,
  BrowserTarget,
  ResponsiveBreakpoint,
} from "./types";
import { StatCard, SPEC_LOAD_SOURCE_TOOLTIPS } from "./spec-badges";

function PerfBudgetRow({ budget }: { budget: PerformanceBudget }) {
  const metricLabel =
    budget.metric === "custom" ? budget.customMetric || "custom" : budget.metric.toUpperCase();
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Zap className="w-3 h-3 text-amber-400/60" />
      <span className="text-xs font-mono font-medium text-foreground">{metricLabel}</span>
      <span className="text-[10px] font-mono text-cyan-400">
        ≤ {budget.budget}
        {budget.unit}
      </span>
      {budget.pageIds && budget.pageIds.length > 0 && (
        <span className="text-[10px] text-muted-foreground/50">
          pages: {budget.pageIds.join(", ")}
        </span>
      )}
      {budget.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {budget.description}
        </span>
      )}
    </div>
  );
}

function BundleBudgetRow({ budget }: { budget: BundleBudget }) {
  const sizeLabel =
    budget.maxSizeBytes >= 1024 * 1024
      ? `${(budget.maxSizeBytes / (1024 * 1024)).toFixed(1)} MB`
      : `${(budget.maxSizeBytes / 1024).toFixed(0)} KB`;
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Layers className="w-3 h-3 text-purple-400/60" />
      <span className="text-xs font-mono text-foreground">{budget.target}</span>
      {budget.name && (
        <span className="text-[10px] font-mono text-muted-foreground">{budget.name}</span>
      )}
      <span className="text-[10px] font-mono text-cyan-400">≤ {sizeLabel}</span>
      <span className="text-[10px] text-muted-foreground/50">
        {budget.compressed ? "gzipped" : "raw"}
      </span>
    </div>
  );
}

function BrowserRow({ target }: { target: BrowserTarget }) {
  const supportColors: Record<string, string> = {
    full: "text-green-400",
    partial: "text-amber-400",
    none: "text-red-400",
  };
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Monitor className="w-3 h-3 text-blue-400/60" />
      <span className="text-xs text-foreground">{target.browser}</span>
      <span className="text-[10px] font-mono text-muted-foreground">≥ {target.minVersion}</span>
      <span
        className={`text-[10px] font-medium ${supportColors[target.support] || "text-muted-foreground"}`}
      >
        {target.support}
      </span>
    </div>
  );
}

function BreakpointRow({ bp }: { bp: ResponsiveBreakpoint }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Smartphone className="w-3 h-3 text-pink-400/60" />
      <span className="text-xs font-medium text-foreground">{bp.name}</span>
      <span className="text-[10px] font-mono text-cyan-400">
        {bp.minWidth}px{bp.maxWidth ? ` – ${bp.maxWidth}px` : "+"}
      </span>
      {bp.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {bp.description}
        </span>
      )}
    </div>
  );
}

export function ConstraintOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ConstraintConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Gauge className="w-4 h-4 text-amber-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Project Constraints"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20 font-medium">
            constraints
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
        <StatCard
          label="Performance"
          value={config.performance?.length || 0}
          color="text-amber-400"
        />
        <StatCard
          label="Bundles"
          value={config.bundleBudgets?.length || 0}
          color="text-purple-400"
        />
        <StatCard label="Browsers" value={config.browsers?.length || 0} color="text-blue-400" />
        <StatCard
          label="Breakpoints"
          value={config.breakpoints?.length || 0}
          color="text-pink-400"
        />
      </div>

      {config.performance && config.performance.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Performance Budgets ({config.performance.length})
          </h3>
          <div className="space-y-1">
            {config.performance.map((budget) => (
              <PerfBudgetRow key={budget.id} budget={budget} />
            ))}
          </div>
        </div>
      )}

      {config.bundleBudgets && config.bundleBudgets.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Bundle Budgets ({config.bundleBudgets.length})
          </h3>
          <div className="space-y-1">
            {config.bundleBudgets.map((budget) => (
              <BundleBudgetRow key={budget.id} budget={budget} />
            ))}
          </div>
        </div>
      )}

      {config.browsers && config.browsers.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Browser Support ({config.browsers.length})
          </h3>
          <div className="space-y-1">
            {config.browsers.map((target) => (
              <BrowserRow key={target.browser} target={target} />
            ))}
          </div>
        </div>
      )}

      {config.breakpoints && config.breakpoints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Responsive Breakpoints ({config.breakpoints.length})
          </h3>
          <div className="space-y-1">
            {config.breakpoints.map((bp) => (
              <BreakpointRow key={bp.id} bp={bp} />
            ))}
          </div>
        </div>
      )}

      {config.accessibility && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">Accessibility</h3>
          <div className="px-3 py-2 rounded border border-white/5 bg-white/[0.02] space-y-1.5">
            <div className="flex items-center gap-2">
              <span className="text-xs font-medium text-foreground">
                WCAG {config.accessibility.level}
              </span>
              {config.accessibility.minContrastRatio && (
                <span className="text-[10px] text-muted-foreground">
                  min contrast: {config.accessibility.minContrastRatio}:1
                </span>
              )}
              {config.accessibility.minTouchTarget && (
                <span className="text-[10px] text-muted-foreground">
                  min touch: {config.accessibility.minTouchTarget}px
                </span>
              )}
            </div>
            {config.accessibility.description && (
              <p className="text-[10px] text-muted-foreground/60 leading-relaxed">
                {config.accessibility.description}
              </p>
            )}
            {config.accessibility.requiredCriteria &&
              config.accessibility.requiredCriteria.length > 0 && (
                <div className="flex flex-wrap gap-1">
                  {config.accessibility.requiredCriteria.map((c) => (
                    <span
                      key={c}
                      className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
                    >
                      {c}
                    </span>
                  ))}
                </div>
              )}
          </div>
        </div>
      )}

      {config.capacity && config.capacity.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Capacity Constraints ({config.capacity.length})
          </h3>
          <div className="space-y-1">
            {config.capacity.map((cap) => (
              <div
                key={cap.id}
                className="px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-foreground">{cap.target}</span>
                  {cap.maxConcurrent && (
                    <span className="text-[10px] text-cyan-400">
                      max concurrent: {cap.maxConcurrent}
                    </span>
                  )}
                  {cap.rateLimit && (
                    <span className="text-[10px] text-amber-400">
                      {cap.rateLimit.requests}/{cap.rateLimit.windowSeconds}s
                    </span>
                  )}
                  {cap.maxResponseTimeMs && (
                    <span className="text-[10px] text-purple-400">≤ {cap.maxResponseTimeMs}ms</span>
                  )}
                </div>
                {cap.description && (
                  <p className="text-[10px] text-muted-foreground/60 mt-0.5">{cap.description}</p>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
