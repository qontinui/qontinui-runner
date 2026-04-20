/**
 * ResourceLimitsSection.tsx
 *
 * Collapsible section for editing resource limits (wall time, files modified,
 * agentic time, warning threshold).
 */

import { useState } from "react";
import { ChevronDown, ChevronRight, Gauge } from "lucide-react";
import type { ResourceLimits } from "@qontinui/shared-types/constraints";
import { DEFAULT_WARNING_THRESHOLD } from "@qontinui/workflow-utils";

interface ResourceLimitsSectionProps {
  resourceLimits: ResourceLimits;
  onUpdate: (limits: Partial<ResourceLimits>) => void;
}

const inputClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 text-sm";

export function ResourceLimitsSection({ resourceLimits, onUpdate }: ResourceLimitsSectionProps) {
  const [expanded, setExpanded] = useState(false);

  const hasAnyLimit =
    resourceLimits.maxWallTimeSecs != null ||
    resourceLimits.maxFilesModified != null ||
    resourceLimits.maxAgenticTimeMs != null ||
    resourceLimits.warningThreshold != null;

  return (
    <div>
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between px-3 py-2 hover:bg-zinc-800/50 rounded-md transition-colors"
      >
        <div className="flex items-center gap-2">
          <Gauge className="w-4 h-4 text-zinc-400" />
          <span className="text-sm font-medium text-zinc-300">Resource Limits</span>
          {hasAnyLimit && (
            <span className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-blue-500/10 text-blue-400">
              configured
            </span>
          )}
        </div>
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-zinc-500" />
        ) : (
          <ChevronRight className="w-4 h-4 text-zinc-500" />
        )}
      </button>

      {expanded && (
        <div className="mt-2 space-y-3 px-3 animate-slideDown">
          {/* Max Wall Time */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">
              Max Wall Time (seconds)
            </label>
            <input
              type="number"
              value={resourceLimits.maxWallTimeSecs ?? ""}
              onChange={(e) => {
                const val = e.target.value ? parseInt(e.target.value, 10) : undefined;
                onUpdate({ maxWallTimeSecs: val });
              }}
              placeholder="No limit"
              min={0}
              className={inputClass}
            />
            <p className="text-[10px] text-zinc-500 mt-0.5">
              Maximum wall-clock time for the entire workflow
            </p>
          </div>

          {/* Max Files Modified */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">
              Max Files Modified
            </label>
            <input
              type="number"
              value={resourceLimits.maxFilesModified ?? ""}
              onChange={(e) => {
                const val = e.target.value ? parseInt(e.target.value, 10) : undefined;
                onUpdate({ maxFilesModified: val });
              }}
              placeholder="No limit"
              min={0}
              className={inputClass}
            />
            <p className="text-[10px] text-zinc-500 mt-0.5">
              Maximum unique files modified across all iterations
            </p>
          </div>

          {/* Max Agentic Time */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">
              Max Agentic Time (ms)
            </label>
            <input
              type="number"
              value={resourceLimits.maxAgenticTimeMs ?? ""}
              onChange={(e) => {
                const val = e.target.value ? parseInt(e.target.value, 10) : undefined;
                onUpdate({ maxAgenticTimeMs: val });
              }}
              placeholder="No limit"
              min={0}
              className={inputClass}
            />
            <p className="text-[10px] text-zinc-500 mt-0.5">
              Maximum total agentic phase duration summed
            </p>
          </div>

          {/* Warning Threshold */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">
              Warning Threshold
            </label>
            <input
              type="number"
              value={resourceLimits.warningThreshold ?? ""}
              onChange={(e) => {
                const val = e.target.value ? parseFloat(e.target.value) : undefined;
                onUpdate({ warningThreshold: val });
              }}
              placeholder={String(DEFAULT_WARNING_THRESHOLD)}
              min={0}
              max={1}
              step={0.05}
              className={inputClass}
            />
            <p className="text-[10px] text-zinc-500 mt-0.5">
              Fraction (0-1) at which to inject resource warnings. Default:{" "}
              {DEFAULT_WARNING_THRESHOLD}
            </p>
          </div>
        </div>
      )}
    </div>
  );
}
