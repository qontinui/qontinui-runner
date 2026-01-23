/**
 * DatasetWizardDialog.tsx
 *
 * A three-step wizard dialog for packaging annotation exports from qontinui-web
 * with local screenshots into a complete YOLO training dataset.
 *
 * Step 1: Select files (manifest, annotation ZIP, output folder)
 * Step 2: Configure splits (train/val/test percentages)
 * Step 3: Review & Package (summary, progress, results)
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import * as Dialog from "@radix-ui/react-dialog";
import { getAccentColors, getStatusColors } from "@/design-system";
import {
  Package,
  FolderOpen,
  FileText,
  CheckCircle,
  XCircle,
  AlertCircle,
  Loader2,
  HardDrive,
  X,
  ChevronRight,
  ChevronLeft,
  Settings2,
  Eye,
} from "lucide-react";

interface ImageMetadata {
  filename: string;
  image_hash: string;
  width: number;
  height: number;
  action_id?: string;
  action_type?: string;
  active_states?: string[];
  timestamp?: string;
}

interface MatchedImage {
  metadata: ImageMetadata;
  local_path: string;
  hash_verified: boolean;
}

interface ScanResult {
  total_in_manifest: number;
  matched: MatchedImage[];
  unmatched: ImageMetadata[];
  screenshot_base_path: string;
}

interface PackageResult {
  success: boolean;
  output_path: string;
  train_count: number;
  val_count: number;
  test_count: number;
  classes: string[];
}

export interface DatasetWizardDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type WizardStep = 1 | 2 | 3;

export function DatasetWizardDialog({ open, onOpenChange }: DatasetWizardDialogProps) {
  // Current wizard step
  const [currentStep, setCurrentStep] = useState<WizardStep>(1);

  // File selection state
  const [manifestPath, setManifestPath] = useState<string>("");
  const [annotationZipPath, setAnnotationZipPath] = useState<string>("");
  const [outputPath, setOutputPath] = useState<string>("");

  // Scan state
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);

  // Package state
  const [packaging, setPackaging] = useState(false);
  const [packageResult, setPackageResult] = useState<PackageResult | null>(null);
  const [packageError, setPackageError] = useState<string | null>(null);

  // Split configuration
  const [trainSplit, setTrainSplit] = useState(70);
  const [valSplit, setValSplit] = useState(20);
  const [testSplit, setTestSplit] = useState(10);
  const [randomSeed, setRandomSeed] = useState(42);

  // Reset wizard when dialog closes
  useEffect(() => {
    if (!open) {
      // Delay reset to avoid flickering during close animation
      const timeout = setTimeout(() => {
        setCurrentStep(1);
        setManifestPath("");
        setAnnotationZipPath("");
        setOutputPath("");
        setScanResult(null);
        setScanError(null);
        setPackageResult(null);
        setPackageError(null);
        setTrainSplit(70);
        setValSplit(20);
        setTestSplit(10);
        setRandomSeed(42);
      }, 300);
      return () => clearTimeout(timeout);
    }
  }, [open]);

  const selectManifestFile = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
        title: "Select image_manifest.json",
      });

      if (selected && typeof selected === "string") {
        setManifestPath(selected);
        setScanResult(null);
        setScanError(null);
        setPackageResult(null);
        setPackageError(null);
      }
    } catch (error) {
      console.error("Failed to select manifest:", error);
    }
  };

  const selectAnnotationZip = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "ZIP", extensions: ["zip"] }],
        title: "Select Annotation Export ZIP",
      });

      if (selected && typeof selected === "string") {
        setAnnotationZipPath(selected);
        setPackageResult(null);
        setPackageError(null);
      }
    } catch (error) {
      console.error("Failed to select annotation ZIP:", error);
    }
  };

  const selectOutputDirectory = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        directory: true,
        title: "Select Output Directory",
      });

      if (selected && typeof selected === "string") {
        setOutputPath(selected);
        setPackageResult(null);
        setPackageError(null);
      }
    } catch (error) {
      console.error("Failed to select output directory:", error);
    }
  };

  const scanLocalImages = async () => {
    if (!manifestPath) {
      setScanError("Please select a manifest file first");
      return;
    }

    setScanning(true);
    setScanError(null);
    setScanResult(null);

    try {
      const response = await invoke<{ success: boolean; message?: string; data?: ScanResult }>(
        "scan_local_images",
        {
          manifestPath,
        },
      );

      if (response.success && response.data) {
        setScanResult(response.data);
      } else {
        setScanError(response.message || "Scan failed");
      }
    } catch (error) {
      console.error("Scan error:", error);
      setScanError(String(error));
    } finally {
      setScanning(false);
    }
  };

  const packageDataset = async () => {
    if (!manifestPath || !annotationZipPath || !outputPath) {
      setPackageError("Please select all required files and output directory");
      return;
    }

    if (!scanResult || scanResult.matched.length === 0) {
      setPackageError("Please scan for local images first");
      return;
    }

    setPackaging(true);
    setPackageError(null);
    setPackageResult(null);

    try {
      const response = await invoke<{ success: boolean; message?: string; data?: PackageResult }>(
        "package_dataset",
        {
          config: {
            manifest_path: manifestPath,
            annotation_zip_path: annotationZipPath,
            output_path: outputPath,
            train_split: trainSplit / 100,
            val_split: valSplit / 100,
            test_split: testSplit / 100,
            random_seed: randomSeed,
          },
        },
      );

      if (response.success && response.data) {
        setPackageResult(response.data);
      } else {
        setPackageError(response.message || "Packaging failed");
      }
    } catch (error) {
      console.error("Package error:", error);
      setPackageError(String(error));
    } finally {
      setPackaging(false);
    }
  };

  const totalSplit = trainSplit + valSplit + testSplit;
  const splitsValid = Math.abs(totalSplit - 100) < 0.1;

  // Check if we can proceed to the next step
  const canProceedToStep2 =
    manifestPath && annotationZipPath && outputPath && scanResult && scanResult.matched.length > 0;
  const canProceedToStep3 = canProceedToStep2 && splitsValid;

  const handleNext = () => {
    if (currentStep === 1 && canProceedToStep2) {
      setCurrentStep(2);
    } else if (currentStep === 2 && canProceedToStep3) {
      setCurrentStep(3);
    }
  };

  const handleBack = () => {
    if (currentStep === 2) {
      setCurrentStep(1);
    } else if (currentStep === 3) {
      setCurrentStep(2);
    }
  };

  const handleClose = () => {
    if (!packaging) {
      onOpenChange(false);
    }
  };

  const StepIndicator = () => (
    <div className="flex items-center justify-center gap-2 mb-6">
      {[1, 2, 3].map((step) => (
        <div key={step} className="flex items-center">
          <div
            className={`w-8 h-8 rounded-full flex items-center justify-center text-sm font-medium transition-colors ${
              step === currentStep
                ? "bg-primary text-primary-foreground"
                : step < currentStep
                  ? "bg-primary/20 text-primary"
                  : "bg-muted text-muted-foreground"
            }`}
          >
            {step < currentStep ? <CheckCircle className="w-4 h-4" /> : step}
          </div>
          {step < 3 && (
            <div
              className={`w-12 h-0.5 mx-1 ${step < currentStep ? "bg-primary/20" : "bg-muted"}`}
            />
          )}
        </div>
      ))}
    </div>
  );

  const getStepTitle = (step: WizardStep): string => {
    switch (step) {
      case 1:
        return "Select Files";
      case 2:
        return "Configure Splits";
      case 3:
        return "Review & Package";
    }
  };

  const getStepIcon = (step: WizardStep) => {
    switch (step) {
      case 1:
        return <FileText className="w-5 h-5" />;
      case 2:
        return <Settings2 className="w-5 h-5" />;
      case 3:
        return <Eye className="w-5 h-5" />;
    }
  };

  // Step 1: File Selection
  const renderStep1 = () => (
    <div className="space-y-4">
      {/* Manifest File */}
      <div className="space-y-2">
        <label className="text-sm font-medium text-foreground">
          Image Manifest (image_manifest.json)
        </label>
        <div className="flex gap-2">
          <input
            type="text"
            value={manifestPath}
            readOnly
            placeholder="Select image_manifest.json from annotation export"
            className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
          />
          <button
            onClick={selectManifestFile}
            className="px-3 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-2"
          >
            <FolderOpen className="w-4 h-4" />
            Browse
          </button>
        </div>
      </div>

      {/* Annotation ZIP */}
      <div className="space-y-2">
        <label className="text-sm font-medium text-foreground">Annotation Export ZIP</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={annotationZipPath}
            readOnly
            placeholder="Select annotation export ZIP from qontinui-web"
            className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
          />
          <button
            onClick={selectAnnotationZip}
            className="px-3 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-2"
          >
            <FolderOpen className="w-4 h-4" />
            Browse
          </button>
        </div>
      </div>

      {/* Output Directory */}
      <div className="space-y-2">
        <label className="text-sm font-medium text-foreground">Output Directory</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={outputPath}
            readOnly
            placeholder="Select where to save the packaged dataset"
            className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
          />
          <button
            onClick={selectOutputDirectory}
            className="px-3 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-2"
          >
            <FolderOpen className="w-4 h-4" />
            Browse
          </button>
        </div>
      </div>

      {/* Scan Button */}
      <div className="pt-2">
        <button
          onClick={scanLocalImages}
          disabled={!manifestPath || scanning}
          className="w-full px-4 py-2 bg-primary hover:bg-primary/80 disabled:bg-muted disabled:cursor-not-allowed text-primary-foreground rounded-md font-medium transition-colors flex items-center justify-center gap-2"
        >
          {scanning ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              Scanning Local Storage...
            </>
          ) : (
            <>
              <HardDrive className="w-4 h-4" />
              Scan Local Images
            </>
          )}
        </button>
      </div>

      {/* Scan Error */}
      {scanError && (
        <div className="p-3 bg-destructive/10 border border-destructive/30 rounded-lg flex items-start gap-2">
          <XCircle className="w-5 h-5 text-destructive flex-shrink-0 mt-0.5" />
          <span className="text-destructive text-sm">{scanError}</span>
        </div>
      )}

      {/* Scan Results */}
      {scanResult && (
        <div className="p-4 bg-card border border-border/50 rounded-lg space-y-3">
          <div className="flex items-center gap-2 font-medium">
            <CheckCircle className={`w-5 h-5 ${getStatusColors("success").icon}`} />
            Scan Complete
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div
              className={`p-3 rounded-lg ${getStatusColors("success").bg} border ${getStatusColors("success").border}`}
            >
              <div className="flex items-center gap-2 mb-1">
                <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />
                <span className={`text-sm font-medium ${getStatusColors("success").icon}`}>
                  Matched
                </span>
              </div>
              <p className="text-xl font-bold text-foreground">{scanResult.matched.length}</p>
              <p className="text-xs text-muted-foreground">
                {scanResult.matched.filter((m) => m.hash_verified).length} hash verified
              </p>
            </div>
            <div className="p-3 rounded-lg bg-destructive/10 border border-destructive/20">
              <div className="flex items-center gap-2 mb-1">
                <XCircle className="w-4 h-4 text-destructive" />
                <span className="text-sm font-medium text-destructive">Unmatched</span>
              </div>
              <p className="text-xl font-bold text-foreground">{scanResult.unmatched.length}</p>
              <p className="text-xs text-muted-foreground">
                of {scanResult.total_in_manifest} total
              </p>
            </div>
          </div>
          {scanResult.unmatched.length > 0 && (
            <div
              className={`p-3 rounded-lg ${getAccentColors("orange").bg} border ${getAccentColors("orange").border} flex items-start gap-2`}
            >
              <AlertCircle
                className={`w-4 h-4 ${getAccentColors("orange").text} flex-shrink-0 mt-0.5`}
              />
              <p className="text-xs text-muted-foreground">
                {scanResult.unmatched.length} images from the manifest were not found locally.
              </p>
            </div>
          )}
        </div>
      )}
    </div>
  );

  // Step 2: Split Configuration
  const renderStep2 = () => (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        Configure how your dataset will be split between training, validation, and test sets.
        {scanResult && (
          <span className="block mt-1">
            Total images available: <strong>{scanResult.matched.length}</strong>
          </span>
        )}
      </p>

      <div className="space-y-4">
        {/* Train Split */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-sm font-medium text-foreground">Train Split</label>
            <span className="text-sm font-medium text-primary">{trainSplit}%</span>
          </div>
          <input
            type="range"
            min="0"
            max="100"
            value={trainSplit}
            onChange={(e) => setTrainSplit(Number(e.target.value))}
            className="w-full h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary"
          />
          <p className="text-xs text-muted-foreground">
            ~{scanResult ? Math.round((scanResult.matched.length * trainSplit) / 100) : 0} images
          </p>
        </div>

        {/* Validation Split */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-sm font-medium text-foreground">Validation Split</label>
            <span className="text-sm font-medium text-primary">{valSplit}%</span>
          </div>
          <input
            type="range"
            min="0"
            max="100"
            value={valSplit}
            onChange={(e) => setValSplit(Number(e.target.value))}
            className="w-full h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary"
          />
          <p className="text-xs text-muted-foreground">
            ~{scanResult ? Math.round((scanResult.matched.length * valSplit) / 100) : 0} images
          </p>
        </div>

        {/* Test Split */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-sm font-medium text-foreground">Test Split</label>
            <span className="text-sm font-medium text-primary">{testSplit}%</span>
          </div>
          <input
            type="range"
            min="0"
            max="100"
            value={testSplit}
            onChange={(e) => setTestSplit(Number(e.target.value))}
            className="w-full h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary"
          />
          <p className="text-xs text-muted-foreground">
            ~{scanResult ? Math.round((scanResult.matched.length * testSplit) / 100) : 0} images
          </p>
        </div>
      </div>

      {/* Split validation warning */}
      {!splitsValid && (
        <div className="p-3 bg-destructive/10 border border-destructive/30 rounded-lg flex items-start gap-2">
          <XCircle className="w-5 h-5 text-destructive flex-shrink-0 mt-0.5" />
          <span className="text-destructive text-sm">
            Split percentages must sum to 100% (currently: {totalSplit}%)
          </span>
        </div>
      )}

      {/* Random Seed */}
      <div className="space-y-2 pt-2 border-t border-border/50">
        <label className="text-sm font-medium text-foreground">Random Seed</label>
        <input
          type="number"
          value={randomSeed}
          onChange={(e) => setRandomSeed(Number(e.target.value))}
          className="w-32 px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
          placeholder="42"
        />
        <p className="text-xs text-muted-foreground">Use the same seed for reproducible splits</p>
      </div>
    </div>
  );

  // Step 3: Review & Package
  const renderStep3 = () => (
    <div className="space-y-4">
      {/* Summary */}
      {!packageResult && !packaging && (
        <>
          <div className="p-4 bg-card border border-border/50 rounded-lg space-y-3">
            <h4 className="font-medium text-foreground">Configuration Summary</h4>
            <div className="space-y-2 text-sm">
              <div className="flex justify-between">
                <span className="text-muted-foreground">Images to package:</span>
                <span className="font-medium">{scanResult?.matched.length || 0}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Train set:</span>
                <span className="font-medium">
                  ~{scanResult ? Math.round((scanResult.matched.length * trainSplit) / 100) : 0}{" "}
                  images ({trainSplit}%)
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Validation set:</span>
                <span className="font-medium">
                  ~{scanResult ? Math.round((scanResult.matched.length * valSplit) / 100) : 0}{" "}
                  images ({valSplit}%)
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Test set:</span>
                <span className="font-medium">
                  ~{scanResult ? Math.round((scanResult.matched.length * testSplit) / 100) : 0}{" "}
                  images ({testSplit}%)
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Random seed:</span>
                <span className="font-medium">{randomSeed}</span>
              </div>
            </div>
          </div>

          <div className="p-4 bg-card border border-border/50 rounded-lg space-y-2">
            <h4 className="font-medium text-foreground">Output Location</h4>
            <p className="text-sm text-muted-foreground break-all">{outputPath}</p>
          </div>

          {/* Package Button */}
          <button
            onClick={packageDataset}
            disabled={packaging}
            className={`w-full px-4 py-3 ${getAccentColors("green").bgSolid} hover:opacity-90 disabled:bg-muted disabled:cursor-not-allowed text-white rounded-md font-medium transition-colors flex items-center justify-center gap-2`}
          >
            <Package className="w-5 h-5" />
            Package Dataset
          </button>
        </>
      )}

      {/* Packaging in Progress */}
      {packaging && (
        <div className="p-6 flex flex-col items-center justify-center space-y-4">
          <Loader2 className="w-12 h-12 text-primary animate-spin" />
          <div className="text-center">
            <p className="font-medium text-foreground">Packaging Dataset...</p>
            <p className="text-sm text-muted-foreground mt-1">
              This may take a moment depending on the number of images.
            </p>
          </div>
        </div>
      )}

      {/* Package Error */}
      {packageError && (
        <div className="p-4 bg-destructive/10 border border-destructive/30 rounded-lg flex items-start gap-3">
          <XCircle className="w-6 h-6 text-destructive flex-shrink-0 mt-0.5" />
          <div>
            <p className="font-semibold text-destructive">Packaging Error</p>
            <p className="text-sm text-muted-foreground mt-1">{packageError}</p>
          </div>
        </div>
      )}

      {/* Package Success */}
      {packageResult && (
        <div className="space-y-4">
          <div
            className={`p-4 ${getAccentColors("green").bg} border ${getAccentColors("green").border} rounded-lg flex items-start gap-3`}
          >
            <CheckCircle
              className={`w-6 h-6 ${getStatusColors("success").icon} flex-shrink-0 mt-0.5`}
            />
            <div>
              <p className={`font-semibold ${getStatusColors("success").icon}`}>
                Dataset Packaged Successfully!
              </p>
              <p className="text-sm text-muted-foreground mt-1">
                Your YOLO dataset is ready for training.
              </p>
            </div>
          </div>

          <div className="p-4 bg-card border border-border/50 rounded-lg space-y-2 text-sm">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Output Path:</span>
              <span className="font-medium text-foreground break-all text-right max-w-[60%]">
                {packageResult.output_path}
              </span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Train Images:</span>
              <span className="font-medium">{packageResult.train_count}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Validation Images:</span>
              <span className="font-medium">{packageResult.val_count}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Test Images:</span>
              <span className="font-medium">{packageResult.test_count}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Classes:</span>
              <span className="font-medium">{packageResult.classes.length}</span>
            </div>
          </div>

          <div className="p-3 bg-muted/50 border border-border rounded-lg">
            <p className="text-sm font-medium text-foreground mb-2">Next Steps:</p>
            <ol className="text-xs text-muted-foreground space-y-1 list-decimal list-inside">
              <li>Review the dataset structure in the output directory</li>
              <li>Use data.yaml for YOLO training configuration</li>
              <li>Verify the train/val/test splits meet your needs</li>
              <li>Start training your model!</li>
            </ol>
          </div>

          <button
            onClick={handleClose}
            className="w-full px-4 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors"
          >
            Done
          </button>
        </div>
      )}
    </div>
  );

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/80 backdrop-blur-sm z-50 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <Dialog.Content
          data-ui-id="dialog-dataset-wizard"
          className="fixed left-[50%] top-[50%] z-50 max-h-[90vh] w-full max-w-[600px] translate-x-[-50%] translate-y-[-50%] bg-card border border-border/50 rounded-lg shadow-lg overflow-hidden data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[state=closed]:slide-out-to-left-1/2 data-[state=closed]:slide-out-to-top-[48%] data-[state=open]:slide-in-from-left-1/2 data-[state=open]:slide-in-from-top-[48%]">
          {/* Header */}
          <div className="flex items-start justify-between p-6 border-b border-border/50">
            <div className="flex items-center gap-3">
              <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
                <Package className="w-5 h-5 text-primary" />
              </div>
              <div>
                <Dialog.Title className="text-lg font-semibold text-foreground">
                  Package Dataset
                </Dialog.Title>
                <Dialog.Description className="text-sm text-muted-foreground">
                  Create a YOLO training dataset from your annotations
                </Dialog.Description>
              </div>
            </div>
            <Dialog.Close asChild>
              <button
                data-ui-id="dialog-dataset-wizard-close-btn"
                className="rounded-md p-2 hover:bg-muted transition-colors"
                aria-label="Close"
                disabled={packaging}
              >
                <X className="w-5 h-5 text-muted-foreground" />
              </button>
            </Dialog.Close>
          </div>

          {/* Content */}
          <div className="p-6 overflow-y-auto max-h-[calc(90vh-200px)]">
            {/* Step Indicator */}
            <StepIndicator />

            {/* Step Title */}
            <div className="flex items-center gap-2 mb-4">
              {getStepIcon(currentStep)}
              <h3 className="font-medium text-foreground">{getStepTitle(currentStep)}</h3>
            </div>

            {/* Step Content */}
            {currentStep === 1 && renderStep1()}
            {currentStep === 2 && renderStep2()}
            {currentStep === 3 && renderStep3()}
          </div>

          {/* Footer Navigation */}
          {!packageResult && !packaging && (
            <div className="flex items-center justify-between p-6 border-t border-border/50">
              <button
                data-ui-id="dialog-dataset-wizard-back-btn"
                onClick={handleBack}
                disabled={currentStep === 1}
                className="px-4 py-2 text-sm font-medium text-muted-foreground hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed transition-colors flex items-center gap-1"
              >
                <ChevronLeft className="w-4 h-4" />
                Back
              </button>

              <div className="text-sm text-muted-foreground">Step {currentStep} of 3</div>

              {currentStep < 3 ? (
                <button
                  data-ui-id="dialog-dataset-wizard-next-btn"
                  onClick={handleNext}
                  disabled={
                    (currentStep === 1 && !canProceedToStep2) ||
                    (currentStep === 2 && !canProceedToStep3)
                  }
                  className="px-4 py-2 bg-primary hover:bg-primary/80 disabled:bg-muted disabled:cursor-not-allowed text-primary-foreground rounded-md text-sm font-medium transition-colors flex items-center gap-1"
                >
                  Next
                  <ChevronRight className="w-4 h-4" />
                </button>
              ) : (
                <div />
              )}
            </div>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
