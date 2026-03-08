/**
 * Element Tree View
 *
 * Displays a hierarchical tree of UI Bridge registered elements.
 * Supports selection, filtering, and state indication.
 * Uses lazy loading for thumbnails to improve performance with large element counts.
 */

import { useState, useMemo, useCallback, useEffect } from "react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui/Badge";
import {
  ChevronRight,
  ChevronDown,
  Box,
  Type,
  ToggleLeft,
  Link,
  Square,
  FormInput,
  MousePointer,
  List,
  Circle,
  EyeOff,
  Lock,
  Search,
  Pointer,
  Accessibility,
  Keyboard,
  Check,
  Globe,
} from "lucide-react";
import type { UIBridgeElement, UIBridgeState } from "./UIBridgeInspectorPanel";
import { ImageViewerModal } from "./ImageViewerModal";
import { cropPreview } from "../../lib/thumbnail-cropper";
import { LazyThumbnail } from "./LazyThumbnail";

interface ElementTreeViewProps {
  elements: UIBridgeElement[];
  states: UIBridgeState[];
  activeStates: string[];
  selectedElementId?: string | null;
  onSelectElement?: (elementId: string | null) => void;
  loading?: boolean;
  /** Map of element ID to base64 thumbnail data (for eager mode or pre-loaded thumbnails) */
  thumbnails?: Map<string, string>;
  /** Whether thumbnails are currently loading (eager mode only) */
  isLoadingThumbnails?: boolean;
  /** Base64 screenshot data for cropping larger previews and lazy thumbnails */
  screenshotData?: string;
  /** Shared thumbnail cache for lazy loading (passed from useElementThumbnails) */
  thumbnailCache?: Map<string, string>;
}

interface TreeNode {
  element: UIBridgeElement;
  children: TreeNode[];
  depth: number;
}

const ELEMENT_TYPE_ICONS: Record<string, React.ReactNode> = {
  button: <MousePointer className="w-3.5 h-3.5" />,
  input: <Type className="w-3.5 h-3.5" />,
  textarea: <FormInput className="w-3.5 h-3.5" />,
  select: <List className="w-3.5 h-3.5" />,
  checkbox: <ToggleLeft className="w-3.5 h-3.5" />,
  radio: <Circle className="w-3.5 h-3.5" />,
  link: <Link className="w-3.5 h-3.5" />,
  form: <Square className="w-3.5 h-3.5" />,
  custom: <Box className="w-3.5 h-3.5" />,
};

export function ElementTreeView({
  elements,
  states,
  activeStates,
  selectedElementId,
  onSelectElement,
  loading = false,
  thumbnails,
  isLoadingThumbnails: _isLoadingThumbnails = false,
  screenshotData,
  thumbnailCache,
}: ElementTreeViewProps) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");

  // Track loaded thumbnails for lazy loading mode
  const [lazyThumbnails, setLazyThumbnails] = useState<Map<string, string>>(new Map());

  // Handler for when a lazy thumbnail loads
  const handleThumbnailLoad = useCallback((elementId: string, thumbnail: string) => {
    setLazyThumbnails((prev) => {
      const next = new Map(prev);
      next.set(elementId, thumbnail);
      return next;
    });
  }, []);

  // Clear lazy thumbnails when screenshot changes
  useEffect(() => {
    setLazyThumbnails(new Map());
  }, [screenshotData]);

  // Build tree structure from flat elements
  const tree = useMemo(() => {
    const nodeMap = new Map<string, TreeNode>();
    const roots: TreeNode[] = [];

    // First pass: create nodes
    elements.forEach((element) => {
      nodeMap.set(element.id, {
        element,
        children: [],
        depth: 0,
      });
    });

    // Second pass: build hierarchy
    elements.forEach((element) => {
      const node = nodeMap.get(element.id)!;
      if (element.parent && nodeMap.has(element.parent)) {
        const parentNode = nodeMap.get(element.parent)!;
        parentNode.children.push(node);
        node.depth = parentNode.depth + 1;
      } else {
        roots.push(node);
      }
    });

    // Sort children by ID
    const sortChildren = (node: TreeNode) => {
      node.children.sort((a, b) => a.element.id.localeCompare(b.element.id));
      node.children.forEach(sortChildren);
    };
    roots.forEach(sortChildren);
    roots.sort((a, b) => a.element.id.localeCompare(b.element.id));

    return roots;
  }, [elements]);

  // Filter tree
  const filteredTree = useMemo(() => {
    if (!filter.trim()) return tree;

    const lowerFilter = filter.toLowerCase();

    const matchesFilter = (node: TreeNode): boolean => {
      const el = node.element;
      return (
        el.id.toLowerCase().includes(lowerFilter) ||
        el.type.toLowerCase().includes(lowerFilter) ||
        el.tagName.toLowerCase().includes(lowerFilter) ||
        el.label?.toLowerCase().includes(lowerFilter) ||
        el.text?.toLowerCase().includes(lowerFilter) ||
        false
      );
    };

    const filterNode = (node: TreeNode): TreeNode | null => {
      const filteredChildren = node.children
        .map(filterNode)
        .filter((n): n is TreeNode => n !== null);

      if (matchesFilter(node) || filteredChildren.length > 0) {
        return {
          ...node,
          children: filteredChildren,
        };
      }
      return null;
    };

    return tree.map(filterNode).filter((n): n is TreeNode => n !== null);
  }, [tree, filter]);

  // Get states for an element
  const getElementStates = useCallback(
    (elementId: string): UIBridgeState[] => {
      return states.filter((state) => state.elements.includes(elementId));
    },
    [states],
  );

  // Toggle expand/collapse
  const toggleExpand = useCallback((id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Expand all nodes containing the filter match
  const expandAll = useCallback(() => {
    const allIds = new Set<string>();
    const collectIds = (node: TreeNode) => {
      if (node.children.length > 0) {
        allIds.add(node.element.id);
        node.children.forEach(collectIds);
      }
    };
    filteredTree.forEach(collectIds);
    setExpandedIds(allIds);
  }, [filteredTree]);

  // Collapse all
  const collapseAll = useCallback(() => {
    setExpandedIds(new Set());
  }, []);

  // Render a single tree node
  const renderNode = (node: TreeNode) => {
    const { element, children, depth } = node;
    const isExpanded = expandedIds.has(element.id);
    const isSelected = selectedElementId === element.id;
    const hasChildren = children.length > 0;
    const elementStates = getElementStates(element.id);
    const isInActiveState = elementStates.some((s) => activeStates.includes(s.id));

    // Get role from element (either top-level or from accessibility object)
    const role = element.role || element.accessibility?.role;
    const isInteractive = element.is_interactive || element.accessibility?.isKeyboardAccessible;

    return (
      <div key={element.id}>
        <div
          className={cn(
            "flex items-center gap-1 py-1 px-2 rounded-md cursor-pointer transition-colors",
            "hover:bg-muted/50",
            isSelected && "bg-primary/10 ring-1 ring-primary/30",
            !element.visible && "opacity-50",
          )}
          style={{ paddingLeft: `${depth * 16 + 8}px` }}
          onClick={() => onSelectElement?.(isSelected ? null : element.id)}
        >
          {/* Expand/Collapse toggle */}
          <button
            className={cn(
              "w-4 h-4 flex items-center justify-center rounded hover:bg-muted",
              !hasChildren && "invisible",
            )}
            onClick={(e) => {
              e.stopPropagation();
              toggleExpand(element.id);
            }}
          >
            {isExpanded ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )}
          </button>

          {/* Thumbnail - uses lazy loading with IntersectionObserver */}
          {/* Cross-origin elements show a placeholder instead of attempting to load */}
          {element.isCrossOrigin ? (
            <div
              className="w-8 h-6 flex items-center justify-center border border-amber-500/30 rounded bg-amber-500/10 flex-shrink-0"
              title="Thumbnail not available - cross-origin iframe content"
            >
              <Globe className="w-3 h-3 text-amber-600" />
            </div>
          ) : screenshotData && element.bounds ? (
            <LazyThumbnail
              elementId={element.id}
              bounds={element.bounds}
              screenshotBase64={screenshotData}
              maxSize={40}
              className="w-8 h-6"
              thumbnailCache={thumbnailCache || thumbnails}
              onLoad={handleThumbnailLoad}
              isCrossOrigin={element.isCrossOrigin}
              fallback={
                <span className="text-muted-foreground">
                  {ELEMENT_TYPE_ICONS[element.type] || ELEMENT_TYPE_ICONS.custom}
                </span>
              }
            />
          ) : thumbnails?.get(element.id) || lazyThumbnails.get(element.id) ? (
            <img
              src={`data:image/png;base64,${thumbnails?.get(element.id) || lazyThumbnails.get(element.id)}`}
              alt=""
              className="w-8 h-6 object-contain border border-border/50 rounded bg-white/50 flex-shrink-0"
            />
          ) : (
            /* Element type icon as fallback */
            <span className="text-muted-foreground w-8 flex justify-center flex-shrink-0">
              {ELEMENT_TYPE_ICONS[element.type] || ELEMENT_TYPE_ICONS.custom}
            </span>
          )}

          {/* Ref badge (like @e1) - displayed prominently */}
          {element.ref && (
            <Badge variant="default" className="text-[10px] px-1.5 py-0 font-mono bg-primary/80">
              {element.ref}
            </Badge>
          )}

          {/* Element ID */}
          <span
            className={cn("flex-1 text-xs font-mono truncate", isInActiveState && "text-accent")}
            title={element.id}
          >
            {element.id}
          </span>

          {/* Interactive indicator */}
          {isInteractive && (
            <span className="inline-flex" title="Interactive element">
              <Pointer className="w-3 h-3 text-accent" />
            </span>
          )}

          {/* State indicators */}
          {element.isCrossOrigin && (
            <span className="inline-flex" title="Cross-origin iframe - thumbnail not available">
              <Globe className="w-3 h-3 text-amber-600" />
            </span>
          )}
          {!element.visible && (
            <span className="inline-flex" title="Hidden">
              <EyeOff className="w-3 h-3 text-muted-foreground" />
            </span>
          )}
          {!element.enabled && (
            <span className="inline-flex" title="Disabled">
              <Lock className="w-3 h-3 text-muted-foreground" />
            </span>
          )}
          {element.focused && <div className="w-2 h-2 rounded-full bg-accent" title="Focused" />}

          {/* Role badge */}
          {role && (
            <Badge
              variant="outline"
              className="text-[10px] px-1 py-0 border-primary/40 text-primary/80"
            >
              {role}
            </Badge>
          )}

          {/* Type badge */}
          <Badge variant="muted" className="text-[10px] px-1 py-0">
            {element.type}
          </Badge>
        </div>

        {/* Children */}
        {hasChildren && isExpanded && <div>{children.map(renderNode)}</div>}
      </div>
    );
  };

  if (loading && elements.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Loading elements...
      </div>
    );
  }

  if (elements.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Box className="w-8 h-8 opacity-50" />
        <p>No UI Bridge elements found</p>
        <p className="text-xs">
          The AutoRegisterProvider automatically discovers interactive elements
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Search and controls */}
      <div className="flex items-center gap-2 mb-2">
        <div className="relative flex-1">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
          <input
            type="text"
            placeholder="Filter elements..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-full pl-7 pr-2 py-1.5 text-xs bg-muted/30 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-primary"
          />
        </div>
        <button
          className="px-2 py-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={expandAll}
          title="Expand all"
        >
          Expand
        </button>
        <button
          className="px-2 py-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={collapseAll}
          title="Collapse all"
        >
          Collapse
        </button>
      </div>

      {/* Tree */}
      <div className="flex-1 overflow-auto">
        {filteredTree.length === 0 ? (
          <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
            No elements match "{filter}"
          </div>
        ) : (
          filteredTree.map(renderNode)
        )}
      </div>

      {/* Selected element details */}
      {selectedElementId && (
        <SelectedElementDetails
          element={elements.find((el) => el.id === selectedElementId)}
          states={getElementStates(selectedElementId)}
          activeStates={activeStates}
          thumbnail={thumbnails?.get(selectedElementId)}
          screenshotData={screenshotData}
        />
      )}
    </div>
  );
}

interface SelectedElementDetailsProps {
  element?: UIBridgeElement;
  states: UIBridgeState[];
  activeStates: string[];
  /** Base64 thumbnail data for the selected element */
  thumbnail?: string;
  /** Base64 screenshot data for cropping larger preview */
  screenshotData?: string;
}

/** Helper to render a state flag badge */
function StateFlagBadge({
  label,
  value,
  showFalse = false,
}: {
  label: string;
  value: boolean | undefined;
  showFalse?: boolean;
}) {
  if (value === undefined && !showFalse) return null;
  if (value === false && !showFalse) return null;

  return (
    <Badge variant={value ? "success" : "muted"} className="text-[10px] px-1.5 py-0">
      {value ? <Check className="w-2.5 h-2.5 mr-0.5" /> : null}
      {label}
    </Badge>
  );
}

function SelectedElementDetails({
  element,
  states,
  activeStates,
  thumbnail,
  screenshotData,
}: SelectedElementDetailsProps) {
  const [showFullImage, setShowFullImage] = useState(false);
  const [largePreview, setLargePreview] = useState<string | null>(null);
  const [detailPreview, setDetailPreview] = useState<string | null>(null);

  // Generate larger preview for detail pane when element changes
  useEffect(() => {
    if (!element || !screenshotData) {
      setDetailPreview(null);
      return;
    }

    let cancelled = false;

    const generatePreview = async () => {
      try {
        const preview = await cropPreview(screenshotData, element.bounds, 192);
        if (!cancelled && preview) {
          setDetailPreview(preview);
        }
      } catch (err) {
        console.error("[SelectedElementDetails] Failed to generate detail preview:", err);
      }
    };

    generatePreview();

    return () => {
      cancelled = true;
    };
  }, [element, screenshotData]);

  // Handle opening the preview modal with a larger cropped image
  const handleOpenPreview = useCallback(async () => {
    if (!element || !screenshotData) {
      setShowFullImage(true);
      return;
    }

    setShowFullImage(true);

    try {
      // Crop a larger preview (max 600px) from the original screenshot
      const preview = await cropPreview(screenshotData, element.bounds, 600);
      setLargePreview(preview);
    } catch (err) {
      console.error("[SelectedElementDetails] Failed to crop large preview:", err);
    }
  }, [element, screenshotData]);

  // Handle closing the preview modal
  const handleClosePreview = useCallback(() => {
    setShowFullImage(false);
    setLargePreview(null);
  }, []);

  if (!element) return null;

  // Get accessibility properties from element or its accessibility object
  const accessibility = element.accessibility;
  const role = element.role || accessibility?.role;
  const accessibleName = element.accessibleName || accessibility?.accessibleName;
  const accessibleDescription = accessibility?.accessibleDescription;
  const hasExplicitRole = accessibility?.hasExplicitRole;
  const implicitRole = accessibility?.implicitRole;

  // State flags (from top-level or accessibility object)
  const isExpanded = element.is_expanded ?? accessibility?.ariaExpanded;
  const isPressed = element.is_pressed;
  const isSelected = element.is_selected ?? accessibility?.ariaSelected;
  const isRequired = element.is_required ?? accessibility?.ariaRequired;
  const isReadonly = element.is_readonly;
  const isInteractive = element.is_interactive;

  // Keyboard accessibility
  const tabIndex = accessibility?.tabIndex;
  const isInTabOrder = accessibility?.isInTabOrder;
  const isKeyboardAccessible = accessibility?.isKeyboardAccessible;

  const hasAccessibilityInfo =
    role ||
    accessibleName ||
    accessibleDescription ||
    isExpanded !== undefined ||
    isPressed !== undefined ||
    isSelected !== undefined ||
    isRequired !== undefined ||
    isReadonly !== undefined ||
    tabIndex !== undefined ||
    isInTabOrder !== undefined ||
    isKeyboardAccessible !== undefined;

  return (
    <div className="mt-2 p-2 bg-muted/30 rounded-md border border-border/50 space-y-3">
      {/* Header with ref and ID */}
      <div className="flex items-center gap-2">
        {element.ref && (
          <Badge variant="default" className="text-[10px] px-1.5 py-0 font-mono bg-primary/80">
            {element.ref}
          </Badge>
        )}
        <span className="text-xs font-semibold truncate">{element.id}</span>
      </div>

      {/* Basic Properties */}
      <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <div className="text-muted-foreground">Type:</div>
        <div>{element.type}</div>
        <div className="text-muted-foreground">Tag:</div>
        <div>{element.tagName}</div>
        {element.label && (
          <>
            <div className="text-muted-foreground">Label:</div>
            <div className="truncate">{element.label}</div>
          </>
        )}
        {element.value !== undefined && (
          <>
            <div className="text-muted-foreground">Value:</div>
            <div className="truncate font-mono">{element.value || "(empty)"}</div>
          </>
        )}
        <div className="text-muted-foreground">Visible:</div>
        <div>{element.visible ? "Yes" : "No"}</div>
        <div className="text-muted-foreground">Enabled:</div>
        <div>{element.enabled ? "Yes" : "No"}</div>
        {states.length > 0 && (
          <>
            <div className="text-muted-foreground">States:</div>
            <div className="flex flex-wrap gap-1">
              {states.map((state) => (
                <Badge
                  key={state.id}
                  variant={activeStates.includes(state.id) ? "default" : "muted"}
                  className="text-[10px]"
                >
                  {state.name || state.id}
                </Badge>
              ))}
            </div>
          </>
        )}
        {element.actions && element.actions.length > 0 && (
          <>
            <div className="text-muted-foreground">Actions:</div>
            <div className="flex flex-wrap gap-1">
              {element.actions.map((action) => (
                <Badge key={action} variant="muted" className="text-[10px]">
                  {action}
                </Badge>
              ))}
            </div>
          </>
        )}
      </div>

      {/* Accessibility Section */}
      {hasAccessibilityInfo && (
        <div className="border-t border-border/30 pt-2">
          <div className="flex items-center gap-1.5 text-xs font-semibold mb-2">
            <Accessibility className="w-3.5 h-3.5 text-primary" />
            Accessibility
          </div>

          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
            {/* Role */}
            {role && (
              <>
                <div className="text-muted-foreground">Role:</div>
                <div className="flex items-center gap-1">
                  <span className="font-mono">{role}</span>
                  {hasExplicitRole !== undefined && (
                    <Badge
                      variant={hasExplicitRole ? "outline" : "muted"}
                      className="text-[9px] px-1 py-0"
                    >
                      {hasExplicitRole ? "explicit" : "implicit"}
                    </Badge>
                  )}
                </div>
              </>
            )}

            {/* Implicit role (if different from current role) */}
            {implicitRole && implicitRole !== role && (
              <>
                <div className="text-muted-foreground">Implicit Role:</div>
                <div className="font-mono text-muted-foreground">{implicitRole}</div>
              </>
            )}

            {/* Accessible Name */}
            {accessibleName && (
              <>
                <div className="text-muted-foreground">Name:</div>
                <div className="truncate" title={accessibleName}>
                  {accessibleName}
                </div>
              </>
            )}

            {/* Accessible Description */}
            {accessibleDescription && (
              <>
                <div className="text-muted-foreground">Description:</div>
                <div className="truncate" title={accessibleDescription}>
                  {accessibleDescription}
                </div>
              </>
            )}
          </div>

          {/* State Flags */}
          <div className="mt-2">
            <div className="text-muted-foreground text-[10px] mb-1">State Flags:</div>
            <div className="flex flex-wrap gap-1">
              <StateFlagBadge label="Interactive" value={isInteractive} />
              <StateFlagBadge label="Expanded" value={isExpanded} />
              <StateFlagBadge label="Pressed" value={isPressed} />
              <StateFlagBadge label="Selected" value={isSelected} />
              <StateFlagBadge label="Required" value={isRequired} />
              <StateFlagBadge label="Readonly" value={isReadonly} />
              {!isInteractive &&
                isExpanded === undefined &&
                isPressed === undefined &&
                isSelected === undefined &&
                isRequired === undefined &&
                isReadonly === undefined && (
                  <span className="text-[10px] text-muted-foreground italic">None</span>
                )}
            </div>
          </div>

          {/* Keyboard Accessibility */}
          {(tabIndex !== undefined ||
            isInTabOrder !== undefined ||
            isKeyboardAccessible !== undefined) && (
            <div className="mt-2">
              <div className="flex items-center gap-1 text-muted-foreground text-[10px] mb-1">
                <Keyboard className="w-3 h-3" />
                Keyboard:
              </div>
              <div className="flex flex-wrap gap-1">
                {tabIndex !== undefined && (
                  <Badge variant="outline" className="text-[10px] px-1.5 py-0">
                    tabIndex: {tabIndex}
                  </Badge>
                )}
                <StateFlagBadge label="In Tab Order" value={isInTabOrder} />
                <StateFlagBadge label="Keyboard Accessible" value={isKeyboardAccessible} />
              </div>
            </div>
          )}
        </div>
      )}

      {/* Preview section */}
      {(detailPreview || thumbnail) && (
        <div className="border-t border-border/30 pt-2">
          <div className="text-xs font-semibold mb-1">Preview</div>
          <img
            src={`data:image/png;base64,${detailPreview || thumbnail}`}
            alt="Element preview"
            className="max-h-48 max-w-48 object-contain rounded border border-border cursor-pointer hover:border-primary transition-colors"
            onClick={handleOpenPreview}
            title="Click to view full size"
          />
        </div>
      )}

      {/* Full image modal */}
      {thumbnail && showFullImage && (
        <ImageViewerModal
          imageData={largePreview || thumbnail}
          title={`Preview: ${element.id}`}
          isOpen={showFullImage}
          onClose={handleClosePreview}
        />
      )}
    </div>
  );
}

export default ElementTreeView;
