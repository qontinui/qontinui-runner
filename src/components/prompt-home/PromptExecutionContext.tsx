import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { usePromptExecution } from "./usePromptExecution";
import { useExplainModeTutorial } from "./useExplainModeTutorial";

const EXPLAIN_KEY = "prompt-home-explain";

/**
 * Detection rule for the UI Bridge generation background task.
 *
 * The runner's home-prompt executor reports `phase: "done"` within seconds of
 * submission, but if the final step clicked the `ui-bridge-generate-button`
 * on `config-ui-bridge`'s `ProjectCoordinator` panel, the actual project-
 * integration AI generation pipeline takes 5-15 minutes to complete in the
 * background. This regex matches the planner's instruction text for that
 * click step so the context can flag a long-running background task and
 * the global pill component can surface progress across tab switches.
 */
const UI_BRIDGE_GENERATE_INSTRUCTION_RE = /click element ui-bridge-generate-button/i;

export type BackgroundTaskKind = "ui-bridge-generation";

export interface BackgroundTask {
  /** Stable id used for keying React lists and targeting per-task dismiss. */
  id: string;
  kind: BackgroundTaskKind;
  /** Original user prompt that kicked off the generation. */
  promptText: string;
  /** Date.now() captured when the background task was first detected. */
  startedAt: number;
}

/**
 * Per-kind metadata shared between the context and the pill. The pill uses
 * this to (a) decide which tab to activate when the body is clicked, (b)
 * classify observed phases into terminal-success/terminal-error/in-progress,
 * and (c) build the human-readable label injected into the success/error
 * toast.
 *
 * Centralizing it here keeps the pill component generic — any future
 * background-task kind only needs an entry here plus a detection rule in
 * the spawn useEffect below.
 */
export interface BackgroundTaskKindMeta {
  /** Tab id to activate when the pill body is clicked. */
  jumpTabId: string;
  /** Phase values that mean "done — success". Pill flips to success tone + fires success toast. */
  terminalSuccess: ReadonlySet<string>;
  /** Phase values that mean "done — error". Pill flips to error tone + fires error toast. */
  terminalError: ReadonlySet<string>;
  /** Human-readable label fragment used in the success/error toast text. */
  label: string;
}

export const BACKGROUND_TASK_KIND_META: Record<BackgroundTaskKind, BackgroundTaskKindMeta> = {
  "ui-bridge-generation": {
    jumpTabId: "config-ui-bridge",
    terminalSuccess: new Set(["applied", "generated", "preview"]),
    terminalError: new Set(["failed"]),
    label: "UI Bridge integration",
  },
};

type PromptExecutionValue = ReturnType<typeof usePromptExecution> & {
  explain: boolean;
  setExplain: (value: boolean) => void;
  backgroundTasks: BackgroundTask[];
  clearBackgroundTask: (id: string) => void;
};

const PromptExecutionContext = createContext<PromptExecutionValue | null>(null);

function generateTaskId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export function PromptExecutionProvider({ children }: { children: ReactNode }) {
  const execution = usePromptExecution();
  const [explain, setExplain] = useState(() => instanceStorage.getItem(EXPLAIN_KEY) === "true");
  const [backgroundTasks, setBackgroundTasks] = useState<BackgroundTask[]>([]);

  // Track the last `phase` we observed so we only react on the rising edge of
  // the "done" transition. Without this, every re-render while phase==="done"
  // would re-evaluate the detection rule and re-append a duplicate task —
  // and dismissing one would just respawn the same task on the next render.
  const lastPhaseRef = useRef(execution.phase);

  useEffect(() => {
    instanceStorage.setItem(EXPLAIN_KEY, String(explain));
  }, [explain]);

  useExplainModeTutorial(
    explain,
    execution.phase,
    execution.progress,
    execution.plan?.steps,
    execution.plan?.summary,
  );

  const clearBackgroundTask = useCallback((id: string) => {
    setBackgroundTasks((prev) => prev.filter((t) => t.id !== id));
  }, []);

  // Detect the long-running UI Bridge generation. We only care about the
  // moment the executor flips to "done" — the planner has already issued the
  // click, so the in-page pipeline state machine (data-pipeline-phase) takes
  // over. Inspect the last executed step's instruction; if it matches the
  // generate-button click pattern, append a backgroundTask. The rising-edge
  // guard ensures we don't re-append the same task on subsequent renders
  // (which would also defeat dismiss).
  useEffect(() => {
    const prevPhase = lastPhaseRef.current;
    lastPhaseRef.current = execution.phase;

    if (execution.phase !== "done" || prevPhase === "done") return;

    const steps = execution.plan?.steps;
    if (!steps || steps.length === 0) return;

    // The executor advances `progress.currentIndex` to `steps.length` after
    // the final step completes; the last actually-executed step is therefore
    // the final entry in the plan.
    const lastStep = steps[steps.length - 1];
    const instruction = lastStep?.instruction ?? "";
    if (!UI_BRIDGE_GENERATE_INSTRUCTION_RE.test(instruction)) return;

    setBackgroundTasks((prev) => [
      ...prev,
      {
        id: generateTaskId(),
        kind: "ui-bridge-generation",
        promptText: execution.lastPrompt ?? "",
        startedAt: Date.now(),
      },
    ]);
  }, [execution.phase, execution.plan, execution.lastPrompt]);

  const value: PromptExecutionValue = {
    ...execution,
    explain,
    setExplain,
    backgroundTasks,
    clearBackgroundTask,
  };

  return (
    <PromptExecutionContext.Provider value={value}>{children}</PromptExecutionContext.Provider>
  );
}

export function usePromptExecutionContext(): PromptExecutionValue {
  const ctx = useContext(PromptExecutionContext);
  if (!ctx) {
    throw new Error("usePromptExecutionContext must be used within PromptExecutionProvider");
  }
  return ctx;
}
