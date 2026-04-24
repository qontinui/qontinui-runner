/**
 * ProjectCoordinator
 *
 * Top-level redesign of the UI Bridge Integration entry point.
 *
 *   1. Project dropdown sourced from the wizard-persisted `useSavedProjects`
 *      store, with "Browse..." + "Type a path manually..." escape hatches.
 *   2. A single big "Integrate this Project" CTA that runs the whole
 *      analyze → integrate → discover → generate flow with smart defaults.
 *   3. An inline "Edit list" modal for removing saved projects.
 *
 * The existing per-stage UI (SourceIntegrationPanel's analyze/integrate/preview,
 * PageSelectionPanel's checklist, HookGenerationPanel's AI controls) is NOT
 * removed — it lives under an "Advanced: per-stage controls" disclosure on the
 * parent page so power users keep their fine-grained workflow.
 *
 * Implementation notes
 * --------------------
 * - The full pipeline reuses the existing HTTP endpoints
 *   (/ui-bridge/integration/{analyze,integrate,discover-pages}) and the
 *   existing `ui-bridge-generate-pages` CustomEvent that HookGenerationPanel
 *   listens for. We do not rewrite the AI state machine; we just fire its
 *   start event with pre-selected pages and smart-default options.
 * - Completion is observed via three CustomEvents HookGenerationPanel now
 *   broadcasts: `ui-bridge-generate-pages-complete` (files ready),
 *   `ui-bridge-generate-pages-applied` (files on disk), and
 *   `ui-bridge-generate-pages-error`. This keeps the coordinator decoupled
 *   from the panel's internal phase enum.
 * - When `useSavedProjects` returns empty we default to manual-path mode so
 *   the panel stays usable for users who skipped the wizard.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Sparkles,
  Loader2,
  CheckCircle2,
  AlertTriangle,
  FolderOpen,
  ChevronDown,
  X,
  Copy,
  ExternalLink,
  RefreshCw,
} from "lucide-react";
import { getApiBase } from "@/lib/runner-api";
import { useSavedProjects, type SavedProject } from "@/hooks/useSavedProjects";
import type {
  ApiResponse,
  ProjectAnalysis,
  IntegrationResult,
  DiscoverPagesResult,
  PageComponent,
  PageGenerationOptions,
} from "./types";

// Manual-entry sentinel. Using Symbol-like sentinel strings in <option value>
// so we can distinguish from a real path. Real paths never start with "::".
const MANUAL_PATH_VALUE = "::manual";
const BROWSE_VALUE = "::browse";

/**
 * localStorage key for the last project path the user picked from the
 * dropdown. Sticky across runner restarts so users don't have to re-select
 * the same project every session.
 */
const LAST_SELECTED_PROJECT_KEY = "qontinui:ui-bridge-integration:last-selected-project";

function readLastSelectedProject(): string | null {
  if (typeof window === "undefined") return null;
  try {
    const v = window.localStorage.getItem(LAST_SELECTED_PROJECT_KEY);
    return v && v.length > 0 ? v : null;
  } catch {
    return null;
  }
}

function writeLastSelectedProject(path: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(LAST_SELECTED_PROJECT_KEY, path);
  } catch {
    /* localStorage quota/permission — non-fatal */
  }
}

type CoordinatorPhase =
  | "idle"
  | "analyzing"
  | "integrating"
  | "discovering"
  | "no-pages"
  | "generating"
  | "generated" // files ready, HookGenerationPanel is in preview
  | "applied" // files written to disk
  | "failed";

interface CompletionDetail {
  filesGenerated: number;
  doneSteps: string[];
  failedSteps: string[];
}

interface AppliedDetail {
  filesWritten: string[];
}

interface ErrorDetail {
  message: string;
}

interface ProjectCoordinatorProps {
  /** Optional: external pre-fill (e.g. from DiscoveryPanel's app card). */
  initialProjectPath?: string;
  /**
   * Called whenever the coordinator's selected project path changes. The
   * parent page forwards this into SourceIntegrationPanel (Advanced) so the
   * two stay in sync.
   */
  onProjectPathChange?: (path: string) => void;
}

export function ProjectCoordinator({
  initialProjectPath,
  onProjectPathChange,
}: ProjectCoordinatorProps) {
  const { projects, loading: projectsLoading, addProject, removeProject } = useSavedProjects();

  // `manualMode` tracks whether the user has explicitly chosen the free-text
  // input. If it's null we derive the default from the saved-projects list
  // (empty list → manual-mode default). Deriving rather than effect-setting
  // sidesteps the react-hooks/set-state-in-effect lint rule.
  const [manualModeOverride, setManualMode] = useState<boolean | null>(null);
  const defaultManualMode = !projectsLoading && projects.length === 0;
  const manualMode = manualModeOverride ?? defaultManualMode;
  // On mount, read the previously-selected project path from localStorage so
  // returning users land on the last project they worked with. The effect
  // below validates it against the loaded `projects` list and falls back to
  // the first saved project if the remembered one is no longer saved.
  //
  // `readLastSelectedProject` is pure (wraps a localStorage.getItem with
  // SSR / exception guards) so calling it twice from the lazy init of two
  // useState slots is equivalent to reading once. Avoids a useRef which
  // would trip react-hooks/refs for being accessed during render.
  const [projectPath, setProjectPath] = useState<string>(
    () => initialProjectPath ?? readLastSelectedProject() ?? "",
  );
  const [dropdownValue, setDropdownValue] = useState<string>(
    () =>
      initialProjectPath ??
      readLastSelectedProject() ??
      (defaultManualMode ? MANUAL_PATH_VALUE : ""),
  );
  const [editListOpen, setEditListOpen] = useState(false);

  // Pipeline state
  const [phase, setPhase] = useState<CoordinatorPhase>("idle");
  const [analysis, setAnalysis] = useState<ProjectAnalysis | null>(null);
  const [discoveredPages, setDiscoveredPages] = useState<PageComponent[]>([]);
  const [progressMessage, setProgressMessage] = useState<string>("");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [doneSteps, setDoneSteps] = useState<string[]>([]);
  const [failedSteps, setFailedSteps] = useState<string[]>([]);
  const [filesGenerated, setFilesGenerated] = useState<number>(0);
  const [filesWritten, setFilesWritten] = useState<string[]>([]);
  const [rebuildCopied, setRebuildCopied] = useState(false);
  // Guards against external analyze() races starting a run mid-flow.
  const runIdRef = useRef(0);

  // Sync external initialProjectPath (from DiscoveryPanel clicks) into our
  // state when it changes.
  const prevInitialRef = useRef(initialProjectPath);
  useEffect(() => {
    if (initialProjectPath && initialProjectPath !== prevInitialRef.current) {
      prevInitialRef.current = initialProjectPath;
      setProjectPath(initialProjectPath);
      setDropdownValue(initialProjectPath);
      setManualMode(false);
    }
  }, [initialProjectPath]);

  // (Empty-list manual-mode default is derived above — no effect needed.)

  // Reconcile the initial selection once the async `projects` list loads.
  // The first render happens with `projects = []` and `projectsLoading = true`,
  // so the useState default lands on the sentinel "" (or manual, if the
  // load had already completed). This effect:
  //
  //   - picks up the remembered-last-project from localStorage if it's still
  //     present in the saved list (sticky across runner restarts);
  //   - falls back to the first saved project if the remembered one is gone;
  //   - does nothing if the user has already made an explicit choice
  //     (`manualModeOverride` set, or a real path already in `projectPath`).
  //
  // This is a legitimate one-time setState-in-effect: the `autoSelectDoneRef`
  // guard means the body runs exactly once per mount (the same invariant
  // the lint rule is protecting), and the initialization depends on an
  // async-loaded value that the useState lazy-initializer can't see.
  const autoSelectDoneRef = useRef(false);
  useEffect(() => {
    if (autoSelectDoneRef.current) return;
    if (projectsLoading) return;
    if (projects.length === 0) {
      // No saved projects — leave the default-manual-mode path unchanged.
      autoSelectDoneRef.current = true;
      return;
    }
    // Respect explicit user choices and external pre-fills.
    if (initialProjectPath) {
      autoSelectDoneRef.current = true;
      return;
    }
    if (manualModeOverride !== null) {
      autoSelectDoneRef.current = true;
      return;
    }
    if (projectPath && projectPath !== "") {
      // Already initialized from localStorage during useState; verify the
      // remembered path is still in the list, otherwise fall back to the
      // first saved project.
      const stillPresent = projects.some((p) => p.path === projectPath);
      if (!stillPresent) {
        const first = projects[0].path;
        // eslint-disable-next-line react-hooks/set-state-in-effect -- one-time post-load reconcile guarded by autoSelectDoneRef
        setProjectPath(first);

        setDropdownValue(first);
      } else {
        // Keep current selection and make sure the dropdown mirrors it.

        setDropdownValue(projectPath);
      }
      autoSelectDoneRef.current = true;
      return;
    }
    // No selection yet — pick the first saved project so the user doesn't
    // have to click anything to reach a usable state. This also prevents the
    // dropdown from defaulting to the Manual sentinel when real projects
    // exist (the previous behavior users flagged as surprising).
    const first = projects[0].path;

    setProjectPath(first);

    setDropdownValue(first);
    autoSelectDoneRef.current = true;
  }, [projectsLoading, projects, initialProjectPath, manualModeOverride, projectPath]);

  // Persist the user's choice so the next runner session lands on the same
  // project. Only write real paths — sentinels and empty strings aren't
  // meaningful across sessions.
  useEffect(() => {
    if (projectPath && !projectPath.startsWith("::")) {
      writeLastSelectedProject(projectPath);
    }
  }, [projectPath]);

  // Notify parent of path changes so Advanced panels stay in sync.
  useEffect(() => {
    onProjectPathChange?.(projectPath);
  }, [projectPath, onProjectPathChange]);

  // =========================================================================
  // Completion-event listeners (fed by HookGenerationPanel broadcast events)
  // =========================================================================
  useEffect(() => {
    const onComplete = (e: Event) => {
      const detail = (e as CustomEvent).detail as CompletionDetail | undefined;
      if (!detail) return;
      setFilesGenerated(detail.filesGenerated);
      setDoneSteps(detail.doneSteps);
      setFailedSteps(detail.failedSteps);
      // "generated" = files ready in HookGenerationPanel's preview. The user
      // still has to click Apply there (in Advanced) to write to disk — we
      // tell them that below.
      setPhase("generated");
    };
    const onApplied = (e: Event) => {
      const detail = (e as CustomEvent).detail as AppliedDetail | undefined;
      if (!detail) return;
      setFilesWritten(detail.filesWritten);
      setPhase("applied");
    };
    const onError = (e: Event) => {
      const detail = (e as CustomEvent).detail as ErrorDetail | undefined;
      if (!detail) return;
      // Only treat as top-level failure when we're mid-pipeline. Stray
      // errors from ad-hoc Advanced-panel runs shouldn't clobber an idle UI.
      if (phase === "generating") {
        setErrorMessage(detail.message);
        setPhase("failed");
      }
    };
    window.addEventListener("ui-bridge-generate-pages-complete", onComplete);
    window.addEventListener("ui-bridge-generate-pages-applied", onApplied);
    window.addEventListener("ui-bridge-generate-pages-error", onError);
    return () => {
      window.removeEventListener("ui-bridge-generate-pages-complete", onComplete);
      window.removeEventListener("ui-bridge-generate-pages-applied", onApplied);
      window.removeEventListener("ui-bridge-generate-pages-error", onError);
    };
  }, [phase]);

  // =========================================================================
  // Pipeline step helpers (thin wrappers around existing endpoints)
  // =========================================================================

  const runAnalyze = useCallback(async (path: string): Promise<ProjectAnalysis | null> => {
    const resp = await fetch(`${getApiBase()}/ui-bridge/integration/analyze`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ project_path: path }),
    });
    const data: ApiResponse<ProjectAnalysis> = await resp.json();
    if (!data.success || !data.data) {
      throw new Error(data.error || "Analysis failed");
    }
    return data.data;
  }, []);

  const runIntegrate = useCallback(async (path: string): Promise<IntegrationResult> => {
    const resp = await fetch(`${getApiBase()}/ui-bridge/integration/integrate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ project_path: path, options: { install_deps: true } }),
    });
    const data: ApiResponse<IntegrationResult> = await resp.json();
    if (!data.success || !data.data) {
      throw new Error(data.error || "Integration failed");
    }
    return data.data;
  }, []);

  const runDiscover = useCallback(async (path: string): Promise<PageComponent[]> => {
    const resp = await fetch(`${getApiBase()}/ui-bridge/integration/discover-pages`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ project_path: path }),
    });
    const data: ApiResponse<DiscoverPagesResult> = await resp.json();
    if (!data.success || !data.data) {
      throw new Error(data.error || "Discovery failed");
    }
    return data.data.pages;
  }, []);

  // =========================================================================
  // The "Integrate this Project" one-click flow
  // =========================================================================

  const defaultGenerationOptions: PageGenerationOptions = useMemo(
    () => ({
      generateRegistrations: true,
      generateDataPageIds: true,
      generateSpecs: false,
      generateTutorials: false,
      generateArchitectureDiagrams: false,
      generateDemoVideos: false,
      generateProductTours: false,
      generateProjectExplainer: false,
    }),
    [],
  );

  const kickOffGeneration = useCallback(
    (pages: PageComponent[], options: PageGenerationOptions) => {
      // Reuse HookGenerationPanel's existing start event — no new machinery.
      window.dispatchEvent(
        new CustomEvent("ui-bridge-generate-pages", { detail: { pages, options } }),
      );
    },
    [],
  );

  const runPipeline = useCallback(
    async (path: string) => {
      if (!path.trim()) return;
      const myRun = ++runIdRef.current;
      setErrorMessage(null);
      setDoneSteps([]);
      setFailedSteps([]);
      setFilesGenerated(0);
      setFilesWritten([]);
      setDiscoveredPages([]);

      try {
        // Step 1 — Analyze
        setPhase("analyzing");
        setProgressMessage("Analyzing project...");
        const a = await runAnalyze(path);
        if (runIdRef.current !== myRun) return;
        setAnalysis(a);
        if (!a) throw new Error("Analysis returned no data");

        // Step 2 — Integrate framework-level SDK if not already fully wired
        if (a.ui_bridge_status !== "full") {
          setPhase("integrating");
          setProgressMessage("Integrating UI Bridge SDK...");
          const ir = await runIntegrate(path);
          if (runIdRef.current !== myRun) return;
          if (!ir.success) throw new Error("SDK integration failed");
          // Re-analyze so downstream status badge is accurate.
          try {
            const re = await runAnalyze(path);
            if (runIdRef.current !== myRun) return;
            if (re) setAnalysis(re);
          } catch {
            // Non-fatal — the integrate result itself reports success.
          }
        }

        // Step 3 — Discover pages
        setPhase("discovering");
        setProgressMessage("Discovering pages...");
        const pages = await runDiscover(path);
        if (runIdRef.current !== myRun) return;
        setDiscoveredPages(pages);
        if (pages.length === 0) {
          setPhase("no-pages");
          return;
        }

        // Step 4 — Pre-select all pages and start generation with defaults.
        setPhase("generating");
        setProgressMessage(
          `Generating for ${pages.length} page${pages.length !== 1 ? "s" : ""}...`,
        );
        kickOffGeneration(pages, defaultGenerationOptions);
        // From here on, the HookGenerationPanel drives phase via the three
        // CustomEvents we subscribed to in the useEffect above.
      } catch (err) {
        if (runIdRef.current !== myRun) return;
        setErrorMessage(err instanceof Error ? err.message : "Integration failed");
        setPhase("failed");
      }
    },
    [runAnalyze, runIntegrate, runDiscover, kickOffGeneration, defaultGenerationOptions],
  );

  const retryFailed = useCallback(() => {
    // Failed pages: we stored labels, but HookGenerationPanel's labels are
    // shaped "<route> (regs+spec+...)". Match them back to discoveredPages
    // by route prefix — the most reliable anchor we have.
    const failedRoutes = new Set(failedSteps.map((label) => label.split(" ")[0]));
    const pagesToRetry = discoveredPages.filter((p) => failedRoutes.has(p.route));
    if (pagesToRetry.length === 0) {
      setErrorMessage("No failed pages could be matched to retry.");
      return;
    }
    setPhase("generating");
    setProgressMessage(
      `Retrying ${pagesToRetry.length} page${pagesToRetry.length !== 1 ? "s" : ""}...`,
    );
    setFailedSteps([]);
    setErrorMessage(null);
    kickOffGeneration(pagesToRetry, defaultGenerationOptions);
  }, [failedSteps, discoveredPages, kickOffGeneration, defaultGenerationOptions]);

  // =========================================================================
  // Dropdown handlers
  // =========================================================================

  const handleDropdownChange = useCallback(
    async (value: string) => {
      setDropdownValue(value);

      if (value === BROWSE_VALUE) {
        try {
          const selected = await openDialog({ directory: true, multiple: false });
          if (!selected) {
            // Revert dropdown to whatever was there before.
            setDropdownValue(projectPath || MANUAL_PATH_VALUE);
            return;
          }
          const picked = selected as string;
          // Name derived from the folder; project_type/manifest unknown at
          // this layer — the wizard fills those in during scanning.
          const folderName = picked.split(/[\\/]/).filter(Boolean).slice(-1)[0] || picked;
          const entry: SavedProject = {
            path: picked,
            name: folderName,
            projectType: "unknown",
            manifest: "",
          };
          try {
            await addProject(entry);
          } catch (err) {
            // Persistence failed (e.g. Tauri not available in a dev browser)
            // — still allow the user to use the path this session.
            console.warn("[ProjectCoordinator] Could not persist project:", err);
          }
          setProjectPath(picked);
          setDropdownValue(picked);
          setManualMode(false);
          void runPipeline(picked);
        } catch (err) {
          console.warn("[ProjectCoordinator] Folder picker failed:", err);
          setDropdownValue(projectPath || MANUAL_PATH_VALUE);
        }
        return;
      }

      if (value === MANUAL_PATH_VALUE) {
        setManualMode(true);
        return;
      }

      // A saved project was picked.
      setManualMode(false);
      setProjectPath(value);
      void runPipeline(value);
    },
    [projectPath, addProject, runPipeline],
  );

  const handleManualSubmit = useCallback(() => {
    if (!projectPath.trim()) return;
    void runPipeline(projectPath.trim());
  }, [projectPath, runPipeline]);

  const handleCopyRebuildCommand = useCallback(async () => {
    try {
      await navigator.clipboard.writeText("npm run build");
      setRebuildCopied(true);
      setTimeout(() => setRebuildCopied(false), 2000);
    } catch (err) {
      console.warn("[ProjectCoordinator] Clipboard write failed:", err);
    }
  }, []);

  // Button disabled / label state
  const pipelineRunning =
    phase === "analyzing" ||
    phase === "integrating" ||
    phase === "discovering" ||
    phase === "generating";

  const canRun = projectPath.trim().length > 0 && !pipelineRunning;

  const phaseLabel = useMemo(() => {
    switch (phase) {
      case "analyzing":
        return "Analyzing";
      case "integrating":
        return "Integrating";
      case "discovering":
        return "Discovering";
      case "generating":
        return "Generating";
      default:
        return null;
    }
  }, [phase]);

  // =========================================================================
  // Render
  // =========================================================================

  return (
    <div className="flex flex-col gap-4">
      {/* ----- Project picker ----- */}
      <div className="p-4 rounded-lg border border-border bg-card/50">
        <div className="flex items-center justify-between mb-2">
          <label className="text-sm font-medium" htmlFor="uib-project-select">
            Project
          </label>
          {projects.length > 0 && (
            <button
              type="button"
              onClick={() => setEditListOpen(true)}
              className="text-xs text-cyan-400 hover:text-cyan-300"
            >
              Edit list
            </button>
          )}
        </div>

        {!manualMode ? (
          <div className="relative">
            <select
              id="uib-project-select"
              value={dropdownValue}
              onChange={(e) => void handleDropdownChange(e.target.value)}
              disabled={pipelineRunning}
              className="w-full appearance-none pl-3 pr-9 py-2 text-sm rounded border border-border bg-background
                         focus:outline-hidden focus:border-cyan-500/50 disabled:opacity-50"
            >
              {/* If the current path isn't in the saved list, show it as the
                  first (disabled) entry so the select still reflects it. */}
              {projectPath && !projects.find((p) => p.path === projectPath) && (
                <option value={projectPath}>
                  {projectPath.split(/[\\/]/).filter(Boolean).slice(-1)[0] || projectPath}
                </option>
              )}
              {projects.length === 0 && !projectPath && (
                <option value="" disabled>
                  No saved projects — use Browse or Type below
                </option>
              )}
              {projects.map((p) => (
                <option key={p.path} value={p.path}>
                  {p.name} — {p.path}
                </option>
              ))}
              <option value={BROWSE_VALUE}>Browse...</option>
              <option value={MANUAL_PATH_VALUE}>Type a path manually...</option>
            </select>
            <ChevronDown className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
            {dropdownValue && !dropdownValue.startsWith("::") && (
              <p className="mt-1 text-[10px] text-muted-foreground truncate">{dropdownValue}</p>
            )}
          </div>
        ) : (
          <div className="flex gap-2">
            <div className="flex-1 relative">
              <FolderOpen className="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
              <input
                type="text"
                value={projectPath}
                onChange={(e) => setProjectPath(e.target.value)}
                onBlur={handleManualSubmit}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleManualSubmit();
                }}
                placeholder="C:\\Users\\you\\projects\\my-app"
                className="w-full pl-9 pr-3 py-2 text-sm rounded border border-border bg-background
                           placeholder:text-muted-foreground focus:outline-hidden focus:border-cyan-500/50"
              />
            </div>
            {projects.length > 0 && (
              <button
                type="button"
                onClick={() => {
                  setManualMode(false);
                  setDropdownValue(projectPath || (projects[0]?.path ?? ""));
                }}
                className="px-3 py-1.5 text-xs rounded border border-border bg-white/5 hover:bg-white/10"
              >
                Use dropdown
              </button>
            )}
          </div>
        )}
      </div>

      {/* ----- Primary CTA ----- */}
      <div className="p-4 rounded-lg border border-cyan-500/20 bg-gradient-to-br from-cyan-500/5 to-purple-500/5">
        <button
          type="button"
          onClick={() => void runPipeline(projectPath.trim())}
          disabled={!canRun}
          className="w-full flex items-center justify-center gap-2 px-4 py-3 text-sm font-semibold rounded-lg
                     bg-gradient-to-r from-cyan-500/30 to-purple-500/30 text-foreground
                     border border-cyan-500/40 hover:border-purple-500/50
                     hover:from-cyan-500/40 hover:to-purple-500/40
                     disabled:opacity-50 disabled:cursor-not-allowed transition-all"
        >
          {pipelineRunning ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <Sparkles className="w-4 h-4 text-purple-400" />
          )}
          {pipelineRunning ? `${phaseLabel}...` : "Integrate this Project"}
        </button>
        <p className="text-[11px] text-muted-foreground text-center mt-2">
          Runs analyze, install SDK (if needed), discover pages, and generate registrations + page
          IDs for every page. For specs, tutorials, or videos, open Advanced below.
        </p>
      </div>

      {/* ----- Unified progress view ----- */}
      {(pipelineRunning || phase === "generated" || phase === "applied" || phase === "failed") && (
        <div className="p-4 rounded-lg border border-border bg-card/50">
          <PhaseBanner
            phase={phase}
            progressMessage={progressMessage}
            filesGenerated={filesGenerated}
            filesWritten={filesWritten.length}
            failedCount={failedSteps.length}
            errorMessage={errorMessage}
          />

          {(phase === "generating" ||
            phase === "generated" ||
            phase === "applied" ||
            phase === "failed") &&
            (doneSteps.length > 0 || failedSteps.length > 0) && (
              <details className="mt-3 text-xs">
                <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
                  See details ({doneSteps.length} done
                  {failedSteps.length > 0 ? `, ${failedSteps.length} failed` : ""})
                </summary>
                <div className="mt-2 space-y-1">
                  {doneSteps.map((s, i) => (
                    <div key={`done-${i}`} className="flex items-center gap-1.5 text-green-400/80">
                      <CheckCircle2 className="w-3 h-3 shrink-0" />
                      <span className="truncate">{s}</span>
                    </div>
                  ))}
                  {failedSteps.map((s, i) => (
                    <div key={`fail-${i}`} className="flex items-center gap-1.5 text-red-400/80">
                      <AlertTriangle className="w-3 h-3 shrink-0" />
                      <span className="truncate">{s}</span>
                    </div>
                  ))}
                </div>
              </details>
            )}

          {/* Success actions */}
          {(phase === "generated" || phase === "applied") && (
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <button
                type="button"
                onClick={() => void handleCopyRebuildCommand()}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                           bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                           hover:bg-cyan-500/20 transition-colors"
              >
                <Copy className="w-3.5 h-3.5" />
                {rebuildCopied ? "Copied!" : "Copy rebuild command"}
              </button>
              {phase === "generated" && (
                <span className="text-[11px] text-muted-foreground">
                  Files are ready — expand Advanced below to review and apply.
                </span>
              )}
            </div>
          )}

          {/* Failure actions */}
          {phase === "failed" && (
            <div className="mt-3 flex flex-wrap items-center gap-2">
              {failedSteps.length > 0 && (
                <button
                  type="button"
                  onClick={retryFailed}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                             bg-yellow-500/10 text-yellow-400 border border-yellow-500/20
                             hover:bg-yellow-500/20 transition-colors"
                >
                  <RefreshCw className="w-3.5 h-3.5" />
                  Retry failed ({failedSteps.length})
                </button>
              )}
              <button
                type="button"
                onClick={() => void runPipeline(projectPath.trim())}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                           bg-white/5 text-muted-foreground border border-border
                           hover:bg-white/10 transition-colors"
              >
                <RefreshCw className="w-3.5 h-3.5" />
                Run again
              </button>
            </div>
          )}
        </div>
      )}

      {/* ----- Zero-pages graceful state ----- */}
      {phase === "no-pages" && (
        <div className="p-4 rounded-lg border border-yellow-500/30 bg-yellow-500/5">
          <div className="flex items-center gap-2 mb-2">
            <AlertTriangle className="w-4 h-4 text-yellow-400" />
            <h3 className="text-sm font-medium">No pages discovered</h3>
          </div>
          <p className="text-xs text-muted-foreground mb-3">
            The Integration tool doesn't yet recognize this framework
            {analysis?.framework ? ` (${analysis.framework})` : ""}. The SDK is installed, but
            automatic page discovery couldn't find any route components to register. You can still
            use Advanced below to inspect the analysis, or report this project so we can add
            support.
          </p>
          <div className="flex flex-wrap gap-2">
            <a
              href="https://github.com/jspinak/qontinui/issues/new?title=Page+discovery+couldn%27t+find+pages&labels=ui-bridge"
              target="_blank"
              rel="noreferrer"
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-yellow-500/10 text-yellow-400 border border-yellow-500/20
                         hover:bg-yellow-500/20 transition-colors"
            >
              <ExternalLink className="w-3.5 h-3.5" />
              Report issue
            </a>
            <button
              type="button"
              onClick={() => void runPipeline(projectPath.trim())}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-white/5 text-muted-foreground border border-border
                         hover:bg-white/10 transition-colors"
            >
              <RefreshCw className="w-3.5 h-3.5" />
              Try again
            </button>
          </div>
        </div>
      )}

      {/* ----- Edit list modal ----- */}
      {editListOpen && (
        <EditProjectsModal
          projects={projects}
          onRemove={removeProject}
          onClose={() => setEditListOpen(false)}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Phase banner — tiny presentational helper so the main component stays lean.
// ---------------------------------------------------------------------------

function PhaseBanner({
  phase,
  progressMessage,
  filesGenerated,
  filesWritten,
  failedCount,
  errorMessage,
}: {
  phase: CoordinatorPhase;
  progressMessage: string;
  filesGenerated: number;
  filesWritten: number;
  failedCount: number;
  errorMessage: string | null;
}) {
  if (phase === "applied") {
    return (
      <div className="flex items-start gap-2">
        <CheckCircle2 className="w-4 h-4 text-green-400 shrink-0 mt-0.5" />
        <div>
          <p className="text-sm font-medium text-green-400">
            Integrated {filesWritten} file{filesWritten !== 1 ? "s" : ""}. Rebuild the project to
            apply.
          </p>
          <p className="text-[11px] text-muted-foreground mt-0.5">
            Run <code className="px-1 rounded bg-white/10">npm run build</code> in the project
            directory, or copy the command with the button below.
          </p>
        </div>
      </div>
    );
  }
  if (phase === "generated") {
    return (
      <div className="flex items-start gap-2">
        <CheckCircle2 className="w-4 h-4 text-cyan-400 shrink-0 mt-0.5" />
        <div>
          <p className="text-sm font-medium">
            Generated {filesGenerated} file{filesGenerated !== 1 ? "s" : ""}
            {failedCount > 0 ? ` — ${failedCount} failed` : ""}.
          </p>
          <p className="text-[11px] text-muted-foreground mt-0.5">
            Review and apply from the Advanced section below, then rebuild.
          </p>
        </div>
      </div>
    );
  }
  if (phase === "failed") {
    return (
      <div className="flex items-start gap-2">
        <AlertTriangle className="w-4 h-4 text-red-400 shrink-0 mt-0.5" />
        <div>
          <p className="text-sm font-medium text-red-400">
            Integration failed{failedCount > 0 ? ` (${failedCount} pages)` : ""}
          </p>
          {errorMessage && <p className="text-[11px] text-red-400/80 mt-0.5">{errorMessage}</p>}
        </div>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-2">
      <Loader2 className="w-4 h-4 text-cyan-400 animate-spin shrink-0" />
      <p className="text-sm font-medium">{progressMessage || "Working..."}</p>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Edit list modal — removal-only. Keeps surface minimal (<60 lines).
// ---------------------------------------------------------------------------

function EditProjectsModal({
  projects,
  onRemove,
  onClose,
}: {
  projects: SavedProject[];
  onRemove: (path: string) => Promise<void>;
  onClose: () => void;
}) {
  const [removing, setRemoving] = useState<string | null>(null);

  const handleRemove = async (path: string) => {
    setRemoving(path);
    try {
      await onRemove(path);
    } catch (err) {
      console.warn("[ProjectCoordinator] Remove failed:", err);
    } finally {
      setRemoving(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md rounded-lg border border-border bg-card p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-medium">Saved projects</h3>
          <button
            type="button"
            onClick={onClose}
            className="text-muted-foreground hover:text-foreground"
            aria-label="Close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        {projects.length === 0 ? (
          <p className="text-xs text-muted-foreground">No saved projects yet.</p>
        ) : (
          <ul className="flex flex-col gap-1 max-h-72 overflow-y-auto">
            {projects.map((p) => (
              <li
                key={p.path}
                className="flex items-center gap-2 p-2 rounded border border-border bg-white/5"
              >
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium truncate">{p.name}</p>
                  <p className="text-[10px] text-muted-foreground truncate">{p.path}</p>
                </div>
                <button
                  type="button"
                  onClick={() => void handleRemove(p.path)}
                  disabled={removing === p.path}
                  className="px-2 py-1 text-xs rounded bg-red-500/10 text-red-400 border border-red-500/20
                             hover:bg-red-500/20 disabled:opacity-50 transition-colors"
                >
                  {removing === p.path ? "Removing..." : "Remove"}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
