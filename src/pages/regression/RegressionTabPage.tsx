/**
 * RegressionTabPage — Section 11, Phase D2 wire-up + FU-2 follow-up.
 *
 * Top-level container for the runner's "Regression" main tab. Hosts a small
 * sub-tab control that switches between:
 *   - "Run"      — mounts {@link RegressionRunPage}, the integration smoke
 *                  surface that walks a hardcoded suite against the live
 *                  registry.
 *   - "Coverage" — picks a persisted suite via the FU-2 Tauri commands
 *                  ({@link CoverageSuitePicker}), loads the IR via the
 *                  Spec API, and mounts {@link CoveragePanel}.
 */

import { useMemo, useState } from "react";
import { Activity, Loader2, Play, RefreshCw } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";

import {
  deserializeSuite,
  type RegressionSuite,
} from "@qontinui/ui-bridge-auto/regression";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";

import { CoveragePanel } from "./CoveragePanel";
import { RegressionRunPage } from "./RegressionRunPage";

type SubTab = "run" | "coverage";

const SUB_TABS: ReadonlyArray<{
  id: SubTab;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
}> = [
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
        {activeSubTab === "coverage" && <CoverageSubTab />}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Coverage sub-tab — picks a suite via FU-2 commands, loads IR, mounts panel
// ---------------------------------------------------------------------------

/** Catalog row returned by `list_regression_suites`. Mirrors the Rust shape. */
interface SuiteCatalogRow {
  id: string;
  ir_doc_id: string;
  created_at: string;
  run_count: number;
  last_run_at: string | null;
}

/** Full-suite row returned by `get_regression_suite_by_id`. */
interface SuiteFullRow {
  id: string;
  ir_doc_id: string;
  suite_json: unknown;
  created_at: string;
}

function CoverageSubTab(): React.JSX.Element {
  const {
    data: suites,
    isLoading: suitesLoading,
    error: suitesError,
    refetch: refetchSuites,
  } = useQuery<SuiteCatalogRow[]>({
    queryKey: ["regression", "list_regression_suites"],
    queryFn: () =>
      invoke<SuiteCatalogRow[]>("list_regression_suites", { limit: 50 }),
    refetchOnWindowFocus: false,
  });

  // User selection (null = follow the auto-selected newest suite).
  const [explicitSelection, setExplicitSelection] = useState<string | null>(null);
  // Effective selection: user pick if any, else the newest suite from the
  // server query (which is sorted by `created_at DESC`). Derived rather
  // than stored — avoids the setState-in-effect pattern.
  const selectedSuiteId =
    explicitSelection ?? (suites && suites.length > 0 ? suites[0].id : null);

  if (suitesLoading) return <CenteredSpinner label="Loading suites..." />;

  if (suitesError) {
    return (
      <CenteredCard
        title="Failed to load suites"
        description={suitesError instanceof Error ? suitesError.message : String(suitesError)}
        action={{ label: "Retry", onClick: () => void refetchSuites() }}
      />
    );
  }

  if (!suites || suites.length === 0) {
    return (
      <CenteredCard
        title="No suites recorded yet"
        description="Run the regression suite from the Run sub-tab first; persisted suites will appear here automatically."
        action={{ label: "Reload", onClick: () => void refetchSuites() }}
      />
    );
  }

  return (
    <div className="flex h-full flex-col">
      <CoverageSuitePicker
        suites={suites}
        selectedSuiteId={selectedSuiteId}
        onSelect={setExplicitSelection}
        onRefresh={() => void refetchSuites()}
      />
      <div className="flex-1 overflow-auto">
        {selectedSuiteId !== null && <SuiteCoverageLoader suiteId={selectedSuiteId} />}
      </div>
    </div>
  );
}

interface PickerProps {
  suites: SuiteCatalogRow[];
  selectedSuiteId: string | null;
  onSelect: (id: string) => void;
  onRefresh: () => void;
}

function CoverageSuitePicker({
  suites,
  selectedSuiteId,
  onSelect,
  onRefresh,
}: PickerProps): React.JSX.Element {
  return (
    <div className="flex items-center gap-3 border-b border-border-primary px-4 py-3">
      <label
        htmlFor="suite-select"
        className="text-sm font-medium text-text-muted"
      >
        Suite:
      </label>
      <select
        id="suite-select"
        value={selectedSuiteId ?? ""}
        onChange={(e) => onSelect(e.target.value)}
        className="min-w-[18rem] rounded-md border border-border-primary bg-surface-primary px-2 py-1 text-sm text-text-primary"
      >
        {suites.map((s) => (
          <option key={s.id} value={s.id}>
            {s.ir_doc_id} — {s.run_count} run{s.run_count === 1 ? "" : "s"}
            {s.last_run_at ? ` (last ${formatTimestamp(s.last_run_at)})` : ""}
          </option>
        ))}
      </select>
      <button
        type="button"
        onClick={onRefresh}
        className="ml-auto inline-flex items-center gap-1.5 rounded-md border border-border-primary px-2 py-1 text-xs text-text-muted hover:bg-surface-secondary hover:text-text-primary"
      >
        <RefreshCw className="size-3" />
        Refresh
      </button>
    </div>
  );
}

function SuiteCoverageLoader({
  suiteId,
}: {
  suiteId: string;
}): React.JSX.Element {
  const {
    data: full,
    isLoading,
    error,
  } = useQuery<SuiteFullRow | null>({
    queryKey: ["regression", "get_regression_suite_by_id", suiteId],
    queryFn: () =>
      invoke<SuiteFullRow | null>("get_regression_suite_by_id", { suiteId }),
    refetchOnWindowFocus: false,
  });

  // Suite JSON parses on every render, but `useMemo` keys to the row stops
  // it from re-running unless the suite actually changes.
  const parsed = useMemo(() => {
    if (!full?.suite_json) return null;
    try {
      const json =
        typeof full.suite_json === "string"
          ? full.suite_json
          : JSON.stringify(full.suite_json);
      return deserializeSuite(json) as RegressionSuite;
    } catch {
      return null;
    }
  }, [full]);

  // The IR document for the suite is loaded via the Spec API (HTTP). The
  // CoveragePanel only needs the IR shape — no Tauri command exists for
  // arbitrary `ir_doc_id` lookups, but the runner exposes the IR over its
  // local HTTP API at `/spec/page/:id`. Use that.
  const {
    data: ir,
    isLoading: irLoading,
    error: irError,
  } = useQuery<IRDocument | null>({
    queryKey: ["regression", "ir-for-suite", full?.ir_doc_id],
    enabled: typeof full?.ir_doc_id === "string" && full.ir_doc_id.length > 0,
    queryFn: async () => {
      const irDocId = full!.ir_doc_id;
      const resp = await fetch(
        `http://localhost:9876/spec/page/${encodeURIComponent(irDocId)}`,
      );
      if (!resp.ok) {
        throw new Error(`spec/page returned ${resp.status}`);
      }
      const json = (await resp.json()) as { success?: boolean; data?: IRDocument };
      if (json.success === false || !json.data) {
        throw new Error("ir document missing in response");
      }
      return json.data;
    },
    refetchOnWindowFocus: false,
  });

  if (isLoading || irLoading)
    return <CenteredSpinner label="Loading suite + IR..." />;

  if (error)
    return (
      <CenteredCard
        title="Failed to load suite"
        description={error instanceof Error ? error.message : String(error)}
      />
    );

  if (!parsed)
    return (
      <CenteredCard
        title="Suite blob unparseable"
        description="The persisted suite JSON could not be parsed by deserializeSuite. The schema may have drifted; re-run the suite to refresh."
      />
    );

  if (irError || !ir)
    return (
      <CenteredCard
        title="Failed to load IR document"
        description={
          irError instanceof Error
            ? irError.message
            : "The IR document for this suite is missing from the Spec API. The suite may reference an IR that has been deleted."
        }
      />
    );

  return <CoveragePanel suiteId={suiteId} suite={parsed} ir={ir} />;
}

// ---------------------------------------------------------------------------
// Tiny shared empty/error/spinner helpers
// ---------------------------------------------------------------------------

function CenteredSpinner({ label }: { label: string }): React.JSX.Element {
  return (
    <div className="flex h-full items-center justify-center text-text-muted">
      <Loader2 className="mr-2 size-4 animate-spin" />
      <span className="text-sm">{label}</span>
    </div>
  );
}

interface CenteredCardProps {
  title: string;
  description: string;
  action?: { label: string; onClick: () => void };
}

function CenteredCard({
  title,
  description,
  action,
}: CenteredCardProps): React.JSX.Element {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="max-w-md rounded-lg border border-border-primary bg-surface-secondary p-6 text-center">
        <Activity className="mx-auto mb-3 size-8 text-text-muted" />
        <h3 className="mb-2 text-base font-semibold text-text-primary">
          {title}
        </h3>
        <p className="text-sm text-text-muted">{description}</p>
        {action && (
          <button
            type="button"
            onClick={action.onClick}
            className="mt-4 inline-flex items-center gap-1.5 rounded-md border border-border-primary px-3 py-1.5 text-sm text-text-primary hover:bg-surface-primary"
          >
            {action.label}
          </button>
        )}
      </div>
    </div>
  );
}

function formatTimestamp(iso: string): string {
  // Compact "MM-DD HH:MM" form to fit the suite picker. Locale-agnostic so
  // the rendered string is stable across machines.
  try {
    const d = new Date(iso);
    const pad = (n: number): string => String(n).padStart(2, "0");
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  } catch {
    return iso;
  }
}

export default RegressionTabPage;
