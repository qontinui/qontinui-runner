/**
 * SpecChatPanel — AI Chat for spec review, editing, creation, and workflow building
 *
 * Right panel in the Specs page. Creates a standalone AI session
 * and prepends selected spec context into the conversation.
 *
 * Features:
 * - Spec review and gap analysis (Step 2 — AI-driven spec modifications)
 * - New spec creation via AI (Step 4 — spec creation from scratch)
 * - Build workflow from specs (Step 5 — spec-to-workflow pipeline)
 */

import { useRef, useEffect, useCallback, useReducer } from "react";
import {
  Bot,
  User,
  Send,
  Square,
  Loader2,
  Sparkles,
  Hammer,
  FilePlus,
  Download,
  RefreshCw,
} from "lucide-react";
import { useAiSession } from "@/hooks/useAiSession";
import { MarkdownViewer } from "@/components/MarkdownViewer";
import {
  SPEC_CREATION_INSTRUCTIONS,
  buildDetailedSpecContext,
  buildSpecReviewPrompt,
  buildSpecCreationWithMergeContext,
  type SpecConfig,
} from "@/lib/spec-prompt-builder";
import type { LoadedSpec, SpecKind } from "./types";

// ============================================================================
// Message bubble
// ============================================================================

function MessageBubble({
  role,
  content,
  specActions,
}: {
  role: "user" | "ai" | "system";
  content: string;
  specActions?: Array<{ kind: FileSpecKind; label: string; onApply: () => void }>;
}) {
  if (role === "system") {
    return (
      <div className="flex justify-center py-1">
        <span className="text-[10px] text-muted-foreground/50 italic px-2 py-0.5 bg-white/[0.02] rounded">
          {content}
        </span>
      </div>
    );
  }

  const isUser = role === "user";

  return (
    <div className={`flex gap-2 ${isUser ? "flex-row-reverse" : ""}`}>
      <div className="shrink-0 mt-1">
        {isUser ? (
          <div className="w-5 h-5 rounded-full bg-cyan-500/20 flex items-center justify-center">
            <User className="w-3 h-3 text-cyan-400" />
          </div>
        ) : (
          <div className="w-5 h-5 rounded-full bg-purple-500/20 flex items-center justify-center">
            <Bot className="w-3 h-3 text-purple-400" />
          </div>
        )}
      </div>
      <div
        className={`flex-1 min-w-0 px-2.5 py-1.5 rounded-lg text-xs leading-relaxed ${
          isUser
            ? "bg-cyan-500/10 border border-cyan-500/20 text-foreground"
            : "bg-white/[0.03] border border-white/5 text-foreground"
        }`}
      >
        <MarkdownViewer content={content} />
        {specActions && specActions.length > 0 && (
          <div className="mt-2 pt-2 border-t border-white/5 space-y-1">
            {specActions.map((action) => (
              <button
                key={`${action.kind}-${action.label}`}
                data-ui-id={`spec-chat-apply-${action.kind}`}
                onClick={action.onApply}
                className="w-full flex items-center gap-1.5 px-2 py-1 text-[10px] font-medium rounded
                  bg-green-500/10 text-green-400 border border-green-500/20
                  hover:bg-green-500/20 transition-colors"
              >
                <Download className="w-3 h-3" />
                Apply as {action.kind.replace("-", " ")} spec: {action.label}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Spec context builder
// ============================================================================

function buildSpecContext(spec: LoadedSpec): string {
  if (spec.kind === "page-spec") {
    return buildDetailedSpecContext({
      specId: spec.specId,
      config: spec.config,
      appName: spec.appName,
    });
  }

  if (spec.kind === "architecture") {
    return [
      `# Selected Spec: ${spec.config.projectName}`,
      spec.config.description ? `Description: ${spec.config.description}` : "",
      `Type: Architecture Spec`,
      `Tech stack: ${spec.config.techStack.map((t) => t.name).join(", ")}`,
      `Features: ${spec.config.features.length}, Patterns: ${spec.config.patterns.length}, Constraints: ${spec.config.constraints.length}`,
    ]
      .filter(Boolean)
      .join("\n");
  }

  if (spec.kind === "api") {
    return [
      `# Selected Spec: ${spec.config.description || spec.config.basePath}`,
      `Type: API Spec`,
      `Base path: ${spec.config.basePath}`,
      `Endpoints: ${spec.config.endpoints.length}, Models: ${spec.config.models.length}`,
      spec.config.authType ? `Auth: ${spec.config.authType}` : "",
    ]
      .filter(Boolean)
      .join("\n");
  }

  if (spec.kind === "data") {
    return [
      `# Selected Spec: ${spec.config.description || "Data Schema"}`,
      `Type: Data Spec`,
      `Database: ${spec.config.database}`,
      `Entities: ${spec.config.entities.length}, Relations: ${spec.config.relations.length}`,
    ]
      .filter(Boolean)
      .join("\n");
  }

  if (spec.kind === "dependency") {
    return [
      `# Selected Spec: ${spec.config.description || "Dependencies"}`,
      `Type: Dependency Spec`,
      `Modules: ${spec.config.modules.length}, Links: ${spec.config.links.length}`,
    ]
      .filter(Boolean)
      .join("\n");
  }

  // constraint
  return [
    `# Selected Spec: ${spec.config.description || "Constraints"}`,
    `Type: Constraint Spec`,
    `Performance budgets: ${spec.config.performance?.length || 0}`,
    `Browser targets: ${spec.config.browsers?.length || 0}`,
  ]
    .filter(Boolean)
    .join("\n");
}

// ============================================================================
// Quick actions
// ============================================================================

type ReviewType = "quality" | "gaps" | "improvements" | "assertions";

const REVIEW_ACTIONS: Array<{ label: string; reviewType: ReviewType }> = [
  { label: "Review spec quality", reviewType: "quality" },
  { label: "Find gaps", reviewType: "gaps" },
  { label: "Suggest improvements", reviewType: "improvements" },
  { label: "Generate assertions", reviewType: "assertions" },
];

type CreateSpecType = "page-spec" | "architecture" | "api" | "data";

const CREATE_ACTIONS: Array<{ label: string; specType: CreateSpecType; placeholder: string }> = [
  {
    label: "New Page Spec",
    specType: "page-spec",
    placeholder: "Describe the page: name, URL, what it does, which project it belongs to...",
  },
  {
    label: "New Architecture Spec",
    specType: "architecture",
    placeholder: "Describe the project: name, tech stack, key patterns...",
  },
  {
    label: "New API Spec",
    specType: "api",
    placeholder: "Describe the API: base path, endpoints, auth type...",
  },
  {
    label: "New Data Spec",
    specType: "data",
    placeholder: "Describe the schema: database type, entities, relations...",
  },
];

// ============================================================================
// JSON spec extraction from AI messages
// ============================================================================

type FileSpecKind = Exclude<SpecKind, "style-guide">;

/** Detect spec kind from parsed JSON content */
function detectSpecKindFromContent(data: Record<string, unknown>): FileSpecKind | null {
  if (data.groups && Array.isArray(data.groups)) return "page-spec";
  if (data.techStack && data.patterns) return "architecture";
  if (data.endpoints && data.basePath) return "api";
  if (data.entities && data.relations && data.database) return "data";
  if (data.modules && data.links) return "dependency";
  if (data.performance || data.bundleBudgets || data.browsers) return "constraint";
  return null;
}

/** Extract valid spec JSON blocks from a message */
function extractSpecJsonFromMessage(
  content: string,
): Array<{ kind: FileSpecKind; config: Record<string, unknown>; label: string }> {
  const results: Array<{ kind: FileSpecKind; config: Record<string, unknown>; label: string }> = [];
  // Match ```json ... ``` code blocks
  const codeBlockRegex = /```(?:json)?\s*\n([\s\S]*?)```/g;
  let match;
  while ((match = codeBlockRegex.exec(content)) !== null) {
    try {
      const parsed = JSON.parse(match[1]) as Record<string, unknown>;
      const kind = detectSpecKindFromContent(parsed);
      if (kind) {
        const label =
          (parsed.description as string) ||
          (parsed.projectName as string) ||
          (parsed.basePath as string) ||
          kind.replace("-", " ");
        results.push({ kind, config: parsed, label });
      }
    } catch {
      // Not valid JSON, skip
    }
  }
  return results;
}

// ============================================================================
// ChatMessageList -- extracted sub-component for message rendering
// ============================================================================

function ChatMessageList({
  messages,
  streamingContent,
  toolActivity,
  targetSpecId,
  onAddSpec,
  messagesEndRef,
}: {
  messages: Array<{ role: "user" | "ai" | "system"; content: string }>;
  streamingContent: string | null;
  toolActivity: string | null;
  targetSpecId: string | null;
  onAddSpec?: (spec: LoadedSpec) => void;
  messagesEndRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <>
      {messages.map((msg) => {
        const specActions =
          msg.role === "ai" && onAddSpec
            ? extractSpecJsonFromMessage(msg.content).map((extracted) => ({
                kind: extracted.kind,
                label: extracted.label,
                onApply: () => {
                  const specId =
                    targetSpecId && extracted.kind === "page-spec"
                      ? targetSpecId
                      : `ai-${extracted.kind}-${Date.now()}`;
                  const appName =
                    targetSpecId && extracted.kind === "page-spec"
                      ? targetSpecId.startsWith("runner:")
                        ? "Qontinui Runner"
                        : "Qontinui Web"
                      : "AI Generated";
                  onAddSpec({
                    specId,
                    appName,
                    kind: extracted.kind,
                    config: extracted.config as never,
                    source: "file",
                  });
                },
              }))
            : undefined;
        return (
          <MessageBubble
            key={`${msg.role}-${msg.content.slice(0, 40)}-${msg.content.length}`}
            role={msg.role}
            content={msg.content}
            specActions={specActions}
          />
        );
      })}

      {streamingContent && <MessageBubble role="ai" content={streamingContent} />}

      {toolActivity && (
        <div className="flex items-center gap-1.5 text-[10px] text-muted-foreground/50 pl-7">
          <Loader2 className="w-3 h-3 animate-spin" />
          <span className="truncate">{toolActivity}</span>
        </div>
      )}

      <div ref={messagesEndRef} />
    </>
  );
}

// ============================================================================
// ChatEmptyState — extracted sub-component for initial empty state
// ============================================================================

function ChatEmptyState({
  selectedSpec,
  showCreateActions,
  pendingCreateType,
  onToggleMode,
  onReviewAction,
  onQuickAction,
  onRegenerateSpec,
  onBuildWorkflow,
  onSelectCreateType,
}: {
  selectedSpec: LoadedSpec | null;
  showCreateActions: boolean;
  pendingCreateType: CreateSpecType | null;
  onToggleMode: (showCreate: boolean) => void;
  onReviewAction: (reviewType: ReviewType) => void;
  onQuickAction: (prompt: string) => void;
  onRegenerateSpec: (specKind: "page-spec" | "architecture") => void;
  onBuildWorkflow: () => void;
  onSelectCreateType: (specType: CreateSpecType) => void;
}) {
  return (
    <div className="flex flex-col items-center justify-center h-full gap-3">
      <Sparkles className="w-6 h-6 text-purple-400/30" />
      <p className="text-[10px] text-muted-foreground/50 text-center leading-relaxed max-w-[200px]">
        {selectedSpec
          ? "Ask AI to review, analyze, or suggest improvements for the selected spec."
          : "Select a spec from the sidebar, or create a new one."}
      </p>

      {/* Mode toggle: review vs create */}
      <div className="flex items-center gap-1 text-[10px]">
        <button
          data-ui-id="spec-chat-mode-review"
          onClick={() => onToggleMode(false)}
          className={`px-2 py-0.5 rounded ${!showCreateActions ? "bg-purple-500/20 text-purple-400" : "text-muted-foreground/50 hover:text-muted-foreground"}`}
        >
          Review
        </button>
        <button
          data-ui-id="spec-chat-mode-create"
          onClick={() => onToggleMode(true)}
          className={`px-2 py-0.5 rounded ${showCreateActions ? "bg-green-500/20 text-green-400" : "text-muted-foreground/50 hover:text-muted-foreground"}`}
        >
          Create
        </button>
      </div>

      {/* Review quick actions */}
      {!showCreateActions && selectedSpec && (
        <div className="space-y-1 w-full">
          {REVIEW_ACTIONS.map((action) => (
            <button
              key={action.label}
              onClick={() =>
                selectedSpec?.kind === "page-spec"
                  ? onReviewAction(action.reviewType)
                  : onQuickAction(`Review this spec: ${action.label}`)
              }
              className="w-full text-left px-2 py-1.5 text-[10px] text-muted-foreground/70 rounded
                bg-white/[0.02] border border-white/5 hover:bg-white/[0.05] hover:text-muted-foreground
                transition-colors"
            >
              {action.label}
            </button>
          ))}

          {/* Regenerate & merge for page specs */}
          {selectedSpec.kind === "page-spec" && (
            <button
              data-ui-id="spec-chat-regenerate"
              onClick={() => onRegenerateSpec("page-spec")}
              className="w-full flex items-center gap-1.5 px-2 py-1.5 text-[10px] text-green-400/70 rounded
                bg-green-500/5 border border-green-500/10 hover:bg-green-500/10 hover:text-green-400
                transition-colors"
            >
              <RefreshCw className="w-3 h-3" />
              Regenerate & merge spec
            </button>
          )}

          {/* Regenerate & merge for architecture specs */}
          {selectedSpec.kind === "architecture" && (
            <button
              data-ui-id="spec-chat-regenerate-arch"
              onClick={() => onRegenerateSpec("architecture")}
              className="w-full flex items-center gap-1.5 px-2 py-1.5 text-[10px] text-green-400/70 rounded
                bg-green-500/5 border border-green-500/10 hover:bg-green-500/10 hover:text-green-400
                transition-colors"
            >
              <RefreshCw className="w-3 h-3" />
              Regenerate & merge spec
            </button>
          )}

          {/* Build workflow button for page specs */}
          {selectedSpec.kind === "page-spec" && (
            <button
              data-ui-id="spec-chat-build-workflow"
              onClick={onBuildWorkflow}
              className="w-full flex items-center gap-1.5 px-2 py-1.5 text-[10px] text-orange-400/70 rounded
                bg-orange-500/5 border border-orange-500/10 hover:bg-orange-500/10 hover:text-orange-400
                transition-colors"
            >
              <Hammer className="w-3 h-3" />
              Build verification workflow
            </button>
          )}
        </div>
      )}

      {/* Create quick actions */}
      {showCreateActions && (
        <div className="space-y-1 w-full">
          {CREATE_ACTIONS.map((action) => (
            <button
              key={action.label}
              onClick={() => onSelectCreateType(action.specType)}
              className={`w-full flex items-center gap-1.5 text-left px-2 py-1.5 text-[10px] rounded
                bg-white/[0.02] border hover:bg-white/[0.05] transition-colors ${
                  pendingCreateType === action.specType
                    ? "text-green-400 border-green-500/30"
                    : "text-muted-foreground/70 border-white/5 hover:text-green-400"
                }`}
            >
              <FilePlus className="w-3 h-3 shrink-0" />
              {action.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}


/** Build the first message with spec context for a new session */
function buildFirstMessage(
  text: string,
  pendingCreateType: CreateSpecType | null,
  selectedSpec: LoadedSpec | null,
): { message: string; setTargetSpecId?: string; clearCreateType?: boolean } | null {
  if (pendingCreateType === "page-spec") {
    if (selectedSpec && selectedSpec.kind === "page-spec") {
      return {
        message: buildSpecCreationWithMergeContext(
          { specId: selectedSpec.specId, config: selectedSpec.config, appName: selectedSpec.appName },
          text,
        ),
        setTargetSpecId: selectedSpec.specId,
        clearCreateType: true,
      };
    }
    return {
      message: `${SPEC_CREATION_INSTRUCTIONS}

---

Create a comprehensive page spec for the following page. Read the source code first, then generate a complete JSON spec.

${text}`,
      clearCreateType: true,
    };
  }
  if (pendingCreateType === "architecture") {
    if (selectedSpec && selectedSpec.kind === "architecture") {
      return {
        message: buildSpecCreationWithMergeContext(
          { specId: selectedSpec.specId, config: selectedSpec.config as unknown as SpecConfig, appName: selectedSpec.appName },
          text,
        ),
        setTargetSpecId: selectedSpec.specId,
        clearCreateType: true,
      };
    }
    return {
      message: `${SPEC_CREATION_INSTRUCTIONS}

---

Create a comprehensive architecture spec for the following project. Read the source code first, then generate a complete JSON spec.

${text}`,
      clearCreateType: true,
    };
  }
  if (selectedSpec) {
    return { message: `${buildSpecContext(selectedSpec)}

---

${text}` };
  }
  return null;
}

// ============================================================================
// ChatHeader -- extracted sub-component for chat header bar
// ============================================================================

function ChatHeader({
  sessionState,
  hasSession,
  onNewSession,
}: {
  sessionState: string;
  hasSession: boolean;
  onNewSession: () => void;
}) {
  return (
    <div className="px-3 py-2 border-b border-border flex items-center gap-2">
      <h2 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground flex-1">
        AI Chat
      </h2>
      {hasSession && (
        <>
          <span
            className={`w-1.5 h-1.5 rounded-full ${
              sessionState === "ready"
                ? "bg-green-400"
                : sessionState === "processing"
                  ? "bg-amber-400 animate-pulse"
                  : "bg-muted-foreground/30"
            }`}
          />
          <button
            data-ui-id="spec-chat-new-session"
            onClick={onNewSession}
            className="text-[10px] text-muted-foreground/50 hover:text-muted-foreground transition-colors"
          >
            New
          </button>
        </>
      )}
    </div>
  );
}

// ============================================================================
// ChatInputArea -- extracted sub-component for chat input
// ============================================================================

function ChatInputArea({
  input,
  onInputChange,
  onKeyDown,
  onSend,
  onInterrupt,
  isProcessing,
  canSend,
  placeholder,
  textareaRef,
  onResize,
}: {
  input: string;
  onInputChange: (value: string) => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  onSend: () => void;
  onInterrupt: () => void;
  isProcessing: boolean;
  canSend: boolean;
  placeholder: string;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  onResize: () => void;
}) {
  return (
    <div className="px-3 py-2 border-t border-border">
      <div className="flex gap-1.5">
        <textarea
          data-ui-id="spec-chat-input"
          ref={textareaRef}
          value={input}
          onChange={(e) => onInputChange(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={placeholder}
          disabled={!canSend}
          rows={1}
          className="flex-1 min-h-[28px] px-2 py-1.5 text-xs leading-relaxed rounded resize-none
            bg-white/5 border border-white/10 text-foreground
            placeholder:text-muted-foreground/40
            focus:outline-hidden focus:border-purple-500/50
            disabled:opacity-40"
          style={{ overflow: "hidden" }}
          onInput={onResize}
        />
        <button
          data-ui-id="spec-chat-send"
          onClick={isProcessing ? onInterrupt : onSend}
          disabled={!isProcessing && (!input.trim() || !canSend)}
          className="shrink-0 w-7 h-7 flex items-center justify-center rounded
            bg-purple-500/10 text-purple-400 border border-purple-500/20
            hover:bg-purple-500/20 disabled:opacity-30 transition-colors"
        >
          {isProcessing ? <Square className="w-3 h-3" /> : <Send className="w-3 h-3" />}
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// SpecChatPanel
// ============================================================================

interface ChatPanelState {
  input: string;
  showCreateActions: boolean;
  pendingCreateType: CreateSpecType | null;
  targetSpecId: string | null;
}

type ChatPanelAction =
  | { type: "SET_INPUT"; value: string }
  | { type: "SET_SHOW_CREATE_ACTIONS"; value: boolean }
  | { type: "SET_PENDING_CREATE_TYPE"; value: CreateSpecType | null }
  | { type: "SET_TARGET_SPEC_ID"; value: string | null }
  | { type: "PREPARE_GENERATE"; targetSpecId: string }
  | { type: "RESET_SESSION" };

function chatPanelReducer(state: ChatPanelState, action: ChatPanelAction): ChatPanelState {
  switch (action.type) {
    case "SET_INPUT":
      return { ...state, input: action.value };
    case "SET_SHOW_CREATE_ACTIONS":
      return { ...state, showCreateActions: action.value };
    case "SET_PENDING_CREATE_TYPE":
      return { ...state, pendingCreateType: action.value };
    case "SET_TARGET_SPEC_ID":
      return { ...state, targetSpecId: action.value };
    case "PREPARE_GENERATE":
      return {
        ...state,
        targetSpecId: action.targetSpecId,
        showCreateActions: true,
        pendingCreateType: null,
        input: "",
      };
    case "RESET_SESSION":
      return {
        ...state,
        showCreateActions: false,
        pendingCreateType: null,
        targetSpecId: null,
      };
  }
}

const CHAT_PANEL_INITIAL: ChatPanelState = {
  input: "",
  showCreateActions: false,
  pendingCreateType: null,
  targetSpecId: null,
};

interface SpecChatPanelProps {
  selectedSpec: LoadedSpec | null;
  onAddSpec?: (spec: LoadedSpec) => void;
  onBuildWorkflow?: (spec: LoadedSpec) => void;
  /** Triggered when user clicks "Generate Spec" from unspecced page detail panel */
  generateSpecRequest?: {
    expectedSpecId: string;
    label: string;
    description: string;
  } | null;
  onGenerateSpecHandled?: () => void;
  /** Triggered when user clicks "Update Spec" in SpecTriageView */
  triageContext?: {
    specId: string;
    groupId: string;
  } | null;
  onTriageContextHandled?: () => void;
}

export function SpecChatPanel({
  selectedSpec,
  onAddSpec,
  onBuildWorkflow,
  generateSpecRequest,
  onGenerateSpecHandled,
  triageContext,
  onTriageContextHandled,
}: SpecChatPanelProps) {
  const session = useAiSession();
  const [chatState, chatDispatch] = useReducer(chatPanelReducer, CHAT_PANEL_INITIAL);
  const { input, showCreateActions, pendingCreateType, targetSpecId } = chatState;
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const resizeTextarea = useCallback(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    // 10 lines max: ~20px per line (12px font * 1.625 leading) + 12px padding
    el.style.height = Math.min(el.scrollHeight, 212) + "px";
    el.style.overflow = el.scrollHeight > 212 ? "auto" : "hidden";
  }, []);

  useEffect(() => {
    resizeTextarea();
  }, [input, resizeTextarea]);

  // Handle generate spec request from detail panel
  useEffect(() => {
    if (!generateSpecRequest) return;
    const { label, description, expectedSpecId } = generateSpecRequest;
    onGenerateSpecHandled?.();

    // Reset session and enter create mode
    if (session.taskRunId) {
      session.close();
      session.resetSession();
    }
    chatDispatch({ type: "PREPARE_GENERATE", targetSpecId: expectedSpecId });

    const pageInfo = `Page: ${label}\nDescription: ${description}\nSpec ID: ${expectedSpecId}`;
    const message = `${SPEC_CREATION_INSTRUCTIONS}\n\n---\n\nCreate a comprehensive page spec for the following page. Read the source code first, then generate a complete JSON spec.\n\n${pageInfo}`;

    // Create session and send in one go
    (async () => {
      const specLabel = label.length > 60 ? label.slice(0, 57) + "..." : label;
      const id = await session.createSession(`Spec Chat: ${specLabel}`);
      if (!id) return;
      await session.sendMessage(message);
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [generateSpecRequest]);

  // Handle triage "Update Spec" request — auto-sends a merge review message
  useEffect(() => {
    if (!triageContext || !selectedSpec || selectedSpec.kind !== "page-spec") return;
    const { groupId } = triageContext;
    onTriageContextHandled?.();

    const specConfig = selectedSpec.config as unknown as SpecConfig;
    const group = specConfig.groups?.find((g: { id: string }) => g.id === groupId);
    const groupName = group?.name || groupId;

    // Build triage-aware merge context
    const triageMessage = buildSpecCreationWithMergeContext(
      {
        specId: selectedSpec.specId,
        config: selectedSpec.config,
        appName: selectedSpec.appName,
      },
      `## Triage Context — Broken Assertions

The following assertions in group "${groupName}" were previously passing but now fail.

Read the current source code to determine if these failures are due to:
A) Intentional code changes (update the spec to match the new behavior)
B) Bugs (note them but update the spec if the new behavior is correct)

Update the spec accordingly, preserving assertion IDs where possible.`,
    );

    // Reset session and send
    if (session.taskRunId) {
      session.close();
      session.resetSession();
    }
    chatDispatch({ type: "PREPARE_GENERATE", targetSpecId: selectedSpec.specId });

    (async () => {
      const label = `Triage: ${groupName}`;
      const id = await session.createSession(`Spec Triage: ${label}`);
      if (!id) return;
      await session.sendMessage(triageMessage);
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [triageContext]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [session.messages, session.streamingContent]);

  const handleSend = useCallback(async () => {
    const text = input.trim();
    if (!text) return;
    chatDispatch({ type: "SET_INPUT", value: "" });

    // Create session if needed
    if (!session.taskRunId) {
      const rawLabel = selectedSpec
        ? selectedSpec.kind === "page-spec"
          ? selectedSpec.config.description || selectedSpec.specId
          : selectedSpec.specId
        : pendingCreateType
          ? "Spec Creation"
          : "Spec Review";
      const specLabel = rawLabel.length > 60 ? rawLabel.slice(0, 57) + "..." : rawLabel;
      const id = await session.createSession(`Spec Chat: ${specLabel}`);
      if (!id) return;

      // First message: include context
      if (pendingCreateType === "page-spec") {
        let message: string;
        if (selectedSpec && selectedSpec.kind === "page-spec") {
          // Merge mode: existing spec selected, use merge-aware creation
          message = buildSpecCreationWithMergeContext(
            {
              specId: selectedSpec.specId,
              config: selectedSpec.config,
              appName: selectedSpec.appName,
            },
            text,
          );
          chatDispatch({ type: "SET_TARGET_SPEC_ID", value: selectedSpec.specId });
        } else {
          // Fresh creation: no existing spec
          message = `${SPEC_CREATION_INSTRUCTIONS}\n\n---\n\nCreate a comprehensive page spec for the following page. Read the source code first, then generate a complete JSON spec.\n\n${text}`;
        }
        chatDispatch({ type: "SET_PENDING_CREATE_TYPE", value: null });
        await session.sendMessage(message);
      } else if (pendingCreateType === "architecture") {
        let message: string;
        if (selectedSpec && selectedSpec.kind === "architecture") {
          // Merge mode: existing architecture spec selected
          message = buildSpecCreationWithMergeContext(
            {
              specId: selectedSpec.specId,
              config: selectedSpec.config as unknown as SpecConfig,
              appName: selectedSpec.appName,
            },
            text,
          );
          chatDispatch({ type: "SET_TARGET_SPEC_ID", value: selectedSpec.specId });
        } else {
          // Fresh creation
          message = `${SPEC_CREATION_INSTRUCTIONS}\n\n---\n\nCreate a comprehensive architecture spec for the following project. Read the source code first, then generate a complete JSON spec.\n\n${text}`;
        }
        chatDispatch({ type: "SET_PENDING_CREATE_TYPE", value: null });
        await session.sendMessage(message);
      } else if (selectedSpec) {
        const context = buildSpecContext(selectedSpec);
        await session.sendMessage(`${context}\n\n---\n\n${text}`);
      } else {
        await session.sendMessage(text);
      }
      return;
    }

    await session.sendMessage(text);
  }, [input, session, selectedSpec, pendingCreateType]);

  const handleQuickAction = useCallback((prompt: string) => {
    chatDispatch({ type: "SET_INPUT", value: prompt });
    textareaRef.current?.focus();
  }, []);

  // Review actions send the full spec context + review instructions directly
  const handleReviewAction = useCallback(
    async (reviewType: ReviewType) => {
      if (!selectedSpec || selectedSpec.kind !== "page-spec") return;

      const reviewPrompt = buildSpecReviewPrompt(
        { specId: selectedSpec.specId, config: selectedSpec.config, appName: selectedSpec.appName },
        reviewType,
      );

      // Create session if needed
      if (!session.taskRunId) {
        const rawLabel = selectedSpec.config.description || selectedSpec.specId;
        const specLabel = rawLabel.length > 60 ? rawLabel.slice(0, 57) + "..." : rawLabel;
        const id = await session.createSession(`Spec Review: ${specLabel}`);
        if (!id) return;
      }

      await session.sendMessage(reviewPrompt);
    },
    [selectedSpec, session],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  // Build workflow from selected spec — delegates to parent (SpecsPage) which saves via API
  const handleBuildWorkflow = useCallback(() => {
    if (!selectedSpec || !onBuildWorkflow) return;
    onBuildWorkflow(selectedSpec);
  }, [selectedSpec, onBuildWorkflow]);

  const handleRegenerateSpec = useCallback((specKind: "page-spec" | "architecture") => {
    if (!selectedSpec) return;
    chatDispatch({ type: "SET_PENDING_CREATE_TYPE", value: specKind });
    chatDispatch({ type: "SET_TARGET_SPEC_ID", value: selectedSpec.specId });
    chatDispatch({ type: "SET_SHOW_CREATE_ACTIONS", value: true });
    const val = specKind === "page-spec"
      ? `Page: ${selectedSpec.config.description || selectedSpec.specId}
Spec ID: ${selectedSpec.specId}`
      : `Project: ${(selectedSpec.config as unknown as { projectName?: string }).projectName || selectedSpec.specId}
Description: ${selectedSpec.config.description || ""}`;
    chatDispatch({ type: "SET_INPUT", value: val });
    textareaRef.current?.focus();
  }, [selectedSpec]);

  const handleSelectCreateType = useCallback((specType: CreateSpecType) => {
    chatDispatch({ type: "SET_PENDING_CREATE_TYPE", value: specType });
    if (pendingCreateType !== specType) chatDispatch({ type: "SET_INPUT", value: "" });
    textareaRef.current?.focus();
  }, [pendingCreateType]);

  const isProcessing = session.sessionState === "processing";
  const hasMessages = session.messages.length > 0;
  const canSend = selectedSpec || session.taskRunId || pendingCreateType;

  return (
    <div className="h-full flex flex-col">
      <ChatHeader
        sessionState={session.sessionState}
        hasSession={!!session.taskRunId}
        onNewSession={() => {
          session.close();
          session.resetSession();
          chatDispatch({ type: "RESET_SESSION" });
        }}
      />

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto px-3 py-2 space-y-3">
        {!hasMessages && !session.streamingContent && (
          <ChatEmptyState
            selectedSpec={selectedSpec}
            showCreateActions={showCreateActions}
            pendingCreateType={pendingCreateType}
            onToggleMode={(v) => chatDispatch({ type: "SET_SHOW_CREATE_ACTIONS", value: v })}
            onReviewAction={handleReviewAction}
            onQuickAction={handleQuickAction}
            onRegenerateSpec={handleRegenerateSpec}
            onBuildWorkflow={handleBuildWorkflow}
            onSelectCreateType={handleSelectCreateType}
          />
        )}

        <ChatMessageList
          messages={session.messages}
          streamingContent={session.streamingContent}
          toolActivity={session.toolActivity}
          targetSpecId={targetSpecId}
          onAddSpec={onAddSpec}
          messagesEndRef={messagesEndRef}
        />
      </div>

      <ChatInputArea
        input={input}
        onInputChange={(v) => chatDispatch({ type: "SET_INPUT", value: v })}
        onKeyDown={handleKeyDown}
        onSend={handleSend}
        onInterrupt={session.interrupt}
        isProcessing={isProcessing}
        canSend={!!canSend}
        placeholder={
          selectedSpec
            ? "Ask about this spec..."
            : pendingCreateType
              ? CREATE_ACTIONS.find((a) => a.specType === pendingCreateType)?.placeholder ||
                "Describe the spec..."
              : showCreateActions
                ? "Select a spec type above..."
                : "Select a spec first..."
        }
        textareaRef={textareaRef}
        onResize={resizeTextarea}
      />
    </div>
  );
}
