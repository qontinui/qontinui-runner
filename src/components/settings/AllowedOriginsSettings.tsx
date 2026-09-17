/**
 * AllowedOriginsSettings.tsx
 *
 * Extra browser origins the runner's loopback API admits as "trusted"
 * (plan 2026-09-17-runner-loopback-api-accepts-any-origin, Phase 3).
 *
 * Trusted = local trust for non-door routes (which include running workflows
 * and checks). Origins added here never get credential doors. Only the four
 * built-in default dev origins (http://localhost:3001, http://127.0.0.1:3001,
 * http://localhost:9875, http://127.0.0.1:9875) still reach the guard's
 * transitional TRUSTED_DOOR_GRACE doors — local command execution and file
 * reads included — until qontinui-web#1380 deploys and the grace is removed.
 * Agents, scripts and MCP clients send no Origin and need no entry here.
 * Saved values are live within ~2 s — no restart.
 */

import { useState, useEffect, useCallback } from "react";
import { Globe, Plus, Trash2 } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface AllowedOriginsSettingsProps {
  onLog: LogFunction;
}

interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface AllowedOriginsData {
  origins: string[];
  defaults?: string[];
  envVar?: string;
}

const ENDPOINT = "/settings/api/allowed-origins";

export function AllowedOriginsSettings({ onLog }: AllowedOriginsSettingsProps) {
  const [origins, setOrigins] = useState<string[]>([]);
  const [initial, setInitial] = useState<string[]>([]);
  const [defaults, setDefaults] = useState<string[]>([]);
  const [envVar, setEnvVar] = useState<string>("QONTINUI_RUNNER_ALLOWED_ORIGINS");
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}${ENDPOINT}`);
      const body: ApiEnvelope<AllowedOriginsData> = await res.json();
      if (!res.ok || !body.success) {
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      const list = body.data?.origins ?? [];
      setOrigins(list);
      setInitial(list);
      setDefaults(body.data?.defaults ?? []);
      if (body.data?.envVar) setEnvVar(body.data.envVar);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      onLog("error", `Failed to load allowed origins: ${msg}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void load();
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  const hasChanges =
    origins.length !== initial.length || origins.some((o, i) => o !== initial[i]);

  const add = () => {
    const value = input.trim();
    if (!value) return;
    if (!origins.includes(value)) setOrigins((prev) => [...prev, value]);
    setInput("");
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}${ENDPOINT}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ origins }),
      });
      const body: ApiEnvelope<{ origins: string[] }> = await res.json();
      if (!res.ok || !body.success) {
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      const saved = body.data?.origins ?? origins;
      setOrigins(saved);
      setInitial(saved);
      onLog("success", `Saved ${saved.length} allowed browser origin(s)`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      onLog("error", `Failed to save allowed origins: ${msg}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-4 mt-8" data-ui-bridge-content="allowed-origins-settings">
      <SectionHeader
        title="Allowed Browser Origins"
        description={`Web pages at these origins get local trust for this runner's ordinary routes, including running workflows and checks, so add only origins served by software you trust as much as the runner itself. Origins you add here never get credential routes (secrets, file reads, server-side requests, command execution). Note: the four built-in default dev origins (localhost and 127.0.0.1, ports 3001 and 9875) currently still reach a transitional set of credential routes, including command execution and file reads, until the web app update that removes that need is deployed. Agents and scripts need no entry. Takes effect within a few seconds, no restart. Headless equivalent: ${envVar}.`}
        icon={<Globe className="w-6 h-6" />}
      />
      <div className="space-y-3 rounded-lg bg-card/50 p-4">
        {defaults.length > 0 && (
          <p className="text-xs text-muted-foreground">
            Always allowed: <code>{defaults.join(", ")}</code>
          </p>
        )}
        {loading ? (
          <div className="text-xs text-muted-foreground">Loading...</div>
        ) : origins.length === 0 ? (
          <div className="text-xs text-muted-foreground italic">No extra origins configured.</div>
        ) : (
          <ul className="space-y-1.5">
            {origins.map((o) => (
              <li
                key={o}
                data-ui-bridge-content={`allowed-origin:${o}`}
                data-ui-bridge-role="listitem"
                className="flex items-center justify-between rounded-md bg-muted/40 px-3 py-1.5"
              >
                <code className="text-sm font-mono">{o}</code>
                <button
                  type="button"
                  onClick={() => setOrigins((prev) => prev.filter((x) => x !== o))}
                  aria-label={`Remove origin ${o}`}
                  className="text-muted-foreground hover:text-destructive transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="flex gap-2">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                add();
              }
            }}
            placeholder="e.g. http://localhost:5173"
            data-ui-bridge-test-id="allowed-origin-input"
            className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md placeholder:text-muted-foreground outline-hidden focus:ring-1 focus:ring-primary/50"
          />
          <button
            type="button"
            onClick={add}
            disabled={!input.trim()}
            className="flex items-center gap-1 px-3 py-1.5 rounded-md text-sm font-medium bg-primary/10 text-primary hover:bg-primary/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            data-ui-bridge-test-id="allowed-origin-add"
          >
            <Plus className="w-3.5 h-3.5" /> Add
          </button>
        </div>
        {error && <div className="text-xs text-destructive">{error}</div>}
        <button
          type="button"
          onClick={() => void save()}
          disabled={!hasChanges || saving}
          className="px-3 py-1.5 rounded-md text-sm font-medium bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          data-ui-bridge-test-id="allowed-origins-save"
        >
          {saving ? "Saving..." : "Save"}
        </button>
      </div>
    </div>
  );
}
