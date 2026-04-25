import { useState, useCallback } from "react";
import { Plug, Settings2 } from "lucide-react";
import { SourceIntegrationPanel } from "./SourceIntegrationPanel";
import { DiscoveryPanel } from "./DiscoveryPanel";
import { ProjectCoordinator } from "./ProjectCoordinator";
import { useUIBridgeIntegrationPageRegistrations } from "@/lib/ui-bridge/pages/uibridgeintegrationpage-registrations";

/**
 * Top-level UI Bridge Integration page.
 *
 * Layout (Phase 3 redesign):
 *   - Header
 *   - ProjectCoordinator — project dropdown + one-click "Integrate this
 *     Project" CTA. Drives the whole pipeline with smart defaults.
 *   - Advanced disclosure — the legacy per-stage UI
 *     (SourceIntegrationPanel + DiscoveryPanel) for power users who want
 *     to pick specific pages, toggle specs/tutorials/videos, or inspect
 *     stage-by-stage output. Closed by default.
 *
 * The two sections share a single `projectPath` so selecting a project in
 * the dropdown pre-fills the Advanced per-stage panels, and selecting an
 * app card in Advanced's DiscoveryPanel updates the coordinator.
 */
export function UIBridgeIntegrationPage() {
  const { advancedDisclosureRef } = useUIBridgeIntegrationPageRegistrations();
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | undefined>();

  const handleSelectApp = useCallback((basePath: string) => {
    setSelectedProjectPath(basePath);
    // Scroll to the integration panel at the top
    window.scrollTo({ top: 0, behavior: "smooth" });
  }, []);

  const handleCoordinatorPathChange = useCallback((path: string) => {
    setSelectedProjectPath(path || undefined);
  }, []);

  return (
    <div className="h-full flex flex-col p-4 gap-6 overflow-auto">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Plug className="w-5 h-5 text-cyan-400" />
        <h1 className="text-lg font-semibold">UI Bridge</h1>
        <span className="text-xs px-1.5 py-0.5 rounded bg-cyan-500/20 text-cyan-400 border border-cyan-500/30 font-medium">
          integration
        </span>
      </div>

      {/* Primary action — project dropdown + one-click CTA */}
      <ProjectCoordinator
        initialProjectPath={selectedProjectPath}
        onProjectPathChange={handleCoordinatorPathChange}
      />

      {/* Advanced: per-stage controls (legacy power-user UI).
          Closed by default. Its state is preserved across open/close because
          the inner panels hold their own state; <details> just toggles
          visibility, it does not remount. */}
      <details className="group rounded-lg border border-border bg-card/30">
        <summary
          ref={advancedDisclosureRef}
          className="flex items-center gap-2 p-3 cursor-pointer text-sm font-medium select-none list-none"
        >
          <Settings2 className="w-4 h-4 text-muted-foreground group-open:text-cyan-400 transition-colors" />
          <span>Advanced: per-stage controls</span>
          <span className="ml-auto text-[10px] text-muted-foreground">
            Pick specific pages, toggle specs / tutorials / videos, inspect each stage
          </span>
        </summary>
        <div className="flex flex-col gap-6 p-4 border-t border-border">
          {/* Source Integration — per-stage analyze / integrate / preview */}
          <SourceIntegrationPanel initialProjectPath={selectedProjectPath} />

          {/* Discovery — scan for running apps */}
          <DiscoveryPanel onSelectApp={handleSelectApp} selectedProjectPath={selectedProjectPath} />
        </div>
      </details>
    </div>
  );
}
