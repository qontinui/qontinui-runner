/**
 * Pure helpers for how the CI-runner settings panel asks for the runner's coord
 * device JWT (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential
 * P3, review R2). Kept apart from `CiRunnerSettings.tsx` so the decisions are
 * unit-testable in the runner's node vitest environment.
 */

/** Shown when the runner holds no device credential at all. */
export const UNPAIRED_ERROR =
  "Pair this runner before enabling CI — no device credential. " +
  "Sign in / pair under Settings → Account, then try again.";

/**
 * Thrown when the token lookup came back empty; `message` is rendered verbatim
 * (no "Failed to …" prefix) because it already names the fix.
 */
export class UnpairedError extends Error {}

/**
 * The args for `get_coord_device_token`. A tenant is named ONLY on a runner that
 * holds more than one tenant (`candidates.length > 1`): there the command
 * refuses a tenant-less call, so the panel must say which tenant the CI runner
 * registers under. On a single-tenant runner the tenant is omitted and the
 * command answers exactly as before — sending `machine.json`'s default there
 * would add a failure mode (a stale or unvalidated default naming a tenant the
 * runner holds nothing for) with nothing to gain.
 */
export function deviceTokenArgs(
  candidates: readonly string[],
  defaultTenantId: string | null,
): { tenantId?: string } {
  if (candidates.length > 1 && defaultTenantId) {
    return { tenantId: defaultTenantId };
  }
  return {};
}

/**
 * Turn the command's answer into an `Authorization` value, or throw the
 * {@link UnpairedError} that names what is missing. A `null` for a tenant-less
 * call means the runner holds no device credential; a `null` for a NAMED tenant
 * means only that it holds none usable for THAT tenant — which is a different
 * fix, so it gets a different message.
 */
export function bearerFromDeviceToken(
  token: string | null | undefined,
  args: { tenantId?: string },
): string {
  if (token) {
    return `Bearer ${token}`;
  }
  if (args.tenantId) {
    throw new UnpairedError(
      `This runner holds no usable coord credential for tenant ${args.tenantId}. ` +
        "Pair this device for that tenant, or switch the default tenant under Settings → Account.",
    );
  }
  throw new UnpairedError(UNPAIRED_ERROR);
}
