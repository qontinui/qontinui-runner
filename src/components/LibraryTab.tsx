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
} from "lucide-react";
import { useContexts } from "./contexts/hooks/useContexts";
import { ContextCard } from "./contexts/ContextCard";
import { ContextEditor } from "./contexts/ContextEditor";
import type {
  ContextWithMetadata,
  CreateContextRequest,
  UpdateContextRequest,
} from "../types/context";
import type { Scriptlet, SavedVerification, VerificationTaskConfig } from "../types";
import type { AiOutputLine } from "./AiOutputTab";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

// Category definitions
type LibraryCategory =
  | "tasks"
  | "workflows"
  | "scripts"
  | "scriptlets"
  | "contexts"
  | "verifications";

interface CategoryConfig {
  id: LibraryCategory;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  color: string;
  description: string;
}

const CATEGORIES: CategoryConfig[] = [
  {
    id: "tasks",
    label: "Tasks",
    icon: FileText,
    color: "text-amber-500",
    description: "Single-step AI tasks",
  },
  {
    id: "workflows",
    label: "Workflows",
    icon: Sparkles,
    color: "text-green-500",
    description: "Multi-step AI workflows",
  },
  {
    id: "scripts",
    label: "Scripts",
    icon: TestTube,
    color: "text-purple-500",
    description: "Playwright scripts",
  },
  {
    id: "scriptlets",
    label: "Scriptlets",
    icon: Puzzle,
    color: "text-cyan-500",
    description: "Reusable snippets",
  },
  {
    id: "contexts",
    label: "Contexts",
    icon: BookOpen,
    color: "text-blue-500",
    description: "AI knowledge base",
  },
  {
    id: "verifications",
    label: "Verifications",
    icon: ShieldCheck,
    color: "text-emerald-500",
    description: "State verification configs",
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

// AI Workflow types
interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt" | "action" | "screenshot";
  name: string;
  takeScreenshot: boolean;
  screenshotDelay?: number;
  screenshotMonitor?: number | "all" | null;
  playwrightScriptId?: string;
  playwrightScriptContent?: string;
  playwrightTargetUrl?: string;
  promptId?: string;
  promptContent?: string;
  actionType?: "click" | "double_click" | "right_click";
  targetImageId?: string;
  targetImageName?: string;
}

interface SavedAiWorkflow {
  id: string;
  name: string;
  description: string;
  steps: ExecutionStep[];
  goal: string;
  max_iterations: number;
  persistent_session: boolean;
  capture_input_validation: boolean;
  category: string;
  tags: string[];
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
  onEditScript?: (scriptId: string) => void;
  onEditWorkflow?: (workflowId: string) => void;
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
  onEditScript,
  onEditWorkflow,
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
  const [workflows, setWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [scriptlets, setScriptlets] = useState<Scriptlet[]>([]);
  const [verifications, setVerifications] = useState<SavedVerification[]>([]);

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

  // Task creation dialog state
  const [showTaskDialog, setShowTaskDialog] = useState(false);
  const [taskForm, setTaskForm] = useState({
    name: "",
    description: "",
    content: "",
    max_sessions: null as number | null,
    category: "general",
    tags: [] as string[],
  });
  const [isSavingTask, setIsSavingTask] = useState(false);

  // Scriptlet creation dialog state
  const [showScriptletDialog, setShowScriptletDialog] = useState(false);
  const [scriptletForm, setScriptletForm] = useState({
    name: "",
    content: "",
    category: "general",
    tags: [] as string[],
  });
  const [isSavingScriptlet, setIsSavingScriptlet] = useState(false);

  // Verification creation dialog state
  const [showVerificationDialog, setShowVerificationDialog] = useState(false);
  const [verificationForm, setVerificationForm] = useState<{
    name: string;
    description: string;
    strategy: VerificationTaskConfig["strategy"];
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
  const [isSavingVerification, setIsSavingVerification] = useState(false);

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
        const [promptsRes, workflowsRes, scriptsRes, scriptletsRes, verificationsRes] =
          await Promise.all([
            fetch(`${API_BASE}/prompts`),
            fetch(`${API_BASE}/ai-workflows`),
            fetch(`${API_BASE}/playwright/scripts`),
            fetch(`${API_BASE}/scriptlets`),
            fetch(`${API_BASE}/verifications`),
          ]);

        const [promptsData, workflowsData, scriptsData, scriptletsData, verificationsData] =
          await Promise.all([
            promptsRes.json(),
            workflowsRes.json(),
            scriptsRes.json(),
            scriptletsRes.json(),
            verificationsRes.json().catch(() => ({ success: false })),
          ]);

        if (promptsData.success) setPrompts(promptsData.data || []);
        if (workflowsData.success) setWorkflows(workflowsData.data || []);
        if (scriptsData.success) setScripts(scriptsData.data || []);
        if (scriptletsData.success) setScriptlets(scriptletsData.data || []);
        if (verificationsData.success) setVerifications(verificationsData.data || []);
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
      case "workflows":
        // Navigate to Workflow Builder tab
        if (onNavigateToBuilder) {
          onNavigateToBuilder();
        } else {
          onLog("info", "Workflow Builder not available");
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
      case "verifications":
        setVerificationForm({
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
        setShowVerificationDialog(true);
        break;
    }
  };

  // Save new task
  const handleSaveTask = async () => {
    if (!taskForm.name.trim() || !taskForm.content.trim()) {
      onLog("warning", "Task name and content are required");
      return;
    }

    setIsSavingTask(true);
    try {
      const response = await fetch(`${API_BASE}/prompts`, {
        method: "POST",
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
        setPrompts((prev) => [...prev, result.data]);
        setShowTaskDialog(false);
        onLog("success", `Created task: ${taskForm.name}`);
      } else {
        throw new Error(result.error || "Failed to create task");
      }
    } catch (error) {
      onLog("error", `Failed to create task: ${error}`);
    } finally {
      setIsSavingTask(false);
    }
  };

  // Save new scriptlet
  const handleSaveScriptlet = async () => {
    if (!scriptletForm.name.trim() || !scriptletForm.content.trim()) {
      onLog("warning", "Scriptlet name and content are required");
      return;
    }

    setIsSavingScriptlet(true);
    try {
      const response = await fetch(`${API_BASE}/scriptlets`, {
        method: "POST",
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
        setScriptlets((prev) => [...prev, result.data]);
        setShowScriptletDialog(false);
        onLog("success", `Created scriptlet: ${scriptletForm.name}`);
      } else {
        throw new Error(result.error || "Failed to create scriptlet");
      }
    } catch (error) {
      onLog("error", `Failed to create scriptlet: ${error}`);
    } finally {
      setIsSavingScriptlet(false);
    }
  };

  // Save new verification
  const handleSaveVerification = async () => {
    if (!verificationForm.name.trim()) {
      onLog("warning", "Verification name is required");
      return;
    }

    setIsSavingVerification(true);
    try {
      const response = await fetch(`${API_BASE}/verifications`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: verificationForm.name.trim(),
          description: verificationForm.description.trim(),
          config: {
            strategy: verificationForm.strategy,
            max_states: verificationForm.max_states,
            max_duration_seconds: verificationForm.max_duration_seconds,
            target_state_ids: [],
            target_transition_ids: [],
            capture_screenshots: verificationForm.capture_screenshots,
            capture_transition_screenshots: false,
            state_delay_ms: verificationForm.state_delay_ms,
            stop_on_first_failure: verificationForm.stop_on_first_failure,
          },
          tags: verificationForm.tags,
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        setVerifications((prev) => [...prev, result.data]);
        setShowVerificationDialog(false);
        onLog("success", `Created verification: ${verificationForm.name}`);
      } else {
        throw new Error(result.error || "Failed to create verification");
      }
    } catch (error) {
      onLog("error", `Failed to create verification: ${error}`);
    } finally {
      setIsSavingVerification(false);
    }
  };

  // Delete verification
  const deleteVerification = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/verifications/${id}`, { method: "DELETE" });
      const result = await response.json();
      if (result.success) {
        setVerifications((prev) => prev.filter((v) => v.id !== id));
        onLog("success", "Verification deleted");
      } else {
        throw new Error(result.error || "Failed to delete verification");
      }
    } catch (error) {
      onLog("error", `Failed to delete verification: ${error}`);
    }
  };

  // Delete selected item
  const handleDeleteSelected = async () => {
    if (!selectedItemId) return;

    switch (selectedCategory) {
      case "tasks":
        await deletePrompt(selectedItemId);
        break;
      case "workflows":
        await deleteWorkflow(selectedItemId);
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
      case "verifications":
        await deleteVerification(selectedItemId);
        break;
    }
    setSelectedItemId(null);
    setShowDetailPanel(false);
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
      case "workflows": {
        const workflow = workflows.find((w) => w.id === selectedItemId);
        if (workflow) runWorkflow(workflow);
        break;
      }
      case "scripts": {
        const script = scripts.find((s) => s.id === selectedItemId);
        if (script) runScript(script);
        break;
      }
      case "verifications": {
        const verification = verifications.find((v) => v.id === selectedItemId);
        if (verification) runVerification(verification);
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

  const runWorkflow = useCallback(
    async (workflow: SavedAiWorkflow) => {
      setRunningId(workflow.id);
      try {
        const prompt = generateWorkflowPrompt(workflow);

        // Determine if this session uses GUI (has any execution steps that need GUI)
        const guiStepTypes = ["workflow", "state", "action", "screenshot"];
        const usesGui = workflow.steps.some((step) => guiStepTypes.includes(step.type));

        // Convert ExecutionStep[] to ExecutionStepConfig[] for deterministic execution
        const executionStepsConfig = workflow.steps.map((step) => ({
          type: step.type,
          name: step.name,
          actionType: step.actionType || null,
          targetImageId: step.targetImageId || null,
          targetImageName: step.targetImageName || null,
          monitorIndex: null,
          takeScreenshot: step.takeScreenshot,
          screenshotDelay: step.screenshotDelay || 0,
          screenshotMonitor: step.screenshotMonitor ?? null,
          playwrightScriptId: step.playwrightScriptId || null,
          promptContent: step.promptContent || null,
        }));

        const response = await fetch(`${API_BASE}/sessions/start`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            name: workflow.name,
            prompt: prompt,
            total_phases: workflow.max_iterations,
            uses_gui: usesGui,
            timeout_seconds: 1800,
            execution_steps: executionStepsConfig,
          }),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started workflow: ${workflow.name}`);
          onNavigateToActive();
        } else {
          throw new Error(result.error || "Failed to run workflow");
        }
      } catch (error) {
        onLog("error", `Failed to run workflow: ${error}`);
      } finally {
        setRunningId(null);
      }
    },
    [onLog, onNavigateToActive],
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

  const runVerification = useCallback(
    async (verification: SavedVerification) => {
      setRunningId(verification.id);
      try {
        const response = await fetch(`${API_BASE}/verifications/${verification.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        });

        const result = await response.json();
        if (result.success) {
          onLog("success", `Started verification: ${verification.name}`);
          // Update last_run_at and run_count
          setVerifications((prev) =>
            prev.map((v) =>
              v.id === verification.id
                ? { ...v, last_run_at: new Date().toISOString(), run_count: v.run_count + 1 }
                : v,
            ),
          );
        } else {
          throw new Error(result.error || "Failed to run verification");
        }
      } catch (error) {
        onLog("error", `Failed to run verification: ${error}`);
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

  const deleteWorkflow = useCallback(
    async (id: string) => {
      if (!confirm("Delete this workflow?")) return;
      try {
        const response = await fetch(`${API_BASE}/ai-workflows/${id}`, { method: "DELETE" });
        const result = await response.json();
        if (result.success) {
          setWorkflows((prev) => prev.filter((w) => w.id !== id));
          onLog("info", "Workflow deleted");
        }
      } catch (error) {
        onLog("error", `Failed to delete workflow: ${error}`);
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
        case "workflows":
          endpoint = `${API_BASE}/ai-workflows/${id}/duplicate`;
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
            case "workflows":
              setWorkflows((prev) => [...prev, result.data]);
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

  // Generate prompt from workflow
  const generateWorkflowPrompt = (workflow: SavedAiWorkflow): string => {
    const lines: string[] = [];
    lines.push(`# AI Automation Task: ${workflow.name}`);
    lines.push("");
    lines.push(`## Goal`);
    lines.push(workflow.goal || "(No goal specified)");
    lines.push("");
    lines.push(`## Execution Steps`);
    workflow.steps.forEach((step, index) => {
      lines.push(
        `${index + 1}. [${step.type}] ${step.name}${step.takeScreenshot ? " (screenshot)" : ""}`,
      );
    });
    lines.push("");
    lines.push(`## Settings`);
    lines.push(`- Max Iterations: ${workflow.max_iterations}`);
    lines.push(`- Mode: ${workflow.persistent_session ? "Persistent Session" : "Standard"}`);
    return lines.join("\n");
  };

  // Get all unique tags for current category
  const getAllTags = (): string[] => {
    const tags = new Set<string>();
    switch (selectedCategory) {
      case "tasks":
        prompts.forEach((p) => p.tags?.forEach((t) => tags.add(t)));
        break;
      case "workflows":
        workflows.forEach((w) => w.tags?.forEach((t) => tags.add(t)));
        break;
      case "scriptlets":
        scriptlets.forEach((s) => s.tags?.forEach((t) => tags.add(t)));
        break;
      case "contexts":
        contexts.forEach((c) => c.tags?.forEach((t) => tags.add(t)));
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
  const filteredWorkflows = filterItems(workflows, (w) => [w.name, w.description, w.goal]);
  const filteredScripts = filterItems(
    scripts.map((s) => ({ ...s, tags: [] })),
    (s) => [s.name, s.description, s.target_url],
  );
  const filteredScriptlets = filterItems(scriptlets, (s) => [s.name, s.content, s.category]);
  const filteredVerifications = filterItems(verifications, (v) => [
    v.name,
    v.description || "",
    v.config.strategy,
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
      case "workflows":
        return filteredWorkflows;
      case "scripts":
        return filteredScripts;
      case "scriptlets":
        return filteredScriptlets;
      case "contexts":
        return filteredContexts;
      case "verifications":
        return filteredVerifications;
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
              <FileText className="w-5 h-5 text-amber-500 flex-shrink-0 mt-0.5" />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{task.name}</span>
                  {task.max_sessions && (
                    <span className="text-xs bg-blue-500/20 text-blue-600 px-1.5 py-0.5 rounded">
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
                    runPrompt(task);
                  }}
                  disabled={runningId === task.id}
                  className="p-1.5 bg-amber-500/10 text-amber-600 rounded hover:bg-amber-500/20 transition-colors disabled:opacity-50"
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

      case "workflows": {
        const workflow = item as SavedAiWorkflow;
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
              <Sparkles className="w-5 h-5 text-green-500 flex-shrink-0 mt-0.5" />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{workflow.name}</span>
                  <span className="text-xs bg-muted px-1.5 py-0.5 rounded">
                    {workflow.steps.length} steps
                  </span>
                </div>
                {workflow.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {workflow.description}
                  </p>
                )}
                <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {formatRelativeDate(workflow.modified_at)}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onEditWorkflow?.(workflow.id);
                  }}
                  className="p-1.5 bg-muted text-muted-foreground rounded hover:bg-muted/80 hover:text-foreground transition-colors"
                  title="Edit workflow"
                >
                  <Pencil className="w-4 h-4" />
                </button>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runWorkflow(workflow);
                  }}
                  disabled={runningId === workflow.id}
                  className="p-1.5 bg-green-500/10 text-green-600 rounded hover:bg-green-500/20 transition-colors disabled:opacity-50"
                  title="Run workflow"
                >
                  {runningId === workflow.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
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
              <TestTube className="w-5 h-5 text-purple-500 flex-shrink-0 mt-0.5" />
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
                  className="p-1.5 bg-purple-500/10 text-purple-600 rounded hover:bg-purple-500/20 transition-colors disabled:opacity-50"
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
              <Puzzle className="w-5 h-5 text-cyan-500 flex-shrink-0 mt-0.5" />
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

      case "verifications": {
        const verification = item as SavedVerification;
        return (
          <div
            key={verification.id}
            className={baseClasses}
            onClick={() => {
              setSelectedItemId(verification.id);
              setShowDetailPanel(true);
            }}
          >
            <div className="flex items-start gap-3">
              <ShieldCheck className="w-5 h-5 text-emerald-500 flex-shrink-0 mt-0.5" />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-medium truncate">{verification.name}</span>
                  <span className="text-xs bg-emerald-500/20 text-emerald-600 px-1.5 py-0.5 rounded">
                    {verification.config.strategy}
                  </span>
                </div>
                {verification.description && (
                  <p className="text-xs text-muted-foreground truncate mt-0.5">
                    {verification.description}
                  </p>
                )}
                <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                  <span className="flex items-center gap-1">
                    <Clock className="w-3 h-3" />
                    {formatRelativeDate(verification.modified_at)}
                  </span>
                  {verification.run_count > 0 && (
                    <span className="flex items-center gap-1">
                      <Play className="w-3 h-3" />
                      {verification.run_count} runs
                    </span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    runVerification(verification);
                  }}
                  disabled={runningId === verification.id}
                  className="p-1.5 bg-emerald-500/10 text-emerald-600 rounded hover:bg-emerald-500/20 transition-colors disabled:opacity-50"
                >
                  {runningId === verification.id ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Play className="w-4 h-4" />
                  )}
                </button>
              </div>
            </div>
            {verification.tags && verification.tags.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-2">
                {verification.tags.slice(0, 3).map((tag) => (
                  <span
                    key={tag}
                    className="text-xs bg-primary/10 text-primary px-1.5 py-0.5 rounded"
                  >
                    {tag}
                  </span>
                ))}
                {verification.tags.length > 3 && (
                  <span className="text-xs text-muted-foreground">
                    +{verification.tags.length - 3}
                  </span>
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
                className="flex-1 flex items-center justify-center gap-2 py-2 bg-amber-500 text-white rounded-lg hover:bg-amber-600 transition-colors disabled:opacity-50"
              >
                {runningId === task.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Task
              </button>
              <button
                onClick={() => duplicateItem(task.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deletePrompt(task.id)}
                className="p-2 border border-border rounded-lg hover:bg-red-500/10 hover:text-red-500 transition-colors"
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
        );
      }

      case "workflows": {
        const workflow = workflows.find((w) => w.id === selectedItemId);
        if (!workflow) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{workflow.name}</h3>
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
            {workflow.goal && (
              <div className="flex items-start gap-2">
                <Target className="w-4 h-4 text-muted-foreground mt-0.5" />
                <p className="text-sm">{workflow.goal}</p>
              </div>
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
            <div>
              <h4 className="text-sm font-medium mb-2 flex items-center gap-2">
                <List className="w-4 h-4" />
                Execution Steps ({workflow.steps.length})
              </h4>
              <div className="space-y-1">
                {workflow.steps.map((step, index) => (
                  <div
                    key={step.id}
                    className="flex items-center gap-2 text-sm bg-muted/50 px-3 py-2 rounded"
                  >
                    <span className="text-muted-foreground font-mono">{index + 1}.</span>
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] uppercase font-medium ${
                        step.type === "workflow"
                          ? "bg-green-500/20 text-green-600"
                          : step.type === "playwright"
                            ? "bg-purple-500/20 text-purple-600"
                            : step.type === "prompt"
                              ? "bg-amber-500/20 text-amber-600"
                              : "bg-blue-500/20 text-blue-600"
                      }`}
                    >
                      {step.type}
                    </span>
                    <span className="flex-1 truncate">{step.name}</span>
                  </div>
                ))}
              </div>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(workflow.created_at)}</span>
              <span>Modified: {formatDate(workflow.modified_at)}</span>
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runWorkflow(workflow)}
                disabled={runningId === workflow.id}
                className="flex-1 flex items-center justify-center gap-2 py-2 bg-green-500 text-white rounded-lg hover:bg-green-600 transition-colors disabled:opacity-50"
              >
                {runningId === workflow.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Workflow
              </button>
              <button
                onClick={() => duplicateItem(workflow.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteWorkflow(workflow.id)}
                className="p-2 border border-border rounded-lg hover:bg-red-500/10 hover:text-red-500 transition-colors"
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
                className="flex-1 flex items-center justify-center gap-2 py-2 bg-purple-500 text-white rounded-lg hover:bg-purple-600 transition-colors disabled:opacity-50"
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
                className="p-2 border border-border rounded-lg hover:bg-red-500/10 hover:text-red-500 transition-colors"
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
                className="flex-1 flex items-center justify-center gap-2 py-2 bg-cyan-500 text-white rounded-lg hover:bg-cyan-600 transition-colors"
              >
                <Copy className="w-4 h-4" />
                Copy Content
              </button>
              <button
                onClick={() => duplicateItem(scriptlet.id)}
                className="p-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                <Copy className="w-4 h-4" />
              </button>
              <button
                onClick={() => deleteScriptlet(scriptlet.id)}
                className="p-2 border border-border rounded-lg hover:bg-red-500/10 hover:text-red-500 transition-colors"
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

      case "verifications": {
        const verification = verifications.find((v) => v.id === selectedItemId);
        if (!verification) return null;
        return (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-lg font-semibold">{verification.name}</h3>
              <button
                onClick={() => setShowDetailPanel(false)}
                className="p-1 hover:bg-muted rounded"
              >
                <X className="w-5 h-5" />
              </button>
            </div>
            {verification.description && (
              <p className="text-muted-foreground">{verification.description}</p>
            )}
            {verification.tags && verification.tags.length > 0 && (
              <div className="flex flex-wrap gap-1">
                {verification.tags.map((tag) => (
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
                  <span className="font-medium">{verification.config.strategy}</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Max States:</span>
                  <span className="font-medium">
                    {verification.config.max_states || "Unlimited"}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">State Delay:</span>
                  <span className="font-medium">{verification.config.state_delay_ms}ms</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground">Screenshots:</span>
                  <span className="font-medium">
                    {verification.config.capture_screenshots ? "Yes" : "No"}
                  </span>
                </div>
              </div>
            </div>
            <div className="flex items-center gap-4 text-xs text-muted-foreground pt-2 border-t">
              <span>Created: {formatDate(verification.created_at)}</span>
              <span>Modified: {formatDate(verification.modified_at)}</span>
              {verification.last_run_at && (
                <span>Last Run: {formatDate(verification.last_run_at)}</span>
              )}
            </div>
            <div className="flex items-center gap-2 pt-2">
              <button
                onClick={() => runVerification(verification)}
                disabled={runningId === verification.id}
                className="flex-1 flex items-center justify-center gap-2 py-2 bg-emerald-500 text-white rounded-lg hover:bg-emerald-600 transition-colors disabled:opacity-50"
              >
                {runningId === verification.id ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Play className="w-4 h-4" />
                )}
                Run Verification
              </button>
              <button
                onClick={() => deleteVerification(verification.id)}
                className="p-2 border border-border rounded-lg hover:bg-red-500/10 hover:text-red-500 transition-colors"
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
                : category.id === "workflows"
                  ? workflows.length
                  : category.id === "scripts"
                    ? scripts.length
                    : category.id === "scriptlets"
                      ? scriptlets.length
                      : contexts.length;

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
                <Icon className={`w-4 h-4 ${category.color}`} />
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
            <CategoryIcon className={`w-5 h-5 ${categoryConfig.color}`} />
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

      {/* Task Creation Dialog */}
      {showTaskDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/50" onClick={() => setShowTaskDialog(false)} />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <FileText className="w-5 h-5 text-amber-500" />
                <h3 className="text-lg font-semibold">New Task</h3>
              </div>
              <button
                onClick={() => setShowTaskDialog(false)}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              Create a new single-step AI task. Tasks can be run directly from the Library.
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
                onClick={() => setShowTaskDialog(false)}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveTask}
                disabled={isSavingTask || !taskForm.name.trim() || !taskForm.content.trim()}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-amber-500 text-white rounded-md font-medium hover:bg-amber-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isSavingTask ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Creating...
                  </>
                ) : (
                  <>
                    <Plus className="w-4 h-4" />
                    Create Task
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Scriptlet Creation Dialog */}
      {showScriptletDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => setShowScriptletDialog(false)}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Puzzle className="w-5 h-5 text-cyan-500" />
                <h3 className="text-lg font-semibold">New Scriptlet</h3>
              </div>
              <button
                onClick={() => setShowScriptletDialog(false)}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              Create a reusable snippet that can be referenced in other tasks and workflows.
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
                onClick={() => setShowScriptletDialog(false)}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveScriptlet}
                disabled={
                  isSavingScriptlet || !scriptletForm.name.trim() || !scriptletForm.content.trim()
                }
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-cyan-500 text-white rounded-md font-medium hover:bg-cyan-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isSavingScriptlet ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Creating...
                  </>
                ) : (
                  <>
                    <Plus className="w-4 h-4" />
                    Create Scriptlet
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Verification Creation Dialog */}
      {showVerificationDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/50"
            onClick={() => setShowVerificationDialog(false)}
          />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-lg mx-4 p-6 space-y-4 max-h-[90vh] overflow-y-auto">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <ShieldCheck className="w-5 h-5 text-emerald-500" />
                <h3 className="text-lg font-semibold">New Verification</h3>
              </div>
              <button
                onClick={() => setShowVerificationDialog(false)}
                className="p-1 text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            <p className="text-sm text-muted-foreground">
              Create a reusable verification configuration that can be run against any loaded
              config.
            </p>

            <div className="space-y-3">
              <div>
                <label className="block text-sm font-medium mb-1">Name *</label>
                <input
                  type="text"
                  value={verificationForm.name}
                  onChange={(e) =>
                    setVerificationForm({ ...verificationForm, name: e.target.value })
                  }
                  placeholder="Enter verification name..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                  autoFocus
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Description</label>
                <textarea
                  value={verificationForm.description}
                  onChange={(e) =>
                    setVerificationForm({ ...verificationForm, description: e.target.value })
                  }
                  placeholder="Optional description..."
                  className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-16 focus:outline-none focus:ring-2 focus:ring-primary"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">Strategy</label>
                <select
                  value={verificationForm.strategy}
                  onChange={(e) =>
                    setVerificationForm({
                      ...verificationForm,
                      strategy: e.target.value as VerificationTaskConfig["strategy"],
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
                    value={verificationForm.max_states}
                    onChange={(e) =>
                      setVerificationForm({
                        ...verificationForm,
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
                    value={verificationForm.state_delay_ms}
                    onChange={(e) =>
                      setVerificationForm({
                        ...verificationForm,
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
                    checked={verificationForm.capture_screenshots}
                    onChange={(e) =>
                      setVerificationForm({
                        ...verificationForm,
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
                    checked={verificationForm.stop_on_first_failure}
                    onChange={(e) =>
                      setVerificationForm({
                        ...verificationForm,
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
                onClick={() => setShowVerificationDialog(false)}
                className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleSaveVerification}
                disabled={isSavingVerification || !verificationForm.name.trim()}
                className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-emerald-500 text-white rounded-md font-medium hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                {isSavingVerification ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Creating...
                  </>
                ) : (
                  <>
                    <Plus className="w-4 h-4" />
                    Create Verification
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
