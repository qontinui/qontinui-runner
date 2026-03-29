import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, CheckCircle2, RefreshCw, XCircle } from "lucide-react";
import {
  fetchApi,
  type WorkflowEvaluation,
  type StepEvaluation,
  type EvaluationDimension,
  type CoverageEntry,
} from "./types";

// ============================================================================
// Dimension Heatmap Cell
// ============================================================================

const DIMENSION_ORDER: EvaluationDimension[] = [
  "entailment",
  "executability",
  "specificity",
  "determinism",
  "coverage",
  "robustness",
];

const DIMENSION_LABELS: Record<EvaluationDimension, string> = {
  entailment: "Ent",
  executability: "Exe",
  specificity: "Spe",
  determinism: "Det",
  coverage: "Cov",
  robustness: "Rob",
};

function scoreColor(score: number): string {
  if (score >= 0.8) return "bg-green-500/30 text-green-300";
  if (score >= 0.6) return "bg-yellow-500/25 text-yellow-300";
  if (score >= 0.4) return "bg-orange-500/25 text-orange-300";
  return "bg-red-500/30 text-red-300";
}

function ScoreCell({ score, title }: { score: number; title?: string }) {
  return (
    <td
      className={`px-2 py-1 text-center text-xs font-mono ${scoreColor(score)}`}
      title={title}
    >
      {score.toFixed(2)}
    </td>
  );
}

// ============================================================================
// Step Row
// ============================================================================

function StepRow({
  step,
  expanded,
  onToggle,
}: {
  step: StepEvaluation;
  expanded: boolean;
  onToggle: () => void;
}) {
  const scoreMap = new Map(step.scores.map((s) => [s.dimension, s]));

  return (
    <>
      <tr
        className="border-b border-border/50 hover:bg-muted/30 cursor-pointer"
        onClick={onToggle}
      >
        <td className="px-2 py-1.5 text-sm max-w-[200px] truncate" title={step.step_name}>
          {step.step_name || step.step_id}
        </td>
        <td className="px-2 py-1 text-xs text-muted-foreground">
          {step.criterion_id || "—"}
        </td>
        {DIMENSION_ORDER.map((dim) => {
          const ds = scoreMap.get(dim);
          return ds ? (
            <ScoreCell
              key={dim}
              score={ds.score}
              title={ds.explanation || undefined}
            />
          ) : (
            <td key={dim} className="px-2 py-1 text-center text-xs text-muted-foreground">
              —
            </td>
          );
        })}
        <ScoreCell score={step.composite_score} title="Weighted composite" />
        <ScoreCell score={step.min_score} title="Min-form (weakest dimension)" />
        <td className="px-2 py-1 text-center">
          {step.flags.length > 0 && (
            <span className="text-xs text-red-400" title={step.flags.map((f) => f.message).join("; ")}>
              {step.flags.length}
            </span>
          )}
        </td>
      </tr>
      {expanded && (
        <tr className="border-b border-border/50 bg-muted/10">
          <td colSpan={11} className="px-4 py-2">
            <div className="grid grid-cols-2 gap-3 text-xs">
              {step.scores.map((ds) => (
                <div key={ds.dimension} className="flex flex-col gap-0.5">
                  <div className="font-medium capitalize">{ds.dimension}</div>
                  <div className="text-muted-foreground">
                    Score: {ds.score.toFixed(2)} (conf: {ds.confidence.toFixed(2)}, tier: {ds.tier})
                  </div>
                  {ds.explanation && (
                    <div className="text-muted-foreground italic">{ds.explanation}</div>
                  )}
                  {ds.evidence.length > 0 && (
                    <ul className="list-disc list-inside text-muted-foreground">
                      {ds.evidence.map((e, i) => (
                        <li key={i}>{e}</li>
                      ))}
                    </ul>
                  )}
                </div>
              ))}
              {step.flags.length > 0 && (
                <div className="col-span-2 mt-1">
                  <div className="font-medium text-red-400">Flags</div>
                  {step.flags.map((f, i) => (
                    <div key={i} className="text-red-300/80">
                      [{f.flag_type}] {f.message}
                    </div>
                  ))}
                </div>
              )}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

// ============================================================================
// Coverage Matrix
// ============================================================================

function CoverageSection({ entries }: { entries: CoverageEntry[] }) {
  if (entries.length === 0) return null;

  return (
    <div className="mt-4">
      <h3 className="text-sm font-semibold mb-2">Semantic Coverage Matrix</h3>
      <div className="space-y-1.5">
        {entries.map((entry) => (
          <div
            key={entry.criterion_id}
            className={`flex items-center gap-2 text-xs px-2 py-1 rounded ${
              entry.coverage_adequate
                ? "bg-green-500/10 border border-green-500/20"
                : "bg-red-500/10 border border-red-500/20"
            }`}
          >
            {entry.coverage_adequate ? (
              <CheckCircle2 className="w-3.5 h-3.5 text-green-400 shrink-0" />
            ) : (
              <XCircle className="w-3.5 h-3.5 text-red-400 shrink-0" />
            )}
            <span className="font-medium min-w-[80px]">{entry.criterion_id}</span>
            <span className="text-muted-foreground flex-1 truncate">
              {entry.criterion_description}
            </span>
            <span
              className={`font-mono ${scoreColor(entry.best_entailment)} px-1 rounded`}
            >
              {entry.best_entailment.toFixed(2)}
            </span>
            <span className="text-muted-foreground capitalize text-[10px]">
              {entry.criterion_priority}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ============================================================================
// Main Tab Component
// ============================================================================

export function StepEvaluationTab() {
  const [evaluation, setEvaluation] = useState<WorkflowEvaluation | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedStep, setExpandedStep] = useState<string | null>(null);
  const [workflowJson, setWorkflowJson] = useState("");

  const runEvaluation = useCallback(async () => {
    if (!workflowJson.trim()) {
      setError("Paste a workflow JSON to evaluate");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const workflow = JSON.parse(workflowJson);
      const result = await fetchApi<WorkflowEvaluation>("/evaluation/workflow", {
        method: "POST",
        body: JSON.stringify({
          workflow,
          strategy: "fast_only",
        }),
      });
      setEvaluation(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Evaluation failed");
    } finally {
      setLoading(false);
    }
  }, [workflowJson]);

  // Auto-load from most recent pipeline artifact
  useEffect(() => {
    (async () => {
      try {
        const artifacts = await fetchApi<{ id: string; workflow_id: string }[]>(
          "/generator-eval/artifacts?limit=1"
        );
        if (artifacts.length > 0) {
          const artifact = await fetchApi<{ final_json?: unknown }>(
            `/generator-eval/artifacts/${artifacts[0].id}`
          );
          if (artifact.final_json) {
            setWorkflowJson(JSON.stringify(artifact.final_json, null, 2));
          }
        }
      } catch {
        // Silent — user can paste manually
      }
    })();
  }, []);

  return (
    <div className="flex flex-col gap-4">
      {/* Input */}
      <div className="flex gap-2 items-start">
        <textarea
          className="flex-1 min-h-[80px] max-h-[200px] rounded border border-border bg-background px-3 py-2 text-xs font-mono resize-y"
          placeholder="Paste workflow JSON here (auto-loads most recent artifact)"
          value={workflowJson}
          onChange={(e) => setWorkflowJson(e.target.value)}
        />
        <button
          className="flex items-center gap-1.5 px-3 py-2 rounded bg-purple-600 hover:bg-purple-500 text-white text-sm disabled:opacity-50"
          onClick={runEvaluation}
          disabled={loading}
        >
          {loading ? (
            <RefreshCw className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <RefreshCw className="w-3.5 h-3.5" />
          )}
          Evaluate
        </button>
      </div>

      {error && (
        <div className="flex items-center gap-2 text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">
          <AlertTriangle className="w-4 h-4 shrink-0" />
          {error}
        </div>
      )}

      {evaluation && (
        <>
          {/* Summary */}
          <div className="grid grid-cols-4 gap-3">
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Overall Score</div>
              <div className={`text-xl font-semibold ${evaluation.overall_score >= 0.6 ? "text-green-400" : "text-red-400"}`}>
                {evaluation.overall_score.toFixed(3)}
              </div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Quality Gate</div>
              <div className={`text-xl font-semibold ${evaluation.quality_gate.passed ? "text-green-400" : "text-red-400"}`}>
                {evaluation.quality_gate.passed ? "PASS" : "FAIL"}
              </div>
              <div className="text-xs text-muted-foreground capitalize">
                {evaluation.quality_gate.gate_level}
              </div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Steps Evaluated</div>
              <div className="text-xl font-semibold">{evaluation.step_evaluations.length}</div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Coverage</div>
              <div className={`text-xl font-semibold ${evaluation.coverage_matrix.overall_coverage_score >= 0.6 ? "text-green-400" : "text-yellow-400"}`}>
                {(evaluation.coverage_matrix.overall_coverage_score * 100).toFixed(0)}%
              </div>
            </div>
          </div>

          {/* Gate Failures */}
          {evaluation.quality_gate.failures.length > 0 && (
            <div className="rounded border border-red-500/20 bg-red-500/5 p-3">
              <div className="text-sm font-medium text-red-400 mb-1">
                Quality Gate Failures
              </div>
              {evaluation.quality_gate.failures.map((f, i) => (
                <div key={i} className="text-xs text-red-300/80">
                  [{f.gate_level}] {f.reason}
                  {f.step_id && <span className="text-muted-foreground"> (step: {f.step_id})</span>}
                </div>
              ))}
            </div>
          )}

          {/* Step Heatmap */}
          <div className="overflow-x-auto">
            <table className="w-full text-left">
              <thead>
                <tr className="border-b border-border text-xs text-muted-foreground">
                  <th className="px-2 py-1.5 font-medium">Step</th>
                  <th className="px-2 py-1 font-medium">Criterion</th>
                  {DIMENSION_ORDER.map((dim) => (
                    <th key={dim} className="px-2 py-1 text-center font-medium" title={dim}>
                      {DIMENSION_LABELS[dim]}
                    </th>
                  ))}
                  <th className="px-2 py-1 text-center font-medium">Comp</th>
                  <th className="px-2 py-1 text-center font-medium">Min</th>
                  <th className="px-2 py-1 text-center font-medium">Flags</th>
                </tr>
              </thead>
              <tbody>
                {evaluation.step_evaluations.map((step) => (
                  <StepRow
                    key={step.step_id}
                    step={step}
                    expanded={expandedStep === step.step_id}
                    onToggle={() =>
                      setExpandedStep(expandedStep === step.step_id ? null : step.step_id)
                    }
                  />
                ))}
              </tbody>
            </table>
          </div>

          {/* Coverage Matrix */}
          <CoverageSection entries={evaluation.coverage_matrix.entries} />

          {/* Uncovered Criteria */}
          {evaluation.coverage_matrix.uncovered_criteria.length > 0 && (
            <div className="mt-2">
              <h3 className="text-sm font-semibold text-red-400 mb-1">
                Uncovered Criteria ({evaluation.coverage_matrix.uncovered_criteria.length})
              </h3>
              {evaluation.coverage_matrix.uncovered_criteria.map((uc) => (
                <div
                  key={uc.criterion_id}
                  className="text-xs text-muted-foreground flex gap-2 py-0.5"
                >
                  <span className="font-medium text-red-300">{uc.criterion_id}</span>
                  <span>{uc.criterion_description}</span>
                  <span className="italic">({uc.suggested_verification})</span>
                </div>
              ))}
            </div>
          )}

          <div className="text-xs text-muted-foreground text-right">
            Strategy: {evaluation.scoring_strategy} | Duration: {evaluation.evaluation_duration_ms}ms
          </div>
        </>
      )}
    </div>
  );
}
