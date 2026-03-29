/**
 * NotificationSettings — User preferences for error toast notifications.
 *
 * Controls severity filter, muted log sources, and max toasts per batch.
 * Preferences are persisted in instanceStorage and read by useErrorNotifications.
 */

import { Bell, X } from "lucide-react";
import { useState } from "react";
import { SectionHeader } from "./SectionHeader";
import {
  useNotificationPreferences,
  type NotificationSeverityFilter,
} from "@/hooks/useNotificationPreferences";
import type { LogFunction } from "./types";

interface NotificationSettingsProps {
  onLog: LogFunction;
}

const SEVERITY_OPTIONS: { value: NotificationSeverityFilter; label: string; description: string }[] = [
  { value: "critical", label: "Critical only", description: "Only show critical errors" },
  { value: "error", label: "Error and above", description: "Show critical and error (default)" },
  { value: "warning", label: "Warning and above", description: "Show critical, error, and warnings" },
  { value: "off", label: "Off", description: "Disable all error toast notifications" },
];

export function NotificationSettings({ onLog }: NotificationSettingsProps) {
  const {
    prefs,
    setSeverityFilter,
    setMaxPerBatch,
    muteSource,
    unmuteSource,
  } = useNotificationPreferences();

  const [newMutedSource, setNewMutedSource] = useState("");

  const handleSeverityChange = (value: NotificationSeverityFilter) => {
    setSeverityFilter(value);
    onLog("success", `Notification severity set to "${value}"`);
  };

  const handleMaxPerBatchChange = (value: string) => {
    const num = parseInt(value, 10);
    if (!isNaN(num) && num >= 0 && num <= 20) {
      setMaxPerBatch(num);
    }
  };

  const handleAddMutedSource = () => {
    const trimmed = newMutedSource.trim();
    if (trimmed && !prefs.mutedSources.includes(trimmed)) {
      muteSource(trimmed);
      setNewMutedSource("");
      onLog("success", `Muted notifications from "${trimmed}"`);
    }
  };

  const handleRemoveMutedSource = (sourceName: string) => {
    unmuteSource(sourceName);
    onLog("success", `Unmuted notifications from "${sourceName}"`);
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Notifications"
        description="Control which error notifications appear as toasts."
        icon={<Bell className="w-6 h-6" />}
      />

      {/* Severity Filter */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Minimum Severity</h4>
        <div className="space-y-2">
          {SEVERITY_OPTIONS.map((opt) => (
            <label
              key={opt.value}
              className="flex items-center gap-3 cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors"
            >
              <input
                type="radio"
                name="severityFilter"
                value={opt.value}
                checked={prefs.severityFilter === opt.value}
                onChange={() => handleSeverityChange(opt.value)}
                className="accent-primary"
              />
              <div className="space-y-0.5">
                <div className="text-sm font-medium">{opt.label}</div>
                <div className="text-xs text-muted-foreground">{opt.description}</div>
              </div>
            </label>
          ))}
        </div>
      </div>

      {/* Max Toasts Per Batch */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Max Toasts Per Batch</h4>
        <div className="flex items-center gap-3 p-3 rounded-lg bg-muted/30">
          <label className="text-sm text-muted-foreground flex-1">
            Maximum number of toast notifications shown per error event
          </label>
          <input
            type="number"
            min={0}
            max={20}
            value={prefs.maxPerBatch}
            onChange={(e) => handleMaxPerBatchChange(e.target.value)}
            className="w-16 px-2 py-1 text-sm rounded border border-border bg-background text-foreground"
          />
        </div>
        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> Set to 0 to suppress individual
            toasts entirely. A summary toast will still appear when errors are rate-limited.
          </div>
        </div>
      </div>

      {/* Muted Sources */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Muted Log Sources</h4>
        <div className="text-xs text-muted-foreground mb-2">
          Errors from muted log sources will not produce toast notifications.
        </div>

        {prefs.mutedSources.length > 0 && (
          <div className="flex flex-wrap gap-2">
            {prefs.mutedSources.map((source) => (
              <span
                key={source}
                className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-muted text-sm"
              >
                {source}
                <button
                  onClick={() => handleRemoveMutedSource(source)}
                  className="text-muted-foreground hover:text-foreground transition-colors"
                  title={`Unmute "${source}"`}
                >
                  <X className="w-3 h-3" />
                </button>
              </span>
            ))}
          </div>
        )}

        <div className="flex gap-2">
          <input
            type="text"
            value={newMutedSource}
            onChange={(e) => setNewMutedSource(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleAddMutedSource()}
            placeholder="Log source name to mute..."
            className="flex-1 px-3 py-1.5 text-sm rounded border border-border bg-background text-foreground placeholder:text-muted-foreground"
          />
          <button
            onClick={handleAddMutedSource}
            disabled={!newMutedSource.trim()}
            className="px-3 py-1.5 text-sm rounded bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Mute
          </button>
        </div>
      </div>
    </div>
  );
}
