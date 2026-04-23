/**
 * Step 2: find the device.
 *
 * Branches by connectionType:
 *   - USB: poll /ui-bridge/apps/scan/mobile (adb_client-backed in Plan 1A)
 *   - LAN: subscribe to Tauri "device-discovered" events (Plan 1C) + manual IP
 *   - Remote: configure tunnel, then wait for the device to register
 */

import { useEffect, useState } from "react";
import { Loader2, RefreshCw, Smartphone, Wifi } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

import { Button } from "../ui/Button";
import type { WizardApi } from "./ConnectionWizard";
import type { DiscoveredDevice, Platform } from "./types";

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

interface MobileDeviceApi {
  deviceId: string;
  deviceType: string;
  model?: string;
  status: string;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface MdnsDiscoveredPayload {
  deviceId: string;
  port: number;
  addresses: string[];
  txtRecords: Record<string, string>;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function DeviceDiscoveryStep({ api }: { api: WizardApi }) {
  const { connectionType } = api.state;

  if (connectionType === "usb") return <UsbBranch api={api} />;
  if (connectionType === "lan") return <LanBranch api={api} />;
  return <RemoteBranch api={api} />;
}

// ---------------------------------------------------------------------------
// USB branch
// ---------------------------------------------------------------------------

interface UsbDevice extends MobileDeviceApi {
  platform: Platform;
}

function UsbBranch({ api }: { api: WizardApi }) {
  const [devices, setDevices] = useState<UsbDevice[]>([]);
  const [loading, setLoading] = useState(false);
  const [iosError, setIosError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    api.setError(null);
    setIosError(null);
    try {
      // Android via adb_client (Plan 1A) and iOS via ios_bridge (Plan 2) —
      // fetched in parallel. iOS failure is non-fatal (user may not have
      // pymobiledevice3 installed); Android failure is.
      const [androidResp, iosResp] = await Promise.all([
        tracedFetch(`${getApiBase()}/ui-bridge/apps/scan/mobile`),
        tracedFetch(`${getApiBase()}/ui-bridge/apps/scan/ios`).catch(() => null),
      ]);

      const androidBody: ApiResponse<MobileDeviceApi[]> = await androidResp.json();
      if (!androidBody.success || !androidBody.data) {
        throw new Error(androidBody.error ?? "Android scan failed");
      }
      const androidDevices: UsbDevice[] = androidBody.data
        .filter((d) => d.status === "online")
        .map((d) => ({ ...d, platform: "android" as const }));

      let iosDevices: UsbDevice[] = [];
      if (iosResp) {
        const iosBody: ApiResponse<MobileDeviceApi[]> = await iosResp.json();
        if (iosBody.success && iosBody.data) {
          // iOS USB shows as connection=usb in our mapping; skip network
          // entries from Bonjour (those belong in the LAN branch).
          iosDevices = iosBody.data
            .filter((d) => d.deviceType === "ios-usb" && d.status === "online")
            .map((d) => ({ ...d, platform: "ios" as const }));
        } else if (iosBody.error) {
          setIosError(iosBody.error);
        }
      }

      setDevices([...androidDevices, ...iosDevices]);
    } catch (e) {
      api.setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside load.
    void Promise.resolve().then(() => {
      if (!cancelled) void load();
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const pick = (d: UsbDevice) => {
    const discovered: DiscoveredDevice = {
      deviceId: d.deviceId,
      platform: d.platform,
      model: d.model,
      adbSerial: d.platform === "android" ? d.deviceId : undefined,
    };
    api.setDevice(discovered, null);
    api.next();
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">
          Connected Android (ADB) + iOS (pymobiledevice3) devices.
        </p>
        <Button variant="ghost" size="sm" onClick={load} disabled={loading}>
          {loading ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <RefreshCw className="w-4 h-4" />
          )}
          Refresh
        </Button>
      </div>

      {devices.length === 0 ? (
        <EmptyDeviceHint
          title="No devices found"
          hints={[
            "Android: enable Developer Options → USB debugging and accept the RSA prompt",
            "iOS: accept 'Trust this computer' on the iPhone",
            "pymobiledevice3 must be installed for iOS detection (pip install pymobiledevice3)",
          ]}
        />
      ) : (
        <ul className="space-y-2">
          {devices.map((d) => (
            <DeviceRow
              key={d.deviceId}
              id={d.deviceId}
              model={d.model}
              sub={`${d.platform} · ${d.deviceType}`}
              onPick={() => pick(d)}
            />
          ))}
        </ul>
      )}

      {iosError && (
        <p className="text-xs text-muted-foreground">iOS scan unavailable: {iosError}</p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// LAN branch
// ---------------------------------------------------------------------------

function LanBranch({ api }: { api: WizardApi }) {
  const [devices, setDevices] = useState<MdnsDiscoveredPayload[]>([]);
  const [manualIp, setManualIp] = useState("");

  useEffect(() => {
    // Subscribe to Tauri events emitted by mdns_scanner.rs::start_with_tauri_events
    // Dynamic import so the web build (no Tauri runtime) doesn't break.
    let unlistenDiscovered: (() => void) | undefined;
    let unlistenRemoved: (() => void) | undefined;
    let cancelled = false;

    (async () => {
      try {
        const { listen } = await import("@tauri-apps/api/event");
        if (cancelled) return;
        unlistenDiscovered = await listen<MdnsDiscoveredPayload>("device-discovered", (ev) => {
          setDevices((prev) => {
            if (prev.some((p) => p.deviceId === ev.payload.deviceId)) return prev;
            return [...prev, ev.payload];
          });
        });
        unlistenRemoved = await listen<{ fullname: string }>("device-removed", (ev) => {
          setDevices((prev) => prev.filter((d) => !ev.payload.fullname.includes(d.deviceId)));
        });
      } catch {
        // Non-Tauri environment — ignore.
      }
    })();

    return () => {
      cancelled = true;
      unlistenDiscovered?.();
      unlistenRemoved?.();
    };
  }, []);

  const pickMdns = (d: MdnsDiscoveredPayload) => {
    const addr = d.addresses[0];
    api.setDevice(
      {
        deviceId: d.deviceId,
        platform: (d.txtRecords.platform as Platform) ?? "android",
        model: d.txtRecords.model,
        address: addr ? `${addr}:${d.port}` : undefined,
      },
      addr ? `http://${addr}:${d.port}` : null,
    );
    api.next();
  };

  const pickManual = () => {
    const trimmed = manualIp.trim();
    if (!trimmed) return;
    // Very lenient parse; real validation happens in Verification.
    const addr = trimmed.includes(":") ? trimmed : `${trimmed}:8087`;
    api.setDevice(
      { deviceId: `manual-${addr}`, platform: "android", address: addr },
      `http://${addr}`,
    );
    api.next();
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Wifi className="w-4 h-4" />
        <span>Listening for devices on this network…</span>
        <Loader2 className="w-3 h-3 animate-spin ml-auto" />
      </div>

      {devices.length === 0 ? (
        <EmptyDeviceHint
          title="No devices yet"
          hints={[
            "Make sure the phone is on the same Wi-Fi as this computer",
            "The mobile app must have UI Bridge mDNS announcing enabled",
            "Some routers block mDNS — try the manual IP entry below",
          ]}
        />
      ) : (
        <ul className="space-y-2">
          {devices.map((d) => (
            <DeviceRow
              key={d.deviceId}
              id={d.deviceId}
              model={d.txtRecords.model}
              sub={d.addresses[0] ?? ""}
              onPick={() => pickMdns(d)}
            />
          ))}
        </ul>
      )}

      <div className="pt-3 border-t border-zinc-800">
        <label className="text-xs text-muted-foreground block mb-1">Manual IP (host:port)</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={manualIp}
            onChange={(e) => setManualIp(e.target.value)}
            placeholder="192.168.1.42:8087"
            className="flex-1 px-3 py-2 rounded-md bg-zinc-800 border border-zinc-700 text-sm text-foreground focus:outline-none focus:border-primary"
          />
          <Button variant="secondary" size="sm" onClick={pickManual}>
            Use
          </Button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Remote branch
// ---------------------------------------------------------------------------

function RemoteBranch({ api }: { api: WizardApi }) {
  const [serverAddr, setServerAddr] = useState("");
  const [token, setToken] = useState("");
  const [clientToml, setClientToml] = useState<string | null>(null);
  const [serverToml, setServerToml] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<"client" | "server">("client");

  const generate = async () => {
    api.setError(null);
    const resp = await tracedFetch(`${getApiBase()}/tunnel/generate-config`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        serverAddr,
        defaultToken: token,
        services: [
          {
            name: "ui-bridge",
            local_addr: "127.0.0.1:9876",
            service_type: "tcp",
            token,
          },
        ],
      }),
    });
    const body: ApiResponse<{ clientToml: string; serverToml: string }> = await resp.json();
    if (!body.success || !body.data) {
      api.setError(body.error ?? "Config generation failed");
      return;
    }
    setClientToml(body.data.clientToml);
    setServerToml(body.data.serverToml);
    api.setTunnelConfig({ serverAddr, token });
  };

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        Point the runner at your rathole server. See{" "}
        <span className="text-foreground">Oracle Cloud Free Tier + rathole</span> docs for a
        recommended free setup.
      </p>

      <div className="space-y-2">
        <label className="text-xs text-muted-foreground block">Server address</label>
        <input
          type="text"
          value={serverAddr}
          onChange={(e) => setServerAddr(e.target.value)}
          placeholder="relay.example.com:2333"
          className="w-full px-3 py-2 rounded-md bg-zinc-800 border border-zinc-700 text-sm text-foreground focus:outline-none focus:border-primary"
        />
      </div>

      <div className="space-y-2">
        <label className="text-xs text-muted-foreground block">Shared token</label>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder="Shared secret with the server"
          className="w-full px-3 py-2 rounded-md bg-zinc-800 border border-zinc-700 text-sm text-foreground focus:outline-none focus:border-primary"
        />
      </div>

      <div className="flex gap-2">
        <Button variant="primary" onClick={generate} disabled={!serverAddr || !token}>
          Generate config
        </Button>
        {clientToml && (
          <Button
            variant="secondary"
            onClick={() => {
              api.next();
            }}
          >
            Continue
          </Button>
        )}
      </div>

      {clientToml && serverToml && (
        <div className="space-y-2 mt-2">
          <div className="flex gap-2 text-xs">
            <button
              onClick={() => setActiveTab("client")}
              className={
                "px-2 py-1 rounded " +
                (activeTab === "client"
                  ? "bg-primary text-primary-foreground"
                  : "bg-zinc-800 text-muted-foreground hover:text-foreground")
              }
            >
              client.toml (runner)
            </button>
            <button
              onClick={() => setActiveTab("server")}
              className={
                "px-2 py-1 rounded " +
                (activeTab === "server"
                  ? "bg-primary text-primary-foreground"
                  : "bg-zinc-800 text-muted-foreground hover:text-foreground")
              }
            >
              server.toml (VPS)
            </button>
          </div>

          <pre className="p-3 rounded-md bg-zinc-900 border border-zinc-700 text-xs text-zinc-300 whitespace-pre-wrap max-h-56 overflow-auto">
            {activeTab === "client" ? clientToml : serverToml}
          </pre>

          {activeTab === "server" && (
            <p className="text-[11px] text-muted-foreground">
              Copy this to your rathole server (Oracle Cloud Free Tier is a good free option) as{" "}
              <code>server.toml</code>. The bundled deploy script at{" "}
              <code>scripts/rathole-oracle-deploy.sh</code> automates the setup.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared subviews
// ---------------------------------------------------------------------------

function DeviceRow({
  id,
  model,
  sub,
  onPick,
}: {
  id: string;
  model?: string;
  sub?: string;
  onPick: () => void;
}) {
  return (
    <li>
      <button
        onClick={onPick}
        className="w-full flex items-center gap-3 p-3 rounded-md bg-zinc-800/50 hover:bg-zinc-800 border border-zinc-700 hover:border-primary/50 transition-colors text-left"
      >
        <Smartphone className="w-4 h-4 text-primary" />
        <div className="flex-1 min-w-0">
          <div className="text-sm text-foreground truncate">{model ?? id}</div>
          <div className="text-xs text-muted-foreground truncate">
            {id}
            {sub ? ` · ${sub}` : ""}
          </div>
        </div>
      </button>
    </li>
  );
}

function EmptyDeviceHint({ title, hints }: { title: string; hints: string[] }) {
  return (
    <div className="p-4 rounded-md border border-dashed border-zinc-700 bg-zinc-800/30">
      <div className="text-sm text-foreground mb-2">{title}</div>
      <ul className="list-disc list-inside text-xs text-muted-foreground space-y-1">
        {hints.map((h) => (
          <li key={h}>{h}</li>
        ))}
      </ul>
    </div>
  );
}
