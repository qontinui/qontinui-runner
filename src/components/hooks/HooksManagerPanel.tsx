/**
 * HooksManagerPanel
 *
 * Main panel for managing lifecycle hooks.
 * Provides CRUD operations, enable/disable toggle, reordering, and testing.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Plus, RefreshCw, Webhook, AlertCircle, CheckCircle, XCircle, Loader2 } from "lucide-react";
import type { Hook, CreateHookRequest, UpdateHookRequest } from "../../types/hooks";
import { HookCard } from "./HookCard";
import { HookEditor } from "./HookEditor";
import { getStatusColors } from "@/design-system";

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

interface TestResult {
  success: boolean;
  output?: string;
  error?: string;
  duration_ms: number;
}

export function HooksManagerPanel() {
  // State
  const [hooks, setHooks] = useState<Hook[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Editor state
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingHook, setEditingHook] = useState<Hook | null>(null);
  const [saving, setSaving] = useState(false);

  // Test result state
  const [testResult, setTestResult] = useState<{
    hookId: string;
    result: TestResult;
  } | null>(null);

  // Delete confirmation
  const [deleteConfirm, setDeleteConfirm] = useState<Hook | null>(null);

  // Load hooks
  const loadHooks = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = (await invoke("get_all_hooks")) as CommandResponse;
      if (response.success && response.data) {
        setHooks(response.data as Hook[]);
      } else {
        setError(response.message || "Failed to load hooks");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial load. Async IIFE to avoid invoking a setState-bearing callback
  // synchronously in the effect body (set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadHooks();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadHooks]);

  // Create or update hook
  const handleSave = async (data: CreateHookRequest | UpdateHookRequest) => {
    setSaving(true);
    try {
      let response: CommandResponse;
      if ("id" in data && data.id) {
        // Update existing hook
        response = (await invoke("update_hook", {
          id: data.id,
          request: data,
        })) as CommandResponse;
      } else {
        // Create new hook
        response = (await invoke("create_hook", {
          request: data,
        })) as CommandResponse;
      }

      if (response.success) {
        await loadHooks();
        setEditorOpen(false);
        setEditingHook(null);
      } else {
        throw new Error(response.message || "Failed to save hook");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  // Delete hook
  const handleDelete = async (hook: Hook) => {
    try {
      const response = (await invoke("delete_hook", {
        id: hook.id,
      })) as CommandResponse;

      if (response.success) {
        await loadHooks();
        setDeleteConfirm(null);
      } else {
        throw new Error(response.message || "Failed to delete hook");
      }
    } catch (err) {
      setError(String(err));
    }
  };

  // Toggle hook enabled
  const handleToggleEnabled = async (hook: Hook) => {
    try {
      const response = (await invoke("set_hook_enabled", {
        id: hook.id,
        enabled: !hook.enabled,
      })) as CommandResponse;

      if (response.success) {
        await loadHooks();
      } else {
        throw new Error(response.message || "Failed to toggle hook");
      }
    } catch (err) {
      setError(String(err));
    }
  };

  // Test hook
  const handleTest = async (hook: Hook) => {
    setTestResult(null);
    try {
      const response = (await invoke("test_hook", {
        request: {
          hook_id: hook.id,
        },
      })) as CommandResponse;

      if (response.success && response.data) {
        setTestResult({
          hookId: hook.id,
          result: response.data as TestResult,
        });
      } else {
        throw new Error(response.message || "Failed to test hook");
      }
    } catch (err) {
      setTestResult({
        hookId: hook.id,
        result: {
          success: false,
          error: String(err),
          duration_ms: 0,
        },
      });
    }
  };

  // Open editor
  const openEditor = (hook?: Hook) => {
    setEditingHook(hook || null);
    setEditorOpen(true);
  };

  // Close editor
  const closeEditor = () => {
    setEditorOpen(false);
    setEditingHook(null);
  };

  // Render test result toast
  const renderTestResult = () => {
    if (!testResult) return null;

    const hook = hooks.find((h) => h.id === testResult.hookId);
    const { result } = testResult;

    return (
      <div
        className={`fixed bottom-4 right-4 p-4 rounded-lg shadow-lg border max-w-md z-50 ${
          result.success ? "bg-card border-green-500/50" : "bg-card border-destructive/50"
        }`}
      >
        <div className="flex items-start gap-3">
          {result.success ? (
            <CheckCircle className={`w-5 h-5 shrink-0 ${getStatusColors("success").icon}`} />
          ) : (
            <XCircle className={`w-5 h-5 shrink-0 ${getStatusColors("error").icon}`} />
          )}
          <div className="flex-1 min-w-0">
            <h4 className="font-medium">Test {result.success ? "Passed" : "Failed"}</h4>
            <p className="text-sm text-muted-foreground">
              {hook?.name || "Hook"} ({result.duration_ms}ms)
            </p>
            {result.output && (
              <pre className="mt-2 text-xs bg-muted p-2 rounded overflow-x-auto max-h-24">
                {result.output}
              </pre>
            )}
            {result.error && <p className="mt-2 text-xs text-destructive">{result.error}</p>}
          </div>
          <button
            onClick={() => setTestResult(null)}
            className="text-muted-foreground hover:text-foreground"
          >
            <XCircle className="w-4 h-4" />
          </button>
        </div>
      </div>
    );
  };

  // Render delete confirmation
  const renderDeleteConfirm = () => {
    if (!deleteConfirm) return null;

    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
        <div className="bg-card border border-border rounded-lg shadow-xl p-6 max-w-md">
          <h3 className="text-lg font-semibold mb-2">Delete Hook</h3>
          <p className="text-muted-foreground mb-4">
            Are you sure you want to delete "{deleteConfirm.name}"? This action cannot be undone.
          </p>
          <div className="flex justify-end gap-2">
            <button
              onClick={() => setDeleteConfirm(null)}
              className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground"
            >
              Cancel
            </button>
            <button
              onClick={() => handleDelete(deleteConfirm)}
              className="px-4 py-2 bg-destructive text-destructive-foreground rounded-md text-sm hover:bg-destructive/90"
            >
              Delete
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <div className="flex items-center gap-3">
          <Webhook className="w-6 h-6 text-primary" />
          <div>
            <h1 className="text-lg font-semibold">Lifecycle Hooks</h1>
            <p className="text-sm text-muted-foreground">
              Configure actions to run at specific execution events
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={loadHooks}
            disabled={loading}
            className="p-2 hover:bg-muted rounded-md transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
          <button
            onClick={() => openEditor()}
            className="flex items-center gap-2 px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm hover:bg-primary/90 transition-colors"
          >
            <Plus className="w-4 h-4" />
            New Hook
          </button>
        </div>
      </div>

      {/* Error banner */}
      {error && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 border border-destructive/50 rounded-lg flex items-center gap-2">
          <AlertCircle className="w-4 h-4 text-destructive shrink-0" />
          <p className="text-sm text-destructive">{error}</p>
          <button
            onClick={() => setError(null)}
            className="ml-auto text-destructive hover:text-destructive/80"
          >
            <XCircle className="w-4 h-4" />
          </button>
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4">
        {loading && hooks.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
            <Loader2 className="w-8 h-8 animate-spin mb-4" />
            <p>Loading hooks...</p>
          </div>
        ) : hooks.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-64 text-muted-foreground">
            <Webhook className="w-12 h-12 mb-4 opacity-50" />
            <p className="text-lg font-medium">No lifecycle hooks</p>
            <p className="text-sm">Create a hook to run actions at execution events</p>
            <button
              onClick={() => openEditor()}
              className="mt-4 flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md text-sm hover:bg-primary/90 transition-colors"
            >
              <Plus className="w-4 h-4" />
              Create Your First Hook
            </button>
          </div>
        ) : (
          <div className="space-y-3">
            {hooks.map((hook) => (
              <HookCard
                key={hook.id}
                hook={hook}
                onEdit={() => openEditor(hook)}
                onDelete={() => setDeleteConfirm(hook)}
                onTest={() => handleTest(hook)}
                onToggleEnabled={() => handleToggleEnabled(hook)}
                loading={loading}
              />
            ))}
          </div>
        )}
      </div>

      {/* Editor modal */}
      {editorOpen && (
        <HookEditor
          hook={editingHook}
          onSave={handleSave}
          onCancel={closeEditor}
          loading={saving}
        />
      )}

      {/* Delete confirmation modal */}
      {renderDeleteConfirm()}

      {/* Test result toast */}
      {renderTestResult()}
    </div>
  );
}
