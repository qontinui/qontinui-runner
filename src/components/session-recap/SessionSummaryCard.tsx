import {
  FileText,
  GitBranch,
  Database,
  Code2,
  Plug,
  Component,
  Plus,
  Minus,
} from "lucide-react";
import type { RecapSummary, RepoSummary } from "@/lib/session-recap/types";

interface Props {
  summary: RecapSummary;
  repos: RepoSummary[];
  lookback: string;
}

export function SessionSummaryCard({ summary, repos, lookback }: Props) {
  const stats = [
    { icon: FileText, label: "Files changed", value: summary.total_files, color: "#3B82F6" },
    { icon: GitBranch, label: "Repos touched", value: summary.total_repos, color: "#8B5CF6" },
    { icon: Code2, label: "Types defined", value: summary.new_types, color: "#F97316" },
    { icon: Plug, label: "Endpoints added", value: summary.new_endpoints, color: "#F59E0B" },
    { icon: Database, label: "DB changes", value: summary.new_tables, color: "#A855F7" },
    { icon: Component, label: "Components", value: summary.new_components, color: "#3B82F6" },
  ];

  const categories = Object.entries(summary.categories).sort(
    ([, a], [, b]) => b - a,
  );

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-foreground">
          Session Recap
        </h2>
        <span className="text-xs text-muted-foreground">
          lookback: {lookback}
        </span>
      </div>

      {/* Stat grid */}
      <div className="grid grid-cols-3 gap-2">
        {stats.map((s) => (
          <div
            key={s.label}
            className="bg-card/50 border border-border/30 rounded-lg p-3 flex items-center gap-2"
          >
            <s.icon className="w-4 h-4 shrink-0" style={{ color: s.color }} />
            <div className="min-w-0">
              <div className="text-lg font-bold leading-none">{s.value}</div>
              <div className="text-[10px] text-muted-foreground truncate">
                {s.label}
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* Lines added / removed */}
      <div className="flex gap-4 text-xs">
        <span className="flex items-center gap-1 text-green-500">
          <Plus className="w-3 h-3" />
          {summary.total_lines_added.toLocaleString()} added
        </span>
        <span className="flex items-center gap-1 text-red-400">
          <Minus className="w-3 h-3" />
          {summary.total_lines_removed.toLocaleString()} removed
        </span>
      </div>

      {/* Category breakdown */}
      {categories.length > 0 && (
        <div className="space-y-1">
          <h3 className="text-[11px] font-medium text-muted-foreground">
            By category
          </h3>
          <div className="flex flex-wrap gap-1.5">
            {categories.map(([cat, count]) => (
              <span
                key={cat}
                className="px-2 py-0.5 rounded-full text-[10px] bg-muted text-muted-foreground"
              >
                {cat}: {count}
              </span>
            ))}
          </div>
        </div>
      )}

      {/* Per-repo */}
      {repos.length > 0 && (
        <div className="space-y-1">
          <h3 className="text-[11px] font-medium text-muted-foreground">
            Repos
          </h3>
          <div className="space-y-1">
            {repos.map((r) => (
              <div
                key={r.name}
                className="flex items-center justify-between text-[11px] px-2 py-1 rounded bg-muted/50"
              >
                <span className="font-medium">{r.name}</span>
                <span className="text-muted-foreground">
                  {r.files_changed} files, +{r.lines_added} -{r.lines_removed}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
