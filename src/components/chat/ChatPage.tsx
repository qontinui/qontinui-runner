import { useState, useCallback } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  ChatHeader,
  ChatInput,
  ChatMessageArea,
  WorkflowPreviewPanel,
} from "@qontinui/workflow-ui";
import type { UnifiedWorkflow } from "@qontinui/shared-types";
import { autoNameFromMessage } from "@qontinui/workflow-utils";
import { useChatSession } from "../../hooks/useChatSession";
import { CHAT_MARKDOWN_COMPONENTS } from "../markdown/shared-components";
import { ChatSidebar } from "./ChatSidebar";

interface ChatPageProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
}

export function ChatPage({ onNavigateToBuilder, onNavigateToActive }: ChatPageProps) {
  const {
    taskRunId,
    sessionState,
    messages,
    streamingContent,
    isGeneratingWorkflow,
    toolActivity,
    createSession,
    switchSession,
    sendMessage,
    interrupt,
    close,
    rename,
    generateWorkflow,
    resetSession,
  } = useChatSession();

  const [sessionName, setSessionName] = useState("New Chat");
  const [generatedWorkflow, setGeneratedWorkflow] = useState<UnifiedWorkflow | null>(null);
  const [workflowError, setWorkflowError] = useState<string | undefined>();
  const [showWorkflowPanel, setShowWorkflowPanel] = useState(false);
  const [sidebarRefreshKey, setSidebarRefreshKey] = useState(0);

  const refreshSidebar = useCallback(() => {
    setSidebarRefreshKey((k) => k + 1);
  }, []);

  const renderMarkdown = useCallback(
    (content: string) => (
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={CHAT_MARKDOWN_COMPONENTS}>
        {content}
      </ReactMarkdown>
    ),
    [],
  );

  const handleCreateSession = useCallback(async () => {
    setSessionName("New Chat");
    setGeneratedWorkflow(null);
    setWorkflowError(undefined);
    setShowWorkflowPanel(false);
    await createSession("New Chat");
    refreshSidebar();
  }, [createSession, refreshSidebar]);

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      setGeneratedWorkflow(null);
      setWorkflowError(undefined);
      setShowWorkflowPanel(false);
      setSessionName(""); // Will be populated from sidebar data
      switchSession(sessionId);
    },
    [switchSession],
  );

  const handleSendMessage = useCallback(
    (content: string) => {
      if (messages.length === 0 && (sessionName === "New Chat" || sessionName === "")) {
        const autoName = "Chat: " + autoNameFromMessage(content, 34);
        setSessionName(autoName);
        rename(autoName);
        // Refresh sidebar so the new name shows up
        setTimeout(refreshSidebar, 500);
      }
      sendMessage(content);
    },
    [messages.length, sessionName, sendMessage, rename, refreshSidebar],
  );

  const handleRename = useCallback(
    (name: string) => {
      setSessionName(name);
      rename(name);
      refreshSidebar();
    },
    [rename, refreshSidebar],
  );

  const handleClose = useCallback(() => {
    close();
    resetSession();
    refreshSidebar();
  }, [close, resetSession, refreshSidebar]);

  const handleGenerateWorkflow = useCallback(
    async (includeUIBridge: boolean) => {
      setShowWorkflowPanel(true);
      setGeneratedWorkflow(null);
      setWorkflowError(undefined);

      const workflow = await generateWorkflow(includeUIBridge);
      if (workflow) {
        setGeneratedWorkflow(workflow as UnifiedWorkflow);
      } else {
        setWorkflowError("Failed to generate workflow. Please try again.");
      }
    },
    [generateWorkflow],
  );

  const handleCreateWorkflowFromMessage = useCallback(
    async (_messageIndex: number, content: string) => {
      setShowWorkflowPanel(true);
      setGeneratedWorkflow(null);
      setWorkflowError(undefined);

      const workflow = await generateWorkflow(true, content);
      if (workflow) {
        setGeneratedWorkflow(workflow as UnifiedWorkflow);
      } else {
        setWorkflowError("Failed to generate workflow. Please try again.");
      }
    },
    [generateWorkflow],
  );

  const handleSaveWorkflow = useCallback(async () => {
    if (!generatedWorkflow) return;
    try {
      await fetch("http://127.0.0.1:9876/unified-workflows", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(generatedWorkflow),
      });
    } catch (e) {
      console.error("[ChatPage] Failed to save workflow:", e);
    }
  }, [generatedWorkflow]);

  const handleExecute = useCallback(async () => {
    if (!generatedWorkflow) return;
    try {
      close();
      await fetch("http://127.0.0.1:9876/unified-workflows/execute-inline", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(generatedWorkflow),
      });
      onNavigateToActive?.();
    } catch (e) {
      console.error("[ChatPage] Failed to execute workflow:", e);
    }
  }, [generatedWorkflow, close, onNavigateToActive]);

  const handleEditInBuilder = useCallback(() => {
    if (!generatedWorkflow) return;
    try {
      localStorage.setItem("qontinui-chat-generated-workflow", JSON.stringify(generatedWorkflow));
    } catch {
      // ignore storage errors
    }
    onNavigateToBuilder?.();
  }, [generatedWorkflow, onNavigateToBuilder]);

  const handleRegenerate = useCallback(async () => {
    setGeneratedWorkflow(null);
    setWorkflowError(undefined);
    const workflow = await generateWorkflow(true);
    if (workflow) {
      setGeneratedWorkflow(workflow as UnifiedWorkflow);
    } else {
      setWorkflowError("Failed to regenerate workflow.");
    }
  }, [generateWorkflow]);

  return (
    <div className="h-full flex">
      {/* Chat sidebar */}
      <ChatSidebar
        activeSessionId={taskRunId}
        onSelectSession={handleSelectSession}
        onNewChat={handleCreateSession}
        refreshKey={sidebarRefreshKey}
      />

      {/* Main area */}
      {!taskRunId ? (
        /* No session selected — empty state */
        <div className="flex-1 h-full flex items-center justify-center">
          <div className="text-center space-y-3 max-w-sm">
            <svg
              className="w-10 h-10 mx-auto text-foreground/20"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
            >
              <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
            </svg>
            <p className="text-foreground/40 text-sm">Select a chat or start a new one</p>
          </div>
        </div>
      ) : (
        /* Active session */
        <>
          <div className={`flex flex-col ${showWorkflowPanel ? "w-1/2" : "flex-1"} h-full`}>
            <ChatHeader
              sessionName={sessionName}
              sessionState={sessionState}
              onRename={handleRename}
              onClose={handleClose}
            />

            <div className="flex-1 min-h-0 flex flex-col px-4">
              <ChatMessageArea
                messages={messages}
                streamingContent={streamingContent}
                isStreaming={sessionState === "processing"}
                renderMarkdown={renderMarkdown}
                onCreateWorkflowFromMessage={handleCreateWorkflowFromMessage}
                toolActivity={toolActivity}
              />
            </div>

            <ChatInput
              sessionState={sessionState}
              onSendMessage={handleSendMessage}
              onInterrupt={interrupt}
              onGenerateWorkflow={handleGenerateWorkflow}
              isGeneratingWorkflow={isGeneratingWorkflow}
              messageCount={messages.length}
            />
          </div>

          {showWorkflowPanel && (
            <div className="w-1/2 h-full">
              <WorkflowPreviewPanel
                workflow={generatedWorkflow}
                isLoading={isGeneratingWorkflow}
                error={workflowError}
                onExecute={handleExecute}
                onEditInBuilder={handleEditInBuilder}
                onRegenerate={handleRegenerate}
                onSave={handleSaveWorkflow}
                onClose={() => setShowWorkflowPanel(false)}
              />
            </div>
          )}
        </>
      )}
    </div>
  );
}
