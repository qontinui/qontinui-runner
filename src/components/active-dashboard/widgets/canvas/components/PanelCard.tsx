/**
 * PanelCard Component
 *
 * Renders a single canvas panel with header, expand/collapse, copy, and maximize actions.
 * Extracted from CanvasWidget for reuse in both active dashboard and run recap.
 */

import { useState, useCallback } from "react";
import { ChevronRight, Copy, Check, Maximize2 } from "lucide-react";
import { cn } from "../../../../../lib/utils";
import { Badge } from "../../../../ui";
import { getAccentColors } from "@/design-system";
import { CanvasComponentRegistry } from "./CanvasComponentRegistry";
import type { CanvasPanel } from "../types";

interface PanelCardProps {
  panel: CanvasPanel;
  isExpanded: boolean;
  onToggle: () => void;
  onMaximize?: () => void;
  readOnly?: boolean;
}

/**
 * Lazy-rendered panel content. Only mounts when the panel is expanded.
 */
function PanelContent({ panel, readOnly }: { panel: CanvasPanel; readOnly?: boolean }) {
  const Component = CanvasComponentRegistry.get(panel.component);
  return (
    <Component data={panel.data} size={panel.size} panelId={panel.panel_id} readOnly={readOnly} />
  );
}

/**
 * Serialize panel data to a copyable text format based on component type.
 */
function serializePanelData(panel: CanvasPanel): string {
  const { component, data } = panel;

  switch (component) {
    case "Markdown":
      return (data.content as string) ?? "";

    case "Table": {
      const columns = (data.columns as string[]) ?? [];
      const rows = (data.rows as (string | number | boolean | null)[][]) ?? [];
      const header = columns.join("\t");
      const body = rows.map((row) => row.map((c) => String(c ?? "")).join("\t")).join("\n");
      return header + "\n" + body;
    }

    case "CodeDiff":
      return (data.unified_diff as string) ?? "";

    case "Terminal": {
      const lines = (data.lines as string[]) ?? [];
      return lines.join("\n");
    }

    case "KeyValue": {
      const pairs = (data.pairs as Array<{ key: string; value: string | number | boolean }>) ?? [];
      return pairs.map((p) => `${p.key}: ${p.value}`).join("\n");
    }

    case "Alert": {
      const severity = (data.severity as string) ?? "info";
      const message = (data.message as string) ?? "";
      return `[${severity}] ${message}`;
    }

    case "Checklist": {
      const items = (data.items as Array<{ label: string; checked: boolean }>) ?? [];
      return items.map((i) => `${i.checked ? "[x]" : "[ ]"} ${i.label}`).join("\n");
    }

    case "FindingList": {
      const findings = (data.findings as Array<{ severity?: string; title: string }>) ?? [];
      return findings.map((f) => `[${f.severity ?? "info"}] ${f.title}`).join("\n");
    }

    case "Timeline": {
      const events = (data.events as Array<{ status?: string; title: string }>) ?? [];
      return events.map((e) => `${e.status ?? "pending"}: ${e.title}`).join("\n");
    }

    case "FileTree": {
      const entries = (data.entries as Array<{ status?: string; path: string }>) ?? [];
      return entries.map((e) => `${e.status ?? ""} ${e.path}`.trim()).join("\n");
    }

    case "ProgressChart": {
      const segments = (data.segments as Array<{ label: string; value: number }>) ?? [];
      return segments.map((s) => `${s.label}: ${s.value}`).join("\n");
    }

    case "SummaryStats": {
      const total = (data.total as number) ?? 0;
      const passed = (data.passed as number) ?? 0;
      const failed = (data.failed as number) ?? 0;
      const skipped = (data.skipped as number) ?? 0;
      return `Passed: ${passed}, Failed: ${failed}, Skipped: ${skipped}, Total: ${total}`;
    }

    case "StateTimeline": {
      const steps =
        (data.steps as Array<{ name: string; iterations: Array<{ status: string }> }>) ?? [];
      return steps
        .map((s) => `${s.name}: ${s.iterations.map((i) => i.status).join(", ")}`)
        .join("\n");
    }

    case "Waterfall": {
      const entries =
        (data.entries as Array<{ name: string; duration_ms: number; status?: string }>) ?? [];
      return entries
        .map((e) => `${e.name}: ${e.duration_ms}ms (${e.status ?? "unknown"})`)
        .join("\n");
    }

    case "Sparkline": {
      const series =
        (data.series as Array<{ name: string; values: Array<{ outcome: string }> }>) ?? [];
      return series.map((s) => `${s.name}: ${s.values.map((v) => v.outcome).join(",")}`).join("\n");
    }

    case "WaffleChart": {
      const cells = (data.cells as Array<{ label: string; status: string }>) ?? [];
      const counts: Record<string, number> = {};
      cells.forEach((c) => {
        counts[c.status] = (counts[c.status] ?? 0) + 1;
      });
      return Object.entries(counts)
        .map(([k, v]) => `${k}: ${v}`)
        .join(", ");
    }

    case "PhaseTimeline": {
      const phases =
        (data.phases as Array<{ name: string; duration_ms: number; status: string }>) ?? [];
      return phases.map((p) => `${p.name}: ${p.duration_ms}ms (${p.status})`).join("\n");
    }

    case "IterationComparison": {
      const iterations =
        (data.iterations as Array<{ iteration: number; passed: number; failed: number }>) ?? [];
      return iterations
        .map((i) => `Iter ${i.iteration}: ${i.passed} passed, ${i.failed} failed`)
        .join("\n");
    }

    case "StepDurationChart": {
      const steps =
        (data.steps as Array<{ name: string; duration_ms: number; status: string }>) ?? [];
      return steps.map((s) => `${s.name}: ${s.duration_ms}ms (${s.status})`).join("\n");
    }

    case "PhaseDistribution": {
      const segments =
        (data.segments as Array<{ phase: string; duration_ms: number; percentage: number }>) ?? [];
      return segments
        .map((s) => `${s.phase}: ${s.percentage.toFixed(1)}% (${s.duration_ms}ms)`)
        .join("\n");
    }

    case "MissionBrief": {
      const name = (data.workflow_name as string) ?? "";
      const prompt = (data.prompt as string) ?? "";
      const progress = data.progress as
        | { phase?: string; activity?: string; current_iteration?: number }
        | undefined;
      const lines = [name];
      if (prompt) lines.push(`\nPrompt:\n${prompt}`);
      if (progress?.phase) lines.push(`Phase: ${progress.phase}`);
      if (progress?.current_iteration) lines.push(`Iteration: ${progress.current_iteration}`);
      if (progress?.activity) lines.push(`Activity: ${progress.activity}`);
      return lines.join("\n");
    }

    default:
      return JSON.stringify(data, null, 2);
  }
}

/**
 * Copy button with copied state feedback.
 */
function CopyPanelButton({ panel }: { panel: CanvasPanel }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      const text = serializePanelData(panel);
      navigator.clipboard.writeText(text).then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
      });
    },
    [panel],
  );

  const Icon = copied ? Check : Copy;

  return (
    <button
      type="button"
      className={cn(
        "p-1 rounded hover:bg-muted/50 transition-colors",
        copied ? "text-green-500" : "text-muted-foreground hover:text-foreground",
      )}
      onClick={handleCopy}
      title="Copy panel content"
    >
      <Icon className="h-3.5 w-3.5" />
    </button>
  );
}

/**
 * Individual panel card with expand/collapse, copy, and maximize.
 */
export function PanelCard({ panel, isExpanded, onToggle, onMaximize, readOnly }: PanelCardProps) {
  const roseColors = getAccentColors("rose");

  return (
    <div className="border-b border-border/50 last:border-b-0 animate-panelEnter">
      {/* Panel header */}
      <button
        type="button"
        className={cn(
          "w-full flex items-center gap-3 px-4 py-2.5 cursor-pointer transition-colors text-left",
          isExpanded
            ? "bg-muted/50 border-l-2 border-primary"
            : "hover:bg-muted/30 border-l-2 border-transparent",
        )}
        onClick={onToggle}
      >
        <div className="flex-shrink-0 text-muted-foreground">
          <ChevronRight className={cn("h-4 w-4 transition-transform", isExpanded && "rotate-90")} />
        </div>

        <Badge
          className={cn(
            "text-[10px] px-1.5 py-0 font-mono border",
            roseColors.bg,
            roseColors.text,
            roseColors.border,
          )}
        >
          {panel.component}
        </Badge>

        <span className="text-sm font-medium text-foreground flex-1 truncate">{panel.title}</span>

        {panel.size && panel.size !== "normal" && (
          <Badge variant="muted" className="text-[10px]">
            {panel.size}
          </Badge>
        )}

        {/* Action buttons */}
        <div className="flex items-center gap-0.5" onClick={(e) => e.stopPropagation()}>
          <CopyPanelButton panel={panel} />
          {onMaximize && (
            <button
              type="button"
              className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-muted/50 transition-colors"
              onClick={(e) => {
                e.stopPropagation();
                onMaximize();
              }}
              title="Maximize panel"
            >
              <Maximize2 className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      </button>

      {/* Panel content — lazy rendered */}
      {isExpanded && (
        <div
          className={cn(
            "border-l-2 border-primary bg-muted/20 px-4 py-3 animate-slideDown transition-all",
            panel.size === "compact"
              ? "max-h-[200px]"
              : panel.size === "large"
                ? "max-h-[600px]"
                : "max-h-[400px]",
            "overflow-y-auto",
          )}
        >
          <PanelContent panel={panel} readOnly={readOnly} />
        </div>
      )}
    </div>
  );
}

export { PanelContent };
