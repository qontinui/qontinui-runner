import React, { useState } from "react";
import { Highlight, themes } from "prism-react-renderer";
import { cn } from "../../lib/utils";
import { Copy, Check, FileCode, ExternalLink } from "lucide-react";

/**
 * Extract text content from React children.
 */
export function extractTextContent(node: React.ReactNode): string {
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
 * Language mapping for syntax highlighting.
 */
export const LANGUAGE_MAP: Record<string, string> = {
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
 * File path pattern for detection.
 */
const FILE_PATH_PATTERN =
  /(?:^|\s|`)((?:[A-Za-z]:\\|\/)?(?:[\w.-]+\/)+[\w.-]+\.[\w]+)(?::(\d+))?(?=[\s,;:`]|$)/g;

/**
 * File path link component.
 */
export function FilePathLink({ path, line }: { path: string; line?: string }) {
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
export function processFilePaths(text: string): React.ReactNode[] {
  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  let match;

  FILE_PATH_PATTERN.lastIndex = 0;

  while ((match = FILE_PATH_PATTERN.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }

    const [fullMatch, filePath, lineNumber] = match;
    const startsWithSpace = fullMatch.startsWith(" ");
    if (startsWithSpace) {
      parts.push(" ");
    }

    parts.push(
      <FilePathLink key={`${filePath}-${match.index}`} path={filePath} line={lineNumber} />,
    );

    lastIndex = match.index + fullMatch.length;
  }

  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return parts.length > 0 ? parts : [text];
}

/**
 * Syntax-highlighted code block with copy button.
 */
export function SyntaxHighlightedCodeBlock({ code, language }: { code: string; language: string }) {
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

  const prismLanguage = LANGUAGE_MAP[language.toLowerCase()] || language.toLowerCase();

  return (
    <div className="relative group my-2">
      {language && (
        <div className="absolute top-0 left-0 px-2 py-0.5 text-[10px] font-mono text-muted-foreground bg-muted/80 rounded-tl-md rounded-br-md">
          {language}
        </div>
      )}
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
export function SimpleCodeBlock({ children }: { children: React.ReactNode }) {
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

export function MarkdownPre({ children }: { children?: React.ReactNode }) {
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

export function MarkdownCode({
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

export function MarkdownH1({ children }: { children?: React.ReactNode }) {
  return (
    <h1 className="text-lg font-bold text-foreground mt-4 mb-2 pb-1 border-b border-border">
      {children}
    </h1>
  );
}

export function MarkdownH2({ children }: { children?: React.ReactNode }) {
  return (
    <h2 className="text-base font-semibold text-foreground mt-4 mb-2 pb-1 border-b border-border/50">
      {children}
    </h2>
  );
}

export function MarkdownH3({ children }: { children?: React.ReactNode }) {
  return (
    <h3 className="text-sm font-semibold text-foreground mt-3 mb-1.5 flex items-center gap-2">
      <span className="w-1 h-4 bg-primary/50 rounded-full" />
      {children}
    </h3>
  );
}

export function MarkdownH4({ children }: { children?: React.ReactNode }) {
  return (
    <h4 className="text-xs font-semibold text-muted-foreground mt-2 mb-1 uppercase tracking-wider">
      {children}
    </h4>
  );
}

export function MarkdownP({ children }: { children?: React.ReactNode }) {
  const processedChildren = React.Children.map(children, (child) => {
    if (typeof child === "string") {
      const parts = processFilePaths(child);
      return parts.length === 1 && typeof parts[0] === "string" ? parts[0] : <>{parts}</>;
    }
    return child;
  });

  return <p className="mb-2 last:mb-0 leading-relaxed">{processedChildren}</p>;
}

export function MarkdownUl({ children }: { children?: React.ReactNode }) {
  return <ul className="list-disc list-inside mb-2 space-y-1 pl-2">{children}</ul>;
}

export function MarkdownOl({ children }: { children?: React.ReactNode }) {
  return <ol className="list-decimal list-inside mb-2 space-y-1 pl-2">{children}</ol>;
}

export function MarkdownLi({ children }: { children?: React.ReactNode }) {
  return <li className="text-foreground/90">{children}</li>;
}

export function MarkdownBlockquote({ children }: { children?: React.ReactNode }) {
  return (
    <blockquote className="border-l-2 border-primary/50 pl-3 my-2 italic text-muted-foreground">
      {children}
    </blockquote>
  );
}

export function MarkdownA({ href, children }: { href?: string; children?: React.ReactNode }) {
  return (
    <a href={href} className="text-primary underline underline-offset-2 hover:text-primary/80">
      {children}
    </a>
  );
}

export function MarkdownHr() {
  return <hr className="my-4 border-border" />;
}

export function MarkdownTable({ children }: { children?: React.ReactNode }) {
  return (
    <div className="overflow-x-auto my-2">
      <table className="min-w-full divide-y divide-border border border-border">{children}</table>
    </div>
  );
}

export function MarkdownTh({ children }: { children?: React.ReactNode }) {
  return (
    <th className="px-3 py-2 bg-muted/50 text-left text-xs font-medium text-muted-foreground uppercase tracking-wider border-b border-border">
      {children}
    </th>
  );
}

export function MarkdownTd({ children }: { children?: React.ReactNode }) {
  return (
    <td className="px-3 py-2 text-sm whitespace-nowrap border-b border-border/50">{children}</td>
  );
}

/**
 * Full-size markdown component map (for MarkdownViewer and similar contexts).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const SHARED_MARKDOWN_COMPONENTS: Record<string, React.ComponentType<any>> = {
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

// Chat-specific heading sizes (slightly smaller for compact chat context)
function ChatH1({ children }: { children?: React.ReactNode }) {
  return (
    <h1 className="text-base font-bold text-foreground mt-3 mb-1.5 pb-1 border-b border-border">
      {children}
    </h1>
  );
}

function ChatH2({ children }: { children?: React.ReactNode }) {
  return <h2 className="text-sm font-semibold text-foreground mt-3 mb-1.5">{children}</h2>;
}

function ChatH3({ children }: { children?: React.ReactNode }) {
  return (
    <h3 className="text-sm font-semibold text-foreground mt-2 mb-1 flex items-center gap-2">
      <span className="w-1 h-3.5 bg-primary/50 rounded-full" />
      {children}
    </h3>
  );
}

function ChatH4({ children }: { children?: React.ReactNode }) {
  return <h4 className="text-xs font-semibold text-muted-foreground mt-2 mb-1">{children}</h4>;
}

/**
 * Chat-specific markdown component map (compact headings for chat bubbles).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const CHAT_MARKDOWN_COMPONENTS: Record<string, React.ComponentType<any>> = {
  h1: ChatH1,
  h2: ChatH2,
  h3: ChatH3,
  h4: ChatH4,
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
