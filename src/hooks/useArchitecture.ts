import { useState, useEffect, useCallback } from "react";
import { tracedFetch } from "@/lib/traced-fetch";
import { getApiBase } from "@/lib/runner-api";
import type {
  ComponentGraph,
  ComponentDetails,
  ImpactAnalysis,
  RebuildResult,
  WorkflowTrends,
  ComponentTrend,
  EffectivenessOverTime,
  SdkArchitectureGraph,
  SdkArchitectureNode,
  SdkArchitectureEdge,
} from "@/types/architecture";
import type { TimeRange } from "@/types/performance-metrics";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export function useArchitectureGraph(workflowName: string) {
  const [graph, setGraph] = useState<ComponentGraph | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!workflowName) return;
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(
        `${getApiBase()}/reflection/architecture?workflow_name=${encodeURIComponent(workflowName)}`,
      );
      const json: ApiResponse<ComponentGraph> = await res.json();
      if (json.success && json.data) {
        setGraph(json.data);
      } else {
        setError(json.error ?? "Failed to load architecture graph");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Network error");
    } finally {
      setLoading(false);
    }
  }, [workflowName]);

  const rebuild = useCallback(async (): Promise<RebuildResult | null> => {
    if (!workflowName) return null;
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(
        `${getApiBase()}/reflection/architecture/rebuild?workflow_name=${encodeURIComponent(workflowName)}`,
        { method: "POST" },
      );
      const json: ApiResponse<RebuildResult> = await res.json();
      if (json.success && json.data) {
        // Auto-refresh graph after rebuild
        await refresh();
        return json.data;
      } else {
        setError(json.error ?? "Failed to rebuild");
        return null;
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Network error");
      return null;
    } finally {
      setLoading(false);
    }
  }, [workflowName, refresh]);

  // Wrap refresh() in async IIFE so its synchronous setLoading(true) isn't
  // the first statement reached from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await refresh();
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  return { graph, loading, error, refresh, rebuild };
}

export function useComponentDetails(workflowName: string, componentPath: string | null) {
  // Tag the fetched details with its (workflowName, componentPath) key so we
  // can derive `details` as a pure value — no setState(null) or setLoading
  // in the effect body for the invalid-deps case (Recipe 1: reset-on-invalid-deps).
  const [fetched, setFetched] = useState<{
    key: string | null;
    data: ComponentDetails | null;
  }>({ key: null, data: null });
  const [loading, setLoading] = useState(false);

  const currentKey = workflowName && componentPath ? `${workflowName}::${componentPath}` : null;

  useEffect(() => {
    if (!currentKey || !workflowName || !componentPath) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/architecture/component?workflow_name=${encodeURIComponent(workflowName)}&path=${encodeURIComponent(componentPath)}`,
        );
        const json: ApiResponse<ComponentDetails> = await res.json();
        if (!cancelled && json.success && json.data) {
          setFetched({ key: currentKey, data: json.data });
        }
      } catch {
        // Silently ignore — component may not exist
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, componentPath, currentKey]);

  const details = fetched.key === currentKey ? fetched.data : null;
  return { details, loading };
}

export function useImpactAnalysis(workflowName: string, componentPath: string | null) {
  // Tag the fetched impact with its key to derive `impact` as a pure value
  // — no setState in the effect body for the invalid-deps case
  // (Recipe 1: reset-on-invalid-deps).
  const [fetched, setFetched] = useState<{
    key: string | null;
    data: ImpactAnalysis | null;
  }>({ key: null, data: null });
  const [loading, setLoading] = useState(false);

  const currentKey = workflowName && componentPath ? `${workflowName}::${componentPath}` : null;

  useEffect(() => {
    if (!currentKey || !workflowName || !componentPath) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/architecture/impact?workflow_name=${encodeURIComponent(workflowName)}&component=${encodeURIComponent(componentPath)}`,
        );
        const json: ApiResponse<ImpactAnalysis> = await res.json();
        if (!cancelled && json.success && json.data) {
          setFetched({ key: currentKey, data: json.data });
        }
      } catch {
        // Silently ignore
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, componentPath, currentKey]);

  const impact = fetched.key === currentKey ? fetched.data : null;
  return { impact, loading };
}

export function useWorkflowTrends(workflowName: string, timeRange: TimeRange) {
  // Tag fetched trends (and error) with the request key so they derive to
  // null when the deps become invalid — no setState in the effect body for
  // the invalid-deps case (Recipe 1: reset-on-invalid-deps).
  const [fetched, setFetched] = useState<{
    key: string | null;
    data: WorkflowTrends | null;
    error: string | null;
  }>({ key: null, data: null, error: null });
  const [loading, setLoading] = useState(false);

  const currentKey = workflowName ? `${workflowName}::${timeRange}` : null;

  useEffect(() => {
    if (!currentKey || !workflowName) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends?workflow_name=${encodeURIComponent(workflowName)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<WorkflowTrends> = await res.json();
        if (cancelled) return;
        if (json.success && json.data) {
          setFetched({ key: currentKey, data: json.data, error: null });
        } else {
          setFetched({ key: currentKey, data: null, error: json.error ?? "Failed to load trends" });
        }
      } catch (e) {
        if (!cancelled) {
          setFetched({
            key: currentKey,
            data: null,
            error: e instanceof Error ? e.message : "Network error",
          });
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, timeRange, currentKey]);

  const trends = fetched.key === currentKey ? fetched.data : null;
  const error = fetched.key === currentKey ? fetched.error : null;
  return { trends, loading, error };
}

export function useComponentTrend(
  workflowName: string,
  componentPath: string | null,
  timeRange: TimeRange,
) {
  // Tag fetched trend with its key to derive null when deps invalid — no
  // setState(null) in the effect body (Recipe 1: reset-on-invalid-deps).
  const [fetched, setFetched] = useState<{
    key: string | null;
    data: ComponentTrend | null;
  }>({ key: null, data: null });
  const [loading, setLoading] = useState(false);

  const currentKey =
    workflowName && componentPath ? `${workflowName}::${componentPath}::${timeRange}` : null;

  useEffect(() => {
    if (!currentKey || !workflowName || !componentPath) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends/component?workflow_name=${encodeURIComponent(workflowName)}&path=${encodeURIComponent(componentPath)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<ComponentTrend> = await res.json();
        if (!cancelled && json.success && json.data) {
          setFetched({ key: currentKey, data: json.data });
        }
      } catch {
        // Silently ignore
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, componentPath, timeRange, currentKey]);

  const trend = fetched.key === currentKey ? fetched.data : null;
  return { trend, loading };
}

export function useEffectivenessTrend(
  workflowName: string,
  timeRange: TimeRange,
  bucket: "week" | "month" = "week",
) {
  // Tag fetched effectiveness with its key to derive null when deps invalid
  // — no setState(null) in the effect body (Recipe 1: reset-on-invalid-deps).
  const [fetched, setFetched] = useState<{
    key: string | null;
    data: EffectivenessOverTime | null;
  }>({ key: null, data: null });
  const [loading, setLoading] = useState(false);

  const currentKey = workflowName ? `${workflowName}::${timeRange}::${bucket}` : null;

  useEffect(() => {
    if (!currentKey || !workflowName) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends/effectiveness?workflow_name=${encodeURIComponent(workflowName)}&bucket=${encodeURIComponent(bucket)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<EffectivenessOverTime> = await res.json();
        if (!cancelled && json.success && json.data) {
          setFetched({ key: currentKey, data: json.data });
        }
      } catch {
        // Silently ignore
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, timeRange, bucket, currentKey]);

  const data = fetched.key === currentKey ? fetched.data : null;
  return { data, loading };
}

export function useSdkArchitecture() {
  const [specs, setSpecs] = useState<SdkArchitectureGraph[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/cached-specs`);
      const json = await res.json();
      if (json.success && json.data) {
        const graphs: SdkArchitectureGraph[] = [];
        for (const spec of json.data) {
          try {
            const parsed =
              typeof spec.spec_json === "string" ? JSON.parse(spec.spec_json) : spec.spec_json;
            // Detect architecture specs by structure
            if (parsed.techStack && parsed.features) {
              const nodes: SdkArchitectureNode[] = [];
              const edges: SdkArchitectureEdge[] = [];

              // Add feature nodes
              for (const feature of parsed.features ?? []) {
                nodes.push({
                  id: `feature:${feature.id}`,
                  label: feature.name,
                  type: "feature",
                  status: feature.status,
                  priority: feature.priority,
                  description: feature.description,
                  entryPoints: feature.entryPoints,
                  techUsed: feature.techUsed,
                });
              }

              // Add tech stack nodes
              for (const tech of parsed.techStack ?? []) {
                nodes.push({
                  id: `tech:${tech.name}`,
                  label: tech.name,
                  type: "tech",
                  category: tech.category,
                  description: tech.purpose,
                });
              }

              // Add pattern nodes
              for (const pattern of parsed.patterns ?? []) {
                nodes.push({
                  id: `pattern:${pattern.id}`,
                  label: pattern.name,
                  type: "pattern",
                  description: pattern.description,
                  category: pattern.category,
                  usedBy: pattern.usedBy,
                });
              }

              // Add dependency edges
              for (const dep of parsed.dependencies ?? []) {
                edges.push({
                  source: `feature:${dep.featureId}`,
                  target: `feature:${dep.dependsOn}`,
                  type: dep.type,
                  label: dep.description,
                });
              }

              graphs.push({
                projectName: parsed.projectName ?? spec.app_name ?? "Unknown",
                description: parsed.description ?? "",
                appUrl: spec.app_url,
                nodes,
                edges,
                techStack: parsed.techStack ?? [],
                directories: parsed.directories ?? [],
                constraints: (parsed.constraints ?? []).map((c: Record<string, unknown>) => ({
                  id: (c.id as string) ?? "",
                  description: (c.description as string) ?? "",
                  category: (c.category as string) ?? "",
                })),
                agenticStructure: parsed.agenticStructure
                  ? {
                      agents: (parsed.agenticStructure.agents ?? []).map(
                        (a: Record<string, unknown>) => ({
                          id: (a.id as string) ?? "",
                          name: (a.name as string) ?? "",
                          role: (a.role as string) ?? "executor",
                          description: (a.description as string) ?? "",
                          parentAgent: a.parentAgent as string | undefined,
                          capabilities: (a.capabilities as string[]) ?? [],
                          delegatesTo: a.delegatesTo as string[] | undefined,
                          triggers: a.triggers as string[] | undefined,
                          decisionAuthority: a.decisionAuthority as string[] | undefined,
                        }),
                      ),
                      feedbackLoops: (parsed.agenticStructure.feedbackLoops ?? []).map(
                        (l: Record<string, unknown>) => ({
                          id: (l.id as string) ?? "",
                          name: (l.name as string) ?? "",
                          description: (l.description as string) ?? "",
                          agents: (l.agents as string[]) ?? [],
                          exitCondition: (l.exitCondition as string) ?? "",
                          type: ((l.type as string) ?? "inner") as "inner" | "outer" | "cross",
                        }),
                      ),
                      delegationChains: (parsed.agenticStructure.delegationChains ?? []).map(
                        (d: Record<string, unknown>) => ({
                          from: (d.from as string) ?? "",
                          to: (d.to as string) ?? "",
                          via: (d.via as string) ?? "",
                          description: (d.description as string) ?? "",
                        }),
                      ),
                    }
                  : undefined,
              });
            }
          } catch {
            // Skip unparseable specs
          }
        }
        setSpecs(graphs);
      } else {
        setError(json.error ?? "Failed to load SDK specs");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Network error");
    } finally {
      setLoading(false);
    }
  }, []);

  // Wrap refresh() in async IIFE so its synchronous setLoading(true) isn't
  // the first statement reached from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await refresh();
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  return { specs, loading, error, refresh };
}
