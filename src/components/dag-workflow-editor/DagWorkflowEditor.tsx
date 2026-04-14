/**
 * DagWorkflowEditor.tsx
 *
 * Split-pane DAG workflow editor: Monaco YAML editor on the left,
 * ReactFlow DAG visualization on the right.
 */

import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import Editor, { type OnMount } from "@monaco-editor/react";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { DagGraphPanel, type ValidationResult } from "./DagGraphPanel";
import {
  FileUp,
  FileDown,
  CheckCircle2,
  AlertTriangle,
  XCircle,
  GitBranch,
  Code2,
  GripVertical,
} from "lucide-react";
import { cn } from "@/lib/utils";

// ============================================================================
// Default template
// ============================================================================

const DEFAULT_YAML = `name: "My Workflow"
description: "A new DAG workflow"

settings:
  max_iterations: 5

nodes:
  step_1:
    name: "Install dependencies"
    command: "npm install"

  step_2:
    name: "Run tests"
    command: "npm test"
    depends_on: [step_1]

  step_3:
    name: "Build project"
    command: "npm run build"
    depends_on: [step_2]
`;

// ============================================================================
// Validation status display
// ============================================================================

type ValidateStatus = "idle" | "validating" | "valid" | "warning" | "error";

interface StatusConfig {
  icon: React.ReactNode;
  label: string;
  badgeVariant: "success" | "warning" | "danger" | "muted";
}

function getStatusConfig(status: ValidateStatus): StatusConfig {
  switch (status) {
    case "valid":
      return {
        icon: <CheckCircle2 className="w-3.5 h-3.5" />,
        label: "Valid",
        badgeVariant: "success",
      };
    case "warning":
      return {
        icon: <AlertTriangle className="w-3.5 h-3.5" />,
        label: "Warnings",
        badgeVariant: "warning",
      };
    case "error":
      return {
        icon: <XCircle className="w-3.5 h-3.5" />,
        label: "Errors",
        badgeVariant: "danger",
      };
    case "validating":
      return {
        icon: <AlertTriangle className="w-3.5 h-3.5 animate-pulse" />,
        label: "Checking...",
        badgeVariant: "muted",
      };
    default:
      return {
        icon: <Code2 className="w-3.5 h-3.5" />,
        label: "Not validated",
        badgeVariant: "muted",
      };
  }
}

// ============================================================================
// Main Editor Component
// ============================================================================

export function DagWorkflowEditor() {
  const [yamlContent, setYamlContent] = useState(DEFAULT_YAML);
  const [workflowName, setWorkflowName] = useState("My Workflow");
  const [validationStatus, setValidationStatus] = useState<ValidateStatus>("idle");
  const [validationResult, setValidationResult] = useState<ValidationResult | null>(null);
  const [nodeCount, setNodeCount] = useState(0);
  const [lastSaved, setLastSaved] = useState<Date | null>(null);
  const [leftWidth, setLeftWidth] = useState(50); // percent
  const [isDragging, setIsDragging] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const editorRef = useRef<Parameters<OnMount>[0] | null>(null);

  // ---- Validation ----

  const validate = useCallback(async (content: string) => {
    if (!content.trim()) {
      setValidationStatus("idle");
      setValidationResult(null);
      setNodeCount(0);
      return;
    }
    setValidationStatus("validating");
    try {
      const result = await invoke<ValidationResult>("validate_dag_workflow", {
        yamlContent: content,
      });
      setValidationResult(result);
      if (result.valid) {
        setValidationStatus("valid");
        setNodeCount(result.node_count ?? 0);
        if (result.name) setWorkflowName(result.name);
      } else {
        setValidationStatus("error");
      }
    } catch (err) {
      // Backend command may not exist yet — use client-side fallback
      try {
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        const yaml = require("js-yaml") as typeof import("js-yaml");
        const parsed = yaml.load(content) as Record<string, unknown> | null;
        if (parsed && typeof parsed === "object") {
          const nodes = parsed.nodes as Record<string, unknown> | undefined;
          const count = nodes ? Object.keys(nodes).length : 0;
          const name = parsed.name as string | undefined;
          setValidationResult({ valid: true, name, node_count: count });
          setValidationStatus("valid");
          setNodeCount(count);
          if (name) setWorkflowName(name);
        } else {
          setValidationResult({ valid: false, errors: ["Empty or invalid YAML"] });
          setValidationStatus("error");
        }
      } catch (yamlErr) {
        const errMsg = yamlErr instanceof Error ? yamlErr.message : String(yamlErr);
        setValidationResult({ valid: false, errors: [errMsg] });
        setValidationStatus("error");
      }
    }
  }, []);

  const scheduleValidation = useCallback(
    (content: string) => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => validate(content), 500);
    },
    [validate],
  );

  useEffect(() => {
    scheduleValidation(yamlContent);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [yamlContent, scheduleValidation]);

  // ---- Import / Export ----

  const handleImport = useCallback(async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "YAML files", extensions: ["yaml", "yml"] }],
      });
      if (!selected || typeof selected !== "string") return;
      const content = await readTextFile(selected);
      setYamlContent(content);
      editorRef.current?.setValue(content);
    } catch (err) {
      console.error("Import failed:", err);
    }
  }, []);

  const handleExport = useCallback(async () => {
    try {
      const path = await save({
        defaultPath: `${workflowName.replace(/\s+/g, "_")}.yaml`,
        filters: [{ name: "YAML files", extensions: ["yaml", "yml"] }],
      });
      if (!path) return;
      await writeTextFile(path, yamlContent);
      setLastSaved(new Date());
    } catch (err) {
      console.error("Export failed:", err);
    }
  }, [yamlContent, workflowName]);

  // ---- Resize handle ----

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  useEffect(() => {
    if (!isDragging) return;

    const onMouseMove = (e: MouseEvent) => {
      if (!containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const pct = (x / rect.width) * 100;
      setLeftWidth(Math.min(Math.max(pct, 20), 80));
    };

    const onMouseUp = () => setIsDragging(false);

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [isDragging]);

  const statusConfig = getStatusConfig(validationStatus);

  return (
    <div className="flex flex-col h-full bg-background text-foreground overflow-hidden">
      {/* ------------------------------------------------------------------ */}
      {/* Toolbar                                                              */}
      {/* ------------------------------------------------------------------ */}
      <div className="flex items-center gap-3 px-4 py-2.5 border-b border-border bg-muted/30 flex-shrink-0">
        {/* Icon + name */}
        <div className="flex items-center gap-2 mr-1">
          <GitBranch className="w-4 h-4 text-indigo-400" />
          <input
            type="text"
            value={workflowName}
            onChange={(e) => setWorkflowName(e.target.value)}
            className="text-sm font-semibold bg-transparent border-none outline-none text-foreground placeholder:text-muted-foreground w-48 focus:bg-muted/30 rounded px-1 -mx-1 transition-colors"
            placeholder="Workflow name"
          />
        </div>

        <div className="h-5 w-px bg-border" />

        {/* Import / Export */}
        <Button variant="ghost" size="sm" onClick={handleImport} className="gap-1.5">
          <FileUp className="w-3.5 h-3.5" />
          Import
        </Button>
        <Button variant="ghost" size="sm" onClick={handleExport} className="gap-1.5">
          <FileDown className="w-3.5 h-3.5" />
          Export
        </Button>

        <div className="h-5 w-px bg-border" />

        {/* Validation status badge */}
        <Badge variant={statusConfig.badgeVariant} size="sm" className="gap-1">
          {statusConfig.icon}
          {statusConfig.label}
        </Badge>

        {/* Error details */}
        {validationStatus === "error" && validationResult?.errors?.length && (
          <span className="text-xs text-red-400 truncate max-w-xs">
            {validationResult.errors[0]}
          </span>
        )}

        <div className="flex-1" />

        {/* Right side meta */}
        <span className="text-xs text-muted-foreground">
          {nodeCount > 0 && `${nodeCount} node${nodeCount !== 1 ? "s" : ""}`}
        </span>
      </div>

      {/* ------------------------------------------------------------------ */}
      {/* Split pane body                                                      */}
      {/* ------------------------------------------------------------------ */}
      <div
        ref={containerRef}
        className={cn("flex flex-1 min-h-0 overflow-hidden", isDragging && "select-none")}
      >
        {/* Left: Monaco YAML editor */}
        <div style={{ width: `${leftWidth}%` }} className="flex flex-col min-w-0 h-full">
          <div className="flex items-center gap-2 px-3 py-1.5 border-b border-border bg-muted/20 flex-shrink-0">
            <Code2 className="w-3.5 h-3.5 text-muted-foreground" />
            <span className="text-xs font-medium text-muted-foreground">YAML Editor</span>
          </div>
          <div className="flex-1 min-h-0">
            <Editor
              height="100%"
              defaultLanguage="yaml"
              value={yamlContent}
              theme="vs-dark"
              onChange={(val) => setYamlContent(val ?? "")}
              onMount={(editor) => {
                editorRef.current = editor;
              }}
              options={{
                minimap: { enabled: false },
                fontSize: 13,
                lineHeight: 20,
                scrollBeyondLastLine: false,
                wordWrap: "on",
                tabSize: 2,
                insertSpaces: true,
                renderLineHighlight: "line",
                scrollbar: {
                  vertical: "auto",
                  horizontal: "auto",
                  useShadows: false,
                },
                padding: { top: 12, bottom: 12 },
                fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
              }}
            />
          </div>
        </div>

        {/* Resize handle */}
        <div
          onMouseDown={handleMouseDown}
          className={cn(
            "flex-shrink-0 w-1.5 cursor-col-resize relative group flex items-center justify-center",
            "border-x border-border bg-muted/10 hover:bg-indigo-500/20 transition-colors",
            isDragging && "bg-indigo-500/30",
          )}
        >
          <GripVertical className="w-3 h-3 text-muted-foreground/40 group-hover:text-indigo-400 transition-colors" />
        </div>

        {/* Right: DAG graph */}
        <div style={{ width: `${100 - leftWidth}%` }} className="flex flex-col min-w-0 h-full">
          <div className="flex items-center gap-2 px-3 py-1.5 border-b border-border bg-muted/20 flex-shrink-0">
            <GitBranch className="w-3.5 h-3.5 text-muted-foreground" />
            <span className="text-xs font-medium text-muted-foreground">DAG Graph</span>
            {nodeCount > 0 && (
              <Badge variant="muted" size="sm">
                {nodeCount} nodes
              </Badge>
            )}
          </div>
          <div className="flex-1 min-h-0">
            <DagGraphPanel yamlContent={yamlContent} validationResult={validationResult} />
          </div>
        </div>
      </div>

      {/* ------------------------------------------------------------------ */}
      {/* Status bar                                                           */}
      {/* ------------------------------------------------------------------ */}
      <div className="flex items-center gap-4 px-4 py-1 border-t border-border bg-muted/20 flex-shrink-0">
        <span className="text-[11px] text-muted-foreground">
          {validationStatus === "valid" && nodeCount > 0
            ? `${nodeCount} node${nodeCount !== 1 ? "s" : ""} · workflow valid`
            : validationStatus === "error"
              ? `${validationResult?.errors?.length ?? 1} error${(validationResult?.errors?.length ?? 1) !== 1 ? "s" : ""} detected`
              : validationStatus === "validating"
                ? "Validating..."
                : "Ready"}
        </span>
        <div className="flex-1" />
        {lastSaved && (
          <span className="text-[11px] text-muted-foreground">
            Saved {lastSaved.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
          </span>
        )}
        <span className="text-[11px] text-muted-foreground">DAG Workflow Editor</span>
      </div>
    </div>
  );
}
