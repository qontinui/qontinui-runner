/**
 * FindingListPanel Component
 *
 * Renders a list of findings/issues with severity badges and details.
 */

import { AlertCircle, AlertTriangle, Info, CheckCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface Finding {
  id?: string;
  title: string;
  description?: string;
  severity?: "info" | "low" | "medium" | "high" | "critical";
  location?: string;
}

const SEVERITY_CONFIG: Record<
  string,
  { icon: typeof Info; classes: string; badgeClasses: string; shape: string }
> = {
  info: {
    icon: Info,
    classes: "text-blue-400",
    badgeClasses: "bg-blue-500/10 text-blue-400 border-blue-500/30",
    shape: "circle",
  },
  low: {
    icon: CheckCircle,
    classes: "text-green-400",
    badgeClasses: "bg-green-500/10 text-green-400 border-green-500/30",
    shape: "circle",
  },
  medium: {
    icon: AlertTriangle,
    classes: "text-yellow-400",
    badgeClasses: "bg-yellow-500/10 text-yellow-400 border-yellow-500/30",
    shape: "triangle",
  },
  high: {
    icon: AlertCircle,
    classes: "text-orange-400",
    badgeClasses: "bg-orange-500/10 text-orange-400 border-orange-500/30",
    shape: "diamond",
  },
  critical: {
    icon: AlertCircle,
    classes: "text-red-400",
    badgeClasses: "bg-red-500/10 text-red-400 border-red-500/30",
    shape: "diamond",
  },
};

export function FindingListPanel({ data, size }: CanvasPanelComponentProps) {
  const findings = (data.findings as Finding[]) ?? [];
  const isCompact = size === "compact";

  if (findings.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No findings</p>;
  }

  // Count by severity for the summary bar
  const counts = findings.reduce(
    (acc, f) => {
      const sev = f.severity ?? "info";
      acc[sev] = (acc[sev] ?? 0) + 1;
      return acc;
    },
    {} as Record<string, number>,
  );
  const severityOrder = ["critical", "high", "medium", "low", "info"];

  return (
    <div className="space-y-2">
      {/* Severity summary */}
      <div className="flex items-center gap-2 text-[10px]">
        <span className="text-muted-foreground">{findings.length} findings:</span>
        {severityOrder
          .filter((s) => counts[s])
          .map((s) => {
            const config = SEVERITY_CONFIG[s] ?? SEVERITY_CONFIG.info;
            return (
              <span key={s} className={cn("font-medium", config.classes)}>
                {counts[s]} {s}
              </span>
            );
          })}
      </div>

      {findings.map((finding, idx) => {
        const severity = finding.severity ?? "info";
        const config = SEVERITY_CONFIG[severity] ?? SEVERITY_CONFIG.info;
        const Icon = config.icon;

        return (
          <div
            key={finding.id ?? idx}
            className={cn(
              "flex gap-2 rounded-md border border-border/50 p-2",
              isCompact && "p-1.5",
            )}
          >
            <div className="relative flex-shrink-0 mt-0.5">
              <Icon className={cn("h-4 w-4", config.classes)} />
              {config.shape === "diamond" && (
                <div className="absolute -bottom-0.5 -right-0.5 w-1.5 h-1.5 bg-current rotate-45 opacity-60" />
              )}
              {config.shape === "triangle" && (
                <div className="absolute -bottom-0.5 -right-0.5 w-0 h-0 border-l-[3px] border-r-[3px] border-b-[5px] border-l-transparent border-r-transparent border-b-current opacity-60" />
              )}
            </div>
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2 flex-wrap">
                <span className={cn("font-medium", isCompact ? "text-xs" : "text-sm")}>
                  {finding.title}
                </span>
                <Badge
                  className={cn(
                    "text-[9px] px-1 py-0 border uppercase tracking-wider",
                    config.badgeClasses,
                  )}
                >
                  {severity}
                </Badge>
              </div>
              {finding.description && (
                <p
                  className={cn(
                    "text-muted-foreground mt-0.5",
                    isCompact ? "text-[10px]" : "text-xs",
                  )}
                >
                  {finding.description}
                </p>
              )}
              {finding.location && (
                <span className="text-[10px] font-mono text-muted-foreground mt-0.5 block">
                  {finding.location}
                </span>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
