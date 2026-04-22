import {
  RefreshCw,
  Wifi,
  WifiOff,
  Search,
  Monitor,
  Globe,
  Smartphone,
  FolderOpen,
  Wrench,
  ChevronDown,
  ChevronRight,
  Plus,
  Radio,
  Trash2,
} from "lucide-react";
import {
  useState,
  useCallback,
  useEffect,
  useMemo,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { DiscoveredApp, MobileDevice, DiscoveryResult, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";
import {
  useAppDiscovery,
  type DiscoveredApp as RegisteredDiscoveredApp,
  type RegisterAppPayload,
} from "@/hooks/useAppDiscovery";

// =============================================================================
// ProcessConfig type (from process manager)
// =============================================================================

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  health_port: number | null;
  category: string;
  enabled: boolean;
}

// =============================================================================
// DiscoveryPanel
// =============================================================================

interface DiscoveryPanelProps {
  onSelectApp?: (projectPath: string) => void;
  selectedProjectPath?: string;
}

export function DiscoveryPanel({ onSelectApp, selectedProjectPath }: DiscoveryPanelProps = {}) {
  const [result, setResult] = useState<DiscoveryResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [processConfigs, setProcessConfigs] = useState<ProcessConfig[]>([]);
  // Port → project path map built from process configs
  const [portToPath, setPortToPath] = useState<Map<number, string>>(new Map());
  const [projectsExpanded, setProjectsExpanded] = useState(true);

  // Registered-apps state comes from the phone-home registry.
  // The hook polls /ui-bridge/apps/registered every 5s and exposes register /
  // deregister helpers. Registered entries use camelCase field names (appId,
  // appName, basePath, ...) from the Rust `DiscoveredApp` struct, which is a
  // separate shape from the snake_case `DiscoveredApp` used by the scan path
  // in `./types` — the two coexist until scan is migrated.
  const {
    registeredApps,
    registerApp,
    deregisterApp,
    refreshRegistered,
    error: registeredError,
  } = useAppDiscovery();

  // Collapse "Your Projects" when a project is selected
  useEffect(() => {
    if (selectedProjectPath) {
      setProjectsExpanded(false);
    }
  }, [selectedProjectPath]);

  // Load process configs via Tauri IPC on mount
  useEffect(() => {
    let cancelled = false;
    invoke<ProcessConfig[]>("get_process_configs")
      .then((configs) => {
        if (cancelled) return;
        setProcessConfigs(configs);
        const map = new Map<number, string>();
        for (const cfg of configs) {
          if (cfg.health_port && cfg.cwd) {
            map.set(cfg.health_port, cfg.cwd);
          }
        }
        setPortToPath(map);
      })
      .catch(() => {
        // Ignore — process configs may not be available
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Get runner's own project path (available in dev mode) via useQuery
  const { data: runnerPathData } = useQuery<string | null>({
    queryKey: ["runner-project-path"],
    queryFn: async () => {
      const r = await fetch(`${getApiBase()}/ui-bridge/integration/runner-project`);
      const data: ApiResponse<string | null> = await r.json();
      return data.success && data.data ? data.data : null;
    },
    staleTime: Infinity,
    retry: false,
  });
  const runnerPath = runnerPathData ?? null;

  const scan = useCallback(async () => {
    setScanning(true);
    try {
      const resp = await fetch(`${getApiBase()}/ui-bridge/apps/scan`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      const data: ApiResponse<DiscoveryResult> = await resp.json();
      if (data.success && data.data) {
        setResult(data.data);
      }
    } catch (err) {
      console.error("Scan failed:", err);
    } finally {
      setScanning(false);
    }
  }, []);

  // Resolve project path for a discovered app: match port → process config cwd
  const resolveProjectPath = useCallback(
    (app: DiscoveredApp): string | undefined => {
      return portToPath.get(app.port);
    },
    [portToPath],
  );

  // Dedupe: drop scan-result entries whose appId already appears in the
  // registered list. Registered (phone-home) entries are authoritative — they
  // confirm the SDK is live in the running page, whereas scan entries only
  // prove a health endpoint responds.
  const registeredIds = useMemo(
    () => new Set(registeredApps.map((a) => a.appId)),
    [registeredApps],
  );
  const filteredResult: DiscoveryResult | null = useMemo(() => {
    if (!result) return null;
    // Scan payload uses snake_case (`app_id`) per ./types, while registered
    // uses camelCase (`appId`). Match both to be safe across the mismatch.
    const isRegistered = (app: DiscoveredApp): boolean => {
      const id = (app as DiscoveredApp & { appId?: string }).appId ?? app.app_id;
      return id ? registeredIds.has(id) : false;
    };
    return {
      ...result,
      web: result.web.filter((a) => !isRegistered(a)),
      desktop: result.desktop.filter((a) => !isRegistered(a)),
      // Mobile entries don't dedupe cleanly — leave them alone for now.
      mobile: result.mobile,
    };
  }, [result, registeredIds]);

  const hasResults =
    filteredResult &&
    (filteredResult.web.length > 0 ||
      filteredResult.desktop.length > 0 ||
      filteredResult.mobile.length > 0);
  const totalApps = filteredResult
    ? filteredResult.web.length +
      filteredResult.desktop.length +
      filteredResult.mobile.filter((d) => d.ui_bridge).length
    : 0;

  // Deduplicate: exclude process configs whose port already appears in scan results
  const scannedPorts = new Set<number>();
  if (result) {
    for (const app of [...result.web, ...result.desktop]) {
      scannedPorts.add(app.port);
    }
  }
  const unscannedProjects = processConfigs.filter(
    (cfg) => cfg.enabled && cfg.cwd && (!cfg.health_port || !scannedPorts.has(cfg.health_port)),
  );

  // Include the runner itself in dev mode (it's excluded from process configs)
  const showRunner = runnerPath && !unscannedProjects.some((p) => p.cwd === runnerPath);

  return (
    <div className="flex flex-col gap-4">
      {/* Known projects from setup wizard + runner itself */}
      {(unscannedProjects.length > 0 || showRunner) && (
        <div>
          <button
            onClick={() => setProjectsExpanded((v) => !v)}
            className="flex items-center gap-1.5 mb-2 w-full text-left group"
          >
            {projectsExpanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            )}
            <div className="flex-1">
              <h3 className="text-sm font-medium group-hover:text-foreground transition-colors">
                Your Projects
              </h3>
              {projectsExpanded && (
                <p className="text-xs text-muted-foreground mt-0.5">
                  Click a project to set up UI Bridge integration
                </p>
              )}
            </div>
            {!projectsExpanded && (
              <span className="text-[10px] text-muted-foreground/60">
                {(showRunner ? 1 : 0) + unscannedProjects.length} project
                {(showRunner ? 1 : 0) + unscannedProjects.length !== 1 ? "s" : ""}
              </span>
            )}
          </button>
          {projectsExpanded && (
            <div className="grid gap-2">
              {showRunner && (
                <ProjectCard
                  name="Qontinui Runner"
                  projectPath={runnerPath}
                  category="runner"
                  port={null}
                  onSelect={onSelectApp}
                />
              )}
              {unscannedProjects.map((cfg) => (
                <ProjectCard
                  key={cfg.id}
                  name={cfg.name}
                  projectPath={cfg.cwd}
                  category={cfg.category}
                  port={cfg.health_port}
                  onSelect={onSelectApp}
                />
              ))}
            </div>
          )}
        </div>
      )}

      {/* Running applications discovery */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium">Running Applications</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Scan for running applications with the UI Bridge SDK
          </p>
        </div>
        <button
          onClick={scan}
          disabled={scanning}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                     bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                     hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
        >
          {scanning ? (
            <RefreshCw className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <Search className="w-3.5 h-3.5" />
          )}
          {scanning ? "Scanning..." : "Scan"}
        </button>
      </div>

      {/* Empty state */}
      {!hasResults && !scanning && (
        <div className="text-center py-8 text-muted-foreground text-sm">
          {result
            ? "No running applications found. Make sure your dev servers are running."
            : 'Click "Scan" to discover running applications with UI Bridge.'}
        </div>
      )}

      {/* Scan results */}
      {hasResults && filteredResult && (
        <div className="flex flex-col gap-4">
          {filteredResult.web.length > 0 && (
            <AppSection
              icon={Globe}
              label="Web"
              apps={filteredResult.web}
              resolveProjectPath={resolveProjectPath}
              onSelectApp={onSelectApp}
            />
          )}
          {filteredResult.desktop.length > 0 && (
            <AppSection
              icon={Monitor}
              label="Desktop"
              apps={filteredResult.desktop}
              resolveProjectPath={resolveProjectPath}
              onSelectApp={onSelectApp}
            />
          )}
          {filteredResult.mobile.length > 0 && <MobileSection devices={filteredResult.mobile} />}
        </div>
      )}

      {result && (
        <p className="text-[10px] text-muted-foreground text-right">
          Scanned in {result.duration_ms}ms
          {totalApps > 0 && (
            <>
              {" · "}
              {totalApps} app{totalApps !== 1 ? "s" : ""} found
            </>
          )}
        </p>
      )}

      {/* Registered applications — populated by SDK phone-home. */}
      <RegisteredAppsSection
        apps={registeredApps}
        onSelectApp={onSelectApp}
        onRefresh={refreshRegistered}
        onDeregister={deregisterApp}
        error={registeredError}
      />

      {/* Add application — manual entry, for apps we can't auto-discover. */}
      <AddApplicationForm onRegister={registerApp} />
    </div>
  );
}

// =============================================================================
// ProjectCard — for known projects from setup wizard (always clickable)
// =============================================================================

function ProjectCard({
  name,
  projectPath,
  category,
  port,
  onSelect,
}: {
  name: string;
  projectPath: string;
  category: string;
  port: number | null;
  onSelect?: (projectPath: string) => void;
}) {
  return (
    <div
      onClick={onSelect ? () => onSelect(projectPath) : undefined}
      onKeyDown={
        onSelect ? (e) => (e.key === "Enter" || e.key === " ") && onSelect(projectPath) : undefined
      }
      role={onSelect ? "button" : undefined}
      tabIndex={onSelect ? 0 : undefined}
      className={`flex items-center gap-3 p-3 rounded-lg border border-border bg-card/50${
        onSelect
          ? " cursor-pointer hover:border-cyan-500/40 hover:bg-cyan-500/5 transition-colors"
          : ""
      }`}
    >
      <FolderOpen className="w-4 h-4 text-cyan-400 shrink-0" />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{name}</span>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
            {category}
          </span>
          {port && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
              :{port}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1 mt-1 text-xs text-muted-foreground">
          <span className="truncate">{projectPath}</span>
        </div>
      </div>
      {onSelect && <Wrench className="w-3.5 h-3.5 text-muted-foreground/40 shrink-0" />}
    </div>
  );
}

// =============================================================================
// AppSection / AppCard — for discovered running apps
// =============================================================================

function AppSection({
  icon: Icon,
  label,
  apps,
  resolveProjectPath,
  onSelectApp,
}: {
  icon: typeof Globe;
  label: string;
  apps: DiscoveredApp[];
  resolveProjectPath: (app: DiscoveredApp) => string | undefined;
  onSelectApp?: (projectPath: string) => void;
}) {
  return (
    <div>
      <div className="flex items-center gap-1.5 mb-2">
        <Icon className="w-3.5 h-3.5 text-muted-foreground" />
        <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
          {label}
        </span>
        <span className="text-[10px] text-muted-foreground">({apps.length})</span>
      </div>
      <div className="grid gap-2">
        {apps.map((app) => (
          <AppCard
            key={app.app_id}
            app={app}
            projectPath={resolveProjectPath(app)}
            onSelect={onSelectApp}
          />
        ))}
      </div>
    </div>
  );
}

function AppCard({
  app,
  projectPath,
  onSelect,
}: {
  app: DiscoveredApp;
  projectPath?: string;
  onSelect?: (projectPath: string) => void;
}) {
  const hasUiBridge = (app.capabilities?.length ?? 0) > 0;
  const clickable = !!projectPath && !!onSelect;

  return (
    <div
      onClick={clickable ? () => onSelect(projectPath) : undefined}
      onKeyDown={
        clickable ? (e) => (e.key === "Enter" || e.key === " ") && onSelect(projectPath) : undefined
      }
      role={clickable ? "button" : undefined}
      tabIndex={clickable ? 0 : undefined}
      className={`flex items-center gap-3 p-3 rounded-lg border border-border bg-card/50${
        clickable
          ? " cursor-pointer hover:border-cyan-500/40 hover:bg-cyan-500/5 transition-colors"
          : ""
      }`}
    >
      <div
        className={`w-2 h-2 rounded-full shrink-0 ${
          hasUiBridge ? "bg-green-500" : "bg-yellow-500"
        }`}
      />

      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{app.app_name}</span>
          {app.framework && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-400 font-medium">
              {app.framework}
            </span>
          )}
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
            :{app.port}
          </span>
        </div>
        <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
          <span className="truncate">{app.url}</span>
          {app.element_count != null && <span>{app.element_count} elements</span>}
          {app.version && <span>v{app.version}</span>}
        </div>
        {projectPath && (
          <div className="flex items-center gap-1 mt-1 text-[10px] text-muted-foreground/60">
            <FolderOpen className="w-3 h-3 shrink-0" />
            <span className="truncate">{projectPath}</span>
          </div>
        )}
        {!projectPath && (
          <div className="mt-1 text-[10px] text-muted-foreground/40 italic">
            No project path configured — add via Process Manager
          </div>
        )}
      </div>

      <div className="flex items-center gap-1.5 shrink-0">
        {hasUiBridge ? (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-green-500/10 text-green-400 font-medium">
            <Wifi className="w-3 h-3" />
            Integrated
          </span>
        ) : (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-yellow-500/10 text-yellow-400 font-medium">
            <WifiOff className="w-3 h-3" />
            Not Integrated
          </span>
        )}
      </div>
    </div>
  );
}

function MobileSection({ devices }: { devices: MobileDevice[] }) {
  return (
    <div>
      <div className="flex items-center gap-1.5 mb-2">
        <Smartphone className="w-3.5 h-3.5 text-muted-foreground" />
        <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
          Mobile
        </span>
        <span className="text-[10px] text-muted-foreground">({devices.length})</span>
      </div>
      <div className="grid gap-2">
        {devices.map((device) => (
          <MobileDeviceCard key={device.device_id} device={device} />
        ))}
      </div>
    </div>
  );
}

function MobileDeviceCard({ device }: { device: MobileDevice }) {
  const isOnline = device.status === "online";
  const hasUiBridge = !!device.ui_bridge;

  return (
    <div className="flex items-center gap-3 p-3 rounded-lg border border-border bg-card/50">
      <div
        className={`w-2 h-2 rounded-full shrink-0 ${
          hasUiBridge ? "bg-green-500" : isOnline ? "bg-yellow-500" : "bg-red-500"
        }`}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{device.model || device.device_id}</span>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
            {device.device_type}
          </span>
        </div>
        <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
          <span>{device.device_id}</span>
          {device.ui_bridge && device.ui_bridge.element_count != null && (
            <span>{device.ui_bridge.element_count} elements</span>
          )}
        </div>
      </div>
      <div className="flex items-center gap-1.5 shrink-0">
        {hasUiBridge ? (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-green-500/10 text-green-400 font-medium">
            <Wifi className="w-3 h-3" />
            Integrated
          </span>
        ) : isOnline ? (
          <span className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-full bg-yellow-500/10 text-yellow-400 font-medium">
            <WifiOff className="w-3 h-3" />
            No UI Bridge
          </span>
        ) : (
          <span className="text-[10px] px-2 py-0.5 rounded-full bg-red-500/10 text-red-400 font-medium">
            {device.status}
          </span>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// RegisteredAppsSection — apps that phoned home via the SDK
// =============================================================================

function RegisteredAppsSection({
  apps,
  onSelectApp,
  onRefresh,
  onDeregister,
  error,
}: {
  apps: RegisteredDiscoveredApp[];
  onSelectApp?: (projectPath: string) => void;
  onRefresh: () => Promise<void>;
  onDeregister: (appId: string) => Promise<boolean>;
  error: string | null;
}) {
  const [refreshing, setRefreshing] = useState(false);

  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  }, [onRefresh]);

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div>
          <h3 className="text-sm font-medium">Registered Applications</h3>
          <p className="text-xs text-muted-foreground mt-0.5">
            Apps that phoned home via the UI Bridge SDK (polled every 5s)
          </p>
        </div>
        <button
          onClick={handleRefresh}
          disabled={refreshing}
          className="flex items-center gap-1.5 px-2.5 py-1 text-[11px] font-medium rounded
                     bg-white/5 text-muted-foreground border border-border
                     hover:bg-white/10 hover:text-foreground disabled:opacity-50 transition-colors"
          title="Refresh registered apps"
        >
          <RefreshCw className={`w-3 h-3 ${refreshing ? "animate-spin" : ""}`} aria-hidden="true" />
          Refresh
        </button>
      </div>

      {error && (
        <div className="mb-2 text-[11px] text-red-400 bg-red-500/10 border border-red-500/20 rounded px-2 py-1">
          {error}
        </div>
      )}

      {apps.length === 0 ? (
        <div className="text-[11px] text-muted-foreground/70 italic py-2">
          No registered applications. Open an app that imports{" "}
          <code className="text-[10px] bg-white/5 px-1 py-0.5 rounded">@qontinui/ui-bridge</code>{" "}
          and it will appear here automatically.
        </div>
      ) : (
        <div className="grid gap-2">
          {apps.map((app) => (
            <RegisteredAppCard
              key={app.appId}
              app={app}
              onSelect={onSelectApp}
              onDeregister={onDeregister}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function RegisteredAppCard({
  app,
  onSelect,
  onDeregister,
}: {
  app: RegisteredDiscoveredApp;
  onSelect?: (projectPath: string) => void;
  onDeregister: (appId: string) => Promise<boolean>;
}) {
  const [removing, setRemoving] = useState(false);
  // Click-through uses the base URL as the "project path" proxy — matches the
  // scan cards' behavior (they pass project path into onSelectApp).
  const selectTarget = app.url;
  const clickable = !!onSelect && !!selectTarget;

  const handleDeregister = useCallback(
    async (e: ReactMouseEvent) => {
      e.stopPropagation();
      if (removing) return;
      setRemoving(true);
      try {
        await onDeregister(app.appId);
      } finally {
        setRemoving(false);
      }
    },
    [onDeregister, app.appId, removing],
  );

  return (
    <div
      onClick={clickable ? () => onSelect(selectTarget) : undefined}
      onKeyDown={
        clickable
          ? (e) => (e.key === "Enter" || e.key === " ") && onSelect(selectTarget)
          : undefined
      }
      role={clickable ? "button" : undefined}
      tabIndex={clickable ? 0 : undefined}
      className={`flex items-center gap-3 p-3 rounded-lg border border-border bg-card/50${
        clickable
          ? " cursor-pointer hover:border-cyan-500/40 hover:bg-cyan-500/5 transition-colors"
          : ""
      }`}
    >
      <Radio className="w-4 h-4 text-green-400 shrink-0" aria-hidden="true" />

      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-sm font-medium truncate">{app.appName}</span>
          {app.framework && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-400 font-medium">
              {app.framework}
            </span>
          )}
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground">
            {app.appType}
          </span>
          <span className="text-[10px] px-2 py-0.5 rounded-full bg-green-500/10 text-green-400 font-medium">
            registered
          </span>
        </div>
        <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
          <span className="truncate">{app.url}</span>
          {app.version && <span>v{app.version}</span>}
        </div>
        <div className="mt-0.5 text-[10px] text-muted-foreground/60 truncate">
          <code>{app.appId}</code>
        </div>
      </div>

      <button
        onClick={handleDeregister}
        disabled={removing}
        className="shrink-0 p-1.5 rounded text-muted-foreground/60 hover:text-red-400 hover:bg-red-500/10
                   disabled:opacity-50 transition-colors"
        title="Deregister this app"
        aria-label={`Deregister ${app.appName}`}
      >
        <Trash2 className="w-3.5 h-3.5" aria-hidden="true" />
      </button>
    </div>
  );
}

// =============================================================================
// AddApplicationForm — manual entry for apps we can't auto-discover
// =============================================================================

/**
 * Extract a reasonable `appId` default from a URL ("manual-hostname-port").
 * Falls back to "manual-app" if the URL can't be parsed.
 */
function defaultAppIdFromUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname || "app";
    const port =
      parsed.port ||
      (parsed.protocol === "https:" ? "443" : parsed.protocol === "http:" ? "80" : "");
    return port ? `manual-${host}-${port}` : `manual-${host}`;
  } catch {
    return "manual-app";
  }
}

/**
 * Build a human name for an app from its URL ("hostname:port").
 */
function defaultAppNameFromUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname || "app";
    return parsed.port ? `${host}:${parsed.port}` : host;
  } catch {
    return url;
  }
}

function AddApplicationForm({
  onRegister,
}: {
  onRegister: (payload: RegisterAppPayload) => Promise<RegisteredDiscoveredApp | null>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [url, setUrl] = useState("");
  const [appId, setAppId] = useState("");
  const [basePath, setBasePath] = useState("/ui-bridge");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<{ kind: "success" | "error"; message: string } | null>(null);

  const buildBaseUrl = useCallback((rawUrl: string, rawBasePath: string): string => {
    const trimmedBase = rawBasePath.trim().replace(/\/+$/, "");
    const trimmedUrl = rawUrl.trim().replace(/\/+$/, "");
    if (!trimmedBase) return trimmedUrl;
    return trimmedBase.startsWith("/")
      ? `${trimmedUrl}${trimmedBase}`
      : `${trimmedUrl}/${trimmedBase}`;
  }, []);

  const handleProbeAndAdd = useCallback(async () => {
    setStatus(null);
    if (!url.trim()) {
      setStatus({ kind: "error", message: "URL is required" });
      return;
    }
    setBusy(true);
    try {
      const baseUrl = buildBaseUrl(url, basePath);
      const healthUrl = `${baseUrl.replace(/\/+$/, "")}/health`;
      // Same-origin fetch from the runner frontend — will likely hit CORS for
      // cross-origin dev servers. If the app exposes CORS or runs on the same
      // origin (e.g. another Tauri webview on tauri://localhost), this works.
      // Otherwise: fall back to "Add without probing".
      let probed: {
        appId?: string;
        appName?: string;
        appType?: string;
        framework?: string;
        capabilities?: string[];
        version?: string;
      } = {};
      try {
        const response = await fetch(healthUrl, {
          method: "GET",
          headers: { Accept: "application/json" },
        });
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }
        const body = await response.json();
        // UI Bridge metadata block. The SDK emits it under `uiBridge`; some
        // older servers emit it at the top level — accept either.
        const meta = (body && (body.uiBridge ?? body)) as Partial<typeof probed>;
        probed = {
          appId: meta.appId,
          appName: meta.appName,
          appType: meta.appType,
          framework: meta.framework,
          capabilities: meta.capabilities,
          version: meta.version,
        };
      } catch (err) {
        setStatus({
          kind: "error",
          message: `Probe failed (${(err as Error).message}). Try "Add without probing".`,
        });
        return;
      }

      if (!probed.appId || !probed.appName || !probed.appType) {
        setStatus({
          kind: "error",
          message: "Probe succeeded but response is missing uiBridge metadata",
        });
        return;
      }

      const payload: RegisterAppPayload = {
        appId: appId.trim() || probed.appId,
        appName: probed.appName,
        appType: probed.appType,
        framework: probed.framework,
        baseUrl,
        capabilities: probed.capabilities ?? [],
        version: probed.version,
      };
      const registered = await onRegister(payload);
      if (registered) {
        setStatus({ kind: "success", message: `Registered "${registered.appName}"` });
        setUrl("");
        setAppId("");
      } else {
        setStatus({ kind: "error", message: "Registration failed — see console for details" });
      }
    } finally {
      setBusy(false);
    }
  }, [url, appId, basePath, buildBaseUrl, onRegister]);

  const handleAddWithoutProbing = useCallback(async () => {
    setStatus(null);
    if (!url.trim()) {
      setStatus({ kind: "error", message: "URL is required" });
      return;
    }
    setBusy(true);
    try {
      const baseUrl = buildBaseUrl(url, basePath);
      const derivedId = appId.trim() || defaultAppIdFromUrl(url);
      const payload: RegisterAppPayload = {
        appId: derivedId,
        appName: defaultAppNameFromUrl(url),
        appType: "other",
        baseUrl,
        capabilities: [],
      };
      // Reconciliation note: the runner-side registry is keyed by `appId`.
      // When the SDK later phones home with the same `appId`, that POST will
      // upsert and overwrite this placeholder with the real metadata — no
      // extra reconciliation code is needed on either side.
      const registered = await onRegister(payload);
      if (registered) {
        setStatus({
          kind: "success",
          message:
            "Added as a placeholder. If the SDK is installed in this app, it should update this entry shortly by phoning home.",
        });
        setUrl("");
        setAppId("");
      } else {
        setStatus({ kind: "error", message: "Registration failed — see console for details" });
      }
    } finally {
      setBusy(false);
    }
  }, [url, appId, basePath, buildBaseUrl, onRegister]);

  return (
    <div className="rounded-lg border border-border bg-card/30">
      <button
        onClick={() => setExpanded((v) => !v)}
        className="flex items-center gap-1.5 w-full text-left px-3 py-2 group"
      >
        {expanded ? (
          <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
        )}
        <Plus className="w-3.5 h-3.5 text-cyan-400 shrink-0" aria-hidden="true" />
        <span className="text-sm font-medium group-hover:text-foreground transition-colors">
          Add Application
        </span>
        {!expanded && (
          <span className="ml-auto text-[10px] text-muted-foreground/60">
            Manual registration for apps not auto-discovered
          </span>
        )}
      </button>

      {expanded && (
        <div className="px-3 pb-3 pt-1 flex flex-col gap-2">
          <div className="flex flex-col gap-1">
            <label className="text-[11px] text-muted-foreground" htmlFor="add-app-url">
              URL <span className="text-red-400">*</span>
            </label>
            <input
              id="add-app-url"
              type="url"
              placeholder="http://localhost:7000"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              disabled={busy}
              className="w-full px-2 py-1.5 text-xs rounded border border-border bg-background/60
                         focus:outline-none focus:border-cyan-500/50"
            />
          </div>

          <div className="flex gap-2">
            <div className="flex-1 flex flex-col gap-1">
              <label className="text-[11px] text-muted-foreground" htmlFor="add-app-id">
                App ID <span className="text-muted-foreground/60">(optional)</span>
              </label>
              <input
                id="add-app-id"
                type="text"
                placeholder={url ? defaultAppIdFromUrl(url) : "auto-derived from URL"}
                value={appId}
                onChange={(e) => setAppId(e.target.value)}
                disabled={busy}
                className="w-full px-2 py-1.5 text-xs rounded border border-border bg-background/60
                           focus:outline-none focus:border-cyan-500/50"
              />
            </div>
            <div className="flex-1 flex flex-col gap-1">
              <label className="text-[11px] text-muted-foreground" htmlFor="add-app-basepath">
                Base path
              </label>
              <input
                id="add-app-basepath"
                type="text"
                placeholder="/ui-bridge"
                value={basePath}
                onChange={(e) => setBasePath(e.target.value)}
                disabled={busy}
                className="w-full px-2 py-1.5 text-xs rounded border border-border bg-background/60
                           focus:outline-none focus:border-cyan-500/50"
              />
            </div>
          </div>

          <div className="flex items-center gap-2 mt-1">
            <button
              onClick={handleProbeAndAdd}
              disabled={busy || !url.trim()}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                         hover:bg-cyan-500/20 disabled:opacity-50 transition-colors"
            >
              {busy ? (
                <RefreshCw className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <Search className="w-3.5 h-3.5" />
              )}
              Probe and add
            </button>
            <button
              onClick={handleAddWithoutProbing}
              disabled={busy || !url.trim()}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded
                         bg-white/5 text-muted-foreground border border-border
                         hover:bg-white/10 hover:text-foreground disabled:opacity-50 transition-colors"
            >
              Add without probing
            </button>
          </div>

          {status && (
            <div
              className={`text-[11px] rounded px-2 py-1 border ${
                status.kind === "success"
                  ? "bg-green-500/10 text-green-400 border-green-500/20"
                  : "bg-red-500/10 text-red-400 border-red-500/20"
              }`}
            >
              {status.message}
            </div>
          )}

          <p className="text-[10px] text-muted-foreground/60 mt-0.5">
            "Probe and add" fetches{" "}
            <code className="text-[10px] bg-white/5 px-1 rounded">
              &lt;url&gt;&lt;basePath&gt;/health
            </code>{" "}
            and expects a <code className="text-[10px] bg-white/5 px-1 rounded">uiBridge</code>{" "}
            metadata block. Cross-origin probes may fail due to CORS — use "Add without probing" in
            that case.
          </p>
        </div>
      )}
    </div>
  );
}
