# Dataset Packaging Feature

## Overview

The Dataset Packager combines annotation exports from qontinui-web with local screenshots stored by the runner to create complete YOLO-format training datasets.

## How It Works

### 1. Annotation Export (from qontinui-web)

When you export annotations from qontinui-web, you get a ZIP file containing:
- `annotations.json` - Annotation data
- `labels/` - YOLO format annotation files (.txt)
- `image_manifest.json` - Metadata about the images

The export does NOT include the actual screenshot images (to save AWS costs).

### 2. Local Screenshots (qontinui-runner)

The runner stores screenshots locally at:
- **Windows**: `C:\Users\{username}\qontinui\screenshots\{session_id}\`
- **macOS**: `~/qontinui/screenshots/{session_id}/`
- **Linux**: `~/qontinui/screenshots/{session_id}/`

### 3. Dataset Packaging Process

The Dataset tab in the runner:

1. **Scans** local storage to find images matching the manifest
2. **Verifies** images using SHA256 hash (optional but recommended)
3. **Combines** local images with annotations from the export
4. **Splits** the dataset into train/val/test sets
5. **Creates** a complete YOLO dataset structure

## Usage

### Step 1: Export Annotations from qontinui-web

1. Go to your project's dataset page
2. Click "Export Dataset"
3. Download the ZIP file
4. Extract `image_manifest.json` from the ZIP

### Step 2: Open Dataset Packager in Runner

1. Open the qontinui-runner desktop app
2. Navigate to the "Dataset" tab
3. You'll see the Dataset Packager interface

### Step 3: Select Files

1. **Image Manifest**: Browse to the extracted `image_manifest.json`
2. **Annotation ZIP**: Browse to the annotation export ZIP file
3. **Output Directory**: Select where to save the packaged dataset

### Step 4: Scan for Local Images

1. Click "Scan Local Images"
2. The packager will search your local storage
3. Review the scan results:
   - **Matched**: Images found in local storage
   - **Unmatched**: Images not found (will be excluded)
   - **Hash Verified**: Images with verified SHA256 hashes

### Step 5: Configure Dataset Split

1. Adjust the train/val/test split percentages using sliders
   - Default: 70% train, 20% validation, 10% test
   - Sliders must sum to 100%

2. Set a random seed for reproducible splits
   - Default: 42
   - Use the same seed to get identical splits

### Step 6: Package Dataset

1. Click "Package Dataset"
2. Wait for the packaging process to complete
3. Your dataset is ready!

## Output Structure

The packaged dataset follows the YOLO format:

```
output_directory/
├── data.yaml           # YOLO configuration file
├── classes.txt         # List of class names
├── images/
│   ├── train/         # Training images
│   ├── val/           # Validation images
│   └── test/          # Test images
└── labels/
    ├── train/         # Training annotations (.txt)
    ├── val/           # Validation annotations (.txt)
    └── test/          # Test annotations (.txt)
```

## Using the Dataset for Training

### With YOLOv8 (Ultralytics)

```python
from ultralytics import YOLO

# Load a model
model = YOLO('yolov8n.pt')

# Train the model
results = model.train(
    data='/path/to/output_directory/data.yaml',
    epochs=100,
    imgsz=640
)
```

### With YOLOv5

```bash
python train.py --data /path/to/output_directory/data.yaml --epochs 100 --img 640
```

## Troubleshooting

### "No matched images found"

**Cause**: The screenshots are not in the runner's local storage.

**Solutions**:
- Ensure you're using the same device that captured the screenshots
- Check the screenshot storage path in Settings
- Verify the session IDs match between manifest and local storage

### "Hash mismatch"

**Cause**: The local image file has been modified or corrupted.

**Solutions**:
- Re-capture the screenshots if possible
- The packager will still use images with hash mismatches (with a warning)
- Check if the file was compressed or edited

### "Split percentages must sum to 100%"

**Cause**: The train/val/test sliders don't add up to 100%.

**Solution**: Adjust the sliders until the total is 100%

## Best Practices

1. **Keep screenshots**: Don't delete local screenshots until you've packaged your dataset
2. **Verify hashes**: Images with verified hashes ensure data integrity
3. **Consistent splits**: Use the same random seed for reproducible experiments
4. **Backup datasets**: Save packaged datasets to external storage
5. **Test locally**: Verify the dataset structure before starting training

## Technical Details

### Image Matching

Images are matched using two methods (in order):
1. **Filename match**: Exact match of the filename
2. **SHA256 hash verification**: Optional integrity check

### Random Splitting

The dataset is shuffled using a seeded random number generator (Rust's `StdRng`). This ensures:
- Reproducible splits when using the same seed
- Fair distribution across train/val/test sets
- No temporal bias in splits

### YOLO Format

Each annotation file (`.txt`) contains one line per bounding box:
```
<class_id> <x_center> <y_center> <width> <height>
```

All coordinates are normalized to [0, 1] range.

## Limitations

1. Only YOLO format is currently supported
2. Images must be PNG format
3. Annotations must come from qontinui-web exports
4. Requires local screenshots (not available for cloud-only sessions)

## Future Enhancements

- Support for other formats (COCO, Pascal VOC)
- Cloud storage integration
- Advanced filtering options
- Data augmentation preview
- Class balancing tools
