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

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { graph, loading, error, refresh, rebuild };
}

export function useComponentDetails(workflowName: string, componentPath: string | null) {
  const [details, setDetails] = useState<ComponentDetails | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!workflowName || !componentPath) {
      setDetails(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/architecture/component?workflow_name=${encodeURIComponent(workflowName)}&path=${encodeURIComponent(componentPath)}`,
        );
        const json: ApiResponse<ComponentDetails> = await res.json();
        if (!cancelled && json.success && json.data) {
          setDetails(json.data);
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
  }, [workflowName, componentPath]);

  return { details, loading };
}

export function useImpactAnalysis(workflowName: string, componentPath: string | null) {
  const [impact, setImpact] = useState<ImpactAnalysis | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!workflowName || !componentPath) {
      setImpact(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/architecture/impact?workflow_name=${encodeURIComponent(workflowName)}&component=${encodeURIComponent(componentPath)}`,
        );
        const json: ApiResponse<ImpactAnalysis> = await res.json();
        if (!cancelled && json.success && json.data) {
          setImpact(json.data);
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
  }, [workflowName, componentPath]);

  return { impact, loading };
}

export function useWorkflowTrends(workflowName: string, timeRange: TimeRange) {
  const [trends, setTrends] = useState<WorkflowTrends | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!workflowName) {
      setTrends(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends?workflow_name=${encodeURIComponent(workflowName)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<WorkflowTrends> = await res.json();
        if (!cancelled && json.success && json.data) {
          setTrends(json.data);
        } else if (!cancelled) {
          setError(json.error ?? "Failed to load trends");
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Network error");
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [workflowName, timeRange]);

  return { trends, loading, error };
}

export function useComponentTrend(
  workflowName: string,
  componentPath: string | null,
  timeRange: TimeRange,
) {
  const [trend, setTrend] = useState<ComponentTrend | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!workflowName || !componentPath) {
      setTrend(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends/component?workflow_name=${encodeURIComponent(workflowName)}&path=${encodeURIComponent(componentPath)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<ComponentTrend> = await res.json();
        if (!cancelled && json.success && json.data) {
          setTrend(json.data);
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
  }, [workflowName, componentPath, timeRange]);

  return { trend, loading };
}

export function useEffectivenessTrend(
  workflowName: string,
  timeRange: TimeRange,
  bucket: "week" | "month" = "week",
) {
  const [data, setData] = useState<EffectivenessOverTime | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!workflowName) {
      setData(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/reflection/trends/effectiveness?workflow_name=${encodeURIComponent(workflowName)}&bucket=${encodeURIComponent(bucket)}&time_range=${encodeURIComponent(timeRange)}`,
        );
        const json: ApiResponse<EffectivenessOverTime> = await res.json();
        if (!cancelled && json.success && json.data) {
          setData(json.data);
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
  }, [workflowName, timeRange, bucket]);

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
                });
              }

              // Add tech stack nodes
              for (const tech of parsed.techStack ?? []) {
                nodes.push({
                  id: `tech:${tech.name}`,
                  label: tech.name,
                  type: "tech",
                });
              }

              // Add pattern nodes
              for (const pattern of parsed.patterns ?? []) {
                nodes.push({
                  id: `pattern:${pattern.id}`,
                  label: pattern.name,
                  type: "pattern",
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
                appUrl: spec.app_url,
                nodes,
                edges,
                techStack: parsed.techStack ?? [],
                directories: parsed.directories ?? [],
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

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { specs, loading, error, refresh };
}
