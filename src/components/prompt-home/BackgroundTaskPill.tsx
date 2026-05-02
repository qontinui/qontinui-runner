import { useEffect, useRef, useState } from "react";
import {
  BACKGROUND_TASK_KIND_META,
  usePromptExecutionContext,
  type BackgroundTask,
  type BackgroundTaskKindMeta,
} from "./PromptExecutionContext";

/**
 * Persistent pills that surface in-progress background tasks (currently the
 * long-running UI Bridge integration generation) from any tab. Mounted in
 * App.tsx inside `PromptExecutionProvider` but outside `<TabContent>` so
 * pills survive tab switches.
 *
 * Phases B/C/D landed: clicking a pill body activates the kind's `jumpTabId`
 * via the `ui-bridge-set-tab` window event; observing a phase that lands in
 * `meta.terminalSuccess`/`meta.terminalError` fires a one-shot toast and
 * triggers a 30-minute auto-dismiss timer; multiple in-flight tasks render
 * as a vertical stack (capped at 3 visible, with a "+N more" indicator).
 *
 * Phase-source convention: any component that owns a long-running operation
 * exposes `data-task-phase="<phase>"` on its root element. Each pill scans
 * every such element on every poll and picks the first non-idle phase it
 * finds. This means multiple pills currently observe the same global phase
 * — a follow-up could correlate via `data-task-id`, but in practice the
 * user rarely runs more than one of the same kind concurrently.
 */

const POLL_INTERVAL_MS = 500;
const ELAPSED_TICK_MS = 1000;
const PROMPT_LABEL_MAX = 40;
const TOAST_VISIBLE_MS = 6_000;
const TERMINAL_AUTO_CLEAR_MS = 30 * 60 * 1000; // 30 minutes
const MAX_VISIBLE_PILLS = 3;

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

function classifyPhase(phase: string | null, meta: BackgroundTaskKindMeta): Tone {
  if (!phase) return "muted";
  if (meta.terminalError.has(phase)) return "error";
  if (meta.terminalSuccess.has(phase)) return "success";
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

/**
 * Dispatch the same `ui-bridge-set-tab` window event the sidebar/state-machine
 * navigation paths use. `useAppNavigation` registers a direct listener that
 * sets `activeTab` synchronously, so the tab swap happens in-process without
 * a Tauri round-trip.
 */
function jumpToTab(tabId: string): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent("ui-bridge-set-tab", { detail: { tab: tabId } }));
}

interface ToastState {
  tone: "success" | "error";
  message: string;
}

interface SinglePillProps {
  task: BackgroundTask;
  onDismiss: (id: string) => void;
}

/**
 * One pill + its inline terminal toast. Each pill polls the global phase
 * surface independently — cheap (a single querySelectorAll at 2Hz), and
 * keeps the per-task lifecycle (toast-fired flag, auto-clear timer) local.
 */
function SinglePill({ task, onDismiss }: SinglePillProps) {
  const meta = BACKGROUND_TASK_KIND_META[task.kind];
  const [activePhase, setActivePhase] = useState<string | null>(null);
  const [now, setNow] = useState<number>(() => Date.now());
  const [toast, setToast] = useState<ToastState | null>(null);

  // Per-task one-shot guard: once a terminal phase fires the toast for this
  // task id, we never fire again even if the phase oscillates (e.g. the user
  // re-runs generation on the same panel and the phase cycles through
  // applied -> generating -> applied).
  const terminalToastFiredRef = useRef(false);
  // Once we observe the first terminal phase, start a 30-minute auto-dismiss
  // timer so stale completed pills don't accumulate. Tracked separately from
  // the toast guard because the timer can be cancelled on unmount.
  const autoClearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Poll `[data-task-phase]` / `[data-pipeline-phase]` while the pill is
  // mounted. Polling rather than MutationObserver because the phase-emitting
  // element may not exist at pill-mount (panels mount lazily mid-flow), so
  // the observer would need to re-attach on body mutations anyway.
  useEffect(() => {
    setActivePhase(readActiveTaskPhase());
    const id = window.setInterval(() => {
      setActivePhase(readActiveTaskPhase());
    }, POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, []);

  // Tick the elapsed-time display once per second. Cheaper than rolling it
  // into the phase poller — we don't want to thrash renders at 2Hz just to
  // refresh seconds.
  useEffect(() => {
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), ELAPSED_TICK_MS);
    return () => window.clearInterval(id);
  }, []);

  // Fire terminal toast + arm auto-dismiss once we observe a terminal phase.
  useEffect(() => {
    if (terminalToastFiredRef.current) return;
    if (!activePhase) return;

    const isSuccess = meta.terminalSuccess.has(activePhase);
    const isError = meta.terminalError.has(activePhase);
    if (!isSuccess && !isError) return;

    terminalToastFiredRef.current = true;

    const tone: ToastState["tone"] = isSuccess ? "success" : "error";
    const message = isSuccess
      ? `✓ ${meta.label} ready`
      : `✕ ${meta.label} failed`;
    setToast({ tone, message });

    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    toastTimerRef.current = setTimeout(() => setToast(null), TOAST_VISIBLE_MS);

    if (autoClearTimerRef.current) clearTimeout(autoClearTimerRef.current);
    autoClearTimerRef.current = setTimeout(
      () => onDismiss(task.id),
      TERMINAL_AUTO_CLEAR_MS,
    );
  }, [activePhase, meta, onDismiss, task.id]);

  // Cancel pending timers on unmount (dismiss / tab-shutdown).
  useEffect(() => {
    return () => {
      if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
      if (autoClearTimerRef.current) clearTimeout(autoClearTimerRef.current);
    };
  }, []);

  const tone = classifyPhase(activePhase, meta);
  const badgeLabel = activePhase ?? "starting…";
  const promptLabel = truncatePrompt(task.promptText) || "(no prompt text)";

  return (
    <div className="flex flex-col items-end gap-2">
      {toast && (
        <div
          role="status"
          aria-live="polite"
          data-testid="background-task-toast"
          className={`max-w-sm px-3 py-2 rounded-lg border bg-card shadow-lg text-sm ${
            toast.tone === "success"
              ? "border-emerald-500/50 text-emerald-400"
              : "border-destructive/50 text-destructive"
          }`}
        >
          {toast.message}
        </div>
      )}
      <div
        role="status"
        aria-live="polite"
        data-testid="background-task-pill"
        className={`flex items-center gap-3 max-w-sm rounded-lg border bg-card shadow-lg text-sm text-foreground ${BORDER_TONE_CLASSES[tone]}`}
      >
        <button
          type="button"
          onClick={() => jumpToTab(meta.jumpTabId)}
          aria-label={`View progress for ${promptLabel}`}
          className="flex flex-col min-w-0 flex-1 text-left px-3 py-2 cursor-pointer hover:bg-muted/30 transition-colors rounded-l-lg"
        >
          <span className="font-medium truncate" title={task.promptText}>
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
              {formatElapsed(task.startedAt, now)}
            </span>
          </div>
        </button>
        <button
          type="button"
          onClick={(e) => {
            // Stop propagation so the click never reaches the body button —
            // dismissing must not also fire a tab jump.
            e.stopPropagation();
            onDismiss(task.id);
          }}
          aria-label="Dismiss background task"
          className="shrink-0 text-muted-foreground hover:text-foreground transition-colors px-2 py-2"
        >
          &times;
        </button>
      </div>
    </div>
  );
}

export function BackgroundTaskPill() {
  const { backgroundTasks, clearBackgroundTask } = usePromptExecutionContext();

  if (backgroundTasks.length === 0) return null;

  // Newest tasks pinned to the bottom of the stack (closest to the corner)
  // so the user sees the latest at a glance; older tasks rise above. With
  // `flex-col-reverse` the array's last entry renders at the bottom.
  const visible = backgroundTasks.slice(-MAX_VISIBLE_PILLS);
  const overflowCount = backgroundTasks.length - visible.length;

  return (
    <div className="fixed bottom-4 right-4 z-toast flex flex-col-reverse items-end gap-2 pointer-events-none">
      {visible.map((task) => (
        <div key={task.id} className="pointer-events-auto">
          <SinglePill task={task} onDismiss={clearBackgroundTask} />
        </div>
      ))}
      {overflowCount > 0 && (
        <div
          className="pointer-events-auto px-3 py-1 rounded-lg border bg-card shadow-lg text-xs text-muted-foreground"
          data-testid="background-task-pill-overflow"
        >
          +{overflowCount} more
        </div>
      )}
    </div>
  );
}
