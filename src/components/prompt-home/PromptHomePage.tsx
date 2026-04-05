import { useState, useCallback, useRef, useEffect } from "react";
import { Search, Loader2, CheckCircle2, XCircle, Sparkles, ChevronRight } from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";
import { PromptSuggestions, savePromptToHistory } from "./PromptSuggestions";
import { usePromptExecution } from "./usePromptExecution";
import { useExplainModeTutorial } from "./useExplainModeTutorial";

const EXPLAIN_KEY = "prompt-home-explain";

export function PromptHomePage() {
  const [input, setInput] = useState("");
  const [explain, setExplain] = useState(
    () => instanceStorage.getItem(EXPLAIN_KEY) === "true",
  );
  const inputRef = useRef<HTMLInputElement>(null);
  const { phase, plan, progress, error, submit, reset } = usePromptExecution();

  useExplainModeTutorial(
    explain,
    phase,
    progress,
    plan?.steps,
    plan?.summary,
  );

  useEffect(() => {
    instanceStorage.setItem(EXPLAIN_KEY, String(explain));
  }, [explain]);

  // Focus input on mount
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleSubmit = useCallback(
    (e?: React.FormEvent) => {
      e?.preventDefault();
      const trimmed = input.trim();
      if (!trimmed || phase === "planning" || phase === "executing") return;
      savePromptToHistory(trimmed);
      submit(trimmed, explain);
    },
    [input, phase, explain, submit],
  );

  const handleSuggestionSelect = useCallback(
    (prompt: string) => {
      setInput(prompt);
      savePromptToHistory(prompt);
      submit(prompt, explain);
    },
    [explain, submit],
  );

  const handleReset = useCallback(() => {
    setInput("");
    reset();
    inputRef.current?.focus();
  }, [reset]);

  const isActive = phase !== "idle";
  const isWorking = phase === "planning" || phase === "executing";

  return (
    <div className="h-full flex flex-col items-center justify-center px-4">
      {/* Logo area */}
      <div className="mb-8 flex items-center gap-2 text-muted-foreground/60">
        <Sparkles className="w-6 h-6" />
        <span className="text-lg font-medium tracking-wide">qontinui</span>
      </div>

      {/* Search input */}
      <form onSubmit={handleSubmit} className="w-full max-w-xl">
        <div
          className={`
            flex items-center gap-3 px-4 py-3 rounded-2xl border bg-card
            shadow-sm transition-all
            ${isWorking ? "border-primary/40 shadow-primary/10 shadow-md" : "border-border hover:shadow-md hover:border-border/80"}
            focus-within:shadow-md focus-within:border-primary/40
          `}
        >
          {isWorking ? (
            <Loader2 className="w-5 h-5 text-primary animate-spin shrink-0" />
          ) : (
            <Search className="w-5 h-5 text-muted-foreground shrink-0" />
          )}
          <input
            ref={inputRef}
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="What would you like to do?"
            disabled={isWorking}
            className="flex-1 bg-transparent outline-none text-base placeholder:text-muted-foreground/50"
          />
          {input.trim() && !isWorking && (
            <button
              type="submit"
              className="p-1 rounded-lg hover:bg-accent transition-colors"
            >
              <ChevronRight className="w-5 h-5 text-primary" />
            </button>
          )}
        </div>
      </form>

      {/* Suggestions (only when idle) */}
      {!isActive && (
        <PromptSuggestions onSelect={handleSuggestionSelect} />
      )}

      {/* Explain toggle */}
      {!isActive && (
        <label className="flex items-center gap-2 mt-6 text-sm text-muted-foreground cursor-pointer select-none">
          <input
            type="checkbox"
            checked={explain}
            onChange={(e) => setExplain(e.target.checked)}
            className="rounded border-border"
          />
          Explain steps
        </label>
      )}

      {/* Status area */}
      {isActive && (
        <div className="mt-6 w-full max-w-xl">
          {/* Planning state */}
          {phase === "planning" && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground justify-center">
              <Loader2 className="w-4 h-4 animate-spin" />
              Understanding your request...
            </div>
          )}

          {/* Executing state */}
          {phase === "executing" && plan && (
            <div className="space-y-3">
              <p className="text-sm text-muted-foreground text-center">{plan.summary}</p>
              <div className="space-y-1">
                {plan.steps.map((step, i) => {
                  const isCurrent = i === progress.currentIndex;
                  const isDone = i < progress.currentIndex;
                  return (
                    <div
                      key={i}
                      className={`flex items-start gap-2 px-3 py-2 rounded-lg text-sm transition-colors ${
                        isCurrent
                          ? "bg-primary/10 text-foreground"
                          : isDone
                            ? "text-muted-foreground"
                            : "text-muted-foreground/40"
                      }`}
                    >
                      <span className="shrink-0 mt-0.5">
                        {isDone ? (
                          <CheckCircle2 className="w-4 h-4 text-green-500" />
                        ) : isCurrent ? (
                          <Loader2 className="w-4 h-4 text-primary animate-spin" />
                        ) : (
                          <span className="w-4 h-4 block rounded-full border border-current" />
                        )}
                      </span>
                      <span>{step.explanation}</span>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Done state */}
          {phase === "done" && (
            <div className="text-center space-y-3">
              <div className="flex items-center gap-2 text-sm text-green-600 justify-center">
                <CheckCircle2 className="w-4 h-4" />
                Done!
              </div>
              <button
                onClick={handleReset}
                className="text-sm text-primary hover:underline"
              >
                Start another task
              </button>
            </div>
          )}

          {/* Error state */}
          {phase === "error" && (
            <div className="text-center space-y-3">
              <div className="flex items-center gap-2 text-sm text-red-500 justify-center">
                <XCircle className="w-4 h-4" />
                {error}
              </div>
              <div className="flex gap-3 justify-center">
                <button
                  onClick={() => {
                    const trimmed = input.trim();
                    if (trimmed) submit(trimmed, explain);
                  }}
                  className="text-sm text-primary hover:underline"
                >
                  Retry
                </button>
                <button
                  onClick={handleReset}
                  className="text-sm text-muted-foreground hover:underline"
                >
                  Start over
                </button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
