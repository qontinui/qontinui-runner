#!/usr/bin/env python3
"""
Build script for qontinui-executor

This script builds the PyInstaller bundle and copies it to the correct
location for Tauri sidecar integration.

Usage:
    python build_executor.py              # CPU-only build
    python build_executor.py --cuda       # Include CUDA support
    python build_executor.py --no-sam3    # Exclude SAM3
    python build_executor.py --clean      # Clean build artifacts first
"""

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path


def get_platform_target():
    """Get the Tauri platform target triple."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == 'windows':
        if machine in ('amd64', 'x86_64'):
            return 'x86_64-pc-windows-msvc'
        elif machine == 'arm64':
            return 'aarch64-pc-windows-msvc'
    elif system == 'darwin':
        if machine == 'arm64':
            return 'aarch64-apple-darwin'
        else:
            return 'x86_64-apple-darwin'
    elif system == 'linux':
        if machine in ('amd64', 'x86_64'):
            return 'x86_64-unknown-linux-gnu'
        elif machine == 'aarch64':
            return 'aarch64-unknown-linux-gnu'

    raise ValueError(f"Unsupported platform: {system}-{machine}")


def get_executable_name():
    """Get the executable name for the current platform."""
    if platform.system() == 'Windows':
        return 'qontinui-executor.exe'
    return 'qontinui-executor'


def clean_build_artifacts(spec_dir: Path):
    """Clean PyInstaller build artifacts."""
    print("Cleaning build artifacts...")

    dirs_to_clean = ['build', 'dist', '__pycache__']
    for dir_name in dirs_to_clean:
        dir_path = spec_dir / dir_name
        if dir_path.exists():
            print(f"  Removing {dir_path}")
            shutil.rmtree(dir_path)

    # Clean .pyc files
    for pyc in spec_dir.rglob('*.pyc'):
        pyc.unlink()


def build_executable(spec_dir: Path, include_cuda: bool, include_sam3: bool):
    """Build the PyInstaller executable."""
    print("\n" + "=" * 60)
    print("Building qontinui-executor")
    print("=" * 60)
    print(f"  Platform:     {platform.system()} {platform.machine()}")
    print(f"  Target:       {get_platform_target()}")
    print(f"  Include CUDA: {include_cuda}")
    print(f"  Include SAM3: {include_sam3}")
    print("=" * 60 + "\n")

    # Set environment variables for the spec file
    env = os.environ.copy()
    env['QONTINUI_INCLUDE_CUDA'] = '1' if include_cuda else '0'
    env['QONTINUI_INCLUDE_SAM3'] = '1' if include_sam3 else '0'

    # Run PyInstaller
    spec_file = spec_dir / 'qontinui-executor.spec'
    cmd = [
        sys.executable, '-m', 'PyInstaller',
        '--clean',
        '--noconfirm',
        str(spec_file),
    ]

    print(f"Running: {' '.join(cmd)}\n")

    result = subprocess.run(cmd, env=env, cwd=str(spec_dir))
    if result.returncode != 0:
        print("\nBuild failed!")
        sys.exit(1)

    print("\nBuild completed successfully!")
    return spec_dir / 'dist' / get_executable_name()


def copy_to_tauri_binaries(executable: Path, tauri_dir: Path):
    """Copy the executable to Tauri binaries directory with platform-specific naming."""
    binaries_dir = tauri_dir / 'binaries'
    binaries_dir.mkdir(parents=True, exist_ok=True)

    target = get_platform_target()
    if platform.system() == 'Windows':
        dest_name = f'qontinui-executor-{target}.exe'
    else:
        dest_name = f'qontinui-executor-{target}'

    dest_path = binaries_dir / dest_name

    print(f"\nCopying executable to Tauri binaries:")
    print(f"  From: {executable}")
    print(f"  To:   {dest_path}")

    shutil.copy2(executable, dest_path)

    # Make executable on Unix
    if platform.system() != 'Windows':
        dest_path.chmod(0o755)

    print(f"\nExecutable ready at: {dest_path}")
    return dest_path


def main():
    parser = argparse.ArgumentParser(description='Build qontinui-executor')
    parser.add_argument('--cuda', action='store_true', help='Include CUDA support')
    parser.add_argument('--no-sam3', action='store_true', help='Exclude SAM3')
    parser.add_argument('--clean', action='store_true', help='Clean build artifacts first')
    parser.add_argument('--skip-copy', action='store_true', help='Skip copying to Tauri binaries')
    args = parser.parse_args()

    # Paths
    spec_dir = Path(__file__).parent
    tauri_dir = spec_dir.parent / 'src-tauri'

    # Clean if requested
    if args.clean:
        clean_build_artifacts(spec_dir)

    # Build
    executable = build_executable(
        spec_dir,
        include_cuda=args.cuda,
        include_sam3=not args.no_sam3,
    )

    # Copy to Tauri binaries
    if not args.skip_copy and tauri_dir.exists():
        copy_to_tauri_binaries(executable, tauri_dir)
    elif not args.skip_copy:
        print(f"\nWarning: Tauri directory not found at {tauri_dir}")
        print("Executable is in dist/ directory")

    print("\n" + "=" * 60)
    print("BUILD COMPLETE")
    print("=" * 60)
    print("\nNext steps:")
    print("  1. Update src-tauri/tauri.conf.json to enable sidecar")
    print("  2. Update Rust code to spawn the bundled executable")
    print("  3. Run 'npm run tauri build' to create the installer")


if __name__ == '__main__':
    main()
