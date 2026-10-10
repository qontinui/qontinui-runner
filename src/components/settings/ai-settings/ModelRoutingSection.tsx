import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Network, ShieldCheck } from "lucide-react";
import type { LogFunction, TauriResult } from "./types";

/** Mirrors `model_gateway::ModelGatewaySettings` (settings.json `model_gateway`). */
export interface ModelGatewaySettings {
  base_url?: string | null;
  headers?: Record<string, string>;
  api_key_helper?: string | null;
  /** The gateway authenticates by network position / mTLS: no helper needed. */
  network_auth?: boolean;
}

/** Mirrors `launch_spec::SessionPermissionSetting` (settings.json `claude_session_permission`). */
export type SessionPermissionSetting =
  | { mode: "site_default" }
  | { mode: "allow_list"; tools: string[] };

interface GatewayData {
  gateway: ModelGatewaySettings;
  declared?: boolean;
  valid?: boolean;
}

interface PermissionData {
  permission: SessionPermissionSetting;
}

export interface ParsedHeaders {
  headers: Record<string, string>;
  /** Non-blank lines that are not `Name: Value` — surfaced, never dropped. */
  malformed: string[];
}

/** `Name: Value` per line -> header map. Blank lines are ignored; any other
 * line without a name before its first `:` is reported in `malformed`. */
export function parseHeaderLines(text: string): ParsedHeaders {
  const headers: Record<string, string> = {};
  const malformed: string[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const idx = line.indexOf(":");
    const name = idx > 0 ? line.slice(0, idx).trim() : "";
    if (!name) {
      malformed.push(line);
      continue;
    }
    headers[name] = line.slice(idx + 1).trim();
  }
  return { headers, malformed };
}

export function formatHeaderLines(headers: Record<string, string> | undefined): string {
  return Object.entries(headers ?? {})
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

/** One tool spec per line (or comma-separated) -> trimmed, non-blank list.
 * A comma or newline inside parentheses belongs to the specifier
 * (`Bash(git log, status)` is one tool). */
export function parseToolList(text: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = "";
  const flush = () => {
    const t = current.trim();
    if (t) out.push(t);
    current = "";
  };
  for (const ch of text) {
    if (ch === "(") depth += 1;
    if (ch === ")" && depth > 0) depth -= 1;
    if ((ch === "," || ch === "\n") && depth === 0) {
      flush();
      continue;
    }
    current += ch;
  }
  flush();
  return out;
}

/**
 * Model gateway (all model calls through the tenant's gateway; subscription
 * accounts and rotation off) and the session permission posture (allow-list
 * instead of bypass, for machines whose managed settings forbid bypass).
 * Both persist in the runner's settings.json; the backend validates on save.
 */
export function ModelRoutingSection({ onLog }: { onLog: LogFunction }) {
  const [baseUrl, setBaseUrl] = useState("");
  const [headers, setHeaders] = useState("");
  const [helper, setHelper] = useState("");
  const [networkAuth, setNetworkAuth] = useState(false);
  const [gatewayError, setGatewayError] = useState<string | null>(null);
  const [savingGateway, setSavingGateway] = useState(false);

  const [allowListMode, setAllowListMode] = useState(false);
  const [tools, setTools] = useState("");
  const [permissionError, setPermissionError] = useState<string | null>(null);
  const [savingPermission, setSavingPermission] = useState(false);

  const load = useCallback(async () => {
    try {
      const g = await invoke<TauriResult<GatewayData>>("get_model_gateway");
      const gw = g?.data?.gateway ?? {};
      setBaseUrl(gw.base_url ?? "");
      setHeaders(formatHeaderLines(gw.headers));
      setHelper(gw.api_key_helper ?? "");
      setNetworkAuth(gw.network_auth === true);
      setGatewayError(g?.data?.valid === false ? (g.message ?? "invalid gateway") : null);

      const p = await invoke<TauriResult<PermissionData>>("get_claude_session_permission");
      const perm = p?.data?.permission;
      if (perm?.mode === "allow_list") {
        setAllowListMode(true);
        setTools(perm.tools.join("\n"));
      } else {
        setAllowListMode(false);
      }
    } catch (err) {
      console.warn("Failed to load model routing settings:", err);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot load of persisted settings from the backend
    void load();
  }, [load]);

  const saveGateway = async () => {
    setSavingGateway(true);
    setGatewayError(null);
    try {
      const parsed = parseHeaderLines(headers);
      if (parsed.malformed.length > 0) {
        throw new Error(
          `header line(s) not in "Name: Value" form: ${parsed.malformed.join(" | ")}`,
        );
      }
      const gateway: ModelGatewaySettings = {
        base_url: baseUrl.trim() || null,
        headers: parsed.headers,
        api_key_helper: helper.trim() || null,
        network_auth: networkAuth,
      };
      await invoke("save_model_gateway", { gateway });
      onLog("success", baseUrl.trim() ? "Model gateway saved" : "Model gateway cleared");
      await load();
    } catch (err) {
      const msg = String(err);
      setGatewayError(msg);
      onLog("error", `Model gateway not saved: ${msg}`);
    } finally {
      setSavingGateway(false);
    }
  };

  const savePermission = async () => {
    setSavingPermission(true);
    setPermissionError(null);
    try {
      const permission: SessionPermissionSetting = allowListMode
        ? { mode: "allow_list", tools: parseToolList(tools) }
        : { mode: "site_default" };
      await invoke("save_claude_session_permission", { permission });
      onLog("success", "Session permission posture saved");
      await load();
    } catch (err) {
      const msg = String(err);
      setPermissionError(msg);
      onLog("error", `Session permission posture not saved: ${msg}`);
    } finally {
      setSavingPermission(false);
    }
  };

  const inputClass =
    "w-full px-2.5 py-1.5 bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 text-sm";

  return (
    <div className="space-y-6">
      <div className="space-y-3" data-ui-id="model-gateway-section">
        <div className="flex items-center gap-2 text-sm font-medium">
          <Network className="w-4 h-4" /> Model gateway
        </div>
        <p className="text-xs text-muted-foreground">
          When set, every model call goes through this gateway: spawned Claude sessions use it as
          their API base URL with the key from the helper command, and the runner&apos;s own API
          calls use it too. Subscription accounts and account rotation are off while a gateway is
          set, and the Gemini, pi and OpenAI-compatible providers are refused. Leave the URL empty
          to use your Claude subscription. A repository&apos;s own{" "}
          <code>.claude/settings.json</code> or managed settings take precedence over the
          runner&apos;s session settings and can still redirect a session; govern those layers too.
        </p>
        <div className="space-y-1.5">
          <label htmlFor="model-gateway-base-url" className="text-xs font-medium">
            Base URL
          </label>
          <input
            id="model-gateway-base-url"
            type="url"
            placeholder="https://llm-gateway.example.com/anthropic"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            className={inputClass}
          />
        </div>
        <div className="space-y-1.5">
          <label htmlFor="model-gateway-helper" className="text-xs font-medium">
            API key helper command
          </label>
          <input
            id="model-gateway-helper"
            type="text"
            placeholder="/usr/local/bin/print-gateway-key"
            value={helper}
            onChange={(e) => setHelper(e.target.value)}
            className={inputClass}
          />
          <p className="text-[10px] text-muted-foreground">
            A command that prints the gateway key. The key itself is never stored. Required unless
            the gateway authenticates by network position.
          </p>
        </div>
        <div className="space-y-1.5">
          <label className="flex items-start gap-2 text-xs">
            <input
              type="checkbox"
              checked={networkAuth}
              onChange={(e) => setNetworkAuth(e.target.checked)}
              className="mt-0.5 accent-primary"
            />
            <span>The gateway authenticates by network position or mTLS (no key helper)</span>
          </label>
        </div>
        <div className="space-y-1.5">
          <label htmlFor="model-gateway-headers" className="text-xs font-medium">
            Extra headers (one <code>Name: Value</code> per line, no credentials)
          </label>
          <textarea
            id="model-gateway-headers"
            rows={3}
            value={headers}
            onChange={(e) => setHeaders(e.target.value)}
            className={inputClass}
          />
        </div>
        {gatewayError && <p className="text-xs text-destructive">{gatewayError}</p>}
        <button
          type="button"
          onClick={() => void saveGateway()}
          disabled={savingGateway}
          className="px-3 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 text-xs"
        >
          {savingGateway ? "Saving..." : "Save gateway"}
        </button>
      </div>

      <div className="space-y-3" data-ui-id="session-permission-section">
        <div className="flex items-center gap-2 text-sm font-medium">
          <ShieldCheck className="w-4 h-4" /> Session permissions
        </div>
        <label className="flex items-start gap-2 text-xs">
          <input
            type="radio"
            name="session-permission"
            checked={!allowListMode}
            onChange={() => setAllowListMode(false)}
            className="mt-0.5 accent-primary"
          />
          <span>Bypass permissions (default for autonomous sessions)</span>
        </label>
        <label className="flex items-start gap-2 text-xs">
          <input
            type="radio"
            name="session-permission"
            checked={allowListMode}
            onChange={() => setAllowListMode(true)}
            className="mt-0.5 accent-primary"
          />
          <span>
            Allow-list: the runner pre-approves only the tools listed below and never asks; a tool
            nothing approved is refused. Settings layers the runner does not control (a
            repository&apos;s <code>.claude/settings.json</code>, managed settings) can still
            pre-approve more. Use this where managed Claude Code settings disable bypass mode.
          </span>
        </label>
        {allowListMode && (
          <div className="space-y-1.5">
            <label htmlFor="session-permission-tools" className="text-xs font-medium">
              Allowed tools (one per line, e.g. <code>Read</code>, <code>Bash(git status)</code>)
            </label>
            <textarea
              id="session-permission-tools"
              rows={4}
              value={tools}
              onChange={(e) => setTools(e.target.value)}
              className={inputClass}
            />
          </div>
        )}
        {permissionError && <p className="text-xs text-destructive">{permissionError}</p>}
        <button
          type="button"
          onClick={() => void savePermission()}
          disabled={savingPermission}
          className="px-3 py-1.5 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md transition-colors disabled:opacity-50 text-xs"
        >
          {savingPermission ? "Saving..." : "Save session permissions"}
        </button>
      </div>
    </div>
  );
}
