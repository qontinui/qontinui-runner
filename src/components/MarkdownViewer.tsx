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

export function MarkdownViewer({ content, className, isAnimated = false }: MarkdownViewerProps) {
  // Pre-process content to transform [FINDING:...] blocks into HTML divs with data attributes
  // This allows ReactMarkdown (via rehype-raw) to parse them as elements we can style
  const processedContent = content.replace(
    /\[FINDING:([\w_]+):(\w+)\]([\s\S]*?)\[\/FINDING\]/g,
    (match: string, category: string, severity: string, body: string) => {
      // We wrap the body in a div with special data attributes
      // Note: We use double newlines before/after body to ensure markdown inside body is parsed
      return `<div data-finding-category="${category}" data-finding-severity="${severity}">\n\n${body}\n\n</div>`;
    },
  );

  return (
    <div
      className={cn(
        "text-xs bg-background p-3 overflow-x-auto prose prose-sm prose-neutral dark:prose-invert max-w-none prose-pre:bg-muted/50 prose-pre:p-0 prose-pre:m-0",
        isAnimated && "animate-in fade-in-0 slide-in-from-top-1 duration-200",
        className,
      )}
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw]}
        components={{
          div: ({ className, ...props }: Record<string, unknown>) => {
            // Check for our special finding data attributes
            const categoryId = props["data-finding-category"] as string | undefined;
            const severity = props["data-finding-severity"] as string | undefined;

            if (categoryId) {
              const category = getCategoryById(categoryId);
              // Default to 'slate'/Info if category not found
              const colorClasses = category
                ? getCategoryColorClasses(category.color)
                : getCategoryColorClasses("slate");

              const Icon = category ? iconMap[category.icon] || Info : Info;
              const categoryName = category ? category.name : categoryId;

              return (
                <div
                  className={`my-4 rounded-lg border ${colorClasses.border} overflow-hidden bg-card`}
                >
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

            const { children: _, ...restProps } = props;
            return <div className={className as string} {...(restProps as React.HTMLAttributes<HTMLDivElement>)} />;
          },
          pre: ({ children }) => (
            <pre className="bg-muted/50 p-2 rounded-md overflow-x-auto">{children}</pre>
          ),
          code: ({ className, children, ...props }) => {
            const match = /language-(\w+)/.exec(className || "");
            return match ? (
              <code className={`${className} bg-muted/50 px-1 py-0.5 rounded`} {...props}>
                {children}
              </code>
            ) : (
              <code className="bg-muted/50 px-1 py-0.5 rounded font-mono text-xs" {...props}>
                {children}
              </code>
            );
          },
          table: ({ children }) => (
            <div className="overflow-x-auto my-2">
              <table className="min-w-full divide-y divide-border border border-border">
                {children}
              </table>
            </div>
          ),
          th: ({ children }) => (
            <th className="px-3 py-2 bg-muted/50 text-left text-xs font-medium text-muted-foreground uppercase tracking-wider border-b border-border">
              {children}
            </th>
          ),
          td: ({ children }) => (
            <td className="px-3 py-2 text-sm whitespace-nowrap border-b border-border/50">
              {children}
            </td>
          ),
        }}
      >
        {processedContent}
      </ReactMarkdown>
    </div>
  );
}
