/**
 * TenantContext — plan 2026-05-22-coord-native-session-coordination
 * §D12 + §Phase 4.
 *
 * Resolves this machine's DEFAULT tenant for NEW sessions and persists the
 * operator's choice in `~/.qontinui/machine.json::active_tenant_id`. The
 * on-disk key keeps its historical name; the in-process API does not, because
 * "active tenant" reads as "the runner's tenant" and no such thing exists on a
 * device bound to more than one.
 * Wraps the `get_active_tenant` / `set_active_tenant` Tauri commands
 * added in this PR (`commands/tenant.rs`).
 *
 * Conditional rendering: when the operator belongs to exactly one tenant
 * (`candidates.length <= 1`), the switcher does NOT render — Phase 4's
 * D12 is explicit that the single-tenant case shows no UI. When the
 * operator belongs to multiple tenants, the runner header (and other
 * consumers) read `useTenant().showSwitcher` and render the switcher
 * accordingly.
 *
 * The `candidates` list is POPULATED, and needs no coord round-trip:
 * `commands/tenant.rs:193` fills it from `pair::read_paired_binding_tenant_ids()`
 * (`pair.rs:551`), a read of this machine's own `paired_user.json` — which
 * `commands/tenant.rs:174-175` states outright, "a local file read, NO coord
 * round-trip, so the switcher renders offline". This doc used to claim the list
 * "ships empty in Phase 4 — Phase 5 dashboard populates it from a coord
 * round-trip"; that outlived the code, and a comment understating a shipped
 * capability made per-tenant UI work look blocked on an unshipped dependency
 * (corrected by plan
 * `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`,
 * P4). An EMPTY list is UNKNOWN about this device's bindings and never evidence
 * of a single tenant — see the field's own doc below.
 *
 * Plan §D12 verbatim: "every session is stamped with its tenant at
 * start and keeps it for life — switching active tenant doesn't migrate
 * live sessions." This file ONLY tracks the *default* tenant for new
 * sessions; mutating it does not affect any running session.
 *
 * Wraps SessionProvider in the React tree:
 *
 *   <TenantProvider>
 *     <SessionProvider>
 *       {children}
 *     </SessionProvider>
 *   </TenantProvider>
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
import { listen } from "@tauri-apps/api/event";

import { createLogger } from "@/lib/logger";

const logger = createLogger("TenantContext");

interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string | null;
  data?: T | null;
}

/**
 * One bound tenant's credential state, as `get_active_tenant` serves it
 * (Rust `commands::tenant::CandidateCredential`, computed by
 * `auth::credential_state` — the rule pinned to agree with the credential
 * selector). A bound tenant can have no usable credential: never paired on this
 * device, or its slot expired. Its reads will 401, and a selector must say so.
 */
export interface TenantCandidateCredential {
  tenant: string;
  /** `true` acts, `false` cannot, `null` UNKNOWN (a store read failed). */
  can_act: boolean | null;
  /** The tenant's OWN slot: `usable` | `present-but-dead` | `absent` | `unreadable`. */
  slot: string;
  /** `can_act` rests on the legacy default slot — it works; no re-pair owed. */
  via_default_slot: boolean;
}

/**
 * The machine pin's posture, as `get_active_tenant` serves it (Rust
 * `commands::tenant::ActiveTenantView.pin`): `pinned` (an operator chose a
 * tenant), `unpinned` (readable, no pin — the paired DEFAULT binding applies,
 * which nobody chose), `unresolvable` (unreadable or malformed — the runner
 * refuses rather than guess).
 */
export type TenantPinPosture = "pinned" | "unpinned" | "unresolvable";

interface GetActiveTenantResponse {
  active_tenant_id: string | null;
  source: "machine.json" | "paired_user.json" | null;
  /** How `machine.json` classifies (`tenant_pin::TenantPin`). Absent from a
   * runner build that predates it — read as UNKNOWN, never as "unpinned". */
  pin?: TenantPinPosture;
  candidates: string[];
  /** One per candidate, in candidate order. Absent from a runner build that
   * predates it — read as UNKNOWN per tenant, never as "usable". */
  credentials?: TenantCandidateCredential[];
}

export interface TenantContextValue {
  /**
   * The device's DEFAULT tenant for NEW sessions — never "the tenant this
   * runner belongs to", a concept that does not exist once a device holds N
   * concurrent tenant bindings (`coord.tenant_devices`). A session stamps its
   * tenant at spawn and keeps it for life, so changing this re-points FUTURE
   * spawns only; it never migrates a running session.
   *
   * It is also not how artifact tenancy is MEANT to be resolved — that is a
   * per-artifact rule keyed on the artifact's own repo. Note the Rust side is
   * weaker than "never": in `session_archive::tenancy` this pin is the
   * documented LAST-RESORT rung, and an attribution derived from it is
   * labelled `derived_sole_binding`, never `declared`.
   *
   * Persisted as `machine.json::active_tenant_id`; the on-disk key keeps its
   * name because it is a file format read by independent Rust consumers.
   * `src-tauri/src/commands/tenant.rs`'s module doc is the authority for that
   * key's full meaning — it is the default for new sessions AND for
   * device-level surfaces (heartbeat, census, backstop, maintenance,
   * doctor, flag-poll). This TS
   * symbol is named for the only half the frontend uses.
   *
   * `null` while loading, or when the operator hasn't paired yet.
   */
  defaultTenantIdForNewSessions: string | null;
  /** Where the default tenant was resolved from. Diagnostic surface for
   * the settings panel. */
  source: "machine.json" | "paired_user.json" | null;
  /**
   * The machine pin's posture, or `null` while loading / against a runner build
   * that serves none — UNKNOWN, never "unpinned". The Fleet view opens on "all
   * tenants" only when this reads `unpinned` on a multi-bound device.
   */
  pin: TenantPinPosture | null;
  /**
   * Tenants this device is bound to, as raw UUID strings — there is no display
   * name on this wire, by design (resolving one needs a coord round-trip and a
   * widened type; `shortTenantId` in `SpawnTenantPicker` is the labelling
   * affordance consumers use).
   *
   * Populated from a LOCAL file, with no coord round-trip:
   * `commands/tenant.rs:193` → `pair::read_paired_binding_tenant_ids()`
   * (`pair.rs:551`) reads this machine's `paired_user.json`, so the list — and
   * every consumer gated on it — works offline. (This doc previously said
   * "Phase 4 ships empty; Phase 5 dashboard populates this via a coord
   * round-trip", which had not been true since that read landed.)
   *
   * An EMPTY list is UNKNOWN about the device's bindings, never evidence that it
   * is bound to exactly one tenant: `read_paired_binding_tenant_ids` returns an
   * empty vec for an absent or unreadable file and never an error, so the two
   * cases are indistinguishable from here. {@link showSwitcher} collapses them
   * because rendering no switcher is the right ACTION for both; a surface that
   * states something about the bindings must say unknown instead.
   */
  candidates: string[];
  /**
   * Per-candidate credential state (see {@link TenantCandidateCredential}).
   * Empty while loading or against a runner build that serves none — a
   * candidate with no entry is UNKNOWN, not usable.
   */
  credentials: TenantCandidateCredential[];
  /** True iff the runner UI should render a tenant switcher. Per D12,
   * single-tenant operators see no UI. */
  showSwitcher: boolean;
  /** Persist the operator's choice of DEFAULT tenant for new sessions to
   * machine.json. Async — best-effort local state update is immediate; the
   * Rust side is the source of truth on next mount. Running sessions are
   * unaffected. */
  setDefaultTenantForNewSessions: (tenantId: string) => Promise<void>;
  /** Re-pull from Rust. */
  refresh: () => Promise<void>;
  /**
   * Re-pull ONLY the per-candidate credential state. For a surface that shows
   * those notes beside a read it makes often: see {@link credentialsAfterReread}
   * for why this leaves the candidates and the pin alone.
   */
  refreshCredentials: () => Promise<void>;
  /**
   * F2 — the tenant the operator EXPLICITLY picked for the next spawn. `null`
   * means no pick: the spawn sends no tenant and Rust stamps the device default.
   * The picker still DISPLAYS its inferred / default tenant; only a pick is
   * published here (plan
   * 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential re-review
   * F1 — a sent tenant is admitted fail-closed, so an unpicked one must not be sent).
   *
   * This is deliberately SEPARATE from `defaultTenantIdForNewSessions`: that
   * is the persisted device default (written to machine.json), whereas this is a
   * transient, un-persisted, per-spawn choice made in the operator's picker. Setting it never writes machine.json
   * and — per D12 — never migrates a RUNNING session; a session's tenant is
   * stamped at spawn and immortal.
   *
   * Written by `SpawnTenantPicker` (the sole writer); read by the spawn
   * handlers on the terminal page.
   */
  spawnTenantId: string | null;
  /** Set the tenant for the next spawn. See {@link spawnTenantId}. */
  setSpawnTenantId: (tenantId: string | null) => void;
}

const TenantContext = createContext<TenantContextValue | null>(null);

interface TenantProviderProps {
  children: ReactNode;
}

/** Same notes, entry for entry: a re-read that changed nothing keeps the old array. */
function sameCredentials(a: TenantCandidateCredential[], b: TenantCandidateCredential[]): boolean {
  return (
    a.length === b.length &&
    a.every((x, i) => {
      const y = b[i];
      return (
        y !== undefined &&
        x.tenant === y.tenant &&
        x.can_act === y.can_act &&
        x.slot === y.slot &&
        x.via_default_slot === y.via_default_slot
      );
    })
  );
}

/**
 * The credential state to keep after a credentials-only re-read.
 *
 * `get_active_tenant` answers success with no candidates when `paired_user.json`
 * cannot be read, so an empty answer may be a transient read failure rather than
 * a device with no bindings. A credentials-only re-read keeps what it had: a real
 * loss of bindings arrives through `refresh`, on the `tenant-bindings-changed` the
 * heartbeat's binding reconcile emits when it drops one. A read that changed
 * nothing also keeps the old array, so the provider's value, and every consumer
 * of it, stays put.
 */
export function credentialsAfterReread(
  previous: TenantCandidateCredential[],
  read: TenantCandidateCredential[] | undefined,
): TenantCandidateCredential[] {
  if (!carriesNotes(read)) return previous;
  return sameCredentials(previous, read) ? previous : read;
}

/** A notes-only re-read carries notes worth applying: see {@link credentialsAfterReread}. */
export function carriesNotes(
  read: TenantCandidateCredential[] | undefined,
): read is TenantCandidateCredential[] {
  return read !== undefined && read.length > 0;
}

/** One read's place in a {@link latestReadGate}. */
export interface GatedRead {
  /** No read begun after this one has applied its answer yet. */
  fresh: () => boolean;
  /** Record that this read's answer was applied. */
  apply: () => void;
}

/**
 * Orders overlapping reads of one piece of state by the answers APPLIED: an
 * older answer that resolves after a newer one has landed is stale, but a newer
 * read that fails, or that applies nothing, does not make an older one stale.
 */
export function latestReadGate(): { begin: () => GatedRead } {
  let begun = 0;
  let applied = 0;
  return {
    begin: () => {
      const mine = ++begun;
      return {
        fresh: () => mine > applied,
        apply: () => {
          applied = Math.max(applied, mine);
        },
      };
    },
  };
}

/** Same tenant ids in the same order: a re-read that changed nothing keeps the old array. */
function sameIds(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

export function TenantProvider({ children }: TenantProviderProps) {
  const [defaultTenantIdForNewSessions, setDefaultTenantIdForNewSessions] = useState<string | null>(
    null,
  );
  const [source, setSource] = useState<TenantContextValue["source"]>(null);
  const [pin, setPin] = useState<TenantPinPosture | null>(null);
  const [candidates, setCandidates] = useState<string[]>([]);
  const [credentials, setCredentials] = useState<TenantCandidateCredential[]>([]);
  const [spawnTenantId, setSpawnTenantId] = useState<string | null>(null);
  // Two gates, because two kinds of read overlap: `refresh` writes the bindings
  // and the notes, `refreshCredentials` only the notes. A later notes-only read
  // must not discard an earlier full read's bindings.
  const [bindingReads] = useState(latestReadGate);
  const [credentialReads] = useState(latestReadGate);

  const refresh = useCallback(async () => {
    const bindingsRead = bindingReads.begin();
    const notesRead = credentialReads.begin();
    try {
      const resp = await invoke<CommandResponse<GetActiveTenantResponse>>("get_active_tenant");
      const data = resp?.data;
      if (bindingsRead.fresh()) {
        bindingsRead.apply();
        setDefaultTenantIdForNewSessions(data?.active_tenant_id ?? null);
        setSource(data?.source ?? null);
        setPin(data?.pin ?? null);
        const ids = data?.candidates ?? [];
        setCandidates((previous) => (sameIds(previous, ids) ? previous : ids));
      }
      if (notesRead.fresh()) {
        notesRead.apply();
        // Unlike a notes-only re-read, an empty answer here is taken as given.
        const read = data?.credentials ?? [];
        setCredentials((previous) => (sameCredentials(previous, read) ? previous : read));
      }
    } catch (e) {
      // Non-fatal — runner still works without tenant resolution,
      // sessions just go out without the default-tenant stamp until
      // pair completes.
      logger.warn(`get_active_tenant failed: ${e}`);
    }
  }, [bindingReads, credentialReads]);

  const refreshCredentials = useCallback(async () => {
    const notesRead = credentialReads.begin();
    try {
      const resp = await invoke<CommandResponse<GetActiveTenantResponse>>("get_active_tenant");
      const read = resp?.data?.credentials;
      // An answer carrying no notes applies nothing, so it leaves an older full
      // read free to land (see `credentialsAfterReread`).
      if (carriesNotes(read) && notesRead.fresh()) {
        notesRead.apply();
        setCredentials((previous) => credentialsAfterReread(previous, read));
      }
    } catch (e) {
      // Non-fatal: the notes stay as they were and the next read tries again.
      logger.warn(`get_active_tenant (credentials) failed: ${e}`);
    }
  }, [credentialReads]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial tenant load on mount; refresh is the explicit synchronization point with the backend
    void refresh();
  }, [refresh]);

  // A sign-in or re-pair ADDS a binding to `paired_user.json` and emits
  // `web-integration-changed`. Without a re-read, `candidates` (and so
  // `showSwitcher`) stayed as they were at mount, and the credential banner's
  // "Switch active tenant" action stayed hidden on a box that had just gained
  // a second tenant (plan 2026-09-14-credential-posture-third-residuals).
  useEffect(() => {
    const unlisten = listen("web-integration-changed", () => {
      void refresh();
    });
    return () => {
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, [refresh]);

  // Rust announces every in-process change to the bindings themselves on
  // `tenant-bindings-changed` (`TENANT_BINDINGS_CHANGED_EVENT`): a sign-in, a
  // pair-code redeem through Settings or the UI Bridge, and the heartbeat's
  // reconcile dropping one. A redeem emits no `web-integration-changed`, and the
  // reconcile must not, because that event's other listeners treat it as a sign-in.
  useEffect(() => {
    const unlisten = listen("tenant-bindings-changed", () => {
      void refresh();
    });
    // A change announced between the mount read and this registration (the
    // startup heal runs beside window creation) would be lost, so read once
    // more when the listener is live.
    unlisten
      .then(() => refresh())
      .catch(() => {
        /* no event bus: the mount read stands */
      });
    return () => {
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, [refresh]);

  const setDefaultTenantForNewSessions = useCallback(
    async (tenantId: string): Promise<void> => {
      const trimmed = tenantId.trim();
      if (!trimmed) {
        throw new Error("tenant id cannot be empty");
      }
      try {
        await invoke<CommandResponse<{ active_tenant_id: string; source: string }>>(
          "set_active_tenant",
          { tenantId: trimmed },
        );
      } catch (e) {
        throw new Error(String(e), { cause: e });
      }
      // Optimistic update; refresh reconciles.
      setDefaultTenantIdForNewSessions(trimmed);
      setSource("machine.json");
      void refresh();
    },
    [refresh],
  );

  const showSwitcher = candidates.length > 1;

  const value = useMemo<TenantContextValue>(
    () => ({
      defaultTenantIdForNewSessions,
      source,
      pin,
      candidates,
      credentials,
      showSwitcher,
      setDefaultTenantForNewSessions,
      refresh,
      refreshCredentials,
      spawnTenantId,
      setSpawnTenantId,
    }),
    [
      defaultTenantIdForNewSessions,
      source,
      pin,
      candidates,
      credentials,
      showSwitcher,
      setDefaultTenantForNewSessions,
      refresh,
      refreshCredentials,
      spawnTenantId,
    ],
  );

  return <TenantContext.Provider value={value}>{children}</TenantContext.Provider>;
}

/**
 * Hook into the default-tenant resolver. Throws when called outside a
 * `TenantProvider`.
 */
export function useTenant(): TenantContextValue {
  const ctx = useContext(TenantContext);
  if (!ctx) {
    throw new Error("useTenant must be used within a TenantProvider");
  }
  return ctx;
}
