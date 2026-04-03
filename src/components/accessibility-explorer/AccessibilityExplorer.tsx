/**
 * AccessibilityExplorer.tsx
 *
 * Dashboard panel for inspecting the native accessibility tree
 * of any desktop application via the Tauri a11y backend.
 */

import { useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Accessibility, RefreshCw, Plug, Unplug, Search, Eye } from "lucide-react";
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
import { DetailsPanel, formatStateBadges } from "./DetailsPanel";
import { QueryPanel, AiContextPanel } from "./QueryPanel";
import type {
  UnifiedNode,
  UnifiedSnapshot,
  ConnectionResult,
  ActionResult,
  QueryResult,
} from "./types";

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

  // -----------------------------------------------------------------------
  // Connection handlers
  // -----------------------------------------------------------------------

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

  // -----------------------------------------------------------------------
  // Action handlers
  // -----------------------------------------------------------------------

  const runAction = useCallback(async (command: string, args: Record<string, unknown>) => {
    setActionMessage(null);
    try {
      const result = await invoke<ActionResult>(command, args);
      setActionMessage(
        result.success ? "Action succeeded" : `Failed: ${result.message ?? "unknown"}`,
      );
    } catch (err) {
      setActionMessage(`Error: ${err instanceof Error ? err.message : String(err)}`);
    }
  }, []);

  const handleClick = useCallback(() => {
    if (selectedNode) runAction("a11y_click", { refId: selectedNode.ref });
  }, [selectedNode, runAction]);

  const handleFocus = useCallback(() => {
    if (selectedNode) runAction("a11y_focus", { refId: selectedNode.ref });
  }, [selectedNode, runAction]);

  const handleTypeText = useCallback(() => {
    if (!selectedNode || !typeText) return;
    runAction("a11y_type_text", { refId: selectedNode.ref, text: typeText, clearFirst: false });
    setTypeText("");
    setShowTypeInput(false);
  }, [selectedNode, typeText, runAction]);

  // -----------------------------------------------------------------------
  // Query handlers
  // -----------------------------------------------------------------------

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

  const handleSelectNode = useCallback((node: UnifiedNode) => {
    setSelectedNode(node);
    setActionMessage(null);
    setShowTypeInput(false);
  }, []);

  const stateBadges = useMemo(
    () => (selectedNode ? formatStateBadges(selectedNode.state) : []),
    [selectedNode],
  );

  // -----------------------------------------------------------------------
  // Render
  // -----------------------------------------------------------------------

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
            {/* Left: Tree View (60%) */}
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

            {/* Right: Details Panel (40%) */}
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
