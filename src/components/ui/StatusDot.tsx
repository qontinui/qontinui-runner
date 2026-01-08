import { cn } from "../../lib/utils";
import {
  getSeverityColors,
  getStatusColors,
  type SeverityLevel,
  type StatusType,
} from "../../design-system";

export interface StatusDotProps {
  /** Use severity for error levels (critical, high, medium, low, info) */
  severity?: SeverityLevel | string;
  /** Use status for execution states (idle, running, success, error, etc.) */
  status?: StatusType | string;
  /** Size of the dot */
  size?: "sm" | "md" | "lg";
  /** Whether to show pulse animation */
  pulse?: boolean;
  className?: string;
}

function StatusDot({ severity, status, size = "md", pulse, className }: StatusDotProps) {
  // Determine color based on severity or status
  let dotColor = "bg-muted-foreground";

  if (severity) {
    dotColor = getSeverityColors(severity).dot;
  } else if (status) {
    const statusColors = getStatusColors(status);
    // Use icon color as it's more vibrant than bg
    dotColor = statusColors.icon.replace("text-", "bg-");
  }

  return (
    <span
      className={cn(
        "inline-block rounded-full",
        {
          "w-1.5 h-1.5": size === "sm",
          "w-2 h-2": size === "md",
          "w-2.5 h-2.5": size === "lg",
        },
        dotColor,
        pulse && "animate-pulse",
        className,
      )}
    />
  );
}

export { StatusDot };
