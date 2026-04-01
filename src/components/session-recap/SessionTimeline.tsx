import { useMemo } from "react";
import { ScatterChart, Scatter, XAxis, YAxis, Tooltip, ResponsiveContainer } from "recharts";
import type { FileChange } from "@/lib/session-recap/types";

interface Props {
  filesCreated: FileChange[];
  filesModified: FileChange[];
  filesDeleted: FileChange[];
}

const LAYER_MAP: Record<string, number> = {
  frontend: 0,
  backend: 1,
  database: 2,
  python: 3,
  test: 4,
  config: 5,
  spec: 0,
  other: 5,
};

const LAYER_LABELS = ["Frontend", "Backend", "Database", "Python", "Tests", "Config"];

const CHANGE_COLORS: Record<string, string> = {
  created: "#22C55E",
  modified: "#3B82F6",
  deleted: "#EF4444",
};

interface DataPoint {
  x: number;
  y: number;
  file: FileChange;
}

export function SessionTimeline({ filesCreated, filesModified, filesDeleted }: Props) {
  const allFiles = useMemo(
    () => [...filesCreated, ...filesModified, ...filesDeleted],
    [filesCreated, filesModified, filesDeleted],
  );

  const data = useMemo(() => {
    // Group by repo for x-axis ordering
    const repos = [...new Set(allFiles.map((f) => f.repo))];
    const repoIndex = Object.fromEntries(repos.map((r, i) => [r, i]));

    return allFiles.map(
      (file, idx): DataPoint => ({
        x: repoIndex[file.repo] * 100 + idx,
        y: LAYER_MAP[file.category] ?? 5,
        file,
      }),
    );
  }, [allFiles]);

  if (allFiles.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-sm text-muted-foreground">
        No file changes in this period
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      <h3 className="text-[11px] font-medium text-muted-foreground mb-2 px-1">
        File Changes by Layer
      </h3>

      {/* Legend */}
      <div className="flex gap-3 px-1 mb-2">
        {Object.entries(CHANGE_COLORS).map(([type_, color]) => (
          <div key={type_} className="flex items-center gap-1 text-[10px] text-muted-foreground">
            <div className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
            {type_}
          </div>
        ))}
      </div>

      <div className="flex-1 min-h-0">
        <ResponsiveContainer width="100%" height="100%">
          <ScatterChart margin={{ left: 60, right: 8, top: 8, bottom: 8 }}>
            <XAxis dataKey="x" type="number" tick={false} axisLine={false} tickLine={false} />
            <YAxis
              dataKey="y"
              type="number"
              domain={[0, 5]}
              ticks={[0, 1, 2, 3, 4, 5]}
              tickFormatter={(v: number) => LAYER_LABELS[v] ?? ""}
              tick={{ fontSize: 10, fill: "hsl(var(--muted-foreground))" }}
              axisLine={false}
              tickLine={false}
              width={56}
            />
            <Tooltip
              content={({ payload }) => {
                const dp = payload?.[0]?.payload as DataPoint | undefined;
                if (!dp) return null;
                const f = dp.file;
                return (
                  <div className="bg-popover border border-border rounded-lg shadow-lg p-2 text-xs max-w-xs">
                    <div className="font-medium">{f.path}</div>
                    <div className="text-muted-foreground">{f.repo}</div>
                    <div className="flex gap-2 mt-1">
                      <span className="text-green-500">+{f.lines_added}</span>
                      <span className="text-red-400">-{f.lines_removed}</span>
                      <span className="text-muted-foreground">{f.language}</span>
                    </div>
                  </div>
                );
              }}
            />
            <Scatter
              data={data}
              shape={(props) => {
                const {
                  cx = 0,
                  cy = 0,
                  payload,
                } = props as { cx?: number; cy?: number; payload: DataPoint };
                const r = Math.min(
                  3 + Math.sqrt(payload.file.lines_added + payload.file.lines_removed) * 0.5,
                  10,
                );
                const fill = CHANGE_COLORS[payload.file.change_type] ?? "#6B7280";
                return <circle cx={cx} cy={cy} r={r} fill={fill} />;
              }}
            />
          </ScatterChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
