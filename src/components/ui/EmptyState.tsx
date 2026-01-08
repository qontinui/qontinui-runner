import { type HTMLAttributes, type ReactNode } from "react";
import { cn } from "../../lib/utils";

export interface EmptyStateProps extends HTMLAttributes<HTMLDivElement> {
  icon?: ReactNode;
  title: string;
  description?: string;
  action?: ReactNode;
}

function EmptyState({ icon, title, description, action, className, ...props }: EmptyStateProps) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center py-12 text-muted-foreground",
        className,
      )}
      {...props}
    >
      {icon && <div className="w-12 h-12 mb-4 opacity-50">{icon}</div>}
      <h3 className="text-lg font-medium mb-2 text-foreground">{title}</h3>
      {description && <p className="text-sm text-center max-w-sm">{description}</p>}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}

export { EmptyState };
