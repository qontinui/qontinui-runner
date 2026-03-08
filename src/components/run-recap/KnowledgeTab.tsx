/**
 * KnowledgeTab Component
 *
 * Displays findings and knowledge entries from a task run.
 * Groups findings by severity and shows knowledge timeline by iteration.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Bug,
  Shield,
  Zap,
  CheckCircle,
  Sparkles,
  Settings,
  FileText,
  FlaskConical,
  Filter,
  Clock,
  ChevronDown,
  ChevronRight,
  Loader2,
  Eye,
  Lightbulb,
  Search,
  Target,
  Wrench,
  Info,
  RotateCcw,
} from "lucide-react";
import type { Finding, FindingSeverity } from "@/types/findings";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface KnowledgeTabProps {
  taskRunId: string;
}

interface KnowledgeEntry {
  id: string;
  task_run_id: string;
  category: string;
  agent_type: string;
  iteration: number;
  content: string;
  evidence?: string;
  confidence: string;
  related_files: string[];
  related_criterion_id?: string;
  is_resolved: boolean;
  resolution_notes?: string;
  resolved_at?: string;
  created_at: string;
}

// Category icons mapping
const CATEGORY_ICONS: Record<string, typeof Bug> = {
  code_bug: Bug,
  security: Shield,
  performance: Zap,
  todo: CheckCircle,
  enhancement: Sparkles,
  config_issue: Settings,
  documentation: FileText,
  test_issue: FlaskConical,
};

// Knowledge category icons and colors
const KNOWLEDGE_CATEGORY_CONFIG: Record<
  string,
  { icon: typeof Bug; color: string; label: string }
> = {
  finding: { icon: Search, color: "text-amber-500", label: "Finding" },
  root_cause: { icon: Target, color: "text-red-400", label: "Root Cause" },
  observation: { icon: Eye, color: "text-blue-400", label: "Observation" },
  hypothesis: { icon: Lightbulb, color: "text-purple-400", label: "Hypothesis" },
  solution: { icon: Wrench, color: "text-green-400", label: "Solution" },
  context: { icon: Info, color: "text-gray-400", label: "Context" },
};

const CONFIDENCE_COLORS: Record<string, string> = {
  high: "bg-green-500/10 text-green-400",
  medium: "bg-amber-500/10 text-amber-400",
  low: "bg-red-500/10 text-red-400",
};

// Severity colors
const SEVERITY_COLORS: Record<FindingSeverity, { bg: string; text: string; border: string }> = {
  critical: { bg: "bg-red-500/10", text: "text-red-500", border: "border-red-500/30" },
  high: { bg: "bg-orange-500/10", text: "text-orange-500", border: "border-orange-500/30" },
  medium: { bg: "bg-amber-500/10", text: "text-amber-500", border: "border-amber-500/30" },
  low: { bg: "bg-blue-500/10", text: "text-blue-500", border: "border-blue-500/30" },
  info: { bg: "bg-gray-500/10", text: "text-gray-400", border: "border-gray-500/30" },
};

interface ReflectionFix {
  id: string;
  source_task_run_id: string;
  reflection_task_run_id: string;
  fix_type: string;
  fix_description: string;
  file_changed?: string;
  old_value?: string;
  new_value?: string;
  confidence: string;
  status: string;
  effectiveness?: string;
  effectiveness_evidence?: string;
  applied_at: string;
  created_at: string;
}

const FIX_TYPE_COLORS: Record<string, { bg: string; text: string }> = {
  knowledge_base_update: { bg: "bg-blue-500/10", text: "text-blue-400" },
  workflow_step_rewrite: { bg: "bg-purple-500/10", text: "text-purple-400" },
  selector_fix: { bg: "bg-amber-500/10", text: "text-amber-400" },
  tool_config_update: { bg: "bg-green-500/10", text: "text-green-400" },
  context_addition: { bg: "bg-cyan-500/10", text: "text-cyan-400" },
  instruction_clarification: { bg: "bg-rose-500/10", text: "text-rose-400" },
};

const EFFECTIVENESS_COLORS: Record<string, { bg: string; text: string }> = {
  effective: { bg: "bg-green-500/10", text: "text-green-400" },
  ineffective: { bg: "bg-red-500/10", text: "text-red-400" },
  caused_regression: { bg: "bg-red-500/10", text: "text-red-400" },
  inconclusive: { bg: "bg-gray-500/10", text: "text-gray-400" },
};

export function KnowledgeTab({ taskRunId }: KnowledgeTabProps) {
  const [findings, setFindings] = useState<Finding[]>([]);
  const [knowledgeEntries, setKnowledgeEntries] = useState<KnowledgeEntry[]>([]);
  const [knowledgeLoading, setKnowledgeLoading] = useState(true);
  const [categoryFilter, setCategoryFilter] = useState<string>("all");
  const [agentFilter, setAgentFilter] = useState<string>("all");
  const [expandedFindings, setExpandedFindings] = useState<Set<string>>(new Set());
  const [expandedKnowledge, setExpandedKnowledge] = useState<Set<string>>(new Set());
  const [reflectionFixes, setReflectionFixes] = useState<ReflectionFix[]>([]);
  const [reflectionLoading, setReflectionLoading] = useState(true);
  const [expandedFixes, setExpandedFixes] = useState<Set<string>>(new Set());
  const [triggeringReflection, setTriggeringReflection] = useState(false);

  // Fetch findings for this specific task run using Tauri command
  useEffect(() => {
    const fetchFindings = async () => {
      if (!taskRunId) return;

      try {
        const findingsArray = await invoke<Finding[]>("get_task_findings", {
          taskRunId,
        });
        setFindings(findingsArray || []);
      } catch (error) {
        console.error("Failed to fetch findings:", error);
        setFindings([]);
      }
    };

    fetchFindings();
    // Poll less frequently since findings don't change as often
    const interval = setInterval(fetchFindings, 10000);
    return () => clearInterval(interval);
  }, [taskRunId]);

  // Fetch knowledge entries
  useEffect(() => {
    const fetchKnowledge = async () => {
      if (!taskRunId) return;

      try {
        const entries = await invoke<KnowledgeEntry[]>("list_task_knowledge_cmd", {
          taskRunId,
          category: null,
        });
        setKnowledgeEntries(entries || []);
      } catch (error) {
        console.error("Failed to fetch knowledge entries:", error);
        setKnowledgeEntries([]);
      } finally {
        setKnowledgeLoading(false);
      }
    };

    fetchKnowledge();
    const interval = setInterval(fetchKnowledge, 10000);
    return () => clearInterval(interval);
  }, [taskRunId]);

  // Fetch reflection fixes
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    const fetchReflectionFixes = async () => {
      if (!taskRunId) return;

      try {
        const response = await tracedFetch(
          `${getApiBase()}/task-runs/${taskRunId}/reflection-fixes`,
          {
            signal: controller.signal,
          },
        );
        if (response.ok) {
          const result = await response.json();
          if (!cancelled && result.success && result.data) {
            setReflectionFixes(result.data);
          }
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Failed to fetch reflection fixes:", error);
          setReflectionFixes([]);
        }
      } finally {
        if (!cancelled) setReflectionLoading(false);
      }
    };

    fetchReflectionFixes();
    const interval = setInterval(fetchReflectionFixes, 10000);
    return () => {
      cancelled = true;
      controller.abort();
      clearInterval(interval);
    };
  }, [taskRunId]);

  const triggerReflection = async () => {
    setTriggeringReflection(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/reflection/trigger/${taskRunId}`, {
        method: "POST",
      });
      if (response.ok) {
        const result = await response.json();
        if (result.success) {
          console.log("Reflection triggered successfully");
        }
      }
    } catch (error) {
      console.error("Failed to trigger reflection:", error);
    } finally {
      setTriggeringReflection(false);
    }
  };

  const toggleFix = (id: string) => {
    setExpandedFixes((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const toggleKnowledge = (id: string) => {
    setExpandedKnowledge((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const toggleFinding = (id: string) => {
    setExpandedFindings((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  // Group findings by severity
  const findingsBySeverity = findings.reduce(
    (acc, finding) => {
      const severity = finding.severity || "info";
      if (!acc[severity]) acc[severity] = [];
      acc[severity].push(finding);
      return acc;
    },
    {} as Record<string, Finding[]>,
  );

  // Categories for filter (from knowledge entries)
  const knowledgeCategories = ["all", ...new Set(knowledgeEntries.map((e) => e.category))];
  const knowledgeAgents = ["all", ...new Set(knowledgeEntries.map((e) => e.agent_type))];

  // Filter knowledge entries
  const filteredKnowledge = knowledgeEntries.filter((entry) => {
    if (categoryFilter !== "all" && entry.category !== categoryFilter) return false;
    if (agentFilter !== "all" && entry.agent_type !== agentFilter) return false;
    return true;
  });

  // Group by iteration
  const knowledgeByIteration = filteredKnowledge.reduce(
    (acc, entry) => {
      const iter = entry.iteration || 0;
      if (!acc[iter]) acc[iter] = [];
      acc[iter].push(entry);
      return acc;
    },
    {} as Record<number, KnowledgeEntry[]>,
  );
  const iterations = Object.keys(knowledgeByIteration)
    .map(Number)
    .sort((a, b) => a - b);

  return (
    <div className="space-y-4">
      {/* Findings Panel */}
      <div className="card p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-medium flex items-center gap-2">
            <AlertTriangle className="w-5 h-5 text-amber-500" />
            Findings ({findings.length})
          </h3>
        </div>

        {/* Findings by Severity */}
        {(["critical", "high", "medium", "low", "info"] as FindingSeverity[]).map((severity) => {
          const severityFindings = findingsBySeverity[severity] || [];
          if (severityFindings.length === 0) return null;

          const colors = SEVERITY_COLORS[severity];

          return (
            <div key={severity} className={`mb-3 rounded-lg border ${colors.border} ${colors.bg}`}>
              <div
                data-content-role="heading"
                data-content-label={`${severity} findings`}
                className={`px-3 py-2 font-medium ${colors.text} capitalize`}
              >
                {severity} ({severityFindings.length})
              </div>
              <div className="divide-y divide-border/50">
                {severityFindings.map((finding) => {
                  const Icon = CATEGORY_ICONS[finding.categoryId] || AlertTriangle;
                  const isExpanded = expandedFindings.has(finding.id);

                  return (
                    <div key={finding.id} className="px-3 py-2">
                      <button
                        onClick={() => toggleFinding(finding.id)}
                        className="w-full flex items-center gap-2 text-left"
                      >
                        <Icon className={`w-4 h-4 ${colors.text}`} />
                        <span
                          className={`text-xs px-1.5 py-0.5 rounded ${colors.bg} ${colors.text}`}
                        >
                          {finding.categoryId}
                        </span>
                        <span className="flex-1 text-sm font-medium truncate">{finding.title}</span>
                        {isExpanded ? (
                          <ChevronDown className="w-4 h-4 text-muted-foreground" />
                        ) : (
                          <ChevronRight className="w-4 h-4 text-muted-foreground" />
                        )}
                      </button>
                      {isExpanded && (
                        <div className="mt-2 ml-6 text-sm text-muted-foreground">
                          <p>{finding.description}</p>
                          {finding.codeContext?.file && (
                            <p className="mt-1 text-xs">
                              File: {finding.codeContext.file}
                              {finding.codeContext.line && `:${finding.codeContext.line}`}
                            </p>
                          )}
                          {finding.resolution && (
                            <p className="mt-1 text-green-400">Resolution: {finding.resolution}</p>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          );
        })}

        {findings.length === 0 && (
          <div className="px-3 py-4 text-center">
            <p className="text-sm text-muted-foreground">No findings recorded for this run.</p>
          </div>
        )}
      </div>

      {/* Reflection Fixes Panel */}
      {(reflectionFixes.length > 0 || !reflectionLoading) && (
        <div className="card p-4">
          <div className="flex items-center justify-between mb-4">
            <h3 className="font-medium flex items-center gap-2">
              <RotateCcw className="w-5 h-5 text-purple-400" />
              <Wrench className="w-4 h-4 text-purple-400 -ml-1" />
              Reflection Fixes ({reflectionFixes.length})
            </h3>
            <button
              onClick={triggerReflection}
              disabled={triggeringReflection}
              className="px-3 py-1.5 text-xs font-medium bg-purple-500/10 text-purple-400 hover:bg-purple-500/20 rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-1.5"
            >
              {triggeringReflection ? (
                <>
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                  Triggering...
                </>
              ) : (
                <>
                  <RotateCcw className="w-3.5 h-3.5" />
                  Trigger Reflection
                </>
              )}
            </button>
          </div>

          {reflectionLoading ? (
            <div className="p-4 flex items-center justify-center gap-2 text-muted-foreground">
              <Loader2 className="w-4 h-4 animate-spin" />
              <span className="text-sm">Loading reflection fixes...</span>
            </div>
          ) : reflectionFixes.length > 0 ? (
            <div className="space-y-2">
              {reflectionFixes.map((fix) => {
                const typeColors = FIX_TYPE_COLORS[fix.fix_type] || {
                  bg: "bg-gray-500/10",
                  text: "text-gray-400",
                };
                const confColors =
                  CONFIDENCE_COLORS[fix.confidence] || "bg-gray-500/10 text-gray-400";
                const effectColors = fix.effectiveness
                  ? EFFECTIVENESS_COLORS[fix.effectiveness] || {
                      bg: "bg-gray-500/10",
                      text: "text-gray-400",
                    }
                  : null;
                const isExpanded = expandedFixes.has(fix.id);

                return (
                  <div key={fix.id} className="rounded-lg border border-border bg-muted/20">
                    <button
                      onClick={() => toggleFix(fix.id)}
                      className="w-full px-3 py-2 flex items-center gap-2 text-left hover:bg-muted/40 transition-colors rounded-lg"
                    >
                      <Wrench className={`w-4 h-4 flex-shrink-0 ${typeColors.text}`} />
                      <span
                        className={`text-xs px-1.5 py-0.5 rounded ${typeColors.bg} ${typeColors.text}`}
                      >
                        {fix.fix_type.replace(/_/g, " ")}
                      </span>
                      <span className={`text-xs px-1.5 py-0.5 rounded ${confColors}`}>
                        {fix.confidence}
                      </span>
                      {effectColors && (
                        <span
                          className={`text-xs px-1.5 py-0.5 rounded ${effectColors.bg} ${effectColors.text}`}
                        >
                          {fix.effectiveness}
                        </span>
                      )}
                      <span className="flex-1 text-sm truncate">{fix.fix_description}</span>
                      {isExpanded ? (
                        <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                      ) : (
                        <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                      )}
                    </button>

                    {isExpanded && (
                      <div className="px-3 pb-3 space-y-2 border-t border-border/50 mt-1 pt-2">
                        <p className="text-sm text-foreground/80">{fix.fix_description}</p>

                        {fix.file_changed && (
                          <div>
                            <span className="text-xs font-medium text-muted-foreground">
                              File changed:
                            </span>
                            <span className="text-xs font-mono bg-muted px-1.5 py-0.5 rounded ml-1">
                              {fix.file_changed}
                            </span>
                          </div>
                        )}

                        {(fix.old_value || fix.new_value) && (
                          <div className="space-y-1.5">
                            {fix.old_value && (
                              <div>
                                <span className="text-xs font-medium text-red-400">Old value:</span>
                                <pre className="text-xs bg-red-500/5 text-red-300 rounded p-2 mt-0.5 overflow-x-auto whitespace-pre-wrap">
                                  {fix.old_value}
                                </pre>
                              </div>
                            )}
                            {fix.new_value && (
                              <div>
                                <span className="text-xs font-medium text-green-400">
                                  New value:
                                </span>
                                <pre className="text-xs bg-green-500/5 text-green-300 rounded p-2 mt-0.5 overflow-x-auto whitespace-pre-wrap">
                                  {fix.new_value}
                                </pre>
                              </div>
                            )}
                          </div>
                        )}

                        {fix.effectiveness_evidence && (
                          <div>
                            <span className="text-xs font-medium text-muted-foreground">
                              Effectiveness evidence:
                            </span>
                            <p className="text-sm text-foreground/70 mt-0.5">
                              {fix.effectiveness_evidence}
                            </p>
                          </div>
                        )}

                        <div className="flex items-center gap-3 text-xs text-muted-foreground">
                          <span>Status: {fix.status}</span>
                          {fix.applied_at && (
                            <span>Applied: {new Date(fix.applied_at).toLocaleString()}</span>
                          )}
                          {fix.created_at && (
                            <span>Created: {new Date(fix.created_at).toLocaleString()}</span>
                          )}
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="px-3 py-4 text-center">
              <p className="text-sm text-muted-foreground">
                No reflection fixes applied for this run. Use the button above to trigger a
                reflection analysis.
              </p>
            </div>
          )}
        </div>
      )}

      {/* Knowledge Timeline */}
      <div className="card p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-medium flex items-center gap-2">
            <Clock className="w-5 h-5 text-primary" />
            Knowledge Timeline
          </h3>

          {/* Filters */}
          <div className="flex items-center gap-2">
            <div className="flex items-center gap-1">
              <Filter className="w-4 h-4 text-muted-foreground" />
              <select
                value={categoryFilter}
                onChange={(e) => setCategoryFilter(e.target.value)}
                className="text-xs bg-muted border-none rounded px-2 py-1"
              >
                {knowledgeCategories.map((cat) => (
                  <option key={cat} value={cat}>
                    {cat === "all"
                      ? "All Categories"
                      : KNOWLEDGE_CATEGORY_CONFIG[cat]?.label || cat}
                  </option>
                ))}
              </select>
            </div>

            <div className="flex items-center gap-1">
              <select
                value={agentFilter}
                onChange={(e) => setAgentFilter(e.target.value)}
                className="text-xs bg-muted border-none rounded px-2 py-1"
              >
                {knowledgeAgents.map((agent) => (
                  <option key={agent} value={agent}>
                    {agent === "all" ? "All Agents" : agent}
                  </option>
                ))}
              </select>
            </div>
          </div>
        </div>

        {knowledgeLoading ? (
          <div className="p-4 flex items-center justify-center gap-2 text-muted-foreground">
            <Loader2 className="w-4 h-4 animate-spin" />
            <span className="text-sm">Loading knowledge entries...</span>
          </div>
        ) : filteredKnowledge.length > 0 ? (
          <div className="space-y-3">
            {iterations.map((iteration) => {
              const entries = knowledgeByIteration[iteration];
              return (
                <div key={iteration}>
                  <div className="text-xs font-medium text-muted-foreground px-1 mb-1.5">
                    Iteration {iteration}
                  </div>
                  <div className="space-y-1.5">
                    {entries.map((entry) => {
                      const config = KNOWLEDGE_CATEGORY_CONFIG[entry.category] || {
                        icon: Info,
                        color: "text-gray-400",
                        label: entry.category,
                      };
                      const Icon = config.icon;
                      const isExpanded = expandedKnowledge.has(entry.id);

                      return (
                        <div key={entry.id} className="rounded-lg border border-border bg-muted/20">
                          <button
                            onClick={() => toggleKnowledge(entry.id)}
                            className="w-full px-3 py-2 flex items-center gap-2 text-left hover:bg-muted/40 transition-colors rounded-lg"
                          >
                            <Icon className={`w-4 h-4 flex-shrink-0 ${config.color}`} />
                            <span
                              className={`text-xs px-1.5 py-0.5 rounded ${CONFIDENCE_COLORS[entry.confidence] || "bg-gray-500/10 text-gray-400"}`}
                            >
                              {entry.confidence}
                            </span>
                            <span className="text-xs text-muted-foreground">
                              {entry.agent_type}
                            </span>
                            <span className="flex-1 text-sm truncate">{entry.content}</span>
                            {entry.is_resolved && (
                              <CheckCircle className="w-3.5 h-3.5 text-green-400 flex-shrink-0" />
                            )}
                            {isExpanded ? (
                              <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                            ) : (
                              <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                            )}
                          </button>

                          {isExpanded && (
                            <div className="px-3 pb-3 space-y-2 border-t border-border/50 mt-1 pt-2">
                              <p className="text-sm text-foreground/80">{entry.content}</p>

                              {entry.evidence && (
                                <div>
                                  <span className="text-xs font-medium text-muted-foreground">
                                    Evidence:
                                  </span>
                                  <p className="text-sm text-foreground/70 mt-0.5">
                                    {entry.evidence}
                                  </p>
                                </div>
                              )}

                              {entry.related_files.length > 0 && (
                                <div>
                                  <span className="text-xs font-medium text-muted-foreground">
                                    Related files:
                                  </span>
                                  <div className="flex flex-wrap gap-1 mt-0.5">
                                    {entry.related_files.map((file, i) => (
                                      <span
                                        key={i}
                                        className="text-xs font-mono bg-muted px-1.5 py-0.5 rounded"
                                      >
                                        {file}
                                      </span>
                                    ))}
                                  </div>
                                </div>
                              )}

                              {entry.is_resolved && entry.resolution_notes && (
                                <div>
                                  <span className="text-xs font-medium text-green-400">
                                    Resolution:
                                  </span>
                                  <p className="text-sm text-green-400/80 mt-0.5">
                                    {entry.resolution_notes}
                                  </p>
                                </div>
                              )}

                              <div className="flex items-center gap-3 text-xs text-muted-foreground">
                                <span>{config.label}</span>
                                <span>{entry.agent_type}</span>
                                {entry.created_at && (
                                  <span>{new Date(entry.created_at).toLocaleString()}</span>
                                )}
                              </div>
                            </div>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="space-y-2">
            <p className="text-sm text-muted-foreground text-center py-4">
              No knowledge entries recorded for this run.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

export default KnowledgeTab;
