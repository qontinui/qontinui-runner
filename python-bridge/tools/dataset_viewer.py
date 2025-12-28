"""
MOVED to qontinui-train/export/dataset_viewer.py

Import from qontinui_train.export instead.
"""

import warnings

warnings.warn(
    "dataset_viewer.py has been moved to qontinui-train. "
    "Import from qontinui_train.export instead.",
    DeprecationWarning,
    stacklevel=2,
)

try:
    from qontinui_train.export.dataset_viewer import *  # noqa: F403
except ImportError:
    # Re-raise ImportError so calling code can catch it gracefully
    raise ImportError(
        "qontinui-train is not installed. Dataset viewer functionality unavailable."
    ) from None
