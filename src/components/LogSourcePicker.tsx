/**
 * LogSourcePicker.tsx
 *
 * Lightweight modal for selecting which global log sources a project uses.
 * Sources are managed in Settings > Log Sources — this just picks which ones.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, Check, Settings, FolderOpen } from "lucide-react";

interface GlobalLogSource {
  id: string;
  name: string;
  description: string;
  type: string;
  path: string;
  enabled: boolean;
  color?: string;
}

interface GlobalLogSourceProfile {
  id: string;
  name: string;
  description?: string;
  source_ids: string[];
}

interface GlobalLogSourceSettings {
  sources: GlobalLogSource[];
  profiles: GlobalLogSourceProfile[];
  default_profile_id?: string;
}

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface LogSourcePickerProps {
  isOpen: boolean;
  onClose: () => void;
  selectedSourceIds: string[];
  globalProfileId?: string;
  onSave: (selectedSourceIds: string[], globalProfileId?: string) => void;
  /** Callback to navigate to Settings > Log Sources */
  onOpenSettings?: () => void;
}

export function LogSourcePicker({
  isOpen,
  onClose,
  selectedSourceIds,
  globalProfileId,
  onSave,
  onOpenSettings,
}: LogSourcePickerProps) {
  const [settings, setSettings] = useState<GlobalLogSourceSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [localSelectedIds, setLocalSelectedIds] = useState<string[]>(selectedSourceIds);
  const [localProfileId, setLocalProfileId] = useState<string | undefined>(globalProfileId);
  const [mode, setMode] = useState<"profile" | "custom">(globalProfileId ? "profile" : "custom");

  // Sync local state with props during render when dialog opens or props change
  const [prevIsOpen, setPrevIsOpen] = useState(isOpen);
  const [prevSelectedSourceIds, setPrevSelectedSourceIds] = useState(selectedSourceIds);
  const [prevGlobalProfileId, setPrevGlobalProfileId] = useState(globalProfileId);
  if (
    isOpen &&
    (isOpen !== prevIsOpen ||
      selectedSourceIds !== prevSelectedSourceIds ||
      globalProfileId !== prevGlobalProfileId)
  ) {
    setPrevIsOpen(isOpen);
    setPrevSelectedSourceIds(selectedSourceIds);
    setPrevGlobalProfileId(globalProfileId);
    setLocalSelectedIds(selectedSourceIds);
    setLocalProfileId(globalProfileId);
    setMode(globalProfileId ? "profile" : "custom");
  }
  if (!isOpen && isOpen !== prevIsOpen) {
    setPrevIsOpen(isOpen);
  }

  // Load global log source settings when the dialog opens. setLoading(true)
  // lives inside the IIFE so it isn't the first statement reached from the
  // effect body (react-hooks/set-state-in-effect).
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    (async () => {
      setLoading(true);
      try {
        const result = await invoke<TauriResult<GlobalLogSourceSettings>>("get_global_log_sources");
        if (cancelled) return;
        if (result.success && result.data) {
          setSettings(result.data);
        }
      } catch (err) {
        console.error("Failed to load global log source settings:", err);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isOpen]);

  if (!isOpen) return null;

  const enabledSources = settings?.sources.filter((s) => s.enabled) ?? [];
  const profiles = settings?.profiles ?? [];

  const toggleSource = (sourceId: string) => {
    setLocalSelectedIds((prev) =>
      prev.includes(sourceId) ? prev.filter((id) => id !== sourceId) : [...prev, sourceId],
    );
    setMode("custom");
    setLocalProfileId(undefined);
  };

  const selectAll = () => {
    setLocalSelectedIds(enabledSources.map((s) => s.id));
    setMode("custom");
    setLocalProfileId(undefined);
  };

  const selectNone = () => {
    setLocalSelectedIds([]);
    setMode("custom");
    setLocalProfileId(undefined);
  };

  const selectProfile = (profileId: string) => {
    setLocalProfileId(profileId);
    setLocalSelectedIds([]);
    setMode("profile");
  };

  const useAllEnabled = () => {
    setLocalProfileId(undefined);
    setLocalSelectedIds([]);
    setMode("custom");
  };

  const handleSave = () => {
    if (mode === "profile") {
      onSave([], localProfileId);
    } else {
      onSave(localSelectedIds, undefined);
    }
    onClose();
  };

  // Determine which source IDs are "active" for the checkboxes display
  const activeSourceIds =
    mode === "profile" && localProfileId
      ? (profiles.find((p) => p.id === localProfileId)?.source_ids ?? [])
      : localSelectedIds;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background border border-border rounded-lg shadow-xl w-full max-w-lg max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 className="text-lg font-semibold">Select Log Sources</h2>
          <button onClick={onClose} className="p-1 hover:bg-muted rounded transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto p-4 space-y-4">
          {loading ? (
            <div className="text-center text-muted-foreground py-8">Loading...</div>
          ) : enabledSources.length === 0 ? (
            <div className="text-center text-muted-foreground py-8 space-y-3">
              <FolderOpen className="w-10 h-10 mx-auto opacity-50" />
              <p>No log sources configured.</p>
              <p className="text-sm">Add sources in Settings &gt; Log Sources first.</p>
              {onOpenSettings && (
                <button
                  onClick={() => {
                    onOpenSettings();
                    onClose();
                  }}
                  className="inline-flex items-center gap-2 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90"
                >
                  <Settings className="w-4 h-4" />
                  Open Log Source Settings
                </button>
              )}
            </div>
          ) : (
            <>
              {/* Profile selector */}
              {profiles.length > 0 && (
                <div>
                  <label className="text-sm font-medium text-muted-foreground mb-2 block">
                    Use a Profile
                  </label>
                  <div className="flex flex-wrap gap-2">
                    <button
                      onClick={useAllEnabled}
                      className={`px-3 py-1.5 text-sm rounded-md border transition-colors ${
                        mode === "custom" && localSelectedIds.length === 0
                          ? "bg-primary text-primary-foreground border-primary"
                          : "bg-muted border-border hover:bg-muted/80"
                      }`}
                    >
                      All Enabled
                    </button>
                    {profiles.map((profile) => (
                      <button
                        key={profile.id}
                        onClick={() => selectProfile(profile.id)}
                        className={`px-3 py-1.5 text-sm rounded-md border transition-colors ${
                          localProfileId === profile.id
                            ? "bg-primary text-primary-foreground border-primary"
                            : "bg-muted border-border hover:bg-muted/80"
                        }`}
                        title={profile.description}
                      >
                        {profile.name}
                        <span className="ml-1 text-xs opacity-70">
                          ({profile.source_ids.length})
                        </span>
                      </button>
                    ))}
                  </div>
                </div>
              )}

              {/* Source checkboxes */}
              <div>
                <div className="flex items-center justify-between mb-2">
                  <label className="text-sm font-medium text-muted-foreground">
                    {mode === "profile" ? "Profile Sources" : "Custom Selection"}
                  </label>
                  {mode === "custom" && (
                    <div className="flex gap-2 text-xs">
                      <button onClick={selectAll} className="text-primary hover:underline">
                        Select All
                      </button>
                      <button
                        onClick={selectNone}
                        className="text-muted-foreground hover:underline"
                      >
                        Clear
                      </button>
                    </div>
                  )}
                </div>
                <div className="space-y-1 max-h-64 overflow-auto">
                  {enabledSources.map((source) => {
                    const isSelected = activeSourceIds.includes(source.id);
                    const isProfileMode = mode === "profile";
                    return (
                      <label
                        key={source.id}
                        className={`flex items-center gap-3 p-2 rounded-md cursor-pointer transition-colors ${
                          isSelected ? "bg-primary/10" : "hover:bg-muted"
                        } ${isProfileMode ? "opacity-70 cursor-default" : ""}`}
                      >
                        <input
                          type="checkbox"
                          checked={isSelected}
                          onChange={() => toggleSource(source.id)}
                          disabled={isProfileMode}
                          className="rounded border-border"
                        />
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2">
                            {source.color && (
                              <span
                                className="w-2 h-2 rounded-full shrink-0"
                                style={{ backgroundColor: source.color }}
                              />
                            )}
                            <span className="text-sm font-medium truncate">{source.name}</span>
                          </div>
                          <div className="text-xs text-muted-foreground truncate">
                            {source.path}
                          </div>
                        </div>
                        {isSelected && <Check className="w-4 h-4 text-primary shrink-0" />}
                      </label>
                    );
                  })}
                </div>
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between p-4 border-t border-border">
          <div>
            {onOpenSettings && (
              <button
                onClick={() => {
                  onOpenSettings();
                  onClose();
                }}
                className="text-sm text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1"
              >
                <Settings className="w-3.5 h-3.5" />
                Manage in Settings
              </button>
            )}
          </div>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm bg-muted hover:bg-muted/80 rounded-md transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleSave}
              className="px-4 py-2 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
            >
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
