/**
 * AboutTab — wrapper metadata + README.
 *
 * Shows manifest-derived metadata (package, version, license, author, repo
 * link) plus the wrapper's README, fetched from
 * `GET /wrappers/:id/readme`. If that endpoint isn't wired (Phase 1 stub),
 * we render a friendly "coming soon" placeholder for the README and still
 * show the structured metadata above.
 */

import { useEffect, useState } from "react";
import { Loader2, ExternalLink, ScrollText } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { fetchReadme } from "@/lib/wrappers/api";
import type { InstalledWrapper } from "@/lib/wrappers/types";

export interface AboutTabProps {
  wrapper: InstalledWrapper;
}

interface MetaRow {
  label: string;
  value: React.ReactNode;
}

export function AboutTab({ wrapper }: AboutTabProps) {
  const [readme, setReadme] = useState<{ available: boolean; content: string } | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset loading on wrapper id change
    setLoading(true);
    (async () => {
      const res = await fetchReadme(wrapper.id);
      if (!cancelled) {
        setReadme(res);
        setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [wrapper.id]);

  const author = (wrapper.manifest as { author?: { name?: string; url?: string } }).author;
  const license = (wrapper.manifest as { license?: string }).license;
  const repo =
    (wrapper.manifest as { repo?: string; repository?: string }).repo ??
    (wrapper.manifest as { repository?: string }).repository;

  const rows: MetaRow[] = [
    { label: "Package", value: <code className="font-mono text-sm">{wrapper.packageName}</code> },
    { label: "Version", value: <span className="text-sm">{wrapper.version}</span> },
    { label: "Transport", value: <span className="text-sm">{wrapper.manifest.transport}</span> },
    {
      label: "Categories",
      value: wrapper.manifest.categories?.length ? (
        <span className="text-sm">{wrapper.manifest.categories.join(", ")}</span>
      ) : (
        <span className="text-sm text-muted-foreground">—</span>
      ),
    },
    {
      label: "Author",
      value: author?.name ? (
        author.url ? (
          <a
            href={author.url}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-sm text-cyan-400 hover:text-cyan-300 transition-colors"
          >
            {author.name}
            <ExternalLink className="w-3 h-3" />
          </a>
        ) : (
          <span className="text-sm">{author.name}</span>
        )
      ) : (
        <span className="text-sm text-muted-foreground">—</span>
      ),
    },
    {
      label: "License",
      value: license ? (
        <span className="text-sm">{license}</span>
      ) : (
        <span className="text-sm text-muted-foreground">—</span>
      ),
    },
    {
      label: "Repository",
      value: repo ? (
        <a
          href={repo}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 text-sm text-cyan-400 hover:text-cyan-300 transition-colors"
        >
          {repo}
          <ExternalLink className="w-3 h-3" />
        </a>
      ) : (
        <span className="text-sm text-muted-foreground">—</span>
      ),
    },
  ];

  return (
    <div className="space-y-4">
      <div className="rounded-xl border border-border bg-card overflow-hidden">
        <div className="px-5 py-3 border-b border-border">
          <h3 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            Metadata
          </h3>
        </div>
        <dl className="divide-y divide-border">
          {rows.map((row) => (
            <div key={row.label} className="grid grid-cols-3 px-5 py-3 gap-4 items-center">
              <dt className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
                {row.label}
              </dt>
              <dd className="col-span-2 text-foreground min-w-0 break-words">{row.value}</dd>
            </div>
          ))}
        </dl>
      </div>

      <div className="rounded-xl border border-border bg-card overflow-hidden">
        <div className="px-5 py-3 border-b border-border">
          <h3 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            README
          </h3>
        </div>
        <div className="p-5">
          {loading ? (
            <div className="flex items-center justify-center py-8 gap-2 text-muted-foreground">
              <Loader2 className="w-4 h-4 animate-spin" />
              <span className="text-sm">Loading README…</span>
            </div>
          ) : !readme || !readme.available ? (
            <div className="rounded-xl border border-dashed border-border bg-card/40 p-8 text-center flex flex-col items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl border border-cyan-500/30 bg-cyan-500/10">
                <ScrollText className="w-4 h-4 text-cyan-400" />
              </div>
              <p className="text-sm text-muted-foreground max-w-sm">
                README content will appear here once the runner exposes the
                <code className="mx-1 px-1 py-0.5 rounded bg-muted/40 font-mono">
                  /wrappers/:id/readme
                </code>
                endpoint.
              </p>
            </div>
          ) : (
            <div className="prose prose-sm prose-invert max-w-none">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{readme.content}</ReactMarkdown>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
