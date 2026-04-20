/**
 * Shared types for the connection wizard (Plan 3).
 */

export type ConnectionType = "usb" | "lan" | "remote";
export type Platform = "android" | "ios";

export interface DiscoveredDevice {
  deviceId: string;
  platform: Platform;
  model?: string;
  // For LAN: the address the runner reached it on (e.g. "192.168.1.42:8087")
  address?: string;
  // For USB: the ADB serial (same as deviceId, kept for clarity)
  adbSerial?: string;
}

export interface TunnelConfig {
  serverAddr: string;
  token: string;
}

export interface WizardElementPreview {
  id: string;
  label: string;
  type: string;
}

export interface WizardState {
  step: 0 | 1 | 2 | 3 | 4;
  connectionType: ConnectionType | null;
  platform: Platform | null;
  deviceId: string | null;
  /** The local proxy URL the runner exposes once the transport is established. */
  proxyUrl: string | null;
  /** Remote path only. */
  tunnelConfig: TunnelConfig | null;
  /** Populated by Step 4 on success. */
  elements: WizardElementPreview[] | null;
  error: string | null;
}

export const INITIAL_STATE: WizardState = {
  step: 0,
  connectionType: null,
  platform: null,
  deviceId: null,
  proxyUrl: null,
  tunnelConfig: null,
  elements: null,
  error: null,
};

export const STEP_LABELS = ["Type", "Device", "App setup", "Verify", "Done"] as const;
