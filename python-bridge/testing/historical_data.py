"""Historical data management for GUI simulation.

This module manages the database of historical captures used for simulating
GUI responses during integration testing. The database is built from real
automation runs (video recordings + action execution records).

Key Concept:
    This is NOT a mock of external applications. This is a database of real
    GUI responses captured during actual automation runs. It allows us to test
    the automation code itself by simulating how the GUI responded in the past.
"""

import json
import logging
import sqlite3
import uuid
from pathlib import Path
from typing import List, Set, Dict, Any, Optional
from datetime import datetime

try:
    from ..models.historical_capture import HistoricalCapture
    from ..models.action_execution_record import ActionExecutionRecord
except ImportError:
    # Fallback for direct execution
    from models.historical_capture import HistoricalCapture
    from models.action_execution_record import ActionExecutionRecord

logger = logging.getLogger(__name__)


class HistoricalData:
    """Database of historical captures for simulation.

    Built from automation run recordings (video + events).
    Source: Captured videos and data from automation executions.

    The database stores historical GUI responses that occurred during real
    automation runs. During testing, when an action is simulated, we query
    this database to find how the GUI actually responded in similar contexts.

    Database Schema:
        captures table:
            - capture_id: Unique identifier
            - action_type: Type of action (CLICK, TYPE, etc.)
            - action_target: Simplified target identifier
            - action_config_json: Full action configuration as JSON
            - states_before_json: Active states before action (JSON array)
            - states_after_json: Active states after action (JSON array)
            - success: Boolean success flag
            - error_message: Error message if failed
            - screenshot_before: Path to before screenshot
            - screenshot_after: Path to after screenshot
            - video_path: Path to source video
            - video_timestamp: Timestamp in video
            - run_id: Source automation run ID
            - original_action_id: Original ActionExecutionRecord ID
            - capture_timestamp: When capture was created
            - metadata_json: Additional data as JSON

        runs table:
            - run_id: Unique run identifier
            - video_path: Path to run video
            - events_path: Path to events file
            - import_timestamp: When run was imported
            - total_actions: Count of actions in run
            - metadata_json: Additional run metadata
    """

    def __init__(self, db_path: Optional[str] = None):
        """Initialize historical data database.

        Args:
            db_path: Path to SQLite database file. If None, uses default location
                    in testing directory. If ":memory:", creates in-memory database.
        """
        if db_path is None:
            db_dir = Path(__file__).parent / "data"
            db_dir.mkdir(exist_ok=True)
            db_path = str(db_dir / "historical_captures.db")

        self.db_path = db_path
        self.conn = sqlite3.connect(db_path, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        self._initialize_database()

        logger.info(f"Initialized HistoricalData database at {db_path}")

    def _initialize_database(self):
        """Create database tables if they don't exist."""
        with self.conn:
            # Captures table - matches HistoricalCapture model structure
            self.conn.execute("""
                CREATE TABLE IF NOT EXISTS captures (
                    session_id TEXT NOT NULL,
                    timestamp REAL NOT NULL,
                    frame_before INTEGER NOT NULL,
                    frame_after INTEGER NOT NULL,
                    states_before_json TEXT NOT NULL,
                    action_type TEXT NOT NULL,
                    action_target TEXT NOT NULL,
                    action_params_json TEXT,
                    states_after_json TEXT NOT NULL,
                    states_appeared_json TEXT,
                    states_disappeared_json TEXT,
                    metadata_json TEXT,
                    PRIMARY KEY (session_id, timestamp)
                )
            """)

            # Index for fast matching queries
            self.conn.execute("""
                CREATE INDEX IF NOT EXISTS idx_action_lookup
                ON captures(action_type, action_target, states_before_json)
            """)

            # Runs table
            self.conn.execute("""
                CREATE TABLE IF NOT EXISTS runs (
                    run_id TEXT PRIMARY KEY,
                    video_path TEXT,
                    events_path TEXT,
                    import_timestamp REAL NOT NULL,
                    total_actions INTEGER NOT NULL,
                    metadata_json TEXT
                )
            """)

        logger.debug("Database tables initialized")

    def find_matching(self,
                     active_states: Set[str],
                     action_type: str,
                     action_target: str) -> List[HistoricalCapture]:
        """Find all historical instances where same context occurred.

        This is the core query for simulation. We find ALL captures where:
        - Same states were active before the action
        - Same action type was performed
        - Same target was acted upon

        The caller will randomly select from the results, making each test
        run different (like real automation where timing/conditions vary).

        Args:
            active_states: Set of currently active state names
            action_type: Type of action to match (e.g., "CLICK", "TYPE")
            action_target: Target identifier to match

        Returns:
            List of all matching HistoricalCapture instances (may be empty)
        """
        try:
            # Convert states to sorted JSON for exact matching
            states_json = json.dumps(sorted(active_states))

            cursor = self.conn.execute("""
                SELECT * FROM captures
                WHERE action_type = ?
                AND action_target = ?
                AND states_before_json = ?
                ORDER BY timestamp DESC
            """, (action_type, action_target, states_json))

            matches = []
            for row in cursor:
                try:
                    capture = self._row_to_capture(row)
                    matches.append(capture)
                except Exception as e:
                    logger.error(f"Error converting row to capture: {e}")
                    continue

            logger.debug(
                f"Found {len(matches)} matches for {action_type} on {action_target} "
                f"with states {active_states}"
            )
            return matches

        except Exception as e:
            logger.error(f"Error finding matches: {e}")
            return []

    def get_match_count(self,
                       active_states: Set[str],
                       action_type: str,
                       action_target: str) -> int:
        """Get count of matching captures without loading them.

        More efficient than find_matching() when you only need the count.

        Args:
            active_states: Set of currently active state names
            action_type: Type of action to match
            action_target: Target identifier to match

        Returns:
            Number of matching captures
        """
        try:
            states_json = json.dumps(sorted(active_states))

            cursor = self.conn.execute("""
                SELECT COUNT(*) as count FROM captures
                WHERE action_type = ?
                AND action_target = ?
                AND states_before_json = ?
                AND success = 1
            """, (action_type, action_target, states_json))

            result = cursor.fetchone()
            count = result["count"] if result else 0

            logger.debug(f"Match count: {count}")
            return count

        except Exception as e:
            logger.error(f"Error counting matches: {e}")
            return 0

    def import_automation_run(self,
                             run_id: str,
                             video_path: str,
                             events_path: str,
                             processed_states: Dict[str, Set[str]]) -> int:
        """Import data from an automation run into historical database.

        This processes a completed automation run and extracts historical
        captures that can be used for simulation. Each action execution
        becomes a potential simulation response.

        Args:
            run_id: Unique identifier for this run
            video_path: Path to video recording of the run
            events_path: Path to JSON file with ActionExecutionRecords
            processed_states: Dict mapping action_id -> Set of active states
                             at the time of that action

        Returns:
            Number of captures imported

        Raises:
            FileNotFoundError: If events file doesn't exist
            ValueError: If events file is malformed
        """
        try:
            events_file = Path(events_path)
            if not events_file.exists():
                raise FileNotFoundError(f"Events file not found: {events_path}")

            # Load action execution records
            with open(events_file, 'r') as f:
                events_data = json.load(f)

            if not isinstance(events_data, list):
                raise ValueError("Events file must contain a JSON array")

            # Process each action execution record
            captures_imported = 0
            with self.conn:
                # Record the run
                self.conn.execute("""
                    INSERT OR REPLACE INTO runs
                    (run_id, video_path, events_path, import_timestamp, total_actions, metadata_json)
                    VALUES (?, ?, ?, ?, ?, ?)
                """, (
                    run_id,
                    video_path,
                    events_path,
                    datetime.now().timestamp(),
                    len(events_data),
                    json.dumps({})
                ))

                # Convert each action to a historical capture
                for event_data in events_data:
                    try:
                        capture = self._event_to_capture(
                            event_data,
                            run_id,
                            video_path,
                            processed_states
                        )

                        if capture:
                            self._insert_capture(capture)
                            captures_imported += 1

                    except Exception as e:
                        logger.warning(f"Failed to import event {event_data.get('action_id')}: {e}")
                        continue

            logger.info(
                f"Imported {captures_imported} captures from run {run_id} "
                f"(video: {video_path})"
            )
            return captures_imported

        except Exception as e:
            logger.error(f"Error importing automation run: {e}")
            raise

    def get_statistics(self) -> Dict[str, Any]:
        """Return database statistics.

        Returns:
            Dictionary containing:
                - total_runs: Number of imported automation runs
                - total_captures: Total number of historical captures
                - total_actions: Total actions across all runs
                - action_type_counts: Count of captures by action type
                - state_coverage: Number of unique state combinations covered
                - earliest_capture: Timestamp of earliest capture
                - latest_capture: Timestamp of latest capture
        """
        try:
            stats = {}

            # Basic counts
            cursor = self.conn.execute("SELECT COUNT(*) as count FROM runs")
            stats["total_runs"] = cursor.fetchone()["count"]

            cursor = self.conn.execute("SELECT COUNT(*) as count FROM captures")
            stats["total_captures"] = cursor.fetchone()["count"]

            cursor = self.conn.execute("SELECT SUM(total_actions) as total FROM runs")
            result = cursor.fetchone()
            stats["total_actions"] = result["total"] if result["total"] else 0

            # Action type distribution
            cursor = self.conn.execute("""
                SELECT action_type, COUNT(*) as count
                FROM captures
                GROUP BY action_type
                ORDER BY count DESC
            """)
            stats["action_type_counts"] = {
                row["action_type"]: row["count"]
                for row in cursor
            }

            # State coverage (unique state combinations)
            cursor = self.conn.execute("""
                SELECT COUNT(DISTINCT states_before_json) as count
                FROM captures
            """)
            stats["state_coverage"] = cursor.fetchone()["count"]

            # Temporal range
            cursor = self.conn.execute("""
                SELECT MIN(timestamp) as earliest,
                       MAX(timestamp) as latest
                FROM captures
            """)
            result = cursor.fetchone()
            stats["earliest_capture"] = result["earliest"]
            stats["latest_capture"] = result["latest"]

            return stats

        except Exception as e:
            logger.error(f"Error getting statistics: {e}")
            return {}

    def _event_to_capture(self,
                         event_data: Dict[str, Any],
                         run_id: str,
                         video_path: str,
                         processed_states: Dict[str, Set[str]]) -> Optional[HistoricalCapture]:
        """Convert an ActionExecutionRecord to a HistoricalCapture.

        Args:
            event_data: Dictionary with action execution data
            run_id: ID of the automation run
            video_path: Path to video of the run
            processed_states: Mapping of action_id -> active states

        Returns:
            HistoricalCapture instance or None if conversion fails
        """
        try:
            action_id = event_data.get("action_id", "")

            # Get states for this action
            states_before = set(event_data.get("active_states_before", []))
            states_after = set(event_data.get("active_states_after", []))

            # Calculate appeared/disappeared states
            states_appeared = states_after - states_before
            states_disappeared = states_before - states_after

            # Extract action target (simplified identifier for matching)
            action_target = self._extract_action_target(event_data)

            # Use frame index if available, otherwise 0
            frame_before = event_data.get("frame_before", 0)
            frame_after = event_data.get("frame_after", frame_before + 1)

            capture = HistoricalCapture(
                session_id=run_id,
                timestamp=event_data.get("start_time", 0.0),
                frame_before=frame_before,
                frame_after=frame_after,
                active_states_before=states_before,
                action_type=event_data.get("action_type", "UNKNOWN"),
                action_target=action_target,
                action_params=event_data.get("config", {}),
                active_states_after=states_after,
                states_appeared=states_appeared,
                states_disappeared=states_disappeared,
                metadata=event_data.get("metadata", {})
            )

            return capture

        except Exception as e:
            logger.error(f"Error converting event to capture: {e}")
            return None

    def _extract_action_target(self, event_data: Dict[str, Any]) -> str:
        """Extract simplified action target identifier from event data.

        The target is a simplified identifier that can be used for matching.
        For example, for a CLICK action it might be the button name or image name.

        Args:
            event_data: Action execution event data

        Returns:
            Target identifier string
        """
        config = event_data.get("config", {})
        action_type = event_data.get("action_type", "")

        # Extract target based on action type
        if action_type == "CLICK":
            return config.get("imageName", config.get("text", "unknown"))
        elif action_type == "TYPE":
            return config.get("field", "unknown")
        elif action_type == "GO_TO_STATE":
            return config.get("target_state", "unknown")
        elif action_type == "RUN_WORKFLOW":
            return config.get("workflow_name", "unknown")
        else:
            return config.get("target", "unknown")

    def _insert_capture(self, capture: HistoricalCapture):
        """Insert a capture into the database.

        Args:
            capture: HistoricalCapture to insert
        """
        self.conn.execute("""
            INSERT OR REPLACE INTO captures
            (session_id, timestamp, frame_before, frame_after,
             states_before_json, action_type, action_target, action_params_json,
             states_after_json, states_appeared_json, states_disappeared_json,
             metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, (
            capture.session_id,
            capture.timestamp,
            capture.frame_before,
            capture.frame_after,
            json.dumps(sorted(capture.active_states_before)),
            capture.action_type,
            capture.action_target,
            json.dumps(capture.action_params),
            json.dumps(sorted(capture.active_states_after)),
            json.dumps(sorted(capture.states_appeared)),
            json.dumps(sorted(capture.states_disappeared)),
            json.dumps(capture.metadata)
        ))

    def _row_to_capture(self, row: sqlite3.Row) -> HistoricalCapture:
        """Convert database row to HistoricalCapture.

        Args:
            row: SQLite row from captures table

        Returns:
            HistoricalCapture instance
        """
        return HistoricalCapture(
            session_id=row["session_id"],
            timestamp=row["timestamp"],
            frame_before=row["frame_before"],
            frame_after=row["frame_after"],
            active_states_before=set(json.loads(row["states_before_json"])),
            action_type=row["action_type"],
            action_target=row["action_target"],
            action_params=json.loads(row["action_params_json"] or "{}"),
            active_states_after=set(json.loads(row["states_after_json"])),
            states_appeared=set(json.loads(row["states_appeared_json"] or "[]")),
            states_disappeared=set(json.loads(row["states_disappeared_json"] or "[]")),
            metadata=json.loads(row["metadata_json"] or "{}")
        )

    def close(self):
        """Close the database connection."""
        if self.conn:
            self.conn.close()
            logger.info("Database connection closed")

    def __enter__(self):
        """Context manager entry."""
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        """Context manager exit."""
        self.close()
