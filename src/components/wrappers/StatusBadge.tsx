/**
 * StatusBadge — wrapper runtime status pill.
 *
 * Visual mapping:
 *   - running    → emerald / green ("alive, accepting dispatches")
 *   - idle       → muted ("installed but no subprocess yet")
 *   - error      → red
 *   - degraded   → amber ("alive but health-check is failing")
 *   - unknown    → muted, italic
 */

import { CircleDot, Circle, AlertTriangle, AlertCircle, HelpCircle } from "lucide-react";
import type { WrapperStatus } from "@/lib/wrappers/types";

export interface StatusBadgeProps {
  status?: WrapperStatus;
  /** Optional label override; defaults to capitalized status. */
  label?: string;
  /** Size variant. `sm` is the default for table rows; `md` for headers. */
  size?: "sm" | "md";
}

const STYLES: Record<
  WrapperStatus,
  { bg: string; text: string; border: string; Icon: typeof Circle; label: string }
> = {
  running: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-400",
    border: "border-emerald-500/40",
    Icon: CircleDot,
    label: "Running",
  },
  idle: {
    bg: "bg-muted/30",
    text: "text-muted-foreground",
    border: "border-border",
    Icon: Circle,
    label: "Idle",
  },
  error: {
    bg: "bg-red-500/10",
    text: "text-red-400",
    border: "border-red-500/40",
    Icon: AlertCircle,
    label: "Error",
  },
  degraded: {
    bg: "bg-amber-500/10",
    text: "text-amber-400",
    border: "border-amber-500/40",
    Icon: AlertTriangle,
    label: "Degraded",
  },
  unknown: {
    bg: "bg-muted/20",
    text: "text-muted-foreground/70",
    border: "border-border",
    Icon: HelpCircle,
    label: "Unknown",
  },
};

export function StatusBadge({ status = "unknown", label, size = "sm" }: StatusBadgeProps) {
  const style = STYLES[status] ?? STYLES.unknown;
  const sizing = size === "md" ? "px-2.5 py-1 text-xs" : "px-2 py-0.5 text-[11px]";
  const Icon = style.Icon;
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full border ${sizing} font-medium tracking-wide ${style.bg} ${style.text} ${style.border}`}
    >
      <Icon className="w-3 h-3" />
      {label ?? style.label}
    </span>
  );
}
