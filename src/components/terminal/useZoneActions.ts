import { useCallback } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import type { SessionState } from "./useZoneLayout";
import type { UIAction } from "./useUIState";
import type { TerminalTab } from "./useTerminalManager";
import type { Metrics } from "./useEventHistory";

interface UseZoneActionsParams {
  tabs: TerminalTab[];
  dispatch: React.Dispatch<UIAction>;
  zoneLayout: {
    layoutId: string;
    assignments: Record<number, string>;
    layout: { zones: unknown[] };
    setFocusedZone: (idx: number) => void;
    assignTabToZone: (zoneIdx: number, tabId: string) => void;
    setLayoutId: (id: string) => void;
    isMultiZone: boolean;
    toggleMaximize: (zoneIdx?: number) => void;
  };
  stateTracking: {
    sessionStates: Record<string, SessionState>;
    lastOutputLines: Record<string, string[]>;
  };
  labelsAndTags: {
    zoneLabels: Record<number, string>;
    setZoneLabel: (zoneIdx: number, label: string) => void;
  };
  transitionEffects: {
    setUnseenNeedsInput: React.Dispatch<React.SetStateAction<Set<string>>>;
  };
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  createPlanTab: (filePath: string) => string | null;
  incrementMetric: (key: keyof Metrics, amount?: number) => void;
  setNotification: React.Dispatch<
    React.SetStateAction<{ message: string; type: "success" | "error" } | null>
  >;
}

export function useZoneActions({
  tabs,
  dispatch,
  zoneLayout,
  stateTracking,
  labelsAndTags,
  transitionEffects,
  createTerminal,
  createPlanTab,
  incrementMetric,
  setNotification,
}: UseZoneActionsParams) {
  const handleZoneClick = useCallback(
    (zoneIndex: number, ctrlKey?: boolean) => {
      if (ctrlKey) {
        dispatch({ type: "TOGGLE_ZONE_SELECTION", payload: zoneIndex });
      } else {
        zoneLayout.setFocusedZone(zoneIndex);
        dispatch({ type: "CLEAR_SELECTION" });
        const focusedTabId = zoneLayout.assignments[zoneIndex];
        if (focusedTabId) {
          transitionEffects.setUnseenNeedsInput((prev) => {
            if (!prev.has(focusedTabId)) return prev;
            const next = new Set(prev);
            next.delete(focusedTabId);
            return next;
          });
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      zoneLayout.setFocusedZone,
      zoneLayout.assignments,
      transitionEffects.setUnseenNeedsInput,
      dispatch,
    ],
  );

  const handleZoneDoubleClick = useCallback(
    (zoneIndex: number) => {
      if (zoneLayout.isMultiZone) {
        zoneLayout.toggleMaximize(zoneIndex);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [zoneLayout.isMultiZone, zoneLayout.toggleMaximize],
  );

  const handleOpenDocFile = useCallback(
    (filePath: string) => {
      const tabId = createPlanTab(filePath);
      if (tabId) {
        const emptyZone = zoneLayout.layout.zones.findIndex(
          (_, idx) => !zoneLayout.assignments[idx],
        );
        if (emptyZone >= 0) {
          zoneLayout.assignTabToZone(emptyZone, tabId);
          zoneLayout.setFocusedZone(emptyZone);
        }
      }
    },
    [createPlanTab, zoneLayout],
  );

  const createAndAssignTerminal = useCallback(
    async (title?: string, workingDir?: string) => {
      incrementMetric("sessionsCreated");
      const tabId = await createTerminal(title, workingDir);
      if (!tabId) return tabId;

      const totalTabs = tabs.length + 1;
      const currentZoneCount = zoneLayout.layout.zones.length;
      const hasEmptyZone = zoneLayout.layout.zones.some((_, idx) => !zoneLayout.assignments[idx]);

      if (totalTabs > currentZoneCount || (!hasEmptyZone && totalTabs > 1)) {
        let targetLayout: string;
        if (totalTabs >= 7) targetLayout = "full-grid";
        else if (totalTabs >= 5) targetLayout = "six-pack";
        else if (totalTabs >= 3) targetLayout = "quad";
        else targetLayout = "split";
        if (targetLayout !== zoneLayout.layoutId) {
          zoneLayout.setLayoutId(targetLayout);
        }
      }

      return tabId;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      createTerminal,
      zoneLayout.layoutId,
      zoneLayout.setLayoutId,
      zoneLayout.layout,
      zoneLayout.assignments,
      tabs.length,
      incrementMetric,
    ],
  );

  const handleSortZones = useCallback(() => {
    const STATE_PRIORITY: Record<SessionState, number> = {
      "needs-input": 0,
      error: 1,
      working: 2,
      idle: 3,
      completed: 4,
    };
    const entries = Object.entries(zoneLayout.assignments)
      .map(([z, tabId]) => ({
        zoneIndex: Number(z),
        tabId,
        priority: STATE_PRIORITY[stateTracking.sessionStates[tabId] ?? "idle"],
      }))
      .sort((a, b) => a.priority - b.priority);

    const sortedTabIds = entries.map((e) => e.tabId);
    for (let i = 0; i < sortedTabIds.length; i++) {
      zoneLayout.assignTabToZone(i, sortedTabIds[i]);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [zoneLayout.assignments, zoneLayout.assignTabToZone, stateTracking.sessionStates]);

  const handleExportOutput = useCallback(async () => {
    const timestamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const filePath = await save({
      defaultPath: `session-output-${timestamp}.txt`,
      filters: [{ name: "Text Files", extensions: ["txt"] }],
    });
    if (!filePath) return;

    const lines: string[] = [];
    lines.push(`Session Output Export — ${new Date().toLocaleString()}`);
    lines.push(`Layout: ${zoneLayout.layoutId}, Tabs: ${tabs.length}`);
    lines.push("=".repeat(60));

    for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) continue;
      const state = stateTracking.sessionStates[tabId] ?? "idle";
      const output = stateTracking.lastOutputLines[tabId] ?? [];
      lines.push("");
      lines.push(`--- Zone ${Number(zoneStr) + 1}: ${tab.title} [${state}] ---`);
      if (tab.workingDir) lines.push(`    Dir: ${tab.workingDir}`);
      if (output.length > 0) {
        lines.push(...output);
      } else {
        lines.push("    (no output)");
      }
    }

    const assignedTabIds = new Set(Object.values(zoneLayout.assignments));
    const unassigned = tabs.filter((t) => !assignedTabIds.has(t.id));
    if (unassigned.length > 0) {
      lines.push("");
      lines.push("--- Unassigned Sessions ---");
      for (const tab of unassigned) {
        const state = stateTracking.sessionStates[tab.id] ?? "idle";
        const output = stateTracking.lastOutputLines[tab.id] ?? [];
        lines.push(`  ${tab.title} [${state}]`);
        if (output.length > 0) lines.push(...output.map((l) => `    ${l}`));
      }
    }

    try {
      await writeTextFile(filePath, lines.join("\n"));
      setNotification({ message: `Exported to ${filePath}`, type: "success" });
    } catch (err) {
      setNotification({
        message: `Export failed: ${err instanceof Error ? err.message : String(err)}`,
        type: "error",
      });
    }
  }, [
    tabs,
    zoneLayout.layoutId,
    zoneLayout.assignments,
    stateTracking.sessionStates,
    stateTracking.lastOutputLines,
    setNotification,
  ]);

  const handleExportZone = useCallback(
    async (zoneIndex: number, format: "text" | "markdown" | "json") => {
      const tabId = zoneLayout.assignments[zoneIndex];
      if (!tabId) return;
      const tab = tabs.find((t) => t.id === tabId);
      const lines = stateTracking.lastOutputLines[tabId] ?? [];
      const title = tab?.title ?? `Zone ${zoneIndex + 1}`;
      const state = stateTracking.sessionStates[tabId] ?? "idle";
      const label = labelsAndTags.zoneLabels[zoneIndex] ?? "";

      let content: string;
      const ext = format === "json" ? "json" : format === "markdown" ? "md" : "txt";

      if (format === "markdown") {
        content = [
          `# ${title}`,
          `- **Zone:** ${zoneIndex + 1}`,
          `- **State:** ${state}`,
          label ? `- **Tags:** ${label}` : "",
          `- **Lines:** ${lines.length}`,
          `- **Exported:** ${new Date().toISOString()}`,
          "",
          "```",
          ...lines,
          "```",
        ]
          .filter(Boolean)
          .join("\n");
      } else if (format === "json") {
        content = JSON.stringify(
          {
            zone: zoneIndex + 1,
            title,
            state,
            tags: label ? label.split(",").map((t) => t.trim()) : [],
            exportedAt: new Date().toISOString(),
            lineCount: lines.length,
            output: lines,
          },
          null,
          2,
        );
      } else {
        content = lines.join("\n");
      }

      try {
        const filePath = await save({
          defaultPath: `zone-${zoneIndex + 1}-output.${ext}`,
          filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
        });
        if (filePath) {
          await writeTextFile(filePath, content);
        }
      } catch (err) {
        console.error("Export failed:", err);
      }
    },
    [
      tabs,
      stateTracking.lastOutputLines,
      stateTracking.sessionStates,
      labelsAndTags.zoneLabels,
      zoneLayout.assignments,
    ],
  );

  return {
    handleZoneClick,
    handleZoneDoubleClick,
    handleOpenDocFile,
    createAndAssignTerminal,
    handleSortZones,
    handleExportOutput,
    handleExportZone,
  };
}
