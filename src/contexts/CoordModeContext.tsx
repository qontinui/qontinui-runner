/**
 * CoordModeContext — plan
 * `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4.
 *
 * The runner ships in two shapes:
 *
 *   - **connected** — the operator has a qontinui account, so a coord
 *     deployment is configured. Fleet-backed surfaces (fleet health, agent
 *     spawn, cross-agent worktree overlap) have something real behind them.
 *   - **isolated** — the runner is a standalone app with no coord
 *     configured. Plans are `.md` files on disk; the fleet-backed surfaces
 *     have nothing behind them at all.
 *
 * Until now the UI could not tell those apart. The only seam was
 * `get_coord_http_base`, which belongs to a resolver family that ALWAYS
 * returns a string — an isolated runner gets `http://localhost:9870` back,
 * indistinguishable from "coord genuinely lives there". So fleet-backed
 * panels rendered as if live, the operator clicked, and nothing happened.
 *
 * `get_coord_mode` is the honest seam: it reports the mode alongside the
 * arm of the resolution chain that decided it.
 *
 * **The mode is derived from CONFIGURATION, never from reachability.** There
 * is deliberately no health probe, no ping and no retry-based "are we
 * online?" heuristic in this file. A network blip must not flip the UI into
 * isolated mode — an unreachable coord is a *connected* runner having a bad
 * minute, and it already has its own retriable error banners. Adding a probe
 * here would make the UI's shape depend on packet loss, which is the exact
 * unpredictability the plan's UX ranking rejects.
 *
 * **An unresolvable mode is UNKNOWN, and unknown leaves surfaces ENABLED.**
 * The `get_coord_mode` command lands on a separate branch; on a runner build
 * that predates it the invoke rejects. A wrongly-DISABLED panel on a
 * connected runner removes a working feature, which is strictly worse than a
 * wrongly-enabled one on an isolated runner (where the underlying call
 * simply fails and the panel shows its normal error). So the gate closes
 * only on a POSITIVE `mode: "isolated"`.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { createLogger } from "@/lib/logger";

const logger = createLogger("CoordModeContext");

// ---------------------------------------------------------------------------
// Wire shape — settled and test-pinned on the Rust side.
// ---------------------------------------------------------------------------

/** The two shapes a runner can be in. */
export type CoordModeName = "connected" | "isolated";

/**
 * Payload of the `get_coord_mode` Tauri command.
 *
 * `base` is the connected coord base, and is `null` iff
 * `mode === "isolated"`. `source` names the arm of the resolution chain that
 * decided the mode and is present in BOTH modes — it is what makes the
 * isolated reason actionable rather than a shrug.
 */
export interface CoordMode {
  mode: CoordModeName;
  base: string | null;
  source: string;
}

/**
 * `source` values this UI distinguishes.
 *
 * - `dev_localhost_fallback` — nothing is configured. The operator has no
 *   qontinui account bound to this runner; connecting one is the fix.
 * - `unknown_tier_prod_default` — the runner's `settings.json` could not be
 *   read, so its tier is UNKNOWN and the resolver fell back to the
 *   production default. That is a DIFFERENT problem with a different fix
 *   (repair that file) and must never be reported as "you have no account".
 *
 * Other values (`env`, `profile`, `tier_default`, …) accompany
 * `mode: "connected"` and need no special handling here.
 */
export const COORD_SOURCE_NO_ACCOUNT = "dev_localhost_fallback";
export const COORD_SOURCE_SETTINGS_UNREADABLE = "unknown_tier_prod_default";

/**
 * Tauri event emitted when the browser loopback callback applies new web
 * integration settings — i.e. a Cognito sign-in completed. Mirrors
 * `WEB_INTEGRATION_CHANGED_EVENT` in `commands/web_integration.rs`.
 */
const WEB_INTEGRATION_CHANGED_EVENT = "web-integration-changed";

// ---------------------------------------------------------------------------
// Fetch-once singleton
// ---------------------------------------------------------------------------

let inFlight: Promise<CoordMode> | null = null;

/**
 * Invoke `get_coord_mode` at most once per app lifetime and share the result
 * with every caller.
 *
 * The mode is config-derived, so N consumers must NOT mean N invokes. On
 * rejection the cache is cleared so a retry is possible — a cached rejection
 * would pin the app in `unknown` for the rest of the session even after the
 * operator fixed the cause.
 *
 * The rejection handler clears the slot **only if it still owns it**. Without
 * that ownership check a slow first call that is superseded by
 * {@link resetCoordModeCache} (which the refresh path uses) would, on
 * rejecting later, null out the slot holding its own SUCCESSOR — breaking
 * at-most-once and letting a third invoke start.
 */
export function fetchCoordModeOnce(): Promise<CoordMode> {
  if (!inFlight) {
    const p: Promise<CoordMode> = invoke<CoordMode>("get_coord_mode").catch((err: unknown) => {
      if (inFlight === p) {
        inFlight = null;
      }
      throw err;
    });
    inFlight = p;
  }
  return inFlight;
}

/** Drop the shared promise so the next call re-invokes. */
export function resetCoordModeCache(): void {
  inFlight = null;
}

// ---------------------------------------------------------------------------
// Derivation — pure, so it is testable under the runner's `environment:
// "node"` vitest config (no jsdom; see FleetHealthPanel.test.tsx for the same
// constraint).
// ---------------------------------------------------------------------------

/**
 * What the UI actually branches on. `unknown` covers both "still loading"
 * and "the invoke rejected" — in neither case have we POSITIVELY learned
 * that coord is unconfigured, so neither closes the gate.
 */
export type CoordAvailability = CoordModeName | "unknown";

/**
 * Map a resolved mode (or its absence) onto the availability the UI branches
 * on. `null` — not yet loaded, or the invoke rejected — is `unknown`.
 */
export function deriveCoordAvailability(data: CoordMode | null): CoordAvailability {
  return data ? data.mode : "unknown";
}

/** The gate a coord-backed surface reads. */
export interface CoordGating {
  /**
   * Whether the surface should be interactive. True for `connected` AND for
   * `unknown` — see the file header on why unknown fails open.
   */
  enabled: boolean;
  /** True only on a POSITIVE `mode: "isolated"`. Drives the disabled copy. */
  isolated: boolean;
  availability: CoordAvailability;
  /** The resolution-chain arm, when known. Selects the isolated message. */
  source: string | null;
}

/** Derive the gate from the resolved mode (or its absence). Pure. */
export function deriveCoordGating(data: CoordMode | null): CoordGating {
  const availability = deriveCoordAvailability(data);
  const isolated = availability === "isolated";
  return {
    enabled: !isolated,
    isolated,
    availability,
    source: data?.source ?? null,
  };
}

/**
 * Whether a coord-backed surface should issue its network call YET.
 *
 * The gate itself fails open on `unknown`, and that is right for INTERACTIVITY
 * — a wrongly-disabled panel on a connected runner removes a working feature.
 * It is not right for EGRESS. `unknown` covers the mount-to-first-answer
 * window, which every gated panel passes through on every app start, so an
 * isolated runner still fired one request per gated surface at a coord that
 * does not exist — and then started an interval doing it again. That is the
 * same waste §6.4 gates the steady state to avoid, just bounded to startup.
 *
 * So the two questions get two answers: controls stay ENABLED while the mode
 * resolves (fail open), and the requests WAIT for it (fail closed). The wait
 * is one already-in-flight `get_coord_mode` invoke, shared process-wide by
 * {@link fetchCoordModeOnce}, and it ends on rejection too — a runner build
 * that predates the command settles to `unknown` and polls exactly as before.
 *
 * Pure, so each panel's poll decision is testable under the runner's
 * `environment: "node"` vitest config.
 */
export function coordCallsReady(coord: { isolated: boolean; loading: boolean }): boolean {
  return !coord.isolated && !coord.loading;
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

export interface CoordModeContextValue extends CoordGating {
  /** The connected coord base; `null` in isolated mode and while unknown. */
  base: string | null;
  /** True until the first `get_coord_mode` settles. */
  loading: boolean;
  /** The invoke rejection, when the mode could not be resolved. */
  error: string | null;
  /** Re-invoke `get_coord_mode`, bypassing the shared cache. */
  refresh: () => Promise<void>;
}

/**
 * Value handed to consumers rendered outside a provider. Fails OPEN for the
 * same reason the `unknown` availability does.
 */
const FALLBACK: CoordModeContextValue = {
  enabled: true,
  isolated: false,
  availability: "unknown",
  source: null,
  base: null,
  loading: false,
  error: null,
  refresh: async () => {},
};

const CoordModeContext = createContext<CoordModeContextValue | null>(null);

export function CoordModeProvider({ children }: { children: ReactNode }) {
  const [data, setData] = useState<CoordMode | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  /**
   * Monotonic generation. A `load()` only writes state if it is still the
   * NEWEST load — otherwise a slow in-flight call that rejects after a
   * successful refresh would discard the refresh's result and put the app
   * back into `unknown`.
   */
  const generation = useRef(0);

  const load = useCallback(async (force: boolean) => {
    if (force) {
      resetCoordModeCache();
    }
    generation.current += 1;
    const mine = generation.current;
    setLoading(true);
    try {
      const fresh = await fetchCoordModeOnce();
      if (mine !== generation.current) return;
      setData(fresh);
      setError(null);
    } catch (err) {
      // The command does not exist on runner builds that predate the
      // backend half of §6.4. That is UNKNOWN, not isolated — the gate
      // stays open and the surfaces keep working exactly as before.
      const msg = err instanceof Error ? err.message : String(err);
      if (mine !== generation.current) return;
      setData(null);
      setError(msg);
      logger.debug("get_coord_mode unavailable; coord surfaces stay enabled", { msg });
    } finally {
      if (mine === generation.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot mode resolution on mount; `load` is the explicit synchronization point with the backend
    void load(false);
  }, [load]);

  const refresh = useCallback(async () => {
    await load(true);
  }, [load]);

  /**
   * Re-resolve the mode whenever the runner's account binding changes.
   *
   * Without this the isolated notice's own promise — "connect an account and
   * this surface enables itself" — would be a lie: the mode is cached for the
   * app's lifetime, so an operator who signed in from Settings → Account came
   * back to panels still disabled and still blaming a missing account. The
   * BACKEND would answer `connected` on the very next call (`coord_base_policy`
   * re-reads settings.json every time); only this cache held it stale.
   *
   * Two signals, because the two sign-in paths announce differently:
   *
   *  - `runner-tier-changed` (window event) is the fleet-wide "the tier or the
   *    account binding just moved" nudge, dispatched by `LoginScreen`,
   *    `AccountSettings` (Cognito sign-in AND sign-out), `WebIntegrationSettings`,
   *    `TierStep` and `GithubRepoPicker`.
   *  - `web-integration-changed` (Tauri event, `WEB_INTEGRATION_CHANGED_EVENT`
   *    in `commands/web_integration.rs`) fires from the browser loopback
   *    callback. `AccountSettings` re-fires it as `runner-tier-changed`, but
   *    only while that panel is mounted — subscribing here directly means the
   *    refresh happens wherever the operator is standing.
   *
   * Sign-OUT is covered by the same listener, so a runner that drops to
   * isolated starts telling the truth immediately too.
   */
  useEffect(() => {
    const onTierChanged = () => {
      void refresh();
    };
    window.addEventListener("runner-tier-changed", onTierChanged);

    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    // Tauri may be absent (tests, a plain browser preview) — tolerate it.
    listen(WEB_INTEGRATION_CHANGED_EVENT, onTierChanged)
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      window.removeEventListener("runner-tier-changed", onTierChanged);
      unlisten?.();
    };
  }, [refresh]);

  const value = useMemo<CoordModeContextValue>(() => {
    const gating = deriveCoordGating(data);
    return {
      ...gating,
      base: data?.base ?? null,
      loading,
      error,
      refresh,
    };
  }, [data, error, loading, refresh]);

  return <CoordModeContext.Provider value={value}>{children}</CoordModeContext.Provider>;
}

/**
 * Read the runner's coord mode.
 *
 * Outside a {@link CoordModeProvider} this returns the fail-open fallback
 * (`availability: "unknown"`, `enabled: true`) rather than throwing: a
 * missing provider must never blank a working panel, and a panel rendered in
 * isolation (Storybook, a pop-out window) should behave exactly as it did
 * before this gate existed.
 */
export function useCoordMode(): CoordModeContextValue {
  return useContext(CoordModeContext) ?? FALLBACK;
}
