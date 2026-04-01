/**
 * ScriptPreview
 *
 * Displays an AI-generated demo script as a step-by-step timeline
 * with narration text and action badges.
 */

import type { DemoScript, DemoStep, DemoAction } from "@/lib/demo-video/types";

// =============================================================================
// Action Badge
// =============================================================================

function actionLabel(action: DemoAction): string {
  switch (action.type) {
    case "navigate":
      return `Navigate → ${action.target}`;
    case "click":
      return `Click: ${action.elementId}`;
    case "type":
      return `Type: "${action.text.length > 20 ? action.text.slice(0, 20) + "…" : action.text}"`;
    case "select":
      return `Select: ${action.value}`;
    case "scroll":
      return `Scroll ${action.direction}`;
    case "wait":
      return `Wait ${action.ms}ms`;
    case "waitForElement":
      return `Wait for element`;
    case "waitForIdle":
      return `Wait for idle`;
    case "visual": {
      const name =
        action.filePath === "PLACEHOLDER" ? "needs asset" : action.filePath.split(/[/\\]/).pop();
      return `Visual: ${name}${action.caption ? ` — "${action.caption}"` : ""}`;
    }
  }
}

function actionColor(type: DemoAction["type"]): string {
  switch (type) {
    case "navigate":
      return "bg-blue-500/20 text-blue-400 border-blue-500/30";
    case "click":
      return "bg-green-500/20 text-green-400 border-green-500/30";
    case "type":
      return "bg-amber-500/20 text-amber-400 border-amber-500/30";
    case "select":
      return "bg-purple-500/20 text-purple-400 border-purple-500/30";
    case "scroll":
      return "bg-cyan-500/20 text-cyan-400 border-cyan-500/30";
    case "visual":
      return "bg-pink-500/20 text-pink-400 border-pink-500/30";
    default:
      return "bg-gray-500/20 text-gray-400 border-gray-500/30";
  }
}

function ActionBadge({ action }: { action: DemoAction }) {
  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded text-xs border ${actionColor(action.type)}`}
    >
      {actionLabel(action)}
    </span>
  );
}

// =============================================================================
// Step Card
// =============================================================================

function StepCard({ step, index, isActive }: { step: DemoStep; index: number; isActive: boolean }) {
  return (
    <div
      className={`relative pl-8 pb-6 border-l-2 ${isActive ? "border-primary" : "border-border"}`}
    >
      {/* Timeline dot */}
      <div
        className={`absolute left-0 top-0 -translate-x-1/2 w-3 h-3 rounded-full border-2 ${
          isActive ? "bg-primary border-primary" : "bg-background border-muted-foreground"
        }`}
      />

      {/* Step content */}
      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <span className="text-xs font-mono text-muted-foreground">Step {index + 1}</span>
          {step.highlight && (
            <span className="text-xs text-primary/70">
              ✦ {step.highlight.type} on {step.highlight.elementId}
            </span>
          )}
          <span className="text-xs text-muted-foreground ml-auto">
            +{(step.pauseAfterMs / 1000).toFixed(1)}s pause
          </span>
        </div>

        <p className="text-sm">{step.narration}</p>

        <div className="flex flex-wrap gap-1.5">
          {step.actions.map((action, i) => (
            <ActionBadge key={i} action={action} />
          ))}
        </div>

        {step.visualSuggestion && (
          <div className="mt-1 text-xs text-pink-400/80 bg-pink-500/5 border border-pink-500/10 rounded px-2 py-1.5 italic">
            Visual idea: {step.visualSuggestion}
          </div>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// ScriptPreview Component
// =============================================================================

interface ScriptPreviewProps {
  script: DemoScript;
  activeStepIndex?: number;
}

export function ScriptPreview({ script, activeStepIndex = -1 }: ScriptPreviewProps) {
  const estimatedMinutes = Math.ceil(script.estimatedDurationSecs / 60);

  return (
    <div className="space-y-4">
      {/* Header */}
      <div>
        <h3 className="text-base font-semibold">{script.title}</h3>
        <p className="text-sm text-muted-foreground mt-1">{script.description}</p>
        <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
          <span>{script.steps.length} steps</span>
          <span>~{estimatedMinutes} min</span>
          <span>Page: {script.targetPage}</span>
        </div>
      </div>

      {/* Timeline */}
      <div className="mt-4">
        {script.steps.map((step, i) => (
          <StepCard key={step.id} step={step} index={i} isActive={i === activeStepIndex} />
        ))}
      </div>
    </div>
  );
}
