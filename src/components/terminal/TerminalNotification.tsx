import { useEffect, useRef } from "react";
import { CheckCircle, XCircle, X } from "lucide-react";

interface TerminalNotificationProps {
  message: string | null;
  type: "success" | "error";
  onDismiss: () => void;
}

export function TerminalNotification({ message, type, onDismiss }: TerminalNotificationProps) {
  const onDismissRef = useRef(onDismiss);
  onDismissRef.current = onDismiss;

  useEffect(() => {
    if (!message) return;
    const timer = setTimeout(() => onDismissRef.current(), 5000);
    return () => clearTimeout(timer);
  }, [message]);

  if (!message) return null;

  const isSuccess = type === "success";
  return (
    <div
      className={`flex items-center gap-2 px-3 py-1.5 text-xs font-medium shrink-0 ${
        isSuccess
          ? "bg-[#9ece6a]/10 text-[#9ece6a] border-b border-[#9ece6a]/20"
          : "bg-[#f7768e]/10 text-[#f7768e] border-b border-[#f7768e]/20"
      }`}
    >
      {isSuccess ? <CheckCircle className="w-3.5 h-3.5" /> : <XCircle className="w-3.5 h-3.5" />}
      <span className="flex-1 truncate">{message}</span>
      <button onClick={onDismiss} className="p-0.5 rounded hover:bg-white/10 transition-colors">
        <X className="w-3 h-3" />
      </button>
    </div>
  );
}
