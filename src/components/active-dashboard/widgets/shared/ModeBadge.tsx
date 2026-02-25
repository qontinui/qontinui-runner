/**
 * ModeBadge Component
 *
 * Standardized filled badge for displaying mode/category labels.
 * Uses the design system's accent colors for consistent coloring.
 */

import { cn } from "../../../../lib/utils";
import { getAccentColors, type AccentColor } from "../../../../design-system/colors";

interface ModeBadgeProps {
  label: string;
  color: AccentColor;
  count?: number;
  className?: string;
}

export function ModeBadge({ label, color, count, className }: ModeBadgeProps) {
  const colors = getAccentColors(color);
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-medium",
        colors.bg,
        colors.text,
        className,
      )}
    >
      {label}
      {count !== undefined && <span className="opacity-70">{count}</span>}
    </span>
  );
}
