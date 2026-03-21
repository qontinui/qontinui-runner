import { type HTMLAttributes, type ReactNode } from "react";
import { cn } from "../../lib/utils";
import { getStatusColors, type StatusType } from "../../design-system";

export interface StatusBannerProps extends HTMLAttributes<HTMLDivElement> {
  status: StatusType | string;
  icon?: ReactNode;
}

function StatusBanner({ status, icon, className, children, ...props }: StatusBannerProps) {
  const colors = getStatusColors(status);

  return (
    <div
      className={cn(
        "px-4 py-3 rounded-lg flex items-center gap-3",
        colors.bg,
        colors.text,
        className,
      )}
      {...props}
    >
      {icon && <span className={cn("shrink-0", colors.icon, colors.pulse)}>{icon}</span>}
      <div className="flex-1 min-w-0">{children}</div>
    </div>
  );
}

export { StatusBanner };
