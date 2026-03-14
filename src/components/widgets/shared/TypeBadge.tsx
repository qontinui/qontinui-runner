/**
 * TypeBadge Component
 *
 * Standardized outline badge for displaying step/action types.
 * Uses the design system's accent colors with a border-only (outline) style,
 * distinct from ModeBadge which uses a filled background.
 */

import { cn } from "@/lib/utils";
import { getAccentColors, type AccentColor } from "@/design-system/colors";

interface TypeBadgeProps {
  label: string;
  color: AccentColor;
  icon?: React.ReactNode;
  className?: string;
}

export function TypeBadge({ label, color, icon, className }: TypeBadgeProps) {
  const colors = getAccentColors(color);
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-xs font-medium font-mono",
        "bg-transparent",
        colors.text,
        colors.border,
        className,
      )}
    >
      {icon}
      {label}
    </span>
  );
}
