import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Info } from "lucide-react";
import { StepIndicator } from "./StepIndicator";
import { TierStep } from "./TierStep";
import { WelcomeStep } from "./WelcomeStep";
import { ProjectStep } from "./ProjectStep";
import { ProcessStep } from "./ProcessStep";
import { DevServicesStep } from "./DevServicesStep";
import { AiProviderStep } from "./AiProviderStep";
import { ClaudeConfigStep } from "./ClaudeConfigStep";

export const SETUP_WIZARD_STEPS = [
  "Tier",
  "Welcome",
  "Projects",
  "Processes",
  "Dev Services",
  "AI Provider",
  "Claude Sessions",
];

/** Local alias so the (many) in-file references stay short. */
const STEPS = SETUP_WIZARD_STEPS;

interface Project {
  path: string;
  name: string;
  type: string;
  manifest: string;
}

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  health_port?: number;
  parser: string;
  category: string;
  auto_start: boolean;
  enabled: boolean;
  buffer_size: number;
  env?: Record<string, string>;
  start_group?: number;
  dev_only?: boolean;
}

interface SetupWizardProps {
  /**
   * The step to show. LIFTED to `AppContent` (with `onStepChange`) so the
   * `setup-wizard` UI-Bridge component — whose `go-to-step` action mutates it —
   * can be registered there and stay addressable even while this component is
   * unmounted. `useUIComponent` only registers while its owner is mounted, so a
   * registration inside the wizard was unreachable in exactly the state a
   * driver needed it: a runner that auto-bypassed the wizard.
   */
  currentStep: number;
  onStepChange: (step: number) => void;
  /**
   * Marks setup complete (persists via `complete_setup`) and dismisses the
   * wizard. Also lifted, for the same reason — it backs the component's
   * `complete` action.
   */
  onComplete: () => void | Promise<void>;
}

export function SetupWizard({ currentStep, onStepChange, onComplete }: SetupWizardProps) {
  const [, setWorkspacePath] = useState("");
  // Wizard-level, non-blocking advisory raised by a step (see the banner
  // below). Kept here so it survives the step transition that raised it.
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedProjects, setSelectedProjects] = useState<Project[]>([]);
  const [selectedProcessConfigs, setSelectedProcessConfigs] = useState<ProcessConfig[]>([]);

  const goNext = useCallback(() => {
    onStepChange(Math.min(currentStep + 1, STEPS.length - 1));
  }, [currentStep, onStepChange]);

  const goBack = useCallback(() => {
    onStepChange(Math.max(currentStep - 1, 0));
  }, [currentStep, onStepChange]);

  const handleAiProviderComplete = useCallback(
    (_aiConfig: { provider: string } | null) => {
      // AiProviderStep saves the provider internally; just advance
      goNext();
    },
    [goNext],
  );

  // Saves what only THIS component knows (the process configs the user picked)
  // and then hands off to the lifted `onComplete`, which owns the
  // `complete_setup` persist + dismissal. A failed config save must not block
  // completion — the user can configure processes later.
  const finishSetup = useCallback(async () => {
    for (const config of selectedProcessConfigs) {
      try {
        await invoke("save_process_config", { config });
      } catch (err) {
        console.error("Failed to save process config during setup:", err);
      }
    }
    await onComplete();
  }, [selectedProcessConfigs, onComplete]);

  // NOTE: the `setup-wizard` UI-Bridge component registration used to live
  // here. It has moved UP to `AppContent` (`src/App.tsx`), beside the
  // `registerOpenableView` call, because `useUIComponent` only registers while
  // its owner is mounted — so a registration here was absent in exactly the
  // state a driver needed it (a runner whose `check_setup_completed` returned
  // true, e.g. any supervisor temp runner with `QONTINUI_TEST_AUTO_LOGIN_EMAIL`
  // set, where the wizard never mounts). `scope: "global"` does not survive an
  // unmount. The step state and the completion callback the two actions mutate
  // are lifted for the same reason (see the props above).

  return (
    <div className="min-h-screen bg-background grid-dots flex flex-col items-center justify-center p-4">
      <div className="card w-full max-w-2xl p-8">
        <StepIndicator steps={STEPS} currentStep={currentStep} />

        {/*
          Wizard-level advisory banner. Lives HERE, not in the step that raises
          it: a step that reports "this only applies to your session" also
          advances the wizard in the same tick, so a notice rendered inside it
          would be unmounted before anyone could read it.
        */}
        {notice && (
          <div
            id="setup-wizard-notice"
            role="status"
            className="flex items-start gap-2 p-3 mb-4 rounded-md bg-muted/40 border border-border/50"
          >
            <Info className="w-4 h-4 text-muted-foreground shrink-0 mt-0.5" />
            <div className="text-xs text-muted-foreground flex-1 min-w-0">{notice}</div>
            <button
              onClick={() => setNotice(null)}
              aria-label="Dismiss notice"
              className="text-muted-foreground hover:text-foreground shrink-0"
            >
              &times;
            </button>
          </div>
        )}

        {currentStep === 0 && <TierStep onNext={goNext} onNotice={setNotice} />}

        {currentStep === 1 && <WelcomeStep onNext={goNext} />}

        {currentStep === 2 && (
          <ProjectStep
            selectedProjects={selectedProjects}
            onProjectsChange={setSelectedProjects}
            onWorkspacePathChange={setWorkspacePath}
            onNext={goNext}
            onBack={goBack}
          />
        )}

        {currentStep === 3 && (
          <ProcessStep
            selectedProjects={selectedProjects}
            selectedConfigs={selectedProcessConfigs}
            onConfigsChange={setSelectedProcessConfigs}
            onNext={goNext}
            onBack={goBack}
          />
        )}

        {currentStep === 4 && (
          <DevServicesStep
            alreadySelectedConfigs={selectedProcessConfigs}
            onNext={goNext}
            onBack={goBack}
          />
        )}

        {currentStep === 5 && (
          <AiProviderStep onComplete={handleAiProviderComplete} onBack={goBack} />
        )}

        {currentStep === 6 && <ClaudeConfigStep onComplete={finishSetup} onBack={goBack} />}
      </div>
    </div>
  );
}
