import { useEffect, useState } from "react";
import { usePromptExecutionContext } from "./PromptExecutionContext";

/**
 * Persistent header pill that surfaces an in-progress background task
 * (currently only the long-running UI Bridge integration generation) from
 * any tab. Mounted in App.tsx inside `PromptExecutionProvider` but outside
 * `<TabContent>` so it survives tab switches.
 *
 * Phase A scope: shows the prompt label, a colored phase badge sourced from
 * any in-page `[data-task-phase]` element (legacy `[data-pipeline-phase]`
 * also honoured), elapsed time, and a manual dismiss button. Phases B/C/D
 * (click-to-jump, terminal-state toast, multi-task array) are deferred.
 *
 * Phase-source convention: any component that owns a long-running operation
 * exposes `data-task-phase="<phase>"` on its root element. The pill scans
 * every such element on every poll/mutation and picks the first non-idle
 * phase it finds. Multiple panels (ProjectCoordinator's one-click flow,
 * HookGenerationPanel's per-page generation) can coexist; whichever is
 * actually working at the moment wins.
 */

const POLL_INTERVAL_MS = 500;
const ELAPSED_TICK_MS = 1000;
const PROMPT_LABEL_MAX = 40;

const TERMINAL_SUCCESS_PHASES: ReadonlySet<string> = new Set([
  "applied",
  "generated",
  "preview", // HookGenerationPanel's "ready to apply" state
]);
const TERMINAL_ERROR_PHASES: ReadonlySet<string> = new Set(["failed"]);
const IN_PROGRESS_PHASE_PREFIXES: readonly string[] = [
  "analyzing",
  "integrating",
  "discovering",
  "generating", // catches HookGenerationPanel's "generating-page-tutorial" etc.
  "applying",
];
const IDLE_PHASES: ReadonlySet<string> = new Set(["idle", "no-pages"]);

/**
 * Scan every `[data-task-phase]` (canonical) and `[data-pipeline-phase]`
 * (legacy) element on the page. Return the first non-idle phase value
 * encountered, or the first idle one if everything is idle, or null if
 * no phase-emitting element is mounted.
 */
function readActiveTaskPhase(): string | null {
  if (typeof document === "undefined") return null;
  const selectors = "[data-task-phase], [data-pipeline-phase]";
  const elements = document.querySelectorAll<HTMLElement>(selectors);
  let firstIdle: string | null = null;
  for (const el of elements) {
    const phase = el.dataset.taskPhase ?? el.dataset.pipelinePhase ?? null;
    if (!phase) continue;
    if (!IDLE_PHASES.has(phase)) {
      // Active surface — prefer it.
      return phase;
    }
    if (firstIdle === null) firstIdle = phase;
  }
  return firstIdle;
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
  if (TERMINAL_ERROR_PHASES.has(phase)) return "error";
  if (TERMINAL_SUCCESS_PHASES.has(phase)) return "success";
  if (IN_PROGRESS_PHASE_PREFIXES.some((p) => phase === p || phase.startsWith(`${p}-`))) {
    return "in-progress";
  }
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
  const [activePhase, setActivePhase] = useState<string | null>(null);
  const [now, setNow] = useState<number>(() => Date.now());

  // Poll `[data-task-phase]` / `[data-pipeline-phase]` while a task is in
  // flight. We use a polling interval rather than MutationObserver because
  // the phase-emitting element may not exist at pill-mount (panels mount
  // lazily mid-flow), so the observer would need to re-attach on body
  // mutations anyway — equivalent cost. Restarting the interval whenever
  // `backgroundTask` changes ensures we tear down the poller as soon as
  // the user dismisses the pill.
  useEffect(() => {
    if (!backgroundTask) {
      setActivePhase(null);
      return;
    }
    setActivePhase(readActiveTaskPhase());
    const id = window.setInterval(() => {
      setActivePhase(readActiveTaskPhase());
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

  const tone = classifyPhase(activePhase);
  const badgeLabel = activePhase ?? "starting…";
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
