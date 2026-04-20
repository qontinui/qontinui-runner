/**
 * useConstraints
 *
 * Hook for managing constraint engine state in the runner frontend.
 * Loads active constraints and raw TOML config on mount, provides local
 * editing state with dirty tracking, and exposes save/validate via the API.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type {
  Constraint,
  ResourceLimits,
  ValidateConfigResponse,
} from "@qontinui/shared-types/constraints";
import {
  isBuiltinConstraint,
  isCustomConstraint,
  isAiConstraint,
  generateConstraintToml,
} from "@qontinui/workflow-utils";
import {
  fetchActiveConstraints,
  fetchConstraintConfig,
  saveConstraintConfig,
  validateConstraintConfig,
} from "@/lib/constraints-api";

// ============================================================================
// Types
// ============================================================================

export interface UseConstraintsOptions {
  /** Project path to scope the config to. If omitted, the backend resolves it. */
  projectPath?: string;
  /** Whether to load data on mount. Default: true. */
  loadOnMount?: boolean;
}

export interface UseConstraintsReturn {
  /** All constraints (builtins + custom), reflecting local edits. */
  constraints: Constraint[];
  /** Only built-in constraints. */
  builtinConstraints: Constraint[];
  /** Only custom (project:) constraints. */
  customConstraints: Constraint[];
  /** Only AI-proposed (ai:) constraints from the current run. */
  aiConstraints: Constraint[];
  /** Resource limits (local state). */
  resourceLimits: ResourceLimits;
  /** Whether the local state differs from the last loaded/saved config. */
  isDirty: boolean;
  /** Whether data is currently loading. */
  loading: boolean;
  /** Error message, if any. */
  error: string | null;
  /** Path to the constraints.toml file (if one exists). */
  configPath: string | undefined;

  // Mutations
  /** Toggle a built-in constraint on/off by its suffix (e.g., "no-secrets"). */
  toggleBuiltin: (builtinSuffix: string, enabled: boolean) => void;
  /** Add a new custom constraint. */
  addConstraint: (constraint: Constraint) => void;
  /** Update an existing constraint by ID. */
  updateConstraint: (id: string, updates: Partial<Omit<Constraint, "id">>) => void;
  /** Remove a custom constraint by ID. */
  removeConstraint: (id: string) => void;
  /** Promote an AI-proposed constraint to a project constraint. */
  promoteConstraint: (id: string) => void;
  /** Update resource limits. */
  updateResourceLimits: (limits: Partial<ResourceLimits>) => void;

  // Actions
  /** Validate current state without saving. */
  validate: () => Promise<ValidateConfigResponse>;
  /** Generate TOML, validate, and save via the API. Returns true on success. */
  save: () => Promise<boolean>;
  /** Reload constraints and config from the backend. */
  reload: () => Promise<void>;
}

// ============================================================================
// Hook
// ============================================================================

export function useConstraints(options: UseConstraintsOptions = {}): UseConstraintsReturn {
  const { projectPath, loadOnMount = true } = options;

  // Local state
  const [constraints, setConstraints] = useState<Constraint[]>([]);
  const [resourceLimits, setResourceLimits] = useState<ResourceLimits>({});
  const [configPath, setConfigPath] = useState<string | undefined>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Snapshot of the last saved/loaded state for dirty tracking
  const savedStateRef = useRef<string>("");

  /** Serialize current state to a string for dirty comparison. */
  const serializeState = useCallback((cs: Constraint[], rl: ResourceLimits): string => {
    return JSON.stringify({ constraints: cs, resourceLimits: rl });
  }, []);

  // Computed properties
  const builtinConstraints = useMemo(
    () => constraints.filter((c) => isBuiltinConstraint(c.id)),
    [constraints],
  );

  const customConstraints = useMemo(
    () => constraints.filter((c) => isCustomConstraint(c.id)),
    [constraints],
  );

  const aiConstraints = useMemo(
    () => constraints.filter((c) => isAiConstraint(c.id)),
    [constraints],
  );

  const isDirty = useMemo(
    // eslint-disable-next-line react-hooks/refs
    () => serializeState(constraints, resourceLimits) !== savedStateRef.current,
    [constraints, resourceLimits, serializeState],
  );

  // --------------------------------------------------------------------------
  // Load
  // --------------------------------------------------------------------------

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [activeConstraints, config] = await Promise.all([
        fetchActiveConstraints(projectPath),
        fetchConstraintConfig(projectPath),
      ]);

      setConstraints(activeConstraints);
      setConfigPath(config.path ?? undefined);

      // Parse resource limits from the raw TOML if available.
      // The active constraints endpoint returns merged constraints but not
      // resource limits directly, so we extract them from the raw TOML text.
      const loadedLimits = config.toml ? parseResourceLimitsFromToml(config.toml) : {};
      setResourceLimits(loadedLimits);

      // Snapshot the loaded state (use local var to avoid stale closure)
      savedStateRef.current = serializeState(activeConstraints, loadedLimits);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[useConstraints] reload failed:", err);
      setError(msg);
    } finally {
      setLoading(false);
    }
  }, [projectPath, serializeState]);

  // Update the saved snapshot after reload completes and resource limits settle
  useEffect(() => {
    if (!loading && constraints.length > 0) {
      savedStateRef.current = serializeState(constraints, resourceLimits);
    }
    // Only run when loading transitions to false
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loading]);

  // Load on mount
  useEffect(() => {
    if (loadOnMount) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      reload();
    }
  }, [loadOnMount, reload]);

  // --------------------------------------------------------------------------
  // Mutations
  // --------------------------------------------------------------------------

  const toggleBuiltin = useCallback((builtinSuffix: string, enabled: boolean) => {
    const fullId = `builtin:${builtinSuffix}`;
    setConstraints((prev) => prev.map((c) => (c.id === fullId ? { ...c, enabled } : c)));
  }, []);

  const addConstraint = useCallback((constraint: Constraint) => {
    setConstraints((prev) => [...prev, constraint]);
  }, []);

  const updateConstraint = useCallback((id: string, updates: Partial<Omit<Constraint, "id">>) => {
    setConstraints((prev) => prev.map((c) => (c.id === id ? { ...c, ...updates } : c)));
  }, []);

  const removeConstraint = useCallback((id: string) => {
    setConstraints((prev) => prev.filter((c) => c.id !== id));
  }, []);

  const promoteConstraint = useCallback((id: string) => {
    setConstraints((prev) =>
      prev.map((c) => {
        if (c.id !== id || !isAiConstraint(c.id)) return c;
        const projectId = c.id.replace(/^ai:/, "project:");
        return { ...c, id: projectId };
      }),
    );
  }, []);

  const updateResourceLimitsHandler = useCallback((limits: Partial<ResourceLimits>) => {
    setResourceLimits((prev) => ({ ...prev, ...limits }));
  }, []);

  // --------------------------------------------------------------------------
  // Validate & Save
  // --------------------------------------------------------------------------

  const validate = useCallback(async (): Promise<ValidateConfigResponse> => {
    const toml = generateConstraintToml(constraints, resourceLimits);
    return validateConstraintConfig(toml);
  }, [constraints, resourceLimits]);

  const save = useCallback(async (): Promise<boolean> => {
    setError(null);
    try {
      const toml = generateConstraintToml(constraints, resourceLimits);

      // Validate first
      const validation = await validateConstraintConfig(toml);
      if (!validation.valid) {
        setError((validation.errors ?? []).join("; "));
        return false;
      }

      // Save
      const result = await saveConstraintConfig(toml, projectPath);
      if (!result.valid) {
        setError((result.errors ?? []).join("; "));
        return false;
      }

      // Update config path and snapshot
      setConfigPath(result.path);
      savedStateRef.current = serializeState(constraints, resourceLimits);
      return true;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[useConstraints] save failed:", err);
      setError(msg);
      return false;
    }
  }, [constraints, resourceLimits, projectPath, serializeState]);

  return {
    constraints,
    builtinConstraints,
    customConstraints,
    aiConstraints,
    resourceLimits,
    isDirty,
    loading,
    error,
    configPath,

    toggleBuiltin,
    addConstraint,
    updateConstraint,
    removeConstraint,
    promoteConstraint,
    updateResourceLimits: updateResourceLimitsHandler,

    validate,
    save,
    reload,
  };
}

// ============================================================================
// Internal Helpers
// ============================================================================

/**
 * Simple extraction of resource limits from raw TOML text.
 *
 * This avoids needing a full TOML parser in the frontend — we just look
 * for the `[resources]` section and extract known keys.
 */
function parseResourceLimitsFromToml(toml: string): ResourceLimits {
  const limits: ResourceLimits = {};

  // Find the [resources] section
  const resourcesMatch = toml.match(/\[resources\]\s*\n([\s\S]*?)(?=\n\[|\n*$)/);
  if (!resourcesMatch) return limits;

  const section = resourcesMatch[1];

  const wallTime = section.match(/max_wall_time_secs\s*=\s*(\d+)/);
  if (wallTime) limits.maxWallTimeSecs = parseInt(wallTime[1], 10);

  const filesModified = section.match(/max_files_modified\s*=\s*(\d+)/);
  if (filesModified) limits.maxFilesModified = parseInt(filesModified[1], 10);

  const agenticTime = section.match(/max_agentic_time_ms\s*=\s*(\d+)/);
  if (agenticTime) limits.maxAgenticTimeMs = parseInt(agenticTime[1], 10);

  const threshold = section.match(/warning_threshold\s*=\s*([\d.]+)/);
  if (threshold) limits.warningThreshold = parseFloat(threshold[1]);

  return limits;
}
