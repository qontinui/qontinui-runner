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
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";

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

// ---------------------------------------------------------------------------
// Fetch-once singleton
// ---------------------------------------------------------------------------

let inFlight: Promise<CoordMode> | null = null;

/**
 * Invoke `get_coord_mode` at most once per app lifetime and share the result
 * with every caller.
 *
 * The mode is config-derived and cannot change without a restart or an
 * explicit account action, so N consumers must NOT mean N invokes. On
 * rejection the cache is cleared so an explicit `refresh()` can retry — a
 * cached rejection would pin the app in `unknown` for the rest of the
 * session even after the operator fixed the cause.
 */
export function fetchCoordModeOnce(): Promise<CoordMode> {
  if (!inFlight) {
    inFlight = invoke<CoordMode>("get_coord_mode").catch((err: unknown) => {
      inFlight = null;
      throw err;
    });
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

/** Raw provider state, split out so the derivations below stay pure. */
export interface CoordModeSnapshot {
  data: CoordMode | null;
  error: string | null;
  loading: boolean;
}

/** Map a raw snapshot onto the availability the UI branches on. */
export function deriveCoordAvailability(snapshot: CoordModeSnapshot): CoordAvailability {
  if (snapshot.data) {
    return snapshot.data.mode;
  }
  return "unknown";
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

/** Derive the gate from a raw snapshot. Pure. */
export function deriveCoordGating(snapshot: CoordModeSnapshot): CoordGating {
  const availability = deriveCoordAvailability(snapshot);
  const isolated = availability === "isolated";
  return {
    enabled: !isolated,
    isolated,
    availability,
    source: snapshot.data?.source ?? null,
  };
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

  const load = useCallback(async (force: boolean) => {
    if (force) {
      resetCoordModeCache();
    }
    setLoading(true);
    try {
      const fresh = await fetchCoordModeOnce();
      setData(fresh);
      setError(null);
    } catch (err) {
      // The command does not exist on runner builds that predate the
      // backend half of §6.4. That is UNKNOWN, not isolated — the gate
      // stays open and the surfaces keep working exactly as before.
      const msg = err instanceof Error ? err.message : String(err);
      setData(null);
      setError(msg);
      logger.debug("get_coord_mode unavailable; coord surfaces stay enabled", { msg });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot mode resolution on mount; `load` is the explicit synchronization point with the backend
    void load(false);
  }, [load]);

  const refresh = useCallback(async () => {
    await load(true);
  }, [load]);

  const value = useMemo<CoordModeContextValue>(() => {
    const gating = deriveCoordGating({ data, error, loading });
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
