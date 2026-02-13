/**
 * AppComparisonWizard.tsx
 *
 * 4-step wizard for comparing two UI Bridge-enabled applications:
 *
 *   1. Scan & Select — discover apps, assign Reference and Target
 *   2. Configure — set routes, description, comparison mode
 *   3. Snapshots — take and preview side-by-side snapshots
 *   4. Results — AI-generated comparison with filterable differences
 */

import { useState, useCallback } from "react";
import {
  Search,
  Settings,
  Layers,
  BarChart3,
  ArrowRight,
  ArrowLeft,
  CheckCircle2,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useAppDiscovery, type DiscoveredApp } from "@/hooks/useAppDiscovery";
import { AppSelector } from "./AppSelector";
import { SnapshotPreview, type SnapshotSummary, type DiffStats } from "./SnapshotPreview";
import { ComparisonResults, type ComparisonResult } from "./ComparisonResults";

// Use 127.0.0.1 to force IPv4 (runner only listens on IPv4)
const API_BASE = "http://127.0.0.1:9876";

// ============================================================================
// Types
// ============================================================================

type WizardStep = 0 | 1 | 2 | 3;
type ComparisonMode = "structural" | "visual" | "both";

interface AppComparisonWizardProps {
  onLog?: (level: string, message: string) => void;
}

const STEP_CONFIG = [
  { label: "Scan Apps", icon: Search },
  { label: "Configure", icon: Settings },
  { label: "Snapshots", icon: Layers },
  { label: "Results", icon: BarChart3 },
] as const;

// ============================================================================
// Helpers
// ============================================================================

function summarizeSnapshot(
  snapshot: Record<string, unknown>,
  appName: string,
  url: string,
): SnapshotSummary {
  const elements = (snapshot.elements as Array<Record<string, unknown>>) || [];
  const components = (snapshot.components as unknown[]) || [];

  const interactiveCount = elements.filter(
    (el) => (el.actions as string[] | undefined)?.length && el.visible,
  ).length;

  const elementTypes: Record<string, number> = {};
  for (const el of elements) {
    const t = (el.type as string) || (el.tagName as string) || "unknown";
    elementTypes[t] = (elementTypes[t] || 0) + 1;
  }

  return {
    appName,
    url,
    elementCount: elements.length,
    componentCount: components.length,
    interactiveCount,
    elementTypes,
  };
}

function computeDiffStats(
  refSnapshot: Record<string, unknown>,
  targetSnapshot: Record<string, unknown>,
): DiffStats {
  const refElements = (refSnapshot.elements as Array<Record<string, unknown>>) || [];
  const targetElements = (targetSnapshot.elements as Array<Record<string, unknown>>) || [];

  // Compare by element id
  const refIds = new Set(refElements.map((el) => el.id as string));
  const targetIds = new Set(targetElements.map((el) => el.id as string));

  let inBoth = 0;
  let onlyInReference = 0;
  let onlyInTarget = 0;

  for (const id of refIds) {
    if (targetIds.has(id)) inBoth++;
    else onlyInReference++;
  }
  for (const id of targetIds) {
    if (!refIds.has(id)) onlyInTarget++;
  }

  return { onlyInReference, onlyInTarget, inBoth };
}

// ============================================================================
// Component
// ============================================================================

export function AppComparisonWizard({ onLog }: AppComparisonWizardProps) {
  // Wizard state
  const [step, setStep] = useState<WizardStep>(0);

  // Step 1: App selection
  const { webApps, desktopApps, isScanning, error: scanError, scanAll } = useAppDiscovery();
  const [referenceApp, setReferenceApp] = useState<DiscoveredApp | null>(null);
  const [targetApp, setTargetApp] = useState<DiscoveredApp | null>(null);

  // SDK connection tracking for discovered apps
  const [connectedUrls, setConnectedUrls] = useState<Set<string>>(new Set());

  // Step 2: Configuration
  const [referenceRoute, setReferenceRoute] = useState("");
  const [targetRoute, setTargetRoute] = useState("");
  const [description, setDescription] = useState("");
  const [comparisonMode, setComparisonMode] = useState<ComparisonMode>("structural");

  // Step 3: Snapshots
  const [referenceSnapshot, setReferenceSnapshot] = useState<Record<string, unknown> | null>(null);
  const [targetSnapshot, setTargetSnapshot] = useState<Record<string, unknown> | null>(null);
  const [referenceSummary, setReferenceSummary] = useState<SnapshotSummary | null>(null);
  const [targetSummary, setTargetSummary] = useState<SnapshotSummary | null>(null);
  const [diffStats, setDiffStats] = useState<DiffStats | null>(null);
  const [isTakingSnapshots, setIsTakingSnapshots] = useState(false);

  // Step 4: Results
  const [comparisonResult, setComparisonResult] = useState<ComparisonResult | null>(null);
  const [isComparing, setIsComparing] = useState(false);
  const [compareError, setCompareError] = useState<string | null>(null);

  // =========================================================================
  // SDK Connect (from discovery results)
  // =========================================================================

  const handleConnectApp = useCallback(
    async (app: DiscoveredApp): Promise<boolean> => {
      const url = app.url.replace(/\/+$/, "");
      onLog?.("info", `Connecting to ${app.appName} at ${url}...`);

      try {
        const resp = await fetch(`${API_BASE}/ui-bridge/sdk/connect`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            url,
            appId: app.appId,
            appName: app.appName,
            appType: app.appType,
            framework: app.framework,
            port: app.port,
          }),
        });

        const json = await resp.json();
        if (!json.success) {
          const msg = json.error || "Connection failed";
          onLog?.("error", `Failed to connect to ${app.appName}: ${msg}`);
          return false;
        }

        setConnectedUrls((prev) => new Set([...prev, url]));
        onLog?.("success", `Connected to ${app.appName}`);
        return true;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        onLog?.("error", `Failed to connect to ${app.appName}: ${msg}`);
        return false;
      }
    },
    [onLog],
  );

  // =========================================================================
  // Navigation
  // =========================================================================

  const canAdvance = useCallback((): boolean => {
    switch (step) {
      case 0:
        return referenceApp != null && targetApp != null;
      case 1:
        return true; // configuration is optional
      case 2:
        return referenceSnapshot != null && targetSnapshot != null;
      case 3:
        return false; // last step
      default:
        return false;
    }
  }, [step, referenceApp, targetApp, referenceSnapshot, targetSnapshot]);

  const goNext = () => setStep((s) => Math.min(s + 1, 3) as WizardStep);
  const goBack = () => setStep((s) => Math.max(s - 1, 0) as WizardStep);

  // =========================================================================
  // SDK Connection & Snapshot helpers
  // =========================================================================

  const connectAndSnapshot = useCallback(
    async (app: DiscoveredApp, route: string): Promise<Record<string, unknown> | null> => {
      const baseUrl = app.url.replace(/\/+$/, "");
      const connectUrl = route ? `${baseUrl}${route}` : baseUrl;

      // Connect
      const connectResp = await fetch(`${API_BASE}/ui-bridge/sdk/connect`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          url: connectUrl,
          appId: app.appId,
          appName: app.appName,
          appType: app.appType,
          framework: app.framework,
          port: app.port,
        }),
      });
      const connectJson = await connectResp.json();
      if (!connectJson.success) {
        throw new Error(connectJson.error || `Failed to connect to ${app.appName}`);
      }

      // Switch to this connection (in case another was active)
      await fetch(`${API_BASE}/ui-bridge/sdk/switch`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: connectUrl }),
      });

      // Take snapshot
      const snapResp = await fetch(`${API_BASE}/ui-bridge/sdk/snapshot`);
      const snapJson = await snapResp.json();
      if (snapJson.success === false) {
        throw new Error(snapJson.error || `Failed to take snapshot of ${app.appName}`);
      }

      return (snapJson.data || snapJson) as Record<string, unknown>;
    },
    [],
  );

  // =========================================================================
  // Step 3: Take Snapshots
  // =========================================================================

  const handleTakeSnapshots = useCallback(async () => {
    if (!referenceApp || !targetApp) return;

    setIsTakingSnapshots(true);
    setReferenceSnapshot(null);
    setTargetSnapshot(null);
    setReferenceSummary(null);
    setTargetSummary(null);
    setDiffStats(null);

    try {
      // Snapshot reference app
      onLog?.("info", `Taking snapshot of reference: ${referenceApp.appName}...`);
      const refSnap = await connectAndSnapshot(referenceApp, referenceRoute);
      if (!refSnap) throw new Error("Reference snapshot returned null");
      setReferenceSnapshot(refSnap);

      const refUrl = referenceRoute ? `${referenceApp.url}${referenceRoute}` : referenceApp.url;
      setReferenceSummary(summarizeSnapshot(refSnap, referenceApp.appName, refUrl));

      // Snapshot target app
      onLog?.("info", `Taking snapshot of target: ${targetApp.appName}...`);
      const tgtSnap = await connectAndSnapshot(targetApp, targetRoute);
      if (!tgtSnap) throw new Error("Target snapshot returned null");
      setTargetSnapshot(tgtSnap);

      const tgtUrl = targetRoute ? `${targetApp.url}${targetRoute}` : targetApp.url;
      setTargetSummary(summarizeSnapshot(tgtSnap, targetApp.appName, tgtUrl));

      // Compute diff stats
      setDiffStats(computeDiffStats(refSnap, tgtSnap));

      onLog?.("success", "Snapshots taken successfully");
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      onLog?.("error", `Snapshot failed: ${msg}`);
    } finally {
      setIsTakingSnapshots(false);
    }
  }, [referenceApp, targetApp, referenceRoute, targetRoute, connectAndSnapshot, onLog]);

  // =========================================================================
  // Step 4: Compare with AI
  // =========================================================================

  const handleCompare = useCallback(async () => {
    if (!referenceSnapshot || !targetSnapshot) return;

    setIsComparing(true);
    setCompareError(null);
    setComparisonResult(null);

    try {
      onLog?.("info", "Comparing snapshots with AI...");

      const resp = await fetch(`${API_BASE}/ai/compare-snapshots`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          reference_snapshot: referenceSnapshot,
          target_snapshot: targetSnapshot,
          comparison_mode: comparisonMode,
          user_prompt: description || null,
        }),
      });

      const json = await resp.json();
      if (!json.success) {
        throw new Error(json.error || "Comparison failed");
      }

      const data = json.data;
      const specs: ComparisonResult["specs"] = data.specs || [];
      const result: ComparisonResult = {
        specs,
        summary: data.summary || "Comparison complete.",
        totalDifferences: specs.length,
        criticalCount: specs.filter((s: { severity: string }) => s.severity === "critical").length,
        majorCount: specs.filter((s: { severity: string }) => s.severity === "major").length,
        minorCount: specs.filter((s: { severity: string }) => s.severity === "minor").length,
        infoCount: specs.filter((s: { severity: string }) => s.severity === "info").length,
      };

      setComparisonResult(result);
      setStep(3);
      onLog?.("success", `Found ${specs.length} differences`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setCompareError(msg);
      onLog?.("error", `Comparison failed: ${msg}`);
    } finally {
      setIsComparing(false);
    }
  }, [referenceSnapshot, targetSnapshot, comparisonMode, description, onLog]);

  // =========================================================================
  // Render
  // =========================================================================

  return (
    <div className="flex h-full flex-col bg-surface-canvas">
      {/* Step indicator */}
      <div className="flex items-center gap-1 border-b border-border px-4 py-3">
        {STEP_CONFIG.map((s, i) => {
          const Icon = s.icon;
          const isActive = step === i;
          const isCompleted = step > i;
          return (
            <div key={i} className="flex items-center gap-1">
              {i > 0 && (
                <div className={cn("h-px w-8", isCompleted ? "bg-brand-primary" : "bg-border")} />
              )}
              <button
                onClick={() => setStep(i as WizardStep)}
                disabled={i > step + 1}
                className={cn(
                  "flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                  isActive
                    ? "bg-brand-primary/10 text-brand-primary"
                    : isCompleted
                      ? "text-brand-primary/70 hover:bg-brand-primary/5"
                      : "text-text-muted",
                  i > step + 1 && "opacity-40 cursor-not-allowed",
                )}
              >
                {isCompleted ? (
                  <CheckCircle2 className="h-3.5 w-3.5" />
                ) : (
                  <Icon className="h-3.5 w-3.5" />
                )}
                {s.label}
              </button>
            </div>
          );
        })}
      </div>

      {/* Step content */}
      <div className="flex-1 overflow-y-auto scrollbar-dark">
        <div className="mx-auto max-w-4xl p-6">
          {step === 0 && (
            <div>
              <h2 className="text-lg font-semibold text-text-primary mb-1">
                Scan & Select Applications
              </h2>
              <p className="text-sm text-text-muted mb-6">
                Discover UI Bridge-enabled apps running locally, then assign one as Reference (gold
                standard) and one as Target (to compare against).
              </p>
              <AppSelector
                webApps={webApps}
                desktopApps={desktopApps}
                referenceApp={referenceApp}
                targetApp={targetApp}
                onAssignReference={(app) => {
                  // If this app was already the target, swap
                  if (targetApp?.appId === app.appId) setTargetApp(null);
                  setReferenceApp(app);
                }}
                onAssignTarget={(app) => {
                  if (referenceApp?.appId === app.appId) setReferenceApp(null);
                  setTargetApp(app);
                }}
                isScanning={isScanning}
                error={scanError}
                onScan={scanAll}
                onConnect={handleConnectApp}
                connectedUrls={connectedUrls}
              />
            </div>
          )}

          {step === 1 && (
            <div>
              <h2 className="text-lg font-semibold text-text-primary mb-1">Configure Comparison</h2>
              <p className="text-sm text-text-muted mb-6">
                Set routes and describe what to focus the comparison on.
              </p>

              <div className="space-y-4">
                {/* Route inputs */}
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="mb-1.5 block text-xs font-medium text-text-muted">
                      Reference Route
                    </label>
                    <input
                      type="text"
                      value={referenceRoute}
                      onChange={(e) => setReferenceRoute(e.target.value)}
                      placeholder="e.g. /runs/123"
                      className="w-full rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none"
                    />
                    <p className="mt-1 text-[10px] text-text-muted">
                      {referenceApp?.appName} — {referenceApp?.url}
                    </p>
                  </div>
                  <div>
                    <label className="mb-1.5 block text-xs font-medium text-text-muted">
                      Target Route
                    </label>
                    <input
                      type="text"
                      value={targetRoute}
                      onChange={(e) => setTargetRoute(e.target.value)}
                      placeholder="e.g. /runs/123"
                      className="w-full rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none"
                    />
                    <p className="mt-1 text-[10px] text-text-muted">
                      {targetApp?.appName} — {targetApp?.url}
                    </p>
                  </div>
                </div>

                {/* Description */}
                <div>
                  <label className="mb-1.5 block text-xs font-medium text-text-muted">
                    Description / Instructions
                  </label>
                  <textarea
                    value={description}
                    onChange={(e) => setDescription(e.target.value)}
                    placeholder="What should the comparison focus on? e.g. 'Compare the sidebar navigation and make sure all items match'"
                    rows={4}
                    className="w-full resize-none rounded-lg border border-border bg-surface-raised px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:border-brand-primary/50 focus:outline-none"
                  />
                </div>

                {/* Comparison mode */}
                <div>
                  <label className="mb-1.5 block text-xs font-medium text-text-muted">
                    Comparison Mode
                  </label>
                  <div className="flex gap-2">
                    {(["structural", "visual", "both"] as ComparisonMode[]).map((mode) => (
                      <button
                        key={mode}
                        onClick={() => setComparisonMode(mode)}
                        className={cn(
                          "flex-1 rounded-lg border py-2.5 text-sm font-medium transition-colors capitalize",
                          comparisonMode === mode
                            ? "border-brand-primary bg-brand-primary/10 text-brand-primary"
                            : "border-border text-text-muted hover:bg-surface-hover hover:text-text-primary",
                        )}
                      >
                        {mode}
                      </button>
                    ))}
                  </div>
                  <p className="mt-1.5 text-[10px] text-text-muted">
                    {comparisonMode === "structural" &&
                      "Compare element structure, hierarchy, and properties"}
                    {comparisonMode === "visual" &&
                      "Compare layout positioning and visual appearance"}
                    {comparisonMode === "both" && "Full comparison: structure + layout"}
                  </p>
                </div>
              </div>
            </div>
          )}

          {step === 2 && (
            <div>
              <h2 className="text-lg font-semibold text-text-primary mb-1">Snapshot Preview</h2>
              <p className="text-sm text-text-muted mb-6">
                Connect to both apps and take UI snapshots for comparison.
              </p>

              <div className="space-y-4">
                {!referenceSnapshot && !targetSnapshot && !isTakingSnapshots && (
                  <button
                    onClick={handleTakeSnapshots}
                    className="w-full flex items-center justify-center gap-2 rounded-lg bg-brand-primary px-4 py-3 text-sm font-medium text-white hover:bg-brand-primary/90 transition-colors"
                  >
                    <Layers className="h-4 w-4" />
                    Take Snapshots
                  </button>
                )}

                <SnapshotPreview
                  referenceSnapshot={referenceSnapshot}
                  targetSnapshot={targetSnapshot}
                  referenceSummary={referenceSummary}
                  targetSummary={targetSummary}
                  diffStats={diffStats}
                  isLoading={isTakingSnapshots}
                />

                {referenceSnapshot && targetSnapshot && (
                  <div className="flex gap-3">
                    <button
                      onClick={handleTakeSnapshots}
                      disabled={isTakingSnapshots}
                      className="flex items-center gap-2 rounded-lg border border-border px-4 py-2.5 text-sm font-medium text-text-muted hover:bg-surface-hover transition-colors disabled:opacity-50"
                    >
                      Retake
                    </button>
                    <button
                      onClick={handleCompare}
                      disabled={isComparing}
                      className="flex-1 flex items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-brand-primary to-brand-primary/80 py-2.5 text-sm font-medium text-white hover:from-brand-primary/90 hover:to-brand-primary/70 disabled:opacity-50 transition-all"
                    >
                      Compare with AI
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}

          {step === 3 && (
            <div>
              <h2 className="text-lg font-semibold text-text-primary mb-1">Comparison Results</h2>
              <p className="text-sm text-text-muted mb-6">
                AI-generated differences between the reference and target applications.
              </p>
              <ComparisonResults
                result={comparisonResult}
                isComparing={isComparing}
                error={compareError}
                onGenerateWorkflow={(specIds) => {
                  onLog?.("info", `Would generate workflow from ${specIds.length} specs`);
                  // Future: call /ai/generate-test with spec IDs
                }}
              />
            </div>
          )}
        </div>
      </div>

      {/* Navigation footer */}
      <div className="flex items-center justify-between border-t border-border px-6 py-3">
        <button
          onClick={goBack}
          disabled={step === 0}
          className="flex items-center gap-1.5 rounded-lg px-4 py-2 text-sm font-medium text-text-muted hover:bg-surface-hover disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
        >
          <ArrowLeft className="h-4 w-4" />
          Back
        </button>

        <div className="text-xs text-text-muted">Step {step + 1} of 4</div>

        {step < 3 && (
          <button
            onClick={goNext}
            disabled={!canAdvance()}
            className="flex items-center gap-1.5 rounded-lg bg-brand-primary px-4 py-2 text-sm font-medium text-white hover:bg-brand-primary/90 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
          >
            Next
            <ArrowRight className="h-4 w-4" />
          </button>
        )}
        {step === 3 && <div />}
      </div>
    </div>
  );
}

export default AppComparisonWizard;
