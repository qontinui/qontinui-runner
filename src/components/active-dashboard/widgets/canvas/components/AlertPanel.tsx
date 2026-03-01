/**
 * AlertPanel Component
 *
 * Renders a colored alert banner (info/success/warning/error).
 */

import { AlertCircle, CheckCircle, AlertTriangle, Info } from "lucide-react";
import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

type Severity = "info" | "success" | "warning" | "error";

const SEVERITY_CONFIG: Record<Severity, { icon: typeof Info; classes: string }> = {
  info: {
    icon: Info,
    classes: "bg-blue-500/10 border-blue-500/30 text-blue-400",
  },
  success: {
    icon: CheckCircle,
    classes: "bg-green-500/10 border-green-500/30 text-green-400",
  },
  warning: {
    icon: AlertTriangle,
    classes: "bg-yellow-500/10 border-yellow-500/30 text-yellow-400",
  },
  error: {
    icon: AlertCircle,
    classes: "bg-red-500/10 border-red-500/30 text-red-400",
  },
};

export function AlertPanel({ data }: CanvasPanelComponentProps) {
  const severity = ((data.severity as string) ?? "info") as Severity;
  const message = (data.message as string) ?? "";
  const details = (data.details as string) ?? "";

  const config = SEVERITY_CONFIG[severity] ?? SEVERITY_CONFIG.info;
  const Icon = config.icon;

  return (
    <div className={cn("flex items-start gap-3 rounded-md border p-3", config.classes)}>
      <Icon className="h-5 w-5 flex-shrink-0 mt-0.5" />
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium">{message}</p>
        {details && <p className="mt-1 text-xs opacity-80">{details}</p>}
      </div>
    </div>
  );
}
