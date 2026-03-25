/**
 * PatternIndicator.tsx
 *
 * Small badge component that shows if active cross-run patterns exist
 * for a given workflow. Intended for use in run history lists.
 */

import { useCrossRunPatterns } from "../../hooks/useGraphAnalytics";

interface PatternIndicatorProps {
  workflowName?: string;
}

export function PatternIndicator({ workflowName }: PatternIndicatorProps) {
  const { data: patterns } = useCrossRunPatterns(workflowName);
  const active = patterns?.filter(p => p.status === "active").length || 0;
  if (active === 0) return null;
  return (
    <span
      className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs bg-orange-500/10 text-orange-400"
      title={`${active} active pattern${active !== 1 ? "s" : ""}`}
    >
      {active}
    </span>
  );
}
