import type { FleetSession, FleetSessionsResponse } from "./useFleetSessions";
import {
  EMPTY_FLEET_WALK,
  FLEET_CURSOR_STALLED_MESSAGE,
  fleetCursorStalled,
  fleetErrorCode,
  fleetErrorInvalidatesCursor,
  fleetErrorIsRestart,
  fleetErrorMessage,
  fleetWalkAccept,
  fleetWalkDropCursor,
  normalizeFleetCursor,
  type FleetErrorCode,
  type FleetScope,
  type FleetServerFilter,
  type FleetWalk,
  type FleetWalkMode,
} from "./fleetDiscovery";

/**
 * ONE tenant's cursor walk over coord's `GET /coord/sessions/fleet`, as a plain
 * object rather than a React hook (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 4).
 *
 * ## Why a class, and why per tenant
 *
 * coord fingerprints the TENANT into its page cursor (`scope_fingerprint`), and
 * its route takes no tenant argument — "read tenant X" is spelled "present
 * tenant X's credential". A view over every bound tenant is therefore N
 * independent walks, each with its own cursor, its own generation and its own
 * failure; a single walk with a widened scope cannot exist. A hook cannot be
 * called N times for a run-time N, so the walk lives here and
 * `useFleetSessions` holds one walker per requested tenant. N=1 is the same
 * object, so the single-tenant view and the merged one cannot drift apart.
 *
 * It is also what makes the walk testable in this repo's node-only vitest: the
 * fetch is injected, so every rule below is exercised against a fake coord
 * instead of being pinned by grepping the hook's source.
 *
 * ## The rules (unchanged from the hook it was lifted out of)
 *
 * - A read either RESTARTS (no cursor, accumulation replaced) or ADVANCES
 *   (`nextCursor` re-sent verbatim, page appended).
 * - A monotonic generation stamps every request; a response whose generation
 *   was superseded — or that lands after {@link FleetTenantWalker.dispose} — is
 *   DISCARDED rather than merged, so a page from a previous scope never lands
 *   in the new scope's list.
 * - `cursor_scope_mismatch` on a request that CARRIED a cursor restarts the
 *   walk and shows nothing: the scope moved under a page in flight, which is
 *   ordinary use. On a request that carried none it cannot be a stale cursor,
 *   so it is reported like any other refusal instead of restarting for ever.
 * - A cursor coord refuses as unusable, or hands back unchanged, is DROPPED:
 *   the rows stay, and the snapshot says why the list may be incomplete.
 * - A failed page keeps the rows already accumulated — a failure must not
 *   silently empty a list the operator is reading.
 */

/** What one page request carries — the `fleet_sessions_list` command's args. */
export interface FleetPageRequest {
  deviceId: string | null;
  state: string | null;
  includeClosed: boolean;
  limit: number;
  cursor: string | null;
  /** The tenant whose credential the read presents; `null` lets the runner's
   * own authority order choose. */
  tenant: string | null;
}

/** One page from coord, or a rejection carrying the command's error string. */
export type FleetPageFetcher = (req: FleetPageRequest) => Promise<FleetSessionsResponse>;

/** Everything a consumer may read about one walk, replaced wholesale on change. */
export interface FleetWalkSnapshot {
  /** The tenant this walk ASKED for (`null` = the runner's authority order). */
  requestedTenant: string | null;
  /** Rows accumulated since the last restart, each stamped with `servedTenantId`. */
  walk: FleetWalk;
  /** The last successful page's envelope, or null. */
  response: FleetSessionsResponse | null;
  /** A page-ONE read is in flight. */
  loading: boolean;
  /** A SUBSEQUENT page is in flight — the rows on screen stay valid. */
  loadingMore: boolean;
  error: string | null;
  errorCode: FleetErrorCode | null;
  /** `error` describes a walk that cannot ADVANCE, not a read that FAILED. */
  walkStalled: boolean;
  /** At least one page was accepted since this walker was created. */
  loaded: boolean;
  /** The query `response` was actually served for. */
  appliedQuery: FleetServerFilter | null;
}

export function initialFleetWalkSnapshot(requestedTenant: string | null): FleetWalkSnapshot {
  return {
    requestedTenant,
    walk: EMPTY_FLEET_WALK,
    response: null,
    loading: false,
    loadingMore: false,
    error: null,
    errorCode: null,
    walkStalled: false,
    loaded: false,
    appliedQuery: null,
  };
}

/**
 * The tenant a page's rows were SERVED under: the envelope's own `tenantId`,
 * else the tenant the read asked for, else null.
 *
 * The envelope leads because it is coord's statement about the principal it
 * scoped the rows to. The requested tenant is the fallback because it names
 * the credential that was presented — coord can only have scoped the rows to
 * that principal — and it matters exactly where the envelope is blank, which
 * `fleet_sessions_list` cannot rule out (it hands coord's body back as untyped
 * JSON).
 */
export function servedTenantOf(
  response: Pick<FleetSessionsResponse, "tenantId"> | null,
  requested: string | null,
): string | null {
  const served = (response?.tenantId ?? "").trim();
  if (served.length > 0) return served;
  const asked = (requested ?? "").trim();
  return asked.length > 0 ? asked : null;
}

/**
 * Stamp every row with the tenant its page was served under. Attach and create
 * mint their grant under THIS tenant — coord resolves a target within the
 * presented principal's tenant only, so in a merged view a row minted under
 * any other tenant (the selection, the default slot) is a 404.
 */
export function stampServedTenant(
  response: FleetSessionsResponse,
  requested: string | null,
): FleetSessionsResponse {
  const servedTenantId = servedTenantOf(response, requested);
  const sessions: FleetSession[] = (response.sessions ?? []).map((s) => ({ ...s, servedTenantId }));
  return { ...response, sessions };
}

export interface FleetWalkerOptions {
  /** The walk's scope, including the tenant it asks for. Fixed for the life of
   * the walker: a scope change is a NEW walker. */
  scope: FleetScope;
  fetchPage: FleetPageFetcher;
  /** The page size as of the CALL (a resize must not restart the walk). */
  pageSize: () => number;
  /** Every snapshot change, synchronously. */
  onChange: (snapshot: FleetWalkSnapshot) => void;
  /** The rows of each ACCEPTED page, after `onChange` — the catalogue feed. */
  onRows?: (rows: FleetSession[]) => void;
  /**
   * The previous scope's snapshot for this SAME tenant, when there was one. Its
   * rows stay on screen while the first page of the new scope is read — a
   * filter change must not blank the list mid-read, and a failed restart must
   * keep "the previous read" rather than empty it. Ignored when its tenant is
   * not this walker's. No cursor is carried: a cursor is valid only in the
   * scope that minted it.
   */
  seed?: FleetWalkSnapshot;
}

export class FleetTenantWalker {
  private readonly opts: FleetWalkerOptions;
  private snap: FleetWalkSnapshot;
  /** The cursor the next page must carry. */
  private cursor: string | null = null;
  /** Monotonic request id; only the newest request may write. */
  private generation = 0;
  private disposed = false;

  constructor(opts: FleetWalkerOptions) {
    this.opts = opts;
    const tenant = opts.scope.tenantId;
    const seed = opts.seed?.requestedTenant === tenant ? opts.seed : undefined;
    this.snap = seed
      ? {
          ...initialFleetWalkSnapshot(tenant),
          walk: seed.walk,
          response: seed.response,
          loaded: seed.loaded,
          appliedQuery: seed.appliedQuery,
        }
      : initialFleetWalkSnapshot(tenant);
  }

  get snapshot(): FleetWalkSnapshot {
    return this.snap;
  }

  /** Whether `loadMore` would issue a request. */
  get canAdvance(): boolean {
    return !this.disposed && this.cursor !== null;
  }

  /** Walk again from coord's first page, with no cursor. */
  restart(): Promise<void> {
    return this.fetch("restart");
  }

  /** Fetch the next page with the cursor. A no-op when there is none. */
  loadMore(): Promise<void> {
    return this.fetch("more");
  }

  /** Stop writing. Any response still in flight is discarded. */
  dispose(): void {
    this.disposed = true;
    this.generation += 1;
  }

  private isCurrent(generation: number): boolean {
    return !this.disposed && this.generation === generation;
  }

  private update(patch: Partial<FleetWalkSnapshot>): void {
    this.snap = { ...this.snap, ...patch };
    this.opts.onChange(this.snap);
  }

  private async fetch(mode: FleetWalkMode): Promise<void> {
    if (this.disposed) return;
    const limit = this.opts.pageSize();
    const cursor = mode === "more" ? this.cursor : null;
    // Nothing to walk. Not an error and not a read: coord said this was the
    // last page, and asking again with no cursor would silently restart.
    if (mode === "more" && cursor === null) return;
    this.generation += 1;
    const generation = this.generation;
    if (mode === "restart") this.cursor = null;
    this.update({
      loading: mode === "restart",
      loadingMore: mode === "more",
      error: null,
      errorCode: null,
      walkStalled: false,
    });
    const { deviceId, state, includeClosed, tenantId } = this.opts.scope;
    try {
      // The command returns coord's body directly and rejects on transport or
      // non-2xx, so a thrown value is the honest failure — including 401/403,
      // which means "no usable credential for this tenant", NOT "empty".
      const raw = await this.opts.fetchPage({
        deviceId,
        state,
        includeClosed,
        limit,
        cursor,
        tenant: tenantId,
      });
      if (!this.isCurrent(generation)) return;

      const next = normalizeFleetCursor(raw.nextCursor);
      // A keyset cursor encodes the page just served, so coord handing back the
      // cursor it was GIVEN means the parameter never reached it. Stop rather
      // than re-serve page one for ever, and say so.
      const stalled = fleetCursorStalled(cursor, next);
      this.cursor = stalled ? null : next;

      const result = stampServedTenant(raw, tenantId);
      const accepted = fleetWalkAccept(this.snap.walk, result, mode);
      this.update({
        walk: stalled ? fleetWalkDropCursor(accepted) : accepted,
        response: result,
        loaded: true,
        loading: false,
        loadingMore: false,
        // Recorded in the SAME update as the response it belongs to, so no
        // consumer can pair these rows with a filter set they were not served
        // for. coord's OWN effective, post-clamp page size where it served one.
        appliedQuery: {
          deviceId,
          state,
          includeClosed,
          limit: typeof raw.limit === "number" && raw.limit > 0 ? raw.limit : limit,
        },
        // coord ANSWERED — the rows are current; only the next page is out of
        // reach, and the snapshot says that rather than borrowing a failure.
        ...(stalled ? { error: FLEET_CURSOR_STALLED_MESSAGE, walkStalled: true } : {}),
      });
      this.opts.onRows?.(result.sessions);
    } catch (err) {
      if (!this.isCurrent(generation)) return;
      const code = fleetErrorCode(err);
      if (fleetErrorIsRestart(code) && cursor !== null) {
        // The scope moved under a page already in flight. Ordinary use — walk
        // again from page one and show the operator nothing.
        this.cursor = null;
        void this.fetch("restart");
        return;
      }
      const patch: Partial<FleetWalkSnapshot> = {
        loading: false,
        loadingMore: false,
        errorCode: code,
        error: fleetErrorMessage(code, err),
      };
      if (mode === "restart") {
        // A failed RESTART keeps the previous read's rows (seeded, or the
        // walk's own), but their cursor belonged to a walk that no longer
        // exists: offering "load more" over them would offer a click that
        // returns at once. Dropped, so the list reads as incomplete-and-
        // unreachable beside the error rather than as reachable.
        patch.walk = fleetWalkDropCursor(this.snap.walk);
      }
      if (fleetErrorInvalidatesCursor(code)) {
        // Keep the pages already accumulated, but stop offering a control that
        // can only fail again — the message is what stops that from reading as
        // a complete list.
        this.cursor = null;
        patch.walk = fleetWalkDropCursor(this.snap.walk);
      }
      this.update(patch);
    }
  }
}
