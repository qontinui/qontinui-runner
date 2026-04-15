import { useEffect } from "react";
import { Loader2, CheckCircle2, XCircle, X, Sparkles } from "lucide-react";
import { usePromptExecutionContext } from "./PromptExecutionContext";

const DONE_AUTO_DISMISS_MS = 4000;

export function PromptAutomationOverlay() {
  const { phase, plan, progress, error, reset } = usePromptExecutionContext();

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

  return (
    <div
      role="status"
      aria-live="polite"
      className={`
        fixed bottom-6 right-6 z-toast
        flex items-center gap-3 px-4 py-3
        rounded-full border bg-card shadow-lg
        max-w-md min-w-[280px]
        transition-all duration-200
        ${isWorking ? "border-primary/40 shadow-primary/20" : ""}
        ${isDone ? "border-green-500/40" : ""}
        ${isError ? "border-destructive/40" : ""}
      `}
    >
      {/* Animated leading icon */}
      <div className="shrink-0 relative">
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

      {/* Progress bar */}
      {isWorking && progress.total > 0 && (
        <div className="absolute left-4 right-4 bottom-1 h-0.5 bg-border rounded-full overflow-hidden">
          <div
            className="h-full bg-primary transition-all duration-300"
            style={{
              width: `${Math.max(0, (progress.currentIndex / progress.total) * 100)}%`,
            }}
          />
        </div>
      )}

      {/* Dismiss */}
      {(isDone || isError) && (
        <button
          onClick={reset}
          className="shrink-0 p-1 rounded-md hover:bg-accent text-muted-foreground"
          aria-label="Dismiss"
        >
          <X className="w-4 h-4" />
        </button>
      )}
    </div>
  );
}
