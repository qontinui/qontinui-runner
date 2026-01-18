#!/bin/bash
#
# Bundle Python executor for qontinui-runner distribution.
#
# This script creates a standalone Python executable that can be distributed
# with the qontinui-runner without requiring users to install Python.
#
# Usage:
#   ./bundle-python.sh              # Standard CPU-only build
#   ./bundle-python.sh --cuda       # Include CUDA support
#   ./bundle-python.sh --minimal    # Minimal build (no ML)
#   ./bundle-python.sh --clean      # Clean artifacts first
#   ./bundle-python.sh --no-sam3    # Exclude SAM3 support
#   ./bundle-python.sh --skip-copy  # Don't copy to Tauri binaries
#
# Size Targets:
#   Minimal:  ~80-100MB  (no ML)
#   Standard: ~400-500MB (CPU PyTorch, OpenCV, EasyOCR framework)
#   CUDA:     ~2-3GB     (full GPU support)
#
# ML Models (downloaded on demand):
#   SAM3:     ~2.5GB
#   CLIP:     ~1.5GB
#   EasyOCR:  ~100MB per language

set -e

# Parse arguments
CLEAN=0
CUDA=0
NO_SAM3=0
SKIP_COPY=0
MINIMAL=0

while [[ $# -gt 0 ]]; do
    case $1 in
        --clean)
            CLEAN=1
            shift
            ;;
        --cuda)
            CUDA=1
            shift
            ;;
        --no-sam3)
            NO_SAM3=1
            shift
            ;;
        --skip-copy)
            SKIP_COPY=1
            shift
            ;;
        --minimal)
            MINIMAL=1
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --clean      Clean build artifacts before building"
            echo "  --cuda       Include CUDA support (larger bundle)"
            echo "  --no-sam3    Exclude SAM3 support"
            echo "  --skip-copy  Skip copying to Tauri binaries"
            echo "  --minimal    Build minimal version (no ML frameworks)"
            echo "  -h, --help   Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_DIR="$(dirname "$SCRIPT_DIR")"
PYTHON_BRIDGE_DIR="$RUNNER_DIR/python-bridge"
TAURI_DIR="$RUNNER_DIR/src-tauri"
BINARIES_DIR="$TAURI_DIR/binaries"

# Detect platform
detect_platform() {
    local os=$(uname -s | tr '[:upper:]' '[:lower:]')
    local arch=$(uname -m)

    case "$os" in
        darwin)
            case "$arch" in
                arm64)
                    echo "aarch64-apple-darwin"
                    ;;
                x86_64)
                    echo "x86_64-apple-darwin"
                    ;;
                *)
                    echo "unsupported"
                    ;;
            esac
            ;;
        linux)
            case "$arch" in
                x86_64)
                    echo "x86_64-unknown-linux-gnu"
                    ;;
                aarch64)
                    echo "aarch64-unknown-linux-gnu"
                    ;;
                *)
                    echo "unsupported"
                    ;;
            esac
            ;;
        *)
            echo "unsupported"
            ;;
    esac
}

PLATFORM=$(detect_platform)

if [ "$PLATFORM" = "unsupported" ]; then
    echo "ERROR: Unsupported platform: $(uname -s) $(uname -m)"
    exit 1
fi

echo "============================================================"
echo "  Qontinui Executor Build Script ($(uname -s))"
echo "============================================================"
echo ""
echo "  Platform:      $PLATFORM"
echo "  Python Bridge: $PYTHON_BRIDGE_DIR"
echo "  Output:        $BINARIES_DIR"
echo ""

# Check prerequisites
check_prerequisites() {
    echo "Checking prerequisites..."

    # Check Python
    if ! command -v python3 &> /dev/null; then
        echo "ERROR: Python3 is not installed or not in PATH"
        exit 1
    fi
    echo "  Python:    $(python3 --version)"

    # Check Poetry
    if ! command -v poetry &> /dev/null; then
        echo "ERROR: Poetry is not installed or not in PATH"
        exit 1
    fi
    echo "  Poetry:    $(poetry --version)"

    # Check PyInstaller (via poetry)
    pushd "$PYTHON_BRIDGE_DIR" > /dev/null
    if ! poetry run pyinstaller --version &> /dev/null; then
        echo "  Installing PyInstaller..."
        poetry add pyinstaller --group dev
    else
        echo "  PyInstaller: $(poetry run pyinstaller --version)"
    fi
    popd > /dev/null

    echo ""
}

# Clean build artifacts
clean_build_artifacts() {
    echo "Cleaning build artifacts..."

    for dir in build dist __pycache__; do
        if [ -d "$PYTHON_BRIDGE_DIR/$dir" ]; then
            echo "  Removing: $PYTHON_BRIDGE_DIR/$dir"
            rm -rf "$PYTHON_BRIDGE_DIR/$dir"
        fi
    done

    # Clean .pyc files
    find "$PYTHON_BRIDGE_DIR" -name "*.pyc" -delete

    echo ""
}

# Build the executable
build_executor() {
    echo "Building qontinui-executor..."
    echo ""

    pushd "$PYTHON_BRIDGE_DIR" > /dev/null

    # Set environment variables for the spec file
    export QONTINUI_INCLUDE_CUDA=$CUDA
    export QONTINUI_INCLUDE_SAM3=$((1 - NO_SAM3))
    export QONTINUI_MINIMAL=$MINIMAL

    # Ensure dependencies are installed
    echo "  Installing dependencies..."
    poetry install --no-interaction

    # Build arguments
    BUILD_ARGS=""
    if [ $CUDA -eq 1 ]; then
        BUILD_ARGS="$BUILD_ARGS --cuda"
    fi
    if [ $NO_SAM3 -eq 1 ]; then
        BUILD_ARGS="$BUILD_ARGS --no-sam3"
    fi
    if [ $CLEAN -eq 1 ]; then
        BUILD_ARGS="$BUILD_ARGS --clean"
    fi
    if [ $SKIP_COPY -eq 1 ]; then
        BUILD_ARGS="$BUILD_ARGS --skip-copy"
    fi

    echo "  Running PyInstaller..."
    poetry run python build_executor.py $BUILD_ARGS

    popd > /dev/null
    echo ""
}

# Copy to Tauri binaries
copy_to_tauri() {
    if [ $SKIP_COPY -eq 1 ]; then
        echo "Skipping copy to Tauri binaries (--skip-copy specified)"
        return
    fi

    echo "Copying to Tauri binaries..."

    # Ensure binaries directory exists
    mkdir -p "$BINARIES_DIR"

    # Source and destination
    local source_exe="$PYTHON_BRIDGE_DIR/dist/qontinui-executor"
    local dest_exe="$BINARIES_DIR/qontinui-executor-$PLATFORM"

    if [ ! -f "$source_exe" ]; then
        echo "ERROR: Built executable not found at: $source_exe"
        exit 1
    fi

    echo "  From: $source_exe"
    echo "  To:   $dest_exe"

    cp "$source_exe" "$dest_exe"
    chmod +x "$dest_exe"

    # Get file size
    local size_bytes=$(stat -f%z "$dest_exe" 2>/dev/null || stat -c%s "$dest_exe" 2>/dev/null)
    local size_mb=$(echo "scale=2; $size_bytes / 1048576" | bc)

    echo ""
    echo "  Executable size: ${size_mb} MB"
    echo ""
}

# Verify the build
verify_build() {
    echo "Verifying build..."

    local exe_path="$BINARIES_DIR/qontinui-executor-$PLATFORM"

    if [ ! -f "$exe_path" ]; then
        exe_path="$PYTHON_BRIDGE_DIR/dist/qontinui-executor"
    fi

    if [ ! -f "$exe_path" ]; then
        echo "ERROR: Executable not found"
        exit 1
    fi

    # Quick version check
    if "$exe_path" --version 2>/dev/null; then
        echo "  Version check: PASSED"
    else
        echo "  Version check: SKIPPED (no --version flag)"
    fi

    echo ""
}

# Print summary
show_summary() {
    local build_type="Standard (CPU)"
    if [ $MINIMAL -eq 1 ]; then
        build_type="Minimal"
    elif [ $CUDA -eq 1 ]; then
        build_type="CUDA"
    fi

    echo "============================================================"
    echo "  BUILD COMPLETE"
    echo "============================================================"
    echo ""
    echo "  Build type:   $build_type"
    echo "  SAM3 support: $([ $NO_SAM3 -eq 1 ] && echo 'No' || echo 'Yes')"
    echo ""
    echo "  Next steps:"
    echo "  1. Run 'npm run tauri build' to create the installer"
    echo "  2. The installer will include the bundled Python executor"
    echo ""
    echo "  For development, use 'npm run tauri dev' which falls back"
    echo "  to Poetry-based Python when the bundled executor is not found."
    echo ""
}

# Main execution
check_prerequisites

if [ $CLEAN -eq 1 ]; then
    clean_build_artifacts
fi

build_executor
copy_to_tauri
verify_build
show_summary
