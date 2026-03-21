import { useState, useEffect, useRef, useMemo, useCallback } from "react";
import type { ReactNode } from "react";
import { readDir, readTextFile } from "@tauri-apps/plugin-fs";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { FileText, Search, X, FolderOpen, Brain, Loader2 } from "lucide-react";

/* ── Types ────────────────────────────────────────────────────────────── */

interface DocFinderModalProps {
  onSelect: (filePath: string) => void;
  onClose: () => void;
  defaultRoot?: string;
}

interface ScannedFile {
  name: string;
  path: string;
  relativePath: string;
}

interface ContentResult {
  file: ScannedFile;
  title: string;
  preview: string;
  score: number;
}

/* ── Constants ────────────────────────────────────────────────────────── */

const DOC_EXTENSIONS = new Set(["md", "txt", "markdown", "mdx"]);
const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "dist",
  "build",
  "__pycache__",
  ".venv",
  "target",
  ".next",
  ".dev-logs",
]);
const DEFAULT_ROOT = "C:\\Users\\jspin\\Documents\\qontinui_parent";

const SEARCH_ITEM_HEIGHT = 50;
const CONTENT_ITEM_HEIGHT = 66;
const VIRTUAL_BUFFER = 10;

/* ── Breadth-first directory scanner ─────────────────────────────────── */

async function scanForDocs(root: string, maxDepth = 5, maxFiles = 1000): Promise<ScannedFile[]> {
  const results: ScannedFile[] = [];
  const normalizedRoot = root.replace(/\//g, "\\").replace(/\\$/, "");

  // BFS queue: [dirPath, depth]
  const queue: [string, number][] = [[normalizedRoot, 0]];

  while (queue.length > 0 && results.length < maxFiles) {
    const [dir, depth] = queue.shift()!;
    if (depth > maxDepth) continue;

    try {
      const entries = await readDir(dir);
      for (const entry of entries) {
        if (results.length >= maxFiles) break;
        const name = entry.name;
        if (!name) continue;
        const fullPath = `${dir}\\${name}`;

        if (entry.isDirectory) {
          if (!SKIP_DIRS.has(name) && !name.startsWith(".")) {
            queue.push([fullPath, depth + 1]);
          }
        } else {
          const ext = name.split(".").pop()?.toLowerCase() ?? "";
          if (DOC_EXTENSIONS.has(ext)) {
            results.push({
              name,
              path: fullPath,
              relativePath: fullPath.slice(normalizedRoot.length + 1),
            });
          }
        }
      }
    } catch {
      // Permission denied or other error - skip this directory
    }
  }

  return results;
}

/* ── Fuzzy match scoring ──────────────────────────────────────────────── */

function fuzzyScore(text: string, query: string): { score: number; indices: number[] } | null {
  const lower = text.toLowerCase();
  const q = query.toLowerCase();

  // Exact prefix match - highest score
  if (lower.startsWith(q)) {
    return {
      score: 100 + q.length,
      indices: Array.from({ length: q.length }, (_, i) => i),
    };
  }

  // Word boundary match
  const words = lower.split(/[\s\-_./\\]/);
  let wordStart = 0;
  const wordBoundaryIndices: number[] = [];
  let qi = 0;
  for (const word of words) {
    if (qi < q.length && word.startsWith(q[qi])) {
      for (let wi = 0; wi < word.length && qi < q.length; wi++) {
        if (word[wi] === q[qi]) {
          wordBoundaryIndices.push(wordStart + wi);
          qi++;
        }
      }
    }
    wordStart += word.length + 1;
  }
  if (qi === q.length) {
    return { score: 70 + q.length, indices: wordBoundaryIndices };
  }

  // Sequential character match (fuzzy)
  const indices: number[] = [];
  let si = 0;
  for (let i = 0; i < lower.length && si < q.length; i++) {
    if (lower[i] === q[si]) {
      indices.push(i);
      si++;
    }
  }
  if (si === q.length) {
    const spread = indices[indices.length - 1] - indices[0];
    return { score: 30 + Math.max(0, 50 - spread), indices };
  }

  return null;
}

function renderHighlighted(text: string, indices: number[]): ReactNode {
  if (indices.length === 0) return text;
  const indexSet = new Set(indices);
  return text.split("").map((char, i) =>
    indexSet.has(i) ? (
      <span key={`hl-${i}`} className="text-[#7aa2f7] font-medium">
        {char}
      </span>
    ) : (
      <span key={`ch-${i}`}>{char}</span>
    ),
  );
}

/* ── Virtualized list hook ────────────────────────────────────────────── */

function useVirtualList(itemCount: number, itemHeight: number, containerHeight: number) {
  const [scrollTop, setScrollTop] = useState(0);

  const { startIndex, endIndex, totalHeight } = useMemo(() => {
    const total = itemCount * itemHeight;
    const start = Math.max(0, Math.floor(scrollTop / itemHeight) - VIRTUAL_BUFFER);
    const visibleCount = Math.ceil(containerHeight / itemHeight);
    const end = Math.min(itemCount - 1, start + visibleCount + VIRTUAL_BUFFER * 2);
    return { startIndex: start, endIndex: end, totalHeight: total };
  }, [itemCount, itemHeight, containerHeight, scrollTop]);

  const onScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop);
  }, []);

  return { startIndex, endIndex, totalHeight, onScroll, scrollTop };
}

/* ── Component ────────────────────────────────────────────────────────── */

type TabId = "browse" | "search" | "content";

const TABS: { id: TabId; label: string; icon: ReactNode }[] = [
  { id: "browse", label: "Browse", icon: <FolderOpen className="w-3.5 h-3.5" /> },
  { id: "search", label: "Search", icon: <Search className="w-3.5 h-3.5" /> },
  { id: "content", label: "Content Search", icon: <Brain className="w-3.5 h-3.5" /> },
];

export function DocFinderModal({ onSelect, onClose, defaultRoot }: DocFinderModalProps) {
  const [activeTab, setActiveTab] = useState<TabId>("search");
  const [searchQuery, setSearchQuery] = useState("");
  const [contentQuery, setContentQuery] = useState("");
  const [scannedFiles, setScannedFiles] = useState<ScannedFile[]>([]);
  const [isScanning, setIsScanning] = useState(false);
  const [root, setRoot] = useState(defaultRoot ?? DEFAULT_ROOT);
  const [contentResults, setContentResults] = useState<ContentResult[]>([]);
  const [isSearchingContent, setIsSearchingContent] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);

  const overlayRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const contentInputRef = useRef<HTMLTextAreaElement>(null);

  // Close on Escape
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [onClose]);

  // Auto-focus inputs when switching tabs
  useEffect(() => {
    if (activeTab === "search") {
      setTimeout(() => searchInputRef.current?.focus(), 50);
    } else if (activeTab === "content") {
      setTimeout(() => contentInputRef.current?.focus(), 50);
    }
  }, [activeTab]);

  // Scan directory for document files
  const runScan = useCallback(async (scanRoot: string) => {
    setIsScanning(true);
    setScanError(null);
    try {
      const files = await scanForDocs(scanRoot);
      files.sort((a, b) => a.relativePath.localeCompare(b.relativePath));
      setScannedFiles(files);
      if (files.length === 0) {
        setScanError(`No document files found in ${scanRoot}`);
      }
    } catch (err) {
      setScannedFiles([]);
      setScanError(`Scan failed: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setIsScanning(false);
    }
  }, []);

  // Scan on mount and root change
  useEffect(() => {
    runScan(root);
  }, [root, runScan]);

  // Fuzzy-filtered search results
  const searchResults = useMemo(() => {
    if (!searchQuery.trim()) return scannedFiles;

    const q = searchQuery.trim();
    const scored: { file: ScannedFile; score: number; indices: number[] }[] = [];

    for (const file of scannedFiles) {
      const pathResult = fuzzyScore(file.relativePath, q);
      const nameResult = fuzzyScore(file.name, q);
      const best =
        pathResult && nameResult
          ? pathResult.score >= nameResult.score
            ? pathResult
            : nameResult
          : (pathResult ?? nameResult);

      if (best) {
        scored.push({ file, score: best.score, indices: best.indices });
      }
    }

    return scored.sort((a, b) => b.score - a.score).map((s) => s.file);
  }, [scannedFiles, searchQuery]);

  // Match indices for search highlighting
  const searchIndicesMap = useMemo(() => {
    const map = new Map<string, number[]>();
    if (!searchQuery.trim()) return map;

    const q = searchQuery.trim();
    for (const file of scannedFiles) {
      const result = fuzzyScore(file.name, q);
      if (result) {
        map.set(file.path, result.indices);
      }
    }
    return map;
  }, [scannedFiles, searchQuery]);

  // Change root directory
  const handleChangeRoot = useCallback(async () => {
    const selected = await openFileDialog({
      directory: true,
      title: "Select Root Directory",
      defaultPath: root,
    });
    if (selected) {
      setRoot(selected as string);
    }
  }, [root]);

  // Browse tab: open file dialog
  const handleBrowseFile = useCallback(async () => {
    const selected = await openFileDialog({
      title: "Open Document",
      filters: [{ name: "Documents", extensions: ["md", "txt", "markdown", "mdx"] }],
    });
    if (selected) {
      onSelect(selected as string);
    }
  }, [onSelect]);

  // Content search — process files in parallel batches for performance
  const handleContentSearch = useCallback(async () => {
    if (!contentQuery.trim() || scannedFiles.length === 0) return;

    setIsSearchingContent(true);
    const keywords = contentQuery
      .toLowerCase()
      .split(/\s+/)
      .filter((w) => w.length >= 3);

    if (keywords.length === 0) {
      setIsSearchingContent(false);
      return;
    }

    const results: ContentResult[] = [];
    const BATCH_SIZE = 20;

    for (let i = 0; i < scannedFiles.length; i += BATCH_SIZE) {
      const batch = scannedFiles.slice(i, i + BATCH_SIZE);
      const batchResults = await Promise.allSettled(
        batch.map(async (file) => {
          const content = await readTextFile(file.path);
          const lines = content.split("\n").slice(0, 50);
          const fullText = lines.join("\n").toLowerCase();

          // Extract metadata
          let title = "";
          const headers: string[] = [];

          for (const line of lines) {
            const trimmed = line.trim();
            if (trimmed.startsWith("# ") && !title) {
              title = trimmed.slice(2).trim();
            } else if (trimmed.startsWith("## ") || trimmed.startsWith("### ")) {
              headers.push(trimmed.replace(/^#+\s*/, "").trim());
            }
          }

          // Score by keyword overlap across filename, title, headers, and body
          let score = 0;
          const filenameLower = file.name.toLowerCase();
          const titleLower = title.toLowerCase();
          const headersLower = headers.join(" ").toLowerCase();

          for (const keyword of keywords) {
            if (filenameLower.includes(keyword)) score += 3;
            if (titleLower.includes(keyword)) score += 2;
            if (headersLower.includes(keyword)) score += 1;
            if (fullText.includes(keyword)) score += 1;
          }

          if (score > 0) {
            const previewLines = lines
              .filter((l) => l.trim().length > 0 && !l.trim().startsWith("#"))
              .slice(0, 2)
              .join(" ")
              .slice(0, 150);

            return {
              file,
              title: title || file.name,
              preview: previewLines || "(no preview)",
              score,
            } as ContentResult;
          }
          return null;
        }),
      );

      for (const r of batchResults) {
        if (r.status === "fulfilled" && r.value) {
          results.push(r.value);
        }
      }
    }

    results.sort((a, b) => b.score - a.score);
    setContentResults(results);
    setIsSearchingContent(false);
  }, [contentQuery, scannedFiles]);

  // Backdrop click
  const handleBackdropClick = (e: React.MouseEvent) => {
    if (e.target === overlayRef.current) {
      onClose();
    }
  };

  // Truncate path for display
  const truncatePath = (p: string, maxLen = 45) => {
    if (p.length <= maxLen) return p;
    return "..." + p.slice(p.length - maxLen + 3);
  };

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm"
      onClick={handleBackdropClick}
    >
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[500px] max-h-[70vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d]">
          <FileText className="w-4 h-4 text-[#7aa2f7]" />
          <h2 className="text-sm font-medium text-[#c0caf5] flex-1">Open Document</h2>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5]"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Tab bar */}
        <div className="flex border-b border-[#2a2d3d]">
          {TABS.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-1.5 px-4 py-2 text-xs font-medium transition-colors ${
                activeTab === tab.id
                  ? "text-[#c0caf5] border-b-2 border-[#7aa2f7]"
                  : "text-[#565f89] hover:text-[#a9b1d6]"
              }`}
            >
              {tab.icon}
              {tab.label}
            </button>
          ))}
        </div>

        {/* Tab content */}
        <div className="flex-1 overflow-y-auto scrollbar-dark">
          {activeTab === "browse" && <BrowseTab onBrowse={handleBrowseFile} />}
          {activeTab === "search" && (
            <SearchTab
              searchQuery={searchQuery}
              onSearchQueryChange={setSearchQuery}
              searchInputRef={searchInputRef}
              results={searchResults}
              indicesMap={searchIndicesMap}
              isScanning={isScanning}
              totalFiles={scannedFiles.length}
              root={root}
              truncatePath={truncatePath}
              onChangeRoot={handleChangeRoot}
              onSelect={onSelect}
              scanError={scanError}
            />
          )}
          {activeTab === "content" && (
            <ContentTab
              contentQuery={contentQuery}
              onContentQueryChange={setContentQuery}
              contentInputRef={contentInputRef}
              onSearch={handleContentSearch}
              results={contentResults}
              isSearching={isSearchingContent}
              isScanning={isScanning}
              totalFiles={scannedFiles.length}
              onSelect={onSelect}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/* ── Browse Tab ───────────────────────────────────────────────────────── */

function BrowseTab({ onBrowse }: { onBrowse: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 px-6">
      <FolderOpen className="w-10 h-10 text-[#565f89] mb-4" />
      <button
        onClick={onBrowse}
        className="px-5 py-2.5 bg-[#7aa2f7] hover:bg-[#6a92e7] text-[#1a1b26] text-sm font-medium rounded transition-colors"
      >
        Choose File...
      </button>
      <p className="mt-4 text-[11px] text-[#565f89]">Supported: .md, .txt, .markdown, .mdx</p>
    </div>
  );
}

/* ── Search Tab ───────────────────────────────────────────────────────── */

interface SearchTabProps {
  searchQuery: string;
  onSearchQueryChange: (q: string) => void;
  searchInputRef: React.RefObject<HTMLInputElement | null>;
  results: ScannedFile[];
  indicesMap: Map<string, number[]>;
  isScanning: boolean;
  totalFiles: number;
  root: string;
  truncatePath: (p: string, maxLen?: number) => string;
  onChangeRoot: () => void;
  onSelect: (filePath: string) => void;
  scanError: string | null;
}

function SearchTab({
  searchQuery,
  onSearchQueryChange,
  searchInputRef,
  results,
  indicesMap,
  isScanning,
  totalFiles,
  root,
  truncatePath,
  onChangeRoot,
  onSelect,
  scanError,
}: SearchTabProps) {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [prevResults, setPrevResults] = useState(results);
  // Reset selection during render when results change (avoids useEffect extra cycle)
  if (results !== prevResults) {
    setPrevResults(results);
    setSelectedIndex(0);
  }
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<Map<number, HTMLDivElement>>(new Map());

  const containerHeight = 300;
  const { startIndex, endIndex, totalHeight, onScroll } = useVirtualList(
    results.length,
    SEARCH_ITEM_HEIGHT,
    containerHeight,
  );

  // Keyboard navigation
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (results.length === 0) return;

      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((prev) => {
          const next = Math.min(prev + 1, results.length - 1);
          requestAnimationFrame(() => {
            const el = itemRefs.current.get(next);
            el?.scrollIntoView({ block: "nearest" });
          });
          return next;
        });
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((prev) => {
          const next = Math.max(prev - 1, 0);
          requestAnimationFrame(() => {
            const el = itemRefs.current.get(next);
            el?.scrollIntoView({ block: "nearest" });
          });
          return next;
        });
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (results[selectedIndex]) {
          onSelect(results[selectedIndex].path);
        }
      }
    },
    [results, selectedIndex, onSelect],
  );

  return (
    <div className="flex flex-col" onKeyDown={handleKeyDown}>
      {/* Search input */}
      <div className="px-3 pt-3 pb-2">
        <div className="flex items-center gap-2 bg-[#13141f] border border-[#2a2d3d] rounded px-3 py-2 focus-within:border-[#7aa2f7] transition-colors">
          <Search className="w-3.5 h-3.5 text-[#565f89] shrink-0" />
          <input
            ref={searchInputRef}
            value={searchQuery}
            onChange={(e) => onSearchQueryChange(e.target.value)}
            placeholder="Search by filename..."
            className="flex-1 bg-transparent text-sm text-[#c0caf5] placeholder-[#565f89] outline-hidden"
          />
        </div>
      </div>

      {/* Root selector */}
      <div className="px-3 pb-2 flex items-center gap-2 text-[11px]">
        <span className="text-[#565f89] shrink-0">Root:</span>
        <span className="text-[#a9b1d6] truncate" title={root}>
          {truncatePath(root)}
        </span>
        <button
          onClick={onChangeRoot}
          className="text-[#7aa2f7] hover:text-[#9bb8fb] transition-colors shrink-0"
        >
          Change
        </button>
      </div>

      {/* Status */}
      <div className="px-3 pb-1 text-[10px] text-[#565f89]">
        {isScanning ? (
          <span className="flex items-center gap-1">
            <Loader2 className="w-3 h-3 animate-spin" />
            Scanning...
          </span>
        ) : (
          <span>
            {totalFiles} file{totalFiles !== 1 ? "s" : ""} found
            {searchQuery.trim() && ` · ${results.length} match${results.length !== 1 ? "es" : ""}`}
          </span>
        )}
      </div>

      {/* Results (virtualized) */}
      <div
        ref={scrollContainerRef}
        className="overflow-y-auto scrollbar-dark"
        style={{ maxHeight: containerHeight }}
        onScroll={onScroll}
      >
        {!isScanning && results.length === 0 && (
          <div className="px-4 py-8 text-center text-[12px] text-[#565f89]">
            {scanError ? (
              <div className="text-[#f7768e]">{scanError}</div>
            ) : searchQuery.trim() ? (
              "No matches"
            ) : (
              "No document files found"
            )}
          </div>
        )}
        {results.length > 0 && (
          <div style={{ height: totalHeight, position: "relative" }}>
            {Array.from(
              { length: Math.max(0, endIndex - startIndex + 1) },
              (_, i) => startIndex + i,
            ).map((idx) => {
              const file = results[idx];
              if (!file) return null;
              return (
                <div
                  key={file.path}
                  ref={(el) => {
                    if (el) itemRefs.current.set(idx, el);
                    else itemRefs.current.delete(idx);
                  }}
                  onClick={() => onSelect(file.path)}
                  style={{
                    position: "absolute",
                    top: idx * SEARCH_ITEM_HEIGHT,
                    left: 0,
                    right: 0,
                    height: SEARCH_ITEM_HEIGHT,
                  }}
                  className={`flex items-center gap-3 px-3 cursor-pointer transition-colors border-b border-[#2a2d3d]/50 group ${
                    idx === selectedIndex ? "bg-[#24283b]" : "hover:bg-[#24283b]/50"
                  }`}
                >
                  <FileText className="w-4 h-4 text-[#565f89] shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="text-[12px] font-medium text-[#c0caf5] truncate">
                      {renderHighlighted(file.name, indicesMap.get(file.path) ?? [])}
                    </div>
                    <div className="text-[10px] text-[#565f89] truncate">{file.relativePath}</div>
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onSelect(file.path);
                    }}
                    className="text-[10px] text-[#7aa2f7] hover:text-[#9bb8fb] opacity-0 group-hover:opacity-100 transition-opacity shrink-0"
                  >
                    Open
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Content Search Tab ───────────────────────────────────────────────── */

interface ContentTabProps {
  contentQuery: string;
  onContentQueryChange: (q: string) => void;
  contentInputRef: React.RefObject<HTMLTextAreaElement | null>;
  onSearch: () => void;
  results: ContentResult[];
  isSearching: boolean;
  isScanning: boolean;
  totalFiles: number;
  onSelect: (filePath: string) => void;
}

function ContentTab({
  contentQuery,
  onContentQueryChange,
  contentInputRef,
  onSearch,
  results,
  isSearching,
  isScanning,
  totalFiles,
  onSelect,
}: ContentTabProps) {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [prevResults, setPrevResults] = useState(results);
  // Reset selection during render when results change (avoids useEffect extra cycle)
  if (results !== prevResults) {
    setPrevResults(results);
    setSelectedIndex(0);
  }
  const itemRefs = useRef<Map<number, HTMLDivElement>>(new Map());

  const containerHeight = 300;
  const { startIndex, endIndex, totalHeight, onScroll } = useVirtualList(
    results.length,
    CONTENT_ITEM_HEIGHT,
    containerHeight,
  );

  // Keyboard: Enter triggers search, Arrow keys navigate results
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        if (results.length === 0) return;
        setSelectedIndex((prev) => {
          const next = Math.min(prev + 1, results.length - 1);
          requestAnimationFrame(() => {
            const el = itemRefs.current.get(next);
            el?.scrollIntoView({ block: "nearest" });
          });
          return next;
        });
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        if (results.length === 0) return;
        setSelectedIndex((prev) => {
          const next = Math.max(prev - 1, 0);
          requestAnimationFrame(() => {
            const el = itemRefs.current.get(next);
            el?.scrollIntoView({ block: "nearest" });
          });
          return next;
        });
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (results.length > 0 && results[selectedIndex]) {
          onSelect(results[selectedIndex].file.path);
        } else {
          onSearch();
        }
      }
    },
    [results, selectedIndex, onSelect, onSearch],
  );

  return (
    <div className="flex flex-col" onKeyDown={handleKeyDown}>
      {/* Search input */}
      <div className="px-3 pt-3 pb-2 flex gap-2">
        <textarea
          ref={contentInputRef}
          value={contentQuery}
          onChange={(e) => onContentQueryChange(e.target.value)}
          placeholder="Describe the document you're looking for..."
          rows={2}
          className="flex-1 bg-[#13141f] border border-[#2a2d3d] rounded px-3 py-2 text-sm text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] resize-none transition-colors"
        />
        <button
          onClick={onSearch}
          disabled={isSearching || isScanning || !contentQuery.trim()}
          className="self-end px-3 py-2 bg-[#7aa2f7] hover:bg-[#6a92e7] disabled:bg-[#2a2d3d] disabled:text-[#565f89] text-[#1a1b26] text-xs font-medium rounded transition-colors shrink-0"
        >
          {isSearching ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : "Search"}
        </button>
      </div>

      {/* Status */}
      <div className="px-3 pb-1 text-[10px] text-[#565f89]">
        {isScanning ? (
          <span className="flex items-center gap-1">
            <Loader2 className="w-3 h-3 animate-spin" />
            Scanning files...
          </span>
        ) : isSearching ? (
          <span className="flex items-center gap-1">
            <Loader2 className="w-3 h-3 animate-spin" />
            Searching {totalFiles} files...
          </span>
        ) : results.length > 0 ? (
          <span>
            {results.length} result{results.length !== 1 ? "s" : ""}
          </span>
        ) : contentQuery.trim() ? (
          <span>Enter keywords and click Search</span>
        ) : (
          <span>{totalFiles} files indexed</span>
        )}
      </div>

      {/* Results (virtualized) */}
      <div
        className="overflow-y-auto scrollbar-dark"
        style={{ maxHeight: containerHeight }}
        onScroll={onScroll}
      >
        {results.length === 0 && contentQuery.trim() && !isSearching && (
          <div className="px-4 py-8 text-center text-[12px] text-[#565f89]">
            No matching documents
          </div>
        )}
        {results.length > 0 && (
          <div style={{ height: totalHeight, position: "relative" }}>
            {Array.from(
              { length: Math.max(0, endIndex - startIndex + 1) },
              (_, i) => startIndex + i,
            ).map((idx) => {
              const result = results[idx];
              if (!result) return null;
              return (
                <div
                  key={result.file.path}
                  ref={(el) => {
                    if (el) itemRefs.current.set(idx, el);
                    else itemRefs.current.delete(idx);
                  }}
                  onClick={() => onSelect(result.file.path)}
                  style={{
                    position: "absolute",
                    top: idx * CONTENT_ITEM_HEIGHT,
                    left: 0,
                    right: 0,
                    height: CONTENT_ITEM_HEIGHT,
                  }}
                  className={`flex items-start gap-3 px-3 py-2.5 cursor-pointer transition-colors border-b border-[#2a2d3d]/50 group ${
                    idx === selectedIndex ? "bg-[#24283b]" : "hover:bg-[#24283b]/50"
                  }`}
                >
                  <FileText className="w-4 h-4 text-[#565f89] shrink-0 mt-0.5" />
                  <div className="flex-1 min-w-0">
                    <div className="text-[12px] font-medium text-[#c0caf5] truncate">
                      {result.file.name}
                    </div>
                    {result.title !== result.file.name && (
                      <div className="text-[11px] text-[#a9b1d6] truncate">{result.title}</div>
                    )}
                    <div className="text-[10px] text-[#565f89] truncate mt-0.5">
                      {result.preview}
                    </div>
                  </div>
                  <div className="flex flex-col items-end gap-1 shrink-0">
                    <span className="text-[9px] font-mono bg-[#2a2d3d] text-[#7aa2f7] rounded px-1.5 py-0.5">
                      {result.score}
                    </span>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onSelect(result.file.path);
                      }}
                      className="text-[10px] text-[#7aa2f7] hover:text-[#9bb8fb] opacity-0 group-hover:opacity-100 transition-opacity"
                    >
                      Open
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
