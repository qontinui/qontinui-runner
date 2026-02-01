/**
 * MetricCard.tsx
 *
 * Reusable summary card for displaying a single metric.
 */

import { type LucideIcon } from "lucide-react";

interface MetricCardProps {
  /** Card title */
  title: string;
  /** Primary value to display */
  value: string | number;
  /** Optional subtitle or secondary info */
  subtitle?: string;
  /** Optional icon component */
  icon?: LucideIcon;
  /** Color variant */
  variant?: "default" | "success" | "warning" | "error";
  /** Additional CSS classes */
  className?: string;
}

export function MetricCard({
  title,
  value,
  subtitle,
  icon: Icon,
  variant = "default",
  className = "",
}: MetricCardProps) {
  const getVariantClasses = () => {
    switch (variant) {
      case "success":
        return "bg-green-500/10 border-green-500/20";
      case "warning":
        return "bg-yellow-500/10 border-yellow-500/20";
      case "error":
        return "bg-red-500/10 border-red-500/20";
      default:
        return "bg-muted/30 border-border";
    }
  };

  const getValueClasses = () => {
    switch (variant) {
      case "success":
        return "text-green-500";
      case "warning":
        return "text-yellow-500";
      case "error":
        return "text-red-500";
      default:
        return "text-foreground";
    }
  };

  return (
    <div className={`p-4 rounded-lg border ${getVariantClasses()} ${className}`}>
      <div className="flex items-center gap-2 mb-2">
        {Icon && <Icon className="w-4 h-4 text-muted-foreground" />}
        <span className="text-sm text-muted-foreground">{title}</span>
      </div>
      <div className={`text-2xl font-bold ${getValueClasses()}`}>{value}</div>
      {subtitle && <div className="text-xs text-muted-foreground mt-1">{subtitle}</div>}
    </div>
  );
}
