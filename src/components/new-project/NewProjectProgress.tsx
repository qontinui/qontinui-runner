import { CheckCircle2, XCircle, Loader2, MinusCircle, Circle } from "lucide-react";

/** Per-step status emitted by the `new-project://progress` Tauri event. */
export type StepStatus = "pending" | "running" | "ok" | "failed" | "skipped";

/** One entry in the progress checklist. */
export interface StepState {
  status: StepStatus;
  detail: string;
}

/**
 * The eight `create_new_project` steps, in execution order. MUST match
 * `STEP_ORDER` in `src-tauri/src/commands/new_project.rs`.
 */
export const STEP_ORDER = [
  "validate",
  "create_remote",
  "init_local",
  "scaffold",
  "commit",
  "push",
  "enroll",
  "register",
] as const;

export type StepName = (typeof STEP_ORDER)[number];

/** Human-readable label for each step. */
const STEP_LABELS: Record<StepName, string> = {
  validate: "Validate name & location",
  create_remote: "Create GitHub repository",
  init_local: "Initialize git repository",
  scaffold: "Write template files",
  commit: "Create initial commit",
  push: "Push to GitHub",
  enroll: "Enroll with Qontinui",
  register: "Register project",
};

/** Fresh, all-pending step map — the starting point for a create run. */
export function initialSteps(): Record<StepName, StepState> {
  return STEP_ORDER.reduce(
    (acc, step) => {
      acc[step] = { status: "pending", detail: "" };
      return acc;
    },
    {} as Record<StepName, StepState>,
  );
}

function StepIcon({ status }: { status: StepStatus }) {
  switch (status) {
    case "ok":
      return <CheckCircle2 className="w-4 h-4 text-green-400 shrink-0" />;
    case "failed":
      return <XCircle className="w-4 h-4 text-red-400 shrink-0" />;
    case "running":
      return <Loader2 className="w-4 h-4 text-primary animate-spin shrink-0" />;
    case "skipped":
      return <MinusCircle className="w-4 h-4 text-zinc-600 shrink-0" />;
    default:
      return <Circle className="w-4 h-4 text-zinc-600 shrink-0" />;
  }
}

interface NewProjectProgressProps {
  steps: Record<StepName, StepState>;
}

/**
 * Renders the eight-step creation checklist. Skipped/pending steps are muted;
 * the running step spins; failed steps show their detail in red.
 */
export function NewProjectProgress({ steps }: NewProjectProgressProps) {
  return (
    <div className="flex flex-col gap-1.5">
      {STEP_ORDER.map((step) => {
        const { status, detail } = steps[step];
        const muted = status === "pending" || status === "skipped";
        return (
          <div
            key={step}
            className={`flex items-start gap-2.5 px-3 py-2 rounded panel ${
              muted ? "opacity-50" : ""
            } ${status === "failed" ? "border-red-500/40 bg-red-500/5" : ""}`}
          >
            <StepIcon status={status} />
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium">{STEP_LABELS[step]}</div>
              {detail && (
                <div
                  className={`text-xs truncate ${
                    status === "failed" ? "text-red-300" : "text-muted-foreground"
                  }`}
                >
                  {detail}
                </div>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
