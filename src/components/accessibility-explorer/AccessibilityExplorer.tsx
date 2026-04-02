/**
 * AccessibilityExplorer.tsx
 *
 * Dashboard panel for inspecting the native accessibility tree
 * of any desktop application via the Tauri a11y backend.
 */

import { useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Accessibility,
  RefreshCw,
  Plug,
  Unplug,
  Search,
  MousePointer,
  Type,
  Focus,
  Copy,
  Eye,
} from "lucide-react";
import { cn } from "../../lib/utils";
import {
  Button,
  Badge,
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  ScrollArea,
  Tabs,
  TabsList,
  TabsTrigger,
  TabsContent,
} from "../ui";
import TreeNodeView from "./TreeNodeView";
import type {
  UnifiedNode,
  UnifiedSnapshot,
  ConnectionResult,
  ActionResult,
  QueryResult,
} from "./types";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatStateBadges(state: UnifiedNode["state"]): Array<{ label: string; active: boolean }> {
  return [
    { label: "focused", active: state.is_focused },
    { label: "disabled", active: state.is_disabled },
    { label: "hidden", active: state.is_hidden },
    { label: "readonly", active: state.is_readonly },
    { label: "required", active: state.is_required },
    { label: "editable", active: state.is_editable },
    { label: "focusable", active: state.is_focusable },
    { label: "modal", active: state.is_modal },
    { label: "multiselectable", active: state.is_multiselectable },
    ...(state.is_expanded !== "n/a"
      ? [{ label: `expanded:${state.is_expanded}`, active: state.is_expanded === "true" }]
      : []),
    ...(state.is_selected !== "n/a"
      ? [{ label: `selected:${state.is_selected}`, active: state.is_selected === "true" }]
      : []),
    ...(state.is_checked !== "n/a"
      ? [{ label: `checked:${state.is_checked}`, active: state.is_checked === "true" }]
      : []),
    ...(state.is_pressed !== "n/a"
      ? [{ label: `pressed:${state.is_pressed}`, active: state.is_pressed === "true" }]
      : []),
  ];
}

async function copyToClipboard(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // fallback: no-op
  }
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export default function AccessibilityExplorer() {
  // Connection state
  const [connected, setConnected] = useState(false);
  const [backendName, setBackendName] = useState<string>("");
  const [target, setTarget] = useState("");
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);

  // Tree state
  const [snapshot, setSnapshot] = useState<UnifiedSnapshot | null>(null);
  const [selectedNode, setSelectedNode] = useState<UnifiedNode | null>(null);
  const [loading, setLoading] = useState(false);

  // Action state
  const [actionMessage, setActionMessage] = useState<string | null>(null);
  const [typeText, setTypeText] = useState("");
  const [showTypeInput, setShowTypeInput] = useState(false);

  // Query state
  const [queryRole, setQueryRole] = useState("");
  const [queryLabel, setQueryLabel] = useState("");
  const [queryInteractiveOnly, setQueryInteractiveOnly] = useState(false);
  const [queryResults, setQueryResults] = useState<UnifiedNode[]>([]);
  const [queryLoading, setQueryLoading] = useState(false);

  // AI Context state
  const [aiContext, setAiContext] = useState<string>("");
  const [aiContextLoading, setAiContextLoading] = useState(false);

  // -------------------------------------------------------------------------
  // Connection
  // -------------------------------------------------------------------------

  const handleConnect = useCallback(async () => {
    if (!target.trim()) return;
    setConnecting(true);
    setConnectionError(null);
    try {
      const result = await invoke<ConnectionResult>("a11y_connect", {
        target: target.trim(),
        backend: "auto",
      });
      if (result.connected) {
        setConnected(true);
        setBackendName(result.backend);
        // Auto-capture on connect
        const snap = await invoke<UnifiedSnapshot>("a11y_capture", {
          includeHidden: false,
          maxDepth: null,
        });
        setSnapshot(snap);
      }
    } catch (err) {
      setConnectionError(err instanceof Error ? err.message : String(err));
    } finally {
      setConnecting(false);
    }
  }, [target]);

  const handleDisconnect = useCallback(async () => {
    try {
      await invoke("a11y_disconnect");
    } catch {
      // best-effort
    }
    setConnected(false);
    setBackendName("");
    setSnapshot(null);
    setSelectedNode(null);
    setActionMessage(null);
    setQueryResults([]);
    setAiContext("");
  }, []);

  const handleRefresh = useCallback(async () => {
    setLoading(true);
    try {
      const snap = await invoke<UnifiedSnapshot>("a11y_capture", {
        includeHidden: false,
        maxDepth: null,
      });
      setSnapshot(snap);
      setSelectedNode(null);
    } catch (err) {
      setConnectionError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  // -------------------------------------------------------------------------
  // Actions
  // -------------------------------------------------------------------------

  const runAction = useCallback(async (command: string, args: Record<string, unknown>) => {
    setActionMessage(null);
    try {
      const result = await invoke<ActionResult>(command, args);
      setActionMessage(result.success ? "Action succeeded" : `Failed: ${result.message ?? "unknown"}`);
    } catch (err) {
      setActionMessage(`Error: ${err instanceof Error ? err.message : String(err)}`);
    }
  }, []);

  const handleClick = useCallback(() => {
    if (!selectedNode) return;
    runAction("a11y_click", { refId: selectedNode.ref });
  }, [selectedNode, runAction]);

  const handleFocus = useCallback(() => {
    if (!selectedNode) return;
    runAction("a11y_focus", { refId: selectedNode.ref });
  }, [selectedNode, runAction]);

  const handleTypeText = useCallback(() => {
    if (!selectedNode || !typeText) return;
    runAction("a11y_type_text", { refId: selectedNode.ref, text: typeText, clearFirst: false });
    setTypeText("");
    setShowTypeInput(false);
  }, [selectedNode, typeText, runAction]);

  // -------------------------------------------------------------------------
  // Query
  // -------------------------------------------------------------------------

  const handleQuery = useCallback(async () => {
    setQueryLoading(true);
    try {
      const result = await invoke<QueryResult>("a11y_query", {
        role: queryRole || null,
        label: null,
        labelContains: queryLabel || null,
        interactiveOnly: queryInteractiveOnly,
      });
      setQueryResults(result.elements);
    } catch (err) {
      setConnectionError(err instanceof Error ? err.message : String(err));
    } finally {
      setQueryLoading(false);
    }
  }, [queryRole, queryLabel, queryInteractiveOnly]);

  const handleFetchAiContext = useCallback(async () => {
    setAiContextLoading(true);
    try {
      const result = await invoke<string>("a11y_ai_context", {
        maxElements: 50,
        interactiveOnly: true,
      });
      setAiContext(result);
    } catch (err) {
      setAiContext(`Error: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setAiContextLoading(false);
    }
  }, []);

  // -------------------------------------------------------------------------
  // Node selection
  // -------------------------------------------------------------------------

  const handleSelectNode = useCallback((node: UnifiedNode) => {
    setSelectedNode(node);
    setActionMessage(null);
    setShowTypeInput(false);
  }, []);

  // -------------------------------------------------------------------------
  // Memoized state badges
  // -------------------------------------------------------------------------

  const stateBadges = useMemo(
    () => (selectedNode ? formatStateBadges(selectedNode.state) : []),
    [selectedNode],
  );

  // -------------------------------------------------------------------------
  // Render
  // -------------------------------------------------------------------------

  return (
    <div className="flex flex-col h-full gap-3 p-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Accessibility className="w-5 h-5 text-primary" />
          <h2 className="text-lg font-semibold">Accessibility Explorer</h2>
          <Badge variant={connected ? "success" : "muted"} size="sm">
            {connected ? backendName.toUpperCase() : "Disconnected"}
          </Badge>
        </div>
        {connected && (
          <div className="flex items-center gap-1.5">
            <Button variant="ghost" size="sm" onClick={handleRefresh} disabled={loading}>
              <RefreshCw className={cn("w-3.5 h-3.5", loading && "animate-spin")} />
              Refresh
            </Button>
            <Button variant="ghost" size="sm" onClick={handleDisconnect}>
              <Unplug className="w-3.5 h-3.5" />
              Disconnect
            </Button>
          </div>
        )}
      </div>

      {/* Connection Bar */}
      {!connected && (
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleConnect()}
            placeholder="Window title, pid:1234, or 'Desktop'"
            className={cn(
              "flex-1 px-3 py-1.5 text-sm rounded-md bg-muted/30 border border-border",
              "text-foreground placeholder:text-muted-foreground/50",
              "focus:outline-none focus:ring-1 focus:ring-primary/50",
            )}
          />
          <Button
            variant="primary"
            size="sm"
            onClick={handleConnect}
            disabled={connecting || !target.trim()}
          >
            <Plug className="w-3.5 h-3.5" />
            {connecting ? "Connecting..." : "Connect"}
          </Button>
        </div>
      )}

      {connectionError && (
        <div className="px-3 py-2 text-xs text-red-400 bg-red-500/10 rounded-md border border-red-500/20">
          {connectionError}
        </div>
      )}

      {/* Main Content */}
      {connected && snapshot && (
        <>
          {/* Stats bar */}
          <div className="flex items-center gap-3 text-xs text-muted-foreground">
            <span>{snapshot.total_nodes} nodes</span>
            <span>{snapshot.interactive_nodes} interactive</span>
            {snapshot.title && <span className="truncate">Title: {snapshot.title}</span>}
            <span className="ml-auto">Gen {snapshot.generation}</span>
          </div>

          {/* Two-column layout */}
          <div className="flex-1 flex gap-3 min-h-0">
            {/* Left: Tree View */}
            <Card className="flex-[3] flex flex-col min-h-0">
              <CardHeader className="py-2 px-3 border-b border-border/30">
                <CardTitle className="text-xs">
                  <Eye className="w-3.5 h-3.5" />
                  Tree View
                </CardTitle>
              </CardHeader>
              <ScrollArea className="flex-1">
                <div className="p-1" role="tree">
                  <TreeNodeView
                    node={snapshot.root}
                    depth={0}
                    selectedRef={selectedNode?.ref ?? null}
                    onSelect={handleSelectNode}
                    defaultExpanded
                  />
                </div>
              </ScrollArea>
            </Card>

            {/* Right: Details Panel */}
            <Card className="flex-[2] flex flex-col min-h-0">
              <CardHeader className="py-2 px-3 border-b border-border/30">
                <CardTitle className="text-xs">
                  <Search className="w-3.5 h-3.5" />
                  Details
                </CardTitle>
              </CardHeader>
              <ScrollArea className="flex-1">
                {selectedNode ? (
                  <DetailsPanel
                    node={selectedNode}
                    stateBadges={stateBadges}
                    actionMessage={actionMessage}
                    showTypeInput={showTypeInput}
                    typeText={typeText}
                    onTypeTextChange={setTypeText}
                    onShowTypeInput={() => setShowTypeInput(true)}
                    onClickAction={handleClick}
                    onFocusAction={handleFocus}
                    onTypeAction={handleTypeText}
                  />
                ) : (
                  <div className="p-4 text-sm text-muted-foreground text-center">
                    Select a node to view details
                  </div>
                )}
              </ScrollArea>
            </Card>
          </div>

          {/* Bottom: Query Bar + AI Context */}
          <Card className="flex-shrink-0">
            <Tabs defaultValue="query">
              <CardHeader className="py-1.5 px-3 border-b border-border/30">
                <TabsList>
                  <TabsTrigger value="query">Query</TabsTrigger>
                  <TabsTrigger value="ai-context">AI Context</TabsTrigger>
                </TabsList>
              </CardHeader>
              <CardContent className="px-3 py-2">
                <TabsContent value="query">
                  <QueryPanel
                    queryRole={queryRole}
                    queryLabel={queryLabel}
                    queryInteractiveOnly={queryInteractiveOnly}
                    queryResults={queryResults}
                    queryLoading={queryLoading}
                    onRoleChange={setQueryRole}
                    onLabelChange={setQueryLabel}
                    onInteractiveOnlyChange={setQueryInteractiveOnly}
                    onSearch={handleQuery}
                    onSelectResult={handleSelectNode}
                  />
                </TabsContent>
                <TabsContent value="ai-context">
                  <AiContextPanel
                    aiContext={aiContext}
                    loading={aiContextLoading}
                    onFetch={handleFetchAiContext}
                  />
                </TabsContent>
              </CardContent>
            </Tabs>
          </Card>
        </>
      )}

      {/* Empty state */}
      {!connected && !connectionError && (
        <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
          Connect to an application to explore its accessibility tree
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components (kept in same file to stay under 400 lines)
// ---------------------------------------------------------------------------

interface DetailsPanelProps {
  node: UnifiedNode;
  stateBadges: Array<{ label: string; active: boolean }>;
  actionMessage: string | null;
  showTypeInput: boolean;
  typeText: string;
  onTypeTextChange: (v: string) => void;
  onShowTypeInput: () => void;
  onClickAction: () => void;
  onFocusAction: () => void;
  onTypeAction: () => void;
}

function DetailsPanel({
  node,
  stateBadges,
  actionMessage,
  showTypeInput,
  typeText,
  onTypeTextChange,
  onShowTypeInput,
  onClickAction,
  onFocusAction,
  onTypeAction,
}: DetailsPanelProps) {
  return (
    <div className="p-3 space-y-3 text-sm">
      {/* Identity */}
      <div className="space-y-1.5">
        <DetailRow label="Role" value={node.role} />
        {node.name && <DetailRow label="Name" value={node.name} />}
        {node.value && <DetailRow label="Value" value={node.value} />}
        {node.description && <DetailRow label="Description" value={node.description} />}
        <div className="flex items-center justify-between">
          <span className="text-muted-foreground text-xs">Ref ID</span>
          <div className="flex items-center gap-1">
            <code className="font-mono text-xs bg-muted/30 px-1.5 py-0.5 rounded">
              {node.ref}
            </code>
            <button
              type="button"
              onClick={() => copyToClipboard(node.ref)}
              className="p-0.5 rounded hover:bg-muted/40"
              title="Copy ref ID"
            >
              <Copy className="w-3 h-3 text-muted-foreground" />
            </button>
          </div>
        </div>
        <DetailRow label="Source" value={node.source.toUpperCase()} />
      </div>

      {/* Bounds */}
      {node.bounds && (
        <div>
          <span className="text-xs text-muted-foreground">Bounds</span>
          <div className="grid grid-cols-4 gap-1 mt-1">
            {(["x", "y", "width", "height"] as const).map((k) => (
              <div key={k} className="text-center">
                <div className="text-[10px] text-muted-foreground/60">{k}</div>
                <div className="text-xs font-mono">{Math.round(node.bounds![k])}</div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* State flags */}
      <div>
        <span className="text-xs text-muted-foreground">State</span>
        <div className="flex flex-wrap gap-1 mt-1">
          {stateBadges.map(({ label, active }) => (
            <Badge
              key={label}
              variant={active ? "success" : "outline"}
              size="sm"
            >
              {label}
            </Badge>
          ))}
        </div>
      </div>

      {/* Patterns */}
      {node.supported_patterns.length > 0 && (
        <div>
          <span className="text-xs text-muted-foreground">Patterns</span>
          <div className="flex flex-wrap gap-1 mt-1">
            {node.supported_patterns.map((p) => (
              <Badge key={p} variant="purple" size="sm">{p}</Badge>
            ))}
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="pt-2 border-t border-border/30 space-y-2">
        <div className="flex items-center gap-1.5">
          <Button variant="outline" size="sm" onClick={onClickAction}>
            <MousePointer className="w-3 h-3" /> Click
          </Button>
          <Button variant="outline" size="sm" onClick={onFocusAction}>
            <Focus className="w-3 h-3" /> Focus
          </Button>
          <Button variant="outline" size="sm" onClick={onShowTypeInput}>
            <Type className="w-3 h-3" /> Type...
          </Button>
        </div>
        {showTypeInput && (
          <div className="flex items-center gap-1.5">
            <input
              type="text"
              value={typeText}
              onChange={(e) => onTypeTextChange(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && onTypeAction()}
              placeholder="Text to type..."
              className="flex-1 px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-primary/50"
            />
            <Button variant="primary" size="sm" onClick={onTypeAction} disabled={!typeText}>
              Send
            </Button>
          </div>
        )}
        {actionMessage && (
          <div
            className={cn(
              "text-xs px-2 py-1 rounded",
              actionMessage.startsWith("Error") || actionMessage.startsWith("Failed")
                ? "bg-red-500/10 text-red-400"
                : "bg-green-500/10 text-green-400",
            )}
          >
            {actionMessage}
          </div>
        )}
      </div>
    </div>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-muted-foreground text-xs">{label}</span>
      <span className="text-xs font-medium truncate ml-2">{value}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Query Panel
// ---------------------------------------------------------------------------

const COMMON_ROLES = [
  "", "button", "checkbox", "combobox", "dialog", "edit", "group",
  "heading", "link", "list", "listitem", "menu", "menuitem", "pane",
  "radio", "slider", "spinbutton", "tab", "tabpanel", "text",
  "textbox", "toolbar", "tree", "treeitem", "window",
];

interface QueryPanelProps {
  queryRole: string;
  queryLabel: string;
  queryInteractiveOnly: boolean;
  queryResults: UnifiedNode[];
  queryLoading: boolean;
  onRoleChange: (v: string) => void;
  onLabelChange: (v: string) => void;
  onInteractiveOnlyChange: (v: boolean) => void;
  onSearch: () => void;
  onSelectResult: (node: UnifiedNode) => void;
}

function QueryPanel({
  queryRole,
  queryLabel,
  queryInteractiveOnly,
  queryResults,
  queryLoading,
  onRoleChange,
  onLabelChange,
  onInteractiveOnlyChange,
  onSearch,
  onSelectResult,
}: QueryPanelProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <select
          value={queryRole}
          onChange={(e) => onRoleChange(e.target.value)}
          className="px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground focus:outline-none focus:ring-1 focus:ring-primary/50"
        >
          {COMMON_ROLES.map((r) => (
            <option key={r} value={r}>{r || "Any role"}</option>
          ))}
        </select>
        <input
          type="text"
          value={queryLabel}
          onChange={(e) => onLabelChange(e.target.value)}
          placeholder="Label contains..."
          className="flex-1 px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-primary/50"
        />
        <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap cursor-pointer">
          <input
            type="checkbox"
            checked={queryInteractiveOnly}
            onChange={(e) => onInteractiveOnlyChange(e.target.checked)}
            className="rounded"
          />
          Interactive only
        </label>
        <Button variant="primary" size="sm" onClick={onSearch} disabled={queryLoading}>
          <Search className="w-3 h-3" />
          {queryLoading ? "..." : "Search"}
        </Button>
      </div>
      {queryResults.length > 0 && (
        <ScrollArea className="max-h-40">
          <div className="space-y-0.5">
            {queryResults.map((node) => (
              <button
                key={node.ref}
                type="button"
                onClick={() => onSelectResult(node)}
                className="w-full flex items-center gap-2 px-2 py-1 text-xs rounded hover:bg-muted/40 text-left"
              >
                <Badge variant="info" size="sm">{node.role}</Badge>
                <span className="truncate">{node.name ?? "(no name)"}</span>
                <span className="ml-auto font-mono text-muted-foreground/60">{node.ref}</span>
              </button>
            ))}
          </div>
        </ScrollArea>
      )}
      {queryResults.length === 0 && !queryLoading && (
        <div className="text-xs text-muted-foreground text-center py-1">
          No results. Run a query above.
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// AI Context Panel
// ---------------------------------------------------------------------------

interface AiContextPanelProps {
  aiContext: string;
  loading: boolean;
  onFetch: () => void;
}

function AiContextPanel({ aiContext, loading, onFetch }: AiContextPanelProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Button variant="outline" size="sm" onClick={onFetch} disabled={loading}>
          <RefreshCw className={cn("w-3 h-3", loading && "animate-spin")} />
          {loading ? "Loading..." : "Generate AI Context"}
        </Button>
        {aiContext && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => copyToClipboard(aiContext)}
          >
            <Copy className="w-3 h-3" /> Copy
          </Button>
        )}
      </div>
      {aiContext && (
        <pre className="text-xs font-mono bg-muted/20 border border-border/30 rounded-md p-3 max-h-48 overflow-auto whitespace-pre-wrap">
          {aiContext}
        </pre>
      )}
    </div>
  );
}
