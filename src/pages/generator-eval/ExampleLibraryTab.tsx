import { useEffect, useState } from "react";
import { RefreshCw, BookOpen } from "lucide-react";
import { fetchApi, type ExampleWorkflow } from "./types";

const STATUS_OPTIONS = [
  { value: "active", label: "Active", color: "bg-green-500/20 text-green-400" },
  { value: "excluded", label: "Excluded", color: "bg-red-500/20 text-red-400" },
  { value: "", label: "None", color: "bg-gray-500/20 text-gray-400" },
];

export function ExampleLibraryTab() {
  const [examples, setExamples] = useState<ExampleWorkflow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await fetchApi<ExampleWorkflow[]>("/generator-eval/examples");
      setExamples(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const updateStatus = async (workflowId: string, status: string | null) => {
    try {
      await fetchApi(`/generator-eval/examples/${workflowId}`, {
        method: "PUT",
        body: JSON.stringify({
          example_status: status || null,
        }),
      });
      // Update local state
      if (status) {
        setExamples((prev) =>
          prev.map((e) => (e.id === workflowId ? { ...e, example_status: status } : e)),
        );
      } else {
        // Removed status — remove from list
        setExamples((prev) => prev.filter((e) => e.id !== workflowId));
      }
    } catch {
      // ignore
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading examples...
      </div>
    );
  }
  if (error) {
    return (
      <div className="text-red-400 p-4">
        Error: {error}{" "}
        <button onClick={loadData} className="underline ml-2">
          Retry
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-sm font-semibold flex items-center gap-1.5">
            <BookOpen className="w-3.5 h-3.5" />
            Example Library ({examples.length})
          </h2>
          <p className="text-xs text-muted-foreground mt-0.5">
            Curate ground truth workflows used as RAG examples during generation. Set example_status
            on workflows in the Workflow Builder.
          </p>
        </div>
        <button
          onClick={loadData}
          className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          <RefreshCw className="w-3 h-3" />
          Refresh
        </button>
      </div>

      {examples.length === 0 ? (
        <div className="text-sm text-muted-foreground text-center py-8">
          No example workflows yet. Mark workflows as examples in the Workflow Builder to see them
          here.
        </div>
      ) : (
        <div className="space-y-2">
          {examples.map((ex) => {
            const statusOption = STATUS_OPTIONS.find((s) => s.value === (ex.example_status || ""));
            return (
              <div
                key={ex.id}
                className="flex items-center gap-3 px-3 py-2 border border-border rounded-lg bg-card"
              >
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium">{ex.name}</div>
                  <div className="text-xs text-muted-foreground truncate">{ex.description}</div>
                  <div className="text-[10px] text-muted-foreground mt-0.5">
                    {ex.category} · {new Date(ex.created_at).toLocaleDateString()}
                  </div>
                </div>

                {/* Status badge */}
                <span
                  className={`px-1.5 py-0.5 rounded text-[10px] font-medium shrink-0 ${
                    statusOption?.color || "bg-gray-500/20 text-gray-400"
                  }`}
                >
                  {ex.example_status || "none"}
                </span>

                {/* Status dropdown */}
                <select
                  value={ex.example_status || ""}
                  onChange={(e) => updateStatus(ex.id, e.target.value || null)}
                  className="text-xs bg-background border border-border rounded px-1.5 py-0.5 shrink-0"
                >
                  {STATUS_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
