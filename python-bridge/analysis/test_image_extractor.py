"""Unit tests for the StateImage extraction service.

Run with: pytest analysis/test_image_extractor.py -v
"""

import numpy as np
import pytest
import cv2

from models import DetectedState, Frame, InputEvent, StateImage
from analysis import ImageExtractionConfig, StateImageExtractor


class TestImageExtractionConfig:
    """Tests for ImageExtractionConfig."""

    def test_default_config(self):
        """Test default configuration values."""
        config = ImageExtractionConfig()

        assert config.min_size == (20, 20)
        assert config.max_size == (500, 500)
        assert config.edge_detection == "canny"
        assert config.extract_at_click_locations is True
        assert config.click_region_padding == 20
        assert config.position_tolerance_px == 5

    def test_custom_config(self):
        """Test custom configuration values."""
        config = ImageExtractionConfig(
            min_size=(30, 30),
            max_size=(400, 400),
            edge_detection="sobel",
            click_region_padding=40,
        )

        assert config.min_size == (30, 30)
        assert config.max_size == (400, 400)
        assert config.edge_detection == "sobel"
        assert config.click_region_padding == 40


class TestStateImageExtractor:
    """Tests for StateImageExtractor."""

    @pytest.fixture
    def extractor(self):
        """Create a default extractor."""
        return StateImageExtractor()

    @pytest.fixture
    def sample_frame(self):
        """Create a sample frame with UI elements."""
        image = np.ones((600, 800, 3), dtype=np.uint8) * 240

        # Draw some UI elements
        cv2.rectangle(image, (50, 50), (150, 150), (100, 100, 255), 2)
        cv2.rectangle(image, (200, 50), (300, 150), (255, 100, 100), -1)
        cv2.circle(image, (400, 100), 50, (100, 255, 100), 2)

        return Frame(
            image=image,
            timestamp=0.0,
            frame_index=0,
            file_path="test_frame.png",
        )

    @pytest.fixture
    def sample_frames(self):
        """Create multiple sample frames."""
        frames = []
        for i in range(5):
            image = np.ones((600, 800, 3), dtype=np.uint8) * 240
            cv2.rectangle(image, (100, 100), (200, 200), (255, 0, 0), -1)

            frame = Frame(
                image=image,
                timestamp=float(i),
                frame_index=i,
                file_path=f"frame_{i}.png",
            )
            frames.append(frame)

        return frames

    @pytest.fixture
    def sample_state(self, sample_frames):
        """Create a sample state."""
        return DetectedState(
            name="TestState",
            description="A test state",
            state_images=[],
            start_frame_index=0,
            end_frame_index=len(sample_frames) - 1,
            frame_indices=list(range(len(sample_frames))),
            click_locations=[(150, 150), (400, 300)],
        )

    @pytest.fixture
    def sample_events(self):
        """Create sample input events."""
        return [
            InputEvent(timestamp=1.0, event_type="click", x=150, y=150, button="left"),
            InputEvent(timestamp=3.0, event_type="click", x=400, y=300, button="left"),
        ]

    def test_initialization(self):
        """Test extractor initialization."""
        config = ImageExtractionConfig(min_size=(30, 30))
        extractor = StateImageExtractor(config)

        assert extractor.config.min_size == (30, 30)

    def test_extract_at_location(self, extractor, sample_frame):
        """Test extracting image at a specific location."""
        state_image = extractor.extract_at_location(
            frame=sample_frame.image,
            x=100,
            y=100,
            padding=30,
            frame_index=0,
        )

        assert state_image is not None
        assert state_image.image is not None
        assert state_image.bbox[2] > 0  # width > 0
        assert state_image.bbox[3] > 0  # height > 0
        assert state_image.position == (state_image.bbox[0], state_image.bbox[1])
        assert state_image.context_image is not None

    def test_extract_at_location_with_small_region(self, extractor, sample_frame):
        """Test extraction with region too small."""
        state_image = extractor.extract_at_location(
            frame=sample_frame.image,
            x=100,
            y=100,
            padding=5,  # Very small padding
            frame_index=0,
        )

        # Should return None if region is too small based on config
        # But with default min_size=(20,20), padding=5 gives 10x10 which is too small
        assert state_image is None

    def test_extract_at_location_out_of_bounds(self, extractor, sample_frame):
        """Test extraction at edge of frame."""
        state_image = extractor.extract_at_location(
            frame=sample_frame.image,
            x=10,
            y=10,
            padding=30,
            frame_index=0,
        )

        # Should handle boundary conditions gracefully
        if state_image:
            assert state_image.bbox[0] >= 0
            assert state_image.bbox[1] >= 0

    def test_detect_contours(self, extractor, sample_frame):
        """Test contour detection."""
        bboxes = extractor.detect_contours(sample_frame.image)

        assert isinstance(bboxes, list)
        assert len(bboxes) > 0  # Should detect some contours

        # Check bbox format
        for bbox in bboxes:
            assert len(bbox) == 4
            x, y, w, h = bbox
            assert w > 0 and h > 0

    def test_detect_contours_with_different_methods(self, sample_frame):
        """Test different edge detection methods."""
        methods = ["canny", "sobel", "laplacian"]

        for method in methods:
            config = ImageExtractionConfig(edge_detection=method)
            extractor = StateImageExtractor(config)

            bboxes = extractor.detect_contours(sample_frame.image)
            assert isinstance(bboxes, list)

    def test_determine_position_type_fixed(self, extractor):
        """Test position type determination for fixed positions."""
        image = np.zeros((50, 50, 3), dtype=np.uint8)
        state_image = StateImage(
            name="test",
            image=image,
            bbox=(0, 0, 50, 50),
            position_type="unknown",
            position=(0, 0),
        )

        # Positions very close together (fixed)
        occurrences = [(100, 100), (101, 100), (100, 101), (101, 101)]
        is_fixed = extractor.determine_position_type(state_image, occurrences)

        assert is_fixed is True

    def test_determine_position_type_dynamic(self, extractor):
        """Test position type determination for dynamic positions."""
        image = np.zeros((50, 50, 3), dtype=np.uint8)
        state_image = StateImage(
            name="test",
            image=image,
            bbox=(0, 0, 50, 50),
            position_type="unknown",
            position=(0, 0),
        )

        # Positions spread out (dynamic)
        occurrences = [(100, 100), (200, 200), (50, 50), (300, 400)]
        is_fixed = extractor.determine_position_type(state_image, occurrences)

        assert is_fixed is False

    def test_determine_position_type_single_occurrence(self, extractor):
        """Test position type with only one occurrence."""
        image = np.zeros((50, 50, 3), dtype=np.uint8)
        state_image = StateImage(
            name="test",
            image=image,
            bbox=(0, 0, 50, 50),
            position_type="unknown",
            position=(0, 0),
        )

        # Single occurrence should be considered fixed
        occurrences = [(100, 100)]
        is_fixed = extractor.determine_position_type(state_image, occurrences)

        assert is_fixed is True

    def test_find_best_crop(self, extractor, sample_frame):
        """Test finding optimal crop boundaries."""
        region = (50, 50, 100, 100)
        state_image = extractor.find_best_crop(sample_frame.image, region)

        assert state_image is not None
        assert state_image.image is not None
        assert state_image.bbox is not None
        assert len(state_image.bbox) == 4

    def test_extract_from_state_basic(self, extractor, sample_state, sample_frames, sample_events):
        """Test basic state extraction."""
        extracted = extractor.extract_from_state(
            state=sample_state,
            frames=sample_frames,
            events=sample_events,
        )

        assert isinstance(extracted, list)
        # Should extract some images (at least from click locations)
        assert len(extracted) > 0

    def test_extract_from_state_with_click_locations(self, sample_state, sample_frames, sample_events):
        """Test extraction with click locations enabled."""
        config = ImageExtractionConfig(
            extract_at_click_locations=True,
            click_region_padding=30,
        )
        extractor = StateImageExtractor(config)

        extracted = extractor.extract_from_state(
            state=sample_state,
            frames=sample_frames,
            events=sample_events,
        )

        # Should have images from click locations
        click_images = [img for img in extracted if img.extraction_method == "click_location"]
        assert len(click_images) > 0

    def test_extract_from_state_without_click_locations(self, sample_state, sample_frames, sample_events):
        """Test extraction with click locations disabled."""
        config = ImageExtractionConfig(
            extract_at_click_locations=False,
        )
        extractor = StateImageExtractor(config)

        extracted = extractor.extract_from_state(
            state=sample_state,
            frames=sample_frames,
            events=sample_events,
        )

        # Should not have images from click locations
        click_images = [img for img in extracted if img.extraction_method == "click_location"]
        assert len(click_images) == 0

    def test_extract_from_state_empty_frames(self, extractor, sample_state, sample_events):
        """Test extraction with no frames."""
        extracted = extractor.extract_from_state(
            state=sample_state,
            frames=[],
            events=sample_events,
        )

        assert extracted == []

    def test_extract_from_state_position_classification(self, sample_state, sample_frames, sample_events):
        """Test that position types are classified."""
        config = ImageExtractionConfig(
            extract_at_click_locations=True,
            click_region_padding=30,
        )
        extractor = StateImageExtractor(config)

        extracted = extractor.extract_from_state(
            state=sample_state,
            frames=sample_frames,
            events=sample_events,
        )

        # All images should have position type set
        for img in extracted:
            assert img.position_type in ["fixed", "dynamic", "unknown"]


class TestStateImage:
    """Tests for StateImage data model."""

    def test_state_image_creation(self):
        """Test creating a StateImage."""
        image = np.zeros((100, 100, 3), dtype=np.uint8)
        state_image = StateImage(
            name="test_image",
            image=image,
            bbox=(10, 20, 80, 60),
            position_type="fixed",
            position=(10, 20),
        )

        assert state_image.name == "test_image"
        assert state_image.bbox == (10, 20, 80, 60)
        assert state_image.position_type == "fixed"
        assert state_image.position == (10, 20)

    def test_state_image_to_dict(self):
        """Test converting StateImage to dictionary."""
        image = np.zeros((100, 100, 3), dtype=np.uint8)
        state_image = StateImage(
            name="test_image",
            image=image,
            bbox=(10, 20, 80, 60),
            position_type="fixed",
            position=(10, 20),
            extraction_method="manual",
            metadata={"test": True},
        )

        data = state_image.to_dict()

        assert data["name"] == "test_image"
        assert data["bbox"] == (10, 20, 80, 60)
        assert data["position_type"] == "fixed"
        assert data["extraction_method"] == "manual"
        assert data["metadata"]["test"] is True
        # Image data should not be in dict
        assert "image" not in data


class TestHelperFunctions:
    """Tests for helper functions."""

    def test_get_state_frames_with_frame_indices(self):
        """Test getting state frames using explicit indices."""
        frames = [
            Frame(np.zeros((100, 100, 3), dtype=np.uint8), 0.0, i, None)
            for i in range(10)
        ]

        state = DetectedState(
            name="test",
            description="test",
            state_images=[],
            start_frame_index=0,
            end_frame_index=9,
            frame_indices=[0, 2, 4, 6, 8],
        )

        extractor = StateImageExtractor()
        state_frames = extractor._get_state_frames(state, frames)

        assert len(state_frames) == 5
        assert [f.frame_index for f in state_frames] == [0, 2, 4, 6, 8]

    def test_get_state_frames_with_range(self):
        """Test getting state frames using start/end range."""
        frames = [
            Frame(np.zeros((100, 100, 3), dtype=np.uint8), 0.0, i, None)
            for i in range(10)
        ]

        state = DetectedState(
            name="test",
            description="test",
            state_images=[],
            start_frame_index=3,
            end_frame_index=6,
            frame_indices=[],
        )

        extractor = StateImageExtractor()
        state_frames = extractor._get_state_frames(state, frames)

        assert len(state_frames) == 4
        assert [f.frame_index for f in state_frames] == [3, 4, 5, 6]

    def test_get_click_locations_from_state(self):
        """Test getting click locations from state."""
        state = DetectedState(
            name="test",
            description="test",
            state_images=[],
            start_frame_index=0,
            end_frame_index=1,
            click_locations=[(100, 100), (200, 200)],
        )

        extractor = StateImageExtractor()
        clicks = extractor._get_click_locations(state, [])

        assert clicks == [(100, 100), (200, 200)]

    def test_get_click_locations_from_events(self):
        """Test getting click locations from events."""
        state = DetectedState(
            name="test",
            description="test",
            state_images=[],
            start_frame_index=0,
            end_frame_index=1,
            click_locations=[],
        )

        events = [
            InputEvent(0.0, "click", x=150, y=150, button="left"),
            InputEvent(1.0, "click", x=250, y=250, button="left"),
            InputEvent(2.0, "key", key="a"),  # Should be filtered
        ]

        extractor = StateImageExtractor()
        clicks = extractor._get_click_locations(state, events)

        assert len(clicks) == 2
        assert (150, 150) in clicks
        assert (250, 250) in clicks


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
