import { Sparkles, FolderSearch, FileText, Bot } from "lucide-react";

interface WelcomeStepProps {
  onNext: () => void;
}

export function WelcomeStep({ onNext }: WelcomeStepProps) {
  return (
    <div className="tutorial-fade-in flex flex-col items-center text-center py-8 px-4">
      <div className="w-16 h-16 rounded-2xl bg-primary/20 flex items-center justify-center mb-6">
        <Sparkles className="w-8 h-8 text-primary" />
      </div>

      <h2 className="text-h2 mb-3">Welcome to Qontinui Runner</h2>
      <p className="text-body text-muted-foreground max-w-md mb-8">
        Let's get your environment set up. This wizard will help you discover projects, configure
        log sources, and optionally set up an AI provider.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4 max-w-lg mb-8 w-full">
        <div className="panel p-4 text-center">
          <FolderSearch className="w-6 h-6 text-cyan-400 mx-auto mb-2" />
          <p className="text-label-sm">Discover Projects</p>
        </div>
        <div className="panel p-4 text-center">
          <FileText className="w-6 h-6 text-emerald-400 mx-auto mb-2" />
          <p className="text-label-sm">Configure Logs</p>
        </div>
        <div className="panel p-4 text-center">
          <Bot className="w-6 h-6 text-purple-400 mx-auto mb-2" />
          <p className="text-label-sm">AI Provider</p>
        </div>
      </div>

      <button
        className="btn-primary px-8 py-2.5 text-base"
        onClick={onNext}
        data-ui-id="setup-wizard-get-started"
      >
        Get Started
      </button>
    </div>
  );
}
