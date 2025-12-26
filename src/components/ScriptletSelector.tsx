/**
 * ScriptletSelector.tsx
 *
 * A dropdown/popup component for selecting and inserting scriptlets.
 * Supports two modes:
 * - dropdown: Button that opens a popover with scriptlet list
 * - popup: Floating popup for @-mention style insertion
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { Puzzle, Search, ChevronDown, X, FolderOpen } from "lucide-react";
import type { Scriptlet } from "../types";

const API_BASE = "http://localhost:9876";

interface ScriptletSelectorProps {
  onSelect: (content: string) => void;
  onClose?: () => void;
  searchQuery?: string;
  position?: { top: number; left: number };
  mode: "dropdown" | "popup";
}

export function ScriptletSelector({
  onSelect,
  onClose,
  searchQuery: initialSearchQuery = "",
  position,
  mode,
}: ScriptletSelectorProps) {
  const [isOpen, setIsOpen] = useState(mode === "popup");
  const [scriptlets, setScriptlets] = useState<Scriptlet[]>([]);
  const [categories, setCategories] = useState<string[]>([]);
  const [searchQuery, setSearchQuery] = useState(initialSearchQuery);
  const [selectedCategory, setSelectedCategory] = useState<string>("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [hoveredScriptlet, setHoveredScriptlet] = useState<Scriptlet | null>(null);

  const containerRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Load scriptlets and categories
  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [scriptletsRes, categoriesRes] = await Promise.all([
        fetch(`${API_BASE}/scriptlets`),
        fetch(`${API_BASE}/scriptlets/categories`),
      ]);

      if (scriptletsRes.ok) {
        const data = await scriptletsRes.json();
        if (data.success) {
          setScriptlets(data.data);
        }
      }

      if (categoriesRes.ok) {
        const data = await categoriesRes.json();
        if (data.success) {
          setCategories(data.data);
        }
      }
    } catch (error) {
      console.error("Failed to load scriptlets:", error);
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

  // Update search query from props (for @-mention)
  useEffect(() => {
    setSearchQuery(initialSearchQuery);
  }, [initialSearchQuery]);

  // Filter scriptlets by search and category
  const filteredScriptlets = scriptlets.filter((s) => {
    const matchesSearch =
      !searchQuery ||
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.content.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));

    const matchesCategory = !selectedCategory || s.category === selectedCategory;

    return matchesSearch && matchesCategory;
  });

  // Reset selected index when filter changes
  useEffect(() => {
    setSelectedIndex(0);
  }, [searchQuery, selectedCategory]);

  // Handle keyboard navigation
  useEffect(() => {
    if (!isOpen) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          setSelectedIndex((prev) => Math.min(prev + 1, filteredScriptlets.length - 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          setSelectedIndex((prev) => Math.max(prev - 1, 0));
          break;
        case "Enter":
          e.preventDefault();
          if (filteredScriptlets[selectedIndex]) {
            handleSelect(filteredScriptlets[selectedIndex]);
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
  }, [isOpen, filteredScriptlets, selectedIndex]);

  // Scroll selected item into view
  useEffect(() => {
    if (listRef.current && filteredScriptlets.length > 0) {
      const selectedElement = listRef.current.children[selectedIndex] as HTMLElement;
      if (selectedElement) {
        selectedElement.scrollIntoView({ block: "nearest" });
      }
    }
  }, [selectedIndex, filteredScriptlets.length]);

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
  }, [isOpen]);

  const handleSelect = (scriptlet: Scriptlet) => {
    onSelect(scriptlet.content);
    handleClose();
  };

  const handleClose = () => {
    setIsOpen(false);
    onClose?.();
  };

  const toggleOpen = () => {
    setIsOpen(!isOpen);
  };

  // Group scriptlets by category for display
  const groupedScriptlets = filteredScriptlets.reduce(
    (acc, scriptlet) => {
      const category = scriptlet.category || "Uncategorized";
      if (!acc[category]) {
        acc[category] = [];
      }
      acc[category].push(scriptlet);
      return acc;
    },
    {} as Record<string, Scriptlet[]>,
  );

  const renderScriptletList = () => (
    <div className="max-h-64 overflow-y-auto" ref={listRef}>
      {loading ? (
        <div className="p-4 text-center text-muted-foreground text-sm">Loading...</div>
      ) : filteredScriptlets.length === 0 ? (
        <div className="p-4 text-center text-muted-foreground text-sm">
          {scriptlets.length === 0 ? "No scriptlets yet" : "No matching scriptlets"}
        </div>
      ) : (
        Object.entries(groupedScriptlets).map(([category, items]) => (
          <div key={category}>
            <div className="px-3 py-1.5 text-xs font-medium text-muted-foreground bg-muted/30 sticky top-0">
              {category}
            </div>
            {items.map((scriptlet, idx) => {
              const globalIndex = filteredScriptlets.indexOf(scriptlet);
              return (
                <button
                  key={scriptlet.id}
                  onClick={() => handleSelect(scriptlet)}
                  onMouseEnter={() => {
                    setSelectedIndex(globalIndex);
                    setHoveredScriptlet(scriptlet);
                  }}
                  onMouseLeave={() => setHoveredScriptlet(null)}
                  className={`w-full px-3 py-2 text-left transition-colors ${
                    globalIndex === selectedIndex
                      ? "bg-primary/10 text-primary"
                      : "hover:bg-muted/50"
                  }`}
                >
                  <div className="font-medium text-sm">{scriptlet.name}</div>
                  <div className="text-xs text-muted-foreground truncate">{scriptlet.content}</div>
                </button>
              );
            })}
          </div>
        ))
      )}
    </div>
  );

  const renderPreview = () => {
    if (!hoveredScriptlet) return null;

    return (
      <div className="absolute right-full top-0 mr-2 w-64 bg-card border border-border rounded-lg shadow-lg p-3 z-50">
        <div className="font-medium text-sm mb-1">{hoveredScriptlet.name}</div>
        <div className="text-xs text-muted-foreground mb-2">
          {hoveredScriptlet.category}
          {hoveredScriptlet.tags.length > 0 && ` • ${hoveredScriptlet.tags.join(", ")}`}
        </div>
        <div className="text-sm bg-muted/30 p-2 rounded max-h-32 overflow-y-auto">
          {hoveredScriptlet.content}
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
          title="Insert scriptlet"
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
                  placeholder="Search scriptlets..."
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

            {/* Scriptlet list */}
            {renderScriptletList()}

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
          Insert Scriptlet
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

      {/* Scriptlet list */}
      {renderScriptletList()}
    </div>
  );
}

/**
 * Hook to manage @-mention style scriptlet insertion
 */
export function useScriptletMention(
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
