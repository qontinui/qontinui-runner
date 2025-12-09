/**
 * DatasetPackager.tsx
 *
 * Component for packaging annotation exports from qontinui-web with local screenshots
 * into a complete YOLO training dataset.
 */

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Package,
  FolderOpen,
  FileText,
  CheckCircle,
  XCircle,
  AlertCircle,
  Loader2,
  HardDrive,
  Database,
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

export function DatasetPackager() {
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

  const selectManifestFile = async () => {
    try {
      const selected = await open({
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
      const selected = await open({
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
      const selected = await open({
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
        }
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
        }
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

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center gap-3 mb-6">
        <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
          <Package className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-2xl font-bold text-foreground">Dataset Packager</h2>
          <p className="text-sm text-muted-foreground">
            Combine annotation exports with local screenshots to create training datasets
          </p>
        </div>
      </div>

      {/* File Selection */}
      <div className="card p-6 space-y-4">
        <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
          <FileText className="w-5 h-5" />
          File Selection
        </h3>

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
              className="flex-1 input-field"
            />
            <button onClick={selectManifestFile} className="btn-secondary">
              <FolderOpen className="w-4 h-4" />
              Browse
            </button>
          </div>
        </div>

        {/* Annotation ZIP */}
        <div className="space-y-2">
          <label className="text-sm font-medium text-foreground">
            Annotation Export ZIP
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              value={annotationZipPath}
              readOnly
              placeholder="Select annotation export ZIP from qontinui-web"
              className="flex-1 input-field"
            />
            <button onClick={selectAnnotationZip} className="btn-secondary">
              <FolderOpen className="w-4 h-4" />
              Browse
            </button>
          </div>
        </div>

        {/* Output Directory */}
        <div className="space-y-2">
          <label className="text-sm font-medium text-foreground">
            Output Directory
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              value={outputPath}
              readOnly
              placeholder="Select where to save the packaged dataset"
              className="flex-1 input-field"
            />
            <button onClick={selectOutputDirectory} className="btn-secondary">
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
            className="btn-primary w-full"
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
      </div>

      {/* Scan Error */}
      {scanError && (
        <div className="card p-4 border-l-4 border-destructive bg-destructive/5">
          <div className="flex items-start gap-3">
            <XCircle className="w-5 h-5 text-destructive flex-shrink-0 mt-0.5" />
            <div>
              <p className="font-semibold text-destructive">Scan Error</p>
              <p className="text-sm text-muted-foreground mt-1">{scanError}</p>
            </div>
          </div>
        </div>
      )}

      {/* Scan Results */}
      {scanResult && (
        <div className="card p-6 space-y-4">
          <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
            <Database className="w-5 h-5" />
            Scan Results
          </h3>

          <div className="grid grid-cols-2 gap-4">
            <div className="p-4 rounded-lg bg-success/10 border border-success/20">
              <div className="flex items-center gap-2 mb-2">
                <CheckCircle className="w-5 h-5 text-success" />
                <span className="font-semibold text-success">Matched</span>
              </div>
              <p className="text-2xl font-bold text-foreground">{scanResult.matched.length}</p>
              <p className="text-sm text-muted-foreground mt-1">
                {scanResult.matched.filter((m) => m.hash_verified).length} hash verified
              </p>
            </div>

            <div className="p-4 rounded-lg bg-destructive/10 border border-destructive/20">
              <div className="flex items-center gap-2 mb-2">
                <XCircle className="w-5 h-5 text-destructive" />
                <span className="font-semibold text-destructive">Unmatched</span>
              </div>
              <p className="text-2xl font-bold text-foreground">{scanResult.unmatched.length}</p>
              <p className="text-sm text-muted-foreground mt-1">
                of {scanResult.total_in_manifest} total
              </p>
            </div>
          </div>

          {scanResult.unmatched.length > 0 && (
            <div className="p-4 rounded-lg bg-warning/10 border border-warning/20">
              <div className="flex items-start gap-3">
                <AlertCircle className="w-5 h-5 text-warning flex-shrink-0 mt-0.5" />
                <div>
                  <p className="font-semibold text-warning">Missing Images</p>
                  <p className="text-sm text-muted-foreground mt-1">
                    {scanResult.unmatched.length} images from the manifest were not found in local
                    storage. The dataset will only include matched images.
                  </p>
                </div>
              </div>
            </div>
          )}

          <div className="text-sm text-muted-foreground">
            <p>
              <span className="font-medium">Local Storage Path:</span>{" "}
              {scanResult.screenshot_base_path}
            </p>
          </div>
        </div>
      )}

      {/* Split Configuration */}
      {scanResult && scanResult.matched.length > 0 && (
        <div className="card p-6 space-y-4">
          <h3 className="text-lg font-semibold text-foreground">Split Configuration</h3>

          <div className="grid grid-cols-3 gap-4">
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                Train Split ({trainSplit}%)
              </label>
              <input
                type="range"
                min="0"
                max="100"
                value={trainSplit}
                onChange={(e) => setTrainSplit(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-xs text-muted-foreground">
                ~{Math.round((scanResult.matched.length * trainSplit) / 100)} images
              </p>
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                Validation Split ({valSplit}%)
              </label>
              <input
                type="range"
                min="0"
                max="100"
                value={valSplit}
                onChange={(e) => setValSplit(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-xs text-muted-foreground">
                ~{Math.round((scanResult.matched.length * valSplit) / 100)} images
              </p>
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                Test Split ({testSplit}%)
              </label>
              <input
                type="range"
                min="0"
                max="100"
                value={testSplit}
                onChange={(e) => setTestSplit(Number(e.target.value))}
                className="w-full"
              />
              <p className="text-xs text-muted-foreground">
                ~{Math.round((scanResult.matched.length * testSplit) / 100)} images
              </p>
            </div>
          </div>

          {!splitsValid && (
            <div className="p-3 rounded-lg bg-destructive/10 border border-destructive/20">
              <p className="text-sm text-destructive">
                Split percentages must sum to 100% (currently: {totalSplit}%)
              </p>
            </div>
          )}

          <div className="space-y-2">
            <label className="text-sm font-medium text-foreground">Random Seed</label>
            <input
              type="number"
              value={randomSeed}
              onChange={(e) => setRandomSeed(Number(e.target.value))}
              className="input-field w-40"
              placeholder="42"
            />
            <p className="text-xs text-muted-foreground">
              Use the same seed for reproducible splits
            </p>
          </div>
        </div>
      )}

      {/* Package Button */}
      {scanResult && scanResult.matched.length > 0 && (
        <div>
          <button
            onClick={packageDataset}
            disabled={
              !manifestPath ||
              !annotationZipPath ||
              !outputPath ||
              !splitsValid ||
              packaging
            }
            className="btn-primary w-full"
          >
            {packaging ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Packaging Dataset...
              </>
            ) : (
              <>
                <Package className="w-4 h-4" />
                Package Dataset
              </>
            )}
          </button>
        </div>
      )}

      {/* Package Error */}
      {packageError && (
        <div className="card p-4 border-l-4 border-destructive bg-destructive/5">
          <div className="flex items-start gap-3">
            <XCircle className="w-5 h-5 text-destructive flex-shrink-0 mt-0.5" />
            <div>
              <p className="font-semibold text-destructive">Packaging Error</p>
              <p className="text-sm text-muted-foreground mt-1">{packageError}</p>
            </div>
          </div>
        </div>
      )}

      {/* Package Success */}
      {packageResult && (
        <div className="card p-6 border-l-4 border-success bg-success/5 space-y-4">
          <div className="flex items-start gap-3">
            <CheckCircle className="w-6 h-6 text-success flex-shrink-0 mt-0.5" />
            <div className="flex-1">
              <p className="text-lg font-semibold text-success">Dataset Packaged Successfully!</p>
              <p className="text-sm text-muted-foreground mt-1">
                Your YOLO dataset is ready for training
              </p>
            </div>
          </div>

          <div className="space-y-2 text-sm">
            <p>
              <span className="font-medium text-foreground">Output Path:</span>{" "}
              <span className="text-muted-foreground">{packageResult.output_path}</span>
            </p>
            <p>
              <span className="font-medium text-foreground">Train Images:</span>{" "}
              <span className="text-muted-foreground">{packageResult.train_count}</span>
            </p>
            <p>
              <span className="font-medium text-foreground">Validation Images:</span>{" "}
              <span className="text-muted-foreground">{packageResult.val_count}</span>
            </p>
            <p>
              <span className="font-medium text-foreground">Test Images:</span>{" "}
              <span className="text-muted-foreground">{packageResult.test_count}</span>
            </p>
            <p>
              <span className="font-medium text-foreground">Classes:</span>{" "}
              <span className="text-muted-foreground">{packageResult.classes.length}</span>
            </p>
          </div>

          <div className="p-3 rounded-lg bg-card border border-border">
            <p className="text-sm font-medium text-foreground mb-2">Next Steps:</p>
            <ol className="text-sm text-muted-foreground space-y-1 list-decimal list-inside">
              <li>Review the dataset structure in the output directory</li>
              <li>Use data.yaml for YOLO training configuration</li>
              <li>Verify the train/val/test splits meet your needs</li>
              <li>Start training your model!</li>
            </ol>
          </div>
        </div>
      )}
    </div>
  );
}
