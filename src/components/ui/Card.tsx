import { forwardRef, type HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

// ============================================
// Card
// ============================================

export interface CardProps extends HTMLAttributes<HTMLDivElement> {
  variant?: "default" | "borderless" | "interactive";
}

const Card = forwardRef<HTMLDivElement, CardProps>(
  ({ className, variant = "default", ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cn(
          "rounded-lg",
          {
            "bg-card text-card-foreground border border-border/50 shadow-lg": variant === "default",
            "bg-card/50": variant === "borderless",
            "bg-card/50 hover:bg-card/70 transition-colors cursor-pointer":
              variant === "interactive",
          },
          className,
        )}
        {...props}
      />
    );
  },
);

Card.displayName = "Card";

// ============================================
// CardHeader
// ============================================

const CardHeader = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cn("flex items-center justify-between px-4 py-3", className)}
        {...props}
      />
    );
  },
);

CardHeader.displayName = "CardHeader";

// ============================================
// CardTitle
// ============================================

const CardTitle = forwardRef<HTMLHeadingElement, HTMLAttributes<HTMLHeadingElement>>(
  ({ className, ...props }, ref) => {
    return (
      <h3
        ref={ref}
        className={cn("flex items-center gap-2 text-sm font-semibold", className)}
        {...props}
      />
    );
  },
);

CardTitle.displayName = "CardTitle";

// ============================================
// CardContent
// ============================================

const CardContent = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => {
    return <div ref={ref} className={cn("px-4 pb-4", className)} {...props} />;
  },
);

CardContent.displayName = "CardContent";

// ============================================
// CardFooter
// ============================================

const CardFooter = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cn(
          "flex items-center justify-end gap-2 px-4 py-3 border-t border-border/50",
          className,
        )}
        {...props}
      />
    );
  },
);

CardFooter.displayName = "CardFooter";

export { Card, CardHeader, CardTitle, CardContent, CardFooter };
