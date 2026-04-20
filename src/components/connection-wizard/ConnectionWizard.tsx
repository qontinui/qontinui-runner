/**
 * ConnectionWizard.tsx
 *
 * Plan 3 modal wrapper. Owns the step state machine; each step is a
 * presentational component that receives the current `WizardState` and a
 * small setter API. The wizard renders one step at a time and uses the
 * existing setup-wizard `StepIndicator`.
 */

import { useState, useCallback } from "react";
import { X } from "lucide-react";

import { StepIndicator } from "../setup-wizard/StepIndicator";
import { Button } from "../ui/Button";

import {
  INITIAL_STATE,
  STEP_LABELS,
  type WizardState,
  type ConnectionType,
  type Platform,
  type DiscoveredDevice,
  type TunnelConfig,
  type WizardElementPreview,
} from "./types";

import { ConnectionTypeStep } from "./ConnectionTypeStep";
import { DeviceDiscoveryStep } from "./DeviceDiscoveryStep";
import { MobileAppGuideStep } from "./MobileAppGuideStep";
import { VerificationStep } from "./VerificationStep";
import { CompleteStep } from "./CompleteStep";

// ---------------------------------------------------------------------------
// Setter API passed to each step
// ---------------------------------------------------------------------------

export interface WizardApi {
  state: WizardState;
  setConnectionType: (t: ConnectionType) => void;
  setPlatform: (p: Platform) => void;
  setDevice: (d: DiscoveredDevice, proxyUrl: string | null) => void;
  setTunnelConfig: (cfg: TunnelConfig) => void;
  setElements: (els: WizardElementPreview[]) => void;
  setError: (msg: string | null) => void;
  next: () => void;
  back: () => void;
  reset: () => void;
}

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

interface ConnectionWizardProps {
  open: boolean;
  onClose: () => void;
  /** Called once when a device reaches verified state. */
  onConnected?: (deviceId: string, proxyUrl: string) => void;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function ConnectionWizard({ open, onClose, onConnected }: ConnectionWizardProps) {
  const [state, setState] = useState<WizardState>(INITIAL_STATE);

  const api: WizardApi = {
    state,
    setConnectionType: (t) => setState((s) => ({ ...s, connectionType: t, error: null })),
    setPlatform: (p) => setState((s) => ({ ...s, platform: p })),
    setDevice: (d, proxyUrl) =>
      setState((s) => ({
        ...s,
        deviceId: d.deviceId,
        platform: d.platform,
        proxyUrl,
      })),
    setTunnelConfig: (cfg) => setState((s) => ({ ...s, tunnelConfig: cfg })),
    setElements: (els) => setState((s) => ({ ...s, elements: els })),
    setError: (msg) => setState((s) => ({ ...s, error: msg })),
    next: () =>
      setState((s) => ({
        ...s,
        step: Math.min(4, s.step + 1) as WizardState["step"],
        error: null,
      })),
    back: () =>
      setState((s) => ({
        ...s,
        step: Math.max(0, s.step - 1) as WizardState["step"],
        error: null,
      })),
    reset: () => setState(INITIAL_STATE),
  };

  const handleClose = useCallback(() => {
    setState(INITIAL_STATE);
    onClose();
  }, [onClose]);

  if (!open) return null;

  // USB path skips the app-setup step (ADB forwarding needs no SDK changes).
  const usbSkipsGuide = state.connectionType === "usb" && state.step === 2;
  if (usbSkipsGuide) {
    // Auto-advance on render; safer than useEffect for immediate progression.
    queueMicrotask(() => api.next());
  }

  const renderStep = () => {
    switch (state.step) {
      case 0:
        return <ConnectionTypeStep api={api} />;
      case 1:
        return <DeviceDiscoveryStep api={api} />;
      case 2:
        return <MobileAppGuideStep api={api} />;
      case 3:
        return (
          <VerificationStep
            api={api}
            onConnected={() => {
              if (state.deviceId && state.proxyUrl) {
                onConnected?.(state.deviceId, state.proxyUrl);
              }
            }}
          />
        );
      case 4:
        return <CompleteStep api={api} onClose={handleClose} />;
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      role="dialog"
      aria-modal="true"
      aria-label="Connect a physical device"
    >
      <div className="relative w-full max-w-2xl mx-4 bg-zinc-900 border border-zinc-700 rounded-xl shadow-2xl">
        <button
          className="absolute top-3 right-3 text-zinc-400 hover:text-foreground"
          aria-label="Close wizard"
          onClick={handleClose}
        >
          <X className="w-5 h-5" />
        </button>

        <div className="p-6">
          <h2 className="text-lg font-semibold text-foreground mb-6">Connect a physical device</h2>

          <StepIndicator steps={[...STEP_LABELS]} currentStep={state.step} />

          <div className="min-h-[280px]">{renderStep()}</div>

          {state.error && (
            <div className="mt-4 p-3 rounded-md bg-destructive/10 border border-destructive/30 text-sm text-destructive">
              {state.error}
            </div>
          )}

          {state.step < 4 && (
            <div className="flex items-center justify-between mt-6 pt-4 border-t border-zinc-800">
              <Button variant="ghost" onClick={api.back} disabled={state.step === 0}>
                Back
              </Button>
              <Button variant="ghost" onClick={handleClose}>
                Cancel
              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
