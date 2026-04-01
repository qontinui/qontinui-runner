/**
 * LiveEvaluationPanel
 *
 * On-demand LLM-as-judge evaluation panel. Lets users provide input, output,
 * and optional context, select an eval spec (saved or inline JSON), and run
 * the `evaluate_with_io` Tauri command. Displays assertion results with
 * pass/fail badges, scores, and reasons.
 */

import { useState, useCallback } from "react";
import { Play, RefreshCw, CheckCircle2, XCircle, AlertTriangle, FileJson } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { Card, CardContent } from "@/components/ui/Card";
import { useLiveEvaluation, type AssertionResult } from "@/hooks/useLiveEvaluation";

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function ResultBadge({ result }: { result: AssertionResult }) {
  return (
    <div className="flex items-start gap-3 p-3 rounded-lg bg-muted/30 border border-border/30">
      <div className="mt-0.5">
        {result.passed ? (
          <CheckCircle2 className="w-4 h-4 text-emerald-500" />
        ) : (
          <XCircle className="w-4 h-4 text-red-500" />
        )}
      </div>
      <div className="flex-1 min-w-0 space-y-1">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">{result.assertion_type}</span>
          <Badge variant={result.passed ? "success" : "danger"} size="sm">
            {result.passed ? "Pass" : "Fail"}
          </Badge>
          <span className="text-xs text-muted-foreground ml-auto">
            {(result.actual_value * 100).toFixed(0)}% / {(result.threshold_value * 100).toFixed(0)}%
          </span>
        </div>
        {result.message && (
          <p className="text-xs text-muted-foreground leading-relaxed">{result.message}</p>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

interface LiveEvaluationPanelProps {
  /** Pre-fill input from experiment result */
  prefillInput?: string;
  /** Pre-fill output from experiment result */
  prefillOutput?: string;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export default function LiveEvaluationPanel({
  prefillInput = "",
  prefillOutput = "",
}: LiveEvaluationPanelProps) {
  const { evaluate, results, loading, error, reset, specs, specsLoading } = useLiveEvaluation();

  // Form state
  const [input, setInput] = useState(prefillInput);
  const [output, setOutput] = useState(prefillOutput);
  const [context, setContext] = useState("");
  const [specMode, setSpecMode] = useState<"saved" | "inline">("saved");
  const [selectedSpecId, setSelectedSpecId] = useState<string>("");
  const [inlineSpec, setInlineSpec] = useState("{}");

  const handleEvaluate = useCallback(() => {
    let specJson: string;
    if (specMode === "saved") {
      const spec = specs.find((s) => s.id === selectedSpecId);
      if (!spec) return;
      specJson = JSON.stringify(spec);
    } else {
      specJson = inlineSpec;
    }
    evaluate({ specJson, input, output, context: context || undefined });
  }, [specMode, selectedSpecId, specs, inlineSpec, input, output, context, evaluate]);

  const handleReset = useCallback(() => {
    reset();
    setInput("");
    setOutput("");
    setContext("");
    setInlineSpec("{}");
  }, [reset]);

  const canEvaluate =
    input.trim().length > 0 &&
    output.trim().length > 0 &&
    (specMode === "inline" || selectedSpecId) &&
    !loading;

  const passCount = results?.filter((r) => r.passed).length ?? 0;
  const totalCount = results?.length ?? 0;

  return (
    <div className="flex flex-col h-full" data-tutorial-id="live-evaluation-panel">
      {/* Header */}
      <div className="px-4 py-3 border-b border-border/50 shrink-0">
        <h3 className="text-sm font-semibold flex items-center gap-2">
          <Play className="w-4 h-4" />
          Run Evaluation
        </h3>
        <p className="text-xs text-muted-foreground mt-0.5">
          Evaluate an input/output pair using LLM-as-judge assertions
        </p>
      </div>

      <div className="flex-1 overflow-auto p-4 space-y-4">
        {/* Eval Spec selector */}
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <label className="text-xs font-medium text-muted-foreground">Eval Spec</label>
            <div className="flex rounded-md overflow-hidden border border-border text-xs ml-auto">
              <button
                className={`px-2.5 py-1 transition-colors ${
                  specMode === "saved"
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/30 hover:bg-muted/50"
                }`}
                onClick={() => setSpecMode("saved")}
              >
                Saved
              </button>
              <button
                className={`px-2.5 py-1 transition-colors ${
                  specMode === "inline"
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/30 hover:bg-muted/50"
                }`}
                onClick={() => setSpecMode("inline")}
              >
                Inline JSON
              </button>
            </div>
          </div>

          {specMode === "saved" ? (
            <select
              className="w-full rounded-md bg-background border border-border px-3 py-1.5 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
              value={selectedSpecId}
              onChange={(e) => setSelectedSpecId(e.target.value)}
              disabled={specsLoading}
            >
              <option value="">
                {specsLoading ? "Loading specs..." : "Select an eval spec..."}
              </option>
              {specs.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                  {s.target_agent ? ` (${s.target_agent})` : ""}
                </option>
              ))}
            </select>
          ) : (
            <div className="relative">
              <FileJson className="absolute top-2 left-2 w-3.5 h-3.5 text-muted-foreground" />
              <textarea
                className="w-full rounded-md bg-background border border-border pl-8 pr-3 py-2 text-xs font-mono focus:outline-hidden focus:ring-2 focus:ring-primary resize-y min-h-[80px]"
                placeholder='{"id":"...","name":"...","test_cases":[...],"thresholds":{...}}'
                value={inlineSpec}
                onChange={(e) => setInlineSpec(e.target.value)}
                rows={4}
              />
            </div>
          )}
        </div>

        {/* Input */}
        <div className="space-y-1">
          <label className="text-xs font-medium text-muted-foreground">
            Input (prompt / question)
          </label>
          <textarea
            className="w-full rounded-md bg-background border border-border px-3 py-2 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary resize-y min-h-[60px]"
            placeholder="Enter the prompt or question that was sent to the LLM..."
            value={input}
            onChange={(e) => setInput(e.target.value)}
            rows={3}
          />
        </div>

        {/* Output */}
        <div className="space-y-1">
          <label className="text-xs font-medium text-muted-foreground">Output (LLM response)</label>
          <textarea
            className="w-full rounded-md bg-background border border-border px-3 py-2 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary resize-y min-h-[60px]"
            placeholder="Enter the LLM response to evaluate..."
            value={output}
            onChange={(e) => setOutput(e.target.value)}
            rows={3}
          />
        </div>

        {/* Context (optional) */}
        <div className="space-y-1">
          <label className="text-xs font-medium text-muted-foreground">
            Context <span className="text-muted-foreground/60">(optional, for RAG)</span>
          </label>
          <textarea
            className="w-full rounded-md bg-background border border-border px-3 py-2 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary resize-y min-h-[40px]"
            placeholder="Provide grounding context if evaluating RAG responses..."
            value={context}
            onChange={(e) => setContext(e.target.value)}
            rows={2}
          />
        </div>

        {/* Actions */}
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            onClick={handleEvaluate}
            disabled={!canEvaluate}
            data-tutorial-id="live-evaluation-run"
          >
            {loading ? (
              <>
                <RefreshCw className="w-3.5 h-3.5 animate-spin" />
                Evaluating...
              </>
            ) : (
              <>
                <Play className="w-3.5 h-3.5" />
                Evaluate
              </>
            )}
          </Button>
          {results && (
            <Button size="sm" variant="ghost" onClick={handleReset}>
              Reset
            </Button>
          )}
        </div>

        {/* Error */}
        {error && (
          <div className="flex items-start gap-2 p-3 rounded-lg bg-red-500/10 border border-red-500/20 text-sm text-red-400">
            <AlertTriangle className="w-4 h-4 mt-0.5 shrink-0" />
            <div>
              <p className="font-medium">Evaluation failed</p>
              <p className="text-xs mt-1 opacity-80">{error}</p>
            </div>
          </div>
        )}

        {/* Results */}
        {results && results.length > 0 && (
          <div className="space-y-3">
            {/* Summary */}
            <Card className="!p-0">
              <CardContent className="!px-3 !py-2">
                <div className="flex items-center justify-between">
                  <span className="text-xs font-medium text-muted-foreground">Results</span>
                  <Badge
                    variant={
                      passCount === totalCount ? "success" : passCount === 0 ? "danger" : "warning"
                    }
                    size="sm"
                  >
                    {passCount}/{totalCount} passed
                  </Badge>
                </div>
              </CardContent>
            </Card>

            {/* Individual results */}
            <div className="space-y-2">
              {results.map((r, i) => (
                <ResultBadge key={`${r.assertion_type}-${i}`} result={r} />
              ))}
            </div>
          </div>
        )}

        {results && results.length === 0 && (
          <p className="text-xs text-muted-foreground text-center py-4">
            No assertions were evaluated. Check your eval spec configuration.
          </p>
        )}
      </div>
    </div>
  );
}
