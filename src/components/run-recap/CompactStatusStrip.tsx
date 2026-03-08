/**
 * CompactStatusStrip
 *
 * A single slim horizontal bar that merges the former StatusBanner + Session Stats
 * into inline pill badges: status, iterations, duration, sessions, output size.
 */

import {
  CheckCircle2,
  XCircle,
  Activity,
  Clock,
  Repeat,
  AlertTriangle,
  StopCircle,
  Users,
  FileText,
} from "lucide-react";
import { formatDuration } from "@/lib/formatting";
import type { LoopResult } from "@/types/aiData";

interface CompactStatusStripProps {
  status: string;
  duration?: number;
  loopResult?: LoopResult | null;
  sessionsCount?: number;
  maxSessions?: number;
  outputLog?: string | null;
  autoContinue?: boolean;
}

export function CompactStatusStrip({
  status,
  duration,
  loopResult,
  sessionsCount,
  maxSessions,
  outputLog,
  autoContinue,
}: CompactStatusStripProps) {
  const isSuccess = status === "complete" || status === "completed";
  const isFailed = status === "failed";
  const isRunning = status === "running";

  let statusBg = "bg-muted";
  let statusText = "text-muted-foreground";
  let StatusIcon = Clock;
  let label = "Unknown";

  if (loopResult) {
    if (loopResult.was_stopped) {
      statusBg = "bg-amber-500/15";
      statusText = "text-amber-400";
      StatusIcon = StopCircle;
      label = "Stopped";
    } else if (loopResult.critical_failure) {
      statusBg = "bg-red-500/15";
      statusText = "text-red-400";
      StatusIcon = AlertTriangle;
      label = "Critical Failure";
    } else if (loopResult.verification_passed) {
      statusBg = "bg-green-500/15";
      statusText = "text-green-400";
      StatusIcon = CheckCircle2;
      label = "Passed";
    } else if (loopResult.max_iterations_reached) {
      statusBg = "bg-amber-500/15";
      statusText = "text-amber-400";
      StatusIcon = AlertTriangle;
      label = "Max Iterations";
    } else if (isSuccess) {
      statusBg = "bg-green-500/15";
      statusText = "text-green-400";
      StatusIcon = CheckCircle2;
      label = "Completed";
    }
  } else if (isSuccess) {
    statusBg = "bg-green-500/15";
    statusText = "text-green-400";
    StatusIcon = CheckCircle2;
    label = "Completed";
  } else if (isFailed) {
    statusBg = "bg-red-500/15";
    statusText = "text-red-400";
    StatusIcon = XCircle;
    label = "Failed";
  } else if (isRunning) {
    statusBg = "bg-blue-500/15";
    statusText = "text-blue-400";
    StatusIcon = Activity;
    label = "Running";
  }

  const outputSize = outputLog ? `${Math.round(outputLog.length / 1024)}KB` : null;

  return (
    <div className={`flex items-center gap-2 px-3 py-2 rounded-lg ${statusBg} flex-wrap`}>
      {/* Status */}
      <div className={`flex items-center gap-1.5 text-sm font-medium ${statusText}`}>
        <StatusIcon className={`w-4 h-4 ${isRunning ? "animate-pulse" : ""}`} />
        <span>{label}</span>
      </div>

      <Separator />

      {/* Iterations */}
      {loopResult && loopResult.iterations_run > 0 && (
        <>
          <Pill>
            <Repeat className="w-3 h-3 text-muted-foreground" />
            <span>
              {loopResult.iterations_run} iter{loopResult.iterations_run !== 1 ? "s" : ""}
            </span>
          </Pill>
          <Separator />
        </>
      )}

      {/* Duration */}
      <Pill>
        <Clock className="w-3 h-3 text-muted-foreground" />
        <span>{duration !== undefined ? formatDuration(duration) : "In progress..."}</span>
      </Pill>

      <Separator />

      {/* Sessions */}
      {sessionsCount !== undefined && (
        <>
          <Pill>
            <Users className="w-3 h-3 text-muted-foreground" />
            <span>
              {sessionsCount} session{sessionsCount !== 1 ? "s" : ""}
              {maxSessions ? ` / ${maxSessions}` : ""}
            </span>
          </Pill>
          <Separator />
        </>
      )}

      {/* Output size */}
      {outputSize && (
        <Pill>
          <FileText className="w-3 h-3 text-muted-foreground" />
          <span>{outputSize} output</span>
        </Pill>
      )}

      {/* Auto-continue badge */}
      {autoContinue && (
        <>
          <Separator />
          <span className="text-xs px-2 py-0.5 rounded bg-blue-500/10 text-blue-400">
            Auto-continue
          </span>
        </>
      )}
    </div>
  );
}

function Separator() {
  return <div className="w-px h-3.5 bg-border/50" />;
}

function Pill({ children }: { children: React.ReactNode }) {
  return <div className="flex items-center gap-1 text-xs text-foreground/70">{children}</div>;
}
