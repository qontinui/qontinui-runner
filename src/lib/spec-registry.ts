/**
 * spec-registry.ts
 *
 * Static registry of all bundled page specs for the runner.
 * Imports spec JSON files from the web frontend so they're available
 * without runtime UI Bridge SDK discovery.
 */

import type { DiscoveredSpec } from "./spec-prompt-builder";

// Import all spec JSON files from the web frontend
import chatSpec from "../../../qontinui-web/frontend/src/app/(app)/chat/chat.spec.uibridge.json";
import inspectorSpec from "../../../qontinui-web/frontend/src/app/(app)/tools/inspector/inspector.spec.uibridge.json";
import workflowsSpec from "../../../qontinui-web/frontend/src/app/(app)/build/workflows/workflows.spec.uibridge.json";
import templatesSpec from "../../../qontinui-web/frontend/src/app/(app)/build/templates/templates.spec.uibridge.json";
import executeSpec from "../../../qontinui-web/frontend/src/app/(app)/execute/execute.spec.uibridge.json";
import runsSpec from "../../../qontinui-web/frontend/src/app/(app)/runs/runs.spec.uibridge.json";
import activeRunsSpec from "../../../qontinui-web/frontend/src/app/(app)/runs/active/active-runs.spec.uibridge.json";
import runnersSpec from "../../../qontinui-web/frontend/src/app/(app)/runners/runners.spec.uibridge.json";
import errorMonitorSpec from "../../../qontinui-web/frontend/src/app/(app)/tools/error-monitor/error-monitor.spec.uibridge.json";
import aiSettingsSpec from "../../../qontinui-web/frontend/src/app/(app)/settings/ai/ai-settings.spec.uibridge.json";
import contextsSpec from "../../../qontinui-web/frontend/src/app/(app)/build/contexts/contexts.spec.uibridge.json";
import findingsSpec from "../../../qontinui-web/frontend/src/app/(app)/runs/findings/findings.spec.uibridge.json";
import statisticsSpec from "../../../qontinui-web/frontend/src/app/(app)/runs/statistics/statistics.spec.uibridge.json";

interface RawSpec {
  specId: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  json: any;
}

const ALL_SPECS: RawSpec[] = [
  { specId: "chat", json: chatSpec },
  { specId: "inspector", json: inspectorSpec },
  { specId: "workflows", json: workflowsSpec },
  { specId: "templates", json: templatesSpec },
  { specId: "execute", json: executeSpec },
  { specId: "runs", json: runsSpec },
  { specId: "active-runs", json: activeRunsSpec },
  { specId: "runners", json: runnersSpec },
  { specId: "error-monitor", json: errorMonitorSpec },
  { specId: "ai-settings", json: aiSettingsSpec },
  { specId: "contexts", json: contextsSpec },
  { specId: "findings", json: findingsSpec },
  { specId: "statistics", json: statisticsSpec },
];

/**
 * Get all page specs as DiscoveredSpec[].
 * Returns all groups from all spec files — no category filtering.
 */
export function getAllSpecs(): DiscoveredSpec[] {
  const result: DiscoveredSpec[] = [];

  for (const { specId, json } of ALL_SPECS) {
    const groups = json.groups ?? [];
    if (groups.length === 0) continue;

    result.push({
      specId,
      config: {
        version: json.version ?? "1.0.0",
        description: json.description ?? "",
        groups,
        metadata: json.metadata,
      },
    });
  }

  return result;
}
