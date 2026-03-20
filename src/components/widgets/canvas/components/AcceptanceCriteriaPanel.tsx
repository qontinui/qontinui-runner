/**
 * AcceptanceCriteriaPanel Component
 *
 * Renders acceptance criteria from the specification agent with live status tracking.
 * Shows goal summary, criteria grouped by category with priority badges and status icons,
 * and a progress bar tracking pass/fail/pending.
 */

import { CircleDashed, Loader2, CheckCircle2, XCircle, MinusCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import type { CanvasPanelComponentProps } from "./types";

interface Criterion {
  id: string;
  description: string;
  method: "command" | "ui_bridge" | "test" | "manual";
  priority: "critical" | "important" | "optional";
  verification_hint: string;
  category: string;
  status: "pending" | "running" | "passed" | "failed" | "skipped";
  last_error?: string;
}

const STATUS_CONFIG: Record<string, { icon: typeof CircleDashed; classes: string; label: string }> =
  {
    pending: { icon: CircleDashed, classes: "text-muted-foreground", label: "Pending" },
    running: { icon: Loader2, classes: "text-blue-400 animate-spin", label: "Running" },
    passed: { icon: CheckCircle2, classes: "text-green-500", label: "Passed" },
    failed: { icon: XCircle, classes: "text-red-500", label: "Failed" },
    skipped: { icon: MinusCircle, classes: "text-muted-foreground", label: "Skipped" },
  };

const PRIORITY_CONFIG: Record<string, { classes: string }> = {
  critical: { classes: "bg-red-500/10 text-red-400 border-red-500/30" },
  important: { classes: "bg-yellow-500/10 text-yellow-400 border-yellow-500/30" },
  optional: { classes: "bg-blue-500/10 text-blue-400 border-blue-500/30" },
};

const METHOD_LABELS: Record<string, string> = {
  command: "CMD",
  ui_bridge: "UI",
  test: "TEST",
  manual: "MANUAL",
};

export function AcceptanceCriteriaPanel({ data, size }: CanvasPanelComponentProps) {
  const goalSummary = (data.goal_summary as string) ?? "";
  const criteria = (data.criteria as Criterion[]) ?? [];
  const assumptions = (data.assumptions as string[]) ?? [];
  const isCompact = size === "compact";

  if (criteria.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No acceptance criteria</p>;
  }

  // Aggregate stats
  const passed = criteria.filter((c) => c.status === "passed").length;
  const failed = criteria.filter((c) => c.status === "failed").length;
  const total = criteria.length;

  // Group by category
  const grouped = criteria.reduce(
    (acc, c) => {
      const cat = c.category || "general";
      if (!acc[cat]) acc[cat] = [];
      acc[cat].push(c);
      return acc;
    },
    {} as Record<string, Criterion[]>,
  );
  const categories = Object.keys(grouped).sort();

  return (
    <div className="space-y-2">
      {/* Goal summary */}
      {goalSummary && (
        <p className={cn("font-medium", isCompact ? "text-xs" : "text-sm")}>{goalSummary}</p>
      )}

      {/* Progress bar */}
      <div className="space-y-1">
        <div className="flex items-center justify-between text-xs">
          <span className="text-muted-foreground">
            {passed}/{total} passed
            {failed > 0 && <span className="text-red-400 ml-1">({failed} failed)</span>}
          </span>
          {total > 0 && (
            <span
              className={cn(
                "font-mono",
                passed === total ? "text-green-500" : "text-muted-foreground",
              )}
            >
              {Math.round((passed / total) * 100)}%
            </span>
          )}
        </div>
        <div className="h-1.5 bg-muted rounded-full overflow-hidden flex">
          {passed > 0 && (
            <div
              className="h-full bg-green-500 transition-all"
              style={{ width: `${(passed / total) * 100}%` }}
            />
          )}
          {failed > 0 && (
            <div
              className="h-full bg-red-500 transition-all"
              style={{ width: `${(failed / total) * 100}%` }}
            />
          )}
        </div>
      </div>

      {/* Criteria grouped by category */}
      {categories.map((category) => (
        <div key={category} className="space-y-1">
          <div className="flex items-center gap-2 text-[10px] uppercase tracking-wider text-muted-foreground font-medium pt-1">
            <span>{category}</span>
            <span className="text-muted-foreground/50">({grouped[category].length})</span>
          </div>
          {grouped[category].map((criterion) => {
            const statusCfg = STATUS_CONFIG[criterion.status] ?? STATUS_CONFIG.pending;
            const priorityCfg = PRIORITY_CONFIG[criterion.priority] ?? PRIORITY_CONFIG.optional;
            const Icon = statusCfg.icon;

            return (
              <div
                key={criterion.id}
                className={cn(
                  "flex gap-2 rounded-md border border-border/50 p-2",
                  isCompact && "p-1.5",
                  criterion.status === "failed" && "border-red-500/30",
                )}
              >
                <div className="flex-shrink-0 mt-0.5">
                  <Icon className={cn("h-4 w-4", statusCfg.classes)} />
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-1.5 flex-wrap">
                    <span className={cn("font-medium", isCompact ? "text-xs" : "text-sm")}>
                      {criterion.description}
                    </span>
                    <Badge
                      className={cn(
                        "text-[9px] px-1 py-0 border uppercase tracking-wider",
                        priorityCfg.classes,
                      )}
                    >
                      {criterion.priority}
                    </Badge>
                    <Badge className="text-[9px] px-1 py-0 border bg-muted/50 text-muted-foreground border-border/50">
                      {METHOD_LABELS[criterion.method] ?? criterion.method}
                    </Badge>
                  </div>
                  {criterion.verification_hint && (
                    <p
                      className={cn(
                        "text-muted-foreground mt-0.5 font-mono",
                        isCompact ? "text-[10px]" : "text-xs",
                      )}
                    >
                      {criterion.verification_hint}
                    </p>
                  )}
                  {criterion.last_error && (
                    <p className="text-red-400 text-[10px] mt-0.5 line-clamp-2">
                      {criterion.last_error}
                    </p>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ))}

      {/* Assumptions */}
      {assumptions.length > 0 && (
        <div className="pt-1 space-y-0.5">
          <span className="text-[10px] uppercase tracking-wider text-muted-foreground font-medium">
            Assumptions
          </span>
          {assumptions.map((a, i) => (
            <p key={`assumption-${i}`} className="text-[10px] text-muted-foreground italic pl-2">
              {a}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
