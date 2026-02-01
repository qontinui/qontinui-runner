import { type HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: "default" | "muted" | "success" | "warning" | "danger" | "info" | "purple" | "outline";
  size?: "sm" | "md";
}

function Badge({ className, variant = "default", size = "md", ...props }: BadgeProps) {
  return (
    <span
      className={cn(
        // Base styles
        "inline-flex items-center gap-1.5 font-medium rounded-md",
        // Size variants
        {
          "px-1.5 py-0.5 text-[10px]": size === "sm",
          "px-2.5 py-1 text-xs": size === "md",
        },
        // Color variants
        {
          "bg-muted/50 text-foreground": variant === "default",
          "bg-muted/50 text-muted-foreground": variant === "muted",
          "bg-green-500/10 text-green-500": variant === "success",
          "bg-yellow-500/10 text-yellow-500": variant === "warning",
          "bg-red-500/10 text-red-500": variant === "danger",
          "bg-blue-500/10 text-blue-400": variant === "info",
          "bg-purple-500/10 text-purple-400": variant === "purple",
          "bg-transparent border border-border text-muted-foreground": variant === "outline",
        },
        className,
      )}
      {...props}
    />
  );
}

export { Badge };
