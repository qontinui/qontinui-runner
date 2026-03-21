import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

export function CollapsibleSection({
  title,
  children,
  defaultOpen = false,
}: {
  title: string;
  children: React.ReactNode;
  defaultOpen?: boolean;
}) {
  const [isOpen, setIsOpen] = useState(defaultOpen);

  return (
    <div>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors mb-1"
      >
        {isOpen ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        {title}
      </button>
      {isOpen && children}
    </div>
  );
}
