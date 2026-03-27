/**
 * DevelopmentIntelligencePage
 *
 * Main page with three sub-panels for coverage gap analysis,
 * complexity scoring, and feature health detection.
 * All analysis works offline — reads spec files, test files,
 * component source, and git history without a running app.
 */

import { useState, useCallback, useEffect } from "react";
import {
  RefreshCw,
  Shield,
  Layers,
  Activity,
  Brain,
  Navigation,
} from "lucide-react";
import { CoverageGapPanel } from "./CoverageGapPanel";
import { ComplexityDashboard } from "./ComplexityDashboard";
import { FeatureHealthPanel } from "./FeatureHealthPanel";
import { NavigationFlowGraph } from "./NavigationFlowGraph";
import type { CoverageAnalysisResult } from "@/lib/development-intelligence/coverage-analyzer";
import type { ComplexityAnalysisResult } from "@/lib/development-intelligence/complexity-scorer";
import type { FeatureHealthResult } from "@/lib/development-intelligence/dead-feature-detector";

type SubPanel = "coverage" | "complexity" | "health" | "navigation";

const API_BASE = "http://localhost:9876";

const PANELS: Array<{
  id: SubPanel;
  label: string;
  icon: typeof Shield;
  description: string;
}> = [
  {
    id: "coverage",
    label: "Coverage Gaps",
    icon: Shield,
    description: "Spec vs test coverage analysis",
  },
  {
    id: "complexity",
    label: "Complexity & Drift",
    icon: Layers,
    description: "Page complexity scoring and drift detection",
  },
  {
    id: "health",
    label: "Feature Health",
    icon: Activity,
    description: "Dead feature and staleness detection",
  },
  {
    id: "navigation",
    label: "Nav Flow",
    icon: Navigation,
    description: "Page navigation flow graph from spec data",
  },
];

async function getProjectPath(): Promise<string> {
  try {
    const response = await fetch(`${API_BASE}/settings`);
    const data = await response.json();
    return data.data?.project_path ?? "";
  } catch {
    return "";
  }
}

export function DevelopmentIntelligencePage() {
  const [activePanel, setActivePanel] = useState<SubPanel>("coverage");
  const [projectPath, setProjectPath] = useState<string>("");

  // Data states
  const [coverageData, setCoverageData] =
    useState<CoverageAnalysisResult | null>(null);
  const [complexityData, setComplexityData] =
    useState<ComplexityAnalysisResult | null>(null);
  const [healthData, setHealthData] = useState<FeatureHealthResult | null>(
    null,
  );

  // Loading states
  const [coverageLoading, setCoverageLoading] = useState(false);
  const [complexityLoading, setComplexityLoading] = useState(false);
  const [healthLoading, setHealthLoading] = useState(false);

  useEffect(() => {
    getProjectPath().then(setProjectPath);
  }, []);

  const fetchCoverage = useCallback(async () => {
    if (!projectPath) return;
    setCoverageLoading(true);
    try {
      const res = await fetch(
        `${API_BASE}/development-intelligence/coverage-analysis`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ project_path: projectPath }),
        },
      );
      const json = await res.json();
      if (json.data) setCoverageData(json.data);
    } catch (err) {
      console.error("Coverage analysis failed:", err);
    } finally {
      setCoverageLoading(false);
    }
  }, [projectPath]);

  const fetchComplexity = useCallback(async () => {
    if (!projectPath) return;
    setComplexityLoading(true);
    try {
      const res = await fetch(
        `${API_BASE}/development-intelligence/complexity-scores`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ project_path: projectPath }),
        },
      );
      const json = await res.json();
      if (json.data) setComplexityData(json.data);
    } catch (err) {
      console.error("Complexity analysis failed:", err);
    } finally {
      setComplexityLoading(false);
    }
  }, [projectPath]);

  const fetchHealth = useCallback(async () => {
    if (!projectPath) return;
    setHealthLoading(true);
    try {
      const res = await fetch(
        `${API_BASE}/development-intelligence/feature-health`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ project_path: projectPath }),
        },
      );
      const json = await res.json();
      if (json.data) setHealthData(json.data);
    } catch (err) {
      console.error("Feature health analysis failed:", err);
    } finally {
      setHealthLoading(false);
    }
  }, [projectPath]);

  const refreshCurrent = useCallback(() => {
    switch (activePanel) {
      case "coverage":
        fetchCoverage();
        break;
      case "complexity":
        fetchComplexity();
        break;
      case "health":
        fetchHealth();
        break;
      case "navigation":
        // Navigation flow loads its own data internally
        break;
    }
  }, [activePanel, fetchCoverage, fetchComplexity, fetchHealth]);

  const refreshAll = useCallback(() => {
    fetchCoverage();
    fetchComplexity();
    fetchHealth();
  }, [fetchCoverage, fetchComplexity, fetchHealth]);

  const isAnyLoading = coverageLoading || complexityLoading || healthLoading;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 py-3 border-b bg-card">
        <Brain className="w-5 h-5 text-purple-500" />
        <h1 className="text-sm font-semibold">Development Intelligence</h1>

        {/* Panel tabs */}
        <div className="flex items-center gap-0.5 ml-4">
          {PANELS.map((panel) => {
            const Icon = panel.icon;
            const isActive = activePanel === panel.id;
            return (
              <button
                key={panel.id}
                className={`flex items-center gap-1.5 px-3 py-1.5 text-xs rounded-md transition-colors ${
                  isActive
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:bg-muted"
                }`}
                onClick={() => setActivePanel(panel.id)}
                title={panel.description}
              >
                <Icon className="w-3.5 h-3.5" />
                {panel.label}
              </button>
            );
          })}
        </div>

        <div className="ml-auto flex items-center gap-2">
          {projectPath && (
            <span className="text-xs text-muted-foreground truncate max-w-[200px]">
              {projectPath}
            </span>
          )}
          <button
            className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-muted hover:bg-muted/80 transition-colors"
            onClick={refreshCurrent}
            disabled={isAnyLoading}
          >
            <RefreshCw
              className={`w-3.5 h-3.5 ${isAnyLoading ? "animate-spin" : ""}`}
            />
            Refresh
          </button>
          <button
            className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-primary/10 text-primary hover:bg-primary/20 transition-colors"
            onClick={refreshAll}
            disabled={isAnyLoading}
          >
            Analyze All
          </button>
        </div>
      </div>

      {/* Active panel */}
      <div className="flex-1 min-h-0">
        {activePanel === "coverage" && (
          <CoverageGapPanel
            data={coverageData}
            loading={coverageLoading}
            onRefresh={fetchCoverage}
          />
        )}
        {activePanel === "complexity" && (
          <ComplexityDashboard
            data={complexityData}
            loading={complexityLoading}
            onRefresh={fetchComplexity}
          />
        )}
        {activePanel === "health" && (
          <FeatureHealthPanel
            data={healthData}
            loading={healthLoading}
            onRefresh={fetchHealth}
          />
        )}
        {activePanel === "navigation" && <NavigationFlowGraph />}
      </div>
    </div>
  );
}
