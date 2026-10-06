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
 */

export const FLEET_PICKER_TENANT_SELECT_ID = "terminal.fleet-picker.tenant";

/** The `<select>` value standing for "no tenant sent — the runner decides". */
export const FLEET_TENANT_DEFAULT_VALUE = "";

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
  /** The selected tenant, or null for the runner's own default. */
  selected: string | null;
  onChange: (tenantId: string | null) => void;
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
      title="Which tenant's fleet to read. coord shows only the sessions of the tenant whose credential this runner presents, so a device bound to several tenants reads one at a time. Affects this view only — it does not change the default tenant for new sessions."
      className={className}
    >
      <option value={FLEET_TENANT_DEFAULT_VALUE}>device default</option>
      {options.map((o) => (
        <option key={o.value} value={o.value} title={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}
