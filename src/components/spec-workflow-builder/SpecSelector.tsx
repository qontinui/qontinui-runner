/**
 * SpecSelector
 *
 * Checklist for selecting which spec groups to include
 * as verification steps in the workflow.
 */

import { useMemo } from "react";
import { Check, TestTube2, AlertTriangle, Info } from "lucide-react";
import type { SpecGroup, SpecSeverity } from "./types";

interface SpecSelectorProps {
  specs: SpecGroup[];
  selectedIds: Set<string>;
  onToggle: (specId: string) => void;
  onSelectAll: () => void;
  onDeselectAll: () => void;
}

const SEVERITY_ICON: Record<SpecSeverity, React.ReactNode> = {
  critical: <AlertTriangle className="w-3 h-3 text-red-400" />,
  warning: <AlertTriangle className="w-3 h-3 text-yellow-400" />,
  info: <Info className="w-3 h-3 text-blue-400" />,
};

export function SpecSelector({
  specs,
  selectedIds,
  onToggle,
  onSelectAll,
  onDeselectAll,
}: SpecSelectorProps) {
  const enabledSpecs = useMemo(
    () => specs.filter((s) => s.assertions.some((a) => a.enabled)),
    [specs],
  );

  const maxSeverity = (spec: SpecGroup): SpecSeverity => {
    if (spec.assertions.some((a) => a.enabled && a.severity === "critical")) return "critical";
    if (spec.assertions.some((a) => a.enabled && a.severity === "warning")) return "warning";
    return "info";
  };

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-4 py-3 border-b border-border bg-muted/50">
        <TestTube2 className="w-4 h-4 text-emerald-400" />
        <span className="text-sm font-medium text-foreground">Select Verification Steps</span>
        <span className="text-xs text-muted-foreground ml-auto">
          {selectedIds.size}/{enabledSpecs.length} selected
        </span>
        <button
          onClick={onSelectAll}
          className="text-xs px-2 py-1 bg-muted/80 text-foreground rounded hover:bg-muted"
        >
          All
        </button>
        <button
          onClick={onDeselectAll}
          className="text-xs px-2 py-1 bg-muted/80 text-foreground rounded hover:bg-muted"
        >
          None
        </button>
      </div>

      {/* Spec list */}
      <div className="flex-1 overflow-auto p-2 space-y-1">
        {enabledSpecs.map((spec) => {
          const isSelected = selectedIds.has(spec.id);
          const severity = maxSeverity(spec);
          const enabledAssertions = spec.assertions.filter((a) => a.enabled).length;

          return (
            <button
              key={spec.id}
              onClick={() => onToggle(spec.id)}
              className={`w-full flex items-center gap-3 px-3 py-2.5 rounded-lg text-left transition-colors ${
                isSelected
                  ? "bg-emerald-500/10 border border-emerald-500/30"
                  : "bg-muted/30 border border-transparent hover:bg-muted/50"
              }`}
            >
              {/* Checkbox */}
              <div
                className={`flex-shrink-0 w-5 h-5 rounded border flex items-center justify-center ${
                  isSelected
                    ? "border-emerald-500 bg-emerald-500/20 text-emerald-400"
                    : "border-border bg-muted"
                }`}
              >
                {isSelected && <Check className="w-3 h-3" />}
              </div>

              {/* Severity icon */}
              {SEVERITY_ICON[severity]}

              {/* Spec info */}
              <div className="flex-1 min-w-0">
                <div className="text-sm text-foreground truncate">{spec.name}</div>
                <div className="text-xs text-muted-foreground">
                  {enabledAssertions} assertion{enabledAssertions !== 1 ? "s" : ""} &middot;{" "}
                  {spec.category}
                </div>
              </div>
            </button>
          );
        })}

        {enabledSpecs.length === 0 && (
          <div className="text-center py-8 text-muted-foreground text-sm">
            No enabled specifications found in the loaded file.
          </div>
        )}
      </div>
    </div>
  );
}
