"""Cross-session element ID resolver using property-based matching.

When element IDs change between sessions (e.g., dynamically generated IDs),
this resolver matches current elements to stored element properties using
multi-strategy matching:

1. Exact ID match (no remap needed)
2. Semantic match: same role + accessible_name + tag_name
3. Structural match: same role + position_zone + tag_name
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from state_machine_persistence import StateMachinePersistence
    from ui_bridge_http_client import ElementProxy

logger = logging.getLogger(__name__)


class ElementResolver:
    """Resolves element IDs across sessions using property-based matching."""

    def __init__(self, persistence: StateMachinePersistence) -> None:
        self._persistence = persistence
        self._id_remap: dict[str, str] = {}  # old_id -> current_id
        self._reverse_remap: dict[str, str] = {}  # current_id -> old_id

    def capture_snapshots(self, elements: list[ElementProxy]) -> None:
        """Capture current element properties and store for future matching."""
        snapshots: dict[str, dict] = {}
        for elem in elements:
            snapshots[elem.id] = {
                "tag_name": elem.tag_name,
                "role": elem.role,
                "accessible_name": elem.accessible_name,
                "label": elem.label,
                "position_zone": self._compute_position_zone(elem.bounds),
                "classes": elem.classes,
            }
        self._persistence.save_element_snapshots(snapshots)

    def build_remap(self, current_elements: list[ElementProxy]) -> dict[str, str]:
        """Build a mapping from stored element IDs to current element IDs.

        For each stored element, find the best-matching current element using:
        1. Exact ID match (no remap needed)
        2. Same role + accessible_name + tag_name
        3. Same role + position_zone + tag_name

        Returns:
            Mapping of old_id -> current_id for elements that changed IDs.
        """
        stored = self._persistence.load_element_snapshots()
        if not stored:
            self._id_remap = {}
            self._reverse_remap = {}
            return {}

        current_by_id = {e.id: e for e in current_elements}
        remap: dict[str, str] = {}
        # Track which current IDs have already been claimed
        claimed: set[str] = set()

        for old_id, props in stored.items():
            # Strategy 1: exact ID still exists
            if old_id in current_by_id:
                claimed.add(old_id)
                continue  # No remap needed

            # Strategy 2: semantic match (role + accessible_name + tag_name)
            best = self._find_semantic_match(props, current_elements, claimed)
            if best:
                remap[old_id] = best
                claimed.add(best)
                continue

            # Strategy 3: structural match (role + position_zone + tag_name)
            best = self._find_structural_match(props, current_elements, claimed)
            if best:
                remap[old_id] = best
                claimed.add(best)

        self._id_remap = remap
        self._reverse_remap = {v: k for k, v in remap.items()}

        if remap:
            logger.info(f"Element ID remap built: {len(remap)} remapped IDs")
            for old_id, new_id in remap.items():
                logger.debug(f"  {old_id} -> {new_id}")

        return remap

    def resolve_id(self, element_id: str) -> str:
        """Resolve a stored element ID to its current ID."""
        return self._id_remap.get(element_id, element_id)

    def resolve_element_ids(self, element_ids: list[str]) -> list[str]:
        """Resolve a list of stored element IDs to current IDs."""
        return [self.resolve_id(eid) for eid in element_ids]

    def get_reverse_remap(self) -> dict[str, str]:
        """Get reverse mapping: current_id -> old_id.

        Used to inject old IDs as aliases so state detection still finds them.
        """
        return self._reverse_remap

    # =========================================================================
    # Matching strategies
    # =========================================================================

    def _find_semantic_match(
        self,
        props: dict,
        current_elements: list[ElementProxy],
        claimed: set[str],
    ) -> str | None:
        """Find element with same role + accessible_name + tag_name."""
        role = props.get("role", "")
        accessible_name = props.get("accessible_name", "")
        tag_name = props.get("tag_name", "")

        # Need at least role or accessible_name to be meaningful
        if not role and not accessible_name:
            return None

        for elem in current_elements:
            if elem.id in claimed:
                continue
            if (
                elem.role == role
                and elem.accessible_name == accessible_name
                and elem.tag_name == tag_name
                and role  # Don't match on empty role
                and accessible_name  # Don't match on empty name
            ):
                return elem.id

        return None

    def _find_structural_match(
        self,
        props: dict,
        current_elements: list[ElementProxy],
        claimed: set[str],
    ) -> str | None:
        """Find element with same role + position_zone + tag_name."""
        role = props.get("role", "")
        position_zone = props.get("position_zone", "")
        tag_name = props.get("tag_name", "")

        if not role or position_zone == "unknown":
            return None

        for elem in current_elements:
            if elem.id in claimed:
                continue
            elem_zone = self._compute_position_zone(elem.bounds)
            if (
                elem.role == role
                and elem_zone == position_zone
                and elem.tag_name == tag_name
            ):
                return elem.id

        return None

    @staticmethod
    def _compute_position_zone(bounds: dict) -> str:
        """Derive a coarse position zone from element bounds."""
        if not bounds:
            return "unknown"
        y = bounds.get("y", 0)
        x = bounds.get("x", 0)
        if y < 80:
            return "header"
        elif y > 800:
            return "footer"
        elif x < 250:
            return "sidebar-left"
        elif x > 1200:
            return "sidebar-right"
        return "main"
