/**
 * StepIndicator — the hook-generation panel's progress list (one row per step).
 */

import { AlertTriangle, CheckCircle2, Circle, Loader2 } from "lucide-react";
import type { StepStatus } from "./steps/types";

export function StepIndicator({ steps }: { steps: StepStatus[] }) {
  return (
    <div className="flex flex-col gap-1 mb-3">
      {steps.map((step, i) => (
        <div key={`${step.label}-${i}`} className="flex items-center gap-2 text-xs">
          {step.state === "done" ? (
            <CheckCircle2 className="w-3.5 h-3.5 text-green-400 shrink-0" />
          ) : step.state === "active" ? (
            <Loader2 className="w-3.5 h-3.5 text-purple-400 animate-spin shrink-0" />
          ) : step.state === "error" ? (
            <AlertTriangle className="w-3.5 h-3.5 text-red-400 shrink-0" />
          ) : step.state === "skipped" ? (
            <Circle className="w-3.5 h-3.5 text-muted-foreground/30 shrink-0" />
          ) : (
            <Circle className="w-3.5 h-3.5 text-muted-foreground/40 shrink-0" />
          )}
          <span
            className={
              step.state === "active"
                ? "text-purple-400 font-medium"
                : step.state === "done"
                  ? "text-green-400"
                  : step.state === "error"
                    ? "text-red-400"
                    : "text-muted-foreground/60"
            }
          >
            {step.label}
          </span>
        </div>
      ))}
    </div>
  );
}
