/**
 * SchedulerSettings
 *
 * Global scheduler settings configuration.
 */

import { useState, useEffect } from "react";
import { Save, Loader2, Play, Pause } from "lucide-react";
import type {
  SchedulerSettings as SchedulerSettingsType,
  SchedulerStatus,
} from "../../types/scheduler";

interface SchedulerSettingsProps {
  settings: SchedulerSettingsType;
  status: SchedulerStatus | null;
  onUpdateSettings: (settings: SchedulerSettingsType) => Promise<boolean>;
  loading: boolean;
}

export function SchedulerSettings({
  settings,
  status,
  onUpdateSettings,
  loading,
}: SchedulerSettingsProps) {
  const [enabled, setEnabled] = useState(settings.enabled);
  const [maxConcurrent, setMaxConcurrent] = useState(settings.max_concurrent);
  const [defaultAutoFix, setDefaultAutoFix] = useState(settings.default_auto_fix_on_failure);
  const [timezone, setTimezone] = useState(settings.timezone || "");
  const [hasChanges, setHasChanges] = useState(false);

  // Track changes
  useEffect(() => {
    const changed =
      enabled !== settings.enabled ||
      maxConcurrent !== settings.max_concurrent ||
      defaultAutoFix !== settings.default_auto_fix_on_failure ||
      timezone !== (settings.timezone || "");
    setHasChanges(changed);
  }, [enabled, maxConcurrent, defaultAutoFix, timezone, settings]);

  const handleSave = async () => {
    const success = await onUpdateSettings({
      enabled,
      max_concurrent: maxConcurrent,
      default_auto_fix_on_failure: defaultAutoFix,
      timezone: timezone || undefined,
    });
    if (success) {
      setHasChanges(false);
    }
  };

  return (
    <div className="max-w-xl space-y-6">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-medium">Scheduler Settings</h3>
        <button
          onClick={handleSave}
          disabled={loading || !hasChanges}
          className="flex items-center gap-2 px-4 py-2 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
          Save Changes
        </button>
      </div>

      {/* Status display */}
      {status && (
        <div className="p-4 bg-muted/50 rounded-lg">
          <h4 className="text-sm font-medium mb-2">Current Status</h4>
          <div className="grid grid-cols-2 gap-4 text-sm">
            <div className="flex items-center gap-2">
              <span className="text-muted-foreground">Status:</span>
              <span
                className={`flex items-center gap-1 ${status.enabled ? "text-green-500" : "text-gray-500"}`}
              >
                {status.enabled ? (
                  <>
                    <Play className="w-4 h-4" /> Running
                  </>
                ) : (
                  <>
                    <Pause className="w-4 h-4" /> Paused
                  </>
                )}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">Running Tasks:</span>{" "}
              <span className={status.running_tasks > 0 ? "text-blue-500" : ""}>
                {status.running_tasks}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">Pending Tasks:</span> {status.pending_tasks}
            </div>
            {status.next_task && (
              <div className="col-span-2">
                <span className="text-muted-foreground">Next Task:</span>{" "}
                <span className="text-primary">{status.next_task.name}</span>
                <span className="text-muted-foreground"> at </span>
                {new Date(status.next_task.next_run).toLocaleString()}
              </div>
            )}
          </div>
        </div>
      )}

      {/* Settings form */}
      <div className="space-y-4">
        {/* Enabled toggle */}
        <div className="flex items-center justify-between p-4 border border-border rounded-lg">
          <div>
            <h4 className="font-medium">Scheduler Enabled</h4>
            <p className="text-sm text-muted-foreground">
              When enabled, scheduled tasks will run automatically at their configured times.
            </p>
          </div>
          <button
            onClick={() => setEnabled(!enabled)}
            className={`relative w-12 h-6 rounded-full transition-colors ${
              enabled ? "bg-primary" : "bg-muted"
            }`}
          >
            <div
              className={`absolute top-1 w-4 h-4 bg-white rounded-full transition-transform ${
                enabled ? "left-7" : "left-1"
              }`}
            />
          </button>
        </div>

        {/* Max concurrent tasks */}
        <div className="p-4 border border-border rounded-lg">
          <div className="flex items-center justify-between">
            <div>
              <h4 className="font-medium">Maximum Concurrent Tasks</h4>
              <p className="text-sm text-muted-foreground">
                How many tasks can run at the same time.
              </p>
            </div>
            <input
              type="number"
              value={maxConcurrent}
              onChange={(e) => setMaxConcurrent(Math.max(1, parseInt(e.target.value, 10) || 1))}
              min={1}
              max={10}
              className="w-20 px-3 py-2 text-center bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
          </div>
        </div>

        {/* Default auto-fix on failure */}
        <div className="flex items-center justify-between p-4 border border-border rounded-lg">
          <div>
            <h4 className="font-medium">Default Auto-Fix on Failure</h4>
            <p className="text-sm text-muted-foreground">
              When enabled, new tasks will have auto-fix on failure enabled by default.
            </p>
          </div>
          <button
            onClick={() => setDefaultAutoFix(!defaultAutoFix)}
            className={`relative w-12 h-6 rounded-full transition-colors ${
              defaultAutoFix ? "bg-orange-500" : "bg-muted"
            }`}
          >
            <div
              className={`absolute top-1 w-4 h-4 bg-white rounded-full transition-transform ${
                defaultAutoFix ? "left-7" : "left-1"
              }`}
            />
          </button>
        </div>

        {/* Timezone (optional) */}
        <div className="p-4 border border-border rounded-lg">
          <div className="flex items-center justify-between">
            <div>
              <h4 className="font-medium">Timezone</h4>
              <p className="text-sm text-muted-foreground">
                Timezone for schedule interpretation. Leave empty for local timezone.
              </p>
            </div>
            <input
              type="text"
              value={timezone}
              onChange={(e) => setTimezone(e.target.value)}
              placeholder="e.g., America/New_York"
              className="w-48 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
          </div>
        </div>
      </div>
    </div>
  );
}
