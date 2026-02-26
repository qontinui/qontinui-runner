import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { cn } from "../../lib/utils";
import { FolderOpen, Plus, Save, X } from "lucide-react";

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  health_port: number | null;
  parser: string;
  auto_start: boolean;
  category: string;
  buffer_size: number;
  enabled: boolean;
}

interface ProcessConfigEditorProps {
  config?: ProcessConfig;
  onSave: (config: ProcessConfig) => void;
  onCancel: () => void;
  className?: string;
}

const PARSER_OPTIONS = [
  { value: "generic", label: "Generic" },
  { value: "python", label: "Python" },
  { value: "javascript", label: "JavaScript" },
  { value: "rust", label: "Rust" },
];

const CATEGORY_OPTIONS = ["backend", "frontend", "database", "build", "testing", "general"];

export function ProcessConfigEditor({
  config,
  onSave,
  onCancel,
  className,
}: ProcessConfigEditorProps) {
  const isNew = !config;
  const [name, setName] = useState(config?.name || "");
  const [command, setCommand] = useState(config?.command || "");
  const [argsStr, setArgsStr] = useState(config?.args.join(" ") || "");
  const [cwd, setCwd] = useState(config?.cwd || "");
  const [healthPort, setHealthPort] = useState(config?.health_port?.toString() || "");
  const [parser, setParser] = useState(config?.parser || "generic");
  const [autoStart, setAutoStart] = useState(config?.auto_start || false);
  const [category, setCategory] = useState(config?.category || "general");
  const [saving, setSaving] = useState(false);

  const browseCwd = useCallback(async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Select working directory",
    });
    if (selected) {
      setCwd(selected as string);
    }
  }, []);

  const handleSave = useCallback(async () => {
    if (!name.trim() || !command.trim() || !cwd.trim()) return;

    setSaving(true);
    try {
      const newConfig: ProcessConfig = {
        id: config?.id || crypto.randomUUID(),
        name: name.trim(),
        command: command.trim(),
        args: argsStr
          .trim()
          .split(/\s+/)
          .filter((a) => a),
        cwd: cwd.trim(),
        env: config?.env || {},
        health_port: healthPort ? parseInt(healthPort) || null : null,
        parser,
        auto_start: autoStart,
        category,
        buffer_size: config?.buffer_size || 2000,
        enabled: true,
      };

      await invoke("save_process_config", { config: newConfig });
      onSave(newConfig);
    } catch (e) {
      console.error("Failed to save config:", e);
    } finally {
      setSaving(false);
    }
  }, [name, command, argsStr, cwd, healthPort, parser, autoStart, category, config, onSave]);

  return (
    <div className={cn("bg-zinc-900 border border-white/10 rounded-lg p-4", className)}>
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-sm font-medium text-zinc-200">
          {isNew ? "Add Process" : "Edit Process"}
        </h3>
        <button onClick={onCancel} className="p-1 text-zinc-500 hover:text-zinc-300">
          <X className="w-4 h-4" />
        </button>
      </div>

      <div className="space-y-3">
        {/* Name */}
        <div>
          <label className="block text-xs text-zinc-400 mb-1">Name</label>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g., FastAPI Backend"
            className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
          />
        </div>

        {/* Command + Args */}
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="block text-xs text-zinc-400 mb-1">Command</label>
            <input
              type="text"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="e.g., python, npm, cargo"
              className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-400 mb-1">Arguments</label>
            <input
              type="text"
              value={argsStr}
              onChange={(e) => setArgsStr(e.target.value)}
              placeholder="e.g., run dev"
              className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
            />
          </div>
        </div>

        {/* Working Directory */}
        <div>
          <label className="block text-xs text-zinc-400 mb-1">Working Directory</label>
          <div className="flex gap-2">
            <input
              type="text"
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              placeholder="/path/to/project"
              className="flex-1 px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
            />
            <button
              onClick={browseCwd}
              className="px-2 py-1.5 bg-zinc-800 border border-white/10 rounded text-zinc-400 hover:text-zinc-200 hover:border-white/20 transition-colors"
            >
              <FolderOpen className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Health Port + Parser + Category */}
        <div className="grid grid-cols-3 gap-3">
          <div>
            <label className="block text-xs text-zinc-400 mb-1">Health Port</label>
            <input
              type="number"
              value={healthPort}
              onChange={(e) => setHealthPort(e.target.value)}
              placeholder="e.g., 8000"
              className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-400 mb-1">Parser</label>
            <select
              value={parser}
              onChange={(e) => setParser(e.target.value)}
              className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 focus:outline-none focus:border-cyan-500/50"
            >
              {PARSER_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs text-zinc-400 mb-1">Category</label>
            <select
              value={category}
              onChange={(e) => setCategory(e.target.value)}
              className="w-full px-3 py-1.5 text-sm bg-zinc-800 border border-white/10 rounded text-zinc-200 focus:outline-none focus:border-cyan-500/50"
            >
              {CATEGORY_OPTIONS.map((cat) => (
                <option key={cat} value={cat}>
                  {cat}
                </option>
              ))}
            </select>
          </div>
        </div>

        {/* Auto-start toggle */}
        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="auto-start"
            checked={autoStart}
            onChange={(e) => setAutoStart(e.target.checked)}
            className="w-4 h-4 rounded border-white/10 bg-zinc-800 text-cyan-500 focus:ring-cyan-500"
          />
          <label htmlFor="auto-start" className="text-xs text-zinc-400">
            Auto-start when runner launches
          </label>
        </div>

        {/* Actions */}
        <div className="flex justify-end gap-2 pt-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs bg-zinc-800 border border-white/10 rounded text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={saving || !name.trim() || !command.trim() || !cwd.trim()}
            className="px-3 py-1.5 text-xs bg-cyan-900/50 border border-cyan-700 rounded text-cyan-300 hover:bg-cyan-800/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-1.5"
          >
            {isNew ? <Plus className="w-3 h-3" /> : <Save className="w-3 h-3" />}
            {saving ? "Saving..." : isNew ? "Add Process" : "Save Changes"}
          </button>
        </div>
      </div>
    </div>
  );
}
