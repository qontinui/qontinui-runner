import React, { useState, useEffect, useMemo, useCallback } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import { cn } from "../lib/utils";
import { getCategoryById, getCategoryColorClasses } from "../services/FindingCategories";
import {
  AlertTriangle,
  Bug,
  CheckCircle,
  CheckSquare,
  ChevronDown,
  ChevronRight,
  Database,
  FileText,
  Info,
  Shield,
  Settings,
  Activity,
  TestTube,
  Sparkles,
  Zap,
} from "lucide-react";
import { SHARED_MARKDOWN_COMPONENTS, extractTextContent } from "./markdown/shared-components";

// Map icon names to Lucide components (matching CategorySection.tsx)
const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
};

interface MarkdownViewerProps {
  content: string;
  className?: string;
  isAnimated?: boolean;
}

/**
 * Module-level store for expanded thinking sections.
 * Persists across re-renders to prevent collapse on parent updates.
 */
const expandedSectionsStore = new Set<string>();

/**
 * Generate a simple hash from content for stable identification.
 */
function hashContent(content: string): string {
  let hash = 0;
  for (let i = 0; i < Math.min(content.length, 200); i++) {
    const char = content.charCodeAt(i);
    hash = (hash << 5) - hash + char;
    hash = hash & hash; // Convert to 32-bit integer
  }
  return `thinking-${hash}`;
}

/**
 * Patterns that indicate "thinking" or analysis text that can be collapsed.
 */
const THINKING_PATTERNS = [
  /^(Let me |I'll |I will |Now I |Looking at |I can see |I understand |Wait,|Actually,|Hmm,)/i,
  /^(The issue is |The problem is |This means |This should |I think |I believe |I suspect )/i,
  /^(Let me check|Let me look|Let me verify|Let me examine|Let me investigate)/i,
  /^(Now let me |First, let me |Next, let me )/i,
];

/**
 * Check if text starts with a thinking pattern.
 */
function isThinkingText(text: string): boolean {
  return THINKING_PATTERNS.some((pattern) => pattern.test(text.trim()));
}

/**
 * Collapsible thinking section.
 */
function ThinkingSection({
  children,
  contentId,
}: {
  children: React.ReactNode;
  contentId?: string;
}) {
  const sectionId = contentId || hashContent(extractTextContent(children));

  const [isExpanded, setIsExpanded] = useState(() => expandedSectionsStore.has(sectionId));

  useEffect(() => {
    setIsExpanded(expandedSectionsStore.has(sectionId));
  }, [sectionId]);

  const toggleExpanded = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      setIsExpanded((prev) => {
        const next = !prev;
        if (next) {
          expandedSectionsStore.add(sectionId);
        } else {
          expandedSectionsStore.delete(sectionId);
        }
        return next;
      });
    },
    [sectionId],
  );

  return (
    <div className="my-2 border border-border/50 rounded-md overflow-hidden">
      <button
        type="button"
        onClick={toggleExpanded}
        onMouseDown={(e) => e.stopPropagation()}
        className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-muted-foreground bg-muted/30 hover:bg-muted/50 transition-colors cursor-pointer select-none"
      >
        {isExpanded ? (
          <ChevronDown className="w-3.5 h-3.5" />
        ) : (
          <ChevronRight className="w-3.5 h-3.5" />
        )}
        <span className="font-medium">Analysis / Thinking</span>
        {!isExpanded && (
          <span className="ml-auto text-muted-foreground/60 text-[10px]">Click to expand</span>
        )}
      </button>
      {isExpanded && (
        <div className="px-3 py-2 text-xs text-muted-foreground bg-muted/10">{children}</div>
      )}
    </div>
  );
}

/**
 * Format finding body content with proper line breaks before field markers.
 */
function formatFindingBody(body: string): string {
  return body
    .replace(/(?<!\n)\s*(Description:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(File:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(Line:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(Resolution:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(Question:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(Options:)/g, "\n\n$1")
    .replace(/(?<!\n)\s*(Context:)/g, "\n\n$1");
}

/**
 * Pre-process content to wrap thinking sections.
 */
function preprocessThinkingSections(content: string): string {
  const lines = content.split("\n");
  const result: string[] = [];
  let inThinkingBlock = false;
  let thinkingLines: string[] = [];

  for (const line of lines) {
    const trimmedLine = line.trim();

    if (!inThinkingBlock && isThinkingText(trimmedLine) && trimmedLine.length > 20) {
      inThinkingBlock = true;
      thinkingLines = [line];
    } else if (inThinkingBlock) {
      if (
        trimmedLine === "" ||
        trimmedLine.startsWith("#") ||
        trimmedLine.startsWith("```") ||
        trimmedLine.startsWith("[FINDING") ||
        trimmedLine.startsWith("- ") ||
        trimmedLine.match(/^\d+\.\s/)
      ) {
        if (thinkingLines.length > 0) {
          const contentHash = hashContent(thinkingLines.join("\n"));
          result.push(
            `<div data-thinking-section="true" data-content-id="${contentHash}">\n\n${thinkingLines.join("\n")}\n\n</div>`,
          );
          thinkingLines = [];
        }
        inThinkingBlock = false;
        result.push(line);
      } else {
        thinkingLines.push(line);
      }
    } else {
      result.push(line);
    }
  }

  if (thinkingLines.length > 0) {
    const contentHash = hashContent(thinkingLines.join("\n"));
    result.push(
      `<div data-thinking-section="true" data-content-id="${contentHash}">\n\n${thinkingLines.join("\n")}\n\n</div>`,
    );
  }

  return result.join("\n");
}

/**
 * Try to parse a string as JSON. Returns pretty-printed JSON or null if not valid.
 */
function tryPrettyJson(text: string): string | null {
  const trimmed = text.trim();
  if (
    trimmed.length < 40 ||
    (trimmed[0] !== "{" && trimmed[0] !== "[")
  ) {
    return null;
  }
  try {
    const parsed = JSON.parse(trimmed);
    return JSON.stringify(parsed, null, 2);
  } catch {
    return null;
  }
}

/**
 * Pre-process content to detect raw JSON blobs (outside of code fences)
 * and wrap them in ```json code blocks for proper formatting.
 */
function preprocessRawJson(content: string): string {
  const lines = content.split("\n");
  const result: string[] = [];
  let inCodeFence = false;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();

    // Track code fence boundaries
    if (trimmed.startsWith("```")) {
      inCodeFence = !inCodeFence;
      result.push(lines[i]);
      continue;
    }

    if (inCodeFence) {
      result.push(lines[i]);
      continue;
    }

    // Try to pretty-print raw JSON lines
    const pretty = tryPrettyJson(trimmed);
    if (pretty) {
      result.push("```json", pretty, "```");
    } else {
      result.push(lines[i]);
    }
  }

  return result.join("\n");
}

const REMARK_PLUGINS = [remarkGfm];
const REHYPE_PLUGINS = [rehypeRaw];

function MarkdownDiv({ className, ...props }: Record<string, unknown>) {
  // Check for thinking section
  if (props["data-thinking-section"]) {
    return (
      <ThinkingSection contentId={props["data-content-id"] as string}>
        {props.children as React.ReactNode}
      </ThinkingSection>
    );
  }

  // Check for finding data attributes
  const categoryId = props["data-finding-category"] as string | undefined;
  const severity = props["data-finding-severity"] as string | undefined;

  if (categoryId) {
    const category = getCategoryById(categoryId);
    const colorClasses = category
      ? getCategoryColorClasses(category.color)
      : getCategoryColorClasses("slate");

    const Icon = category ? iconMap[category.icon] || Info : Info;
    const categoryName = category ? category.name : categoryId;

    return (
      <div className={`my-4 rounded-lg border ${colorClasses.border} overflow-hidden bg-card`}>
        <div
          className={`flex items-center gap-2 px-3 py-2 ${colorClasses.bg} border-b ${colorClasses.border}`}
        >
          <Icon className={`w-4 h-4 ${colorClasses.text}`} />
          <span className={`font-semibold ${colorClasses.text}`}>{categoryName}</span>
          {severity && (
            <span className="ml-auto text-[10px] uppercase tracking-wider font-medium px-1.5 py-0.5 rounded-full bg-background/50 opacity-70">
              {severity}
            </span>
          )}
        </div>
        <div className="p-3 bg-background/50">{props.children as React.ReactNode}</div>
      </div>
    );
  }

  const { children: divChildren, node: _node, ...restProps } = props;
  return (
    <div className={className as string} {...(restProps as React.HTMLAttributes<HTMLDivElement>)}>
      {divChildren as React.ReactNode}
    </div>
  );
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const MARKDOWN_COMPONENTS: Record<string, React.ComponentType<any>> = {
  ...SHARED_MARKDOWN_COMPONENTS,
  div: MarkdownDiv,
};

export function MarkdownViewer({ content, className, isAnimated = false }: MarkdownViewerProps) {
  const processedContent = useMemo(() => {
    let processed = content;

    // Transform [FINDING:...] blocks into HTML divs with data attributes
    processed = processed.replace(
      /\[FINDING:([\w_]+):(\w+)\]([\s\S]*?)\[\/FINDING\]/g,
      (match: string, category: string, severity: string, body: string) => {
        const formattedBody = formatFindingBody(body);
        return `<div data-finding-category="${category}" data-finding-severity="${severity}">\n\n${formattedBody}\n\n</div>`;
      },
    );

    // Detect raw JSON and wrap in code blocks
    processed = preprocessRawJson(processed);

    return processed;
  }, [content]);

  return (
    <div
      className={cn(
        "text-xs bg-background p-3 max-w-none [overflow-wrap:anywhere]",
        isAnimated && "animate-in fade-in-0 slide-in-from-top-1 duration-200",
        className,
      )}
    >
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        rehypePlugins={REHYPE_PLUGINS}
        components={MARKDOWN_COMPONENTS}
      >
        {processedContent}
      </ReactMarkdown>
    </div>
  );
}
