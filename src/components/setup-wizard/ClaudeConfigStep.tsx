import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderSearch, Plus, Loader2, X } from "lucide-react";

interface DiscoveredDir {
  path: string;
  label: string;
  source: string;
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

  useEffect(() => {
    // eslint-disable-next-line react-hooks/immutability
    discoverDirs();
  }, []);

  const discoverDirs = async () => {
    setLoading(true);
    try {
      const found = await invoke<DiscoveredDir[]>("discover_claude_config_dirs");
      setDirs(found);
      // Pre-check all discovered dirs
      setChecked(new Set(found.map((d) => d.path)));
    } catch (err) {
      console.error("Failed to discover Claude config dirs:", err);
    } finally {
      setLoading(false);
    }
  };

  const toggleDir = useCallback((path: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }, []);

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
    try {
      const selectedDirs = dirs.filter((d) => checked.has(d.path)).map((d) => d.path);
      await invoke("save_claude_config_dirs", { dirs: selectedDirs });
    } catch (err) {
      console.error("Failed to save Claude config dirs:", err);
    } finally {
      setSaving(false);
      onComplete();
    }
  }, [dirs, checked, onComplete]);

  const handleSkip = useCallback(() => {
    onComplete();
  }, [onComplete]);

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">Claude Code Sessions</h2>
        <p className="text-body text-muted-foreground">
          Select Claude Code config directories to browse session transcripts. These are
          auto-detected from your system.
        </p>
      </div>

      <div className="max-w-xl mx-auto w-full space-y-3">
        {loading ? (
          <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground">
            <Loader2 className="w-5 h-5 animate-spin" />
            <span className="text-sm">Scanning for Claude Code installations...</span>
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
                checked={checked.has(dir.path)}
                onChange={() => toggleDir(dir.path)}
                className="w-4 h-4 rounded border-zinc-600 accent-primary"
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium truncate">{dir.label}</div>
                <div className="text-xs text-muted-foreground truncate">{dir.path}</div>
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
        >
          <Plus className="w-4 h-4" />
          Add Directory
        </button>
      </div>

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
