/**
 * The termhost-bench setup-adapter interface (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3) and the
 * registry of adapters. One adapter drives one terminal host; Phases 4 and 5 add
 * `vscode`, `windows-terminal` and the `tmux-wsl` variant behind the same calls.
 *
 * The run loop drives one UNIT — one host launch for one (run index, N, setup,
 * variant, build role) — through these methods, in this order:
 *
 *   check()                      doctor facts, no side effects:
 *                                  -> {mechanisms: {<id>: {state, reason}}}
 *   launch(ctx)                  start the host (private instance, D5/D12):
 *                                  -> {ok: true} (throws {@link AdapterRefusal} or Error)
 *   spawnSessions(n, argv)       start n sessions running the D13 argv:
 *                                  -> {sessions: [{hostId, tCmdNs}], admission: null | {...}}
 *   arrange(arm, actx)           realise "focus" (V = 1) or "tiled" (V ~ min(N, 9)):
 *                                  -> {ok, vTarget, focusSession?, switchable?, paneSizes?, failure?}
 *   show(k)                      show session k (the focus arm's switch):
 *                                  -> {ok, tCmdNs}
 *   spawnOne()                   one more session, shown the setup's own way:
 *                                  -> {ok, hostId, tCmdNs, admission?}
 *   closeOne(hostId?)            close that session (default: the last spawnOne)
 *   focusObserved()              keyboard focus on the shown session's pane:
 *                                  -> {ok, session?, failure?: {reason, detail}}
 *   processRoots()               PIDs the probe roots the host process tree at
 *                                  (always passed: without live roots the host
 *                                  group is unknown, never 0)
 *   describe()                   the results file's `setup` object
 *   teardown()                   close everything it started, with read-back:
 *                                  -> {closed: [...], survivors: [...], incidents: [...]}
 *
 * Optional extensions every Phase 3 adapter also implements:
 *   windowTarget()               how the probe finds the window: {pid} | {hwnd} | {title}
 *   diagnostics()                runner-only diagnostics (every one non-comparable, D1)
 *   hostIdKey                    the env var the host exports into each session
 *                                  (`slot-<k>.host_terminal_id[<key>]`), so the run
 *                                  loop maps slots to host sessions exactly
 *
 * Rules every adapter keeps:
 *   - sessions start from an argv array; the only string is the command line the
 *     host types into, or hands to, the session's shell (`rundir.mjs` `commandLine`);
 *   - everything written lives under the run directory (`rundir.mjs` `assertInside`);
 *   - teardown acts only on what this adapter started — by handle, by private
 *     socket, by the instance it spawned, or by a PID checked against the process
 *     table (`proc.mjs` `terminatePid`) — never by image name.
 */

/** The interface methods the plan lists; every adapter has every one. */
export const REQUIRED_METHODS = Object.freeze([
  "check",
  "launch",
  "spawnSessions",
  "arrange",
  "show",
  "spawnOne",
  "closeOne",
  "focusObserved",
  "processRoots",
  "describe",
  "teardown",
]);

/** The optional extensions the run loop uses when present. */
export const OPTIONAL_METHODS = Object.freeze(["windowTarget", "diagnostics"]);

/**
 * A launch the adapter refused on purpose, with the catalog reason the unit's
 * rows carry (e.g. `device_drained`, `admission_refused`, `harness_error`).
 */
export class AdapterRefusal extends Error {
  /**
   * @param {string} reason a kpi-catalog REASON_CODES key
   * @param {string} detail
   * @param {{incident?: object}} [extra]
   */
  constructor(reason, detail, extra = {}) {
    super(detail);
    this.name = "AdapterRefusal";
    this.reason = reason;
    this.detail = detail;
    this.incident = extra.incident ?? null;
  }
}

/** Throw unless `adapter` implements the interface. */
export function assertAdapter(adapter) {
  const missing = REQUIRED_METHODS.filter((m) => typeof adapter?.[m] !== "function");
  if (missing.length > 0) {
    throw new TypeError(`adapter ${adapter?.id ?? "?"} lacks ${missing.join(", ")}`);
  }
  if (typeof adapter.id !== "string") throw new TypeError("an adapter has a string id");
  return adapter;
}

/**
 * Order sessions row-major by their markers' positions — rows clustered by y with a
 * tolerance of half the median marker height (markers sit at row 0 of each pane, so
 * panes on one grid row share a y up to rounding), then by x. The zone order of the
 * runner's tiled layouts (`useZoneLayout.ts`) is row-major, so a session's rank here
 * is its zone index among the visible zones.
 *
 * @param {Map<number, {x: number, y: number, w?: number, h?: number}>} sessions
 * @returns {{order: number[], rows: number[][]}}
 */
export function rowMajorOrder(sessions) {
  const items = [...sessions.entries()].map(([session, r]) => ({ session, ...r }));
  if (items.length === 0) return { order: [], rows: [] };
  const heights = items.map((i) => i.h ?? 0).filter((h) => h > 0).sort((a, b) => a - b);
  const median = heights.length > 0 ? heights[Math.floor(heights.length / 2)] : 16;
  const tolerance = Math.max(4, median / 2);
  items.sort((a, b) => a.y - b.y || a.x - b.x);
  const rows = [];
  for (const it of items) {
    const row = rows[rows.length - 1];
    if (row && Math.abs(it.y - row.y) <= tolerance) row.items.push(it);
    else rows.push({ y: it.y, items: [it] });
  }
  const out = rows.map((r) => r.items.sort((a, b) => a.x - b.x).map((i) => i.session));
  return { order: out.flat(), rows: out };
}

/**
 * Create the adapter for one unit.
 *
 * @param {string} setupId
 * @param {object} options passed to the adapter's constructor
 */
export async function createAdapter(setupId, options) {
  switch (setupId) {
    case "runner": {
      const { RunnerAdapter } = await import("./runner.mjs");
      return assertAdapter(new RunnerAdapter(options));
    }
    case "tmux": {
      const { TmuxAdapter } = await import("./tmux.mjs");
      return assertAdapter(new TmuxAdapter(options));
    }
    case "vscode":
    case "windows-terminal":
    case "tmux-wsl":
      throw new RangeError(
        `the ${setupId} adapter arrives with ${setupId === "vscode" ? "Phase 4" : "Phase 5"} of the plan`,
      );
    default:
      throw new RangeError(`unknown setup "${setupId}"`);
  }
}
