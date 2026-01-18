<#
.SYNOPSIS
    Bundles the Python executor for qontinui-runner distribution.

.DESCRIPTION
    This script creates a standalone Python executable that can be distributed
    with the qontinui-runner without requiring users to install Python.

    The bundled executable includes:
    - Python 3.12 runtime
    - Core dependencies (OpenCV, Pillow, NumPy, Playwright, etc.)
    - The qontinui library
    - The python-bridge executor

    Large ML models (SAM, CLIP, EasyOCR language packs) are NOT bundled.
    They are downloaded on first use via the model manager.

.PARAMETER Clean
    Clean build artifacts before building.

.PARAMETER Cuda
    Include CUDA support (larger bundle, ~2GB vs ~500MB).

.PARAMETER NoSam3
    Exclude SAM3 support entirely.

.PARAMETER SkipCopy
    Skip copying to Tauri binaries directory.

.PARAMETER Minimal
    Build minimal version (no ML frameworks, ~100MB).

.EXAMPLE
    .\bundle-python.ps1
    # Standard CPU-only build with all features

.EXAMPLE
    .\bundle-python.ps1 -Cuda
    # Build with CUDA support for GPU acceleration

.EXAMPLE
    .\bundle-python.ps1 -Minimal
    # Minimal build for testing (no ML models)

.EXAMPLE
    .\bundle-python.ps1 -Clean
    # Clean previous build artifacts first

.NOTES
    Size Targets:
    - Minimal:  ~80-100MB  (no ML)
    - Standard: ~400-500MB (CPU PyTorch, OpenCV, EasyOCR framework)
    - CUDA:     ~2-3GB     (full GPU support)

    ML Models (downloaded on demand):
    - SAM3:     ~2.5GB
    - CLIP:     ~1.5GB
    - EasyOCR:  ~100MB per language
#>

[CmdletBinding()]
param(
    [switch]$Clean,
    [switch]$Cuda,
    [switch]$NoSam3,
    [switch]$SkipCopy,
    [switch]$Minimal
)

$ErrorActionPreference = "Stop"

# Paths
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RunnerDir = Split-Path -Parent $ScriptDir
$PythonBridgeDir = Join-Path $RunnerDir "python-bridge"
$TauriDir = Join-Path $RunnerDir "src-tauri"
$BinariesDir = Join-Path $TauriDir "binaries"

# Detect platform
$Platform = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
    "aarch64-pc-windows-msvc"
} else {
    "x86_64-pc-windows-msvc"
}

Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  Qontinui Executor Build Script (Windows)" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Platform:     $Platform"
Write-Host "  Python Bridge: $PythonBridgeDir"
Write-Host "  Output:       $BinariesDir"
Write-Host ""

# Check prerequisites
function Test-Prerequisites {
    Write-Host "Checking prerequisites..." -ForegroundColor Yellow

    # Check Python
    $pythonVersion = python --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Python is not installed or not in PATH"
    }
    Write-Host "  Python:    $pythonVersion" -ForegroundColor Green

    # Check Poetry
    $poetryVersion = poetry --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Poetry is not installed or not in PATH"
    }
    Write-Host "  Poetry:    $poetryVersion" -ForegroundColor Green

    # Check PyInstaller (via poetry)
    Push-Location $PythonBridgeDir
    try {
        $pyinstallerVersion = poetry run pyinstaller --version 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  Installing PyInstaller..." -ForegroundColor Yellow
            poetry add pyinstaller --group dev
        } else {
            Write-Host "  PyInstaller: $pyinstallerVersion" -ForegroundColor Green
        }
    } finally {
        Pop-Location
    }

    Write-Host ""
}

# Clean build artifacts
function Clear-BuildArtifacts {
    Write-Host "Cleaning build artifacts..." -ForegroundColor Yellow

    $dirsToClean = @(
        (Join-Path $PythonBridgeDir "build"),
        (Join-Path $PythonBridgeDir "dist"),
        (Join-Path $PythonBridgeDir "__pycache__")
    )

    foreach ($dir in $dirsToClean) {
        if (Test-Path $dir) {
            Write-Host "  Removing: $dir"
            Remove-Item -Recurse -Force $dir
        }
    }

    # Clean .pyc files
    Get-ChildItem -Path $PythonBridgeDir -Filter "*.pyc" -Recurse | Remove-Item -Force

    Write-Host ""
}

# Build the executable
function Build-Executor {
    Write-Host "Building qontinui-executor..." -ForegroundColor Yellow
    Write-Host ""

    Push-Location $PythonBridgeDir
    try {
        # Set environment variables for the spec file
        $env:QONTINUI_INCLUDE_CUDA = if ($Cuda) { "1" } else { "0" }
        $env:QONTINUI_INCLUDE_SAM3 = if ($NoSam3) { "0" } else { "1" }
        $env:QONTINUI_MINIMAL = if ($Minimal) { "1" } else { "0" }

        # Ensure dependencies are installed
        Write-Host "  Installing dependencies..." -ForegroundColor Yellow
        poetry install --no-interaction

        # Run the build script
        Write-Host "  Running PyInstaller..." -ForegroundColor Yellow
        $buildArgs = @()
        if ($Cuda) { $buildArgs += "--cuda" }
        if ($NoSam3) { $buildArgs += "--no-sam3" }
        if ($Clean) { $buildArgs += "--clean" }
        if ($SkipCopy) { $buildArgs += "--skip-copy" }

        poetry run python build_executor.py @buildArgs

        if ($LASTEXITCODE -ne 0) {
            throw "Build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }

    Write-Host ""
}

# Copy to Tauri binaries
function Copy-ToTauri {
    if ($SkipCopy) {
        Write-Host "Skipping copy to Tauri binaries (--SkipCopy specified)" -ForegroundColor Yellow
        return
    }

    Write-Host "Copying to Tauri binaries..." -ForegroundColor Yellow

    # Ensure binaries directory exists
    if (!(Test-Path $BinariesDir)) {
        New-Item -ItemType Directory -Path $BinariesDir -Force | Out-Null
    }

    # Source and destination
    $sourceExe = Join-Path $PythonBridgeDir "dist\qontinui-executor.exe"
    $destExe = Join-Path $BinariesDir "qontinui-executor-$Platform.exe"

    if (!(Test-Path $sourceExe)) {
        throw "Built executable not found at: $sourceExe"
    }

    Write-Host "  From: $sourceExe"
    Write-Host "  To:   $destExe"

    Copy-Item -Path $sourceExe -Destination $destExe -Force

    # Get file size
    $sizeBytes = (Get-Item $destExe).Length
    $sizeMB = [math]::Round($sizeBytes / 1MB, 2)

    Write-Host ""
    Write-Host "  Executable size: $sizeMB MB" -ForegroundColor Green
    Write-Host ""
}

# Verify the build
function Test-Build {
    Write-Host "Verifying build..." -ForegroundColor Yellow

    $exePath = Join-Path $BinariesDir "qontinui-executor-$Platform.exe"

    if (!(Test-Path $exePath)) {
        $exePath = Join-Path $PythonBridgeDir "dist\qontinui-executor.exe"
    }

    if (!(Test-Path $exePath)) {
        throw "Executable not found"
    }

    # Quick version check
    $output = & $exePath --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  Version check: PASSED" -ForegroundColor Green
        Write-Host "  Output: $output"
    } else {
        Write-Host "  Version check: SKIPPED (no --version flag)" -ForegroundColor Yellow
    }

    Write-Host ""
}

# Print summary
function Show-Summary {
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host "  BUILD COMPLETE" -ForegroundColor Cyan
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  Build type:   $(if ($Minimal) {'Minimal'} elseif ($Cuda) {'CUDA'} else {'Standard (CPU)'})"
    Write-Host "  SAM3 support: $(if ($NoSam3) {'No'} else {'Yes'})"
    Write-Host ""
    Write-Host "  Next steps:" -ForegroundColor Yellow
    Write-Host "  1. Run 'npm run tauri build' to create the installer"
    Write-Host "  2. The installer will include the bundled Python executor"
    Write-Host ""
    Write-Host "  For development, use 'npm run tauri dev' which falls back"
    Write-Host "  to Poetry-based Python when the bundled executor is not found."
    Write-Host ""
}

# Main execution
try {
    Test-Prerequisites

    if ($Clean) {
        Clear-BuildArtifacts
    }

    Build-Executor
    Copy-ToTauri
    Test-Build
    Show-Summary

} catch {
    Write-Host ""
    Write-Host "ERROR: $_" -ForegroundColor Red
    Write-Host ""
    exit 1
}
