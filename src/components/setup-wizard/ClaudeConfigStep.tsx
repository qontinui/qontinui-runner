import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderSearch, Plus, Loader2, X } from "lucide-react";

export interface DiscoveredDir {
  path: string;
  label: string;
  source: string;
}

/** The source discovery gives a dir that is already in the roster. */
export const CONFIGURED_SOURCE = "configured";

/**
 * The dirs this step should ADD to the roster: the checked ones that are not
 * already configured.
 *
 * Finishing the wizard only ever adds accounts (`add_claude_config_dirs_from_setup`,
 * which never removes one and skips dirs already present). Removing an account
 * is done in Settings. Before qontinui-runner#2544 this step saved its checked
 * list through the replace-style `save_claude_config_dirs`, and an empty
 * selection emptied the roster (coord finding `bc9fdc87`).
 */
export function claudeDirsToAdd(dirs: DiscoveredDir[], checked: Set<string>): string[] {
  return dirs
    .filter((d) => d.source !== CONFIGURED_SOURCE && checked.has(d.path))
    .map((d) => d.path);
}

interface ClaudeConfigStepProps {
  onComplete: () => void;
  onBack: () => void;
}

export function ClaudeConfigStep({ onComplete, onBack }: ClaudeConfigStepProps) {
  const [dirs, setDirs] = useState<DiscoveredDir[]>([]);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  // Discovery refuses (e.g. an unreadable claude-accounts.json); then Finish
  // adds nothing, because what is configured could not be read.
  const [discoveryError, setDiscoveryError] = useState<string | null>(null);
  // The add was refused or failed: shown inline, and the wizard stays on this
  // step so the user sees it rather than finishing with nothing added.
  const [addError, setAddError] = useState<string | null>(null);

  const discoverDirs = async () => {
    setLoading(true);
    try {
      const found = await invoke<DiscoveredDir[]>("discover_claude_config_dirs");
      setDirs(found);
      setDiscoveryError(null);
      // Pre-check all discovered dirs (configured ones stay checked: they are
      // shown, not offered).
      setChecked(new Set(found.map((d) => d.path)));
    } catch (err) {
      console.error("Failed to discover Claude config dirs:", err);
      setDiscoveryError(String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void discoverDirs();
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleDir = useCallback(
    (path: string) => {
      if (dirs.some((d) => d.path === path && d.source === CONFIGURED_SOURCE)) return;
      setChecked((prev) => {
        const next = new Set(prev);
        if (next.has(path)) {
          next.delete(path);
        } else {
          next.add(path);
        }
        return next;
      });
    },
    [dirs],
  );

  const removeDir = useCallback((path: string) => {
    setDirs((prev) => prev.filter((d) => d.path !== path));
    setChecked((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
  }, []);

  const handleAddDirectory = useCallback(async () => {
    const selected = await open({ directory: true, multiple: false });
    if (selected) {
      const path = selected as string;
      // Avoid duplicates
      if (!dirs.some((d) => d.path === path)) {
        const label = path.split(/[\\/]/).pop() || path;
        const newDir: DiscoveredDir = { path, label, source: "manual" };
        setDirs((prev) => [...prev, newDir]);
        setChecked((prev) => new Set(prev).add(path));
      }
    }
  }, [dirs]);

  const handleFinish = useCallback(async () => {
    setSaving(true);
    setAddError(null);
    try {
      const toAdd = discoveryError === null ? claudeDirsToAdd(dirs, checked) : [];
      if (toAdd.length > 0) {
        await invoke("add_claude_config_dirs_from_setup", { dirs: toAdd });
      }
    } catch (err) {
      console.error("Failed to add Claude config dirs:", err);
      setAddError(String(err));
      setSaving(false);
      return;
    }
    setSaving(false);
    onComplete();
  }, [dirs, checked, discoveryError, onComplete]);

  const handleSkip = useCallback(() => {
    onComplete();
  }, [onComplete]);

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">Claude Code Sessions</h2>
        <p className="text-body text-muted-foreground">
          Select Claude Code config directories to add. Accounts that are already configured are
          listed and kept; remove an account in Settings.
        </p>
      </div>

      <div className="max-w-xl mx-auto w-full space-y-3">
        {loading ? (
          <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground">
            <Loader2 className="w-5 h-5 animate-spin" />
            <span className="text-sm">Scanning for Claude Code installations...</span>
          </div>
        ) : discoveryError !== null ? (
          <div className="text-center py-8" data-testid="claude-config-discovery-error">
            <p className="text-sm text-red-400">
              Could not read the configured Claude accounts, so none will be changed here:{" "}
              {discoveryError}
            </p>
          </div>
        ) : dirs.length === 0 ? (
          <div className="text-center py-8">
            <FolderSearch className="w-10 h-10 text-muted-foreground mx-auto mb-3" />
            <p className="text-sm text-muted-foreground">
              No Claude Code installations detected. You can add directories manually or skip this
              step.
            </p>
          </div>
        ) : (
          dirs.map((dir) => (
            <label
              key={dir.path}
              className="flex items-center gap-3 p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors cursor-pointer"
            >
              <input
                type="checkbox"
                checked={dir.source === CONFIGURED_SOURCE || checked.has(dir.path)}
                disabled={dir.source === CONFIGURED_SOURCE}
                onChange={() => toggleDir(dir.path)}
                className="w-4 h-4 rounded border-zinc-600 accent-primary"
                data-testid="claude-config-dir-checkbox"
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium truncate">{dir.label}</div>
                <div className="text-xs text-muted-foreground truncate">{dir.path}</div>
                {dir.source === CONFIGURED_SOURCE && (
                  <div className="text-xs text-muted-foreground">
                    Already configured. Remove it in Settings.
                  </div>
                )}
              </div>
              <span className="text-xs text-zinc-500 shrink-0">{dir.source}</span>
              {dir.source === "manual" && (
                <button
                  onClick={(e) => {
                    e.preventDefault();
                    removeDir(dir.path);
                  }}
                  className="text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <X className="w-4 h-4" />
                </button>
              )}
            </label>
          ))
        )}

        <button
          className="btn-secondary w-full flex items-center justify-center gap-2"
          onClick={handleAddDirectory}
          // A folder picked while the roster cannot be read would be neither
          // shown nor added, so picking one is not offered.
          disabled={discoveryError !== null}
          data-testid="claude-config-add-directory"
        >
          <Plus className="w-4 h-4" />
          Add Directory
        </button>
      </div>

      {addError !== null && (
        <p
          className="max-w-xl mx-auto w-full text-sm text-red-400"
          data-testid="claude-config-add-error"
        >
          Could not add the selected Claude accounts: {addError}
        </p>
      )}

      {/* Navigation */}
      <div className="flex justify-between max-w-xl mx-auto w-full pt-4">
        <button className="btn-secondary" onClick={onBack}>
          Back
        </button>
        <div className="flex gap-2">
          <button className="btn-secondary" onClick={handleSkip}>
            Skip
          </button>
          <button
            className="btn-primary flex items-center gap-2"
            onClick={handleFinish}
            data-testid="claude-config-finish"
            disabled={saving}
          >
            {saving && <Loader2 className="w-4 h-4 animate-spin" />}
            Finish Setup
          </button>
        </div>
      </div>
    </div>
  );
}
