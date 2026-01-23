/**
 * Element Picker Component
 *
 * Displays an annotated screenshot with detected elements.
 * Users can hover to highlight elements and click to reference them
 * in AI test generation prompts.
 */

import { useState, useCallback, useMemo } from "react";
import { Copy, Check, Layers, Search, X } from "lucide-react";
import type { PageAnalysis, DetectedElement } from "./types";

interface ElementPickerProps {
  analysis: PageAnalysis;
  onElementClick?: (element: DetectedElement) => void;
  onElementReference?: (reference: string) => void;
  selectedElementIds?: string[];
}

export function ElementPicker({
  analysis,
  onElementClick,
  onElementReference,
  selectedElementIds = [],
}: ElementPickerProps) {
  const [hoveredElementId, setHoveredElementId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [copiedId, setCopiedId] = useState<string | null>(null);

  // Filter elements by search query
  const filteredElements = useMemo(() => {
    if (!searchQuery) return analysis.elements;
    const query = searchQuery.toLowerCase();
    return analysis.elements.filter(
      (el) =>
        el.id.toLowerCase().includes(query) ||
        el.label.toLowerCase().includes(query) ||
        el.element_type.toLowerCase().includes(query) ||
        el.text_content?.toLowerCase().includes(query),
    );
  }, [analysis.elements, searchQuery]);

  // Handle element click
  const handleElementClick = useCallback(
    (element: DetectedElement) => {
      onElementClick?.(element);
    },
    [onElementClick],
  );

  // Copy element reference to clipboard
  const handleCopyReference = useCallback(
    async (element: DetectedElement) => {
      const reference = element.selector || `element:${element.id}`;
      await navigator.clipboard.writeText(reference);
      setCopiedId(element.id);
      onElementReference?.(reference);
      setTimeout(() => setCopiedId(null), 2000);
    },
    [onElementReference],
  );

  // Get element at a point (for image click handling)
  const _findElementAtPoint = useCallback(
    (_x: number, _y: number, _imageWidth: number, _imageHeight: number) => {
      // Convert click coordinates to analysis coordinates
      // Assuming the displayed image maintains aspect ratio
      for (const element of analysis.elements) {
        const box = element.bounding_box;
        if (_x >= box.x && _x <= box.x + box.width && _y >= box.y && _y <= box.y + box.height) {
          return element;
        }
      }
      return null;
    },
    [analysis.elements],
  );

  // Render bounding box overlay
  const renderBoundingBox = (element: DetectedElement, index: number) => {
    const box = element.bounding_box;
    const isHovered = hoveredElementId === element.id;
    const isSelected = selectedElementIds.includes(element.id);

    return (
      <div
        key={element.id}
        className={`absolute border-2 transition-all pointer-events-none ${
          isHovered
            ? "border-blue-400 bg-blue-400/20"
            : isSelected
              ? "border-green-400 bg-green-400/10"
              : "border-amber-400/60"
        }`}
        style={{
          left: `${box.x}px`,
          top: `${box.y}px`,
          width: `${box.width}px`,
          height: `${box.height}px`,
        }}
      >
        {/* Element number badge */}
        <div
          className={`absolute -top-5 -left-1 px-1.5 py-0.5 text-xs font-medium rounded ${
            isHovered
              ? "bg-blue-500 text-white"
              : isSelected
                ? "bg-green-500 text-white"
                : "bg-amber-500 text-black"
          }`}
        >
          {index + 1}
        </div>
      </div>
    );
  };

  return (
    <div className="flex flex-col h-full bg-neutral-900 rounded-lg border border-neutral-700 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-neutral-700">
        <div className="flex items-center gap-2">
          <Layers className="w-4 h-4 text-blue-400" />
          <span className="text-sm font-medium text-neutral-200">
            Detected Elements ({analysis.elements.length})
          </span>
        </div>
        <span className="text-xs text-neutral-500">
          Source: {analysis.source === "playwright" ? "Browser DOM" : "Vision Analysis"}
        </span>
      </div>

      {/* Main content - split view */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left: Annotated Screenshot */}
        <div className="flex-1 relative overflow-auto bg-neutral-950 p-4">
          <div className="relative inline-block">
            <img
              src={`data:image/png;base64,${analysis.annotated_screenshot_base64 || analysis.screenshot_base64}`}
              alt="Annotated page screenshot"
              className="max-w-full h-auto"
            />
            {/* Bounding box overlays - only show if using original screenshot */}
            {!analysis.annotated_screenshot_base64 &&
              analysis.elements.map((el, idx) => renderBoundingBox(el, idx))}
          </div>
        </div>

        {/* Right: Element List */}
        <div className="w-80 flex flex-col border-l border-neutral-700">
          {/* Search */}
          <div className="p-2 border-b border-neutral-700">
            <div className="relative">
              <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-500" />
              <input
                type="text"
                placeholder="Search elements..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="w-full pl-8 pr-8 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-blue-500"
                data-ui-id="test-builder-element-search-input"
              />
              {searchQuery && (
                <button
                  onClick={() => setSearchQuery("")}
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-neutral-500 hover:text-neutral-300"
                  data-ui-id="test-builder-element-clear-search-btn"
                >
                  <X className="w-4 h-4" />
                </button>
              )}
            </div>
          </div>

          {/* Element list */}
          <div className="flex-1 overflow-y-auto">
            {filteredElements.map((element, index) => (
              <div
                key={element.id}
                className={`p-2 border-b border-neutral-800 cursor-pointer transition-colors ${
                  hoveredElementId === element.id
                    ? "bg-blue-900/30"
                    : selectedElementIds.includes(element.id)
                      ? "bg-green-900/30"
                      : "hover:bg-neutral-800"
                }`}
                onMouseEnter={() => setHoveredElementId(element.id)}
                onMouseLeave={() => setHoveredElementId(null)}
                onClick={() => handleElementClick(element)}
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="flex-shrink-0 w-5 h-5 flex items-center justify-center bg-amber-500 text-black text-xs font-medium rounded">
                        {index + 1}
                      </span>
                      <span className="text-sm font-medium text-neutral-200 truncate">
                        {element.label}
                      </span>
                    </div>
                    <div className="mt-1 text-xs text-neutral-500">
                      <span className="inline-block px-1.5 py-0.5 bg-neutral-800 rounded mr-2">
                        {element.element_type}
                      </span>
                      {element.text_content && (
                        <span className="truncate">"{element.text_content}"</span>
                      )}
                    </div>
                    {element.selector && (
                      <div className="mt-1 text-xs text-neutral-600 font-mono truncate">
                        {element.selector}
                      </div>
                    )}
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleCopyReference(element);
                    }}
                    className="flex-shrink-0 p-1 text-neutral-500 hover:text-neutral-300 transition-colors"
                    title="Copy reference"
                    data-ui-id={`test-builder-element-copy-${element.id}-btn`}
                  >
                    {copiedId === element.id ? (
                      <Check className="w-4 h-4 text-green-400" />
                    ) : (
                      <Copy className="w-4 h-4" />
                    )}
                  </button>
                </div>
                {/* Confidence and bounds */}
                <div className="mt-1 flex items-center gap-3 text-xs text-neutral-600">
                  <span>Confidence: {Math.round(element.confidence * 100)}%</span>
                  <span>
                    ({element.bounding_box.x}, {element.bounding_box.y}){" "}
                    {element.bounding_box.width}x{element.bounding_box.height}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-neutral-700 text-xs text-neutral-500">
        {analysis.url && <span className="mr-4">URL: {analysis.url}</span>}
        <span>Captured: {new Date(analysis.captured_at).toLocaleString()}</span>
      </div>
    </div>
  );
}
