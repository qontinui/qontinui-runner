"""SQLite persistence for UI Bridge state machine config and element snapshots.

Stores the full config JSON for complete restoration on restart, and element
property snapshots for cross-session ID remapping.

Uses StatePersistence from the qontinui library for states/transitions
(ui_bridge_states, ui_bridge_transitions tables), and adds two extra tables:
- config_snapshots: singleton row with the full config JSON
- element_snapshots: element property snapshots for ID matching
"""

from __future__ import annotations

import json
import logging
import os
import sqlite3
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


def get_app_data_dir() -> Path:
    """Resolve the runner's app data directory.

    Reuses the same platform-specific pattern as ModelManager.
    """
    models_dir = os.environ.get("QONTINUI_MODELS_DIR")
    if models_dir:
        return Path(models_dir).parent  # Strip /models

    if sys.platform == "win32":
        app_data = os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming")
        return Path(app_data) / "com.qontinui.runner"
    elif sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "com.qontinui.runner"
    else:
        xdg_data = os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")
        return Path(xdg_data) / "com.qontinui.runner"


class StateMachinePersistence:
    """Persistence layer for state machine config and element snapshots.

    Wraps StatePersistence for states/transitions and manages extra tables
    for config JSON and element property snapshots.
    """

    def __init__(self, db_path: Path) -> None:
        self._db_path = db_path
        self._db_path.parent.mkdir(parents=True, exist_ok=True)

        # Import StatePersistence lazily so this module can be loaded
        # even if the qontinui library isn't installed yet.
        self._state_persistence: Any = None
        self._ensure_extra_tables()

    def _get_state_persistence(self) -> Any:
        """Lazy-initialize StatePersistence from qontinui library."""
        if self._state_persistence is None:
            from qontinui.state_machine.persistence import StatePersistence

            self._state_persistence = StatePersistence(self._db_path)
        return self._state_persistence

    @contextmanager
    def _get_connection(self) -> Iterator[sqlite3.Connection]:
        """Get a database connection."""
        conn = sqlite3.connect(str(self._db_path), timeout=30.0)
        conn.row_factory = sqlite3.Row
        try:
            yield conn
        finally:
            conn.close()

    def _ensure_extra_tables(self) -> None:
        """Create config_snapshots and element_snapshots tables if needed."""
        with self._get_connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                CREATE TABLE IF NOT EXISTS config_snapshots (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    config_json TEXT NOT NULL,
                    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
                )
                """
            )

            cursor.execute(
                """
                CREATE TABLE IF NOT EXISTS element_snapshots (
                    element_id TEXT PRIMARY KEY,
                    tag_name TEXT,
                    role TEXT,
                    accessible_name TEXT,
                    label TEXT,
                    position_zone TEXT,
                    classes_json TEXT,
                    captured_at TEXT DEFAULT CURRENT_TIMESTAMP
                )
                """
            )

            conn.commit()

    # =========================================================================
    # Config persistence
    # =========================================================================

    def save_config(self, config_json: dict) -> None:
        """Save the full config JSON and delegate states/transitions to StatePersistence."""
        # Store raw config JSON
        with self._get_connection() as conn:
            cursor = conn.cursor()
            now = datetime.now(UTC).isoformat()
            cursor.execute(
                """
                INSERT INTO config_snapshots (id, config_json, created_at, updated_at)
                VALUES (1, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    config_json = excluded.config_json,
                    updated_at = excluded.updated_at
                """,
                (json.dumps(config_json), now, now),
            )
            conn.commit()

        # Delegate states/transitions to StatePersistence
        try:
            from qontinui.state_machine.ui_bridge_runtime import (
                UIBridgeState,
                UIBridgeTransition,
            )

            persistence = self._get_state_persistence()
            persistence.clear_all()

            states = []
            for state_data in config_json.get("states", {}).values():
                states.append(UIBridgeState(**state_data))
            if states:
                persistence.save_states(states)

            transitions = []
            for trans_data in config_json.get("transitions", {}).values():
                transitions.append(UIBridgeTransition(**trans_data))
            if transitions:
                persistence.save_transitions(transitions)

            logger.info(f"Persisted config: {len(states)} states, {len(transitions)} transitions")
        except ImportError:
            logger.warning(
                "qontinui library not available; config JSON saved but states/transitions skipped"
            )
        except Exception as e:
            logger.warning(f"Failed to persist states/transitions: {e}")

    def load_config(self) -> dict | None:
        """Load persisted config JSON. Returns None if nothing persisted."""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT config_json FROM config_snapshots WHERE id = 1")
            row = cursor.fetchone()
            if row is None:
                return None
            try:
                result: dict[Any, Any] = json.loads(row["config_json"])
                return result
            except (json.JSONDecodeError, TypeError):
                return None

    # =========================================================================
    # Element snapshots
    # =========================================================================

    def save_element_snapshots(self, snapshots: dict[str, dict]) -> None:
        """Save element property snapshots for ID mapping.

        Args:
            snapshots: Mapping of element_id -> property dict with keys:
                tag_name, role, accessible_name, label, position_zone, classes
        """
        with self._get_connection() as conn:
            cursor = conn.cursor()

            # Clear old snapshots
            cursor.execute("DELETE FROM element_snapshots")

            now = datetime.now(UTC).isoformat()
            for elem_id, props in snapshots.items():
                cursor.execute(
                    """
                    INSERT INTO element_snapshots
                        (element_id, tag_name, role, accessible_name, label,
                         position_zone, classes_json, captured_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        elem_id,
                        props.get("tag_name", ""),
                        props.get("role", ""),
                        props.get("accessible_name", ""),
                        props.get("label", ""),
                        props.get("position_zone", ""),
                        json.dumps(props.get("classes", [])),
                        now,
                    ),
                )

            conn.commit()
            logger.debug(f"Saved {len(snapshots)} element snapshots")

    def load_element_snapshots(self) -> dict[str, dict]:
        """Load stored element property snapshots.

        Returns:
            Mapping of element_id -> property dict
        """
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT element_id, tag_name, role, accessible_name, label,
                       position_zone, classes_json
                FROM element_snapshots
                """
            )

            snapshots: dict[str, dict] = {}
            for row in cursor.fetchall():
                snapshots[row["element_id"]] = {
                    "tag_name": row["tag_name"] or "",
                    "role": row["role"] or "",
                    "accessible_name": row["accessible_name"] or "",
                    "label": row["label"] or "",
                    "position_zone": row["position_zone"] or "",
                    "classes": json.loads(row["classes_json"] or "[]"),
                }
            return snapshots

    # =========================================================================
    # Lifecycle
    # =========================================================================

    def clear(self) -> None:
        """Clear all persisted data."""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM config_snapshots")
            cursor.execute("DELETE FROM element_snapshots")
            conn.commit()

        try:
            persistence = self._get_state_persistence()
            persistence.clear_all()
        except Exception:
            pass

        logger.info("Cleared all state machine persistence data")

    def has_data(self) -> bool:
        """Check if there's a persisted state machine."""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) FROM config_snapshots")
            count = cursor.fetchone()[0]
            return bool(count > 0)
