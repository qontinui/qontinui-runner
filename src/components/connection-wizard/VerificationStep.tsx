/**
 * Step 4: run a live health check against the selected device.
 *
 * Stages:
 *   1. Transport established (USB: run /ui-bridge/apps/forward-device;
 *      LAN: reuse the proxyUrl from Step 2; Remote: tunnel already up)
 *   2. Health endpoint returns 200 + uiBridge metadata
 *   3. UI elements readable (calls the proxy URL's /ui-bridge/elements)
 */

import { useEffect, useRef, useState } from "react";
import { Check, Loader2, XCircle, RefreshCw } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

import { Button } from "../ui/Button";
import type { WizardApi } from "./ConnectionWizard";
import type { WizardElementPreview } from "./types";

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
}

interface ForwardDeviceResponse {
  localPort: number;
  deviceId: string;
  remotePort: number;
}

interface HealthBody {
  uiBridge?: {
    appId?: string;
    appName?: string;
    capabilities?: string[];
  };
}

// Shape returned by the UI Bridge /elements endpoint. Real payload is richer;
// we only pluck label/type for the preview.
interface ElementsBody {
  elements?: Array<{ id: string; label?: string; type?: string }>;
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
    { id: "health", label: "Health check passed", state: "pending" },
    { id: "elements", label: "UI elements readable", state: "pending" },
  ]);
  const [runId, setRunId] = useState(0);
  const startedRef = useRef(false);

  useEffect(() => {
    // Reset the guard on each explicit retry (runId changes).
    startedRef.current = false;
  }, [runId]);

  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    void runCheck();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runId]);

  const update = (id: string, patch: Partial<Stage>) => {
    setStages((prev) => prev.map((s) => (s.id === id ? { ...s, ...patch } : s)));
  };

  const runCheck = async () => {
    api.setError(null);
    setStages((prev) => prev.map((s) => ({ ...s, state: "pending" as const, detail: undefined })));

    // Stage 1: transport
    update("transport", { state: "running" });
    let proxyUrl = api.state.proxyUrl;
    try {
      if (api.state.connectionType === "usb" && api.state.deviceId) {
        const resp = await tracedFetch(`${getApiBase()}/ui-bridge/apps/forward-device`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ deviceId: api.state.deviceId }),
        });
        const body: ApiResponse<ForwardDeviceResponse> = await resp.json();
        if (!body.success || !body.data) {
          throw new Error(body.error ?? "ADB forward failed");
        }
        proxyUrl = `http://127.0.0.1:${body.data.localPort}`;
      }
      if (!proxyUrl) throw new Error("No proxy URL available");
      update("transport", { state: "ok", detail: proxyUrl });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      update("transport", { state: "fail", detail: msg });
      api.setError(msg);
      return;
    }

    // Stage 2: health
    update("health", { state: "running" });
    try {
      const resp = await tracedFetch(`${proxyUrl}/ui-bridge/health`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const body: HealthBody = await resp.json();
      if (!body.uiBridge) throw new Error("response missing uiBridge field");
      const caps = body.uiBridge.capabilities?.length ?? 0;
      update("health", {
        state: "ok",
        detail: `${body.uiBridge.appName ?? "unknown app"} · ${caps} capabilities`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      update("health", { state: "fail", detail: msg });
      api.setError(msg);
      return;
    }

    // Stage 3: elements
    update("elements", { state: "running" });
    try {
      const resp = await tracedFetch(`${proxyUrl}/ui-bridge/elements`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const body: ElementsBody = await resp.json();
      const elements = body.elements ?? [];
      if (elements.length === 0) {
        throw new Error("elements endpoint returned empty list");
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
      const msg = e instanceof Error ? e.message : String(e);
      update("elements", { state: "fail", detail: msg });
      api.setError(msg);
    }
  };

  const failed = stages.some((s) => s.state === "fail");

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        Running live health checks against the device…
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

      {failed && (
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
