import { useEffect, useState } from "react";
import {
  Play,
  Plus,
  Trash2,
  CheckCircle2,
  XCircle,
  ChevronDown,
  ChevronRight,
  Loader2,
} from "lucide-react";
import { fetchApi, type Benchmark, type BenchmarkResult, type ExpectedStructure } from "./types";

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString();
}

function ScoreBadge({ score }: { score: number | null }) {
  if (score == null) return <span className="text-muted-foreground">—</span>;
  const pct = (score * 100).toFixed(0);
  const color = score >= 0.8 ? "text-green-400" : score >= 0.5 ? "text-yellow-400" : "text-red-400";
  return <span className={`font-mono ${color}`}>{pct}%</span>;
}

function CreateBenchmarkDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (b: Benchmark) => void;
}) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState("");
  const [keywords, setKeywords] = useState("");
  const [stepTypes, setStepTypes] = useState("command,prompt");
  const [checkTypes, setCheckTypes] = useState("");
  const [saving, setSaving] = useState(false);

  const handleCreate = async () => {
    if (!name || !description) return;
    setSaving(true);
    try {
      const expected: ExpectedStructure = {
        has_setup: true,
        has_verification: true,
        has_agentic: true,
        expected_step_types: stepTypes
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        keywords: keywords
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        expected_check_types: checkTypes
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        expected_test_types: [],
      };
      const benchmark = await fetchApi<Benchmark>("/generator-eval/benchmarks", {
        method: "POST",
        body: JSON.stringify({
          name,
          description,
          category: category || undefined,
          expected_structure: expected,
        }),
      });
      onCreated(benchmark);
    } catch {
      // ignore
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-card border border-border rounded-lg p-4 w-[480px] max-h-[90vh] overflow-auto">
        <h2 className="text-sm font-semibold mb-3">Create Benchmark</h2>
        <div className="space-y-2">
          <div>
            <label className="text-xs text-muted-foreground">Name</label>
            <input
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. Basic Lint Workflow"
            />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">
              Description (the prompt to generate)
            </label>
            <textarea
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5 h-20"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="e.g. Create a workflow that runs ESLint on the frontend..."
            />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">Category</label>
            <input
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5"
              value={category}
              onChange={(e) => setCategory(e.target.value)}
              placeholder="e.g. testing"
            />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">
              Expected keywords (comma-separated)
            </label>
            <input
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5"
              value={keywords}
              onChange={(e) => setKeywords(e.target.value)}
              placeholder="e.g. eslint, lint, npm"
            />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">
              Expected step types (comma-separated)
            </label>
            <input
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5"
              value={stepTypes}
              onChange={(e) => setStepTypes(e.target.value)}
              placeholder="command,prompt"
            />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">
              Expected check types (comma-separated)
            </label>
            <input
              className="w-full bg-background border border-border rounded px-2 py-1 text-sm mt-0.5"
              value={checkTypes}
              onChange={(e) => setCheckTypes(e.target.value)}
              placeholder="e.g. lint, typecheck"
            />
          </div>
        </div>
        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onClose}
            className="px-3 py-1 text-sm rounded border border-border hover:bg-muted transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!name || !description || saving}
            className="px-3 py-1 text-sm rounded bg-purple-600 hover:bg-purple-700 text-white disabled:opacity-50 transition-colors"
          >
            {saving ? "Creating..." : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}

function BenchmarkRow({
  benchmark,
  onDelete,
  onRun,
}: {
  benchmark: Benchmark;
  onDelete: (id: string) => void;
  onRun: (id: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [results, setResults] = useState<BenchmarkResult[]>([]);
  const [running, setRunning] = useState(false);
  const [loadingResults, setLoadingResults] = useState(false);

  const toggleExpand = async () => {
    if (!expanded && results.length === 0) {
      setLoadingResults(true);
      try {
        const r = await fetchApi<BenchmarkResult[]>(
          `/generator-eval/benchmarks/${benchmark.id}/results?limit=10`,
        );
        setResults(r);
      } catch {
        // ignore
      } finally {
        setLoadingResults(false);
      }
    }
    setExpanded(!expanded);
  };

  const handleRun = async () => {
    setRunning(true);
    try {
      const result = await fetchApi<BenchmarkResult>(
        `/generator-eval/benchmarks/${benchmark.id}/run`,
        { method: "POST" },
      );
      setResults((prev) => [result, ...prev]);
      onRun(benchmark.id);
    } catch {
      // ignore
    } finally {
      setRunning(false);
    }
  };

  const latestResult = results[0];

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <div className="flex items-center gap-2 px-3 py-2 bg-card">
        <button onClick={toggleExpand} className="shrink-0">
          {expanded ? (
            <ChevronDown className="w-3.5 h-3.5" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" />
          )}
        </button>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium">{benchmark.name}</div>
          <div className="text-[10px] text-muted-foreground truncate">
            {benchmark.description.slice(0, 80)}
          </div>
        </div>
        {latestResult && (
          <div className="flex items-center gap-1.5 shrink-0">
            {latestResult.passed ? (
              <CheckCircle2 className="w-3.5 h-3.5 text-green-500" />
            ) : (
              <XCircle className="w-3.5 h-3.5 text-red-500" />
            )}
            <ScoreBadge score={latestResult.overall_score} />
          </div>
        )}
        <button
          onClick={handleRun}
          disabled={running}
          className="shrink-0 px-2 py-1 text-xs rounded bg-purple-600 hover:bg-purple-700 text-white disabled:opacity-50 flex items-center gap-1 transition-colors"
        >
          {running ? <Loader2 className="w-3 h-3 animate-spin" /> : <Play className="w-3 h-3" />}
          Run
        </button>
        <button
          onClick={() => onDelete(benchmark.id)}
          className="shrink-0 p-1 text-muted-foreground hover:text-red-400 transition-colors"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>

      {expanded && (
        <div className="border-t border-border px-3 py-2">
          <div className="text-xs text-muted-foreground mb-1">
            Expected: {benchmark.expected_structure.expected_step_types?.join(", ") || "any"} steps,
            keywords: {benchmark.expected_structure.keywords?.join(", ") || "none"}
          </div>

          {loadingResults ? (
            <div className="text-xs text-muted-foreground">Loading results...</div>
          ) : results.length === 0 ? (
            <div className="text-xs text-muted-foreground">
              No results yet. Click Run to execute this benchmark.
            </div>
          ) : (
            <table className="w-full text-xs mt-2">
              <thead>
                <tr className="text-muted-foreground">
                  <th className="text-left py-1">Date</th>
                  <th className="text-right py-1">Structure</th>
                  <th className="text-right py-1">Content</th>
                  <th className="text-right py-1">Types</th>
                  <th className="text-right py-1">Overall</th>
                  <th className="text-right py-1">Pass</th>
                </tr>
              </thead>
              <tbody>
                {results.map((r) => (
                  <tr key={r.id} className="border-t border-border/50">
                    <td className="py-1">{formatDate(r.run_at).split(",")[0]}</td>
                    <td className="text-right py-1">
                      <ScoreBadge score={r.structure_score} />
                    </td>
                    <td className="text-right py-1">
                      <ScoreBadge score={r.content_score} />
                    </td>
                    <td className="text-right py-1">
                      <ScoreBadge score={r.step_type_score} />
                    </td>
                    <td className="text-right py-1">
                      <ScoreBadge score={r.overall_score} />
                    </td>
                    <td className="text-right py-1">
                      {r.passed ? (
                        <CheckCircle2 className="w-3 h-3 text-green-500 inline" />
                      ) : (
                        <XCircle className="w-3 h-3 text-red-500 inline" />
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}

export function BenchmarksTab() {
  const [benchmarks, setBenchmarks] = useState<Benchmark[]>([]);
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);

  useEffect(() => {
    fetchApi<Benchmark[]>("/generator-eval/benchmarks")
      .then(setBenchmarks)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const handleDelete = async (id: string) => {
    try {
      await fetchApi(`/generator-eval/benchmarks/${id}`, {
        method: "DELETE",
      });
      setBenchmarks((prev) => prev.filter((b) => b.id !== id));
    } catch {
      // ignore
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading benchmarks...
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <h2 className="text-sm font-semibold">Benchmark Suite ({benchmarks.length})</h2>
        <button
          onClick={() => setShowCreate(true)}
          className="flex items-center gap-1.5 px-3 py-1 text-xs rounded bg-purple-600 hover:bg-purple-700 text-white transition-colors"
        >
          <Plus className="w-3 h-3" />
          New Benchmark
        </button>
      </div>

      {benchmarks.length === 0 ? (
        <div className="text-sm text-muted-foreground text-center py-8">
          No benchmarks yet. Create one to start testing generation quality.
        </div>
      ) : (
        <div className="space-y-2">
          {benchmarks.map((b) => (
            <BenchmarkRow key={b.id} benchmark={b} onDelete={handleDelete} onRun={() => {}} />
          ))}
        </div>
      )}

      {showCreate && (
        <CreateBenchmarkDialog
          onClose={() => setShowCreate(false)}
          onCreated={(b) => {
            setBenchmarks((prev) => [b, ...prev]);
            setShowCreate(false);
          }}
        />
      )}
    </div>
  );
}
