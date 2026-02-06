# Python Executor Bundling

This document explains how to bundle the Python executor for zero-setup installation.

## Overview

The qontinui-runner can operate in two modes:

1. **Development Mode**: Uses Poetry to run `qontinui_executor.py` directly
2. **Production Mode**: Uses a bundled PyInstaller executable

When the bundled executable is present, the runner uses it automatically. This enables users to download and run without installing Python.

## Build Scripts

### Windows (PowerShell)

```powershell
# Standard build (CPU-only, ~400-500MB)
.\scripts\bundle-python.ps1

# With CUDA support (~2-3GB)
.\scripts\bundle-python.ps1 -Cuda

# Minimal build (no ML frameworks, ~80-100MB)
.\scripts\bundle-python.ps1 -Minimal

# Clean and rebuild
.\scripts\bundle-python.ps1 -Clean
```

### macOS / Linux (Bash)

```bash
# Standard build
./scripts/bundle-python.sh

# With CUDA support
./scripts/bundle-python.sh --cuda

# Minimal build
./scripts/bundle-python.sh --minimal

# Clean and rebuild
./scripts/bundle-python.sh --clean
```

## Build Variants

| Variant  | Size       | Description                     |
| -------- | ---------- | ------------------------------- |
| Minimal  | ~80-100MB  | Core Python + basic deps, no ML |
| Standard | ~400-500MB | + CPU PyTorch, OpenCV, EasyOCR  |
| CUDA     | ~2-3GB     | + GPU support                   |

## ML Models (Lazy Loading)

Large ML models are NOT included in the bundle. They are downloaded on first use:

| Model         | Size        | Purpose                       |
| ------------- | ----------- | ----------------------------- |
| SAM3 Base     | ~375MB      | Segment Anything              |
| SAM3 Large    | ~1.25GB     | Higher quality segmentation   |
| CLIP ViT-B/32 | ~354MB      | Vision-language understanding |
| EasyOCR       | ~100MB/lang | Text recognition              |

Models are stored in:

- Windows: `%APPDATA%/com.qontinui.runner/models/`
- macOS: `~/Library/Application Support/com.qontinui.runner/models/`
- Linux: `~/.local/share/com.qontinui.runner/models/`

## Python Priority

The runner tries to find Python in this order:

1. **QONTINUI_PYTHON_PATH** - Environment variable for custom Python
2. **Bundled Executor** - PyInstaller executable in `binaries/`
3. **Poetry** - Development mode with `pyproject.toml`
4. **Virtual Environment** - `.venv/` or `venv/`
5. **System Python** - Last resort fallback

## Output Location

The bundled executable is placed in:

```
src-tauri/binaries/qontinui-executor-{target}.exe  (Windows)
src-tauri/binaries/qontinui-executor-{target}      (macOS/Linux)
```

Where `{target}` is the platform triple:

- `x86_64-pc-windows-msvc` (Windows x64)
- `aarch64-pc-windows-msvc` (Windows ARM64)
- `x86_64-apple-darwin` (macOS Intel)
- `aarch64-apple-darwin` (macOS Apple Silicon)
- `x86_64-unknown-linux-gnu` (Linux x64)
- `aarch64-unknown-linux-gnu` (Linux ARM64)

## CI/CD Integration

The GitHub Actions workflow `build-python-executor.yml` automatically builds the executor for all platforms.

The release workflow can optionally include bundled Python:

```yaml
workflow_dispatch:
  inputs:
    include_bundled_python:
      description: "Bundle Python executor"
      type: boolean
      default: true
```

## Troubleshooting

### Executor Not Found

If the runner falls back to Python mode when you expect bundled mode:

1. Check the file exists in `binaries/`
2. Verify the file size is > 1MB (placeholder files are skipped)
3. Check the platform triple matches your system

### Import Errors at Runtime

If the bundled executable fails with import errors:

1. Add missing modules to `hidden_imports` in `qontinui-executor.spec`
2. Rebuild with `--clean` flag
3. Use `--debug all` flag to see detailed errors:

```bash
poetry run pyinstaller --debug all qontinui-executor.spec
```

### Model Download Issues

If models fail to download:

1. Check internet connectivity
2. Verify write permissions to the models directory
3. Check disk space (models can be several GB)
4. Look at the SHA256 verification errors in logs

## Development

When developing, the runner uses Poetry automatically. The bundled executor is only used when present and valid.

To force development mode even with bundled executor present:

```bash
# Remove or rename the bundled executor
mv binaries/qontinui-executor-x86_64-pc-windows-msvc.exe binaries/qontinui-executor-x86_64-pc-windows-msvc.exe.bak
```

Or set a custom Python path:

```bash
export QONTINUI_PYTHON_PATH=/path/to/python
```
