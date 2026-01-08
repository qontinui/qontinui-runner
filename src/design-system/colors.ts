/**
 * Centralized color system for qontinui-runner
 *
 * This module provides type-safe color mappings for semantic colors
 * used throughout the application. All colors reference either:
 * - Design tokens from @qontinui/design-tokens
 * - Tailwind color classes for consistency
 */

// ============================================
// Severity Colors
// For error/warning/info level indicators
// ============================================

export type SeverityLevel = "critical" | "high" | "medium" | "low" | "info";

export interface SeverityColorClasses {
  bg: string;
  text: string;
  border: string;
  dot: string;
}

const SEVERITY_COLORS: Record<SeverityLevel, SeverityColorClasses> = {
  critical: {
    bg: "bg-red-500/10",
    text: "text-red-500",
    border: "border-red-500/30",
    dot: "bg-red-500",
  },
  high: {
    bg: "bg-orange-500/10",
    text: "text-orange-500",
    border: "border-orange-500/30",
    dot: "bg-orange-500",
  },
  medium: {
    bg: "bg-yellow-500/10",
    text: "text-yellow-500",
    border: "border-yellow-500/30",
    dot: "bg-yellow-500",
  },
  low: {
    bg: "bg-blue-500/10",
    text: "text-blue-500",
    border: "border-blue-500/30",
    dot: "bg-blue-500",
  },
  info: {
    bg: "bg-slate-500/10",
    text: "text-slate-400",
    border: "border-slate-500/30",
    dot: "bg-slate-500",
  },
};

export function getSeverityColors(severity: string): SeverityColorClasses {
  return (
    SEVERITY_COLORS[severity as SeverityLevel] || {
      bg: "bg-muted/50",
      text: "text-muted-foreground",
      border: "border-border",
      dot: "bg-muted-foreground",
    }
  );
}

// ============================================
// Status Colors
// For execution/task/process status indicators
// ============================================

export type StatusType =
  | "idle"
  | "running"
  | "success"
  | "error"
  | "warning"
  | "pending"
  | "paused"
  | "cancelled";

export interface StatusColorClasses {
  bg: string;
  text: string;
  border: string;
  icon: string;
  pulse?: string;
}

const STATUS_COLORS: Record<StatusType, StatusColorClasses> = {
  idle: {
    bg: "bg-muted/30",
    text: "text-muted-foreground",
    border: "border-border",
    icon: "text-muted-foreground",
  },
  running: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-400",
    border: "border-emerald-500/30",
    icon: "text-emerald-400",
    pulse: "animate-pulse",
  },
  success: {
    bg: "bg-green-500/10",
    text: "text-green-500",
    border: "border-green-500/30",
    icon: "text-green-500",
  },
  error: {
    bg: "bg-red-500/10",
    text: "text-red-500",
    border: "border-red-500/30",
    icon: "text-red-500",
  },
  warning: {
    bg: "bg-yellow-500/10",
    text: "text-yellow-500",
    border: "border-yellow-500/30",
    icon: "text-yellow-500",
  },
  pending: {
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    border: "border-blue-500/30",
    icon: "text-blue-400",
  },
  paused: {
    bg: "bg-amber-500/10",
    text: "text-amber-400",
    border: "border-amber-500/30",
    icon: "text-amber-400",
  },
  cancelled: {
    bg: "bg-slate-500/10",
    text: "text-slate-400",
    border: "border-slate-500/30",
    icon: "text-slate-400",
  },
};

export function getStatusColors(status: string): StatusColorClasses {
  return (
    STATUS_COLORS[status as StatusType] || {
      bg: "bg-muted/30",
      text: "text-muted-foreground",
      border: "border-border",
      icon: "text-muted-foreground",
    }
  );
}

// ============================================
// Action Type Colors
// For different action categories
// ============================================

export type ActionType =
  | "auto_fix"
  | "needs_user_input"
  | "manual"
  | "informational"
  | "skip"
  | "defer";

export interface ActionColorClasses {
  bg: string;
  text: string;
  border: string;
  badge: string;
}

const ACTION_COLORS: Record<ActionType, ActionColorClasses> = {
  auto_fix: {
    bg: "bg-green-500/10",
    text: "text-green-500",
    border: "border-green-500/30",
    badge: "bg-green-500",
  },
  needs_user_input: {
    bg: "bg-amber-500/10",
    text: "text-amber-500",
    border: "border-amber-500/30",
    badge: "bg-amber-500",
  },
  manual: {
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    border: "border-blue-500/30",
    badge: "bg-blue-500",
  },
  informational: {
    bg: "bg-slate-500/10",
    text: "text-slate-400",
    border: "border-slate-500/30",
    badge: "bg-slate-500",
  },
  skip: {
    bg: "bg-muted/50",
    text: "text-muted-foreground",
    border: "border-border",
    badge: "bg-muted-foreground",
  },
  defer: {
    bg: "bg-purple-500/10",
    text: "text-purple-400",
    border: "border-purple-500/30",
    badge: "bg-purple-500",
  },
};

export function getActionColors(actionType: string): ActionColorClasses {
  return (
    ACTION_COLORS[actionType as ActionType] || {
      bg: "bg-muted/50",
      text: "text-muted-foreground",
      border: "border-border",
      badge: "bg-muted-foreground",
    }
  );
}

// ============================================
// Semantic/Accent Colors
// For general UI coloring (charts, highlights, etc.)
// ============================================

export type AccentColor =
  | "red"
  | "orange"
  | "amber"
  | "yellow"
  | "green"
  | "emerald"
  | "blue"
  | "cyan"
  | "purple"
  | "slate";

export interface AccentColorClasses {
  bg: string;
  bgSolid: string;
  text: string;
  border: string;
}

const ACCENT_COLORS: Record<AccentColor, AccentColorClasses> = {
  red: {
    bg: "bg-red-500/10",
    bgSolid: "bg-red-500",
    text: "text-red-500",
    border: "border-red-500/30",
  },
  orange: {
    bg: "bg-orange-500/10",
    bgSolid: "bg-orange-500",
    text: "text-orange-500",
    border: "border-orange-500/30",
  },
  amber: {
    bg: "bg-amber-500/10",
    bgSolid: "bg-amber-500",
    text: "text-amber-500",
    border: "border-amber-500/30",
  },
  yellow: {
    bg: "bg-yellow-500/10",
    bgSolid: "bg-yellow-500",
    text: "text-yellow-500",
    border: "border-yellow-500/30",
  },
  green: {
    bg: "bg-green-500/10",
    bgSolid: "bg-green-500",
    text: "text-green-500",
    border: "border-green-500/30",
  },
  emerald: {
    bg: "bg-emerald-500/10",
    bgSolid: "bg-emerald-500",
    text: "text-emerald-400",
    border: "border-emerald-500/30",
  },
  blue: {
    bg: "bg-blue-500/10",
    bgSolid: "bg-blue-500",
    text: "text-blue-400",
    border: "border-blue-500/30",
  },
  cyan: {
    bg: "bg-cyan-500/10",
    bgSolid: "bg-cyan-500",
    text: "text-cyan-400",
    border: "border-cyan-500/30",
  },
  purple: {
    bg: "bg-purple-500/10",
    bgSolid: "bg-purple-500",
    text: "text-purple-400",
    border: "border-purple-500/30",
  },
  slate: {
    bg: "bg-slate-500/10",
    bgSolid: "bg-slate-500",
    text: "text-slate-400",
    border: "border-slate-500/30",
  },
};

export function getAccentColors(color: string): AccentColorClasses {
  return (
    ACCENT_COLORS[color as AccentColor] || {
      bg: "bg-muted/50",
      bgSolid: "bg-muted-foreground",
      text: "text-muted-foreground",
      border: "border-border",
    }
  );
}
