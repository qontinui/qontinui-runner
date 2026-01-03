/**
 * LogSourceManager.tsx
 *
 * Modal/dialog component for managing external log sources with profile support.
 * Allows users to create, edit, and switch between named log source profiles.
 */

import { useState, useRef, useEffect } from "react";
import {
  X,
  Plus,
  Trash2,
  Save,
  FileText,
  Folder,
  Eye,
  EyeOff,
  ChevronDown,
  Zap,
  FolderOpen,
  Copy,
  Edit3,
  Check,
  Layers,
} from "lucide-react";
import type {
  LogSource,
  LogSourceProfile,
  ProjectLogConfig,
  CommonLogPath,
} from "../types/projectLogs";
import {
  LOG_SOURCE_TEMPLATES,
  COMMON_LOG_PATHS,
  generateLogSourceId,
  createLogSource,
  getActiveProfile,
  getActiveLogSources,
} from "../types/projectLogs";

interface LogSourceManagerProps {
  /** Current project configuration */
  config: ProjectLogConfig;
  /** Whether the modal is open */
  isOpen: boolean;
  /** Callback to close the modal */
  onClose: () => void;
  /** Callback to save changes to the active profile */
  onSave: (sources: LogSource[]) => void;
  /** Callback to create a new profile */
  onCreateProfile?: (name: string, sources: LogSource[]) => void;
  /** Callback to delete a profile */
  onDeleteProfile?: (profileId: string) => void;
  /** Callback to switch active profile */
  onSwitchProfile?: (profileId: string) => void;
  /** Callback to rename a profile */
  onRenameProfile?: (profileId: string, newName: string) => void;
  /** Callback to duplicate a profile */
  onDuplicateProfile?: (profileId: string, newName: string) => void;
  /** Optional project directory for auto-detecting log paths */
  projectDirectory?: string;
}

export function LogSourceManager({
  config,
  isOpen,
  onClose,
  onSave,
  onCreateProfile,
  onDeleteProfile,
  onSwitchProfile,
  onRenameProfile,
  onDuplicateProfile,
  projectDirectory,
}: LogSourceManagerProps) {
  const activeProfile = getActiveProfile(config);
  const [sources, setSources] = useState<LogSource[]>(
    activeProfile?.logSources || getActiveLogSources(config) || [],
  );
  const [hasChanges, setHasChanges] = useState(false);
  const [showQuickAdd, setShowQuickAdd] = useState(false);
  const [showProfileMenu, setShowProfileMenu] = useState(false);
  const [isCreatingProfile, setIsCreatingProfile] = useState(false);
  const [newProfileName, setNewProfileName] = useState("");
  const [editingProfileId, setEditingProfileId] = useState<string | null>(null);
  const [editingProfileName, setEditingProfileName] = useState("");
  const quickAddRef = useRef<HTMLDivElement>(null);
  const profileMenuRef = useRef<HTMLDivElement>(null);

  // Update sources when active profile changes
  useEffect(() => {
    const profile = getActiveProfile(config);
    setSources(profile?.logSources || getActiveLogSources(config) || []);
    setHasChanges(false);
  }, [config.activeProfileId, config]);

  // Close quick add dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (quickAddRef.current && !quickAddRef.current.contains(event.target as Node)) {
        setShowQuickAdd(false);
      }
      if (profileMenuRef.current && !profileMenuRef.current.contains(event.target as Node)) {
        setShowProfileMenu(false);
      }
    }
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  if (!isOpen) return null;

  // Profile management handlers
  const handleCreateProfile = () => {
    if (newProfileName.trim() && onCreateProfile) {
      onCreateProfile(newProfileName.trim(), sources);
      setNewProfileName("");
      setIsCreatingProfile(false);
    }
  };

  const handleSwitchProfile = (profileId: string) => {
    if (hasChanges) {
      // Save current changes before switching
      onSave(sources);
    }
    onSwitchProfile?.(profileId);
    setShowProfileMenu(false);
  };

  const handleStartRename = (profile: LogSourceProfile) => {
    setEditingProfileId(profile.id);
    setEditingProfileName(profile.name);
  };

  const handleFinishRename = () => {
    if (editingProfileId && editingProfileName.trim() && onRenameProfile) {
      onRenameProfile(editingProfileId, editingProfileName.trim());
    }
    setEditingProfileId(null);
    setEditingProfileName("");
  };

  const handleDuplicateProfile = (profile: LogSourceProfile) => {
    if (onDuplicateProfile) {
      onDuplicateProfile(profile.id, `${profile.name} (Copy)`);
    }
    setShowProfileMenu(false);
  };

  const handleDeleteProfile = (profileId: string) => {
    if (onDeleteProfile && config.profiles.length > 1) {
      onDeleteProfile(profileId);
    }
    setShowProfileMenu(false);
  };

  const handleAddSource = () => {
    const newSource = createLogSource();
    setSources([...sources, newSource]);
    setHasChanges(true);
  };

  const handleRemoveSource = (id: string) => {
    setSources(sources.filter((s) => s.id !== id));
    setHasChanges(true);
  };

  const handleUpdateSource = (id: string, updates: Partial<LogSource>) => {
    setSources(sources.map((s) => (s.id === id ? { ...s, ...updates } : s)));
    setHasChanges(true);
  };

  const handleToggleSource = (id: string) => {
    setSources(sources.map((s) => (s.id === id ? { ...s, enabled: !s.enabled } : s)));
    setHasChanges(true);
  };

  const handleApplyTemplate = (templateKey: string) => {
    const template = LOG_SOURCE_TEMPLATES[templateKey];
    if (template) {
      const newSources = template.map((partial) =>
        createLogSource({
          ...partial,
          id: generateLogSourceId(),
        }),
      );
      setSources([...sources, ...newSources]);
      setHasChanges(true);
    }
  };

  const handleQuickAddCommonPath = (commonPath: CommonLogPath) => {
    // Build absolute path from project directory + relative path
    const absolutePath = projectDirectory
      ? `${projectDirectory}/${commonPath.relativePath}`.replace(/\\/g, "/")
      : commonPath.relativePath;

    const newSource = createLogSource({
      name: commonPath.name,
      type: commonPath.type,
      path: absolutePath,
      pattern: commonPath.pattern,
      color: commonPath.color,
      enabled: true,
    });

    setSources([...sources, newSource]);
    setHasChanges(true);
    setShowQuickAdd(false);
  };

  // Group common paths by category
  const commonPathsByCategory = COMMON_LOG_PATHS.reduce(
    (acc, path) => {
      if (!acc[path.category]) acc[path.category] = [];
      acc[path.category].push(path);
      return acc;
    },
    {} as Record<string, CommonLogPath[]>,
  );

  const handleSave = () => {
    onSave(sources);
    setHasChanges(false);
    onClose();
  };

  const handleCancel = () => {
    // Reset to original from active profile
    const profile = getActiveProfile(config);
    setSources(profile?.logSources || getActiveLogSources(config) || []);
    setHasChanges(false);
    setIsCreatingProfile(false);
    setEditingProfileId(null);
    onClose();
  };

  // Color options for log sources
  const colorOptions = [
    { value: "#3b82f6", label: "Blue" },
    { value: "#22c55e", label: "Green" },
    { value: "#f97316", label: "Orange" },
    { value: "#8b5cf6", label: "Purple" },
    { value: "#ef4444", label: "Red" },
    { value: "#eab308", label: "Yellow" },
    { value: "#06b6d4", label: "Cyan" },
    { value: "#ec4899", label: "Pink" },
  ];

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={handleCancel} />

      {/* Modal */}
      <div className="relative bg-background border border-border rounded-lg shadow-xl w-full max-w-2xl max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <h2 className="text-lg font-semibold">Configure Log Sources</h2>
          <button onClick={handleCancel} className="p-1 hover:bg-muted rounded transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Profile Selector */}
        {config.profiles.length > 0 && (
          <div className="px-4 py-2 border-b border-border bg-muted/30">
            <div className="flex items-center gap-2">
              <Layers className="w-4 h-4 text-muted-foreground" />
              <span className="text-sm text-muted-foreground">Profile:</span>
              <div ref={profileMenuRef} className="relative flex-1">
                <button
                  onClick={() => setShowProfileMenu(!showProfileMenu)}
                  className="flex items-center gap-2 px-3 py-1.5 bg-background border border-border rounded-md hover:bg-muted transition-colors text-sm font-medium"
                >
                  {activeProfile?.name || "Select Profile"}
                  <ChevronDown
                    className={`w-4 h-4 transition-transform ${showProfileMenu ? "rotate-180" : ""}`}
                  />
                </button>

                {/* Profile Dropdown Menu */}
                {showProfileMenu && (
                  <div className="absolute top-full left-0 mt-1 w-64 bg-background border border-border rounded-lg shadow-xl z-50 max-h-60 overflow-auto">
                    {config.profiles.map((profile) => (
                      <div
                        key={profile.id}
                        className={`flex items-center justify-between px-3 py-2 hover:bg-muted transition-colors ${
                          profile.id === config.activeProfileId ? "bg-muted/50" : ""
                        }`}
                      >
                        {editingProfileId === profile.id ? (
                          <div className="flex items-center gap-2 flex-1">
                            <input
                              type="text"
                              value={editingProfileName}
                              onChange={(e) => setEditingProfileName(e.target.value)}
                              onKeyDown={(e) => e.key === "Enter" && handleFinishRename()}
                              className="flex-1 bg-background border border-border rounded px-2 py-1 text-sm"
                              autoFocus
                            />
                            <button
                              onClick={handleFinishRename}
                              className="p-1 text-green-500 hover:bg-green-500/10 rounded"
                            >
                              <Check className="w-4 h-4" />
                            </button>
                          </div>
                        ) : (
                          <>
                            <button
                              onClick={() => handleSwitchProfile(profile.id)}
                              className="flex-1 text-left text-sm font-medium"
                            >
                              {profile.name}
                              {profile.id === config.activeProfileId && (
                                <span className="ml-2 text-xs text-primary">(Active)</span>
                              )}
                            </button>
                            <div className="flex items-center gap-1">
                              <button
                                onClick={() => handleStartRename(profile)}
                                className="p-1 text-muted-foreground hover:text-foreground hover:bg-muted rounded"
                                title="Rename"
                              >
                                <Edit3 className="w-3.5 h-3.5" />
                              </button>
                              <button
                                onClick={() => handleDuplicateProfile(profile)}
                                className="p-1 text-muted-foreground hover:text-foreground hover:bg-muted rounded"
                                title="Duplicate"
                              >
                                <Copy className="w-3.5 h-3.5" />
                              </button>
                              {config.profiles.length > 1 && (
                                <button
                                  onClick={() => handleDeleteProfile(profile.id)}
                                  className="p-1 text-muted-foreground hover:text-destructive hover:bg-destructive/10 rounded"
                                  title="Delete"
                                >
                                  <Trash2 className="w-3.5 h-3.5" />
                                </button>
                              )}
                            </div>
                          </>
                        )}
                      </div>
                    ))}

                    {/* Create new profile */}
                    <div className="border-t border-border">
                      {isCreatingProfile ? (
                        <div className="flex items-center gap-2 px-3 py-2">
                          <input
                            type="text"
                            value={newProfileName}
                            onChange={(e) => setNewProfileName(e.target.value)}
                            onKeyDown={(e) => e.key === "Enter" && handleCreateProfile()}
                            placeholder="Profile name..."
                            className="flex-1 bg-background border border-border rounded px-2 py-1 text-sm"
                            autoFocus
                          />
                          <button
                            onClick={handleCreateProfile}
                            disabled={!newProfileName.trim()}
                            className="p-1 text-green-500 hover:bg-green-500/10 rounded disabled:opacity-50"
                          >
                            <Check className="w-4 h-4" />
                          </button>
                          <button
                            onClick={() => {
                              setIsCreatingProfile(false);
                              setNewProfileName("");
                            }}
                            className="p-1 text-muted-foreground hover:bg-muted rounded"
                          >
                            <X className="w-4 h-4" />
                          </button>
                        </div>
                      ) : (
                        <button
                          onClick={() => setIsCreatingProfile(true)}
                          className="w-full flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground hover:text-foreground hover:bg-muted transition-colors"
                        >
                          <Plus className="w-4 h-4" />
                          Create New Profile
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </div>

              {/* Save as new profile button */}
              {hasChanges && onCreateProfile && (
                <button
                  onClick={() => setIsCreatingProfile(true)}
                  className="text-xs text-primary hover:underline"
                >
                  Save as new profile...
                </button>
              )}
            </div>
          </div>
        )}

        {/* Create first profile prompt */}
        {config.profiles.length === 0 && onCreateProfile && (
          <div className="px-4 py-3 border-b border-border bg-muted/30">
            <div className="flex items-center justify-between">
              <span className="text-sm text-muted-foreground">
                No profiles yet. Save your log sources as a profile to switch between
                configurations.
              </span>
              {isCreatingProfile ? (
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    value={newProfileName}
                    onChange={(e) => setNewProfileName(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && handleCreateProfile()}
                    placeholder="Profile name..."
                    className="bg-background border border-border rounded px-2 py-1 text-sm w-40"
                    autoFocus
                  />
                  <button
                    onClick={handleCreateProfile}
                    disabled={!newProfileName.trim()}
                    className="px-2 py-1 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
                  >
                    Create
                  </button>
                  <button
                    onClick={() => {
                      setIsCreatingProfile(false);
                      setNewProfileName("");
                    }}
                    className="p-1 text-muted-foreground hover:bg-muted rounded"
                  >
                    <X className="w-4 h-4" />
                  </button>
                </div>
              ) : (
                <button
                  onClick={() => setIsCreatingProfile(true)}
                  className="flex items-center gap-1 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90"
                >
                  <Plus className="w-4 h-4" />
                  Create Profile
                </button>
              )}
            </div>
          </div>
        )}

        {/* Content */}
        <div className="flex-1 overflow-auto p-4">
          {/* Quick templates */}
          <div className="mb-4">
            <p className="text-sm text-muted-foreground mb-2">Quick add templates:</p>
            <div className="flex gap-2 flex-wrap">
              {Object.keys(LOG_SOURCE_TEMPLATES).map((key) => (
                <button
                  key={key}
                  onClick={() => handleApplyTemplate(key)}
                  className="px-3 py-1 text-sm bg-muted hover:bg-muted/80 rounded-md transition-colors"
                >
                  {key}
                </button>
              ))}
            </div>
          </div>

          {/* Sources list */}
          <div className="space-y-4">
            {sources.length === 0 ? (
              <div className="text-center text-muted-foreground py-8">
                No log sources configured. Add one below or use a template above.
              </div>
            ) : (
              sources.map((source) => (
                <div
                  key={source.id}
                  className="border border-border rounded-lg p-4 space-y-3"
                  style={{ borderLeftWidth: "4px", borderLeftColor: source.color || "#6b7280" }}
                >
                  {/* Source header */}
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      {/* Enable/disable toggle */}
                      <button
                        onClick={() => handleToggleSource(source.id)}
                        className={`p-1 rounded transition-colors ${
                          source.enabled
                            ? "text-green-500 hover:bg-green-500/10"
                            : "text-muted-foreground hover:bg-muted"
                        }`}
                        title={
                          source.enabled
                            ? "Enabled - click to disable"
                            : "Disabled - click to enable"
                        }
                      >
                        {source.enabled ? (
                          <Eye className="w-5 h-5" />
                        ) : (
                          <EyeOff className="w-5 h-5" />
                        )}
                      </button>

                      {/* Name input */}
                      <input
                        type="text"
                        value={source.name}
                        onChange={(e) => handleUpdateSource(source.id, { name: e.target.value })}
                        className="bg-transparent border-b border-border focus:border-primary focus:outline-none text-sm font-medium"
                        placeholder="Source name"
                      />
                    </div>

                    {/* Delete button */}
                    <button
                      onClick={() => handleRemoveSource(source.id)}
                      className="p-1 text-muted-foreground hover:text-destructive hover:bg-destructive/10 rounded transition-colors"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>

                  {/* Source details */}
                  <div className="grid grid-cols-2 gap-3">
                    {/* Type selector */}
                    <div>
                      <label className="text-xs text-muted-foreground block mb-1">Type</label>
                      <select
                        value={source.type}
                        onChange={(e) =>
                          handleUpdateSource(source.id, {
                            type: e.target.value as "file" | "directory",
                          })
                        }
                        className="w-full bg-muted border border-border rounded px-2 py-1.5 text-sm"
                      >
                        <option value="file">Single File</option>
                        <option value="directory">Directory</option>
                      </select>
                    </div>

                    {/* Tail lines */}
                    <div>
                      <label className="text-xs text-muted-foreground block mb-1">Tail Lines</label>
                      <input
                        type="number"
                        value={source.tailLines || 100}
                        onChange={(e) =>
                          handleUpdateSource(source.id, {
                            tailLines: parseInt(e.target.value) || 100,
                          })
                        }
                        min={10}
                        max={10000}
                        className="w-full bg-muted border border-border rounded px-2 py-1.5 text-sm"
                      />
                    </div>
                  </div>

                  {/* Path input */}
                  <div>
                    <label className="text-xs text-muted-foreground block mb-1">
                      {source.type === "file" ? "File Path" : "Directory Path"}
                    </label>
                    <div className="flex gap-2">
                      <div className="flex-1 flex items-center gap-2 bg-muted border border-border rounded px-2 py-1.5">
                        {source.type === "file" ? (
                          <FileText className="w-4 h-4 text-muted-foreground" />
                        ) : (
                          <Folder className="w-4 h-4 text-muted-foreground" />
                        )}
                        <input
                          type="text"
                          value={source.path}
                          onChange={(e) => handleUpdateSource(source.id, { path: e.target.value })}
                          placeholder={
                            source.type === "file" ? "/path/to/your/app.log" : "/path/to/logs/"
                          }
                          className="flex-1 bg-transparent text-sm focus:outline-none"
                        />
                      </div>
                    </div>
                  </div>

                  {/* Pattern (for directory type) */}
                  {source.type === "directory" && (
                    <div>
                      <label className="text-xs text-muted-foreground block mb-1">
                        File Pattern (glob)
                      </label>
                      <input
                        type="text"
                        value={source.pattern || ""}
                        onChange={(e) => handleUpdateSource(source.id, { pattern: e.target.value })}
                        placeholder="*.log"
                        className="w-full bg-muted border border-border rounded px-2 py-1.5 text-sm"
                      />
                    </div>
                  )}

                  {/* Color picker */}
                  <div>
                    <label className="text-xs text-muted-foreground block mb-1">Color</label>
                    <div className="flex gap-2">
                      {colorOptions.map((color) => (
                        <button
                          key={color.value}
                          onClick={() => handleUpdateSource(source.id, { color: color.value })}
                          className={`w-6 h-6 rounded-full border-2 transition-all ${
                            source.color === color.value
                              ? "border-foreground scale-110"
                              : "border-transparent hover:scale-110"
                          }`}
                          style={{ backgroundColor: color.value }}
                          title={color.label}
                        />
                      ))}
                    </div>
                  </div>
                </div>
              ))
            )}
          </div>

          {/* Add source buttons */}
          <div className="mt-4 flex gap-2">
            {/* Quick add dropdown */}
            <div ref={quickAddRef} className="relative">
              <button
                onClick={() => setShowQuickAdd(!showQuickAdd)}
                className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
              >
                <Zap className="w-4 h-4" />
                Quick Add
                <ChevronDown
                  className={`w-4 h-4 transition-transform ${showQuickAdd ? "rotate-180" : ""}`}
                />
              </button>

              {/* Dropdown menu */}
              {showQuickAdd && (
                <div className="absolute top-full left-0 mt-2 w-72 bg-background border border-border rounded-lg shadow-xl z-50 max-h-80 overflow-auto">
                  {/* Project directory info */}
                  {projectDirectory && (
                    <div className="px-3 py-2 text-xs text-muted-foreground border-b border-border bg-muted/50">
                      <div className="flex items-center gap-2">
                        <FolderOpen className="w-3 h-3" />
                        <span className="truncate" title={projectDirectory}>
                          {projectDirectory}
                        </span>
                      </div>
                    </div>
                  )}

                  {/* Categorized common paths */}
                  {Object.entries(commonPathsByCategory).map(([category, paths]) => (
                    <div key={category}>
                      <div className="px-3 py-1.5 text-xs font-semibold text-muted-foreground uppercase tracking-wider bg-muted/30">
                        {category}
                      </div>
                      {paths.map((commonPath) => (
                        <button
                          key={commonPath.name}
                          onClick={() => handleQuickAddCommonPath(commonPath)}
                          className="w-full text-left px-3 py-2 hover:bg-muted transition-colors flex items-start gap-3"
                        >
                          <div
                            className="w-3 h-3 rounded-full mt-1 flex-shrink-0"
                            style={{ backgroundColor: commonPath.color }}
                          />
                          <div className="flex-1 min-w-0">
                            <div className="text-sm font-medium">{commonPath.name}</div>
                            <div className="text-xs text-muted-foreground truncate">
                              {commonPath.relativePath}
                            </div>
                          </div>
                        </button>
                      ))}
                    </div>
                  ))}
                </div>
              )}
            </div>

            {/* Manual add button */}
            <button
              onClick={handleAddSource}
              className="flex-1 flex items-center justify-center gap-2 px-4 py-2 border-2 border-dashed border-border rounded-lg text-muted-foreground hover:text-foreground hover:border-foreground/50 transition-colors"
            >
              <Plus className="w-4 h-4" />
              Add Custom Source
            </button>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-border">
          <p className="text-xs text-muted-foreground">
            {sources.length} source{sources.length !== 1 ? "s" : ""} configured
            {hasChanges && " (unsaved changes)"}
          </p>
          <div className="flex gap-2">
            <button
              onClick={handleCancel}
              className="px-4 py-2 text-sm border border-border rounded-md hover:bg-muted transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleSave}
              disabled={!hasChanges}
              className="flex items-center gap-2 px-4 py-2 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              <Save className="w-4 h-4" />
              Save Changes
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
