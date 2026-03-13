import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Cog, Loader2, CheckSquare, Square, Database, Globe, Server } from "lucide-react";

interface DevServiceConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  env?: Record<string, string>;
  health_port?: number;
  parser: string;
  category: string;
  auto_start: boolean;
  enabled: boolean;
  buffer_size: number;
  start_group: number;
  dev_only: boolean;
}

interface DevServicesStepProps {
  onNext: () => void;
  onBack: () => void;
}

const CATEGORY_ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  infrastructure: Database,
  backend: Server,
  frontend: Globe,
};

const CATEGORY_COLORS: Record<string, string> = {
  infrastructure: "badge-warning",
  backend: "badge-success",
  frontend: "badge-info",
};

export function DevServicesStep({ onNext, onBack }: DevServicesStepProps) {
  const [services, setServices] = useState<DevServiceConfig[]>([]);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadDevServices() {
      setLoading(true);
      setError(null);
      try {
        const result = await invoke<DevServiceConfig[]>("suggest_dev_services_for_setup");
        if (cancelled) return;
        setServices(result);
        // Auto-select all by default
        setSelectedIds(new Set(result.map((s) => s.id)));
      } catch (err) {
        if (cancelled) return;
        // Not an error if workspace not found — just means no dev services to suggest
        setServices([]);
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    loadDevServices();
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleService = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const handleContinue = useCallback(async () => {
    if (services.length === 0) {
      onNext();
      return;
    }

    setSaving(true);
    try {
      await invoke("save_dev_services_from_setup", {
        services,
        selectedIds: Array.from(selectedIds),
      });
      onNext();
    } catch (err) {
      setError(String(err));
      setSaving(false);
    }
  }, [services, selectedIds, onNext]);

  if (loading) {
    return (
      <div className="tutorial-fade-in flex flex-col items-center py-12 gap-3">
        <Loader2 className="w-8 h-8 text-primary animate-spin" />
        <p className="text-muted-foreground">Detecting dev services...</p>
      </div>
    );
  }

  if (services.length === 0) {
    return (
      <div className="tutorial-fade-in flex flex-col items-center py-8 px-4">
        <div className="empty-state py-8">
          <Cog className="w-10 h-10 text-muted-foreground mb-2 mx-auto" />
          <p className="text-muted-foreground mb-2">
            No dev services detected for this workspace.
          </p>
          <p className="text-xs text-muted-foreground">
            You can configure dev services later from the Process Manager.
          </p>
        </div>
        <div className="flex justify-between w-full max-w-xl pt-4">
          <button className="btn-secondary" onClick={onBack}>
            Back
          </button>
          <button className="btn-primary" onClick={onNext}>
            Skip
          </button>
        </div>
      </div>
    );
  }

  // Sort by start_group for display
  const sortedServices = [...services].sort((a, b) => a.start_group - b.start_group);

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">Dev Services</h2>
        <p className="text-body text-muted-foreground">
          Select services the runner should auto-start in development mode. These services start in
          order — infrastructure first, then your app services.
        </p>
      </div>

      {error && (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 max-w-xl mx-auto w-full">
          {error}
        </div>
      )}

      <div className="max-w-xl mx-auto w-full flex flex-col gap-1.5">
        {sortedServices.map((service) => {
          const isSelected = selectedIds.has(service.id);
          const Icon = CATEGORY_ICONS[service.category] || Server;
          return (
            <button
              key={service.id}
              className={`panel-interactive flex items-center gap-3 p-2.5 text-left transition-colors ${
                isSelected ? "border-primary/50" : ""
              }`}
              onClick={() => toggleService(service.id)}
            >
              {isSelected ? (
                <CheckSquare className="w-4 h-4 text-primary shrink-0" />
              ) : (
                <Square className="w-4 h-4 text-zinc-500 shrink-0" />
              )}
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <Icon className="w-3.5 h-3.5 text-muted-foreground" />
                  <span className="text-sm font-medium">{service.name}</span>
                </div>
                <div className="mt-0.5 text-xs text-muted-foreground font-mono truncate">
                  {service.command} {service.args.join(" ")}
                </div>
                <div className="mt-0.5 text-xs text-muted-foreground flex items-center gap-3">
                  {service.health_port && <span>Port: {service.health_port}</span>}
                  <span>Start group: {service.start_group}</span>
                </div>
              </div>
              <div className="flex gap-1.5 shrink-0">
                <span
                  className={`badge text-xs ${CATEGORY_COLORS[service.category] || "badge-muted"}`}
                >
                  {service.category}
                </span>
              </div>
            </button>
          );
        })}
      </div>

      <div className="max-w-xl mx-auto w-full">
        <p className="text-xs text-muted-foreground text-center">
          Dev services are only active in development builds. Users of your app can configure their
          own services in the setup wizard.
        </p>
      </div>

      {/* Navigation */}
      <div className="flex justify-between max-w-xl mx-auto w-full pt-4">
        <button className="btn-secondary" onClick={onBack}>
          Back
        </button>
        <button className="btn-primary" onClick={handleContinue} disabled={saving}>
          {saving
            ? "Saving..."
            : selectedIds.size > 0
              ? `Continue with ${selectedIds.size} service${selectedIds.size !== 1 ? "s" : ""}`
              : "Skip"}
        </button>
      </div>
    </div>
  );
}
