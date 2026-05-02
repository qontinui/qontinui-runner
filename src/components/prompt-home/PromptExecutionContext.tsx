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

export type BackgroundTask = {
  kind: "ui-bridge-generation";
  /** Original user prompt that kicked off the generation. */
  promptText: string;
  /** Date.now() captured when the background task was first detected. */
  startedAt: number;
};

type PromptExecutionValue = ReturnType<typeof usePromptExecution> & {
  explain: boolean;
  setExplain: (value: boolean) => void;
  backgroundTask: BackgroundTask | null;
  clearBackgroundTask: () => void;
};

const PromptExecutionContext = createContext<PromptExecutionValue | null>(null);

export function PromptExecutionProvider({ children }: { children: ReactNode }) {
  const execution = usePromptExecution();
  const [explain, setExplain] = useState(() => instanceStorage.getItem(EXPLAIN_KEY) === "true");
  const [backgroundTask, setBackgroundTask] = useState<BackgroundTask | null>(null);

  // Track the last `phase` we observed so we only react on the rising edge of
  // the "done" transition. Without this, every re-render while phase==="done"
  // would re-evaluate the detection rule and could overwrite a user-dismissed
  // backgroundTask the moment the inner hook re-rendered.
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

  const clearBackgroundTask = useCallback(() => {
    setBackgroundTask(null);
  }, []);

  // Detect the long-running UI Bridge generation. We only care about the
  // moment the executor flips to "done" — the planner has already issued the
  // click, so the in-page pipeline state machine (data-pipeline-phase) takes
  // over. Inspect the last executed step's instruction; if it matches the
  // generate-button click pattern, register a backgroundTask. The pill is
  // dismissed exclusively via clearBackgroundTask() (Phase A scope).
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

    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional: register pill on phase transition
    setBackgroundTask({
      kind: "ui-bridge-generation",
      promptText: execution.lastPrompt ?? "",
      startedAt: Date.now(),
    });
  }, [execution.phase, execution.plan, execution.lastPrompt]);

  const value: PromptExecutionValue = {
    ...execution,
    explain,
    setExplain,
    backgroundTask,
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
