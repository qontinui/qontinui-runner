/**
 * ActionStream Component
 *
 * Displays a live stream of actions being executed.
 * Shows action type, status, timing, and hierarchical structure.
 */

import { CheckCircle2, XCircle, Loader2, ChevronRight } from "lucide-react";
import { Badge, ScrollArea } from "../ui";
import type { ActionStreamProps, ActionItem, ActionType, ActionStatus } from "./types";

const actionTypeColors: Record<ActionType, string> = {
  FIND: "bg-blue-500/20 text-blue-400 border border-blue-500/50",
  CLICK: "bg-green-500/20 text-green-400 border border-green-500/50",
  TYPE: "bg-purple-500/20 text-purple-400 border border-purple-500/50",
  WAIT: "bg-zinc-500/20 text-zinc-400 border border-zinc-500/50",
  GO_TO_STATE: "bg-cyan-500/20 text-cyan-400 border border-cyan-500/50",
  RUN_WORKFLOW: "bg-indigo-500/20 text-indigo-400 border border-indigo-500/50",
};

function StatusIcon({ status }: { status: ActionStatus }) {
  switch (status) {
    case "running":
      return <Loader2 className="h-4 w-4 animate-spin text-blue-400" />;
    case "success":
      return <CheckCircle2 className="h-4 w-4 text-green-400" />;
    case "failed":
      return <XCircle className="h-4 w-4 text-red-400" />;
    default:
      return <div className="h-4 w-4 rounded-full border-2 border-muted-foreground/50" />;
  }
}

function ActionRow({ action, isActive }: { action: ActionItem; isActive: boolean }) {
  const relativeTime = action.timestamp
    ? `+${((Date.now() - action.timestamp) / 1000).toFixed(1)}s`
    : "0.0s";

  return (
    <div
      className={`flex items-start gap-3 border-l-2 px-4 py-3 transition-colors hover:bg-muted/30 ${
        isActive ? "border-blue-500 bg-blue-500/5" : "border-transparent"
      }`}
      style={{ paddingLeft: `${action.level * 24 + 16}px` }}
    >
      <StatusIcon status={action.status} />

      <span className="w-16 font-mono text-xs text-muted-foreground">{relativeTime}</span>

      <Badge className={`${actionTypeColors[action.action_type]} text-xs`}>
        {action.action_type}
      </Badge>

      <div className="flex-1">
        <div className="flex items-center gap-2">
          <span className="font-mono text-sm text-foreground">{action.target}</span>
          {action.children && action.children.length > 0 && (
            <ChevronRight className="h-3 w-3 text-muted-foreground" />
          )}
        </div>
        {action.result && <p className="mt-1 text-xs text-muted-foreground">{action.result}</p>}
        {action.error && <p className="mt-1 text-xs text-red-400">{action.error}</p>}
      </div>

      {action.duration && (
        <span className="font-mono text-xs text-muted-foreground">{action.duration}ms</span>
      )}
    </div>
  );
}

export function ActionStream({ actions, currentAction: _currentAction }: ActionStreamProps) {
  return (
    <div className="flex h-[40%] flex-col bg-card">
      <div className="flex items-center justify-between border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-foreground">Action Stream</h3>
        <div className="flex gap-2">
          <Badge variant="muted" className="text-xs">
            {actions.length} actions
          </Badge>
        </div>
      </div>

      <ScrollArea className="flex-1">
        <div className="flex flex-col-reverse">
          {actions.map((action) => (
            <ActionRow key={action.id} action={action} isActive={action.status === "running"} />
          ))}
        </div>
      </ScrollArea>
    </div>
  );
}
