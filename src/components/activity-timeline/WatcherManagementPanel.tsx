/**
 * Watcher Management Panel — CRUD UI for screenpipe-inspired scheduled reactive agents.
 *
 * Each watcher queries the activity timeline on a schedule, reasons with AI,
 * and triggers a conditional action (run workflow, create observation, or log only).
 *
 * All interactive elements are registered with UI Bridge for AI control.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent, useUIElement } from "ui-bridge";
import {
  Eye,
  Plus,
  Trash2,
  ToggleLeft,
  ToggleRight,
  Clock,
  X,
} from "lucide-react";

interface Watcher {
  id: string;
  name: string;
  scheduleJson: string;
  timelineQuery: string;
  appNameFilter: string | null;
  sourceTypeFilter: string | null;
  lookbackWindow: string;
  reasoningPrompt: string;
  actionJson: string;
  enabled: boolean;
  lastRunAt: string | null;
  lastResultJson: string | null;
  createdAt: string;
  updatedAt: string;
}

interface WatcherFormData {
  name: string;
  timelineQuery: string;
  appNameFilter: string;
  sourceTypeFilter: string;
  lookbackWindow: string;
  reasoningPrompt: string;
  scheduleInterval: string;
  actionType: string;
}

const DEFAULT_FORM: WatcherFormData = {
  name: "",
  timelineQuery: "",
  appNameFilter: "",
  sourceTypeFilter: "",
  lookbackWindow: "15 minutes",
  reasoningPrompt:
    "You are a watcher agent. Here are {{result_count}} results from the activity timeline matching the query '{{query}}':\n\n{{results}}\n\nBased on these results, should any action be taken? Respond with your analysis.",
  scheduleInterval: "5 minutes",
  actionType: "LogOnly",
};

export function WatcherManagementPanel() {
  const [watchers, setWatchers] = useState<Watcher[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [form, setForm] = useState<WatcherFormData>({ ...DEFAULT_FORM });
  const [editingId, setEditingId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const loadWatchers = useCallback(async () => {
    try {
      const list = await invoke<Watcher[]>("list_watchers");
      setWatchers(list);
    } catch {
      // PG not available
    }
  }, []);

  useEffect(() => {
    loadWatchers();
  }, [loadWatchers]);

  const handleCreate = async () => {
    if (!form.name.trim() || !form.timelineQuery.trim()) return;
    setLoading(true);
    try {
      await invoke("create_watcher", {
        input: {
          name: form.name,
          scheduleJson: JSON.stringify({
            type: "interval",
            value: form.scheduleInterval,
          }),
          timelineQuery: form.timelineQuery,
          appNameFilter: form.appNameFilter || null,
          sourceTypeFilter: form.sourceTypeFilter || null,
          lookbackWindow: form.lookbackWindow,
          reasoningPrompt: form.reasoningPrompt,
          actionJson: JSON.stringify({ type: form.actionType }),
        },
      });
      setShowForm(false);
      setForm({ ...DEFAULT_FORM });
      await loadWatchers();
    } catch (err) {
      console.error("Failed to create watcher:", err);
    } finally {
      setLoading(false);
    }
  };

  const handleToggle = async (id: string, enabled: boolean) => {
    try {
      await invoke("set_watcher_enabled", { id, enabled: !enabled });
      await loadWatchers();
    } catch (err) {
      console.error("Failed to toggle watcher:", err);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await invoke("delete_watcher", { id });
      await loadWatchers();
    } catch (err) {
      console.error("Failed to delete watcher:", err);
    }
  };

  const formatTime = (iso: string | null) => {
    if (!iso) return "Never";
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  };

  // ---- UI Bridge registrations ----

  useUIComponent({
    id: "watcher-management",
    name: "Watcher Management",
    description: "CRUD panel for scheduled reactive AI agents that monitor the activity timeline",
    actions: [
      {
        id: "new-watcher",
        label: "New Watcher",
        handler: async () => { setShowForm(true); setEditingId(null); setForm({ ...DEFAULT_FORM }); },
      },
      {
        id: "cancel-form",
        label: "Cancel Form",
        handler: async () => { setShowForm(false); },
      },
      {
        id: "refresh",
        label: "Refresh List",
        handler: async () => { await loadWatchers(); },
      },
    ],
  });

  const { ref: newWatcherBtnRef } = useUIElement({
    id: "watchers-new-button",
    type: "button",
    label: showForm ? "Cancel" : "New Watcher",
    actions: ["click"],
  });

  const { ref: nameInputRef } = useUIElement({
    id: "watchers-name-input",
    type: "input",
    label: "Watcher name",
  });

  const { ref: queryInputRef } = useUIElement({
    id: "watchers-query-input",
    type: "input",
    label: "Timeline FTS query",
  });

  const { ref: scheduleSelectRef } = useUIElement({
    id: "watchers-schedule-select",
    type: "select",
    label: "Schedule interval",
  });

  const { ref: lookbackSelectRef } = useUIElement({
    id: "watchers-lookback-select",
    type: "select",
    label: "Lookback window",
  });

  const { ref: actionSelectRef } = useUIElement({
    id: "watchers-action-select",
    type: "select",
    label: "Action type",
  });

  const { ref: appFilterInputRef } = useUIElement({
    id: "watchers-app-filter-input",
    type: "input",
    label: "App name filter (optional)",
  });

  const { ref: sourceFilterSelectRef } = useUIElement({
    id: "watchers-source-filter-select",
    type: "select",
    label: "Source type filter (optional)",
  });

  const { ref: promptTextareaRef } = useUIElement({
    id: "watchers-prompt-textarea",
    type: "input",
    label: "AI reasoning prompt template",
  });

  const { ref: createBtnRef } = useUIElement({
    id: "watchers-create-button",
    type: "button",
    label: loading ? "Creating..." : "Create Watcher",
    actions: ["click"],
  });

  const { ref: watcherListRef } = useUIElement({
    id: "watchers-list",
    type: "generic",
    label: `Watcher list (${watchers.length} watchers)`,
  });

  return (
    <div className="flex flex-col h-full p-4 gap-3" data-tutorial-id="watchers-page">
      {/* Header */}
      <div className="flex items-center justify-between" data-tutorial-id="watchers-header">
        <div className="flex items-center gap-2">
          <Eye className="h-5 w-5 text-primary" />
          <h2 className="text-lg font-semibold">Watchers</h2>
          <span className="text-xs text-muted-foreground">
            ({watchers.length})
          </span>
        </div>
        <button
          ref={newWatcherBtnRef}
          onClick={() => {
            setShowForm(!showForm);
            setEditingId(null);
            setForm({ ...DEFAULT_FORM });
          }}
          className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90"
        >
          {showForm ? <X className="h-4 w-4" /> : <Plus className="h-4 w-4" />}
          {showForm ? "Cancel" : "New Watcher"}
        </button>
      </div>

      {/* Create/Edit Form */}
      {showForm && (
        <div className="border border-border rounded-md p-4 space-y-3 bg-card" data-tutorial-id="watchers-create-form">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-xs font-medium text-muted-foreground">Name</label>
              <input
                ref={nameInputRef}
                type="text"
                placeholder="e.g., Error Toast Watcher"
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
              />
            </div>
            <div>
              <label className="text-xs font-medium text-muted-foreground">Timeline Query (FTS)</label>
              <input
                ref={queryInputRef}
                type="text"
                placeholder="e.g., error toast failed"
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.timelineQuery}
                onChange={(e) => setForm({ ...form, timelineQuery: e.target.value })}
              />
            </div>
          </div>
          <div className="grid grid-cols-3 gap-3">
            <div>
              <label className="text-xs font-medium text-muted-foreground">Schedule Interval</label>
              <select
                ref={scheduleSelectRef}
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.scheduleInterval}
                onChange={(e) => setForm({ ...form, scheduleInterval: e.target.value })}
              >
                <option value="1 minute">Every 1 minute</option>
                <option value="5 minutes">Every 5 minutes</option>
                <option value="15 minutes">Every 15 minutes</option>
                <option value="30 minutes">Every 30 minutes</option>
                <option value="1 hour">Every 1 hour</option>
              </select>
            </div>
            <div>
              <label className="text-xs font-medium text-muted-foreground">Lookback Window</label>
              <select
                ref={lookbackSelectRef}
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.lookbackWindow}
                onChange={(e) => setForm({ ...form, lookbackWindow: e.target.value })}
              >
                <option value="5 minutes">5 minutes</option>
                <option value="15 minutes">15 minutes</option>
                <option value="30 minutes">30 minutes</option>
                <option value="1 hour">1 hour</option>
                <option value="6 hours">6 hours</option>
                <option value="1 day">1 day</option>
              </select>
            </div>
            <div>
              <label className="text-xs font-medium text-muted-foreground">Action</label>
              <select
                ref={actionSelectRef}
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.actionType}
                onChange={(e) => setForm({ ...form, actionType: e.target.value })}
              >
                <option value="LogOnly">Log Only</option>
                <option value="CreateObservation">Create Observation</option>
                <option value="Notify">Notify</option>
              </select>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="text-xs font-medium text-muted-foreground">App Filter (optional)</label>
              <input
                ref={appFilterInputRef}
                type="text"
                placeholder="e.g., Chrome"
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.appNameFilter}
                onChange={(e) => setForm({ ...form, appNameFilter: e.target.value })}
              />
            </div>
            <div>
              <label className="text-xs font-medium text-muted-foreground">Source Filter (optional)</label>
              <select
                ref={sourceFilterSelectRef}
                className="w-full px-3 py-1.5 mt-1 rounded-md border border-border bg-background text-sm"
                value={form.sourceTypeFilter}
                onChange={(e) => setForm({ ...form, sourceTypeFilter: e.target.value })}
              >
                <option value="">All sources</option>
                <option value="ui_bridge">UI Bridge</option>
                <option value="accessibility">Accessibility</option>
                <option value="ocr">OCR</option>
              </select>
            </div>
          </div>
          <div>
            <label className="text-xs font-medium text-muted-foreground">
              AI Reasoning Prompt
            </label>
            <textarea
              ref={promptTextareaRef}
              className="w-full px-3 py-2 mt-1 rounded-md border border-border bg-background text-sm font-mono resize-y"
              rows={4}
              value={form.reasoningPrompt}
              onChange={(e) => setForm({ ...form, reasoningPrompt: e.target.value })}
            />
            <p className="text-xs text-muted-foreground mt-1">
              Available placeholders: {"{{results}}"}, {"{{result_count}}"}, {"{{query}}"}
            </p>
          </div>
          <button
            ref={createBtnRef}
            onClick={handleCreate}
            disabled={loading || !form.name.trim() || !form.timelineQuery.trim()}
            className="px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
          >
            {loading ? "Creating..." : "Create Watcher"}
          </button>
        </div>
      )}

      {/* Watcher list */}
      <div ref={watcherListRef} className="flex-1 overflow-auto">
        {watchers.length === 0 && !showForm && (
          <div className="text-center text-muted-foreground py-8 text-sm">
            No watchers configured. Create one to start monitoring the activity timeline.
          </div>
        )}
        {watchers.map((w) => (
          <WatcherCard
            key={w.id}
            watcher={w}
            onToggle={handleToggle}
            onDelete={handleDelete}
            formatTime={formatTime}
          />
        ))}
      </div>
    </div>
  );
}

/** Individual watcher card with its own UI Bridge element registrations. */
function WatcherCard({
  watcher: w,
  onToggle,
  onDelete,
  formatTime,
}: {
  watcher: Watcher;
  onToggle: (id: string, enabled: boolean) => void;
  onDelete: (id: string) => void;
  formatTime: (iso: string | null) => string;
}) {
  const { ref: toggleRef } = useUIElement({
    id: `watcher-toggle-${w.id}`,
    type: "button",
    label: w.enabled ? `Disable watcher "${w.name}"` : `Enable watcher "${w.name}"`,
    actions: ["click"],
  });

  const { ref: deleteRef } = useUIElement({
    id: `watcher-delete-${w.id}`,
    type: "button",
    label: `Delete watcher "${w.name}"`,
    actions: ["click"],
  });

  const { ref: cardRef } = useUIElement({
    id: `watcher-card-${w.id}`,
    type: "generic",
    label: `Watcher: ${w.name} (${w.enabled ? "enabled" : "disabled"}) — query: "${w.timelineQuery}", lookback: ${w.lookbackWindow}, last run: ${formatTime(w.lastRunAt)}`,
  });

  return (
    <div
      ref={cardRef}
      className="border border-border rounded-md p-3 mb-2 hover:bg-accent/50 transition-colors"
    >
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <button
            ref={toggleRef}
            onClick={() => onToggle(w.id, w.enabled)}
            className="text-muted-foreground hover:text-foreground"
            title={w.enabled ? "Disable" : "Enable"}
          >
            {w.enabled ? (
              <ToggleRight className="h-5 w-5 text-green-500" />
            ) : (
              <ToggleLeft className="h-5 w-5" />
            )}
          </button>
          <span className="font-medium text-sm">{w.name}</span>
          <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
            {w.timelineQuery}
          </code>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-muted-foreground flex items-center gap-1">
            <Clock className="h-3 w-3" />
            {w.lookbackWindow}
          </span>
          <button
            ref={deleteRef}
            onClick={() => onDelete(w.id)}
            className="text-muted-foreground hover:text-destructive"
            title="Delete"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        </div>
      </div>
      <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
        <span>Last run: {formatTime(w.lastRunAt)}</span>
        {w.appNameFilter && <span>App: {w.appNameFilter}</span>}
        {w.sourceTypeFilter && <span>Source: {w.sourceTypeFilter}</span>}
      </div>
    </div>
  );
}
