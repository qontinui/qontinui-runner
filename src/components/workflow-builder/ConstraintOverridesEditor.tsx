/**
 * ConstraintOverridesEditor.tsx
 *
 * Per-workflow constraint override editor for the runner workflow builder.
 * Fetches available constraints from the runner API and displays a 3-state
 * toggle for each: Default (use project config), Force Enable, Force Disable.
 */

import { useState, useEffect } from "react";
import { ShieldCheck, Loader2, AlertTriangle } from "lucide-react";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { fetchActiveConstraints } from "@/lib/constraints-api";
import type { Constraint } from "@qontinui/shared-types/constraints";

type OverrideState = "default" | "enable" | "disable";

function getOverrideState(
  overrides: Record<string, boolean> | undefined,
  constraintId: string,
): OverrideState {
  if (!overrides || !(constraintId in overrides)) return "default";
  return overrides[constraintId] ? "enable" : "disable";
}

function cycleOverrideState(current: OverrideState): OverrideState {
  switch (current) {
    case "default":
      return "enable";
    case "enable":
      return "disable";
    case "disable":
      return "default";
  }
}

const STATE_LABELS: Record<OverrideState, string> = {
  default: "Default",
  enable: "Force On",
  disable: "Force Off",
};

const STATE_COLORS: Record<OverrideState, string> = {
  default: "bg-zinc-700 text-zinc-400",
  enable: "bg-emerald-500/20 text-emerald-400 border-emerald-500/30",
  disable: "bg-red-500/20 text-red-400 border-red-500/30",
};

export function ConstraintOverridesEditor() {
  const { state, updateWorkflow } = useWorkflowBuilder();
  const { workflow } = state;
  const overrides = workflow.constraint_overrides;

  const [constraints, setConstraints] = useState<Constraint[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    fetchActiveConstraints()
      .then((result) => {
        if (!cancelled) {
          setConstraints(result);
          setLoading(false);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "Failed to load constraints");
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  const handleToggle = (constraintId: string) => {
    const current = getOverrideState(overrides, constraintId);
    const next = cycleOverrideState(current);
    const updated = { ...(overrides ?? {}) };

    if (next === "default") {
      delete updated[constraintId];
    } else {
      updated[constraintId] = next === "enable";
    }

    // Clean up: only store if there are actual overrides
    updateWorkflow({
      constraint_overrides: Object.keys(updated).length > 0 ? updated : undefined,
    });
  };

  const overrideCount = overrides ? Object.keys(overrides).length : 0;

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-xs text-zinc-500 py-2">
        <Loader2 className="w-3.5 h-3.5 animate-spin" />
        Loading constraints...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center gap-2 text-xs text-amber-400/80 py-2">
        <AlertTriangle className="w-3.5 h-3.5" />
        <span>{error}</span>
      </div>
    );
  }

  if (constraints.length === 0) {
    return (
      <div className="text-xs text-zinc-500 py-2">
        No constraints configured. Add constraints via the Constraints panel or constraints.toml.
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <ShieldCheck className="w-4 h-4 text-blue-400" />
        <span className="text-sm font-medium text-zinc-300">Constraint Overrides</span>
        {overrideCount > 0 && (
          <span className="px-1.5 py-0.5 text-[10px] font-medium bg-blue-500/20 text-blue-400 rounded">
            {overrideCount} override{overrideCount !== 1 ? "s" : ""}
          </span>
        )}
      </div>
      <p className="text-[10px] text-zinc-500">
        Override project-level constraint settings for this workflow only.
      </p>
      <div className="space-y-1">
        {constraints.map((constraint) => {
          const currentState = getOverrideState(overrides, constraint.id);
          return (
            <div key={constraint.id} className="flex items-center gap-2 py-1 group">
              <button
                type="button"
                onClick={() => handleToggle(constraint.id)}
                className={`shrink-0 px-2 py-0.5 text-[10px] font-medium rounded border transition-colors cursor-pointer ${STATE_COLORS[currentState]}`}
                title={`Click to cycle: Default -> Force On -> Force Off`}
              >
                {STATE_LABELS[currentState]}
              </button>
              <div className="flex-1 min-w-0">
                <span className="text-[11px] text-zinc-300 truncate block">{constraint.name}</span>
              </div>
              <span
                className={`shrink-0 text-[10px] ${
                  constraint.enabled ? "text-emerald-500/60" : "text-zinc-600"
                }`}
                title={
                  constraint.enabled ? "Enabled at project level" : "Disabled at project level"
                }
              >
                {constraint.enabled ? "on" : "off"}
              </span>
            </div>
          );
        })}
      </div>
      {overrideCount > 0 && (
        <button
          type="button"
          onClick={() => updateWorkflow({ constraint_overrides: undefined })}
          className="text-[10px] text-zinc-500 hover:text-red-400 transition-colors"
        >
          Clear all overrides
        </button>
      )}
    </div>
  );
}
