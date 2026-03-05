import { Play, Square, ExternalLink, RefreshCw, Plug } from "lucide-react";
import { useState, useEffect, useCallback } from "react";
import type { ProxyInstance, InjectResponse, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";

// Reuse the ProxyInfo shape from the API (ProxyInstance minus shutdown_tx)
type ProxyInfo = Omit<ProxyInstance, "shutdown_tx">;

interface RuntimeInjectionPanelProps {
  prefillUrl?: string | null;
  onPrefillConsumed?: () => void;
}

export function RuntimeInjectionPanel({
  prefillUrl,
  onPrefillConsumed,
}: RuntimeInjectionPanelProps) {
  const [targetUrl, setTargetUrl] = useState("");
  const [label, setLabel] = useState("");

  // Apply prefill URL when it changes
  useEffect(() => {
    if (prefillUrl) {
      setTargetUrl(prefillUrl);
      onPrefillConsumed?.();
    }
  }, [prefillUrl, onPrefillConsumed]);
  const [proxies, setProxies] = useState<ProxyInfo[]>([]);
  const [injecting, setInjecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchProxies = useCallback(async () => {
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/proxies`);
      const data: ApiResponse<ProxyInfo[]> = await resp.json();
      if (data.success && data.data) {
        setProxies(data.data);
      }
    } catch {
      // Silently ignore fetch errors during polling
    }
  }, []);

  useEffect(() => {
    fetchProxies();
    const interval = setInterval(fetchProxies, 5000);
    return () => clearInterval(interval);
  }, [fetchProxies]);

  const inject = useCallback(async () => {
    if (!targetUrl.trim()) return;
    setInjecting(true);
    setError(null);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/integration/inject`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          target_url: targetUrl.trim(),
          label: label.trim() || undefined,
        }),
      });
      const data: ApiResponse<InjectResponse> = await resp.json();
      if (data.success) {
        setTargetUrl("");
        setLabel("");
        fetchProxies();
      } else {
        setError(data.error || "Failed to start proxy");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to inject");
    } finally {
      setInjecting(false);
    }
  }, [targetUrl, label, fetchProxies]);

  const stopProxy = useCallback(
    async (port: number) => {
      try {
        await fetch(`${getApiBase()}/ui-bridge/integration/inject/${port}`, {
          method: "DELETE",
        });
        fetchProxies();
      } catch (err) {
        console.error("Failed to stop proxy:", err);
      }
    },
    [fetchProxies],
  );

  return (
    <div className="flex flex-col gap-4">
      {/* Inject form */}
      <div className="p-4 rounded-lg border border-border bg-card/50">
        <h3 className="text-sm font-medium mb-3">Start Runtime Injection</h3>
        <p className="text-xs text-muted-foreground mb-3">
          Create a reverse proxy that injects the UI Bridge SDK into any running web app — no code
          changes required.
        </p>
        <div className="flex gap-2">
          <input
            type="text"
            value={targetUrl}
            onChange={(e) => setTargetUrl(e.target.value)}
            placeholder="http://localhost:3000"
            className="flex-1 px-3 py-1.5 text-sm rounded border border-border bg-background
                       placeholder:text-muted-foreground focus:outline-none focus:border-cyan-500/50"
          />
          <input
            type="text"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="Label (optional)"
            className="w-40 px-3 py-1.5 text-sm rounded border border-border bg-background
                       placeholder:text-muted-foreground focus:outline-none focus:border-cyan-500/50"
          />
          <button
            onClick={inject}
            disabled={injecting || !targetUrl.trim()}
            className="flex items-center gap-1.5 px-4 py-1.5 text-sm font-medium rounded
                       bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                       hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
          >
            {injecting ? (
              <RefreshCw className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Play className="w-3.5 h-3.5" />
            )}
            Inject
          </button>
        </div>
        {error && <p className="text-xs text-red-400 mt-2">{error}</p>}
      </div>

      {/* Active proxies */}
      <div>
        <div className="flex items-center justify-between mb-2">
          <h3 className="text-sm font-medium">Active Proxies</h3>
          <button
            onClick={fetchProxies}
            className="text-muted-foreground hover:text-foreground transition-colors"
          >
            <RefreshCw className="w-3.5 h-3.5" />
          </button>
        </div>

        {proxies.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground text-sm">
            No active proxies. Inject a target URL above to get started.
          </div>
        ) : (
          <div className="border border-border rounded-lg overflow-hidden">
            <table className="w-full text-sm">
              <thead>
                <tr className="bg-card/80 border-b border-border">
                  <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                    Label
                  </th>
                  <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                    Target
                  </th>
                  <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                    Proxy URL
                  </th>
                  <th className="text-left px-3 py-2 text-xs font-medium text-muted-foreground">
                    Status
                  </th>
                  <th className="text-right px-3 py-2 text-xs font-medium text-muted-foreground">
                    Actions
                  </th>
                </tr>
              </thead>
              <tbody>
                {proxies.map((proxy) => (
                  <tr key={proxy.proxy_port} className="border-b border-border/50 last:border-0">
                    <td className="px-3 py-2 text-foreground">{proxy.label}</td>
                    <td className="px-3 py-2 text-muted-foreground truncate max-w-[200px]">
                      {proxy.target_url}
                    </td>
                    <td className="px-3 py-2">
                      <code className="text-xs text-cyan-400">{proxy.proxy_url}</code>
                    </td>
                    <td className="px-3 py-2">
                      <span className="inline-flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded-full bg-green-500/10 text-green-400 font-medium">
                        <span className="w-1.5 h-1.5 rounded-full bg-green-500" />
                        {proxy.status}
                      </span>
                    </td>
                    <td className="px-3 py-2">
                      <div className="flex items-center justify-end gap-1">
                        <button
                          onClick={() => window.open(proxy.proxy_url, "_blank")}
                          className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
                          title="Open proxy URL"
                        >
                          <ExternalLink className="w-3.5 h-3.5" />
                        </button>
                        <button
                          onClick={async () => {
                            try {
                              await fetch(`${getApiBase()}/ui-bridge/sdk/connect`, {
                                method: "POST",
                                headers: { "Content-Type": "application/json" },
                                body: JSON.stringify({ url: proxy.proxy_url }),
                              });
                            } catch {
                              /* ignore connection errors */
                            }
                          }}
                          className="p-1 rounded text-muted-foreground hover:text-cyan-400 hover:bg-cyan-500/10 transition-colors"
                          title="Connect SDK client to this proxy"
                        >
                          <Plug className="w-3.5 h-3.5" />
                        </button>
                        <button
                          onClick={() => stopProxy(proxy.proxy_port)}
                          className="p-1 rounded text-muted-foreground hover:text-red-400 hover:bg-red-500/10 transition-colors"
                          title="Stop proxy"
                        >
                          <Square className="w-3.5 h-3.5" />
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
