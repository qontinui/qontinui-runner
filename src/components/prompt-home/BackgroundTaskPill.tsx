import { useEffect, useState } from "react";
import { usePromptExecutionContext } from "./PromptExecutionContext";

/**
 * Persistent header pill that surfaces an in-progress background task
 * (currently only the long-running UI Bridge integration generation) from
 * any tab. Mounted in App.tsx inside `PromptExecutionProvider` but outside
 * `<TabContent>` so it survives tab switches.
 *
 * Phase A scope: shows the prompt label, a colored phase badge polled from
 * the in-page `[data-pipeline-phase]` attribute, elapsed time, and a manual
 * dismiss button. Phases B/C/D (click-to-jump, terminal-state toast,
 * multi-task array) are deferred.
 */

const POLL_INTERVAL_MS = 500;
const ELAPSED_TICK_MS = 1000;
const PROMPT_LABEL_MAX = 40;

const TERMINAL_SUCCESS_PHASES: ReadonlySet<string> = new Set(["applied", "generated"]);
const TERMINAL_ERROR_PHASES: ReadonlySet<string> = new Set(["failed"]);
const IN_PROGRESS_PHASES: ReadonlySet<string> = new Set([
  "analyzing",
  "integrating",
  "discovering",
  "generating",
]);

function readPipelinePhase(): string | null {
  if (typeof document === "undefined") return null;
  const el = document.querySelector("[data-pipeline-phase]");
  if (!el) return null;
  // dataset is only typed on HTMLElement, so narrow before reading.
  if (el instanceof HTMLElement) {
    return el.dataset.pipelinePhase ?? null;
  }
  return el.getAttribute("data-pipeline-phase");
}

function formatElapsed(startedAt: number, now: number): string {
  const totalSec = Math.max(0, Math.floor((now - startedAt) / 1000));
  const minutes = Math.floor(totalSec / 60);
  const seconds = totalSec % 60;
  return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
}

function truncatePrompt(text: string): string {
  const trimmed = text.trim();
  if (trimmed.length <= PROMPT_LABEL_MAX) return trimmed;
  return `${trimmed.slice(0, PROMPT_LABEL_MAX).trimEnd()}…`;
}

type Tone = "in-progress" | "success" | "error" | "muted";

function classifyPhase(phase: string | null): Tone {
  if (!phase) return "muted";
  if (TERMINAL_SUCCESS_PHASES.has(phase)) return "success";
  if (TERMINAL_ERROR_PHASES.has(phase)) return "error";
  if (IN_PROGRESS_PHASES.has(phase)) return "in-progress";
  return "muted";
}

const BADGE_TONE_CLASSES: Record<Tone, string> = {
  "in-progress": "bg-primary/20 text-primary border-primary/40",
  success: "bg-emerald-500/20 text-emerald-400 border-emerald-500/40",
  error: "bg-destructive/20 text-destructive border-destructive/40",
  muted: "bg-muted/40 text-muted-foreground border-border",
};

const BORDER_TONE_CLASSES: Record<Tone, string> = {
  "in-progress": "border-primary/50",
  success: "border-emerald-500/50",
  error: "border-destructive/50",
  muted: "border-border",
};

export function BackgroundTaskPill() {
  const { backgroundTask, clearBackgroundTask } = usePromptExecutionContext();
  const [pipelinePhase, setPipelinePhase] = useState<string | null>(null);
  const [now, setNow] = useState<number>(() => Date.now());

  // Poll `data-pipeline-phase` while a task is in flight. Restarting the
  // interval whenever `backgroundTask` changes ensures we tear down the
  // poller as soon as the user dismisses the pill.
  useEffect(() => {
    if (!backgroundTask) {
      setPipelinePhase(null);
      return;
    }
    setPipelinePhase(readPipelinePhase());
    const id = window.setInterval(() => {
      setPipelinePhase(readPipelinePhase());
    }, POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [backgroundTask]);

  // Tick the elapsed-time display once per second. Cheaper than rolling it
  // into the phase poller — we don't want to thrash renders at 2Hz just to
  // refresh seconds.
  useEffect(() => {
    if (!backgroundTask) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), ELAPSED_TICK_MS);
    return () => window.clearInterval(id);
  }, [backgroundTask]);

  if (!backgroundTask) return null;

  const tone = classifyPhase(pipelinePhase);
  const badgeLabel = pipelinePhase ?? "starting…";
  const promptLabel = truncatePrompt(backgroundTask.promptText) || "(no prompt text)";

  return (
    <div
      role="status"
      aria-live="polite"
      data-testid="background-task-pill"
      className={`fixed bottom-4 right-4 z-toast flex items-center gap-3 max-w-sm px-3 py-2 rounded-lg border bg-card shadow-lg text-sm text-foreground ${BORDER_TONE_CLASSES[tone]}`}
    >
      <div className="flex flex-col min-w-0">
        <span className="font-medium truncate" title={backgroundTask.promptText}>
          {promptLabel}
        </span>
        <div className="flex items-center gap-2 mt-0.5">
          <span
            className={`inline-flex items-center px-2 py-0.5 rounded border text-xs font-medium ${BADGE_TONE_CLASSES[tone]}`}
            data-testid="background-task-pill-phase"
          >
            {badgeLabel}
          </span>
          <span className="text-xs text-muted-foreground tabular-nums">
            {formatElapsed(backgroundTask.startedAt, now)}
          </span>
        </div>
      </div>
      <button
        type="button"
        onClick={clearBackgroundTask}
        aria-label="Dismiss background task"
        className="shrink-0 text-muted-foreground hover:text-foreground transition-colors px-1"
      >
        &times;
      </button>
    </div>
  );
}
