/**
 * StatusBadge — wrapper runtime status pill.
 *
 * Visual mapping:
 *   - running    → emerald / green ("alive, accepting dispatches")
 *   - stopped    → muted ("installed but no subprocess")
 *   - degraded   → amber ("process alive, health-check failing — NOT routable")
 *   - unknown    → muted, italic (the status read failed)
 *
 * Labels come from `wrapperStatusLabel` so the degraded pill states that
 * dispatch will not reach the wrapper.
 */

import { CircleDot, Circle, AlertTriangle, HelpCircle } from "lucide-react";
import type { WrapperStatus } from "@/lib/wrappers/types";
import { wrapperStatusLabel } from "@/lib/wrappers/status";

export interface StatusBadgeProps {
  status?: WrapperStatus;
  /** Optional label override; defaults to capitalized status. */
  label?: string;
  /** Size variant. `sm` is the default for table rows; `md` for headers. */
  size?: "sm" | "md";
}

const STYLES: Record<
  WrapperStatus,
  { bg: string; text: string; border: string; Icon: typeof Circle }
> = {
  running: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-400",
    border: "border-emerald-500/40",
    Icon: CircleDot,
  },
  stopped: {
    bg: "bg-muted/30",
    text: "text-muted-foreground",
    border: "border-border",
    Icon: Circle,
  },
  degraded: {
    bg: "bg-amber-500/10",
    text: "text-amber-400",
    border: "border-amber-500/40",
    Icon: AlertTriangle,
  },
  unknown: {
    bg: "bg-muted/20",
    text: "text-muted-foreground/70",
    border: "border-border",
    Icon: HelpCircle,
  },
};

export function StatusBadge({ status = "unknown", label, size = "sm" }: StatusBadgeProps) {
  const known: WrapperStatus = Object.hasOwn(STYLES, status) ? status : "unknown";
  const style = STYLES[known];
  const sizing = size === "md" ? "px-2.5 py-1 text-xs" : "px-2 py-0.5 text-[11px]";
  const Icon = style.Icon;
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full border ${sizing} font-medium tracking-wide ${style.bg} ${style.text} ${style.border}`}
    >
      <Icon className="w-3 h-3" />
      {label ?? wrapperStatusLabel(known)}
    </span>
  );
}
