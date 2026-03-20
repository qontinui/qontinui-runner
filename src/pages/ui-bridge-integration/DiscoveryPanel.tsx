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
} from "lucide-react";
import { useState, useCallback, useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { DiscoveredApp, MobileDevice, DiscoveryResult, ApiResponse } from "./types";
import { getApiBase } from "@/lib/runner-api";

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

  const hasResults =
    result && (result.web.length > 0 || result.desktop.length > 0 || result.mobile.length > 0);
  const totalApps = result
    ? result.web.length + result.desktop.length + result.mobile.filter((d) => d.ui_bridge).length
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
      {hasResults && (
        <div className="flex flex-col gap-4">
          {result.web.length > 0 && (
            <AppSection
              icon={Globe}
              label="Web"
              apps={result.web}
              resolveProjectPath={resolveProjectPath}
              onSelectApp={onSelectApp}
            />
          )}
          {result.desktop.length > 0 && (
            <AppSection
              icon={Monitor}
              label="Desktop"
              apps={result.desktop}
              resolveProjectPath={resolveProjectPath}
              onSelectApp={onSelectApp}
            />
          )}
          {result.mobile.length > 0 && <MobileSection devices={result.mobile} />}
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
