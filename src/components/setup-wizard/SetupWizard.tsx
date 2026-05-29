import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent } from "@qontinui/ui-bridge";
import { StepIndicator } from "./StepIndicator";
import { TierStep } from "./TierStep";
import { WelcomeStep } from "./WelcomeStep";
import { ProjectStep } from "./ProjectStep";
import { ProcessStep } from "./ProcessStep";
import { DevServicesStep } from "./DevServicesStep";
import { AiProviderStep } from "./AiProviderStep";
import { ClaudeConfigStep } from "./ClaudeConfigStep";

const STEPS = [
  "Tier",
  "Welcome",
  "Projects",
  "Processes",
  "Dev Services",
  "AI Provider",
  "Claude Sessions",
];

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
  onComplete: () => void;
}

export function SetupWizard({ onComplete }: SetupWizardProps) {
  const [currentStep, setCurrentStep] = useState(0);
  const [, setWorkspacePath] = useState("");
  const [selectedProjects, setSelectedProjects] = useState<Project[]>([]);
  const [selectedProcessConfigs, setSelectedProcessConfigs] = useState<ProcessConfig[]>([]);

  const goNext = useCallback(() => {
    setCurrentStep((s) => Math.min(s + 1, STEPS.length - 1));
  }, []);

  const goBack = useCallback(() => {
    setCurrentStep((s) => Math.max(s - 1, 0));
  }, []);

  const handleAiProviderComplete = useCallback(
    (_aiConfig: { provider: string } | null) => {
      // AiProviderStep saves the provider internally; just advance
      goNext();
    },
    [goNext],
  );

  const finishSetup = useCallback(async () => {
    try {
      // Save process configs
      for (const config of selectedProcessConfigs) {
        await invoke("save_process_config", { config });
      }

      // Mark setup as completed
      await invoke("complete_setup");
      onComplete();
    } catch (err) {
      console.error("Failed to complete setup:", err);
      // Still complete even if save fails - user can configure later
      try {
        await invoke("complete_setup");
      } catch {
        // Ignore
      }
      onComplete();
    }
  }, [selectedProcessConfigs, onComplete]);

  // UI Bridge: expose programmatic wizard control so test runs and
  // automation can dismiss the wizard without driving 7 steps of OS
  // dialogs. `scope: 'global'` so the component is discoverable even
  // when the wizard isn't the active route (matters less in practice
  // since the wizard owns the whole viewport when shown, but keeps the
  // discovery contract explicit for `/control/components` callers who
  // are spawning a fresh temp runner and haven't navigated anywhere
  // yet). The `complete` action calls the same `finishSetup` flow that
  // the Claude-Sessions "Finish" button does, so persisted state ends
  // up identical to a real user completion (`complete_setup` Tauri
  // command writes `settings.setup_completed=true`, plus any process
  // configs the user picked).
  //
  // Operators who set the supervisor-forwarded
  // `QONTINUI_TEST_AUTO_LOGIN_EMAIL` env var don't need this action at
  // all — `check_setup_completed` returns true on the Rust side and
  // the wizard never mounts. This component-action is the second
  // discovery vector: useful for non-supervisor automation (e.g. a
  // CI smoke from outside the supervisor's spawn lane) and for
  // operators who want to dismiss the wizard from a UI Bridge driver
  // without restarting the runner.
  useUIComponent({
    id: "setup-wizard",
    name: "Setup Wizard",
    description:
      "First-launch onboarding wizard. Use 'complete' to dismiss programmatically " +
      "without driving 7 step interactions; equivalent to clicking through every step's " +
      "default + 'Finish'. Use 'go-to-step' to jump directly to a specific step (0-6).",
    scope: "global",
    actions: [
      {
        id: "complete",
        label: "Complete Setup",
        description:
          "Mark setup complete and dismiss the wizard. Persists via the same " +
          "`complete_setup` Tauri command the user-driven 'Finish' button uses.",
        handler: async () => {
          await finishSetup();
          return { success: true };
        },
      },
      {
        id: "go-to-step",
        label: "Jump to Step",
        description:
          "Set the wizard's current step. Values 0-6 correspond to: " +
          "Tier, Welcome, Projects, Processes, Dev Services, AI Provider, Claude Sessions.",
        paramSchema: { step: "number (0-6)" },
        handler: (params?: unknown) => {
          const { step } = (params ?? {}) as { step?: number };
          if (typeof step !== "number" || step < 0 || step >= STEPS.length) {
            throw new Error(
              `go-to-step requires { step: number } where 0 <= step < ${STEPS.length}`,
            );
          }
          setCurrentStep(step);
          return { success: true, step };
        },
      },
    ],
  });

  return (
    <div className="min-h-screen bg-background grid-dots flex flex-col items-center justify-center p-4">
      <div className="card w-full max-w-2xl p-8">
        <StepIndicator steps={STEPS} currentStep={currentStep} />

        {currentStep === 0 && <TierStep onNext={goNext} />}

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
