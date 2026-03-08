/**
 * useKnownIssues
 *
 * Hook for managing known issues in the runner.
 * Wraps Tauri invoke calls for CRUD operations on the known issues registry.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  KnownIssue,
  CreateKnownIssueRequest,
  UpdateKnownIssueRequest,
  ListKnownIssuesQuery,
  IssuePatternTemplate,
} from "@qontinui/shared-types";

export function useKnownIssues() {
  const [issues, setIssues] = useState<KnownIssue[]>([]);
  const [templates, setTemplates] = useState<IssuePatternTemplate[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadIssues = useCallback(async (query?: ListKnownIssuesQuery) => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await invoke<KnownIssue[]>("list_known_issues", {
        query: query || {},
      });
      setIssues(result);
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      return [];
    } finally {
      setIsLoading(false);
    }
  }, []);

  const loadIssuesForSpec = useCallback(async (specId: string, pageUrl?: string) => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await invoke<KnownIssue[]>("find_issues_for_spec", {
        specId,
        pageUrl: pageUrl || null,
      });
      setIssues(result);
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      return [];
    } finally {
      setIsLoading(false);
    }
  }, []);

  const createIssue = useCallback(async (req: CreateKnownIssueRequest) => {
    try {
      const result = await invoke<KnownIssue>("create_known_issue", {
        request: req,
      });
      setIssues((prev) => [...prev, result]);
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg);
    }
  }, []);

  const updateIssue = useCallback(async (id: string, req: UpdateKnownIssueRequest) => {
    try {
      const result = await invoke<KnownIssue>("update_known_issue", {
        id,
        request: req,
      });
      setIssues((prev) => prev.map((i) => (i.id === id ? result : i)));
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg);
    }
  }, []);

  const deleteIssue = useCallback(async (id: string) => {
    try {
      await invoke<boolean>("delete_known_issue", { id });
      setIssues((prev) => prev.filter((i) => i.id !== id));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg);
    }
  }, []);

  const resolveIssue = useCallback(async (id: string, resolution?: string) => {
    try {
      await invoke("resolve_known_issue", {
        id,
        resolution: resolution || null,
      });
      setIssues((prev) =>
        prev.map((i) => (i.id === id ? { ...i, status: "resolved" as const } : i)),
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg);
    }
  }, []);

  const loadTemplates = useCallback(async () => {
    try {
      const result = await invoke<IssuePatternTemplate[]>("list_pattern_templates");
      setTemplates(result);
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      return [];
    }
  }, []);

  return {
    issues,
    templates,
    isLoading,
    error,
    loadIssues,
    loadIssuesForSpec,
    createIssue,
    updateIssue,
    deleteIssue,
    resolveIssue,
    loadTemplates,
  };
}
