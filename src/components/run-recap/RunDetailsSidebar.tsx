/**
 * RunDetailsSidebar
 *
 * Compact sidebar card showing run metadata, token usage, timestamps,
 * and the "Continue Run" action. Replaces the former full-width
 * UsageSection, timestamps row, and Continue Run card.
 */

import { useState, useEffect } from "react";
import { Calendar, Play, Loader2, Target, ChevronDown, ChevronRight, Cpu, Zap } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { getAccentColors, getStatusColors } from "@/design-system";

interface PhaseUsage {
  phase: string;
  stage_index: number | null;
  iteration: number | null;
  model_used: string | null;
  provider_used: string | null;
  input_tokens: number;
  output_tokens: number;
  cost_cents: number;
  duration_ms: number | null;
}

interface UsageData {
  task_run_id: string;
  phases: PhaseUsage[];
  totals: {
    input_tokens: number;
    output_tokens: number;
    cost_cents: number;
  };
}

interface RunDetailsSidebarProps {
  taskRunId: string;
  startTime?: string;
  endTime?: string;
  isFinished: boolean;
  goalAchieved?: boolean | null;
  onContinueRun: () => void;
  isContinuePending: boolean;
  continueError?: string | null;
  continueSuccess?: boolean;
  additionalSessions: number;
  onAdditionalSessionsChange: (value: number) => void;
}

const formatTokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));

const formatTime = (isoString?: string) => {
  if (!isoString) return null;
  try {
    const d = new Date(isoString);
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  } catch {
    return null;
  }
};

export function RunDetailsSidebar({
  taskRunId,
  startTime,
  endTime,
  isFinished,
  goalAchieved,
  onContinueRun,
  isContinuePending,
  continueError,
  continueSuccess,
  additionalSessions,
  onAdditionalSessionsChange,
}: RunDetailsSidebarProps) {
  const [usage, setUsage] = useState<UsageData | null>(null);
  const [phaseExpanded, setPhaseExpanded] = useState(false);

  useEffect(() => {
    tracedFetch(`${getApiBase()}/task-runs/${taskRunId}/usage`)
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then(setUsage)
      .catch(() => {});
  }, [taskRunId]);

  const formattedStart = formatTime(startTime);
  const formattedEnd = formatTime(endTime);
  const primaryModel = usage?.phases?.[0]?.model_used ?? null;

  return (
    <div className="rounded-lg border border-border bg-card space-y-0 divide-y divide-border">
      {/* Key-value metrics */}
      <div className="p-3 space-y-2">
        <h3 className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
          Run Details
        </h3>
        <dl className="space-y-1.5 text-sm">
          {primaryModel && (
            <DetailRow icon={<Cpu className="w-3.5 h-3.5" />} label="Model" value={primaryModel} />
          )}
          {usage && (
            <>
              <DetailRow
                icon={<Zap className="w-3.5 h-3.5" />}
                label="Input"
                value={`${formatTokens(usage.totals.input_tokens)} tokens`}
              />
              <DetailRow
                label="Output"
                value={`${formatTokens(usage.totals.output_tokens)} tokens`}
              />
            </>
          )}
          {formattedStart && (
            <DetailRow
              icon={<Calendar className="w-3.5 h-3.5" />}
              label="Started"
              value={formattedStart}
            />
          )}
          {formattedEnd && <DetailRow label="Ended" value={formattedEnd} />}
        </dl>
      </div>

      {/* Collapsible phase breakdown */}
      {usage && usage.phases.length > 1 && (
        <div className="p-3">
          <button
            onClick={() => setPhaseExpanded(!phaseExpanded)}
            className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors w-full"
          >
            {phaseExpanded ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )}
            <span>Token breakdown by phase</span>
          </button>
          {phaseExpanded && (
            <div className="mt-2 space-y-1">
              {usage.phases.map((p, i) => (
                <div
                  key={`${p.phase}-${p.stage_index ?? ""}-${p.iteration ?? ""}-${i}`}
                  className="flex items-center justify-between text-[11px] text-muted-foreground"
                >
                  <span className="capitalize truncate flex-1 min-w-0">
                    {p.phase}
                    {p.stage_index != null && (
                      <span className="text-muted-foreground/60 ml-1">S{p.stage_index + 1}</span>
                    )}
                    {p.iteration != null && (
                      <span className="text-muted-foreground/60 ml-1">#{p.iteration}</span>
                    )}
                  </span>
                  <span className="tabular-nums ml-2 whitespace-nowrap">
                    {formatTokens(p.input_tokens)} / {formatTokens(p.output_tokens)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Continue Run */}
      {isFinished && (
        <div className="p-3 space-y-2">
          <div className="flex items-center gap-2">
            <Play className="w-3.5 h-3.5 text-muted-foreground" />
            <span className="text-xs font-medium">Continue Run</span>
            {goalAchieved === false && (
              <span
                className={`flex items-center gap-1 px-1.5 py-0.5 text-[10px] rounded ${getAccentColors("amber").bg} ${getAccentColors("amber").text}`}
              >
                <Target className="w-2.5 h-2.5" />
                Goal Not Met
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <input
              type="number"
              min={1}
              max={20}
              value={additionalSessions}
              onChange={(e) =>
                onAdditionalSessionsChange(Math.max(1, Math.min(20, parseInt(e.target.value) || 1)))
              }
              className="w-12 px-1.5 py-1 text-xs border border-border rounded bg-background text-foreground text-center"
              title="Additional sessions"
            />
            <button
              onClick={onContinueRun}
              disabled={isContinuePending}
              className="flex-1 flex items-center justify-center gap-1.5 px-3 py-1.5 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isContinuePending ? (
                <>
                  <Loader2 className="w-3 h-3 animate-spin" />
                  <span>Reopening...</span>
                </>
              ) : (
                <>
                  <Play className="w-3 h-3" />
                  <span>Continue</span>
                </>
              )}
            </button>
          </div>
          {continueError && (
            <p className={`text-[11px] ${getStatusColors("error").text}`}>{continueError}</p>
          )}
          {continueSuccess && (
            <p className={`text-[11px] ${getStatusColors("success").text}`}>
              Reopened! Continuing automatically.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function DetailRow({
  icon,
  label,
  value,
}: {
  icon?: React.ReactNode;
  label: string;
  value: string;
}) {
  return (
    <div className="flex items-center justify-between gap-2">
      <dt className="flex items-center gap-1.5 text-muted-foreground min-w-0">
        {icon && <span className="shrink-0">{icon}</span>}
        <span className="text-xs">{label}</span>
      </dt>
      <dd className="text-xs text-foreground truncate max-w-[160px] text-right" title={value}>
        {value}
      </dd>
    </div>
  );
}
