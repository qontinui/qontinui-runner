/**
 * PromptSnippetSelector.tsx
 *
 * A dropdown/popup component for selecting and inserting prompt snippets.
 * Supports two modes:
 * - dropdown: Button that opens a popover with prompt snippet list
 * - popup: Floating popup for @-mention style insertion
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { Puzzle, Search, ChevronDown, X } from "lucide-react";
import type { PromptSnippet } from "../types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface PromptSnippetSelectorProps {
  onSelect: (content: string) => void;
  onClose?: () => void;
  searchQuery?: string;
  position?: { top: number; left: number };
  mode: "dropdown" | "popup";
}

export function PromptSnippetSelector({
  onSelect,
  onClose,
  searchQuery: initialSearchQuery = "",
  position,
  mode,
}: PromptSnippetSelectorProps) {
  const [isOpen, setIsOpen] = useState(mode === "popup");
  const [snippets, setSnippets] = useState<PromptSnippet[]>([]);
  const [categories, setCategories] = useState<string[]>([]);
  const [searchQuery, setSearchQuery] = useState(initialSearchQuery);
  const [selectedCategory, setSelectedCategory] = useState<string>("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [hoveredSnippet, setHoveredSnippet] = useState<PromptSnippet | null>(null);

  const containerRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Sync search query from props during render (derived state, no useEffect)
  const prevInitialSearchQueryRef = useRef(initialSearchQuery);
  if (prevInitialSearchQueryRef.current !== initialSearchQuery) {
    prevInitialSearchQueryRef.current = initialSearchQuery;
    setSearchQuery(initialSearchQuery);
  }

  // Reset selected index when filter changes (derived state, no useEffect)
  const prevSearchQueryRef = useRef(searchQuery);
  const prevSelectedCategoryRef = useRef(selectedCategory);
  if (
    prevSearchQueryRef.current !== searchQuery ||
    prevSelectedCategoryRef.current !== selectedCategory
  ) {
    prevSearchQueryRef.current = searchQuery;
    prevSelectedCategoryRef.current = selectedCategory;
    setSelectedIndex(0);
  }

  // Load prompt snippets and categories
  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [snippetsRes, categoriesRes] = await Promise.all([
        tracedFetch(`${getApiBase()}/prompt-snippets`),
        tracedFetch(`${getApiBase()}/prompt-snippets/categories`),
      ]);

      if (snippetsRes.ok) {
        const data = await snippetsRes.json();
        if (data.success) {
          setSnippets(data.data);
        }
      }

      if (categoriesRes.ok) {
        const data = await categoriesRes.json();
        if (data.success) {
          setCategories(data.data);
        }
      }
    } catch (error) {
      console.error("Failed to load prompt snippets:", error);
    } finally {
      setLoading(false);
    }
  }, []);

  // Load data when opened
  useEffect(() => {
    if (isOpen) {
      loadData();
    }
  }, [isOpen, loadData]);

  // Filter prompt snippets by search and category
  const filteredSnippets = snippets.filter((s) => {
    const matchesSearch =
      !searchQuery ||
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.content.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));

    const matchesCategory = !selectedCategory || s.category === selectedCategory;

    return matchesSearch && matchesCategory;
  });

  // Handle keyboard navigation
  useEffect(() => {
    if (!isOpen) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setSelectedIndex((prev) => Math.min(prev + 1, filteredSnippets.length - 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          setSelectedIndex((prev) => Math.max(prev - 1, 0));
          break;
        case "Enter":
          e.preventDefault();
          if (filteredSnippets[selectedIndex]) {
            handleSelect(filteredSnippets[selectedIndex]);
          }
          break;
        case "Escape":
          e.preventDefault();
          handleClose();
          break;
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, filteredSnippets, selectedIndex]);

  // Scroll selected item into view
  useEffect(() => {
    if (listRef.current && filteredSnippets.length > 0) {
      const selectedElement = listRef.current.children[selectedIndex] as HTMLElement;
      if (selectedElement) {
        selectedElement.scrollIntoView({ block: "nearest" });
      }
    }
  }, [selectedIndex, filteredSnippets.length]);

  // Focus search input when opened
  useEffect(() => {
    if (isOpen && searchInputRef.current) {
      setTimeout(() => searchInputRef.current?.focus(), 50);
    }
  }, [isOpen]);

  // Click outside to close
  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        handleClose();
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen]);

  const handleSelect = (snippet: PromptSnippet) => {
    onSelect(snippet.content);
    handleClose();
  };

  const handleClose = () => {
    setIsOpen(false);
    onClose?.();
  };

  const toggleOpen = () => {
    setIsOpen(!isOpen);
  };

  // Group prompt snippets by category for display
  const groupedSnippets = filteredSnippets.reduce(
    (acc, snippet) => {
      const category = snippet.category || "Uncategorized";
      if (!acc[category]) {
        acc[category] = [];
      }
      acc[category].push(snippet);
      return acc;
    },
    {} as Record<string, PromptSnippet[]>,
  );

  const renderSnippetList = () => (
    <div className="max-h-64 overflow-y-auto" ref={listRef}>
      {loading ? (
        <div className="p-4 text-center text-muted-foreground text-sm">Loading...</div>
      ) : filteredSnippets.length === 0 ? (
        <div className="p-4 text-center text-muted-foreground text-sm">
          {snippets.length === 0 ? "No prompt snippets yet" : "No matching prompt snippets"}
        </div>
      ) : (
        Object.entries(groupedSnippets).map(([category, items]) => (
          <div key={category}>
            <div className="px-3 py-1.5 text-xs font-medium text-muted-foreground bg-muted/30 sticky top-0">
              {category}
            </div>
            {items.map((snippet) => {
              const globalIndex = filteredSnippets.indexOf(snippet);
              return (
                <button
                  key={snippet.id}
                  onClick={() => handleSelect(snippet)}
                  onMouseEnter={() => {
                    setSelectedIndex(globalIndex);
                    setHoveredSnippet(snippet);
                  }}
                  onMouseLeave={() => setHoveredSnippet(null)}
                  className={`w-full px-3 py-2 text-left transition-colors ${
                    globalIndex === selectedIndex
                      ? "bg-primary/10 text-primary"
                      : "hover:bg-muted/50"
                  }`}
                >
                  <div className="font-medium text-sm">{snippet.name}</div>
                  <div className="text-xs text-muted-foreground truncate">{snippet.content}</div>
                </button>
              );
            })}
          </div>
        ))
      )}
    </div>
  );

  const renderPreview = () => {
    if (!hoveredSnippet) return null;

    return (
      <div className="absolute right-full top-0 mr-2 w-64 bg-card border border-border rounded-lg shadow-lg p-3 z-50">
        <div className="font-medium text-sm mb-1">{hoveredSnippet.name}</div>
        <div className="text-xs text-muted-foreground mb-2">
          {hoveredSnippet.category}
          {hoveredSnippet.tags.length > 0 && ` \u2022 ${hoveredSnippet.tags.join(", ")}`}
        </div>
        <div className="text-sm bg-muted/30 p-2 rounded max-h-32 overflow-y-auto">
          {hoveredSnippet.content}
        </div>
      </div>
    );
  };

  // Dropdown mode: button + popover
  if (mode === "dropdown") {
    return (
      <div className="relative" ref={containerRef}>
        <button
          onClick={toggleOpen}
          className="flex items-center gap-1.5 px-2 py-1.5 text-sm bg-card border border-border rounded-lg hover:bg-muted transition-colors"
          title="Insert prompt snippet"
        >
          <Puzzle className="w-4 h-4" />
          <ChevronDown className={`w-3 h-3 transition-transform ${isOpen ? "rotate-180" : ""}`} />
        </button>

        {isOpen && (
          <div className="absolute top-full mt-1 right-0 w-72 bg-card border border-border rounded-lg shadow-xl z-50">
            {/* Search and filter */}
            <div className="p-2 border-b border-border space-y-2">
              <div className="relative">
                <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
                <input
                  ref={searchInputRef}
                  type="text"
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  placeholder="Search prompt snippets..."
                  className="w-full pl-8 pr-3 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-primary"
                />
              </div>

              {categories.length > 0 && (
                <select
                  value={selectedCategory}
                  onChange={(e) => setSelectedCategory(e.target.value)}
                  className="w-full px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-primary"
                >
                  <option value="">All Categories</option>
                  {categories.map((cat) => (
                    <option key={cat} value={cat}>
                      {cat}
                    </option>
                  ))}
                </select>
              )}
            </div>

            {/* Prompt snippet list */}
            {renderSnippetList()}

            {/* Preview on hover */}
            {renderPreview()}
          </div>
        )}
      </div>
    );
  }

  // Popup mode: floating panel
  return (
    <div
      ref={containerRef}
      className="fixed bg-card border border-border rounded-lg shadow-xl z-50 w-72"
      style={position ? { top: position.top, left: position.left } : undefined}
    >
      {/* Header with close button */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-border">
        <div className="flex items-center gap-2 text-sm font-medium">
          <Puzzle className="w-4 h-4" />
          Insert Prompt Snippet
        </div>
        <button onClick={handleClose} className="text-muted-foreground hover:text-foreground">
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Search */}
      <div className="p-2 border-b border-border">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search..."
            className="w-full pl-8 pr-3 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-primary"
          />
        </div>
      </div>

      {/* Prompt snippet list */}
      {renderSnippetList()}
    </div>
  );
}

/**
 * Hook to manage @-mention style prompt snippet insertion
 */
export function usePromptSnippetMention(
  textareaRef: React.RefObject<HTMLTextAreaElement>,
  onInsert: (text: string, triggerStart: number, triggerEnd: number) => void,
) {
  const [isActive, setIsActive] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const [triggerPosition, setTriggerPosition] = useState<number | null>(null);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "@") {
        const textarea = textareaRef.current;
        if (!textarea) return;

        // Get cursor position and calculate popup position
        const cursorPos = textarea.selectionStart;
        setTriggerPosition(cursorPos);

        // Calculate position based on textarea and cursor
        const rect = textarea.getBoundingClientRect();
        const lineHeight = parseInt(getComputedStyle(textarea).lineHeight) || 20;
        const paddingTop = parseInt(getComputedStyle(textarea).paddingTop) || 0;

        // Rough estimate of cursor Y position
        const value = textarea.value.substring(0, cursorPos);
        const lines = value.split("\n").length;

        setPosition({
          top: rect.top + paddingTop + lines * lineHeight + 20,
          left: rect.left + 20,
        });

        setSearchQuery("");
        setIsActive(true);
      }
    },
    [textareaRef],
  );

  const handleInput = useCallback(
    (e: React.FormEvent<HTMLTextAreaElement>) => {
      if (!isActive || triggerPosition === null) return;

      const textarea = e.currentTarget;
      const cursorPos = textarea.selectionStart;
      const value = textarea.value;

      // Check if cursor moved before trigger or @ was deleted
      if (cursorPos <= triggerPosition || value[triggerPosition] !== "@") {
        setIsActive(false);
        setTriggerPosition(null);
        return;
      }

      // Extract search query (text after @)
      const query = value.substring(triggerPosition + 1, cursorPos);
      setSearchQuery(query);
    },
    [isActive, triggerPosition],
  );

  const handleSelect = useCallback(
    (content: string) => {
      if (triggerPosition === null) return;

      const textarea = textareaRef.current;
      if (!textarea) return;

      const cursorPos = textarea.selectionStart;
      onInsert(content, triggerPosition, cursorPos);

      setIsActive(false);
      setTriggerPosition(null);
    },
    [textareaRef, triggerPosition, onInsert],
  );

  const handleClose = useCallback(() => {
    setIsActive(false);
    setTriggerPosition(null);
    textareaRef.current?.focus();
  }, [textareaRef]);

  return {
    isActive,
    searchQuery,
    position,
    handleKeyDown,
    handleInput,
    handleSelect,
    handleClose,
  };
}
