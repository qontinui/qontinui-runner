import {
  FolderOpen,
  Search,
  Wrench,
  CheckCircle2,
  AlertTriangle,
  FileCode,
  RefreshCw,
  Eye,
  ArrowUpCircle,
} from "lucide-react";
import { useState, useCallback } from "react";
import type { ProjectAnalysis, IntegrationResult, FileModification, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";

export function SourceIntegrationPanel() {
  const [projectPath, setProjectPath] = useState("");
  const [analysis, setAnalysis] = useState<ProjectAnalysis | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [integrating, setIntegrating] = useState(false);
  const [result, setResult] = useState<IntegrationResult | null>(null);
  const [preview, setPreview] = useState<FileModification[] | null>(null);
  const [updating, setUpdating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const analyze = useCallback(async () => {
    if (!projectPath.trim()) return;
    setAnalyzing(true);
    setError(null);
    setAnalysis(null);
    setResult(null);
    setPreview(null);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/analyze`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ project_path: projectPath.trim() }),
      });
      const data: ApiResponse<ProjectAnalysis> = await resp.json();
      if (data.success && data.data) {
        setAnalysis(data.data);
      } else {
        setError(data.error || "Analysis failed");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Analysis failed");
    } finally {
      setAnalyzing(false);
    }
  }, [projectPath]);

  const previewChanges = useCallback(async () => {
    if (!projectPath.trim()) return;
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/preview`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          project_path: projectPath.trim(),
          options: { install_deps: true },
        }),
      });
      const data: ApiResponse<FileModification[]> = await resp.json();
      if (data.success && data.data) {
        setPreview(data.data);
      }
    } catch (err) {
      console.error("Preview failed:", err);
    }
  }, [projectPath]);

  const integrate = useCallback(async () => {
    if (!projectPath.trim()) return;
    setIntegrating(true);
    setError(null);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/integrate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          project_path: projectPath.trim(),
          options: { install_deps: true },
        }),
      });
      const data: ApiResponse<IntegrationResult> = await resp.json();
      if (data.success && data.data) {
        setResult(data.data);
        // Re-analyze to show updated status
        analyze();
      } else {
        setError(data.error || "Integration failed");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Integration failed");
    } finally {
      setIntegrating(false);
    }
  }, [projectPath, analyze]);

  const updateSdk = useCallback(async () => {
    if (!projectPath.trim()) return;
    setUpdating(true);
    setError(null);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/update`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          project_path: projectPath.trim(),
          sdk_version: "latest",
        }),
      });
      const data: ApiResponse<IntegrationResult> = await resp.json();
      if (data.success && data.data) {
        setResult(data.data);
        analyze();
      } else {
        setError(data.error || "Update failed");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Update failed");
    } finally {
      setUpdating(false);
    }
  }, [projectPath, analyze]);

  return (
    <div className="flex flex-col gap-4">
      {/* Path input */}
      <div className="p-4 rounded-lg border border-border bg-card/50">
        <h3 className="text-sm font-medium mb-1">Project Directory</h3>
        <p className="text-xs text-muted-foreground mb-3">
          Enter the path to a web application project to analyze and integrate the UI Bridge SDK.
          The SDK automatically discovers and registers interactive elements with stable semantic
          IDs at runtime via the bridge registry.
        </p>
        <div className="flex gap-2">
          <div className="flex-1 relative">
            <FolderOpen className="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
            <input
              type="text"
              value={projectPath}
              onChange={(e) => setProjectPath(e.target.value)}
              placeholder="C:\Users\you\projects\my-app"
              className="w-full pl-9 pr-3 py-1.5 text-sm rounded border border-border bg-background
                         placeholder:text-muted-foreground focus:outline-none focus:border-cyan-500/50"
            />
          </div>
          <button
            onClick={analyze}
            disabled={analyzing || !projectPath.trim()}
            className="flex items-center gap-1.5 px-4 py-1.5 text-sm font-medium rounded
                       bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                       hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
          >
            {analyzing ? (
              <RefreshCw className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Search className="w-3.5 h-3.5" />
            )}
            Analyze
          </button>
        </div>
        {error && <p className="text-xs text-red-400 mt-2">{error}</p>}
      </div>

      {/* Analysis results */}
      {analysis && (
        <div className="p-4 rounded-lg border border-border bg-card/50">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-medium">Analysis Results</h3>
            <StatusBadge status={analysis.ui_bridge_status} />
          </div>

          <div className="grid grid-cols-2 gap-3 text-xs">
            <InfoRow label="Framework" value={formatFramework(analysis.framework)} />
            <InfoRow label="Package Manager" value={analysis.package_manager} />
            <InfoRow label="SDK Version" value={analysis.existing_sdk_version || "Not installed"} />
            <InfoRow label="Server Adapter" value={analysis.server_adapter || "None"} />
            <InfoRow
              label="Dev Server Port"
              value={analysis.dev_server_port?.toString() || "Unknown"}
            />
            <InfoRow label="Entry Points" value={analysis.entry_points.length.toString()} />
          </div>

          {analysis.entry_points.length > 0 && (
            <div className="mt-3">
              <p className="text-[10px] text-muted-foreground font-medium mb-1">Entry Points:</p>
              {analysis.entry_points.map((ep, i) => (
                <div key={i} className="flex items-center gap-2 text-xs text-muted-foreground">
                  <FileCode className="w-3 h-3 shrink-0" />
                  <span className="truncate">{ep.path}</span>
                  <span className="text-[10px] px-1 py-0.5 rounded bg-white/5">
                    {ep.entry_type}
                  </span>
                </div>
              ))}
            </div>
          )}

          {analysis.issues.length > 0 && (
            <div className="mt-3">
              <p className="text-[10px] text-yellow-400 font-medium mb-1">Issues:</p>
              {analysis.issues.map((issue, i) => (
                <div key={i} className="flex items-center gap-1.5 text-xs text-yellow-400/80">
                  <AlertTriangle className="w-3 h-3 shrink-0" />
                  {issue}
                </div>
              ))}
            </div>
          )}

          {/* Actions */}
          <div className="flex items-center gap-2 mt-4 pt-3 border-t border-border">
            {analysis.ui_bridge_status !== "full" && (
              <>
                <button
                  onClick={previewChanges}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                             bg-white/5 text-muted-foreground border border-border
                             hover:bg-white/10 transition-colors"
                >
                  <Eye className="w-3.5 h-3.5" />
                  Preview Changes
                </button>
                <button
                  onClick={integrate}
                  disabled={integrating}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                             bg-green-500/10 text-green-400 border border-green-500/20
                             hover:bg-green-500/20 disabled:opacity-50 transition-colors"
                >
                  {integrating ? (
                    <RefreshCw className="w-3.5 h-3.5 animate-spin" />
                  ) : (
                    <Wrench className="w-3.5 h-3.5" />
                  )}
                  {integrating ? "Integrating..." : "Integrate"}
                </button>
              </>
            )}
            {(analysis.ui_bridge_status === "partial" || analysis.ui_bridge_status === "full") && (
              <button
                onClick={updateSdk}
                disabled={updating}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                           bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                           hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
              >
                {updating ? (
                  <RefreshCw className="w-3.5 h-3.5 animate-spin" />
                ) : (
                  <ArrowUpCircle className="w-3.5 h-3.5" />
                )}
                {updating ? "Updating..." : "Update SDK"}
              </button>
            )}
          </div>
        </div>
      )}

      {/* Preview */}
      {preview && preview.length > 0 && (
        <div className="p-4 rounded-lg border border-border bg-card/50">
          <h3 className="text-sm font-medium mb-3">Planned Modifications</h3>
          <div className="flex flex-col gap-2">
            {preview.map((mod, i) => (
              <div key={i} className="flex items-start gap-2 p-2 rounded bg-white/5 text-xs">
                <FileCode className="w-3.5 h-3.5 text-cyan-400 shrink-0 mt-0.5" />
                <div>
                  <p className="font-medium text-foreground">{mod.file_path}</p>
                  <p className="text-muted-foreground">{mod.description}</p>
                  <span className="text-[10px] px-1 py-0.5 rounded bg-cyan-500/10 text-cyan-400">
                    {mod.modification_type}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Integration result */}
      {result && (
        <div
          className={`p-4 rounded-lg border ${
            result.success ? "border-green-500/30 bg-green-500/5" : "border-red-500/30 bg-red-500/5"
          }`}
        >
          <div className="flex items-center gap-2 mb-2">
            {result.success ? (
              <CheckCircle2 className="w-4 h-4 text-green-400" />
            ) : (
              <AlertTriangle className="w-4 h-4 text-red-400" />
            )}
            <h3 className="text-sm font-medium">
              {result.success ? "Integration Successful" : "Integration Failed"}
            </h3>
          </div>

          {result.modifications.length > 0 && (
            <div className="mb-2">
              <p className="text-[10px] text-muted-foreground font-medium mb-1">Modified files:</p>
              {result.modifications.map((mod, i) => (
                <p key={i} className="text-xs text-muted-foreground">
                  {mod.modification_type === "create_new" ? "+" : "~"} {mod.file_path}
                </p>
              ))}
            </div>
          )}

          {result.next_steps.length > 0 && (
            <div>
              <p className="text-[10px] text-muted-foreground font-medium mb-1">Next steps:</p>
              {result.next_steps.map((step, i) => (
                <p key={i} className="text-xs text-muted-foreground">
                  {i + 1}. {step}
                </p>
              ))}
            </div>
          )}

          {result.warnings.length > 0 && (
            <div className="mt-2">
              {result.warnings.map((w, i) => (
                <p key={i} className="text-xs text-yellow-400/80">
                  {w}
                </p>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <span className="text-muted-foreground">{label}:</span>{" "}
      <span className="text-foreground font-medium">{value}</span>
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const config: Record<string, { bg: string; text: string; label: string }> = {
    none: {
      bg: "bg-yellow-500/10",
      text: "text-yellow-400",
      label: "Not Integrated",
    },
    partial: {
      bg: "bg-orange-500/10",
      text: "text-orange-400",
      label: "Partially Integrated",
    },
    full: {
      bg: "bg-green-500/10",
      text: "text-green-400",
      label: "Fully Integrated",
    },
  };
  const c = config[status] || config.none;
  return (
    <span className={`text-[10px] px-2 py-0.5 rounded-full font-medium ${c.bg} ${c.text}`}>
      {c.label}
    </span>
  );
}

function formatFramework(fw: string): string {
  const map: Record<string, string> = {
    react: "React",
    next_js: "Next.js",
    nextjs: "Next.js",
    vue: "Vue",
    angular: "Angular",
    svelte: "Svelte",
    plain_html: "Plain HTML",
    unknown: "Unknown",
  };
  return map[fw] || fw;
}
