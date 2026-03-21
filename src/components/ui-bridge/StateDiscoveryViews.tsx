import { useMemo } from "react";
import { Badge } from "../ui/Badge";
import { AlertCircle } from "lucide-react";
import type { CooccurrenceExport } from "../../types/ui-bridge-types";
import { ZONE_COLORS, SIZE_LABELS } from "./state-discovery-constants";

export function StatsView({ data }: { data: CooccurrenceExport }) {
  const stats = useMemo(() => {
    const zoneDistribution: Record<string, number> = {};
    const landmarkDistribution: Record<string, number> = {};
    const sizeDistribution: Record<string, number> = {};
    let repeatingCount = 0;
    let totalAppearances = 0;

    Object.values(data.fingerprintDetails).forEach((fp) => {
      zoneDistribution[fp.positionZone] = (zoneDistribution[fp.positionZone] || 0) + 1;
      landmarkDistribution[fp.landmarkContext] =
        (landmarkDistribution[fp.landmarkContext] || 0) + 1;
      sizeDistribution[fp.sizeCategory] = (sizeDistribution[fp.sizeCategory] || 0) + 1;
      if (fp.isRepeating) repeatingCount++;
    });

    Object.values(data.fingerprintStats).forEach((stat) => {
      totalAppearances += stat.totalAppearances;
    });

    return {
      totalFingerprints: data.allFingerprints.length,
      totalCaptures: data.presenceMatrix.length,
      totalTransitions: data.transitions.length,
      stateCandidates: data.stateCandidates.length,
      repeatingCount,
      avgAppearances:
        data.allFingerprints.length > 0
          ? (totalAppearances / data.allFingerprints.length).toFixed(1)
          : "0",
      zoneDistribution,
      landmarkDistribution,
      sizeDistribution,
    };
  }, [data]);

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalFingerprints}</div>
          <div className="text-xs text-muted-foreground">Unique Fingerprints</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalCaptures}</div>
          <div className="text-xs text-muted-foreground">Captures</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.stateCandidates}</div>
          <div className="text-xs text-muted-foreground">State Candidates</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalTransitions}</div>
          <div className="text-xs text-muted-foreground">Transitions</div>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-lg font-bold">{stats.repeatingCount}</div>
          <div className="text-xs text-muted-foreground">Repeating Elements</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-lg font-bold">{stats.avgAppearances}</div>
          <div className="text-xs text-muted-foreground">Avg Appearances/Element</div>
        </div>
      </div>

      <div className="space-y-2">
        <div className="text-sm font-medium">Position Zone Distribution</div>
        <div className="space-y-1">
          {Object.entries(stats.zoneDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([zone, count]) => {
              const percentage = ((count / stats.totalFingerprints) * 100).toFixed(0);
              return (
                <div key={zone} className="flex items-center gap-2">
                  <div className="w-24 text-xs text-muted-foreground">{zone}</div>
                  <div className="flex-1 h-4 bg-muted/30 rounded overflow-hidden">
                    <div
                      className={`h-full ${ZONE_COLORS[zone]?.split(" ")[0] || "bg-primary/50"}`}
                      style={{ width: `${percentage}%` }}
                    />
                  </div>
                  <div className="w-12 text-xs text-right">{count}</div>
                </div>
              );
            })}
        </div>
      </div>

      <div className="space-y-2">
        <div className="text-sm font-medium">Size Category Distribution</div>
        <div className="space-y-1">
          {Object.entries(stats.sizeDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([size, count]) => {
              const percentage = ((count / stats.totalFingerprints) * 100).toFixed(0);
              return (
                <div key={size} className="flex items-center gap-2">
                  <div className="w-24 text-xs text-muted-foreground">
                    {SIZE_LABELS[size] || size}
                  </div>
                  <div className="flex-1 h-4 bg-muted/30 rounded overflow-hidden">
                    <div className="h-full bg-primary/50" style={{ width: `${percentage}%` }} />
                  </div>
                  <div className="w-12 text-xs text-right">{count}</div>
                </div>
              );
            })}
        </div>
      </div>

      <div className="space-y-2">
        <div className="text-sm font-medium">Landmark Distribution</div>
        <div className="flex flex-wrap gap-2">
          {Object.entries(stats.landmarkDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([landmark, count]) => (
              <Badge key={landmark} variant="purple">
                {landmark}: {count}
              </Badge>
            ))}
        </div>
      </div>
    </div>
  );
}

export function CooccurrenceMatrixView({ data }: { data: CooccurrenceExport }) {
  const matrixSize = data.allFingerprints.length;
  const showFullMatrix = matrixSize <= 20;

  if (!showFullMatrix) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-2 text-yellow-500">
          <AlertCircle className="w-4 h-4" />
          <span className="text-sm">
            Matrix too large to display ({matrixSize}x{matrixSize}). Showing summary instead.
          </span>
        </div>

        <div className="space-y-2">
          <div className="text-sm font-medium">High Co-occurrence Pairs</div>
          <div className="text-xs text-muted-foreground mb-2">
            Fingerprint pairs that always appear together (100% co-occurrence)
          </div>
          <div className="max-h-64 overflow-y-auto space-y-1">
            {data.stateCandidates.slice(0, 10).map((candidate) => {
              const firstHash = candidate.fingerprints[0];
              const key = firstHash ?? candidate.cooccurrenceRate.toString();
              return (
                <div key={key} className="p-2 bg-muted/30 rounded text-xs flex items-center gap-2">
                  <Badge variant="success" size="sm">
                    {candidate.fingerprints.length} elements
                  </Badge>
                  <span className="text-muted-foreground">always appear together</span>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="text-sm font-medium">Co-occurrence Matrix</div>
      <div className="text-xs text-muted-foreground">
        Shows how often fingerprint pairs appear together (normalized 0-1)
      </div>
      <div className="overflow-auto max-h-96">
        <table className="text-[10px] border-collapse">
          <thead>
            <tr>
              <th className="p-1" />
              {data.allFingerprints.slice(0, 20).map((fp, i) => (
                <th
                  key={fp}
                  className="p-1 font-mono text-muted-foreground rotate-45 origin-left"
                  title={fp}
                >
                  {i + 1}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.allFingerprints.slice(0, 20).map((fp1) => (
              <tr key={fp1}>
                <td className="p-1 font-mono text-muted-foreground" title={fp1}>
                  {data.allFingerprints.indexOf(fp1) + 1}
                </td>
                {data.allFingerprints.slice(0, 20).map((fp2) => {
                  const count1 = data.cooccurrenceCounts[fp1]?.[fp2] || 0;
                  const total1 = data.fingerprintStats[fp1]?.totalAppearances || 1;
                  const rate = count1 / total1;

                  const bgColor =
                    fp1 === fp2
                      ? "bg-primary/30"
                      : rate > 0.9
                        ? "bg-green-500/50"
                        : rate > 0.5
                          ? "bg-yellow-500/30"
                          : rate > 0
                            ? "bg-red-500/20"
                            : "";

                  return (
                    <td
                      key={fp2}
                      className={`p-1 text-center border border-border/20 ${bgColor}`}
                      title={`${fp1} + ${fp2}: ${(rate * 100).toFixed(0)}%`}
                    >
                      {rate > 0 ? (rate * 100).toFixed(0) : ""}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
