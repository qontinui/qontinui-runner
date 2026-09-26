/**
 * Pre-start restart capability for the Orchestration Loop panels.
 *
 * The restart between-iteration modes (`restart_runner`, `restart_on_signal`)
 * can only run when the orchestrating runner has a way to restart the target:
 * in-process through its own `InstanceManager` (a child it spawned), or — for
 * `rebuild: true` only — through the dev supervisor. The backend command
 * `orchestration_loop_restart_capability` answers that BEFORE start; this
 * module asks it (debounced, stale answers dropped), renders its reason, and
 * derives whether Start is blocked. A mode is never hidden: the user sees why
 * it cannot run here and can change the mode or the target.
 *
 * Plan: 2026-09-22-orchestration-loop-restart-modes-depend-on-the-dev-only-supervisor
 * (Design §2).
 */

import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Loader2 } from "lucide-react";
import { cn } from "../../lib/utils";

// --- Wire types ---

export type RestartPathKind = "instance_manager" | "dev_supervisor" | "not_needed";

export type RestartUnsupportedCode =
  | "target_is_orchestrator"
  | "target_not_runner_managed"
  | "rebuild_needs_dev_supervisor";

/**
 * Local mirror of qontinui-schemas `RestartCapability`
 * (`ts/src/generated/RestartCapability.d.ts`, from
 * `rust/src/orchestration_config.rs`). The runner frontend does not resolve
 * generated qontinui-schemas TS (no `@qontinui/schemas` import exists under
 * `src/`, and the published `@qontinui/shared-types` predates this type), so
 * the shape is mirrored field-for-field: camelCase keys, optional fields
 * OMITTED (not null) when the Rust side holds `None`; enum values snake_case.
 */
export interface RestartCapability {
  code?: RestartUnsupportedCode | null;
  instanceId?: string | null;
  path?: RestartPathKind | null;
  reason?: string | null;
  supported: boolean;
  targetPort: number;
}

/**
 * Validate one IPC response; `null` when it carries no `supported` verdict
 * (an absent verdict is UNKNOWN, never "supported"). The schema type accepts
 * `target_port` / `instance_id` as deserialize aliases, so a snake_case
 * payload is folded onto the camelCase keys rather than dropped.
 */
export function normalizeRestartCapability(raw: unknown): RestartCapability | null {
  if (!raw || typeof raw !== "object") return null;
  const w = raw as RestartCapability & { target_port?: number; instance_id?: string | null };
  if (typeof w.supported !== "boolean") return null;
  return {
    supported: w.supported,
    path: w.path ?? null,
    code: w.code ?? null,
    reason: w.reason ?? null,
    targetPort: w.targetPort ?? w.target_port ?? 0,
    instanceId: w.instanceId ?? w.instance_id ?? null,
  };
}

// --- Between-mode helpers shared by both panels ---

export type BetweenWire = { type: string; rebuild?: boolean };

/** Map a "Between" select value to the backend's `between_iterations`. */
export function betweenToWire(between: string): BetweenWire {
  if (between === "restart_on_signal") return { type: "restart_on_signal", rebuild: true };
  if (between === "restart_on_signal_no_rebuild")
    return { type: "restart_on_signal", rebuild: false };
  if (between === "restart_runner") return { type: "restart_runner", rebuild: true };
  if (between === "restart_runner_no_rebuild") return { type: "restart_runner", rebuild: false };
  if (between === "wait_healthy") return { type: "wait_healthy" };
  return { type: "none" };
}

/** True for the select values whose mode restarts the target. */
export function isRestartMode(between: string): boolean {
  return between.startsWith("restart_");
}

/**
 * Default "Between" value for a fresh form. Targeting the orchestrating runner
 * (the default target), no restart mode can run — restarting it would end the
 * loop — so a first-run user is handed `wait_healthy`, not an invalid config.
 */
export function defaultBetween(targetIsSelf: boolean): string {
  return targetIsSelf ? "wait_healthy" : "restart_on_signal";
}

/**
 * Value restored from a saved config. A saved value is kept VERBATIM even when
 * the capability read later refuses it (the panel shows the reason instead);
 * only a missing value falls back to the default for the restored target.
 */
export function restoreBetween(saved: string | null | undefined, targetIsSelf: boolean): string {
  return saved || defaultBetween(targetIsSelf);
}

// --- The capability request ---

/** The restart-relevant slice of an `OrchestrationLoopConfig`. */
export interface RestartCapabilityProbe {
  target_runner_port: number | null;
  target_runner_id: string | null;
  supervisor_port: number;
  between_iterations: BetweenWire;
}

/**
 * The config object sent to `orchestration_loop_restart_capability`. The
 * verdict depends only on the probe fields; the rest are the backend's own
 * defaults so the object deserializes exactly like a start request.
 */
export function capabilityConfig(probe: RestartCapabilityProbe) {
  return {
    target_runner_port: probe.target_runner_port,
    target_runner_id: probe.target_runner_id,
    supervisor_port: probe.supervisor_port,
    workflow_id: "",
    max_iterations: null,
    exit_strategy: { type: "reflection" },
    between_iterations: probe.between_iterations,
    retry_on_failure: false,
    wait_for_fixer: true,
    pipeline: null,
  };
}

export type RestartCapabilityState =
  /** Nothing to ask (no target configured yet). */
  | { status: "idle" }
  /** A request for the CURRENT probe is pending. */
  | { status: "checking" }
  | { status: "ready"; capability: RestartCapability }
  /** The command failed or answered without a verdict — UNKNOWN, not "supported". */
  | { status: "unknown"; error: string };

export type CapabilityInvoke = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;

/** A settled answer — what the backend said, or UNKNOWN. */
export type CapabilityResult = Exclude<RestartCapabilityState, { status: "idle" | "checking" }>;

export const CAPABILITY_DEBOUNCE_MS = 250;

/**
 * Ask the backend once for `probe`. Never throws: a failed or verdict-less
 * answer is `unknown`.
 */
export async function fetchRestartCapability(
  probe: RestartCapabilityProbe,
  invokeFn: CapabilityInvoke = invoke,
): Promise<CapabilityResult> {
  try {
    const raw = await invokeFn("orchestration_loop_restart_capability", {
      config: capabilityConfig(probe),
    });
    const capability = normalizeRestartCapability(raw);
    if (!capability) {
      return { status: "unknown", error: "capability check returned no verdict" };
    }
    return { status: "ready", capability };
  } catch (e) {
    return { status: "unknown", error: String(e) };
  }
}

/**
 * Debounced, stale-safe requester over a LIST of probes (one per loop; the
 * single-loop panel passes one). Each `request` supersedes the previous one:
 * a pending timer is cleared, and answers that arrive after a newer request
 * (or a `cancel`) are dropped instead of delivered.
 */
export function createCapabilityRequester(
  invokeFn: CapabilityInvoke = invoke,
  delayMs: number = CAPABILITY_DEBOUNCE_MS,
) {
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const cancel = () => {
    seq += 1;
    if (timer) clearTimeout(timer);
    timer = null;
  };
  const request = (
    probes: RestartCapabilityProbe[],
    onResult: (results: CapabilityResult[]) => void,
  ) => {
    cancel();
    const mine = seq;
    timer = setTimeout(() => {
      timer = null;
      void Promise.all(probes.map((p) => fetchRestartCapability(p, invokeFn))).then((results) => {
        if (mine === seq) onResult(results);
      });
    }, delayMs);
  };
  return { request, cancel };
}

/**
 * Ask the capability command for every probe whenever the list changes. Each
 * returned state is DERIVED against the current key, so a verdict for a
 * previous target/mode is never shown (or used to gate Start) while the new
 * one is pending.
 */
export function useRestartCapabilities(
  probes: RestartCapabilityProbe[],
  invokeFn: CapabilityInvoke = invoke,
): RestartCapabilityState[] {
  const key = probes.length > 0 ? JSON.stringify(probes) : "";
  const [answer, setAnswer] = useState<{ key: string; results: CapabilityResult[] } | null>(null);

  useEffect(() => {
    if (!key) return;
    const requester = createCapabilityRequester(invokeFn);
    requester.request(JSON.parse(key) as RestartCapabilityProbe[], (results) =>
      setAnswer({ key, results }),
    );
    return requester.cancel;
  }, [key, invokeFn]);

  if (!key) return [];
  if (!answer || answer.key !== key) return probes.map(() => ({ status: "checking" }));
  return answer.results;
}

/** Single-probe form of {@link useRestartCapabilities}. */
export function useRestartCapability(
  probe: RestartCapabilityProbe | null,
  invokeFn: CapabilityInvoke = invoke,
): RestartCapabilityState {
  const states = useRestartCapabilities(probe ? [probe] : [], invokeFn);
  return states[0] ?? { status: "idle" };
}

/**
 * Why Start is blocked, or `null` when it is not. Only a definite
 * `supported: false` verdict blocks; an UNKNOWN answer does not, because the
 * backend re-runs the same check at start and refuses there with
 * `unsupported here: …`.
 */
export function startBlockedReason(state: RestartCapabilityState): string | null {
  if (state.status !== "ready" || state.capability.supported) return null;
  return state.capability.reason || unsupportedFallbackReason(state.capability.code ?? null);
}

/**
 * Start-blocked reason for a multi-loop: the first loop whose target refuses
 * the mode, named by its label. The backend refuses the WHOLE multi-loop when
 * any one entry is unsupported, so one refusal blocks Start All.
 */
export function multiStartBlockedReason(
  states: RestartCapabilityState[],
  labels: string[],
): string | null {
  const blocked = states
    .map((st, i) => ({ reason: startBlockedReason(st), label: labels[i] ?? `loop ${i + 1}` }))
    .filter((b): b is { reason: string; label: string } => b.reason !== null);
  if (blocked.length === 0) return null;
  const first = `${blocked[0].label}: ${blocked[0].reason}`;
  return blocked.length === 1 ? first : `${first} (+${blocked.length - 1} more)`;
}

function unsupportedFallbackReason(code: RestartUnsupportedCode | null): string {
  switch (code) {
    case "target_is_orchestrator":
      return "The loop runs inside this runner; restarting it would end the loop. Target a secondary instance.";
    case "target_not_runner_managed":
      return "The target runner was not started by this runner, so it cannot restart it.";
    case "rebuild_needs_dev_supervisor":
      return "Rebuild compiles the runner from a source checkout, which needs the dev supervisor.";
    default:
      return "This between-iterations mode cannot restart the target here.";
  }
}

const PATH_LABEL: Record<RestartPathKind, string> = {
  instance_manager: "restarts in-process (runner-managed instance)",
  dev_supervisor: "restarts through the dev supervisor (rebuild)",
  not_needed: "",
};

// --- Presentation ---

/**
 * Inline verdict under a "Between" select. Renders nothing for a mode that
 * never restarts; otherwise the refusal reason, the resolved path, the
 * pending state, or an explicit UNKNOWN.
 */
export function RestartCapabilityNotice({
  state,
  className,
  testId,
}: {
  state: RestartCapabilityState;
  className?: string;
  testId?: string;
}) {
  const base = cn("text-[0.7rem] leading-snug", className);
  if (state.status === "idle") return null;
  if (state.status === "checking") {
    return (
      <div className={cn(base, "text-muted-foreground")} data-testid={testId} role="status">
        <Loader2 className="w-3 h-3 inline mr-1 animate-spin" />
        Checking whether this runner can restart the target…
      </div>
    );
  }
  if (state.status === "unknown") {
    return (
      <div className={cn(base, "text-yellow-400")} data-testid={testId} role="status">
        Restart capability unknown ({state.error}); the runner re-checks when the loop starts.
      </div>
    );
  }
  const cap = state.capability;
  if (!cap.supported) {
    return (
      <div
        className={cn(base, "text-red-400")}
        data-testid={testId}
        data-restart-code={cap.code ?? ""}
        role="alert"
      >
        <AlertTriangle className="w-3 h-3 inline mr-1" />
        Unsupported here: {startBlockedReason(state)}
      </div>
    );
  }
  const label = cap.path ? PATH_LABEL[cap.path] : "";
  if (!label) return null;
  return (
    <div className={cn(base, "text-muted-foreground")} data-testid={testId}>
      Target :{cap.targetPort} {label}
    </div>
  );
}

/**
 * Start button gated on the capability verdict. When blocked it is disabled,
 * carries the reason as its `title`, and the reason is also rendered as
 * visible text beside it — never a silent grey button.
 */
export function GatedStartButton({
  blockedReason,
  extraDisabled = false,
  onClick,
  label,
  icon,
  className,
}: {
  blockedReason: string | null;
  extraDisabled?: boolean;
  onClick: () => void;
  label: string;
  icon?: ReactNode;
  className?: string;
}) {
  const blocked = blockedReason !== null;
  return (
    <span className="inline-flex items-center gap-2">
      {blocked && (
        <span className="text-[0.7rem] text-red-400 max-w-[260px] truncate" title={blockedReason}>
          Start blocked: {blockedReason}
        </span>
      )}
      <button
        onClick={onClick}
        disabled={blocked || extraDisabled}
        title={blocked ? `Unsupported here: ${blockedReason}` : undefined}
        className={cn(
          "px-2.5 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 disabled:opacity-40",
          className,
        )}
      >
        {icon}
        {label}
      </button>
    </span>
  );
}
