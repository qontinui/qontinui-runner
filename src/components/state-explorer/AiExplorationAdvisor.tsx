/**
 * AI Exploration Advisor Modal
 *
 * Suggests exploration strategies and target selection based on user goals.
 * Uses AI to recommend optimal settings for state machine testing.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, Loader2, Check, X, Compass, Wand2, Target, AlertTriangle } from "lucide-react";

interface ConfigStateInfo {
  id: string;
  name: string;
  description?: string;
  is_initial: boolean;
  is_final: boolean;
}

interface ConfigTransitionInfo {
  id: string;
  name: string;
  from_state?: string;
  to_state?: string;
}

type ExplorationStrategy = "exhaustive" | "smoke_test" | "regression" | "random_walk" | "targeted";

interface ExplorationSuggestion {
  strategy: ExplorationStrategy;
  target_state_ids: string[];
  target_transition_ids: string[];
  max_states: number;
  max_duration_seconds: number;
  rationale: string;
}

interface GenerateExplorationResponse {
  success: boolean;
  data?: ExplorationSuggestion;
  error?: string;
}

interface AiExplorationAdvisorProps {
  /** Available states from loaded config */
  availableStates: ConfigStateInfo[];
  /** Available transitions from loaded config */
  availableTransitions: ConfigTransitionInfo[];
  /** Called when a suggestion is accepted */
  onSuggestionAccepted: (suggestion: ExplorationSuggestion) => void;
  /** Called when the modal is closed */
  onClose: () => void;
}

const STRATEGY_INFO: Record<ExplorationStrategy, { name: string; description: string }> = {
  smoke_test: { name: "Smoke Test", description: "Quick verification of critical paths" },
  exhaustive: { name: "Exhaustive", description: "Verify all states and transitions" },
  regression: { name: "Regression", description: "Focus on previously failed areas" },
  random_walk: { name: "Random Walk", description: "Random exploration" },
  targeted: { name: "Targeted", description: "Verify specific states/transitions" },
};

export function AiExplorationAdvisor({
  availableStates,
  availableTransitions,
  onSuggestionAccepted,
  onClose,
}: AiExplorationAdvisorProps) {
  const [goal, setGoal] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [suggestion, setSuggestion] = useState<ExplorationSuggestion | null>(null);
  const [error, setError] = useState<string | null>(null);

  const hasConfig = availableStates.length > 0 || availableTransitions.length > 0;

  // Generate exploration suggestion using AI
  const handleGenerate = useCallback(async () => {
    if (!goal.trim()) {
      setError("Please describe your exploration goal");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setSuggestion(null);

    try {
      const response = await invoke<{
        success: boolean;
        message?: string;
        data?: GenerateExplorationResponse;
      }>("suggest_exploration_strategy_with_ai", {
        input: {
          user_goal: goal,
          available_states: availableStates.map((s) => ({
            id: s.id,
            name: s.name,
            description: s.description,
            is_initial: s.is_initial,
            is_final: s.is_final,
          })),
          available_transitions: availableTransitions.map((t) => ({
            id: t.id,
            name: t.name,
            from_state: t.from_state,
            to_state: t.to_state,
          })),
        },
      });

      if (response.success && response.data?.data) {
        setSuggestion(response.data.data);
      } else {
        setError(response.data?.error || response.message || "Failed to generate suggestion");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [goal, availableStates, availableTransitions]);

  // Accept suggestion
  const handleAccept = useCallback(() => {
    if (suggestion) {
      onSuggestionAccepted(suggestion);
    }
  }, [suggestion, onSuggestionAccepted]);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-card rounded-lg border border-border w-full max-w-2xl max-h-[80vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-2">
            <Sparkles className="w-5 h-5 text-emerald-400" />
            <span className="text-lg font-medium text-foreground">AI Exploration Advisor</span>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Main content */}
        <div className="flex-1 overflow-auto p-4">
          {/* Config summary */}
          <div className="mb-4 p-3 bg-muted/50 rounded-lg border border-border">
            <div className="flex items-center gap-2 text-sm text-foreground mb-2">
              <Compass className="w-4 h-4 text-emerald-400" />
              <span className="font-medium">Configuration Summary</span>
            </div>
            {hasConfig ? (
              <div className="flex gap-4 text-xs text-muted-foreground">
                <span>{availableStates.length} states loaded</span>
                <span>{availableTransitions.length} transitions loaded</span>
              </div>
            ) : (
              <div className="flex items-center gap-2 text-xs text-amber-400">
                <AlertTriangle className="w-3.5 h-3.5" />
                <span>
                  No configuration loaded. Load a config file first for targeted suggestions.
                </span>
              </div>
            )}
          </div>

          {!suggestion ? (
            <>
              {/* Goal input */}
              <div className="mb-4">
                <label className="block text-sm font-medium text-foreground mb-2">
                  What's your exploration goal?
                </label>
                <textarea
                  value={goal}
                  onChange={(e) => setGoal(e.target.value)}
                  placeholder="Example: Test all login-related paths thoroughly, including error states and edge cases"
                  className="w-full px-3 py-2 bg-muted border border-border rounded-lg text-sm text-foreground placeholder:text-muted-foreground resize-none focus:outline-hidden focus:border-emerald-500 min-h-[100px]"
                />
              </div>

              {/* Example goals */}
              <div className="mb-4">
                <label className="block text-xs text-muted-foreground mb-2">Example goals</label>
                <div className="flex flex-wrap gap-2">
                  {[
                    "Test all login-related paths",
                    "Quick smoke test of critical flows",
                    "Verify checkout process end-to-end",
                    "Explore all error handling states",
                    "Regression test recent changes",
                  ].map((example) => (
                    <button
                      key={example}
                      onClick={() => setGoal(example)}
                      className="px-2 py-1 text-xs bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                    >
                      {example}
                    </button>
                  ))}
                </div>
              </div>

              {/* Error display */}
              {error && (
                <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded">
                  <p className="text-sm text-red-300">{error}</p>
                </div>
              )}
            </>
          ) : (
            <>
              {/* Suggestion display */}
              <div className="space-y-4">
                {/* Strategy */}
                <div>
                  <label className="block text-xs text-muted-foreground mb-1">
                    Recommended Strategy
                  </label>
                  <div className="p-3 bg-emerald-900/20 border border-emerald-700 rounded-lg">
                    <div className="flex items-center gap-2">
                      <Target className="w-4 h-4 text-emerald-400" />
                      <span className="font-medium text-emerald-300">
                        {STRATEGY_INFO[suggestion.strategy].name}
                      </span>
                    </div>
                    <p className="text-xs text-muted-foreground mt-1">
                      {STRATEGY_INFO[suggestion.strategy].description}
                    </p>
                  </div>
                </div>

                {/* Rationale */}
                <div>
                  <label className="block text-xs text-muted-foreground mb-1">Rationale</label>
                  <div className="p-3 bg-muted rounded-lg">
                    <p className="text-sm text-foreground">{suggestion.rationale}</p>
                  </div>
                </div>

                {/* Target states */}
                {suggestion.strategy === "targeted" && suggestion.target_state_ids.length > 0 && (
                  <div>
                    <label className="block text-xs text-muted-foreground mb-1">
                      Target States ({suggestion.target_state_ids.length})
                    </label>
                    <div className="flex flex-wrap gap-1">
                      {suggestion.target_state_ids.map((stateId) => {
                        const state = availableStates.find((s) => s.id === stateId);
                        return (
                          <span
                            key={stateId}
                            className="px-2 py-1 text-xs bg-emerald-900/30 text-emerald-300 rounded"
                          >
                            {state?.name || stateId}
                          </span>
                        );
                      })}
                    </div>
                  </div>
                )}

                {/* Target transitions */}
                {suggestion.strategy === "targeted" &&
                  suggestion.target_transition_ids.length > 0 && (
                    <div>
                      <label className="block text-xs text-muted-foreground mb-1">
                        Target Transitions ({suggestion.target_transition_ids.length})
                      </label>
                      <div className="flex flex-wrap gap-1">
                        {suggestion.target_transition_ids.map((transitionId) => {
                          const transition = availableTransitions.find(
                            (t) => t.id === transitionId,
                          );
                          return (
                            <span
                              key={transitionId}
                              className="px-2 py-1 text-xs bg-blue-900/30 text-blue-300 rounded"
                            >
                              {transition?.name || transitionId}
                            </span>
                          );
                        })}
                      </div>
                    </div>
                  )}

                {/* Limits */}
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="block text-xs text-muted-foreground mb-1">Max States</label>
                    <div className="p-2 bg-muted rounded">
                      <span className="text-sm text-foreground">
                        {suggestion.max_states === 0 ? "Unlimited" : suggestion.max_states}
                      </span>
                    </div>
                  </div>
                  <div>
                    <label className="block text-xs text-muted-foreground mb-1">Max Duration</label>
                    <div className="p-2 bg-muted rounded">
                      <span className="text-sm text-foreground">
                        {suggestion.max_duration_seconds === 0
                          ? "Unlimited"
                          : `${suggestion.max_duration_seconds} seconds`}
                      </span>
                    </div>
                  </div>
                </div>
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 p-4 border-t border-border">
          {!suggestion ? (
            <>
              <button
                onClick={onClose}
                className="px-4 py-2 text-sm bg-muted/80 text-white rounded-lg hover:bg-muted transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleGenerate}
                disabled={isGenerating || !goal.trim()}
                className="flex items-center gap-2 px-4 py-2 text-sm bg-emerald-600 text-white rounded-lg hover:bg-emerald-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isGenerating ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Analyzing...
                  </>
                ) : (
                  <>
                    <Wand2 className="w-4 h-4" />
                    Get Suggestions
                  </>
                )}
              </button>
            </>
          ) : (
            <>
              <button
                onClick={() => setSuggestion(null)}
                className="px-4 py-2 text-sm bg-muted/80 text-white rounded-lg hover:bg-muted transition-colors"
              >
                Try Again
              </button>
              <button
                onClick={handleAccept}
                className="flex items-center gap-2 px-4 py-2 text-sm bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
              >
                <Check className="w-4 h-4" />
                Apply Settings
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

export default AiExplorationAdvisor;
