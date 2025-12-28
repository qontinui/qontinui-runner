"""
MOVED to qontinui-train/export/training_export_service.py

Import from qontinui_train.export instead.
"""

import warnings

warnings.warn(
    "training_export_service.py has been moved to qontinui-train. "
    "Import from qontinui_train.export instead.",
    DeprecationWarning,
    stacklevel=2,
)

try:
    from qontinui_train.export.training_export_service import *  # noqa: F403
except ImportError:
    # Re-raise ImportError so calling code can catch it gracefully
    raise ImportError(
        "qontinui-train is not installed. Training export functionality unavailable."
    ) from None
