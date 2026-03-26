/**
 * SkillApprovalPanel.tsx
 *
 * Review panel for auto-extracted procedural skills (hermes-agent-inspired).
 * Shows pending skills with their provenance (source task run, fix description)
 * and allows users to approve/reject before they are injected into worker prompts.
 */

import React, { useEffect, useState, useCallback } from "react";
import {
  CheckCircle,
  XCircle,
  Clock,
  Zap,
  RefreshCw,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { getApiBase } from "../../lib/runner-api";

// =============================================================================
// Types
// =============================================================================

interface SkillDefinition {
  id: string;
  name: string;
  slug: string;
  description: string;
  category: string;
  tags: string[];
  icon: string;
  color: string;
  allowed_phases: string[];
  source: string;
  version?: string;
  usage_count?: number;
  approval_status?: string;
  created_at?: string;
  source_fix_id?: string;
  source_pattern_id?: string;
}

interface SkillProvenance {
  fix_type?: string;
  fix_description?: string;
  source_task_run_id?: string;
  effectiveness?: string;
}

// =============================================================================
// Component
// =============================================================================

export function SkillApprovalPanel() {
  const [skills, setSkills] = useState<SkillDefinition[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [filter, setFilter] = useState<"pending" | "approved" | "rejected" | "all">("pending");
  const [provenance, setProvenance] = useState<Record<string, SkillProvenance>>({});

  const fetchSkills = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetch(`${getApiBase()}/skills`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      const all: SkillDefinition[] = json.data || [];
      // Only show auto-extracted skills in this panel
      const autoSkills = all.filter((s) => s.source === "auto");
      setSkills(autoSkills);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch skills");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchSkills();
  }, [fetchSkills]);

  const fetchProvenance = async (skill: SkillDefinition) => {
    if (!skill.source_fix_id || provenance[skill.id]) return;
    try {
      // Parse source_task_run_id from the description (format: "task_run: <id>")
      const trMatch = skill.description.match(/task_run:\s*([^\s,)]+)/);
      const sourceTaskRunId = trMatch?.[1];

      // Parse fix details from description
      const fixTypeMatch = skill.description.match(/Fix type:\s*([^\s.]+)/);
      const fixType = fixTypeMatch?.[1];

      setProvenance((prev) => ({
        ...prev,
        [skill.id]: {
          fix_type: fixType || skill.category,
          fix_description: skill.description.split(".")[0],
          source_task_run_id: sourceTaskRunId,
          effectiveness: "effective",
        },
      }));
    } catch {
      // Provenance is best-effort
    }
  };

  const handleExpand = (skill: SkillDefinition) => {
    const newId = expandedId === skill.id ? null : skill.id;
    setExpandedId(newId);
    if (newId) fetchProvenance(skill);
  };

  const updateSkillStatus = async (id: string, status: string) => {
    try {
      const res = await fetch(`${getApiBase()}/skills/${id}/approve`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ status }),
      });
      if (!res.ok) {
        console.error(`Failed to update skill ${id} to ${status}: HTTP ${res.status}`);
        return;
      }
      setSkills((prev) =>
        prev.map((s) =>
          s.id === id ? { ...s, approval_status: status } : s
        )
      );
    } catch (e) {
      console.error(`Failed to update skill ${id} to ${status}:`, e);
    }
  };

  const handleApprove = (id: string) => updateSkillStatus(id, "approved");
  const handleReject = (id: string) => updateSkillStatus(id, "rejected");
  const handleReset = (id: string) => updateSkillStatus(id, "pending");

  const filtered = skills.filter((s) => {
    if (filter === "all") return true;
    return (s.approval_status || "pending") === filter;
  });

  const counts = {
    pending: skills.filter((s) => !s.approval_status || s.approval_status === "pending").length,
    approved: skills.filter((s) => s.approval_status === "approved").length,
    rejected: skills.filter((s) => s.approval_status === "rejected").length,
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border">
        <div className="flex items-center gap-2">
          <Zap className="w-4 h-4 text-amber-500" />
          <h2 className="text-sm font-semibold">Procedural Skills</h2>
          <span className="text-xs text-muted-foreground">
            Auto-extracted from successful runs
          </span>
        </div>
        <button
          onClick={fetchSkills}
          className="p-1 rounded hover:bg-accent"
          title="Refresh"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Filter tabs */}
      <div className="flex gap-1 px-4 py-2 border-b border-border">
        {(["pending", "approved", "rejected", "all"] as const).map((f) => (
          <button
            key={f}
            onClick={() => setFilter(f)}
            className={`px-2.5 py-1 text-xs rounded-md transition-colors ${
              filter === f
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:bg-accent"
            }`}
          >
            {f === "all" ? "All" : f.charAt(0).toUpperCase() + f.slice(1)}
            {f !== "all" && (
              <span className="ml-1 opacity-70">
                ({counts[f]})
              </span>
            )}
          </button>
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto">
        {error && (
          <div className="px-4 py-3 text-sm text-destructive">{error}</div>
        )}

        {!loading && filtered.length === 0 && (
          <div className="px-4 py-8 text-center text-sm text-muted-foreground">
            {filter === "pending"
              ? "No pending skills to review. Skills are auto-extracted when workflows succeed after complex fixes."
              : `No ${filter} skills found.`}
          </div>
        )}

        {filtered.map((skill) => {
          const isExpanded = expandedId === skill.id;
          const status = skill.approval_status || "pending";

          return (
            <div
              key={skill.id}
              className="border-b border-border last:border-0"
            >
              {/* Skill row */}
              <div className="flex items-center gap-2 px-4 py-2.5 hover:bg-accent/50">
                <button
                  onClick={() => handleExpand(skill)}
                  className="p-0.5"
                >
                  {isExpanded ? (
                    <ChevronDown className="w-3.5 h-3.5" />
                  ) : (
                    <ChevronRight className="w-3.5 h-3.5" />
                  )}
                </button>

                {/* Status icon */}
                {status === "approved" && (
                  <CheckCircle className="w-3.5 h-3.5 text-green-500 shrink-0" />
                )}
                {status === "rejected" && (
                  <XCircle className="w-3.5 h-3.5 text-red-500 shrink-0" />
                )}
                {status === "pending" && (
                  <Clock className="w-3.5 h-3.5 text-amber-500 shrink-0" />
                )}

                {/* Skill info */}
                <div className="flex-1 min-w-0">
                  <div className="text-xs font-medium truncate">
                    {skill.name}
                  </div>
                  <div className="text-[10px] text-muted-foreground truncate">
                    {skill.category} &middot; v{skill.version || "1.0.0"} &middot; used{" "}
                    {skill.usage_count || 0}x
                  </div>
                </div>

                {/* Action buttons (only for pending) */}
                {status === "pending" && (
                  <div className="flex gap-1 shrink-0">
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleApprove(skill.id);
                      }}
                      className="px-2 py-0.5 text-[10px] rounded bg-green-500/10 text-green-600 hover:bg-green-500/20"
                    >
                      Approve
                    </button>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleReject(skill.id);
                      }}
                      className="px-2 py-0.5 text-[10px] rounded bg-red-500/10 text-red-600 hover:bg-red-500/20"
                    >
                      Reject
                    </button>
                  </div>
                )}
              </div>

              {/* Expanded detail */}
              {isExpanded && (
                <div className="px-4 pb-3 pl-10 text-xs space-y-2">
                  <div>
                    <span className="font-medium text-muted-foreground">
                      Description:
                    </span>{" "}
                    {skill.description}
                  </div>
                  <div>
                    <span className="font-medium text-muted-foreground">
                      Tags:
                    </span>{" "}
                    {skill.tags?.join(", ") || "none"}
                  </div>
                  <div>
                    <span className="font-medium text-muted-foreground">
                      Phases:
                    </span>{" "}
                    {skill.allowed_phases?.join(", ")}
                  </div>
                  <div>
                    <span className="font-medium text-muted-foreground">
                      Created:
                    </span>{" "}
                    {skill.created_at
                      ? new Date(skill.created_at).toLocaleString()
                      : "unknown"}
                  </div>

                  {/* Provenance: source fix and task run */}
                  {provenance[skill.id] && (
                    <div className="mt-2 p-2 rounded bg-muted/50 space-y-1">
                      <div className="font-medium text-muted-foreground">Provenance</div>
                      {provenance[skill.id].fix_type && (
                        <div>
                          <span className="text-muted-foreground">Fix type:</span>{" "}
                          <span className="font-mono">{provenance[skill.id].fix_type}</span>
                        </div>
                      )}
                      {provenance[skill.id].source_task_run_id && (
                        <div>
                          <span className="text-muted-foreground">Source run:</span>{" "}
                          <span className="font-mono text-[10px]">
                            {provenance[skill.id].source_task_run_id}
                          </span>
                        </div>
                      )}
                      {skill.source_fix_id && (
                        <div>
                          <span className="text-muted-foreground">Fix ID:</span>{" "}
                          <span className="font-mono text-[10px]">{skill.source_fix_id}</span>
                        </div>
                      )}
                      {provenance[skill.id].effectiveness && (
                        <div>
                          <span className="text-muted-foreground">Effectiveness:</span>{" "}
                          <span className="text-green-600">{provenance[skill.id].effectiveness}</span>
                        </div>
                      )}
                    </div>
                  )}

                  {/* Show approve/reject for non-pending too (re-review) */}
                  {status !== "pending" && (
                    <div className="flex gap-2 pt-1">
                      {status !== "approved" && (
                        <button
                          onClick={() => handleApprove(skill.id)}
                          className="px-2 py-0.5 text-[10px] rounded bg-green-500/10 text-green-600 hover:bg-green-500/20"
                        >
                          Approve
                        </button>
                      )}
                      {status !== "rejected" && (
                        <button
                          onClick={() => handleReject(skill.id)}
                          className="px-2 py-0.5 text-[10px] rounded bg-red-500/10 text-red-600 hover:bg-red-500/20"
                        >
                          Reject
                        </button>
                      )}
                      <button
                        onClick={() => handleReset(skill.id)}
                        className="px-2 py-0.5 text-[10px] rounded bg-amber-500/10 text-amber-600 hover:bg-amber-500/20"
                      >
                        Reset to Pending
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
