/**
 * RunDetailsPanel.tsx
 *
 * Detailed view of a specific run showing:
 * - Run metadata (status, timing, workflow)
 * - Actions summary
 * - States visited
 * - Transitions executed
 * - Template matches
 * - Anomalies detected
 * - Error details if failed
 */

import { useState } from "react";
import {
  X,
  Clock,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Activity,
  ArrowRight,
  Image,
  AlertCircle,
} from "lucide-react";
import { getStatusColors, getAccentColors, getSeverityColors } from "@/design-system";
import type {
  RunDetails,
  RunStatus,
  TransitionRecord,
  TemplateMatchRecord,
  Anomaly,
  AnomalySeverity,
} from "../../types/statistics";

interface RunDetailsPanelProps {
  run: RunDetails;
  onClose: () => void;
}

export function RunDetailsPanel({ run, onClose }: RunDetailsPanelProps) {
  const [expandedSections, setExpandedSections] = useState<Set<string>>(new Set(["overview"]));

  const toggleSection = (section: string) => {
    setExpandedSections((prev) => {
      const next = new Set(prev);
      if (next.has(section)) {
        next.delete(section);
      } else {
        next.add(section);
      }
      return next;
    });
  };

  return (
    <div className="h-full flex flex-col bg-background">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <div className="flex items-center gap-3">
          <StatusIcon status={run.status} />
          <div>
            <h2 className="font-semibold">{run.workflow_name || "Unnamed Run"}</h2>
            <p className="text-sm text-muted-foreground font-mono">{run.id.slice(0, 8)}</p>
          </div>
        </div>
        <button onClick={onClose} className="p-2 rounded-md hover:bg-muted transition-colors">
          <X className="w-5 h-5" />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Overview Section */}
        <CollapsibleSection
          title="Overview"
          icon={<Activity className="w-4 h-4" />}
          isOpen={expandedSections.has("overview")}
          onToggle={() => toggleSection("overview")}
        >
          <div className="grid grid-cols-2 gap-4">
            <InfoCard label="Status" value={<StatusBadge status={run.status} />} />
            <InfoCard
              label="Duration"
              value={run.duration_ms ? `${(run.duration_ms / 1000).toFixed(2)}s` : "N/A"}
            />
            <InfoCard label="Started" value={new Date(run.started_at).toLocaleString()} />
            <InfoCard
              label="Ended"
              value={run.ended_at ? new Date(run.ended_at).toLocaleString() : "In Progress"}
            />
          </div>

          {/* Actions Summary */}
          {run.actions_summary && (
            <div className="mt-4">
              <h4 className="text-sm font-medium mb-2">Actions</h4>
              <div className="grid grid-cols-4 gap-2">
                <ActionStat
                  label="Total"
                  value={run.actions_summary.total}
                  color="text-foreground"
                />
                <ActionStat
                  label="Success"
                  value={run.actions_summary.success}
                  color={getStatusColors("success").text}
                />
                <ActionStat
                  label="Failed"
                  value={run.actions_summary.failed}
                  color={getStatusColors("error").text}
                />
                <ActionStat
                  label="Skipped"
                  value={run.actions_summary.skipped}
                  color="text-muted-foreground"
                />
              </div>
            </div>
          )}
        </CollapsibleSection>

        {/* Error Details */}
        {run.error_message && (
          <CollapsibleSection
            title="Error Details"
            icon={<XCircle className="w-4 h-4 text-destructive" />}
            isOpen={expandedSections.has("error")}
            onToggle={() => toggleSection("error")}
            badge={<span className="text-xs text-destructive">{run.error_type}</span>}
          >
            <div className="p-3 rounded bg-destructive/10 border border-destructive/20 text-sm">
              <code className="text-destructive whitespace-pre-wrap break-all">
                {run.error_message}
              </code>
            </div>
          </CollapsibleSection>
        )}

        {/* States Visited */}
        {run.states_visited.length > 0 && (
          <CollapsibleSection
            title="States Visited"
            icon={<Activity className="w-4 h-4" />}
            isOpen={expandedSections.has("states")}
            onToggle={() => toggleSection("states")}
            badge={<span className="text-xs">{run.states_visited.length}</span>}
          >
            <div className="flex flex-wrap gap-2">
              {run.states_visited.map((state, idx) => (
                <span
                  key={`${state}-${idx}`}
                  className="px-2 py-1 text-xs rounded bg-primary/10 text-primary"
                >
                  {state}
                </span>
              ))}
            </div>
          </CollapsibleSection>
        )}

        {/* Transitions Executed */}
        {run.transitions_executed.length > 0 && (
          <CollapsibleSection
            title="Transitions"
            icon={<ArrowRight className="w-4 h-4" />}
            isOpen={expandedSections.has("transitions")}
            onToggle={() => toggleSection("transitions")}
            badge={<span className="text-xs">{run.transitions_executed.length}</span>}
          >
            <div className="space-y-2">
              {run.transitions_executed.map((transition, idx) => (
                <TransitionItem
                  key={`${transition.from_state}-${transition.to_state}-${idx}`}
                  transition={transition}
                />
              ))}
            </div>
          </CollapsibleSection>
        )}

        {/* Template Matches */}
        {run.template_matches.length > 0 && (
          <CollapsibleSection
            title="Template Matches"
            icon={<Image className="w-4 h-4" />}
            isOpen={expandedSections.has("templates")}
            onToggle={() => toggleSection("templates")}
            badge={<span className="text-xs">{run.template_matches.length}</span>}
          >
            <div className="space-y-2">
              {run.template_matches.map((match, idx) => (
                <TemplateMatchItem key={`${match.template}-${idx}`} match={match} />
              ))}
            </div>
          </CollapsibleSection>
        )}

        {/* Anomalies */}
        {run.anomalies.length > 0 && (
          <CollapsibleSection
            title="Anomalies"
            icon={<AlertCircle className={`w-4 h-4 ${getStatusColors("warning").icon}`} />}
            isOpen={expandedSections.has("anomalies")}
            onToggle={() => toggleSection("anomalies")}
            badge={
              <span className={`text-xs ${getStatusColors("warning").text}`}>
                {run.anomalies.length}
              </span>
            }
          >
            <div className="space-y-2">
              {run.anomalies.map((anomaly, idx) => (
                <AnomalyItem key={`${anomaly.anomaly_type}-${idx}`} anomaly={anomaly} />
              ))}
            </div>
          </CollapsibleSection>
        )}
      </div>
    </div>
  );
}

// Helper Components

function StatusIcon({ status }: { status: RunStatus }) {
  switch (status) {
    case "completed":
      return <CheckCircle2 className={`w-6 h-6 ${getStatusColors("success").icon}`} />;
    case "failed":
      return <XCircle className={`w-6 h-6 ${getStatusColors("error").icon}`} />;
    case "timeout":
      return <Clock className={`w-6 h-6 ${getAccentColors("orange").text}`} />;
    case "cancelled":
      return <XCircle className="w-6 h-6 text-muted-foreground" />;
    case "running":
      return <Activity className={`w-6 h-6 ${getStatusColors("running").icon} animate-pulse`} />;
    default:
      return <AlertTriangle className="w-6 h-6 text-muted-foreground" />;
  }
}

function StatusBadge({ status }: { status: RunStatus }) {
  const statusMap: Record<RunStatus, string> = {
    completed: "success",
    failed: "error",
    timeout: "warning",
    cancelled: "cancelled",
    running: "running",
  };
  const colors = getStatusColors(statusMap[status] || "idle");

  return (
    <span className={`px-2 py-0.5 rounded text-xs font-medium ${colors.bg} ${colors.text}`}>
      {status}
    </span>
  );
}

function InfoCard({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="p-3 rounded bg-muted/50">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="font-medium mt-1">{value}</p>
    </div>
  );
}

function ActionStat({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="text-center p-2 rounded bg-muted/30">
      <p className={`text-lg font-semibold ${color}`}>{value}</p>
      <p className="text-xs text-muted-foreground">{label}</p>
    </div>
  );
}

interface CollapsibleSectionProps {
  title: string;
  icon: React.ReactNode;
  isOpen: boolean;
  onToggle: () => void;
  badge?: React.ReactNode;
  children: React.ReactNode;
}

function CollapsibleSection({
  title,
  icon,
  isOpen,
  onToggle,
  badge,
  children,
}: CollapsibleSectionProps) {
  return (
    <div className="rounded-lg border border-border bg-card">
      <button
        onClick={onToggle}
        className="w-full flex items-center justify-between p-3 hover:bg-muted/30 transition-colors"
      >
        <div className="flex items-center gap-2">
          {isOpen ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
          {icon}
          <span className="font-medium">{title}</span>
        </div>
        {badge}
      </button>
      {isOpen && <div className="p-3 pt-0 border-t border-border">{children}</div>}
    </div>
  );
}

function TransitionItem({ transition }: { transition: TransitionRecord }) {
  return (
    <div className="flex items-center gap-2 p-2 rounded bg-muted/30 text-sm">
      <span className="font-medium">{transition.from_state}</span>
      <ArrowRight className="w-3 h-3 text-muted-foreground" />
      <span className="font-medium">{transition.to_state}</span>
      <span className="text-muted-foreground">via {transition.action}</span>
      <span className="ml-auto text-xs text-muted-foreground">{transition.duration_ms}ms</span>
      {transition.success ? (
        <CheckCircle2 className={`w-4 h-4 ${getStatusColors("success").icon}`} />
      ) : (
        <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />
      )}
      {transition.error && (
        <span className="text-xs text-destructive" title={transition.error}>
          !
        </span>
      )}
    </div>
  );
}

function TemplateMatchItem({ match }: { match: TemplateMatchRecord }) {
  const successRate = ((match.count - match.failures) / match.count) * 100;

  return (
    <div className="flex items-center gap-3 p-2 rounded bg-muted/30 text-sm">
      <Image className="w-4 h-4 text-muted-foreground" />
      <span className="font-medium flex-1 truncate">{match.template}</span>
      <div className="flex items-center gap-4 text-xs text-muted-foreground">
        <span>{match.count} matches</span>
        <span>{(match.avg_confidence * 100).toFixed(0)}% confidence</span>
        <span
          className={
            successRate < 80 ? getStatusColors("warning").text : getStatusColors("success").text
          }
        >
          {successRate.toFixed(0)}% success
        </span>
      </div>
    </div>
  );
}

function AnomalyItem({ anomaly }: { anomaly: Anomaly }) {
  const getSeverityStyle = (severity: AnomalySeverity): string => {
    const severityMap: Record<AnomalySeverity, string> = {
      low: "low",
      medium: "medium",
      high: "high",
    };
    const colors = getSeverityColors(severityMap[severity]);
    return `${colors.bg} ${colors.text} ${colors.border}`;
  };

  return (
    <div className={`p-3 rounded border ${getSeverityStyle(anomaly.severity)} text-sm`}>
      <div className="flex items-center justify-between mb-1">
        <span className="font-medium capitalize">{anomaly.anomaly_type.replace(/_/g, " ")}</span>
        <span className="text-xs uppercase">{anomaly.severity}</span>
      </div>
      <p className="text-xs opacity-80">{anomaly.description}</p>
      {(anomaly.expected || anomaly.actual) && (
        <div className="mt-2 text-xs grid grid-cols-2 gap-2">
          {anomaly.expected && (
            <div>
              <span className="opacity-60">Expected: </span>
              <span>{anomaly.expected}</span>
            </div>
          )}
          {anomaly.actual && (
            <div>
              <span className="opacity-60">Actual: </span>
              <span>{anomaly.actual}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
