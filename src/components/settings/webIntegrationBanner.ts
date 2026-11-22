/**
 * Pure decision for the Backend Connection status banner.
 *
 * Every state the banner can show is named here and each is derived only from
 * fields the runner actually reports. There is deliberately no "Registering"
 * state: `WebIntegrationStatus` carries no in-flight-registration field, so a
 * spinner claiming one would not observe the state it names.
 */

/** The subset of `get_web_integration_status` the banner decision reads. */
export interface BannerStatusInput {
  enabled: boolean;
  runnerId: string | null;
  registrationError: string | null;
  wsConnected: boolean;
}

export type StatusBannerState =
  /** Integration disabled: no banner. */
  | { kind: "none" }
  | { kind: "connected"; runnerId: string }
  /** Registered earlier, socket dropped, no error recorded: relay is retrying. */
  | { kind: "reconnecting" }
  /** A registration error is recorded (takes precedence over reconnecting). */
  | { kind: "failed"; error: string }
  /** Enabled but never paired: no runner id, no error, no socket. */
  | { kind: "not-connected" }
  /** Any combination no named state covers (e.g. socket up with no runner id). */
  | { kind: "unknown" };

export function deriveStatusBanner(status: BannerStatusInput | null): StatusBannerState {
  if (!status?.enabled) return { kind: "none" };
  if (status.wsConnected && status.runnerId) {
    return { kind: "connected", runnerId: status.runnerId };
  }
  if (status.registrationError) {
    return { kind: "failed", error: status.registrationError };
  }
  if (status.runnerId && !status.wsConnected) return { kind: "reconnecting" };
  if (!status.runnerId && !status.wsConnected) return { kind: "not-connected" };
  return { kind: "unknown" };
}
