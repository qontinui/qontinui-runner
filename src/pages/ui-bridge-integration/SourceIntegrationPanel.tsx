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
  Sparkles,
  Loader2,
} from "lucide-react";
import { useState, useCallback, useEffect, useRef } from "react";
import type { ProjectAnalysis, IntegrationResult, FileModification, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";
import { HookGenerationPanel } from "./HookGenerationPanel";
import { PageSelectionPanel } from "./PageSelectionPanel";
import type {
  PageComponent,
  PageGenerationOptions,
  DiscoverPagesResult,
  ApiResponse as ApiResp,
} from "./types";

interface SourceIntegrationPanelProps {
  /** When set externally (e.g. from a discovered app card), pre-fills the path and auto-analyzes */
  initialProjectPath?: string;
}

export function SourceIntegrationPanel({ initialProjectPath }: SourceIntegrationPanelProps = {}) {
  const [projectPath, setProjectPath] = useState(initialProjectPath || "");
  const [analysis, setAnalysis] = useState<ProjectAnalysis | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [integrating, setIntegrating] = useState(false);
  const [result, setResult] = useState<IntegrationResult | null>(null);
  const [preview, setPreview] = useState<FileModification[] | null>(null);
  const [updating, setUpdating] = useState(false);
  const [preparingAll, setPreparingAll] = useState(false);
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

  // When initialProjectPath changes externally, update path and auto-analyze
  const prevInitialPath = useRef(initialProjectPath);
  useEffect(() => {
    if (initialProjectPath && initialProjectPath !== prevInitialPath.current) {
      prevInitialPath.current = initialProjectPath;
      setProjectPath(initialProjectPath);
    }
  }, [initialProjectPath]);

  // Auto-analyze when path is set from outside (initialProjectPath)
  useEffect(() => {
    if (initialProjectPath && projectPath === initialProjectPath && !analysis && !analyzing) {
      analyze();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectPath, initialProjectPath]);

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

  // "Prepare All" — chains: install SDK → discover all pages → generate all
  const handlePrepareAll = useCallback(async () => {
    if (!projectPath.trim()) return;
    setPreparingAll(true);
    setError(null);

    try {
      // Step 1: If SDK not installed, install it first
      if (analysis?.ui_bridge_status === "none") {
        const resp = await fetch(`${getApiBase()}/ui-bridge/integration/integrate`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            project_path: projectPath.trim(),
            options: { install_deps: true },
          }),
        });
        const data: ApiResp<IntegrationResult> = await resp.json();
        if (!data.success || !data.data?.success) {
          setError("SDK installation failed — cannot proceed with preparation");
          setPreparingAll(false);
          return;
        }
        setResult(data.data);
        // Re-analyze to get updated status
        await analyze();
      }

      // Step 2: Discover all pages
      const discResp = await fetch(`${getApiBase()}/ui-bridge/integration/discover-pages`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ project_path: projectPath.trim() }),
      });
      const discData: ApiResp<DiscoverPagesResult> = await discResp.json();
      if (!discData.success || !discData.data || discData.data.pages.length === 0) {
        setError("No pages discovered in this project");
        setPreparingAll(false);
        return;
      }

      // Step 3: Trigger generation for ALL pages with all options
      const allPages = discData.data.pages;
      const options: PageGenerationOptions = {
        generateRegistrations: true,
        generateDataPageIds: true,
        generateSpecs: true,
        generateTutorials: false, // Off by default to keep it faster
        generateDemoVideos: false,
        generateProductTours: false,
      };

      window.dispatchEvent(
        new CustomEvent("ui-bridge-generate-pages", { detail: { pages: allPages, options } }),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : "Prepare All failed");
    } finally {
      setPreparingAll(false);
    }
  }, [projectPath, analysis, analyze]);

  return (
    <div className="flex flex-col gap-4">
      {/* Path input */}
      <div className="p-4 rounded-lg border border-border bg-card/50">
        <h3 className="text-sm font-medium mb-1">Integrate Project</h3>
        <p className="text-xs text-muted-foreground mb-3">
          Enter the path to your project to analyze and integrate the UI Bridge SDK. Works with web
          apps (React, Next.js), desktop apps (Tauri), and mobile projects (React Native).
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
                         placeholder:text-muted-foreground focus:outline-hidden focus:border-cyan-500/50"
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
                <div
                  key={`${ep.path}-${i}`}
                  className="flex items-center gap-2 text-xs text-muted-foreground"
                >
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
                <div
                  key={`${issue}-${i}`}
                  className="flex items-center gap-1.5 text-xs text-yellow-400/80"
                >
                  <AlertTriangle className="w-3 h-3 shrink-0" />
                  {issue}
                </div>
              ))}
            </div>
          )}

          {/* Prepare All — one-click full setup */}
          <div className="mt-4 pt-3 border-t border-border mb-2">
            <button
              onClick={handlePrepareAll}
              disabled={preparingAll || integrating}
              className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium rounded-lg
                         bg-gradient-to-r from-cyan-500/20 to-purple-500/20 text-foreground
                         border border-cyan-500/30 hover:border-purple-500/30
                         hover:from-cyan-500/30 hover:to-purple-500/30
                         disabled:opacity-50 transition-all"
            >
              {preparingAll ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Sparkles className="w-4 h-4 text-purple-400" />
              )}
              {preparingAll
                ? "Preparing..."
                : analysis.ui_bridge_status === "none"
                  ? "Prepare Repository"
                  : "Prepare All Pages"}
            </button>
            <p className="text-[10px] text-muted-foreground text-center mt-1">
              {analysis.ui_bridge_status === "none"
                ? "Installs SDK, discovers pages, generates registrations + specs for all pages"
                : "Discovers pages, generates registrations + specs for all pages"}
            </p>
          </div>

          {/* Individual actions */}
          <div className="flex items-center gap-2">
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
              <div
                key={`${mod.file_path}-${i}`}
                className="flex items-start gap-2 p-2 rounded bg-white/5 text-xs"
              >
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
                <p key={`${mod.file_path}-${i}`} className="text-xs text-muted-foreground">
                  {mod.modification_type === "create_new" ? "+" : "~"} {mod.file_path}
                </p>
              ))}
            </div>
          )}

          {result.next_steps.length > 0 && (
            <div>
              <p className="text-[10px] text-muted-foreground font-medium mb-1">Next steps:</p>
              {result.next_steps.map((step, i) => (
                <p key={`${step}-${i}`} className="text-xs text-muted-foreground">
                  {i + 1}. {step}
                </p>
              ))}
            </div>
          )}

          {result.warnings.length > 0 && (
            <div className="mt-2">
              {result.warnings.map((w, i) => (
                <p key={`${w}-${i}`} className="text-xs text-yellow-400/80">
                  {w}
                </p>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Page discovery & AI generation — shown when SDK is at least partially integrated */}
      {analysis && analysis.ui_bridge_status !== "none" && (
        <PageSelectionPanel
          projectPath={projectPath}
          onStartGeneration={(pages: PageComponent[], options: PageGenerationOptions) => {
            window.dispatchEvent(
              new CustomEvent("ui-bridge-generate-pages", { detail: { pages, options } }),
            );
          }}
        />
      )}

      {/* Hook generation panel — shown when SDK is at least partially integrated */}
      {analysis && analysis.ui_bridge_status !== "none" && (
        <HookGenerationPanel
          projectPath={projectPath}
          analysis={analysis}
          onRefreshAnalysis={analyze}
        />
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
