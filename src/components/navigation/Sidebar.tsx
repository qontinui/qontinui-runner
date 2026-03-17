/**
 * Sidebar.tsx
 *
 * Two-level sidebar navigation for the qontinui-runner application.
 * Uses the shared qontinui-navigation package for navigation structure and state management.
 *
 * Features:
 * - Main sidebar with collapsible groups
 * - Secondary flyout sidebar that slides out for items with children
 * - Matches the qontinui-web navigation pattern
 * - Persistent expanded/collapsed state in instanceStorage
 * - Keyboard navigation support
 * - Tooltips on hover when collapsed
 * - Workflow Queue for sequencing workflows
 */

import {
  useEffect,
  useState,
  useCallback,
  useRef,
  useReducer,
  useMemo,
  KeyboardEvent,
} from "react";
import { instanceStorage } from "@/lib/instance-storage";
import {
  Play,
  Bot,
  ScrollText,
  BookOpen,
  BookText,
  Bug,
  Camera,
  Calendar,
  CheckCircle2,
  Settings,
  HelpCircle,
  ChevronDown,
  ChevronRight,
  PanelLeftClose,
  PanelLeft,
  Eye,
  LucideIcon,
  Sparkles,
  TestTube,
  Activity,
  History,
  MousePointer2,
  Layers,
  LayoutDashboard,
  ClipboardCheck,
  Zap,
  Image,
  ClipboardList,
  FileText,
  FileSearch,
  BarChart3,
  Cloud,
  FolderOpen,
  Tag,
  Database,
  User,
  HardDrive,
  Wrench,
  Download,
  Archive,
  FlaskConical,
  Globe,
  Code,
  Puzzle,
  ShieldCheck,
  GitBranch,
  Monitor,
  Palette,
  Bell,
  Key,
  CreditCard,
  X,
  Accessibility,
  Brain,
  Webhook,
  Wifi,
  Terminal,
  AlertCircle,
  ListChecks,
  RotateCcw,
  MessageSquare,
  Server,
  Workflow,
  Cpu,
  Plug,
  Network,
  Repeat,
} from "lucide-react";

// Import shared navigation structure and state management
import {
  type NavigationItem as SharedNavigationItem,
  type NavigationGroup as SharedNavigationGroup,
  type IconName,
  getRunnerNavigation,
  getChildrenForPlatform,
  setProductMode,
  STORAGE_KEYS,
  createInitialState,
  navigationReducer,
  navigationActions,
  isGroupExpanded,
  serializeState,
  deserializeState,
} from "qontinui-navigation";
import { useProductMode, type ProductMode } from "@/contexts/ProductModeContext";

// ============================================================================
// Icon Mapping
// ============================================================================

const ICON_MAP: Record<IconName, LucideIcon> = {
  Play,
  Activity,
  History,
  Bot,
  Bug,
  Settings,
  HelpCircle,
  ChevronDown,
  ChevronRight,
  ScrollText,
  LayoutDashboard,
  ClipboardCheck,
  Zap,
  Image,
  ClipboardList,
  FileText,
  FileSearch,
  TestTube,
  BarChart3,
  Database,
  Cloud,
  BookOpen,
  BookText,
  CheckCircle2,
  Sparkles,
  MousePointer2,
  Layers,
  FlaskConical,
  Camera,
  GitBranch,
  Globe,
  Code,
  Puzzle,
  ShieldCheck,
  FolderOpen,
  Tag,
  Plug,
  Calendar,
  User,
  HardDrive,
  Wrench,
  Download,
  Archive,
  Monitor,
  Palette,
  Bell,
  Key,
  CreditCard,
  Accessibility,
  Brain,
  Webhook,
  Wifi,
  Terminal,
  AlertCircle,
  ListChecks,
  RotateCcw,
  MessageSquare,
  Server,
  Workflow,
  Cpu,
  Network,
  Repeat,
};

function getIconComponent(iconName: IconName): LucideIcon {
  return ICON_MAP[iconName] || HelpCircle;
}

// ============================================================================
// Types
// ============================================================================

export interface SidebarProps {
  activeTab: string;
  onTabChange: (tab: string) => void;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
}

interface ResolvedNavigationItem {
  id: string;
  label: string;
  icon: LucideIcon;
  description?: string;
  hasChildren?: boolean;
  selectsFirstChild?: boolean;
  badge?: { type: string; value?: string | number; variant?: string };
  hiddenInProd?: boolean; // For showing "dev" badge on items hidden in production
}

interface ResolvedNavigationGroup {
  id: string;
  label: string;
  items: ResolvedNavigationItem[];
  defaultExpanded?: boolean;
}

// ============================================================================
// Navigation Data Transformation
// ============================================================================

function transformItem(item: SharedNavigationItem): ResolvedNavigationItem {
  return {
    id: item.id,
    label: item.label,
    icon: getIconComponent(item.icon),
    description: item.description,
    hasChildren: item.hasChildren,
    selectsFirstChild: item.selectsFirstChild,
    hiddenInProd: item.hiddenInProd,
  };
}

function transformGroup(group: SharedNavigationGroup): ResolvedNavigationGroup {
  return {
    id: group.id,
    label: group.label,
    items: group.items.map(transformItem),
    defaultExpanded: group.defaultExpanded,
  };
}

function getChildItems(parentId: string): ResolvedNavigationItem[] {
  const children = getChildrenForPlatform(parentId, "runner");
  return children.map(transformItem);
}

function buildNavigationGroups(): ResolvedNavigationGroup[] {
  const sharedGroups = getRunnerNavigation();
  return sharedGroups.map(transformGroup);
}

// ============================================================================
// Constants
// ============================================================================

const SIDEBAR_WIDTH_EXPANDED = 200;
const SIDEBAR_WIDTH_COLLAPSED = 56;
const FLYOUT_WIDTH = 260;

// ============================================================================
// Tooltip Component
// ============================================================================

interface TooltipProps {
  content: string;
  children: React.ReactNode;
  disabled?: boolean;
}

function Tooltip({ content, children, disabled }: TooltipProps) {
  const [visible, setVisible] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const triggerRef = useRef<HTMLDivElement>(null);
  const timeoutRef = useRef<number | null>(null);

  const showTooltip = useCallback(() => {
    if (disabled) return;
    timeoutRef.current = window.setTimeout(() => {
      if (triggerRef.current) {
        const rect = triggerRef.current.getBoundingClientRect();
        setPosition({ top: rect.top + rect.height / 2, left: rect.right + 8 });
      }
      setVisible(true);
    }, 200);
  }, [disabled]);

  const hideTooltip = useCallback(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    setVisible(false);
  }, []);

  useEffect(() => {
    return () => {
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, []);

  return (
    <div
      ref={triggerRef}
      onMouseEnter={showTooltip}
      onMouseLeave={hideTooltip}
      onFocus={showTooltip}
      onBlur={hideTooltip}
      className="relative"
    >
      {children}
      {visible && !disabled && (
        <div
          className="fixed z-50 px-2 py-1 text-sm bg-popover text-popover-foreground
                     border border-border rounded-md shadow-lg whitespace-nowrap
                     animate-in fade-in-0 zoom-in-95 duration-100"
          style={{
            left: position.left,
            top: position.top,
            transform: "translateY(-50%)",
          }}
        >
          {content}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Navigation Item Component
// ============================================================================

interface NavItemProps {
  item: ResolvedNavigationItem;
  isActive: boolean;
  isParentActive?: boolean;
  collapsed: boolean;
  onClick: () => void;
  onKeyDown: (e: KeyboardEvent) => void;
  tabIndex: number;
  dataNavItem: string;
}

function NavItem({
  item,
  isActive,
  isParentActive,
  collapsed,
  onClick,
  onKeyDown,
  tabIndex,
  dataNavItem,
}: NavItemProps) {
  const Icon = item.icon;
  const showActiveState = isActive || isParentActive;

  const button = (
    <button
      data-nav-item={dataNavItem}
      onClick={onClick}
      onKeyDown={onKeyDown}
      tabIndex={tabIndex}
      className={`
        w-full flex items-center gap-3 py-2 rounded-md text-sm font-medium
        transition-all duration-200 outline-none
        focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2
        focus-visible:ring-offset-background
        px-3
        ${
          showActiveState
            ? "bg-primary/15 text-primary border-l-2 border-primary ml-[-1px]"
            : "text-muted-foreground hover:text-foreground hover:bg-muted/30"
        }
        ${collapsed ? "justify-center px-0" : ""}
      `}
      aria-current={isActive ? "page" : undefined}
    >
      <Icon className={`w-4 h-4 flex-shrink-0 ${showActiveState ? "text-primary" : ""}`} />
      {!collapsed && (
        <>
          <span className="truncate flex-1 text-left">{item.label}</span>
          {item.hiddenInProd && (
            <span className="text-[9px] font-bold uppercase px-1 py-0.5 rounded bg-gray-500/20 text-gray-400 border border-gray-500/30">
              dev
            </span>
          )}
          {item.hasChildren && <ChevronRight className="w-3 h-3 opacity-50" />}
        </>
      )}
    </button>
  );

  if (collapsed) {
    return <Tooltip content={item.label}>{button}</Tooltip>;
  }

  return button;
}

// ============================================================================
// Flyout Item Component (for secondary sidebar)
// ============================================================================

interface FlyoutItemProps {
  item: ResolvedNavigationItem;
  isActive: boolean;
  onClick: () => void;
  index: number;
}

function FlyoutItem({ item, isActive, onClick, index }: FlyoutItemProps) {
  const Icon = item.icon;

  return (
    <button
      onClick={onClick}
      className={`
        w-full p-3 rounded-lg flex items-start gap-3 transition-all duration-200 text-left group
        animate-in fade-in slide-in-from-left-2
        ${isActive ? "bg-primary/10" : "hover:bg-muted/30"}
      `}
      style={{
        animationDelay: `${index * 30}ms`,
        animationFillMode: "backwards",
      }}
      aria-current={isActive ? "page" : undefined}
    >
      {/* Icon */}
      <div
        className={`
          flex-shrink-0 p-2 rounded-lg transition-all duration-200
          ${isActive ? "bg-primary/15" : "bg-muted/30 group-hover:bg-muted/50"}
        `}
      >
        <Icon
          className={`w-4 h-4 ${isActive ? "text-primary" : "text-muted-foreground group-hover:text-foreground"}`}
        />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 pt-0.5">
        <div className="flex items-center gap-2">
          <span
            className={`
              font-medium truncate
              ${isActive ? "text-primary" : "text-muted-foreground group-hover:text-foreground"}
            `}
          >
            {item.label}
          </span>
          {item.hiddenInProd && (
            <span className="text-[9px] font-bold uppercase px-1 py-0.5 rounded bg-gray-500/20 text-gray-400 border border-gray-500/30">
              dev
            </span>
          )}
        </div>
        {item.description && (
          <p className="text-xs text-muted-foreground/70 mt-0.5 line-clamp-2">{item.description}</p>
        )}
      </div>

      {/* Active indicator */}
      {isActive && <div className="flex-shrink-0 w-1.5 h-1.5 rounded-full bg-primary mt-2" />}
    </button>
  );
}

// ============================================================================
// Navigation Group Component
// ============================================================================

interface NavGroupProps {
  group: ResolvedNavigationGroup;
  isExpanded: boolean;
  onToggle: () => void;
  activeTab: string;
  onItemClick: (item: ResolvedNavigationItem) => void;
  collapsed: boolean;
  onKeyDown: (e: KeyboardEvent, itemId: string) => void;
  getTabIndex: (itemId: string) => number;
  openFlyoutId: string | null;
}

function NavGroup({
  group,
  isExpanded,
  onToggle,
  activeTab,
  onItemClick,
  collapsed,
  onKeyDown,
  getTabIndex,
  openFlyoutId,
}: NavGroupProps) {
  const ChevronIcon = isExpanded ? ChevronDown : ChevronRight;

  if (collapsed) {
    return (
      <div className="space-y-1">
        {group.items.map((item) => {
          const isParentActive =
            item.hasChildren && getChildItems(item.id).some((child) => child.id === activeTab);

          return (
            <NavItem
              key={item.id}
              item={item}
              isActive={activeTab === item.id}
              isParentActive={isParentActive}
              collapsed={collapsed}
              onClick={() => onItemClick(item)}
              onKeyDown={(e) => onKeyDown(e, item.id)}
              tabIndex={getTabIndex(item.id)}
              dataNavItem={item.id}
            />
          );
        })}
      </div>
    );
  }

  return (
    <div className="space-y-1">
      <button
        onClick={onToggle}
        className="w-full flex items-center justify-between px-2 py-1.5 text-xs
                   font-semibold text-muted-foreground/70 hover:text-muted-foreground
                   transition-colors uppercase tracking-wider outline-none
                   focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-inset"
        aria-expanded={isExpanded}
      >
        <span>{group.label}</span>
        <ChevronIcon className="w-3 h-3" />
      </button>

      <div
        className={`
          space-y-0.5 overflow-hidden transition-all duration-200
          ${isExpanded ? "opacity-100 max-h-[600px]" : "opacity-0 max-h-0"}
        `}
      >
        {group.items.map((item) => {
          const isParentActive =
            item.hasChildren && getChildItems(item.id).some((child) => child.id === activeTab);
          const isFlyoutOpen = openFlyoutId === item.id;

          return (
            <NavItem
              key={item.id}
              item={item}
              isActive={activeTab === item.id}
              isParentActive={isParentActive || isFlyoutOpen}
              collapsed={collapsed}
              onClick={() => onItemClick(item)}
              onKeyDown={(e) => onKeyDown(e, item.id)}
              tabIndex={getTabIndex(item.id)}
              dataNavItem={item.id}
            />
          );
        })}
      </div>
    </div>
  );
}

// ============================================================================
// Flyout Sidebar Component
// ============================================================================

interface FlyoutSidebarProps {
  isOpen: boolean;
  parentId: string | null;
  parentLabel: string;
  items: ResolvedNavigationItem[];
  activeTab: string;
  onTabChange: (tab: string) => void;
  onClose: () => void;
}

function FlyoutSidebar({
  isOpen,
  parentLabel,
  items,
  activeTab,
  onTabChange,
  onClose,
}: FlyoutSidebarProps) {
  const flyoutRef = useRef<HTMLDivElement>(null);

  // Handle click outside to close
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (flyoutRef.current && !flyoutRef.current.contains(event.target as Node)) {
        const sidebar = document.querySelector('[data-sidebar="true"]');
        if (sidebar && sidebar.contains(event.target as Node)) {
          return;
        }
        onClose();
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      document.addEventListener("keydown", handleEscape as unknown as EventListener);
    }

    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleEscape as unknown as EventListener);
    };
  }, [isOpen, onClose]);

  return (
    <div
      ref={flyoutRef}
      className={`
        h-full flex flex-col bg-card border-r border-border/50
        transition-all duration-300 ease-in-out overflow-hidden
        ${isOpen ? "opacity-100" : "opacity-0 pointer-events-none"}
      `}
      style={{ width: isOpen ? FLYOUT_WIDTH : 0 }}
    >
      {/* Header */}
      <div className="h-12 border-b border-border/50 flex items-center justify-between px-4 bg-gradient-to-r from-primary/5 to-transparent">
        <div className="flex items-center gap-3">
          <div className="w-1 h-6 rounded-full bg-primary" />
          <h2 className="text-sm font-semibold text-foreground">{parentLabel}</h2>
        </div>
        <button
          onClick={onClose}
          className="p-1.5 rounded-md text-muted-foreground hover:text-foreground
                     hover:bg-muted/50 transition-colors"
          aria-label="Close flyout"
        >
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Items */}
      <div className="flex-1 overflow-y-auto p-3 space-y-1">
        {items.map((item, index) => (
          <FlyoutItem
            key={item.id}
            item={item}
            isActive={activeTab === item.id}
            onClick={() => {
              onTabChange(item.id);
              onClose();
            }}
            index={index}
          />
        ))}
      </div>

      {/* Bottom gradient */}
      <div className="absolute bottom-0 left-0 right-0 h-16 pointer-events-none bg-gradient-to-t from-primary/5 to-transparent" />
    </div>
  );
}

// ============================================================================
// Product Mode Switcher Component
// ============================================================================

interface ProductModeSwitcherProps {
  mode: ProductMode;
  onModeChange: (mode: ProductMode) => void;
  collapsed: boolean;
}

function ProductModeSwitcher({ mode, onModeChange, collapsed }: ProductModeSwitcherProps) {
  if (collapsed) {
    return (
      <Tooltip content={mode === "ai" ? "Switch to Visual" : "Switch to AI Dev"}>
        <button
          onClick={() => onModeChange(mode === "ai" ? "visual" : "ai")}
          aria-label={mode === "ai" ? "Switch to Visual mode" : "Switch to AI Dev mode"}
          className="w-full flex items-center justify-center py-2 rounded-md
                     hover:bg-muted/30 transition-colors"
        >
          {mode === "ai" ? (
            <Bot className="w-4 h-4 text-primary" />
          ) : (
            <Eye className="w-4 h-4 text-cyan-400" />
          )}
        </button>
      </Tooltip>
    );
  }

  return (
    <div
      className="flex items-center gap-0.5 p-0.5 rounded-md bg-muted/20 border border-border/50"
      role="group"
      aria-label="Product mode"
    >
      <button
        onClick={() => onModeChange("ai")}
        aria-pressed={mode === "ai"}
        className={`
          flex-1 flex items-center justify-center gap-1.5 px-2 py-1 rounded text-xs font-medium
          transition-all duration-150
          ${
            mode === "ai"
              ? "bg-card text-foreground shadow-sm"
              : "text-muted-foreground hover:text-foreground"
          }
        `}
      >
        <Bot className="w-3.5 h-3.5" />
        AI Dev
      </button>
      <button
        onClick={() => onModeChange("visual")}
        aria-pressed={mode === "visual"}
        className={`
          flex-1 flex items-center justify-center gap-1.5 px-2 py-1 rounded text-xs font-medium
          transition-all duration-150
          ${
            mode === "visual"
              ? "bg-card text-foreground shadow-sm"
              : "text-muted-foreground hover:text-foreground"
          }
        `}
      >
        <Eye className="w-3.5 h-3.5" />
        Visual
      </button>
    </div>
  );
}

// ============================================================================
// Sidebar Component
// ============================================================================

export function Sidebar({ activeTab, onTabChange, collapsed, onCollapsedChange }: SidebarProps) {
  // Use shared navigation state management
  const [navState, dispatch] = useReducer(navigationReducer, undefined, () => {
    try {
      const stored = instanceStorage.getItem(STORAGE_KEYS.state);
      if (stored) {
        const parsed = deserializeState(stored);
        if (parsed) {
          return {
            activeItemId: activeTab,
            expandedGroups: parsed.expandedGroups ?? new Set(["run", "system"]),
            expandedItems: parsed.expandedItems ?? new Set(),
            secondarySidebar: { isOpen: false, parentId: null, items: [] },
            isCollapsed: collapsed,
          };
        }
      }
    } catch {
      // Ignore
    }
    return createInitialState({
      activeItemId: activeTab,
      isCollapsed: collapsed,
    });
  });

  // Product mode filtering
  const { mode: productMode, setMode: setProductModeState } = useProductMode();

  // Sync product mode to shared navigation package and rebuild groups
  const navigationGroups = useMemo(() => {
    setProductMode(productMode);
    return buildNavigationGroups();
  }, [productMode]);

  // Flyout state
  const [openFlyout, setOpenFlyout] = useState<{
    id: string;
    label: string;
    items: ResolvedNavigationItem[];
  } | null>(null);

  const [focusedItemId, setFocusedItemId] = useState<string | null>(null);

  // Keep a ref to navState so effects can read it without depending on it
  const navStateRef = useRef(navState);
  navStateRef.current = navState;

  // Persist state changes
  useEffect(() => {
    instanceStorage.setItem(STORAGE_KEYS.state, serializeState(navState));
  }, [navState]);

  // Auto-expand the group containing the active tab (accordion-aware).
  // Uses navStateRef to read current state without creating a dependency cycle
  // (this effect dispatches actions that update navState).
  useEffect(() => {
    if (collapsed) return;
    const currentState = navStateRef.current;
    const activeGroup = navigationGroups.find((g) =>
      g.items.some(
        (item) =>
          item.id === activeTab ||
          (item.hasChildren && getChildItems(item.id).some((child) => child.id === activeTab)),
      ),
    );
    if (activeGroup && !isGroupExpanded(currentState, activeGroup.id)) {
      // Collapse all other groups, then expand the active one
      navigationGroups.forEach((g) => {
        if (g.id !== activeGroup.id && isGroupExpanded(currentState, g.id)) {
          dispatch(navigationActions.collapseGroup(g.id));
        }
      });
      dispatch(navigationActions.expandGroup(activeGroup.id));
    }
  }, [activeTab, collapsed, navigationGroups]);

  const toggleGroup = useCallback(
    (groupId: string) => {
      const isCurrentlyExpanded = isGroupExpanded(navState, groupId);

      if (!isCurrentlyExpanded) {
        // Accordion: collapse all other groups before expanding this one
        navigationGroups.forEach((g) => {
          if (g.id !== groupId && isGroupExpanded(navState, g.id)) {
            dispatch(navigationActions.collapseGroup(g.id));
          }
        });
      }

      dispatch(navigationActions.toggleGroup(groupId));
    },
    [navState, navigationGroups],
  );

  const openFlyoutSidebar = useCallback((item: ResolvedNavigationItem) => {
    const children = getChildItems(item.id);
    setOpenFlyout({
      id: item.id,
      label: item.label,
      items: children,
    });
  }, []);

  const closeFlyout = useCallback(() => {
    setOpenFlyout(null);
  }, []);

  const handleItemClick = useCallback(
    (item: ResolvedNavigationItem) => {
      if (item.hasChildren) {
        if (openFlyout?.id === item.id) {
          closeFlyout();
        } else {
          openFlyoutSidebar(item);
          if (item.selectsFirstChild) {
            const children = getChildItems(item.id);
            if (children.length > 0) {
              onTabChange(children[0].id);
            }
          }
        }
      } else {
        onTabChange(item.id);
        closeFlyout();
      }
    },
    [openFlyout, closeFlyout, openFlyoutSidebar, onTabChange],
  );

  const flattenedItems = useMemo(
    () =>
      navigationGroups.flatMap((group) =>
        group.items.map((item) => ({
          ...item,
          groupId: group.id,
        })),
      ),
    [navigationGroups],
  );

  const handleKeyDown = useCallback(
    (e: KeyboardEvent, currentItemId: string) => {
      const currentIndex = flattenedItems.findIndex((item) => item.id === currentItemId);

      switch (e.key) {
        case "ArrowDown": {
          e.preventDefault();
          const nextIndex = Math.min(currentIndex + 1, flattenedItems.length - 1);
          const nextItem = flattenedItems[nextIndex];
          if (nextItem) {
            setFocusedItemId(nextItem.id);
            if (!isGroupExpanded(navState, nextItem.groupId) && !collapsed) {
              dispatch(navigationActions.expandGroup(nextItem.groupId));
            }
          }
          break;
        }
        case "ArrowUp": {
          e.preventDefault();
          const prevIndex = Math.max(currentIndex - 1, 0);
          const prevItem = flattenedItems[prevIndex];
          if (prevItem) {
            setFocusedItemId(prevItem.id);
            if (!isGroupExpanded(navState, prevItem.groupId) && !collapsed) {
              dispatch(navigationActions.expandGroup(prevItem.groupId));
            }
          }
          break;
        }
        case "ArrowRight": {
          e.preventDefault();
          const currentItem = flattenedItems[currentIndex];
          if (currentItem?.hasChildren) {
            openFlyoutSidebar(currentItem);
          }
          break;
        }
        case "ArrowLeft": {
          e.preventDefault();
          if (openFlyout) {
            closeFlyout();
          }
          break;
        }
        case "Enter":
        case " ": {
          e.preventDefault();
          const currentItem = flattenedItems[currentIndex];
          if (currentItem) {
            handleItemClick(currentItem);
          }
          break;
        }
      }
    },
    [
      flattenedItems,
      navState,
      collapsed,
      openFlyout,
      openFlyoutSidebar,
      closeFlyout,
      handleItemClick,
    ],
  );

  useEffect(() => {
    if (focusedItemId) {
      const element = document.querySelector(
        `[data-nav-item="${focusedItemId}"]`,
      ) as HTMLButtonElement;
      element?.focus();
    }
  }, [focusedItemId]);

  const getTabIndex = useCallback(
    (itemId: string) => {
      const firstItem = flattenedItems[0];
      return firstItem?.id === itemId ? 0 : -1;
    },
    [flattenedItems],
  );

  return (
    <div className="flex h-full" data-sidebar="true">
      {/* Main Sidebar */}
      <nav
        data-tutorial-id="sidebar"
        className={`
          h-full flex flex-col bg-card border-r border-border/50
          transition-all duration-300 ease-in-out flex-shrink-0
        `}
        style={{ width: collapsed ? SIDEBAR_WIDTH_COLLAPSED : SIDEBAR_WIDTH_EXPANDED }}
        aria-label="Main navigation"
      >
        {/* Product Mode Switcher */}
        <div className="px-2 pt-2 pb-0">
          <ProductModeSwitcher
            mode={productMode}
            onModeChange={setProductModeState}
            collapsed={collapsed}
          />
        </div>

        {/* Navigation Groups */}
        <div className="flex-1 overflow-y-auto py-2 px-2 space-y-4">
          {navigationGroups.map((group) => (
            <NavGroup
              key={group.id}
              group={group}
              isExpanded={collapsed || isGroupExpanded(navState, group.id)}
              onToggle={() => toggleGroup(group.id)}
              activeTab={activeTab}
              onItemClick={handleItemClick}
              collapsed={collapsed}
              onKeyDown={handleKeyDown}
              getTabIndex={getTabIndex}
              openFlyoutId={openFlyout?.id ?? null}
            />
          ))}
        </div>

        {/* Collapse Toggle Button */}
        <div className="border-t border-border/50 p-2">
          <Tooltip
            content={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            disabled={!collapsed}
          >
            <button
              onClick={() => onCollapsedChange(!collapsed)}
              className={`
                w-full flex items-center gap-3 px-3 py-2 rounded-md text-sm
                text-muted-foreground hover:text-foreground hover:bg-muted/30
                transition-colors outline-none
                focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2
                focus-visible:ring-offset-background
                ${collapsed ? "justify-center px-0" : ""}
              `}
              aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            >
              {collapsed ? (
                <PanelLeft className="w-4 h-4" />
              ) : (
                <>
                  <PanelLeftClose className="w-4 h-4" />
                  <span>Collapse</span>
                </>
              )}
            </button>
          </Tooltip>
        </div>
      </nav>

      {/* Flyout Sidebar */}
      <FlyoutSidebar
        isOpen={!!openFlyout}
        parentId={openFlyout?.id ?? null}
        parentLabel={openFlyout?.label ?? ""}
        items={openFlyout?.items ?? []}
        activeTab={activeTab}
        onTabChange={onTabChange}
        onClose={closeFlyout}
      />
    </div>
  );
}

// ============================================================================
// Hook for managing sidebar state with instanceStorage persistence
// ============================================================================

export function useSidebarState() {
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      return instanceStorage.getJSON(STORAGE_KEYS.collapsed, false);
    } catch {
      return false;
    }
  });

  const handleCollapsedChange = useCallback((value: boolean) => {
    setCollapsed(value);
    instanceStorage.setJSON(STORAGE_KEYS.collapsed, value);
  }, []);

  return {
    collapsed,
    onCollapsedChange: handleCollapsedChange,
  };
}

export default Sidebar;
