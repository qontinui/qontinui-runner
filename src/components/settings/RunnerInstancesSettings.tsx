/**
 * RunnerInstancesSettings.tsx
 *
 * Settings panel for managing secondary runner instances.
 * Each instance runs on its own port and gets its own Tauri window.
 *
 * Phase 2a: per-instance spawn placement editor.
 *   The supervisor's spawn-test endpoint round-robins through enabled
 *   placements (slot 0 = primary), so the order shown in the banner
 *   matches the order the supervisor will use for temp runners.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Monitor, Plus, Trash2, Play, Square, ChevronDown, ChevronRight } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getApiPort } from "@/lib/runner-api";
import type { LogFunction } from "./types";

// ---------------------------------------------------------------------------
// Types mirroring the Rust shapes from `commands::instances` /
// `settings::SpawnPlacement`.
// ---------------------------------------------------------------------------

type MonitorDescriptor =
  | { kind: "index"; index: number }
  | { kind: "position"; position: "primary" | "left" | "right" | "center" }
  | { kind: "name"; name: string };

interface SpawnPlacement {
  monitor: MonitorDescriptor;
  x: number;
  y: number;
  width?: number | null;
  height?: number | null;
}

interface ResolvedPlacement {
  global_x: number;
  global_y: number;
  width: number;
  height: number;
  monitor_label: string;
}

interface MonitorEntry {
  index: number;
  name: string | null;
  position_label: "primary" | "left" | "right" | "center";
  x: number;
  y: number;
  width: number;
  height: number;
  scale_factor: number;
  is_primary: boolean;
}

interface MonitorList {
  count: number;
  monitors: MonitorEntry[];
}

interface InstanceStatus {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
  source?: "local" | "registered" | "configured" | "discovered";
  registry_status?: string;
  running_tasks?: number;
  spawn_placement?: SpawnPlacement | null;
}

interface RunnerInstancesSettingsProps {
  onLog: LogFunction;
}

// ---------------------------------------------------------------------------
// Editor state for the placement form. We keep numeric fields as strings
// so the user can type negative numbers / clear the field mid-edit without
// React clobbering the input.
// ---------------------------------------------------------------------------

interface PlacementDraft {
  enabled: boolean;
  /** "primary" | "left" | "right" | "center" — the position role we'll save. */
  position: "primary" | "left" | "right" | "center";
  x: string;
  y: string;
  width: string;
  height: string;
}

const DEFAULT_DRAFT: PlacementDraft = {
  enabled: false,
  position: "primary",
  x: "0",
  y: "0",
  width: "",
  height: "",
};

function placementToDraft(placement: SpawnPlacement | null | undefined): PlacementDraft {
  if (!placement) return { ...DEFAULT_DRAFT };
  let position: PlacementDraft["position"] = "primary";
  if (placement.monitor.kind === "position") {
    position = placement.monitor.position;
  }
  // For index/name kinds we still surface them as "primary" so users get a
  // sensible default — committing the form re-saves as kind "position".
  return {
    enabled: true,
    position,
    x: String(placement.x),
    y: String(placement.y),
    width: placement.width != null ? String(placement.width) : "",
    height: placement.height != null ? String(placement.height) : "",
  };
}

function draftToPlacement(draft: PlacementDraft): SpawnPlacement | null {
  if (!draft.enabled) return null;
  const x = Number.parseInt(draft.x, 10);
  const y = Number.parseInt(draft.y, 10);
  if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
  const width = draft.width.trim() === "" ? null : Number.parseInt(draft.width, 10);
  const height = draft.height.trim() === "" ? null : Number.parseInt(draft.height, 10);
  return {
    monitor: { kind: "position", position: draft.position },
    x,
    y,
    width: width != null && Number.isFinite(width) ? width : null,
    height: height != null && Number.isFinite(height) ? height : null,
  };
}

/** Find the monitor entry that matches the chosen position role.
 *  Falls back to the primary monitor when the role isn't present. */
function findMonitorForRole(
  monitors: MonitorEntry[],
  role: PlacementDraft["position"],
): MonitorEntry | null {
  if (monitors.length === 0) return null;
  if (role === "primary") {
    return monitors.find((m) => m.is_primary) ?? monitors[0];
  }
  return (
    monitors.find((m) => m.position_label === role) ??
    monitors.find((m) => m.is_primary) ??
    monitors[0]
  );
}

function logicalDims(m: MonitorEntry | null): { width: number; height: number } | null {
  if (!m) return null;
  // Logical CSS pixels = physical / scale. Round to int.
  return {
    width: Math.round(m.width / m.scale_factor),
    height: Math.round(m.height / m.scale_factor),
  };
}

// ---------------------------------------------------------------------------
// Sub-component: PlacementEditor
// Renders the monitor dropdown, x/y/w/h inputs, presets, live preview,
// and a "Clear placement" button. Owned by parent via {draft, onChange}.
// ---------------------------------------------------------------------------

interface PlacementEditorProps {
  draft: PlacementDraft;
  onChange: (next: PlacementDraft) => void;
  monitors: MonitorEntry[];
  onLog: LogFunction;
}

function PlacementEditor({ draft, onChange, monitors, onLog }: PlacementEditorProps) {
  const [preview, setPreview] = useState<ResolvedPlacement | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);

  // Debounced live preview
  useEffect(() => {
    if (!draft.enabled) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    const placement = draftToPlacement(draft);
    if (!placement) {
      setPreview(null);
      setPreviewError("Enter valid X/Y values");
      return;
    }
    if (debounceRef.current != null) {
      window.clearTimeout(debounceRef.current);
    }
    debounceRef.current = window.setTimeout(() => {
      void (async () => {
        try {
          const resolved = await invoke<ResolvedPlacement>("preview_spawn_placement", {
            placement,
          });
          setPreview(resolved);
          setPreviewError(null);
        } catch (err) {
          setPreview(null);
          setPreviewError(String(err));
        }
      })();
    }, 300);
    return () => {
      if (debounceRef.current != null) {
        window.clearTimeout(debounceRef.current);
        debounceRef.current = null;
      }
    };
  }, [draft]);

  const selectedMonitor = useMemo(
    () => findMonitorForRole(monitors, draft.position),
    [monitors, draft.position],
  );

  // Available position roles, deduplicated against the actual monitor set.
  // We always offer "primary"; we offer "left"/"right"/"center" only if a
  // monitor in the current layout actually carries that label.
  const availableRoles = useMemo<PlacementDraft["position"][]>(() => {
    const roles = new Set<PlacementDraft["position"]>(["primary"]);
    for (const m of monitors) {
      roles.add(m.position_label);
    }
    return Array.from(roles);
  }, [monitors]);

  const setField = <K extends keyof PlacementDraft>(key: K, value: PlacementDraft[K]) => {
    onChange({ ...draft, [key]: value });
  };

  const usePresetCentered = () => {
    const m = selectedMonitor;
    const dims = logicalDims(m);
    if (!dims) {
      onLog("warning", "No monitor info available for centering");
      return;
    }
    const w = Number.parseInt(draft.width, 10);
    const h = Number.parseInt(draft.height, 10);
    const useW = Number.isFinite(w) ? w : 1920;
    const useH = Number.isFinite(h) ? h : 1080;
    onChange({
      ...draft,
      x: String(Math.max(0, Math.round((dims.width - useW) / 2))),
      y: String(Math.max(0, Math.round((dims.height - useH) / 2))),
    });
  };

  const usePresetTopLeft = () => {
    onChange({ ...draft, x: "0", y: "0" });
  };

  const usePresetFull = () => {
    const dims = logicalDims(selectedMonitor);
    if (!dims) {
      onLog("warning", "No monitor info available for full placement");
      return;
    }
    onChange({
      ...draft,
      x: "0",
      y: "0",
      width: String(dims.width),
      height: String(dims.height),
    });
  };

  if (!draft.enabled) {
    return (
      <div className="mt-2 pt-2 border-t border-border/60">
        <button
          onClick={() => onChange({ ...DEFAULT_DRAFT, enabled: true })}
          className="text-xs px-2 py-1 rounded border border-border hover:bg-accent transition-colors"
        >
          + Add spawn placement
        </button>
        <p className="text-[10px] text-muted-foreground mt-1">
          No placement — runner uses OS-default window position.
        </p>
      </div>
    );
  }

  return (
    <div className="mt-2 pt-2 border-t border-border/60 space-y-2">
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium">Spawn placement</span>
        <button
          onClick={() => onChange({ ...DEFAULT_DRAFT, enabled: false })}
          className="text-[10px] px-2 py-0.5 rounded border border-border text-muted-foreground hover:bg-accent transition-colors"
          title="Clear placement"
        >
          Clear placement
        </button>
      </div>

      <div className="flex items-center gap-2">
        <label className="text-[11px] text-muted-foreground w-14 shrink-0">Monitor</label>
        <select
          value={draft.position}
          onChange={(e) => setField("position", e.target.value as PlacementDraft["position"])}
          className="flex-1 px-2 py-1 text-xs rounded border border-border bg-background"
        >
          {availableRoles.map((role) => {
            const m = findMonitorForRole(monitors, role);
            const label = m
              ? `${role} (${Math.round(m.width / m.scale_factor)}x${Math.round(m.height / m.scale_factor)} @ ${m.scale_factor}x)${m.name ? ` ${m.name}` : ""}`
              : role;
            return (
              <option key={role} value={role}>
                {label}
              </option>
            );
          })}
        </select>
      </div>

      <div className="grid grid-cols-4 gap-2">
        <NumField label="X" value={draft.x} onChange={(v) => setField("x", v)} placeholder="0" />
        <NumField label="Y" value={draft.y} onChange={(v) => setField("y", v)} placeholder="0" />
        <NumField
          label="W"
          value={draft.width}
          onChange={(v) => setField("width", v)}
          placeholder="1920"
        />
        <NumField
          label="H"
          value={draft.height}
          onChange={(v) => setField("height", v)}
          placeholder="1080"
        />
      </div>

      <div className="flex flex-wrap gap-1">
        <button
          onClick={usePresetTopLeft}
          className="text-[10px] px-2 py-0.5 rounded border border-border hover:bg-accent transition-colors"
        >
          Use top-left
        </button>
        <button
          onClick={usePresetCentered}
          className="text-[10px] px-2 py-0.5 rounded border border-border hover:bg-accent transition-colors"
        >
          Use centered
        </button>
        <button
          onClick={usePresetFull}
          className="text-[10px] px-2 py-0.5 rounded border border-border hover:bg-accent transition-colors"
        >
          Use full
        </button>
      </div>

      {/* Live preview */}
      <div className="text-[11px] text-muted-foreground font-mono">
        {previewError ? (
          <span className="text-destructive">Preview failed: {previewError}</span>
        ) : preview ? (
          <span>
            → global ({preview.global_x}, {preview.global_y}) {preview.width}×{preview.height} on{" "}
            {preview.monitor_label}
          </span>
        ) : (
          <span>resolving…</span>
        )}
      </div>
    </div>
  );
}

interface NumFieldProps {
  label: string;
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
}

function NumField({ label, value, onChange, placeholder }: NumFieldProps) {
  return (
    <label className="flex flex-col gap-0.5">
      <span className="text-[10px] text-muted-foreground">{label}</span>
      <input
        type="text"
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="px-1.5 py-1 text-xs rounded border border-border bg-background"
      />
    </label>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: InstanceRow
// ---------------------------------------------------------------------------

interface InstanceRowProps {
  instance: InstanceStatus;
  monitors: MonitorEntry[];
  actionInProgress: string | null;
  onLaunch: (instance: InstanceStatus) => void;
  onStop: (instance: InstanceStatus) => void;
  onDelete: (instance: InstanceStatus) => void;
  onSavePlacement: (instance: InstanceStatus, placement: SpawnPlacement | null) => void;
  onLog: LogFunction;
}

function InstanceRow({
  instance,
  monitors,
  actionInProgress,
  onLaunch,
  onStop,
  onDelete,
  onSavePlacement,
  onLog,
}: InstanceRowProps) {
  const isConfigured = instance.source !== "registered";
  const [draft, setDraft] = useState<PlacementDraft>(() =>
    placementToDraft(instance.spawn_placement),
  );
  const [expanded, setExpanded] = useState(false);

  // Resync draft when the saved placement changes (e.g. after another save).
  // We compare via JSON to avoid stomping the user's in-flight edits.
  const lastSavedRef = useRef<string>(JSON.stringify(instance.spawn_placement ?? null));
  useEffect(() => {
    const incoming = JSON.stringify(instance.spawn_placement ?? null);
    if (incoming !== lastSavedRef.current) {
      lastSavedRef.current = incoming;
      setDraft(placementToDraft(instance.spawn_placement));
    }
  }, [instance.spawn_placement]);

  const dirty = useMemo(() => {
    const current = draftToPlacement(draft);
    const saved = instance.spawn_placement ?? null;
    return JSON.stringify(current) !== JSON.stringify(saved);
  }, [draft, instance.spawn_placement]);

  const handleSave = () => {
    onSavePlacement(instance, draftToPlacement(draft));
  };

  return (
    <div className="p-3 rounded-lg border border-border bg-card">
      <div className="flex items-center gap-3">
        <div
          className={`w-2.5 h-2.5 rounded-full shrink-0 ${
            instance.running && instance.api_ready
              ? "bg-green-500"
              : instance.running
                ? "bg-yellow-500"
                : "bg-gray-500"
          }`}
          title={
            instance.running && instance.api_ready
              ? `Ready (PID: ${instance.pid})`
              : instance.running
                ? `Starting... (PID: ${instance.pid})`
                : "Stopped"
          }
        />

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium truncate">{instance.name}</span>
            {instance.source === "registered" && (
              <span className="px-1.5 py-0.5 text-[10px] font-medium rounded bg-blue-500/15 text-blue-500">
                registered
              </span>
            )}
            {instance.spawn_placement && (
              <span className="px-1.5 py-0.5 text-[10px] font-medium rounded bg-primary/15 text-primary">
                placement
              </span>
            )}
          </div>
          <div className="text-xs text-muted-foreground">
            Port {instance.port}
            {instance.running && instance.pid && <span className="ml-2">PID {instance.pid}</span>}
            {instance.running_tasks != null && instance.running_tasks > 0 && (
              <span className="ml-2">
                {instance.running_tasks} task{instance.running_tasks !== 1 ? "s" : ""}
              </span>
            )}
          </div>
        </div>

        <div className="flex items-center gap-1 shrink-0">
          {isConfigured && (
            <button
              onClick={() => setExpanded((v) => !v)}
              className="p-1.5 rounded hover:bg-accent text-muted-foreground transition-colors"
              title={expanded ? "Hide placement editor" : "Edit placement"}
            >
              {expanded ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
            </button>
          )}
          {instance.source === "registered" ? (
            <span className="text-[10px] text-muted-foreground px-2">external</span>
          ) : instance.running ? (
            <button
              onClick={() => onStop(instance)}
              disabled={actionInProgress === instance.id}
              className="p-1.5 rounded hover:bg-destructive/10 text-destructive transition-colors disabled:opacity-50"
              title="Stop instance"
            >
              <Square className="w-4 h-4" />
            </button>
          ) : (
            <>
              <button
                onClick={() => onLaunch(instance)}
                disabled={actionInProgress === instance.id}
                className="p-1.5 rounded hover:bg-green-500/10 text-green-500 transition-colors disabled:opacity-50"
                title="Launch instance"
              >
                <Play className="w-4 h-4" />
              </button>
              <button
                onClick={() => onDelete(instance)}
                disabled={actionInProgress === instance.id}
                className="p-1.5 rounded hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors disabled:opacity-50"
                title="Delete instance"
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </>
          )}
        </div>
      </div>

      {isConfigured && expanded && (
        <>
          <PlacementEditor draft={draft} onChange={setDraft} monitors={monitors} onLog={onLog} />
          {dirty && (
            <div className="mt-2 flex items-center justify-end gap-2">
              <button
                onClick={() => setDraft(placementToDraft(instance.spawn_placement))}
                className="px-2 py-0.5 text-[11px] rounded border border-border hover:bg-accent transition-colors"
              >
                Discard
              </button>
              <button
                onClick={handleSave}
                className="px-2 py-0.5 text-[11px] font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90"
              >
                Save placement
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-component: SpawnBanner
// Lists the round-robin order the supervisor will hit.
// ---------------------------------------------------------------------------

interface BannerSlot {
  index: number;
  label: string;
  preview: ResolvedPlacement | null;
  error: string | null;
  hasPlacement: boolean;
}

interface SpawnBannerProps {
  slots: BannerSlot[];
}

function SpawnBanner({ slots }: SpawnBannerProps) {
  if (slots.length === 0) return null;
  return (
    <div className="p-3 rounded-lg border border-border bg-muted/30 font-mono text-[11px] space-y-0.5">
      <div className="text-xs font-semibold mb-1 font-sans">
        Temp runner placement (round-robin):
      </div>
      {slots.map((slot) => (
        <div key={slot.index} className="flex flex-wrap gap-x-2">
          <span className="text-muted-foreground shrink-0">Slot {slot.index}</span>
          <span className="truncate">{slot.label}:</span>
          <span className="ml-auto text-right">
            {slot.error ? (
              <span className="text-destructive">{slot.error}</span>
            ) : slot.preview ? (
              <span>
                → global ({slot.preview.global_x}, {slot.preview.global_y}) {slot.preview.width}×
                {slot.preview.height} on {slot.preview.monitor_label}
              </span>
            ) : (
              <span className="text-muted-foreground">no placement</span>
            )}
          </span>
        </div>
      ))}
      <div className="text-[10px] text-muted-foreground pt-1 font-sans">
        The supervisor's spawn-test endpoint cycles through enabled placements in this order.
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function RunnerInstancesSettings({ onLog }: RunnerInstancesSettingsProps) {
  const [instances, setInstances] = useState<InstanceStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [actionInProgress, setActionInProgress] = useState<string | null>(null);
  const [monitors, setMonitors] = useState<MonitorEntry[]>([]);
  const [bannerSlots, setBannerSlots] = useState<BannerSlot[]>([]);

  // New instance form
  const [showAddForm, setShowAddForm] = useState(false);
  const [newName, setNewName] = useState("");
  const [newPort, setNewPort] = useState(9877);
  const [newDraft, setNewDraft] = useState<PlacementDraft>({ ...DEFAULT_DRAFT });

  // Primary slot (slot 0) placement — owned by this runner's settings entry
  // for itself. Phase 1 didn't add a "self" slot, so we render slot 0 as
  // "primary (this runner)" with no placement and no edit affordance for
  // now. Configurable secondary slots start at slot index 1.
  // (Tracked as a follow-up — see report.)

  const loadInstances = useCallback(async () => {
    try {
      const local = await invoke<InstanceStatus[]>("get_runner_instances");
      const localPorts = new Set(local.map((i) => i.port));

      const merged: InstanceStatus[] = local.map((i) => ({
        ...i,
        source: i.source === "discovered" ? "registered" : "local",
      }));

      try {
        const port = getApiPort();
        const resp = await fetch(`http://localhost:${port}/runners`);
        if (resp.ok) {
          const runners: Array<{
            id: string;
            name: string;
            port: number;
            is_primary: boolean;
            running: boolean;
            pid: number | null;
            api_responding: boolean;
          }> = await resp.json();

          for (const r of runners) {
            if (r.is_primary || localPorts.has(r.port) || r.port === port) continue;
            merged.push({
              id: r.id,
              name: r.name,
              port: r.port,
              running: r.running,
              pid: r.pid,
              api_ready: r.api_responding,
              source: "registered",
              registry_status: r.running ? "healthy" : "stopped",
            });
          }
        }
      } catch {
        // Non-fatal
      }

      setInstances(merged);
    } catch (err) {
      onLog("error", `Failed to load instances: ${err}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  const loadMonitors = useCallback(async () => {
    try {
      const list = await invoke<MonitorList>("list_monitors_for_placement");
      setMonitors(list.monitors);
    } catch (err) {
      onLog("error", `Failed to load monitors: ${err}`);
    }
  }, [onLog]);

  // Build banner slots. Slot 0 = primary runner (this one). Configured
  // instances follow in port order.
  const refreshBannerSlots = useCallback(async () => {
    const configured = instances
      .filter((i) => i.source !== "registered")
      .slice()
      .sort((a, b) => a.port - b.port);

    const slots: BannerSlot[] = [];
    slots.push({
      index: 0,
      label: "(primary)",
      preview: null,
      error: null,
      hasPlacement: false,
    });

    for (let i = 0; i < configured.length; i++) {
      const inst = configured[i];
      const slotIdx = i + 1;
      const labelBase = `(named "${inst.name}")`;
      if (!inst.spawn_placement) {
        slots.push({
          index: slotIdx,
          label: labelBase,
          preview: null,
          error: null,
          hasPlacement: false,
        });
        continue;
      }
      try {
        const resolved = await invoke<ResolvedPlacement>("preview_spawn_placement", {
          placement: inst.spawn_placement,
        });
        slots.push({
          index: slotIdx,
          label: labelBase,
          preview: resolved,
          error: null,
          hasPlacement: true,
        });
      } catch (err) {
        slots.push({
          index: slotIdx,
          label: labelBase,
          preview: null,
          error: String(err),
          hasPlacement: true,
        });
      }
    }

    setBannerSlots(slots);
  }, [instances]);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) {
        void loadInstances();
        void loadMonitors();
      }
    });
    const interval = setInterval(loadInstances, 3000);
    const unlisten = listen("runner-instances-restored", () => {
      void loadInstances();
    });
    return () => {
      cancelled = true;
      clearInterval(interval);
      unlisten.then((fn) => fn());
    };
  }, [loadInstances, loadMonitors]);

  useEffect(() => {
    void refreshBannerSlots();
  }, [refreshBannerSlots]);

  // Auto-suggest next available port
  useEffect(() => {
    if (!showAddForm) return;
    const usedPorts = new Set(instances.map((i) => i.port));
    usedPorts.add(getApiPort());
    let nextPort = 9877;
    while (usedPorts.has(nextPort)) nextPort++;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) setNewPort(nextPort);
    });
    return () => {
      cancelled = true;
    };
  }, [showAddForm, instances]);

  const handleAdd = async () => {
    if (!newName.trim()) {
      onLog("warning", "Instance name is required");
      return;
    }

    if (newPort === getApiPort()) {
      onLog("warning", "Port conflicts with the primary runner");
      return;
    }

    const conflicting = instances.find((i) => i.port === newPort);
    if (conflicting) {
      onLog("warning", `Port already used by instance '${conflicting.name}'`);
      return;
    }

    try {
      const id = `instance-${Date.now()}`;
      const placement = draftToPlacement(newDraft);
      await invoke("save_runner_instance", {
        id,
        name: newName.trim(),
        port: newPort,
        spawnPlacement: placement,
      });
      onLog("success", `Instance '${newName}' added on port ${newPort}`);
      setNewName("");
      setNewDraft({ ...DEFAULT_DRAFT });
      setShowAddForm(false);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to add instance: ${err}`);
    }
  };

  const handleDelete = async (instance: InstanceStatus) => {
    try {
      await invoke("delete_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' deleted`);
      await loadInstances();
    } catch (err) {
      onLog("error", `${err}`);
    }
  };

  const handleLaunch = async (instance: InstanceStatus) => {
    setActionInProgress(instance.id);
    try {
      await invoke("launch_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' launched on port ${instance.port}`);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to launch: ${err}`);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleStop = async (instance: InstanceStatus) => {
    setActionInProgress(instance.id);
    try {
      await invoke("stop_runner_instance", { id: instance.id });
      onLog("success", `Instance '${instance.name}' stopped`);
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to stop: ${err}`);
    } finally {
      setActionInProgress(null);
    }
  };

  const handleSavePlacement = async (
    instance: InstanceStatus,
    placement: SpawnPlacement | null,
  ) => {
    try {
      await invoke("save_runner_instance", {
        id: instance.id,
        name: instance.name,
        port: instance.port,
        spawnPlacement: placement,
      });
      onLog(
        "success",
        placement
          ? `Placement saved for '${instance.name}'`
          : `Placement cleared for '${instance.name}'`,
      );
      await loadInstances();
    } catch (err) {
      onLog("error", `Failed to save placement: ${err}`);
    }
  };

  return (
    <div className="space-y-4">
      <SectionHeader
        title="Runner Instances"
        description="Launch additional runner instances on different ports for concurrent AI workflow automation."
        icon={<Monitor />}
      />

      {/* Banner */}
      <SpawnBanner slots={bannerSlots} />

      {/* Instance list */}
      <div className="space-y-2">
        {loading && instances.length === 0 && (
          <p className="text-xs text-muted-foreground">Loading...</p>
        )}

        {!loading && instances.length === 0 && !showAddForm && (
          <p className="text-xs text-muted-foreground">
            No instances configured. Click "Add Instance" to create one.
          </p>
        )}

        {instances.map((instance) => (
          <InstanceRow
            key={instance.id}
            instance={instance}
            monitors={monitors}
            actionInProgress={actionInProgress}
            onLaunch={handleLaunch}
            onStop={handleStop}
            onDelete={handleDelete}
            onSavePlacement={handleSavePlacement}
            onLog={onLog}
          />
        ))}

        {/* Add instance form */}
        {showAddForm && (
          <div className="p-3 rounded-lg border border-border bg-card space-y-2">
            <div className="flex items-center gap-2">
              <input
                type="text"
                placeholder="Instance name"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void handleAdd();
                }}
                className="flex-1 min-w-0 px-2 py-1 text-sm rounded border border-border bg-background"
                autoFocus
              />
              <input
                type="number"
                value={newPort}
                onChange={(e) => setNewPort(Number(e.target.value))}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void handleAdd();
                }}
                className="w-20 px-2 py-1 text-sm rounded border border-border bg-background"
                min={1024}
                max={65535}
              />
              <button
                onClick={() => void handleAdd()}
                className="px-3 py-1 text-xs font-medium rounded bg-primary text-primary-foreground hover:bg-primary/90"
              >
                Save
              </button>
              <button
                onClick={() => {
                  setShowAddForm(false);
                  setNewDraft({ ...DEFAULT_DRAFT });
                }}
                className="px-3 py-1 text-xs font-medium rounded border border-border hover:bg-accent"
              >
                Cancel
              </button>
            </div>

            <PlacementEditor
              draft={newDraft}
              onChange={setNewDraft}
              monitors={monitors}
              onLog={onLog}
            />
          </div>
        )}
      </div>

      {/* Add button */}
      {!showAddForm && (
        <button
          onClick={() => setShowAddForm(true)}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded border border-border hover:bg-accent transition-colors"
        >
          <Plus className="w-3.5 h-3.5" />
          Add Instance
        </button>
      )}
    </div>
  );
}
