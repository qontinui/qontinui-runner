import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import type { UnifiedWorkflow, WorkflowExport } from "../../types";
import { registerUserSkills } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import { createLogger } from "@/lib/logger";

const log = createLogger("WorkflowBuilder");

export function useWorkflowLibrary(
  currentWorkflowId: string | undefined,
  isSaving: boolean,
  resetToNew: () => void,
  loadWorkflow: (id: string) => Promise<boolean>,
) {
  const [savedWorkflows, setSavedWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [workflowsLoading, setWorkflowsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [showFavoritesOnly, setShowFavoritesOnly] = useState(false);

  const [isWorkflowSelectionMode, setIsWorkflowSelectionMode] = useState(false);
  const [selectedWorkflowIds, setSelectedWorkflowIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeletingWorkflows, setIsDeletingWorkflows] = useState(false);

  const [isExportingWorkflow, setIsExportingWorkflow] = useState(false);
  const [workflowImportError, setWorkflowImportError] = useState<string | null>(null);

  const [isExportingSkills, setIsExportingSkills] = useState(false);
  const [isImportingSkills, setIsImportingSkills] = useState(false);
  const skillFileInputRef = useRef<HTMLInputElement>(null);
  const workflowFileInputRef = useRef<HTMLInputElement>(null);
  const [showSkillImportDialog, setShowSkillImportDialog] = useState(false);
  const [pendingImportSkills, setPendingImportSkills] = useState<unknown[] | null>(null);
  const [importConflictMode, setImportConflictMode] = useState<"skip" | "overwrite">("skip");
  const [showSkillExportDialog, setShowSkillExportDialog] = useState(false);
  const [availableSkillsForExport, setAvailableSkillsForExport] = useState<
    Array<{ id: string; name: string; source: string }>
  >([]);
  const [selectedSkillIdsForExport, setSelectedSkillIdsForExport] = useState<Set<string>>(
    new Set(),
  );
  const [showCompositionBuilder, setShowCompositionBuilder] = useState(false);

  const [showLibrary, setShowLibrary] = useState(() => {
    return instanceStorage.getItem("qontinui-workflow-library-visible") === "true";
  });

  const toggleLibrary = useCallback(() => {
    setShowLibrary((prev) => {
      const next = !prev;
      instanceStorage.setItem("qontinui-workflow-library-visible", String(next));
      return next;
    });
  }, []);

  const fetchWorkflows = useCallback(async () => {
    setWorkflowsLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/unified-workflows`);
      const result = await response.json();
      if (result.success && result.data) {
        setSavedWorkflows(result.data);
      } else if (Array.isArray(result)) {
        setSavedWorkflows(result);
      } else {
        setSavedWorkflows([]);
      }
    } catch (error) {
      console.error("Failed to fetch workflows:", error);
    } finally {
      setWorkflowsLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchWorkflows();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchWorkflows]);

  useEffect(() => {
    if (isSaving || !currentWorkflowId) return;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchWorkflows();
    });
    return () => {
      cancelled = true;
    };
  }, [isSaving, currentWorkflowId, fetchWorkflows]);

  const toggleFavorite = useCallback(async (workflowId: string) => {
    setSavedWorkflows((prev) =>
      prev.map((w) => (w.id === workflowId ? { ...w, isFavorite: !w.isFavorite } : w)),
    );
    try {
      const response = await tracedFetch(
        `${getApiBase()}/unified-workflows/${workflowId}/favorite`,
        { method: "POST" },
      );
      const result = await response.json();
      if (result.success) {
        const newState = result.data.isFavorite;
        setSavedWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, isFavorite: newState } : w)),
        );
      } else {
        setSavedWorkflows((prev) =>
          prev.map((w) => (w.id === workflowId ? { ...w, isFavorite: !w.isFavorite } : w)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle favorite:", error);
      setSavedWorkflows((prev) =>
        prev.map((w) => (w.id === workflowId ? { ...w, isFavorite: !w.isFavorite } : w)),
      );
    }
  }, []);

  const hasFavorites = useMemo(() => savedWorkflows.some((w) => w.isFavorite), [savedWorkflows]);

  const toggleWorkflowSelection = useCallback((workflowId: string) => {
    setSelectedWorkflowIds((prev) => {
      const next = new Set(prev);
      if (next.has(workflowId)) {
        next.delete(workflowId);
      } else {
        next.add(workflowId);
      }
      return next;
    });
  }, []);

  const exitWorkflowSelectionMode = useCallback(() => {
    setIsWorkflowSelectionMode(false);
    setSelectedWorkflowIds(new Set());
  }, []);

  const deleteSelectedWorkflows = useCallback(async () => {
    if (selectedWorkflowIds.size === 0) return;

    setIsDeletingWorkflows(true);
    try {
      const deletePromises = Array.from(selectedWorkflowIds).map((id) =>
        tracedFetch(`${getApiBase()}/unified-workflows/${id}`, { method: "DELETE" }),
      );
      await Promise.all(deletePromises);
      await fetchWorkflows();

      if (currentWorkflowId && selectedWorkflowIds.has(currentWorkflowId)) {
        resetToNew();
      }

      exitWorkflowSelectionMode();
      setShowBatchDeleteDialog(false);
    } catch (error) {
      console.error("Failed to delete workflows:", error);
    } finally {
      setIsDeletingWorkflows(false);
    }
  }, [
    selectedWorkflowIds,
    fetchWorkflows,
    currentWorkflowId,
    resetToNew,
    exitWorkflowSelectionMode,
  ]);

  const getSelectedWorkflowNames = useCallback((): string[] => {
    return savedWorkflows.filter((w) => selectedWorkflowIds.has(w.id)).map((w) => w.name);
  }, [savedWorkflows, selectedWorkflowIds]);

  const handleExportWorkflow = useCallback(async () => {
    if (!currentWorkflowId) return;

    setIsExportingWorkflow(true);
    try {
      const response = await tracedFetch(
        `${getApiBase()}/unified-workflows/${currentWorkflowId}/export`,
      );
      const data = await response.json();
      if (data.success && data.data) {
        const exportData = data.data as WorkflowExport;
        const blob = new Blob([JSON.stringify(exportData, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `workflow-${exportData.workflow.name.replace(/[^a-zA-Z0-9]/g, "-")}.json`;
        a.click();
        URL.revokeObjectURL(url);
      } else {
        console.error("Failed to export workflow:", data.error);
      }
    } catch (error) {
      console.error("Failed to export workflow:", error);
    } finally {
      setIsExportingWorkflow(false);
    }
  }, [currentWorkflowId]);

  const handleImportWorkflow = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      setWorkflowImportError(null);
      setWorkflowsLoading(true);

      try {
        const text = await file.text();
        const data = JSON.parse(text);

        let workflowData: UnifiedWorkflow;
        if (data.manifest && data.workflow) {
          workflowData = data.workflow;
        } else if (data.setup_steps !== undefined) {
          workflowData = data;
        } else {
          throw new Error("Invalid workflow file format");
        }

        const response = await tracedFetch(`${getApiBase()}/unified-workflows/import`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            workflow: workflowData,
            conflict_strategy: "generate",
          }),
        });

        const result = await response.json();
        if (result.success && result.data) {
          await fetchWorkflows();
          await loadWorkflow(result.data.workflow.id);
        } else {
          setWorkflowImportError(result.error || "Failed to import workflow");
        }
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : "Failed to import workflow";
        setWorkflowImportError(errorMsg);
        console.error("Failed to import workflow:", error);
      } finally {
        setWorkflowsLoading(false);
        event.target.value = "";
      }
    },
    [fetchWorkflows, loadWorkflow],
  );

  const handleOpenExportDialog = useCallback(async () => {
    try {
      const res = await tracedFetch(`${getApiBase()}/skills`);
      const data = await res.json();
      const allSkills = data.data ?? data ?? [];
      const nonBuiltin = allSkills
        .filter((s: { source: string }) => s.source !== "builtin")
        .map((s: { id: string; name: string; source: string }) => ({
          id: s.id,
          name: s.name,
          source: s.source,
        }));
      if (nonBuiltin.length === 0) {
        log.debug("No user or community skills to export.");
        return;
      }
      setAvailableSkillsForExport(nonBuiltin);
      setSelectedSkillIdsForExport(new Set(nonBuiltin.map((s: { id: string }) => s.id)));
      setShowSkillExportDialog(true);
    } catch (error) {
      console.error("Failed to load skills for export:", error);
    }
  }, []);

  const handleExportSkills = useCallback(async () => {
    setIsExportingSkills(true);
    setShowSkillExportDialog(false);
    try {
      const skillIds = Array.from(selectedSkillIdsForExport);
      const response = await tracedFetch(`${getApiBase()}/skills/export`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ skill_ids: skillIds }),
      });
      const data = await response.json();
      if (data.success && data.data) {
        const exportData = data.data;
        const blob = new Blob([JSON.stringify(exportData, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        const date = new Date().toISOString().split("T")[0];
        a.download = `qontinui-skills-${date}.json`;
        a.click();
        URL.revokeObjectURL(url);
      } else {
        console.error("Failed to export skills:", data.error);
      }
    } catch (error) {
      console.error("Failed to export skills:", error);
    } finally {
      setIsExportingSkills(false);
    }
  }, [selectedSkillIdsForExport]);

  const handleImportFileSelected = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      try {
        const text = await file.text();
        const data = JSON.parse(text);

        if (!data.manifest || data.manifest.content_type !== "skills" || !data.skills) {
          throw new Error("Invalid skill file format. Expected a skill export file.");
        }

        setPendingImportSkills(data.skills);
        setImportConflictMode("skip");
        setShowSkillImportDialog(true);
      } catch (error) {
        console.error("Failed to read skill file:", error);
      } finally {
        event.target.value = "";
      }
    },
    [],
  );

  const handleConfirmImport = useCallback(async () => {
    if (!pendingImportSkills) return;

    setShowSkillImportDialog(false);
    setIsImportingSkills(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/import`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          skills: pendingImportSkills,
          conflict_mode: importConflictMode,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        const { imported, skipped, overwritten, errors } = result.data;
        log.debug(
          `Skills imported: ${imported}, skipped: ${skipped}, overwritten: ${overwritten}`,
          errors.length > 0 ? `Errors: ${errors.join(", ")}` : "",
        );

        if (imported > 0 || overwritten > 0) {
          try {
            const skillsRes = await tracedFetch(`${getApiBase()}/skills`);
            const skillsData = await skillsRes.json();
            const allSkills = skillsData.data ?? skillsData ?? [];
            const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
            registerUserSkills(nonBuiltin);
          } catch {
            // Skill refresh is non-critical
          }
        }
      } else {
        console.error("Failed to import skills:", result.error);
      }
    } catch (error) {
      console.error("Failed to import skills:", error);
    } finally {
      setIsImportingSkills(false);
      setPendingImportSkills(null);
    }
  }, [pendingImportSkills, importConflictMode]);

  const filteredWorkflows = savedWorkflows
    .filter((w) => {
      if (showFavoritesOnly && !w.isFavorite) return false;
      if (!searchQuery) return true;
      const query = searchQuery.toLowerCase();
      return (
        w.name.toLowerCase().includes(query) ||
        w.description?.toLowerCase().includes(query) ||
        w.category?.toLowerCase().includes(query)
      );
    })
    .sort((a, b) => {
      const favA = a.isFavorite ? 1 : 0;
      const favB = b.isFavorite ? 1 : 0;
      return favB - favA;
    });

  const selectWorkflow = async (workflow: UnifiedWorkflow) => {
    await loadWorkflow(workflow.id);
  };

  return {
    savedWorkflows,
    workflowsLoading,
    searchQuery,
    setSearchQuery,
    showFavoritesOnly,
    setShowFavoritesOnly,
    isWorkflowSelectionMode,
    setIsWorkflowSelectionMode,
    selectedWorkflowIds,
    showBatchDeleteDialog,
    setShowBatchDeleteDialog,
    isDeletingWorkflows,
    isExportingWorkflow,
    workflowImportError,
    setWorkflowImportError,
    isExportingSkills,
    isImportingSkills,
    skillFileInputRef,
    workflowFileInputRef,
    showSkillImportDialog,
    setShowSkillImportDialog,
    pendingImportSkills,
    setPendingImportSkills,
    importConflictMode,
    setImportConflictMode,
    showSkillExportDialog,
    setShowSkillExportDialog,
    availableSkillsForExport,
    selectedSkillIdsForExport,
    setSelectedSkillIdsForExport,
    showCompositionBuilder,
    setShowCompositionBuilder,
    showLibrary,
    toggleLibrary,
    hasFavorites,
    filteredWorkflows,
    toggleFavorite,
    toggleWorkflowSelection,
    exitWorkflowSelectionMode,
    deleteSelectedWorkflows,
    getSelectedWorkflowNames,
    handleExportWorkflow,
    handleImportWorkflow,
    handleOpenExportDialog,
    handleExportSkills,
    handleImportFileSelected,
    handleConfirmImport,
    selectWorkflow,
  };
}
