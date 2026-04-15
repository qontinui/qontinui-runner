import { useEffect, useState } from "react";
import {
  Loader2,
  CheckCircle2,
  XCircle,
  X,
  Sparkles,
  ChevronUp,
  ChevronDown,
  RotateCw,
} from "lucide-react";
import { usePromptExecutionContext } from "./PromptExecutionContext";

const DONE_AUTO_DISMISS_MS = 4000;

export function PromptAutomationOverlay() {
  const { phase, plan, progress, error, lastPrompt, retry, reset } = usePromptExecutionContext();
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (phase !== "done") return;
    const t = setTimeout(reset, DONE_AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [phase, reset]);

  if (phase === "idle") return null;

  const currentStep = progress.currentStep;
  const stepLabel = currentStep?.explanation ?? "";
  const counter =
    progress.total > 0 && progress.currentIndex >= 0
      ? `${Math.min(progress.currentIndex + 1, progress.total)}/${progress.total}`
      : null;

  const isWorking = phase === "planning" || phase === "executing";
  const isDone = phase === "done";
  const isError = phase === "error";
  const canExpand = Boolean(plan && plan.steps.length > 0);

  return (
    <div
      role="status"
      aria-live="polite"
      className={`
        fixed bottom-6 right-6 z-toast
        flex flex-col
        rounded-2xl border bg-card shadow-lg
        max-w-md min-w-[320px]
        transition-all duration-200
        ${isWorking ? "border-primary/40 shadow-primary/20" : ""}
        ${isDone ? "border-green-500/40" : ""}
        ${isError ? "border-destructive/40" : ""}
      `}
    >
      {/* Header row */}
      <div className="flex items-center gap-3 px-4 py-3">
        {/* Animated leading icon */}
        <div className="shrink-0 relative w-5 h-5">
          {isWorking && (
            <>
              <Loader2 className="w-5 h-5 text-primary animate-spin" />
              <span className="absolute inset-0 rounded-full bg-primary/20 animate-ping" />
            </>
          )}
          {isDone && <CheckCircle2 className="w-5 h-5 text-green-500" />}
          {isError && <XCircle className="w-5 h-5 text-destructive" />}
        </div>

        {/* Text */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Sparkles className="w-3 h-3" />
            <span>qontinui</span>
            {counter && isWorking && <span className="font-mono">· {counter}</span>}
          </div>
          <div className="text-sm text-foreground truncate">
            {phase === "planning" && "Understanding your request..."}
            {phase === "executing" && (stepLabel || plan?.summary || "Automating...")}
            {isDone && "Done"}
            {isError && (error || "Something went wrong")}
          </div>
        </div>

        {/* Actions */}
        <div className="flex items-center gap-1 shrink-0">
          {canExpand && (
            <button
              onClick={() => setExpanded((v) => !v)}
              className="p-1 rounded-md hover:bg-accent text-muted-foreground"
              aria-label={expanded ? "Collapse" : "Expand"}
              aria-expanded={expanded}
            >
              {expanded ? <ChevronDown className="w-4 h-4" /> : <ChevronUp className="w-4 h-4" />}
            </button>
          )}
          {isError && lastPrompt && (
            <button
              onClick={retry}
              className="p-1 rounded-md hover:bg-accent text-primary"
              aria-label="Retry"
              title="Retry"
            >
              <RotateCw className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={reset}
            className="p-1 rounded-md hover:bg-accent text-muted-foreground"
            aria-label={isWorking ? "Cancel" : "Dismiss"}
            title={isWorking ? "Cancel" : "Dismiss"}
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Progress bar (working only) */}
      {isWorking && progress.total > 0 && (
        <div className="mx-4 mb-2 h-0.5 bg-border rounded-full overflow-hidden">
          <div
            className="h-full bg-primary transition-all duration-300"
            style={{
              width: `${Math.max(0, (progress.currentIndex / progress.total) * 100)}%`,
            }}
          />
        </div>
      )}

      {/* Expanded step list */}
      {expanded && plan && (
        <div className="border-t border-border px-3 py-2 max-h-64 overflow-y-auto">
          {plan.summary && (
            <p className="px-2 py-1 text-xs text-muted-foreground">{plan.summary}</p>
          )}
          <ul className="space-y-0.5">
            {plan.steps.map((step, i) => {
              const isCurrent = i === progress.currentIndex && isWorking;
              const isStepDone = i < progress.currentIndex || isDone;
              return (
                <li
                  key={i}
                  className={`flex items-start gap-2 px-2 py-1.5 rounded-md text-xs ${
                    isCurrent
                      ? "bg-primary/10 text-foreground"
                      : isStepDone
                        ? "text-muted-foreground"
                        : "text-muted-foreground/50"
                  }`}
                >
                  <span className="shrink-0 mt-0.5">
                    {isStepDone ? (
                      <CheckCircle2 className="w-3.5 h-3.5 text-green-500" />
                    ) : isCurrent ? (
                      <Loader2 className="w-3.5 h-3.5 text-primary animate-spin" />
                    ) : (
                      <span className="w-3.5 h-3.5 block rounded-full border border-current" />
                    )}
                  </span>
                  <span className="leading-snug">{step.explanation}</span>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}
