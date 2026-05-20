import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
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
