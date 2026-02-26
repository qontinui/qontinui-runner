/**
 * SchedulerStats
 *
 * Statistics dashboard showing key scheduler metrics.
 * Displays total tasks, active tasks, execution count, success rate, and average duration.
 */

import { useMemo } from "react";
import {
  Calendar,
  Play,
  Activity,
  CheckCircle,
  Clock,
  TrendingUp,
  TrendingDown,
} from "lucide-react";
import type { ScheduledTask, TaskExecutionRecord, SchedulerStatus } from "../../types/scheduler";

interface SchedulerStatsProps {
  tasks: ScheduledTask[];
  taskHistory: TaskExecutionRecord[];
  status: SchedulerStatus | null;
}

interface StatCard {
  label: string;
  value: string;
  icon: React.ReactNode;
  colorClass?: string;
  trend?: "up" | "down" | null;
}

/**
 * Format a duration in seconds to a human-readable string.
 */
function formatDuration(seconds: number): string {
  if (!isFinite(seconds) || seconds < 0) return "\u2014";
  const rounded = Math.round(seconds);
  if (rounded < 60) return `${rounded}s`;
  const minutes = Math.floor(rounded / 60);
  const remainingSeconds = rounded % 60;
  if (minutes < 60) {
    return remainingSeconds > 0 ? `${minutes}m ${remainingSeconds}s` : `${minutes}m`;
  }
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}

/**
 * Get the appropriate color class for a success rate percentage.
 */
function getSuccessRateColor(rate: number): string {
  if (rate > 80) return "text-green-500";
  if (rate > 50) return "text-yellow-500";
  return "text-red-500";
}

export function SchedulerStats({ tasks, taskHistory, status }: SchedulerStatsProps) {
  const stats = useMemo((): StatCard[] => {
    const totalTasks = tasks.length;
    const activeTasks = tasks.filter((t) => t.enabled).length;
    const totalExecutions = taskHistory.length;

    // Success rate calculation
    let successRateValue: string;
    let successRateColor: string | undefined;
    let successTrend: "up" | "down" | null = null;

    if (totalExecutions === 0) {
      successRateValue = "\u2014";
    } else {
      const successfulExecutions = taskHistory.filter((r) => r.success).length;
      const overallRate = (successfulExecutions / totalExecutions) * 100;
      successRateValue = `${overallRate.toFixed(0)}%`;
      successRateColor = getSuccessRateColor(overallRate);

      // Calculate trend: compare last 10 executions vs overall
      if (totalExecutions > 10) {
        const recentRecords = taskHistory.slice(-10);
        const recentSuccessful = recentRecords.filter((r) => r.success).length;
        const recentRate = (recentSuccessful / recentRecords.length) * 100;
        if (recentRate > overallRate + 5) {
          successTrend = "up";
        } else if (recentRate < overallRate - 5) {
          successTrend = "down";
        }
      }
    }

    // Average duration calculation
    let avgDurationValue: string;
    const completedRecords = taskHistory.filter((r) => r.ended_at);
    if (completedRecords.length === 0) {
      avgDurationValue = "\u2014";
    } else {
      const totalDuration = completedRecords.reduce((sum, r) => {
        const start = new Date(r.started_at).getTime();
        const end = new Date(r.ended_at!).getTime();
        return sum + (end - start) / 1000;
      }, 0);
      avgDurationValue = formatDuration(totalDuration / completedRecords.length);
    }

    return [
      {
        label: "Total Tasks",
        value: String(totalTasks),
        icon: <Calendar className="w-4 h-4 text-muted-foreground" />,
      },
      {
        label: "Active Tasks",
        value: String(activeTasks),
        icon: <Play className="w-4 h-4 text-muted-foreground" />,
      },
      {
        label: "Total Executions",
        value: String(totalExecutions),
        icon: <Activity className="w-4 h-4 text-muted-foreground" />,
      },
      {
        label: "Success Rate",
        value: successRateValue,
        icon: <CheckCircle className="w-4 h-4 text-muted-foreground" />,
        colorClass: successRateColor,
        trend: successTrend,
      },
      {
        label: "Avg Duration",
        value: avgDurationValue,
        icon: <Clock className="w-4 h-4 text-muted-foreground" />,
      },
    ];
  }, [tasks, taskHistory]);

  return (
    <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-5 gap-3 px-4 pt-4">
      {stats.map((stat) => (
        <div key={stat.label} className="bg-card border border-border rounded-lg p-3">
          <div className="flex items-center gap-2 mb-1">
            {stat.icon}
            <span className="text-xs text-muted-foreground">{stat.label}</span>
          </div>
          <div className="flex items-center gap-1.5">
            <span className={`text-lg font-semibold ${stat.colorClass ?? ""}`}>{stat.value}</span>
            {stat.trend === "up" && <TrendingUp className="w-3.5 h-3.5 text-green-500" />}
            {stat.trend === "down" && <TrendingDown className="w-3.5 h-3.5 text-red-500" />}
          </div>
        </div>
      ))}
    </div>
  );
}
