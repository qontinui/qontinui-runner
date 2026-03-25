/**
 * useGraphAnalytics
 *
 * TanStack Query hooks for the Knowledge Graph and UI Bridge analytics.
 * Fetches cross-run patterns, step provenance, element interaction history,
 * and element reliability data from the runner's MCP HTTP API.
 */

import { useQuery, useQueryClient } from "@tanstack/react-query";

// ============================================================================
// Types
// ============================================================================

export interface CrossRunPattern {
  id: string;
  pattern_type: string;
  signature_hash: string;
  workflow_name: string | null;
  occurrence_count: number;
  first_seen_task_run_id: string | null;
  last_seen_task_run_id: string | null;
  first_seen_at: string;
  last_seen_at: string;
  status: string;
  resolved_by_fix_id: string | null;
  affected_components: string | null;
  pattern_data: string | null;
  created_at: string;
  updated_at: string;
}

export interface StepProvenance {
  id: string;
  workflow_id: string;
  workflow_version_id: string | null;
  step_name: string;
  step_index: number;
  phase: string;
  generating_agent: string;
  generation_iteration: number | null;
  original_step_json: string | null;
  final_step_json: string | null;
  ui_bridge_event_ids: string | null;
  created_at: string;
}

export interface UiBridgeEvent {
  id: number;
  task_run_id: number | null;
  timestamp: number;
  sequence: number;
  event_type: string;
  element_id: string | null;
  state_id: string | null;
  transition_id: string | null;
  action: string | null;
  params: string | null;
  result: string | null;
  duration_ms: number | null;
  success: boolean;
  error_message: string | null;
  metadata: string | null;
}

export interface ElementReliability {
  element_id: string;
  total_interactions: number;
  successful_interactions: number;
  success_rate: number;
  last_failure_reason: string | null;
  flaky: boolean;
  recommended_confidence: number;
}

export interface PipelineEvent {
  id: string;
  task_run_id: string;
  workflow_id: string | null;
  event_type: string;
  phase: string | null;
  iteration: number | null;
  payload: string | null;
  duration_ms: number | null;
  token_count: number | null;
  validation_errors_before: number | null;
  validation_errors_after: number | null;
  created_at: string;
}

export interface WorkflowVersion {
  id: string;
  workflow_id: string;
  version_number: number;
  parent_version_id: string | null;
  generation_task_run_id: string | null;
  workflow_json: string;
  diff_summary: string | null;
  diff_json: string | null;
  trigger: string;
  created_at: string;
}

export interface RuleInfluence {
  id: string;
  rule_id: string;
  task_run_id: string;
  workflow_id: string | null;
  influence_type: string;
  evidence: string | null;
  phase: string | null;
  created_at: string;
}

export interface IneffectiveRule {
  rule_id: string;
  no_effect_count: number;
  prevented_error_count: number;
  total_loaded_count: number;
}

export interface PhaseStats {
  phase: string;
  event_count: number;
  avg_duration_ms: number | null;
  total_token_count: number;
  avg_errors_before: number | null;
  avg_errors_after: number | null;
}

export interface UnifiedSearchResult {
  entity_type: string;
  entity_id: string;
  title: string;
  snippet: string;
  relevance_score: number;
  source_table: string;
}

// ============================================================================
// Fetchers
// ============================================================================

const API_BASE = "http://localhost:9876";

async function fetchJson<T>(path: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const json = await res.json();
  return json.data ?? json;
}

// ============================================================================
// Query keys
// ============================================================================

export const graphAnalyticsKeys = {
  all: ["graph-analytics"] as const,
  crossRunPatterns: (workflowName?: string) =>
    [...graphAnalyticsKeys.all, "cross-run-patterns", workflowName ?? "all"] as const,
  stepProvenance: (workflowId: string) =>
    [...graphAnalyticsKeys.all, "step-provenance", workflowId] as const,
  elementHistory: (taskRunId: number) =>
    [...graphAnalyticsKeys.all, "element-history", taskRunId] as const,
  flakyElements: () => [...graphAnalyticsKeys.all, "flaky-elements"] as const,
  elementReliability: (elementId: string) =>
    [...graphAnalyticsKeys.all, "element-reliability", elementId] as const,
  pipelineEvents: (taskRunId: string) =>
    [...graphAnalyticsKeys.all, "pipeline-events", taskRunId] as const,
  workflowVersions: (workflowId: string) =>
    [...graphAnalyticsKeys.all, "workflow-versions", workflowId] as const,
  ruleInfluence: (ruleId: string) =>
    [...graphAnalyticsKeys.all, "rule-influence", ruleId] as const,
  ineffectiveRules: (threshold?: number) =>
    [...graphAnalyticsKeys.all, "ineffective-rules", threshold ?? 3] as const,
  phaseStats: (workflowId: string) =>
    [...graphAnalyticsKeys.all, "phase-stats", workflowId] as const,
  unifiedSearch: (query: string) =>
    [...graphAnalyticsKeys.all, "search", query] as const,
};

// ============================================================================
// Hooks
// ============================================================================

/** Fetch cross-run patterns (recurring findings, fix oscillations). */
export function useCrossRunPatterns(workflowName?: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.crossRunPatterns(workflowName),
    queryFn: () => {
      const params = workflowName ? `?workflow_name=${encodeURIComponent(workflowName)}` : "";
      return fetchJson<CrossRunPattern[]>(`/graph/cross-run-patterns${params}`);
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** Fetch step provenance for a workflow. */
export function useStepProvenance(workflowId: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.stepProvenance(workflowId),
    queryFn: () =>
      fetchJson<StepProvenance[]>(`/graph/step-provenance?workflow_id=${encodeURIComponent(workflowId)}`),
    staleTime: 30_000,
    enabled: !!workflowId,
  });
}

/** Fetch UI Bridge element interactions for a task run. */
export function useElementHistory(taskRunId: number) {
  return useQuery({
    queryKey: graphAnalyticsKeys.elementHistory(taskRunId),
    queryFn: () =>
      fetchJson<UiBridgeEvent[]>(`/ui-bridge/history/elements?task_run_id=${taskRunId}`),
    staleTime: 30_000,
    enabled: taskRunId > 0,
  });
}

/** Fetch flaky elements across all runs. */
export function useFlakyElements() {
  return useQuery({
    queryKey: graphAnalyticsKeys.flakyElements(),
    queryFn: () =>
      fetchJson<ElementReliability[]>("/ui-bridge/history/flaky?min_interactions=5&max_success_rate=0.9"),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** Fetch pipeline events for a task run. */
export function usePipelineEvents(taskRunId: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.pipelineEvents(taskRunId),
    queryFn: () => fetchJson<PipelineEvent[]>(`/graph/pipeline-events?task_run_id=${encodeURIComponent(taskRunId)}`),
    staleTime: 30_000,
    enabled: !!taskRunId,
  });
}

/** Fetch workflow versions for a workflow. */
export function useWorkflowVersions(workflowId: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.workflowVersions(workflowId),
    queryFn: () => fetchJson<WorkflowVersion[]>(`/graph/workflow-versions?workflow_id=${encodeURIComponent(workflowId)}`),
    staleTime: 30_000,
    enabled: !!workflowId,
  });
}

/** Fetch rule influence records for a rule. */
export function useRuleInfluence(ruleId: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.ruleInfluence(ruleId),
    queryFn: () => fetchJson<RuleInfluence[]>(`/graph/rule-influence?rule_id=${encodeURIComponent(ruleId)}`),
    staleTime: 30_000,
    enabled: !!ruleId,
  });
}

/** Fetch rules that appear to have no effect. */
export function useIneffectiveRules(threshold?: number) {
  return useQuery({
    queryKey: graphAnalyticsKeys.ineffectiveRules(threshold),
    queryFn: () => fetchJson<IneffectiveRule[]>(`/graph/ineffective-rules?threshold=${threshold ?? 3}`),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** Fetch aggregated phase statistics for a workflow. */
export function usePhaseStats(workflowId: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.phaseStats(workflowId),
    queryFn: () => fetchJson<PhaseStats[]>(`/graph/phase-stats?workflow_id=${encodeURIComponent(workflowId)}`),
    staleTime: 30_000,
    enabled: !!workflowId,
  });
}

/** Unified search across knowledge graph entities. */
export function useUnifiedSearch(query: string) {
  return useQuery({
    queryKey: graphAnalyticsKeys.unifiedSearch(query),
    queryFn: () => fetchJson<UnifiedSearchResult[]>(`/graph/search?q=${encodeURIComponent(query)}&limit=20`),
    staleTime: 10_000,
    enabled: query.length >= 2,
  });
}

/** Refresh all graph analytics queries. */
export function useGraphAnalyticsRefresh() {
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: graphAnalyticsKeys.all });
}
