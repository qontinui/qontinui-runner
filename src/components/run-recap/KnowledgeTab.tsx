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
} from "lucide-react";
import type { Finding, FindingSeverity } from "@/types/findings";

interface KnowledgeTabProps {
  taskRunId: string;
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

// Severity colors
const SEVERITY_COLORS: Record<FindingSeverity, { bg: string; text: string; border: string }> = {
  critical: { bg: "bg-red-500/10", text: "text-red-500", border: "border-red-500/30" },
  high: { bg: "bg-orange-500/10", text: "text-orange-500", border: "border-orange-500/30" },
  medium: { bg: "bg-amber-500/10", text: "text-amber-500", border: "border-amber-500/30" },
  low: { bg: "bg-blue-500/10", text: "text-blue-500", border: "border-blue-500/30" },
  info: { bg: "bg-gray-500/10", text: "text-gray-400", border: "border-gray-500/30" },
};

interface KnowledgeEntry {
  id: string;
  category: string;
  content: string;
  iteration: number;
  timestamp: number;
  agent?: string;
}

export function KnowledgeTab({ taskRunId }: KnowledgeTabProps) {
  const [findings, setFindings] = useState<Finding[]>([]);
  const [categoryFilter, setCategoryFilter] = useState<string>("all");
  const [agentFilter, setAgentFilter] = useState<string>("all");
  const [expandedFindings, setExpandedFindings] = useState<Set<string>>(new Set());

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
    {} as Record<string, Finding[]>
  );

  // Categories for filter
  const categories = ["all", ...new Set(findings.map((f) => f.categoryId))];
  const agents = ["all", "planning", "worker", "verification", "system"];

  return (
    <div className="space-y-4">
      {/* Findings Panel */}
      <div data-ui-id="recap-findings-panel" className="card p-4">
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
            <div
              key={severity}
              data-ui-id={`recap-findings-${severity}`}
              className={`mb-3 rounded-lg border ${colors.border} ${colors.bg}`}
            >
              <div className={`px-3 py-2 font-medium ${colors.text} capitalize`}>
                {severity} ({severityFindings.length})
              </div>
              <div className="divide-y divide-border/50">
                {severityFindings.map((finding) => {
                  const Icon = CATEGORY_ICONS[finding.categoryId] || AlertTriangle;
                  const isExpanded = expandedFindings.has(finding.id);

                  return (
                    <div
                      key={finding.id}
                      data-ui-id={`recap-finding-${finding.id}`}
                      className="px-3 py-2"
                    >
                      <button
                        onClick={() => toggleFinding(finding.id)}
                        className="w-full flex items-center gap-2 text-left"
                      >
                        <Icon className={`w-4 h-4 ${colors.text}`} />
                        <span
                          data-ui-id={`recap-finding-${finding.id}-category`}
                          className={`text-xs px-1.5 py-0.5 rounded ${colors.bg} ${colors.text}`}
                        >
                          {finding.categoryId}
                        </span>
                        <span className="flex-1 text-sm font-medium truncate">
                          {finding.title}
                        </span>
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
                            <p className="mt-1 text-green-400">
                              Resolution: {finding.resolution}
                            </p>
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
          <div
            data-ui-id="recap-finding-placeholder"
            className="px-3 py-4 text-center"
          >
            <p className="text-sm text-muted-foreground">
              No findings recorded for this run.
            </p>
          </div>
        )}
      </div>

      {/* Knowledge Timeline */}
      <div data-ui-id="recap-knowledge-timeline" className="card p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-medium flex items-center gap-2">
            <Clock className="w-5 h-5 text-primary" />
            Knowledge Timeline
          </h3>

          {/* Filters */}
          <div className="flex items-center gap-2">
            <div data-ui-id="recap-knowledge-filter-category" className="flex items-center gap-1">
              <Filter className="w-4 h-4 text-muted-foreground" />
              <select
                value={categoryFilter}
                onChange={(e) => setCategoryFilter(e.target.value)}
                className="text-xs bg-muted border-none rounded px-2 py-1"
              >
                {categories.map((cat) => (
                  <option key={cat} value={cat}>
                    {cat === "all" ? "All Categories" : cat}
                  </option>
                ))}
              </select>
            </div>

            <div data-ui-id="recap-knowledge-filter-agent" className="flex items-center gap-1">
              <select
                value={agentFilter}
                onChange={(e) => setAgentFilter(e.target.value)}
                className="text-xs bg-muted border-none rounded px-2 py-1"
              >
                {agents.map((agent) => (
                  <option key={agent} value={agent}>
                    {agent === "all" ? "All Agents" : agent}
                  </option>
                ))}
              </select>
            </div>
          </div>
        </div>

        {/* Knowledge entries would be displayed here */}
        <div className="space-y-2">
          <p
            data-ui-id="recap-knowledge-entry-placeholder"
            className="text-sm text-muted-foreground text-center py-4"
          >
            No knowledge entries recorded for this run.
          </p>
        </div>
      </div>
    </div>
  );
}

export default KnowledgeTab;
