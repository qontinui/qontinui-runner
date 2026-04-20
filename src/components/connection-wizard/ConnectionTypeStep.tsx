/**
 * Step 1: pick how the phone is reaching the runner.
 *
 * Three cards: USB / LAN / Remote. Remote card reflects the cost strategy —
 * self-hosted P2P/rathole is emphasized, managed-relay is disabled.
 */

import { Usb, Wifi, Cloud, ChevronRight } from "lucide-react";
import type { WizardApi } from "./ConnectionWizard";
import type { ConnectionType } from "./types";

interface CardDef {
  type: ConnectionType;
  title: string;
  body: string;
  icon: React.ReactNode;
  platforms: ("android" | "ios")[];
}

const CARDS: CardDef[] = [
  {
    type: "usb",
    title: "USB",
    body: "Phone plugged into this computer with USB debugging enabled.",
    icon: <Usb className="w-5 h-5" />,
    platforms: ["android", "ios"],
  },
  {
    type: "lan",
    title: "Wi-Fi",
    body: "Phone on the same local network as the runner.",
    icon: <Wifi className="w-5 h-5" />,
    platforms: ["android", "ios"],
  },
  {
    type: "remote",
    title: "Remote",
    body: "Phone on a different network. Uses P2P where possible; falls back to a self-hosted rathole tunnel.",
    icon: <Cloud className="w-5 h-5" />,
    platforms: ["android", "ios"],
  },
];

export function ConnectionTypeStep({ api }: { api: WizardApi }) {
  const select = (t: ConnectionType) => {
    api.setConnectionType(t);
    api.next();
  };

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground mb-4">How is your phone reaching the runner?</p>

      {CARDS.map((card) => (
        <button
          key={card.type}
          onClick={() => select(card.type)}
          className={
            "w-full flex items-start gap-4 p-4 rounded-lg border border-zinc-700 " +
            "bg-zinc-800/50 hover:bg-zinc-800 hover:border-primary/50 " +
            "transition-colors text-left"
          }
          aria-label={`Use ${card.title}`}
        >
          <div className="p-2 rounded-md bg-primary/10 text-primary">{card.icon}</div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="font-medium text-foreground">{card.title}</span>
              <div className="flex gap-1">
                {card.platforms.map((p) => (
                  <span
                    key={p}
                    className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-300"
                  >
                    {p}
                  </span>
                ))}
              </div>
            </div>
            <p className="text-xs text-muted-foreground mt-1">{card.body}</p>
          </div>
          <ChevronRight className="w-5 h-5 text-muted-foreground self-center" />
        </button>
      ))}

      <div className="mt-2 p-3 rounded-md bg-zinc-800/50 border border-dashed border-zinc-700 text-xs text-muted-foreground">
        <span className="font-medium text-foreground">Managed relay</span> — coming soon. Until
        then, Remote uses P2P or a rathole tunnel you host yourself (free tier on Oracle Cloud is
        recommended).
      </div>
    </div>
  );
}
