/**
 * DiscoverySettings.tsx
 *
 * Manages the user-configurable port list that the UI Bridge app scanner
 * probes alongside the hardcoded defaults. A user running dev servers on
 * unusual ports (7000, 5000, etc.) adds them here so they show up in the
 * integration tool's "Scan" results.
 */

import { useState, useEffect, useCallback } from "react";
import { Radar, Plus, Trash2 } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface DiscoverySettingsProps {
  onLog: LogFunction;
}

interface DiscoveryPortsPayload {
  ports: number[];
}

interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// Kept in sync with `DESKTOP_APP_PORTS` in `mcp/app_discovery.rs`. Shown to
// the user as "always scanned" so they don't duplicate them in their list.
const HARDCODED_DESKTOP_DEFAULTS: number[] = [1420, 9875, 9876, 9877, 9878, 8888, 3333];
const HARDCODED_WEB_DEFAULTS: number[] = [
  3000, 3001, 3002, 3003, 3004, 3005, 3006, 3007, 3008, 3009, 3010, 4200, 5173, 5174, 5175, 8080,
  8081, 4000,
];

export function DiscoverySettings({ onLog }: DiscoverySettingsProps) {
  const [ports, setPorts] = useState<number[]>([]);
  const [initialPorts, setInitialPorts] = useState<number[]>([]);
  const [newPortInput, setNewPortInput] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadPorts = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}/settings/discovery-ports`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`);
      }
      const body: ApiEnvelope<DiscoveryPortsPayload> = await res.json();
      if (!body.success) {
        throw new Error(body.error || "Failed to load discovery ports");
      }
      const list = body.data?.ports ?? [];
      setPorts(list);
      setInitialPorts(list);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      onLog("error", `Failed to load discovery ports: ${msg}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void loadPorts();
    });
    return () => {
      cancelled = true;
    };
  }, [loadPorts]);

  const hasChanges =
    ports.length !== initialPorts.length || ports.some((p, i) => p !== initialPorts[i]);

  const normalized = Array.from(new Set(ports)).sort((a, b) => a - b);

  const handleAddPort = () => {
    const parsed = Number.parseInt(newPortInput.trim(), 10);
    if (!Number.isFinite(parsed) || parsed < 1 || parsed > 65535) {
      onLog("warning", `Invalid port: ${newPortInput}. Must be 1-65535.`);
      return;
    }
    if (ports.includes(parsed)) {
      onLog("info", `Port ${parsed} is already in the list.`);
      setNewPortInput("");
      return;
    }
    setPorts((prev) => [...prev, parsed].sort((a, b) => a - b));
    setNewPortInput("");
  };

  const handleRemovePort = (port: number) => {
    setPorts((prev) => prev.filter((p) => p !== port));
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}/settings/discovery-ports`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ ports: normalized }),
      });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}: ${res.statusText}`);
      }
      const body: ApiEnvelope<null> = await res.json();
      if (!body.success) {
        throw new Error(body.error || "Failed to save discovery ports");
      }
      setInitialPorts(normalized);
      onLog("success", `Saved ${normalized.length} custom discovery port(s)`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      onLog("error", `Failed to save discovery ports: ${msg}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="App Discovery"
        description="Extra ports for the UI Bridge scanner to probe when looking for SDK-enabled apps. Hardcoded defaults are always scanned; this list adds to them."
        icon={<Radar className="w-6 h-6" />}
      />

      {/* Custom ports list */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div>
          <h4 className="font-medium text-sm">Custom Ports</h4>
          <p className="text-xs text-muted-foreground mt-1">
            Apps served on these ports are included in every desktop scan. Valid range: 1-65535.
          </p>
        </div>

        {loading ? (
          <div className="text-xs text-muted-foreground">Loading...</div>
        ) : ports.length === 0 ? (
          <div className="text-xs text-muted-foreground italic">
            No custom ports configured. Add one below.
          </div>
        ) : (
          <ul className="space-y-1.5">
            {ports
              .slice()
              .sort((a, b) => a - b)
              .map((port) => (
                <li
                  key={port}
                  className="flex items-center justify-between rounded-md bg-muted/40 px-3 py-1.5"
                >
                  <code className="text-sm font-mono">{port}</code>
                  <button
                    type="button"
                    onClick={() => handleRemovePort(port)}
                    aria-label={`Remove port ${port}`}
                    className="text-muted-foreground hover:text-destructive transition-colors"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                  </button>
                </li>
              ))}
          </ul>
        )}

        {/* Add-port row */}
        <div className="flex gap-2">
          <input
            type="number"
            inputMode="numeric"
            min={1}
            max={65535}
            value={newPortInput}
            onChange={(e) => setNewPortInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                handleAddPort();
              }
            }}
            placeholder="e.g. 7000"
            className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          <button
            type="button"
            onClick={handleAddPort}
            disabled={!newPortInput.trim()}
            className="flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium bg-primary/10 text-primary hover:bg-primary/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            <Plus className="w-3.5 h-3.5" />
            Add
          </button>
        </div>

        {error && <div className="text-xs text-destructive">{error}</div>}
      </div>

      {/* Always-scanned reference */}
      <div className="space-y-2 rounded-lg bg-muted/20 p-4">
        <h4 className="font-medium text-sm">Always Scanned</h4>
        <p className="text-xs text-muted-foreground">
          These defaults ship with the runner and cannot be disabled. Adding them to the custom list
          above has no effect.
        </p>
        <div className="space-y-1.5 mt-3">
          <div>
            <span className="text-[10px] uppercase tracking-wider text-muted-foreground">
              Desktop
            </span>
            <div className="flex flex-wrap gap-1.5 mt-1">
              {HARDCODED_DESKTOP_DEFAULTS.map((p) => (
                <code
                  key={p}
                  className="text-[11px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground"
                >
                  {p}
                </code>
              ))}
            </div>
          </div>
          <div>
            <span className="text-[10px] uppercase tracking-wider text-muted-foreground">Web</span>
            <div className="flex flex-wrap gap-1.5 mt-1">
              {HARDCODED_WEB_DEFAULTS.map((p) => (
                <code
                  key={p}
                  className="text-[11px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground"
                >
                  {p}
                </code>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* Save */}
      <div className="flex justify-end">
        <button
          type="button"
          onClick={handleSave}
          disabled={!hasChanges || saving || loading}
          className={`px-4 py-2 rounded-md text-sm font-medium transition-colors ${
            hasChanges && !saving && !loading
              ? "bg-primary text-primary-foreground hover:bg-primary/90"
              : "bg-muted text-muted-foreground cursor-not-allowed"
          }`}
        >
          {saving ? "Saving..." : "Save Settings"}
        </button>
      </div>

      <div className="p-3 bg-primary/5 rounded-lg">
        <div className="text-xs text-muted-foreground">
          <strong className="text-foreground">Note:</strong> Apps that load the UI Bridge SDK
          advertise themselves automatically via phone-home registration; they appear in the
          integration tool under "Registered Apps" without needing a scan or a port entry here. This
          list is only for the fallback "Scan" path and for apps that don't use the SDK.
        </div>
      </div>
    </div>
  );
}
