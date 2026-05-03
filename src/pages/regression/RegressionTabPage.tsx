/**
 * RegressionTabPage — Section 11, Phase D2 wire-up.
 *
 * Top-level container for the runner's "Regression" main tab. Hosts a small
 * sub-tab control that switches between:
 *   - "Run"      — mounts {@link RegressionRunPage}, the integration smoke
 *                  surface that walks a hardcoded suite against the live
 *                  registry.
 *   - "Coverage" — mounts {@link CoveragePanel} once a suite + IR are
 *                  available. The runner currently has no Tauri command
 *                  to list persisted suites, so this panel renders an
 *                  "execute a run first" empty state until that data
 *                  loading is wired up.
 *
 * Deferred (tracked for follow-up):
 *   - There is no `list_regression_suites` / `get_suite_by_id` Tauri command
 *     yet. When one lands (mirroring `query_assertion_executions_for_suite`),
 *     replace the empty-state in the Coverage sub-tab with a real fetch and
 *     mount {@link CoveragePanel} with the loaded suite + IR.
 *   - The IR document for a suite must also be loaded — IR storage already
 *     exists via `spec_api::storage::read_ir`, but no TS-side Tauri command
 *     surfaces it yet for arbitrary `ir_doc_id`s.
 */

import { useState } from "react";
import { Activity, Play } from "lucide-react";

import { RegressionRunPage } from "./RegressionRunPage";

type SubTab = "run" | "coverage";

const SUB_TABS: ReadonlyArray<{ id: SubTab; label: string; icon: React.ComponentType<{ className?: string }> }> = [
  { id: "run", label: "Run", icon: Play },
  { id: "coverage", label: "Coverage", icon: Activity },
];

export function RegressionTabPage(): React.JSX.Element {
  const [activeSubTab, setActiveSubTab] = useState<SubTab>("run");

  return (
    <div className="flex h-full flex-col">
      {/* Sub-tab control */}
      <div
        role="tablist"
        aria-label="Regression sub-tabs"
        className="flex items-center gap-1 border-b border-border-primary px-4"
      >
        {SUB_TABS.map((t) => {
          const Icon = t.icon;
          const isActive = t.id === activeSubTab;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={isActive}
              onClick={() => setActiveSubTab(t.id)}
              className={[
                "inline-flex items-center gap-1.5 px-3 py-2 text-sm font-medium border-b-2 -mb-px",
                isActive
                  ? "border-brand-primary text-text-primary"
                  : "border-transparent text-text-muted hover:text-text-primary",
              ].join(" ")}
            >
              <Icon className="size-3.5" />
              {t.label}
            </button>
          );
        })}
      </div>

      {/* Sub-tab content */}
      <div className="flex-1 overflow-hidden">
        {activeSubTab === "run" && <RegressionRunPage />}
        {activeSubTab === "coverage" && <CoverageEmptyState />}
      </div>
    </div>
  );
}

/**
 * Placeholder for the Coverage sub-tab.
 *
 * `CoveragePanel` requires a loaded `RegressionSuite` + `IRDocument` as
 * props, but the runner does not yet expose Tauri commands to list
 * persisted suites or load arbitrary IR documents from the suite store.
 * Until those land, render a friendly empty state explaining the
 * dependency.
 */
function CoverageEmptyState(): React.JSX.Element {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="max-w-md rounded-lg border border-border-primary bg-surface-secondary p-6 text-center">
        <Activity className="mx-auto mb-3 size-8 text-text-muted" />
        <h3 className="mb-2 text-base font-semibold text-text-primary">
          No suite loaded
        </h3>
        <p className="text-sm text-text-muted">
          Coverage analysis needs a persisted regression suite to load. Run
          the demo suite from the <span className="font-medium">Run</span>{" "}
          sub-tab first; once the runner exposes a{" "}
          <code className="rounded bg-surface-primary px-1 py-0.5 text-xs">
            list_regression_suites
          </code>{" "}
          command, this view will surface live coverage data automatically.
        </p>
      </div>
    </div>
  );
}

export default RegressionTabPage;
