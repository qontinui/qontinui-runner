/**
 * AiClientsTab — one card per supported AI client (Claude CLI, Claude
 * Desktop, Cursor, Continue, Cline). Detection + connect/disconnect is
 * driven by `/wrappers/clients/*`.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Loader2,
  Plug,
  RefreshCw,
} from "lucide-react";
import {
  type AiClientId,
  type ClientInfo,
  connectAiClient,
  disconnectAiClient,
  listAiClients,
} from "@/lib/wrappers/clients-api";

const MCP_KEY = "qontinui-wrappers";

type SuccessFlash = { id: AiClientId; message: string };

export function AiClientsTab() {
  const [clients, setClients] = useState<ClientInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [actionError, setActionError] = useState<Record<string, string>>({});
  const [flash, setFlash] = useState<SuccessFlash | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const list = await listAiClients();
      if (!mountedRef.current) return;
      setClients(list);
      setLoadError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setLoadError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Auto-dismiss the success flash.
  useEffect(() => {
    if (!flash) return;
    const t = window.setTimeout(() => setFlash(null), 3_000);
    return () => window.clearTimeout(t);
  }, [flash]);

  const setBusyFor = (id: AiClientId, value: boolean) => {
    setBusy((prev) => ({ ...prev, [id]: value }));
  };

  const clearError = (id: AiClientId) => {
    setActionError((prev) => {
      if (!(id in prev)) return prev;
      const next = { ...prev };
      delete next[id];
      return next;
    });
  };

  const handleConnect = useCallback(
    async (info: ClientInfo, label: string) => {
      setBusyFor(info.id, true);
      clearError(info.id);
      try {
        await connectAiClient(info.id);
        if (!mountedRef.current) return;
        setFlash({ id: info.id, message: `${info.display_name} ${label.toLowerCase()}ed` });
        await load();
      } catch (err) {
        if (!mountedRef.current) return;
        setActionError((prev) => ({
          ...prev,
          [info.id]: err instanceof Error ? err.message : String(err),
        }));
      } finally {
        if (mountedRef.current) setBusyFor(info.id, false);
      }
    },
    [load],
  );

  const handleDisconnect = useCallback(
    async (info: ClientInfo) => {
      setBusyFor(info.id, true);
      clearError(info.id);
      try {
        await disconnectAiClient(info.id);
        if (!mountedRef.current) return;
        setFlash({ id: info.id, message: `${info.display_name} disconnected` });
        await load();
      } catch (err) {
        if (!mountedRef.current) return;
        setActionError((prev) => ({
          ...prev,
          [info.id]: err instanceof Error ? err.message : String(err),
        }));
      } finally {
        if (mountedRef.current) setBusyFor(info.id, false);
      }
    },
    [load],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin" />
        <span className="text-sm">Loading AI clients…</span>
      </div>
    );
  }

  if (loadError) {
    return (
      <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-5 flex items-start gap-3">
        <AlertTriangle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <p className="text-sm font-medium text-red-400">Failed to load AI clients</p>
          <p className="mt-1 text-xs text-muted-foreground break-words">{loadError}</p>
          <button
            onClick={() => {
              setLoadError(null);
              setLoading(true);
              load();
            }}
            className="mt-3 inline-flex items-center gap-1.5 px-3 py-1 rounded-md border border-border bg-card text-xs text-foreground hover:bg-muted/30 transition-colors"
          >
            <RefreshCw className="w-3.5 h-3.5" />
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {flash && (
        <div className="rounded-md border border-emerald-500/40 bg-emerald-500/5 px-4 py-2 flex items-center gap-2 text-sm text-emerald-400">
          <CheckCircle2 className="w-4 h-4 shrink-0" />
          <span>{flash.message}</span>
        </div>
      )}
      <div className="grid gap-4 grid-cols-1 md:grid-cols-2">
        {clients.map((info) => (
          <ClientCard
            key={info.id}
            info={info}
            busy={Boolean(busy[info.id])}
            error={actionError[info.id]}
            expanded={Boolean(expanded[info.id])}
            onToggleExpand={() => setExpanded((prev) => ({ ...prev, [info.id]: !prev[info.id] }))}
            onConnect={() => handleConnect(info, "Connect")}
            onReconnect={() => handleConnect(info, "Reconnect")}
            onDisconnect={() => handleDisconnect(info)}
          />
        ))}
      </div>
    </div>
  );
}

interface ClientCardProps {
  info: ClientInfo;
  busy: boolean;
  error?: string;
  expanded: boolean;
  onToggleExpand: () => void;
  onConnect: () => void;
  onReconnect: () => void;
  onDisconnect: () => void;
}

function ClientCard({
  info,
  busy,
  error,
  expanded,
  onToggleExpand,
  onConnect,
  onReconnect,
  onDisconnect,
}: ClientCardProps) {
  const subtitle = info.installed ? "Detected" : "Not detected on this machine";

  return (
    <div className="rounded-xl border border-border bg-card p-5 shadow-sm flex flex-col gap-3">
      <div className="flex items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-cyan-500/30 bg-cyan-500/10">
          <Plug className="w-4 h-4 text-cyan-400" />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="text-sm font-semibold text-foreground">{info.display_name}</h3>
          <p className="mt-0.5 text-xs text-muted-foreground">{subtitle}</p>
        </div>
        <StatusPill info={info} />
      </div>

      <div className="flex items-center justify-between gap-2">
        <ActionButton
          info={info}
          busy={busy}
          onConnect={onConnect}
          onReconnect={onReconnect}
          onDisconnect={onDisconnect}
        />
        <button
          onClick={onToggleExpand}
          className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          {expanded ? (
            <ChevronDown className="w-3.5 h-3.5" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" />
          )}
          Details
        </button>
      </div>

      {error && (
        <div className="rounded-md border border-red-500/40 bg-red-500/5 px-3 py-2 flex items-start gap-2">
          <AlertTriangle className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5" />
          <p className="text-xs text-red-400 break-words">{error}</p>
        </div>
      )}

      {expanded && <DetailsPanel info={info} />}
    </div>
  );
}

function StatusPill({ info }: { info: ClientInfo }) {
  if (!info.installed) {
    return (
      <span className="inline-flex items-center rounded-full border border-border bg-muted/30 px-2 py-0.5 text-[10px] font-medium text-muted-foreground uppercase tracking-wider whitespace-nowrap">
        Not detected
      </span>
    );
  }
  switch (info.connection.kind) {
    case "Connected":
      return (
        <span className="inline-flex items-center rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-400 uppercase tracking-wider whitespace-nowrap">
          Connected
        </span>
      );
    case "Drifted":
      return (
        <span className="inline-flex items-center rounded-full border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-400 uppercase tracking-wider whitespace-nowrap">
          Connected (out of date)
        </span>
      );
    case "NotConnected":
      return (
        <span className="inline-flex items-center rounded-full border border-border bg-muted/30 px-2 py-0.5 text-[10px] font-medium text-muted-foreground uppercase tracking-wider whitespace-nowrap">
          Not connected
        </span>
      );
    case "ConfigParseError":
      return (
        <span className="inline-flex items-center rounded-full border border-red-500/40 bg-red-500/10 px-2 py-0.5 text-[10px] font-medium text-red-400 uppercase tracking-wider whitespace-nowrap">
          Invalid JSON
        </span>
      );
  }
}

interface ActionButtonProps {
  info: ClientInfo;
  busy: boolean;
  onConnect: () => void;
  onReconnect: () => void;
  onDisconnect: () => void;
}

function ActionButton({ info, busy, onConnect, onReconnect, onDisconnect }: ActionButtonProps) {
  const baseClass =
    "inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-colors disabled:opacity-50";

  if (!info.installed) {
    return (
      <button
        disabled
        title={`${info.display_name} not found on this machine`}
        className={`${baseClass} border border-border bg-muted/30 text-muted-foreground cursor-not-allowed`}
      >
        Connect
      </button>
    );
  }

  const spinner = busy ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : null;

  switch (info.connection.kind) {
    case "Connected":
      return (
        <button
          onClick={onDisconnect}
          disabled={busy}
          className={`${baseClass} border border-border bg-card text-foreground hover:bg-muted/30`}
        >
          {spinner}
          {busy ? "Disconnecting…" : "Disconnect"}
        </button>
      );
    case "Drifted":
      return (
        <button
          onClick={onReconnect}
          disabled={busy}
          className={`${baseClass} bg-gradient-to-r from-amber-600 to-orange-600 text-white hover:from-amber-500 hover:to-orange-500`}
        >
          {spinner}
          {busy ? "Reconnecting…" : "Reconnect"}
        </button>
      );
    case "NotConnected":
      return (
        <button
          onClick={onConnect}
          disabled={busy}
          className={`${baseClass} bg-gradient-to-r from-cyan-600 to-purple-600 text-white hover:from-cyan-500 hover:to-purple-500`}
        >
          {spinner}
          {busy ? "Connecting…" : "Connect"}
        </button>
      );
    case "ConfigParseError":
      return (
        <button
          disabled
          title={`Fix ${info.connection.path} first`}
          className={`${baseClass} border border-border bg-muted/30 text-muted-foreground cursor-not-allowed`}
        >
          Connect
        </button>
      );
  }
}

function DetailsPanel({ info }: { info: ClientInfo }) {
  const driftedCommand =
    info.connection.kind === "Drifted" ? info.connection.current_command : null;
  const driftedPort = info.connection.kind === "Drifted" ? info.connection.current_port : null;
  const parseErrorPath =
    info.connection.kind === "ConfigParseError" ? info.connection.path : null;

  return (
    <div className="rounded-md border border-border bg-background/40 p-3 text-xs space-y-3">
      <div>
        <span className="font-medium text-muted-foreground">Config file:</span>{" "}
        <span className="font-mono text-foreground break-all">{info.config_path ?? "—"}</span>
      </div>

      {parseErrorPath && (
        <div className="rounded border border-red-500/30 bg-red-500/5 p-2 space-y-1">
          <p className="font-medium text-red-400">Config error:</p>
          <p>
            <span className="font-mono break-all text-foreground">{parseErrorPath}</span>
          </p>
          <p className="text-[11px] text-muted-foreground">
            Open this file and fix the JSON, or delete it to start fresh.
          </p>
        </div>
      )}

      <div>
        <p className="font-medium text-muted-foreground mb-1">
          MCP server entry (under <span className="font-mono">mcpServers</span>):
        </p>
        <pre className="rounded bg-muted/30 p-2 overflow-x-auto font-mono text-[11px] text-foreground">
          {`"${MCP_KEY}": {
  "command": "<launcher path>",
  "args": [],
  "env": {
    "QONTINUI_PRIMARY_PORT": "<port>"
  }
}`}
        </pre>
        <p className="mt-1 text-[11px] text-muted-foreground">
          The runner fills in its own stable launcher path and current port.
        </p>
      </div>

      {driftedCommand && (
        <div className="rounded border border-amber-500/30 bg-amber-500/5 p-2 space-y-1">
          <p className="font-medium text-amber-400">Currently in config (out of date):</p>
          <p>
            <span className="text-muted-foreground">command:</span>{" "}
            <span className="font-mono break-all text-foreground">{driftedCommand}</span>
          </p>
          {driftedPort !== null && (
            <p>
              <span className="text-muted-foreground">port:</span>{" "}
              <span className="font-mono text-foreground">{driftedPort}</span>
            </p>
          )}
          <p className="text-[11px] text-muted-foreground">
            Reconnect overwrites these with the runner's current values.
          </p>
        </div>
      )}
    </div>
  );
}
