/**
 * AiConfigureChecksModal.tsx
 *
 * Modal for AI-assisted check configuration.
 * Scans workspace to detect projects and generates appropriate check configurations.
 */

import { useState, useCallback, useMemo } from "react";
import {
  Sparkles,
  X,
  Loader2,
  Check,
  AlertCircle,
  FolderSearch,
  ChevronDown,
  ChevronUp,
  Folder,
  Settings2,
  CheckSquare,
  Square,
  MinusSquare,
} from "lucide-react";
import type { CreateCheckInput } from "./types";
import { getAccentColors } from "@/design-system";
import { open } from "@tauri-apps/plugin-dialog";
import { useCheckBuilder } from "./CheckBuilderContext";

// Color palette for auto-generated groups
const GROUP_COLORS = [
  "#8B5CF6", // Purple
  "#3B82F6", // Blue
  "#10B981", // Emerald
  "#F59E0B", // Amber
  "#EF4444", // Red
  "#EC4899", // Pink
  "#06B6D4", // Cyan
  "#84CC16", // Lime
];

const API_BASE = "http://localhost:9876";

// Types matching the Rust backend
interface DetectedProject {
  path: string;
  relative_path: string;
  languages: string[];
  config_files: string[];
  name: string;
}

interface WorkspaceScanResult {
  projects: DetectedProject[];
  total_directories_scanned: number;
  base_directory: string;
}

interface SuggestedCheckWithReason {
  check: CreateCheckInput;
  reason: string;
  project_path: string;
}

interface GenerateChecksResponse {
  suggested_checks: SuggestedCheckWithReason[];
  success: boolean;
  error?: string;
}

interface UserPreferences {
  prefer_auto_fix: boolean;
  include_security: boolean;
  include_formatting: boolean;
  include_type_checking: boolean;
}

type ModalStep = "input" | "scanning" | "generating" | "review";

interface AiConfigureChecksModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** @deprecated The modal now creates checks and groups directly via context. This prop is kept for backward compatibility. */
  onChecksCreated?: (checks: CreateCheckInput[]) => Promise<void>;
}

export function AiConfigureChecksModal({ isOpen, onClose }: AiConfigureChecksModalProps) {
  // Get context for creating groups
  const { createCheck, createCheckGroup, setChecksInGroup, loadChecks, loadCheckGroups } =
    useCheckBuilder();

  // Step state
  const [step, setStep] = useState<ModalStep>("input");
  const [error, setError] = useState<string | null>(null);

  // Input state
  const [baseDirectory, setBaseDirectory] = useState("");
  const [maxDepth, setMaxDepth] = useState(2);
  const [showAdvanced, setShowAdvanced] = useState(false);

  // Preferences
  const [preferAutoFix, setPreferAutoFix] = useState(true);
  const [includeSecurity, setIncludeSecurity] = useState(true);
  const [includeFormatting, setIncludeFormatting] = useState(true);
  const [includeTypeChecking, setIncludeTypeChecking] = useState(true);

  // Scan results
  const [scanResult, setScanResult] = useState<WorkspaceScanResult | null>(null);

  // Generation results
  const [suggestedChecks, setSuggestedChecks] = useState<SuggestedCheckWithReason[]>([]);
  const [selectedChecks, setSelectedChecks] = useState<Set<number>>(new Set());

  // Creating state
  const [isCreating, setIsCreating] = useState(false);

  const accentColors = getAccentColors("purple");

  // Select directory
  const handleSelectDirectory = useCallback(async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Select Workspace Directory",
      });
      if (selected) {
        setBaseDirectory(selected as string);
      }
    } catch (err) {
      console.error("Failed to open directory picker:", err);
    }
  }, []);

  // Generate checks (defined before handleScan since handleScan depends on it)
  const handleGenerate = useCallback(
    async (scan: WorkspaceScanResult) => {
      setStep("generating");
      setError(null);

      try {
        const preferences: UserPreferences = {
          prefer_auto_fix: preferAutoFix,
          include_security: includeSecurity,
          include_formatting: includeFormatting,
          include_type_checking: includeTypeChecking,
        };

        const response = await fetch(`${API_BASE}/checks/generate`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            workspace_scan: scan,
            user_preferences: preferences,
          }),
        });

        const result = await response.json();

        if (!response.ok) {
          throw new Error(result.error || `HTTP ${response.status}`);
        }

        const data: GenerateChecksResponse = result.data || result;

        if (!data.success) {
          throw new Error(data.error || "Generation failed");
        }

        setSuggestedChecks(data.suggested_checks);
        // Select all by default
        setSelectedChecks(new Set(data.suggested_checks.map((_, i) => i)));
        setStep("review");
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to generate checks");
        setStep("input");
      }
    },
    [preferAutoFix, includeSecurity, includeFormatting, includeTypeChecking],
  );

  // Scan workspace
  const handleScan = useCallback(async () => {
    if (!baseDirectory) {
      setError("Please select a directory to scan");
      return;
    }

    setStep("scanning");
    setError(null);

    try {
      const response = await fetch(`${API_BASE}/checks/scan-workspace`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          base_directory: baseDirectory,
          max_depth: maxDepth,
        }),
      });

      const result = await response.json();

      if (!response.ok) {
        throw new Error(result.error || `HTTP ${response.status}`);
      }

      const data: WorkspaceScanResult = result.data || result;
      setScanResult(data);

      if (data.projects.length === 0) {
        setError("No projects found in the selected directory");
        setStep("input");
        return;
      }

      // Automatically generate checks
      await handleGenerate(data);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to scan workspace");
      setStep("input");
    }
  }, [baseDirectory, maxDepth, handleGenerate]);

  // Toggle check selection
  const toggleCheckSelection = useCallback((index: number) => {
    setSelectedChecks((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  }, []);

  // Select/deselect all
  const toggleAll = useCallback(() => {
    if (selectedChecks.size === suggestedChecks.length) {
      setSelectedChecks(new Set());
    } else {
      setSelectedChecks(new Set(suggestedChecks.map((_, i) => i)));
    }
  }, [selectedChecks.size, suggestedChecks]);

  // Group checks by project
  const checksByProject = useMemo(() => {
    const groups: Map<string, { indices: number[]; projectName: string; languages: string[] }> =
      new Map();

    suggestedChecks.forEach((suggestion, index) => {
      const projectPath = suggestion.project_path;
      if (!groups.has(projectPath)) {
        // Find the project in scan results to get name and languages
        const project = scanResult?.projects.find((p) => p.path === projectPath);
        groups.set(projectPath, {
          indices: [],
          projectName: project?.name || projectPath.split(/[/\\]/).pop() || projectPath,
          languages: project?.languages || [],
        });
      }
      groups.get(projectPath)!.indices.push(index);
    });

    return groups;
  }, [suggestedChecks, scanResult]);

  // Toggle all checks for a project
  const toggleProject = useCallback(
    (projectPath: string) => {
      const projectInfo = checksByProject.get(projectPath);
      if (!projectInfo) return;

      const allSelected = projectInfo.indices.every((i) => selectedChecks.has(i));

      setSelectedChecks((prev) => {
        const next = new Set(prev);
        if (allSelected) {
          // Deselect all in this project
          projectInfo.indices.forEach((i) => next.delete(i));
        } else {
          // Select all in this project
          projectInfo.indices.forEach((i) => next.add(i));
        }
        return next;
      });
    },
    [checksByProject, selectedChecks],
  );

  // Check if all checks in a project are selected
  const isProjectFullySelected = useCallback(
    (projectPath: string) => {
      const projectInfo = checksByProject.get(projectPath);
      if (!projectInfo) return false;
      return projectInfo.indices.every((i) => selectedChecks.has(i));
    },
    [checksByProject, selectedChecks],
  );

  // Check if some (but not all) checks in a project are selected
  const isProjectPartiallySelected = useCallback(
    (projectPath: string) => {
      const projectInfo = checksByProject.get(projectPath);
      if (!projectInfo) return false;
      const selectedCount = projectInfo.indices.filter((i) => selectedChecks.has(i)).length;
      return selectedCount > 0 && selectedCount < projectInfo.indices.length;
    },
    [checksByProject, selectedChecks],
  );

  // Reset and close (defined before handleCreateChecks since handleCreateChecks depends on it)
  const handleClose = useCallback(() => {
    setStep("input");
    setError(null);
    setBaseDirectory("");
    setMaxDepth(2);
    setScanResult(null);
    setSuggestedChecks([]);
    setSelectedChecks(new Set());
    setShowAdvanced(false);
    onClose();
  }, [onClose]);

  // Create selected checks and groups
  const handleCreateChecks = useCallback(async () => {
    const selectedSuggestions = suggestedChecks.filter((_, i) => selectedChecks.has(i));

    if (selectedSuggestions.length === 0) {
      setError("Please select at least one check to create");
      return;
    }

    setIsCreating(true);
    setError(null);

    try {
      // Group selected suggestions by project path
      const suggestionsByProject = new Map<
        string,
        { suggestion: SuggestedCheckWithReason; index: number }[]
      >();
      selectedSuggestions.forEach((suggestion, idx) => {
        const projectPath = suggestion.project_path;
        if (!suggestionsByProject.has(projectPath)) {
          suggestionsByProject.set(projectPath, []);
        }
        suggestionsByProject.get(projectPath)!.push({ suggestion, index: idx });
      });

      // Create checks first, tracking which check belongs to which project
      const checkIdsByProject = new Map<string, string[]>();

      for (const [projectPath, suggestions] of suggestionsByProject) {
        const checkIds: string[] = [];
        for (const { suggestion } of suggestions) {
          const createdCheck = await createCheck(suggestion.check);
          if (createdCheck) {
            checkIds.push(createdCheck.id);
          }
        }
        checkIdsByProject.set(projectPath, checkIds);
      }

      // Create groups for each project and assign checks
      let colorIndex = 0;
      for (const [projectPath, checkIds] of checkIdsByProject) {
        if (checkIds.length === 0) continue;

        // Get project info for the group name
        const project = scanResult?.projects.find((p) => p.path === projectPath);
        const projectName = project?.name || projectPath.split(/[/\\]/).pop() || projectPath;
        const languages = project?.languages || [];

        // Create the group
        const group = await createCheckGroup({
          name: projectName,
          description: `Auto-generated group for ${projectName}${languages.length > 0 ? ` (${languages.join(", ")})` : ""}`,
          color: GROUP_COLORS[colorIndex % GROUP_COLORS.length],
          enabled: true,
          run_in_parallel: false,
          stop_on_failure: false,
          tags: languages,
        });

        // Assign checks to the group
        if (group) {
          await setChecksInGroup(group.id, checkIds);
        }

        colorIndex++;
      }

      // Reload checks and groups to show all new ones
      await loadChecks();
      await loadCheckGroups();

      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to create checks");
    } finally {
      setIsCreating(false);
    }
  }, [
    suggestedChecks,
    selectedChecks,
    scanResult,
    createCheck,
    createCheckGroup,
    setChecksInGroup,
    loadChecks,
    loadCheckGroups,
    handleClose,
  ]);

  // Go back
  const handleBack = useCallback(() => {
    if (step === "review") {
      setStep("input");
      setSuggestedChecks([]);
      setSelectedChecks(new Set());
    }
  }, [step]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={handleClose} />

      {/* Dialog */}
      <div
        data-ui-id="dialog-ai-configure-checks"
        className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-3xl mx-4 max-h-[85vh] flex flex-col overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div className="flex items-center gap-2">
            <Sparkles className={`w-5 h-5 ${accentColors.text}`} />
            <h3 className="text-lg font-semibold">Detect Project & Suggest Checks</h3>
          </div>
          <button
            onClick={handleClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
            data-ui-id="check-builder-ai-modal-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6">
          {/* Error Alert */}
          {error && (
            <div className="mb-4 p-3 bg-red-500/10 border border-red-500/30 rounded-md flex items-start gap-2">
              <AlertCircle className="w-5 h-5 text-red-500 flex-shrink-0 mt-0.5" />
              <p className="text-sm text-red-400">{error}</p>
            </div>
          )}

          {/* Step: Input */}
          {step === "input" && (
            <div className="space-y-4">
              <p className="text-sm text-muted-foreground">
                Select a workspace directory to scan for projects. The AI will detect project types
                and suggest appropriate code quality checks.
              </p>

              {/* Directory Selection */}
              <div>
                <label className="block text-sm font-medium mb-1.5">Workspace Directory</label>
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={baseDirectory}
                    onChange={(e) => setBaseDirectory(e.target.value)}
                    placeholder="Select or enter directory path..."
                    className="flex-1 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-purple-500/50"
                    data-ui-id="check-builder-ai-directory-input"
                  />
                  <button
                    onClick={handleSelectDirectory}
                    className="px-3 py-2 bg-purple-900/30 hover:bg-purple-900/50 border border-purple-700/30 rounded-md flex items-center gap-2 transition-colors"
                    data-ui-id="check-builder-ai-browse-btn"
                  >
                    <FolderSearch className="w-4 h-4 text-purple-400" />
                    <span className="text-sm text-purple-300">Browse</span>
                  </button>
                </div>
              </div>

              {/* Scan Depth */}
              <div>
                <label className="block text-sm font-medium mb-1.5">Scan Depth</label>
                <select
                  value={maxDepth}
                  onChange={(e) => setMaxDepth(Number(e.target.value))}
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-purple-500/50"
                  data-ui-id="check-builder-ai-scan-depth-select"
                >
                  <option value={1}>1 level (immediate subdirectories only)</option>
                  <option value={2}>2 levels (recommended for monorepos)</option>
                  <option value={3}>3 levels (deep scan)</option>
                </select>
                <p className="text-xs text-muted-foreground mt-1">
                  Higher depth finds more projects but takes longer
                </p>
              </div>

              {/* Advanced Options Toggle */}
              <button
                onClick={() => setShowAdvanced(!showAdvanced)}
                className="flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
              >
                <Settings2 className="w-4 h-4" />
                <span>Advanced Options</span>
                {showAdvanced ? (
                  <ChevronUp className="w-4 h-4" />
                ) : (
                  <ChevronDown className="w-4 h-4" />
                )}
              </button>

              {/* Advanced Options */}
              {showAdvanced && (
                <div className="pl-6 space-y-3 border-l-2 border-border">
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={preferAutoFix}
                      onChange={(e) => setPreferAutoFix(e.target.checked)}
                      className="w-4 h-4 rounded border-border"
                      data-ui-id="check-builder-ai-prefer-autofix-checkbox"
                    />
                    <span className="text-sm">Prefer auto-fix when available</span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={includeFormatting}
                      onChange={(e) => setIncludeFormatting(e.target.checked)}
                      className="w-4 h-4 rounded border-border"
                      data-ui-id="check-builder-ai-include-formatting-checkbox"
                    />
                    <span className="text-sm">Include formatting checks</span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={includeTypeChecking}
                      onChange={(e) => setIncludeTypeChecking(e.target.checked)}
                      className="w-4 h-4 rounded border-border"
                      data-ui-id="check-builder-ai-include-typecheck-checkbox"
                    />
                    <span className="text-sm">Include type checking</span>
                  </label>
                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={includeSecurity}
                      onChange={(e) => setIncludeSecurity(e.target.checked)}
                      className="w-4 h-4 rounded border-border"
                      data-ui-id="check-builder-ai-include-security-checkbox"
                    />
                    <span className="text-sm">Include security checks</span>
                  </label>
                </div>
              )}
            </div>
          )}

          {/* Step: Scanning */}
          {step === "scanning" && (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <Loader2 className="w-8 h-8 text-purple-400 animate-spin mb-4" />
              <p className="text-sm text-muted-foreground">Scanning workspace for projects...</p>
              <p className="text-xs text-muted-foreground mt-1">{baseDirectory}</p>
            </div>
          )}

          {/* Step: Generating */}
          {step === "generating" && (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <Sparkles className="w-8 h-8 text-purple-400 animate-pulse mb-4" />
              <p className="text-sm text-muted-foreground">Generating check suggestions...</p>
              {scanResult && (
                <p className="text-xs text-muted-foreground mt-1">
                  Analyzing {scanResult.projects.length} detected project
                  {scanResult.projects.length !== 1 ? "s" : ""}
                </p>
              )}
            </div>
          )}

          {/* Step: Review */}
          {step === "review" && (
            <div className="space-y-4">
              {/* Selection Header */}
              <div className="flex items-center justify-between">
                <p className="text-sm text-muted-foreground">
                  {selectedChecks.size} of {suggestedChecks.length} check
                  {suggestedChecks.length !== 1 ? "s" : ""} selected
                  {" · "}
                  {checksByProject.size} project{checksByProject.size !== 1 ? "s" : ""}
                </p>
                <button
                  onClick={toggleAll}
                  className="text-sm text-purple-400 hover:text-purple-300 transition-colors"
                >
                  {selectedChecks.size === suggestedChecks.length ? "Deselect All" : "Select All"}
                </button>
              </div>

              {/* Grouped Check List */}
              <div className="space-y-3 max-h-[50vh] overflow-y-auto">
                {Array.from(checksByProject.entries()).map(([projectPath, projectInfo]) => (
                  <ProjectCheckGroup
                    key={projectPath}
                    projectPath={projectPath}
                    projectName={projectInfo.projectName}
                    languages={projectInfo.languages}
                    checks={projectInfo.indices.map((i) => ({
                      index: i,
                      suggestion: suggestedChecks[i],
                    }))}
                    selectedChecks={selectedChecks}
                    isFullySelected={isProjectFullySelected(projectPath)}
                    isPartiallySelected={isProjectPartiallySelected(projectPath)}
                    onToggleProject={() => toggleProject(projectPath)}
                    onToggleCheck={toggleCheckSelection}
                  />
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-6 py-4 border-t border-border">
          <div>
            {step === "review" && (
              <button
                onClick={handleBack}
                className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
                data-ui-id="check-builder-ai-back-btn"
              >
                ← Back
              </button>
            )}
          </div>
          <div className="flex items-center gap-3">
            <button
              onClick={handleClose}
              className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
              data-ui-id="check-builder-ai-cancel-btn"
            >
              Cancel
            </button>
            {step === "input" && (
              <button
                onClick={handleScan}
                disabled={!baseDirectory}
                className={`px-4 py-2 rounded-md text-sm font-medium transition-colors flex items-center gap-2 ${
                  baseDirectory
                    ? `${accentColors.bgSolid} text-white hover:opacity-90`
                    : "bg-neutral-700 text-neutral-400 cursor-not-allowed"
                }`}
                data-ui-id="check-builder-ai-scan-btn"
              >
                <FolderSearch className="w-4 h-4" />
                Scan & Generate
              </button>
            )}
            {step === "review" && (
              <button
                onClick={handleCreateChecks}
                disabled={selectedChecks.size === 0 || isCreating}
                className={`px-4 py-2 rounded-md text-sm font-medium transition-colors flex items-center gap-2 ${
                  selectedChecks.size > 0 && !isCreating
                    ? `${accentColors.bgSolid} text-white hover:opacity-90`
                    : "bg-neutral-700 text-neutral-400 cursor-not-allowed"
                }`}
                data-ui-id="check-builder-ai-create-btn"
              >
                {isCreating ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Creating...
                  </>
                ) : (
                  <>
                    <Check className="w-4 h-4" />
                    Create {selectedChecks.size} Check{selectedChecks.size !== 1 ? "s" : ""}
                  </>
                )}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// Sub-component for displaying a suggested check
interface SuggestedCheckCardProps {
  suggestion: SuggestedCheckWithReason;
  isSelected: boolean;
  onToggle: () => void;
}

function _SuggestedCheckCard({ suggestion, isSelected, onToggle }: SuggestedCheckCardProps) {
  const { check, reason, project_path } = suggestion;

  // Get language color
  const getLanguageColor = (tool: string) => {
    if (["black", "ruff", "mypy", "pyright", "isort"].includes(tool)) {
      return "text-blue-400 bg-blue-900/20";
    }
    if (["eslint", "prettier", "tsc", "biome"].includes(tool)) {
      return "text-yellow-400 bg-yellow-900/20";
    }
    if (["clippy", "rustfmt", "cargo_check"].includes(tool)) {
      return "text-orange-400 bg-orange-900/20";
    }
    return "text-gray-400 bg-gray-900/20";
  };

  // Get check type color
  const getTypeColor = (type: string) => {
    switch (type) {
      case "lint":
        return "text-amber-400";
      case "format":
        return "text-cyan-400";
      case "typecheck":
        return "text-purple-400";
      case "security":
        return "text-red-400";
      default:
        return "text-gray-400";
    }
  };

  return (
    <button
      onClick={onToggle}
      className={`w-full p-3 rounded-lg border transition-colors text-left ${
        isSelected
          ? "bg-purple-900/20 border-purple-700/50"
          : "bg-card border-border hover:border-purple-700/30"
      }`}
    >
      <div className="flex items-start gap-3">
        {/* Checkbox */}
        <div className="mt-0.5">
          {isSelected ? (
            <CheckSquare className="w-5 h-5 text-purple-400" />
          ) : (
            <Square className="w-5 h-5 text-muted-foreground" />
          )}
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm">{check.name}</span>
            <span className={`px-1.5 py-0.5 rounded text-xs ${getLanguageColor(check.tool)}`}>
              {check.tool}
            </span>
            <span className={`text-xs ${getTypeColor(check.check_type)}`}>{check.check_type}</span>
          </div>

          <p className="text-xs text-muted-foreground mt-1 line-clamp-1">{reason}</p>

          <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <Folder className="w-3 h-3" />
              <span className="truncate max-w-48" title={project_path}>
                {project_path.split(/[/\\]/).pop() || project_path}
              </span>
            </span>
            {check.auto_fix && (
              <span className="text-emerald-400">
                <Settings2 className="w-3 h-3 inline" /> auto-fix
              </span>
            )}
          </div>
        </div>
      </div>
    </button>
  );
}

// Project group component - displays a collapsible group of checks for a project
interface ProjectCheckGroupProps {
  projectPath: string;
  projectName: string;
  languages: string[];
  checks: Array<{ index: number; suggestion: SuggestedCheckWithReason }>;
  selectedChecks: Set<number>;
  isFullySelected: boolean;
  isPartiallySelected: boolean;
  onToggleProject: () => void;
  onToggleCheck: (index: number) => void;
}

function ProjectCheckGroup({
  projectPath: _projectPath,
  projectName,
  languages,
  checks,
  selectedChecks,
  isFullySelected,
  isPartiallySelected,
  onToggleProject,
  onToggleCheck,
}: ProjectCheckGroupProps) {
  const [isExpanded, setIsExpanded] = useState(true);

  const selectedCount = checks.filter((c) => selectedChecks.has(c.index)).length;

  // Get language color for badge
  const getLanguageBadgeColor = (lang: string) => {
    switch (lang) {
      case "python":
        return "bg-blue-900/30 text-blue-400";
      case "typescript":
      case "javascript":
        return "bg-yellow-900/30 text-yellow-400";
      case "rust":
        return "bg-orange-900/30 text-orange-400";
      default:
        return "bg-gray-900/30 text-gray-400";
    }
  };

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      {/* Project Header */}
      <div className="bg-neutral-800/50 px-3 py-2 flex items-center gap-2">
        {/* Selection checkbox */}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleProject();
          }}
          className="flex-shrink-0"
        >
          {isFullySelected ? (
            <CheckSquare className="w-5 h-5 text-purple-400" />
          ) : isPartiallySelected ? (
            <MinusSquare className="w-5 h-5 text-purple-400" />
          ) : (
            <Square className="w-5 h-5 text-muted-foreground" />
          )}
        </button>

        {/* Expand/collapse toggle */}
        <button
          onClick={() => setIsExpanded(!isExpanded)}
          className="flex-1 flex items-center gap-2 text-left"
        >
          <Folder className="w-4 h-4 text-purple-400 flex-shrink-0" />
          <span className="font-medium text-sm truncate">{projectName}</span>

          {/* Language badges */}
          <div className="flex gap-1 flex-shrink-0">
            {languages.map((lang) => (
              <span
                key={lang}
                className={`px-1.5 py-0.5 rounded text-xs ${getLanguageBadgeColor(lang)}`}
              >
                {lang}
              </span>
            ))}
          </div>

          {/* Selection count */}
          <span className="text-xs text-muted-foreground ml-auto flex-shrink-0">
            {selectedCount}/{checks.length} selected
          </span>

          {/* Chevron */}
          {isExpanded ? (
            <ChevronUp className="w-4 h-4 text-muted-foreground flex-shrink-0" />
          ) : (
            <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
          )}
        </button>
      </div>

      {/* Checks List */}
      {isExpanded && (
        <div className="divide-y divide-border">
          {checks.map(({ index, suggestion }) => (
            <CompactCheckItem
              key={index}
              suggestion={suggestion}
              isSelected={selectedChecks.has(index)}
              onToggle={() => onToggleCheck(index)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// Compact check item for use inside project groups
interface CompactCheckItemProps {
  suggestion: SuggestedCheckWithReason;
  isSelected: boolean;
  onToggle: () => void;
}

function CompactCheckItem({ suggestion, isSelected, onToggle }: CompactCheckItemProps) {
  const { check, reason: _reason } = suggestion;

  // Get check type color
  const getTypeColor = (type: string) => {
    switch (type) {
      case "lint":
        return "text-amber-400";
      case "format":
        return "text-cyan-400";
      case "typecheck":
        return "text-purple-400";
      case "security":
        return "text-red-400";
      default:
        return "text-gray-400";
    }
  };

  // Get tool color
  const getToolColor = (tool: string) => {
    if (["black", "ruff", "mypy", "pyright", "isort"].includes(tool)) {
      return "text-blue-400 bg-blue-900/20";
    }
    if (["eslint", "prettier", "tsc", "biome"].includes(tool)) {
      return "text-yellow-400 bg-yellow-900/20";
    }
    if (["clippy", "rustfmt", "cargo_check"].includes(tool)) {
      return "text-orange-400 bg-orange-900/20";
    }
    return "text-gray-400 bg-gray-900/20";
  };

  return (
    <button
      onClick={onToggle}
      className={`w-full px-3 py-2 flex items-center gap-3 text-left transition-colors ${
        isSelected ? "bg-purple-900/10" : "hover:bg-neutral-800/30"
      }`}
    >
      {/* Checkbox */}
      <div className="flex-shrink-0">
        {isSelected ? (
          <CheckSquare className="w-4 h-4 text-purple-400" />
        ) : (
          <Square className="w-4 h-4 text-muted-foreground" />
        )}
      </div>

      {/* Check info */}
      <div className="flex-1 min-w-0 flex items-center gap-2">
        <span className={`px-1.5 py-0.5 rounded text-xs ${getToolColor(check.tool)}`}>
          {check.tool}
        </span>
        <span className={`text-xs ${getTypeColor(check.check_type)}`}>{check.check_type}</span>
        <span className="text-sm truncate">
          {check.name.replace(`${suggestion.project_path.split(/[/\\]/).pop()} - `, "")}
        </span>
      </div>

      {/* Auto-fix badge */}
      {check.auto_fix && <span className="text-xs text-emerald-400 flex-shrink-0">auto-fix</span>}
    </button>
  );
}
