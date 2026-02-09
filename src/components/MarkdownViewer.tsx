import React, { useState, useEffect, useMemo, useCallback } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import { Highlight, themes } from "prism-react-renderer";
import { cn } from "../lib/utils";
import { getCategoryById, getCategoryColorClasses } from "../services/FindingCategories";
import {
  AlertTriangle,
  Bug,
  CheckCircle,
  CheckSquare,
  ChevronDown,
  ChevronRight,
  Copy,
  Check,
  Database,
  FileText,
  Info,
  Shield,
  Settings,
  Activity,
  TestTube,
  Sparkles,
  Zap,
  FileCode,
  ExternalLink,
} from "lucide-react";

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
 * Extract text content from React children.
 */
function extractTextContent(node: React.ReactNode): string {
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (!node) return "";
  if (Array.isArray(node)) return node.map(extractTextContent).join("");
  if (React.isValidElement(node)) {
    const props = node.props as { children?: React.ReactNode };
    return extractTextContent(props.children);
  }
  return "";
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
 * File path pattern for detection.
 * Matches paths like: src/components/Foo.tsx, C:\Users\..., /home/user/..., etc.
 */
const FILE_PATH_PATTERN =
  /(?:^|\s|`)((?:[A-Za-z]:\\|\/)?(?:[\w.-]+\/)+[\w.-]+\.[\w]+)(?::(\d+))?(?=[\s,;:`]|$)/g;

/**
 * Language mapping for syntax highlighting.
 */
const LANGUAGE_MAP: Record<string, string> = {
  js: "javascript",
  ts: "typescript",
  tsx: "tsx",
  jsx: "jsx",
  py: "python",
  rb: "ruby",
  rs: "rust",
  go: "go",
  css: "css",
  scss: "scss",
  html: "markup",
  xml: "markup",
  json: "json",
  yaml: "yaml",
  yml: "yaml",
  md: "markdown",
  sql: "sql",
  sh: "bash",
  bash: "bash",
  shell: "bash",
  powershell: "powershell",
  ps1: "powershell",
};

/**
 * Syntax-highlighted code block with copy button.
 */
function SyntaxHighlightedCodeBlock({ code, language }: { code: string; language: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  };

  // Map language aliases
  const prismLanguage = LANGUAGE_MAP[language.toLowerCase()] || language.toLowerCase();

  return (
    <div className="relative group my-2">
      {/* Language label */}
      {language && (
        <div className="absolute top-0 left-0 px-2 py-0.5 text-[10px] font-mono text-muted-foreground bg-muted/80 rounded-tl-md rounded-br-md">
          {language}
        </div>
      )}
      {/* Copy button */}
      <button
        onClick={handleCopy}
        className="absolute top-1 right-1 p-1.5 rounded bg-muted/80 opacity-0 group-hover:opacity-100 transition-opacity hover:bg-muted z-10"
        title="Copy code"
      >
        {copied ? (
          <Check className="w-3.5 h-3.5 text-green-500" />
        ) : (
          <Copy className="w-3.5 h-3.5 text-muted-foreground" />
        )}
      </button>
      <Highlight
        theme={themes.oneDark}
        code={code.trim()}
        language={prismLanguage as Parameters<typeof Highlight>[0]["language"]}
      >
        {({ className, style, tokens, getLineProps, getTokenProps }) => (
          <pre
            className={cn(
              "p-3 pt-6 rounded-md overflow-x-auto border-l-2 border-primary/30 text-xs",
              className,
            )}
            style={{ ...style, backgroundColor: "hsl(var(--muted) / 0.5)" }}
          >
            {tokens.map((line, i) => (
              <div key={i} {...getLineProps({ line })} className="table-row">
                <span className="table-cell pr-4 text-muted-foreground/50 select-none text-right w-8">
                  {i + 1}
                </span>
                <span className="table-cell">
                  {line.map((token, key) => (
                    <span key={key} {...getTokenProps({ token })} />
                  ))}
                </span>
              </div>
            ))}
          </pre>
        )}
      </Highlight>
    </div>
  );
}

/**
 * Simple code block with copy button (for unknown languages).
 */
function SimpleCodeBlock({ children }: { children: React.ReactNode }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      const textContent = extractTextContent(children);
      await navigator.clipboard.writeText(textContent);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  };

  return (
    <div className="relative group my-2">
      <pre className="bg-muted/50 p-3 rounded-md overflow-x-auto border-l-2 border-border text-xs font-mono">
        {children}
      </pre>
      <button
        onClick={handleCopy}
        className="absolute top-1 right-1 p-1.5 rounded bg-muted/80 opacity-0 group-hover:opacity-100 transition-opacity hover:bg-muted"
        title="Copy code"
      >
        {copied ? (
          <Check className="w-3.5 h-3.5 text-green-500" />
        ) : (
          <Copy className="w-3.5 h-3.5 text-muted-foreground" />
        )}
      </button>
    </div>
  );
}

/**
 * Collapsible thinking section.
 * Uses local state for reliable toggle, backed by a module-level store
 * to persist expanded state across React re-mounts (e.g. when ReactMarkdown
 * rebuilds its component tree on parent re-renders).
 */
function ThinkingSection({
  children,
  contentId,
}: {
  children: React.ReactNode;
  contentId?: string;
}) {
  // Generate a stable ID from content if not provided
  const sectionId = contentId || hashContent(extractTextContent(children));

  // Local state, initialized from the module-level store
  const [isExpanded, setIsExpanded] = useState(() => expandedSectionsStore.has(sectionId));

  // Sync from store when sectionId changes (handles re-mount with same content)
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
 * File path link component.
 */
function FilePathLink({ path, line }: { path: string; line?: string }) {
  return (
    <span className="inline-flex items-center gap-1 px-1.5 py-0.5 mx-0.5 bg-muted/70 rounded text-[11px] font-mono text-primary/90 hover:bg-muted hover:text-primary transition-colors cursor-pointer group">
      <FileCode className="w-3 h-3 opacity-60" />
      <span>{path}</span>
      {line && <span className="text-muted-foreground">:{line}</span>}
      <ExternalLink className="w-2.5 h-2.5 opacity-0 group-hover:opacity-60 transition-opacity" />
    </span>
  );
}

/**
 * Process text to highlight file paths.
 */
function processFilePaths(text: string): React.ReactNode[] {
  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  let match;

  // Reset regex
  FILE_PATH_PATTERN.lastIndex = 0;

  while ((match = FILE_PATH_PATTERN.exec(text)) !== null) {
    // Add text before the match
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }

    const [fullMatch, filePath, lineNumber] = match;
    // Check if it starts with a space (to preserve it)
    const startsWithSpace = fullMatch.startsWith(" ");
    if (startsWithSpace) {
      parts.push(" ");
    }

    parts.push(
      <FilePathLink key={`${filePath}-${match.index}`} path={filePath} line={lineNumber} />,
    );

    lastIndex = match.index + fullMatch.length;
  }

  // Add remaining text
  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return parts.length > 0 ? parts : [text];
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

    // Check if this line starts a thinking section
    if (!inThinkingBlock && isThinkingText(trimmedLine) && trimmedLine.length > 20) {
      inThinkingBlock = true;
      thinkingLines = [line];
    } else if (inThinkingBlock) {
      // Continue thinking block until we hit an empty line, header, or code block
      if (
        trimmedLine === "" ||
        trimmedLine.startsWith("#") ||
        trimmedLine.startsWith("```") ||
        trimmedLine.startsWith("[FINDING") ||
        trimmedLine.startsWith("- ") ||
        trimmedLine.match(/^\d+\.\s/)
      ) {
        // End thinking block
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

  // Don't forget remaining thinking lines
  if (thinkingLines.length > 0) {
    const contentHash = hashContent(thinkingLines.join("\n"));
    result.push(
      `<div data-thinking-section="true" data-content-id="${contentHash}">\n\n${thinkingLines.join("\n")}\n\n</div>`,
    );
  }

  return result.join("\n");
}

/**
 * Stable ReactMarkdown component overrides.
 * Defined at module level so references never change between renders.
 * This prevents ReactMarkdown from tearing down and rebuilding its DOM tree
 * on every parent re-render, which would break interactive elements like
 * ThinkingSection (the DOM node swap between mousedown/mouseup kills click events).
 */
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

function MarkdownH1({ children }: { children?: React.ReactNode }) {
  return (
    <h1 className="text-lg font-bold text-foreground mt-4 mb-2 pb-1 border-b border-border">
      {children}
    </h1>
  );
}

function MarkdownH2({ children }: { children?: React.ReactNode }) {
  return (
    <h2 className="text-base font-semibold text-foreground mt-4 mb-2 pb-1 border-b border-border/50">
      {children}
    </h2>
  );
}

function MarkdownH3({ children }: { children?: React.ReactNode }) {
  return (
    <h3 className="text-sm font-semibold text-foreground mt-3 mb-1.5 flex items-center gap-2">
      <span className="w-1 h-4 bg-primary/50 rounded-full" />
      {children}
    </h3>
  );
}

function MarkdownH4({ children }: { children?: React.ReactNode }) {
  return (
    <h4 className="text-xs font-semibold text-muted-foreground mt-2 mb-1 uppercase tracking-wider">
      {children}
    </h4>
  );
}

function MarkdownPre({ children }: { children?: React.ReactNode }) {
  const codeElement = React.Children.toArray(children).find(
    (child) => React.isValidElement(child) && child.type === "code",
  ) as React.ReactElement | undefined;

  if (codeElement) {
    const codeProps = codeElement.props as {
      className?: string;
      children?: React.ReactNode;
    };
    const match = /language-(\w+)/.exec(codeProps.className || "");
    const language = match ? match[1] : "";
    const code = extractTextContent(codeProps.children);

    if (language) {
      return <SyntaxHighlightedCodeBlock code={code} language={language} />;
    }
  }

  return <SimpleCodeBlock>{children}</SimpleCodeBlock>;
}

function MarkdownCode({
  className,
  children,
  ...props
}: {
  className?: string;
  children?: React.ReactNode;
}) {
  const match = /language-(\w+)/.exec(className || "");
  const isInline = !match && typeof children === "string" && !children.includes("\n");
  return isInline ? (
    <code
      className="bg-muted/70 px-1.5 py-0.5 rounded font-mono text-[11px] text-foreground"
      {...props}
    >
      {children}
    </code>
  ) : (
    <code className={cn("bg-transparent font-mono text-xs", className)} {...props}>
      {children}
    </code>
  );
}

function MarkdownP({ children }: { children?: React.ReactNode }) {
  const processedChildren = React.Children.map(children, (child) => {
    if (typeof child === "string") {
      const parts = processFilePaths(child);
      return parts.length === 1 && typeof parts[0] === "string" ? parts[0] : <>{parts}</>;
    }
    return child;
  });

  return <p className="mb-2 last:mb-0 leading-relaxed">{processedChildren}</p>;
}

function MarkdownUl({ children }: { children?: React.ReactNode }) {
  return <ul className="list-disc list-inside mb-2 space-y-1 pl-2">{children}</ul>;
}

function MarkdownOl({ children }: { children?: React.ReactNode }) {
  return <ol className="list-decimal list-inside mb-2 space-y-1 pl-2">{children}</ol>;
}

function MarkdownLi({ children }: { children?: React.ReactNode }) {
  return <li className="text-foreground/90">{children}</li>;
}

function MarkdownBlockquote({ children }: { children?: React.ReactNode }) {
  return (
    <blockquote className="border-l-2 border-primary/50 pl-3 my-2 italic text-muted-foreground">
      {children}
    </blockquote>
  );
}

function MarkdownA({ href, children }: { href?: string; children?: React.ReactNode }) {
  return (
    <a href={href} className="text-primary underline underline-offset-2 hover:text-primary/80">
      {children}
    </a>
  );
}

function MarkdownHr() {
  return <hr className="my-4 border-border" />;
}

function MarkdownTable({ children }: { children?: React.ReactNode }) {
  return (
    <div className="overflow-x-auto my-2">
      <table className="min-w-full divide-y divide-border border border-border">{children}</table>
    </div>
  );
}

function MarkdownTh({ children }: { children?: React.ReactNode }) {
  return (
    <th className="px-3 py-2 bg-muted/50 text-left text-xs font-medium text-muted-foreground uppercase tracking-wider border-b border-border">
      {children}
    </th>
  );
}

function MarkdownTd({ children }: { children?: React.ReactNode }) {
  return (
    <td className="px-3 py-2 text-sm whitespace-nowrap border-b border-border/50">{children}</td>
  );
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const MARKDOWN_COMPONENTS: Record<string, React.ComponentType<any>> = {
  div: MarkdownDiv,
  h1: MarkdownH1,
  h2: MarkdownH2,
  h3: MarkdownH3,
  h4: MarkdownH4,
  pre: MarkdownPre,
  code: MarkdownCode,
  p: MarkdownP,
  ul: MarkdownUl,
  ol: MarkdownOl,
  li: MarkdownLi,
  blockquote: MarkdownBlockquote,
  a: MarkdownA,
  hr: MarkdownHr,
  table: MarkdownTable,
  th: MarkdownTh,
  td: MarkdownTd,
};

export function MarkdownViewer({ content, className, isAnimated = false }: MarkdownViewerProps) {
  // Pre-process content
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

    // Wrap thinking sections
    processed = preprocessThinkingSections(processed);

    return processed;
  }, [content]);

  return (
    <div
      className={cn(
        "text-xs bg-background p-3 overflow-x-auto prose prose-sm prose-neutral dark:prose-invert max-w-none prose-pre:bg-muted/50 prose-pre:p-0 prose-pre:m-0",
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
