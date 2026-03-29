import React from "react";

interface ExplorationStats {
  total_candidates_explored: number;
  search_depth_reached: number;
  search_duration_ms: number;
  score_progression: [number, number][];
  strategy_used: string;
}

interface ExplorationPanelProps {
  stats: ExplorationStats | null;
}

export const ExplorationPanel: React.FC<ExplorationPanelProps> = ({ stats }) => {
  if (!stats) return null;

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 p-4 space-y-3">
      <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
        Exploration Results
      </h3>
      <div className="grid grid-cols-2 gap-3 text-sm">
        <div>
          <span className="text-zinc-500 dark:text-zinc-400">Candidates explored</span>
          <p className="font-mono text-zinc-900 dark:text-zinc-100">{stats.total_candidates_explored}</p>
        </div>
        <div>
          <span className="text-zinc-500 dark:text-zinc-400">Search depth</span>
          <p className="font-mono text-zinc-900 dark:text-zinc-100">{stats.search_depth_reached}</p>
        </div>
        <div>
          <span className="text-zinc-500 dark:text-zinc-400">Duration</span>
          <p className="font-mono text-zinc-900 dark:text-zinc-100">{stats.search_duration_ms}ms</p>
        </div>
        <div>
          <span className="text-zinc-500 dark:text-zinc-400">Strategy</span>
          <p className="font-mono text-zinc-900 dark:text-zinc-100 truncate">{stats.strategy_used}</p>
        </div>
      </div>
      {stats.score_progression.length > 0 && (
        <div className="space-y-1">
          <span className="text-xs text-zinc-500 dark:text-zinc-400">Score progression</span>
          <div className="flex items-end gap-1 h-12">
            {stats.score_progression.map(([idx, score], i) => (
              <div
                key={i}
                className="flex-1 bg-blue-500 dark:bg-blue-400 rounded-t-sm transition-all"
                style={{ height: `${score * 100}%` }}
                title={`Candidate ${idx}: ${(score * 100).toFixed(1)}%`}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
};
