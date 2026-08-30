import { useCallback, useEffect, useState } from "react";
import { Bot } from "lucide-react";
import { resolvePort } from "@/lib/runner-api";

/**
 * One-click start/stop for each steward session the runner can launch
 * (plan `2026-07-19-runner-steward-button-and-pr-shepherd-retirement.md`,
 * generalized off the single hardcoded merge-train steward 2026-08-28).
 *
 * Rendered in `SessionManagerPanel.tsx`, which IS mounted in the live
 * component tree (`TerminalPage.tsx` — `{workflowGen.showSidebar &&
 * <SessionManagerPanel .../>}`). An earlier iteration placed this control in
 * `LaunchMenu.tsx`, which compiles and has tests but is not actually rendered
 * anywhere in the app (only self-referenced by its own test file) — moved
 * here so a human operator can actually see and click it.
 *
 * The backend (`mcp/steward.rs`) spawns the PTY server-side and emits the
 * same `terminal-created` event any other server-created terminal does, so
 * once `POST /steward/{kind}/start` returns, the new tab appears via the
 * existing `useTerminalManager` ingest listener — no separate assign/focus
 * plumbing needed here.
 *
 * **The roster comes from the backend, never from this file.** `GET /stewards`
 * returns one row per launchable steward *with its live status*, so adding a
 * steward is a `STEWARDS` row in `mcp/steward.rs` and this component picks it
 * up with no change. One poll covers every row rather than fanning out a
 * request per kind.
 *
 * Self-contained: owns its own fetch/poll state rather than threading new
 * props through `SessionManagerHeader` (a purely presentational component
 * driven entirely by `useSessionManager` today) — mirrors how
 * `WaitForReleasePanel` in `LaunchMenu.tsx` is a self-contained subcomponent
 * with its own effects.
 */

/** Polling interval while the panel is mounted (ms). */
export const STEWARD_POLL_INTERVAL_MS = 2000;

/** One element of `GET /stewards`, and the shape of `GET /steward/{kind}/status`. */
export interface StewardStatus {
  /** Stable identifier and URL path segment (`merge-train`, `dev-ops`, …). */
  kind: string;
  /** Human-readable label, rendered verbatim. */
  label: string;
  /** The slash-command this launches, without the leading slash. */
  skill: string;
  default_mode: string;
  default_interval: string;
  running: boolean;
  session_id?: string;
  mode?: string;
  interval?: string;
  started_at?: number;
}

interface StewardApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/** The roster endpoint: every steward plus its live status, in one request. */
export function buildStewardsUrl(port: number): string {
  return `http://127.0.0.1:${port}/stewards`;
}

/**
 * Per-steward action endpoint. `kind` is the backend's own `kind` field —
 * never the skill name, which is a different string (`dev-ops` vs
 * `dev-ops-steward`).
 */
export function buildStewardUrl(
  port: number,
  kind: string,
  path: "status" | "start" | "stop",
): string {
  return `http://127.0.0.1:${port}/steward/${kind}/${path}`;
}

/**
 * The reason a non-2xx response carries, falling back to its status code.
 *
 * Shared by the action path and the roster poll so both surface the backend's
 * own message. That message is load-bearing: `POST /steward/{kind}/start`
 * answers 400 with the character it refused, 409 with the terminal already
 * running, and — from the unattended-spawn resource floor — a refusal quoting
 * lane/headroom/floor. Only the status code survives if the body is not JSON.
 */
export async function readErrorDetail(resp: {
  status: number;
  json: () => Promise<unknown>;
}): Promise<string> {
  try {
    const json = (await resp.json()) as StewardApiResponse<unknown>;
    if (json.error) return json.error;
  } catch {
    // Non-JSON body — keep the status-code fallback.
  }
  return `HTTP ${resp.status}`;
}

/**
 * Coarse "how long has this been up" for a running steward, from the
 * `started_at` the roster already returns.
 *
 * Returns `null` rather than a placeholder whenever the input cannot support a
 * claim: a missing/zero timestamp (`TerminalInfo::created_at` defaults to 0),
 * a non-finite one, or a future one. A steward's whole purpose is to run
 * unattended, so "has this actually been alive, or did it restart a minute
 * ago?" is the operator's first question — and an uptime that silently reads
 * `0m` for an absent timestamp answers it wrongly.
 */
export function formatUptime(startedAtMs: number | undefined, nowMs: number): string | null {
  if (typeof startedAtMs !== "number" || !Number.isFinite(startedAtMs) || startedAtMs <= 0) {
    return null;
  }
  const elapsedMs = nowMs - startedAtMs;
  // Clock skew between the runner's `created_at` and the renderer can put a
  // just-started steward slightly in the future; treat that as "just now"
  // rather than refusing to render.
  if (elapsedMs < 0) return "0m";

  const minutes = Math.floor(elapsedMs / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/**
 * The detail line for a running steward: what it was launched as, and for how
 * long.
 *
 * `GET /stewards` has always returned `interval` and `started_at` alongside
 * `mode`, but the running row rendered `mode` alone — so the cadence an
 * operator can read on the *stopped* row (`default_mode · default_interval`)
 * disappeared the moment the steward started, which is when it matters. Every
 * part is omitted when absent rather than rendered as a blank, so a runner
 * that reports less still produces a well-formed line.
 */
export function formatRunningSummary(steward: StewardStatus, nowMs: number): string {
  const uptime = formatUptime(steward.started_at, nowMs);
  return [steward.mode, steward.interval, uptime ? `up ${uptime}` : undefined]
    .filter((part): part is string => Boolean(part))
    .join(" · ");
}

export function StewardControl() {
  const [stewards, setStewards] = useState<StewardStatus[]>([]);
  const [busyKinds, setBusyKinds] = useState<ReadonlySet<string>>(() => new Set());
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  /** Why the roster is not current, or `null` while it is. */
  const [rosterError, setRosterError] = useState<string | null>(null);

  const fetchStatus = useCallback(async (): Promise<void> => {
    try {
      const port = resolvePort();
      const resp = await fetch(buildStewardsUrl(port));
      // `fetch` does not throw on a non-2xx. The action path was fixed to read
      // the error body; this path still discarded it AND left the previous
      // "reachable" verdict standing, so a 500 from the roster endpoint made
      // the whole control render `null` — silently absent, indistinguishable
      // from a runner with no stewards. Every non-success arm now names itself.
      if (!resp.ok) {
        setRosterError(await readErrorDetail(resp));
        return;
      }
      const json = (await resp.json()) as StewardApiResponse<StewardStatus[]>;
      if (json.success && json.data) {
        setStewards(json.data);
        setRosterError(null);
      } else {
        setRosterError(json.error ?? "runner returned no roster");
      }
    } catch {
      // Network error — leave the existing roster in place so the control
      // doesn't flicker when the runner is briefly busy, but stop claiming
      // the roster is current. An empty roster that renders nothing at all
      // is indistinguishable from "this runner has no stewards", so the
      // unreachable case gets its own line below.
      setRosterError("runner API unreachable");
    }
  }, []);

  // The state-updating call is only ever invoked from a timer callback,
  // never synchronously in the effect body (avoids
  // react-hooks/set-state-in-effect). The zero-delay `setTimeout` gives an
  // effectively-immediate first fetch.
  useEffect(() => {
    const tick = () => void fetchStatus();
    const immediate = setTimeout(tick, 0);
    const interval = setInterval(tick, STEWARD_POLL_INTERVAL_MS);
    return () => {
      clearTimeout(immediate);
      clearInterval(interval);
    };
  }, [fetchStatus]);

  const act = async (kind: string, path: "start" | "stop"): Promise<void> => {
    setBusyKinds((prev) => new Set(prev).add(kind));
    setErrors((prev) => {
      if (!(kind in prev)) return prev;
      const next = { ...prev };
      delete next[kind];
      return next;
    });
    try {
      const port = resolvePort();
      const resp = await fetch(buildStewardUrl(port, kind, path), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
      });
      // `fetch` does not throw on a non-2xx, so without this the button
      // would fail silently. The backend deliberately puts its reason in the
      // body — a 400 (a mode/interval the launcher refused to type into a
      // shell), a 409 (already running) and, more importantly, the
      // unattended-spawn resource-floor refusal that carries lane/headroom/
      // floor detail. Discarding that leaves an operator with a button that
      // does nothing and says nothing.
      if (!resp.ok) {
        const detail = await readErrorDetail(resp);
        setErrors((prev) => ({ ...prev, [kind]: detail }));
      }
    } catch (e) {
      setErrors((prev) => ({
        ...prev,
        [kind]: e instanceof Error ? e.message : "request failed",
      }));
    } finally {
      // Clear only THIS kind's flag. A single `busyKind` string was cleared
      // by whichever request finished last, which re-enabled a row whose own
      // request was still in flight.
      setBusyKinds((prev) => {
        if (!prev.has(kind)) return prev;
        const next = new Set(prev);
        next.delete(kind);
        return next;
      });
      void fetchStatus();
    }
  };

  if (stewards.length === 0) {
    // Render nothing only when the roster is genuinely empty AND the runner
    // answered successfully, so we know that. Vanishing on any failure would
    // hide the control exactly when something is wrong.
    if (rosterError === null) return null;
    return (
      <div
        className="px-3 py-1.5 border-b border-[#2a2d3d] bg-[#13141f] text-[11px] text-[#565f89]"
        data-testid="steward-unreachable"
      >
        Stewards unavailable — {rosterError}
      </div>
    );
  }

  return (
    <div className="border-b border-[#2a2d3d] bg-[#13141f]" data-testid="steward-control">
      {stewards.map((steward) => {
        const busy = busyKinds.has(steward.kind);
        const error = errors[steward.kind];
        // Read the clock per render rather than holding it in state: the poll
        // above replaces `stewards` every STEWARD_POLL_INTERVAL_MS with a new
        // array, so this component already re-renders on that cadence and the
        // uptime advances with it. A second timer would only add a re-render.
        const summary = formatRunningSummary(steward, Date.now());
        return (
          <div key={steward.kind} data-testid={`steward-row-${steward.kind}`}>
            <div className="flex items-center gap-2 px-3 py-1.5">
              {steward.running ? (
                <>
                  <span
                    data-testid={`steward-running-dot-${steward.kind}`}
                    className="w-2 h-2 rounded-full bg-[#9ece6a] shrink-0"
                  />
                  <span
                    className="flex-1 text-[11px] text-[#c0caf5]"
                    data-testid={`steward-status-label-${steward.kind}`}
                  >
                    {steward.label}: Running
                    {summary && (
                      <span
                        className="ml-1.5 text-[10px] text-[#565f89]"
                        data-testid={`steward-running-summary-${steward.kind}`}
                      >
                        ({summary})
                      </span>
                    )}
                  </span>
                  <button
                    type="button"
                    data-testid={`steward-stop-button-${steward.kind}`}
                    disabled={busy}
                    onClick={() => void act(steward.kind, "stop")}
                    className="px-1.5 py-0.5 rounded text-[10px] font-mono bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 transition-colors disabled:opacity-50 shrink-0"
                  >
                    Stop
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  data-testid={`steward-start-button-${steward.kind}`}
                  disabled={busy}
                  onClick={() => void act(steward.kind, "start")}
                  className="w-full flex items-center gap-2 text-[11px] text-[#c0caf5] hover:text-[#7aa2f7] transition-colors disabled:opacity-50"
                >
                  <Bot className="w-3.5 h-3.5 text-[#565f89]" />
                  <span className="flex-1 text-left">Start {steward.label.toLowerCase()}</span>
                  <span className="text-[10px] text-[#565f89] shrink-0">
                    {steward.default_mode} · {steward.default_interval}
                  </span>
                </button>
              )}
            </div>
            {error && (
              <div
                className="px-3 pb-1.5 text-[10px] text-[#f7768e]"
                data-testid={`steward-error-${steward.kind}`}
              >
                {error}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
