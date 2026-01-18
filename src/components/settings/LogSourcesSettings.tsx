/**
 * LogSourcesSettings.tsx
 *
 * Global log source management - one central place to configure all log sources
 * that can be used across all projects. Supports profiles for grouping sources
 * and AI-assisted selection.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  FileText,
  Plus,
  Trash2,
  Edit2,
  FolderOpen,
  Save,
  X,
  ChevronDown,
  ChevronRight,
  Sparkles,
  RefreshCw,
  Copy,
  Check,
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { LogFunction } from "./types";

// Types matching Rust backend
interface GlobalLogSource {
  id: string;
  name: string;
  description: string;
  category: string;
  type: string;
  path: string;
  pattern?: string;
  tail_lines: number;
  enabled: boolean;
  color?: string;
  keywords: string[];
}

interface GlobalLogSourceProfile {
  id: string;
  name: string;
  description?: string;
  source_ids: string[];
  created_at?: string;
  updated_at?: string;
}

interface GlobalLogSourceSettings {
  sources: GlobalLogSource[];
  profiles: GlobalLogSourceProfile[];
  default_profile_id?: string;
  ai_selection_mode: "dynamic" | "static" | "disabled";
  include_all_when_no_profile: boolean;
}

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

const CATEGORIES = [
  { value: "frontend", label: "Frontend", color: "#3b82f6" },
  { value: "backend", label: "Backend", color: "#22c55e" },
  { value: "api", label: "API", color: "#06b6d4" },
  { value: "mobile", label: "Mobile", color: "#f97316" },
  { value: "database", label: "Database", color: "#8b5cf6" },
  { value: "build", label: "Build", color: "#eab308" },
  { value: "testing", label: "Testing", color: "#ec4899" },
  { value: "runner", label: "Runner", color: "#f97316" },
  { value: "general", label: "General", color: "#6b7280" },
];

interface LogSourcesSettingsProps {
  onLog: LogFunction;
}

export function LogSourcesSettings({ onLog }: LogSourcesSettingsProps) {
  const [settings, setSettings] = useState<GlobalLogSourceSettings>({
    sources: [],
    profiles: [],
    ai_selection_mode: "dynamic",
    include_all_when_no_profile: true,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

  // UI state
  const [expandedSections, setExpandedSections] = useState({
    sources: true,
    profiles: true,
    aiSettings: true,
  });
  const [editingSource, setEditingSource] = useState<GlobalLogSource | null>(null);
  const [editingProfile, setEditingProfile] = useState<GlobalLogSourceProfile | null>(null);
  const [showAddSource, setShowAddSource] = useState(false);
  const [showAddProfile, setShowAddProfile] = useState(false);

  // Load settings on mount
  useEffect(() => {
    loadSettings();
  }, []);

  const loadSettings = async () => {
    setLoading(true);
    try {
      const result = await invoke<TauriResult<GlobalLogSourceSettings>>("get_global_log_sources");
      if (result.success && result.data) {
        setSettings(result.data);
      }
    } catch (err) {
      console.error("Failed to load log source settings:", err);
      onLog("error", `Failed to load settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const saveSettings = async () => {
    setSaving(true);
    try {
      const result = await invoke<TauriResult<null>>("save_global_log_sources", {
        settings,
      });
      if (result.success) {
        onLog("success", "Log source settings saved");
      } else {
        onLog("error", result.message || "Failed to save settings");
      }
    } catch (err) {
      console.error("Failed to save settings:", err);
      onLog("error", `Failed to save settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const migrateFromProjects = async () => {
    try {
      const result = await invoke<TauriResult<{ migrated: number }>>(
        "migrate_project_sources_to_global",
      );
      if (result.success && result.data) {
        onLog("success", `Migrated ${result.data.migrated} log sources`);
        await loadSettings();
      } else {
        onLog("error", result.message || "Migration failed");
      }
    } catch (err) {
      console.error("Migration failed:", err);
      onLog("error", `Migration failed: ${err}`);
    }
  };

  // Source operations
  const addSource = (source: Omit<GlobalLogSource, "id">) => {
    const newSource: GlobalLogSource = {
      ...source,
      id: `source-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
    };
    setSettings((prev) => ({
      ...prev,
      sources: [...prev.sources, newSource],
    }));
    setShowAddSource(false);
  };

  const updateSource = (source: GlobalLogSource) => {
    setSettings((prev) => ({
      ...prev,
      sources: prev.sources.map((s) => (s.id === source.id ? source : s)),
    }));
    setEditingSource(null);
  };

  const deleteSource = (id: string) => {
    setSettings((prev) => ({
      ...prev,
      sources: prev.sources.filter((s) => s.id !== id),
      profiles: prev.profiles.map((p) => ({
        ...p,
        source_ids: p.source_ids.filter((sid) => sid !== id),
      })),
    }));
  };

  const toggleSourceEnabled = (id: string) => {
    setSettings((prev) => ({
      ...prev,
      sources: prev.sources.map((s) => (s.id === id ? { ...s, enabled: !s.enabled } : s)),
    }));
  };

  // Profile operations
  const addProfile = (
    profile: Omit<GlobalLogSourceProfile, "id" | "created_at" | "updated_at">,
  ) => {
    const now = new Date().toISOString();
    const newProfile: GlobalLogSourceProfile = {
      ...profile,
      id: `profile-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
      created_at: now,
      updated_at: now,
    };
    setSettings((prev) => ({
      ...prev,
      profiles: [...prev.profiles, newProfile],
      default_profile_id: prev.default_profile_id || newProfile.id,
    }));
    setShowAddProfile(false);
  };

  const updateProfile = (profile: GlobalLogSourceProfile) => {
    setSettings((prev) => ({
      ...prev,
      profiles: prev.profiles.map((p) =>
        p.id === profile.id ? { ...profile, updated_at: new Date().toISOString() } : p,
      ),
    }));
    setEditingProfile(null);
  };

  const deleteProfile = (id: string) => {
    setSettings((prev) => ({
      ...prev,
      profiles: prev.profiles.filter((p) => p.id !== id),
      default_profile_id: prev.default_profile_id === id ? undefined : prev.default_profile_id,
    }));
  };

  const setDefaultProfile = (id: string | undefined) => {
    setSettings((prev) => ({
      ...prev,
      default_profile_id: id,
    }));
  };

  const setAiSelectionMode = (mode: "dynamic" | "static" | "disabled") => {
    setSettings((prev) => ({
      ...prev,
      ai_selection_mode: mode,
    }));
  };

  const toggleSection = (section: keyof typeof expandedSections) => {
    setExpandedSections((prev) => ({
      ...prev,
      [section]: !prev[section],
    }));
  };

  const getCategoryColor = (category: string) => {
    return CATEGORIES.find((c) => c.value === category)?.color || "#6b7280";
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Log Sources"
        description="Configure global log sources shared across all projects. Use profiles to group sources for different workflows, or let AI automatically select relevant sources."
        icon={<FileText className="w-6 h-6" />}
      />

      {/* Action Buttons */}
      <div className="flex items-center justify-between">
        <button
          onClick={migrateFromProjects}
          className="flex items-center gap-2 px-3 py-1.5 text-xs bg-muted/50 hover:bg-muted rounded-md transition-colors"
        >
          <Copy className="w-3.5 h-3.5" />
          Import from Projects
        </button>
        <button
          onClick={saveSettings}
          disabled={saving}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50 transition-colors"
        >
          {saving ? <RefreshCw className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
          Save Changes
        </button>
      </div>

      {/* AI Selection Mode */}
      <div className="rounded-lg bg-card/50 p-4">
        <button
          onClick={() => toggleSection("aiSettings")}
          className="flex items-center gap-2 w-full text-left"
        >
          {expandedSections.aiSettings ? (
            <ChevronDown className="w-4 h-4" />
          ) : (
            <ChevronRight className="w-4 h-4" />
          )}
          <Sparkles className="w-4 h-4 text-primary" />
          <span className="font-medium text-sm">AI Source Selection</span>
        </button>

        {expandedSections.aiSettings && (
          <div className="mt-4 space-y-3 pl-6">
            <p className="text-xs text-muted-foreground">
              Let AI automatically select relevant log sources based on your task description.
            </p>
            <div className="flex flex-col gap-2">
              {[
                {
                  value: "dynamic" as const,
                  label: "Dynamic",
                  desc: "Re-evaluate at each verification round",
                },
                {
                  value: "static" as const,
                  label: "Static",
                  desc: "Select once at workflow start",
                },
                {
                  value: "disabled" as const,
                  label: "Disabled",
                  desc: "Use profiles or all enabled sources",
                },
              ].map((mode) => (
                <label
                  key={mode.value}
                  className={`flex items-center gap-3 p-2 rounded-md cursor-pointer transition-colors ${
                    settings.ai_selection_mode === mode.value
                      ? "bg-primary/10 border border-primary/20"
                      : "hover:bg-muted/50"
                  }`}
                >
                  <input
                    type="radio"
                    name="ai_mode"
                    checked={settings.ai_selection_mode === mode.value}
                    onChange={() => setAiSelectionMode(mode.value)}
                    className="w-4 h-4"
                  />
                  <div>
                    <div className="text-sm font-medium">{mode.label}</div>
                    <div className="text-xs text-muted-foreground">{mode.desc}</div>
                  </div>
                </label>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Sources Section */}
      <div className="rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <button
            onClick={() => toggleSection("sources")}
            className="flex items-center gap-2 text-left"
          >
            {expandedSections.sources ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            <span className="font-medium text-sm">Log Sources ({settings.sources.length})</span>
          </button>
          <button
            onClick={() => setShowAddSource(true)}
            className="flex items-center gap-1 px-2 py-1 text-xs bg-primary/10 hover:bg-primary/20 text-primary rounded transition-colors"
          >
            <Plus className="w-3.5 h-3.5" />
            Add Source
          </button>
        </div>

        {expandedSections.sources && (
          <div className="mt-4 space-y-2">
            {settings.sources.length === 0 ? (
              <p className="text-xs text-muted-foreground text-center py-4">
                No log sources configured. Add sources or import from existing projects.
              </p>
            ) : (
              settings.sources.map((source) => (
                <SourceRow
                  key={source.id}
                  source={source}
                  onEdit={() => setEditingSource(source)}
                  onDelete={() => deleteSource(source.id)}
                  onToggle={() => toggleSourceEnabled(source.id)}
                  getCategoryColor={getCategoryColor}
                />
              ))
            )}
          </div>
        )}
      </div>

      {/* Profiles Section */}
      <div className="rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <button
            onClick={() => toggleSection("profiles")}
            className="flex items-center gap-2 text-left"
          >
            {expandedSections.profiles ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            <span className="font-medium text-sm">Profiles ({settings.profiles.length})</span>
          </button>
          <button
            onClick={() => setShowAddProfile(true)}
            className="flex items-center gap-1 px-2 py-1 text-xs bg-primary/10 hover:bg-primary/20 text-primary rounded transition-colors"
          >
            <Plus className="w-3.5 h-3.5" />
            Add Profile
          </button>
        </div>

        {expandedSections.profiles && (
          <div className="mt-4 space-y-2">
            {settings.profiles.length === 0 ? (
              <p className="text-xs text-muted-foreground text-center py-4">
                No profiles configured. Profiles group log sources for different workflows.
              </p>
            ) : (
              settings.profiles.map((profile) => (
                <ProfileRow
                  key={profile.id}
                  profile={profile}
                  isDefault={settings.default_profile_id === profile.id}
                  sources={settings.sources}
                  onEdit={() => setEditingProfile(profile)}
                  onDelete={() => deleteProfile(profile.id)}
                  onSetDefault={() => setDefaultProfile(profile.id)}
                />
              ))
            )}
          </div>
        )}
      </div>

      {/* Source Editor Modal */}
      {(showAddSource || editingSource) && (
        <SourceEditor
          source={editingSource}
          onSave={(source) => (editingSource ? updateSource(source) : addSource(source))}
          onCancel={() => {
            setShowAddSource(false);
            setEditingSource(null);
          }}
        />
      )}

      {/* Profile Editor Modal */}
      {(showAddProfile || editingProfile) && (
        <ProfileEditor
          profile={editingProfile}
          sources={settings.sources}
          onSave={(profile) => (editingProfile ? updateProfile(profile) : addProfile(profile))}
          onCancel={() => {
            setShowAddProfile(false);
            setEditingProfile(null);
          }}
        />
      )}
    </div>
  );
}

// Source Row Component
function SourceRow({
  source,
  onEdit,
  onDelete,
  onToggle,
  getCategoryColor,
}: {
  source: GlobalLogSource;
  onEdit: () => void;
  onDelete: () => void;
  onToggle: () => void;
  getCategoryColor: (cat: string) => string;
}) {
  return (
    <div
      className={`flex items-center gap-3 p-2 rounded-md ${source.enabled ? "bg-muted/30" : "bg-muted/10 opacity-60"}`}
    >
      <button
        onClick={onToggle}
        className={`w-4 h-4 rounded border-2 flex items-center justify-center transition-colors ${
          source.enabled ? "bg-primary border-primary" : "border-muted-foreground"
        }`}
      >
        {source.enabled && <Check className="w-3 h-3 text-primary-foreground" />}
      </button>
      <div
        className="w-2 h-2 rounded-full flex-shrink-0"
        style={{ backgroundColor: source.color || getCategoryColor(source.category) }}
      />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{source.name}</div>
        <div className="text-xs text-muted-foreground truncate">{source.path}</div>
      </div>
      <span className="px-1.5 py-0.5 text-[10px] bg-muted rounded capitalize">
        {source.category}
      </span>
      <div className="flex items-center gap-1">
        <button
          onClick={onEdit}
          className="p-1 text-muted-foreground hover:text-foreground transition-colors"
        >
          <Edit2 className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={onDelete}
          className="p-1 text-muted-foreground hover:text-destructive transition-colors"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

// Profile Row Component
function ProfileRow({
  profile,
  isDefault,
  sources,
  onEdit,
  onDelete,
  onSetDefault,
}: {
  profile: GlobalLogSourceProfile;
  isDefault: boolean;
  sources: GlobalLogSource[];
  onEdit: () => void;
  onDelete: () => void;
  onSetDefault: () => void;
}) {
  const sourceCount = profile.source_ids.length;
  const enabledCount = profile.source_ids.filter((id) =>
    sources.find((s) => s.id === id && s.enabled),
  ).length;

  return (
    <div className="flex items-center gap-3 p-2 rounded-md bg-muted/30">
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">{profile.name}</span>
          {isDefault && (
            <span className="px-1.5 py-0.5 text-[10px] bg-primary/20 text-primary rounded">
              Default
            </span>
          )}
        </div>
        <div className="text-xs text-muted-foreground">
          {enabledCount}/{sourceCount} sources enabled
        </div>
      </div>
      <div className="flex items-center gap-1">
        {!isDefault && (
          <button
            onClick={onSetDefault}
            className="px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          >
            Set Default
          </button>
        )}
        <button
          onClick={onEdit}
          className="p-1 text-muted-foreground hover:text-foreground transition-colors"
        >
          <Edit2 className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={onDelete}
          className="p-1 text-muted-foreground hover:text-destructive transition-colors"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

// Source Editor Modal
function SourceEditor({
  source,
  onSave,
  onCancel,
}: {
  source: GlobalLogSource | null;
  onSave: (source: GlobalLogSource | Omit<GlobalLogSource, "id">) => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState({
    name: source?.name || "",
    description: source?.description || "",
    category: source?.category || "general",
    type: source?.type || "file",
    path: source?.path || "",
    pattern: source?.pattern || "",
    tail_lines: source?.tail_lines || 100,
    color: source?.color || "",
    keywords: source?.keywords?.join(", ") || "",
  });

  const handleSubmit = () => {
    if (!form.name || !form.path) return;

    const data = {
      ...(source ? { id: source.id } : {}),
      name: form.name,
      description: form.description,
      category: form.category,
      type: form.type,
      path: form.path,
      pattern: form.pattern || undefined,
      tail_lines: form.tail_lines,
      enabled: source?.enabled ?? true,
      color: form.color || undefined,
      keywords: form.keywords
        .split(",")
        .map((k) => k.trim())
        .filter(Boolean),
    };

    onSave(data as GlobalLogSource);
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background rounded-lg shadow-xl w-full max-w-md p-4 space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="font-medium">{source ? "Edit Source" : "Add Source"}</h3>
          <button onClick={onCancel} className="p-1 hover:bg-muted rounded">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="space-y-3">
          <div>
            <label className="text-xs font-medium">Name *</label>
            <input
              type="text"
              value={form.name}
              onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
              placeholder="Backend Logs"
              className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
            />
          </div>

          <div>
            <label className="text-xs font-medium">Description</label>
            <input
              type="text"
              value={form.description}
              onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
              placeholder="FastAPI backend server logs"
              className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-xs font-medium">Category</label>
              <select
                value={form.category}
                onChange={(e) => setForm((f) => ({ ...f, category: e.target.value }))}
                className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              >
                {CATEGORIES.map((cat) => (
                  <option key={cat.value} value={cat.value}>
                    {cat.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="text-xs font-medium">Type</label>
              <select
                value={form.type}
                onChange={(e) => setForm((f) => ({ ...f, type: e.target.value }))}
                className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              >
                <option value="file">File</option>
                <option value="directory">Directory</option>
              </select>
            </div>
          </div>

          <div>
            <label className="text-xs font-medium">Path *</label>
            <div className="flex gap-2 mt-1">
              <input
                type="text"
                value={form.path}
                onChange={(e) => setForm((f) => ({ ...f, path: e.target.value }))}
                placeholder="/path/to/logs/app.log"
                className="flex-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              />
              <button className="p-2 bg-muted/50 hover:bg-muted rounded-md">
                <FolderOpen className="w-4 h-4" />
              </button>
            </div>
          </div>

          {form.type === "directory" && (
            <div>
              <label className="text-xs font-medium">Pattern</label>
              <input
                type="text"
                value={form.pattern}
                onChange={(e) => setForm((f) => ({ ...f, pattern: e.target.value }))}
                placeholder="*.log"
                className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              />
            </div>
          )}

          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-xs font-medium">Tail Lines</label>
              <input
                type="number"
                value={form.tail_lines}
                onChange={(e) =>
                  setForm((f) => ({ ...f, tail_lines: parseInt(e.target.value) || 100 }))
                }
                min={10}
                max={10000}
                className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              />
            </div>
            <div>
              <label className="text-xs font-medium">Color</label>
              <input
                type="text"
                value={form.color}
                onChange={(e) => setForm((f) => ({ ...f, color: e.target.value }))}
                placeholder="#22c55e"
                className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
              />
            </div>
          </div>

          <div>
            <label className="text-xs font-medium">Keywords (comma-separated)</label>
            <input
              type="text"
              value={form.keywords}
              onChange={(e) => setForm((f) => ({ ...f, keywords: e.target.value }))}
              placeholder="python, fastapi, http, api"
              className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
            />
            <p className="text-[10px] text-muted-foreground mt-1">
              Keywords help AI identify when this source is relevant
            </p>
          </div>
        </div>

        <div className="flex justify-end gap-2 pt-2">
          <button onClick={onCancel} className="px-3 py-1.5 text-sm hover:bg-muted rounded-md">
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={!form.name || !form.path}
            className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50"
          >
            {source ? "Update" : "Add"}
          </button>
        </div>
      </div>
    </div>
  );
}

// Profile Editor Modal
function ProfileEditor({
  profile,
  sources,
  onSave,
  onCancel,
}: {
  profile: GlobalLogSourceProfile | null;
  sources: GlobalLogSource[];
  onSave: (
    profile:
      | GlobalLogSourceProfile
      | Omit<GlobalLogSourceProfile, "id" | "created_at" | "updated_at">,
  ) => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState({
    name: profile?.name || "",
    description: profile?.description || "",
    source_ids: profile?.source_ids || [],
  });

  const toggleSource = (id: string) => {
    setForm((f) => ({
      ...f,
      source_ids: f.source_ids.includes(id)
        ? f.source_ids.filter((sid) => sid !== id)
        : [...f.source_ids, id],
    }));
  };

  const selectByCategory = (category: string) => {
    const categorySourceIds = sources.filter((s) => s.category === category).map((s) => s.id);
    setForm((f) => ({
      ...f,
      source_ids: [...new Set([...f.source_ids, ...categorySourceIds])],
    }));
  };

  const handleSubmit = () => {
    if (!form.name) return;

    const data = {
      ...(profile ? { id: profile.id, created_at: profile.created_at } : {}),
      name: form.name,
      description: form.description || undefined,
      source_ids: form.source_ids,
    };

    onSave(data as GlobalLogSourceProfile);
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background rounded-lg shadow-xl w-full max-w-md p-4 space-y-4 max-h-[80vh] overflow-y-auto">
        <div className="flex items-center justify-between">
          <h3 className="font-medium">{profile ? "Edit Profile" : "Add Profile"}</h3>
          <button onClick={onCancel} className="p-1 hover:bg-muted rounded">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="space-y-3">
          <div>
            <label className="text-xs font-medium">Name *</label>
            <input
              type="text"
              value={form.name}
              onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
              placeholder="Web Development"
              className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
            />
          </div>

          <div>
            <label className="text-xs font-medium">Description</label>
            <input
              type="text"
              value={form.description}
              onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
              placeholder="Sources for web frontend and backend development"
              className="w-full mt-1 px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-none focus:ring-1 focus:ring-primary/50"
            />
          </div>

          <div>
            <div className="flex items-center justify-between mb-2">
              <label className="text-xs font-medium">Sources</label>
              <div className="flex gap-1">
                {["frontend", "backend", "mobile"].map((cat) => (
                  <button
                    key={cat}
                    onClick={() => selectByCategory(cat)}
                    className="px-1.5 py-0.5 text-[10px] bg-muted hover:bg-muted/80 rounded capitalize"
                  >
                    + {cat}
                  </button>
                ))}
              </div>
            </div>
            <div className="space-y-1 max-h-48 overflow-y-auto">
              {sources.map((source) => (
                <label
                  key={source.id}
                  className="flex items-center gap-2 p-1.5 rounded hover:bg-muted/50 cursor-pointer"
                >
                  <input
                    type="checkbox"
                    checked={form.source_ids.includes(source.id)}
                    onChange={() => toggleSource(source.id)}
                    className="w-4 h-4"
                  />
                  <span className="text-sm">{source.name}</span>
                  <span className="text-[10px] text-muted-foreground capitalize">
                    ({source.category})
                  </span>
                </label>
              ))}
            </div>
          </div>
        </div>

        <div className="flex justify-end gap-2 pt-2">
          <button onClick={onCancel} className="px-3 py-1.5 text-sm hover:bg-muted rounded-md">
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={!form.name}
            className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50"
          >
            {profile ? "Update" : "Add"}
          </button>
        </div>
      </div>
    </div>
  );
}
