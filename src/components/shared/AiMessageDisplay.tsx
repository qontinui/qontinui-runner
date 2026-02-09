/**
 * AiMessageDisplay Component
 *
 * Shared component for displaying AI conversation messages.
 * Handles phase/source grouping and markdown rendering.
 * Used by both AiOutputTab and AiConversationWidget for consistent display.
 *
 * Features:
 * - Groups consecutive entries by source (prompt, claude, user_hint)
 * - Renders markdown content with MarkdownViewer
 * - Supports multiple display modes (log style vs chat bubbles)
 * - Shows findings summary when findings are detected
 * - Displays orchestrator agent messages with distinct styling
 */

import { useMemo } from "react";
import {
  Bot,
  BookOpen,
  CheckCircle,
  CheckSquare,
  Cog,
  FileText,
  Info,
  Lightbulb,
  MessageSquare,
  Play,
  User,
  XCircle,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { MarkdownViewer } from "../MarkdownViewer";
import { getAccentColors } from "@/design-system";

// ============================================================================
// Orchestrator Source Constants
// ============================================================================
// These match the source identifiers defined in orchestrator/output.rs

/** Source identifier for Planning Agent output */
export const SOURCE_PLANNING = "planning_agent";

/** Source identifier for Verification Agent output */
export const SOURCE_VERIFICATION = "verification_agent";

/** Source identifier for Knowledge Base output */
export const SOURCE_KNOWLEDGE = "knowledge_base";

/** Source identifier for Orchestrator system output */
export const SOURCE_ORCHESTRATOR = "orchestrator";

/** All orchestrator-related sources */
export const ORCHESTRATOR_SOURCES = [
  SOURCE_PLANNING,
  SOURCE_VERIFICATION,
  SOURCE_KNOWLEDGE,
  SOURCE_ORCHESTRATOR,
] as const;

/** Check if a source is an orchestrator agent */
export function isOrchestratorSource(source: string): boolean {
  return ORCHESTRATOR_SOURCES.includes(source as (typeof ORCHESTRATOR_SOURCES)[number]);
}

/**
 * Orchestrator agent type for display purposes.
 */
export type OrchestratorAgent =
  | "planning"
  | "verification"
  | "knowledge"
  | "orchestrator"
  | "worker"
  | null;

/**
 * Configuration for orchestrator agent display in the status bar.
 */
export interface OrchestratorAgentDisplayConfig {
  label: string;
  icon: "Lightbulb" | "CheckSquare" | "BookOpen" | "Cog" | "Bot";
  color: "blue" | "emerald" | "purple" | "amber" | "slate";
}

/**
 * Map orchestrator sources to agent types.
 */
export const ORCHESTRATOR_AGENT_MAP: Record<string, OrchestratorAgent> = {
  [SOURCE_PLANNING]: "planning",
  [SOURCE_VERIFICATION]: "verification",
  [SOURCE_KNOWLEDGE]: "knowledge",
  [SOURCE_ORCHESTRATOR]: "orchestrator",
};

/**
 * Display configuration for each orchestrator agent.
 */
export const ORCHESTRATOR_AGENT_DISPLAY: Record<
  NonNullable<OrchestratorAgent>,
  OrchestratorAgentDisplayConfig
> = {
  planning: { label: "Planning", icon: "Lightbulb", color: "blue" },
  verification: { label: "Verification", icon: "CheckSquare", color: "emerald" },
  knowledge: { label: "Knowledge Base", icon: "BookOpen", color: "purple" },
  orchestrator: { label: "Orchestrator", icon: "Cog", color: "amber" },
  worker: { label: "Worker", icon: "Bot", color: "slate" },
};

/**
 * Detect the current orchestrator agent from AI output entries.
 * Returns the most recently active agent based on the latest entries.
 *
 * @param entries AI output entries to analyze
 * @param lookbackMs How far back to look for agent activity (default 10 seconds)
 * @returns The current active agent or null if no orchestrator activity
 */
export function detectCurrentOrchestratorAgent(
  entries: Array<{ source: string; timestamp: number }>,
  lookbackMs: number = 10000,
): OrchestratorAgent {
  if (entries.length === 0) return null;

  const now = Date.now();
  const cutoff = now - lookbackMs;

  // Look at recent entries from newest to oldest
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];

    // Only consider recent entries
    if (entry.timestamp < cutoff) break;

    // Check if this is an orchestrator source
    if (isOrchestratorSource(entry.source)) {
      return ORCHESTRATOR_AGENT_MAP[entry.source] ?? null;
    }

    // If we see claude/response without orchestrator prefix, it's the worker
    if (entry.source === "claude" || entry.source === "response") {
      // Check if there's recent orchestrator activity - if not, it's the worker
      const hasRecentOrchestratorActivity = entries
        .slice(Math.max(0, i - 5), i)
        .some((e) => isOrchestratorSource(e.source));

      if (!hasRecentOrchestratorActivity) {
        return "worker";
      }
    }
  }

  return null;
}

/**
 * Entry type for AI messages.
 * Compatible with both AiOutputLine and AiOutputEntry.
 */
export interface AiMessageEntry {
  id: string;
  timestamp: number;
  line: string;
  source: string; // "prompt", "claude", "response", "user_hint", "user_message", or orchestrator sources
}

/**
 * A group of consecutive entries from the same source.
 */
export interface MessageGroup {
  source: string;
  entries: AiMessageEntry[];
  timestamp: number;
  combinedText: string;
}

/**
 * Display mode for the messages.
 */
export type DisplayMode = "log" | "chat";

/**
 * Props for the AiMessageDisplay component.
 */
export interface AiMessageDisplayProps {
  /** The grouped messages to display */
  groups: MessageGroup[];
  /** Display mode: "log" (compact) or "chat" (bubbles) */
  mode?: DisplayMode;
  /** Whether to animate the markdown (typing effect) */
  isAnimated?: boolean;
  /** Additional class name */
  className?: string;
}

/**
 * Props for individual message rendering.
 */
interface MessageBlockProps {
  group: MessageGroup;
  mode: DisplayMode;
  isAnimated?: boolean;
  index: number;
}

/**
 * Groups consecutive entries by their source.
 * This is the core grouping logic used by both AiOutputTab and AiConversationWidget.
 */
export function groupEntriesBySource<T extends AiMessageEntry>(entries: T[]): MessageGroup[] {
  if (entries.length === 0) return [];

  const groups: MessageGroup[] = [];
  let currentGroup: MessageGroup | null = null;

  for (const entry of entries) {
    // Normalize source: "response" is treated as "claude"
    const normalizedSource = entry.source === "response" ? "claude" : entry.source;

    if (!currentGroup || currentGroup.source !== normalizedSource) {
      // Finalize current group
      if (currentGroup) {
        groups.push(currentGroup);
      }
      // Start new group
      currentGroup = {
        source: normalizedSource,
        entries: [entry],
        timestamp: entry.timestamp,
        combinedText: entry.line,
      };
    } else {
      // Add to existing group
      currentGroup.entries.push(entry);
      currentGroup.combinedText += "\n" + entry.line;
    }
  }

  // Don't forget the last group
  if (currentGroup) {
    groups.push(currentGroup);
  }

  return groups;
}

/**
 * Format timestamp for display.
 */
function formatTimestamp(timestamp: number): string {
  const date = new Date(timestamp);
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/**
 * Extract findings from message content.
 */
function extractFindings(
  content: string,
): Array<{ category: string; severity: string; title?: string }> {
  const findings: Array<{ category: string; severity: string; title?: string }> = [];
  const regex = /\[FINDING:([\w_]+):(\w+)\]([\s\S]*?)\[\/FINDING\]/g;
  let match;

  while ((match = regex.exec(content)) !== null) {
    const [, category, severity, body] = match;
    // Try to extract title from body
    const titleMatch = body.match(/Title:\s*(.+?)(?:\n|$)/);
    findings.push({
      category,
      severity,
      title: titleMatch?.[1]?.trim(),
    });
  }

  return findings;
}

/**
 * Findings summary banner.
 */
function FindingsSummary({
  findings,
}: {
  findings: Array<{ category: string; severity: string; title?: string }>;
}) {
  if (findings.length === 0) return null;

  const amberColors = getAccentColors("amber");
  const severityCounts = findings.reduce(
    (acc, f) => {
      acc[f.severity] = (acc[f.severity] || 0) + 1;
      return acc;
    },
    {} as Record<string, number>,
  );

  return (
    <div
      className={cn(
        "rounded-lg p-2 mb-3 flex items-center gap-2 text-xs",
        amberColors.bg,
        "border",
        amberColors.border,
      )}
    >
      <FileText className={cn("w-4 h-4", amberColors.text)} />
      <span className={cn("font-medium", amberColors.text)}>
        {findings.length} finding{findings.length > 1 ? "s" : ""} detected
      </span>
      <span className="text-muted-foreground">
        (
        {Object.entries(severityCounts)
          .map(([sev, count]) => `${count} ${sev}`)
          .join(", ")}
        )
      </span>
    </div>
  );
}

/**
 * Renders a prompt message in log mode.
 */
function PromptLogBlock({ group, isAnimated }: { group: MessageGroup; isAnimated?: boolean }) {
  const blueColors = getAccentColors("blue");

  return (
    <div className={cn("rounded-lg p-3 mb-3", blueColors.bg, "border", blueColors.border)}>
      <div className={cn("flex items-center gap-2 text-xs font-semibold mb-2", blueColors.text)}>
        <MessageSquare className="w-4 h-4" />
        <span>PROMPT</span>
        <span className="text-muted-foreground font-normal ml-auto">
          {formatTimestamp(group.timestamp)}
        </span>
      </div>
      <MarkdownViewer content={group.combinedText} isAnimated={isAnimated} />
    </div>
  );
}

/**
 * Renders a user message in log mode.
 * Visually distinct from prompts with a "YOU" label and User icon.
 */
function UserMessageLogBlock({ group, isAnimated }: { group: MessageGroup; isAnimated?: boolean }) {
  const blueColors = getAccentColors("blue");

  return (
    <div className={cn("rounded-lg p-3 mb-3", blueColors.bg, "border", blueColors.border)}>
      <div className={cn("flex items-center gap-2 text-xs font-semibold mb-2", blueColors.text)}>
        <User className="w-4 h-4" />
        <span>YOU</span>
        <span className="text-muted-foreground font-normal ml-auto">
          {formatTimestamp(group.timestamp)}
        </span>
      </div>
      <MarkdownViewer content={group.combinedText} isAnimated={isAnimated} />
    </div>
  );
}

/**
 * Renders a user hint message in log mode.
 */
function UserHintLogBlock({ group, isAnimated }: { group: MessageGroup; isAnimated?: boolean }) {
  const purpleColors = getAccentColors("purple");

  return (
    <div className={cn("rounded-lg p-3 mb-3", purpleColors.bg, "border", purpleColors.border)}>
      <div className={cn("flex items-center gap-2 text-xs font-semibold mb-2", purpleColors.text)}>
        <MessageSquare className="w-4 h-4" />
        <span>USER HINT</span>
        <span className="text-muted-foreground font-normal ml-auto">
          {formatTimestamp(group.timestamp)}
        </span>
      </div>
      <MarkdownViewer content={group.combinedText} isAnimated={isAnimated} />
    </div>
  );
}

// ============================================================================
// Orchestrator Agent Log Blocks
// ============================================================================

/**
 * Get styling configuration for an orchestrator source.
 */
function getOrchestratorAgentConfig(source: string) {
  switch (source) {
    case SOURCE_PLANNING:
      return {
        label: "PLANNING AGENT",
        Icon: Lightbulb,
        colors: getAccentColors("blue"),
        description: "Creating verification plan",
      };
    case SOURCE_VERIFICATION:
      return {
        label: "VERIFICATION AGENT",
        Icon: CheckSquare,
        colors: getAccentColors("emerald"),
        description: "Running checks",
      };
    case SOURCE_KNOWLEDGE:
      return {
        label: "KNOWLEDGE BASE",
        Icon: BookOpen,
        colors: getAccentColors("purple"),
        description: "Recording findings",
      };
    case SOURCE_ORCHESTRATOR:
      return {
        label: "ORCHESTRATOR",
        Icon: Cog,
        colors: getAccentColors("amber"),
        description: "Coordinating agents",
      };
    default:
      return {
        label: "AGENT",
        Icon: Bot,
        colors: getAccentColors("zinc"),
        description: "",
      };
  }
}

/**
 * Renders an orchestrator agent message in log mode.
 * Each agent type has distinct styling for easy identification.
 */
function OrchestratorLogBlock({ group }: { group: MessageGroup }) {
  const config = getOrchestratorAgentConfig(group.source);
  const { label, Icon, colors } = config;

  // Check if this is a pass/fail message for verification
  const isPass = group.combinedText.includes("[PASS]");
  const isFail = group.combinedText.includes("[FAIL]");

  // Use red styling for failed verifications
  const effectiveColors = isFail ? getAccentColors("red") : colors;

  // Format the content - orchestrator messages are typically single lines
  // with prefixes like [Planning Agent], [Verification Agent], etc.
  // We preserve the format but style it nicely
  const lines = group.combinedText.split("\n").filter((l) => l.trim());

  return (
    <div
      className={cn("rounded-lg px-3 py-2 mb-2 border", effectiveColors.bg, effectiveColors.border)}
    >
      <div
        className={cn("flex items-center gap-2 text-xs font-semibold mb-1", effectiveColors.text)}
      >
        <Icon className="w-4 h-4" />
        <span>{label}</span>
        {isPass && <CheckCircle className="w-3 h-3 text-green-500" />}
        {isFail && <XCircle className="w-3 h-3 text-red-500" />}
        <span className="text-muted-foreground font-normal ml-auto text-[10px]">
          {formatTimestamp(group.timestamp)}
        </span>
      </div>
      <div className="text-xs text-foreground/90 space-y-0.5">
        {lines.map((line, idx) => {
          // Strip the agent prefix since we show it in the header
          const cleanLine = line
            .replace(/^\[Planning Agent\]\s*/i, "")
            .replace(/^\[Verification Agent\]\s*/i, "")
            .replace(/^\[Knowledge Base\]\s*/i, "")
            .replace(/^\[Orchestrator\]\s*/i, "");

          // Style pass/fail indicators
          const hasPassIndicator = cleanLine.includes("[PASS]");
          const hasFailIndicator = cleanLine.includes("[FAIL]");
          const hasAiIndicator = cleanLine.includes("[AI]");
          const hasDeterministicIndicator = cleanLine.includes("[DETERMINISTIC]");
          const hasCriticalIndicator = cleanLine.includes("[CRITICAL]");

          return (
            <div
              key={idx}
              className={cn(
                "font-mono leading-relaxed",
                hasPassIndicator && "text-green-400",
                hasFailIndicator && "text-red-400",
                hasCriticalIndicator && "font-bold",
              )}
            >
              {/* Add subtle icon indicators */}
              {hasPassIndicator && <span className="mr-1">✓</span>}
              {hasFailIndicator && <span className="mr-1">✗</span>}
              {hasAiIndicator && <span className="mr-1 text-purple-400">🤖</span>}
              {hasDeterministicIndicator && <span className="mr-1 text-blue-400">⚡</span>}
              {cleanLine
                .replace("[PASS]", "")
                .replace("[FAIL]", "")
                .replace("[AI]", "")
                .replace("[DETERMINISTIC]", "")
                .replace("[CRITICAL]", "")
                .trim()}
              {hasCriticalIndicator && (
                <span className="ml-1 text-red-400 text-[10px]">CRITICAL</span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/**
 * Renders an AI response message in log mode.
 */
function ResponseLogBlock({
  group,
  isAnimated,
  isFirst,
}: {
  group: MessageGroup;
  isAnimated?: boolean;
  isFirst: boolean;
}) {
  const emeraldColors = getAccentColors("emerald");
  const lineCount = group.combinedText.split("\n").length;

  return (
    <div>
      {isFirst && (
        <div
          className={cn(
            "flex items-center gap-2 text-xs font-semibold mb-2 mt-1",
            emeraldColors.text,
          )}
        >
          <Bot className="w-4 h-4" />
          <span>RESPONSE</span>
          <span className="text-muted-foreground font-normal">{lineCount} lines</span>
        </div>
      )}
      <div className="py-0.5 pl-6">
        <MarkdownViewer content={group.combinedText} isAnimated={isAnimated} />
      </div>
    </div>
  );
}

/**
 * Status type detection from message content.
 */
type StatusType = "running" | "success" | "error" | "finding" | "info";

function detectStatusType(text: string): StatusType {
  const lowerText = text.toLowerCase();

  // Check for success indicators
  if (
    text.includes("✅") ||
    lowerText.includes("completed successfully") ||
    lowerText.includes("success")
  ) {
    return "success";
  }

  // Check for running indicators
  if (text.includes("🚀") || lowerText.includes("running") || lowerText.includes("starting")) {
    return "running";
  }

  // Check for error indicators
  if (text.includes("❌") || lowerText.includes("failed") || lowerText.includes("error")) {
    return "error";
  }

  // Check for finding indicators
  if (text.includes("📋") || lowerText.includes("finding detected")) {
    return "finding";
  }

  return "info";
}

/**
 * Get status banner styling based on status type.
 */
function getStatusStyles(type: StatusType) {
  switch (type) {
    case "success":
      return {
        colors: getAccentColors("green"),
        Icon: CheckCircle,
      };
    case "running":
      return {
        colors: getAccentColors("blue"),
        Icon: Play,
      };
    case "error":
      return {
        colors: getAccentColors("red"),
        Icon: XCircle,
      };
    case "finding":
      return {
        colors: getAccentColors("amber"),
        Icon: FileText,
      };
    default:
      return {
        colors: getAccentColors("zinc"),
        Icon: Info,
      };
  }
}

/**
 * Renders a status message as a banner.
 */
function StatusBanner({ group }: { group: MessageGroup }) {
  const statusType = detectStatusType(group.combinedText);
  const { colors, Icon } = getStatusStyles(statusType);

  // Remove emoji from the beginning of the text for cleaner display
  // Using Unicode property escapes to properly match emoji sequences
  const cleanText = group.combinedText.replace(/^(?:🚀|✅|❌|📋|⚠️|ℹ️)\s*/gmu, "").trim();

  return (
    <div
      className={cn(
        "flex items-center gap-2 px-3 py-2 my-2 rounded-md text-xs font-medium border",
        colors.bg,
        colors.border,
        colors.text,
      )}
    >
      <Icon className="w-4 h-4 flex-shrink-0" />
      <span className="flex-1">{cleanText}</span>
      <span className="text-muted-foreground/60 text-[10px] font-normal">
        {formatTimestamp(group.timestamp)}
      </span>
    </div>
  );
}

/**
 * Renders a finding notification as a compact banner.
 */
function FindingBanner({ group }: { group: MessageGroup }) {
  const amberColors = getAccentColors("amber");

  // Extract finding info from text like "📋 Finding detected: [code_bug:medium] Title here"
  const text = group.combinedText;
  const match = text.match(/\[([\w_]+):([\w]+)\]\s*(.+)?/);
  const category = match?.[1] || "finding";
  const severity = match?.[2] || "medium";
  const title = match?.[3]?.trim() || text.replace(/^📋\s*Finding detected:\s*/i, "");

  return (
    <div
      className={cn(
        "flex items-center gap-2 px-3 py-1.5 my-1 rounded-md text-xs border",
        amberColors.bg,
        amberColors.border,
      )}
    >
      <FileText className={cn("w-3.5 h-3.5 flex-shrink-0", amberColors.text)} />
      <span className={cn("font-medium", amberColors.text)}>{category}</span>
      <span className="text-muted-foreground/60 text-[10px] uppercase">{severity}</span>
      {title && <span className="text-muted-foreground truncate flex-1">{title}</span>}
    </div>
  );
}

/**
 * Renders a message bubble for chat mode.
 */
function ChatBubble({ group, isAnimated }: { group: MessageGroup; isAnimated?: boolean }) {
  const isUser = group.source === "prompt" || group.source === "user_message";
  const isHint = group.source === "user_hint";
  const blueColors = getAccentColors("blue");
  const purpleColors = getAccentColors("purple");
  const greenColors = getAccentColors("green");

  // Determine colors based on source
  const avatarColors = isUser || isHint ? (isHint ? purpleColors : blueColors) : greenColors;

  const bubbleColors = isUser
    ? { bg: blueColors.bg, border: blueColors.border }
    : isHint
      ? { bg: purpleColors.bg, border: purpleColors.border }
      : { bg: "bg-muted/50", border: "border-border" };

  const label = isHint ? "Hint" : isUser ? "You" : "AI";

  return (
    <div className={cn("flex gap-3 px-4 py-3", isUser ? "flex-row-reverse" : "flex-row")}>
      {/* Avatar */}
      <div
        className={cn(
          "flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
          avatarColors.bg,
          avatarColors.text,
        )}
      >
        {isUser || isHint ? <User className="w-4 h-4" /> : <Bot className="w-4 h-4" />}
      </div>

      {/* Message content */}
      <div
        className={cn(
          "flex-1 max-w-[80%] rounded-lg px-4 py-2 border",
          bubbleColors.bg,
          bubbleColors.border,
        )}
      >
        <div className="flex items-center gap-2 mb-1">
          <span className="text-xs font-medium text-muted-foreground">{label}</span>
          <span className="text-xs text-muted-foreground/60">
            {formatTimestamp(group.timestamp)}
          </span>
        </div>
        <MarkdownViewer content={group.combinedText} isAnimated={isAnimated} />
      </div>
    </div>
  );
}

/**
 * Renders a single message block based on mode.
 */
function MessageBlock({ group, mode, isAnimated, index }: MessageBlockProps) {
  // Status messages are always rendered as banners regardless of mode
  if (group.source === "status") {
    return <StatusBanner group={group} />;
  }

  // Finding notifications are rendered as compact banners
  if (group.source === "finding") {
    return <FindingBanner group={group} />;
  }

  // Orchestrator agent messages have their own styling
  if (isOrchestratorSource(group.source)) {
    return <OrchestratorLogBlock group={group} />;
  }

  if (mode === "chat") {
    return <ChatBubble group={group} isAnimated={isAnimated} />;
  }

  // Log mode
  switch (group.source) {
    case "prompt":
      return <PromptLogBlock group={group} isAnimated={isAnimated} />;
    case "user_message":
      return <UserMessageLogBlock group={group} isAnimated={isAnimated} />;
    case "user_hint":
      return <UserHintLogBlock group={group} isAnimated={isAnimated} />;
    case "claude":
    case "response":
      return <ResponseLogBlock group={group} isAnimated={isAnimated} isFirst={index === 0} />;
    default:
      // Unknown source - render as response
      return <ResponseLogBlock group={group} isAnimated={isAnimated} isFirst={false} />;
  }
}

/**
 * AiMessageDisplay component.
 * Renders grouped AI messages with consistent styling.
 */
export function AiMessageDisplay({
  groups,
  mode = "log",
  isAnimated = false,
  className,
}: AiMessageDisplayProps) {
  // Extract all findings from response groups
  const allFindings = useMemo(() => {
    const findings: Array<{ category: string; severity: string; title?: string }> = [];
    for (const group of groups) {
      if (group.source === "claude" || group.source === "response") {
        findings.push(...extractFindings(group.combinedText));
      }
    }
    return findings;
  }, [groups]);

  if (groups.length === 0) {
    return null;
  }

  // Track if we've seen a response yet (for "first response" header)
  let responseIndex = 0;

  return (
    <div className={cn("flex flex-col", className)}>
      {/* Show findings summary at the top if there are any */}
      {mode === "log" && allFindings.length > 0 && <FindingsSummary findings={allFindings} />}

      {groups.map((group, index) => {
        const isResponse = group.source === "claude" || group.source === "response";
        const effectiveIndex = isResponse ? responseIndex++ : 0;

        return (
          <MessageBlock
            key={`${group.timestamp}-${index}`}
            group={group}
            mode={mode}
            isAnimated={isAnimated}
            index={effectiveIndex}
          />
        );
      })}
    </div>
  );
}

export default AiMessageDisplay;
