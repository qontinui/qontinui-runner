import { QRScanner } from "./QRScanner";
import * as Dialog from "@radix-ui/react-dialog";
import { X } from "lucide-react";

export interface QRScannerDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onScan: (data: string) => void;
}

export function QRScannerDialog({ open, onOpenChange, onScan }: QRScannerDialogProps) {
  const handleScan = (data: string) => {
    onScan(data);
    onOpenChange(false);
  };

  const handleClose = () => {
    onOpenChange(false);
  };

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/80 backdrop-blur-sm z-50 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <Dialog.Content className="fixed left-[50%] top-[50%] z-50 max-h-[90vh] w-full max-w-[600px] translate-x-[-50%] translate-y-[-50%] bg-card border border-border/50 rounded-lg shadow-lg p-6 overflow-y-auto data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[state=closed]:slide-out-to-left-1/2 data-[state=closed]:slide-out-to-top-[48%] data-[state=open]:slide-in-from-left-1/2 data-[state=open]:slide-in-from-top-[48%]">
          <div className="flex flex-col space-y-4">
            {/* Header */}
            <div className="flex items-start justify-between">
              <div className="space-y-1">
                <Dialog.Title className="text-xl font-semibold text-foreground">
                  Scan QR Code
                </Dialog.Title>
                <Dialog.Description className="text-sm text-muted-foreground">
                  Point your camera at the QR code from <strong>qontinui.com/connect-runner</strong>
                </Dialog.Description>
              </div>
              <Dialog.Close asChild>
                <button
                  className="rounded-md p-2 hover:bg-muted transition-colors"
                  aria-label="Close"
                >
                  <X className="w-5 h-5 text-muted-foreground" />
                </button>
              </Dialog.Close>
            </div>

            {/* Scanner Component */}
            <QRScanner onScan={handleScan} onClose={handleClose} />
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
