import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, CheckCircle2, XCircle, Clock } from "lucide-react";
import { fetchApi, type PipelineArtifactSummary, type PipelineArtifact } from "./types";

function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString();
}

/** Collapsible section for each pipeline stage */
function StageCard({
  title,
  duration,
  defaultOpen,
  children,
}: {
  title: string;
  duration?: number | null;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen ?? false);
  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <button
        onClick={() => setOpen(!open)}
        className="w-full flex items-center gap-2 px-3 py-2 bg-card hover:bg-muted/50 transition-colors text-left"
      >
        {open ? (
          <ChevronDown className="w-3.5 h-3.5 shrink-0" />
        ) : (
          <ChevronRight className="w-3.5 h-3.5 shrink-0" />
        )}
        <span className="text-sm font-medium flex-1">{title}</span>
        {duration != null && (
          <span className="text-xs text-muted-foreground flex items-center gap-1">
            <Clock className="w-3 h-3" />
            {formatDuration(duration)}
          </span>
        )}
      </button>
      {open && <div className="px-3 py-2 border-t border-border">{children}</div>}
    </div>
  );
}

function JsonBlock({ data, maxHeight }: { data: unknown; maxHeight?: string }) {
  if (data == null) return <span className="text-muted-foreground text-xs">null</span>;
  return (
    <pre
      className="text-xs bg-black/30 rounded p-2 overflow-auto"
      style={{ maxHeight: maxHeight ?? "300px" }}
    >
      {typeof data === "string" ? data : JSON.stringify(data, null, 2)}
    </pre>
  );
}

function TimingBar({ artifact }: { artifact: PipelineArtifact }) {
  const stages = [
    { label: "Discovery", ms: artifact.discovery_duration_ms, color: "#06b6d4" },
    { label: "Builder", ms: artifact.builder_duration_ms, color: "#8b5cf6" },
    { label: "Auto-fix", ms: artifact.autofix_duration_ms, color: "#f59e0b" },
    { label: "Verification", ms: artifact.verification_duration_ms, color: "#ef4444" },
    { label: "Hardener", ms: artifact.hardener_duration_ms, color: "#10b981" },
  ].filter((s) => s.ms != null && s.ms > 0);

  const total = stages.reduce((sum, s) => sum + (s.ms ?? 0), 0);
  if (total === 0) return null;

  return (
    <div className="space-y-1">
      <div className="flex h-4 rounded overflow-hidden">
        {stages.map((s) => (
          <div
            key={s.label}
            title={`${s.label}: ${formatDuration(s.ms)}`}
            style={{
              width: `${((s.ms ?? 0) / total) * 100}%`,
              backgroundColor: s.color,
              minWidth: 2,
            }}
          />
        ))}
      </div>
      <div className="flex gap-3 text-[10px] text-muted-foreground flex-wrap">
        {stages.map((s) => (
          <span key={s.label} className="flex items-center gap-1">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: s.color }} />
            {s.label} {formatDuration(s.ms)}
          </span>
        ))}
      </div>
    </div>
  );
}

export function PipelineInspectorTab() {
  const [artifacts, setArtifacts] = useState<PipelineArtifactSummary[]>([]);
  const [selected, setSelected] = useState<PipelineArtifact | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailLoading, setDetailLoading] = useState(false);

  useEffect(() => {
    fetchApi<PipelineArtifactSummary[]>("/generator-eval/artifacts?limit=50")
      .then(setArtifacts)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const loadDetail = async (id: string) => {
    setDetailLoading(true);
    try {
      const detail = await fetchApi<PipelineArtifact>(`/generator-eval/artifacts/${id}`);
      setSelected(detail);
    } catch {
      // ignore
    } finally {
      setDetailLoading(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading artifacts...
      </div>
    );
  }

  if (artifacts.length === 0) {
    return (
      <div className="text-sm text-muted-foreground text-center py-8">
        No generation artifacts yet. Generate a workflow to see pipeline data here.
      </div>
    );
  }

  return (
    <div className="flex gap-4 h-full min-h-0">
      {/* Left: artifact list */}
      <div className="w-72 shrink-0 border border-border rounded-lg overflow-auto">
        {artifacts.map((a) => (
          <button
            key={a.id}
            onClick={() => loadDetail(a.id)}
            className={`w-full text-left px-3 py-2 border-b border-border hover:bg-muted/50 transition-colors ${
              selected?.id === a.id ? "bg-muted/30" : ""
            }`}
          >
            <div className="flex items-center gap-1.5">
              {a.success ? (
                <CheckCircle2 className="w-3 h-3 text-green-500 shrink-0" />
              ) : (
                <XCircle className="w-3 h-3 text-red-500 shrink-0" />
              )}
              <span className="text-xs font-medium truncate flex-1">
                {a.description.slice(0, 60)}
              </span>
            </div>
            <div className="text-[10px] text-muted-foreground mt-0.5 flex gap-2">
              <span>{formatDuration(a.total_duration_ms)}</span>
              <span>{a.verification_iteration_count} iters</span>
              <span>{formatDate(a.created_at).split(",")[0]}</span>
            </div>
          </button>
        ))}
      </div>

      {/* Right: detail view */}
      <div className="flex-1 min-w-0 overflow-auto">
        {detailLoading && <div className="text-muted-foreground text-sm">Loading detail...</div>}
        {!detailLoading && !selected && (
          <div className="text-muted-foreground text-sm text-center py-8">
            Select a generation to inspect its pipeline
          </div>
        )}
        {!detailLoading && selected && (
          <div className="space-y-3">
            {/* Header */}
            <div>
              <div className="text-sm font-medium">{selected.description}</div>
              <div className="text-xs text-muted-foreground">
                {formatDate(selected.created_at)}
                {selected.model_used && ` · ${selected.model_used}`}
                {selected.category && ` · ${selected.category}`}
              </div>
            </div>

            {/* Timing bar */}
            <TimingBar artifact={selected} />

            {/* Pipeline stages */}
            <StageCard title="1. Discovery" duration={selected.discovery_duration_ms}>
              <JsonBlock data={selected.discovery_calls} />
            </StageCard>

            <StageCard title="2. Builder" duration={selected.builder_duration_ms}>
              {selected.builder_raw_output && (
                <div className="mb-2">
                  <div className="text-xs text-muted-foreground mb-1">Raw AI output:</div>
                  <JsonBlock data={selected.builder_raw_output} />
                </div>
              )}
              <div className="text-xs text-muted-foreground mb-1">Parsed workflow:</div>
              <JsonBlock data={selected.builder_parsed_json} />
            </StageCard>

            <StageCard title="3. Auto-fix" duration={selected.autofix_duration_ms}>
              {selected.autofix_diff ? (
                <JsonBlock data={selected.autofix_diff} />
              ) : (
                <div className="text-xs text-muted-foreground">No changes needed</div>
              )}
            </StageCard>

            <StageCard
              title="4. Verification / Fixer Loop"
              duration={selected.verification_duration_ms}
              defaultOpen
            >
              <JsonBlock data={selected.verification_iterations} />
              {selected.fixer_snapshots && selected.fixer_snapshots.length > 0 && (
                <div className="mt-2">
                  <div className="text-xs text-muted-foreground mb-1">
                    Fixer snapshots ({selected.fixer_snapshots.length}):
                  </div>
                  <JsonBlock data={selected.fixer_snapshots} />
                </div>
              )}
            </StageCard>

            <StageCard title="5. Hardener" duration={selected.hardener_duration_ms}>
              <JsonBlock data={selected.hardening_summary} />
            </StageCard>

            <StageCard title="6. Final Output">
              {!!selected.validation_errors && (
                <div className="mb-2">
                  <div className="text-xs text-muted-foreground mb-1">Validation errors:</div>
                  <JsonBlock data={selected.validation_errors} />
                </div>
              )}
              <div className="text-xs text-muted-foreground mb-1">Final workflow:</div>
              <JsonBlock data={selected.final_json} maxHeight="500px" />
            </StageCard>
          </div>
        )}
      </div>
    </div>
  );
}
