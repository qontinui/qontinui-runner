/**
 * ProjectExplainerPage — browse generated explainer markdown for a project.
 *
 * Layout: three panes.
 *  - Left  (fixed): project path input + breadcrumb + history.
 *  - Center:         rendered markdown with inline Mermaid for ```mermaid blocks.
 *                    Internal relative links (./other.md) navigate within the explainer.
 *  - Right  (fixed): AI chat panel with free-form knowledge; the current file's
 *                    rendered text is injected into the first prompt of each
 *                    new question as grounding context.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, ArrowLeft, Home, Loader2, Send, X } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import mermaid from "mermaid";
import { getApiBase } from "@/lib/runner-api";
import { useAiSession } from "@/hooks/useAiSession";
import { useUIElement } from "@qontinui/ui-bridge";

const EXPLAINER_ROOT = "src/specs/explainer";

// Global, one-time Mermaid init. Theme matches the runner's dark UI.
let mermaidInitialized = false;
function ensureMermaidInitialized() {
  if (mermaidInitialized) return;
  mermaid.initialize({
    startOnLoad: false,
    theme: "dark",
    securityLevel: "strict",
    flowchart: { htmlLabels: true, curve: "basis" },
  });
  mermaidInitialized = true;
}

function MermaidDiagram({ chart }: { chart: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    ensureMermaidInitialized();
    let cancelled = false;
    const id = `mermaid-${Math.random().toString(36).slice(2)}`;
    mermaid
      .render(id, chart)
      .then(({ svg }) => {
        if (cancelled || !ref.current) return;
        ref.current.innerHTML = svg;
        setError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [chart]);
  if (error) {
    return (
      <pre className="text-xs text-destructive bg-destructive/10 border border-destructive/30 rounded p-3 overflow-x-auto">
        Mermaid render error: {error}
        {"\n\n"}
        {chart}
      </pre>
    );
  }
  return <div ref={ref} className="my-4 flex justify-center overflow-x-auto" />;
}

interface FileCache {
  [relPath: string]: string;
}

export function ProjectExplainerPage() {
  const [projectPath, setProjectPath] = useState("");
  const [currentFile, setCurrentFile] = useState<string>("index.md");
  const [markdown, setMarkdown] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [history, setHistory] = useState<string[]>([]);
  const cacheRef = useRef<FileCache>({});

  useUIElement({ id: "explainer-project-path", type: "input", label: "Project path" });
  const { ref: homeBtnRef } = useUIElement({
    id: "explainer-btn-home",
    type: "button",
    label: "Return to index",
    actions: ["click"],
  });
  const { ref: backBtnRef } = useUIElement({
    id: "explainer-btn-back",
    type: "button",
    label: "Back",
    actions: ["click"],
  });

  const loadFile = useCallback(
    async (relPath: string, pushHistory = true) => {
      if (!projectPath.trim()) return;
      if (cacheRef.current[relPath]) {
        if (pushHistory) setHistory((h) => [...h, currentFile]);
        setCurrentFile(relPath);
        setMarkdown(cacheRef.current[relPath]);
        setLoadError(null);
        return;
      }
      setLoading(true);
      setLoadError(null);
      try {
        const resp = await fetch(`${getApiBase()}/ui-bridge/integration/read-file`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            project_path: projectPath.trim(),
            file_path: `${EXPLAINER_ROOT}/${relPath}`,
          }),
        });
        const data = (await resp.json()) as { success: boolean; data?: string; error?: string };
        if (!data.success || data.data === undefined) {
          setLoadError(data.error || `Could not load ${relPath}`);
          setMarkdown("");
          return;
        }
        cacheRef.current[relPath] = data.data;
        if (pushHistory && currentFile !== relPath) setHistory((h) => [...h, currentFile]);
        setCurrentFile(relPath);
        setMarkdown(data.data);
      } catch (e) {
        setLoadError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [projectPath, currentFile],
  );

  const goHome = useCallback(() => {
    void loadFile("index.md", true);
  }, [loadFile]);

  const goBack = useCallback(() => {
    setHistory((h) => {
      if (h.length === 0) return h;
      const prev = h[h.length - 1];
      // Load without pushing history to avoid infinite back.
      void loadFile(prev, false);
      return h.slice(0, -1);
    });
  }, [loadFile]);

  // Resolve a relative markdown link target to a file path under EXPLAINER_ROOT.
  const resolveLink = useCallback(
    (href: string): string | null => {
      if (!href) return null;
      // External / anchor-only links: let the browser handle them (or ignore anchors).
      if (/^https?:\/\//i.test(href)) return null;
      if (href.startsWith("#")) return null;
      // Strip any leading ./
      let target = href.replace(/^\.\//, "");
      // If the link doesn't end in .md, assume it's a directory index.
      if (!target.endsWith(".md")) target = target.replace(/\/$/, "") + ".md";
      // Resolve relative to currentFile's directory.
      const currentDir = currentFile.includes("/")
        ? currentFile.slice(0, currentFile.lastIndexOf("/"))
        : "";
      // Handle ../ segments
      const parts = (currentDir ? currentDir.split("/") : []).concat(target.split("/"));
      const stack: string[] = [];
      for (const p of parts) {
        if (p === "" || p === ".") continue;
        if (p === "..") stack.pop();
        else stack.push(p);
      }
      return stack.join("/");
    },
    [currentFile],
  );

  const handleLinkClick = useCallback(
    (e: React.MouseEvent, href: string | undefined) => {
      if (!href) return;
      const resolved = resolveLink(href);
      if (!resolved) return; // external; default behavior
      e.preventDefault();
      void loadFile(resolved);
    },
    [resolveLink, loadFile],
  );

  // On project path set, load the index.
  useEffect(() => {
    if (!projectPath.trim()) return;
    cacheRef.current = {};
    setHistory([]);
    void loadFile("index.md", false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectPath]);

  const breadcrumb = useMemo(() => {
    const parts = currentFile.split("/");
    return parts.join(" › ");
  }, [currentFile]);

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 py-3 border-b border-border bg-card/40">
        <BookOpen className="w-5 h-5 text-cyan-400" />
        <h1 className="text-lg font-semibold">Explainer</h1>
        <input
          id="explainer-project-path"
          type="text"
          value={projectPath}
          onChange={(e) => setProjectPath(e.target.value)}
          placeholder="Project path (e.g. D:/qontinui-root/qontinui-runner)"
          className="flex-1 max-w-xl px-3 py-1 text-sm rounded border border-border bg-background
                     placeholder:text-muted-foreground focus:outline-hidden focus:border-cyan-500/50"
        />
        <button
          ref={backBtnRef}
          onClick={goBack}
          disabled={history.length === 0}
          className="p-1.5 rounded hover:bg-accent disabled:opacity-40"
          title="Back"
        >
          <ArrowLeft className="w-4 h-4" />
        </button>
        <button
          ref={homeBtnRef}
          onClick={goHome}
          disabled={!projectPath.trim()}
          className="p-1.5 rounded hover:bg-accent disabled:opacity-40"
          title="Back to index"
        >
          <Home className="w-4 h-4" />
        </button>
      </div>

      {/* Body: markdown pane | AI panel */}
      <div className="flex-1 flex min-h-0">
        {/* Markdown pane */}
        <div className="flex-1 overflow-y-auto px-6 py-4 border-r border-border">
          {projectPath.trim() === "" ? (
            <EmptyState />
          ) : loading ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="w-4 h-4 animate-spin" /> Loading {currentFile}...
            </div>
          ) : loadError ? (
            <LoadErrorView
              path={currentFile}
              message={loadError}
              onRetry={() => loadFile(currentFile, false)}
            />
          ) : (
            <>
              <div className="text-[11px] text-muted-foreground mb-2 font-mono">{breadcrumb}</div>
              <ExplainerMarkdown markdown={markdown} onLinkClick={handleLinkClick} />
            </>
          )}
        </div>

        {/* AI side panel */}
        <ExplainerAiPanel getContext={() => markdown} />
      </div>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="text-sm text-muted-foreground max-w-xl">
      <p className="mb-2">
        Enter a project path above to browse its generated explainer. The explainer lives under{" "}
        <code className="text-xs bg-muted/40 px-1 py-0.5 rounded">{EXPLAINER_ROOT}/</code> in that
        project — produced by enabling <b>Project Explainer</b> on the UI Bridge Integration page.
      </p>
      <p>
        The viewer starts at <code>index.md</code> and follows markdown links from there.
      </p>
    </div>
  );
}

function LoadErrorView({
  path,
  message,
  onRetry,
}: {
  path: string;
  message: string;
  onRetry: () => void;
}) {
  const isIndexMissing = path === "index.md";
  return (
    <div className="max-w-xl text-sm">
      <div className="rounded-md border border-destructive/30 bg-destructive/10 p-3 mb-3 text-destructive">
        {message}
      </div>
      {isIndexMissing && (
        <p className="text-muted-foreground mb-3">
          No explainer exists yet for this project. Open the <b>UI Bridge Integration</b> page,
          check <b>Project Explainer</b>, and start generation.
        </p>
      )}
      <button
        onClick={onRetry}
        className="px-3 py-1 text-xs rounded bg-primary/15 text-primary hover:bg-primary/25"
      >
        Retry
      </button>
    </div>
  );
}

function ExplainerMarkdown({
  markdown,
  onLinkClick,
}: {
  markdown: string;
  onLinkClick: (e: React.MouseEvent, href: string | undefined) => void;
}) {
  return (
    <div className="prose prose-sm prose-invert max-w-none [&_a]:text-cyan-400 [&_code]:text-cyan-300">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          code(props: { className?: string; children?: React.ReactNode; inline?: boolean }) {
            const { className, children, inline } = props;
            const match = /language-(\w+)/.exec(className || "");
            if (!inline && match && match[1] === "mermaid") {
              return <MermaidDiagram chart={String(children).trim()} />;
            }
            return <code className={className}>{children}</code>;
          },
          a({ href, children }) {
            return (
              <a href={href} onClick={(e) => onLinkClick(e, href)}>
                {children}
              </a>
            );
          },
        }}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}

// ---------------------------------------------------------------------------
// AI side panel
// ---------------------------------------------------------------------------

function ExplainerAiPanel({ getContext }: { getContext: () => string }) {
  const session = useAiSession();
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sessionStarted, setSessionStarted] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const { ref: inputRef } = useUIElement({
    id: "explainer-ai-input",
    type: "input",
    label: "Ask AI about this explainer",
  });
  const { ref: sendRef } = useUIElement({
    id: "explainer-ai-send",
    type: "button",
    label: "Send",
    actions: ["click"],
  });
  const { ref: resetRef } = useUIElement({
    id: "explainer-ai-reset",
    type: "button",
    label: "Reset AI conversation",
    actions: ["click"],
  });

  // Auto-scroll when new messages arrive.
  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [session.messages, session.streamingContent]);

  const handleSend = useCallback(async () => {
    const trimmed = input.trim();
    if (!trimmed || sending) return;
    setSending(true);
    try {
      if (!sessionStarted) {
        await session.createSession("Project Explainer Chat");
        setSessionStarted(true);
      }
      // Prepend the current explainer page as grounding context on EVERY turn so
      // the model always has the page the user is reading on hand. (Duplicating
      // is cheap compared to the cognitive cost of the AI losing the thread when
      // the user scrolls or navigates.)
      const context = getContext();
      const grounded =
        context.trim().length > 0
          ? `The user is currently reading this explainer page:\n\n---\n${context}\n---\n\n` +
            `Question:\n${trimmed}`
          : trimmed;
      await session.sendMessage(grounded);
      setInput("");
    } finally {
      setSending(false);
    }
  }, [input, sending, sessionStarted, session, getContext]);

  const handleReset = useCallback(async () => {
    if (session.taskRunId) await session.close();
    session.resetSession();
    setSessionStarted(false);
  }, [session]);

  return (
    <div className="w-80 flex flex-col border-l border-border bg-card/20">
      <div className="flex items-center justify-between px-3 py-2 border-b border-border">
        <div className="text-xs font-semibold text-muted-foreground">Ask AI</div>
        {sessionStarted && (
          <button
            ref={resetRef}
            onClick={handleReset}
            className="p-1 text-muted-foreground hover:text-foreground"
            title="Reset conversation"
          >
            <X className="w-3.5 h-3.5" />
          </button>
        )}
      </div>
      <div ref={scrollRef} className="flex-1 overflow-y-auto p-3 space-y-3 text-sm">
        {session.messages.length === 0 && !session.streamingContent && (
          <p className="text-xs text-muted-foreground">
            Ask questions about anything in the explainer. The current page is included as context
            on every turn; free-form knowledge is also allowed.
          </p>
        )}
        {session.messages.map((m, i) => (
          <div key={i} className={m.role === "user" ? "text-foreground" : "text-muted-foreground"}>
            <div className="text-[10px] uppercase tracking-wider mb-0.5">
              {m.role === "user" ? "You" : "AI"}
            </div>
            <div className="whitespace-pre-wrap break-words">
              {m.role === "user" ? stripGrounding(m.content) : m.content}
            </div>
          </div>
        ))}
        {session.streamingContent && (
          <div className="text-muted-foreground">
            <div className="text-[10px] uppercase tracking-wider mb-0.5">AI</div>
            <div className="whitespace-pre-wrap break-words">{session.streamingContent}</div>
          </div>
        )}
      </div>
      <div className="p-2 border-t border-border flex items-end gap-2">
        <textarea
          ref={inputRef as unknown as React.Ref<HTMLTextAreaElement>}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void handleSend();
            }
          }}
          placeholder="Ask a question..."
          rows={2}
          className="flex-1 px-2 py-1 text-sm rounded border border-border bg-background
                     placeholder:text-muted-foreground resize-none focus:outline-hidden focus:border-cyan-500/50"
        />
        <button
          ref={sendRef}
          onClick={handleSend}
          disabled={!input.trim() || sending}
          className="p-2 rounded bg-primary/15 text-primary hover:bg-primary/25 disabled:opacity-40"
          title="Send (Enter)"
        >
          {sending ? <Loader2 className="w-4 h-4 animate-spin" /> : <Send className="w-4 h-4" />}
        </button>
      </div>
    </div>
  );
}

/** Strip the grounding prefix so the transcript shows the user's actual question. */
function stripGrounding(content: string): string {
  const idx = content.indexOf("\nQuestion:\n");
  if (idx >= 0) return content.slice(idx + "\nQuestion:\n".length);
  return content;
}
