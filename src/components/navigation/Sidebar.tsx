/**
 * Sidebar.tsx
 *
 * Collapsible left sidebar navigation for the qontinui-runner application.
 *
 * Features:
 * - Collapsible sidebar (220px expanded, 56px collapsed icons-only)
 * - 6 collapsible groups: RUN, OBSERVE, BUILD, CONFIGURE, SCHEDULE, SYSTEM
 * - Active state with distinct background and colored left border
 * - Group expand/collapse on header click
 * - Persistent expanded/collapsed state in localStorage
 * - Keyboard navigation (arrow keys, Enter to select)
 * - Tooltips on hover when collapsed
 * - Smooth transitions for expand/collapse animations
 * - Indented items for visual grouping (run-specific pages under Run Dashboard)
 *
 * Navigation Structure:
 * - RUN: Execute, Active (unified monitoring), History
 * - OBSERVE: General Logs, Run Dashboard (+ indented run-specific pages), Discoveries
 * - BUILD: Library, Workflows, Scripts, Capture
 * - CONFIGURE: Log Sources, Findings
 * - SCHEDULE: Tasks
 * - SYSTEM: Settings, Help
 */

import { useEffect, useState, useCallback, useRef, KeyboardEvent } from "react";
import {
  Play,
  Bot,
  ScrollText,
  BookOpen,
  Camera,
  Calendar,
  Settings,
  HelpCircle,
  ChevronDown,
  ChevronRight,
  PanelLeftClose,
  PanelLeft,
  LucideIcon,
  Sparkles,
  TestTube,
  Activity,
  History,
  MousePointer2,
  // Observe icons
  LayoutDashboard,
  Zap,
  Image,
  ClipboardList,
  FileText,
  FileSearch,
  BarChart3,
  Cloud,
  // Configure icons
  FolderOpen,
  Tag,
  Database,
  // Settings sub-page icons
  User,
  HardDrive,
  Wrench,
  Download,
  Archive,
  FlaskConical,
} from "lucide-react";

// ============================================================================
// Types
// ============================================================================

export interface SidebarProps {
  activeTab: string;
  onTabChange: (tab: string) => void;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
}

interface NavigationItem {
  id: string;
  label: string;
  icon: LucideIcon;
  /** Whether this item should be visually indented (for sub-items) */
  indent?: boolean;
  /** If set, this item is a child of another item and will be collapsed with it */
  parentId?: string;
  /** If true, this item has children and can be toggled to show/hide them */
  hasChildren?: boolean;
}

interface NavigationGroup {
  id: string;
  label: string;
  items: NavigationItem[];
  defaultExpanded?: boolean;
}

// ============================================================================
// Constants
// ============================================================================

const SIDEBAR_COLLAPSED_KEY = "qontinui-sidebar-collapsed";
const SIDEBAR_GROUPS_KEY = "qontinui-sidebar-groups";
const SIDEBAR_SUBGROUPS_KEY = "qontinui-sidebar-subgroups";

const SIDEBAR_WIDTH_EXPANDED = 220;
const SIDEBAR_WIDTH_COLLAPSED = 56;

const NAVIGATION_GROUPS: NavigationGroup[] = [
  {
    id: "run",
    label: "RUN",
    defaultExpanded: true,
    items: [
      { id: "run", label: "Execute", icon: Play },
      { id: "active", label: "Active", icon: Activity },
      { id: "history", label: "History", icon: History },
    ],
  },
  {
    id: "observe",
    label: "OBSERVE",
    defaultExpanded: false,
    items: [
      // Standalone: General logs (not run-specific)
      { id: "general-logs", label: "General Logs", icon: ScrollText },
      // Run Dashboard + collapsible run-specific pages
      { id: "run-dashboard", label: "Run Dashboard", icon: LayoutDashboard, hasChildren: true },
      { id: "run-actions", label: "Actions", icon: Zap, indent: true, parentId: "run-dashboard" },
      {
        id: "run-image",
        label: "Image Recognition",
        icon: Image,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-summary",
        label: "Summary",
        icon: ClipboardList,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-findings",
        label: "Findings",
        icon: FileText,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-verification",
        label: "Verification",
        icon: FileSearch,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-tests",
        label: "Test Results",
        icon: TestTube,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-ai-output",
        label: "AI Output",
        icon: Bot,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-statistics",
        label: "Statistics",
        icon: BarChart3,
        indent: true,
        parentId: "run-dashboard",
      },
      {
        id: "run-ai-data",
        label: "AI Data View",
        icon: Database,
        indent: true,
        parentId: "run-dashboard",
      },
      // Standalone: Discoveries sync queue
      { id: "discoveries", label: "Discoveries", icon: Cloud },
    ],
  },
  {
    id: "build",
    label: "BUILD",
    defaultExpanded: false,
    items: [
      { id: "library", label: "Library", icon: BookOpen },
      { id: "workflow-builder", label: "AI Workflows", icon: Sparkles },
      { id: "gui-workflow-builder", label: "GUI Workflows", icon: MousePointer2 },
      { id: "script-builder", label: "Scripts", icon: TestTube },
      { id: "test-builder", label: "Tests", icon: FlaskConical },
      { id: "capture", label: "Capture", icon: Camera },
    ],
  },
  {
    id: "configure",
    label: "CONFIGURE",
    defaultExpanded: false,
    items: [
      { id: "config-log-sources", label: "Log Sources", icon: FolderOpen },
      { id: "config-findings", label: "Findings", icon: Tag },
    ],
  },
  {
    id: "schedule",
    label: "SCHEDULE",
    defaultExpanded: false,
    items: [{ id: "tasks", label: "Tasks", icon: Calendar }],
  },
  {
    id: "system",
    label: "SYSTEM",
    defaultExpanded: true,
    items: [
      { id: "settings", label: "Settings", icon: Settings, hasChildren: true },
      { id: "settings-account", label: "Account", icon: User, indent: true, parentId: "settings" },
      { id: "settings-ai", label: "AI Providers", icon: Bot, indent: true, parentId: "settings" },
      {
        id: "settings-playwright",
        label: "Playwright",
        icon: FlaskConical,
        indent: true,
        parentId: "settings",
      },
      {
        id: "settings-general",
        label: "Preferences",
        icon: Settings,
        indent: true,
        parentId: "settings",
      },
      {
        id: "settings-storage",
        label: "Storage",
        icon: HardDrive,
        indent: true,
        parentId: "settings",
      },
      { id: "settings-backup", label: "Backup", icon: Archive, indent: true, parentId: "settings" },
      { id: "settings-debug", label: "Debug", icon: Wrench, indent: true, parentId: "settings" },
      {
        id: "settings-updates",
        label: "Updates",
        icon: Download,
        indent: true,
        parentId: "settings",
      },
      { id: "help", label: "Help", icon: HelpCircle },
    ],
  },
];

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
  const [position, setPosition] = useState({ top: 0 });
  const triggerRef = useRef<HTMLDivElement>(null);
  const timeoutRef = useRef<number | null>(null);

  const showTooltip = useCallback(() => {
    if (disabled) return;
    timeoutRef.current = window.setTimeout(() => {
      if (triggerRef.current) {
        const rect = triggerRef.current.getBoundingClientRect();
        setPosition({ top: rect.top + rect.height / 2 });
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
            left: SIDEBAR_WIDTH_COLLAPSED + 8,
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
  item: NavigationItem;
  isActive: boolean;
  collapsed: boolean;
  onClick: () => void;
  onKeyDown: (e: KeyboardEvent) => void;
  tabIndex: number;
  dataNavItem: string;
  /** For parent items: callback to toggle children visibility */
  onToggleSubgroup?: () => void;
}

function NavItem({
  item,
  isActive,
  collapsed,
  onClick,
  onKeyDown,
  tabIndex,
  dataNavItem,
  onToggleSubgroup,
}: NavItemProps) {
  const Icon = item.icon;
  const isIndented = item.indent && !collapsed;
  const hasChildren = item.hasChildren && !collapsed;

  const handleClick = () => {
    onClick();
    // If this is a parent item, also toggle its children
    if (hasChildren && onToggleSubgroup) {
      onToggleSubgroup();
    }
  };

  const button = (
    <button
      data-nav-item={dataNavItem}
      onClick={handleClick}
      onKeyDown={onKeyDown}
      tabIndex={tabIndex}
      className={`
        w-full flex items-center gap-3 py-2 rounded-md text-sm font-medium
        transition-all duration-200 outline-none
        focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2
        focus-visible:ring-offset-background
        ${isIndented ? "pl-6 pr-3" : "px-3"}
        ${
          isActive
            ? "bg-primary/15 text-primary border-l-2 border-primary ml-[-1px]"
            : "text-muted-foreground hover:text-foreground hover:bg-muted/30"
        }
        ${collapsed ? "justify-center px-0" : ""}
      `}
      aria-current={isActive ? "page" : undefined}
    >
      <Icon
        className={`w-4 h-4 flex-shrink-0 ${isActive ? "text-primary" : ""} ${isIndented ? "w-3.5 h-3.5" : ""}`}
      />
      {!collapsed && <span className="truncate">{item.label}</span>}
    </button>
  );

  if (collapsed) {
    return <Tooltip content={item.label}>{button}</Tooltip>;
  }

  return button;
}

// ============================================================================
// Navigation Group Component
// ============================================================================

interface NavGroupProps {
  group: NavigationGroup;
  isExpanded: boolean;
  onToggle: () => void;
  activeTab: string;
  onTabChange: (tab: string) => void;
  collapsed: boolean;
  onKeyDown: (e: KeyboardEvent, itemId: string) => void;
  getTabIndex: (itemId: string) => number;
  /** State for which subgroups (parent items) are expanded */
  expandedSubgroups: Record<string, boolean>;
  /** Callback to toggle a subgroup */
  onToggleSubgroup: (parentId: string) => void;
}

function NavGroup({
  group,
  isExpanded,
  onToggle,
  activeTab,
  onTabChange,
  collapsed,
  onKeyDown,
  getTabIndex,
  expandedSubgroups,
  onToggleSubgroup,
}: NavGroupProps) {
  const ChevronIcon = isExpanded ? ChevronDown : ChevronRight;

  // Filter items based on parent expansion state (only when not collapsed sidebar)
  const visibleItems = collapsed
    ? group.items
    : group.items.filter((item) => {
        // If item has no parent, always show it
        if (!item.parentId) return true;
        // If parent is expanded, show the child
        return expandedSubgroups[item.parentId] ?? false;
      });

  // In collapsed mode, only show icons without headers
  if (collapsed) {
    return (
      <div className="space-y-1">
        {visibleItems.map((item) => (
          <NavItem
            key={item.id}
            item={item}
            isActive={activeTab === item.id}
            collapsed={collapsed}
            onClick={() => onTabChange(item.id)}
            onKeyDown={(e) => onKeyDown(e, item.id)}
            tabIndex={getTabIndex(item.id)}
            dataNavItem={item.id}
          />
        ))}
      </div>
    );
  }

  return (
    <div className="space-y-1">
      {/* Group Header */}
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

      {/* Group Items */}
      <div
        className={`
          space-y-0.5 overflow-hidden transition-all duration-200
          ${isExpanded ? "opacity-100 max-h-[600px]" : "opacity-0 max-h-0"}
        `}
      >
        {visibleItems.map((item) => (
          <NavItem
            key={item.id}
            item={item}
            isActive={activeTab === item.id}
            collapsed={collapsed}
            onClick={() => onTabChange(item.id)}
            onKeyDown={(e) => onKeyDown(e, item.id)}
            tabIndex={getTabIndex(item.id)}
            dataNavItem={item.id}
            onToggleSubgroup={item.hasChildren ? () => onToggleSubgroup(item.id) : undefined}
          />
        ))}
      </div>
    </div>
  );
}

// ============================================================================
// Sidebar Component
// ============================================================================

export function Sidebar({ activeTab, onTabChange, collapsed, onCollapsedChange }: SidebarProps) {
  // Load expanded groups from localStorage
  const [expandedGroups, setExpandedGroups] = useState<Record<string, boolean>>(() => {
    try {
      const stored = localStorage.getItem(SIDEBAR_GROUPS_KEY);
      if (stored) {
        return JSON.parse(stored);
      }
    } catch {
      // Ignore parse errors
    }
    // Default state: use defaultExpanded from groups
    return NAVIGATION_GROUPS.reduce(
      (acc, group) => {
        acc[group.id] = group.defaultExpanded ?? false;
        return acc;
      },
      {} as Record<string, boolean>,
    );
  });

  // Load expanded subgroups (parent items like "Run Dashboard") from localStorage
  const [expandedSubgroups, setExpandedSubgroups] = useState<Record<string, boolean>>(() => {
    try {
      const stored = localStorage.getItem(SIDEBAR_SUBGROUPS_KEY);
      if (stored) {
        return JSON.parse(stored);
      }
    } catch {
      // Ignore parse errors
    }
    // Default: Run Dashboard and Settings are expanded
    return { "run-dashboard": true, settings: true };
  });

  // Track focused item for keyboard navigation
  const [focusedItemId, setFocusedItemId] = useState<string | null>(null);

  // Persist expanded groups
  useEffect(() => {
    localStorage.setItem(SIDEBAR_GROUPS_KEY, JSON.stringify(expandedGroups));
  }, [expandedGroups]);

  // Persist expanded subgroups
  useEffect(() => {
    localStorage.setItem(SIDEBAR_SUBGROUPS_KEY, JSON.stringify(expandedSubgroups));
  }, [expandedSubgroups]);

  // Toggle group expansion
  const toggleGroup = useCallback((groupId: string) => {
    setExpandedGroups((prev) => ({
      ...prev,
      [groupId]: !prev[groupId],
    }));
  }, []);

  // Toggle subgroup expansion (for parent items like Run Dashboard)
  const toggleSubgroup = useCallback((parentId: string) => {
    setExpandedSubgroups((prev) => ({
      ...prev,
      [parentId]: !prev[parentId],
    }));
  }, []);

  // Flatten items for keyboard navigation
  const flattenedItems = NAVIGATION_GROUPS.flatMap((group) =>
    group.items.map((item) => ({
      ...item,
      groupId: group.id,
    })),
  );

  // Handle keyboard navigation
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
            // Ensure group is expanded
            if (!expandedGroups[nextItem.groupId] && !collapsed) {
              setExpandedGroups((prev) => ({ ...prev, [nextItem.groupId]: true }));
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
            // Ensure group is expanded
            if (!expandedGroups[prevItem.groupId] && !collapsed) {
              setExpandedGroups((prev) => ({ ...prev, [prevItem.groupId]: true }));
            }
          }
          break;
        }
        case "Enter":
        case " ": {
          e.preventDefault();
          onTabChange(currentItemId);
          break;
        }
        case "Home": {
          e.preventDefault();
          const firstItem = flattenedItems[0];
          if (firstItem) {
            setFocusedItemId(firstItem.id);
            if (!expandedGroups[firstItem.groupId] && !collapsed) {
              setExpandedGroups((prev) => ({ ...prev, [firstItem.groupId]: true }));
            }
          }
          break;
        }
        case "End": {
          e.preventDefault();
          const lastItem = flattenedItems[flattenedItems.length - 1];
          if (lastItem) {
            setFocusedItemId(lastItem.id);
            if (!expandedGroups[lastItem.groupId] && !collapsed) {
              setExpandedGroups((prev) => ({ ...prev, [lastItem.groupId]: true }));
            }
          }
          break;
        }
      }
    },
    [flattenedItems, expandedGroups, collapsed, onTabChange],
  );

  // Focus management effect
  useEffect(() => {
    if (focusedItemId) {
      const element = document.querySelector(
        `[data-nav-item="${focusedItemId}"]`,
      ) as HTMLButtonElement;
      element?.focus();
    }
  }, [focusedItemId]);

  // Get tab index for an item (first item gets 0, rest get -1 for roving tabindex)
  const getTabIndex = useCallback(
    (itemId: string) => {
      const firstItem = flattenedItems[0];
      return firstItem?.id === itemId ? 0 : -1;
    },
    [flattenedItems],
  );

  return (
    <nav
      className={`
        h-full flex flex-col bg-card border-r border-border/50
        transition-all duration-300 ease-in-out
      `}
      style={{ width: collapsed ? SIDEBAR_WIDTH_COLLAPSED : SIDEBAR_WIDTH_EXPANDED }}
      aria-label="Main navigation"
    >
      {/* Navigation Groups */}
      <div className="flex-1 overflow-y-auto py-4 px-2 space-y-4">
        {NAVIGATION_GROUPS.map((group) => (
          <NavGroup
            key={group.id}
            group={group}
            isExpanded={collapsed || expandedGroups[group.id]}
            onToggle={() => toggleGroup(group.id)}
            activeTab={activeTab}
            onTabChange={onTabChange}
            collapsed={collapsed}
            onKeyDown={handleKeyDown}
            getTabIndex={getTabIndex}
            expandedSubgroups={expandedSubgroups}
            onToggleSubgroup={toggleSubgroup}
          />
        ))}
      </div>

      {/* Collapse Toggle Button */}
      <div className="border-t border-border/50 p-2">
        <Tooltip content={collapsed ? "Expand sidebar" : "Collapse sidebar"} disabled={!collapsed}>
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
  );
}

// ============================================================================
// Hook for managing sidebar state with localStorage persistence
// ============================================================================

export function useSidebarState() {
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try {
      const stored = localStorage.getItem(SIDEBAR_COLLAPSED_KEY);
      return stored ? JSON.parse(stored) : false;
    } catch {
      return false;
    }
  });

  const handleCollapsedChange = useCallback((value: boolean) => {
    setCollapsed(value);
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, JSON.stringify(value));
  }, []);

  return {
    collapsed,
    onCollapsedChange: handleCollapsedChange,
  };
}

export default Sidebar;
