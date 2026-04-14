/**
 * BreakpointInspector Component
 *
 * Collapsible panel that displays breakpoint snapshot details including
 * variable tree, screenshot thumbnail, pending steps, and freshness status.
 */

import { useMemo } from "react";
import { X, Clock, Eye, ChevronRight, ChevronDown, AlertTriangle } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle, Badge, ScrollArea } from "../ui";
import { getAccentColors } from "@/design-system";
import type { BreakpointSnapshot } from "./types";

export interface BreakpointInspectorProps {
  snapshot: BreakpointSnapshot | null;
  onClose: () => void;
}

/**
 * Compute freshness status from the snapshot timestamp.
 * Returns "fresh" (<10 min), "stale" (10-30 min), or "expired" (>30 min).
 */
function getFreshnessStatus(freshnessTs: string): {
  label: string;
  color: "green" | "amber" | "red";
  ageMinutes: number;
} {
  const age = Date.now() - new Date(freshnessTs).getTime();
  const ageMinutes = Math.floor(age / 60_000);

  if (ageMinutes >= 30) {
    return { label: `${ageMinutes}m ago (expired)`, color: "red", ageMinutes };
  }
  if (ageMinutes >= 10) {
    return { label: `${ageMinutes}m ago (stale)`, color: "amber", ageMinutes };
  }
  return { label: ageMinutes <= 0 ? "just now" : `${ageMinutes}m ago`, color: "green", ageMinutes };
}

/**
 * Recursive variable tree renderer. Renders objects/arrays as nested lists.
 */
function VariableNode({
  name,
  value,
  depth = 0,
}: {
  name: string;
  value: unknown;
  depth?: number;
}) {
  const isObject = value !== null && typeof value === "object";
  const isArray = Array.isArray(value);

  if (!isObject) {
    return (
      <div className="flex items-baseline gap-2 py-0.5" style={{ paddingLeft: `${depth * 16}px` }}>
        <span className="text-xs font-mono text-muted-foreground">{name}:</span>
        <span className="text-xs font-mono text-foreground truncate">
          {value === null ? "null" : String(value)}
        </span>
      </div>
    );
  }

  const entries = isArray
    ? (value as unknown[]).map((v, i) => [String(i), v] as [string, unknown])
    : Object.entries(value as Record<string, unknown>);

  if (entries.length === 0) {
    return (
      <div className="flex items-baseline gap-2 py-0.5" style={{ paddingLeft: `${depth * 16}px` }}>
        <span className="text-xs font-mono text-muted-foreground">{name}:</span>
        <span className="text-xs font-mono text-muted-foreground/60">{isArray ? "[]" : "{}"}</span>
      </div>
    );
  }

  return (
    <details open={depth < 2} className="group">
      <summary
        className="flex items-center gap-1 py-0.5 cursor-pointer select-none hover:bg-muted/30 rounded"
        style={{ paddingLeft: `${depth * 16}px` }}
      >
        <ChevronRight className="w-3 h-3 text-muted-foreground group-open:hidden" />
        <ChevronDown className="w-3 h-3 text-muted-foreground hidden group-open:block" />
        <span className="text-xs font-mono text-muted-foreground">{name}</span>
        <span className="text-[10px] text-muted-foreground/60">
          {isArray ? `[${entries.length}]` : `{${entries.length}}`}
        </span>
      </summary>
      <div>
        {entries.map(([key, val]) => (
          <VariableNode key={key} name={key} value={val} depth={depth + 1} />
        ))}
      </div>
    </details>
  );
}

export function BreakpointInspector({ snapshot, onClose }: BreakpointInspectorProps) {
  const variables = useMemo(() => {
    if (!snapshot) return null;
    try {
      return JSON.parse(snapshot.variables_json) as Record<string, unknown>;
    } catch {
      return null;
    }
  }, [snapshot]);

  const pendingSteps = useMemo(() => {
    if (!snapshot) return [];
    try {
      return JSON.parse(snapshot.pending_steps_json) as Array<{
        name?: string;
        step_name?: string;
        index?: number;
      }>;
    } catch {
      return [];
    }
  }, [snapshot]);

  if (!snapshot) return null;

  const freshness = getFreshnessStatus(snapshot.freshness_ts);
  const freshnessColors = getAccentColors(freshness.color);
  const amberColors = getAccentColors("amber");

  return (
    <div className="fixed inset-y-0 right-0 w-96 z-40 bg-card border-l border-border shadow-xl flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border">
        <div className="flex items-center gap-2">
          <Eye className="w-4 h-4 text-amber-500" />
          <span className="text-sm font-semibold text-foreground">Breakpoint Inspector</span>
        </div>
        <button
          onClick={onClose}
          className="p-1 rounded hover:bg-muted transition-colors"
          aria-label="Close inspector"
        >
          <X className="w-4 h-4 text-muted-foreground" />
        </button>
      </div>

      {/* Breakpoint info bar */}
      <div className="px-4 py-2 border-b border-border bg-muted/30 flex items-center gap-2 flex-wrap">
        <Badge className={`text-xs ${amberColors.bg} ${amberColors.text} ${amberColors.border}`}>
          Step {snapshot.step_index}
        </Badge>
        {snapshot.step_name && (
          <span className="text-xs text-foreground font-mono truncate">{snapshot.step_name}</span>
        )}
        {snapshot.phase && (
          <Badge variant="muted" className="text-[10px]">
            {snapshot.phase}
          </Badge>
        )}
        {snapshot.iteration != null && (
          <span className="text-[10px] text-muted-foreground">iter {snapshot.iteration}</span>
        )}
      </div>

      {/* Freshness indicator */}
      <div className="px-4 py-2 border-b border-border flex items-center gap-2">
        <Clock className={`w-3.5 h-3.5 ${freshnessColors.text}`} />
        <span className={`text-xs ${freshnessColors.text}`}>{freshness.label}</span>
        {freshness.ageMinutes >= 10 && (
          <AlertTriangle className={`w-3.5 h-3.5 ${freshnessColors.text}`} />
        )}
        <Badge
          className={`text-[10px] ml-auto ${
            snapshot.status === "waiting"
              ? `${amberColors.bg} ${amberColors.text}`
              : "bg-muted text-muted-foreground"
          }`}
        >
          {snapshot.status}
        </Badge>
      </div>

      {/* Scrollable content */}
      <ScrollArea className="flex-1">
        <div className="flex flex-col gap-3 p-4">
          {/* Variables tree */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-2">
              <CardTitle className="text-xs font-semibold">Variables</CardTitle>
            </CardHeader>
            <CardContent>
              {variables && Object.keys(variables).length > 0 ? (
                <div className="max-h-64 overflow-auto">
                  {Object.entries(variables).map(([key, val]) => (
                    <VariableNode key={key} name={key} value={val} />
                  ))}
                </div>
              ) : (
                <p className="text-xs text-muted-foreground">No variables captured</p>
              )}
            </CardContent>
          </Card>

          {/* Screenshot thumbnail */}
          {snapshot.last_screenshot_ref && (
            <Card className="border-border bg-card/50">
              <CardHeader className="pb-2">
                <CardTitle className="text-xs font-semibold">Last Screenshot</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="aspect-video bg-muted/50 rounded-md flex items-center justify-center overflow-hidden">
                  <img
                    src={snapshot.last_screenshot_ref}
                    alt="Breakpoint screenshot"
                    className="w-full h-full object-contain"
                    onError={(e) => {
                      (e.target as HTMLImageElement).style.display = "none";
                    }}
                  />
                </div>
              </CardContent>
            </Card>
          )}

          {/* Pending steps */}
          <Card className="border-border bg-card/50">
            <CardHeader className="pb-2">
              <div className="flex items-center justify-between">
                <CardTitle className="text-xs font-semibold">Pending Steps</CardTitle>
                <Badge variant="muted" className="text-[10px]">
                  {pendingSteps.length}
                </Badge>
              </div>
            </CardHeader>
            <CardContent>
              {pendingSteps.length > 0 ? (
                <ul className="space-y-1">
                  {pendingSteps.map((step, i) => (
                    <li
                      key={step.index ?? i}
                      className="flex items-center gap-2 text-xs py-1 px-2 rounded bg-muted/30"
                    >
                      <span className="font-mono text-muted-foreground w-5 text-right shrink-0">
                        {step.index ?? i}
                      </span>
                      <span className="text-foreground truncate">
                        {step.name ?? step.step_name ?? "unnamed"}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-xs text-muted-foreground">No pending steps</p>
              )}
            </CardContent>
          </Card>
        </div>
      </ScrollArea>
    </div>
  );
}
