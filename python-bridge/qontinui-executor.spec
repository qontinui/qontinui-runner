# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec file for qontinui-executor

This bundles the Python bridge and qontinui library into a single executable
for distribution with the Tauri-based qontinui-runner.

Build commands:
    CPU-only build:
        pyinstaller qontinui-executor.spec

    With CUDA support (larger bundle):
        QONTINUI_INCLUDE_CUDA=1 pyinstaller qontinui-executor.spec

    Debug build (console visible):
        pyinstaller qontinui-executor.spec --debug all
"""

import os
import sys
from pathlib import Path

# Determine build configuration
INCLUDE_CUDA = os.getenv('QONTINUI_INCLUDE_CUDA', '0') == '1'
INCLUDE_SAM3 = os.getenv('QONTINUI_INCLUDE_SAM3', '1') == '1'

# Paths
SPEC_DIR = Path(SPECPATH)
PYTHON_BRIDGE_DIR = SPEC_DIR
QONTINUI_LIB_DIR = SPEC_DIR.parent.parent / 'qontinui' / 'src'

# Verify qontinui library exists
if not QONTINUI_LIB_DIR.exists():
    raise FileNotFoundError(f"Qontinui library not found at: {QONTINUI_LIB_DIR}")

print(f"Building qontinui-executor")
print(f"  Python bridge: {PYTHON_BRIDGE_DIR}")
print(f"  Qontinui lib:  {QONTINUI_LIB_DIR}")
print(f"  Include CUDA:  {INCLUDE_CUDA}")
print(f"  Include SAM3:  {INCLUDE_SAM3}")

# =============================================================================
# Analysis Configuration
# =============================================================================

# Hidden imports - modules that are imported dynamically
hidden_imports = [
    # Python bridge modules
    'event_translator',
    'execution_tree',
    'action_definitions',
    'event_emitter',
    'executor_wrapper',
    'websocket_client',
    'websocket_config',
    'services',
    'services.unified_data_collector',
    'services.screenshot_service',
    'services.tree_view_layer',
    'services.capture_tool_service',
    'services.training_export_service',
    'services.state_detection_service',
    'models',
    'models.action_execution_record',
    'exporters',
    'exporters.training_data_exporter',

    # Qontinui library core
    'qontinui',
    'qontinui.actions',
    'qontinui.config',
    'qontinui.discovery',
    'qontinui.discovery.experimental',
    'qontinui.discovery.experimental.sam3_detector',
    'qontinui.discovery.element_detection',
    'qontinui.discovery.region_analysis',
    'qontinui.discovery.state_detection',
    'qontinui.discovery.state_construction',
    'qontinui.discovery.pixel_analysis',
    'qontinui.find',
    'qontinui.model',
    'qontinui.model.state',
    'qontinui.model.element',
    'qontinui.model.transition',
    'qontinui.primitives',
    'qontinui.action_executors',
    'qontinui.json_executor',
    'qontinui.json_executor.config_parser',
    'qontinui.reporting',
    'qontinui.navigation_api',
    'qontinui.registry',
    'qontinui.dsl',
    'qontinui.state_management',

    # Core dependencies
    'PIL',
    'PIL.Image',
    'PIL.ImageGrab',
    'cv2',
    'numpy',
    'scipy',
    'scipy.ndimage',
    'mss',
    'pynput',
    'pynput.mouse',
    'pynput.keyboard',
    'pyautogui',
    'easyocr',
    'structlog',
    'pydantic',
    'pydantic.fields',
    'websockets',
    'websockets.client',
    'asyncio',
    'json',
    'logging',

    # Windows-specific
    'ctypes',
    'ctypes.wintypes',
    'win32api',
    'win32con',
    'win32gui',
]

# PyTorch hidden imports (always included, CPU or CUDA)
pytorch_imports = [
    'torch',
    'torch.nn',
    'torch.nn.functional',
    'torch.utils',
    'torch.utils.data',
    'torchvision',
    'torchvision.transforms',
    'torchvision.models',
]
hidden_imports.extend(pytorch_imports)

# SAM3 hidden imports (if enabled)
if INCLUDE_SAM3:
    sam3_imports = [
        'sam3',
        'sam3.build_sam',
        'sam3.modeling',
        'sam3.utils',
    ]
    hidden_imports.extend(sam3_imports)

# =============================================================================
# Excludes - modules to exclude to reduce bundle size
# =============================================================================

excludes = [
    # Testing frameworks
    'pytest',
    'pytest_asyncio',
    '_pytest',
    'unittest',

    # Development tools
    'black',
    'mypy',
    'pylint',
    'flake8',
    'isort',
    'ruff',
    'pre_commit',

    # Documentation
    'sphinx',
    'docutils',

    # Jupyter/IPython
    'IPython',
    'jupyter',
    'notebook',
    'ipykernel',

    # Unused ML frameworks
    'tensorflow',
    'keras',
    'jax',
    'flax',

    # Unused visualization
    'matplotlib',
    'seaborn',
    'plotly',

    # Large unused packages
    'pandas',  # Re-add if needed
    'transformers',  # Large, add only if using for embeddings
]

# Exclude CUDA libraries if building CPU-only
if not INCLUDE_CUDA:
    excludes.extend([
        'torch.cuda',
        'cudnn',
        'cublas',
        'cufft',
        'curand',
        'cusparse',
        'nccl',
    ])

# =============================================================================
# Data files and binaries
# =============================================================================

# Data files to include
datas = [
    # Include qontinui library source
    (str(QONTINUI_LIB_DIR / 'qontinui'), 'qontinui'),
]

# Binary files (DLLs, shared libraries)
binaries = []

# =============================================================================
# Analysis
# =============================================================================

a = Analysis(
    [str(PYTHON_BRIDGE_DIR / 'qontinui_executor.py')],
    pathex=[
        str(PYTHON_BRIDGE_DIR),
        str(QONTINUI_LIB_DIR),
    ],
    binaries=binaries,
    datas=datas,
    hiddenimports=hidden_imports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=excludes,
    noarchive=False,
    optimize=0,
)

# =============================================================================
# PYZ Archive
# =============================================================================

pyz = PYZ(a.pure)

# =============================================================================
# Executable
# =============================================================================

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='qontinui-executor',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,  # Enable UPX compression to reduce size
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,  # Keep console for stdio communication with Tauri
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=None,  # Add icon path if desired
)

# =============================================================================
# Notes
# =============================================================================
"""
Post-build steps:

1. The executable will be in: dist/qontinui-executor.exe (Windows)
   or dist/qontinui-executor (macOS/Linux)

2. Copy to Tauri binaries directory with platform-specific naming:
   - Windows: binaries/qontinui-executor-x86_64-pc-windows-msvc.exe
   - macOS:   binaries/qontinui-executor-x86_64-apple-darwin
   - macOS ARM: binaries/qontinui-executor-aarch64-apple-darwin
   - Linux:   binaries/qontinui-executor-x86_64-unknown-linux-gnu

3. Update tauri.conf.json to include the sidecar

4. For SAM3 model checkpoints:
   - Either bundle with the installer (large)
   - Or implement first-run download in the executor

Troubleshooting:
- If imports fail at runtime, add them to hidden_imports
- Use --debug all flag to see detailed import errors
- Check that all __init__.py files exist in packages
"""
