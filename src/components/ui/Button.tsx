import { forwardRef, type ButtonHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "primary" | "secondary" | "success" | "danger" | "ghost" | "outline";
  size?: "sm" | "md" | "lg";
}

const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = "primary", size = "md", ...props }, ref) => {
    return (
      <button
        ref={ref}
        className={cn(
          // Base styles
          "inline-flex items-center justify-center gap-2 font-medium transition-all",
          "disabled:opacity-50 disabled:cursor-not-allowed",
          // Size variants
          {
            "px-2.5 py-1 text-xs rounded-md": size === "sm",
            "px-4 py-2 text-sm rounded-lg": size === "md",
            "px-6 py-3 text-base rounded-lg": size === "lg",
          },
          // Color variants
          {
            "bg-primary text-primary-foreground hover:opacity-90 hover:shadow-lg":
              variant === "primary",
            "bg-secondary/20 text-secondary border border-secondary/50 hover:bg-secondary/30":
              variant === "secondary",
            "bg-accent text-accent-foreground hover:opacity-90 hover:shadow-lg":
              variant === "success",
            "bg-destructive text-destructive-foreground hover:opacity-90 hover:shadow-lg":
              variant === "danger",
            "bg-transparent text-foreground hover:bg-muted/50": variant === "ghost",
            "border border-border bg-transparent text-foreground hover:bg-muted/30":
              variant === "outline",
          },
          className,
        )}
        {...props}
      />
    );
  },
);

Button.displayName = "Button";

export { Button };
