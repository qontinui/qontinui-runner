/**
 * useSpecsState — Core state management for the Spec Viewer/Editor
 */

import { useState, useCallback, useMemo, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getAllSpecs } from "@/lib/spec-registry";
import { getKnownPages, type KnownPage } from "@/lib/page-catalog";
import { useSpecFileLoader } from "./useSpecFileLoader";
import type { LoadedSpec, SpecSelection, SpecTreeNode, ConnectionState } from "./types";
import type { SpecConfig, SpecGroup, SpecAssertion, SetupAction } from "@qontinui/ui-bridge";

function buildNodeFromSpec(spec: LoadedSpec): SpecTreeNode {
  if (spec.kind === "page-spec") {
    const groups = spec.config.groups || [];
    const groupChildren: SpecTreeNode[] = groups.map((group) => ({
      id: `${spec.specId}::${group.id}`,
      label: group.name,
      type: "group" as const,
      specId: spec.specId,
      groupId: group.id,
      childCount: group.assertions.filter((a) => a.enabled).length,
    }));
    const totalAssertions = groups.reduce(
      (sum, g) => sum + g.assertions.filter((a) => a.enabled).length,
      0,
    );
    // Extract page name from description: "Active Dashboard page -- details..." → "Active Dashboard"
    const desc = spec.config.description || "";
    const pageNameMatch = desc.match(/^(.+?)\s+page\b/i);
    const label = pageNameMatch
      ? pageNameMatch[1]
      : desc.split(/\s*[—–-]{1,2}\s*/)[0] || spec.specId;

    return {
      id: spec.specId,
      label,
      type: "page-spec",
      specId: spec.specId,
      appName: spec.appName,
      pageUrl: spec.config.metadata?.pageUrl as string | undefined,
      childCount: totalAssertions,
      children: groupChildren,
    };
  }

  if (spec.kind === "architecture") {
    return {
      id: spec.specId,
      label: spec.config.projectName || spec.specId,
      type: "architecture",
      specId: spec.specId,
      appName: spec.appName,
      childCount: spec.config.features.length,
    };
  }

  if (spec.kind === "api") {
    return {
      id: spec.specId,
      label: spec.config.description || spec.config.basePath || spec.specId,
      type: "api",
      specId: spec.specId,
      appName: spec.appName,
      childCount: spec.config.endpoints.length,
    };
  }

  if (spec.kind === "data") {
    return {
      id: spec.specId,
      label: spec.config.description || `${spec.config.database} schema`,
      type: "data",
      specId: spec.specId,
      appName: spec.appName,
      childCount: spec.config.entities.length,
    };
  }

  if (spec.kind === "dependency") {
    return {
      id: spec.specId,
      label: spec.config.description || "Dependencies",
      type: "dependency",
      specId: spec.specId,
      appName: spec.appName,
      childCount: spec.config.links.length,
    };
  }

  // constraint spec
  const constraintCount =
    (spec.config.performance?.length || 0) +
    (spec.config.bundleBudgets?.length || 0) +
    (spec.config.browsers?.length || 0) +
    (spec.config.capacity?.length || 0) +
    (spec.config.breakpoints?.length || 0) +
    (spec.config.accessibility ? 1 : 0);
  return {
    id: spec.specId,
    label: spec.config.description || "Constraints",
    type: "constraint",
    specId: spec.specId,
    appName: spec.appName,
    childCount: constraintCount,
  };
}

function buildTreeFromSpecs(specs: LoadedSpec[], unspeccedPages: KnownPage[]): SpecTreeNode[] {
  // Group specs by app name
  const byApp = new Map<string, LoadedSpec[]>();
  for (const spec of specs) {
    const appName = spec.appName || "Unknown";
    if (!byApp.has(appName)) byApp.set(appName, []);
    byApp.get(appName)!.push(spec);
  }

  // Group unspecced pages by app name
  const unspeccedByApp = new Map<string, KnownPage[]>();
  for (const page of unspeccedPages) {
    if (!unspeccedByApp.has(page.appName)) unspeccedByApp.set(page.appName, []);
    unspeccedByApp.get(page.appName)!.push(page);
  }

  // Merge app names from both sources
  const allAppNames = new Set([...byApp.keys(), ...unspeccedByApp.keys()]);
  const roots: SpecTreeNode[] = [];

  for (const appName of allAppNames) {
    const appSpecs = byApp.get(appName) || [];
    const appUnspecced = unspeccedByApp.get(appName) || [];

    const specChildren: SpecTreeNode[] = appSpecs.map(buildNodeFromSpec);

    // Add unspecced page nodes (sorted alphabetically, at the end)
    const unspeccedNodes: SpecTreeNode[] = appUnspecced
      .sort((a, b) => a.label.localeCompare(b.label))
      .map((page) => ({
        id: `unspecced:${page.expectedSpecId}`,
        label: page.label,
        type: "page-spec" as const,
        appName: page.appName,
        unspecced: true,
        description: page.description,
        specId: page.expectedSpecId,
      }));

    const allChildren = [...specChildren, ...unspeccedNodes];

    if (allAppNames.size === 1) {
      roots.push(...allChildren);
    } else {
      roots.push({
        id: `app:${appName}`,
        label: appName,
        type: "page-spec",
        childCount: allChildren.length,
        children: allChildren,
      });
    }
  }

  return roots;
}

// =============================================================================
// Module-level cache — survives component unmount/remount (tab navigation)
// =============================================================================

interface SpecsCache {
  specs: LoadedSpec[];
  selection: SpecSelection;
  expandedNodes: Set<string>;
  connection: ConnectionState;
  editMode: boolean;
}

let _cache: SpecsCache | null = null;

export function useSpecsState() {
  // Loaded specs (bundled + discovered + file)
  const [specs, setSpecs] = useState<LoadedSpec[]>(_cache?.specs ?? []);
  const [selection, setSelection] = useState<SpecSelection>(_cache?.selection ?? { type: "none" });
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(
    _cache?.expandedNodes ?? new Set(),
  );
  const [connection, setConnection] = useState<ConnectionState>(
    _cache?.connection ?? { status: "disconnected", url: "" },
  );
  const [isLoading, setIsLoading] = useState(false);
  const [editMode, setEditMode] = useState(_cache?.editMode ?? false);

  const fileLoader = useSpecFileLoader();

  // Whether we restored from cache (checked once on mount)
  const [restoredFromCache] = useState(() => _cache !== null && _cache.specs.length > 0);

  // Sync state to module-level cache so it survives unmount
  useEffect(() => {
    _cache = { specs, selection, expandedNodes, connection, editMode };
  }, [specs, selection, expandedNodes, connection, editMode]);

  // Load bundled + user-saved specs. User specs overlay bundled ones (same specId wins).
  const loadBundledSpecs = useCallback(async () => {
    const discovered = getAllSpecs();
    const bundled: LoadedSpec[] = discovered.map((spec) => ({
      specId: spec.specId,
      appName: spec.appName || "Unknown",
      kind: "page-spec" as const,
      config: spec.config as SpecConfig,
      source: "bundled" as const,
    }));

    let userSpecs: LoadedSpec[] = [];
    try {
      const raw = await invoke<Array<{ specId: string; specJson: SpecConfig }>>("load_user_specs");
      userSpecs = raw.map(({ specId, specJson }) => ({
        specId,
        appName: specId.startsWith("runner:") ? "Qontinui Runner" : "Qontinui Web",
        kind: "page-spec" as const,
        config: specJson,
        source: "file" as const,
      }));
    } catch (e) {
      console.warn("Failed to load user specs:", e);
    }

    // Merge: user specs replace bundled ones with the same ID
    const userIds = new Set(userSpecs.map((s) => s.specId));
    const merged = [...bundled.filter((s) => !userIds.has(s.specId)), ...userSpecs];
    setSpecs(merged);
  }, []);

  // Compute unspecced pages — pages in the app that don't have specs
  const unspeccedPages = useMemo(() => {
    const specIds = new Set(specs.map((s) => s.specId));
    return getKnownPages().filter((page) => !specIds.has(page.expectedSpecId));
  }, [specs]);

  // Build tree from current specs + unspecced pages
  const tree = useMemo(() => buildTreeFromSpecs(specs, unspeccedPages), [specs, unspeccedPages]);

  // Toggle tree node expansion
  const toggleNode = useCallback((nodeId: string) => {
    setExpandedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(nodeId)) next.delete(nodeId);
      else next.add(nodeId);
      return next;
    });
  }, []);

  // Select a tree node
  const selectNode = useCallback((node: SpecTreeNode) => {
    if (node.unspecced && node.specId) {
      setSelection({
        type: "unspecced-page",
        expectedSpecId: node.specId,
        label: node.label,
        description: node.description || "",
      });
    } else if (node.type === "group" && node.specId && node.groupId) {
      setSelection({ type: "group", specId: node.specId, groupId: node.groupId });
    } else if (node.specId) {
      setSelection({ type: "spec", specId: node.specId });
    }
  }, []);

  // Get the currently selected spec
  const selectedSpec = useMemo(() => {
    if (selection.type === "none" || selection.type === "unspecced-page") return null;
    return specs.find((s) => s.specId === selection.specId) || null;
  }, [specs, selection]);

  // Get the currently selected group (only page specs have groups)
  const selectedGroup = useMemo((): SpecGroup | null => {
    if (selection.type !== "group" || !selectedSpec || selectedSpec.kind !== "page-spec")
      return null;
    return selectedSpec.config.groups.find((g) => g.id === selection.groupId) || null;
  }, [selectedSpec, selection]);

  // Discover specs from a connected app via UI Bridge
  const discoverFromApp = useCallback(async (url: string) => {
    setConnection({ status: "connecting", url });
    setIsLoading(true);
    try {
      const { getApiBase } = await import("@/lib/runner-api");
      const resp = await fetch(`${getApiBase()}/ui-bridge/sdk/discover-and-cache`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url }),
      });
      const data = await resp.json();
      if (data.success && data.data) {
        const discovered: LoadedSpec[] = (data.data as Array<Record<string, unknown>>).map(
          (item) => ({
            specId: item.spec_id as string,
            appName: (item.app_name as string) || "Discovered",
            kind: "page-spec" as const,
            config: JSON.parse(item.spec_json as string) as SpecConfig,
            source: "discovered" as const,
          }),
        );
        // Merge with existing (replace discovered, keep bundled + file)
        setSpecs((prev) => {
          const kept = prev.filter((s) => s.source !== "discovered");
          return [...kept, ...discovered];
        });
        setConnection({
          status: "connected",
          url,
          appName: discovered[0]?.appName,
        });
      } else {
        setConnection({
          status: "error",
          url,
          error: data.error || "Discovery failed",
        });
      }
    } catch (err) {
      setConnection({
        status: "error",
        url,
        error: err instanceof Error ? err.message : "Connection failed",
      });
    } finally {
      setIsLoading(false);
    }
  }, []);

  // ============================================================================
  // File loading (Step 3)
  // ============================================================================

  const loadFromFile = useCallback(async () => {
    const spec = await fileLoader.loadFromFile();
    if (spec) {
      setSpecs((prev) => {
        // Replace if same specId exists, otherwise append
        const existing = prev.findIndex((s) => s.specId === spec.specId);
        if (existing >= 0) {
          const next = [...prev];
          next[existing] = spec;
          return next;
        }
        return [...prev, spec];
      });
      setSelection({ type: "spec", specId: spec.specId });
      // Auto-expand if it has children
      setExpandedNodes((prev) => new Set([...prev, spec.specId]));
    }
  }, [fileLoader]);

  const saveToFile = useCallback(async () => {
    if (!selectedSpec) return false;
    return fileLoader.saveToFile(selectedSpec);
  }, [fileLoader, selectedSpec]);

  // ============================================================================
  // Spec CRUD (Step 1 — editing)
  // ============================================================================

  /** Add a spec (used by creation flow) and persist page-specs to the user spec store. */
  const addSpec = useCallback((spec: LoadedSpec) => {
    setSpecs((prev) => {
      const existing = prev.findIndex((s) => s.specId === spec.specId);
      if (existing >= 0) {
        const next = [...prev];
        next[existing] = spec;
        return next;
      }
      return [...prev, spec];
    });

    // Persist page specs to the user spec store so they survive restarts.
    if (spec.kind === "page-spec") {
      const specJson = JSON.stringify(spec.config);
      invoke("save_page_spec", { specId: spec.specId, specJson }).catch((e: unknown) => {
        console.warn("Failed to persist page spec:", e);
      });
    }
    setSelection({ type: "spec", specId: spec.specId });
    setExpandedNodes((prev) => new Set([...prev, spec.specId]));
  }, []);

  /** Remove a spec */
  const removeSpec = useCallback((specId: string) => {
    setSpecs((prev) => prev.filter((s) => s.specId !== specId));
    setSelection((prev) =>
      prev.type !== "none" && "specId" in prev && prev.specId === specId ? { type: "none" } : prev,
    );
  }, []);

  /** Toggle an assertion's enabled state (page-spec only) */
  const toggleAssertion = useCallback((specId: string, groupId: string, assertionId: string) => {
    setSpecs((prev) =>
      prev.map((spec) => {
        if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
        return {
          ...spec,
          config: {
            ...spec.config,
            groups: spec.config.groups.map((g) => {
              if (g.id !== groupId) return g;
              return {
                ...g,
                assertions: g.assertions.map((a) =>
                  a.id === assertionId ? { ...a, enabled: !a.enabled } : a,
                ),
              };
            }),
          },
        };
      }),
    );
  }, []);

  /** Remove an assertion (page-spec only) */
  const removeAssertion = useCallback((specId: string, groupId: string, assertionId: string) => {
    setSpecs((prev) =>
      prev.map((spec) => {
        if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
        return {
          ...spec,
          config: {
            ...spec.config,
            groups: spec.config.groups.map((g) => {
              if (g.id !== groupId) return g;
              return {
                ...g,
                assertions: g.assertions.filter((a) => a.id !== assertionId),
              };
            }),
          },
        };
      }),
    );
  }, []);

  /** Add an assertion to a group (page-spec only) */
  const addAssertion = useCallback((specId: string, groupId: string, assertion: SpecAssertion) => {
    setSpecs((prev) =>
      prev.map((spec) => {
        if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
        return {
          ...spec,
          config: {
            ...spec.config,
            groups: spec.config.groups.map((g) => {
              if (g.id !== groupId) return g;
              return {
                ...g,
                assertions: [...g.assertions, assertion],
              };
            }),
          },
        };
      }),
    );
  }, []);

  /** Add a group to a page spec */
  const addGroup = useCallback((specId: string, group: SpecGroup) => {
    setSpecs((prev) =>
      prev.map((spec) => {
        if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
        return {
          ...spec,
          config: {
            ...spec.config,
            groups: [...spec.config.groups, group],
          },
        };
      }),
    );
    setExpandedNodes((prev) => new Set([...prev, specId]));
  }, []);

  /** Update setup actions for a group (page-spec only) */
  const updateSetupActions = useCallback(
    (specId: string, groupId: string, setupActions: SetupAction[]) => {
      setSpecs((prev) =>
        prev.map((spec) => {
          if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
          return {
            ...spec,
            config: {
              ...spec.config,
              groups: spec.config.groups.map((g) => {
                if (g.id !== groupId) return g;
                return {
                  ...g,
                  setupActions,
                };
              }),
            },
          };
        }),
      );
    },
    [],
  );

  /** Remove a group from a page spec */
  const removeGroup = useCallback((specId: string, groupId: string) => {
    setSpecs((prev) =>
      prev.map((spec) => {
        if (spec.specId !== specId || spec.kind !== "page-spec") return spec;
        return {
          ...spec,
          config: {
            ...spec.config,
            groups: spec.config.groups.filter((g) => g.id !== groupId),
          },
        };
      }),
    );
    // If the removed group was selected, deselect
    setSelection((prev) =>
      prev.type === "group" && prev.specId === specId && prev.groupId === groupId
        ? { type: "spec", specId }
        : prev,
    );
  }, []);

  // Stats
  const stats = useMemo(() => {
    let totalSpecs = specs.length;
    let totalGroups = 0;
    let totalAssertions = 0;
    for (const spec of specs) {
      if (spec.kind === "page-spec") {
        totalGroups += spec.config.groups.length;
        for (const group of spec.config.groups) {
          totalAssertions += group.assertions.filter((a) => a.enabled).length;
        }
      } else if (spec.kind === "architecture") {
        totalGroups += spec.config.features.length;
        totalAssertions += spec.config.constraints.length;
      } else if (spec.kind === "api") {
        totalGroups += spec.config.endpoints.length;
        totalAssertions += spec.config.models.length;
      } else if (spec.kind === "data") {
        totalGroups += spec.config.entities.length;
        totalAssertions += spec.config.relations.length;
      } else if (spec.kind === "dependency") {
        totalGroups += spec.config.modules.length;
        totalAssertions += spec.config.links.length;
      } else if (spec.kind === "constraint") {
        totalGroups +=
          (spec.config.performance?.length || 0) + (spec.config.bundleBudgets?.length || 0);
        totalAssertions +=
          (spec.config.browsers?.length || 0) + (spec.config.capacity?.length || 0);
      }
    }
    return { totalSpecs, totalGroups, totalAssertions };
  }, [specs]);

  return {
    specs,
    tree,
    selection,
    selectedSpec,
    selectedGroup,
    expandedNodes,
    connection,
    isLoading,
    stats,
    editMode,
    restoredFromCache,
    fileLoaderError: fileLoader.error,
    fileLoaderIsLoading: fileLoader.isLoading,
    loadBundledSpecs,
    discoverFromApp,
    toggleNode,
    selectNode,
    setSelection,
    setConnection,
    setEditMode,
    // File loading / saving
    loadFromFile,
    saveToFile,
    // Editing
    addSpec,
    removeSpec,
    toggleAssertion,
    removeAssertion,
    addAssertion,
    addGroup,
    updateSetupActions,
    removeGroup,
  };
}
