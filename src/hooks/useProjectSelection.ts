/**
 * useProjectSelection
 *
 * Hook for managing project selection state across the application.
 * The selected project is shared between Connection settings and Capture settings.
 * It persists the selection to localStorage and emits events for StatusIndicator.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Project } from "../types/auth";
import { instanceStorage } from "@/lib/instance-storage";
import { createLogger } from "@/lib/logger";

const log = createLogger("useProjectSelection");

const SELECTED_PROJECT_STORAGE_KEY = "qontinui-selected-project";

export interface ProjectSelectionState {
  selectedProjectId: string | null;
  selectedProjectName: string | null;
}

interface UseProjectSelectionReturn {
  projects: Project[];
  selectedProjectId: string | null;
  selectedProjectName: string | null;
  loading: boolean;
  error: string | null;
  setSelectedProject: (projectId: string | null) => void;
  loadProjects: () => Promise<void>;
}

/**
 * Hook to manage project selection across the application.
 * Projects are fetched from the backend and the selection is persisted.
 */
export function useProjectSelection(): UseProjectSelectionReturn {
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectIdState] = useState<string | null>(() => {
    // Load from instanceStorage on mount
    const parsed = instanceStorage.getJSON<ProjectSelectionState | null>(
      SELECTED_PROJECT_STORAGE_KEY,
      null,
    );
    return parsed?.selectedProjectId ?? null;
  });
  const [selectedProjectName, setSelectedProjectName] = useState<string | null>(() => {
    const parsed = instanceStorage.getJSON<ProjectSelectionState | null>(
      SELECTED_PROJECT_STORAGE_KEY,
      null,
    );
    return parsed?.selectedProjectName ?? null;
  });
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Load projects from the backend
   * Retries once on auth failure to handle race conditions with keychain access
   */
  const loadProjects = useCallback(async () => {
    setLoading(true);
    setError(null);

    const attemptLoad = async (retryCount = 0): Promise<Project[]> => {
      try {
        return await invoke<Project[]>("get_user_projects");
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : String(err);
        // Retry once on auth failure (handles keychain race condition)
        if (retryCount === 0 && errorMsg.includes("Not authenticated")) {
          log.debug("Auth race condition, retrying in 500ms...");
          await new Promise((resolve) => setTimeout(resolve, 500));
          return attemptLoad(1);
        }
        throw err;
      }
    };

    try {
      const projectList = await attemptLoad();
      setProjects(projectList);
      log.debug("Loaded", projectList.length, "projects");

      // If we have a stored selection, validate it still exists
      if (selectedProjectId) {
        const projectStillExists = projectList.some((p) => p.id === selectedProjectId);
        if (!projectStillExists && projectList.length > 0) {
          // Auto-select first project if stored project no longer exists
          setSelectedProject(projectList[0].id);
        } else if (projectStillExists) {
          // Update name in case it changed
          const project = projectList.find((p) => p.id === selectedProjectId);
          if (project) {
            setSelectedProjectName(project.name);
          }
        }
      } else if (projectList.length > 0) {
        // Auto-select first project if none selected
        setSelectedProject(projectList[0].id);
      }
    } catch (err) {
      console.error("[PROJECT_SELECTION] Failed to load projects:", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setSelectedProject is defined below and stable
  }, [selectedProjectId]);

  /**
   * Set the selected project and persist to localStorage
   */
  const setSelectedProject = useCallback(
    (projectId: string | null) => {
      setSelectedProjectIdState(projectId);

      // Find the project name
      const project = projects.find((p) => p.id === projectId);
      const projectName = project?.name || null;
      setSelectedProjectName(projectName);

      // Persist to localStorage
      const state: ProjectSelectionState = {
        selectedProjectId: projectId,
        selectedProjectName: projectName,
      };
      instanceStorage.setJSON(SELECTED_PROJECT_STORAGE_KEY, state);

      // Dispatch event for StatusIndicator
      window.dispatchEvent(
        new CustomEvent("project-selection-changed", {
          detail: { projectId, projectName },
        }),
      );

      log.debug("Selected project:", projectId, projectName);
    },
    [projects],
  );

  // When projects change, update the selected project name if needed
  useEffect(() => {
    if (selectedProjectId && projects.length > 0) {
      const project = projects.find((p) => p.id === selectedProjectId);
      if (project && project.name !== selectedProjectName) {
        setSelectedProjectName(project.name);
        // Update localStorage
        const state: ProjectSelectionState = {
          selectedProjectId,
          selectedProjectName: project.name,
        };
        instanceStorage.setJSON(SELECTED_PROJECT_STORAGE_KEY, state);
      }
    }
  }, [projects, selectedProjectId, selectedProjectName]);

  return {
    projects,
    selectedProjectId,
    selectedProjectName,
    loading,
    error,
    setSelectedProject,
    loadProjects,
  };
}
