import type { TenantCandidateCredential } from "@/contexts/TenantContext";
import { shortTenantId } from "./SpawnTenantPicker";

/**
 * The Fleet view's tenant selector (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 3).
 *
 * coord's fleet route scopes every row to the tenant of the credential it is
 * shown, so a device bound to N tenants sees ONE of them per read. Before this
 * control it saw whichever the default slot named, with no way to change it.
 *
 * The choice is VIEW-LOCAL. It is never written through `set_active_tenant`,
 * which would move the default for new sessions and for unpinned live sessions
 * — reading another tenant's fleet must not re-point anything else.
 *
 * Rendered only when the device is bound to more than one tenant
 * (`useTenant().showSwitcher`): at N=1 there is nothing to choose and nothing
 * new is shown.
 *
 * Phase 4 adds "all tenants": one independent walk per bound tenant, merged
 * ({@link FLEET_TENANT_ALL_VALUE}); it is the opening choice on an UNPINNED
 * multi-bound device ({@link defaultFleetTenantChoice}).
 */

export const FLEET_PICKER_TENANT_SELECT_ID = "terminal.fleet-picker.tenant";

/** The `<select>` value standing for "no tenant sent — the runner decides". */
export const FLEET_TENANT_DEFAULT_VALUE = "";

/**
 * The `<select>` value — and the view's choice — standing for "every bound
 * tenant, merged" (Phase 4). `*` can never collide with a tenant uuid. It is
 * also what the picker projects as `data-fleet-tenant-requested`, so a UI
 * Bridge driver asserts the merged view by that one value.
 */
export const FLEET_TENANT_ALL_VALUE = "*";

/**
 * The view's tenant choice: a tenant uuid (read that one), `null` (send none —
 * the runner's own authority order picks), or {@link FLEET_TENANT_ALL_VALUE}
 * (one walk per bound tenant, merged).
 */
export type FleetTenantChoice = string | null;

export function isAllTenants(choice: FleetTenantChoice): boolean {
  return choice === FLEET_TENANT_ALL_VALUE;
}

/**
 * The choice the view OPENS on, before the operator picks anything.
 *
 * "All tenants" only on a device that is UNPINNED and bound to more than one
 * tenant: there the "device default" is merely whichever binding the default
 * credential slot names — nobody chose it — so opening on it shows a fraction
 * of the device's own fleet as if it were the whole (the 2026-09-28 report).
 * A PINNED device opens on its pin, which `TenantPin`'s contract already
 * settles: someone chose that tenant. `unresolvable`, an absent `pin` (a
 * runner build that serves none) and N≤1 all keep Phase 3's default — no
 * tenant sent — so a single-tenant device behaves exactly as before.
 */
export function defaultFleetTenantChoice(
  pin: string | null,
  candidates: readonly string[],
): FleetTenantChoice {
  return pin === "unpinned" && candidates.length > 1 ? FLEET_TENANT_ALL_VALUE : null;
}

/**
 * The tenants to walk for a choice — one independent cursor walk each, because
 * coord fingerprints the tenant into its cursor. `null` in the result means
 * "send no tenant". "All tenants" with no known candidates (a list still
 * loading, or an unreadable `paired_user.json`) degrades to the runner's own
 * default rather than to zero walks: an empty walk set would render as an
 * empty fleet.
 */
export function fleetWalkTenants(
  choice: FleetTenantChoice,
  candidates: readonly string[],
): (string | null)[] {
  if (!isAllTenants(choice)) return [choice];
  return candidates.length > 0 ? [...candidates] : [null];
}

/**
 * Why a bound tenant's read is expected to fail, or `null` when its credential
 * is usable. Such a tenant stays SELECTABLE: hiding it would hide the 401 the
 * operator needs to see to fix it.
 *
 * A candidate with no credential entry (a runner build that serves none) is
 * "unknown" — never assumed usable.
 */
export function fleetTenantCredentialNote(
  credential: TenantCandidateCredential | undefined,
): string | null {
  if (!credential) return "unknown";
  if (credential.can_act === true) return null;
  if (credential.can_act === null) return "unknown";
  switch (credential.slot) {
    case "absent":
      return "pair this tenant";
    case "present-but-dead":
      return "credential expired";
    default:
      return "unknown";
  }
}

export interface FleetTenantOption {
  /** The tenant uuid sent as the read's `tenant`. */
  value: string;
  label: string;
  /** The credential note, or null when the credential is usable. */
  note: string | null;
}

/** One option per bound tenant, in the order the runner lists them. */
export function fleetTenantOptions(
  candidates: string[],
  credentials: TenantCandidateCredential[],
): FleetTenantOption[] {
  return candidates.map((tenant) => {
    const note = fleetTenantCredentialNote(credentials.find((c) => c.tenant === tenant));
    const short = shortTenantId(tenant);
    return { value: tenant, label: note ? `${short} — ${note}` : short, note };
  });
}

interface FleetTenantSelectProps {
  /** `useTenant().showSwitcher` — false renders nothing. */
  showSwitcher: boolean;
  candidates: string[];
  credentials: TenantCandidateCredential[];
  /** The selected tenant, `null` for the runner's own default, or
   * {@link FLEET_TENANT_ALL_VALUE} for every bound tenant. */
  selected: FleetTenantChoice;
  onChange: (choice: FleetTenantChoice) => void;
  className?: string;
}

export function FleetTenantSelect({
  showSwitcher,
  candidates,
  credentials,
  selected,
  onChange,
  className,
}: FleetTenantSelectProps) {
  if (!showSwitcher) return null;
  const options = fleetTenantOptions(candidates, credentials);
  return (
    <select
      data-ui-bridge-id={FLEET_PICKER_TENANT_SELECT_ID}
      aria-label="Fleet tenant"
      value={selected ?? FLEET_TENANT_DEFAULT_VALUE}
      onChange={(e) =>
        onChange(e.target.value === FLEET_TENANT_DEFAULT_VALUE ? null : e.target.value)
      }
      title="Which tenant's fleet to read. coord shows only the sessions of the tenant whose credential this runner presents, so “all tenants” reads each bound tenant separately and merges the rows — each row keeps the tenant it was served under. Affects this view only — it does not change the default tenant for new sessions."
      className={className}
    >
      <option value={FLEET_TENANT_ALL_VALUE} title="One read per bound tenant, merged">
        all tenants ({candidates.length})
      </option>
      <option value={FLEET_TENANT_DEFAULT_VALUE}>device default</option>
      {options.map((o) => (
        <option key={o.value} value={o.value} title={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}
