/**
 * Step 4: connect the runner to the selected device and read its UI.
 *
 * Every request goes through the runner, never from this webview to the device.
 * A phone's UI Bridge (`@qontinui/ui-bridge-native` 0.11.0+) refuses any browser
 * caller, and off a cable it admits only a runner that signs each request with
 * the token the phone issued it.
 *
 * Stages:
 *   1. Transport established. USB runs /ui-bridge/apps/forward-device (iOS:
 *      /ui-bridge/ios/forward), which also fetches the phone's pairing token over
 *      the cable, so no code is ever needed there. LAN finds the device's row in
 *      /ui-bridge/devices, or registers it. Remote uses the tunnel from Step 2.
 *   2. Runner connected. A LAN row connects with POST /ui-bridge/devices/{id}/connect,
 *      anything else with POST /ui-bridge/sdk/connect. A phone that requires
 *      pairing asks here for the 6-digit code its Settings shows after Pair is
 *      tapped, then the check runs again.
 *   3. UI elements readable, read with GET /ui-bridge/sdk/snapshot on the runner.
 */

import { useEffect, useRef, useState } from "react";
import { Check, Loader2, XCircle, RefreshCw } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

import { Button } from "../ui/Button";
import type { WizardApi } from "./ConnectionWizard";
import type { WizardElementPreview } from "./types";
import { describeThrown } from "@/lib/utils";

type StageState = "pending" | "running" | "ok" | "fail";

interface Stage {
  id: string;
  label: string;
  state: StageState;
  detail?: string;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
  code?: string;
}

/** What fetching the pairing token over a fresh forward came to. */
interface LocalPairing {
  status: "paired" | "not_gated" | "not_local" | "not_cable" | "unreachable" | "failed";
  uiBridgeDeviceId?: string;
  code?: string;
}

interface ForwardDeviceResponse {
  localPort: number;
  deviceId: string;
  remotePort: number;
  pairing?: LocalPairing;
}

interface SdkConnectResponse {
  app: {
    appName: string;
    capabilities: string[];
  };
  url: string;
  authPath: string;
}

interface DeviceConnectResponse {
  deviceId: string;
  proxyUrl: string;
  authPath: string;
}

interface RegisteredDevice {
  info: { id: string };
}

// Shape returned by /ui-bridge/sdk/snapshot (the connected app's own
// /control/snapshot). The snapshot is the canonical element source across
// runner and mobile SDK. Real payload is richer; we pluck label/type/id for the
// wizard's preview.
interface SnapshotBody {
  success?: boolean;
  error?: string;
  data?: {
    elements?: Array<{ id: string; label?: string; type?: string }>;
  };
  elements?: Array<{ id: string; label?: string; type?: string }>;
}

/** A phone that asked for a pairing code, by its row in /ui-bridge/devices. */
interface PendingPairing {
  registryId: string;
  message: string;
}

const PAIRING_CODE = /^\d{6}$/;

function forwardPairingDetail(pairing: LocalPairing | undefined): string {
  switch (pairing?.status) {
    case "paired":
      return " · paired over the cable";
    case "not_local":
      return " · the phone asks for a pairing code even over this cable";
    case "failed":
      return ` · pairing token not fetched (${pairing.code ?? "unknown"})`;
    default:
      return "";
  }
}

/** Whether /ui-bridge/devices has a row under `id`. */
async function deviceRowExists(id: string): Promise<boolean> {
  const resp = await tracedFetch(`${getApiBase()}/ui-bridge/devices/${encodeURIComponent(id)}`);
  if (!resp.ok) return false;
  const body: ApiResponse<RegisteredDevice> = await resp.json();
  return body.success;
}

/** The row in /ui-bridge/devices for a LAN device, registering it when absent. */
async function findOrRegisterLanDevice(deviceId: string | null, proxyUrl: string): Promise<string> {
  // An mDNS pick is registered by the runner under the phone's own id.
  if (deviceId && !deviceId.startsWith("manual-") && (await deviceRowExists(deviceId))) {
    return deviceId;
  }
  // A manual address the runner already registered, on an earlier run of this
  // check, is under register-lan's own id for it: reuse that row and its proxy
  // rather than starting another.
  const host = new URL(proxyUrl).hostname;
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(host)) {
    const lanId = `lan-${host.replace(/\./g, "-")}`;
    if (await deviceRowExists(lanId)) return lanId;
  }
  const resp = await tracedFetch(`${getApiBase()}/ui-bridge/devices/register-lan`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ address: new URL(proxyUrl).host }),
  });
  const body: ApiResponse<RegisteredDevice> = await resp.json();
  if (!body.success || !body.data) {
    throw new Error(body.error ?? "LAN registration failed");
  }
  return body.data.info.id;
}

// ---------------------------------------------------------------------------

export function VerificationStep({
  api,
  onConnected,
}: {
  api: WizardApi;
  onConnected: () => void;
}) {
  const [stages, setStages] = useState<Stage[]>([
    { id: "transport", label: "Transport established", state: "pending" },
    { id: "health", label: "Runner connected", state: "pending" },
    { id: "elements", label: "UI elements readable", state: "pending" },
  ]);
  const [runId, setRunId] = useState(0);
  const startedRef = useRef(false);

  const [pairing, setPairing] = useState<PendingPairing | null>(null);
  const [pairCode, setPairCode] = useState("");
  const [pairBusy, setPairBusy] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);
  const [pairHeld, setPairHeld] = useState(false);
  // The operator said the pairing was revoked in the app: the next code replaces
  // the token the runner holds.
  const [replaceArmed, setReplaceArmed] = useState(false);

  useEffect(() => {
    // Reset the guard on each explicit retry (runId changes).
    startedRef.current = false;
  }, [runId]);

  const update = (id: string, patch: Partial<Stage>) => {
    setStages((prev) => prev.map((s) => (s.id === id ? { ...s, ...patch } : s)));
  };

  const runCheck = async () => {
    api.setError(null);
    setPairing(null);
    setStages((prev) => prev.map((s) => ({ ...s, state: "pending" as const, detail: undefined })));

    // Stage 1: transport
    update("transport", { state: "running" });
    let proxyUrl = api.state.proxyUrl;
    let registryId: string | null = null;
    try {
      let detail = "";
      if (api.state.connectionType === "usb" && api.state.deviceId) {
        // Android uses ADB forward; iOS uses the pymobiledevice3 sidecar.
        const isIos = api.state.platform === "ios";
        const endpoint = isIos
          ? `${getApiBase()}/ui-bridge/ios/forward`
          : `${getApiBase()}/ui-bridge/apps/forward-device`;
        const payload = isIos
          ? { udid: api.state.deviceId, devicePort: 9876 }
          : { deviceId: api.state.deviceId };
        const resp = await tracedFetch(endpoint, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
        const body: ApiResponse<ForwardDeviceResponse> = await resp.json();
        if (!body.success || !body.data) {
          throw new Error(body.error ?? "USB forward failed");
        }
        proxyUrl = `http://127.0.0.1:${body.data.localPort}`;
        detail = forwardPairingDetail(body.data.pairing);
      } else if (api.state.connectionType === "lan" && proxyUrl) {
        registryId = await findOrRegisterLanDevice(api.state.deviceId, proxyUrl);
      }
      if (!proxyUrl) throw new Error("No proxy URL available");
      update("transport", { state: "ok", detail: `${proxyUrl}${detail}` });
    } catch (e) {
      const msg = describeThrown(e, "Transport check failed");
      update("transport", { state: "fail", detail: msg });
      api.setError(msg);
      return;
    }

    // Stage 2: the runner connects to the device.
    update("health", { state: "running" });
    try {
      if (registryId) {
        const resp = await tracedFetch(
          `${getApiBase()}/ui-bridge/devices/${encodeURIComponent(registryId)}/connect`,
          { method: "POST" },
        );
        const body: ApiResponse<DeviceConnectResponse> = await resp.json();
        if (!body.success && body.code === "PAIRING_REQUIRED") {
          setPairing({ registryId, message: body.error ?? "This device requires pairing." });
          update("health", { state: "fail", detail: "Pairing required: enter the code below" });
          return;
        }
        if (!body.success || !body.data) {
          throw new Error(body.error ?? `HTTP ${resp.status}`);
        }
        update("health", { state: "ok", detail: `connected (${body.data.authPath})` });
      } else {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/connect`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ url: proxyUrl, appType: "mobile" }),
        });
        const body: ApiResponse<SdkConnectResponse> = await resp.json();
        if (!body.success && body.code === "PAIRING_REQUIRED" && api.state.deviceId) {
          // A phone that did not admit this cable as on the phone itself asks for
          // a code. It is paired through its row in /ui-bridge/devices, under the
          // cable's serial, which exists only once the runner's USB scanner has
          // listed the phone (an Android phone; an iPhone gets no USB row).
          if (!(await deviceRowExists(api.state.deviceId))) {
            throw new Error(
              `${body.error ?? "This device requires pairing."} The runner has no row for ` +
                `${api.state.deviceId} in /ui-bridge/devices to pair it through yet: pair it ` +
                "over Wi-Fi from Settings > Physical Devices, or retry once its USB row appears.",
            );
          }
          setPairing({
            registryId: api.state.deviceId,
            message: body.error ?? "This device requires pairing.",
          });
          update("health", { state: "fail", detail: "Pairing required: enter the code below" });
          return;
        }
        if (!body.success || !body.data) {
          throw new Error(body.error ?? `HTTP ${resp.status}`);
        }
        const caps = body.data.app.capabilities.length;
        update("health", {
          state: "ok",
          detail: `${body.data.app.appName} · ${caps} capabilities`,
        });
      }
    } catch (e) {
      const msg = describeThrown(e, "Connect failed");
      update("health", { state: "fail", detail: msg });
      api.setError(msg);
      return;
    }

    // Stage 3: elements via the runner's snapshot of the connected app.
    update("elements", { state: "running" });
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot`);
      const body: SnapshotBody = await resp.json();
      if (!resp.ok || body.success === false) {
        throw new Error(body.error ?? `HTTP ${resp.status}`);
      }
      const elements = body.data?.elements ?? body.elements ?? [];
      if (elements.length === 0) {
        throw new Error("snapshot returned no elements");
      }
      const preview: WizardElementPreview[] = elements.slice(0, 3).map((e) => ({
        id: e.id,
        label: e.label ?? e.id,
        type: e.type ?? "unknown",
      }));
      api.setElements(
        elements.map((e) => ({
          id: e.id,
          label: e.label ?? e.id,
          type: e.type ?? "unknown",
        })),
      );
      update("elements", {
        state: "ok",
        detail: `${elements.length} elements · ${preview.map((p) => p.label).join(", ")}`,
      });
      // Success — advance to Complete.
      onConnected();
      setTimeout(() => api.next(), 400);
    } catch (e) {
      const msg = describeThrown(e, "Element check failed");
      update("elements", { state: "fail", detail: msg });
      api.setError(msg);
    }
  };

  const submitPairing = async () => {
    if (!pairing) return;
    const replace = replaceArmed;
    setPairBusy(true);
    setPairError(null);
    setPairHeld(false);
    try {
      const resp = await tracedFetch(
        `${getApiBase()}/ui-bridge/devices/${encodeURIComponent(pairing.registryId)}/pair`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(replace ? { code: pairCode, replace } : { code: pairCode }),
        },
      );
      const body: ApiResponse<unknown> = await resp.json();
      if (!body.success) {
        setPairError(body.error ?? "Pairing failed");
        // The runner holds a different token for this phone: replace it only if
        // the pairing was revoked in the app's Settings. The phone spent the code
        // on this pairing, so the entry is cleared for a new one.
        if (body.code === "PAIRING_TOKEN_HELD") {
          setPairHeld(true);
          setPairCode("");
        }
        return;
      }
      setPairCode("");
      setReplaceArmed(false);
      setRunId((n) => n + 1);
    } catch (e) {
      setPairError(describeThrown(e, "Pairing failed"));
    } finally {
      setPairBusy(false);
    }
  };

  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;

    void runCheck();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runId]);

  const failed = stages.some((s) => s.state === "fail");

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">Connecting the runner to the device…</p>
      <p className="text-xs text-muted-foreground">
        The runner drives one app at a time: connecting makes this device its active target, in
        place of any app it was connected to.
      </p>

      <ul className="space-y-2">
        {stages.map((s) => (
          <li
            key={s.id}
            className="flex items-center gap-3 p-3 rounded-md bg-zinc-800/50 border border-zinc-700"
          >
            <StageIcon state={s.state} />
            <div className="flex-1 min-w-0">
              <div className="text-sm text-foreground">{s.label}</div>
              {s.detail && <div className="text-xs text-muted-foreground truncate">{s.detail}</div>}
            </div>
          </li>
        ))}
      </ul>

      {pairing && (
        <div className="space-y-2 p-3 rounded-md bg-zinc-800/50 border border-zinc-700">
          <p className="text-sm text-foreground">Pair this device</p>
          <p className="text-xs text-muted-foreground">
            {replaceArmed
              ? "Tap Pair in the app's Settings again and enter the new code: the last one is spent. Pressing Replace keeps the token the phone answers with, in place of the one this runner holds."
              : "Tap Pair in the app's Settings and enter the 6-digit code it shows. A phone that admits the USB cable as on the phone itself is paired over it with no code."}
          </p>
          <div className="flex gap-2">
            <input
              aria-label="Pairing code"
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={6}
              value={pairCode}
              onChange={(e) => setPairCode(e.target.value.replace(/\D/g, ""))}
              placeholder="123456"
              className="w-32 px-2.5 py-1.5 font-mono text-sm tracking-[0.3em] bg-zinc-900 rounded-md border border-zinc-700 outline-hidden focus:ring-1 focus:ring-primary/50"
            />
            <Button
              onClick={() => submitPairing()}
              disabled={pairBusy || !PAIRING_CODE.test(pairCode)}
            >
              {pairBusy ? "Pairing…" : replaceArmed ? "Replace" : "Pair"}
            </Button>
          </div>
          {pairError && <p className="text-xs text-destructive">{pairError}</p>}
          {pairHeld && !replaceArmed && (
            <Button
              variant="secondary"
              onClick={() => {
                setReplaceArmed(true);
                setPairCode("");
              }}
              disabled={pairBusy}
            >
              I revoked it in the app: replace the pairing
            </Button>
          )}
        </div>
      )}

      {failed && !pairing && (
        <div className="flex justify-end">
          <Button variant="secondary" onClick={() => setRunId((n) => n + 1)}>
            <RefreshCw className="w-4 h-4" />
            Retry
          </Button>
        </div>
      )}
    </div>
  );
}

function StageIcon({ state }: { state: StageState }) {
  switch (state) {
    case "pending":
      return <div className="w-5 h-5 rounded-full bg-zinc-700" aria-hidden />;
    case "running":
      return <Loader2 className="w-5 h-5 text-primary animate-spin" />;
    case "ok":
      return <Check className="w-5 h-5 text-emerald-400" />;
    case "fail":
      return <XCircle className="w-5 h-5 text-destructive" />;
  }
}
