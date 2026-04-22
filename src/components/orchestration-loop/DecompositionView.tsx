import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ListTree,
  ChevronDown,
  ChevronRight,
  Loader2,
  Check,
  X,
  Clock,
  SkipForward,
  ArrowRight,
} from "lucide-react";
import { cn } from "../../lib/utils";

// --- Types matching backend decomposition ---

type SubtaskStatus = "pending" | "running" | "completed" | "failed" | "skipped";

interface Subtask {
  index: number;
  title: string;
  description: string | null;
  status: SubtaskStatus;
  dependencies: number[];
  error: string | null;
  started_at: string | null;
  completed_at: string | null;
}

interface DecompositionPlan {
  task_id: string;
  total_subtasks: number;
  subtasks: Subtask[];
  created_at: string;
}

// --- Status styling ---

const STATUS_CONFIG: Record<
  SubtaskStatus,
  { color: string; bg: string; border: string; icon: React.ReactNode }
> = {
  pending: {
    color: "text-muted-foreground",
    bg: "bg-muted",
    border: "border-muted-foreground/30",
    icon: <Clock className="w-3 h-3" />,
  },
  running: {
    color: "text-blue-400",
    bg: "bg-blue-500/10",
    border: "border-blue-500",
    icon: <Loader2 className="w-3 h-3 animate-spin" />,
  },
  completed: {
    color: "text-green-400",
    bg: "bg-green-500/10",
    border: "border-green-500",
    icon: <Check className="w-3 h-3" />,
  },
  failed: {
    color: "text-red-400",
    bg: "bg-red-500/10",
    border: "border-red-500",
    icon: <X className="w-3 h-3" />,
  },
  skipped: {
    color: "text-yellow-400",
    bg: "bg-yellow-500/10",
    border: "border-yellow-500",
    icon: <SkipForward className="w-3 h-3" />,
  },
};

interface SubtaskRowProps {
  subtask: Subtask;
  isLast: boolean;
}

function SubtaskRow({ subtask, isLast }: SubtaskRowProps) {
  const [expanded, setExpanded] = useState(false);
  const config = STATUS_CONFIG[subtask.status];
  const hasDetails = subtask.description || subtask.error;

  const duration =
    subtask.started_at && subtask.completed_at
      ? (
          (new Date(subtask.completed_at).getTime() - new Date(subtask.started_at).getTime()) /
          1000
        ).toFixed(1) + "s"
      : null;

  return (
    <div className="flex gap-2">
      {/* Vertical timeline line + dot */}
      <div className="flex flex-col items-center shrink-0 w-5">
        <div
          className={cn(
            "w-2.5 h-2.5 rounded-full border-2 shrink-0 mt-1",
            config.border,
            subtask.status === "running" ? "bg-blue-500" : config.bg,
          )}
        />
        {!isLast && <div className="w-px flex-1 bg-border/50 my-0.5" />}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 pb-3">
        <div className="flex items-center gap-2">
          {hasDetails ? (
            <button
              onClick={() => setExpanded(!expanded)}
              className="shrink-0 text-muted-foreground hover:text-foreground"
            >
              {expanded ? (
                <ChevronDown className="w-3 h-3" />
              ) : (
                <ChevronRight className="w-3 h-3" />
              )}
            </button>
          ) : (
            <span className="w-3 shrink-0" />
          )}

          <span className={cn("shrink-0", config.color)}>{config.icon}</span>

          <span className="text-xs font-mono text-muted-foreground shrink-0">
            #{subtask.index + 1}
          </span>

          <span className="text-xs font-medium truncate">{subtask.title}</span>

          {duration && (
            <span className="text-[0.65rem] text-muted-foreground font-mono shrink-0">
              {duration}
            </span>
          )}

          {/* Status badge */}
          <span
            className={cn(
              "inline-flex px-1.5 py-0.5 rounded-full text-[0.6rem] font-semibold font-mono shrink-0 ml-auto",
              config.color,
              config.bg,
            )}
          >
            {subtask.status}
          </span>
        </div>

        {/* Dependencies */}
        {subtask.dependencies.length > 0 && (
          <div className="flex items-center gap-1 mt-1 ml-6">
            <ArrowRight className="w-2.5 h-2.5 text-muted-foreground/50" />
            <span className="text-[0.65rem] text-muted-foreground/70">
              depends on: {subtask.dependencies.map((d) => `#${d + 1}`).join(", ")}
            </span>
          </div>
        )}

        {/* Expanded details */}
        {expanded && hasDetails && (
          <div className="mt-1.5 ml-6 space-y-1">
            {subtask.description && (
              <p className="text-[0.7rem] text-muted-foreground leading-snug">
                {subtask.description}
              </p>
            )}
            {subtask.error && (
              <div className="border-l-2 border-red-500 bg-red-500/10 px-2 py-1 text-[0.7rem] text-red-400 rounded-r">
                {subtask.error}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

interface DecompositionViewProps {
  /** Whether the orchestration loop is running */
  running: boolean;
}

export function DecompositionView({ running }: DecompositionViewProps) {
  const [plan, setPlan] = useState<DecompositionPlan | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(false);

  const fetchPlan = useCallback(async () => {
    try {
      const result = await invoke<DecompositionPlan | null>("get_decomposition_plan");
      setPlan(result);
      setError(null);
    } catch (e) {
      // Command may not exist yet — only set error if we had a plan before
      if (plan) {
        setError(String(e));
      }
    } finally {
      setLoading(false);
    }
  }, [plan]);

  useEffect(() => {
    if (!running) {
      return;
    }

    setLoading(true);
    fetchPlan();
    const interval = setInterval(fetchPlan, 3000);
    return () => clearInterval(interval);
  }, [running]); // eslint-disable-line react-hooks/exhaustive-deps

  // Nothing to show
  if (!plan || !running) {
    return null;
  }

  const completedCount = plan.subtasks.filter((s) => s.status === "completed").length;
  const failedCount = plan.subtasks.filter((s) => s.status === "failed").length;

  return (
    <div className="border border-border rounded">
      {/* Header */}
      <button
        onClick={() => setCollapsed(!collapsed)}
        className="w-full px-3 py-1.5 border-b border-border flex items-center justify-between hover:bg-muted/30 transition-colors"
      >
        <div className="flex items-center gap-2">
          <ListTree className="w-3.5 h-3.5 text-primary" />
          <span className="text-xs font-semibold">Decomposition Plan</span>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-[0.7rem] text-muted-foreground font-mono">
            {completedCount}/{plan.total_subtasks}
            {failedCount > 0 && <span className="text-red-400 ml-1">({failedCount} failed)</span>}
          </span>
          {collapsed ? (
            <ChevronRight className="w-3.5 h-3.5 text-muted-foreground" />
          ) : (
            <ChevronDown className="w-3.5 h-3.5 text-muted-foreground" />
          )}
        </div>
      </button>

      {/* Subtask list */}
      {!collapsed && (
        <div className="p-3">
          {loading && !plan.subtasks.length ? (
            <div className="flex items-center justify-center py-4 text-muted-foreground text-xs">
              <Loader2 className="w-3.5 h-3.5 animate-spin mr-2" />
              Loading plan...
            </div>
          ) : error ? (
            <div className="border-l-2 border-red-500 bg-red-500/10 px-3 py-1.5 text-xs text-red-400 rounded-r">
              {error}
            </div>
          ) : plan.subtasks.length === 0 ? (
            <div className="text-center text-xs text-muted-foreground py-3">
              No subtasks in plan
            </div>
          ) : (
            plan.subtasks.map((subtask, idx) => (
              <SubtaskRow
                key={subtask.index}
                subtask={subtask}
                isLast={idx === plan.subtasks.length - 1}
              />
            ))
          )}
        </div>
      )}
    </div>
  );
}
