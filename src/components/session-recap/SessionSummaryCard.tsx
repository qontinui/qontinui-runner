import {
  FileText,
  GitBranch,
  Database,
  Code2,
  Plug,
  Component,
  Plus,
  Minus,
  AlertTriangle,
} from "lucide-react";
import type { RecapSummary, RepoScanGap, RepoSummary } from "@/lib/session-recap/types";

interface Props {
  summary: RecapSummary;
  repos: RepoSummary[];
  lookback: string;
  /**
   * The repos the git budget cost the scan something on. Every number in this
   * card is a LOWER BOUND while this is non-empty, and saying so is the whole
   * reason it is passed down: a truncated scan and a quiet one produce the
   * same zeros.
   */
  scanGaps: RepoScanGap[];
}

export function SessionSummaryCard({ summary, repos, lookback, scanGaps }: Props) {
  const notStarted = scanGaps.filter((g) => g.state === "not-started");
  const cutShort = scanGaps.filter((g) => g.state === "cut-short");
  const stats = [
    { icon: FileText, label: "Files changed", value: summary.total_files, color: "#3B82F6" },
    { icon: GitBranch, label: "Repos touched", value: summary.total_repos, color: "#8B5CF6" },
    { icon: Code2, label: "Types defined", value: summary.new_types, color: "#F97316" },
    { icon: Plug, label: "Endpoints added", value: summary.new_endpoints, color: "#F59E0B" },
    { icon: Database, label: "DB changes", value: summary.new_tables, color: "#A855F7" },
    { icon: Component, label: "Components", value: summary.new_components, color: "#3B82F6" },
  ];

  const categories = Object.entries(summary.categories).sort(([, a], [, b]) => b - a);

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-foreground">Session Recap</h2>
        <span className="text-xs text-muted-foreground">lookback: {lookback}</span>
      </div>

      {/*
        PARTIALITY, ABOVE THE NUMBERS. A recap whose git budget ran out is not
        an empty recap, and every stat below it is a floor rather than a count.
        Rendered from the summary's own `scan_complete`, so the banner and what
        gets persisted are the same statement.
      */}
      {!summary.scan_complete && (
        <div className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2.5 py-2 space-y-1.5">
          <div className="flex items-center gap-1.5 text-[11px] font-medium text-amber-500">
            <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
            Partial recap — the git budget ran out
          </div>
          <p className="text-[10px] leading-snug text-muted-foreground">
            Every number below is a LOWER BOUND, not a count. An empty list here does not mean there
            was nothing to find.
          </p>
          {notStarted.length > 0 && (
            <div className="text-[10px] leading-snug">
              <span className="font-medium text-foreground">
                {notStarted.length} repo(s) not scanned at all:
              </span>{" "}
              <span className="text-muted-foreground font-mono">
                {notStarted.map((g) => g.repo).join(", ")}
              </span>
              <div className="text-muted-foreground">
                Absent from everything here — that absence says nothing about whether they changed.
              </div>
            </div>
          )}
          {cutShort.length > 0 && (
            <div className="text-[10px] leading-snug">
              <span className="font-medium text-foreground">
                {cutShort.length} repo(s) scanned only in part:
              </span>{" "}
              <span className="text-muted-foreground font-mono">
                {cutShort.map((g) => g.repo).join(", ")}
              </span>
              <div className="text-muted-foreground">
                Counted below with incomplete numbers — their empty type / endpoint / table lists do
                not mean there were none.
              </div>
            </div>
          )}
        </div>
      )}

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
              <div className="text-[10px] text-muted-foreground truncate">{s.label}</div>
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
          <h3 className="text-[11px] font-medium text-muted-foreground">By category</h3>
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
          <h3 className="text-[11px] font-medium text-muted-foreground">Repos</h3>
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
