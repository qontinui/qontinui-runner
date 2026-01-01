/**
 * ConfigHealthPanel.tsx
 *
 * Shows overall config health metrics:
 * - Success rate (with color coding)
 * - Total runs
 * - Average duration
 * - Last run timestamp
 */

import { CheckCircle2, XCircle, Clock, Activity } from "lucide-react";
import type { ConfigStatistics } from "../../types/statistics";

interface ConfigHealthPanelProps {
  stats: ConfigStatistics;
}

export function ConfigHealthPanel({ stats }: ConfigHealthPanelProps) {
  const successRate = stats.total_runs > 0 ? (stats.successful_runs / stats.total_runs) * 100 : 0;

  const getHealthColor = (rate: number) => {
    if (rate >= 90) return "text-green-500";
    if (rate >= 70) return "text-yellow-500";
    return "text-red-500";
  };

  const getHealthBg = (rate: number) => {
    if (rate >= 90) return "bg-green-500/10";
    if (rate >= 70) return "bg-yellow-500/10";
    return "bg-red-500/10";
  };

  const formatDuration = (ms: number | undefined) => {
    if (!ms) return "N/A";
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
    return `${(ms / 60000).toFixed(1)}m`;
  };

  const formatDate = (iso: string | undefined) => {
    if (!iso) return "Never";
    const date = new Date(iso);
    return date.toLocaleDateString() + " " + date.toLocaleTimeString();
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Activity className="w-4 h-4" />
        Config Health
      </h3>

      <div className="grid grid-cols-2 gap-4">
        {/* Success Rate */}
        <div className={`p-3 rounded-md ${getHealthBg(successRate)}`}>
          <div className="flex items-center gap-2 mb-1">
            {successRate >= 70 ? (
              <CheckCircle2 className={`w-4 h-4 ${getHealthColor(successRate)}`} />
            ) : (
              <XCircle className={`w-4 h-4 ${getHealthColor(successRate)}`} />
            )}
            <span className="text-sm text-muted-foreground">Success Rate</span>
          </div>
          <div className={`text-2xl font-bold ${getHealthColor(successRate)}`}>
            {successRate.toFixed(1)}%
          </div>
          {stats.recent_success_rate !== undefined && (
            <div className="text-xs text-muted-foreground mt-1">
              Recent: {(stats.recent_success_rate * 100).toFixed(1)}%
            </div>
          )}
        </div>

        {/* Total Runs */}
        <div className="p-3 rounded-md bg-muted/30">
          <div className="text-sm text-muted-foreground mb-1">Total Runs</div>
          <div className="text-2xl font-bold">{stats.total_runs}</div>
          <div className="text-xs text-muted-foreground mt-1">
            {stats.successful_runs} passed / {stats.failed_runs} failed
          </div>
        </div>

        {/* Average Duration */}
        <div className="p-3 rounded-md bg-muted/30">
          <div className="flex items-center gap-2 mb-1">
            <Clock className="w-4 h-4 text-muted-foreground" />
            <span className="text-sm text-muted-foreground">Avg Duration</span>
          </div>
          <div className="text-2xl font-bold">{formatDuration(stats.avg_duration_ms)}</div>
          {stats.recent_avg_duration_ms && (
            <div className="text-xs text-muted-foreground mt-1">
              Recent: {formatDuration(stats.recent_avg_duration_ms)}
            </div>
          )}
        </div>

        {/* Last Run */}
        <div className="p-3 rounded-md bg-muted/30">
          <div className="text-sm text-muted-foreground mb-1">Last Run</div>
          <div className="text-sm font-medium">{formatDate(stats.last_run_at)}</div>
          {stats.first_run_at && (
            <div className="text-xs text-muted-foreground mt-1">
              First: {formatDate(stats.first_run_at)}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
