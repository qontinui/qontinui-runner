/**
 * LibraryTab.tsx
 *
 * Unified Library view that combines all automation assets into a single cohesive view:
 * - Tasks (single-step AI tasks)
 * - Workflows (multi-step AI workflows)
 * - Scripts (Playwright scripts)
 * - Scriptlets (reusable snippets)
 * - Contexts (AI knowledge base)
 *
 * Features:
 * - Left sidebar with category navigation
 * - Search and filter capabilities
 * - Grid/list view toggle
 * - Collapsible detail panel
 * - Keyboard shortcuts (Ctrl+N, Delete, Enter)
 */

import { useState, useEffect, useCallback, useRef } from "react";
import {
  FileText,
  Sparkles,
  TestTube,
  Search,
  Play,
  Trash2,
  Loader2,
  Tag,
  FolderOpen,
  Settings,
  Target,
  List,
  Link,
  Code,
  Plus,
  Grid3X3,
  LayoutList,
  X,
  Copy,
  BookOpen,
  Clock,
  Filter,
  Puzzle,
  Pencil,
  ShieldCheck,
  MousePointer2,
  Globe,
} from "lucide-react";
import { useContexts } from "./contexts/hooks/useContexts";
import { ContextCard } from "./contexts/ContextCard";
import { ContextEditor } from "./contexts/ContextEditor";
import type {
  ContextWithMetadata,
  CreateContextRequest,
  UpdateContextRequest,
} from "../types/context";
import type {
  Scriptlet,
  SavedExploration,
  ExplorationConfig,
  SavedApiRequest,
  ApiVariableExtraction,
  ApiAssertion,
} from "../types";
import type { SavedMacro } from "../types/macro";
import type { HttpMethod, ApiContentType, UnifiedWorkflow } from "../types/unified-workflow";
import type { AiOutputLine } from "./AiOutputTab";
import { getAccentColors, getStatusColors } from "@/design-system";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

// Category definitions
type LibraryCategory =
  | "tasks"
  | "unified-workflows"
  | "macros"
  | "scripts"
  | "scriptlets"
  | "contexts"
  | "state-explorer"
  | "api-requests";

// Accent color type for categories
type CategoryAccentColor =
  | "amber"
  | "green"
  | "orange"
  | "purple"
  | "cyan"
  | "blue"
  | "emerald"
  | "indigo";

interface CategoryConfig {
  id: LibraryCategory;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  accentColor: CategoryAccentColor;
  description: string;
}

const CATEGORIES: CategoryConfig[] = [
  {
    id: "tasks",
    label: "Tasks",
    icon: FileText,
    accentColor: "amber",
    description: "Single-step AI tasks",
  },
  {
    id: "unified-workflows",
    label: "Workflows",
    icon: Sparkles,
    accentColor: "green",
    description: "Phase-based automation workflows",
  },
  {
    id: "macros",
    label: "Macros",
    icon: MousePointer2,
    accentColor: "orange",
    description: "Sequential GUI automation",
  },
  {
    id: "scripts",
    label: "Scripts",
    icon: TestTube,
    accentColor: "purple",
    description: "Playwright scripts",
  },
  {
    id: "scriptlets",
    label: "Scriptlets",
    icon: Puzzle,
    accentColor: "cyan",
    description: "Reusable snippets",
  },
  {
    id: "contexts",
    label: "Contexts",
    icon: BookOpen,
    accentColor: "blue",
    description: "AI knowledge base",
  },
  {
    id: "state-explorer",
    label: "State Explorer",
    icon: ShieldCheck,
    accentColor: "emerald",
    description: "State exploration configs",
  },
  {
    id: "api-requests",
    label: "API Requests",
    icon: Globe,
    accentColor: "indigo",
    description: "Saved HTTP request templates",
  },
];

// Task types
interface SavedPrompt {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  max_sessions: number | null;
  created_at: string;
  modified_at: string;
}

// Playwright Script types
interface PlaywrightScript {
  id: string;
  name: string;
  description: string;
  target_url: string;
  script_content: string;
  created_at: string;
  modified_at: string;
}

interface LibraryTabProps {
  onLog: (level: LogLevel, message: string) => void;
  onNavigateToActive: () => void;
  onNavigateToBuilder?: () => void;
  onNavigateToScriptBuilder?: () => void;
  onNavigateToMacroBuilder?: () => void;
  onEditScript?: (scriptId: string) => void;
  onEditWorkflow?: (workflowId: string) => void;
  onEditMacro?: (macroId: string) => void;
  aiOutputLines?: AiOutputLine[];
}

type ViewMode = "grid" | "list";

const STORAGE_KEY_CATEGORY = "qontinui-library-category";
const STORAGE_KEY_VIEW_MODE = "qontinui-library-view-mode";
const API_BASE = "http://localhost:9876";

export function LibraryTab({
  onLog,
  onNavigateToActive,
  onNavigateToBuilder,
  onNavigateToScriptBuilder,
  onNavigateToMacroBuilder,
  onEditScript,
  onEditWorkflow,
  onEditMacro,
  aiOutputLines: _aiOutputLines = [],
}: LibraryTabProps) {
  // Category and view state
  const [selectedCategory, setSelectedCategory] = useState<LibraryCategory>(() => {
    const saved = localStorage.getItem(STORAGE_KEY_CATEGORY);
    return (saved as LibraryCategory) || "tasks";
  });
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    const saved = localStorage.getItem(STORAGE_KEY_VIEW_MODE);
    return (saved as ViewMode) || "list";
  });

  // Data state
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [macros, setMacros] = useState<SavedMacro[]>([]);
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [scriptlets, setScriptlets] = useState<Scriptlet[]>([]);
  const [explorations, setExplorations] = useState<SavedExploration[]>([]);
  const [savedApiRequests, setSavedApiRequests] = useState<SavedApiRequest[]>([]);
  const [unifiedWorkflows, setUnifiedWorkflows] = useState<UnifiedWorkflow[]>([]);

  // Loading states
  const [loading, setLoading] = useState(true);
  const [runningId, setRunningId] = useState<string | null>(null);

  // UI state
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedItemId, setSelectedItemId] = useState<string | null>(null);
  const [showDetailPanel, setShowDetailPanel] = useState(false);
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [showTagDropdown, setShowTagDropdown] = useState(false);

  // Creation/editing state
  const [_showCreateForm, _setShowCreateForm] = useState(false);
  const [_editingItem, _setEditingItem] = useState<unknown | null>(null);

  // Task creation/editing dialog state
  const [showTaskDialog, setShowTaskDialog] = useState(false);
  const [editingTaskId, setEditingTaskId] = useState<string | null>(null);
  const [taskForm, setTaskForm] = useState({
    name: "",
    description: "",
    content: "",
    max_sessions: null as number | null,
    category: "general",
    tags: [] as string[],
  });
  const [isSavingTask, setIsSavingTask] = useState(false);

  // Scriptlet creation/editing dialog state
  const [showScriptletDialog, setShowScriptletDialog] = useState(false);
  const [editingScriptletId, setEditingScriptletId] = useState<string | null>(null);
  const [scriptletForm, setScriptletForm] = useState({
    name: "",
    content: "",
    category: "general",
    tags: [] as string[],
  });
  const [isSavingScriptlet, setIsSavingScriptlet] = useState(false);

  // Exploration creation/editing dialog state
  const [showExplorationDialog, setShowExplorationDialog] = useState(false);
  const [editingExplorationId, setEditingExplorationId] = useState<string | null>(null);
  const [explorationForm, setExplorationForm] = useState<{
    name: string;
    description: string;
    strategy: ExplorationConfig["strategy"];
    max_states: number;
    max_duration_seconds: number;
    capture_screenshots: boolean;
    stop_on_first_failure: boolean;
    state_delay_ms: number;
    tags: string[];
  }>({
    name: "",
    description: "",
    strategy: "smoke_test",
    max_states: 0,
    max_duration_seconds: 0,
    capture_screenshots: true,
    stop_on_first_failure: false,
    state_delay_ms: 500,
    tags: [],
  });
  const [isSavingExploration, setIsSavingExploration] = useState(false);

  // API Request creation/editing dialog state
  const [showApiRequestDialog, setShowApiRequestDialog] = useState(false);
  const [editingApiRequestId, setEditingApiRequestId] = useState<string | null>(null);
  const [apiRequestForm, setApiRequestForm] = useState({
    name: "",
    description: "",
    category: "general",
    tags: [] as string[],
    method: "GET" as HttpMethod,
    url: "",
    headers: {} as Record<string, string>,
    body: "",
    body_content_type: "application/json" as ApiContentType,
    timeout_ms: 30000,
    follow_redirects: true,
    variable_extractions: [] as ApiVariableExtraction[],
    assertions: [] as ApiAssertion[],
    credential_id: undefined as string | undefined,
  });
  const [isSavingApiRequest, setIsSavingApiRequest] = useState(false);

  // Context hook
  const {
    contexts,
    filteredContexts,
    categories: contextCategories,
    tags: contextTags,
    createContext,
    updateContext,
    deleteContext,
    duplicateContext,
    enableContext,
    disableContext,
    refresh: _refreshContexts,
    loading: contextsLoading,
  } = useContexts();

  // Context editor state
  const [contextEditorState, setContextEditorState] = useState<{
    isOpen: boolean;
    context: ContextWithMetadata | null;
    mode: "create" | "edit" | "view";
    createScope?: "project" | "user";
  }>({
    isOpen: false,
    context: null,
    mode: "create",
  });
  const [isSavingContext, setIsSavingContext] = useState(false);

  // Refs
  const containerRef = useRef<HTMLDivElement>(null);

  // Persist category and view mode
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_CATEGORY, selectedCategory);
  }, [selectedCategory]);

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY_VIEW_MODE, viewMode);
  }, [viewMode]);

  // Fetch all data on mount
  useEffect(() => {
    const fetchAll = async () => {
      setLoading(true);
      try {
        const [
          promptsRes,
          macrosRes,
          scriptsRes,
          scriptletsRes,
          explorationsRes,
          savedApiRequestsRes,
          unifiedWorkflowsRes,
        ] = await Promise.all([
          fetch(`${API_BASE}/prompts`),
          fetch(`${API_BASE}/macros`),
          fetch(`${API_BASE}/playwright/scripts`),
          fetch(`${API_BASE}/scriptlets`),
          fetch(`${API_BASE}/saved-explorations`),
          fetch(`${API_BASE}/saved-api-requests`),
          fetch(`${API_BASE}/unified-workflows`),
        ]);

        const [
          promptsData,
          macrosData,
          scriptsData,
          scriptletsData,
          explorationsData,
          savedApiRequestsData,
          unifiedWorkflowsData,
        ] = await Promise.all([
          promptsRes.json(),
          macrosRes.json(),
          scriptsRes.json(),
          scriptletsRes.json(),
          explorationsRes.json().catch(() => ({ success: false })),
          savedApiRequestsRes.json().catch(() => ({ success: false })),
          unifiedWorkflowsRes.json().catch(() => ({ success: false })),
        ]);

        if (promptsData.success) setPrompts(promptsData.data || []);
        if (macrosData.success) setMacros(macrosData.data || []);
        if (scriptsData.success) setScripts(scriptsData.data || []);
        if (scriptletsData.success) setScriptlets(scriptletsData.data || []);
        if (explorationsData.success) setExplorations(explorationsData.data || []);
        if (savedApiRequestsData.success) setSavedApiRequests(savedApiRequestsData.data || []);
        if (unifiedWorkflowsData.success) setUnifiedWorkflows(unifiedWorkflowsData.data || []);
      } catch (error) {
        onLog("error", `Failed to fetch library data: ${error}`);
      } finally {
        setLoading(false);
      }
    };
    fetchAll();
  }, [onLog]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ctrl+N: New item
      if (e.ctrlKey && e.key === "n") {
        e.preventDefault();
        handleCreateNew();
      }
      // Delete: Delete selected
      if (e.key === "Delete" && selectedItemId) {
        e.preventDefault();
        handleDeleteSelected();
      }
      // Enter: Run selected
      if (e.key === "Enter" && selectedItemId && !e.ctrlKey && !e.shiftKey) {
        e.preventDefault();
        handleRunSelected();
      }
      // Escape: Close detail panel
      if (e.key === "Escape") {
        setShowDetailPanel(false);
        setSelectedItemId(null);
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [selectedItemId, selectedCategory]);

  // Create new item
  const handleCreateNew = () => {
    switch (selectedCategory) {
      case "tasks":
        setTaskForm({
          name: "",
          description: "",
          content: "",
          max_sessions: null,
          category: "general",
          tags: [],
        });
        setShowTaskDialog(true);
        break;
      case "macros":
        // Navigate to Macro Builder tab
        if (onNavigateToMacroBuilder) {
          onNavigateToMacroBuilder();
        } else {
          onLog("info", "Macro Builder not available");
        }
        break;
      case "scripts":
        // Navigate to Script Builder tab
        if (onNavigateToScriptBuilder) {
          onNavigateToScriptBuilder();
        } else {
          onLog("info", "Script Builder not available");
        }
        break;
      case "scriptlets":
        setScriptletForm({
          name: "",
          content: "",
          category: "general",
          tags: [],
        });
        setShowScriptletDialog(true);
        break;
      case "contexts":
        setContextEditorState({
          isOpen: true,
          context: null,
          mode: "create",
          createScope: "user",
        });
        break;
      case "state-explorer":
        setExplorationForm({
          name: "",
          description: "",
          strategy: "smoke_test",
          max_states: 0,
          max_duration_seconds: 0,
          capture_screenshots: true,
          stop_on_first_failure: false,
          state_delay_ms: 500,
          tags: [],
        });
        setShowExplorationDialog(true);
        break;
      case "api-requests":
        setApiRequestForm({
          name: "",
          description: "",
          category: "general",
          tags: [],
          method: "GET",
          url: "",
          headers: {},
          body: "",
          body_content_type: "application/json",
          timeout_ms: 30000,
          follow_redirects: true,
          variable_extractions: [],
          assertions: [],
          credential_id: undefined,
        });
        setEditingApiRequestId(null);
        setShowApiRequestDialog(true);
        break;
      case "unified-workflows":
        // Navigate to Workflow Builder for creation
        onLog("info", "Open Workflow Builder to create a new workflow");
        break;
    }
  };

  // Save task (create or update)
  const handleSaveTask = async () => {
    if (!taskForm.name.trim() || !taskForm.content.trim()) {
      onLog("warning", "Task name and content are required");
      return;
    }

    setIsSavingTask(true);
    try {
      const isEditing = editingTaskId !== null;
      const url = isEditing ? `${API_BASE}/prompts/${editingTaskId}` : `${API_BASE}/prompts`;
      const response = await fetch(url, {
        method: isEditing ? "PUT" : "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: taskForm.name.trim(),
          description: taskForm.description.trim(),
          content: taskForm.content.trim(),
          max_sessions: taskForm.max_sessions,
          category: taskForm.category,
          tags: taskForm.tags,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        if (isEditing) {
          setPrompts((prev) => prev.map((p) => (p.id === editingTaskId ? result.data : p)));
          onLog("success", `Updated task: ${taskForm.name}`);
        } else {
          setPrompts((prev) => [...prev, result.data]);
          onLog("success", `Created task: ${taskForm.name}`);
        }
        setShowTaskDialog(false);
        setEditingTaskId(null);
      } else {
        throw new Error(result.error || `Failed to ${isEditing ? "update" : "create"} task`);
      }
    } catch (error) {
      onLog("error", `Failed to ${editingTaskId ? "update" : "create"} task: ${error}`);
    } finally {
      setIsSavingTask(false);
    }
  };

  // Save scriptlet (create or update)
  const handleSaveScriptlet = async () => {
    if (!scriptletForm.name.trim() || !scriptletForm.content.trim()) {
      onLog("warning", "Scriptlet name and content are required");
      return;
    }

    setIsSavingScriptlet(true);
    try {
      const isEditing = editingScriptletId !== null;
      const url = isEditing
        ? `${API_BASE}/scriptlets/${editingScriptletId}`
        : `${API_BASE}/scriptlets`;
      const response = await fetch(url, {
        method: isEditing ? "PUT" : "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: scriptletForm.name.trim(),
          content: scriptletForm.content.trim(),
          category: scriptletForm.category,
          tags: scriptletForm.tags,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        if (isEditing) {
          setScriptlets((prev) => prev.map((s) => (s.id === editingScriptletId ? result.data : s)));
          onLog("success", `Updated scriptlet: ${scriptletForm.name}`);
        } else {
          setScriptlets((prev) => [...prev, result.data]);
          onLog("success", `Created scriptlet: ${scriptletForm.name}`);
        }
        setShowScriptletDialog(false);
        setEditingScriptletId(null);
      } else {
        throw new Error(result.error || `Failed to ${isEditing ? "update" : "create"} scriptlet`);
      }
    } catch (error) {
      onLog("error", `Failed to ${editingScriptletId ? "update" : "create"} scriptlet: ${error}`);
    } finally {
      setIsSavingScriptlet(false);
    }
  };

  // Save exploration (create or update)
  const handleSaveExploration = async () => {
    if (!explorationForm.name.trim()) {
      onLog("warning", "Exploration name is required");
      return;
    }

    setIsSavingExploration(true);
    try {
      const isEditing = editingExplorationId !== null;
      const url = isEditing
        ? `${API_BASE}/saved-explorations/${editingExplorationId}`
        : `${API_BASE}/saved-explorations`;
      const response = await fetch(url, {
        method: isEditing ? "PUT" : "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: explorationForm.name.trim(),
          description: explorationForm.description.trim(),
          config: {
            strategy: explorationForm.strategy,
            max_states: explorationForm.max_states,
            max_duration_seconds: explorationForm.max_duration_seconds,
            target_state_ids: [],
            target_transition_ids: [],
            capture_screenshots: explorationForm.capture_screenshots,
            capture_transition_screenshots: false,
            state_delay_ms: explorationForm.state_delay_ms,
            stop_on_first_failure: explorationForm.stop_on_first_failure,
          },
          tags: explorationForm.tags,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        if (isEditing) {
          setExplorations((prev) =>
            prev.map((v) => (v.id === editingExplorationId ? result.data : v)),
          );
          onLog("success", `Updated exploration: ${explorationForm.name}`);
        } else {
          setExplorations((prev) => [...prev, result.data]);
          onLog("success", `Created exploration: ${explorationForm.name}`);
        }
        setShowExplorationDialog(false);
        setEditingExplorationId(null);
      } else {
        throw new Error(result.error || `Failed to ${isEditing ? "update" : "create"} exploration`);
      }
    } catch (error) {
      onLog(
        "error",
        `Failed to ${editingExplorationId ? "update" : "create"} exploration: ${error}`,
      );
    } finally {
      setIsSavingExploration(false);
    }
  };

  // Delete exploration
  const deleteExploration = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/saved-explorations/${id}`, { method: "DELETE" });
      const result = await response.json();
      if (result.success) {
        setExplorations((prev) => prev.filter((v) => v.id !== id));
        onLog("success", "Exploration deleted");
      } else {
        throw new Error(result.error || "Failed to delete exploration");
      }
    } catch (error) {
      onLog("error", `Failed to delete exploration: ${error}`);
    }
  };

  // Edit handlers
  const handleEditTask = (task: SavedPrompt) => {
    setTaskForm({
      name: task.name,
      description: task.description || "",
      content: task.content,
      max_sessions: task.max_sessions,
      category: task.category || "general",
      tags: task.tags || [],
    });
    setEditingTaskId(task.id);
    setShowTaskDialog(true);
  };

  const handleEditScriptlet = (scriptlet: Scriptlet) => {
    setScriptletForm({
      name: scriptlet.name,
      content: scriptlet.content,
      category: scriptlet.category || "general",
      tags: scriptlet.tags || [],
    });
    setEditingScriptletId(scriptlet.id);
    setShowScriptletDialog(true);
  };

  const handleEditExploration = (exploration: SavedExploration) => {
    setExplorationForm({
      name: exploration.name,
      description: exploration.description || "",
      strategy: exploration.config.strategy,
      max_states: exploration.config.max_states,
      max_duration_seconds: exploration.config.max_duration_seconds,
      capture_screenshots: exploration.config.capture_screenshots,
      stop_on_first_failure: exploration.config.stop_on_first_failure || false,
      state_delay_ms: exploration.config.state_delay_ms,
      tags: exploration.tags || [],
    });
    setEditingExplorationId(exploration.id);
    setShowExplorationDialog(true);
  };

  const handleEditApiRequest = (apiRequest: SavedApiRequest) => {
    setApiRequestForm({
      name: apiRequest.name,
      description: apiRequest.description || "",
      category: apiRequest.category || "general",
      tags: apiRequest.tags || [],
      method: apiRequest.method,
      url: apiRequest.url,
      headers: apiRequest.headers || {},
      body: apiRequest.body || "",
      body_content_type: apiRequest.body_content_type || "application/json",
      timeout_ms: apiRequest.timeout_ms || 30000,
      follow_redirects: apiRequest.follow_redirects ?? true,
      variable_extractions: apiRequest.variable_extractions || [],
      assertions: apiRequest.assertions || [],
      credential_id: apiRequest.credential_id,
    });
    setEditingApiRequestId(apiRequest.id);
    setShowApiRequestDialog(true);
  };

  // Save API request (create or update)
  const handleSaveApiRequest = async () => {
    if (!apiRequestForm.name.trim()) {
      onLog("warning", "API request name is required");
      return;
    }
    if (!apiRequestForm.url.trim()) {
      onLog("warning", "API request URL is required");
      return;
    }

    setIsSavingApiRequest(true);
    try {
      const isEditing = editingApiRequestId !== null;
      const url = isEditing
        ? `${API_BASE}/saved-api-requests/${editingApiRequestId}`
        : `${API_BASE}/saved-api-requests`;
      const response = await fetch(url, {
        method: isEditing ? "PUT" : "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: apiRequestForm.name.trim(),
          description: apiRequestForm.description.trim(),
          category: apiRequestForm.category,
          tags: apiRequestForm.tags,
          method: apiRequestForm.method,
          url: apiRequestForm.url.trim(),
          headers: apiRequestForm.headers,
          body: apiRequestForm.body || undefined,
          body_content_type: apiRequestForm.body_content_type,
          timeout_ms: apiRequestForm.timeout_ms,
          follow_redirects: apiRequestForm.follow_redirects,
          variable_extractions: apiRequestForm.variable_extractions,
          assertions: apiRequestForm.assertions,
          credential_id: apiRequestForm.credential_id || undefined,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        if (isEditing) {
          setSavedApiRequests((prev) =>
            prev.map((r) => (r.id === editingApiRequestId ? result.data : r)),
          );
          onLog("success", `Updated API request: ${apiRequestForm.name}`);
        } else {
          setSavedApiRequests((prev) => [result.data, ...prev]);
          onLog("success", `Created API request: ${apiRequestForm.name}`);
        }
        setShowApiRequestDialog(false);
        setEditingApiRequestId(null);
      } else {
        throw new Error(result.error || `Failed to ${isEditing ? "update" : "create"} API request`);
      }
    } catch (error) {
      onLog(
        "error",
        `Failed to ${editingApiRequestId ? "update" : "create"} API request: ${error}`,
      );
    } finally {
      setIsSavingApiRequest(false);
    }
  };

  const handleDuplicateApiRequest = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/saved-api-requests/${id}/duplicate`, {
        method: "POST",
      });
      const data = await response.json();
      if (data.success) {
        setSavedApiRequests((prev) => [data.data, ...prev]);
        onLog("success", "API request duplicated");
      } else {
        onLog("error", `Failed to duplicate: ${data.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to duplicate API request: ${error}`);
    }
  };

  const deleteApiRequest = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/saved-api-requests/${id}`, {
        method: "DELETE",
      });
      const data = await response.json();
      if (data.success) {
        setSavedApiRequests((prev) => prev.filter((r) => r.id !== id));
        onLog("success", "API request deleted");
        if (selectedItemId === id) {
          setSelectedItemId(null);
          setShowDetailPanel(false);
        }
      } else {
        onLog("error", `Failed to delete: ${data.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete API request: ${error}`);
    }
  };

  // Delete selected item
  const handleDeleteSelected = async () => {
    if (!selectedItemId) return;

    switch (selectedCategory) {
      case "tasks":
        await deletePrompt(selectedItemId);
        break;
      case "macros":
        await deleteMacro(selectedItemId);
        break;
      case "scripts":
        await deleteScript(selectedItemId);
        break;
      case "scriptlets":
        await deleteScriptlet(selectedItemId);
        break;
      case "contexts": {
        const context = contexts.find((c) => c.id === selectedItemId);
        if (context) {
          await deleteContext(context.scope, context.id);
        }
        break;
      }
      case "state-explorer":
        await deleteExploration(selectedItemId);
        break;
      case "api-requests":
        await deleteApiRequest(selectedItemId);
        break;
      case "unified-workflows":
        await deleteUnifiedWorkflow(selectedItemId);
        break;
    }
    setSelectedItemId(null);
    setShowDetailPanel(false);
  };

  // Delete unified workflow
  const deleteUnifiedWorkflow = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/unified-workflows/${id}`, {
        method: "DELETE",
      });
      if (response.ok) {
        setUnifiedWorkflows((prev) => prev.filter((w) => w.id !== id));
        onLog("success", "Workflow deleted");
      }
    } catch (error) {
      onLog("error", `Failed to delete workflow: ${error}`);
    }
  };

  // Run selected item
  const handleRunSelected = () => {
    if (!selectedItemId) return;

    switch (selectedCategory) {
      case "tasks": {
        const prompt = prompts.find((p) => p.id === selectedItemId);
        if (prompt) runPrompt(prompt);
        break;
      }
      case "macros": {
        const macro = macros.find((m) => m.id === selectedItemId);
        if (macro) runMacro(macro);
        break;
      }
      case "scripts": {
        const script = scripts.find((s) => s.id === selectedItemId);
        if (script) runScript(script);
        break;
      }
      case "state-explorer": {
        const exploration = explorations.find((v) => v.id === selectedItemId);
        if (exploration) runExploration(exploration);
        break;
      }
    }
  };

  // Run handlers
  const runPrompt = useCallback(
    async (task: SavedPrompt) => {
      setRunningId(task.id);
      try {
        const response = await fetch(`${API_BASE}/sessions/start`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            name: task.name,
            prompt: task.content,
            uses_gui: false,
            timeout_seconds: 1800,
          }),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started task: ${task.name}`);
          onNavigateToActive();
        } else {
          throw new Error(result.error || "Failed to run task");
        }
      } catch (error) {
        onLog("error", `Failed to run task: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToActive],
  );

  const runMacro = useCallback(
    async (macro: SavedMacro) => {
      setRunningId(macro.id);
      try {
        const response = await fetch(`${API_BASE}/macros/${macro.id}/run`, {
          method: "POST",
        });

        const result = await response.json();
        if (result.success) {
          const data = result.data;
          if (data.failed_steps === 0) {
            onLog(
              "success",
              `Completed macro: ${macro.name} (${data.successful_steps}/${data.total_steps} steps)`,
            );
          } else {
            onLog(
              "warning",
              `Macro completed with failures: ${data.failed_steps}/${data.total_steps} steps failed`,
            );
          }
        } else {
          throw new Error(result.error || "Failed to run macro");
        }
      } catch (error) {
        onLog("error", `Failed to run macro: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog],
  );

  const runScript = useCallback(
    async (script: PlaywrightScript) => {
      setRunningId(script.id);
      try {
        const response = await fetch(`${API_BASE}/playwright/scripts/${script.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started script: ${script.name}`);
          onNavigateToActive();
        } else {
          throw new Error(result.error || "Failed to run script");
        }
      } catch (error) {
        onLog("error", `Failed to run script: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToActive],
  );

  const runExploration = useCallback(
    async (exploration: SavedExploration) => {
      setRunningId(exploration.id);
      try {
        const response = await fetch(`${API_BASE}/saved-explorations/${exploration.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started exploration: ${exploration.name}`);
          // Update last_run_at and run_count
          setExplorations((prev) =>
            prev.map((v) =>
              v.id === exploration.id
                ? { ...v, last_run_at: new Date().toISOString(), run_count: v.run_count + 1 }
                : v,
            ),
          );
        } else {
          throw new Error(result.error || "Failed to run exploration");
        }
      } catch (error) {
        onLog("error", `Failed to run exploration: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog],
  );

  // Delete handlers
  const deletePrompt = useCallback(
    async (id: string) => {
      if (!confirm("Delete this task?")) return;
      try {
        const response = await fetch(`${API_BASE}/prompts/${id}`, { method: "DELETE" });
        const result = await response.json();
        if (result.success) {
          setPrompts((prev) => prev.filter((p) => p.id !== id));
          onLog("info", "Task deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete task: ${error}`);
      }
    },
    [onLog],
  );

  const deleteMacro = useCallback(
    async (id: string) => {
      if (!confirm("Delete this macro?")) return;
      try {
        const response = await fetch(`${API_BASE}/macros/${id}`, { method: "DELETE" });
        const result = await response.json();
        if (result.success) {
          setMacros((prev) => prev.filter((m) => m.id !== id));
          onLog("info", "Macro deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete macro: ${error}`);
      }
    },
    [onLog],
  );

  const deleteScript = useCallback(
    async (id: string) => {
      if (!confirm("Delete this script?")) return;
      try {
        const response = await fetch(`${API_BASE}/playwright/scripts/${id}`, { method: "DELETE" });
        const result = await response.json();
        if (result.success) {
          setScripts((prev) => prev.filter((s) => s.id !== id));
          onLog("info", "Script deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete script: ${error}`);
      }
    },
    [onLog],
  );

  const deleteScriptlet = useCallback(
    async (id: string) => {
      if (!confirm("Delete this scriptlet?")) return;
      try {
        const response = await fetch(`${API_BASE}/scriptlets/${id}`, { method: "DELETE" });
        const result = await response.json();
        if (result.success) {
          setScriptlets((prev) => prev.filter((s) => s.id !== id));
          onLog("info", "Scriptlet deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete scriptlet: ${error}`);
      }
    },
    [onLog],
  );

  // Duplicate handler
  const duplicateItem = useCallback(
    async (id: string) => {
      let endpoint = "";
      let _updateState: (prev: unknown[]) => unknown[];

      switch (selectedCategory) {
        case "tasks":
          endpoint = `${API_BASE}/prompts/${id}/duplicate`;
          _updateState = (prev: unknown[]) => [...prev];
          break;
        case "scripts":
          endpoint = `${API_BASE}/playwright/scripts/${id}/duplicate`;
          _updateState = (prev: unknown[]) => [...prev];
          break;
        case "scriptlets":
          endpoint = `${API_BASE}/scriptlets/${id}/duplicate`;
          _updateState = (prev: unknown[]) => [...prev];
          break;
        default:
          return;
      }

      try {
        const response = await fetch(endpoint, { method: "POST" });
        const result = await response.json();
        if (result.success && result.data) {
          switch (selectedCategory) {
            case "tasks":
              setPrompts((prev) => [...prev, result.data]);
              break;
            case "scripts":
              setScripts((prev) => [...prev, result.data]);
              break;
            case "scriptlets":
              setScriptlets((prev) => [...prev, result.data]);
              break;
          }
          onLog("success", "Item duplicated");
        }
      } catch (error) {
        onLog("error", `Failed to duplicate: ${error}`);
      }
    },
    [selectedCategory, onLog],
  );

  // Get all unique tags for current category
  const getAllTags = (): string[] => {
    const tags = new Set<string>();
    switch (selectedCategory) {
      case "tasks":
        prompts.forEach((p) => p.tags?.forEach((t) => tags.add(t)));
        break;
      case "macros":
        macros.forEach((m) => m.tags?.forEach((t) => tags.add(t)));
        break;
      case "scriptlets":
        scriptlets.forEach((s) => s.tags?.forEach((t) => tags.add(t)));
        break;
      case "contexts":
        contexts.forEach((c) => c.tags?.forEach((t) => tags.add(t)));
        break;
      case "api-requests":
        savedApiRequests.forEach((r) => r.tags?.forEach((t) => tags.add(t)));
        break;
      case "unified-workflows":
        unifiedWorkflows.forEach((w) => w.tags?.forEach((t) => tags.add(t)));
        break;
    }
    return Array.from(tags).sort();
  };

  // Filter items based on search and tag
  const filterItems = <T extends { name: string; tags?: string[] }>(
    items: T[],
    getSearchFields: (item: T) => string[],
  ): T[] => {
    return items.filter((item) => {
      const matchesSearch =
        !searchQuery ||
        getSearchFields(item).some((field) =>
          field.toLowerCase().includes(searchQuery.toLowerCase()),
        );
      const matchesTag = !tagFilter || item.tags?.includes(tagFilter);
      return matchesSearch && matchesTag;
    });
  };

  const filteredPrompts = filterItems(prompts, (p) => [p.name, p.description, p.category]);
  const filteredMacros = filterItems(macros, (m) => [
    m.name,
    m.description,
    m.category,
  ]);
  const filteredScripts = filterItems(
    scripts.map((s) => ({ ...s, tags: [] })),
    (s) => [s.name, s.description, s.target_url],
  );
  const filteredScriptlets = filterItems(scriptlets, (s) => [s.name, s.content, s.category]);
  const filteredExplorations = filterItems(explorations, (v) => [
    v.name,
    v.description || "",
    v.config.strategy,
  ]);
  const filteredSavedApiRequests = filterItems(savedApiRequests, (r) => [
    r.name,
    r.description,
    r.url,
    r.category,
  ]);
  const filteredUnifiedWorkflows = filterItems(unifiedWorkflows, (w) => [
    w.name,
    w.description,
    w.category,
  ]);

  const formatDate = (dateStr: string) => {
    const date = new Date(dateStr);
    return date.toLocaleDateString();
  };

  const formatRelativeDate = (dateStr: string) => {
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMins / 60);
    const diffDays = Math.floor(diffHours / 24);

    if (diffMins < 1) return "Just now";
    if (diffMins < 60) return `${diffMins}m ago`;
    if (diffHours < 24) return `${diffHours}h ago`;
    if (diffDays < 7) return `${diffDays}d ago`;
    return formatDate(dateStr);
  };

  // Context handlers
  const handleContextEdit = (context: ContextWithMetadata) => {
    setContextEditorState({
      isOpen: true,
      context,
      mode: context.scope === "builtin" ? "view" : "edit",
    });
  };

  const handleContextDelete = async (context: ContextWithMetadata) => {
    if (context.scope === "builtin") {
      onLog("warning", "Cannot delete built-in contexts");
      return;
    }
    if (!confirm(`Delete context "${context.name}"?`)) return;
    const success = await deleteContext(context.scope, context.id);
    if (success) {
      onLog("success", `Deleted context: ${context.name}`);
    } else {
      onLog("error", `Failed to delete context: ${context.name}`);
    }
  };

  const handleContextDuplicate = async (context: ContextWithMetadata) => {
    const result = await duplicateContext(context.scope, context.id, "user");
    if (result) {
      onLog("success", `Duplicated context: ${result.name}`);
    } else {
      onLog("error", `Failed to duplicate context`);
    }
  };

  const handleContextToggleEnabled = async (context: ContextWithMetadata) => {
    const success = context.enabled
      ? await disableContext(context.id)
      : await enableContext(context.id);
    if (success) {
      onLog("success", `${context.enabled ? "Disabled" : "Enabled"} context: ${context.name}`);
    }
  };

  const handleContextSave = async (
    data: CreateContextRequest | UpdateContextRequest,
  ): Promise<boolean> => {
    setIsSavingContext(true);
    try {
      if (contextEditorState.mode === "create") {
        const result = await createContext(
          contextEditorState.createScope || "user",
          data as CreateContextRequest,
        );
        if (result) {
          onLog("success", `Created context: ${result.name}`);
          return true;
        }
      } else if (contextEditorState.mode === "edit" && contextEditorState.context) {
        const result = await updateContext(
          contextEditorState.context.scope,
          contextEditorState.context.id,
          data as UpdateContextRequest,
        );
        if (result) {
          onLog("success", `Updated context: ${result.name}`);
          return true;
        }
      }
      return false;
    } finally {
      setIsSavingContext(false);
    }
  };

  // Get current items based on selected category
  const getCurrentItems = () => {
    switch (selectedCategory) {
      case "tasks":
        return filteredPrompts;
      case "macros":
        return filteredMacros;
      case "scripts":
        return filteredScripts;
      case "scriptlets":
        return filteredScriptlets;
      case "contexts":
        return filteredContexts;
      case "state-explorer":
        return filteredExplorations;
      case "api-requests":
        return filteredSavedApiRequests;
      case "unified-workflows":
        return filteredUnifiedWorkflows;
      default:
        return [];
    }
  };

  const currentItems = getCurrentItems();
  const allTags = getAllTags();
  const categoryConfig = CATEGORIES.find((c) => c.id === selectedCategory)!;
  const CategoryIcon = categoryConfig.icon;

  if (loading || contextsLoading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 className="w-8 h-8 animate-spin text-primary" />
      </div>
    );
  }

  // Render item card based on category
  const renderItemCard = (item: unknown, isSelected: boolean) => {
    const baseClasses = `p-3 rounded-lg border cursor-pointer transition-all ${
      isSelected
        ? "border-primary bg-primary/5"
        : "border-border hover:border-primary/30 hover:bg-muted/20"
    }`;

    switch (selectedCategory) {
      case "tasks": {
        const task = item as SavedPrompt;
        return (
          <div
            key={task.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(task.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <FileText
                className={`w-5 h-5 ${getAccentColors("amber").text} flex-shrink-0 mt-0.5`}
              />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{task.name}</span>
                  {task.max_sessions && (
                    <span
                      className={`text-xs ${getAccentColors("blue").bg} ${getAccentColors("blue").text} px-1.5 py-0.5 rounded`}
                    >
                      Max {task.max_sessions}
                    </span>
                  )}
                </div>
                {task.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {task.description}
                  </p>
                )}
                <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {formatRelativeDate(task.modified_at)}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleEditTask(task);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit task"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runPrompt(task);
                  }}
                  disabled={runningId === task.id}
                  className={`p-1.5 ${getAccentColors("amber").bg} ${getAccentColors("amber").text} rounded hover:opacity-80 transition-colors disabled:opacity-50`}
                  title="Run task"
                >
                  {runningId === task.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                </button>
              </div>
            </div>
            {task.tags && task.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {task.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {task.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">+{task.tags.length - 3}</span>
                )}
              </div>
            )}
          </div>
        );
      }

      case "macros": {
        const macro = item as SavedMacro;
        return (
          <div
            key={macro.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(macro.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <MousePointer2
                className={`w-5 h-5 ${getAccentColors("orange").text} flex-shrink-0 mt-0.5`}
              />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{macro.name}</span>
                  <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                    {macro.steps.length} steps
                  </span>
                </div>
                {macro.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {macro.description}
                  </p>
                )}
                <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {formatRelativeDate(macro.modified_at)}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onEditMacro?.(macro.id);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit macro"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runMacro(macro);
                  }}
                  disabled={runningId === macro.id}
                  className={`p-1.5 ${getAccentColors("orange").bg} ${getAccentColors("orange").text} rounded hover:${getAccentColors("orange").bgSolid}/20 transition-colors disabled:opacity-50`}
                  title="Run macro"
                >
                  {runningId === macro.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                </button>
              </div>
            </div>
            {macro.tags && macro.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {macro.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {macro.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">
                    +{macro.tags.length - 3}
                  </span>
                )}
              </div>
            )}
          </div>
        );
      }

      case "scripts": {
        const script = item as PlaywrightScript;
        return (
          <div
            key={script.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(script.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <TestTube
                className={`w-5 h-5 ${getAccentColors("purple").text} flex-shrink-0 mt-0.5`}
              />
              <div className="flex-1 min-w-0">
                <span className="font-medium truncate block">{script.name}</span>
                {script.target_url && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {script.target_url}
                  </p>
                )}
                <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {formatRelativeDate(script.modified_at)}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onEditScript?.(script.id);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit script"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runScript(script);
                  }}
                  disabled={runningId === script.id}
                  className={`p-1.5 ${getAccentColors("purple").bg} ${getAccentColors("purple").text} rounded hover:${getAccentColors("purple").bgSolid}/20 transition-colors disabled:opacity-50`}
                  title="Run script"
                >
                  {runningId === script.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                </button>
              </div>
            </div>
          </div>
        );
      }

      case "scriptlets": {
        const scriptlet = item as Scriptlet;
        return (
          <div
            key={scriptlet.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(scriptlet.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <Puzzle className={`w-5 h-5 ${getAccentColors("cyan").text} flex-shrink-0 mt-0.5`} />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{scriptlet.name}</span>
                  <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                    {scriptlet.category}
                  </span>
                </div>
                <p className="text-xs text-muted-foreground line-clamp-2 mt-0.5">
                  {scriptlet.content}
                </p>
                <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {formatRelativeDate(scriptlet.modified_at)}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleEditScriptlet(scriptlet);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit scriptlet"
                >
                  <Pencil className="w-4 h-4" />
                </button>
              </div>
            </div>
            {scriptlet.tags && scriptlet.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {scriptlet.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {scriptlet.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">
                    +{scriptlet.tags.length - 3}
                  </span>
                )}
              </div>
            )}
          </div>
        );
      }

      case "state-explorer": {
        const exploration = item as SavedExploration;
        return (
          <div
            key={exploration.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(exploration.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <ShieldCheck
                className={`w-5 h-5 ${getAccentColors("emerald").text} flex-shrink-0 mt-0.5`}
              />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{exploration.name}</span>
                  <span
                    className={`text-xs ${getAccentColors("emerald").bg} ${getAccentColors("emerald").text} px-1.5 py-0.5 rounded`}
                  >
                    {exploration.config.strategy}
                  </span>
                </div>
                {exploration.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {exploration.description}
                  </p>
                )}
                <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                  <span className="flex items-center gap-1">
                    <Clock className="w-3 h-3" />
                    {formatRelativeDate(exploration.modified_at)}
                  </span>
                  {exploration.run_count > 0 && (
                    <span className="flex items-center gap-1">
                      <Play className="w-3 h-3" />
                      {exploration.run_count} runs
                    </span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleEditExploration(exploration);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit exploration"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runExploration(exploration);
                  }}
                  disabled={runningId === exploration.id}
                  className={`p-1.5 ${getAccentColors("emerald").bg} ${getAccentColors("emerald").text} rounded hover:${getAccentColors("emerald").bgSolid}/20 transition-colors disabled:opacity-50`}
                  title="Run exploration"
                >
                  {runningId === exploration.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                </button>
              </div>
            </div>
            {exploration.tags && exploration.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {exploration.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {exploration.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">
                    +{exploration.tags.length - 3}
                  </span>
                )}
              </div>
            )}
          </div>
        );
      }

      case "api-requests": {
        const apiRequest = item as SavedApiRequest;
        return (
          <div
            key={apiRequest.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(apiRequest.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <Globe className={`w-5 h-5 ${getAccentColors("indigo").text} flex-shrink-0 mt-0.5`} />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{apiRequest.name}</span>
                  <span
                    className={`text-xs ${getAccentColors("indigo").bg} ${getAccentColors("indigo").text} px-1.5 py-0.5 rounded font-mono`}
                  >
                    {apiRequest.method}
                  </span>
                </div>
                <p className="text-xs text-muted-foreground truncate mt-0.5 font-mono">
                  {apiRequest.url}
                </p>
                <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                  <span className="flex items-center gap-1">
                    <Clock className="w-3 h-3" />
                    {formatRelativeDate(apiRequest.updated_at)}
                  </span>
                  {apiRequest.category && (
                    <span className="flex items-center gap-1">
                      <FolderOpen className="w-3 h-3" />
                      {apiRequest.category}
                    </span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleEditApiRequest(apiRequest);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit API request"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDuplicateApiRequest(apiRequest.id);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Duplicate API request"
                >
                  <Copy className="w-4 h-4" />
                </button>
              </div>
            </div>
            {apiRequest.tags && apiRequest.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {apiRequest.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {apiRequest.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">
                    +{apiRequest.tags.length - 3}
                  </span>
                )}
              </div>
            )}
          </div>
        );
      }

      case "unified-workflows": {
        const workflow = item as UnifiedWorkflow;
        const stepCount =
          workflow.setup_steps.length +
          workflow.verification_steps.length +
          workflow.agentic_steps.length;
        return (
          <div
            key={workflow.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(workflow.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <Sparkles
                className={`w-5 h-5 ${getAccentColors("green").text} flex-shrink-0 mt-0.5`}
              />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{workflow.name || "Untitled"}</span>
                  <span
                    className={`text-xs ${getAccentColors("green").bg} ${getAccentColors("green").text} px-1.5 py-0.5 rounded`}
                  >
                    {stepCount} steps
                  </span>
                </div>
                {workflow.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {workflow.description}
                  </p>
                )}
                <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                  <span className="flex items-center gap-1">
                    <Clock className="w-3 h-3" />
                    {formatRelativeDate(workflow.modified_at)}
                  </span>
                  {workflow.category && workflow.category !== "general" && (
                    <span className="flex items-center gap-1">
                      <FolderOpen className="w-3 h-3" />
                      {workflow.category}
                    </span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    // Navigate to Workflow Builder with this workflow ID
                    onLog("info", `Open Workflow Builder for workflow: ${workflow.id}`);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit workflow"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDuplicateUnifiedWorkflow(workflow.id);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Duplicate workflow"
                >
                  <Copy className="w-4 h-4" />
                </button>
              </div>
            </div>
            {workflow.tags && workflow.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {workflow.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {workflow.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">+{workflow.tags.length - 3}</span>
                )}
              </div>
            )}
          </div>
        );
      }

      default:
        return null;
    }
  };

  // Duplicate unified workflow
  const handleDuplicateUnifiedWorkflow = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/unified-workflows/${id}/duplicate`, {
        method: "POST",
      });
      const data = await response.json();
      if (data.success && data.data) {
        setUnifiedWorkflows((prev) => [data.data, ...prev]);
        onLog("success", "Workflow duplicated");
      }
    } catch (error) {
      onLog("error", `Failed to duplicate workflow: ${error}`);
    }
  };

  // Render detail panel content
  const renderDetailPanel = () => {
    if (!selectedItemId) return null;

    switch (selectedCategory) {
      case "tasks": {
        const task = prompts.find((p) => p.id === selectedItemId);
        if (!task) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{task.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {task.description && <p className="text-muted-foreground">{task.description}</p>}
            {task.tags && task.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {task.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Settings className="w-4 h-4" />
              Max sessions: {task.max_sessions ?? "unlimited"}
            </div>
            <div>
              <h4 className="text-sm font-medium mb-2 flex items-center gap-2">
                <Code className="w-4 h-4" />
                Task Content
              </h4>
              <pre className="text-sm bg-muted p-3 rounded-lg overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
                {task.content}
              </pre>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(task.created_at)}</span>
              <span>Modified: {formatDate(task.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runPrompt(task)}
                disabled={runningId === task.id}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("amber").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors disabled:opacity-50`}
              >
                {runningId === task.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Task
              </button>
              <button
                onClick={() => handleEditTask(task)}
                className="flex items-center justify-center gap-2 px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
                title="Edit task"
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => duplicateItem(task.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deletePrompt(task.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "macros": {
        const macro = macros.find((m) => m.id === selectedItemId);
        if (!macro) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{macro.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {macro.description && (
              <p className="text-muted-foreground">{macro.description}</p>
            )}
            {macro.tags && macro.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {macro.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div>
              <h4 className="text-sm font-medium mb-2 flex items-center gap-2">
                <List className="w-4 h-4" />
                Steps ({macro.steps.length})
              </h4>
              <div className="space-y-1">
                {macro.steps.map((step, index) => (
                  <div
                    key={step.id}
                    className="flex items-center gap-2 text-sm bg-muted/50 px-3 py-2 rounded"
                  >
                    <span className="text-muted-foreground font-mono">{index + 1}.</span>
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] uppercase font-medium ${
                        step.action_type === "click"
                          ? `${getAccentColors("orange").bg} ${getAccentColors("orange").text}`
                          : step.action_type === "type"
                            ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                            : step.action_type === "hotkey"
                              ? `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`
                              : `${getAccentColors("green").bg} ${getAccentColors("green").text}`
                      }`}
                    >
                      {step.action_type}
                    </span>
                    <span className="flex-1 truncate">{step.name}</span>
                  </div>
                ))}
              </div>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(macro.created_at)}</span>
              <span>Modified: {formatDate(macro.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runMacro(macro)}
                disabled={runningId === macro.id}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("orange").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors disabled:opacity-50`}
              >
                {runningId === macro.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Macro
              </button>
              <button
                onClick={() => deleteMacro(macro.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "scripts": {
        const script = scripts.find((s) => s.id === selectedItemId);
        if (!script) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{script.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {script.description && <p className="text-muted-foreground">{script.description}</p>}
            {script.target_url && (
              <div className="flex items-center gap-2">
                <Link className="w-4 h-4 text-muted-foreground" />
                <a
                  href={script.target_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-primary hover:underline break-all"
                >
                  {script.target_url}
                </a>
              </div>
            )}
            <div>
              <h4 className="text-sm font-medium mb-2 flex items-center gap-2">
                <Code className="w-4 h-4" />
                Script Content
              </h4>
              <pre className="text-xs bg-muted p-3 rounded-lg overflow-x-auto max-h-64 overflow-y-auto font-mono">
                {script.script_content}
              </pre>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(script.created_at)}</span>
              <span>Modified: {formatDate(script.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runScript(script)}
                disabled={runningId === script.id}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("purple").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors disabled:opacity-50`}
              >
                {runningId === script.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Script
              </button>
              <button
                onClick={() => onEditScript?.(script.id)}
                className="flex items-center justify-center gap-2 px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
                title="Edit in Script Builder"
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => duplicateItem(script.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteScript(script.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "scriptlets": {
        const scriptlet = scriptlets.find((s) => s.id === selectedItemId);
        if (!scriptlet) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{scriptlet.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-xs bg-muted px-2 py-1 rounded">{scriptlet.category}</span>
            </div>
            {scriptlet.tags && scriptlet.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {scriptlet.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div>
              <h4 className="text-sm font-medium mb-2 flex items-center gap-2">
                <Code className="w-4 h-4" />
                Content
              </h4>
              <pre className="text-sm bg-muted p-3 rounded-lg overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap font-mono">
                {scriptlet.content}
              </pre>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(scriptlet.created_at)}</span>
              <span>Modified: {formatDate(scriptlet.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={async () => {
                  await navigator.clipboard.writeText(scriptlet.content);
                  onLog("success", "Copied to clipboard");
                }}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("cyan").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors`}
              >
                <Copy className="w-4 h-4" />
                Copy Content
              </button>
              <button
                onClick={() => handleEditScriptlet(scriptlet)}
                className="flex items-center justify-center gap-2 px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
                title="Edit scriptlet"
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => duplicateItem(scriptlet.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteScriptlet(scriptlet.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "contexts": {
        const context = contexts.find((c) => c.id === selectedItemId);
        if (!context) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{context.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            <ContextCard
              context={context}
              onEdit={handleContextEdit}
              onDelete={handleContextDelete}
              onDuplicate={handleContextDuplicate}
              onToggleEnabled={handleContextToggleEnabled}
              isProcessing={false}
            />
          </div>
        );
      }

      case "state-explorer": {
        const exploration = explorations.find((v) => v.id === selectedItemId);
        if (!exploration) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{exploration.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {exploration.description && (
              <p className="text-muted-foreground">{exploration.description}</p>
            )}
            {exploration.tags && exploration.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {exploration.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div className="space-y-2">
              <h4 className="text-sm font-medium">Configuration</h4>
              <div className="grid grid-cols-2 gap-2 text-sm">
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Strategy:</span>
                  <span className="font-medium">{exploration.config.strategy}</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Max States:</span>
                  <span className="font-medium">
                    {exploration.config.max_states || "Unlimited"}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">State Delay:</span>
                  <span className="font-medium">{exploration.config.state_delay_ms}ms</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Screenshots:</span>
                  <span className="font-medium">
                    {exploration.config.capture_screenshots ? "Yes" : "No"}
                  </span>
                </div>
              </div>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(exploration.created_at)}</span>
              <span>Modified: {formatDate(exploration.modified_at)}</span>
              {exploration.last_run_at && (
                <span>Last Run: {formatDate(exploration.last_run_at)}</span>
              )}
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runExploration(exploration)}
                disabled={runningId === exploration.id}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("emerald").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors disabled:opacity-50`}
              >
                {runningId === exploration.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Exploration
              </button>
              <button
                onClick={() => handleEditExploration(exploration)}
                className="flex items-center justify-center gap-2 px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
                title="Edit exploration"
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => deleteExploration(exploration.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "api-requests": {
        const apiRequest = savedApiRequests.find((r) => r.id === selectedItemId);
        if (!apiRequest) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{apiRequest.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {apiRequest.description && (
              <p className="text-muted-foreground">{apiRequest.description}</p>
            )}
            {apiRequest.tags && apiRequest.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {apiRequest.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div className="space-y-2">
              <h4 className="text-sm font-medium">Request Details</h4>
              <div className="flex items-center gap-2">
                <span
                  className={`px-2 py-1 text-xs font-medium rounded ${
                    apiRequest.method === "GET"
                      ? "bg-green-500/20 text-green-400"
                      : apiRequest.method === "POST"
                        ? "bg-blue-500/20 text-blue-400"
                        : apiRequest.method === "PUT"
                          ? "bg-amber-500/20 text-amber-400"
                          : apiRequest.method === "DELETE"
                            ? "bg-red-500/20 text-red-400"
                            : "bg-purple-500/20 text-purple-400"
                  }`}
                >
                  {apiRequest.method}
                </span>
                <span className="text-sm font-mono text-muted-foreground truncate">
                  {apiRequest.url}
                </span>
              </div>
              <div className="grid grid-cols-2 gap-2 text-sm">
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Timeout:</span>
                  <span className="font-medium">{apiRequest.timeout_ms}ms</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Follow Redirects:</span>
                  <span className="font-medium">{apiRequest.follow_redirects ? "Yes" : "No"}</span>
                </div>
              </div>
            </div>
            {apiRequest.body && (
              <div>
                <h4 className="text-sm font-medium mb-2">Request Body</h4>
                <pre className="text-sm bg-muted p-3 rounded-lg overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap font-mono">
                  {apiRequest.body}
                </pre>
              </div>
            )}
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(apiRequest.created_at)}</span>
              <span>Modified: {formatDate(apiRequest.updated_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => handleEditApiRequest(apiRequest)}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("indigo").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors`}
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => handleDuplicateApiRequest(apiRequest.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteApiRequest(apiRequest.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "unified-workflows": {
        const workflow = unifiedWorkflows.find((w) => w.id === selectedItemId);
        if (!workflow) return null;
        const stepCount =
          workflow.setup_steps.length +
          workflow.verification_steps.length +
          workflow.agentic_steps.length;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{workflow.name || "Untitled"}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {workflow.description && (
              <p className="text-muted-foreground">{workflow.description}</p>
            )}
            {workflow.tags && workflow.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {workflow.tags.map((tag) => (
                  <span key={tag} className="text-xs bg-primary/10 text-primary px-2 py-1 rounded">
                    {tag}
                  </span>
                ))}
              </div>
            )}
            <div className="space-y-2">
              <h4 className="text-sm font-medium">Workflow Phases</h4>
              <div className="grid grid-cols-3 gap-2 text-sm">
                <div className="p-2 bg-blue-500/10 rounded text-center">
                  <div className="text-blue-400 font-medium">{workflow.setup_steps.length}</div>
                  <div className="text-xs text-muted-foreground">Setup</div>
                </div>
                <div className="p-2 bg-green-500/10 rounded text-center">
                  <div className="text-green-400 font-medium">
                    {workflow.verification_steps.length}
                  </div>
                  <div className="text-xs text-muted-foreground">Verification</div>
                </div>
                <div className="p-2 bg-amber-500/10 rounded text-center">
                  <div className="text-amber-400 font-medium">{workflow.agentic_steps.length}</div>
                  <div className="text-xs text-muted-foreground">Agentic</div>
                </div>
              </div>
              {workflow.max_iterations && (
                <div className="text-sm text-muted-foreground">
                  Max Iterations: {workflow.max_iterations}
                </div>
              )}
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(workflow.created_at)}</span>
              <span>Modified: {formatDate(workflow.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => {
                  // Navigate to Workflow Builder
                  onLog("info", `Open Workflow Builder for workflow: ${workflow.id}`);
                }}
                className={`flex-1 flex items-center justify-center gap-2 py-2 ${getAccentColors("green").bgSolid} text-white rounded-lg hover:opacity-90 transition-colors`}
              >
                <Pencil className="w-4 h-4" />
                Edit
              </button>
              <button
                onClick={() => handleDuplicateUnifiedWorkflow(workflow.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteUnifiedWorkflow(workflow.id)}
                className={`p-2 border border-border rounded-lg hover:${getStatusColors("error").bg} hover:${getStatusColors("error").text} transition-colors`}
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      default:
        return null;
    }
  };

  return (
    <div ref={containerRef} className="h-full flex overflow-hidden">
      {/* Left Sidebar - Category Navigation */}
      <div className="w-48 border-r border-border flex-shrink-0 flex flex-col">
        <div className="p-4">
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <FolderOpen className="w-5 h-5" />
            Library
          </h2>
        </div>
        <nav className="flex-1 px-2">
          {CATEGORIES.map((category) => {
            const Icon = category.icon;
            const count =
              category.id === "tasks"
                ? prompts.length
                : category.id === "unified-workflows"
                  ? unifiedWorkflows.length
                  : category.id === "macros"
                    ? macros.length
                    : category.id === "scripts"
                      ? scripts.length
                      : category.id === "scriptlets"
                        ? scriptlets.length
                        : category.id === "contexts"
                          ? contexts.length
                          : category.id === "state-explorer"
                            ? explorations.length
                            : category.id === "api-requests"
                              ? savedApiRequests.length
                              : 0;

            return (
              <button
                key={category.id}
                onClick={() => {
                  setSelectedCategory(category.id);
                  setSelectedItemId(null);
                  setShowDetailPanel(false);
                  setSearchQuery("");
                  setTagFilter(null);
                }}
                className={`w-full flex items-center gap-3 px-3 py-2 rounded-lg mb-1 transition-colors ${
                  selectedCategory === category.id
                    ? "bg-primary/10 text-primary"
                    : "hover:bg-muted text-foreground"
                }`}
              >
                <Icon className={`w-4 h-4 ${getAccentColors(category.accentColor).text}`} />
                <span className="flex-1 text-left text-sm">{category.label}</span>
                <span className="text-xs text-muted-foreground">{count}</span>
              </button>
            );
          })}
        </nav>
        <div className="p-2 border-t border-border">
          <button
            onClick={handleCreateNew}
            className="w-full flex items-center justify-center gap-2 px-3 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors text-sm"
          >
            <Plus className="w-4 h-4" />
            New {categoryConfig.label.slice(0, -1)}
          </button>
        </div>
      </div>

      {/* Main Content Area */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Header with Search and Filters */}
        <div className="p-4 border-b border-border flex items-center gap-3">
          <div className="flex items-center gap-2">
            <CategoryIcon
              className={`w-5 h-5 ${getAccentColors(categoryConfig.accentColor).text}`}
            />
            <h3 className="font-semibold">{categoryConfig.label}</h3>
            <span className="text-sm text-muted-foreground">({currentItems.length})</span>
          </div>

          <div className="flex-1 relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder={`Search ${categoryConfig.label.toLowerCase()}...`}
              className="w-full pl-9 pr-4 py-2 bg-background border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {allTags.length > 0 && (
            <div className="relative">
              <button
                onClick={() => setShowTagDropdown(!showTagDropdown)}
                className={`flex items-center gap-2 px-3 py-2 border rounded-lg text-sm transition-colors ${
                  tagFilter
                    ? "border-primary bg-primary/10 text-primary"
                    : "border-border hover:bg-muted"
                }`}
              >
                <Filter className="w-4 h-4" />
                {tagFilter || "Filter by tag"}
                {tagFilter && (
                  <X
                    className="w-3 h-3 ml-1"
                    onClick={(e) => {
                      e.stopPropagation();
                      setTagFilter(null);
                    }}
                  />
                )}
              </button>
              {showTagDropdown && (
                <>
                  <div className="fixed inset-0 z-10" onClick={() => setShowTagDropdown(false)} />
                  <div className="absolute right-0 top-full mt-1 z-20 w-48 bg-card border border-border rounded-lg shadow-lg py-1 max-h-64 overflow-y-auto">
                    {allTags.map((tag) => (
                      <button
                        key={tag}
                        onClick={() => {
                          setTagFilter(tag);
                          setShowTagDropdown(false);
                        }}
                        className={`w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-muted transition-colors ${
                          tagFilter === tag ? "bg-primary/10 text-primary" : ""
                        }`}
                      >
                        <Tag className="w-3 h-3" />
                        {tag}
                      </button>
                    ))}
                  </div>
                </>
              )}
            </div>
          )}

          <div className="flex items-center gap-1 border border-border rounded-lg p-1">
            <button
              onClick={() => setViewMode("list")}
              className={`p-1.5 rounded transition-colors ${
                viewMode === "list" ? "bg-primary text-primary-foreground" : "hover:bg-muted"
              }`}
              title="List view"
            >
              <LayoutList className="w-4 h-4" />
            </button>
            <button
              onClick={() => setViewMode("grid")}
              className={`p-1.5 rounded transition-colors ${
                viewMode === "grid" ? "bg-primary text-primary-foreground" : "hover:bg-muted"
              }`}
              title="Grid view"
            >
              <Grid3X3 className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Item List/Grid */}
        <div className="flex-1 overflow-y-auto p-4">
          {currentItems.length === 0 ? (
            <div className="text-center py-12 text-muted-foreground">
              <FolderOpen className="w-12 h-12 mx-auto mb-4 opacity-50" />
              <p className="text-lg mb-2">No {categoryConfig.label.toLowerCase()} found</p>
              <p className="text-sm mb-4">
                {searchQuery || tagFilter
                  ? "Try adjusting your search or filters"
                  : `Create your first ${categoryConfig.label.slice(0, -1).toLowerCase()} to get started`}
              </p>
              {!searchQuery && !tagFilter && (
                <button
                  onClick={handleCreateNew}
                  className="inline-flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
                >
                  <Plus className="w-4 h-4" />
                  New {categoryConfig.label.slice(0, -1)}
                </button>
              )}
            </div>
          ) : selectedCategory === "contexts" ? (
            // Special rendering for contexts using ContextCard
            <div className="space-y-2">
              {filteredContexts.map((context) => (
                <div
                  key={context.id}
                  onClick={() => {
                    setSelectedItemId(context.id);
                    setShowDetailPanel(true);
                  }}
                  className={`cursor-pointer ${
                    selectedItemId === context.id ? "ring-2 ring-primary" : ""
                  }`}
                >
                  <ContextCard
                    context={context}
                    onEdit={handleContextEdit}
                    onDelete={handleContextDelete}
                    onDuplicate={handleContextDuplicate}
                    onToggleEnabled={handleContextToggleEnabled}
                    isProcessing={false}
                  />
                </div>
              ))}
            </div>
          ) : (
            <div
              className={
                viewMode === "grid" ? "grid grid-cols-2 lg:grid-cols-3 gap-3" : "space-y-2"
              }
            >
              {currentItems.map((item: unknown) =>
                renderItemCard(item, (item as { id: string }).id === selectedItemId),
              )}
            </div>
          )}
        </div>
      </div>

      {/* Right Detail Panel */}
      {showDetailPanel && selectedItemId && selectedCategory !== "contexts" && (
        <div className="w-80 border-l border-border flex-shrink-0 overflow-y-auto p-4 bg-card">
          {renderDetailPanel()}
        </div>
      )}

      {/* Context Editor Dialog */}
      <ContextEditor
        isOpen={contextEditorState.isOpen}
        onClose={() => setContextEditorState({ isOpen: false, context: null, mode: "create" })}
        context={contextEditorState.context}
        mode={contextEditorState.mode}
        createScope={contextEditorState.createScope}
        categories={contextCategories}
        tags={contextTags}
        onSave={handleContextSave}
        isSaving={isSavingContext}
      />

      {/* Task Creation/Edit Dialog */}
      {showTaskDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => {
              setShowTaskDialog(false);
              setEditingTaskId(null);
            }}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <FileText className={`w-5 h-5 ${getAccentColors("amber").text}`} />
                <h3 className="text-lg font-semibold">
                  {editingTaskId ? "Edit Task" : "New Task"}
                </h3>
              </div>
              <button
                onClick={() => {
                  setShowTaskDialog(false);
                  setEditingTaskId(null);
                }}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              {editingTaskId
                ? "Edit the task details below."
                : "Create a new single-step AI task. Tasks can be run directly from the Library."}
            </p>

            <div className="space-y-3">
              <div>
                <label className="block text-sm font-medium mb-1">Name *</label>
                <input
                  type="text"
                  value={taskForm.name}
                  onChange={(e) => setTaskForm({ ...taskForm, name: e.target.value })}
                  placeholder="Enter task name..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  autoFocus
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Description</label>
                <input
                  type="text"
                  value={taskForm.description}
                  onChange={(e) => setTaskForm({ ...taskForm, description: e.target.value })}
                  placeholder="Brief description (optional)..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Task Content *</label>
                <textarea
                  value={taskForm.content}
                  onChange={(e) => setTaskForm({ ...taskForm, content: e.target.value })}
                  placeholder="Enter the task prompt or instructions..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-32 focus:outline-none focus:ring-2 focus:ring-primary font-mono"
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-sm font-medium mb-1">Category</label>
                  <input
                    type="text"
                    value={taskForm.category}
                    onChange={(e) => setTaskForm({ ...taskForm, category: e.target.value })}
                    placeholder="general"
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">Max Sessions</label>
                  <input
                    type="number"
                    value={taskForm.max_sessions || ""}
                    onChange={(e) =>
                      setTaskForm({
                        ...taskForm,
                        max_sessions: e.target.value ? parseInt(e.target.value) : null,
                      })
                    }
                    placeholder="Unlimited"
                    min="1"
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
              </div>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => {
                  setShowTaskDialog(false);
                  setEditingTaskId(null);
                }}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveTask}
                disabled={isSavingTask || !taskForm.name.trim() || !taskForm.content.trim()}
                className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("amber").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
              >
                {isSavingTask ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {editingTaskId ? "Saving..." : "Creating..."}
                  </>
                ) : (
                  <>
                    {editingTaskId ? <Pencil className="w-4 h-4" /> : <Plus className="w-4 h-4" />}
                    {editingTaskId ? "Save Changes" : "Create Task"}
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Scriptlet Creation/Edit Dialog */}
      {showScriptletDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => {
              setShowScriptletDialog(false);
              setEditingScriptletId(null);
            }}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Puzzle className={`w-5 h-5 ${getAccentColors("cyan").text}`} />
                <h3 className="text-lg font-semibold">
                  {editingScriptletId ? "Edit Scriptlet" : "New Scriptlet"}
                </h3>
              </div>
              <button
                onClick={() => {
                  setShowScriptletDialog(false);
                  setEditingScriptletId(null);
                }}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              {editingScriptletId
                ? "Edit the scriptlet details below."
                : "Create a reusable snippet that can be referenced in other tasks and workflows."}
            </p>

            <div className="space-y-3">
              <div>
                <label className="block text-sm font-medium mb-1">Name *</label>
                <input
                  type="text"
                  value={scriptletForm.name}
                  onChange={(e) => setScriptletForm({ ...scriptletForm, name: e.target.value })}
                  placeholder="Enter scriptlet name..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  autoFocus
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Category</label>
                <input
                  type="text"
                  value={scriptletForm.category}
                  onChange={(e) => setScriptletForm({ ...scriptletForm, category: e.target.value })}
                  placeholder="general"
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Content *</label>
                <textarea
                  value={scriptletForm.content}
                  onChange={(e) => setScriptletForm({ ...scriptletForm, content: e.target.value })}
                  placeholder="Enter the scriptlet content..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-40 focus:outline-none focus:ring-2 focus:ring-primary font-mono"
                />
              </div>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => {
                  setShowScriptletDialog(false);
                  setEditingScriptletId(null);
                }}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveScriptlet}
                disabled={
                  isSavingScriptlet || !scriptletForm.name.trim() || !scriptletForm.content.trim()
                }
                className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("cyan").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
              >
                {isSavingScriptlet ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {editingScriptletId ? "Saving..." : "Creating..."}
                  </>
                ) : (
                  <>
                    {editingScriptletId ? (
                      <Pencil className="w-4 h-4" />
                    ) : (
                      <Plus className="w-4 h-4" />
                    )}
                    {editingScriptletId ? "Save Changes" : "Create Scriptlet"}
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Exploration Creation/Edit Dialog */}
      {showExplorationDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => {
              setShowExplorationDialog(false);
              setEditingExplorationId(null);
            }}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4 max-h-[90vh] overflow-y-auto">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <ShieldCheck className={`w-5 h-5 ${getAccentColors("emerald").text}`} />
                <h3 className="text-lg font-semibold">
                  {editingExplorationId ? "Edit Exploration" : "New Exploration"}
                </h3>
              </div>
              <button
                onClick={() => {
                  setShowExplorationDialog(false);
                  setEditingExplorationId(null);
                }}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              {editingExplorationId
                ? "Edit the exploration configuration below."
                : "Create a reusable exploration configuration that can be run against any loaded config."}
            </p>

            <div className="space-y-3">
              <div>
                <label className="block text-sm font-medium mb-1">Name *</label>
                <input
                  type="text"
                  value={explorationForm.name}
                  onChange={(e) => setExplorationForm({ ...explorationForm, name: e.target.value })}
                  placeholder="Enter exploration name..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  autoFocus
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Description</label>
                <textarea
                  value={explorationForm.description}
                  onChange={(e) =>
                    setExplorationForm({ ...explorationForm, description: e.target.value })
                  }
                  placeholder="Optional description..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-16 focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Strategy</label>
                <select
                  value={explorationForm.strategy}
                  onChange={(e) =>
                    setExplorationForm({
                      ...explorationForm,
                      strategy: e.target.value as ExplorationConfig["strategy"],
                    })
                  }
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                >
                  <option value="smoke_test">Smoke Test - Quick pass through key states</option>
                  <option value="exhaustive">Exhaustive - Visit all states and transitions</option>
                  <option value="regression">Regression - Focus on previously failed areas</option>
                  <option value="random_walk">Random Walk - Explore randomly</option>
                  <option value="targeted">Targeted - Specific states only</option>
                </select>
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-sm font-medium mb-1">Max States (0=unlimited)</label>
                  <input
                    type="number"
                    min="0"
                    value={explorationForm.max_states}
                    onChange={(e) =>
                      setExplorationForm({
                        ...explorationForm,
                        max_states: parseInt(e.target.value) || 0,
                      })
                    }
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">State Delay (ms)</label>
                  <input
                    type="number"
                    min="0"
                    value={explorationForm.state_delay_ms}
                    onChange={(e) =>
                      setExplorationForm({
                        ...explorationForm,
                        state_delay_ms: parseInt(e.target.value) || 500,
                      })
                    }
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
              </div>

              <div className="flex items-center gap-4">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={explorationForm.capture_screenshots}
                    onChange={(e) =>
                      setExplorationForm({
                        ...explorationForm,
                        capture_screenshots: e.target.checked,
                      })
                    }
                    className="rounded border-border"
                  />
                  <span className="text-sm">Capture Screenshots</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={explorationForm.stop_on_first_failure}
                    onChange={(e) =>
                      setExplorationForm({
                        ...explorationForm,
                        stop_on_first_failure: e.target.checked,
                      })
                    }
                    className="rounded border-border"
                  />
                  <span className="text-sm">Stop on First Failure</span>
                </label>
              </div>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => {
                  setShowExplorationDialog(false);
                  setEditingExplorationId(null);
                }}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveExploration}
                disabled={isSavingExploration || !explorationForm.name.trim()}
                className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("emerald").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
              >
                {isSavingExploration ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {editingExplorationId ? "Saving..." : "Creating..."}
                  </>
                ) : (
                  <>
                    {editingExplorationId ? (
                      <Pencil className="w-4 h-4" />
                    ) : (
                      <Plus className="w-4 h-4" />
                    )}
                    {editingExplorationId ? "Save Changes" : "Create Exploration"}
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* API Request Creation/Edit Dialog */}
      {showApiRequestDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => {
              setShowApiRequestDialog(false);
              setEditingApiRequestId(null);
            }}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 p-6 space-y-4 max-h-[90vh] overflow-y-auto">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Globe className={`w-5 h-5 ${getAccentColors("indigo").text}`} />
                <h3 className="text-lg font-semibold">
                  {editingApiRequestId ? "Edit API Request" : "New API Request"}
                </h3>
              </div>
              <button
                onClick={() => {
                  setShowApiRequestDialog(false);
                  setEditingApiRequestId(null);
                }}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              {editingApiRequestId
                ? "Edit the saved API request template."
                : "Create a reusable API request template that can be used in workflows."}
            </p>

            <div className="space-y-3">
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-sm font-medium mb-1">Name *</label>
                  <input
                    type="text"
                    value={apiRequestForm.name}
                    onChange={(e) => setApiRequestForm({ ...apiRequestForm, name: e.target.value })}
                    placeholder="Enter request name..."
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                    autoFocus
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">Category</label>
                  <input
                    type="text"
                    value={apiRequestForm.category}
                    onChange={(e) =>
                      setApiRequestForm({ ...apiRequestForm, category: e.target.value })
                    }
                    placeholder="general"
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Description</label>
                <textarea
                  value={apiRequestForm.description}
                  onChange={(e) =>
                    setApiRequestForm({ ...apiRequestForm, description: e.target.value })
                  }
                  placeholder="Optional description..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-16 focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div className="grid grid-cols-[120px_1fr] gap-3">
                <div>
                  <label className="block text-sm font-medium mb-1">Method *</label>
                  <select
                    value={apiRequestForm.method}
                    onChange={(e) =>
                      setApiRequestForm({
                        ...apiRequestForm,
                        method: e.target.value as typeof apiRequestForm.method,
                      })
                    }
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  >
                    <option value="GET">GET</option>
                    <option value="POST">POST</option>
                    <option value="PUT">PUT</option>
                    <option value="PATCH">PATCH</option>
                    <option value="DELETE">DELETE</option>
                    <option value="HEAD">HEAD</option>
                    <option value="OPTIONS">OPTIONS</option>
                  </select>
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">URL *</label>
                  <input
                    type="text"
                    value={apiRequestForm.url}
                    onChange={(e) => setApiRequestForm({ ...apiRequestForm, url: e.target.value })}
                    placeholder="https://api.example.com/endpoint"
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm font-mono focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Request Body</label>
                <textarea
                  value={apiRequestForm.body}
                  onChange={(e) => setApiRequestForm({ ...apiRequestForm, body: e.target.value })}
                  placeholder='{"key": "value"}'
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm font-mono resize-none h-24 focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-sm font-medium mb-1">Timeout (ms)</label>
                  <input
                    type="number"
                    min="1000"
                    value={apiRequestForm.timeout_ms}
                    onChange={(e) =>
                      setApiRequestForm({
                        ...apiRequestForm,
                        timeout_ms: parseInt(e.target.value) || 30000,
                      })
                    }
                    className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                </div>
                <div className="flex items-end">
                  <label className="flex items-center gap-2 cursor-pointer pb-2">
                    <input
                      type="checkbox"
                      checked={apiRequestForm.follow_redirects}
                      onChange={(e) =>
                        setApiRequestForm({
                          ...apiRequestForm,
                          follow_redirects: e.target.checked,
                        })
                      }
                      className="rounded border-border"
                    />
                    <span className="text-sm">Follow Redirects</span>
                  </label>
                </div>
              </div>
            </div>

            <div className="flex gap-3 pt-2">
              <button
                onClick={() => {
                  setShowApiRequestDialog(false);
                  setEditingApiRequestId(null);
                }}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveApiRequest}
                disabled={
                  isSavingApiRequest || !apiRequestForm.name.trim() || !apiRequestForm.url.trim()
                }
                className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("indigo").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
              >
                {isSavingApiRequest ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {editingApiRequestId ? "Saving..." : "Creating..."}
                  </>
                ) : (
                  <>
                    {editingApiRequestId ? (
                      <Pencil className="w-4 h-4" />
                    ) : (
                      <Plus className="w-4 h-4" />
                    )}
                    {editingApiRequestId ? "Save Changes" : "Create API Request"}
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Keyboard Shortcuts Hint */}
      <div className="absolute bottom-4 right-4 text-xs text-muted-foreground bg-card border border-border rounded-lg px-3 py-2 shadow-lg opacity-60 hover:opacity-100 transition-opacity">
        <span className="font-medium">Shortcuts:</span> <span className="text-primary">Ctrl+N</span>{" "}
        New | <span className="text-primary">Enter</span> Run |{" "}
        <span className="text-primary">Del</span> Delete | <span className="text-primary">Esc</span>{" "}
        Close
      </div>
    </div>
  );
}

export default LibraryTab;
